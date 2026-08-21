#![cfg(feature = "postgres")]

use openwork_execution::action_executor::ActionExecutionReceipt;
use openwork_execution::approval::ApprovalRepository;
use openwork_execution::audit::AuditAppend;
use openwork_execution::store::postgres::{CRASH_RECOVERY_REASON, PostgresExecutionStore};
use openwork_execution::store::{
    CancelRequest, CancellationEvidence, ExecutionStore, RunQueueRepository,
};
use openwork_execution::{
    ActionId, ActionRequest, ActorId, ApprovalDecision, ApprovalId, ApprovalRequest,
    ApprovalStatus, AuditEventType, EXECUTION_SCHEMA_VERSION, Run, RunId, RunStatus,
    SandboxCleanupStatus, SandboxResult, SandboxTermination, UtcTimestamp, sha256_bytes,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex, MutexGuard, OnceLock};
use time::Duration;

const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");
static DATABASE_TEST_LOCK: Mutex<()> = Mutex::new(());
static DATABASE_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

#[test]
fn postgres_queue_claim_and_cancel_are_transactional() {
    let _database_guard = database_test_guard();
    let Some(store) = postgres_store() else {
        return;
    };
    let run = create_run_in_status(&store, RunStatus::Queued);
    let barrier = Arc::new(Barrier::new(3));
    let attempts = ["worker:first", "worker:second"].map(|owner| {
        let worker_store = store.clone();
        let worker_barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            worker_barrier.wait();
            worker_store.claim_next_run(
                ActorId::parse(owner).expect("worker actor"),
                timestamp("2026-08-21T01:00:01Z"),
                Duration::seconds(30),
            )
        })
    });
    barrier.wait();
    let claims = attempts.map(|attempt| attempt.join().expect("claim thread").expect("claim"));
    assert_eq!(claims.iter().filter(|claim| claim.is_some()).count(), 1);
    let lease = claims
        .into_iter()
        .flatten()
        .next()
        .expect("one worker owns the run");
    assert_eq!(lease.run.id, run.id);

    assert_eq!(
        store
            .request_cancel(&run.id, actor(), timestamp("2026-08-21T01:00:02Z"))
            .expect("request cancel"),
        CancelRequest::Requested
    );
    assert!(
        store
            .lease_cancel_requested(&lease, timestamp("2026-08-21T01:00:02Z"))
            .expect("poll cancellation")
    );
    assert_eq!(
        store
            .request_cancel(&run.id, actor(), timestamp("2026-08-21T01:00:01Z"))
            .expect("idempotent active cancellation replay"),
        CancelRequest::Requested
    );
    let sandbox_result = SandboxResult {
        schema_version: EXECUTION_SCHEMA_VERSION,
        run_id: run.id.clone(),
        sandbox_id: "postgres-cancel-sandbox".to_owned(),
        termination: SandboxTermination::Cancelled,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        started_at: timestamp("2026-08-21T01:00:01Z"),
        completed_at: timestamp("2026-08-21T01:00:03Z"),
        output_paths: Vec::new(),
        cleanup: SandboxCleanupStatus::Succeeded,
    };
    let evidence =
        CancellationEvidence::verify(&lease, &sandbox_result).expect("cancellation evidence");
    let cancelled = store
        .confirm_cancel(&lease, timestamp("2026-08-21T01:00:03Z"), evidence)
        .expect("confirm cancellation");
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(
        store
            .request_cancel(&run.id, actor(), timestamp("2026-08-21T01:00:01Z"))
            .expect("idempotent terminal cancellation replay"),
        CancelRequest::AlreadyTerminal(RunStatus::Cancelled)
    );
    let events = store.audit_events(&run.id).expect("cancellation audit");
    assert_eq!(
        events.last().expect("confirmation event").actor,
        lease.owner
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>(),
        vec![
            AuditEventType::RunCreated,
            AuditEventType::RuntimeSelected,
            AuditEventType::CancelRequested,
            AuditEventType::CancelConfirmed,
        ]
    );
}

#[test]
fn postgres_expired_lease_fails_closed() {
    let _database_guard = database_test_guard();
    let Some(store) = postgres_store() else {
        return;
    };
    let expiring = create_run_in_status(&store, RunStatus::Queued);
    store
        .claim_next_run(
            actor(),
            timestamp("2026-08-21T02:00:01Z"),
            Duration::seconds(1),
        )
        .expect("claim expiring run")
        .expect("expiring lease");
    assert_eq!(
        store
            .recover_expired_leases(actor(), timestamp("2026-08-21T02:00:02Z"))
            .expect("recover expired lease"),
        vec![expiring.id.clone()]
    );
    assert_eq!(
        store
            .get_run(&expiring.id)
            .expect("read recovered run")
            .expect("recovered run")
            .status,
        RunStatus::Failed
    );
}

