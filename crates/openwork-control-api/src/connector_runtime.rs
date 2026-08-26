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
use openwork_apps::{ScopedCredential, SecretStore};
use openwork_execution::action_executor::ClaimedAction;

const GITHUB_IMAGE: &str = "ghcr.io/github/github-mcp-server@sha256:1817b57d43916532dc002bdc5f344d639bd9fb54a9148d42168458f7c3280567";
const LARK_PACKAGE: &str = "@larksuiteoapi/lark-mcp@0.5.1";
const LARK_READ_TOOLS: &str = "im.v1.chat.search,im.v1.message.list,bitable.v1.appTableRecord.search,docx.v1.document.rawContent,wiki.v1.node.search,wiki.v2.space.getNode";
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOOLS: usize = 512;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_TEXT_BYTES: usize = 4 * 1024;
const MAX_CALL_RESULT_BYTES: usize = 256 * 1024;

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
    secret_store: Option<Arc<dyn SecretStore>>,
}

impl ConnectorRegistry {
    /// Registry over static connector definitions without a credential store.
    ///
    /// This is the test- and fixture-facing constructor; production builds the
    /// registry through [`ConnectorRegistry::with_secret_store`].
    #[cfg(test)]
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
            secret_store: None,
        }
    }

    /// Builds the registry on top of the reviewed credential boundary.
    ///
    /// When a connector definition carries a credential slot, the store is
    /// authoritative: a stored credential is always used, and a revoked or
    /// expired stored credential makes the connector unavailable without
    /// falling back to operator environment configuration.
    #[must_use]
    pub(super) fn with_secret_store(
        config: ConnectorRuntimeConfig,
        secret_store: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            entries: Arc::new(
                config
                    .definitions
                    .into_iter()
                    .map(|entry| (entry.id.clone(), entry))
                    .collect(),
            ),
            cache: Arc::new(Mutex::new(HashMap::new())),
            success_ttl: Duration::from_mins(1),
            failure_ttl: Duration::from_secs(15),
            secret_store: Some(secret_store),
        }
    }

    pub(super) fn empty() -> Self {
        Self {
            entries: Arc::new(BTreeMap::new()),
            cache: Arc::new(Mutex::new(HashMap::new())),
            success_ttl: Duration::from_mins(1),
            failure_ttl: Duration::from_secs(15),
            secret_store: None,
        }
    }

    /// Resolves the exact process configuration for one connector, injecting
    /// the reviewed credential only after the store authorizes it.
    fn process_config(
        &self,
        entry: &ConnectorDefinition,
    ) -> Option<(ProcessConfig, Option<ScopedCredential>)> {
        let config = entry.process.clone()?;
        let Some(credential_ref) = entry.credential_ref.as_deref() else {
            return Some((config, None));
        };
        let Some(store) = self.secret_store.as_deref() else {
            return Some((config, None));
        };
        if store.metadata(credential_ref).is_err() {
            // No reviewed credential: fall back to the connector's static
            // operator environment configuration.
            return Some((config, None));
        }
        // The store owns this credential's lifecycle. A revoked or expired
        // stored credential is authoritative and never falls back.
        store
            .resolve(credential_ref)
            .ok()
            .map(|credential| (config, Some(credential)))
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
        let (process, credential) = self
            .process_config(entry)
            .ok_or(LookupError::NotConfigured)?;
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

        let result = discover_tools(&process, credential.as_ref());
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

    /// Invokes only a capability-to-tool mapping compiled into `OpenWork`.
    /// Callers cannot name a connector tool directly.
    pub(super) fn call_capability(
        &self,
        capability: &str,
        parameters: &Value,
    ) -> Result<Value, CallError> {
        let mapping = github_mapping(capability).ok_or(CallError::CapabilityNotMapped)?;
        let arguments = github_arguments(mapping, parameters)?;
        let entry = self.entries.get("github").ok_or(CallError::Unavailable)?;
        let (process, credential) = self.process_config(entry).ok_or(CallError::NotConfigured)?;
        call_tool(
            &process,
            credential.as_ref(),
            mapping.tool,
            &arguments,
            mapping.read_only,
        )
        .map_err(CallError::from)
    }
}

/// Internal side-effect boundary. It accepts the execution crate's verified,
/// consumed claim type rather than HTTP data, so clients cannot submit an
/// `approved: true` flag or replay a bare capability request.
#[allow(dead_code, clippy::needless_pass_by_value)]
pub(super) fn execute_claimed_github_capability(
    registry: &ConnectorRegistry,
    claimed: ClaimedAction,
) -> Result<Value, CallError> {
    let action = claimed.action();
    if action.action != "github.issue.create" {
        return Err(CallError::CapabilityNotMapped);
    }
    registry.call_capability(&action.action, &action.parameters)
}

