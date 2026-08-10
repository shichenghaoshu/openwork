//! Runtime contracts, registry, manifests, and external-managed adapters.

mod claude;
mod codex;
mod manifest;
mod mock;
mod registry;
mod system_downloader;

pub mod compatibility;

pub use claude::ClaudeRuntime;
pub use codex::CodexRuntime;
pub use manifest::{
    InstallerSource, RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest, VerificationPolicy,
    parse_manifest_json,
};
pub use mock::MockRuntime;
pub use registry::RuntimeRegistry;
pub use system_downloader::{DownloadPolicy, SystemDownloader};

use openwork_core::OpenWorkError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

pub type RuntimeResult<T> = Result<T, OpenWorkError>;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RuntimeId(pub String);

impl From<&str> for RuntimeId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for RuntimeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistributionModel {
    ExternalManaged,
    Embedded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeMetadata {
    pub id: RuntimeId,
    pub name: String,
    pub upstream: String,
    pub license: String,
    pub distribution: DistributionModel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionState {
    Missing,
    Healthy,
    Broken,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDetection {
    pub state: DetectionState,
    pub executable: Option<PathBuf>,
    pub details: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    Authenticated,
    Unauthenticated,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct RuntimeCapabilities {
    pub install: bool,
    pub uninstall: bool,
    pub update: bool,
    pub authenticate: bool,
    pub run: bool,
    pub cancel: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDoctorCheck {
    pub id: String,
    pub healthy: bool,
    pub summary: String,
    pub remediation: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeInstallPlan {
    pub source_url: String,
    pub version: Option<String>,
    pub downloads: Vec<DownloadRequest>,
    pub commands: Vec<CommandSpec>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeInstallOutcome {
    pub installed: bool,
    pub version: Option<String>,
    pub executable: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRunRequest {
    pub prompt: String,
    pub working_directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventKind {
    Started,
    Output,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub kind: RuntimeEventKind,
    pub message: String,
}

#[allow(clippy::missing_errors_doc)]
pub trait AgentRuntime: Send + Sync {
    fn metadata(&self) -> RuntimeMetadata;
    fn detect(&self) -> RuntimeResult<RuntimeDetection>;
    fn install_plan(&self, version: Option<&str>) -> RuntimeResult<RuntimeInstallPlan>;
    fn install(&self, plan: &RuntimeInstallPlan) -> RuntimeResult<RuntimeInstallOutcome>;
    fn uninstall(&self) -> RuntimeResult<()>;
    fn version(&self) -> RuntimeResult<Option<String>>;
    fn update(&self, version: Option<&str>) -> RuntimeResult<RuntimeInstallOutcome>;
    fn doctor(&self) -> RuntimeResult<Vec<RuntimeDoctorCheck>>;
    fn auth_status(&self) -> RuntimeResult<AuthStatus>;
    fn capabilities(&self) -> RuntimeCapabilities;
    fn run(
        &self,
        request: &RuntimeRunRequest,
        cancellation: &CancellationToken,
    ) -> RuntimeResult<Vec<RuntimeEvent>>;
    fn cancel(&self, cancellation: &CancellationToken) -> RuntimeResult<()>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<PathBuf>,
    pub timeout_millis: u64,
}

impl CommandSpec {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>, arguments: Vec<String>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            arguments,
            environment: BTreeMap::new(),
            working_directory: None,
            timeout_millis: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub truncated: bool,
}

#[allow(clippy::missing_errors_doc)]
pub trait CommandRunner: Send + Sync {
    fn find_executable(&self, executable: &str) -> Option<PathBuf>;

    fn run(
        &self,
        command: &CommandSpec,
        cancellation: &CancellationToken,
    ) -> RuntimeResult<CommandOutput>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn find_executable(&self, executable: &str) -> Option<PathBuf> {
        executable_on_path(executable)
    }

    fn run(
        &self,
        command: &CommandSpec,
        cancellation: &CancellationToken,
    ) -> RuntimeResult<CommandOutput> {
        let mut process = Command::new(&command.program);
        process.args(&command.arguments);
        process.envs(&command.environment);
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = &command.working_directory {
            process.current_dir(directory);
        }
        let mut child = process.spawn().map_err(command_error)?;
        let stdout = child.stdout.take().map(read_stream);
        let stderr = child.stderr.take().map(read_stream);
        let started = Instant::now();
        let timeout = Duration::from_millis(command.timeout_millis);
        let mut timed_out = false;
        let mut cancelled = false;

        let status = loop {
            if let Some(status) = child.try_wait().map_err(command_error)? {
                break status;
            }
            cancelled = cancellation.is_cancelled();
            timed_out = started.elapsed() >= timeout;
            if cancelled || timed_out {
                let _ = child.kill();
                break child.wait().map_err(command_error)?;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let (stdout, stdout_truncated) = join_stream(stdout)?;
        let (stderr, stderr_truncated) = join_stream(stderr)?;
        Ok(CommandOutput {
            exit_code: status.code(),
            stdout,
            stderr,
            timed_out,
            cancelled,
            truncated: stdout_truncated || stderr_truncated,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DownloadRequest {
    pub url: String,
    pub destination: PathBuf,
    pub expected_sha256: Option<String>,
    pub timeout_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DownloadReceipt {
    pub bytes_written: u64,
    pub observed_sha256: String,
    pub verified: bool,
}

#[allow(clippy::missing_errors_doc)]
pub trait Downloader: Send + Sync {
    fn download(
        &self,
        request: &DownloadRequest,
        cancellation: &CancellationToken,
    ) -> RuntimeResult<DownloadReceipt>;
}

fn read_stream(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<(String, bool)> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0_u8; 8192];
        let mut truncated = false;
        while let Ok(count) = stream.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let remaining = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
            captured.extend_from_slice(&buffer[..count.min(remaining)]);
            truncated |= count > remaining;
        }
        (String::from_utf8_lossy(&captured).into_owned(), truncated)
    })
}

fn join_stream(
    handle: Option<thread::JoinHandle<(String, bool)>>,
) -> RuntimeResult<(String, bool)> {
    handle.map_or(Ok((String::new(), false)), |reader| {
        reader.join().map_err(|_| {
            OpenWorkError::new(
                openwork_core::ErrorCode::Internal,
                "command output reader failed",
            )
        })
    })
}

#[allow(clippy::needless_pass_by_value)]
fn command_error(error: std::io::Error) -> OpenWorkError {
    OpenWorkError::new(
        openwork_core::ErrorCode::Io,
        format!("command execution failed: {error}"),
    )
}

fn executable_on_path(executable: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_owned());
    std::env::split_paths(&path).find_map(|directory| {
        let direct = directory.join(executable);
        if direct.is_file() {
            return Some(direct);
        }
        cfg!(windows).then(|| {
            extensions
                .split(';')
                .map(|extension| directory.join(format!("{executable}{extension}")))
                .find(|candidate| candidate.is_file())
        })?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_spec_preserves_argument_boundaries() {
        let command = CommandSpec::new(
            "tool",
            vec![
                "value with spaces".to_owned(),
                "$(never-expanded)".to_owned(),
            ],
            Duration::from_secs(2),
        );
        assert_eq!(command.program, PathBuf::from("tool"));
        assert_eq!(command.arguments.len(), 2);
        assert!(command.environment.is_empty());
    }

    #[test]
    fn cancellation_tokens_are_shared_and_monotonic() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn command_output_is_capture_bounded() {
        let input = vec![b'x'; MAX_CAPTURE_BYTES + 16];
        let (output, truncated) = read_stream(std::io::Cursor::new(input)).join().unwrap();
        assert_eq!(output.len(), MAX_CAPTURE_BYTES);
        assert!(truncated);
    }

    #[test]
    fn system_runner_enforces_timeout() {
        let command = CommandSpec::new(
            std::env::current_exe().unwrap(),
            vec![
                "--ignored".to_owned(),
                "--exact".to_owned(),
                "tests::sleep_helper".to_owned(),
            ],
            Duration::from_millis(40),
        );
        let output = SystemCommandRunner
            .run(&command, &CancellationToken::new())
            .unwrap();
        assert!(output.timed_out);
        assert!(!output.cancelled);
    }

    #[test]
    fn system_runner_honors_cancellation() {
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            cancellation.cancel();
        });
        let command = CommandSpec::new(
            std::env::current_exe().unwrap(),
            vec![
                "--ignored".to_owned(),
                "--exact".to_owned(),
                "tests::sleep_helper".to_owned(),
            ],
            Duration::from_secs(5),
        );
        let output = SystemCommandRunner.run(&command, &token).unwrap();
        canceller.join().unwrap();
        assert!(output.cancelled);
        assert!(!output.timed_out);
    }

    #[test]
    #[ignore = "subprocess helper for timeout and cancellation tests"]
    fn sleep_helper() {
        thread::sleep(Duration::from_secs(2));
    }
}
