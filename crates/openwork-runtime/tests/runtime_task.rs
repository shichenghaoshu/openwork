use openwork_core::ErrorCode;
use openwork_execution::{
    EXECUTION_SCHEMA_VERSION, RunId, RuntimeEventPayload, RuntimeTask, SandboxCleanupStatus,
    SandboxResult, SandboxTermination, UtcTimestamp,
};
use openwork_runtime::task::{
    CLAUDE_CLI_REFERENCE_URL, CLAUDE_HEADLESS_URL, CODEX_CLI_REFERENCE_URL,
    CODEX_NON_INTERACTIVE_URL, ClaudeTaskAdapter, CodexTaskAdapter, MAX_PROVIDER_LINE_BYTES,
    MAX_RUNTIME_PROMPT_BYTES, OPENCODE_CLI_REFERENCE_URL, OpenCodeTaskAdapter,
    PI_RPC_REFERENCE_URL, PROVIDER_DOCS_ACCESSED_ON, PROVIDER_VERSION_GATE_POLICY, PiTaskAdapter,
    RuntimeEventDecoder, RuntimeTaskAdapter, decode_sandbox_result,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

fn task(
    runtime: &str,
    capabilities: &[&str],
    prompt: &str,
    working_directory: &str,
) -> RuntimeTask {
    let digest = Sha256::digest(prompt.as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut digest, byte| {
            write!(digest, "{byte:02x}").expect("writing to String cannot fail");
            digest
        },
    );
    serde_json::from_value(json!({
        "schema_version": 1,
        "run_id": RunId::generate(),
        "runtime": runtime,
        "prompt": prompt,
        "prompt_hash": digest,
        "working_directory": working_directory,
        "timeout_seconds": 300,
        "capabilities": capabilities
    }))
    .expect("valid runtime task")
}

fn arguments(invocation: &openwork_runtime::task::RuntimeInvocation) -> &[String] {
    invocation.command.arguments()
}

fn decode_fixture(
    decoder: &mut dyn RuntimeEventDecoder,
    fixture: &str,
) -> Vec<openwork_execution::RuntimeEvent> {
    fixture
        .lines()
        .flat_map(|line| {
            decoder
                .decode_stdout_line(line.as_bytes())
                .expect("fixture line")
        })
        .collect()
}

fn sandbox_result(
    run_id: RunId,
    termination: SandboxTermination,
    exit_code: Option<i32>,
    stdout: impl Into<String>,
    stderr: impl Into<String>,
) -> SandboxResult {
    let timestamp = UtcTimestamp::parse("2026-08-10T00:00:00Z").expect("timestamp");
    SandboxResult {
        schema_version: EXECUTION_SCHEMA_VERSION,
        run_id,
        sandbox_id: "sandbox-runtime-output".to_owned(),
        termination,
        exit_code,
        stdout: stdout.into(),
        stderr: stderr.into(),
        truncated: false,
        started_at: timestamp,
        completed_at: timestamp,
        output_paths: Vec::new(),
        cleanup: SandboxCleanupStatus::Succeeded,
    }
}

#[test]
fn claude_preparation_is_shell_free_ephemeral_and_prompt_private() {
    let prompt = "Analyze data; $(touch /tmp/not-run) --dangerously-skip-permissions";
    let task = task(
        "claude-code",
        &["filesystem.read", "filesystem.write"],
        prompt,
        "/workspace/output",
    );
    let invocation = ClaudeTaskAdapter::new("/usr/local/bin/claude")
        .prepare(&task)
        .expect("Claude invocation");
    let debug = format!("{task:?}\n{invocation:?}");
    let args = arguments(&invocation);

    assert!(!debug.contains(prompt));
    assert_eq!(invocation.command.program(), "/usr/local/bin/claude");
    assert_eq!(invocation.stdin, prompt.as_bytes());
    assert_eq!(invocation.working_directory.as_str(), "/workspace/output");
    assert!(!args.iter().any(|argument| argument.contains(prompt)));
    for required in [
        "--safe-mode",
        "--print",
        "stream-json",
        "--no-session-persistence",
        "--no-chrome",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "Read,Write,Edit,Glob,Grep",
        "acceptEdits",
    ] {
        assert!(
            args.iter().any(|argument| argument == required),
            "missing {required}"
        );
    }
    assert!(!args.iter().any(|argument| {
        argument == "--dangerously-skip-permissions" || argument == "bypassPermissions"
    }));
    assert!(invocation.command.environment().is_empty());
}

#[test]
fn codex_preparation_sets_explicit_defense_in_depth_flags() {
    let prompt = "Create summary.md\n; rm -rf /";
    let task = task(
        "codex",
        &["filesystem.read", "filesystem.write"],
        prompt,
        "/workspace/output",
    );
    let invocation = CodexTaskAdapter::new("/usr/local/bin/codex")
        .prepare(&task)
        .expect("Codex invocation");
    let args = arguments(&invocation);

    assert_eq!(invocation.command.program(), "/usr/local/bin/codex");
    assert_eq!(invocation.stdin, prompt.as_bytes());
    assert_eq!(invocation.working_directory.as_str(), "/workspace/output");
    assert_eq!(args.first().map(String::as_str), Some("exec"));
    assert_eq!(args.last().map(String::as_str), Some("-"));
    assert!(!args.iter().any(|argument| argument.contains(prompt)));
    for required in [
        "--json",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--strict-config",
        "--sandbox",
        "workspace-write",
        "--skip-git-repo-check",
    ] {
        assert!(
            args.iter().any(|argument| argument == required),
            "missing {required}"
        );
    }
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--cd", "/workspace/output"])
    );
    // --ask-for-approval was removed from the Codex CLI (post v0.128);
    // --full-auto is deprecated in favour of --sandbox <mode>.
    // Neither flag should appear in a current invocation.
    for removed_flag in ["--ask-for-approval", "--full-auto"] {
        assert!(
            !args.iter().any(|argument| argument == removed_flag),
            "removed/deprecated flag {removed_flag} must not be present"
        );
    }
    assert!(!args.iter().any(|argument| {
        argument == "--yolo" || argument == "--dangerously-bypass-approvals-and-sandbox"
    }));
}

