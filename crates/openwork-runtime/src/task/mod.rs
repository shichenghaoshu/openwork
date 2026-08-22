//! Provider task preparation and bounded JSONL decoding for sandbox execution.

mod claude;
mod codex;
mod opencode;
mod pi;

pub use claude::{CLAUDE_REQUIRED_FLAGS, CLAUDE_RUNTIME_ID, ClaudeTaskAdapter, ClaudeTaskDecoder};
pub use codex::{CODEX_REQUIRED_FLAGS, CODEX_RUNTIME_ID, CodexTaskAdapter, CodexTaskDecoder};
pub use opencode::{
    OPENCODE_REQUIRED_FLAGS, OPENCODE_RUNTIME_ID, OpenCodeTaskAdapter, OpenCodeTaskDecoder,
};
pub use pi::{PI_REQUIRED_FLAGS, PI_RUNTIME_ID, PiTaskAdapter, PiTaskDecoder};

use openwork_core::{ErrorCode, OpenWorkError, redact_json, redact_text};
use openwork_execution::{
    ApprovedMountDirectory, DigestPinnedImageRef, EXECUTION_SCHEMA_VERSION, RedactedAuditMetadata,
    RunId, RuntimeEvent, RuntimeEventPayload, RuntimeTask, SandboxCleanupStatus, SandboxCommand,
    SandboxLimits, SandboxRequest, SandboxResult, SandboxTermination, SandboxUser,
    SandboxWorkingDirectory, UtcTimestamp,
};
use serde::Deserializer;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const PROVIDER_DOCS_ACCESSED_ON: &str = "2026-08-23";
pub const OPENCODE_CLI_REFERENCE_URL: &str =
    "https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/run.ts";
pub const PI_RPC_REFERENCE_URL: &str =
    "https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/rpc.md";
pub const CLAUDE_CLI_REFERENCE_URL: &str = "https://code.claude.com/docs/en/cli-usage";
pub const CLAUDE_HEADLESS_URL: &str = "https://code.claude.com/docs/en/headless";
pub const CODEX_NON_INTERACTIVE_URL: &str = "https://learn.chatgpt.com/docs/non-interactive-mode";
pub const CODEX_CLI_REFERENCE_URL: &str =
    "https://learn.chatgpt.com/docs/developer-commands?surface=cli";
pub const PROVIDER_VERSION_GATE_POLICY: &str =
    "Require every documented flag; no unverified minimum CLI version is claimed.";

pub const MAX_PROVIDER_LINE_BYTES: usize = 64 * 1024;
pub const MAX_RUNTIME_EVENT_BYTES: usize = 16 * 1024;
pub const MAX_RUNTIME_EVENTS: u64 = 4096;
pub const MAX_RUNTIME_PROMPT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeOutputProtocol {
    JsonLines,
}

/// Prepared provider invocation. The prompt is kept off argv and supplied via stdin.
#[derive(Clone, Eq, PartialEq)]
pub struct RuntimeInvocation {
    pub command: SandboxCommand,
    pub working_directory: SandboxWorkingDirectory,
    pub stdin: Vec<u8>,
    pub output_protocol: RuntimeOutputProtocol,
}

impl std::fmt::Debug for RuntimeInvocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeInvocation")
            .field("command", &self.command)
            .field("working_directory", &self.working_directory)
            .field("stdin", &"<redacted>")
            .field("stdin_bytes", &self.stdin.len())
            .field("output_protocol", &self.output_protocol)
            .finish()
    }
}

/// Converts a prepared provider invocation into a bounded sandbox execution request.
///
/// The runtime adapter's stdin takes precedence over any existing `SandboxCommand` stdin.
/// All other parameters (image, user, mounts, limits) come from the execution environment.
///
/// # Errors
///
/// Returns an error for invalid mount directories, resource limits, or duplicate paths.
pub fn into_sandbox_request(
    invocation: RuntimeInvocation,
    run_id: RunId,
    image: DigestPinnedImageRef,
    user: SandboxUser,
    input_directory: ApprovedMountDirectory,
    output_directory: ApprovedMountDirectory,
    limits: SandboxLimits,
) -> Result<SandboxRequest, OpenWorkError> {
    let mut command = invocation.command;
    if !invocation.stdin.is_empty() {
        // Rebuild the command with the adapter-supplied stdin, replacing any
        // default empty stdin from the adapter constructor.
        command = SandboxCommand::with_stdin(
            command.program().to_owned(),
            command.arguments().to_vec(),
            command.environment().clone(),
            invocation.stdin,
        )?;
    }
    SandboxRequest::new(
        run_id,
        image,
        command,
        user,
        input_directory,
        output_directory,
        limits,
    )
}

