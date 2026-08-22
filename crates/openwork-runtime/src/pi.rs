use crate::{
    AgentRuntime, AuthStatus, CancellationToken, CommandRunner, CommandSpec, DetectionState,
    DistributionModel, RuntimeCapabilities, RuntimeDetection, RuntimeDoctorCheck, RuntimeEvent,
    RuntimeId, RuntimeInstallOutcome, RuntimeInstallPlan, RuntimeMetadata, RuntimeResult,
    RuntimeRunRequest,
};
use openwork_core::{ErrorCode, OpenWorkError, redact_text};
use std::sync::Arc;
use std::time::Duration;

/// External-managed adapter for the official Pi coding-agent CLI.
pub struct PiRuntime {
    runner: Arc<dyn CommandRunner>,
}

impl PiRuntime {
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
    fn executable(&self) -> Option<std::path::PathBuf> {
        self.runner.find_executable("pi")
    }
    fn command(&self, arguments: &[&str]) -> RuntimeResult<crate::CommandOutput> {
        let executable = self
            .executable()
            .ok_or_else(|| OpenWorkError::new(ErrorCode::RuntimeNotFound, "Pi is not installed"))?;
        self.runner.run(
            &CommandSpec::new(
                executable,
                arguments
                    .iter()
                    .map(|argument| (*argument).to_owned())
                    .collect(),
                Duration::from_secs(8),
            ),
            &CancellationToken::new(),
        )
    }
    fn external_managed(operation: &str) -> OpenWorkError {
        OpenWorkError::new(
            ErrorCode::RuntimeUnhealthy,
            format!("Pi {operation} is managed outside OpenWork"),
        )
        .with_remediation(
            "Install or update Pi using the official upstream instructions, then retry detection.",
        )
    }
}

impl AgentRuntime for PiRuntime {
    fn metadata(&self) -> RuntimeMetadata {
        RuntimeMetadata {
            id: RuntimeId::from("pi"),
            name: "Pi".to_owned(),
            upstream: "https://github.com/badlogic/pi-mono".to_owned(),
            license: "MIT".to_owned(),
            distribution: DistributionModel::ExternalManaged,
        }
    }
    fn detect(&self) -> RuntimeResult<RuntimeDetection> {
        let Some(executable) = self.executable() else {
            return Ok(RuntimeDetection {
                state: DetectionState::Missing,
                executable: None,
                details: None,
            });
        };
        let output = self.command(&["--version"])?;
        let healthy = !output.timed_out && !output.cancelled && output.exit_code == Some(0);
        Ok(RuntimeDetection {
            state: if healthy {
                DetectionState::Healthy
            } else {
                DetectionState::Broken
            },
            executable: Some(executable),
            details: (!healthy).then(|| {
                if output.timed_out {
                    "Pi version check timed out".to_owned()
                } else {
                    redact_text(&output.stderr)
                }
            }),
        })
    }
    fn install_plan(&self, _: Option<&str>) -> RuntimeResult<RuntimeInstallPlan> {
        Err(Self::external_managed("installation"))
    }
    fn install(&self, _: &RuntimeInstallPlan) -> RuntimeResult<RuntimeInstallOutcome> {
        Err(Self::external_managed("installation"))
    }
    fn uninstall(&self) -> RuntimeResult<()> {
        Err(Self::external_managed("uninstall"))
    }
    fn version(&self) -> RuntimeResult<Option<String>> {
        if self.executable().is_none() {
            return Ok(None);
        }
        let output = self.command(&["--version"])?;
        Ok((output.exit_code == Some(0) && !output.timed_out).then(|| {
            redact_text(if output.stdout.trim().is_empty() {
                output.stderr.trim()
            } else {
                output.stdout.trim()
            })
        }))
    }
    fn update(&self, _: Option<&str>) -> RuntimeResult<RuntimeInstallOutcome> {
        Err(Self::external_managed("update"))
    }
    fn doctor(&self) -> RuntimeResult<Vec<RuntimeDoctorCheck>> {
        let detection = self.detect()?;
        Ok(vec![RuntimeDoctorCheck {
            id: "pi.detect".to_owned(),
            healthy: detection.state == DetectionState::Healthy,
            summary: format!("Pi is {:?}", detection.state),
            remediation: (detection.state != DetectionState::Healthy)
                .then(|| "Install or repair Pi using its official upstream installer.".to_owned()),
        }])
    }
    fn auth_status(&self) -> RuntimeResult<AuthStatus> {
        Ok(AuthStatus::Unknown)
    }
    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            install: false,
            uninstall: false,
            update: false,
            authenticate: false,
            run: false,
            cancel: false,
        }
    }
    fn run(
        &self,
        _: &RuntimeRunRequest,
        _: &CancellationToken,
    ) -> RuntimeResult<Vec<RuntimeEvent>> {
        Err(Self::external_managed("host execution"))
    }
    fn cancel(&self, _: &CancellationToken) -> RuntimeResult<()> {
        Err(Self::external_managed("host cancellation"))
    }
}
