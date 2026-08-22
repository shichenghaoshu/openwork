use clap::{Parser, Subcommand};
use openwork_config::{
    ChecksumAuthority, LockfileError, LockfileStore, RequestedRuntime, ResolvedRuntime,
    RuntimeChecksum, RuntimeInstallStatus, RuntimeLicense, RuntimeLockEntry, RuntimeLockfile,
    RuntimeSource, RuntimeSourceKind, RuntimeTimestamps, StoragePaths, UpstreamProvenance,
};
use openwork_core::{ErrorCode, OpenWorkError, PRODUCT_NAME};
use openwork_doctor::{CheckStatus, DoctorReport, inspect_platform};
use openwork_e2e::sales_demo::{SalesDemoConfig, SalesDemoReport, run_sales_demo};
use openwork_execution::{DigestPinnedImageRef, Run};
use openwork_installer::{
    ExecutionMode, InstallExecutionFailure, InstallExecutionReport, InstallExecutor, InstallPlan,
    StepResult, StepStatus, dry_run_plan, managed_runtime_plan,
};
use openwork_platform::{PlatformInfo, PlatformProbe, SystemPlatformProbe, detect};
use openwork_runtime::{
    AgentRuntime, AuthStatus, ClaudeRuntime, CodexRuntime, DetectionState, RuntimeCapabilities,
    RuntimeDetection, RuntimeId, RuntimeInstallPlan, RuntimeMetadata, RuntimeRegistry,
    SystemCommandRunner, SystemDownloader,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(
    name = "openwork",
    about = "Cross-platform Bootstrap runtime for OpenWork",
    disable_version_flag = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Preview a Bootstrap installation without changing the host.
    Install {
        #[arg(long, conflicts_with = "execute", required_unless_present = "execute")]
        dry_run: bool,
        /// Apply the reviewed plan. Requires an explicit consent flag.
        #[arg(long, conflicts_with = "dry_run", requires = "yes")]
        execute: bool,
        /// Confirm that the selected plan may modify managed `OpenWork` paths.
        #[arg(long, requires = "execute")]
        yes: bool,
        /// Include one external-managed runtime in the plan.
        #[arg(long)]
        runtime: Option<String>,
        /// Advisory upstream version or channel, when the adapter supports it.
        #[arg(long, requires = "runtime")]
        version: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show current host and Bootstrap state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Run structured host diagnostics.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Inspect registered agent runtimes.
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    /// Execute a task through a managed runtime in a safe sandbox.
    Run {
        /// Target AI runtime (claude-code, codex).
        #[arg(long, short)]
        runtime: String,
        /// Workspace directory containing input files.
        #[arg(long, short)]
        workspace: String,
        /// Natural-language task description (supplied via stdin to the provider).
        #[arg(long)]
        prompt: String,
        /// Wait for the run to complete before returning.
        #[arg(long)]
        wait: bool,
        /// Emit a structured error or result.
        #[arg(long)]
        json: bool,
    },
    /// Run a deterministic, evidence-producing product demo.
    Demo {
        #[command(subcommand)]
        command: DemoCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DemoCommand {
    /// Analyze the embedded July/August sales fixtures in a real container.
    Sales {
        /// Absolute Docker-compatible CLI path. Known Docker Desktop paths are auto-detected.
        #[arg(long)]
        engine_bin: Option<PathBuf>,
        /// Exact digest-pinned BusyBox-compatible image.
        #[arg(long, default_value = SALES_DEMO_IMAGE)]
        image: String,
        /// Root that retains the unique run output directory.
        #[arg(long, default_value = "openwork-demo-output")]
        output_root: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

const SALES_DEMO_IMAGE: &str = "docker.io/library/busybox@sha256:9db7b59979c38555a39def84a31fb98b5296952f9e3afd4f6f11f05b07adfab0";

#[derive(Debug, Subcommand)]
enum RuntimeCommand {
    /// List registered runtimes.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one runtime.
    Info {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct StatusReport {
    schema_version: u32,
    state: &'static str,
    platform: PlatformInfo,
    runtimes: Vec<RuntimeSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lockfile: Option<RuntimeLockfile>,
}

#[derive(Serialize)]
struct RuntimeSummary {
    metadata: RuntimeMetadata,
    detection: RuntimeDetection,
    version: Option<String>,
    auth: AuthStatus,
    capabilities: RuntimeCapabilities,
}

struct InstallRequest {
    execute: bool,
    runtime: Option<String>,
    version: Option<String>,
    json: bool,
}

struct PreparedRuntime {
    runtime: Arc<dyn AgentRuntime>,
    install_plan: RuntimeInstallPlan,
    requested: String,
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.len() == 2 && matches!(arguments[1].as_str(), "--version" | "-V") {
        println!("{PRODUCT_NAME} {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    match Cli::try_parse_from(&arguments) {
        Ok(cli) => run(cli, &SystemPlatformProbe),
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            let _ = error.print();
            ExitCode::from(code)
        }
    }
}

fn run(cli: Cli, probe: &impl PlatformProbe) -> ExitCode {
    match execute(cli, probe) {
        Ok(code) => ExitCode::from(code),
        Err((error, json)) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&error).unwrap_or_default()
                );
            } else {
                eprintln!("error[{:?}]: {error}", error.code);
            }
            ExitCode::from(error.exit_code())
        }
    }
}

fn execute(cli: Cli, probe: &impl PlatformProbe) -> Result<u8, (OpenWorkError, bool)> {
    match cli.command {
        Command::Install {
            dry_run: _,
            execute,
            yes: _,
            runtime,
            version,
            json,
        } => execute_install(
            probe,
            InstallRequest {
                execute,
                runtime,
                version,
                json,
            },
        ),
        Command::Status { json } => execute_status(probe, json),
        Command::Doctor { json } => {
            let report = inspect_platform(&platform(probe, json)?);
            render_doctor(&report, json);
            Ok(if report.has_failures() {
                ErrorCode::PreflightFailed.exit_code()
            } else {
                0
            })
        }
        Command::Runtime {
            command: RuntimeCommand::List { json },
        } => {
            let host = platform(probe, json)?;
            let runtimes = runtime_summaries(&runtime_registry(&host), json)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&runtimes).unwrap_or_default()
                );
            } else {
                for runtime in &runtimes {
                    println!(
                        "{}\t{:?}\t{}",
                        runtime.metadata.id,
                        runtime.detection.state,
                        runtime.version.as_deref().unwrap_or("unknown")
                    );
                }
            }
            Ok(0)
        }
        Command::Run {
            runtime,
            workspace,
            prompt,
            wait,
            json,
        } => execute_run(&runtime, &workspace, &prompt, wait, json),
        Command::Demo { command } => execute_demo(command),
        Command::Runtime {
            command: RuntimeCommand::Info { id, json },
        } => {
            let host = platform(probe, json)?;
            let registry = runtime_registry(&host);
            let runtime = registry.get(&RuntimeId::from(id.as_str())).ok_or_else(|| {
                (
                    OpenWorkError::new(
                        ErrorCode::RuntimeNotFound,
                        format!("runtime `{id}` is not registered"),
                    )
                    .with_remediation("Run `openwork runtime list` to see available runtimes."),
                    json,
                )
            })?;
            let summary = runtime_summary(runtime.as_ref()).map_err(|error| (error, json))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary).unwrap_or_default()
                );
            } else {
                println!("Runtime: {}", summary.metadata.name);
                println!("State: {:?}", summary.detection.state);
                println!(
                    "Version: {}",
                    summary.version.as_deref().unwrap_or("unknown")
                );
                println!("Auth: {:?}", summary.auth);
                println!("Distribution: {:?}", summary.metadata.distribution);
                if let Some(details) = &summary.detection.details {
                    println!("Details: {details}");
                }
            }
            Ok(0)
        }
    }
}

