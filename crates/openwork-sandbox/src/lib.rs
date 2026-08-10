//! Ephemeral, fail-closed Docker sandbox backend for M1 safe execution.

mod cli;
mod filesystem;

pub use cli::{CliOutput, DockerCli, SystemDockerCli};

use filesystem::{OwnedTemporaryDirectory, collect_output_paths, mount_argument, validate_mount};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::{
    EXECUTION_SCHEMA_VERSION, RunId, SandboxBackend, SandboxCleanupStatus, SandboxRequest,
    SandboxResult, SandboxTermination, UtcTimestamp,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CONTROL_OUTPUT_LIMIT: u64 = 64 * 1024;
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct ActiveContainer {
    id: String,
    cancel_requested: AtomicBool,
}

/// Host-side backend that creates one disposable, networkless Docker container per request.
pub struct DockerSandbox<C: DockerCli> {
    cli: Arc<C>,
    temporary_root: PathBuf,
    active: Arc<Mutex<BTreeMap<String, Arc<ActiveContainer>>>>,
    poll_interval: Duration,
}

impl<C: DockerCli> DockerSandbox<C> {
    /// Creates a backend. `temporary_root` must be a backend-owned existing directory.
    #[must_use]
    pub fn new(cli: Arc<C>, temporary_root: PathBuf) -> Self {
        Self {
            cli,
            temporary_root,
            active: Arc::new(Mutex::new(BTreeMap::new())),
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// Overrides polling for deterministic tests.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    fn invoke(&self, args: &[OsString], limit: u64) -> Result<CliOutput, OpenWorkError> {
        self.cli.run(args, limit, CLI_TIMEOUT)
    }

    fn execute_inner(&self, request: &SandboxRequest) -> Result<SandboxResult, OpenWorkError> {
        validate_mount(request.input_directory.as_path())?;
        validate_mount(request.output_directory.as_path())?;
        let temporary = OwnedTemporaryDirectory::create(&self.temporary_root)?;
        let environment_file = temporary.write_environment(request.command.environment())?;
        let create_args = create_arguments(request, &temporary, environment_file)?;
        let create = self.invoke(&create_args, CONTROL_OUTPUT_LIMIT);
        let container_id = match temporary.read_container_id() {
            Ok(id) => id,
            Err(error) => return Err(create.err().unwrap_or(error)),
        };
        let mut guard = ContainerGuard::new(Arc::clone(&self.cli), container_id.clone());
        match create {
            Ok(output) if output.success => {}
            Ok(_) => {
                return Err(sandbox_error(
                    ErrorCode::ExecutionFailed,
                    "Docker container creation failed",
                ));
            }
            Err(error) => return Err(error),
        }
        let active = Arc::new(ActiveContainer {
            id: container_id.clone(),
            cancel_requested: AtomicBool::new(false),
        });
        let key = run_key(&request.run_id)?;
        let _registration =
            ActiveRegistration::insert(Arc::clone(&self.active), key, Arc::clone(&active))?;
        let started_at = UtcTimestamp::now();
        let start = self.invoke(&os_args(["start", &container_id]), CONTROL_OUTPUT_LIMIT)?;
        let (mut termination, mut exit_code) = (SandboxTermination::Failed, None);
        if start.success {
            let deadline = Instant::now() + Duration::from_secs(request.limits.timeout_seconds());
            loop {
                if active.cancel_requested.load(Ordering::Acquire) {
                    termination = SandboxTermination::Cancelled;
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = self.invoke(&os_args(["kill", &container_id]), CONTROL_OUTPUT_LIMIT);
                    termination = SandboxTermination::TimedOut;
                    break;
                }
                match self.inspect(&container_id) {
                    Ok(state) if state.running => thread::sleep(self.poll_interval),
                    Ok(state) if state.oom_killed => {
                        termination = SandboxTermination::OutOfMemory;
                        break;
                    }
                    Ok(state) => {
                        termination = SandboxTermination::Exited;
                        exit_code = Some(state.exit_code);
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        let logs = self
            .invoke(
                &os_args(["logs", &container_id]),
                request.limits.max_output_bytes(),
            )
            .unwrap_or_else(|_| CliOutput {
                success: false,
                stdout: Vec::new(),
                stderr: Vec::new(),
                truncated: false,
            });
        let cleanup = guard.cleanup();
        let output_paths =
            if let Ok(paths) = collect_output_paths(request.output_directory.as_path()) {
                paths
            } else {
                termination = SandboxTermination::Failed;
                exit_code = None;
                Vec::new()
            };
        let cleanup = combine_cleanup(cleanup, temporary.close());
        let (stdout, stdout_invalid) = decode_output(logs.stdout);
        let (stderr, stderr_invalid) = decode_output(logs.stderr);
        let result = SandboxResult {
            schema_version: EXECUTION_SCHEMA_VERSION,
            run_id: request.run_id.clone(),
            sandbox_id: container_id,
            termination,
            exit_code,
            stdout,
            stderr,
            truncated: logs.truncated || stdout_invalid || stderr_invalid,
            started_at,
            completed_at: UtcTimestamp::now(),
            output_paths,
            cleanup,
        };
        result.validate()?;
        Ok(result)
    }

    fn inspect(&self, container_id: &str) -> Result<ContainerState, OpenWorkError> {
        let output = self.invoke(
            &os_args(["inspect", "--format", "{{json .State}}", container_id]),
            CONTROL_OUTPUT_LIMIT,
        )?;
        if !output.success {
            return Err(sandbox_error(
                ErrorCode::ExecutionFailed,
                "Docker container inspection failed",
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            sandbox_error(ErrorCode::ExecutionFailed, "Docker returned invalid state")
        })?;
        Ok(ContainerState {
            running: state_bool(&value, "Running")?,
            oom_killed: state_bool(&value, "OOMKilled")?,
            exit_code: value
                .get("ExitCode")
                .and_then(serde_json::Value::as_i64)
                .and_then(|code| i32::try_from(code).ok())
                .ok_or_else(|| {
                    sandbox_error(ErrorCode::ExecutionFailed, "Docker state omitted exit code")
                })?,
        })
    }
}

impl<C: DockerCli> SandboxBackend for DockerSandbox<C> {
    fn health(&self) -> Result<(), OpenWorkError> {
        let output = self.invoke(
            &os_args(["version", "--format", "{{.Server.Version}}"]),
            CONTROL_OUTPUT_LIMIT,
        )?;
        if output.success && !output.stdout.is_empty() {
            Ok(())
        } else {
            Err(sandbox_error(
                ErrorCode::SandboxUnavailable,
                "Docker daemon is unavailable",
            ))
        }
    }

    fn execute(&self, request: &SandboxRequest) -> Result<SandboxResult, OpenWorkError> {
        self.execute_inner(request)
    }

    fn cancel(&self, run_id: &RunId) -> Result<(), OpenWorkError> {
        let key = run_key(run_id)?;
        let active = self
            .active
            .lock()
            .map_err(|_| registry_error())?
            .get(&key)
            .cloned()
            .ok_or_else(|| sandbox_error(ErrorCode::RunCancelled, "run has no active sandbox"))?;
        active.cancel_requested.store(true, Ordering::Release);
        let output = self.invoke(&os_args(["kill", &active.id]), CONTROL_OUTPUT_LIMIT)?;
        if output.success {
            Ok(())
        } else {
            Err(sandbox_error(
                ErrorCode::ExecutionFailed,
                "active sandbox could not be cancelled",
            ))
        }
    }

    fn cleanup(&self, run_id: &RunId) -> Result<(), OpenWorkError> {
        let key = run_key(run_id)?;
        let active = self
            .active
            .lock()
            .map_err(|_| registry_error())?
            .remove(&key)
            .ok_or_else(|| {
                sandbox_error(ErrorCode::ExecutionFailed, "run has no active sandbox")
            })?;
        let mut guard = ContainerGuard::new(Arc::clone(&self.cli), active.id.clone());
        match guard.cleanup() {
            SandboxCleanupStatus::Succeeded => Ok(()),
            SandboxCleanupStatus::Failed { .. } => Err(sandbox_error(
                ErrorCode::ExecutionFailed,
                "active sandbox cleanup failed",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ContainerState {
    running: bool,
    oom_killed: bool,
    exit_code: i32,
}

struct ActiveRegistration {
    active: Arc<Mutex<BTreeMap<String, Arc<ActiveContainer>>>>,
    key: String,
}

impl ActiveRegistration {
    fn insert(
        active: Arc<Mutex<BTreeMap<String, Arc<ActiveContainer>>>>,
        key: String,
        container: Arc<ActiveContainer>,
    ) -> Result<Self, OpenWorkError> {
        {
            use std::collections::btree_map::Entry;
            match active
                .lock()
                .map_err(|_| registry_error())?
                .entry(key.clone())
            {
                Entry::Vacant(entry) => {
                    entry.insert(container);
                }
                Entry::Occupied(_) => {
                    return Err(sandbox_error(
                        ErrorCode::InvalidStateTransition,
                        "run already has an active sandbox",
                    ));
                }
            }
        }
        Ok(Self { active, key })
    }
}

impl Drop for ActiveRegistration {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.key);
        }
    }
}

struct ContainerGuard<C: DockerCli> {
    cli: Arc<C>,
    container_id: String,
    cleaned: bool,
}

impl<C: DockerCli> ContainerGuard<C> {
    fn new(cli: Arc<C>, container_id: String) -> Self {
        Self {
            cli,
            container_id,
            cleaned: false,
        }
    }

    fn cleanup(&mut self) -> SandboxCleanupStatus {
        let _ = self.cli.run(
            &os_args(["kill", &self.container_id]),
            CONTROL_OUTPUT_LIMIT,
            CLI_TIMEOUT,
        );
        let removed = self.cli.run(
            &os_args(["rm", "--force", &self.container_id]),
            CONTROL_OUTPUT_LIMIT,
            CLI_TIMEOUT,
        );
        self.cleaned = removed.as_ref().is_ok_and(|output| output.success);
        if self.cleaned {
            SandboxCleanupStatus::Succeeded
        } else {
            SandboxCleanupStatus::Failed {
                error_code: "docker.remove_failed".to_owned(),
            }
        }
    }
}

impl<C: DockerCli> Drop for ContainerGuard<C> {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

fn create_arguments(
    request: &SandboxRequest,
    temporary: &OwnedTemporaryDirectory,
    environment_file: PathBuf,
) -> Result<Vec<OsString>, OpenWorkError> {
    let mut args = os_args(["create", "--network", "none", "--read-only"]);
    push_pair(&mut args, "--cap-drop", "ALL");
    push_pair(&mut args, "--security-opt", "no-new-privileges");
    push_pair(
        &mut args,
        "--user",
        format!("{}:{}", request.user.uid(), request.user.gid()),
    );
    push_pair(&mut args, "--cpu-period", "100000");
    push_pair(
        &mut args,
        "--cpu-quota",
        request.limits.cpu_millis().saturating_mul(100).to_string(),
    );
    push_pair(
        &mut args,
        "--memory",
        request.limits.memory_bytes().to_string(),
    );
    push_pair(
        &mut args,
        "--pids-limit",
        request.limits.pid_limit().to_string(),
    );
    push_pair(&mut args, "--workdir", "/workspace");
    push_pair(&mut args, "--cidfile", temporary.cidfile().into_os_string());
    push_pair(&mut args, "--env-file", environment_file.into_os_string());
    push_pair(
        &mut args,
        "--mount",
        mount_argument(request.input_directory.as_path(), "/workspace/input", true)?,
    );
    push_pair(
        &mut args,
        "--mount",
        mount_argument(
            request.output_directory.as_path(),
            "/workspace/output",
            false,
        )?,
    );
    push_pair(
        &mut args,
        "--mount",
        mount_argument(temporary.runtime_path(), "/workspace/tmp", false)?,
    );
    push_pair(
        &mut args,
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,nodev,size=67108864",
    );
    args.push(OsString::from(request.image.as_str()));
    args.push(OsString::from(request.command.program()));
    args.extend(request.command.arguments().iter().map(OsString::from));
    Ok(args)
}

fn push_pair(args: &mut Vec<OsString>, key: impl Into<OsString>, value: impl Into<OsString>) {
    args.push(key.into());
    args.push(value.into());
}

fn decode_output(bytes: Vec<u8>) -> (String, bool) {
    match String::from_utf8(bytes) {
        Ok(value) => (value, false),
        Err(_) => (String::new(), true),
    }
}

fn state_bool(value: &serde_json::Value, key: &str) -> Result<bool, OpenWorkError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| sandbox_error(ErrorCode::ExecutionFailed, "Docker state was incomplete"))
}

fn run_key(run_id: &RunId) -> Result<String, OpenWorkError> {
    serde_json::to_string(run_id)
        .map(|value| value.trim_matches('"').to_owned())
        .map_err(|_| sandbox_error(ErrorCode::Internal, "run ID could not be encoded"))
}

fn combine_cleanup(
    container: SandboxCleanupStatus,
    temporary: Result<(), OpenWorkError>,
) -> SandboxCleanupStatus {
    match (container, temporary) {
        (SandboxCleanupStatus::Succeeded, Ok(())) => SandboxCleanupStatus::Succeeded,
        (SandboxCleanupStatus::Failed { error_code }, _) => {
            SandboxCleanupStatus::Failed { error_code }
        }
        (_, Err(_)) => SandboxCleanupStatus::Failed {
            error_code: "temporary.remove_failed".to_owned(),
        },
    }
}

fn os_args<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

fn registry_error() -> OpenWorkError {
    sandbox_error(ErrorCode::Internal, "sandbox registry lock poisoned")
}

pub(crate) fn sandbox_error(code: ErrorCode, message: &'static str) -> OpenWorkError {
    OpenWorkError::new(code, message)
}
