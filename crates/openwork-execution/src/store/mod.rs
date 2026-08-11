//! Transactional execution persistence boundary and deterministic memory store.

use crate::approval::{ActionClaim, ApprovalConsumption, ApprovalRepository};
use crate::audit::AuditAppend;
use crate::{
    ActionId, ActionRequest, ActorId, ApprovalDecision, ApprovalDecisionRecord, ApprovalId,
    ApprovalRequest, ApprovalStatus, Artifact, AuditEvent, AuditEventId, AuditEventType,
    RedactedAuditMetadata, Run, RunId, RunStatus, UtcTimestamp,
};
use openwork_core::{ErrorCode, OpenWorkError, redact_text};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Mutex;

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

impl InMemoryExecutionStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, OpenWorkError> {
        self.state
            .lock()
            .map_err(|_| internal_error("execution store lock poisoned"))
    }
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
            approval_id: updated.id.clone(),
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
