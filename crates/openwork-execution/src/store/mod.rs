//! Transactional execution persistence boundary and deterministic memory store.

use crate::action_executor::ActionExecutionReceipt;
use crate::approval::{ActionClaim, ApprovalConsumption, ApprovalRepository};
use crate::audit::AuditAppend;
use crate::{
    ActionId, ActionRequest, ActorId, ApprovalDecision, ApprovalDecisionRecord, ApprovalId,
    ApprovalRequest, ApprovalStatus, Artifact, AuditEvent, AuditEventId, AuditEventType,
    RedactedAuditMetadata, Run, RunId, RunStatus, SandboxCleanupStatus, SandboxResult,
    SandboxTermination, UtcTimestamp,
};
use openwork_core::{ErrorCode, OpenWorkError, redact_text};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Mutex;
use time::Duration;
use uuid::Uuid;

/// Opaque, unguessable capability proving ownership of one claimed run.
#[derive(Clone, Eq, PartialEq)]
pub struct LeaseToken(pub(crate) Uuid);

impl std::fmt::Debug for LeaseToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LeaseToken(<redacted>)")
    }
}

impl LeaseToken {
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}

/// A durable worker claim. The lease token must be presented for every active
/// operation, preventing a stale worker from completing another worker's run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunLease {
    pub run: Run,
    pub token: LeaseToken,
    pub owner: ActorId,
    pub acquired_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub cancel_requested: bool,
}

/// Outcome of a durable cancellation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CancelRequest {
    Cancelled,
    Requested,
    AlreadyTerminal(RunStatus),
}

/// The only accepted proof before a worker may confirm cancellation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancellationEvidence {
    run_id: RunId,
    sandbox_id: String,
    lease_token: LeaseToken,
    lease_owner: ActorId,
}

impl CancellationEvidence {
    /// Builds non-forgeable cancellation evidence from a validated sandbox result.
    ///
    /// # Errors
    ///
    /// Returns an error when the result is invalid or is not a completed cancellation.
    pub fn verify(lease: &RunLease, result: &SandboxResult) -> Result<Self, OpenWorkError> {
        result.validate()?;
        if result.run_id != lease.run.id
            || result.termination != SandboxTermination::Cancelled
            || result.exit_code.is_some()
            || !matches!(result.cleanup, SandboxCleanupStatus::Succeeded)
        {
            return Err(state_error(
                "sandbox result is not cancellation evidence for this lease",
            ));
        }
        Ok(Self {
            run_id: result.run_id.clone(),
            sandbox_id: result.sandbox_id.clone(),
            lease_token: lease.token.clone(),
            lease_owner: lease.owner.clone(),
        })
    }
}