#[test]
fn opencode_preparation_uses_piped_prompt_and_machine_json() {
    let prompt = "classify this; $(do-not-run)";
    let invocation = OpenCodeTaskAdapter::new("/usr/local/bin/opencode")
        .prepare(&task(
            "opencode",
            &["filesystem.read"],
            prompt,
            "/workspace",
        ))
        .expect("OpenCode invocation");
    assert_eq!(invocation.command.program(), "/usr/local/bin/opencode");
    assert_eq!(arguments(&invocation), ["run", "--format", "json"]);
    assert_eq!(invocation.stdin, prompt.as_bytes());
    assert!(!format!("{invocation:?}").contains(prompt));
    assert!(
        !arguments(&invocation)
            .iter()
            .any(|arg| arg.contains(prompt))
    );
}

#[test]
fn pi_preparation_uses_rpc_jsonl_over_stdin() {
    let prompt = "inspect project; $(do-not-run)";
    let invocation = PiTaskAdapter::new("/usr/local/bin/pi")
        .prepare(&task("pi", &["filesystem.read"], prompt, "/workspace"))
        .expect("Pi invocation");
    assert_eq!(invocation.command.program(), "/usr/local/bin/pi");
    assert_eq!(arguments(&invocation), ["--mode", "rpc", "--no-session"]);
    let request: serde_json::Value = serde_json::from_slice(&invocation.stdin).expect("RPC JSON");
    assert_eq!(request["type"], "prompt");
    assert_eq!(request["message"], prompt);
    assert!(!format!("{invocation:?}").contains(prompt));
    assert!(
        !arguments(&invocation)
            .iter()
            .any(|arg| arg.contains(prompt))
    );
}

