//! OpenWork-owned MCP discovery gateway.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    env, fmt,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::ConfigError;

const GITHUB_IMAGE: &str = "ghcr.io/github/github-mcp-server@sha256:1817b57d43916532dc002bdc5f344d639bd9fb54a9148d42168458f7c3280567";
const LARK_PACKAGE: &str = "@larksuiteoapi/lark-mcp@0.5.1";
const LARK_READ_TOOLS: &str = "im.v1.chat.search,im.v1.message.list,bitable.v1.appTableRecord.search,docx.v1.document.rawContent,wiki.v1.node.search,wiki.v2.space.getNode";
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOOLS: usize = 512;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_TEXT_BYTES: usize = 4 * 1024;

/// Environment-derived MCP connector definitions.
#[derive(Clone)]
pub(super) struct ConnectorRuntimeConfig {
    definitions: Vec<ConnectorDefinition>,
}

impl ConnectorRuntimeConfig {
    pub(super) fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            definitions: vec![github_from_env()?, lark_from_env()?],
        })
    }
}

/// Safe connector information returned to Employee Workspace.
#[derive(Clone, Debug, Serialize)]
pub(super) struct ConnectorSummary {
    pub id: String,
    pub name: String,
    pub status: ConnectorStatus,
    pub tool_count: Option<usize>,
    pub last_error: Option<&'static str>,
}

/// Connector availability without credential or process details.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConnectorStatus {
    NotConfigured,
    Ready,
    Unavailable,
    Unknown,
}

/// Redacted MCP tool metadata. Input schemas are represented only by a digest.
#[derive(Clone, Debug, Serialize)]
pub(super) struct ConnectorTool {
    pub id: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub read_only: bool,
    pub input_schema_sha256: String,
}

#[derive(Clone)]
pub(super) struct ConnectorRegistry {
    entries: Arc<BTreeMap<String, ConnectorDefinition>>,
    cache: Arc<Mutex<HashMap<String, CacheEntry>>>,
    success_ttl: Duration,
    failure_ttl: Duration,
}

impl ConnectorRegistry {
    pub(super) fn new(config: ConnectorRuntimeConfig) -> Self {
        let entries = config
            .definitions
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect();
        Self {
            entries: Arc::new(entries),
            cache: Arc::new(Mutex::new(HashMap::new())),
            success_ttl: Duration::from_mins(1),
            failure_ttl: Duration::from_secs(15),
        }
    }

    pub(super) fn empty() -> Self {
        Self {
            entries: Arc::new(BTreeMap::new()),
            cache: Arc::new(Mutex::new(HashMap::new())),
            success_ttl: Duration::from_mins(1),
            failure_ttl: Duration::from_secs(15),
        }
    }

    pub(super) fn summaries(&self) -> Vec<ConnectorSummary> {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.entries
            .values()
            .map(|entry| {
                if entry.process.is_none() {
                    return ConnectorSummary {
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                        status: ConnectorStatus::NotConfigured,
                        tool_count: None,
                        last_error: None,
                    };
                }
                let cached = cache.get(&entry.id);
                ConnectorSummary {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    status: cached.map_or(ConnectorStatus::Unknown, |cached| {
                        if cached.result.is_ok() {
                            ConnectorStatus::Ready
                        } else {
                            ConnectorStatus::Unavailable
                        }
                    }),
                    tool_count: cached.and_then(|cached| cached.result.as_ref().ok().map(Vec::len)),
                    last_error: cached
                        .and_then(|cached| cached.result.as_ref().err().map(McpError::code)),
                }
            })
            .collect()
    }

    pub(super) fn tools(&self, id: &str) -> Result<Vec<ConnectorTool>, LookupError> {
        let entry = self.entries.get(id).ok_or(LookupError::NotFound)?;
        let process = entry.process.as_ref().ok_or(LookupError::NotConfigured)?;
        let now = Instant::now();
        if let Some(cached) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .filter(|cached| now.duration_since(cached.created_at) < cached.ttl)
            .cloned()
        {
            return cached.result.map_err(|_| LookupError::Unavailable);
        }

        let result = discover_tools(process);
        let ttl = if result.is_ok() {
            self.success_ttl
        } else {
            self.failure_ttl
        };
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id.to_owned(),
                CacheEntry {
                    created_at: now,
                    ttl,
                    result: result.clone(),
                },
            );
        result.map_err(|_| LookupError::Unavailable)
    }
}

#[derive(Clone)]
struct CacheEntry {
    created_at: Instant,
    ttl: Duration,
    result: Result<Vec<ConnectorTool>, McpError>,
}

#[derive(Debug)]
pub(super) enum LookupError {
    NotFound,
    NotConfigured,
    Unavailable,
}

