use clap::{Parser, Subcommand};
use openwork_core::{ErrorCode, OpenWorkError, PRODUCT_NAME};
use openwork_doctor::{CheckStatus, DoctorReport, inspect_platform};
use openwork_installer::{
    ExecutionMode, InstallExecutionReport, InstallExecutor, InstallPlan, dry_run_plan,
    managed_runtime_plan,
};
use openwork_platform::{PlatformInfo, PlatformProbe, SystemPlatformProbe, detect};
use openwork_runtime::{
    AgentRuntime, AuthStatus, ClaudeRuntime, CodexRuntime, DetectionState, RuntimeCapabilities,
    RuntimeDetection, RuntimeId, RuntimeMetadata, RuntimeRegistry, SystemCommandRunner,
    SystemDownloader,
};
use serde::Serialize;
use std::process::ExitCode;
use std::sync::Arc;

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
}

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
        Command::Status { json } => {
            let host = platform(probe, json)?;
            let runtimes = runtime_summaries(&runtime_registry(&host), json)?;
            let report = StatusReport {
                schema_version: 1,
                state: "not_installed",
                platform: host,
                runtimes,
            };
            render_status(&report, json);
            Ok(0)
        }
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

fn execute_install(
    probe: &impl PlatformProbe,
    request: InstallRequest,
) -> Result<u8, (OpenWorkError, bool)> {
    let host = platform(probe, request.json)?;
    let registry = runtime_registry(&host);
    let (plan, selected_runtime) = if let Some(requested_id) = request.runtime {
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
        (
            managed_runtime_plan(&host, normalized_id, &runtime_plan),
            Some(runtime),
        )
    } else {
        (dry_run_plan(&host), None)
    };

    if !request.execute {
        render_install(&plan, request.json);
        return Ok(0);
    }
    ensure_runtime_is_missing(selected_runtime.as_deref(), request.json)?;

    let downloader = SystemDownloader::new().map_err(|error| (error, request.json))?;
    let runner = SystemCommandRunner;
    match InstallExecutor::new(&downloader, &runner).run(
        &plan,
        ExecutionMode::Execute,
        &openwork_runtime::CancellationToken::new(),
    ) {
        Ok(report) => {
            render_install_execution(&report, request.json);
            Ok(0)
        }
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
