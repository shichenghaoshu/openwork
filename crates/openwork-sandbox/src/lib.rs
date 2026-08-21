//! Ephemeral, fail-closed container sandbox backend for M1 safe execution.

mod cli;
mod engine;
mod filesystem;

pub use cli::DockerCli as ContainerCli;
pub use cli::{CliOutput, DockerCli, SystemDockerCli, SystemPodmanCli};
pub use engine::{
    CapabilitySupport, ContainerEngine, ContainerEngineCapabilities, ContainerEngineHealth,
    ContainerEngineKind, ContainerEngineStatus, DockerEngine, PodmanEngine,
};

use filesystem::{OwnedTemporaryDirectory, collect_output_paths, mount_argument, validate_mount};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::{
    EXECUTION_SCHEMA_VERSION, RunId, SandboxBackend, SandboxCleanupStatus, SandboxRequest,
    SandboxResult, SandboxTermination, UtcTimestamp,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CONTROL_OUTPUT_LIMIT: u64 = 64 * 1024;
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HEALTH_NOT_CHECKED: u8 = 0;
const HEALTH_AVAILABLE: u8 = 1;
const HEALTH_UNAVAILABLE: u8 = 2;

#[derive(Debug)]
struct ActiveContainer {
    id: String,
    cancellation_confirmed: Mutex<bool>,
}

/// Host-side backend that creates one disposable, networkless container per request.
pub struct ContainerSandbox<C: DockerCli, E: ContainerEngine> {
    cli: Arc<C>,
    engine: E,
    temporary_root: PathBuf,
    active: Arc<Mutex<BTreeMap<String, Arc<ActiveContainer>>>>,
    poll_interval: Duration,
    health: AtomicU8,
}

/// Backwards-compatible Docker sandbox name.
pub type DockerSandbox<C> = ContainerSandbox<C, DockerEngine>;

/// Podman-backed sandbox using the same lifecycle and security policy as Docker.
pub type PodmanSandbox<C> = ContainerSandbox<C, PodmanEngine>;

impl<C: DockerCli> ContainerSandbox<C, DockerEngine> {
    /// Creates a backend. `temporary_root` must be a backend-owned existing directory.
    #[must_use]
    pub fn new(cli: Arc<C>, temporary_root: PathBuf) -> Self {
        Self::for_engine(cli, temporary_root, DockerEngine)
    }
}

impl<C: DockerCli> ContainerSandbox<C, PodmanEngine> {
    /// Creates a Podman backend. `temporary_root` must be a backend-owned existing directory.
    #[must_use]
    pub fn new(cli: Arc<C>, temporary_root: PathBuf) -> Self {
        Self::for_engine(cli, temporary_root, PodmanEngine)
    }
}

impl<C: DockerCli, E: ContainerEngine> ContainerSandbox<C, E> {
    fn for_engine(cli: Arc<C>, temporary_root: PathBuf, engine: E) -> Self {
        Self {
            cli,
            engine,
            temporary_root,
            active: Arc::new(Mutex::new(BTreeMap::new())),
            poll_interval: DEFAULT_POLL_INTERVAL,
            health: AtomicU8::new(HEALTH_NOT_CHECKED),
        }
    }

    /// Reports adapter capabilities and the result of the most recent health probe.
    #[must_use]
    pub fn engine_status(&self) -> ContainerEngineStatus {
        let health = match self.health.load(Ordering::Acquire) {
            HEALTH_AVAILABLE => ContainerEngineHealth::Available,
            HEALTH_UNAVAILABLE => ContainerEngineHealth::Unavailable,
            _ => ContainerEngineHealth::NotChecked,
        };
        ContainerEngineStatus {
            kind: self.engine.kind(),
            capabilities: self.engine.capabilities(),
            health,
        }
    }

    /// Overrides polling for deterministic tests.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    fn invoke(&self, args: &[OsString], limit: u64) -> Result<CliOutput, OpenWorkError> {
        self.cli.run(args, limit, CLI_TIMEOUT, &[])
    }

    fn execute_inner(&self, request: &SandboxRequest) -> Result<SandboxResult, OpenWorkError> {
        validate_mount(request.input_directory.as_path())?;
        validate_mount(request.output_directory.as_path())?;
        let temporary = OwnedTemporaryDirectory::create(&self.temporary_root)?;
        let environment_file = temporary.write_environment(request.command.environment())?;
        let create_args = create_arguments(self.engine, request, &temporary, environment_file)?;
        let create = self.invoke(&create_args, CONTROL_OUTPUT_LIMIT);
        let container_id = match temporary.read_container_id() {
            Ok(id) => id,
            Err(error) => return Err(create.err().unwrap_or(error)),
        };
        let mut guard =
            ContainerGuard::new(Arc::clone(&self.cli), self.engine, container_id.clone());
        match create {
            Ok(output) if output.success => {}
            Ok(_) => {
                return Err(sandbox_error(
                    ErrorCode::ExecutionFailed,
                    "Container creation failed",
                ));
            }
            Err(error) => return Err(error),
        }
        let active = Arc::new(ActiveContainer {
            id: container_id.clone(),
            cancellation_confirmed: Mutex::new(false),
        });
        let key = run_key(&request.run_id)?;
        let _registration =
            ActiveRegistration::insert(Arc::clone(&self.active), key, Arc::clone(&active))?;
        let started_at = UtcTimestamp::now();
        let (mut termination, mut exit_code) =
            self.run_container(&container_id, &active, request)?;
        let logs = self
            .invoke(
                &self.engine.logs_arguments(&container_id),
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
        // A cancelled terminal result is a confirmation claim.  Do not emit it
        // when disposal was not proven: an operator must be able to distinguish
        // "kill was requested" from a sandbox that may still be reachable.
        if matches!(termination, SandboxTermination::Cancelled)
            && !matches!(cleanup, SandboxCleanupStatus::Succeeded)
        {
            termination = SandboxTermination::Failed;
            exit_code = None;
        }
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

    fn run_container(
        &self,
        container_id: &str,
        active: &ActiveContainer,
        request: &SandboxRequest,
    ) -> Result<(SandboxTermination, Option<i32>), OpenWorkError> {
        let start = self.invoke(
            &self.engine.start_arguments(container_id),
            CONTROL_OUTPUT_LIMIT,
        )?;
        if !start.success {
            return Ok((SandboxTermination::Failed, None));
        }
        let deadline = Instant::now() + Duration::from_secs(request.limits.timeout_seconds());
        if request.command.stdin().is_empty() {
            return Ok(self.monitor_container(container_id, active, deadline, None));
        }
        Ok(thread::scope(|scope| {
            let (attachment_sender, attachment_receiver) = std::sync::mpsc::sync_channel(1);
            let attachment_timeout =
                Duration::from_secs(request.limits.timeout_seconds()).saturating_add(CLI_TIMEOUT);
            let attachment_arguments = self.engine.attach_arguments(container_id);
            let cli = Arc::clone(&self.cli);
            let stdin = request.command.stdin();
            let attachment = scope.spawn(move || {
                let output = cli.run(
                    &attachment_arguments,
                    CONTROL_OUTPUT_LIMIT,
                    attachment_timeout,
                    stdin,
                );
                let _ = attachment_sender.send(output);
            });
            let outcome =
                self.monitor_container(container_id, active, deadline, Some(&attachment_receiver));
            let _ = attachment.join();
            outcome
        }))
    }

    fn inspect(&self, container_id: &str) -> Result<ContainerState, OpenWorkError> {
        let output = self.invoke(
            &self.engine.inspect_arguments(container_id),
            CONTROL_OUTPUT_LIMIT,
        )?;
        if !output.success {
            return Err(sandbox_error(
                ErrorCode::ExecutionFailed,
                "Container inspection failed",
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            sandbox_error(
                ErrorCode::ExecutionFailed,
                "Container engine returned invalid state",
            )
        })?;
        Ok(ContainerState {
            running: state_bool(&value, "Running")?,
            oom_killed: state_bool(&value, "OOMKilled")?,
            exit_code: value
                .get("ExitCode")
                .and_then(serde_json::Value::as_i64)
                .and_then(|code| i32::try_from(code).ok())
                .ok_or_else(|| {
                    sandbox_error(
                        ErrorCode::ExecutionFailed,
                        "Container state omitted exit code",
                    )
                })?,
        })
    }

    fn monitor_container(
        &self,
        container_id: &str,
        active: &ActiveContainer,
        deadline: Instant,
        attachment: Option<&Receiver<Result<CliOutput, OpenWorkError>>>,
    ) -> (SandboxTermination, Option<i32>) {
        let mut attachment_failed = false;
        loop {
            // Hold the cancellation lock across inspection and every competing
            // kill decision.  A successful cancel therefore cannot stop the
            // container before its confirmation becomes visible to this
            // monitor.
            let Ok(cancellation_confirmed) = active.cancellation_confirmed.lock() else {
                return (SandboxTermination::Failed, None);
            };
            if *cancellation_confirmed {
                return (SandboxTermination::Cancelled, None);
            }
            if Instant::now() >= deadline {
                return if self.kill(container_id) {
                    (SandboxTermination::TimedOut, None)
                } else {
                    (SandboxTermination::Failed, None)
                };
            }
            if let Some(attachment) = attachment {
                match attachment.try_recv() {
                    Ok(Ok(output)) => attachment_failed |= !output.success,
                    Ok(Err(_)) | Err(TryRecvError::Disconnected) => attachment_failed = true,
                    Err(TryRecvError::Empty) => {}
                }
            }
            match self.inspect(container_id) {
                Ok(state) if state.running && attachment_failed => {
                    let _ = self.invoke(
                        &self.engine.kill_arguments(container_id),
                        CONTROL_OUTPUT_LIMIT,
                    );
                    return (SandboxTermination::Failed, None);
                }
                Ok(state) if state.running => {
                    drop(cancellation_confirmed);
                    thread::sleep(self.poll_interval);
                }
                Ok(state) if state.oom_killed => {
                    return (SandboxTermination::OutOfMemory, None);
                }
                Ok(state) => {
                    return (SandboxTermination::Exited, Some(state.exit_code));
                }
                Err(_) => {
                    let _ = self.invoke(
                        &self.engine.kill_arguments(container_id),
                        CONTROL_OUTPUT_LIMIT,
                    );
                    return (SandboxTermination::Failed, None);
                }
            }
        }
    }

    fn kill(&self, container_id: &str) -> bool {
        self.invoke(
            &self.engine.kill_arguments(container_id),
            CONTROL_OUTPUT_LIMIT,
        )
        .is_ok_and(|output| output.success)
    }
}

impl<C: DockerCli, E: ContainerEngine> SandboxBackend for ContainerSandbox<C, E> {
    fn health(&self) -> Result<(), OpenWorkError> {
        match self.invoke(&self.engine.health_arguments(), CONTROL_OUTPUT_LIMIT) {
            Ok(output) if output.success && !output.stdout.is_empty() => {
                self.health.store(HEALTH_AVAILABLE, Ordering::Release);
                Ok(())
            }
            Ok(_) => {
                self.health.store(HEALTH_UNAVAILABLE, Ordering::Release);
                Err(sandbox_error(
                    ErrorCode::SandboxUnavailable,
                    self.engine.unavailable_message(),
                ))
            }
            Err(error) => {
                self.health.store(HEALTH_UNAVAILABLE, Ordering::Release);
                Err(error)
            }
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
        let mut cancellation_confirmed = active
            .cancellation_confirmed
            .lock()
            .map_err(|_| registry_error())?;
        if *cancellation_confirmed {
            return Ok(());
        }
        let output = self.invoke(
            &self.engine.kill_arguments(&active.id),
            CONTROL_OUTPUT_LIMIT,
        )?;
        if output.success {
            // The monitor cannot inspect the stopped container until this
            // confirmation is published because both operations use the same
            // lock.
            *cancellation_confirmed = true;
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
        let mut guard = ContainerGuard::new(Arc::clone(&self.cli), self.engine, active.id.clone());
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

struct ContainerGuard<C: DockerCli, E: ContainerEngine> {
    cli: Arc<C>,
    engine: E,
    container_id: String,
    cleaned: bool,
}

impl<C: DockerCli, E: ContainerEngine> ContainerGuard<C, E> {
    fn new(cli: Arc<C>, engine: E, container_id: String) -> Self {
        Self {
            cli,
            engine,
            container_id,
            cleaned: false,
        }
    }

    fn cleanup(&mut self) -> SandboxCleanupStatus {
        let _ = self.cli.run(
            &self.engine.kill_arguments(&self.container_id),
            CONTROL_OUTPUT_LIMIT,
            CLI_TIMEOUT,
            &[],
        );
        let removed = self.cli.run(
            &self.engine.remove_arguments(&self.container_id),
            CONTROL_OUTPUT_LIMIT,
            CLI_TIMEOUT,
            &[],
        );
        self.cleaned = removed.as_ref().is_ok_and(|output| output.success);
        if self.cleaned {
            SandboxCleanupStatus::Succeeded
        } else {
            SandboxCleanupStatus::Failed {
                error_code: self.engine.remove_failure_code().to_owned(),
            }
        }
    }
}

impl<C: DockerCli, E: ContainerEngine> Drop for ContainerGuard<C, E> {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

fn create_arguments<E: ContainerEngine>(
    engine: E,
    request: &SandboxRequest,
    temporary: &OwnedTemporaryDirectory,
    environment_file: PathBuf,
) -> Result<Vec<OsString>, OpenWorkError> {
    let mut args = engine.create_arguments();
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
    if !request.command.stdin().is_empty() {
        args.push(OsString::from("-i"));
    }
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

fn registry_error() -> OpenWorkError {
    sandbox_error(ErrorCode::Internal, "sandbox registry lock poisoned")
}

pub(crate) fn sandbox_error(code: ErrorCode, message: &'static str) -> OpenWorkError {
    OpenWorkError::new(code, message)
}
