//! Process-local production worker wiring for the Control API binary.

use super::{ConfigError, DeliveredPrompt, PromptDeliveryError, SharedPromptBoundary};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::store::{RunLease, RunQueueRepository, postgres::PostgresExecutionStore};
use openwork_execution::{
    ActorId, ApprovedMountDirectory, DEFAULT_MAX_ARTIFACT_BYTES, DigestPinnedImageRef, RunStatus,
    SandboxBackend, SandboxLimits, SandboxNetworkName, SandboxNetworkPolicy, SandboxUser,
    SandboxWorkingDirectory, UtcTimestamp,
};
use openwork_runtime::task::{
    CLAUDE_RUNTIME_ID, CODEX_RUNTIME_ID, ClaudeTaskAdapter, CodexTaskAdapter,
};
use openwork_sandbox::{DockerSandbox, SystemDockerCli};
use openwork_worker::{
    OneTimePrompt, SingleRunWorker, StartDisposition, SupervisorConfig, WorkerEnvironment,
    WorkerOutcome, WorkerTaskSpec,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const IDLE_POLL: Duration = Duration::from_millis(250);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const LEASE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct ProviderConfig {
    image: DigestPinnedImageRef,
    executable: String,
}

/// Explicit configuration for the in-process worker. When it is absent, run
/// creation remains unavailable rather than manufacturing unowned queued work.
#[derive(Clone)]
pub(crate) struct WorkerRuntimeConfig {
    docker_bin: PathBuf,
    docker_host: Option<OsString>,
    temporary_root: PathBuf,
    output_root: PathBuf,
    actor: ActorId,
    provider_network: SandboxNetworkName,
    providers: BTreeMap<String, ProviderConfig>,
}

impl WorkerRuntimeConfig {
    pub(crate) fn from_env() -> Result<Option<Self>, ConfigError> {
        let enabled = match std::env::var("OPENWORK_WORKER_ENABLED").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("0") => false,
            Ok("1") => true,
            _ => return Err(ConfigError("OPENWORK_WORKER_ENABLED must be 0 or 1")),
        };
        if !enabled {
            return Ok(None);
        }

        let docker_bin = required_path("OPENWORK_DOCKER_BIN")?;
        if !docker_bin.is_absolute() {
            return Err(ConfigError("OPENWORK_DOCKER_BIN must be absolute"));
        }
        let temporary_root = canonical_directory("OPENWORK_WORKER_TEMP_ROOT")?;
        let output_root = canonical_directory("OPENWORK_WORKER_OUTPUT_ROOT")?;
        let actor = ActorId::parse(
            std::env::var("OPENWORK_WORKER_ACTOR").unwrap_or_else(|_| "worker:local".to_owned()),
        )
        .map_err(|_| ConfigError("OPENWORK_WORKER_ACTOR is invalid"))?;
        let provider_network = SandboxNetworkName::parse(
            std::env::var("OPENWORK_PROVIDER_NETWORK")
                .map_err(|_| ConfigError("OPENWORK_PROVIDER_NETWORK is required"))?,
        )
        .map_err(|_| ConfigError("OPENWORK_PROVIDER_NETWORK is invalid"))?;
        let mut providers = BTreeMap::new();
        register_provider(
            &mut providers,
            CODEX_RUNTIME_ID,
            "OPENWORK_CODEX_IMAGE",
            "OPENWORK_CODEX_EXECUTABLE",
            "/usr/local/bin/codex",
        )?;
        register_provider(
            &mut providers,
            CLAUDE_RUNTIME_ID,
            "OPENWORK_CLAUDE_IMAGE",
            "OPENWORK_CLAUDE_EXECUTABLE",
            "/usr/local/bin/claude",
        )?;
        if providers.is_empty() {
            return Err(ConfigError(
                "worker requires OPENWORK_CODEX_IMAGE or OPENWORK_CLAUDE_IMAGE",
            ));
        }
        Ok(Some(Self {
            docker_bin,
            docker_host: std::env::var_os("OPENWORK_DOCKER_HOST").filter(|value| !value.is_empty()),
            temporary_root,
            output_root,
            actor,
            provider_network,
            providers,
        }))
    }

    pub(crate) fn runtimes(&self) -> BTreeSet<String> {
        self.providers.keys().cloned().collect()
    }
}

fn required_path(name: &'static str) -> Result<PathBuf, ConfigError> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(ConfigError(name))
}

fn canonical_directory(name: &'static str) -> Result<PathBuf, ConfigError> {
    let path = required_path(name)?;
    let canonical = fs::canonicalize(path).map_err(|_| ConfigError(name))?;
    if !fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(ConfigError(name));
    }
    Ok(canonical)
}