#[derive(Clone)]
struct ConnectorDefinition {
    id: String,
    name: String,
    process: Option<ProcessConfig>,
}

#[derive(Clone)]
struct ProcessConfig {
    executable: PathBuf,
    args: Vec<String>,
    environment: BTreeMap<String, SecretValue>,
    timeout: Duration,
}

impl fmt::Debug for ProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessConfig")
            .field("executable", &self.executable)
            .field("argument_count", &self.args.len())
            .field("environment_keys", &self.environment.keys())
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Clone)]
struct SecretValue(String);

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug)]
enum McpError {
    Spawn,
    Io,
    Timeout,
    Protocol,
    OutputLimit,
}

impl McpError {
    const fn code(&self) -> &'static str {
        match self {
            Self::Spawn => "connector_spawn_failed",
            Self::Io => "connector_io_failed",
            Self::Timeout => "connector_timeout",
            Self::Protocol => "connector_protocol_error",
            Self::OutputLimit => "connector_output_limit",
        }
    }
}

fn github_from_env() -> Result<ConnectorDefinition, ConfigError> {
    let token = optional_nonempty_env("OPENWORK_GITHUB_TOKEN");
    let process = token
        .map(|token| {
            let executable =
                absolute_executable("OPENWORK_GITHUB_MCP_DOCKER_BIN", "/usr/local/bin/docker")?;
            let mut environment = BTreeMap::from([
                (
                    "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
                    SecretValue(token),
                ),
                (
                    "GITHUB_TOOLSETS".to_owned(),
                    SecretValue("repos,issues,pull_requests,actions,users".to_owned()),
                ),
                ("GITHUB_READ_ONLY".to_owned(), SecretValue("1".to_owned())),
            ]);
            if let Some(host) = optional_nonempty_env("OPENWORK_DOCKER_HOST") {
                environment.insert("DOCKER_HOST".to_owned(), SecretValue(host));
            }
            let mut args = vec!["run".to_owned(), "-i".to_owned(), "--rm".to_owned()];
            for key in environment
                .keys()
                .filter(|key| key.as_str() != "DOCKER_HOST")
            {
                args.push("-e".to_owned());
                args.push(key.clone());
            }
            args.push(GITHUB_IMAGE.to_owned());
            Ok(ProcessConfig {
                executable,
                args,
                environment,
                timeout: Duration::from_secs(20),
            })
        })
        .transpose()?;
    Ok(ConnectorDefinition {
        id: "github".to_owned(),
        name: "GitHub".to_owned(),
        process,
    })
}

fn lark_from_env() -> Result<ConnectorDefinition, ConfigError> {
    let app_id = optional_nonempty_env("OPENWORK_LARK_APP_ID");
    let app_secret = optional_nonempty_env("OPENWORK_LARK_APP_SECRET");
    let process = match (app_id, app_secret) {
        (None, None) => None,
        (Some(app_id), Some(app_secret)) => {
            let executable =
                absolute_executable("OPENWORK_LARK_MCP_NPX_BIN", "/usr/local/bin/npx")?;
            let domain = optional_nonempty_env("OPENWORK_LARK_DOMAIN")
                .unwrap_or_else(|| "https://open.feishu.cn".to_owned());
            let mut environment = BTreeMap::from([
                ("APP_ID".to_owned(), SecretValue(app_id)),
                ("APP_SECRET".to_owned(), SecretValue(app_secret)),
                (
                    "LARK_TOOLS".to_owned(),
                    SecretValue(LARK_READ_TOOLS.to_owned()),
                ),
                ("LARK_DOMAIN".to_owned(), SecretValue(domain)),
                (
                    "LARK_TOKEN_MODE".to_owned(),
                    SecretValue("tenant_access_token".to_owned()),
                ),
                (
                    "PATH".to_owned(),
                    SecretValue("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".to_owned()),
                ),
            ]);
            if let Some(home) = optional_nonempty_env("HOME") {
                environment.insert("HOME".to_owned(), SecretValue(home));
            }
            Some(ProcessConfig {
                executable,
                args: vec![
                    "-y".to_owned(),
                    LARK_PACKAGE.to_owned(),
                    "mcp".to_owned(),
                    "--token-mode".to_owned(),
                    "tenant_access_token".to_owned(),
                ],
                environment,
                timeout: Duration::from_secs(30),
            })
        }
        _ => {
            return Err(ConfigError(
                "OPENWORK_LARK_APP_ID and OPENWORK_LARK_APP_SECRET must be set together",
            ));
        }
    };
    Ok(ConnectorDefinition {
        id: "lark".to_owned(),
        name: "Feishu / Lark".to_owned(),
        process,
    })
}

