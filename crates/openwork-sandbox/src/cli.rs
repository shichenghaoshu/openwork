use crate::sandbox_error;
use openwork_core::{ErrorCode, OpenWorkError};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Bounded result of one Docker CLI invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

/// Narrow command boundary used by the production runner and deterministic fakes.
pub trait DockerCli: Send + Sync {
    /// Runs Docker without a shell and retains at most `max_output_bytes` in total.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the process cannot run, times out, or cannot be read.
    fn run(
        &self,
        arguments: &[OsString],
        max_output_bytes: u64,
        timeout: Duration,
    ) -> Result<CliOutput, OpenWorkError>;
}

/// Docker CLI process runner with an empty inherited environment and bounded pipes.
#[derive(Clone, Debug)]
pub struct SystemDockerCli {
    executable: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}

impl SystemDockerCli {
    /// Creates a runner for an absolute Docker executable path.
    ///
    /// # Errors
    ///
    /// Rejects relative paths. The executable is intentionally not discovered via `PATH`.
    pub fn new(executable: PathBuf) -> Result<Self, OpenWorkError> {
        if !executable.is_absolute() {
            return Err(sandbox_error(
                ErrorCode::InvalidArguments,
                "Docker executable must be an absolute path",
            ));
        }
        Ok(Self {
            executable,
            environment: BTreeMap::new(),
        })
    }

    /// Adds an explicit non-secret environment value needed by a local Docker transport.
    ///
    /// Values are never forwarded into the task container.
    #[must_use]
    pub fn with_cli_environment(mut self, key: OsString, value: OsString) -> Self {
        self.environment.insert(key, value);
        self
    }
}

impl DockerCli for SystemDockerCli {
    fn run(
        &self,
        arguments: &[OsString],
        max_output_bytes: u64,
        timeout: Duration,
    ) -> Result<CliOutput, OpenWorkError> {
        let mut child = Command::new(&self.executable)
            .args(arguments)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| {
                sandbox_error(ErrorCode::SandboxUnavailable, "Docker CLI could not start")
            })?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(sandbox_error(
                ErrorCode::Internal,
                "Docker output pipes were unavailable",
            ));
        };
        let budget = Arc::new(Mutex::new(CaptureBudget::new(max_output_bytes)));
        let stdout_reader = spawn_reader(stdout, Arc::clone(&budget));
        let stderr_reader = spawn_reader(stderr, Arc::clone(&budget));
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = join_reader(stdout_reader);
                    let _ = join_reader(stderr_reader);
                    return Err(sandbox_error(
                        ErrorCode::ExecutionFailed,
                        "Docker CLI status failed",
                    ));
                }
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_reader(stdout_reader);
                let _ = join_reader(stderr_reader);
                return Err(sandbox_error(
                    ErrorCode::RunTimedOut,
                    "Docker CLI invocation timed out",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        };
        let stdout = join_reader(stdout_reader)?;
        let stderr = join_reader(stderr_reader)?;
        let truncated = budget.lock().map_or(true, |state| state.truncated);
        Ok(CliOutput {
            success: status.success(),
            stdout,
            stderr,
            truncated,
        })
    }
}

#[derive(Debug)]
struct CaptureBudget {
    remaining: usize,
    truncated: bool,
}

impl CaptureBudget {
    fn new(limit: u64) -> Self {
        Self {
            remaining: usize::try_from(limit).unwrap_or(usize::MAX),
            truncated: false,
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    budget: Arc<Mutex<CaptureBudget>>,
) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut retained = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            let mut state = budget
                .lock()
                .map_err(|_| io::Error::other("capture budget lock poisoned"))?;
            let keep = read.min(state.remaining);
            retained.extend_from_slice(&chunk[..keep]);
            state.remaining -= keep;
            state.truncated |= keep < read;
        }
        Ok(retained)
    })
}

fn join_reader(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, OpenWorkError> {
    handle
        .join()
        .map_err(|_| sandbox_error(ErrorCode::Internal, "Docker output reader panicked"))?
        .map_err(|_| sandbox_error(ErrorCode::Io, "Docker output could not be read"))
}
