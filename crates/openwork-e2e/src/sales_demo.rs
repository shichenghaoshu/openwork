//! Reusable real-container runner for the deterministic sales demo.

use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::artifact::ArtifactScanner;
use openwork_execution::orchestrator::ExecutionOrchestrator;
use openwork_execution::store::{ExecutionStore, InMemoryExecutionStore};
use openwork_execution::{
    ActorId, ApprovedMountDirectory, AuditEventType, DigestPinnedImageRef, RelativeArtifactPath,
    Run, RunId, RunStatus, SandboxBackend, SandboxCommand, SandboxLimits, SandboxRequest,
    SandboxUser, Sha256Digest, UtcTimestamp,
};
use openwork_sandbox::{DockerSandbox, SystemDockerCli};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const JULY: &str = include_str!("../../../samples/sales/sales_july.csv");
const AUGUST: &str = include_str!("../../../samples/sales/sales_august.csv");
const ANALYZER: &str = include_str!("../../../samples/sales/analyze.awk");
const GOLDEN_ANALYSIS: &str = include_str!("../../../samples/sales/golden/sales-analysis.csv");
const GOLDEN_SUMMARY: &str = include_str!("../../../samples/sales/golden/summary.md");

/// Explicit configuration for one real-container sales demo run.
pub struct SalesDemoConfig {
    /// Absolute path to a Docker-compatible CLI executable.
    pub engine_bin: PathBuf,
    /// BusyBox-compatible image pinned by an exact sha256 digest.
    pub image: DigestPinnedImageRef,
    /// Optional absolute root under which a unique run output directory is retained.
    pub output_root: Option<PathBuf>,
}

/// Non-sensitive artifact evidence returned by the demo runner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SalesArtifactReport {
    pub path: RelativeArtifactPath,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
}

/// Non-sensitive append-only audit evidence returned by the demo runner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SalesAuditReport {
    pub sequence: u64,
    pub event_type: AuditEventType,
    pub event_hash: Sha256Digest,
}

/// Completed real-container run evidence. Raw input, prompt, stdout, and stderr are omitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SalesDemoReport {
    pub run_id: RunId,
    pub status: RunStatus,
    pub revision: u64,
    pub artifacts: Vec<SalesArtifactReport>,
    pub audit_events: Vec<SalesAuditReport>,
    /// Present only when the caller requested retained output.
    pub output_directory: Option<PathBuf>,
}

/// Runs the fixed sales analyzer through the production Docker sandbox and orchestrator.
///
/// # Errors
///
/// Fails closed for relative executable/output paths, an unavailable engine,
/// sandbox failures, unsafe artifacts, or any byte-level golden mismatch.
pub fn run_sales_demo(config: SalesDemoConfig) -> Result<SalesDemoReport, OpenWorkError> {
    validate_config(&config)?;
    let SalesDemoConfig {
        engine_bin,
        image,
        output_root,
    } = config;
    let fixture = SalesFixture::new()?;
    let store = InMemoryExecutionStore::default();
    let scanner = ArtifactScanner::new(1024 * 1024)?;
    let orchestrator = ExecutionOrchestrator::new(store, scanner);
    let actor = ActorId::parse("demo:sales")?;
    let run = orchestrator.create_run(
        "sales-analyzer",
        &fixture.workspace,
        actor.clone(),
        "Analyze the fixed July and August sales and order-count inputs.",
        UtcTimestamp::now(),
    )?;
    let cli = Arc::new(SystemDockerCli::new(engine_bin)?);
    let sandbox = DockerSandbox::new(cli, fixture.sandbox_temporary.clone());
    sandbox.health()?;
    let output = SalesOutput::new(output_root, fixture.temporary.path(), &run.id)?;
    let request = sandbox_request(&run.id, image, &fixture, &output)?;
    let final_run = orchestrator.execute_with_output_processor(
        &run,
        &sandbox,
        &request,
        actor,
        UtcTimestamp::now(),
        |_| validate_exact_output(&output.directory),
    )?;
    collect_report(&orchestrator, final_run, output.retained_directory)
}

fn collect_report(
    orchestrator: &ExecutionOrchestrator<InMemoryExecutionStore>,
    final_run: Run,
    output_directory: Option<PathBuf>,
) -> Result<SalesDemoReport, OpenWorkError> {
    let artifacts = orchestrator
        .store()
        .artifacts(&final_run.id)?
        .into_iter()
        .map(|artifact| SalesArtifactReport {
            path: artifact.path,
            media_type: artifact.media_type,
            size_bytes: artifact.size_bytes.get(),
            sha256: artifact.sha256,
        })
        .collect();
    let audit_events = orchestrator
        .store()
        .audit_events(&final_run.id)?
        .into_iter()
        .map(|event| SalesAuditReport {
            sequence: event.sequence,
            event_type: event.event_type,
            event_hash: event.event_hash().clone(),
        })
        .collect();
    Ok(SalesDemoReport {
        run_id: final_run.id,
        status: final_run.status,
        revision: final_run.revision,
        artifacts,
        audit_events,
        output_directory,
    })
}

struct SalesFixture {
    temporary: tempfile::TempDir,
    workspace: PathBuf,
    input_root: PathBuf,
    input: PathBuf,
    sandbox_temporary: PathBuf,
}