fn register_provider(
    providers: &mut BTreeMap<String, ProviderConfig>,
    runtime: &str,
    image_env: &'static str,
    executable_env: &'static str,
    default_executable: &str,
) -> Result<(), ConfigError> {
    let Some(raw_image) = std::env::var(image_env)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let image = DigestPinnedImageRef::parse(raw_image)
        .map_err(|_| ConfigError("worker provider image must be digest-pinned"))?;
    let executable = std::env::var(executable_env)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_executable.to_owned());
    if !executable.starts_with('/') {
        return Err(ConfigError(
            "worker provider executable must be an absolute container path",
        ));
    }
    providers.insert(runtime.to_owned(), ProviderConfig { image, executable });
    Ok(())
}

/// Owns one worker thread and stops it when the HTTP server exits.
pub(crate) struct WorkerRuntime {
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    runtimes: BTreeSet<String>,
}

impl WorkerRuntime {
    pub(crate) fn start(
        config: WorkerRuntimeConfig,
        store: PostgresExecutionStore,
        prompts: Arc<SharedPromptBoundary>,
        workspace_root: PathBuf,
    ) -> Result<Self, OpenWorkError> {
        let mut cli = SystemDockerCli::new(config.docker_bin.clone())?;
        if let Some(docker_host) = &config.docker_host {
            cli = cli.with_cli_environment(OsString::from("DOCKER_HOST"), docker_host.clone());
        }
        let sandbox = Arc::new(DockerSandbox::new(
            Arc::new(cli),
            config.temporary_root.clone(),
        ));
        sandbox.health()?;
        let runtimes = config.runtimes();
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(true));
        let worker_stop = Arc::clone(&stop);
        let worker_ready = Arc::clone(&ready);
        let worker = thread::Builder::new()
            .name("openwork-worker".to_owned())
            .spawn(move || {
                let _readiness = ReadinessGuard(worker_ready);
                run_loop(
                    &config,
                    &store,
                    prompts.as_ref(),
                    &workspace_root,
                    sandbox.as_ref(),
                    worker_stop.as_ref(),
                );
            })
            .map_err(|_| worker_error("worker thread could not start"))?;
        Ok(Self {
            stop,
            ready,
            worker: Some(worker),
            runtimes,
        })
    }

    pub(crate) fn runtimes(&self) -> &BTreeSet<String> {
        &self.runtimes
    }

    pub(crate) fn readiness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ready)
    }
}

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        self.ready.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

struct ReadinessGuard(Arc<AtomicBool>);

impl Drop for ReadinessGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn run_loop(
    config: &WorkerRuntimeConfig,
    store: &PostgresExecutionStore,
    prompts: &SharedPromptBoundary,
    workspace_root: &Path,
    sandbox: &DockerSandbox<SystemDockerCli>,
    stop: &AtomicBool,
) {
    let mut next_recovery = Instant::now();
    while !stop.load(Ordering::Acquire) {
        if Instant::now() >= next_recovery {
            if let Err(error) =
                store.recover_expired_leases(config.actor.clone(), UtcTimestamp::now())
            {
                eprintln!("worker lease recovery failed: {:?}", error.code);
            }
            next_recovery = Instant::now() + HEARTBEAT_INTERVAL;
        }
        match claim_with_prompt(config, store, prompts) {
            Ok(Some((lease, prompt))) => {
                execute_claim(config, store, workspace_root, sandbox, &lease, prompt);
            }
            Ok(None) => thread::park_timeout(IDLE_POLL),
            Err(error) => {
                eprintln!("worker queue operation failed: {:?}", error.code);
                thread::park_timeout(IDLE_POLL);
            }
        }
    }
}

fn claim_with_prompt(
    config: &WorkerRuntimeConfig,
    store: &PostgresExecutionStore,
    prompts: &SharedPromptBoundary,
) -> Result<Option<(RunLease, DeliveredPrompt)>, OpenWorkError> {
    let now = UtcTimestamp::now();
    let claimed = prompts
        .with_worker_claim(|delivery| {
            let Some(lease) =
                store.claim_next_run(config.actor.clone(), now, time::Duration::seconds(30))?
            else {
                delivery.purge_expired(now);
                return Ok(None);
            };
            match delivery.take_prompt(&lease.run.id, &lease.run.prompt_sha256, now) {
                Ok(prompt) => Ok(Some((lease, prompt))),
                Err(_error) => {
                    store.complete_leased_run(
                        &lease,
                        lease.run.revision,
                        RunStatus::Failed,
                        Some("sensitive run input is unavailable"),
                        UtcTimestamp::now(),
                    )?;
                    Ok(None)
                }
            }
        })
        .map_err(prompt_error)??;
    Ok(claimed)
}

