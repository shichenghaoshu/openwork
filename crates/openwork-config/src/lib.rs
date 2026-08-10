//! Versioned configuration, lockfile, provenance, and secret-storage boundaries.
//!
//! The runtime lockfile is deliberately a resolved-state document, not a place
//! for credentials. Configuration, resolved runtime state, and secrets have
//! separate paths so callers cannot accidentally persist authentication data in
//! a portable lockfile.

use atomicwrites::{AtomicFile, Error as AtomicWriteError, OverwriteBehavior};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The only lockfile schema version understood by this build.
pub const RUNTIME_LOCKFILE_VERSION: u32 = 1;

/// Physical separation between user configuration, resolved state, and secrets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoragePaths {
    pub config: PathBuf,
    pub lockfile: PathBuf,
    pub secrets: PathBuf,
}

impl StoragePaths {
    #[must_use]
    pub fn new(config: PathBuf, lockfile: PathBuf, secrets: PathBuf) -> Self {
        Self {
            config,
            lockfile,
            secrets,
        }
    }

    /// Rejects aliases that would collapse a secret store into a portable file.
    ///
    /// # Errors
    ///
    /// Returns [`LockfileError::StorageBoundary`] when any two paths are equal.
    pub fn validate(&self) -> Result<(), LockfileError> {
        if self.config == self.lockfile
            || self.config == self.secrets
            || self.lockfile == self.secrets
        {
            return Err(LockfileError::StorageBoundary(
                "config, lockfile, and secrets paths must be distinct".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeLockfile {
    pub schema_version: u32,
    pub generated_at: String,
    pub runtimes: BTreeMap<String, RuntimeLockEntry>,
}

impl RuntimeLockfile {
    #[must_use]
    pub fn empty(generated_at: impl Into<String>) -> Self {
        Self {
            schema_version: RUNTIME_LOCKFILE_VERSION,
            generated_at: generated_at.into(),
            runtimes: BTreeMap::new(),
        }
    }

    /// Validates schema compatibility and every resolved runtime record.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported versions, missing provenance, or invalid
    /// resolved runtime records.
    pub fn validate(&self) -> Result<(), LockfileError> {
        if self.schema_version != RUNTIME_LOCKFILE_VERSION {
            return Err(LockfileError::UnsupportedVersion {
                found: u64::from(self.schema_version),
                supported: RUNTIME_LOCKFILE_VERSION,
            });
        }
        require_non_empty("generatedAt", &self.generated_at)?;
        for (runtime_id, entry) in &self.runtimes {
            require_non_empty("runtime id", runtime_id)?;
            entry.validate(runtime_id)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeLockEntry {
    pub requested: RequestedRuntime,
    pub resolved: ResolvedRuntime,
    pub source: RuntimeSource,
    pub checksum: RuntimeChecksum,
    pub installed_path: PathBuf,
    pub timestamps: RuntimeTimestamps,
    pub status: RuntimeInstallStatus,
    pub upstream: UpstreamProvenance,
    pub license: RuntimeLicense,
}

impl RuntimeLockEntry {
    fn validate(&self, runtime_id: &str) -> Result<(), LockfileError> {
        require_non_empty("requested.constraint", &self.requested.constraint)?;
        require_non_empty("resolved.version", &self.resolved.version)?;
        require_non_empty("source.uri", &self.source.uri)?;
        require_non_empty("timestamps.createdAt", &self.timestamps.created_at)?;
        require_non_empty("timestamps.updatedAt", &self.timestamps.updated_at)?;
        require_non_empty("upstream.projectUrl", &self.upstream.project_url)?;
        require_non_empty("license.spdx", &self.license.spdx)?;
        if self.installed_path.as_os_str().is_empty() {
            return Err(LockfileError::Invalid(format!(
                "runtime {runtime_id}: installedPath must not be empty"
            )));
        }
        require_non_empty("checksum.algorithm", &self.checksum.algorithm)?;
        match (self.checksum.authority, &self.checksum.digest) {
            (ChecksumAuthority::Unavailable, None) => {}
            (ChecksumAuthority::Unavailable, Some(_)) => {
                return Err(LockfileError::Invalid(format!(
                    "runtime {runtime_id}: unavailable checksum authority requires digest=null"
                )));
            }
            (_, Some(digest)) => require_non_empty("checksum.digest", digest)?,
            (_, None) => {
                return Err(LockfileError::Invalid(format!(
                    "runtime {runtime_id}: checksum digest is required for this authority"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestedRuntime {
    pub constraint: String,
    pub channel: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedRuntime {
    pub version: String,
    pub artifact: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeSource {
    pub kind: RuntimeSourceKind,
    pub uri: String,
    pub reference: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeSourceKind {
    OfficialInstaller,
    PackageManager,
    ReleaseArtifact,
    SystemPath,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeChecksum {
    pub algorithm: String,
    pub digest: Option<String>,
    pub authority: ChecksumAuthority,
    pub authority_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChecksumAuthority {
    Upstream,
    InstallerMetadata,
    PackageManager,
    Observed,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeTimestamps {
    pub created_at: String,
    pub updated_at: String,
    pub verified_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeInstallStatus {
    Pending,
    Installed,
    Failed,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpstreamProvenance {
    pub project_url: String,
    pub release_url: Option<String>,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeLicense {
    pub spdx: String,
    pub url: Option<String>,
}

#[derive(Debug)]
pub enum LockfileError {
    Io(io::Error),
    Json(serde_json::Error),
    Invalid(String),
    UnsupportedVersion { found: u64, supported: u32 },
    StorageBoundary(String),
}

impl fmt::Display for LockfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "lockfile I/O error: {error}"),
            Self::Json(error) => write!(formatter, "invalid lockfile JSON: {error}"),
            Self::Invalid(message) | Self::StorageBoundary(message) => formatter.write_str(message),
            Self::UnsupportedVersion { found, supported } => write!(
                formatter,
                "unsupported runtime lockfile version {found}; this build supports {supported}"
            ),
        }
    }
}

impl std::error::Error for LockfileError {}

impl From<io::Error> for LockfileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LockfileError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Explicit schema migration boundary. Add old-to-new migrations here, never in
/// deserialization call sites.
pub struct LockfileMigrator;

impl LockfileMigrator {
    /// Migrates a JSON document into the current typed lockfile.
    ///
    /// # Errors
    ///
    /// Returns an error when the version is absent, invalid, or newer than this
    /// build, or when a current-version document fails validation.
    pub fn migrate(value: Value) -> Result<RuntimeLockfile, LockfileError> {
        let version = value
            .get("schemaVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| LockfileError::Invalid("schemaVersion must be an integer".into()))?;

        match version {
            1 => {
                let lockfile: RuntimeLockfile = serde_json::from_value(value)?;
                lockfile.validate()?;
                Ok(lockfile)
            }
            // Future migrations are intentionally explicit match arms.
            other => Err(LockfileError::UnsupportedVersion {
                found: other,
                supported: RUNTIME_LOCKFILE_VERSION,
            }),
        }
    }
}

/// Serialized access plus atomic replacement for a single runtime lockfile.
#[derive(Clone, Debug)]
pub struct LockfileStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl LockfileStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let lock_path = sidecar_lock_path(&path);
        Self { path, lock_path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads, migrates, and validates the stored document.
    ///
    /// # Errors
    ///
    /// Returns an I/O, JSON, migration, or validation error.
    pub fn read(&self) -> Result<RuntimeLockfile, LockfileError> {
        read_lockfile(&self.path)
    }

    /// Atomically replaces the stored document under an exclusive process lock.
    ///
    /// # Errors
    ///
    /// Returns an error if validation, serialization, locking, or the atomic
    /// replacement fails. The previous target remains intact on write failure.
    pub fn write(&self, lockfile: &RuntimeLockfile) -> Result<(), LockfileError> {
        lockfile.validate()?;
        let bytes = serde_json::to_vec_pretty(lockfile)?;
        self.with_exclusive_lock(|| atomic_replace(&self.path, &bytes))
    }

    /// Applies one read-modify-write transaction under the cross-process lock.
    ///
    /// # Errors
    ///
    /// Returns an error from reading, the update callback, validation, locking,
    /// or atomic replacement.
    pub fn update<F>(&self, update: F) -> Result<RuntimeLockfile, LockfileError>
    where
        F: FnOnce(&mut RuntimeLockfile) -> Result<(), LockfileError>,
    {
        self.with_exclusive_lock(|| {
            let mut lockfile = read_lockfile(&self.path)?;
            update(&mut lockfile)?;
            lockfile.validate()?;
            let bytes = serde_json::to_vec_pretty(&lockfile)?;
            atomic_replace(&self.path, &bytes)?;
            Ok(lockfile)
        })
    }

    fn with_exclusive_lock<T>(
        &self,
        action: impl FnOnce() -> Result<T, LockfileError>,
    ) -> Result<T, LockfileError> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)?;
        lock.lock_exclusive()?;
        let result = action();
        let unlock_result = FileExt::unlock(&lock);
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(LockfileError::Io(error)),
        }
    }
}

fn read_lockfile(path: &Path) -> Result<RuntimeLockfile, LockfileError> {
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    LockfileMigrator::migrate(value)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), LockfileError> {
    atomic_replace_with(path, |file| {
        file.write_all(bytes)?;
        file.sync_all()?;
        set_private_permissions(file)
    })
}

fn atomic_replace_with(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<(), LockfileError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    AtomicFile::new(path, OverwriteBehavior::AllowOverwrite)
        .write(write)
        .map_err(|error| match error {
            AtomicWriteError::Internal(error) | AtomicWriteError::User(error) => {
                LockfileError::Io(error)
            }
        })
}

#[cfg(unix)]
fn set_private_permissions(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

fn sidecar_lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".write-lock");
    PathBuf::from(name)
}

fn require_non_empty(field: &str, value: &str) -> Result<(), LockfileError> {
    if value.trim().is_empty() {
        Err(LockfileError::Invalid(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::tempdir;

    fn fixture() -> RuntimeLockfile {
        let mut lockfile = RuntimeLockfile::empty("2026-08-10T10:00:00Z");
        lockfile.runtimes.insert(
            "codex".into(),
            RuntimeLockEntry {
                requested: RequestedRuntime {
                    constraint: "stable".into(),
                    channel: Some("stable".into()),
                },
                resolved: ResolvedRuntime {
                    version: "0.147.0".into(),
                    artifact: Some("codex-aarch64-apple-darwin".into()),
                },
                source: RuntimeSource {
                    kind: RuntimeSourceKind::OfficialInstaller,
                    uri: "https://chatgpt.com/codex/install.sh".into(),
                    reference: Some("rust-v0.147.0".into()),
                },
                checksum: RuntimeChecksum {
                    algorithm: "sha256".into(),
                    digest: Some("abc123".into()),
                    authority: ChecksumAuthority::Upstream,
                    authority_url: Some("https://releases.openai.com/codex/metadata.json".into()),
                },
                installed_path: PathBuf::from("/opt/openwork/bin/codex"),
                timestamps: RuntimeTimestamps {
                    created_at: "2026-08-10T10:00:00Z".into(),
                    updated_at: "2026-08-10T10:00:00Z".into(),
                    verified_at: Some("2026-08-10T10:01:00Z".into()),
                },
                status: RuntimeInstallStatus::Installed,
                upstream: UpstreamProvenance {
                    project_url: "https://github.com/openai/codex".into(),
                    release_url: Some(
                        "https://github.com/openai/codex/releases/tag/rust-v0.147.0".into(),
                    ),
                    revision: Some("rust-v0.147.0".into()),
                },
                license: RuntimeLicense {
                    spdx: "Apache-2.0".into(),
                    url: Some("https://github.com/openai/codex/blob/main/LICENSE".into()),
                },
            },
        );
        lockfile
    }

    #[test]
    fn round_trip_preserves_provenance_without_secrets() {
        let lockfile = fixture();
        let json = serde_json::to_string_pretty(&lockfile).expect("serialize fixture");
        assert!(!json.to_ascii_lowercase().contains("token"));
        assert!(!json.to_ascii_lowercase().contains("secret"));
        let restored =
            LockfileMigrator::migrate(serde_json::from_str(&json).expect("deserialize JSON value"))
                .expect("migrate current version");
        assert_eq!(restored, lockfile);
    }

    #[test]
    fn rejects_missing_invalid_and_future_versions() {
        for json in [
            r#"{"generatedAt":"now","runtimes":{}}"#,
            r#"{"schemaVersion":"1","generatedAt":"now","runtimes":{}}"#,
            r#"{"schemaVersion":0,"generatedAt":"now","runtimes":{}}"#,
            r#"{"schemaVersion":2,"generatedAt":"now","runtimes":{}}"#,
        ] {
            let value = serde_json::from_str(json).expect("valid JSON");
            assert!(LockfileMigrator::migrate(value).is_err(), "accepted {json}");
        }
    }

    #[test]
    fn rejects_secret_fields_and_path_aliases() {
        let mut value = serde_json::to_value(fixture()).expect("serialize fixture");
        value["runtimes"]["codex"]["authToken"] = Value::String("nope".into());
        assert!(LockfileMigrator::migrate(value).is_err());

        let paths = StoragePaths::new("same".into(), "lock".into(), "same".into());
        assert!(paths.validate().is_err());
    }

    #[test]
    fn schema_tracks_the_typed_version_and_secret_boundary() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../../contracts/schemas/runtime-lockfile.v1.schema.json"
        ))
        .expect("valid schema JSON");
        assert_eq!(schema["properties"]["schemaVersion"]["const"], 1);
        assert_eq!(schema["$defs"]["runtime"]["additionalProperties"], false);
        assert!(schema.to_string().find("authToken").is_none());
    }

    #[test]
    fn failed_atomic_write_preserves_existing_file() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("runtime.lock.json");
        fs::write(&path, b"original").expect("seed target");

        let result = atomic_replace_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("simulated interruption"))
        });

        assert!(result.is_err());
        assert_eq!(fs::read(&path).expect("read preserved target"), b"original");
    }

    #[test]
    fn concurrent_updates_are_serialized_without_lost_writes() {
        let directory = tempdir().expect("tempdir");
        let store = LockfileStore::new(directory.path().join("runtime.lock.json"));
        store
            .write(&RuntimeLockfile::empty("initial"))
            .expect("seed");
        let barrier = Arc::new(Barrier::new(5));
        let mut threads = Vec::new();

        for index in 0..4 {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(thread::spawn(move || {
                barrier.wait();
                store
                    .update(|lockfile| {
                        lockfile.generated_at.push(char::from(b'0' + index));
                        Ok(())
                    })
                    .expect("serialized update");
            }));
        }
        barrier.wait();
        for handle in threads {
            handle.join().expect("update thread");
        }

        let result = store.read().expect("read result");
        assert_eq!(result.generated_at.len(), "initial".len() + 4);
        for digit in ['0', '1', '2', '3'] {
            assert!(result.generated_at.contains(digit));
        }
    }

    #[cfg(unix)]
    #[test]
    fn lockfile_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("runtime.lock.json");
        LockfileStore::new(&path).write(&fixture()).expect("write");
        let mode = fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