/// Provider-specific preparation that never executes a process on the host.
pub trait RuntimeTaskAdapter: Send + Sync {
    /// Converts a validated task into a shell-free container invocation.
    ///
    /// # Errors
    ///
    /// Fails closed for a mismatched provider, unsupported capability, invalid
    /// container executable, oversized prompt, or invalid frozen task.
    fn prepare(&self, task: &RuntimeTask) -> Result<RuntimeInvocation, OpenWorkError>;

    fn decoder(&self, run_id: RunId) -> Box<dyn RuntimeEventDecoder>;
}

/// Stateful, bounded decoder for one provider JSONL stream.
pub trait RuntimeEventDecoder: Send {
    /// Decodes one stdout JSONL record into zero or more unified events.
    ///
    /// # Errors
    ///
    /// Rejects malformed JSON, duplicate keys, oversized data, invalid event
    /// shapes, or records after a terminal failure.
    fn decode_stdout_line(&mut self, line: &[u8]) -> Result<Vec<RuntimeEvent>, OpenWorkError>;

    /// Converts one provider stderr line to a centrally redacted event.
    ///
    /// # Errors
    ///
    /// Rejects oversized or non-UTF-8 stderr lines.
    fn decode_stderr_line(&mut self, line: &[u8]) -> Result<RuntimeEvent, OpenWorkError>;

    /// Emits the process terminal event from the sandbox-observed exit code.
    ///
    /// # Errors
    ///
    /// Rejects an exhausted event budget. Returns `None` after a provider
    /// failure has already made the stream terminal.
    fn finish(&mut self, exit_code: i32) -> Result<Option<RuntimeEvent>, OpenWorkError>;
}

/// Decodes one validated sandbox result into bounded, unified runtime events.
///
/// Stdout is treated as provider JSONL and stderr as independent text lines.
/// The captured streams cannot preserve their original interleaving, so stdout
/// records are decoded before stderr records. Only an `Exited` result may call
/// [`RuntimeEventDecoder::finish`], and it does so with the sandbox-observed
/// exit code. Cancellation, timeout, out-of-memory, and backend failure remain
/// execution-layer terminal states and never synthesize provider completion.
///
/// This function deliberately returns events to the caller without persisting
/// either raw output or decoded events. Callers may use it directly as the
/// output processor for `ExecutionOrchestrator::execute_with_output_processor`:
///
/// ```text
/// |result| decode_sandbox_result(result, decoder.as_mut()).map(|_| ())
/// ```
///
/// # Errors
///
/// Fails closed when the result is invalid or truncated, a line is malformed,
/// duplicated, empty, or oversized, the event budget is exhausted, output
/// arrives after a decoder terminal event, or decoder finalization fails.
pub fn decode_sandbox_result(
    result: &SandboxResult,
    decoder: &mut dyn RuntimeEventDecoder,
) -> Result<Vec<RuntimeEvent>, OpenWorkError> {
    result.validate()?;
    if !matches!(result.cleanup, SandboxCleanupStatus::Succeeded) {
        return Err(runtime_error("sandbox cleanup failed"));
    }
    if result.truncated {
        return Err(runtime_error("sandbox output was truncated"));
    }

    let mut events = Vec::new();
    decode_lines(&result.stdout, |line| {
        for event in decoder.decode_stdout_line(line)? {
            append_decoded_event(result, &mut events, event, DecodedEventSource::Stdout)?;
        }
        Ok(())
    })?;
    decode_lines(&result.stderr, |line| {
        let event = decoder.decode_stderr_line(line)?;
        append_decoded_event(result, &mut events, event, DecodedEventSource::Stderr)?;
        Ok(())
    })?;

    if result.termination == SandboxTermination::Exited {
        let exit_code = result
            .exit_code
            .ok_or_else(|| runtime_error("exited sandbox omitted its exit code"))?;
        if let Some(event) = decoder.finish(exit_code)? {
            append_decoded_event(result, &mut events, event, DecodedEventSource::Finish)?;
        } else if !events.last().is_some_and(is_terminal_event) {
            return Err(runtime_error(
                "runtime decoder omitted its terminal exit event",
            ));
        }
    }
    Ok(events)
}

#[derive(Clone, Copy)]
enum DecodedEventSource {
    Stdout,
    Stderr,
    Finish,
}

