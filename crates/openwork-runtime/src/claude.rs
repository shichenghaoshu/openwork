use crate::{
    AgentRuntime, AuthStatus, CancellationToken, CommandRunner, CommandSpec, DetectionState,
    DistributionModel, DownloadRequest, Downloader, RuntimeCapabilities, RuntimeDetection,
    RuntimeDoctorCheck, RuntimeEvent, RuntimeId, RuntimeInstallOutcome, RuntimeInstallPlan,
    RuntimeMetadata, RuntimeResult, RuntimeRunRequest,
};
use openwork_core::{ErrorCode, OpenWorkError, redact_text};
use openwork_platform::{OperatingSystem, PlatformInfo};
use std::sync::Arc;
use std::time::Duration;

const UNIX_INSTALL_URL: &str = "https://claude.ai/install.sh";
const WINDOWS_INSTALL_URL: &str = "https://claude.ai/install.ps1";

pub struct ClaudeRuntime {
    runner: Arc<dyn CommandRunner>,
    downloader: Option<Arc<dyn Downloader>>,
    platform: PlatformInfo,
}

impl ClaudeRuntime {
    #[must_use]
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        downloader: Option<Arc<dyn Downloader>>,
        platform: PlatformInfo,
    ) -> Self {
        Self {
            runner,
            downloader,
            platform,
        }
    }

    fn executable(&self) -> Option<std::path::PathBuf> {
        self.runner.find_executable("claude")
    }

    fn command(&self, arguments: &[&str]) -> RuntimeResult<crate::CommandOutput> {
        let executable = self.executable().ok_or_else(|| {
            OpenWorkError::new(ErrorCode::RuntimeNotFound, "Claude Code is not installed")
        })?;
        self.runner.run(
            &CommandSpec::new(
                executable,
                arguments.iter().map(|value| (*value).to_owned()).collect(),
                Duration::from_secs(8),
            ),
            &CancellationToken::new(),
        )
    }

    fn execute_plan(&self, plan: &RuntimeInstallPlan) -> RuntimeResult<RuntimeInstallOutcome> {
        let downloader = self.downloader.as_ref().ok_or_else(|| {
            OpenWorkError::new(
                ErrorCode::InstallFailed,
                "Claude Code installer execution is unavailable in read-only mode",
            )
            .with_remediation("Use `openwork install` with the managed installer executor.")
        })?;
        let cancellation = CancellationToken::new();
        for request in &plan.downloads {
            downloader.download(request, &cancellation)?;
        }
        for command in &plan.commands {
            let output = self.runner.run(command, &cancellation)?;
            if output.timed_out || output.cancelled || output.exit_code != Some(0) {
                return Err(OpenWorkError::new(
                    ErrorCode::InstallFailed,
                    format!("Claude Code official installer failed: {}", output.stderr),
                ));
            }
        }
        Ok(RuntimeInstallOutcome {
            installed: true,
            version: self.version()?,
            executable: self.executable(),
        })
    }

    fn unsupported(operation: &str) -> OpenWorkError {
        OpenWorkError::new(
            ErrorCode::RuntimeUnhealthy,
            format!("Claude Code {operation} is not supported by the Bootstrap adapter"),
        )
    }
}

