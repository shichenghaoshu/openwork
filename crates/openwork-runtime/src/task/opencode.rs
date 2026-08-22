use super::{
    EventFactory, RuntimeEventDecoder, RuntimeInvocation, RuntimeOutputProtocol,
    RuntimeTaskAdapter, bounded_redacted_text, bounded_redacted_value, metadata_from_untrusted,
    parse_json_line, runtime_error, validate_task,
};
use openwork_core::OpenWorkError;
use openwork_execution::{RunId, RuntimeEvent, RuntimeEventPayload, RuntimeTask, SandboxCommand};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const OPENCODE_RUNTIME_ID: &str = "opencode";
/// Official `opencode run` flags used for a non-interactive JSONL stream.
/// The prompt is intentionally absent: `OpenCode`'s current run command accepts
/// piped stdin when no positional message is supplied.
pub const OPENCODE_REQUIRED_FLAGS: &[&str] = &["run", "--format", "json"];

/// Adapts `OpenCode`'s official non-interactive JSONL command for sandbox use.
pub struct OpenCodeTaskAdapter {
    container_executable: String,
}

impl OpenCodeTaskAdapter {
    #[must_use]
    pub fn new(container_executable: impl Into<String>) -> Self {
        Self {
            container_executable: container_executable.into(),
        }
    }
}

impl RuntimeTaskAdapter for OpenCodeTaskAdapter {
    fn prepare(&self, task: &RuntimeTask) -> Result<RuntimeInvocation, OpenWorkError> {
        validate_task(task, OPENCODE_RUNTIME_ID)?;
        let command = SandboxCommand::new(
            self.container_executable.clone(),
            vec!["run".to_owned(), "--format".to_owned(), "json".to_owned()],
            BTreeMap::new(),
        )?;
        Ok(RuntimeInvocation {
            command,
            working_directory: task.working_directory.clone(),
            stdin: task.prompt.as_bytes().to_vec(),
            output_protocol: RuntimeOutputProtocol::JsonLines,
        })
    }

    fn decoder(&self, run_id: RunId) -> Box<dyn RuntimeEventDecoder> {
        Box::new(OpenCodeTaskDecoder::new(run_id))
    }
}

pub struct OpenCodeTaskDecoder {
    events: EventFactory,
}

impl OpenCodeTaskDecoder {
    #[must_use]
    pub const fn new(run_id: RunId) -> Self {
        Self {
            events: EventFactory::new(run_id),
        }
    }

    fn metadata(value: &Value) -> Result<openwork_execution::RedactedAuditMetadata, OpenWorkError> {
        metadata_from_untrusted(&json!({
            "provider_event_type": value.get("type"),
            "session_id": value.get("sessionID"),
        }))
    }
}

impl RuntimeEventDecoder for OpenCodeTaskDecoder {
    fn decode_stdout_line(&mut self, line: &[u8]) -> Result<Vec<RuntimeEvent>, OpenWorkError> {
        let value = parse_json_line(line)?;
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| runtime_error("OpenCode event omitted its type"))?;
        match event_type {
            "session.created" | "session.status" => {
                Ok(vec![self.events.event(
                    RuntimeEventPayload::Started,
                    Self::metadata(&value)?,
                )?])
            }
            "message.part.updated" => {
                let part = value
                    .pointer("/properties/part")
                    .or_else(|| value.get("part"))
                    .ok_or_else(|| runtime_error("OpenCode part event omitted its part"))?;
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = part
                            .pointer("/text")
                            .or_else(|| part.pointer("/content"))
                            .and_then(Value::as_str)
                            .ok_or_else(|| runtime_error("OpenCode text part omitted content"))?;
                        Ok(vec![self.events.event(
                            RuntimeEventPayload::Message {
                                content: bounded_redacted_text(text)?,
                            },
                            Self::metadata(&value)?,
                        )?])
                    }
                    Some("tool") => Ok(vec![self.events.event(
                        RuntimeEventPayload::ToolCall {
                            name: "opencode.tool".to_owned(),
                            parameters: bounded_redacted_value(part)?,
                        },
                        Self::metadata(&value)?,
                    )?]),
                    _ => Ok(vec![]),
                }
            }
            "tool_use" => Ok(vec![
                self.events.event(
                    RuntimeEventPayload::ToolCall {
                        name: value
                            .pointer("/part/tool")
                            .and_then(Value::as_str)
                            .unwrap_or("opencode.tool")
                            .to_owned(),
                        parameters: bounded_redacted_value(
                            value.get("part").unwrap_or(&Value::Null),
                        )?,
                    },
                    Self::metadata(&value)?,
                )?,
            ]),
            "session.error" | "error" => {
                let message = value
                    .pointer("/properties/error/message")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenCode reported a provider failure");
                Ok(vec![self.events.fail(
                    "opencode_provider_error",
                    message,
                    Self::metadata(&value)?,
                )?])
            }
            _ => Ok(vec![]),
        }
    }
    fn decode_stderr_line(&mut self, line: &[u8]) -> Result<RuntimeEvent, OpenWorkError> {
        self.events.stderr(line)
    }
    fn finish(&mut self, exit_code: i32) -> Result<Option<RuntimeEvent>, OpenWorkError> {
        self.events.finish(exit_code)
    }
}