fn execute_status(probe: &impl PlatformProbe, json: bool) -> Result<u8, (OpenWorkError, bool)> {
    let host = platform(probe, json)?;
    let runtimes = runtime_summaries(&runtime_registry(&host), json)?;
    let lockfile = read_runtime_lockfile(&host).map_err(|error| (error, json))?;
    let report = StatusReport {
        schema_version: 1,
        state: if lockfile.is_some() {
            "installed"
        } else {
            "not_installed"
        },
        platform: host,
        runtimes,
        lockfile,
    };
    render_status(&report, json);
    Ok(0)
}

fn execute_install(
    probe: &impl PlatformProbe,
    request: InstallRequest,
) -> Result<u8, (OpenWorkError, bool)> {
    let host = platform(probe, request.json)?;
    let registry = runtime_registry(&host);
    let prepared_runtime = if let Some(requested_id) = request.runtime {
        let normalized_id = normalize_runtime_id(&requested_id);
        let runtime = registry
            .get(&RuntimeId::from(normalized_id))
            .ok_or_else(|| {
                (
                    OpenWorkError::new(
                        ErrorCode::RuntimeNotFound,
                        format!("runtime `{requested_id}` is not registered"),
                    )
                    .with_remediation("Run `openwork runtime list` to see available runtimes."),
                    request.json,
                )
            })?;
        let runtime_plan = runtime
            .install_plan(request.version.as_deref())
            .map_err(|error| (error, request.json))?;
        Some(PreparedRuntime {
            runtime,
            requested: request.version.unwrap_or_else(|| "latest".to_owned()),
            install_plan: runtime_plan,
        })
    } else {
        None
    };
    let plan = prepared_runtime.as_ref().map_or_else(
        || dry_run_plan(&host),
        |prepared| {
            managed_runtime_plan(
                &host,
                prepared.runtime.metadata().id.0.as_str(),
                &prepared.install_plan,
            )
        },
    );

    if !request.execute {
        render_install(&plan, request.json);
        return Ok(0);
    }
    ensure_runtime_is_missing(
        prepared_runtime
            .as_ref()
            .map(|prepared| prepared.runtime.as_ref()),
        request.json,
    )?;

    let downloader = SystemDownloader::new().map_err(|error| (error, request.json))?;
    let runner = SystemCommandRunner;
    match InstallExecutor::new(&downloader, &runner).run(
        &plan,
        ExecutionMode::Execute,
        &openwork_runtime::CancellationToken::new(),
    ) {
        Ok(mut report) => match persist_install_state(&host, prepared_runtime.as_ref(), &report) {
            Ok(()) => {
                render_install_execution(&report, request.json);
                Ok(0)
            }
            Err(error) => {
                report.completed = false;
                report.partial_state = true;
                report.steps.push(StepResult {
                    id: "state.runtime-lockfile".to_owned(),
                    status: StepStatus::Failed,
                    detail: error.message.clone(),
                    download_receipt: None,
                });
                let failure = InstallExecutionFailure { error, report };
                render_install_failure(&failure, request.json);
                Ok(failure.error.exit_code())
            }
        },
        Err(failure) => {
            render_install_failure(&failure, request.json);
            Ok(failure.error.exit_code())
        }
    }
}