/// Durable queue and cancellation boundary. Implementations must claim queued
/// work exactly once, enforce the lease token, and fail expired leases closed.
pub trait RunQueueRepository: Send + Sync {
    /// Claims the oldest queued run and atomically creates its worker lease.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid lease duration, a backwards timestamp,
    /// a state conflict, or a storage failure.
    fn claim_next_run(
        &self,
        owner: ActorId,
        now: UtcTimestamp,
        ttl: Duration,
    ) -> Result<Option<RunLease>, OpenWorkError>;
    /// Extends a current, unexpired lease and observes cancellation intent.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease capability is stale, expired, invalid,
    /// or cannot be persisted.
    fn heartbeat_lease(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        ttl: Duration,
    ) -> Result<RunLease, OpenWorkError>;
    /// Advances a leased run to a non-terminal state. The caller must present
    /// the current capability and the exact revision it previously observed.
    ///
    /// # Errors
    ///
    /// Returns an error for a terminal target, stale lease or revision,
    /// expired lease, backwards time, or illegal state transition.
    fn transition_leased_run(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        now: UtcTimestamp,
    ) -> Result<RunLease, OpenWorkError>;
    /// Completes a leased run. `Cancelled` is deliberately excluded: only
    /// [`Self::confirm_cancel`] may commit that terminal state.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale lease or revision, expired lease,
    /// cancellation intent combined with success, cancellation as a normal
    /// completion, or an illegal terminal transition.
    fn complete_leased_run(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        now: UtcTimestamp,
    ) -> Result<Run, OpenWorkError>;
    /// Cancels unowned waiting work or records intent for a leased worker.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is absent, its state cannot be cancelled,
    /// time moves backwards for a new audit event, or persistence fails.
    fn request_cancel(
        &self,
        run_id: &RunId,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<CancelRequest, OpenWorkError>;
    /// Reads cancellation intent after validating the current lease and time.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease capability is stale or expired, or the
    /// store cannot be read.
    fn lease_cancel_requested(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
    ) -> Result<bool, OpenWorkError>;
    /// Confirms terminal cancellation from exact lease-bound sandbox evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when the evidence, lease, run state, timestamp, or
    /// persistence transaction is invalid.
    fn confirm_cancel(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        evidence: CancellationEvidence,
    ) -> Result<Run, OpenWorkError>;
    /// Fails every expired lease without requeueing possibly executed work.
    ///
    /// # Errors
    ///
    /// Returns an error when recovery would violate state, time, or audit
    /// invariants, or when the storage transaction fails.
    fn recover_expired_leases(
        &self,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<Vec<RunId>, OpenWorkError>;
}

#[cfg(feature = "postgres")]
pub mod postgres;

/// Storage transaction boundary implemented by memory storage now and Postgres later.
pub trait ExecutionStore: Send + Sync {
    /// Creates a queued run and its genesis audit event atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid state, duplicate identity, or storage failure.
    fn create_run(&self, run: Run, audit: AuditAppend) -> Result<Run, OpenWorkError>;
    /// Applies a revision-checked transition and its audit event atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale revision, illegal transition, or storage failure.
    fn transition_run(
        &self,
        run_id: &RunId,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        audit: AuditAppend,
    ) -> Result<Run, OpenWorkError>;
    /// Appends one centrally redacted event at the next per-run sequence.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is absent or persistence fails.
    fn append_audit(
        &self,
        run_id: &RunId,
        event_type: AuditEventType,
        audit: AuditAppend,
    ) -> Result<AuditEvent, OpenWorkError>;
    /// Atomically appends one exact action-executed receipt if absent.
    ///
    /// Returns `true` when this call appended the event and `false` when the
    /// exact receipt was already audited. Implementations must serialize the
    /// check and append so concurrent reconciliation cannot create duplicates.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing/mismatched durable action claim,
    /// conflicting receipt, invalid audit chain, or storage failure.
    fn reconcile_action_execution(
        &self,
        receipt: &ActionExecutionReceipt,
        audit: AuditAppend,
    ) -> Result<bool, OpenWorkError>;
    /// Persists a complete artifact batch or none of it.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing run, duplicate path, mismatch, or storage failure.
    fn record_artifacts(
        &self,
        run_id: &RunId,
        artifacts: Vec<Artifact>,
        audit: AuditAppend,
    ) -> Result<(), OpenWorkError>;
    /// Reads one run.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be read.
    fn get_run(&self, run_id: &RunId) -> Result<Option<Run>, OpenWorkError>;
    /// Reads a run's ordered audit chain.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be read.
    fn audit_events(&self, run_id: &RunId) -> Result<Vec<AuditEvent>, OpenWorkError>;
    /// Reads a run's artifacts.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be read.
    fn artifacts(&self, run_id: &RunId) -> Result<Vec<Artifact>, OpenWorkError>;
}

/// Deterministic single-process store used by local mode and tests.
#[derive(Default)]
pub struct InMemoryExecutionStore {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    runs: BTreeMap<RunId, Run>,
    audits: BTreeMap<RunId, Vec<AuditEvent>>,
    artifacts: BTreeMap<RunId, Vec<Artifact>>,
    approvals: BTreeMap<ApprovalId, ApprovalRequest>,
    approval_windows: BTreeMap<ApprovalId, u64>,
    action_claims: BTreeMap<ActionId, ActionClaim>,
    leases: BTreeMap<RunId, RunLease>,
    cancel_requested: BTreeMap<RunId, UtcTimestamp>,
}

impl ExecutionStore for InMemoryExecutionStore {
    fn create_run(&self, run: Run, audit: AuditAppend) -> Result<Run, OpenWorkError> {
        validate_new_run(&run)?;
        if audit.timestamp != run.created_at {
            return Err(state_error(
                "genesis audit timestamp must match run creation",
            ));
        }
        let mut state = self.lock()?;
        if state.runs.contains_key(&run.id) {
            return Err(state_error("run already exists"));
        }
        let event = audit.build(run.id.clone(), 1, AuditEventType::RunCreated, None)?;
        state.audits.insert(run.id.clone(), vec![event]);
        state.runs.insert(run.id.clone(), run.clone());
        Ok(run)
    }

    fn transition_run(
        &self,
        run_id: &RunId,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        audit: AuditAppend,
    ) -> Result<Run, OpenWorkError> {
        let mut state = self.lock()?;
        if state.leases.contains_key(run_id) {
            return Err(state_error(
                "leased runs must use a lease-capability-bound transition",
            ));
        }
        let current = state.runs.get(run_id).ok_or_else(run_missing)?.clone();
        if current.revision != expected_revision || !current.status.can_transition_to(next) {
            return Err(state_error(
                "run revision is stale or transition is illegal",
            ));
        }
        if audit.timestamp < current.updated_at {
            return Err(state_error("run timestamps cannot move backwards"));
        }
        let events = state.audits.get(run_id).ok_or_else(audit_missing)?;
        if events
            .last()
            .is_some_and(|event| audit.timestamp < event.timestamp)
        {
            return Err(state_error("audit timestamps cannot move backwards"));
        }
        let sequence = u64::try_from(events.len())
            .map_err(|_| internal_error("audit sequence overflow"))?
            .checked_add(1)
            .ok_or_else(|| internal_error("audit sequence overflow"))?;
        let previous = events.last().map(|event| event.event_hash().clone());
        let audit_timestamp = audit.timestamp;
        let event = audit.with_run_status(next).build(
            run_id.clone(),
            sequence,
            transition_event(next),
            previous,
        )?;

        let mut updated = current;
        updated.status = next;
        updated.revision = updated
            .revision
            .checked_add(1)
            .ok_or_else(|| state_error("run revision overflow"))?;
        updated.updated_at = audit_timestamp;
        if next == RunStatus::Running && updated.started_at.is_none() {
            updated.started_at = Some(audit_timestamp);
        }
        if next.is_terminal() {
            updated.completed_at = Some(audit_timestamp);
            updated.terminal_reason = (next != RunStatus::Succeeded)
                .then(|| reason.map_or_else(|| "unspecified".to_owned(), redact_text));
        }
        state.runs.insert(run_id.clone(), updated.clone());
        state
            .audits
            .get_mut(run_id)
            .ok_or_else(audit_missing)?
            .push(event);
        Ok(updated)
    }

    fn append_audit(
        &self,
        run_id: &RunId,
        event_type: AuditEventType,
        audit: AuditAppend,
    ) -> Result<AuditEvent, OpenWorkError> {
        let mut state = self.lock()?;
        if !state.runs.contains_key(run_id) {
            return Err(run_missing());
        }
        let events = state.audits.get(run_id).ok_or_else(audit_missing)?;
        if events
            .last()
            .is_some_and(|event| audit.timestamp < event.timestamp)
        {
            return Err(state_error("audit timestamps cannot move backwards"));
        }
        let sequence = u64::try_from(events.len())
            .map_err(|_| internal_error("audit sequence overflow"))?
            .checked_add(1)
            .ok_or_else(|| internal_error("audit sequence overflow"))?;
        let previous = events.last().map(|event| event.event_hash().clone());
        let event = audit.build(run_id.clone(), sequence, event_type, previous)?;
        state
            .audits
            .get_mut(run_id)
            .ok_or_else(audit_missing)?
            .push(event.clone());
        Ok(event)
    }

    fn reconcile_action_execution(
        &self,
        receipt: &ActionExecutionReceipt,
        audit: AuditAppend,
    ) -> Result<bool, OpenWorkError> {
        let mut state = self.lock()?;
        verify_execution_claim(&state, receipt)?;
        let events = state
            .audits
            .get(&receipt.run_id)
            .ok_or_else(audit_missing)?;
        if execution_receipt_was_audited(events, receipt)? {
            return Ok(false);
        }
        if events
            .last()
            .is_some_and(|event| audit.timestamp < event.timestamp)
        {
            return Err(state_error(
                "action execution audit timestamp moved backwards",
            ));
        }
        let sequence = u64::try_from(events.len())
            .map_err(|_| internal_error("audit sequence overflow"))?
            .checked_add(1)
            .ok_or_else(|| internal_error("audit sequence overflow"))?;
        let previous = events.last().map(|event| event.event_hash().clone());
        let event = audit
            .with_action_execution(receipt.action_id.clone(), receipt.parameter_hash.clone())
            .build(
                receipt.run_id.clone(),
                sequence,
                AuditEventType::ActionExecuted,
                previous,
            )?;
        state
            .audits
            .get_mut(&receipt.run_id)
            .ok_or_else(audit_missing)?
            .push(event);
        Ok(true)
    }

    fn record_artifacts(
        &self,
        run_id: &RunId,
        artifacts: Vec<Artifact>,
        audit: AuditAppend,
    ) -> Result<(), OpenWorkError> {
        let mut state = self.lock()?;
        if !state.runs.contains_key(run_id)
            || artifacts.iter().any(|artifact| &artifact.run_id != run_id)
        {
            return Err(OpenWorkError::new(
                ErrorCode::ArtifactInvalid,
                "artifact run mismatch",
            ));
        }
        if artifacts
            .iter()
            .any(|artifact| artifact.media_type.trim().is_empty())
        {
            return Err(OpenWorkError::new(
                ErrorCode::ArtifactInvalid,
                "artifact media type is empty",
            ));
        }
        let existing = state.artifacts.get(run_id).cloned().unwrap_or_default();
        if artifacts.iter().any(|candidate| {
            existing.iter().any(|stored| stored.path == candidate.path)
                || artifacts
                    .iter()
                    .filter(|item| item.path == candidate.path)
                    .count()
                    > 1
        }) {
            return Err(OpenWorkError::new(
                ErrorCode::ArtifactInvalid,
                "duplicate artifact path",
            ));
        }
        let audit_chain = state.audits.get(run_id).ok_or_else(audit_missing)?;
        if audit_chain
            .last()
            .is_some_and(|event| audit.timestamp < event.timestamp)
        {
            return Err(state_error("audit timestamps cannot move backwards"));
        }
        let mut previous = audit_chain.last().map(|event| event.event_hash().clone());
        let mut next_sequence = u64::try_from(audit_chain.len())
            .map_err(|_| internal_error("audit sequence overflow"))?
            .checked_add(1)
            .ok_or_else(|| internal_error("audit sequence overflow"))?;
        let mut events = Vec::with_capacity(artifacts.len());
        for artifact in &artifacts {
            let append = audit.clone().with_artifact(artifact);
            let event = append.build(
                run_id.clone(),
                next_sequence,
                AuditEventType::ArtifactCreated,
                previous,
            )?;
            previous = Some(event.event_hash().clone());
            next_sequence = next_sequence
                .checked_add(1)
                .ok_or_else(|| internal_error("audit sequence overflow"))?;
            events.push(event);
        }
        state
            .artifacts
            .entry(run_id.clone())
            .or_default()
            .extend(artifacts);
        state
            .audits
            .get_mut(run_id)
            .ok_or_else(audit_missing)?
            .extend(events);
        Ok(())
    }

    fn get_run(&self, run_id: &RunId) -> Result<Option<Run>, OpenWorkError> {
        Ok(self.lock()?.runs.get(run_id).cloned())
    }

    fn audit_events(&self, run_id: &RunId) -> Result<Vec<AuditEvent>, OpenWorkError> {
        Ok(self.lock()?.audits.get(run_id).cloned().unwrap_or_default())
    }

    fn artifacts(&self, run_id: &RunId) -> Result<Vec<Artifact>, OpenWorkError> {
        Ok(self
            .lock()?
            .artifacts
            .get(run_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn verify_execution_claim(
    state: &State,
    receipt: &ActionExecutionReceipt,
) -> Result<(), OpenWorkError> {
    let claim = state
        .action_claims
        .get(&receipt.action_id)
        .ok_or_else(|| approval_error("action execution has no durable claim"))?;
    if claim.run_id != receipt.run_id || claim.parameter_hash != receipt.parameter_hash {
        return Err(approval_error(
            "action execution receipt does not match its durable claim",
        ));
    }
    Ok(())
}

pub(crate) fn execution_receipt_was_audited(
    events: &[AuditEvent],
    receipt: &ActionExecutionReceipt,
) -> Result<bool, OpenWorkError> {
    let expected_action_id = receipt.action_id.to_hyphenated();
    let expected_hash = receipt.parameter_hash.as_str();
    let mut exact_matches = 0_u8;
    for event in events {
        if event.event_type != AuditEventType::ActionExecuted {
            continue;
        }
        let metadata = event.metadata.as_map();
        let action_id = metadata
            .get("action_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| approval_error("action execution audit metadata is invalid"))?;
        let parameter_hash = metadata
            .get("parameter_hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| approval_error("action execution audit metadata is invalid"))?;
        if action_id == expected_action_id {
            if event.run_id != receipt.run_id || parameter_hash != expected_hash {
                return Err(approval_error("action execution audit binding is invalid"));
            }
            exact_matches = exact_matches
                .checked_add(1)
                .ok_or_else(|| approval_error("duplicate action execution audit"))?;
        }
    }
    match exact_matches {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(approval_error("duplicate action execution audit")),
    }
}

impl InMemoryExecutionStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, OpenWorkError> {
        self.state
            .lock()
            .map_err(|_| internal_error("execution store lock poisoned"))
    }
}

impl RunQueueRepository for InMemoryExecutionStore {
    fn claim_next_run(
        &self,
        owner: ActorId,
        now: UtcTimestamp,
        ttl: Duration,
    ) -> Result<Option<RunLease>, OpenWorkError> {
        let expires_at = checked_lease_expiry(now, ttl)?;
        let mut state = self.lock()?;
        let Some(run_id) = state
            .runs
            .iter()
            .filter(|(id, run)| run.status == RunStatus::Queued && !state.leases.contains_key(*id))
            .min_by(|(left_id, left), (right_id, right)| {
                (left.created_at, *left_id).cmp(&(right.created_at, *right_id))
            })
            .map(|(id, _)| id.clone())
        else {
            return Ok(None);
        };
        let current = state.runs.get(&run_id).ok_or_else(run_missing)?.clone();
        if now < current.updated_at {
            return Err(state_error("claim timestamp cannot move backwards"));
        }
        let updated = apply_memory_transition(
            &mut state,
            &current,
            RunStatus::Planning,
            now,
            owner.clone(),
            AuditEventType::RuntimeSelected,
        )?;
        let lease = RunLease {
            run: updated,
            token: LeaseToken::generate(),
            owner,
            acquired_at: now,
            expires_at,
            cancel_requested: false,
        };
        state.leases.insert(run_id, lease.clone());
        Ok(Some(lease))
    }

    fn heartbeat_lease(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        ttl: Duration,
    ) -> Result<RunLease, OpenWorkError> {
        let expires_at = checked_lease_expiry(now, ttl)?;
        let mut state = self.lock()?;
        let cancel_requested = state.cancel_requested.contains_key(&lease.run.id);
        let current = state
            .runs
            .get(&lease.run.id)
            .ok_or_else(run_missing)?
            .clone();
        let stored = state
            .leases
            .get_mut(&lease.run.id)
            .ok_or_else(|| state_error("lease does not exist"))?;
        if stored.token != lease.token
            || stored.owner != lease.owner
            || stored.run.revision != lease.run.revision
            || current.revision != lease.run.revision
            || now < stored.acquired_at
            || now >= stored.expires_at
            || now < current.updated_at
        {
            return Err(state_error("lease is not current"));
        }
        stored.expires_at = expires_at;
        stored.cancel_requested = cancel_requested;
        Ok(stored.clone())
    }

    fn transition_leased_run(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        now: UtcTimestamp,
    ) -> Result<RunLease, OpenWorkError> {
        if next.is_terminal() {
            return Err(state_error("leased transition target must be non-terminal"));
        }
        let mut state = self.lock()?;
        let current = validate_memory_lease(&state, lease, expected_revision, now)?;
        let updated = apply_memory_transition(
            &mut state,
            &current,
            next,
            now,
            lease.owner.clone(),
            transition_event(next),
        )?;
        let mut refreshed = state
            .leases
            .get(&lease.run.id)
            .ok_or_else(|| state_error("lease does not exist"))?
            .clone();
        refreshed.run = updated;
        refreshed.cancel_requested = state.cancel_requested.contains_key(&lease.run.id);
        state.leases.insert(lease.run.id.clone(), refreshed.clone());
        Ok(refreshed)
    }

    fn complete_leased_run(
        &self,
        lease: &RunLease,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        now: UtcTimestamp,
    ) -> Result<Run, OpenWorkError> {
        if !matches!(
            next,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::TimedOut
        ) {
            return Err(state_error(
                "leased completion must be succeeded, failed, or timed_out",
            ));
        }
        let mut state = self.lock()?;
        let current = validate_memory_lease(&state, lease, expected_revision, now)?;
        if next == RunStatus::Succeeded && state.cancel_requested.contains_key(&lease.run.id) {
            return Err(state_error(
                "cannot complete a cancellation-requested run successfully",
            ));
        }
        let updated = apply_memory_transition_with_reason(
            &mut state,
            &current,
            next,
            reason,
            now,
            lease.owner.clone(),
            transition_event(next),
        )?;
        state.leases.remove(&lease.run.id);
        state.cancel_requested.remove(&lease.run.id);
        Ok(updated)
    }

    fn request_cancel(
        &self,
        run_id: &RunId,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<CancelRequest, OpenWorkError> {
        let mut state = self.lock()?;
        let current = state.runs.get(run_id).ok_or_else(run_missing)?.clone();
        if current.status.is_terminal() {
            return Ok(CancelRequest::AlreadyTerminal(current.status));
        }
        if matches!(
            current.status,
            RunStatus::Queued | RunStatus::AwaitingApproval
        ) && !state.leases.contains_key(run_id)
        {
            ensure_memory_audit_time(&state, run_id, now, current.updated_at)?;
            let _ = apply_memory_transition(
                &mut state,
                &current,
                RunStatus::Cancelled,
                now,
                actor,
                AuditEventType::CancelConfirmed,
            )?;
            return Ok(CancelRequest::Cancelled);
        }
        if !matches!(
            current.status,
            RunStatus::Planning | RunStatus::AwaitingApproval | RunStatus::Running
        ) {
            return Err(state_error("only active runs may request cancellation"));
        }
        if !state.leases.contains_key(run_id) {
            return Err(state_error(
                "active run has no current worker lease for cancellation",
            ));
        }
        if state.cancel_requested.contains_key(run_id) {
            return Ok(CancelRequest::Requested);
        }
        ensure_memory_audit_time(&state, run_id, now, current.updated_at)?;
        append_memory_audit(
            &mut state,
            run_id,
            actor,
            now,
            AuditEventType::CancelRequested,
        )?;
        state.cancel_requested.insert(run_id.clone(), now);
        if let Some(lease) = state.leases.get_mut(run_id) {
            lease.cancel_requested = true;
        }
        Ok(CancelRequest::Requested)
    }

    fn lease_cancel_requested(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
    ) -> Result<bool, OpenWorkError> {
        let state = self.lock()?;
        let stored = state
            .leases
            .get(&lease.run.id)
            .ok_or_else(|| state_error("lease does not exist"))?;
        if stored.token != lease.token
            || stored.owner != lease.owner
            || stored.run.revision != lease.run.revision
            || now < stored.acquired_at
            || now >= stored.expires_at
        {
            return Err(state_error("lease is not current"));
        }
        Ok(state.cancel_requested.contains_key(&lease.run.id))
    }

    fn confirm_cancel(
        &self,
        lease: &RunLease,
        now: UtcTimestamp,
        evidence: CancellationEvidence,
    ) -> Result<Run, OpenWorkError> {
        if evidence.run_id != lease.run.id
            || evidence.sandbox_id.is_empty()
            || evidence.lease_token != lease.token
            || evidence.lease_owner != lease.owner
        {
            return Err(state_error("cancellation evidence does not match lease"));
        }
        let mut state = self.lock()?;
        let stored = state
            .leases
            .get(&lease.run.id)
            .ok_or_else(|| state_error("lease does not exist"))?;
        if stored.token != lease.token
            || stored.owner != lease.owner
            || stored.run.revision != lease.run.revision
            || now >= stored.expires_at
            || !state.cancel_requested.contains_key(&lease.run.id)
        {
            return Err(state_error("lease is not eligible to confirm cancellation"));
        }
        let current = state
            .runs
            .get(&lease.run.id)
            .ok_or_else(run_missing)?
            .clone();
        if current.revision != lease.run.revision || now < stored.acquired_at {
            return Err(state_error(
                "lease capability, revision, or time is not current",
            ));
        }
        ensure_memory_audit_time(&state, &lease.run.id, now, current.updated_at)?;
        let updated = apply_memory_transition(
            &mut state,
            &current,
            RunStatus::Cancelled,
            now,
            lease.owner.clone(),
            AuditEventType::CancelConfirmed,
        )?;
        state.leases.remove(&lease.run.id);
        state.cancel_requested.remove(&lease.run.id);
        Ok(updated)
    }

    fn recover_expired_leases(
        &self,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<Vec<RunId>, OpenWorkError> {
        let mut state = self.lock()?;
        let expired: Vec<_> = state
            .leases
            .iter()
            .filter(|(_, lease)| lease.expires_at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            let current = state.runs.get(id).ok_or_else(run_missing)?.clone();
            ensure_memory_audit_time(&state, id, now, current.updated_at)?;
            if !current.status.is_terminal() {
                let _ = apply_memory_transition(
                    &mut state,
                    &current,
                    RunStatus::Failed,
                    now,
                    actor.clone(),
                    AuditEventType::RunFailed,
                )?;
            }
            state.leases.remove(id);
            state.cancel_requested.remove(id);
        }
        Ok(expired)
    }
}

fn checked_lease_expiry(now: UtcTimestamp, ttl: Duration) -> Result<UtcTimestamp, OpenWorkError> {
    if ttl <= Duration::ZERO || ttl > Duration::hours(1) {
        return Err(state_error(
            "lease ttl must be positive and no more than one hour",
        ));
    }
    now.0
        .checked_add(ttl)
        .map(UtcTimestamp)
        .ok_or_else(|| state_error("lease expiry overflows timestamp"))
}

fn ensure_memory_audit_time(
    state: &State,
    run_id: &RunId,
    timestamp: UtcTimestamp,
    updated_at: UtcTimestamp,
) -> Result<(), OpenWorkError> {
    if timestamp < updated_at
        || state
            .audits
            .get(run_id)
            .and_then(|events| events.last())
            .is_some_and(|event| timestamp < event.timestamp)
    {
        return Err(state_error("audit timestamps cannot move backwards"));
    }
    Ok(())
}

fn validate_memory_lease(
    state: &State,
    lease: &RunLease,
    expected_revision: u64,
    now: UtcTimestamp,
) -> Result<Run, OpenWorkError> {
    let stored = state
        .leases
        .get(&lease.run.id)
        .ok_or_else(|| state_error("lease does not exist"))?;
    let current = state.runs.get(&lease.run.id).ok_or_else(run_missing)?;
    if stored.token != lease.token
        || stored.owner != lease.owner
        || lease.run.revision != expected_revision
        || current.revision != expected_revision
        || now < stored.acquired_at
        || now >= stored.expires_at
        || now < current.updated_at
    {
        return Err(state_error(
            "lease capability, revision, or time is not current",
        ));
    }
    ensure_memory_audit_time(state, &lease.run.id, now, current.updated_at)?;
    Ok(current.clone())
}

fn append_memory_audit(
    state: &mut State,
    run_id: &RunId,
    actor: ActorId,
    timestamp: UtcTimestamp,
    event_type: AuditEventType,
) -> Result<(), OpenWorkError> {
    let events = state.audits.get(run_id).ok_or_else(audit_missing)?;
    if events
        .last()
        .is_some_and(|event| timestamp < event.timestamp)
    {
        return Err(state_error("audit timestamps cannot move backwards"));
    }
    let sequence = u64::try_from(events.len())
        .map_err(|_| internal_error("audit sequence overflow"))?
        .checked_add(1)
        .ok_or_else(|| internal_error("audit sequence overflow"))?;
    let previous = events.last().map(|event| event.event_hash().clone());
    let event =
        AuditAppend::new(actor, timestamp).build(run_id.clone(), sequence, event_type, previous)?;
    state
        .audits
        .get_mut(run_id)
        .ok_or_else(audit_missing)?
        .push(event);
    Ok(())
}

fn apply_memory_transition(
    state: &mut State,
    current: &Run,
    next: RunStatus,
    timestamp: UtcTimestamp,
    actor: ActorId,
    event_type: AuditEventType,
) -> Result<Run, OpenWorkError> {
    apply_memory_transition_with_reason(state, current, next, None, timestamp, actor, event_type)
}

fn apply_memory_transition_with_reason(
    state: &mut State,
    current: &Run,
    next: RunStatus,
    reason: Option<&str>,
    timestamp: UtcTimestamp,
    actor: ActorId,
    event_type: AuditEventType,
) -> Result<Run, OpenWorkError> {
    if !current.status.can_transition_to(next) || timestamp < current.updated_at {
        return Err(state_error(
            "run transition is illegal or timestamps move backwards",
        ));
    }
    let mut updated = current.clone();
    updated.status = next;
    updated.revision = updated
        .revision
        .checked_add(1)
        .ok_or_else(|| state_error("run revision overflow"))?;
    updated.updated_at = timestamp;
    if next == RunStatus::Running && updated.started_at.is_none() {
        updated.started_at = Some(timestamp);
    }
    if next.is_terminal() {
        updated.completed_at = Some(timestamp);
        updated.terminal_reason = (next != RunStatus::Succeeded)
            .then(|| reason.map_or_else(|| "unspecified".to_owned(), redact_text));
    }
    append_memory_audit(state, &current.id, actor, timestamp, event_type)?;
    state.runs.insert(current.id.clone(), updated.clone());
    Ok(updated)
}

impl ApprovalRepository for InMemoryExecutionStore {
    fn create_approval(
        &self,
        mut request: ApprovalRequest,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError> {
        if request.status != ApprovalStatus::Pending
            || request.revision != 0
            || request.requested_by != trusted_actor
            || request.created_at != trusted_now
            || request.request_reason.len() > 2048
        {
            return Err(approval_error("new approval invariants are invalid"));
        }
        request.request_reason = redact_text(&request.request_reason);
        request.validate()?;

        let mut state = self.lock()?;
        let run = state.runs.get(&request.run_id).ok_or_else(run_missing)?;
        if run.status != RunStatus::AwaitingApproval {
            return Err(approval_error("approval run is not awaiting approval"));
        }
        let run_revision = run.revision;
        if state.approvals.contains_key(&request.id)
            || state.action_claims.contains_key(&request.action_id)
            || state
                .approvals
                .values()
                .any(|stored| stored.action_id == request.action_id)
            || state.approvals.values().any(|stored| {
                stored.run_id == request.run_id
                    && stored.action_id == request.action_id
                    && stored.parameter_hash == request.parameter_hash
                    && matches!(
                        stored.status,
                        ApprovalStatus::Pending | ApprovalStatus::Approved
                    )
            })
        {
            return Err(approval_error("approval already exists"));
        }
        let event = approval_event(
            &state,
            &request,
            AuditEventType::ApprovalRequested,
            trusted_actor,
            trusted_now,
        )?;
        let State {
            approvals,
            approval_windows,
            audits,
            ..
        } = &mut *state;
        let events = audits.get_mut(&request.run_id).ok_or_else(audit_missing)?;
        approvals.insert(request.id.clone(), request.clone());
        approval_windows.insert(request.id.clone(), run_revision);
        events.push(event);
        Ok(request)
    }

    fn decide_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        decision: ApprovalDecision,
        trusted_actor: ActorId,
        reason: Option<&str>,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError> {
        if reason.is_some_and(|value| value.len() > 2048) {
            return Err(approval_error("approval decision reason is too long"));
        }
        let mut state = self.lock()?;
        let current = state
            .approvals
            .get(approval_id)
            .ok_or_else(approval_missing)?
            .clone();
        if current.status != ApprovalStatus::Pending
            || current.revision != expected_revision
            || current.is_expired_at(trusted_now)
        {
            return Err(approval_error(
                "approval is expired, stale, or no longer pending",
            ));
        }
        if decision == ApprovalDecision::Approved {
            approval_window_run(&state, &current)?;
        }
        let status = match decision {
            ApprovalDecision::Approved => ApprovalStatus::Approved,
            ApprovalDecision::Denied => ApprovalStatus::Denied,
        };
        let mut updated = current;
        updated.status = status;
        updated.revision = next_revision(updated.revision)?;
        updated.decision = Some(ApprovalDecisionRecord {
            decision,
            actor: trusted_actor.clone(),
            reason: reason.map(redact_text),
            decided_at: trusted_now,
        });
        updated.validate()?;
        let event_type = match decision {
            ApprovalDecision::Approved => AuditEventType::ApprovalApproved,
            ApprovalDecision::Denied => AuditEventType::ApprovalDenied,
        };
        let event = approval_event(&state, &updated, event_type, trusted_actor, trusted_now)?;
        commit_approval(&mut state, &updated, event)?;
        Ok(updated)
    }

    fn expire_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError> {
        let mut state = self.lock()?;
        let mut updated = state
            .approvals
            .get(approval_id)
            .ok_or_else(approval_missing)?
            .clone();
        if !matches!(
            updated.status,
            ApprovalStatus::Pending | ApprovalStatus::Approved
        ) || updated.revision != expected_revision
            || !updated.is_expired_at(trusted_now)
        {
            return Err(approval_error(
                "approval cannot expire at this revision and time",
            ));
        }
        updated.status = ApprovalStatus::Expired;
        updated.revision = next_revision(updated.revision)?;
        updated.validate()?;
        let event = approval_event(
            &state,
            &updated,
            AuditEventType::ApprovalExpired,
            trusted_actor,
            trusted_now,
        )?;
        commit_approval(&mut state, &updated, event)?;
        Ok(updated)
    }

    fn consume_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        action: &ActionRequest,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalConsumption, OpenWorkError> {
        let mut state = self.lock()?;
        let current = state
            .approvals
            .get(approval_id)
            .ok_or_else(approval_missing)?
            .clone();
        let run = approval_window_run(&state, &current)?;
        if current.status == ApprovalStatus::Approved
            && current.revision == expected_revision
            && !current.is_expired_at(trusted_now)
            && !current.binding_matches(action)
        {
            let event = approval_event(
                &state,
                &current,
                AuditEventType::ApprovalBindingMismatch,
                trusted_actor,
                trusted_now,
            )?;
            state
                .audits
                .get_mut(&current.run_id)
                .ok_or_else(audit_missing)?
                .push(event);
            return Err(approval_error("approval binding does not match action"));
        }
        current.can_consume_at(action, expected_revision, trusted_now)?;
        if state.action_claims.contains_key(&action.id) {
            return Err(approval_error("action was already claimed"));
        }
        let mut updated = current;
        updated.status = ApprovalStatus::Consumed;
        updated.revision = next_revision(updated.revision)?;
        updated.consumed_at = Some(trusted_now);
        updated.validate()?;
        let claim = ActionClaim {
            run_id: action.run_id.clone(),
            action_id: action.id.clone(),
            parameter_hash: action.parameter_hash().clone(),
            actor: trusted_actor.clone(),
            claimed_at: trusted_now,
        };
        let event = approval_event(
            &state,
            &updated,
            AuditEventType::ApprovalConsumed,
            trusted_actor.clone(),
            trusted_now,
        )?;
        let mut updated_run = run;
        updated_run.status = RunStatus::Running;
        updated_run.revision = updated_run
            .revision
            .checked_add(1)
            .ok_or_else(|| state_error("run revision overflow"))?;
        updated_run.updated_at = trusted_now;
        if updated_run.started_at.is_none() {
            updated_run.started_at = Some(trusted_now);
        }
        let next_sequence = event
            .sequence
            .checked_add(1)
            .ok_or_else(|| internal_error("audit sequence overflow"))?;
        let runtime_event = AuditAppend::new(trusted_actor, trusted_now)
            .with_run_status(RunStatus::Running)
            .build(
                updated_run.id.clone(),
                next_sequence,
                AuditEventType::RuntimeStarted,
                Some(event.event_hash().clone()),
            )?;
        let State {
            runs,
            audits,
            approvals,
            action_claims,
            ..
        } = &mut *state;
        let events = audits.get_mut(&updated.run_id).ok_or_else(audit_missing)?;
        approvals.insert(updated.id.clone(), updated.clone());
        runs.insert(updated_run.id.clone(), updated_run);
        events.push(event);
        events.push(runtime_event);
        action_claims.insert(claim.action_id.clone(), claim.clone());
        Ok(ApprovalConsumption {
            approval: updated,
            action_claim: claim,
        })
    }

    fn get_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<Option<ApprovalRequest>, OpenWorkError> {
        Ok(self.lock()?.approvals.get(approval_id).cloned())
    }

    fn get_action_claim(&self, action_id: &ActionId) -> Result<Option<ActionClaim>, OpenWorkError> {
        Ok(self.lock()?.action_claims.get(action_id).cloned())
    }
}

fn approval_window_run(state: &State, approval: &ApprovalRequest) -> Result<Run, OpenWorkError> {
    let run = state.runs.get(&approval.run_id).ok_or_else(run_missing)?;
    if run.status != RunStatus::AwaitingApproval
        || state.approval_windows.get(&approval.id) != Some(&run.revision)
    {
        return Err(approval_error(
            "approval does not belong to the current awaiting-approval window",
        ));
    }
    Ok(run.clone())
}

fn approval_event(
    state: &State,
    approval: &ApprovalRequest,
    event_type: AuditEventType,
    actor: ActorId,
    timestamp: UtcTimestamp,
) -> Result<AuditEvent, OpenWorkError> {
    let events = state
        .audits
        .get(&approval.run_id)
        .ok_or_else(audit_missing)?;
    if events
        .last()
        .is_some_and(|event| timestamp < event.timestamp)
    {
        return Err(approval_error("approval audit timestamp moved backwards"));
    }
    let sequence = u64::try_from(events.len())
        .map_err(|_| internal_error("audit sequence overflow"))?
        .checked_add(1)
        .ok_or_else(|| internal_error("audit sequence overflow"))?;
    let metadata = BTreeMap::from([
        ("approval_id".to_owned(), json!(&approval.id)),
        ("action_id".to_owned(), json!(&approval.action_id)),
        ("parameter_hash".to_owned(), json!(&approval.parameter_hash)),
        ("revision".to_owned(), json!(approval.revision)),
        ("status".to_owned(), json!(approval.status)),
    ]);
    AuditEvent::new(
        AuditEventId::generate(),
        approval.run_id.clone(),
        sequence,
        event_type,
        actor,
        timestamp,
        RedactedAuditMetadata::from_untrusted(&metadata),
        events.last().map(|event| event.event_hash().clone()),
    )
}

fn commit_approval(
    state: &mut State,
    approval: &ApprovalRequest,
    event: AuditEvent,
) -> Result<(), OpenWorkError> {
    let State {
        approvals, audits, ..
    } = state;
    let events = audits.get_mut(&approval.run_id).ok_or_else(audit_missing)?;
    approvals.insert(approval.id.clone(), approval.clone());
    events.push(event);
    Ok(())
}

fn next_revision(revision: u64) -> Result<u64, OpenWorkError> {
    revision
        .checked_add(1)
        .ok_or_else(|| approval_error("approval revision overflow"))
}

fn approval_missing() -> OpenWorkError {
    approval_error("approval does not exist")
}

fn approval_error(message: &str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ApprovalInvalid, message)
}

fn validate_new_run(run: &Run) -> Result<(), OpenWorkError> {
    if run.status != RunStatus::Queued
        || run.revision != 0
        || run.runtime.trim().is_empty()
        || run.runtime.len() > 128
        || run.workspace.as_os_str().is_empty()
        || run.updated_at != run.created_at
        || run.started_at.is_some()
        || run.completed_at.is_some()
        || run.terminal_reason.is_some()
    {
        return Err(state_error("new run invariants are invalid"));
    }
    Ok(())
}

const fn transition_event(status: RunStatus) -> AuditEventType {
    match status {
        RunStatus::Planning => AuditEventType::RuntimeSelected,
        RunStatus::AwaitingApproval => AuditEventType::ActionRequested,
        RunStatus::Running => AuditEventType::RuntimeStarted,
        RunStatus::Succeeded => AuditEventType::RunCompleted,
        RunStatus::Failed | RunStatus::Cancelled | RunStatus::TimedOut | RunStatus::Queued => {
            AuditEventType::RunFailed
        }
    }
}

fn state_error(message: &str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::InvalidStateTransition, message)
}

fn run_missing() -> OpenWorkError {
    state_error("run does not exist")
}

fn audit_missing() -> OpenWorkError {
    internal_error("run audit chain does not exist")
}

fn internal_error(message: &str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::Internal, message)
}
