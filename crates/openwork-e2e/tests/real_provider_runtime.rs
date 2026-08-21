//! Credential-gated, `HostOnly` provider probes.
//!
//! These tests are deliberately ignored. They execute a locally installed
//! provider CLI only after an operator opts in with all of:
//! `OPENWORK_REAL_RUNTIME_TESTS=1`, `OPENWORK_REAL_RUNTIME_PROVIDER`, an
//! absolute `OPENWORK_REAL_RUNTIME_BIN`, and `OPENWORK_REAL_RUNTIME_AUTH=1`.
//! The auth gate is an operator assertion because providers can authenticate
//! through their own credential stores; no credential name or value is read or
//! emitted by this harness. This is not a Docker sandbox E2E: it proves only
//! the real provider CLI JSONL boundary and decoder contract on an operator's host.

use openwork_execution::{
    EXECUTION_SCHEMA_VERSION, RunId, RuntimeEventPayload, RuntimeTask, SandboxCleanupStatus,
    SandboxResult, SandboxTermination, UtcTimestamp,
};
use openwork_runtime::task::{
    CLAUDE_RUNTIME_ID, CODEX_RUNTIME_ID, ClaudeTaskAdapter, CodexTaskAdapter, RuntimeTaskAdapter,
    decode_sandbox_result,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::env;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const DEFAULT_TIMEOUT_SECONDS: u64 = 90;
const MAX_TIMEOUT_SECONDS: u64 = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Provider {
    Claude,
    Codex,
}

impl Provider {
    fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            CLAUDE_RUNTIME_ID => Ok(Self::Claude),
            CODEX_RUNTIME_ID => Ok(Self::Codex),
            _ => Err("provider must be claude-code or codex"),
        }
    }

    const fn runtime_id(self) -> &'static str {
        match self {
            Self::Claude => CLAUDE_RUNTIME_ID,
            Self::Codex => CODEX_RUNTIME_ID,
        }
    }
}

struct RuntimeHarnessConfig {
    provider: Provider,
    binary: PathBuf,
    timeout: Duration,
}

impl RuntimeHarnessConfig {
    fn from_environment() -> Result<Self, &'static str> {
        let enabled = env::var("OPENWORK_REAL_RUNTIME_TESTS").ok();
        let provider = env::var("OPENWORK_REAL_RUNTIME_PROVIDER").ok();
        let binary = env::var("OPENWORK_REAL_RUNTIME_BIN").ok();
        let authenticated = env::var("OPENWORK_REAL_RUNTIME_AUTH").ok();
        Self::from_values(
            enabled.as_deref(),
            provider.as_deref(),
            binary.as_deref(),
            authenticated.as_deref(),
            env::var("OPENWORK_REAL_RUNTIME_TIMEOUT_SECONDS")
                .ok()
                .as_deref(),
        )
    }

    fn from_values(
        enabled: Option<&str>,
        provider: Option<&str>,
        binary: Option<&str>,
        authenticated: Option<&str>,
        timeout: Option<&str>,
    ) -> Result<Self, &'static str> {
        if enabled != Some("1") {
            return Err("set OPENWORK_REAL_RUNTIME_TESTS=1 to enable this probe");
        }
        if authenticated != Some("1") {
            return Err("set OPENWORK_REAL_RUNTIME_AUTH=1 after authenticating the selected CLI");
        }
        let provider =
            Provider::parse(provider.ok_or("OPENWORK_REAL_RUNTIME_PROVIDER is required")?)?;
        let binary = PathBuf::from(binary.ok_or("OPENWORK_REAL_RUNTIME_BIN is required")?);
        if !binary.is_absolute() || !binary.is_file() {
            return Err("OPENWORK_REAL_RUNTIME_BIN must be an absolute regular file");
        }
        let timeout_seconds = timeout
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "runtime timeout must be an integer")
            })
            .transpose()?
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        if !(1..=MAX_TIMEOUT_SECONDS).contains(&timeout_seconds) {
            return Err("runtime timeout must be between 1 and 300 seconds");
        }
        Ok(Self {
            provider,
            binary,
            timeout: Duration::from_secs(timeout_seconds),
        })
    }
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

