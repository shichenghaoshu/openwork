use openwork_core::OpenWorkError;
use openwork_execution::approval::ApprovalRepository;
use openwork_execution::audit::AuditAppend;
use openwork_execution::store::{ExecutionStore, InMemoryExecutionStore};
use openwork_execution::{
    ActionId, ActionRequest, ActorId, ApprovalDecision, ApprovalId, ApprovalRequest,
    ApprovalStatus, AuditEventType, EXECUTION_SCHEMA_VERSION, Run, RunId, RunStatus, UtcTimestamp,
    sha256_bytes,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

#[test]
fn approval_is_single_use_and_claims_the_exact_action() {
    let store = seeded_store();
    let action = action("finance@example.com");
    let approval = pending_approval(&action);
    assert!(
        store
            .create_approval(approval.clone(), actor("attacker"), time(0))
            .is_err()
    );
    assert!(store.get_approval(&approval.id).unwrap().is_none());
    assert_eq!(event_count(&store, AuditEventType::ApprovalRequested), 0);
    store
        .create_approval(approval.clone(), actor("requester"), time(0))
        .expect("create approval");
    assert_eq!(event_count(&store, AuditEventType::ApprovalRequested), 1);
    let rebound_action = action_with_id("02", "changed@example.com", "changed.csv");
    let mut rebound = pending_approval(&rebound_action);
    rebound.id = ApprovalId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a07").unwrap();
    let audit_before = store.audit_events(&run_id()).unwrap();
    assert!(
        store
            .create_approval(rebound.clone(), actor("requester"), time(0))
            .is_err()
    );
    assert_eq!(store.audit_events(&run_id()).unwrap(), audit_before);
    let approved = store
        .decide_approval(
            &approval.id,
            0,
            ApprovalDecision::Approved,
            actor("admin"),
            Some("token=must-not-persist"),
            time(1),
        )
        .expect("approve");
    assert_eq!(
        approved.decision.as_ref().expect("decision").actor,
        actor("admin")
    );
    assert!(
        !serde_json::to_string(&approved)
            .unwrap()
            .contains("must-not-persist")
    );

    let consumed = store
        .consume_approval(&approval.id, 1, &action, actor("executor"), time(2))
        .expect("consume once");
    assert_eq!(consumed.approval.status, ApprovalStatus::Consumed);
    assert_eq!(
        consumed.action_claim.parameter_hash,
        *action.parameter_hash()
    );
    assert!(
        store
            .consume_approval(&approval.id, 2, &action, actor("executor"), time(3))
            .is_err()
    );
    assert_eq!(
        store.get_action_claim(&action.id).unwrap(),
        Some(consumed.action_claim)
    );
    let audit = store.audit_events(&run_id()).unwrap();
    assert_eq!(
        audit[audit.len() - 2].event_type,
        AuditEventType::ApprovalConsumed
    );
    assert_eq!(
        audit.last().unwrap().event_type,
        AuditEventType::RuntimeStarted
    );
    let run = store.get_run(&run_id()).unwrap().unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.started_at, Some(time(2)));
    verify_audit(&store, 7);

    transition(&store, 3, RunStatus::AwaitingApproval, 3).unwrap();
    rebound.created_at = time(3);
    let audit_before = store.audit_events(&run_id()).unwrap();
    let claim_before = store.get_action_claim(&action.id).unwrap();
    assert!(
        store
            .create_approval(rebound, actor("requester"), time(3))
            .is_err()
    );
    assert_eq!(store.audit_events(&run_id()).unwrap(), audit_before);
    assert_eq!(store.get_action_claim(&action.id).unwrap(), claim_before);
    verify_audit(&store, 8);
}

#[test]
fn expiry_uses_now_greater_than_or_equal_to_deadline() {
    let action = action("internal@example.com");
    let approval = pending_approval(&action);
    let store = pending_store(&action);
    assert!(decide(&store, &approval.id, ApprovalDecision::Approved, 5).is_err());
    let expired = store
        .expire_approval(&approval.id, 0, actor("system"), time(5))
        .expect("expire at deadline");
    assert_eq!(expired.status, ApprovalStatus::Expired);
    assert_eq!(
        store
            .audit_events(&run_id())
            .unwrap()
            .last()
            .unwrap()
            .event_type,
        openwork_execution::AuditEventType::ApprovalExpired
    );
    assert!(
        store
            .consume_approval(&approval.id, 1, &action, actor("executor"), time(5))
            .is_err()
    );
    verify_audit(&store, 5);

    let approved_store = approved_store(&action);
    let expired_approved = approved_store
        .expire_approval(&approval.id, 1, actor("system"), time(5))
        .unwrap();
    assert_eq!(expired_approved.status, ApprovalStatus::Expired);
    verify_audit(&approved_store, 6);
}

