use crate::sandbox_error;
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::RelativeArtifactPath;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const MAX_OUTPUT_ENTRIES: u32 = 4096;
const MAX_OUTPUT_FILES: usize = 1024;
const MAX_OUTPUT_DEPTH: u16 = 64;

#[derive(Debug)]
pub(crate) struct OwnedTemporaryDirectory {
    path: PathBuf,
    runtime_path: PathBuf,
    closed: bool,
}

impl OwnedTemporaryDirectory {
    pub(crate) fn create(root: &Path) -> Result<Self, OpenWorkError> {
        validate_mount(root)?;
        ensure_private_storage_supported()?;
        for _ in 0..32 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("openwork-{}-{sequence}", std::process::id()));
            match create_private_directory(&path) {
                Ok(()) => {
                    let runtime_path = path.join("runtime");
                    if create_runtime_directory(&runtime_path).is_err() {
                        let _ = fs::remove_dir_all(&path);
                        return Err(sandbox_error(
                            ErrorCode::Io,
                            "runtime temporary directory creation failed",
                        ));
                    }
                    return Ok(Self {
                        path,
                        runtime_path,
                        closed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => {
                    return Err(sandbox_error(
                        ErrorCode::Io,
                        "backend temporary directory could not be created",
                    ));
                }
            }
        }
        Err(sandbox_error(
            ErrorCode::Io,
            "backend temporary directory allocation was exhausted",
        ))
    }

    pub(crate) fn runtime_path(&self) -> &Path {
        &self.runtime_path
    }

    pub(crate) fn cidfile(&self) -> PathBuf {
        self.path.join("container.id")
    }

    pub(crate) fn write_environment(
        &self,
        environment: &BTreeMap<String, String>,
    ) -> Result<PathBuf, OpenWorkError> {
        let path = self.path.join("container.env");
        let mut file = create_private_file(&path)
            .map_err(|_| sandbox_error(ErrorCode::Io, "container environment file failed"))?;
        for (key, value) in environment {
            if value.contains(['\r', '\n']) {
                return Err(sandbox_error(
                    ErrorCode::InvalidArguments,
                    "container environment contains an unsupported newline",
                ));
            }
            writeln!(file, "{key}={value}")
                .map_err(|_| sandbox_error(ErrorCode::Io, "container environment file failed"))?;
        }
        Ok(path)
    }

    pub(crate) fn read_container_id(&self) -> Result<String, OpenWorkError> {
        parse_container_id(&fs::read(self.cidfile()).map_err(|_| {
            sandbox_error(
                ErrorCode::ExecutionFailed,
                "Docker container ID was unavailable",
            )
        })?)
    }

    pub(crate) fn close(mut self) -> Result<(), OpenWorkError> {
        fs::remove_dir_all(&self.path).map_err(|_| {
            sandbox_error(ErrorCode::Io, "backend temporary directory cleanup failed")
        })?;
        self.closed = true;
        Ok(())
    }
}

impl Drop for OwnedTemporaryDirectory {
    fn drop(&mut self) {
        if !self.closed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) fn mount_argument(
    source: &Path,
    target: &str,
    read_only: bool,
) -> Result<OsString, OpenWorkError> {
    let source = source.to_str().ok_or_else(|| {
        sandbox_error(
            ErrorCode::InvalidArguments,
            "sandbox mount path is not UTF-8",
        )
    })?;
    if source.contains(',') {
        return Err(sandbox_error(
            ErrorCode::InvalidArguments,
            "sandbox mount path contains an unsupported delimiter",
        ));
    }
    Ok(OsString::from(format!(
        "type=bind,src={source},dst={target}{}",
        if read_only { ",readonly" } else { "" }
    )))
}

pub(crate) fn validate_mount(path: &Path) -> Result<(), OpenWorkError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| sandbox_error(ErrorCode::InvalidArguments, "sandbox mount is unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(sandbox_error(
            ErrorCode::InvalidArguments,
            "sandbox mount must be a real directory",
        ));
    }
    Ok(())
}