#[test]
#[ignore = "requires explicit, authenticated local Claude Code or Codex CLI access (HostOnly)"]
fn real_provider_host_only_jsonl_is_bounded_and_decodes_to_a_terminal_event() {
    let config = RuntimeHarnessConfig::from_environment().expect("real-provider configuration");
    let workspace = tempfile::tempdir().expect("temporary working directory");
    let prompt = "Reply with a short confirmation that the local provider probe ran. Do not access files, tools, networks, or secrets.";
    let task = runtime_task(config.provider.runtime_id(), prompt);
    let invocation = match config.provider {
        Provider::Claude => {
            ClaudeTaskAdapter::new(config.binary.display().to_string()).prepare(&task)
        }
        Provider::Codex => {
            CodexTaskAdapter::new(config.binary.display().to_string()).prepare(&task)
        }
    }
    .expect("provider invocation preparation");

    assert_eq!(invocation.stdin, prompt.as_bytes());
    assert!(
        !invocation
            .command
            .arguments()
            .iter()
            .any(|argument| argument.contains(prompt)),
        "prompt must be supplied only on stdin"
    );

    let captured = run_bounded(&invocation, workspace.path(), config.timeout)
        .expect("real provider HostOnly process");
    assert!(
        !captured.stdout.truncated,
        "provider stdout exceeded capture limit"
    );
    assert!(
        !captured.stderr.truncated,
        "provider stderr exceeded capture limit"
    );
    assert_eq!(captured.exit_code, 0, "provider exited unsuccessfully");

    let completed_at = UtcTimestamp::now();
    let result = SandboxResult {
        schema_version: EXECUTION_SCHEMA_VERSION,
        run_id: task.run_id.clone(),
        sandbox_id: "real-provider-host-only-probe".to_owned(),
        termination: SandboxTermination::Exited,
        exit_code: Some(captured.exit_code),
        stdout: String::from_utf8(captured.stdout.bytes).expect("provider stdout must be UTF-8"),
        stderr: String::from_utf8(captured.stderr.bytes).expect("provider stderr must be UTF-8"),
        truncated: false,
        started_at: completed_at,
        completed_at,
        output_paths: Vec::new(),
        cleanup: SandboxCleanupStatus::Succeeded,
    };
    let mut decoder = match config.provider {
        Provider::Claude => {
            ClaudeTaskAdapter::new(config.binary.display().to_string()).decoder(task.run_id)
        }
        Provider::Codex => {
            CodexTaskAdapter::new(config.binary.display().to_string()).decoder(task.run_id)
        }
    };
    let events =
        decode_sandbox_result(&result, decoder.as_mut()).expect("validated provider output");
    assert!(
        matches!(
            events.last().map(|event| &event.payload),
            Some(RuntimeEventPayload::Completed { exit_code: 0 })
        ),
        "provider decoder must produce an observed successful terminal event"
    );
}

#[test]
fn provider_harness_configuration_fails_closed() {
    assert!(RuntimeHarnessConfig::from_values(None, None, None, None, None).is_err());
    assert!(
        RuntimeHarnessConfig::from_values(
            Some("1"),
            Some("unknown"),
            Some("/bin/sh"),
            Some("1"),
            None
        )
        .is_err()
    );
    assert!(
        RuntimeHarnessConfig::from_values(
            Some("1"),
            Some(CODEX_RUNTIME_ID),
            Some("relative-bin"),
            Some("1"),
            None
        )
        .is_err()
    );
    assert!(
        RuntimeHarnessConfig::from_values(
            Some("1"),
            Some(CODEX_RUNTIME_ID),
            Some("/bin/sh"),
            Some("0"),
            None
        )
        .is_err()
    );
    assert!(
        RuntimeHarnessConfig::from_values(
            Some("1"),
            Some(CODEX_RUNTIME_ID),
            Some("/bin/sh"),
            Some("1"),
            Some("301")
        )
        .is_err()
    );
}