#[test]
fn postgres_leased_updates_require_current_capability_and_cancel_blocks_success() {
    let _database_guard = database_test_guard();
    let Some(store) = postgres_store() else {
        return;
    };
    let run = create_run_in_status(&store, RunStatus::Queued);
    let lease = store
        .claim_next_run(
            actor(),
            timestamp("2026-08-21T02:30:01Z"),
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
                AuditAppend::new(actor(), timestamp("2026-08-21T02:30:02Z")),
            )
            .is_err()
    );
    let running = store
        .transition_leased_run(
            &lease,
            lease.run.revision,
            RunStatus::Running,
            timestamp("2026-08-21T02:30:02Z"),
        )
        .expect("start");
    assert!(
        store
            .transition_leased_run(
                &lease,
                lease.run.revision,
                RunStatus::AwaitingApproval,
                timestamp("2026-08-21T02:30:03Z"),
            )
            .is_err()
    );
    assert_eq!(
        store
            .request_cancel(&run.id, actor(), timestamp("2026-08-21T02:30:03Z"))
            .expect("request cancellation"),
        CancelRequest::Requested
    );
    assert!(
        store
            .complete_leased_run(
                &running,
                running.run.revision,
                RunStatus::Succeeded,
                None,
                timestamp("2026-08-21T02:30:04Z"),
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
                timestamp("2026-08-21T02:30:04Z"),
            )
            .expect("failure may complete")
            .status,
        RunStatus::Failed
    );
    assert!(
        store
            .heartbeat_lease(
                &running,
                timestamp("2026-08-21T02:30:05Z"),
                Duration::seconds(30),
            )
            .is_err()
    );
}

#[test]
fn postgres_boundary_normalizes_precision_before_hashing_and_round_trips() {
    let _database_guard = database_test_guard();
    let Some(store) = postgres_store() else {
        return;
    };
    let exact = timestamp("2026-08-21T01:00:00.123456789Z");
    let run = Run {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: RunId::generate(),
        runtime: "mock".to_owned(),
        workspace: PathBuf::from("/tmp/openwork-postgres-precision-test"),
        status: RunStatus::Queued,
        revision: 0,
        actor_id: actor(),
        prompt_sha256: sha256_bytes(b"postgres precision test"),
        created_at: exact,
        updated_at: exact,
        started_at: None,
        completed_at: None,
        terminal_reason: None,
    };

    let created = store
        .create_run(run, AuditAppend::new(actor(), exact))
        .expect("create precise run");
    assert_eq!(exact.unix_timestamp_nanos() % 1_000, 789);
    assert_eq!(created.created_at.unix_timestamp_nanos() % 1_000, 0);
    assert_eq!(created.updated_at, created.created_at);

    let stored = store
        .get_run(&created.id)
        .expect("read run")
        .expect("stored run");
    assert_eq!(stored.created_at, created.created_at);
    let audit = store
        .audit_events(&created.id)
        .expect("verified audit chain");
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].timestamp, created.created_at);
    audit[0]
        .verify_integrity(1, None)
        .expect("Postgres audit hash round-trip");
}

