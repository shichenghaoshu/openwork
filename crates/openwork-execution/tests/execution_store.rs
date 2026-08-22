use openwork_execution::audit::AuditAppend;
use openwork_execution::store::{
    CancelRequest, CancellationEvidence, ExecutionStore, InMemoryExecutionStore, LeaseToken,
    RunLease, RunQueueRepository,
};
use openwork_execution::{
    ActionId, ActorId, Artifact, ArtifactId, ArtifactSizeBytes, AuditEventType,
    EXECUTION_SCHEMA_VERSION, RelativeArtifactPath, Run, RunId, RunStatus, SandboxCleanupStatus,
    SandboxResult, SandboxTermination, UtcTimestamp, sha256_bytes,
};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use time::Duration;

#[test]
fn queued_cancel_is_immediate_but_leased_cancel_requires_worker_confirmation() {
    let store = InMemoryExecutionStore::default();
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create");
    assert_eq!(
        store
            .request_cancel(&run.id, actor(), timestamp("2026-08-10T00:00:01Z"))
            .expect("cancel"),
        CancelRequest::Cancelled
    );
    assert_eq!(
        store.get_run(&run.id).expect("read").expect("run").status,
        RunStatus::Cancelled
    );
    assert_eq!(
        store
            .request_cancel(&run.id, actor(), timestamp("2026-08-10T00:00:00Z"))
            .expect("terminal cancellation replay"),
        CancelRequest::AlreadyTerminal(RunStatus::Cancelled)
    );

    let mut leased = queued_run();
    leased.id = RunId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a03").expect("UUIDv7");
    leased.created_at = timestamp("2026-08-10T00:01:00Z");
    leased.updated_at = timestamp("2026-08-10T00:01:00Z");
    store
        .create_run(leased.clone(), audit("2026-08-10T00:01:00Z"))
        .expect("create");
    let lease = store
        .claim_next_run(
            actor(),
            timestamp("2026-08-10T00:01:01Z"),
            Duration::seconds(30),
        )
        .expect("claim")
        .expect("lease");
    assert_eq!(
        store
            .request_cancel(&leased.id, actor(), timestamp("2026-08-10T00:01:02Z"))
            .expect("request"),
        CancelRequest::Requested
    );
    assert!(
        store
            .lease_cancel_requested(&lease, timestamp("2026-08-10T00:01:02Z"))
            .expect("poll")
    );
    assert_eq!(
        store
            .request_cancel(&leased.id, actor(), timestamp("2026-08-10T00:01:01Z"))
            .expect("active cancellation replay"),
        CancelRequest::Requested
    );
    assert_eq!(
        store
            .get_run(&leased.id)
            .expect("read")
            .expect("run")
            .status,
        RunStatus::Planning
    );
}

#[test]
fn expired_lease_fails_closed_without_requeue() {
    let store = InMemoryExecutionStore::default();
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create");
    let lease = store
        .claim_next_run(
            actor(),
            timestamp("2026-08-10T00:00:01Z"),
            Duration::seconds(1),
        )
        .expect("claim")
        .expect("lease");
    assert_eq!(
        store
            .recover_expired_leases(actor(), timestamp("2026-08-10T00:00:02Z"))
            .expect("recover"),
        vec![run.id.clone()]
    );
    assert_eq!(
        store.get_run(&run.id).expect("read").expect("run").status,
        RunStatus::Failed
    );
    assert!(
        store
            .heartbeat_lease(
                &lease,
                timestamp("2026-08-10T00:00:02Z"),
                Duration::seconds(1)
            )
            .is_err()
    );
    assert!(
        store
            .claim_next_run(
                actor(),
                timestamp("2026-08-10T00:00:03Z"),
                Duration::seconds(1)
            )
            .expect("claim")
            .is_none()
    );
}