struct ProcessCapture {
    stdout: CapturedOutput,
    stderr: CapturedOutput,
    exit_code: i32,
}

fn run_bounded(
    invocation: &openwork_runtime::task::RuntimeInvocation,
    host_working_directory: &Path,
    timeout: Duration,
) -> Result<ProcessCapture, &'static str> {
    let mut command = Command::new(invocation.command.program());
    command
        .args(invocation.command.arguments())
        .current_dir(host_working_directory)
        .env_clear();
    copy_environment_allowlist(&mut command);
    command.envs(invocation.command.environment());
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "unable to start configured provider binary")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("provider stdin pipe unavailable")?;
    stdin
        .write_all(&invocation.stdin)
        .and_then(|()| stdin.flush())
        .map_err(|_| "unable to write provider prompt to stdin")?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or("provider stdout pipe unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("provider stderr pipe unavailable")?;
    let stdout_reader = capture_output(stdout);
    let stderr_reader = capture_output(stderr);
    let started = Instant::now();
    let completion = loop {
        match child
            .try_wait()
            .map_err(|_| "unable to poll provider process")?
        {
            Some(status) => break ProcessCompletion::Exited(status),
            None if started.elapsed() >= timeout => {
                cancel_process(&mut child)?;
                break ProcessCompletion::TimedOut;
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "provider stdout reader failed")??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "provider stderr reader failed")??;
    let ProcessCompletion::Exited(status) = completion else {
        return Err("provider HostOnly probe timed out and was cancelled");
    };
    Ok(ProcessCapture {
        stdout,
        stderr,
        exit_code: status
            .code()
            .ok_or("provider process did not report an exit code")?,
    })
}

enum ProcessCompletion {
    Exited(ExitStatus),
    TimedOut,
}

fn cancel_process(child: &mut Child) -> Result<(), &'static str> {
    let killed = child.kill().is_ok();
    let reaped = child.wait().is_ok();
    if !killed || !reaped {
        return Err("unable to cancel and reap provider process");
    }
    Ok(())
}

fn copy_environment_allowlist(command: &mut Command) {
    // The CLI executable path is absolute, but Node-based wrappers may require
    // PATH. HOME/XDG locations allow the CLI's own authenticated credential
    // store without copying the surrounding process environment wholesale.
    const COMMON: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ];
    const PROVIDER_AUTH: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "OPENAI_API_KEY",
    ];
    for key in COMMON.iter().chain(PROVIDER_AUTH) {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn capture_output<R: Read + Send + 'static>(
    mut reader: R,
) -> thread::JoinHandle<Result<CapturedOutput, &'static str>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut truncated = false;
        let mut buffer = [0_u8; 8192];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| match error.kind() {
                    io::ErrorKind::Interrupted => "provider output read interrupted",
                    _ => "provider output read failed",
                })?;
            if count == 0 {
                return Ok(CapturedOutput { bytes, truncated });
            }
            let remaining = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
            let retained = count.min(remaining);
            bytes.extend_from_slice(&buffer[..retained]);
            truncated |= retained != count;
        }
    })
}

fn runtime_task(runtime: &str, prompt: &str) -> RuntimeTask {
    let prompt_hash = Sha256::digest(prompt.as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        },
    );
    serde_json::from_value(json!({
        "schema_version": 1,
        "run_id": RunId::generate(),
        "runtime": runtime,
        "prompt": prompt,
        "prompt_hash": prompt_hash,
        "working_directory": "/workspace",
        "timeout_seconds": DEFAULT_TIMEOUT_SECONDS,
        "capabilities": ["filesystem.read"],
    }))
    .expect("valid real-provider task")
}