impl AgentRuntime for ClaudeRuntime {
    fn metadata(&self) -> RuntimeMetadata {
        RuntimeMetadata {
            id: RuntimeId::from("claude-code"),
            name: "Claude Code".to_owned(),
            upstream: "https://github.com/anthropics/claude-code".to_owned(),
            license: "Anthropic commercial terms".to_owned(),
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
        let details = if healthy {
            None
        } else if output.timed_out {
            Some("Claude Code version check timed out".to_owned())
        } else {
            Some(redact_text(&output.stderr))
        };
        Ok(RuntimeDetection {
            state: if healthy {
                DetectionState::Healthy
            } else {
                DetectionState::Broken
            },
            executable: Some(executable),
            details,
        })
    }

    fn install_plan(&self, version: Option<&str>) -> RuntimeResult<RuntimeInstallPlan> {
        let windows = self.platform.os == OperatingSystem::Windows;
        let source_url = if windows {
            WINDOWS_INSTALL_URL
        } else {
            UNIX_INSTALL_URL
        };
        let extension = if windows { "ps1" } else { "sh" };
        let destination = self
            .platform
            .paths
            .cache
            .join(format!("downloads/claude-install.{extension}"));
        let command = if windows {
            CommandSpec::new(
                "powershell.exe",
                vec![
                    "-NoProfile".to_owned(),
                    "-ExecutionPolicy".to_owned(),
                    "Bypass".to_owned(),
                    "-File".to_owned(),
                    destination.to_string_lossy().into_owned(),
                ],
                Duration::from_mins(5),
            )
        } else {
            CommandSpec::new(
                "/bin/bash",
                vec![destination.to_string_lossy().into_owned()],
                Duration::from_mins(5),
            )
        };
        let mut warnings = vec![
            "Claude Code remains external managed and is never redistributed by OpenWork."
                .to_owned(),
            "Anthropic does not publish a checksum beside this bootstrap URL; provenance records verification as unavailable."
                .to_owned(),
        ];
        if version.is_some() {
            warnings.push(
                "The official bootstrap installer does not expose a documented version pin; the requested version is advisory."
                    .to_owned(),
            );
        }
        Ok(RuntimeInstallPlan {
            source_url: source_url.to_owned(),
            version: version.map(str::to_owned),
            downloads: vec![DownloadRequest {
                url: source_url.to_owned(),
                destination,
                expected_sha256: None,
                timeout_millis: 30_000,
            }],
            commands: vec![command],
            warnings,
        })
    }

    fn install(&self, plan: &RuntimeInstallPlan) -> RuntimeResult<RuntimeInstallOutcome> {
        let detection = self.detect()?;
        if detection.state != DetectionState::Missing {
            return Ok(RuntimeInstallOutcome {
                installed: detection.state == DetectionState::Healthy,
                version: self.version()?,
                executable: detection.executable,
            });
        }
        self.execute_plan(plan)
    }

    fn uninstall(&self) -> RuntimeResult<()> {
        Err(Self::unsupported("uninstall"))
    }

    fn version(&self) -> RuntimeResult<Option<String>> {
        let Some(_) = self.executable() else {
            return Ok(None);
        };
        let output = self.command(&["--version"])?;
        if output.exit_code == Some(0) && !output.timed_out {
            let version = if output.stdout.trim().is_empty() {
                output.stderr.trim()
            } else {
                output.stdout.trim()
            };
            Ok(Some(redact_text(version)))
        } else {
            Ok(None)
        }
    }

    fn update(&self, version: Option<&str>) -> RuntimeResult<RuntimeInstallOutcome> {
        self.execute_plan(&self.install_plan(version)?)
    }

    fn doctor(&self) -> RuntimeResult<Vec<RuntimeDoctorCheck>> {
        let detection = self.detect()?;
        Ok(vec![RuntimeDoctorCheck {
            id: "claude.detect".to_owned(),
            healthy: detection.state == DetectionState::Healthy,
            summary: format!("Claude Code is {:?}", detection.state),
            remediation: (detection.state != DetectionState::Healthy)
                .then(|| "Review `openwork runtime info claude-code` or the official Anthropic setup guide.".to_owned()),
        }])
    }

    fn auth_status(&self) -> RuntimeResult<AuthStatus> {
        if self.executable().is_none() {
            return Ok(AuthStatus::Unknown);
        }
        let output = self.command(&["auth", "status", "--json"])?;
        if output.exit_code != Some(0) || output.timed_out {
            return Ok(AuthStatus::Unknown);
        }
        let value: serde_json::Value = serde_json::from_str(&output.stdout).unwrap_or_default();
        Ok(
            match value.get("loggedIn").and_then(serde_json::Value::as_bool) {
                Some(true) => AuthStatus::Authenticated,
                Some(false) => AuthStatus::Unauthenticated,
                None => AuthStatus::Unknown,
            },
        )
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            install: true,
            uninstall: false,
            update: true,
            authenticate: true,
            run: false,
            cancel: false,
        }
    }

    fn run(
        &self,
        _: &RuntimeRunRequest,
        _: &CancellationToken,
    ) -> RuntimeResult<Vec<RuntimeEvent>> {
        Err(Self::unsupported("run"))
    }