#[test]
fn opencode_and_pi_decoders_accept_documented_machine_events() {
    let run_id = RunId::generate();
    let mut opencode = OpenCodeTaskAdapter::new("/opencode").decoder(run_id.clone());
    let start = opencode
        .decode_stdout_line(br#"{"type":"session.created","sessionID":"s"}"#)
        .expect("OpenCode start");
    assert!(matches!(start[0].payload, RuntimeEventPayload::Started));
    let message = opencode
        .decode_stdout_line(br#"{"type":"message.part.updated","properties":{"part":{"type":"text","text":"hello"}}}"#)
        .expect("OpenCode message");
    assert!(
        matches!(&message[0].payload, RuntimeEventPayload::Message { content } if content == "hello")
    );

    let mut pi = PiTaskAdapter::new("/pi").decoder(run_id);
    let session = pi
        .decode_stdout_line(br#"{"type":"session","version":3}"#)
        .expect("Pi session");
    assert!(matches!(session[0].payload, RuntimeEventPayload::Started));
    let delta = pi.decode_stdout_line(br#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello"}}"#).expect("Pi delta");
    assert!(
        matches!(&delta[0].payload, RuntimeEventPayload::Message { content } if content == "hello")
    );
}

#[test]
fn read_only_tasks_reduce_provider_permissions() {
    let claude = ClaudeTaskAdapter::new("/opt/claude")
        .prepare(&task(
            "claude-code",
            &["filesystem.read"],
            "read",
            "/workspace",
        ))
        .unwrap();
    assert!(
        arguments(&claude)
            .iter()
            .any(|argument| argument == "Read,Glob,Grep")
    );
    assert!(
        arguments(&claude)
            .iter()
            .any(|argument| argument == "dontAsk")
    );

    let codex = CodexTaskAdapter::new("/opt/codex")
        .prepare(&task("codex", &["filesystem.read"], "read", "/workspace"))
        .unwrap();
    assert!(
        arguments(&codex)
            .iter()
            .any(|argument| argument == "read-only")
    );
}

#[test]
fn mismatched_unknown_and_oversized_tasks_fail_closed() {
    let adapter = ClaudeTaskAdapter::new("/opt/claude");
    assert!(
        adapter
            .prepare(&task("codex", &["filesystem.read"], "read", "/workspace"))
            .is_err()
    );
    assert!(
        adapter
            .prepare(&task("claude-code", &["http.get"], "read", "/workspace"))
            .is_err()
    );
    assert!(
        adapter
            .prepare(&task("claude-code", &[], "read", "/workspace"))
            .is_err()
    );
    assert!(
        adapter
            .prepare(&task(
                "claude-code",
                &["filesystem.write"],
                "write",
                "/workspace"
            ))
            .is_err()
    );
    let oversized = "x".repeat(MAX_RUNTIME_PROMPT_BYTES + 1);
    assert!(
        adapter
            .prepare(&task(
                "claude-code",
                &["filesystem.read"],
                &oversized,
                "/workspace"
            ))
            .is_err()
    );
    assert!(
        ClaudeTaskAdapter::new("claude")
            .prepare(&task(
                "claude-code",
                &["filesystem.read"],
                "read",
                "/workspace"
            ))
            .is_err()
    );
}

#[test]
fn claude_fixture_maps_events_with_redaction_and_sequence() {
    let run_id = RunId::generate();
    let adapter = ClaudeTaskAdapter::new("/opt/claude");
    let mut decoder = adapter.decoder(run_id.clone());
    let events = decode_fixture(
        decoder.as_mut(),
        include_str!("fixtures/claude-stream.jsonl"),
    );
    assert_eq!(events.len(), 5);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.run_id, run_id);
        assert_eq!(event.sequence, u64::try_from(index + 1).unwrap());
    }
    assert!(matches!(events[0].payload, RuntimeEventPayload::Started));
    assert!(matches!(
        &events[1].payload,
        RuntimeEventPayload::Message { content } if content == "analysis [REDACTED] complete"
    ));
    let RuntimeEventPayload::ToolCall { parameters, .. } = &events[2].payload else {
        panic!("expected tool call");
    };
    assert_eq!(parameters["api_key"], "[REDACTED]");
    assert_eq!(
        events[3].vendor_metadata.as_map()["vendor_payload"],
        "[REDACTED]"
    );
    assert!(matches!(
        decoder.finish(0).unwrap().unwrap().payload,
        RuntimeEventPayload::Completed { exit_code: 0 }
    ));
}

#[test]
fn codex_fixture_maps_tools_but_never_claims_trusted_artifacts() {
    let run_id = RunId::generate();
    let adapter = CodexTaskAdapter::new("/opt/codex");
    let mut decoder = adapter.decoder(run_id);
    let events = decode_fixture(
        decoder.as_mut(),
        include_str!("fixtures/codex-stream.jsonl"),
    );
    assert_eq!(events.len(), 6);
    assert!(matches!(events[0].payload, RuntimeEventPayload::Started));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.payload, RuntimeEventPayload::Artifact { .. }))
    );
    let command = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeEventPayload::ToolCall { name, parameters } if name == "command_execution" => {
                Some(parameters)
            }
            _ => None,
        })
        .expect("command event");
    assert_eq!(command["command"], "printf [REDACTED]");
    assert!(matches!(
        decoder.finish(0).unwrap().unwrap().payload,
        RuntimeEventPayload::Completed { exit_code: 0 }
    ));
}

