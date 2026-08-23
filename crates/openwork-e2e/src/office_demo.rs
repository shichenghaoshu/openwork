//! Real-container `CLI-Anything` and `LibreOffice` artifact runner.

use openwork_apps::{
    AppId, AppRegistry, CapabilityAuthorizationResult, CapabilityGateway, CapabilityGrant,
    CapabilityId, CapabilityInvocation, CliAppConfig, CliCapabilityProvider, InstalledApp,
};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::artifact::ArtifactScanner;
use openwork_execution::orchestrator::ExecutionOrchestrator;
use openwork_execution::store::{ExecutionStore, InMemoryExecutionStore};
use openwork_execution::{
    ActionPolicy, ActionRequest, ActorId, ApprovedMountDirectory, DigestPinnedImageRef,
    EXECUTION_SCHEMA_VERSION, PolicyDecision, PolicyEvaluation, RelativeArtifactPath, RiskLevel,
    RunId, RunStatus, SandboxBackend, SandboxLimits, SandboxUser, Sha256Digest, UtcTimestamp,
};
use openwork_sandbox::{DockerSandbox, SystemDockerCli};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const LIBREOFFICE_APP_ID: &str = "org.openwork.libreoffice";
const BUNDLE_CAPABILITY: &str = "libreoffice.office_bundle.create";