fn ensure_runtime_is_missing(
    runtime: Option<&dyn AgentRuntime>,
    json: bool,
) -> Result<(), (OpenWorkError, bool)> {
    let Some(runtime) = runtime else {
        return Ok(());
    };
    let detection = runtime.detect().map_err(|error| (error, json))?;
    if detection.state == DetectionState::Missing {
        return Ok(());
    }
    Err((
        OpenWorkError::new(
            ErrorCode::InstallFailed,
            format!(
                "refusing to modify the existing `{}` runtime installation",
                runtime.metadata().id
            ),
        )
        .with_remediation(
            "Preserve the existing installation and use its official updater explicitly.",
        ),
        json,
    ))
}

fn read_runtime_lockfile(host: &PlatformInfo) -> Result<Option<RuntimeLockfile>, OpenWorkError> {
    let path = runtime_lockfile_path(host);
    if !path.exists() {
        return Ok(None);
    }
    LockfileStore::new(path)
        .read()
        .map(Some)
        .map_err(|error| lockfile_error(&error))
}

fn persist_install_state(
    host: &PlatformInfo,
    prepared: Option<&PreparedRuntime>,
    report: &InstallExecutionReport,
) -> Result<(), OpenWorkError> {
    let lockfile_path = runtime_lockfile_path(host);
    StoragePaths::new(
        host.paths.config.join("config.json"),
        lockfile_path.clone(),
        host.paths.data.join("secrets.json"),
    )
    .validate()
    .map_err(|error| lockfile_error(&error))?;

    let timestamp = current_timestamp();
    let runtime_entry = prepared
        .map(|prepared| resolved_runtime_entry(host, prepared, report, &timestamp))
        .transpose()?;
    let unhealthy = runtime_entry
        .as_ref()
        .is_some_and(|(_, entry)| entry.status != RuntimeInstallStatus::Installed);
    LockfileStore::new(lockfile_path)
        .update_or_create(RuntimeLockfile::empty(&timestamp), |lockfile| {
            lockfile.generated_at.clone_from(&timestamp);
            if let Some((runtime_id, entry)) = &runtime_entry {
                lockfile.runtimes.insert(runtime_id.clone(), entry.clone());
            }
            Ok(())
        })
        .map_err(|error| lockfile_error(&error))?;

    if unhealthy {
        return Err(OpenWorkError::new(
            ErrorCode::RuntimeUnhealthy,
            "the official installer exited successfully but the runtime did not become healthy",
        )
        .with_remediation(
            "The failed state was recorded; run `openwork runtime info <id>` and `openwork doctor` before retrying.",
        ));
    }
    Ok(())
}