#[derive(Clone, Copy)]
struct GithubCapabilityMapping {
    tool: &'static str,
    read_only: bool,
    fixed_method: Option<&'static str>,
}

fn github_mapping(capability: &str) -> Option<GithubCapabilityMapping> {
    let (tool, read_only, fixed_method) = match capability {
        "github.repository.read" => ("search_repositories", true, None),
        "github.file.read" => ("get_file_contents", true, None),
        "github.issue.read" => ("list_issues", true, None),
        "github.actions.read" => ("actions_list", true, Some("list_workflow_runs")),
        "github.pull_request.read" => ("list_pull_requests", true, None),
        "github.issue.create" => ("issue_write", false, Some("create")),
        _ => return None,
    };
    Some(GithubCapabilityMapping {
        tool,
        read_only,
        fixed_method,
    })
}

fn github_arguments(
    mapping: GithubCapabilityMapping,
    parameters: &Value,
) -> Result<Value, CallError> {
    let mut arguments = parameters
        .as_object()
        .cloned()
        .ok_or(CallError::InvalidParameters)?;
    if let Some(method) = mapping.fixed_method {
        if arguments
            .get("method")
            .is_some_and(|value| value.as_str() != Some(method))
        {
            return Err(CallError::InvalidParameters);
        }
        arguments.insert("method".to_owned(), Value::String(method.to_owned()));
    }
    Ok(Value::Object(arguments))
}

#[derive(Debug)]
pub(super) enum CallError {
    CapabilityNotMapped,
    InvalidParameters,
    NotConfigured,
    Unavailable,
}