pub(crate) fn collect_output_paths(
    root: &Path,
) -> Result<Vec<RelativeArtifactPath>, OpenWorkError> {
    let mut output = Vec::new();
    let mut directories = vec![(root.to_path_buf(), 0_u16)];
    let mut entries_seen = 0_u32;
    while let Some((directory, depth)) = directories.pop() {
        for entry in
            fs::read_dir(directory).map_err(|_| artifact_error("output cannot be scanned"))?
        {
            entries_seen = entries_seen.saturating_add(1);
            if entries_seen > MAX_OUTPUT_ENTRIES {
                return Err(artifact_error("sandbox output tree is too large"));
            }
            let entry = entry.map_err(|_| artifact_error("output entry is invalid"))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| artifact_error("output metadata is invalid"))?;
            if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
                return Err(artifact_error("output contains a symlink or special file"));
            }
            if metadata.is_dir() {
                let child_depth = depth.saturating_add(1);
                if child_depth > MAX_OUTPUT_DEPTH {
                    return Err(artifact_error("sandbox output tree is too deep"));
                }
                directories.push((path, child_depth));
            } else {
                if output.len() >= MAX_OUTPUT_FILES {
                    return Err(artifact_error("sandbox produced too many files"));
                }
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| artifact_error("sandbox output escaped its root"))?;
                let portable = relative
                    .to_str()
                    .ok_or_else(|| artifact_error("output path is not UTF-8"))?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                output.push(RelativeArtifactPath::parse(portable)?);
            }
        }
    }
    output.sort();
    Ok(output)
}

fn parse_container_id(bytes: &[u8]) -> Result<String, OpenWorkError> {
    let id = String::from_utf8_lossy(bytes).trim().to_owned();
    let valid = id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(id)
    } else {
        Err(sandbox_error(
            ErrorCode::ExecutionFailed,
            "Docker did not return a valid container ID",
        ))
    }
}

fn artifact_error(message: &'static str) -> OpenWorkError {
    sandbox_error(ErrorCode::ArtifactInvalid, message)
}

fn ensure_private_storage_supported() -> Result<(), OpenWorkError> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(sandbox_error(
            ErrorCode::SandboxUnavailable,
            "secure sandbox temporary storage is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owner-only directory creation is unsupported",
    ))
}

#[cfg(unix)]
fn create_runtime_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::PermissionsExt;

    fs::DirBuilder::new().mode(0o777).create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o777))
}

#[cfg(not(unix))]
fn create_runtime_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "runtime directory creation is unsupported",
    ))
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owner-only file creation is unsupported",
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_OUTPUT_DEPTH, MAX_OUTPUT_ENTRIES, OwnedTemporaryDirectory, collect_output_paths,
    };
    use openwork_core::ErrorCode;
    use std::fs;

    #[test]
    fn output_scan_bounds_combined_files_and_directories() {
        let root = tempfile::tempdir().expect("temporary output root");
        for index in 0..MAX_OUTPUT_ENTRIES {
            fs::create_dir(root.path().join(format!("empty-{index}")))
                .expect("empty output directory");
        }
        fs::write(root.path().join("one-more-entry"), b"output").expect("output file");

        let error = collect_output_paths(root.path()).expect_err("entry limit must be enforced");

        assert_eq!(error.code, ErrorCode::ArtifactInvalid);
        assert!(error.message.contains("too large"));
    }

    #[test]
    fn output_scan_rejects_excessive_depth_without_recursion() {
        let root = tempfile::tempdir().expect("temporary output root");
        let mut directory = root.path().to_path_buf();
        for _ in 0..=MAX_OUTPUT_DEPTH {
            directory = directory.join("d");
            fs::create_dir(&directory).expect("nested output directory");
        }

        let error = collect_output_paths(root.path()).expect_err("depth limit must be enforced");

        assert_eq!(error.code, ErrorCode::ArtifactInvalid);
        assert!(error.message.contains("too deep"));
    }

    #[cfg(unix)]
    #[test]
    fn temporary_storage_is_private_at_creation() {
        use std::collections::BTreeMap;
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temporary backend root");
        let temporary = OwnedTemporaryDirectory::create(root.path()).expect("private temporary");
        let environment = temporary
            .write_environment(&BTreeMap::from([("LANG".to_owned(), "C".to_owned())]))
            .expect("private environment file");

        let directory_mode = fs::metadata(environment.parent().expect("temporary directory"))
            .expect("temporary metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(environment)
            .expect("environment metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }

    #[cfg(not(unix))]
    #[test]
    fn temporary_storage_fails_closed_before_creating_data() {
        let root = tempfile::tempdir().expect("temporary backend root");

        let error = OwnedTemporaryDirectory::create(root.path())
            .expect_err("platform without owner-only ACL support must fail");

        assert_eq!(error.code, ErrorCode::SandboxUnavailable);
        assert_eq!(fs::read_dir(root.path()).expect("backend root").count(), 0);
    }
}