fn resolved_runtime_entry(
    host: &PlatformInfo,
    prepared: &PreparedRuntime,
    report: &InstallExecutionReport,
    timestamp: &str,
) -> Result<(String, RuntimeLockEntry), OpenWorkError> {
    let metadata = prepared.runtime.metadata();
    let detection = prepared.runtime.detect()?;
    let healthy = detection.state == DetectionState::Healthy;
    let version = prepared
        .runtime
        .version()?
        .unwrap_or_else(|| "unknown".to_owned());
    let receipt = report
        .steps
        .iter()
        .find_map(|step| step.download_receipt.as_ref());
    let expected_digest = prepared
        .install_plan
        .downloads
        .first()
        .and_then(|download| download.expected_sha256.as_ref());
    let checksum = receipt.map_or(
        RuntimeChecksum {
            algorithm: "sha256".to_owned(),
            digest: None,
            authority: ChecksumAuthority::Unavailable,
            authority_url: None,
        },
        |receipt| RuntimeChecksum {
            algorithm: "sha256".to_owned(),
            digest: Some(receipt.observed_sha256.clone()),
            authority: if expected_digest.is_some() {
                ChecksumAuthority::Upstream
            } else {
                ChecksumAuthority::Observed
            },
            authority_url: expected_digest.map(|_| prepared.install_plan.source_url.clone()),
        },
    );
    let installed_path = detection.executable.unwrap_or_else(|| {
        host.paths
            .data
            .join("runtimes")
            .join(metadata.id.0.as_str())
    });
    let artifact = prepared
        .install_plan
        .downloads
        .first()
        .and_then(|download| download.destination.file_name())
        .map(|name| name.to_string_lossy().into_owned());
    let license = if metadata.license.contains("commercial") {
        "LicenseRef-Anthropic-Commercial".to_owned()
    } else {
        metadata.license.clone()
    };

    Ok((
        metadata.id.0,
        RuntimeLockEntry {
            requested: RequestedRuntime {
                constraint: prepared.requested.clone(),
                channel: None,
            },
            resolved: ResolvedRuntime { version, artifact },
            source: RuntimeSource {
                kind: RuntimeSourceKind::OfficialInstaller,
                uri: prepared.install_plan.source_url.clone(),
                reference: prepared.install_plan.version.clone(),
            },
            checksum,
            installed_path,
            timestamps: RuntimeTimestamps {
                created_at: timestamp.to_owned(),
                updated_at: timestamp.to_owned(),
                verified_at: healthy.then(|| timestamp.to_owned()),
            },
            status: if healthy {
                RuntimeInstallStatus::Installed
            } else {
                RuntimeInstallStatus::Failed
            },
            upstream: UpstreamProvenance {
                project_url: metadata.upstream,
                release_url: None,
                revision: prepared.install_plan.version.clone(),
            },
            license: RuntimeLicense {
                spdx: license,
                url: None,
            },
        },
    ))
}