fn append_decoded_event(
    result: &SandboxResult,
    events: &mut Vec<RuntimeEvent>,
    event: RuntimeEvent,
    source: DecodedEventSource,
) -> Result<(), OpenWorkError> {
    if events.last().is_some_and(is_terminal_event) {
        return Err(runtime_error(
            "runtime decoder emitted data after termination",
        ));
    }
    let expected_sequence = u64::try_from(events.len())
        .ok()
        .and_then(|sequence| sequence.checked_add(1))
        .ok_or_else(|| runtime_error("runtime event sequence overflowed"))?;
    if event.schema_version != EXECUTION_SCHEMA_VERSION
        || event.run_id != result.run_id
        || event.sequence != expected_sequence
        || event.sequence > MAX_RUNTIME_EVENTS
    {
        return Err(runtime_error(
            "runtime decoder emitted an invalid event envelope",
        ));
    }
    ensure_event_bounds(&event.payload, &event.vendor_metadata)?;

    let source_valid = match source {
        DecodedEventSource::Stdout => !matches!(
            &event.payload,
            RuntimeEventPayload::Completed { .. } | RuntimeEventPayload::Cancelled
        ),
        DecodedEventSource::Stderr => {
            matches!(&event.payload, RuntimeEventPayload::Stderr { .. })
        }
        DecodedEventSource::Finish => match (&event.payload, result.exit_code) {
            (RuntimeEventPayload::Completed { exit_code }, Some(observed)) => {
                observed == 0 && *exit_code == observed
            }
            (RuntimeEventPayload::Failed { .. }, Some(observed)) => observed != 0,
            _ => false,
        },
    };
    if !source_valid {
        return Err(runtime_error(
            "runtime decoder emitted an event from an invalid source",
        ));
    }
    events.push(event);
    Ok(())
}

const fn is_terminal_event(event: &RuntimeEvent) -> bool {
    matches!(
        &event.payload,
        RuntimeEventPayload::Completed { .. }
            | RuntimeEventPayload::Failed { .. }
            | RuntimeEventPayload::Cancelled
    )
}

fn decode_lines(
    output: &str,
    mut decode: impl FnMut(&[u8]) -> Result<(), OpenWorkError>,
) -> Result<(), OpenWorkError> {
    if output.is_empty() {
        return Ok(());
    }
    for line in output.lines() {
        decode(line.as_bytes())?;
    }
    Ok(())
}

pub(crate) struct EventFactory {
    run_id: RunId,
    next_sequence: u64,
    terminal: bool,
}

impl EventFactory {
    pub(crate) const fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            next_sequence: 1,
            terminal: false,
        }
    }

    pub(crate) fn event(
        &mut self,
        payload: RuntimeEventPayload,
        metadata: RedactedAuditMetadata,
    ) -> Result<RuntimeEvent, OpenWorkError> {
        if self.terminal || self.next_sequence > MAX_RUNTIME_EVENTS {
            return Err(runtime_error(
                "runtime event stream is terminal or exhausted",
            ));
        }
        ensure_event_bounds(&payload, &metadata)?;
        let event = RuntimeEvent {
            schema_version: EXECUTION_SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            sequence: self.next_sequence,
            timestamp: UtcTimestamp::now(),
            payload,
            vendor_metadata: metadata,
        };
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| runtime_error("runtime event sequence overflowed"))?;
        Ok(event)
    }

    pub(crate) fn fail(
        &mut self,
        code: &str,
        message: &str,
        metadata: RedactedAuditMetadata,
    ) -> Result<RuntimeEvent, OpenWorkError> {
        let event = self.event(
            RuntimeEventPayload::Failed {
                code: code.to_owned(),
                message: bounded_redacted_text(message)?,
            },
            metadata,
        )?;
        self.terminal = true;
        Ok(event)
    }

    pub(crate) fn stderr(&mut self, line: &[u8]) -> Result<RuntimeEvent, OpenWorkError> {
        validate_line(line)?;
        let text = std::str::from_utf8(line)
            .map_err(|_| runtime_error("provider stderr is not valid UTF-8"))?;
        self.event(
            RuntimeEventPayload::Stderr {
                chunk: bounded_redacted_text(text)?,
                truncated: false,
            },
            empty_metadata(),
        )
    }

    pub(crate) fn finish(&mut self, exit_code: i32) -> Result<Option<RuntimeEvent>, OpenWorkError> {
        if self.terminal {
            return Ok(None);
        }
        let event = if exit_code == 0 {
            self.event(
                RuntimeEventPayload::Completed { exit_code },
                empty_metadata(),
            )?
        } else {
            self.fail(
                "provider_exit_nonzero",
                "provider process exited unsuccessfully",
                empty_metadata(),
            )?
        };
        self.terminal = true;
        Ok(Some(event))
    }
}

pub(crate) fn parse_json_line(line: &[u8]) -> Result<Value, OpenWorkError> {
    validate_line(line)?;
    let mut deserializer = serde_json::Deserializer::from_slice(line);
    let value = StrictValue
        .deserialize(&mut deserializer)
        .map_err(|_| runtime_error("provider emitted malformed JSONL"))?;
    deserializer
        .end()
        .map_err(|_| runtime_error("provider JSONL has trailing data"))?;
    Ok(value)
}

