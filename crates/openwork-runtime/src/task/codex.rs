use super::{
    EventFactory, RuntimeEventDecoder, RuntimeInvocation, RuntimeOutputProtocol,
    RuntimeTaskAdapter, bounded_redacted_text, bounded_redacted_value, empty_metadata,
    metadata_from_untrusted, parse_json_line, runtime_error, validate_task,
};
use openwork_core::OpenWorkError;
use openwork_execution::{RunId, RuntimeEvent, RuntimeEventPayload, RuntimeTask, SandboxCommand};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const CODEX_RUNTIME_ID: &str = "codex";
pub const CODEX_REQUIRED_FLAGS: &[&str] = &[
    "exec",
    "--json",
    "--ephemeral",
    "--ignore-user-config",
    "--ignore-rules",
    "--strict-config",
    "--ask-for-approval never",
    "--sandbox",
    "--cd",
    "--skip-git-repo-check",
];

/// Prepares the current documented Codex non-interactive JSONL protocol.
pub struct CodexTaskAdapter {
    container_executable: String,
}

impl CodexTaskAdapter {
    #[must_use]
    pub fn new(container_executable: impl Into<String>) -> Self {
        Self {
            container_executable: container_executable.into(),
        }
    }
}

impl RuntimeTaskAdapter for CodexTaskAdapter {
    fn prepare(&self, task: &RuntimeTask) -> Result<RuntimeInvocation, OpenWorkError> {
        validate_task(task, CODEX_RUNTIME_ID)?;
        let can_write = task
            .capabilities
            .iter()
            .any(|capability| capability == "filesystem.write");
        let sandbox = if can_write {
            "workspace-write"
        } else {
            "read-only"
        };
        let working_directory = task.working_directory.clone();
        let command = SandboxCommand::new(
            self.container_executable.clone(),
            vec![
                "exec".to_owned(),
                "--json".to_owned(),
                "--ephemeral".to_owned(),
                "--ignore-user-config".to_owned(),
                "--ignore-rules".to_owned(),
                "--strict-config".to_owned(),
                "--ask-for-approval".to_owned(),
                "never".to_owned(),
                "--sandbox".to_owned(),
                sandbox.to_owned(),
                "--cd".to_owned(),
                working_directory.as_str().to_owned(),
                "--skip-git-repo-check".to_owned(),
                "-".to_owned(),
            ],
            BTreeMap::new(),
        )?;
        Ok(RuntimeInvocation {
            command,
            working_directory,
            stdin: task.prompt.as_bytes().to_vec(),
            output_protocol: RuntimeOutputProtocol::JsonLines,
        })
    }

    fn decoder(&self, run_id: RunId) -> Box<dyn RuntimeEventDecoder> {
        Box::new(CodexTaskDecoder::new(run_id))
    }
}

pub struct CodexTaskDecoder {
    events: EventFactory,
}

impl CodexTaskDecoder {
    #[must_use]
    pub const fn new(run_id: RunId) -> Self {
        Self {
            events: EventFactory::new(run_id),
        }
    }

    fn decode_item(&mut self, value: &Value) -> Result<Vec<RuntimeEvent>, OpenWorkError> {
        let item = value
            .get("item")
            .ok_or_else(|| runtime_error("Codex item event omitted its item"))?;
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| runtime_error("Codex item omitted its type"))?;
        match item_type {
            "agent_message" => {
                Ok(vec![self.events.event(
                    RuntimeEventPayload::Message {
                        content:
                            bounded_redacted_text(
                                item.get("text").and_then(Value::as_str).ok_or_else(|| {
                                    runtime_error("Codex agent message omitted text")
                                })?,
                            )?,
                    },
                    empty_metadata(),
                )?])
            }
            "command_execution" | "file_change" | "mcp_tool_call" | "web_search" => {
                Ok(vec![self.events.event(
                    RuntimeEventPayload::ToolCall {
                        name: item_type.to_owned(),
                        parameters: bounded_redacted_value(item)?,
                    },
                    empty_metadata(),
                )?])
            }
            _ => Ok(vec![self.events.event(
                RuntimeEventPayload::Message {
                    content: "Codex vendor item omitted".to_owned(),
                },
                unknown_metadata(item, item_type)?,
            )?]),
        }
    }
}

impl RuntimeEventDecoder for CodexTaskDecoder {
    fn decode_stdout_line(&mut self, line: &[u8]) -> Result<Vec<RuntimeEvent>, OpenWorkError> {
        let value = parse_json_line(line)?;
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| runtime_error("Codex event omitted its type"))?;
        match event_type {
            "thread.started" => {
                Ok(vec![self.events.event(
                    RuntimeEventPayload::Started,
                    minimal_metadata(&value)?,
                )?])
            }
            "item.completed" => self.decode_item(&value),
            "error" | "turn.failed" => {
                let message = value
                    .get("message")
                    .or_else(|| value.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex reported a provider failure");
                Ok(vec![self.events.fail(
                    "codex_provider_error",
                    message,
                    minimal_metadata(&value)?,
                )?])
            }
            "turn.completed" => Ok(vec![self.events.event(
                RuntimeEventPayload::Message {
                    content: "Codex provider turn completed".to_owned(),
                },
                minimal_metadata(&value)?,
            )?]),
            _ => Ok(vec![self.events.event(
                RuntimeEventPayload::Message {
                    content: "Codex vendor event omitted".to_owned(),
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
        "thread_id": value.get("thread_id")
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