#[test]
fn tampered_action_is_rejected_and_safely_audited() {
    let action = action("finance@example.com");
    let approval = pending_approval(&action);
    let store = approved_store(&action);
    let mut tampered = action.clone();
    tampered.resource = "attacker@example.net".to_owned();
    assert!(
        store
            .consume_approval(&approval.id, 1, &tampered, actor("executor"), time(2))
            .is_err()
    );
    assert!(store.get_action_claim(&action.id).unwrap().is_none());
    assert_eq!(
        store.get_approval(&approval.id).unwrap().unwrap().revision,
        1
    );
    let audit = store.audit_events(&run_id()).unwrap();
    assert_eq!(
        audit.last().unwrap().event_type,
        openwork_execution::AuditEventType::ApprovalBindingMismatch
    );
    assert!(
        !serde_json::to_string(&audit)
            .unwrap()
            .contains("attacker@example.net")
    );
    verify_audit(&store, 6);
}

#[test]
fn concurrent_consumers_have_one_cas_winner() {
    let action = action("finance@example.com");
    let approval = pending_approval(&action);
    let store = Arc::new(approved_store(&action));
    let barrier = Arc::new(Barrier::new(3));
    let attempts = ["executor-a", "executor-b"].map(|name| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let approval_id = approval.id.clone();
        let action = action.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.consume_approval(&approval_id, 1, &action, actor(name), time(2))
        })
    });
    barrier.wait();
    let results = attempts.map(|handle| handle.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(store.get_action_claim(&action.id).unwrap().is_some());
    verify_audit(&store, 7);
}

#[test]
fn concurrent_approve_and_deny_have_one_cas_winner() {
    let action = action("finance@example.com");
    let approval = pending_approval(&action);
    let store = Arc::new(pending_store(&action));
    let barrier = Arc::new(Barrier::new(3));
    let attempts = [ApprovalDecision::Approved, ApprovalDecision::Denied].map(|decision| {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let approval_id = approval.id.clone();
        std::thread::spawn(move || {
            barrier.wait();
            decide(&store, &approval_id, decision, 1)
        })
    });
    barrier.wait();
    let results = attempts.map(|handle| handle.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        store.get_approval(&approval.id).unwrap().unwrap().revision,
        1
    );
    verify_audit(&store, 5);
}