#[test]
fn leased_worker_writes_require_capability_and_preserve_atomic_artifacts() {
    let store = InMemoryExecutionStore::default();
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create");
    let lease = store
        .claim_next_run(
            ActorId::parse("worker:leased").expect("worker"),
            timestamp("2026-08-10T00:00:01Z"),
            Duration::seconds(30),
        )
        .expect("claim")
        .expect("lease");
    let now = timestamp("2026-08-10T00:00:02Z");

    assert!(
        store
            .append_audit(
                &run.id,
                AuditEventType::RuntimeOutput,
                AuditAppend::new(actor(), now)
            )
            .is_err()
    );
    assert!(
        store
            .record_artifacts(
                &run.id,
                vec![artifact(&run.id, "report.txt")],
                audit("2026-08-10T00:00:02Z")
            )
            .is_err()
    );
    let receipt = openwork_execution::action_executor::ActionExecutionReceipt {
        run_id: run.id.clone(),
        action_id: ActionId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a12").expect("id"),
        parameter_hash: sha256_bytes(b"parameters"),
    };
    assert!(
        store
            .reconcile_action_execution(&receipt, audit("2026-08-10T00:00:02Z"))
            .is_err()
    );

    let event = store
        .append_leased_runtime_audit(
            &lease,
            lease.run.revision,
            AuditEventType::RuntimeStarted,
            now,
        )
        .expect("lease-bound runtime audit");
    assert_eq!(event.actor, lease.owner);
    assert!(
        store
            .append_leased_runtime_audit(
                &lease,
                lease.run.revision,
                AuditEventType::PolicyAllowed,
                timestamp("2026-08-10T00:00:03Z"),
            )
            .is_err()
    );

    let first = artifact(&run.id, "report.txt");
    let duplicate = artifact(&run.id, "report.txt");
    assert!(
        store
            .record_leased_artifacts(
                &lease,
                lease.run.revision,
                vec![first, duplicate],
                timestamp("2026-08-10T00:00:03Z"),
            )
            .is_err()
    );
    assert!(store.artifacts(&run.id).expect("read artifacts").is_empty());
    store
        .record_leased_artifacts(
            &lease,
            lease.run.revision,
            vec![artifact(&run.id, "report.txt")],
            timestamp("2026-08-10T00:00:03Z"),
        )
        .expect("atomic leased artifact batch");
    let events = store.audit_events(&run.id).expect("events");
    assert_eq!(events.last().expect("artifact event").actor, lease.owner);
}

#[test]
fn cancellation_evidence_cannot_cross_lease_capabilities() {
    let store = InMemoryExecutionStore::default();
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create");
    let lease = store
        .claim_next_run(
            actor(),
            timestamp("2026-08-10T00:00:01Z"),
            Duration::seconds(30),
        )
        .expect("claim")
        .expect("lease");
    store
        .request_cancel(&run.id, actor(), timestamp("2026-08-10T00:00:02Z"))
        .expect("request cancellation");
    let result = SandboxResult {
        schema_version: EXECUTION_SCHEMA_VERSION,
        run_id: run.id.clone(),
        sandbox_id: "sandbox-for-current-lease".to_owned(),
        termination: SandboxTermination::Cancelled,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        started_at: timestamp("2026-08-10T00:00:01Z"),
        completed_at: timestamp("2026-08-10T00:00:03Z"),
        output_paths: Vec::new(),
        cleanup: SandboxCleanupStatus::Succeeded,
    };
    let evidence = CancellationEvidence::verify(&lease, &result).expect("valid evidence");
    let different_lease = RunLease {
        token: LeaseToken::generate(),
        ..lease.clone()
    };

    assert!(
        store
            .confirm_cancel(
                &different_lease,
                timestamp("2026-08-10T00:00:04Z"),
                evidence.clone(),
            )
            .is_err()
    );
    assert_eq!(
        store
            .confirm_cancel(&lease, timestamp("2026-08-10T00:00:04Z"), evidence)
            .expect("current lease confirms cancellation")
            .status,
        RunStatus::Cancelled
    );
}