fn execute_run(
    runtime: &str,
    workspace: &str,
    prompt: &str,
    wait: bool,
    json: bool,
) -> Result<u8, (OpenWorkError, bool)> {
    let workspace_path = Path::new(workspace);
    if !matches!(runtime, "claude-code" | "codex")
        || prompt.is_empty()
        || workspace_path.is_absolute()
        || !workspace_path.is_dir()
    {
        return Err((
            OpenWorkError::new(ErrorCode::InvalidArguments, "invalid run request"),
            json,
        ));
    }
    let workspace = workspace.strip_prefix("./").unwrap_or(workspace).to_owned();
    let base = control_api_url().map_err(|error| (error, json))?;
    let token = std::env::var("OPENWORK_API_TOKEN")
        .ok()
        .filter(|value| value.len() >= 32)
        .ok_or_else(|| {
            (
                OpenWorkError::new(
                    ErrorCode::ConfigInvalid,
                    "OPENWORK_API_TOKEN is required and must contain at least 32 bytes",
                ),
                json,
            )
        })?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| (control_error("Control API client could not start"), json))?;
    let endpoint = base
        .join("/v1/runs")
        .map_err(|_| (control_error("Control API URL is invalid"), json))?;
    let response = client
        .post(endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "runtime": runtime,
            "workspace": workspace,
            "prompt": prompt,
        }))
        .send()
        .map_err(|_| (control_error("Control API is unavailable"), json))?;
    if !response.status().is_success() {
        return Err((
            control_error("Control API rejected the run; no successful execution was claimed"),
            json,
        ));
    }
    let mut run = response
        .json::<Run>()
        .map_err(|_| (control_error("Control API returned an invalid run"), json))?;
    if wait {
        let deadline = Instant::now() + Duration::from_hours(1);
        while !run.status.is_terminal() {
            if Instant::now() >= deadline {
                return Err((control_error("Timed out waiting for the run"), json));
            }
            std::thread::sleep(Duration::from_millis(250));
            let endpoint = base
                .join(&format!("/v1/runs/{}", run.id.to_hyphenated()))
                .map_err(|_| (control_error("Control API URL is invalid"), json))?;
            let response = client
                .get(endpoint)
                .bearer_auth(&token)
                .send()
                .map_err(|_| (control_error("Control API is unavailable"), json))?;
            if !response.status().is_success() {
                return Err((control_error("Control API run lookup failed"), json));
            }
            run = response
                .json::<Run>()
                .map_err(|_| (control_error("Control API returned an invalid run"), json))?;
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&run).unwrap_or_else(|_| "{}".to_owned())
        );
    } else {
        println!("{}\t{:?}", run.id.to_hyphenated(), run.status);
    }
    Ok(0)
}