/// Structured office plan expected from a governed runtime such as Pi.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfficePlan {
    pub title: String,
    pub spreadsheet: SpreadsheetPlan,
    pub presentation: PresentationPlan,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpreadsheetPlan {
    pub filename: String,
    pub sheet: String,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationPlan {
    pub filename: String,
    pub slides: Vec<SlidePlan>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlidePlan {
    pub title: String,
    pub content: String,
}

/// Explicit real-container configuration.
pub struct OfficeDemoConfig {
    pub engine_bin: PathBuf,
    pub image: DigestPinnedImageRef,
    pub output_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OfficeArtifactReport {
    pub path: RelativeArtifactPath,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OfficeDemoReport {
    pub run_id: RunId,
    pub status: RunStatus,
    pub artifacts: Vec<OfficeArtifactReport>,
    pub output_directory: PathBuf,
}

/// Executes one exact-bound Office capability through the policy gateway and
/// production Docker sandbox.
///
/// # Errors
///
/// Fails closed when configuration, authorization, sandbox execution, or
/// artifact validation fails.
pub fn run_office_demo(
    config: OfficeDemoConfig,
    plan: OfficePlan,
) -> Result<OfficeDemoReport, OpenWorkError> {
    if !config.engine_bin.is_absolute() || !config.output_root.is_absolute() {
        return Err(demo_error("office demo paths must be absolute"));
    }
    let fixture = OfficeFixture::new(&config.output_root)?;
    let store = InMemoryExecutionStore::default();
    let orchestrator = ExecutionOrchestrator::new(store, ArtifactScanner::new(32 * 1024 * 1024)?);
    let actor = ActorId::parse("demo:office")?;
    let run = orchestrator.create_run(
        "pi-libreoffice",
        &fixture.workspace,
        actor.clone(),
        "Create a governed spreadsheet and presentation bundle.",
        UtcTimestamp::now(),
    )?;
    let app_id = AppId::parse(LIBREOFFICE_APP_ID)?;
    let capability_id = CapabilityId::parse(BUNDLE_CAPABILITY)?;
    let registry = AppRegistry::load(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../registry/apps")
            .as_path(),
    )?;
    let app = registry
        .get(&app_id)
        .cloned()
        .ok_or_else(|| demo_error("LibreOffice App is not registered"))?;
    let grant = CapabilityGrant {
        principal: actor.as_str().to_owned(),
        app_id: app_id.clone(),
        capabilities: BTreeSet::from([capability_id.clone()]),
    };
    let gateway = CapabilityGateway::new(
        DemoPolicy,
        [app],
        [InstalledApp {
            app_id: app_id.clone(),
            enabled: true,
            credential_ref: None,
        }],
        [grant],
    );
    let parameters =
        serde_json::to_value(plan).map_err(|_| demo_error("office plan could not be encoded"))?;
    let invocation = CapabilityInvocation::new(
        actor.clone(),
        run.id.clone(),
        app_id.clone(),
        capability_id,
        "artifact:office-bundle",
        parameters,
    )?;
    let authorization = match gateway.authorize(invocation)? {
        CapabilityAuthorizationResult::Authorized(authorization) => authorization,
        CapabilityAuthorizationResult::PendingApproval(_) => {
            return Err(demo_error(
                "office capability unexpectedly requires approval",
            ));
        }
    };
    let provider = CliCapabilityProvider::new(vec![CliAppConfig {
        app_id,
        image: config.image,
        program: "/opt/openwork/bin/libreoffice-capability".to_owned(),
        fixed_arguments: Vec::new(),
        user: SandboxUser::new(65_534, 65_534)?,
        limits: SandboxLimits::new(1_000, 1024 * 1024 * 1024, 128, 180, 2 * 1024 * 1024)?,
    }])?;
    let request = provider.prepare(
        &authorization,
        ApprovedMountDirectory::under_root(&fixture.input, &fixture.root)?,
        ApprovedMountDirectory::under_root(&fixture.output, &fixture.root)?,
    )?;
    let sandbox = DockerSandbox::new(
        Arc::new(SystemDockerCli::new(config.engine_bin)?),
        fixture.sandbox_temporary,
    );
    sandbox.health()?;
    let final_run = orchestrator.execute_with_output_processor(
        &run,
        &sandbox,
        &request,
        actor,
        UtcTimestamp::now(),
        |_| validate_office_artifacts(&fixture.output),
    )?;
    let artifacts = orchestrator.store().artifacts(&final_run.id)?;
    Ok(office_report(final_run, artifacts, fixture.output))
}

fn office_report(
    run: openwork_execution::Run,
    artifacts: Vec<openwork_execution::Artifact>,
    output_directory: PathBuf,
) -> OfficeDemoReport {
    let artifacts = artifacts
        .into_iter()
        .map(|artifact| OfficeArtifactReport {
            path: artifact.path,
            media_type: artifact.media_type,
            size_bytes: artifact.size_bytes.get(),
            sha256: artifact.sha256,
        })
        .collect();
    OfficeDemoReport {
        run_id: run.id,
        status: run.status,
        artifacts,
        output_directory,
    }
}

struct DemoPolicy;

impl ActionPolicy for DemoPolicy {
    fn evaluate(&self, request: &ActionRequest) -> PolicyEvaluation {
        PolicyEvaluation {
            schema_version: EXECUTION_SCHEMA_VERSION,
            run_id: request.run_id.clone(),
            action_id: request.id.clone(),
            parameter_hash: request.parameter_hash().clone(),
            decision: PolicyDecision::Allow,
            effective_risk: RiskLevel::LocalWrite,
            policy_version: "office-demo-v1".to_owned(),
            rule_id: "allow-local-artifact-create".to_owned(),
            reason_code: "artifact_only".to_owned(),
            evaluated_at: UtcTimestamp::now(),
        }
    }
}

struct OfficeFixture {
    root: PathBuf,
    workspace: PathBuf,
    input: PathBuf,
    output: PathBuf,
    sandbox_temporary: PathBuf,
}

impl OfficeFixture {
    fn new(output_root: &Path) -> Result<Self, OpenWorkError> {
        let root = canonical_directory(output_root)?;
        let workspace = canonical_directory(&root.join("workspace"))?;
        let input = canonical_directory(&root.join("input"))?;
        let output = canonical_directory(&root.join("artifacts"))?;
        let sandbox_temporary = canonical_directory(&root.join("sandbox"))?;
        make_container_writable(&output)?;
        Ok(Self {
            root,
            workspace,
            input,
            output,
            sandbox_temporary,
        })
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, OpenWorkError> {
    fs::create_dir_all(path).map_err(|_| demo_error("office demo directory unavailable"))?;
    fs::canonicalize(path).map_err(|_| demo_error("office demo directory unavailable"))
}

fn validate_office_artifacts(output: &Path) -> Result<(), OpenWorkError> {
    let mut paths = fs::read_dir(output)
        .map_err(|_| artifact_error("office artifact directory unavailable"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    if paths.len() != 2
        || !paths
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "xlsx"))
        || !paths
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "pptx"))
    {
        return Err(artifact_error(
            "office demo did not create the exact artifact pair",
        ));
    }
    for path in paths {
        let bytes = fs::read(path).map_err(|_| artifact_error("office artifact is unreadable"))?;
        if bytes.len() < 4 || &bytes[..4] != b"PK\x03\x04" {
            return Err(artifact_error("office artifact is not an OOXML package"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_container_writable(path: &Path) -> Result<(), OpenWorkError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o1777))
        .map_err(|_| demo_error("office artifact permissions failed"))
}

#[cfg(not(unix))]
fn make_container_writable(_path: &Path) -> Result<(), OpenWorkError> {
    Ok(())
}

fn artifact_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ArtifactInvalid, message)
}

fn demo_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ExecutionFailed, message)
}