#[test]
fn malformed_duplicate_oversized_and_secret_stderr_are_handled() {
    let mut malformed = CodexTaskAdapter::new("/opt/codex").decoder(RunId::generate());
    assert_eq!(
        malformed.decode_stdout_line(b"not json").unwrap_err().code,
        ErrorCode::ExecutionFailed
    );
    assert!(
        malformed
            .decode_stdout_line(br#"{"type":"turn.started","type":"turn.completed"}"#)
            .is_err()
    );
    assert!(
        malformed
            .decode_stdout_line(&vec![b'x'; MAX_PROVIDER_LINE_BYTES + 1])
            .is_err()
    );
    let stderr = malformed
        .decode_stderr_line(b"Authorization: Bearer secret")
        .expect("redacted stderr");
    let RuntimeEventPayload::Stderr { chunk, .. } = stderr.payload else {
        panic!("expected stderr event");
    };
    assert!(!chunk.contains("Bearer"));
    assert!(!chunk.contains("secret"));
    assert!(chunk.contains("[REDACTED]"));

    let mut tool_name = ClaudeTaskAdapter::new("/opt/claude").decoder(RunId::generate());
    let tool_event = tool_name
        .decode_stdout_line(
            br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TOKEN=secret","input":{}}]}}"#,
        )
        .expect("redacted tool event");
    assert!(matches!(
        &tool_event[0].payload,
        RuntimeEventPayload::ToolCall { name, .. } if name == "[REDACTED]"
    ));

    let large = json!({
        "type": "item.completed",
        "item": {"type": "command_execution", "safe": "x".repeat(20 * 1024)}
    });
    assert!(
        malformed
            .decode_stdout_line(large.to_string().as_bytes())
            .is_err()
    );
}

#[test]
fn provider_failure_is_terminal_and_real_exit_code_drives_completion() {
    let mut claude = ClaudeTaskAdapter::new("/opt/claude").decoder(RunId::generate());
    let failed = claude
        .decode_stdout_line(
            br#"{"type":"result","subtype":"error","is_error":true,"result":"TOKEN=secret"}"#,
        )
        .unwrap();
    assert!(matches!(
        &failed[0].payload,
        RuntimeEventPayload::Failed { message, .. } if message == "[REDACTED]"
    ));
    assert!(claude.finish(1).unwrap().is_none());

    let mut codex = CodexTaskAdapter::new("/opt/codex").decoder(RunId::generate());
    assert!(matches!(
        codex.finish(17).unwrap().unwrap().payload,
        RuntimeEventPayload::Failed { ref code, .. } if code == "provider_exit_nonzero"
    ));
}

#[test]
fn sandbox_result_decoding_maps_jsonl_stderr_and_observed_exit() {
    let run_id = RunId::generate();
    let result = sandbox_result(
        run_id.clone(),
        SandboxTermination::Exited,
        Some(0),
        include_str!("fixtures/codex-stream.jsonl"),
        "Authorization: Bearer stderr-secret\n",
    );
    let mut decoder = CodexTaskAdapter::new("/opt/codex").decoder(run_id.clone());

    let events = decode_sandbox_result(&result, decoder.as_mut()).expect("decoded result");

    assert_eq!(events.len(), 8);
    assert!(events.iter().enumerate().all(|(index, event)| {
        event.run_id == run_id && event.sequence == u64::try_from(index + 1).unwrap()
    }));
    let RuntimeEventPayload::Stderr {
        chunk,
        truncated: false,
    } = &events[6].payload
    else {
        panic!("expected stderr event");
    };
    assert!(chunk.contains("[REDACTED]"));
    assert!(!chunk.contains("Bearer"));
    assert!(!chunk.contains("stderr-secret"));
    assert!(matches!(
        events[7].payload,
        RuntimeEventPayload::Completed { exit_code: 0 }
    ));
    let debug = format!("{events:?}");
    assert!(!debug.contains("stderr-secret"));
    assert!(!debug.contains("TOKEN=visible"));
}

#[test]
fn sandbox_result_decoding_fails_closed_for_incomplete_or_invalid_streams() {
    let run_id = RunId::generate();
    let mut truncated = sandbox_result(
        run_id.clone(),
        SandboxTermination::Exited,
        Some(0),
        r#"{"type":"thread.started"}"#,
        "",
    );
    truncated.truncated = true;
    let mut decoder = CodexTaskAdapter::new("/opt/codex").decoder(run_id.clone());
    assert!(decode_sandbox_result(&truncated, decoder.as_mut()).is_err());

    for stdout in [
        "not-json".to_owned(),
        r#"{"type":"thread.started","type":"turn.completed"}"#.to_owned(),
        "\n".to_owned(),
        format!(
            "{{\"type\":\"turn.started\",\"padding\":\"{}\"}}",
            "x".repeat(MAX_PROVIDER_LINE_BYTES)
        ),
    ] {
        let result = sandbox_result(
            run_id.clone(),
            SandboxTermination::Exited,
            Some(0),
            stdout,
            "",
        );
        let mut decoder = CodexTaskAdapter::new("/opt/codex").decoder(run_id.clone());
        assert!(decode_sandbox_result(&result, decoder.as_mut()).is_err());
    }
}

#[test]
fn non_exited_sandbox_states_never_synthesize_provider_completion() {
    for (termination, exit_code) in [
        (SandboxTermination::Cancelled, None),
        (SandboxTermination::TimedOut, None),
        (SandboxTermination::OutOfMemory, None),
        (SandboxTermination::Failed, None),
        (SandboxTermination::Failed, Some(137)),
    ] {
        let run_id = RunId::generate();
        let result = sandbox_result(
            run_id.clone(),
            termination,
            exit_code,
            r#"{"type":"thread.started"}"#,
            "",
        );
        let mut decoder = CodexTaskAdapter::new("/opt/codex").decoder(run_id);
        let events = decode_sandbox_result(&result, decoder.as_mut()).expect("decoded output");
        assert!(matches!(events[0].payload, RuntimeEventPayload::Started));
        assert!(events.iter().all(|event| !matches!(
            event.payload,
            RuntimeEventPayload::Completed { .. }
                | RuntimeEventPayload::Failed { .. }
                | RuntimeEventPayload::Cancelled
        )));
    }
}

#[test]
fn output_after_provider_terminal_and_mismatched_decoder_fail_closed() {
    let run_id = RunId::generate();
    let terminal_then_output = sandbox_result(
        run_id.clone(),
        SandboxTermination::Exited,
        Some(1),
        concat!(
            r#"{"type":"turn.failed","message":"TOKEN=secret"}"#,
            "\n",
            r#"{"type":"turn.completed"}"#
        ),
        "",
    );
    let mut decoder = CodexTaskAdapter::new("/opt/codex").decoder(run_id.clone());
    assert!(decode_sandbox_result(&terminal_then_output, decoder.as_mut()).is_err());

    let valid = sandbox_result(
        run_id,
        SandboxTermination::Exited,
        Some(0),
        r#"{"type":"thread.started"}"#,
        "",
    );
    let mut wrong_decoder = CodexTaskAdapter::new("/opt/codex").decoder(RunId::generate());
    assert!(decode_sandbox_result(&valid, wrong_decoder.as_mut()).is_err());
}

#[test]
fn orchestrator_output_processor_shape_propagates_decoder_errors() {
    let run_id = RunId::generate();
    let malformed = sandbox_result(
        run_id.clone(),
        SandboxTermination::Exited,
        Some(0),
        "not-json",
        "",
    );
    let mut decoder = CodexTaskAdapter::new("/opt/codex").decoder(run_id);
    let mut process_output =
        |result: &SandboxResult| decode_sandbox_result(result, decoder.as_mut()).map(|_events| ());

    // This closure is the exact shape accepted by
    // ExecutionOrchestrator::execute_with_output_processor. The execution
    // crate owns the assertion that this propagated error transitions a run
    // to Failed.
    assert!(process_output(&malformed).is_err());
}

#[test]
fn implementation_records_current_primary_sources() {
    assert_eq!(PROVIDER_DOCS_ACCESSED_ON, "2026-08-23");
    for url in [
        CLAUDE_CLI_REFERENCE_URL,
        CLAUDE_HEADLESS_URL,
        CODEX_NON_INTERACTIVE_URL,
        CODEX_CLI_REFERENCE_URL,
        OPENCODE_CLI_REFERENCE_URL,
        PI_RPC_REFERENCE_URL,
    ] {
        assert!(url.starts_with("https://"));
    }
    assert!(PROVIDER_VERSION_GATE_POLICY.contains("no unverified minimum"));
}