impl SalesFixture {
    fn new() -> Result<Self, OpenWorkError> {
        let temporary = tempfile::tempdir().map_err(|_| {
            demo_error(
                ErrorCode::Internal,
                "sales demo temporary workspace is unavailable",
            )
        })?;
        let workspace = create_directory(&temporary.path().join("workspace"))?;
        let input_root = create_directory(&temporary.path().join("approved-inputs"))?;
        let input = create_directory(&input_root.join("run"))?;
        let sandbox_temporary = create_directory(&temporary.path().join("sandbox-temporary"))?;
        write_fixture(&input.join("july.csv"), JULY)?;
        write_fixture(&input.join("august.csv"), AUGUST)?;
        write_fixture(&input.join("analyze.awk"), ANALYZER)?;
        Ok(Self {
            temporary,
            workspace,
            input_root,
            input,
            sandbox_temporary,
        })
    }
}

struct SalesOutput {
    root: PathBuf,
    directory: PathBuf,
    retained_directory: Option<PathBuf>,
}

impl SalesOutput {
    fn new(
        configured_root: Option<PathBuf>,
        temporary_root: &Path,
        run_id: &RunId,
    ) -> Result<Self, OpenWorkError> {
        let (root, directory, retained_directory) = if let Some(root) = configured_root {
            let root = create_directory(&root)?;
            let directory = create_unique_directory(&root.join(run_id.to_hyphenated()))?;
            (root, directory.clone(), Some(directory))
        } else {
            let root = create_directory(&temporary_root.join("approved-outputs"))?;
            let directory = create_directory(&root.join("run"))?;
            (root, directory, None)
        };
        make_container_writable(&directory)?;
        Ok(Self {
            root,
            directory,
            retained_directory,
        })
    }
}

fn sandbox_request(
    run_id: &RunId,
    image: DigestPinnedImageRef,
    fixture: &SalesFixture,
    output: &SalesOutput,
) -> Result<SandboxRequest, OpenWorkError> {
    let command = SandboxCommand::new(
        "/bin/awk",
        vec![
            "-f".to_owned(),
            "/workspace/input/analyze.awk".to_owned(),
            "/workspace/input/july.csv".to_owned(),
            "/workspace/input/august.csv".to_owned(),
        ],
        BTreeMap::from([(
            "OPENWORK_OUTPUT_DIR".to_owned(),
            "/workspace/output".to_owned(),
        )]),
    )?;
    SandboxRequest::new(
        run_id.clone(),
        image,
        command,
        SandboxUser::new(65_534, 65_534)?,
        ApprovedMountDirectory::under_root(&fixture.input, &fixture.input_root)?,
        ApprovedMountDirectory::under_root(&output.directory, &output.root)?,
        SandboxLimits::new(500, 64 * 1024 * 1024, 32, 30, 256 * 1024)?,
    )
}

fn validate_config(config: &SalesDemoConfig) -> Result<(), OpenWorkError> {
    if config.engine_bin.is_absolute()
        && config
            .output_root
            .as_ref()
            .is_none_or(|root| root.is_absolute())
    {
        return Ok(());
    }
    Err(demo_error(
        ErrorCode::InvalidArguments,
        "sales demo paths must be absolute",
    ))
}

fn validate_exact_output(output: &Path) -> Result<(), OpenWorkError> {
    let analysis = fs::read(output.join("sales-analysis.csv"))
        .map_err(|_| artifact_error("sales analysis artifact is unavailable"))?;
    let summary = fs::read(output.join("summary.md"))
        .map_err(|_| artifact_error("sales summary artifact is unavailable"))?;
    if analysis != GOLDEN_ANALYSIS.as_bytes() || summary != GOLDEN_SUMMARY.as_bytes() {
        return Err(artifact_error(
            "sales demo output does not match exact golden",
        ));
    }
    Ok(())
}

fn create_directory(path: &Path) -> Result<PathBuf, OpenWorkError> {
    fs::create_dir_all(path).map_err(|_| {
        demo_error(
            ErrorCode::InvalidArguments,
            "sales demo directory unavailable",
        )
    })?;
    let canonical = fs::canonicalize(path).map_err(|_| {
        demo_error(
            ErrorCode::InvalidArguments,
            "sales demo directory unavailable",
        )
    })?;
    if !fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(demo_error(
            ErrorCode::InvalidArguments,
            "sales demo path is not a directory",
        ));
    }
    Ok(canonical)
}

fn create_unique_directory(path: &Path) -> Result<PathBuf, OpenWorkError> {
    fs::create_dir(path).map_err(|_| {
        demo_error(
            ErrorCode::InvalidArguments,
            "sales demo output directory is unavailable or already exists",
        )
    })?;
    fs::canonicalize(path).map_err(|_| {
        demo_error(
            ErrorCode::InvalidArguments,
            "sales demo directory unavailable",
        )
    })
}

fn write_fixture(path: &Path, contents: &str) -> Result<(), OpenWorkError> {
    fs::write(path, contents)
        .map_err(|_| demo_error(ErrorCode::Internal, "sales demo fixture write failed"))
}

#[cfg(unix)]
fn make_container_writable(path: &Path) -> Result<(), OpenWorkError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o1777))
        .map_err(|_| demo_error(ErrorCode::Internal, "sales output permissions failed"))
}

#[cfg(not(unix))]
fn make_container_writable(_path: &Path) -> Result<(), OpenWorkError> {
    Ok(())
}

fn artifact_error(message: &'static str) -> OpenWorkError {
    demo_error(ErrorCode::ArtifactInvalid, message)
}

fn demo_error(code: ErrorCode, message: &'static str) -> OpenWorkError {
    OpenWorkError::new(code, message)
}