    fn cancel(&self, _: &CancellationToken) -> RuntimeResult<()> {
        Err(Self::unsupported("cancel"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandOutput, DownloadReceipt};
    use openwork_platform::{
        Architecture, HostEnvironment, OpenWorkPaths, PermissionFacts, PrerequisiteFacts,
        ResourceFacts, SupportTier,
    };
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeRunner {
        executable: bool,
        outputs: Mutex<VecDeque<CommandOutput>>,
    }
    impl CommandRunner for FakeRunner {
        fn find_executable(&self, _: &str) -> Option<PathBuf> {
            self.executable.then(|| PathBuf::from("/fixture/claude"))
        }
        fn run(&self, _: &CommandSpec, _: &CancellationToken) -> RuntimeResult<CommandOutput> {
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| OpenWorkError::new(ErrorCode::Internal, "missing fake output"))
        }
    }
    #[derive(Default)]
    struct FakeDownloader {
        calls: Mutex<usize>,
    }
    impl Downloader for FakeDownloader {
        fn download(
            &self,
            _: &DownloadRequest,
            _: &CancellationToken,
        ) -> RuntimeResult<DownloadReceipt> {
            *self.calls.lock().unwrap() += 1;
            Ok(DownloadReceipt {
                bytes_written: 1,
                observed_sha256: "fixture".to_owned(),
                verified: false,
            })
        }
    }
    fn output(code: Option<i32>, stdout: &str, stderr: &str, timed_out: bool) -> CommandOutput {
        CommandOutput {
            exit_code: code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            timed_out,
            cancelled: false,
            truncated: false,
        }
    }
    fn platform(os: OperatingSystem) -> PlatformInfo {
        let root = PathBuf::from("/fixture");
        PlatformInfo {
            schema_version: 1,
            os,
            os_version: None,
            architecture: Architecture::Arm64,
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
    fn runtime(
        executable: bool,
        outputs: Vec<CommandOutput>,
        os: OperatingSystem,
    ) -> ClaudeRuntime {
        ClaudeRuntime::new(
            Arc::new(FakeRunner {
                executable,
                outputs: Mutex::new(outputs.into()),
            }),
            None,
            platform(os),
        )
    }

    #[test]
    fn detects_missing_healthy_broken_and_timeout_states() {
        assert_eq!(
            runtime(false, vec![], OperatingSystem::MacOs)
                .detect()
                .unwrap()
                .state,
            DetectionState::Missing
        );
        assert_eq!(
            runtime(
                true,
                vec![output(Some(0), "2.1.226", "", false)],
                OperatingSystem::MacOs
            )
            .detect()
            .unwrap()
            .state,
            DetectionState::Healthy
        );
        assert_eq!(
            runtime(
                true,
                vec![output(Some(1), "", "broken", false)],
                OperatingSystem::MacOs
            )
            .detect()
            .unwrap()
            .state,
            DetectionState::Broken
        );
        assert_eq!(
            runtime(
                true,
                vec![output(None, "", "", true)],
                OperatingSystem::MacOs
            )
            .detect()
            .unwrap()
            .state,
            DetectionState::Broken
        );
    }

    #[test]
    fn parses_auth_without_exposing_auth_payload() {
        let runtime = runtime(
            true,
            vec![output(
                Some(0),
                r#"{"loggedIn":false,"token":"synthetic"}"#,
                "",
                false,
            )],
            OperatingSystem::MacOs,
        );
        assert_eq!(runtime.auth_status().unwrap(), AuthStatus::Unauthenticated);
    }

    #[test]
    fn plans_only_official_platform_sources() {
        let unix = runtime(false, vec![], OperatingSystem::Linux)
            .install_plan(None)
            .unwrap();
        assert_eq!(unix.source_url, UNIX_INSTALL_URL);
        assert_eq!(unix.commands[0].program, PathBuf::from("/bin/bash"));
        let windows = runtime(false, vec![], OperatingSystem::Windows)
            .install_plan(Some("2"))
            .unwrap();
        assert_eq!(windows.source_url, WINDOWS_INSTALL_URL);
        assert!(
            windows.commands[0]
                .arguments
                .iter()
                .any(|argument| argument == "-File")
        );
    }

    #[test]
    fn managed_install_uses_injected_downloader_and_runner() {
        let runner = Arc::new(FakeRunner {
            executable: false,
            outputs: Mutex::new(vec![output(Some(0), "", "", false)].into()),
        });
        let downloader = Arc::new(FakeDownloader::default());
        let runtime = ClaudeRuntime::new(
            runner,
            Some(downloader.clone()),
            platform(OperatingSystem::MacOs),
        );
        let outcome = runtime
            .install(&runtime.install_plan(None).unwrap())
            .unwrap();
        assert!(outcome.installed);
        assert_eq!(*downloader.calls.lock().unwrap(), 1);
    }
}
