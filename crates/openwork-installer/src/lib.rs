//! Reversible installation planning and execution.

use openwork_core::{ErrorCode, OpenWorkError};
use openwork_platform::PlatformInfo;
use openwork_runtime::{
    CancellationToken, CommandRunner, CommandSpec, DownloadRequest, Downloader, RuntimeInstallPlan,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const INSTALL_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallAction {
    CreateDirectory,
    Download,
    RunCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallStep {
    pub id: String,
    pub action: InstallAction,
    pub path: PathBuf,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandSpec>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallPlan {
    pub schema_version: u32,
    /// Indicates the plan was produced for presentation. Execution still
    /// requires an explicit [`ExecutionMode::Execute`] selection.
    pub dry_run: bool,
    pub steps: Vec<InstallStep>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    DryRun,
    Execute,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Planned,
    Created,
    Preserved,
    Downloaded,
    Executed,
    RolledBack,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    pub id: String,
    pub status: StepStatus,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_receipt: Option<openwork_runtime::DownloadReceipt>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallExecutionReport {
    pub mode: ExecutionMode,
    pub completed: bool,
    /// True when an attempted command may have changed state that `OpenWork`
    /// cannot reverse, or when a rollback operation itself failed.
    pub partial_state: bool,
    pub steps: Vec<StepResult>,
    pub rollback_warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InstallExecutionFailure {
    pub error: OpenWorkError,
    pub report: InstallExecutionReport,
}

impl fmt::Display for InstallExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for InstallExecutionFailure {}

/// Executes a reviewed plan through injected boundaries.
pub struct InstallExecutor<'a> {
    downloader: &'a dyn Downloader,
    runner: &'a dyn CommandRunner,
}

impl<'a> InstallExecutor<'a> {
    #[must_use]
    pub const fn new(downloader: &'a dyn Downloader, runner: &'a dyn CommandRunner) -> Self {
        Self { downloader, runner }
    }

    /// Uses the identical plan for preview and execution. Only `mode` controls
    /// whether side effects are permitted.
    ///
    /// # Errors
    ///
    /// Returns a failure with a structured rollback report when the plan is
    /// invalid, cancelled, or any operation fails.
    pub fn run(
        &self,
        plan: &InstallPlan,
        mode: ExecutionMode,
        cancellation: &CancellationToken,
    ) -> Result<InstallExecutionReport, InstallExecutionFailure> {
        if plan.schema_version != INSTALL_PLAN_SCHEMA_VERSION {
            return Err(failure(
                mode,
                OpenWorkError::new(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "unsupported install plan schema version {}",
                        plan.schema_version
                    ),
                )
                .with_remediation("Regenerate the install plan with this OpenWork version."),
                Vec::new(),
                false,
            ));
        }

        if mode == ExecutionMode::DryRun {
            return Ok(InstallExecutionReport {
                mode,
                completed: true,
                partial_state: false,
                steps: plan
                    .steps
                    .iter()
                    .map(|step| StepResult {
                        id: step.id.clone(),
                        status: StepStatus::Planned,
                        detail: step.reason.clone(),
                        download_receipt: None,
                    })
                    .collect(),
                rollback_warnings: Vec::new(),
            });
        }

        let mut results = Vec::with_capacity(plan.steps.len());
        let mut reversible = Vec::new();
        let mut irreversible_attempted = false;

        for step in &plan.steps {
            if cancellation.is_cancelled() {
                let error = OpenWorkError::new(ErrorCode::InstallFailed, "installation cancelled")
                    .with_remediation("Review the rollback report before retrying.");
                return Err(rollback_failure(
                    mode,
                    error,
                    results,
                    &mut reversible,
                    irreversible_attempted,
                ));
            }

            let result = match step.action {
                InstallAction::CreateDirectory => execute_directory(step, &mut reversible),
                InstallAction::Download => {
                    self.execute_download(step, cancellation, &mut reversible)
                }
                InstallAction::RunCommand => {
                    irreversible_attempted = true;
                    self.execute_command(step, cancellation)
                }
            };

            match result {
                Ok(result) => results.push(result),
                Err(error) => {
                    results.push(StepResult {
                        id: step.id.clone(),
                        status: StepStatus::Failed,
                        detail: error.message.clone(),
                        download_receipt: None,
                    });
                    return Err(rollback_failure(
                        mode,
                        error,
                        results,
                        &mut reversible,
                        irreversible_attempted,
                    ));
                }
            }
        }

        Ok(InstallExecutionReport {
            mode,
            completed: true,
            partial_state: false,
            steps: results,
            rollback_warnings: Vec::new(),
        })
    }

    fn execute_download(
        &self,
        step: &InstallStep,
        cancellation: &CancellationToken,
        reversible: &mut Vec<ReversibleChange>,
    ) -> Result<StepResult, OpenWorkError> {
        let request = step.download.as_ref().ok_or_else(|| malformed_step(step))?;
        if request.destination != step.path {
            return Err(OpenWorkError::new(
                ErrorCode::ConfigInvalid,
                format!("download step `{}` has inconsistent destinations", step.id),
            ));
        }
        if step.path.exists() {
            return Err(OpenWorkError::new(
                ErrorCode::InstallFailed,
                format!(
                    "refusing to overwrite existing download `{}`",
                    step.path.display()
                ),
            )
            .with_remediation("Review and remove only the stale managed download, then retry."));
        }
        let receipt = self.downloader.download(request, cancellation)?;
        reversible.push(ReversibleChange::File(step.path.clone()));
        Ok(StepResult {
            id: step.id.clone(),
            status: StepStatus::Downloaded,
            detail: format!(
                "downloaded {} bytes; sha256={}; verified={}",
                receipt.bytes_written, receipt.observed_sha256, receipt.verified
            ),
            download_receipt: Some(receipt),
        })
    }

    fn execute_command(
        &self,
        step: &InstallStep,
        cancellation: &CancellationToken,
    ) -> Result<StepResult, OpenWorkError> {
        let command = step.command.as_ref().ok_or_else(|| malformed_step(step))?;
        if command.program != step.path {
            return Err(OpenWorkError::new(
                ErrorCode::ConfigInvalid,
                format!("command step `{}` has an inconsistent program", step.id),
            ));
        }
        let output = self.runner.run(command, cancellation)?;
        if output.cancelled {
            return Err(OpenWorkError::new(
                ErrorCode::InstallFailed,
                format!("install command `{}` was cancelled", step.id),
            ));
        }
        if output.timed_out {
            return Err(OpenWorkError::new(
                ErrorCode::InstallFailed,
                format!("install command `{}` timed out", step.id),
            ));
        }
        if output.exit_code != Some(0) {
            return Err(OpenWorkError::new(
                ErrorCode::InstallFailed,
                format!("install command `{}` failed", step.id),
            )
            .with_remediation(
                "Inspect redacted runtime diagnostics and the partial-state report.",
            ));
        }
        Ok(StepResult {
            id: step.id.clone(),
            status: StepStatus::Executed,
            detail: "command completed successfully".to_owned(),
            download_receipt: None,
        })
    }
}

/// Builds the side-effect-free Bootstrap directory plan.
#[must_use]
pub fn dry_run_plan(platform: &PlatformInfo) -> InstallPlan {
    let paths = &platform.paths;
    let steps = [
        ("config", &paths.config),
        ("data", &paths.data),
        ("cache", &paths.cache),
        ("logs", &paths.logs),
        ("bin", &paths.bin),
    ]
    .into_iter()
    .map(|(id, path)| InstallStep {
        id: format!("directory.{id}"),
        action: InstallAction::CreateDirectory,
        path: path.clone(),
        reason: format!("Prepare the OpenWork {id} location"),
        download: None,
        command: None,
    })
    .collect();

    InstallPlan {
        schema_version: INSTALL_PLAN_SCHEMA_VERSION,
        dry_run: true,
        steps,
        warnings: vec![
            "Dry-run only: no directories, downloads, or subprocesses were executed.".to_owned(),
            "Runtime install steps will be added after runtime selection.".to_owned(),
        ],
    }
}

/// Extends the Bootstrap directory plan with a selected runtime's exact
/// download and command operations without executing either.
#[must_use]
pub fn managed_runtime_plan(
    platform: &PlatformInfo,
    runtime_id: &str,
    runtime: &RuntimeInstallPlan,
) -> InstallPlan {
    let mut plan = dry_run_plan(platform);
    plan.warnings.pop();
    plan.warnings.extend(runtime.warnings.iter().cloned());
    let mut planned_directories = plan
        .steps
        .iter()
        .filter(|step| step.action == InstallAction::CreateDirectory)
        .map(|step| step.path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for (index, request) in runtime.downloads.iter().enumerate() {
        if let Some(parent) = request.destination.parent()
            && planned_directories.insert(parent.to_path_buf())
        {
            plan.steps.push(InstallStep {
                id: format!("runtime.{runtime_id}.download-directory.{index}"),
                action: InstallAction::CreateDirectory,
                path: parent.to_path_buf(),
                reason: format!("Prepare the managed {runtime_id} download location"),
                download: None,
                command: None,
            });
        }
        plan.steps.push(InstallStep {
            id: format!("runtime.{runtime_id}.download.{index}"),
            action: InstallAction::Download,
            path: request.destination.clone(),
            reason: format!("Fetch {runtime_id} from its official managed source"),
            download: Some(request.clone()),
            command: None,
        });
    }
    plan.steps.extend(
        runtime
            .commands
            .iter()
            .enumerate()
            .map(|(index, command)| InstallStep {
                id: format!("runtime.{runtime_id}.command.{index}"),
                action: InstallAction::RunCommand,
                path: command.program.clone(),
                reason: format!("Run the reviewed {runtime_id} official installer"),
                download: None,
                command: Some(command.clone()),
            }),
    );
    plan
}

fn execute_directory(
    step: &InstallStep,
    reversible: &mut Vec<ReversibleChange>,
) -> Result<StepResult, OpenWorkError> {
    if step.download.is_some() || step.command.is_some() {
        return Err(malformed_step(step));
    }
    if step.path.is_dir() {
        return Ok(StepResult {
            id: step.id.clone(),
            status: StepStatus::Preserved,
            detail: "existing directory preserved".to_owned(),
            download_receipt: None,
        });
    }
    if step.path.exists() {
        return Err(OpenWorkError::new(
            ErrorCode::InstallFailed,
            format!(
                "cannot create directory because `{}` already exists as a file",
                step.path.display()
            ),
        ));
    }
    let mut missing_directories = Vec::new();
    let mut candidate = Some(step.path.as_path());
    while let Some(path) = candidate {
        if path.exists() {
            break;
        }
        missing_directories.push(path.to_path_buf());
        candidate = path.parent();
    }
    fs::create_dir_all(&step.path).map_err(install_io_error)?;
    reversible.extend(
        missing_directories
            .into_iter()
            .rev()
            .map(ReversibleChange::Directory),
    );
    Ok(StepResult {
        id: step.id.clone(),
        status: StepStatus::Created,
        detail: "directory created".to_owned(),
        download_receipt: None,
    })
}

#[derive(Clone, Debug)]
enum ReversibleChange {
    File(PathBuf),
    Directory(PathBuf),
}

fn rollback_failure(
    mode: ExecutionMode,
    error: OpenWorkError,
    mut results: Vec<StepResult>,
    changes: &mut Vec<ReversibleChange>,
    irreversible_attempted: bool,
) -> InstallExecutionFailure {
    let mut rollback_warnings = Vec::new();
    while let Some(change) = changes.pop() {
        let (path, result) = match change {
            ReversibleChange::File(path) => {
                let result = remove_file_if_present(&path);
                (path, result)
            }
            ReversibleChange::Directory(path) => {
                let result = remove_directory_if_empty(&path);
                (path, result)
            }
        };
        match result {
            Ok(()) => results.push(StepResult {
                id: format!("rollback.{}", path.display()),
                status: StepStatus::RolledBack,
                detail: "reversible install change removed".to_owned(),
                download_receipt: None,
            }),
            Err(rollback_error) => rollback_warnings.push(format!(
                "could not roll back `{}`: {}",
                path.display(),
                rollback_error
            )),
        }
    }
    InstallExecutionFailure {
        error,
        report: InstallExecutionReport {
            mode,
            completed: false,
            partial_state: irreversible_attempted || !rollback_warnings.is_empty(),
            steps: results,
            rollback_warnings,
        },
    }
}

fn failure(
    mode: ExecutionMode,
    error: OpenWorkError,
    steps: Vec<StepResult>,
    partial_state: bool,
) -> InstallExecutionFailure {
    InstallExecutionFailure {
        error,
        report: InstallExecutionReport {
            mode,
            completed: false,
            partial_state,
            steps,
            rollback_warnings: Vec::new(),
        },
    }
}

fn remove_file_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_directory_if_empty(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn malformed_step(step: &InstallStep) -> OpenWorkError {
    OpenWorkError::new(
        ErrorCode::ConfigInvalid,
        format!("install step `{}` does not match its action", step.id),
    )
    .with_remediation("Regenerate the install plan with this OpenWork version.")
}

#[allow(clippy::needless_pass_by_value)]
fn install_io_error(error: std::io::Error) -> OpenWorkError {
    OpenWorkError::new(
        ErrorCode::InstallFailed,
        format!("installation filesystem operation failed: {error}"),
    )
    .with_remediation("Check managed-directory permissions and free space.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwork_platform::{
        Architecture, HostEnvironment, OpenWorkPaths, OperatingSystem, PermissionFacts,
        PrerequisiteFacts, ResourceFacts, SupportTier,
    };
    use openwork_runtime::{CommandOutput, DownloadReceipt, RuntimeResult};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    struct FakeDownloader {
        calls: AtomicUsize,
    }

    impl Downloader for FakeDownloader {
        fn download(
            &self,
            request: &DownloadRequest,
            _: &CancellationToken,
        ) -> RuntimeResult<DownloadReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            fs::create_dir_all(request.destination.parent().unwrap()).map_err(install_io_error)?;
            fs::write(&request.destination, b"fixture").map_err(install_io_error)?;
            Ok(DownloadReceipt {
                bytes_written: 7,
                observed_sha256: "fixture-digest".to_owned(),
                verified: false,
            })
        }
    }

    struct FakeRunner {
        calls: AtomicUsize,
        exit_code: Mutex<Option<i32>>,
    }

    impl CommandRunner for FakeRunner {
        fn find_executable(&self, executable: &str) -> Option<PathBuf> {
            Some(PathBuf::from(executable))
        }

        fn run(&self, _: &CommandSpec, _: &CancellationToken) -> RuntimeResult<CommandOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CommandOutput {
                exit_code: *self.exit_code.lock().unwrap(),
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                cancelled: false,
                truncated: false,
            })
        }
    }

    fn fixture_platform(root: &Path) -> PlatformInfo {
        PlatformInfo {
            schema_version: 1,
            os: OperatingSystem::Linux,
            os_version: None,
            architecture: Architecture::X64,
            environment: HostEnvironment::Native,
            support_tier: SupportTier::Tier1,
            shell: None,
            package_managers: vec![],
            paths: OpenWorkPaths {
                config: root.join("config"),
                data: root.join("data"),
                cache: root.join("cache"),
                logs: root.join("logs"),
                bin: root.join("bin"),
            },
            permissions: PermissionFacts {
                home_writable: true,
                install_dir_writable: true,
                elevated: false,
            },
            resources: ResourceFacts {
                total_memory_bytes: None,
                available_disk_bytes: None,
            },
            prerequisites: PrerequisiteFacts {
                git_present: true,
                docker_present: false,
            },
        }
    }

    fn temporary_root(label: &str) -> PathBuf {
        let unique = NEXT_TEMP.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "openwork-installer-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    fn fakes(exit_code: i32) -> (FakeDownloader, FakeRunner) {
        (
            FakeDownloader {
                calls: AtomicUsize::new(0),
            },
            FakeRunner {
                calls: AtomicUsize::new(0),
                exit_code: Mutex::new(Some(exit_code)),
            },
        )
    }

    #[test]
    fn dry_run_contains_only_declarative_steps() {
        let root = PathBuf::from("/path/that/openwork/test/does/not/create");
        let plan = dry_run_plan(&fixture_platform(&root));
        assert!(plan.dry_run);
        assert_eq!(plan.steps.len(), 5);
        assert!(plan.steps.iter().all(|step| !step.path.exists()));
    }

    #[test]
    fn same_plan_supports_preview_then_execution() {
        let root = temporary_root("same-plan");
        let platform = fixture_platform(&root);
        let plan = dry_run_plan(&platform);
        let (downloader, runner) = fakes(0);
        let executor = InstallExecutor::new(&downloader, &runner);

        let preview = executor
            .run(&plan, ExecutionMode::DryRun, &CancellationToken::new())
            .unwrap();
        assert!(
            preview
                .steps
                .iter()
                .all(|step| step.status == StepStatus::Planned)
        );
        assert!(!root.exists());

        let executed = executor
            .run(&plan, ExecutionMode::Execute, &CancellationToken::new())
            .unwrap();
        assert!(executed.completed);
        assert!(platform.paths.config.is_dir());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn runtime_failure_rolls_back_downloads_and_created_directories() {
        let root = temporary_root("rollback");
        let platform = fixture_platform(&root);
        let download = platform.paths.cache.join("downloads/installer.sh");
        let runtime = RuntimeInstallPlan {
            source_url: "https://example.invalid/installer.sh".to_owned(),
            version: None,
            downloads: vec![DownloadRequest {
                url: "https://example.invalid/installer.sh".to_owned(),
                destination: download.clone(),
                expected_sha256: None,
                timeout_millis: 100,
            }],
            commands: vec![CommandSpec {
                program: PathBuf::from("fixture-installer"),
                arguments: vec!["--fixture".to_owned()],
                environment: BTreeMap::new(),
                working_directory: None,
                timeout_millis: 1_000,
            }],
            warnings: vec![],
        };
        let plan = managed_runtime_plan(&platform, "fixture", &runtime);
        let (downloader, runner) = fakes(1);
        let failure = InstallExecutor::new(&downloader, &runner)
            .run(&plan, ExecutionMode::Execute, &CancellationToken::new())
            .unwrap_err();

        assert!(failure.report.partial_state);
        assert!(!download.exists());
        assert!(!root.exists());
    }

    #[test]
    fn existing_managed_paths_are_preserved() {
        let root = temporary_root("preserve");
        let platform = fixture_platform(&root);
        fs::create_dir_all(&platform.paths.config).unwrap();
        fs::write(platform.paths.config.join("owned-by-user"), b"keep").unwrap();
        let (downloader, runner) = fakes(0);
        let report = InstallExecutor::new(&downloader, &runner)
            .run(
                &dry_run_plan(&platform),
                ExecutionMode::Execute,
                &CancellationToken::new(),
            )
            .unwrap();

        assert_eq!(report.steps[0].status, StepStatus::Preserved);
        assert!(platform.paths.config.join("owned-by-user").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn execution_never_overwrites_existing_download() {
        let root = temporary_root("no-clobber");
        let platform = fixture_platform(&root);
        let destination = platform.paths.cache.join("downloads/installer.sh");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"existing").unwrap();
        let runtime = RuntimeInstallPlan {
            source_url: "https://example.invalid/installer.sh".to_owned(),
            version: None,
            downloads: vec![DownloadRequest {
                url: "https://example.invalid/installer.sh".to_owned(),
                destination: destination.clone(),
                expected_sha256: None,
                timeout_millis: 100,
            }],
            commands: vec![],
            warnings: vec![],
        };
        let (downloader, runner) = fakes(0);
        let failure = InstallExecutor::new(&downloader, &runner)
            .run(
                &managed_runtime_plan(&platform, "fixture", &runtime),
                ExecutionMode::Execute,
                &CancellationToken::new(),
            )
            .unwrap_err();

        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 0);
        assert!(!failure.report.partial_state);
        fs::remove_dir_all(&root).unwrap();
    }
}
