use super::{
    EventFactory, RuntimeEventDecoder, RuntimeInvocation, RuntimeOutputProtocol,
    RuntimeTaskAdapter, bounded_redacted_text, bounded_redacted_value, metadata_from_untrusted,
    parse_json_line, runtime_error, validate_task,
};
use openwork_core::OpenWorkError;
use openwork_execution::{RunId, RuntimeEvent, RuntimeEventPayload, RuntimeTask, SandboxCommand};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const PI_RUNTIME_ID: &str = "pi";
/// Pi's documented machine interface.  RPC avoids terminal emulation and
/// accepts the prompt exclusively as one JSONL command over stdin.
pub const PI_REQUIRED_FLAGS: &[&str] = &["--mode", "rpc", "--no-session"];

pub struct PiTaskAdapter {
    container_executable: String,
}

impl PiTaskAdapter {
    #[must_use]
    pub fn new(container_executable: impl Into<String>) -> Self {
        Self {
            container_executable: container_executable.into(),
        }
    }
}

impl RuntimeTaskAdapter for PiTaskAdapter {
    fn prepare(&self, task: &RuntimeTask) -> Result<RuntimeInvocation, OpenWorkError> {
        validate_task(task, PI_RUNTIME_ID)?;
        let stdin = serde_json::to_vec(
            &json!({ "id": "openwork-task", "type": "prompt", "message": task.prompt }),
        )
        .map_err(|_| runtime_error("Pi prompt command could not be encoded"))?;
        let mut stdin = stdin;
        stdin.push(b'\n');
        let command = SandboxCommand::new(
            self.container_executable.clone(),
            PI_REQUIRED_FLAGS
                .iter()
                .map(|flag| (*flag).to_owned())
                .collect(),
            BTreeMap::new(),
        )?;
        Ok(RuntimeInvocation {
            command,
            working_directory: task.working_directory.clone(),
            stdin,
            output_protocol: RuntimeOutputProtocol::JsonLines,
        })
    }
    fn decoder(&self, run_id: RunId) -> Box<dyn RuntimeEventDecoder> {
        Box::new(PiTaskDecoder::new(run_id))
    }
}

pub struct PiTaskDecoder {
    events: EventFactory,
}
impl PiTaskDecoder {
    #[must_use]
    pub const fn new(run_id: RunId) -> Self {
        Self {
            events: EventFactory::new(run_id),
        }
    }
    fn metadata(value: &Value) -> Result<openwork_execution::RedactedAuditMetadata, OpenWorkError> {
        metadata_from_untrusted(
            &json!({ "provider_event_type": value.get("type"), "command": value.get("command") }),
        )
    }
}

impl RuntimeEventDecoder for PiTaskDecoder {
    fn decode_stdout_line(&mut self, line: &[u8]) -> Result<Vec<RuntimeEvent>, OpenWorkError> {
        let value = parse_json_line(line)?;
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| runtime_error("Pi event omitted its type"))?;
        match event_type {
            "session" | "agent_start" | "turn_start" => {
                Ok(vec![self.events.event(
                    RuntimeEventPayload::Started,
                    Self::metadata(&value)?,
                )?])
            }
            "message_update" => {
                let update = value
                    .get("assistantMessageEvent")
                    .ok_or_else(|| runtime_error("Pi message update omitted assistant event"))?;
                if update.get("type").and_then(Value::as_str) == Some("text_delta") {
                    let text = update
                        .get("delta")
                        .and_then(Value::as_str)
                        .ok_or_else(|| runtime_error("Pi text delta omitted text"))?;
                    Ok(vec![self.events.event(
                        RuntimeEventPayload::Message {
                            content: bounded_redacted_text(text)?,
                        },
                        Self::metadata(&value)?,
                    )?])
                } else {
                    Ok(vec![])
                }
            }
            "tool_execution_start" => {
                let name = value
                    .get("toolName")
                    .and_then(Value::as_str)
                    .ok_or_else(|| runtime_error("Pi tool event omitted name"))?;
                Ok(vec![self.events.event(
                    RuntimeEventPayload::ToolCall {
                        name: bounded_redacted_text(name)?,
                        parameters: bounded_redacted_value(
                            value.get("args").unwrap_or(&Value::Null),
                        )?,
                    },
                    Self::metadata(&value)?,
                )?])
            }
            "response" if value.get("success").and_then(Value::as_bool) == Some(false) => Ok(vec![
                self.events.fail(
                    "pi_rpc_rejected",
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Pi rejected the RPC command"),
                    Self::metadata(&value)?,
                )?,
            ]),
            "error" => Ok(vec![
                self.events.fail(
                    "pi_provider_error",
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Pi reported a provider failure"),
                    Self::metadata(&value)?,
                )?,
            ]),
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