#[test]
fn terminal_run_fails_closed_for_create_approve_and_consume() {
    let action = action("finance@example.com");
    let approval = pending_approval(&action);
    let consume_store = approved_store(&action);
    transition(&consume_store, 2, RunStatus::Cancelled, 2).unwrap();
    let before = consume_store.audit_events(&run_id()).unwrap();
    assert!(
        consume_store
            .consume_approval(&approval.id, 1, &action, actor("executor"), time(3))
            .is_err()
    );
    let stored = consume_store.get_approval(&approval.id).unwrap().unwrap();
    assert_eq!(
        (stored.status, stored.revision),
        (ApprovalStatus::Approved, 1)
    );
    assert!(
        consume_store
            .get_action_claim(&action.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(consume_store.audit_events(&run_id()).unwrap(), before);

    let create_store = seeded_store();
    transition(&create_store, 2, RunStatus::Cancelled, 1).unwrap();
    let mut late = approval.clone();
    late.created_at = time(2);
    let before = create_store.audit_events(&run_id()).unwrap();
    assert!(
        create_store
            .create_approval(late.clone(), actor("requester"), time(2))
            .is_err()
    );
    assert!(create_store.get_approval(&late.id).unwrap().is_none());
    assert_eq!(create_store.audit_events(&run_id()).unwrap(), before);

    let approve_store = seeded_store();
    approve_store
        .create_approval(approval.clone(), actor("requester"), time(0))
        .unwrap();
    transition(&approve_store, 2, RunStatus::Cancelled, 1).unwrap();
    let before = approve_store.audit_events(&run_id()).unwrap();
    assert!(decide(&approve_store, &approval.id, ApprovalDecision::Approved, 2).is_err());
    assert_eq!(approve_store.audit_events(&run_id()).unwrap(), before);
    let denied = decide(&approve_store, &approval.id, ApprovalDecision::Denied, 2).unwrap();
    assert_eq!(denied.status, ApprovalStatus::Denied);
}

#[test]
fn cancel_and_consume_race_has_one_atomic_winner() {
    let action = action("finance@example.com");
    let store = Arc::new(approved_store(&action));
    let action_id = action.id.clone();
    let approval = pending_approval(&action);
    let approval_id = approval.id.clone();
    let barrier = Arc::new(Barrier::new(3));

    let cancel_store = Arc::clone(&store);
    let cancel_barrier = Arc::clone(&barrier);
    let cancel = std::thread::spawn(move || {
        cancel_barrier.wait();
        transition(&cancel_store, 2, RunStatus::Cancelled, 2)
    });
    let consume_store = Arc::clone(&store);
    let consume_barrier = Arc::clone(&barrier);
    let consume_approval_id = approval_id.clone();
    let consume = std::thread::spawn(move || {
        consume_barrier.wait();
        consume_store.consume_approval(&consume_approval_id, 1, &action, actor("executor"), time(2))
    });
    barrier.wait();
    let cancel_result = cancel.join().unwrap();
    let consume_result = consume.join().unwrap();
    assert_ne!(cancel_result.is_ok(), consume_result.is_ok());

    let stored = store.get_approval(&approval_id).unwrap().unwrap();
    let run = store.get_run(&run_id()).unwrap().unwrap();
    let audit = store.audit_events(&run_id()).unwrap();
    if consume_result.is_ok() {
        assert_eq!(stored.status, ApprovalStatus::Consumed);
        assert_eq!(stored.revision, 2);
        assert!(store.get_action_claim(&action_id).unwrap().is_some());
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.revision, 3);
        assert_eq!(run.started_at, Some(time(2)));
        assert_eq!(
            audit[audit.len() - 2].event_type,
            AuditEventType::ApprovalConsumed
        );
        assert_eq!(
            audit.last().unwrap().event_type,
            AuditEventType::RuntimeStarted
        );
        verify_audit(&store, 7);
    } else {
        assert_eq!(stored.status, ApprovalStatus::Approved);
        assert_eq!(stored.revision, 1);
        assert!(store.get_action_claim(&action_id).unwrap().is_none());
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(run.revision, 3);
        assert_eq!(audit.last().unwrap().event_type, AuditEventType::RunFailed);
        assert_eq!(event_count(&store, AuditEventType::ApprovalConsumed), 0);
        assert_eq!(event_count(&store, AuditEventType::RuntimeStarted), 0);
        verify_audit(&store, 6);
    }
}

#[test]
fn approvals_are_bound_to_one_awaiting_run_window() {
    let store = seeded_store();
    let old_action = action("finance@example.com");
    let old_approval = pending_approval(&old_action);
    let pending_action = action_with_id("04", "legal@example.com", "sales-analysis.csv");
    let mut pending = pending_approval(&pending_action);
    pending.id = ApprovalId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a05").unwrap();
    store
        .create_approval(old_approval.clone(), actor("requester"), time(0))
        .unwrap();
    store
        .create_approval(pending.clone(), actor("requester"), time(0))
        .unwrap();
    decide(&store, &old_approval.id, ApprovalDecision::Approved, 1).unwrap();
    transition(&store, 2, RunStatus::Running, 2).unwrap();
    transition(&store, 3, RunStatus::AwaitingApproval, 3).unwrap();
    let audit_before = store.audit_events(&run_id()).unwrap();

    assert!(
        store
            .consume_approval(&old_approval.id, 1, &old_action, actor("executor"), time(4))
            .is_err()
    );
    assert!(decide(&store, &pending.id, ApprovalDecision::Approved, 4).is_err());
    assert_eq!(store.audit_events(&run_id()).unwrap(), audit_before);
    assert_eq!(
        store
            .get_approval(&old_approval.id)
            .unwrap()
            .unwrap()
            .status,
        ApprovalStatus::Approved
    );
    assert_eq!(
        store.get_approval(&pending.id).unwrap().unwrap().status,
        ApprovalStatus::Pending
    );
    assert!(store.get_action_claim(&old_action.id).unwrap().is_none());

    store
        .expire_approval(&old_approval.id, 1, actor("system"), time(5))
        .unwrap();
    store
        .expire_approval(&pending.id, 0, actor("system"), time(5))
        .unwrap();
    let fresh_action = action_with_id("08", "fresh@example.com", "sales-analysis.csv");
    let mut fresh = pending_approval(&fresh_action);
    fresh.id = ApprovalId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a06").unwrap();
    fresh.created_at = time(6);
    fresh.expires_at = time(9);
    store
        .create_approval(fresh.clone(), actor("requester"), time(6))
        .unwrap();
    decide(&store, &fresh.id, ApprovalDecision::Approved, 7).unwrap();
    store
        .consume_approval(&fresh.id, 1, &fresh_action, actor("executor"), time(8))
        .unwrap();

    let run = store.get_run(&run_id()).unwrap().unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.revision, 5);
    assert_eq!(run.started_at, Some(time(2)));
    assert!(store.get_action_claim(&fresh_action.id).unwrap().is_some());
    verify_audit(&store, 14);
}