#[test]
fn postgres_approve_vs_deny_has_one_cas_winner() {
    let _database_guard = database_test_guard();
    let Some(fixture) = pending_approval_fixture() else {
        return;
    };
    let barrier = Arc::new(Barrier::new(2));
    let approve_store = fixture.store.clone();
    let approve_id = fixture.approval.id.clone();
    let approve_barrier = barrier.clone();
    let approve = std::thread::spawn(move || {
        approve_barrier.wait();
        approve_store.decide_approval(
            &approve_id,
            0,
            ApprovalDecision::Approved,
            actor(),
            None,
            timestamp("2026-08-21T01:00:04Z"),
        )
    });
    let deny_store = fixture.store.clone();
    let deny_id = fixture.approval.id.clone();
    let deny = std::thread::spawn(move || {
        barrier.wait();
        deny_store.decide_approval(
            &deny_id,
            0,
            ApprovalDecision::Denied,
            actor(),
            Some("review decision"),
            timestamp("2026-08-21T01:00:04Z"),
        )
    });

    let results = [
        approve.join().expect("approve thread"),
        deny.join().expect("deny thread"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let stored = fixture
        .store
        .get_approval(&fixture.approval.id)
        .expect("read approval")
        .expect("stored approval");
    assert_eq!(stored.revision, 1);
    assert!(matches!(
        stored.status,
        ApprovalStatus::Approved | ApprovalStatus::Denied
    ));
}

#[test]
fn postgres_consume_vs_consume_creates_one_claim() {
    let _database_guard = database_test_guard();
    let Some(fixture) = approved_approval_fixture() else {
        return;
    };
    let barrier = Arc::new(Barrier::new(2));
    let first_store = fixture.store.clone();
    let first_id = fixture.approval.id.clone();
    let first_action = fixture.action.clone();
    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first_store.consume_approval(
            &first_id,
            1,
            &first_action,
            actor(),
            timestamp("2026-08-21T01:00:05Z"),
        )
    });
    let second_store = fixture.store.clone();
    let second_id = fixture.approval.id.clone();
    let second_action = fixture.action.clone();
    let second = std::thread::spawn(move || {
        barrier.wait();
        second_store.consume_approval(
            &second_id,
            1,
            &second_action,
            actor(),
            timestamp("2026-08-21T01:00:05Z"),
        )
    });

    let results = [
        first.join().expect("first thread"),
        second.join().expect("second thread"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let stored = fixture
        .store
        .get_approval(&fixture.approval.id)
        .expect("read approval")
        .expect("stored approval");
    assert_eq!(stored.status, ApprovalStatus::Consumed);
    assert_eq!(stored.revision, 2);
    assert!(
        fixture
            .store
            .get_action_claim(&fixture.action.id)
            .expect("read claim")
            .is_some()
    );
    let running = fixture
        .store
        .get_run(&fixture.action.run_id)
        .expect("read consumed run")
        .expect("consumed run");
    fixture
        .store
        .transition_run(
            &running.id,
            running.revision,
            RunStatus::Succeeded,
            None,
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:06Z")),
        )
        .expect("finish consumed run");
}

#[test]
fn postgres_action_audit_reconciliation_is_atomic() {
    let _database_guard = database_test_guard();
    let Some(fixture) = approved_approval_fixture() else {
        return;
    };
    let consumption = fixture
        .store
        .consume_approval(
            &fixture.approval.id,
            fixture.approval.revision,
            &fixture.action,
            actor(),
            timestamp("2026-08-21T01:00:05Z"),
        )
        .expect("consume action approval");
    let receipt = ActionExecutionReceipt {
        run_id: fixture.action.run_id.clone(),
        action_id: fixture.action.id.clone(),
        parameter_hash: consumption.action_claim.parameter_hash,
    };
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let worker_store = fixture.store.clone();
        let worker_receipt = receipt.clone();
        let worker_barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            worker_barrier.wait();
            worker_store.reconcile_action_execution(
                &worker_receipt,
                AuditAppend::new(actor(), timestamp("2026-08-21T01:00:06Z")),
            )
        }));
    }
    let mut outcomes = workers
        .into_iter()
        .map(|worker| worker.join().expect("reconciliation thread"))
        .collect::<Result<Vec<_>, _>>()
        .expect("atomic reconciliation outcomes");
    outcomes.sort_unstable();
    assert_eq!(outcomes, vec![false, true]);
    let events = fixture
        .store
        .audit_events(&fixture.action.run_id)
        .expect("action audit events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == AuditEventType::ActionExecuted)
            .count(),
        1
    );
    let running = fixture
        .store
        .get_run(&fixture.action.run_id)
        .expect("read running run")
        .expect("running run");
    fixture
        .store
        .transition_run(
            &running.id,
            running.revision,
            RunStatus::Succeeded,
            None,
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:07Z")),
        )
        .expect("finish reconciled run");
}