fn execute_claim(
    config: &WorkerRuntimeConfig,
    store: &PostgresExecutionStore,
    workspace_root: &Path,
    sandbox: &DockerSandbox<SystemDockerCli>,
    lease: &RunLease,
    prompt: DeliveredPrompt,
) {
    let run_id = lease.run.id.clone();
    let output = match create_output_directory(&config.output_root, &run_id.to_hyphenated()) {
        Ok(output) => output,
        Err(error) => {
            fail_claim(store, lease, "worker output directory is unavailable");
            eprintln!("worker run {run_id:?} failed: {:?}", error.code);
            return;
        }
    };
    let result = prepare_environment(config, workspace_root, lease, output.clone()).and_then(
        |(provider, environment)| {
            let task = WorkerTaskSpec {
                runtime: lease.run.runtime.clone(),
                prompt_sha256: lease.run.prompt_sha256.clone(),
                working_directory: SandboxWorkingDirectory::parse("/workspace")?,
                timeout_seconds: environment.limits.timeout_seconds(),
                capabilities: vec!["filesystem.read".to_owned(), "filesystem.write".to_owned()],
            };
            let supervisor = SupervisorConfig {
                heartbeat_interval: HEARTBEAT_INTERVAL,
                lease_ttl: LEASE_TTL,
            };
            let prompt = OneTimePrompt::new(prompt.into_string());
            match lease.run.runtime.as_str() {
                CODEX_RUNTIME_ID => SingleRunWorker::new(
                    store,
                    sandbox,
                    &CodexTaskAdapter::new(provider.executable.clone()),
                )
                .with_supervisor(supervisor)?
                .execute(
                    lease.clone(),
                    task,
                    prompt,
                    environment,
                    StartDisposition::Run,
                ),
                CLAUDE_RUNTIME_ID => SingleRunWorker::new(
                    store,
                    sandbox,
                    &ClaudeTaskAdapter::new(provider.executable.clone()),
                )
                .with_supervisor(supervisor)?
                .execute(
                    lease.clone(),
                    task,
                    prompt,
                    environment,
                    StartDisposition::Run,
                ),
                _ => Err(worker_error("claimed runtime is not configured")),
            }
        },
    );
    match result {
        Ok(WorkerOutcome::Completed(run)) if run.status == RunStatus::Succeeded => {}
        Ok(_) => remove_failed_output(&config.output_root, &output),
        Err(error) => {
            fail_claim(store, lease, "worker execution failed closed");
            eprintln!("worker run {run_id:?} failed: {:?}", error.code);
            remove_failed_output(&config.output_root, &output);
        }
    }
}

fn prepare_environment(
    config: &WorkerRuntimeConfig,
    workspace_root: &Path,
    lease: &RunLease,
    output: PathBuf,
) -> Result<(ProviderConfig, WorkerEnvironment), OpenWorkError> {
    let provider = config
        .providers
        .get(&lease.run.runtime)
        .cloned()
        .ok_or_else(|| worker_error("claimed runtime is not configured"))?;
    let input = ApprovedMountDirectory::under_root(&lease.run.workspace, workspace_root)?;
    let output_mount = ApprovedMountDirectory::under_root(&output, &config.output_root)?;
    Ok((
        provider.clone(),
        WorkerEnvironment {
            image: provider.image,
            user: SandboxUser::new(65_534, 65_534)?,
            input_directory: input,
            output_directory: output_mount,
            limits: SandboxLimits::new(1_000, 512 * 1024 * 1024, 128, 600, 4 * 1024 * 1024)?,
            network: SandboxNetworkPolicy::Restricted(config.provider_network.clone()),
            artifact_output_root: output,
            max_artifact_bytes: DEFAULT_MAX_ARTIFACT_BYTES,
        },
    ))
}

fn create_output_directory(root: &Path, run_id: &str) -> Result<PathBuf, OpenWorkError> {
    let output = root.join(run_id);
    fs::create_dir(&output)
        .map_err(|_| worker_error("worker output directory could not be created"))?;
    make_container_writable(&output)?;
    Ok(output)
}

#[cfg(unix)]
fn make_container_writable(path: &Path) -> Result<(), OpenWorkError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o1777))
        .map_err(|_| worker_error("worker output permissions could not be set"))
}

#[cfg(not(unix))]
fn make_container_writable(_path: &Path) -> Result<(), OpenWorkError> {
    Ok(())
}

fn remove_failed_output(root: &Path, output: &Path) {
    if output.parent() == Some(root) {
        let _ = fs::remove_dir_all(output);
    }
}

fn fail_claim(store: &PostgresExecutionStore, lease: &RunLease, reason: &'static str) {
    let _ = store.complete_leased_run(
        lease,
        lease.run.revision,
        RunStatus::Failed,
        Some(reason),
        UtcTimestamp::now(),
    );
}

fn prompt_error(_error: PromptDeliveryError) -> OpenWorkError {
    worker_error("sensitive run input boundary is unavailable")
}

fn worker_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ExecutionFailed, message)
}