fn optional_nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn absolute_executable(name: &'static str, default: &str) -> Result<PathBuf, ConfigError> {
    let value = env::var_os(name).map_or_else(|| PathBuf::from(default), PathBuf::from);
    if !value.is_absolute() {
        return Err(ConfigError(name));
    }
    Ok(value)
}

fn discover_tools(config: &ProcessConfig) -> Result<Vec<ConnectorTool>, McpError> {
    let mut child = spawn(config)?;
    let result = exchange(&mut child, config.timeout);
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

fn spawn(config: &ProcessConfig) -> Result<Child, McpError> {
    if !Path::new(&config.executable).is_absolute() {
        return Err(McpError::Spawn);
    }
    let mut command = Command::new(&config.executable);
    command
        .args(&config.args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &config.environment {
        command.env(key, &value.0);
    }
    command.spawn().map_err(|_| McpError::Spawn)
}

fn exchange(child: &mut Child, timeout: Duration) -> Result<Vec<ConnectorTool>, McpError> {
    let stdout = child.stdout.take().ok_or(McpError::Io)?;
    let stderr = child.stderr.take().ok_or(McpError::Io)?;
    thread::spawn(move || {
        let _ = std::io::copy(&mut BufReader::new(stderr), &mut std::io::sink());
    });
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = sender.send(Err(McpError::Io));
                    break;
                }
            }
        }
    });

    let stdin = child.stdin.as_mut().ok_or(McpError::Io)?;
    send_json(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "openwork-control", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
    )?;
    let deadline = Instant::now() + timeout;
    let initialize = receive_response(&receiver, 1, deadline)?;
    if initialize.get("result").is_none() {
        return Err(McpError::Protocol);
    }
    send_json(
        stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    send_json(
        stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )?;
    let response = receive_response(&receiver, 2, deadline)?;
    parse_tools(&response)
}

fn send_json(stdin: &mut impl Write, value: &Value) -> Result<(), McpError> {
    serde_json::to_writer(&mut *stdin, value).map_err(|_| McpError::Io)?;
    stdin.write_all(b"\n").map_err(|_| McpError::Io)?;
    stdin.flush().map_err(|_| McpError::Io)
}

fn receive_response(
    receiver: &mpsc::Receiver<Result<String, McpError>>,
    expected_id: u64,
    deadline: Instant,
) -> Result<Value, McpError> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(McpError::Timeout)?;
        let line = receiver
            .recv_timeout(remaining)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => McpError::Timeout,
                mpsc::RecvTimeoutError::Disconnected => McpError::Io,
            })??;
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::OutputLimit);
        }
        let value: Value = serde_json::from_str(&line).map_err(|_| McpError::Protocol)?;
        if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return Ok(value);
        }
    }
}

fn parse_tools(response: &Value) -> Result<Vec<ConnectorTool>, McpError> {
    let tools = response
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or(McpError::Protocol)?;
    if tools.len() > MAX_TOOLS {
        return Err(McpError::OutputLimit);
    }
    tools
        .iter()
        .map(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or(McpError::Protocol)?;
            if name.is_empty() || name.len() > MAX_TOOL_NAME_BYTES {
                return Err(McpError::OutputLimit);
            }
            let title = bounded_optional_text(tool.get("title"))?;
            let description = bounded_optional_text(tool.get("description"))?;
            let input_schema = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let canonical = serde_json::to_vec(&input_schema).map_err(|_| McpError::Protocol)?;
            Ok(ConnectorTool {
                id: name.to_owned(),
                name: name.to_owned(),
                title,
                description,
                read_only: tool
                    .pointer("/annotations/readOnlyHint")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                input_schema_sha256: hex_digest(Sha256::digest(canonical)),
            })
        })
        .collect()
}

fn bounded_optional_text(value: Option<&Value>) -> Result<Option<String>, McpError> {
    let Some(text) = value.and_then(Value::as_str) else {
        return Ok(None);
    };
    if text.len() > MAX_TOOL_TEXT_BYTES {
        return Err(McpError::OutputLimit);
    }
    Ok(Some(text.to_owned()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Docker, a running daemon, and OPENWORK_GITHUB_TOKEN"]
    fn real_github_mcp_lists_read_only_tools() {
        let definition = github_from_env().expect("valid GitHub MCP configuration");
        assert!(
            definition.process.is_some(),
            "OPENWORK_GITHUB_TOKEN is required"
        );
        let registry = ConnectorRegistry::new(ConnectorRuntimeConfig {
            definitions: vec![definition],
        });
        let tools = registry.tools("github").expect("GitHub MCP tools/list");
        assert!(!tools.is_empty());
        assert!(tools.iter().all(|tool| tool.read_only));
    }
}