#[test]
fn leased_lifecycle_requires_current_capability_and_removes_it_on_completion() {
    let store = InMemoryExecutionStore::default();
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create");
    let lease = store
        .claim_next_run(
            actor(),
            timestamp("2026-08-10T00:00:01Z"),
            Duration::seconds(30),
        )
        .expect("claim")
        .expect("lease");
    assert!(
        store
            .transition_run(
                &run.id,
                lease.run.revision,
                RunStatus::Running,
                None,
                audit("2026-08-10T00:00:02Z"),
            )
            .is_err()
    );
    let running = store
        .transition_leased_run(
            &lease,
            lease.run.revision,
            RunStatus::Running,
            timestamp("2026-08-10T00:00:02Z"),
        )
        .expect("lease-bound start");
    assert_eq!(running.run.status, RunStatus::Running);
    assert_eq!(running.run.revision, lease.run.revision + 1);
    assert_eq!(
        store
            .heartbeat_lease(
                &running,
                timestamp("2026-08-10T00:00:02Z"),
                Duration::seconds(30),
            )
            .expect("heartbeat")
            .run,
        running.run
    );
    assert!(
        store
            .transition_leased_run(
                &lease,
                lease.run.revision,
                RunStatus::AwaitingApproval,
                timestamp("2026-08-10T00:00:03Z"),
            )
            .is_err()
    );
    let completed = store
        .complete_leased_run(
            &running,
            running.run.revision,
            RunStatus::Succeeded,
            None,
            timestamp("2026-08-10T00:00:03Z"),
        )
        .expect("lease-bound completion");
    assert_eq!(completed.status, RunStatus::Succeeded);
    assert!(
        store
            .heartbeat_lease(
                &running,
                timestamp("2026-08-10T00:00:04Z"),
                Duration::seconds(30),
            )
            .is_err()
    );
}

#[test]
fn cancellation_intent_rejects_success_but_allows_failure_and_orphans_fail_closed() {
    let store = InMemoryExecutionStore::default();
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create");
    let lease = store
        .claim_next_run(
            actor(),
            timestamp("2026-08-10T00:00:01Z"),
            Duration::seconds(30),
        )
        .expect("claim")
        .expect("lease");
    let running = store
        .transition_leased_run(
            &lease,
            lease.run.revision,
            RunStatus::Running,
            timestamp("2026-08-10T00:00:02Z"),
        )
        .expect("start");
    assert_eq!(
        store
            .request_cancel(&run.id, actor(), timestamp("2026-08-10T00:00:03Z"))
            .expect("intent"),
        CancelRequest::Requested
    );
    assert!(
        store
            .complete_leased_run(
                &running,
                running.run.revision,
                RunStatus::Succeeded,
                None,
                timestamp("2026-08-10T00:00:04Z"),
            )
            .is_err()
    );
    assert!(
        store
            .complete_leased_run(
                &running,
                running.run.revision,
                RunStatus::Cancelled,
                None,
                timestamp("2026-08-10T00:00:04Z"),
            )
            .is_err()
    );
    assert_eq!(
        store
            .complete_leased_run(
                &running,
                running.run.revision,
                RunStatus::Failed,
                Some("runtime stopped after cancellation"),
                timestamp("2026-08-10T00:00:04Z"),
            )
            .expect("failure may close cancellation race")
            .status,
        RunStatus::Failed
    );

    let mut orphan = queued_run();
    orphan.id = RunId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a04").expect("UUIDv7");
    orphan.created_at = timestamp("2026-08-10T00:01:00Z");
    orphan.updated_at = orphan.created_at;
    store
        .create_run(orphan.clone(), audit("2026-08-10T00:01:00Z"))
        .expect("create orphan");
    store
        .transition_run(
            &orphan.id,
            0,
            RunStatus::Planning,
            None,
            audit("2026-08-10T00:01:01Z"),
        )
        .expect("make orphan active");
    assert!(
        store
            .request_cancel(&orphan.id, actor(), timestamp("2026-08-10T00:01:02Z"))
            .is_err()
    );
}

#[test]
fn illegal_transition_is_atomic_and_audit_is_redacted() {
    let store = InMemoryExecutionStore::default();
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create run");
    let illegal = store.transition_run(
        &run.id,
        0,
        RunStatus::Succeeded,
        None,
        audit("2026-08-10T00:00:01Z"),
    );
    assert!(illegal.is_err());
    assert_eq!(store.get_run(&run.id).expect("read run"), Some(run.clone()));

    let event = store
        .append_audit(
            &run.id,
            AuditEventType::RuntimeOutput,
            AuditAppend::new(actor(), timestamp("2026-08-10T00:00:02Z")),
        )
        .expect("append event");
    let encoded = serde_json::to_string(&event).expect("serialize event");
    assert_eq!(event.metadata.as_map().len(), 0);
    assert!(!encoded.contains("Authorization"));

    let events = store.audit_events(&run.id).expect("read audit");
    events[0].verify_integrity(1, None).expect("genesis");
    events[1]
        .verify_integrity(2, Some(events[0].event_hash()))
        .expect("second event");
}

