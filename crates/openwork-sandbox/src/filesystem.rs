use crate::sandbox_error;
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::RelativeArtifactPath;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct OwnedTemporaryDirectory {
    path: PathBuf,
    runtime_path: PathBuf,
    closed: bool,
}

impl OwnedTemporaryDirectory {
    pub(crate) fn create(root: &Path) -> Result<Self, OpenWorkError> {
        validate_mount(root)?;
        for _ in 0..32 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("openwork-{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    set_private_permissions(&path)?;
                    let runtime_path = path.join("runtime");
                    fs::create_dir(&runtime_path).map_err(|_| {
                        sandbox_error(ErrorCode::Io, "runtime temporary directory creation failed")
                    })?;
                    set_runtime_permissions(&runtime_path)?;
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
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| sandbox_error(ErrorCode::Io, "container environment file failed"))?;
        set_file_permissions(&path)?;
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
    let mut directories = vec![root.to_path_buf()];
    let mut entries_seen = 0_u32;
    while let Some(directory) = directories.pop() {
        for entry in
            fs::read_dir(directory).map_err(|_| artifact_error("output cannot be scanned"))?
        {
            entries_seen = entries_seen.saturating_add(1);
            if entries_seen > 4096 {
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
                directories.push(path);
            } else {
                if output.len() >= 1024 {
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

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), OpenWorkError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| sandbox_error(ErrorCode::Io, "temporary permissions failed"))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), OpenWorkError> {
    Ok(())
}

#[cfg(unix)]
fn set_runtime_permissions(path: &Path) -> Result<(), OpenWorkError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o777))
        .map_err(|_| sandbox_error(ErrorCode::Io, "runtime permissions failed"))
}

#[cfg(not(unix))]
fn set_runtime_permissions(_path: &Path) -> Result<(), OpenWorkError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), OpenWorkError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| sandbox_error(ErrorCode::Io, "environment permissions failed"))
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<(), OpenWorkError> {
    Ok(())
}
