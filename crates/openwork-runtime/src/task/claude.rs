use super::{
    EventFactory, RuntimeEventDecoder, RuntimeInvocation, RuntimeOutputProtocol,
    RuntimeTaskAdapter, bounded_redacted_text, bounded_redacted_value, empty_metadata,
    metadata_from_untrusted, parse_json_line, runtime_error, validate_task,
};
use openwork_core::OpenWorkError;
use openwork_execution::{RunId, RuntimeEvent, RuntimeEventPayload, RuntimeTask, SandboxCommand};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const CLAUDE_RUNTIME_ID: &str = "claude-code";
pub const CLAUDE_REQUIRED_FLAGS: &[&str] = &[
    "--print",
    "--output-format stream-json",
    "--safe-mode",
    "--no-session-persistence",
    "--tools",
    "--strict-mcp-config",
];

/// Prepares the current documented Claude Code non-interactive JSON stream.
pub struct ClaudeTaskAdapter {
    container_executable: String,
}

impl ClaudeTaskAdapter {
    #[must_use]
    pub fn new(container_executable: impl Into<String>) -> Self {
        Self {
            container_executable: container_executable.into(),
        }
    }
}

impl RuntimeTaskAdapter for ClaudeTaskAdapter {
    fn prepare(&self, task: &RuntimeTask) -> Result<RuntimeInvocation, OpenWorkError> {
        validate_task(task, CLAUDE_RUNTIME_ID)?;
        let can_write = task
            .capabilities
            .iter()
            .any(|capability| capability == "filesystem.write");
        let tools = if can_write {
            "Read,Write,Edit,Glob,Grep"
        } else {
            "Read,Glob,Grep"
        };
        let permission_mode = if can_write { "acceptEdits" } else { "dontAsk" };
        let command = SandboxCommand::new(
            self.container_executable.clone(),
            vec![
                "--safe-mode".to_owned(),
                "--print".to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                "--verbose".to_owned(),
                "--no-session-persistence".to_owned(),
                "--no-chrome".to_owned(),
                "--disable-slash-commands".to_owned(),
                "--strict-mcp-config".to_owned(),
                "--tools".to_owned(),
                tools.to_owned(),
                "--permission-mode".to_owned(),
                permission_mode.to_owned(),
                "Use the task supplied on standard input. Keep all file access inside the working directory."
                    .to_owned(),
            ],
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
        Box::new(ClaudeTaskDecoder::new(run_id))
    }
}

pub struct ClaudeTaskDecoder {
    events: EventFactory,
}

impl ClaudeTaskDecoder {
    #[must_use]
    pub const fn new(run_id: RunId) -> Self {
        Self {
            events: EventFactory::new(run_id),
        }
    }

    fn decode_assistant(&mut self, value: &Value) -> Result<Vec<RuntimeEvent>, OpenWorkError> {
        let content = value
            .pointer("/message/content")
            .and_then(Value::as_array)
            .ok_or_else(|| runtime_error("Claude assistant event omitted content"))?;
        if content.len() > 128 {
            return Err(runtime_error("Claude assistant event has too many blocks"));
        }
        content
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => self.events.event(
                    RuntimeEventPayload::Message {
                        content: bounded_redacted_text(
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .ok_or_else(|| runtime_error("Claude text block omitted text"))?,
                        )?,
                    },
                    empty_metadata(),
                ),
                Some("tool_use") => {
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| runtime_error("Claude tool event omitted the tool name"))?;
                    if name.is_empty() || name.len() > 256 {
                        return Err(runtime_error("Claude tool name is invalid"));
                    }
                    self.events.event(
                        RuntimeEventPayload::ToolCall {
                            name: name.to_owned(),
                            parameters: bounded_redacted_value(
                                block.get("input").unwrap_or(&Value::Null),
                            )?,
                        },
                        empty_metadata(),
                    )
                }
                _ => self.events.event(
                    RuntimeEventPayload::Message {
                        content: "Claude vendor content block omitted".to_owned(),
                    },
                    unknown_metadata(block, "assistant.content.unknown")?,
                ),
            })
            .collect()
    }
}

impl RuntimeEventDecoder for ClaudeTaskDecoder {
    fn decode_stdout_line(&mut self, line: &[u8]) -> Result<Vec<RuntimeEvent>, OpenWorkError> {
        let value = parse_json_line(line)?;
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| runtime_error("Claude event omitted its type"))?;
        match event_type {
            "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => {
                Ok(vec![self.events.event(
                    RuntimeEventPayload::Started,
                    minimal_metadata(&value)?,
                )?])
            }
            "assistant" => self.decode_assistant(&value),
            "result" if value.get("is_error").and_then(Value::as_bool) == Some(true) => {
                let message = value
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("Claude reported a provider failure");
                Ok(vec![self.events.fail(
                    "claude_result_error",
                    message,
                    minimal_metadata(&value)?,
                )?])
            }
            "result" => Ok(vec![self.events.event(
                RuntimeEventPayload::Message {
                    content: "Claude provider result received".to_owned(),
                },
                minimal_metadata(&value)?,
            )?]),
            _ => Ok(vec![self.events.event(
                RuntimeEventPayload::Message {
                    content: "Claude vendor event omitted".to_owned(),
                },
                unknown_metadata(&value, event_type)?,
            )?]),
        }
    }

    fn decode_stderr_line(&mut self, line: &[u8]) -> Result<RuntimeEvent, OpenWorkError> {
        self.events.stderr(line)
    }

    fn finish(&mut self, exit_code: i32) -> Result<Option<RuntimeEvent>, OpenWorkError> {
        self.events.finish(exit_code)
    }
}

fn minimal_metadata(
    value: &Value,
) -> Result<openwork_execution::RedactedAuditMetadata, OpenWorkError> {
    metadata_from_untrusted(&json!({
        "provider_event_type": value.get("type"),
        "provider_event_subtype": value.get("subtype"),
        "is_error": value.get("is_error")
    }))
}

fn unknown_metadata(
    value: &Value,
    event_type: &str,
) -> Result<openwork_execution::RedactedAuditMetadata, OpenWorkError> {
    metadata_from_untrusted(&json!({
        "provider_event_type": event_type,
        "vendor_payload": value
    }))
}