#[test]
fn persistence_rechecks_public_contract_fields() {
    let store = InMemoryExecutionStore::default();
    let mut invalid_run = queued_run();
    invalid_run.runtime = "x".repeat(129);
    assert!(
        store
            .create_run(invalid_run, audit("2026-08-10T00:00:00Z"))
            .is_err()
    );

    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create valid run");
    let invalid_artifact = Artifact {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: ArtifactId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a02").expect("UUIDv7"),
        run_id: run.id.clone(),
        path: RelativeArtifactPath::parse("report.txt").expect("path"),
        media_type: String::new(),
        size_bytes: ArtifactSizeBytes::new(1).expect("size"),
        sha256: sha256_bytes(b"x"),
        created_at: timestamp("2026-08-10T00:00:01Z"),
    };
    assert!(
        store
            .record_artifacts(
                &run.id,
                vec![invalid_artifact],
                audit("2026-08-10T00:00:01Z"),
            )
            .is_err()
    );
    assert!(store.artifacts(&run.id).expect("artifacts").is_empty());
}

#[test]
fn concurrent_cancel_and_complete_have_one_cas_winner() {
    let store = Arc::new(InMemoryExecutionStore::default());
    let run = queued_run();
    store
        .create_run(run.clone(), audit("2026-08-10T00:00:00Z"))
        .expect("create run");
    store
        .transition_run(
            &run.id,
            0,
            RunStatus::Planning,
            None,
            audit("2026-08-10T00:00:01Z"),
        )
        .expect("plan");
    store
        .transition_run(
            &run.id,
            1,
            RunStatus::Running,
            None,
            audit("2026-08-10T00:00:02Z"),
        )
        .expect("start");

    let barrier = Arc::new(Barrier::new(3));
    let attempts = [RunStatus::Cancelled, RunStatus::Succeeded].map(|status| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let run_id = run.id.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.transition_run(
                &run_id,
                2,
                status,
                Some("token=must-not-persist"),
                audit("2026-08-10T00:00:03Z"),
            )
        })
    });
    barrier.wait();
    let results = attempts.map(|handle| handle.join().expect("thread"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let stored = store
        .get_run(&run.id)
        .expect("read run")
        .expect("stored run");
    assert!(stored.status.is_terminal());
    assert_eq!(stored.revision, 3);
    let events = store.audit_events(&run.id).expect("audit");
    assert_eq!(events.len(), 4);
    assert_eq!(
        events.last().expect("terminal event").metadata.as_map()["run_status"],
        serde_json::to_value(stored.status).expect("status")
    );
    assert!(
        !stored
            .terminal_reason
            .as_deref()
            .unwrap_or_default()
            .contains("must-not-persist")
    );
}

fn queued_run() -> Run {
    let now = timestamp("2026-08-10T00:00:00Z");
    Run {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: RunId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a01").expect("UUIDv7"),
        runtime: "mock".to_owned(),
        workspace: PathBuf::from("/workspace"),
        status: RunStatus::Queued,
        revision: 0,
        actor_id: actor(),
        prompt_sha256: sha256_bytes(b"enterprise prompt"),
        created_at: now,
        updated_at: now,
        started_at: None,
        completed_at: None,
        terminal_reason: None,
    }
}

fn audit(value: &str) -> AuditAppend {
    AuditAppend::new(actor(), timestamp(value))
}

fn artifact(run_id: &RunId, path: &str) -> Artifact {
    Artifact {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: ArtifactId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a11").expect("UUIDv7"),
        run_id: run_id.clone(),
        path: RelativeArtifactPath::parse(path).expect("path"),
        media_type: "text/plain".to_owned(),
        size_bytes: ArtifactSizeBytes::new(4).expect("size"),
        sha256: sha256_bytes(b"data"),
        created_at: timestamp("2026-08-10T00:00:03Z"),
    }
}

fn actor() -> ActorId {
    ActorId::parse("user:test").expect("actor")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("timestamp")
}
