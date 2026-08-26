//! Artifact discovery beneath a trusted, quiescent sandbox output root.

use crate::{
    Artifact, ArtifactId, ArtifactSizeBytes, RelativeArtifactPath, RunId, Sha256Digest,
    UtcTimestamp,
};
use openwork_core::{ErrorCode, OpenWorkError};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Bounded scanner that rejects traversal, symlinks, special files, and changed output.
pub struct ArtifactScanner {
    max_bytes: u64,
}

impl ArtifactScanner {
    /// Creates a scanner with a limit no larger than the frozen artifact maximum.
    ///
    /// # Errors
    ///
    /// Returns an error when `max_bytes` exceeds the contract limit.
    pub fn new(max_bytes: u64) -> Result<Self, OpenWorkError> {
        ArtifactSizeBytes::new(max_bytes)?;
        Ok(Self { max_bytes })
    }

    /// Scans only explicitly reported output paths and computes authoritative metadata.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe path, non-regular file, I/O failure, or size violation.
    pub fn scan(
        &self,
        run_id: &RunId,
        output_root: &Path,
        paths: &[RelativeArtifactPath],
        created_at: UtcTimestamp,
    ) -> Result<Vec<Artifact>, OpenWorkError> {
        let root = trusted_root(output_root)?;
        let mut seen = BTreeSet::new();
        paths
            .iter()
            .map(|path| {
                if !seen.insert(path.as_str().to_owned()) {
                    return Err(artifact_error("duplicate output path"));
                }
                self.scan_one(run_id, &root, path, created_at)
            })
            .collect()
    }

    /// Re-hashes a persisted artifact and rejects size or digest drift.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact is unsafe, unreadable, or differs from its record.
    pub fn verify(&self, output_root: &Path, artifact: &Artifact) -> Result<(), OpenWorkError> {
        let root = trusted_root(output_root)?;
        let current =
            self.scan_one(&artifact.run_id, &root, &artifact.path, artifact.created_at)?;
        if current.size_bytes != artifact.size_bytes || current.sha256 != artifact.sha256 {
            return Err(artifact_error("artifact size or SHA-256 mismatch"));
        }
        Ok(())
    }

    fn scan_one(
        &self,
        run_id: &RunId,
        root: &Path,
        relative: &RelativeArtifactPath,
        created_at: UtcTimestamp,
    ) -> Result<Artifact, OpenWorkError> {
        let candidate = root.join(relative.as_str());
        reject_symlink_components(root, relative)?;
        let canonical =
            fs::canonicalize(&candidate).map_err(|_| artifact_error("artifact is unavailable"))?;
        if !canonical.starts_with(root) || canonical == root {
            return Err(artifact_error("artifact escapes the output root"));
        }
        let mut file =
            File::open(&canonical).map_err(|_| artifact_error("artifact cannot be read"))?;
        let before = file
            .metadata()
            .map_err(|_| artifact_error("artifact metadata unavailable"))?;
        if !before.is_file() || before.len() > self.max_bytes {
            return Err(artifact_error("artifact is not a bounded regular file"));
        }
        let mut hasher = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|_| artifact_error("artifact cannot be hashed"))?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(
                    u64::try_from(count).map_err(|_| artifact_error("artifact too large"))?,
                )
                .ok_or_else(|| artifact_error("artifact too large"))?;
            if copied > self.max_bytes {
                return Err(artifact_error("artifact exceeds its configured limit"));
            }
            hasher.update(&buffer[..count]);
        }
        let after = file
            .metadata()
            .map_err(|_| artifact_error("artifact metadata unavailable"))?;
        if copied != before.len() || copied > self.max_bytes || before.len() != after.len() {
            return Err(artifact_error(
                "artifact changed while scanning or exceeded its limit",
            ));
        }
        let mut encoded = String::with_capacity(64);
        for byte in hasher.finalize() {
            write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
        }
        let sha256 = Sha256Digest::parse(encoded)?;
        Ok(Artifact {
            schema_version: crate::EXECUTION_SCHEMA_VERSION,
            id: ArtifactId::generate(),
            run_id: run_id.clone(),
            path: relative.clone(),
            media_type: media_type(&canonical).to_owned(),
            size_bytes: ArtifactSizeBytes::new(copied)?,
            sha256,
            created_at,
        })
    }
}

fn trusted_root(path: &Path) -> Result<PathBuf, OpenWorkError> {
    if fs::symlink_metadata(path)
        .map_err(|_| artifact_error("output root is unavailable"))?
        .file_type()
        .is_symlink()
    {
        return Err(artifact_error("output root cannot be a symlink"));
    }
    let root = fs::canonicalize(path).map_err(|_| artifact_error("output root is unavailable"))?;
    if !fs::metadata(&root).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(artifact_error("output root is not a directory"));
    }
    Ok(root)
}

fn reject_symlink_components(
    root: &Path,
    relative: &RelativeArtifactPath,
) -> Result<(), OpenWorkError> {
    let mut current = root.to_path_buf();
    for component in relative.as_str().split('/') {
        current.push(component);
        if fs::symlink_metadata(&current)
            .map_err(|_| artifact_error("artifact path is unavailable"))?
            .file_type()
            .is_symlink()
        {
            return Err(artifact_error("artifact path cannot contain symlinks"));
        }
    }
    Ok(())
}

fn media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

fn artifact_error(message: &str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ArtifactInvalid, message)
}