fn control_api_url() -> Result<reqwest::Url, OpenWorkError> {
    let raw = std::env::var("OPENWORK_CONTROL_URL").map_err(|_| {
        control_error("OPENWORK_CONTROL_URL is required; no run was created")
            .with_remediation("Start a configured Control API worker or run `openwork demo sales`.")
    })?;
    let url = reqwest::Url::parse(&raw).map_err(|_| control_error("Control API URL is invalid"))?;
    let loopback_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "::1" | "localhost"));
    if (url.scheme() != "https" && !loopback_http)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(control_error(
            "Control API URL must use HTTPS or loopback HTTP and contain no credentials",
        ));
    }
    Ok(url)
}

fn control_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::SandboxUnavailable, message)
}

fn execute_demo(command: DemoCommand) -> Result<u8, (OpenWorkError, bool)> {
    match command {
        DemoCommand::Sales {
            engine_bin,
            image,
            output_root,
            json,
        } => execute_sales_demo(engine_bin, &image, &output_root, json),
    }
}

fn execute_sales_demo(
    engine_bin: Option<PathBuf>,
    image: &str,
    output_root: &Path,
    json: bool,
) -> Result<u8, (OpenWorkError, bool)> {
    let engine_bin = resolve_container_engine(engine_bin).map_err(|error| (error, json))?;
    let image = DigestPinnedImageRef::parse(image).map_err(|error| (error, json))?;
    let output_root = if output_root.is_absolute() {
        output_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| {
                (
                    OpenWorkError::new(
                        ErrorCode::Io,
                        "current directory is unavailable for demo output",
                    ),
                    json,
                )
            })?
            .join(output_root)
    };
    let report = run_sales_demo(SalesDemoConfig {
        engine_bin,
        image,
        output_root: Some(output_root),
    })
    .map_err(|error| (error, json))?;
    render_sales_demo(&report, json);
    Ok(0)
}

fn resolve_container_engine(configured: Option<PathBuf>) -> Result<PathBuf, OpenWorkError> {
    if let Some(path) =
        configured.or_else(|| std::env::var_os("OPENWORK_DOCKER_BIN").map(PathBuf::from))
    {
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
        return Err(OpenWorkError::new(
            ErrorCode::InvalidArguments,
            "container engine path must be an existing absolute file",
        ));
    }
    ["/usr/bin/docker", "/usr/local/bin/docker"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| {
            OpenWorkError::new(
                ErrorCode::SandboxUnavailable,
                "Docker CLI was not found in a supported location",
            )
            .with_remediation("Pass --engine-bin with an absolute Docker-compatible CLI path.")
        })
}

fn render_sales_demo(report: &SalesDemoReport, json: bool) {
    if json {
        let artifacts = report
            .artifacts
            .iter()
            .map(|artifact| {
                serde_json::json!({
                    "path": artifact.path,
                    "media_type": artifact.media_type,
                    "size_bytes": artifact.size_bytes,
                    "sha256": artifact.sha256,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": report.run_id,
                "status": report.status,
                "revision": report.revision,
                "artifacts": artifacts,
                "audit_event_count": report.audit_events.len(),
                "output_directory": report.output_directory,
            }))
            .unwrap_or_default()
        );
        return;
    }
    println!("OpenWork M1 sales demo succeeded");
    println!("Run ID: {}", report.run_id);
    println!("Status: {:?}", report.status);
    if let Some(output) = &report.output_directory {
        println!("Artifacts: {}", output.display());
    }
    println!("Audit events: {}", report.audit_events.len());
}

fn runtime_lockfile_path(host: &PlatformInfo) -> std::path::PathBuf {
    host.paths.data.join("runtime.lock.json")
}

fn current_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format!("unix:{seconds}")
}

fn lockfile_error(error: &LockfileError) -> OpenWorkError {
    OpenWorkError::new(
        ErrorCode::ConfigInvalid,
        format!("runtime lockfile update failed: {error}"),
    )
    .with_remediation(
        "Preserve the current state file, fix its permissions or schema, and rerun status.",
    )
}