impl From<McpError> for CallError {
    fn from(_: McpError) -> Self {
        Self::Unavailable
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
    /// Reviewed credential slot consulted before every process launch.
    credential_ref: Option<String>,
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
    let Some(executable) =
        optional_absolute_executable("OPENWORK_GITHUB_MCP_DOCKER_BIN", "/usr/local/bin/docker")?
    else {
        return Ok(ConnectorDefinition {
            id: "github".to_owned(),
            name: "GitHub".to_owned(),
            process: None,
            credential_ref: Some("github.app".to_owned()),
        });
    };
    let mut environment = BTreeMap::from([(
        "GITHUB_TOOLSETS".to_owned(),
        SecretValue("repos,issues,pull_requests,actions,users".to_owned()),
    )]);
    if let Some(host) = optional_nonempty_env("OPENWORK_DOCKER_HOST") {
        environment.insert("DOCKER_HOST".to_owned(), SecretValue(host));
    }
    // Legacy operator configuration remains a fallback: a reviewed credential
    // stored in the SecretStore is authoritative and injected at launch.
    if let Some(token) = optional_nonempty_env("OPENWORK_GITHUB_TOKEN") {
        environment.insert(
            "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
            SecretValue(token),
        );
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
    Ok(ConnectorDefinition {
        id: "github".to_owned(),
        name: "GitHub".to_owned(),
        process: Some(ProcessConfig {
            executable,
            args,
            environment,
            timeout: Duration::from_secs(20),
        }),
        credential_ref: Some("github.app".to_owned()),
    })
}

fn lark_from_env() -> Result<ConnectorDefinition, ConfigError> {
    let app_id = optional_nonempty_env("OPENWORK_LARK_APP_ID");
    let app_secret = optional_nonempty_env("OPENWORK_LARK_APP_SECRET");
    if app_id.is_none() && app_secret.is_none() {
        return Ok(ConnectorDefinition {
            id: "lark".to_owned(),
            name: "Feishu / Lark".to_owned(),
            process: None,
            credential_ref: Some("feishu.app".to_owned()),
        });
    }
    let Some(executable) =
        optional_absolute_executable("OPENWORK_LARK_MCP_NPX_BIN", "/usr/local/bin/npx")?
    else {
        return Ok(ConnectorDefinition {
            id: "lark".to_owned(),
            name: "Feishu / Lark".to_owned(),
            process: None,
            credential_ref: Some("feishu.app".to_owned()),
        });
    };
    let domain = optional_nonempty_env("OPENWORK_LARK_DOMAIN")
        .unwrap_or_else(|| "https://open.feishu.cn".to_owned());
    let mut environment = BTreeMap::from([
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
    if let Some(app_id) = app_id {
        environment.insert("APP_ID".to_owned(), SecretValue(app_id));
    }
    if let Some(app_secret) = app_secret {
        environment.insert("APP_SECRET".to_owned(), SecretValue(app_secret));
    }
    if let Some(home) = optional_nonempty_env("HOME") {
        environment.insert("HOME".to_owned(), SecretValue(home));
    }
    Ok(ConnectorDefinition {
        id: "lark".to_owned(),
        name: "Feishu / Lark".to_owned(),
        process: Some(ProcessConfig {
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
        }),
        credential_ref: Some("feishu.app".to_owned()),
    })
}

fn optional_nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn optional_absolute_executable(
    name: &'static str,
    default: &str,
) -> Result<Option<PathBuf>, ConfigError> {
    let explicit = env::var_os(name).is_some();
    let value = env::var_os(name).map_or_else(|| PathBuf::from(default), PathBuf::from);
    if !value.is_absolute() {
        return Err(ConfigError(name));
    }
    // An explicit override is always honored (a broken value fails at launch);
    // an absent default leaves the connector unconfigured without failing
    // control-plane startup.
    if explicit || value.exists() {
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn discover_tools(
    config: &ProcessConfig,
    credential: Option<&ScopedCredential>,
) -> Result<Vec<ConnectorTool>, McpError> {
    let mut child = spawn(config, credential)?;
    let result = exchange(&mut child, config.timeout);
    // Discovery sessions are one-shot. Terminate even after success so a
    // normal long-lived stdio server cannot block this request in `wait`.
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn spawn(config: &ProcessConfig, credential: Option<&ScopedCredential>) -> Result<Child, McpError> {
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
    // The reviewed credential is injected last, so a stored credential always
    // wins over any static operator environment fallback.
    if let Some(credential) = credential {
        credential.inject_into_provider(&mut command);
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

fn call_tool(
    config: &ProcessConfig,
    credential: Option<&ScopedCredential>,
    expected_tool: &str,
    parameters: &Value,
    require_read_only: bool,
) -> Result<Value, McpError> {
    let mut child = spawn(config, credential)?;
    let result = call_exchange(
        &mut child,
        config.timeout,
        expected_tool,
        parameters,
        require_read_only,
    );
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn call_exchange(
    child: &mut Child,
    timeout: Duration,
    expected_tool: &str,
    parameters: &Value,
    require_read_only: bool,
) -> Result<Value, McpError> {
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
                Ok(_) if sender.send(Ok(line)).is_err() => break,
                Ok(_) => {}
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
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"openwork-control","version":env!("CARGO_PKG_VERSION")}}}),
    )?;
    let deadline = Instant::now() + timeout;
    if receive_response(&receiver, 1, deadline)?
        .get("result")
        .is_none()
    {
        return Err(McpError::Protocol);
    }
    send_json(
        stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )?;
    send_json(
        stdin,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )?;
    let tools = parse_tools(&receive_response(&receiver, 2, deadline)?)?;
    let tool = tools
        .iter()
        .find(|tool| tool.name == expected_tool)
        .ok_or(McpError::Protocol)?;
    if require_read_only && !tool.read_only {
        return Err(McpError::Protocol);
    }
    if serde_json::to_vec(parameters).map_or(true, |encoded| encoded.len() > 64 * 1024) {
        return Err(McpError::OutputLimit);
    }
    send_json(
        stdin,
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":expected_tool,"arguments":parameters}}),
    )?;
    let output = receive_response(&receiver, 3, deadline)?;
    if serde_json::to_vec(&output).map_or(true, |encoded| encoded.len() > MAX_CALL_RESULT_BYTES) {
        return Err(McpError::OutputLimit);
    }
    validated_call_result(&output)
}

fn validated_call_result(output: &Value) -> Result<Value, McpError> {
    if output.get("error").is_some() {
        return Err(McpError::Protocol);
    }
    let result = output.get("result").cloned().ok_or(McpError::Protocol)?;
    match result.get("isError") {
        Some(Value::Bool(false)) | None => Ok(result),
        Some(_) => Err(McpError::Protocol),
    }
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
    use openwork_apps::MemorySecretStore;

    #[test]
    fn stored_credential_is_authoritative_and_revocation_fails_closed() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let definition = ConnectorDefinition {
            id: "github".to_owned(),
            name: "GitHub".to_owned(),
            process: Some(ProcessConfig {
                executable: PathBuf::from("/usr/bin/docker"),
                args: Vec::new(),
                environment: BTreeMap::new(),
                timeout: Duration::from_secs(1),
            }),
            credential_ref: Some("github.app".to_owned()),
        };
        let registry = ConnectorRegistry::with_secret_store(
            ConnectorRuntimeConfig {
                definitions: vec![definition],
            },
            store.clone(),
        );
        let entry = registry
            .entries
            .get("github")
            .expect("connector definition");
        // No reviewed credential yet: the static operator configuration is used.
        assert!(
            registry
                .process_config(entry)
                .expect("fallback")
                .1
                .is_none()
        );

        store
            .store(openwork_apps::SecretStoreEntry {
                credential_ref: "github.app".to_owned(),
                provider: "github".to_owned(),
                environment_variable: "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
                scope: "repos".to_owned(),
                owner: "control-admin".to_owned(),
                value: "github_app_token".to_owned(),
                expires_at: None,
            })
            .expect("stored credential");
        assert!(registry.process_config(entry).expect("stored").1.is_some());

        // Rotation keeps the same authoritative store path.
        store
            .rotate("github.app", "new_token")
            .expect("rotated credential");
        assert!(registry.process_config(entry).expect("rotated").1.is_some());

        // Revocation must be authoritative: no fallback to operator env.
        store.revoke("github.app").expect("revoked credential");
        assert!(registry.process_config(entry).is_none());

        // Re-enabling restores the same store path.
        store.enable("github.app").expect("enabled credential");
        assert!(registry.process_config(entry).expect("enabled").1.is_some());
    }

    #[test]
    fn github_capability_mapping_is_exact_and_rejects_arbitrary_tools() {
        let file = github_mapping("github.file.read").expect("mapped read capability");
        assert_eq!(file.tool, "get_file_contents");
        assert!(file.read_only);
        let create = github_mapping("github.issue.create").expect("mapped write capability");
        assert_eq!(create.tool, "issue_write");
        assert_eq!(create.fixed_method, Some("create"));
        assert!(!create.read_only);
        assert!(github_arguments(create, &json!({"method":"update"})).is_err());
        assert_eq!(
            github_arguments(create, &json!({"owner":"o"}))
                .expect("fixed method")
                .get("method")
                .and_then(Value::as_str),
            Some("create")
        );
        assert!(github_mapping("github.tools.call").is_none());
        assert!(github_mapping("delete_repository").is_none());
    }

    #[test]
    fn mcp_tool_error_result_is_never_reported_as_success() {
        assert!(validated_call_result(&json!({"result":{"isError":true,"content":[]}})).is_err());
        assert!(validated_call_result(&json!({"result":{"isError":false,"content":[]}})).is_ok());
    }

    #[test]
    #[ignore = "requires Docker, a running daemon, and OPENWORK_GITHUB_TOKEN"]
    fn real_github_mcp_exposes_exact_governed_tools() {
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
        for capability in [
            "github.repository.read",
            "github.file.read",
            "github.issue.read",
            "github.actions.read",
            "github.pull_request.read",
        ] {
            let mapping = github_mapping(capability).expect("compiled mapping");
            assert!(
                tools
                    .iter()
                    .any(|tool| tool.name == mapping.tool && tool.read_only),
                "missing read-only tool {}",
                mapping.tool
            );
        }
        let create = github_mapping("github.issue.create").expect("create mapping");
        assert!(
            tools
                .iter()
                .any(|tool| tool.name == create.tool && !tool.read_only)
        );
    }

    #[test]
    #[ignore = "requires Docker, a running daemon, and OPENWORK_GITHUB_TOKEN"]
    fn real_github_mcp_reads_repository_through_capability_mapping() {
        let definition = github_from_env().expect("valid GitHub MCP configuration");
        assert!(
            definition.process.is_some(),
            "OPENWORK_GITHUB_TOKEN is required"
        );
        let registry = ConnectorRegistry::new(ConnectorRuntimeConfig {
            definitions: vec![definition],
        });
        let result = registry
            .call_capability(
                "github.repository.read",
                &json!({
                    "query": "openwork in:name user:shichenghaoshu",
                    "minimal_output": true,
                    "perPage": 1
                }),
            )
            .expect("governed GitHub repository read");
        let encoded = serde_json::to_vec(&result).expect("bounded MCP result");
        assert!(!encoded.is_empty());
        assert!(encoded.len() <= MAX_CALL_RESULT_BYTES);
    }
}