fn seeded_store() -> InMemoryExecutionStore {
    let store = InMemoryExecutionStore::default();
    store
        .create_run(
            Run {
                schema_version: EXECUTION_SCHEMA_VERSION,
                id: run_id(),
                runtime: "mock".to_owned(),
                workspace: PathBuf::from("/workspace"),
                status: RunStatus::Queued,
                revision: 0,
                actor_id: actor("requester"),
                prompt_sha256: sha256_bytes(b"prompt"),
                created_at: time(0),
                updated_at: time(0),
                started_at: None,
                completed_at: None,
                terminal_reason: None,
            },
            AuditAppend::new(actor("requester"), time(0)),
        )
        .unwrap();
    transition(&store, 0, RunStatus::Planning, 0).unwrap();
    transition(&store, 1, RunStatus::AwaitingApproval, 0).unwrap();
    let transition = store.audit_events(&run_id()).unwrap().pop().unwrap();
    assert_eq!(
        transition.event_type,
        openwork_execution::AuditEventType::ActionRequested
    );
    assert_eq!(
        transition.metadata.as_map()["run_status"],
        json!("awaiting_approval")
    );
    store
}

fn pending_store(action: &ActionRequest) -> InMemoryExecutionStore {
    let store = seeded_store();
    let approval = pending_approval(action);
    store
        .create_approval(approval.clone(), actor("requester"), time(0))
        .unwrap();
    store
}

fn approved_store(action: &ActionRequest) -> InMemoryExecutionStore {
    let store = pending_store(action);
    let approval = pending_approval(action);
    decide(&store, &approval.id, ApprovalDecision::Approved, 1).unwrap();
    store
}

fn action(resource: &str) -> ActionRequest {
    action_with_id("02", resource, "sales-analysis.csv")
}

fn action_with_id(suffix: &str, resource: &str, attachment: &str) -> ActionRequest {
    ActionRequest::new(
        ActionId::parse(&format!("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a{suffix}")).unwrap(),
        run_id(),
        "email.send",
        resource,
        json!({"attachment": attachment}),
    )
    .unwrap()
}

fn pending_approval(action: &ActionRequest) -> ApprovalRequest {
    ApprovalRequest {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: ApprovalId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a03").unwrap(),
        run_id: action.run_id.clone(),
        action_id: action.id.clone(),
        parameter_hash: action.parameter_hash().clone(),
        requested_by: actor("requester"),
        request_reason: "external email requires review".to_owned(),
        created_at: time(0),
        expires_at: time(5),
        status: ApprovalStatus::Pending,
        revision: 0,
        decision: None,
        consumed_at: None,
    }
}

fn transition(
    store: &InMemoryExecutionStore,
    revision: u64,
    status: RunStatus,
    minute: u8,
) -> Result<Run, OpenWorkError> {
    store.transition_run(
        &run_id(),
        revision,
        status,
        None,
        AuditAppend::new(actor("executor"), time(minute)),
    )
}

fn event_count(store: &InMemoryExecutionStore, event_type: AuditEventType) -> usize {
    store
        .audit_events(&run_id())
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == event_type)
        .count()
}

fn decide(
    store: &InMemoryExecutionStore,
    approval_id: &ApprovalId,
    decision: ApprovalDecision,
    minute: u8,
) -> Result<ApprovalRequest, OpenWorkError> {
    store.decide_approval(approval_id, 0, decision, actor("admin"), None, time(minute))
}

fn verify_audit(store: &InMemoryExecutionStore, expected: usize) {
    let events = store.audit_events(&run_id()).unwrap();
    assert_eq!(events.len(), expected);
    for (index, event) in events.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .map(|offset| events[offset].event_hash());
        event
            .verify_integrity((index + 1) as u64, previous)
            .unwrap();
    }
}

fn run_id() -> RunId {
    RunId::parse("01890f3e-a5f1-7cc2-98c0-5f9c6f5e7a01").unwrap()
}

fn actor(name: &str) -> ActorId {
    ActorId::parse(format!("user:{name}")).unwrap()
}

fn time(minute: u8) -> UtcTimestamp {
    UtcTimestamp::parse(format!("2026-08-10T00:{minute:02}:00Z")).unwrap()
}