#[test]
fn postgres_cancel_vs_complete_has_one_cas_winner() {
    let _database_guard = database_test_guard();
    let Some(store) = postgres_store() else {
        return;
    };
    let run = create_run_in_status(&store, RunStatus::Running);
    let expected_revision = run.revision;
    let barrier = Arc::new(Barrier::new(2));
    let cancel_store = store.clone();
    let cancel_id = run.id.clone();
    let cancel_barrier = barrier.clone();
    let cancel = std::thread::spawn(move || {
        cancel_barrier.wait();
        cancel_store.transition_run(
            &cancel_id,
            expected_revision,
            RunStatus::Cancelled,
            Some("cancelled by test"),
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:04Z")),
        )
    });
    let complete_store = store.clone();
    let complete_id = run.id.clone();
    let complete = std::thread::spawn(move || {
        barrier.wait();
        complete_store.transition_run(
            &complete_id,
            expected_revision,
            RunStatus::Succeeded,
            None,
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:04Z")),
        )
    });

    let results = [
        cancel.join().expect("cancel thread"),
        complete.join().expect("complete thread"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let stored = store
        .get_run(&run.id)
        .expect("read run")
        .expect("stored run");
    assert!(matches!(
        stored.status,
        RunStatus::Cancelled | RunStatus::Succeeded
    ));
    assert_eq!(stored.revision, run.revision + 1);
}

#[test]
fn postgres_revision_race_has_one_transition_winner() {
    let _database_guard = database_test_guard();
    let Some(store) = postgres_store() else {
        return;
    };
    let run = create_run_in_status(&store, RunStatus::Planning);
    let expected_revision = run.revision;
    let barrier = Arc::new(Barrier::new(2));
    let awaiting_store = store.clone();
    let awaiting_id = run.id.clone();
    let awaiting_barrier = barrier.clone();
    let awaiting = std::thread::spawn(move || {
        awaiting_barrier.wait();
        awaiting_store.transition_run(
            &awaiting_id,
            expected_revision,
            RunStatus::AwaitingApproval,
            None,
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:03Z")),
        )
    });
    let running_store = store.clone();
    let running_id = run.id.clone();
    let running = std::thread::spawn(move || {
        barrier.wait();
        running_store.transition_run(
            &running_id,
            expected_revision,
            RunStatus::Running,
            None,
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:03Z")),
        )
    });

    let results = [
        awaiting.join().expect("awaiting thread"),
        running.join().expect("running thread"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let stored = store
        .get_run(&run.id)
        .expect("read run")
        .expect("stored run");
    assert!(matches!(
        stored.status,
        RunStatus::AwaitingApproval | RunStatus::Running
    ));
    assert_eq!(stored.revision, run.revision + 1);
    store
        .transition_run(
            &stored.id,
            stored.revision,
            RunStatus::Cancelled,
            Some("test cleanup"),
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:04Z")),
        )
        .expect("finish revision-race run");
}

#[test]
fn postgres_recovery_is_selective_atomic_and_idempotent() {
    let _database_guard = database_test_guard();
    let Some(store) = postgres_store() else {
        return;
    };
    let queued = create_run_in_status(&store, RunStatus::Queued);
    let planning = create_run_in_status(&store, RunStatus::Planning);
    let running = create_run_in_status(&store, RunStatus::Running);
    let awaiting = create_run_in_status(&store, RunStatus::AwaitingApproval);
    let succeeded = create_run_in_status(&store, RunStatus::Succeeded);

    let report = store
        .recover_interrupted_runs(actor(), timestamp("2026-08-21T01:00:09Z"))
        .expect("recover");
    let recovered = report
        .recovered_run_ids
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        recovered,
        BTreeSet::from([planning.id.clone(), running.id.clone()])
    );

    for interrupted in [&planning, &running] {
        let stored = store
            .get_run(&interrupted.id)
            .expect("read recovered run")
            .expect("recovered run");
        assert_eq!(stored.status, RunStatus::Failed);
        assert_eq!(stored.revision, interrupted.revision + 1);
        assert_eq!(
            stored.terminal_reason.as_deref(),
            Some(CRASH_RECOVERY_REASON)
        );
        let events = store.audit_events(&interrupted.id).expect("audit chain");
        assert_eq!(
            events.last().expect("recovery event").event_type,
            AuditEventType::RunFailed
        );
    }

    assert_eq!(
        store
            .get_run(&queued.id)
            .expect("queued")
            .expect("run")
            .status,
        RunStatus::Queued
    );
    assert_eq!(
        store
            .get_run(&awaiting.id)
            .expect("awaiting")
            .expect("run")
            .status,
        RunStatus::AwaitingApproval
    );
    assert_eq!(
        store
            .get_run(&succeeded.id)
            .expect("succeeded")
            .expect("run")
            .status,
        RunStatus::Succeeded
    );
    assert!(
        store
            .recover_interrupted_runs(actor(), timestamp("2026-08-21T01:00:10Z"))
            .expect("idempotent recovery")
            .recovered_run_ids
            .is_empty()
    );
}

struct ApprovalFixture {
    store: PostgresExecutionStore,
    action: ActionRequest,
    approval: ApprovalRequest,
}

fn approved_approval_fixture() -> Option<ApprovalFixture> {
    let fixture = pending_approval_fixture()?;
    let approval = fixture
        .store
        .decide_approval(
            &fixture.approval.id,
            0,
            ApprovalDecision::Approved,
            actor(),
            None,
            timestamp("2026-08-21T01:00:04Z"),
        )
        .expect("approve fixture");
    Some(ApprovalFixture {
        approval,
        ..fixture
    })
}

fn pending_approval_fixture() -> Option<ApprovalFixture> {
    let store = postgres_store()?;
    let run = create_run_in_status(&store, RunStatus::AwaitingApproval);
    let action = ActionRequest::new(
        ActionId::generate(),
        run.id.clone(),
        "email.send",
        "sales-manager@example.invalid",
        json!({"report": "august"}),
    )
    .expect("action");
    let created_at = timestamp("2026-08-21T01:00:03Z");
    let approval = ApprovalRequest {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: ApprovalId::generate(),
        run_id: run.id,
        action_id: action.id.clone(),
        parameter_hash: action.parameter_hash().clone(),
        requested_by: actor(),
        request_reason: "external side effect".to_owned(),
        created_at,
        expires_at: timestamp("2026-08-21T01:10:00Z"),
        status: ApprovalStatus::Pending,
        revision: 0,
        decision: None,
        consumed_at: None,
    };
    let approval = store
        .create_approval(approval, actor(), created_at)
        .expect("create approval");
    Some(ApprovalFixture {
        store,
        action,
        approval,
    })
}

fn postgres_store() -> Option<PostgresExecutionStore> {
    let Ok(database_url) = env::var("OPENWORK_TEST_DATABASE_URL") else {
        eprintln!("skipping real Postgres test: OPENWORK_TEST_DATABASE_URL is not set");
        return None;
    };
    assert_eq!(
        env::var("OPENWORK_TEST_DATABASE_RESET").as_deref(),
        Ok("1"),
        "set OPENWORK_TEST_DATABASE_RESET=1 for the dedicated disposable test database"
    );
    assert!(
        (database_url.contains("@127.0.0.1:") || database_url.contains("@localhost:"))
            && database_url
                .split_once('?')
                .map_or(database_url.as_str(), |(base, _)| base)
                .ends_with("/openwork_test"),
        "Postgres concurrency tests require a loopback database named openwork_test"
    );
    let runtime = DATABASE_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test database runtime")
    });
    let pool = runtime
        .block_on(async {
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .connect(&database_url)
                .await
                .map_err(|error| error.to_string())?;
            MIGRATIONS
                .run(&pool)
                .await
                .map_err(|error| error.to_string())?;
            sqlx::query("TRUNCATE TABLE runs CASCADE")
                .execute(&pool)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(pool)
        })
        .expect("connect and migrate test Postgres");
    Some(PostgresExecutionStore::new(pool))
}

fn database_test_guard() -> MutexGuard<'static, ()> {
    DATABASE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn create_run_in_status(store: &PostgresExecutionStore, target: RunStatus) -> Run {
    let created_at = timestamp("2026-08-21T01:00:00Z");
    let run = Run {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: RunId::generate(),
        runtime: "mock".to_owned(),
        workspace: PathBuf::from("/tmp/openwork-postgres-test"),
        status: RunStatus::Queued,
        revision: 0,
        actor_id: actor(),
        prompt_sha256: sha256_bytes(b"postgres concurrency test"),
        created_at,
        updated_at: created_at,
        started_at: None,
        completed_at: None,
        terminal_reason: None,
    };
    let mut current = store
        .create_run(run, AuditAppend::new(actor(), created_at))
        .expect("create run");
    if target == RunStatus::Queued {
        return current;
    }
    current = store
        .transition_run(
            &current.id,
            current.revision,
            RunStatus::Planning,
            None,
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:01Z")),
        )
        .expect("planning");
    if target == RunStatus::Planning {
        return current;
    }
    let next = if target == RunStatus::AwaitingApproval {
        RunStatus::AwaitingApproval
    } else {
        RunStatus::Running
    };
    current = store
        .transition_run(
            &current.id,
            current.revision,
            next,
            None,
            AuditAppend::new(actor(), timestamp("2026-08-21T01:00:02Z")),
        )
        .expect("second transition");
    if target == RunStatus::Succeeded {
        current = store
            .transition_run(
                &current.id,
                current.revision,
                RunStatus::Succeeded,
                None,
                AuditAppend::new(actor(), timestamp("2026-08-21T01:00:03Z")),
            )
            .expect("succeeded");
    }
    current
}

fn actor() -> ActorId {
    ActorId::parse("service:postgres-test").expect("actor")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("timestamp")
}