pub(crate) fn metadata_from_untrusted(
    value: &Value,
) -> Result<RedactedAuditMetadata, OpenWorkError> {
    let Value::Object(entries) = value else {
        return Err(runtime_error("provider metadata must be an object"));
    };
    let metadata = RedactedAuditMetadata::from_untrusted(
        &entries.clone().into_iter().collect::<BTreeMap<_, _>>(),
    );
    let encoded = serde_json::to_vec(&metadata)
        .map_err(|_| runtime_error("provider metadata could not be encoded"))?;
    if encoded.len() > MAX_RUNTIME_EVENT_BYTES {
        return Err(runtime_error("provider metadata exceeds the event limit"));
    }
    Ok(metadata)
}

pub(crate) fn empty_metadata() -> RedactedAuditMetadata {
    RedactedAuditMetadata::from_untrusted(&BTreeMap::new())
}

pub(crate) fn bounded_redacted_value(value: &Value) -> Result<Value, OpenWorkError> {
    let redacted = redact_json(value);
    if serde_json::to_vec(&redacted).map_or(true, |encoded| encoded.len() > MAX_RUNTIME_EVENT_BYTES)
    {
        return Err(runtime_error(
            "provider event value exceeds the event limit",
        ));
    }
    Ok(redacted)
}

pub(crate) fn bounded_redacted_text(value: &str) -> Result<String, OpenWorkError> {
    let redacted = redact_text(value);
    if redacted.len() > MAX_RUNTIME_EVENT_BYTES {
        return Err(runtime_error("provider event text exceeds the event limit"));
    }
    Ok(redacted)
}

pub(crate) fn validate_task(task: &RuntimeTask, provider: &str) -> Result<(), OpenWorkError> {
    task.validate()?;
    if task.runtime != provider || task.prompt.len() > MAX_RUNTIME_PROMPT_BYTES {
        return Err(runtime_error(
            "runtime task does not match the provider or size limit",
        ));
    }
    if task
        .capabilities
        .iter()
        .any(|capability| !matches!(capability.as_str(), "filesystem.read" | "filesystem.write"))
    {
        return Err(runtime_error(
            "runtime task requests an unsupported capability",
        ));
    }
    if !task
        .capabilities
        .iter()
        .any(|capability| capability == "filesystem.read")
    {
        return Err(runtime_error(
            "provider tasks require the filesystem.read capability",
        ));
    }
    Ok(())
}

fn validate_line(line: &[u8]) -> Result<(), OpenWorkError> {
    if line.is_empty() || line.len() > MAX_PROVIDER_LINE_BYTES {
        return Err(runtime_error("provider output line is empty or oversized"));
    }
    Ok(())
}

fn ensure_event_bounds(
    payload: &RuntimeEventPayload,
    metadata: &RedactedAuditMetadata,
) -> Result<(), OpenWorkError> {
    let payload_bytes = match payload {
        RuntimeEventPayload::Started | RuntimeEventPayload::Cancelled => 0,
        RuntimeEventPayload::Stdout { chunk, .. } | RuntimeEventPayload::Stderr { chunk, .. } => {
            chunk.len()
        }
        RuntimeEventPayload::Message { content } => content.len(),
        RuntimeEventPayload::ToolCall { name, parameters } => name
            .len()
            .checked_add(
                serde_json::to_vec(parameters)
                    .map_err(|_| runtime_error("runtime event could not be bounded"))?
                    .len(),
            )
            .ok_or_else(|| runtime_error("runtime event size overflowed"))?,
        RuntimeEventPayload::Artifact { relative_path } => relative_path.len(),
        RuntimeEventPayload::Completed { .. } => std::mem::size_of::<i32>(),
        RuntimeEventPayload::Failed { code, message } => code
            .len()
            .checked_add(message.len())
            .ok_or_else(|| runtime_error("runtime event size overflowed"))?,
    };
    let metadata_bytes = serde_json::to_vec(metadata)
        .map_err(|_| runtime_error("runtime event metadata could not be bounded"))?
        .len();
    let estimated_wire_bytes = payload_bytes
        .checked_add(metadata_bytes)
        .and_then(|bytes| bytes.checked_add(512))
        .ok_or_else(|| runtime_error("runtime event size overflowed"))?;
    if estimated_wire_bytes > MAX_RUNTIME_EVENT_BYTES {
        return Err(runtime_error("runtime event exceeds the event limit"));
    }
    Ok(())
}

pub(crate) fn runtime_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ExecutionFailed, message)
}

struct StrictValue;

impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictValue)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut keys = BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            values.insert(key, object.next_value_seed(StrictValue)?);
        }
        Ok(Value::Object(values))
    }
}