fn render_install_failure(failure: &openwork_installer::InstallExecutionFailure, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(failure).unwrap_or_default()
        );
        return;
    }
    eprintln!("error[{:?}]: {}", failure.error.code, failure.error);
    eprintln!("partial state: {}", failure.report.partial_state);
    for warning in &failure.report.rollback_warnings {
        eprintln!("rollback warning: {warning}");
    }
}

fn runtime_registry(host: &PlatformInfo) -> RuntimeRegistry {
    let mut registry = RuntimeRegistry::new();
    registry
        .register(Arc::new(ClaudeRuntime::new(
            Arc::new(SystemCommandRunner),
            None,
            host.clone(),
        )))
        .expect("built-in runtime ids are unique");
    registry
        .register(Arc::new(CodexRuntime::new(
            Arc::new(SystemCommandRunner),
            None,
            host.clone(),
        )))
        .expect("built-in runtime ids are unique");
    registry
}

fn normalize_runtime_id(id: &str) -> &str {
    if id == "claude" { "claude-code" } else { id }
}

fn runtime_summaries(
    registry: &RuntimeRegistry,
    json: bool,
) -> Result<Vec<RuntimeSummary>, (OpenWorkError, bool)> {
    registry
        .metadata()
        .into_iter()
        .map(|metadata| {
            registry
                .get(&metadata.id)
                .expect("metadata came from this registry")
        })
        .map(|runtime| runtime_summary(runtime.as_ref()).map_err(|error| (error, json)))
        .collect()
}

fn runtime_summary(runtime: &dyn AgentRuntime) -> Result<RuntimeSummary, OpenWorkError> {
    let detection = runtime.detect()?;
    Ok(RuntimeSummary {
        metadata: runtime.metadata(),
        detection,
        version: runtime.version()?,
        auth: runtime.auth_status()?,
        capabilities: runtime.capabilities(),
    })
}

fn platform(probe: &impl PlatformProbe, json: bool) -> Result<PlatformInfo, (OpenWorkError, bool)> {
    detect(probe).map_err(|error| {
        (
            OpenWorkError::new(ErrorCode::UnsupportedPlatform, error.to_string())
                .with_remediation("Use a documented Tier 1 host and rerun the command."),
            json,
        )
    })
}

fn render_install(plan: &InstallPlan, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(plan).unwrap_or_default());
        return;
    }
    println!("OpenWork installation plan (dry-run)");
    for step in &plan.steps {
        println!("- {}: {}", step.id, step.path.display());
    }
    for warning in &plan.warnings {
        println!("warning: {warning}");
    }
}

fn render_install_execution(report: &InstallExecutionReport, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report).unwrap_or_default()
        );
        return;
    }
    println!("OpenWork installation completed: {}", report.completed);
    for step in &report.steps {
        println!("- {} [{:?}]: {}", step.id, step.status, step.detail);
    }
}

fn render_status(report: &StatusReport, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report).unwrap_or_default()
        );
    } else {
        println!("OpenWork state: {}", report.state);
        println!(
            "Host: {:?} {:?}",
            report.platform.os, report.platform.architecture
        );
        println!("Runtimes: {}", report.runtimes.len());
    }
}

fn render_doctor(report: &DoctorReport, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report).unwrap_or_default()
        );
        return;
    }
    println!("OpenWork Doctor");
    for check in &report.checks {
        let marker = match check.status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
            CheckStatus::Skip => "SKIP",
        };
        println!("[{marker}] {} — {}", check.id, check.summary);
        if let Some(remediation) = &check.remediation {
            println!("       remediation: {remediation}");
        }
    }
    println!(
        "Summary: {} pass, {} warn, {} fail, {} skip",
        report.summary.pass, report.summary.warn, report.summary.fail, report.summary.skip
    );
}
