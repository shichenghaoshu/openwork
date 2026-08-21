//! Postgres-backed execution store and approval repository.
//!
//! Every write operation acquires a row-level lock (`SELECT ... FOR UPDATE`),
//! checks the compare-and-swap revision, and appends a hash-chained audit event
//! inside a single database transaction. The `action_claims` unique constraint
//! serves as the final defense against replay attacks on consumed approvals.
//!
//! All public trait methods are synchronous (matching the [`ExecutionStore`] and
//! [`ApprovalRepository`] signatures) and internally drive their async database
//! work through a runtime-aware adapter so they may be called from synchronous,
//! current-thread Tokio, or multi-thread Tokio contexts.

use crate::action_executor::ActionExecutionReceipt;
use crate::approval::{ActionClaim, ApprovalConsumption, ApprovalRepository};
use crate::audit::AuditAppend;
use crate::{
    ActionId, ActionRequest, ActorId, ApprovalDecision, ApprovalDecisionRecord, ApprovalId,
    ApprovalRequest, ApprovalStatus, Artifact, AuditEvent, AuditEventId, AuditEventType,
    RedactedAuditMetadata, Run, RunId, RunStatus, Sha256Digest, UtcTimestamp,
};
use crate::{EXECUTION_SCHEMA_VERSION, audit_event_type_name};
use openwork_core::{ErrorCode, OpenWorkError};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Public store handle
// ---------------------------------------------------------------------------

/// Postgres-backed execution store that implements both [`ExecutionStore`] and
/// [`ApprovalRepository`] with full CAS semantics and hash-chain integrity.
#[derive(Clone)]
pub struct PostgresExecutionStore {
    pool: PgPool,
}

/// Postgres `timestamptz` stores microseconds. Keep the frozen public v1
/// timestamp contract lossless and normalize only objects crossing this store
/// boundary, before timestamp-dependent audit hashes are constructed.
fn postgres_timestamp(timestamp: UtcTimestamp) -> UtcTimestamp {
    let microsecond_nanoseconds = (timestamp.0.nanosecond() / 1_000) * 1_000;
    UtcTimestamp(
        timestamp
            .0
            .replace_nanosecond(microsecond_nanoseconds)
            .expect("a truncated nanosecond is always in range"),
    )
}

fn postgres_run(mut run: Run) -> Run {
    run.created_at = postgres_timestamp(run.created_at);
    run.updated_at = postgres_timestamp(run.updated_at);
    run.started_at = run.started_at.map(postgres_timestamp);
    run.completed_at = run.completed_at.map(postgres_timestamp);
    run
}

fn postgres_artifact(mut artifact: Artifact) -> Artifact {
    artifact.created_at = postgres_timestamp(artifact.created_at);
    artifact
}

fn postgres_approval(mut approval: ApprovalRequest) -> ApprovalRequest {
    approval.created_at = postgres_timestamp(approval.created_at);
    approval.expires_at = postgres_timestamp(approval.expires_at);
    approval.consumed_at = approval.consumed_at.map(postgres_timestamp);
    if let Some(decision) = &mut approval.decision {
        decision.decided_at = postgres_timestamp(decision.decided_at);
    }
    approval
}

fn postgres_audit(mut audit: AuditAppend) -> AuditAppend {
    audit.timestamp = postgres_timestamp(audit.timestamp);
    audit
}

/// Stable, non-sensitive reason persisted for runs interrupted by a control
/// plane restart.
pub const CRASH_RECOVERY_REASON: &str = "control plane restarted during active execution";

/// Result of one deterministic startup-recovery pass.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryReport {
    pub recovered_run_ids: Vec<RunId>,
}

impl PostgresExecutionStore {
    /// Wraps an existing connection pool. The caller is responsible for running
    /// migrations before the store is used.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Atomically fails runs left in `planning` or `running` by an interrupted
    /// control-plane process and appends one hash-chained `run_failed` event per
    /// recovered run.
    ///
    /// `queued`, `awaiting_approval`, terminal runs, and active runs with a
    /// still-durable lease are intentionally left unchanged. Lease expiry is
    /// handled separately by [`super::RunQueueRepository::recover_expired_leases`]
    /// so a control API restart cannot kill a healthy independent worker. Rows
    /// are locked and revision-CAS updated in UUID order; repeated calls are
    /// idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error and rolls back the entire recovery pass when a database,
    /// revision, timestamp, or audit-chain invariant fails.
    pub fn recover_interrupted_runs(
        &self,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<RecoveryReport, OpenWorkError> {
        let trusted_now = postgres_timestamp(trusted_now);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let rows = sqlx::query(
                "SELECT id, runtime, workspace, status::text, revision, actor_id,
                        prompt_sha256::text, created_at, updated_at, started_at,
                        completed_at, terminal_reason
                 FROM runs
                 WHERE status IN ('planning'::run_status, 'running'::run_status)
                   AND NOT EXISTS (SELECT 1 FROM run_leases WHERE run_leases.run_id = runs.id)
                 ORDER BY id
                 FOR UPDATE",
            )
            .fetch_all(&mut *tx)
            .await
            .map_err(internal_db)?;
            let interrupted = rows.iter().map(row_to_run).collect::<Result<Vec<_>, _>>()?;
            let mut recovered_run_ids = Vec::with_capacity(interrupted.len());

            for current in interrupted {
                let last_audit_ts = last_audit_timestamp_tx(&mut tx, &current.id).await?;
                if last_audit_ts.is_some_and(|timestamp| trusted_now < timestamp) {
                    return Err(super::state_error(
                        "recovery timestamp cannot move audit time backwards",
                    ));
                }
                let sequence = next_audit_sequence_tx(&mut tx, &current.id).await?;
                let previous_hash = last_audit_hash_tx(&mut tx, &current.id).await?;
                let updated = apply_run_transition(
                    &current,
                    RunStatus::Failed,
                    Some(CRASH_RECOVERY_REASON),
                    trusted_now,
                )?;
                let event = AuditAppend::new(trusted_actor.clone(), trusted_now)
                    .with_run_status(RunStatus::Failed)
                    .build(
                        current.id.clone(),
                        sequence,
                        AuditEventType::RunFailed,
                        previous_hash,
                    )?;

                update_run_tx(&mut tx, &updated, current.revision).await?;
                insert_audit_event_tx(&mut tx, &event).await?;
                recovered_run_ids.push(current.id);
            }

            tx.commit().await.map_err(internal_db)?;
            Ok(RecoveryReport { recovered_run_ids })
        })
    }
}

impl super::RunQueueRepository for PostgresExecutionStore {
    fn claim_next_run(
        &self,
        owner: ActorId,
        now: UtcTimestamp,
        ttl: time::Duration,
    ) -> Result<Option<super::RunLease>, OpenWorkError> {
        let now = postgres_timestamp(now);
        let expires = super::checked_lease_expiry(now, ttl)?;
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let row = sqlx::query("SELECT id, runtime, workspace, status::text, revision, actor_id, prompt_sha256::text, created_at, updated_at, started_at, completed_at, terminal_reason FROM runs WHERE status = 'queued'::run_status AND NOT EXISTS (SELECT 1 FROM run_leases WHERE run_leases.run_id = runs.id) ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1")
                .fetch_optional(&mut *tx).await.map_err(internal_db)?;
            let Some(row) = row else {
                tx.commit().await.map_err(internal_db)?;
                return Ok(None);
            };
            let current = row_to_run(&row)?;
            if now < current.updated_at {
                return Err(super::state_error("claim timestamp cannot move backwards"));
            }
            let updated = apply_run_transition(&current, RunStatus::Planning, None, now)?;
            let sequence = next_audit_sequence_tx(&mut tx, &current.id).await?;
            let event = AuditAppend::new(owner.clone(), now)
                .with_run_status(RunStatus::Planning)
                .build(
                    current.id.clone(),
                    sequence,
                    AuditEventType::RuntimeSelected,
                    last_audit_hash_tx(&mut tx, &current.id).await?,
                )?;
            let token = super::LeaseToken::generate();
            update_run_tx(&mut tx, &updated, current.revision).await?;
            sqlx::query("INSERT INTO run_leases (run_id, lease_token, owner_id, acquired_at, expires_at) VALUES ($1, $2, $3, $4, $5)")
                .bind(current.id.0).bind(token.0).bind(owner.as_str()).bind(now.0).bind(expires.0).execute(&mut *tx).await.map_err(internal_db)?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(Some(super::RunLease {
                run: updated,
                token,
                owner,
                acquired_at: now,
                expires_at: expires,
                cancel_requested: false,
            }))
        })
    }

    fn heartbeat_lease(
        &self,
        lease: &super::RunLease,
        now: UtcTimestamp,
        ttl: time::Duration,
    ) -> Result<super::RunLease, OpenWorkError> {
        let lease = lease.clone();
        let now = postgres_timestamp(now);
        let expires = super::checked_lease_expiry(now, ttl)?;
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_run_tx(&mut tx, &lease.run.id).await?;
            validate_current_lease_tx(&mut tx, &lease, &current, lease.run.revision, now).await?;
            let requested: Option<time::OffsetDateTime> =
                sqlx::query_scalar("SELECT cancel_requested_at FROM runs WHERE id=$1")
                    .bind(lease.run.id.0)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(internal_db)?;
            sqlx::query("UPDATE run_leases SET expires_at=$3 WHERE run_id=$1 AND lease_token=$2")
                .bind(lease.run.id.0)
                .bind(lease.token.0)
                .bind(expires.0)
                .execute(&mut *tx)
                .await
                .map_err(internal_db)?;
            tx.commit().await.map_err(internal_db)?;
            Ok(super::RunLease {
                run: current,
                expires_at: expires,
                cancel_requested: requested.is_some(),
                token: lease.token,
                owner: lease.owner,
                acquired_at: lease.acquired_at,
            })
        })
    }

    fn transition_leased_run(
        &self,
        lease: &super::RunLease,
        expected_revision: u64,
        next: RunStatus,
        now: UtcTimestamp,
    ) -> Result<super::RunLease, OpenWorkError> {
        if next.is_terminal() {
            return Err(super::state_error(
                "leased transition target must be non-terminal",
            ));
        }
        let lease = lease.clone();
        let now = postgres_timestamp(now);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_run_tx(&mut tx, &lease.run.id).await?;
            validate_current_lease_tx(&mut tx, &lease, &current, expected_revision, now).await?;
            ensure_audit_time_tx(&mut tx, &lease.run.id, now, current.updated_at).await?;
            let updated = apply_run_transition(&current, next, None, now)?;
            let event = AuditAppend::new(lease.owner.clone(), now)
                .with_run_status(next)
                .build(
                    lease.run.id.clone(),
                    next_audit_sequence_tx(&mut tx, &lease.run.id).await?,
                    super::transition_event(next),
                    last_audit_hash_tx(&mut tx, &lease.run.id).await?,
                )?;
            update_run_tx(&mut tx, &updated, expected_revision).await?;
            insert_audit_event_tx(&mut tx, &event).await?;
            let cancel_requested: bool =
                sqlx::query_scalar("SELECT cancel_requested_at IS NOT NULL FROM runs WHERE id=$1")
                    .bind(lease.run.id.0)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(internal_db)?;
            tx.commit().await.map_err(internal_db)?;
            Ok(super::RunLease {
                run: updated,
                cancel_requested,
                ..lease
            })
        })
    }

    fn complete_leased_run(
        &self,
        lease: &super::RunLease,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        now: UtcTimestamp,
    ) -> Result<Run, OpenWorkError> {
        if !matches!(
            next,
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::TimedOut
        ) {
            return Err(super::state_error(
                "leased completion must be succeeded, failed, or timed_out",
            ));
        }
        let lease = lease.clone();
        let reason = reason.map(String::from);
        let now = postgres_timestamp(now);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_run_tx(&mut tx, &lease.run.id).await?;
            validate_current_lease_tx(&mut tx, &lease, &current, expected_revision, now).await?;
            ensure_audit_time_tx(&mut tx, &lease.run.id, now, current.updated_at).await?;
            let cancel_requested: bool =
                sqlx::query_scalar("SELECT cancel_requested_at IS NOT NULL FROM runs WHERE id=$1")
                    .bind(lease.run.id.0)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(internal_db)?;
            if next == RunStatus::Succeeded && cancel_requested {
                return Err(super::state_error(
                    "cannot complete a cancellation-requested run successfully",
                ));
            }
            let updated = apply_run_transition(&current, next, reason.as_deref(), now)?;
            let event = AuditAppend::new(lease.owner.clone(), now)
                .with_run_status(next)
                .build(
                    lease.run.id.clone(),
                    next_audit_sequence_tx(&mut tx, &lease.run.id).await?,
                    super::transition_event(next),
                    last_audit_hash_tx(&mut tx, &lease.run.id).await?,
                )?;
            update_run_tx(&mut tx, &updated, expected_revision).await?;
            insert_audit_event_tx(&mut tx, &event).await?;
            let deleted = sqlx::query(
                "DELETE FROM run_leases WHERE run_id=$1 AND lease_token=$2 AND owner_id=$3",
            )
            .bind(lease.run.id.0)
            .bind(lease.token.0)
            .bind(lease.owner.as_str())
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;
            if deleted.rows_affected() != 1 {
                return Err(super::state_error("lease is not current"));
            }
            tx.commit().await.map_err(internal_db)?;
            Ok(updated)
        })
    }

    fn request_cancel(
        &self,
        run_id: &RunId,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<super::CancelRequest, OpenWorkError> {
        let run_id = run_id.clone();
        let now = postgres_timestamp(now);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_run_tx(&mut tx, &run_id).await?;
            if current.status.is_terminal() {
                tx.commit().await.map_err(internal_db)?;
                return Ok(super::CancelRequest::AlreadyTerminal(current.status));
            }
            let has_lease = sqlx::query("SELECT run_id FROM run_leases WHERE run_id=$1 FOR UPDATE")
                .bind(run_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal_db)?
                .is_some();
            if matches!(
                current.status,
                RunStatus::Queued | RunStatus::AwaitingApproval
            ) && !has_lease
            {
                ensure_audit_time_tx(&mut tx, &run_id, now, current.updated_at).await?;
                let updated = apply_run_transition(
                    &current,
                    RunStatus::Cancelled,
                    Some("cancelled before worker claim"),
                    now,
                )?;
                let event = AuditAppend::new(actor, now)
                    .with_run_status(RunStatus::Cancelled)
                    .build(
                        run_id.clone(),
                        next_audit_sequence_tx(&mut tx, &run_id).await?,
                        AuditEventType::CancelConfirmed,
                        last_audit_hash_tx(&mut tx, &run_id).await?,
                    )?;
                update_run_tx(&mut tx, &updated, current.revision).await?;
                insert_audit_event_tx(&mut tx, &event).await?;
                tx.commit().await.map_err(internal_db)?;
                return Ok(super::CancelRequest::Cancelled);
            }
            if !matches!(
                current.status,
                RunStatus::Planning | RunStatus::AwaitingApproval | RunStatus::Running
            ) {
                return Err(super::state_error(
                    "only active runs may request cancellation",
                ));
            }
            if !has_lease {
                return Err(super::state_error(
                    "active run has no current worker lease for cancellation",
                ));
            }
            let already: Option<time::OffsetDateTime> =
                sqlx::query_scalar("SELECT cancel_requested_at FROM runs WHERE id=$1")
                    .bind(run_id.0)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(internal_db)?;
            if already.is_some() {
                tx.commit().await.map_err(internal_db)?;
                return Ok(super::CancelRequest::Requested);
            }
            ensure_audit_time_tx(&mut tx, &run_id, now, current.updated_at).await?;
            let event = AuditAppend::new(actor, now).build(
                run_id.clone(),
                next_audit_sequence_tx(&mut tx, &run_id).await?,
                AuditEventType::CancelRequested,
                last_audit_hash_tx(&mut tx, &run_id).await?,
            )?;
            sqlx::query("UPDATE runs SET cancel_requested_at=$2 WHERE id=$1")
                .bind(run_id.0)
                .bind(now.0)
                .execute(&mut *tx)
                .await
                .map_err(internal_db)?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(super::CancelRequest::Requested)
        })
    }

    fn lease_cancel_requested(
        &self,
        lease: &super::RunLease,
        now: UtcTimestamp,
    ) -> Result<bool, OpenWorkError> {
        let lease = lease.clone();
        let now = postgres_timestamp(now);
        let pool = self.pool.clone();
        block_on(async move {
            let found: Option<bool> = sqlx::query_scalar("SELECT r.cancel_requested_at IS NOT NULL FROM run_leases l JOIN runs r ON r.id=l.run_id WHERE l.run_id=$1 AND l.lease_token=$2 AND l.owner_id=$3 AND l.acquired_at <= $4 AND l.expires_at > $4 AND r.revision=$5 AND r.updated_at <= $4")
                .bind(lease.run.id.0).bind(lease.token.0).bind(lease.owner.as_str()).bind(now.0).bind(i64::try_from(lease.run.revision).map_err(|_| super::state_error("revision overflow"))?).fetch_optional(&pool).await.map_err(internal_db)?;
            found.ok_or_else(|| super::state_error("lease is not current"))
        })
    }

    fn confirm_cancel(
        &self,
        lease: &super::RunLease,
        now: UtcTimestamp,
        evidence: super::CancellationEvidence,
    ) -> Result<Run, OpenWorkError> {
        if evidence.run_id != lease.run.id
            || evidence.sandbox_id.is_empty()
            || evidence.lease_token != lease.token
            || evidence.lease_owner != lease.owner
        {
            return Err(super::state_error(
                "cancellation evidence does not match lease",
            ));
        }
        let lease = lease.clone();
        let now = postgres_timestamp(now);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_run_tx(&mut tx, &lease.run.id).await?;
            validate_current_lease_tx(&mut tx, &lease, &current, lease.run.revision, now).await?;
            ensure_audit_time_tx(&mut tx, &lease.run.id, now, current.updated_at).await?;
            let cancel_requested: bool =
                sqlx::query_scalar("SELECT cancel_requested_at IS NOT NULL FROM runs WHERE id=$1")
                    .bind(lease.run.id.0)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(internal_db)?;
            if !cancel_requested {
                return Err(super::state_error(
                    "lease is not eligible to confirm cancellation",
                ));
            }
            let updated = apply_run_transition(
                &current,
                RunStatus::Cancelled,
                Some("worker confirmed sandbox termination and cleanup"),
                now,
            )?;
            let event = AuditAppend::new(lease.owner.clone(), now)
                .with_run_status(RunStatus::Cancelled)
                .build(
                    lease.run.id.clone(),
                    next_audit_sequence_tx(&mut tx, &lease.run.id).await?,
                    AuditEventType::CancelConfirmed,
                    last_audit_hash_tx(&mut tx, &lease.run.id).await?,
                )?;
            update_run_tx(&mut tx, &updated, current.revision).await?;
            insert_audit_event_tx(&mut tx, &event).await?;
            let deleted = sqlx::query(
                "DELETE FROM run_leases WHERE run_id=$1 AND lease_token=$2 AND owner_id=$3",
            )
            .bind(lease.run.id.0)
            .bind(lease.token.0)
            .bind(lease.owner.as_str())
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;
            if deleted.rows_affected() != 1 {
                return Err(super::state_error("lease is not current"));
            }
            tx.commit().await.map_err(internal_db)?;
            Ok(updated)
        })
    }

    fn recover_expired_leases(
        &self,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<Vec<RunId>, OpenWorkError> {
        let now = postgres_timestamp(now);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let rows = sqlx::query("SELECT r.id, r.runtime, r.workspace, r.status::text, r.revision, r.actor_id, r.prompt_sha256::text, r.created_at, r.updated_at, r.started_at, r.completed_at, r.terminal_reason FROM runs r JOIN run_leases l ON l.run_id=r.id WHERE l.expires_at <= $1 ORDER BY r.id FOR UPDATE OF r").bind(now.0).fetch_all(&mut *tx).await.map_err(internal_db)?;
            let mut ids = Vec::new();
            for row in rows {
                let current = row_to_run(&row)?;
                ensure_audit_time_tx(&mut tx, &current.id, now, current.updated_at).await?;
                if !current.status.is_terminal() {
                    let updated = apply_run_transition(
                        &current,
                        RunStatus::Failed,
                        Some("worker lease expired without durable completion evidence"),
                        now,
                    )?;
                    let event = AuditAppend::new(actor.clone(), now)
                        .with_run_status(RunStatus::Failed)
                        .build(
                            current.id.clone(),
                            next_audit_sequence_tx(&mut tx, &current.id).await?,
                            AuditEventType::RunFailed,
                            last_audit_hash_tx(&mut tx, &current.id).await?,
                        )?;
                    update_run_tx(&mut tx, &updated, current.revision).await?;
                    insert_audit_event_tx(&mut tx, &event).await?;
                }
                sqlx::query("DELETE FROM run_leases WHERE run_id=$1")
                    .bind(current.id.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(internal_db)?;
                ids.push(current.id);
            }
            tx.commit().await.map_err(internal_db)?;
            Ok(ids)
        })
    }
}

// ---------------------------------------------------------------------------
// ExecutionStore
// ---------------------------------------------------------------------------

impl super::ExecutionStore for PostgresExecutionStore {
    fn create_run(&self, run: Run, audit: AuditAppend) -> Result<Run, OpenWorkError> {
        super::validate_new_run(&run)?;
        if audit.timestamp != run.created_at {
            return Err(super::state_error(
                "genesis audit timestamp must match run creation",
            ));
        }
        let run = postgres_run(run);
        let audit = postgres_audit(audit);
        let event = audit.build(run.id.clone(), 1, AuditEventType::RunCreated, None)?;
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            insert_run_tx(&mut tx, &run).await?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(run)
        })
    }

    fn transition_run(
        &self,
        run_id: &RunId,
        expected_revision: u64,
        next: RunStatus,
        reason: Option<&str>,
        audit: AuditAppend,
    ) -> Result<Run, OpenWorkError> {
        let run_id = run_id.clone();
        let reason = reason.map(String::from);
        let audit = postgres_audit(audit);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_run_tx(&mut tx, &run_id).await?;
            let leased = sqlx::query("SELECT run_id FROM run_leases WHERE run_id=$1 FOR UPDATE")
                .bind(run_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal_db)?;
            if leased.is_some() {
                return Err(super::state_error(
                    "leased runs must use a lease-capability-bound transition",
                ));
            }
            if current.revision != expected_revision || !current.status.can_transition_to(next) {
                return Err(super::state_error(
                    "run revision is stale or transition is illegal",
                ));
            }
            if audit.timestamp < current.updated_at {
                return Err(super::state_error("run timestamps cannot move backwards"));
            }
            let last_hash = last_audit_hash_tx(&mut tx, &run_id).await?;
            let last_audit_ts = last_audit_timestamp_tx(&mut tx, &run_id).await?;
            if last_audit_ts.is_some_and(|ts| audit.timestamp < ts) {
                return Err(super::state_error("audit timestamps cannot move backwards"));
            }
            let sequence = next_audit_sequence_tx(&mut tx, &run_id).await?;
            let event_type = super::transition_event(next);
            let audit_ts = audit.timestamp;
            let event = audit.with_run_status(next).build(
                run_id.clone(),
                sequence,
                event_type,
                last_hash,
            )?;
            let updated = apply_run_transition(&current, next, reason.as_deref(), audit_ts)?;
            update_run_tx(&mut tx, &updated, expected_revision).await?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(updated)
        })
    }

    fn append_audit(
        &self,
        run_id: &RunId,
        event_type: AuditEventType,
        audit: AuditAppend,
    ) -> Result<AuditEvent, OpenWorkError> {
        let run_id = run_id.clone();
        let audit = postgres_audit(audit);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let _run = lock_run_tx(&mut tx, &run_id).await?;
            let last_audit_ts = last_audit_timestamp_tx(&mut tx, &run_id).await?;
            if last_audit_ts.is_some_and(|ts| audit.timestamp < ts) {
                return Err(super::state_error("audit timestamps cannot move backwards"));
            }
            let sequence = next_audit_sequence_tx(&mut tx, &run_id).await?;
            let last_hash = last_audit_hash_tx(&mut tx, &run_id).await?;
            let event = audit.build(run_id.clone(), sequence, event_type, last_hash)?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(event)
        })
    }

    fn reconcile_action_execution(
        &self,
        receipt: &ActionExecutionReceipt,
        audit: AuditAppend,
    ) -> Result<bool, OpenWorkError> {
        let receipt = receipt.clone();
        let audit = postgres_audit(audit);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let _run = lock_run_tx(&mut tx, &receipt.run_id).await?;
            let claim = sqlx::query(
                "SELECT run_id, parameter_hash::text
                 FROM action_claims WHERE action_id = $1",
            )
            .bind(receipt.action_id.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal_db)?
            .ok_or_else(|| approval_err("action execution has no durable claim"))?;
            if claim.get::<Uuid, _>("run_id") != receipt.run_id.0
                || claim.get::<String, _>("parameter_hash") != receipt.parameter_hash.as_str()
            {
                return Err(approval_err(
                    "action execution receipt does not match its durable claim",
                ));
            }

            let rows = sqlx::query(
                "SELECT redacted_metadata
                 FROM audit_events
                 WHERE run_id = $1 AND event_type = 'action_executed'",
            )
            .bind(receipt.run_id.0)
            .fetch_all(&mut *tx)
            .await
            .map_err(internal_db)?;
            let expected_action_id = receipt.action_id.to_hyphenated();
            let mut exact_matches = 0_u8;
            for row in rows {
                let metadata = row.get::<Value, _>("redacted_metadata");
                let action_id = metadata
                    .get("action_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| approval_err("action execution audit metadata is invalid"))?;
                let parameter_hash = metadata
                    .get("parameter_hash")
                    .and_then(Value::as_str)
                    .ok_or_else(|| approval_err("action execution audit metadata is invalid"))?;
                if action_id == expected_action_id {
                    if parameter_hash != receipt.parameter_hash.as_str() {
                        return Err(approval_err("action execution audit binding is invalid"));
                    }
                    exact_matches = exact_matches
                        .checked_add(1)
                        .ok_or_else(|| approval_err("duplicate action execution audit"))?;
                }
            }
            match exact_matches {
                1 => {
                    tx.commit().await.map_err(internal_db)?;
                    return Ok(false);
                }
                0 => {}
                _ => return Err(approval_err("duplicate action execution audit")),
            }

            let last_audit_ts = last_audit_timestamp_tx(&mut tx, &receipt.run_id).await?;
            if last_audit_ts.is_some_and(|timestamp| audit.timestamp < timestamp) {
                return Err(super::state_error(
                    "action execution audit timestamp moved backwards",
                ));
            }
            let sequence = next_audit_sequence_tx(&mut tx, &receipt.run_id).await?;
            let previous_hash = last_audit_hash_tx(&mut tx, &receipt.run_id).await?;
            let event = audit
                .with_action_execution(receipt.action_id.clone(), receipt.parameter_hash.clone())
                .build(
                    receipt.run_id,
                    sequence,
                    AuditEventType::ActionExecuted,
                    previous_hash,
                )?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(true)
        })
    }

    fn record_artifacts(
        &self,
        run_id: &RunId,
        artifacts: Vec<Artifact>,
        audit: AuditAppend,
    ) -> Result<(), OpenWorkError> {
        let artifacts = artifacts
            .into_iter()
            .map(postgres_artifact)
            .collect::<Vec<_>>();
        let audit = postgres_audit(audit);
        if artifacts.iter().any(|a| &a.run_id != run_id) {
            return Err(OpenWorkError::new(
                ErrorCode::ArtifactInvalid,
                "artifact run mismatch",
            ));
        }
        if artifacts.iter().any(|a| a.media_type.trim().is_empty()) {
            return Err(OpenWorkError::new(
                ErrorCode::ArtifactInvalid,
                "artifact media type is empty",
            ));
        }
        {
            let mut paths: Vec<&str> = artifacts.iter().map(|a| a.path.as_str()).collect();
            paths.sort_unstable();
            if paths.windows(2).any(|w| w[0] == w[1]) {
                return Err(OpenWorkError::new(
                    ErrorCode::ArtifactInvalid,
                    "duplicate artifact path",
                ));
            }
        }
        let run_id = run_id.clone();
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let _run = lock_run_tx(&mut tx, &run_id).await?;
            for artifact in &artifacts {
                let exists = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM artifacts WHERE run_id = $1 AND path = $2)",
                )
                .bind(run_id.0)
                .bind(artifact.path.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(internal_db)?;
                if exists {
                    return Err(OpenWorkError::new(
                        ErrorCode::ArtifactInvalid,
                        "duplicate artifact path",
                    ));
                }
            }
            let last_audit_ts = last_audit_timestamp_tx(&mut tx, &run_id).await?;
            if last_audit_ts.is_some_and(|ts| audit.timestamp < ts) {
                return Err(super::state_error("audit timestamps cannot move backwards"));
            }
            let mut prev_hash = last_audit_hash_tx(&mut tx, &run_id).await?;
            let mut next_seq = next_audit_sequence_tx(&mut tx, &run_id).await?;
            for artifact in &artifacts {
                sqlx::query(
                    "INSERT INTO artifacts (id, run_id, path, media_type, size_bytes, sha256, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                )
                .bind(artifact.id.0)
                .bind(run_id.0)
                .bind(artifact.path.as_str())
                .bind(&artifact.media_type)
                .bind(i64::try_from(artifact.size_bytes.get()).unwrap_or(i64::MAX))
                .bind(artifact.sha256.as_str())
                .bind(artifact.created_at.0)
                .execute(&mut *tx)
                .await
                .map_err(internal_db)?;
                let event = audit.clone().with_artifact(artifact).build(
                    run_id.clone(),
                    next_seq,
                    AuditEventType::ArtifactCreated,
                    prev_hash,
                )?;
                prev_hash = Some(event.event_hash().clone());
                next_seq = next_seq
                    .checked_add(1)
                    .ok_or_else(|| super::internal_error("audit sequence overflow"))?;
                insert_audit_event_tx(&mut tx, &event).await?;
            }
            tx.commit().await.map_err(internal_db)?;
            Ok(())
        })
    }

    fn get_run(&self, run_id: &RunId) -> Result<Option<Run>, OpenWorkError> {
        let run_id = run_id.clone();
        let pool = self.pool.clone();
        block_on(async move {
            sqlx::query(
                "SELECT id, runtime, workspace, status::text, revision, actor_id, prompt_sha256::text,
                        created_at, updated_at, started_at, completed_at, terminal_reason
                 FROM runs WHERE id = $1",
            )
            .bind(run_id.0)
            .fetch_optional(&pool)
            .await
            .map_err(internal_db)
        })?
        .map(|row| row_to_run(&row))
        .transpose()
    }

    fn audit_events(&self, run_id: &RunId) -> Result<Vec<AuditEvent>, OpenWorkError> {
        let run_id = run_id.clone();
        let pool = self.pool.clone();
        let rows = block_on(async move {
            sqlx::query(
                "SELECT id, run_id, sequence, event_type, actor_id, occurred_at,
                        redacted_metadata, previous_hash::text, event_hash::text
                 FROM audit_events WHERE run_id = $1 ORDER BY sequence",
            )
            .bind(run_id.0)
            .fetch_all(&pool)
            .await
            .map_err(internal_db)
        })?;
        let events: Vec<AuditEvent> = rows
            .iter()
            .map(row_to_audit_event)
            .collect::<Result<_, _>>()?;
        verify_audit_chain(&events)?;
        Ok(events)
    }

    fn artifacts(&self, run_id: &RunId) -> Result<Vec<Artifact>, OpenWorkError> {
        let run_id = run_id.clone();
        let pool = self.pool.clone();
        let rows = block_on(async move {
            sqlx::query(
                "SELECT id, run_id, path, media_type, size_bytes, sha256::text, created_at
                 FROM artifacts WHERE run_id = $1 ORDER BY created_at, id",
            )
            .bind(run_id.0)
            .fetch_all(&pool)
            .await
            .map_err(internal_db)
        })?;
        rows.iter().map(row_to_artifact).collect()
    }
}

// ---------------------------------------------------------------------------
// ApprovalRepository
// ---------------------------------------------------------------------------

impl ApprovalRepository for PostgresExecutionStore {
    fn create_approval(
        &self,
        mut request: ApprovalRequest,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError> {
        use openwork_core::redact_text;

        if request.status != ApprovalStatus::Pending
            || request.revision != 0
            || request.requested_by != trusted_actor
            || request.created_at != trusted_now
            || request.request_reason.len() > 2048
        {
            return Err(approval_err("new approval invariants are invalid"));
        }
        request.validate()?;
        let trusted_now = postgres_timestamp(trusted_now);
        request = postgres_approval(request);
        request.request_reason = redact_text(&request.request_reason);
        request.validate()?;

        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let run = lock_run_tx(&mut tx, &request.run_id).await?;
            if run.status != RunStatus::AwaitingApproval {
                return Err(approval_err("approval run is not awaiting approval"));
            }
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(
                    SELECT 1 FROM approval_requests
                    WHERE id = $1 OR action_id = $2
                )",
            )
            .bind(request.id.0)
            .bind(request.action_id.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(internal_db)?;
            if exists {
                return Err(approval_err("approval already exists"));
            }
            let claimed = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM action_claims WHERE action_id = $1)",
            )
            .bind(request.action_id.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(internal_db)?;
            if claimed {
                return Err(approval_err("approval already exists"));
            }

            let event = build_approval_event_tx(
                &mut tx,
                &request,
                AuditEventType::ApprovalRequested,
                trusted_actor,
                trusted_now,
            )
            .await?;

            sqlx::query(
                "INSERT INTO approval_requests
                    (id, run_id, action_id, parameter_hash, requested_by, request_reason,
                     created_at, expires_at, status, revision, awaiting_run_revision)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending'::approval_status, 0, $9)",
            )
            .bind(request.id.0)
            .bind(request.run_id.0)
            .bind(request.action_id.0)
            .bind(request.parameter_hash.as_str())
            .bind(request.requested_by.as_str())
            .bind(&request.request_reason)
            .bind(request.created_at.0)
            .bind(request.expires_at.0)
            .bind(i64::try_from(run.revision).map_err(|_| approval_err("run revision overflow"))?)
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;

            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(request)
        })
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
        use openwork_core::redact_text;

        if reason.is_some_and(|value| value.len() > 2048) {
            return Err(approval_err("approval decision reason is too long"));
        }
        let trusted_now = postgres_timestamp(trusted_now);
        let approval_id = approval_id.clone();
        let reason = reason.map(String::from);
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_approval_tx(&mut tx, &approval_id).await?;
            if current.status != ApprovalStatus::Pending
                || current.revision != expected_revision
                || current.is_expired_at(trusted_now)
            {
                return Err(approval_err(
                    "approval is expired, stale, or no longer pending",
                ));
            }
            if decision == ApprovalDecision::Approved {
                verify_approval_window_tx(&mut tx, &current).await?;
            }
            let new_status = match decision {
                ApprovalDecision::Approved => ApprovalStatus::Approved,
                ApprovalDecision::Denied => ApprovalStatus::Denied,
            };
            let new_revision = current
                .revision
                .checked_add(1)
                .ok_or_else(|| approval_err("approval revision overflow"))?;
            let mut updated = current;
            updated.status = new_status;
            updated.revision = new_revision;
            updated.decision = Some(ApprovalDecisionRecord {
                decision,
                actor: trusted_actor.clone(),
                reason: reason.map(|r| redact_text(&r)),
                decided_at: trusted_now,
            });
            updated.validate()?;

            let rows = sqlx::query(
                "UPDATE approval_requests
                 SET status = $2::approval_status, revision = $3
                 WHERE id = $1 AND revision = $4",
            )
            .bind(approval_id.0)
            .bind(approval_status_str(updated.status))
            .bind(i64::try_from(updated.revision).map_err(|_| approval_err("revision overflow"))?)
            .bind(i64::try_from(expected_revision).map_err(|_| approval_err("revision overflow"))?)
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;
            if rows.rows_affected() == 0 {
                return Err(approval_err("approval revision is stale"));
            }

            let decision_record = updated.decision.as_ref().unwrap();
            sqlx::query(
                "INSERT INTO approval_decisions
                    (approval_id, decision, actor_id, reason, decided_at, approval_revision)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(approval_id.0)
            .bind(match decision_record.decision {
                ApprovalDecision::Approved => "approved",
                ApprovalDecision::Denied => "denied",
            })
            .bind(decision_record.actor.as_str())
            .bind(&decision_record.reason)
            .bind(decision_record.decided_at.0)
            .bind(i64::try_from(updated.revision).map_err(|_| approval_err("revision overflow"))?)
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;

            let event_type = match decision {
                ApprovalDecision::Approved => AuditEventType::ApprovalApproved,
                ApprovalDecision::Denied => AuditEventType::ApprovalDenied,
            };
            let event =
                build_approval_event_tx(&mut tx, &updated, event_type, trusted_actor, trusted_now)
                    .await?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(updated)
        })
    }

    fn expire_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError> {
        let trusted_now = postgres_timestamp(trusted_now);
        let approval_id = approval_id.clone();
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_approval_tx(&mut tx, &approval_id).await?;
            if !matches!(
                current.status,
                ApprovalStatus::Pending | ApprovalStatus::Approved
            ) || current.revision != expected_revision
                || !current.is_expired_at(trusted_now)
            {
                return Err(approval_err(
                    "approval cannot expire at this revision and time",
                ));
            }
            let new_revision = current
                .revision
                .checked_add(1)
                .ok_or_else(|| approval_err("approval revision overflow"))?;
            let mut updated = current;
            updated.status = ApprovalStatus::Expired;
            updated.revision = new_revision;
            updated.validate()?;

            let rows = sqlx::query(
                "UPDATE approval_requests
                 SET status = 'expired'::approval_status, revision = $2
                 WHERE id = $1 AND revision = $3",
            )
            .bind(approval_id.0)
            .bind(i64::try_from(updated.revision).map_err(|_| approval_err("revision overflow"))?)
            .bind(i64::try_from(expected_revision).map_err(|_| approval_err("revision overflow"))?)
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;
            if rows.rows_affected() == 0 {
                return Err(approval_err("approval revision is stale"));
            }

            let event = build_approval_event_tx(
                &mut tx,
                &updated,
                AuditEventType::ApprovalExpired,
                trusted_actor,
                trusted_now,
            )
            .await?;
            insert_audit_event_tx(&mut tx, &event).await?;
            tx.commit().await.map_err(internal_db)?;
            Ok(updated)
        })
    }

    #[allow(clippy::too_many_lines)]
    fn consume_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        action: &ActionRequest,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalConsumption, OpenWorkError> {
        let trusted_now = postgres_timestamp(trusted_now);
        let approval_id = approval_id.clone();
        let action = action.clone();
        let pool = self.pool.clone();
        block_on(async move {
            let mut tx = pool.begin().await.map_err(internal_db)?;
            let current = lock_approval_tx(&mut tx, &approval_id).await?;
            let run = lock_run_tx(&mut tx, &current.run_id).await?;
            let awaiting_run_revision = approval_window_revision_tx(&mut tx, &current.id).await?;
            let run_revision = run.revision;
            verify_approval_window(awaiting_run_revision, &run)?;

            if current.status == ApprovalStatus::Approved
                && current.revision == expected_revision
                && !current.is_expired_at(trusted_now)
                && !current.binding_matches(&action)
            {
                let mismatch_event = build_approval_event_tx(
                    &mut tx,
                    &current,
                    AuditEventType::ApprovalBindingMismatch,
                    trusted_actor,
                    trusted_now,
                )
                .await?;
                insert_audit_event_tx(&mut tx, &mismatch_event).await?;
                tx.commit().await.map_err(internal_db)?;
                return Err(approval_err("approval binding does not match action"));
            }

            current.can_consume_at(&action, expected_revision, trusted_now)?;

            let already_claimed = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM action_claims WHERE action_id = $1)",
            )
            .bind(action.id.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(internal_db)?;
            if already_claimed {
                return Err(approval_err("action was already claimed"));
            }

            let new_approval_revision = current
                .revision
                .checked_add(1)
                .ok_or_else(|| approval_err("approval revision overflow"))?;
            let mut updated_approval = current;
            updated_approval.status = ApprovalStatus::Consumed;
            updated_approval.revision = new_approval_revision;
            updated_approval.consumed_at = Some(trusted_now);
            updated_approval.validate()?;

            let claim = ActionClaim {
                run_id: action.run_id.clone(),
                action_id: action.id.clone(),
                parameter_hash: action.parameter_hash().clone(),
                actor: trusted_actor.clone(),
                claimed_at: trusted_now,
            };

            let new_run_revision = run
                .revision
                .checked_add(1)
                .ok_or_else(|| super::state_error("run revision overflow"))?;
            let mut updated_run = run;
            updated_run.status = RunStatus::Running;
            updated_run.revision = new_run_revision;
            updated_run.updated_at = trusted_now;
            if updated_run.started_at.is_none() {
                updated_run.started_at = Some(trusted_now);
            }

            let consume_event = build_approval_event_tx(
                &mut tx,
                &updated_approval,
                AuditEventType::ApprovalConsumed,
                trusted_actor.clone(),
                trusted_now,
            )
            .await?;

            let next_sequence = consume_event
                .sequence
                .checked_add(1)
                .ok_or_else(|| super::internal_error("audit sequence overflow"))?;
            let runtime_event = AuditAppend::new(trusted_actor, trusted_now)
                .with_run_status(RunStatus::Running)
                .build(
                    updated_run.id.clone(),
                    next_sequence,
                    AuditEventType::RuntimeStarted,
                    Some(consume_event.event_hash().clone()),
                )?;

            // CAS-update approval_requests.
            let approval_rows = sqlx::query(
                "UPDATE approval_requests
                 SET status = 'consumed'::approval_status, revision = $2, consumed_at = $3
                 WHERE id = $1 AND revision = $4",
            )
            .bind(approval_id.0)
            .bind(
                i64::try_from(updated_approval.revision)
                    .map_err(|_| approval_err("revision overflow"))?,
            )
            .bind(updated_approval.consumed_at.map(|t| t.0))
            .bind(i64::try_from(expected_revision).map_err(|_| approval_err("revision overflow"))?)
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;
            if approval_rows.rows_affected() == 0 {
                return Err(approval_err("approval revision is stale"));
            }

            // CAS-update runs.
            let run_rows = sqlx::query(
                "UPDATE runs
                 SET status = 'running'::run_status, revision = $2, updated_at = $3,
                     started_at = COALESCE(started_at, $3)
                 WHERE id = $1 AND revision = $4",
            )
            .bind(updated_run.id.0)
            .bind(
                i64::try_from(updated_run.revision)
                    .map_err(|_| super::state_error("revision overflow"))?,
            )
            .bind(updated_run.updated_at.0)
            .bind(i64::try_from(run_revision).map_err(|_| super::state_error("revision overflow"))?)
            .execute(&mut *tx)
            .await
            .map_err(internal_db)?;
            if run_rows.rows_affected() == 0 {
                return Err(super::state_error("run revision is stale"));
            }

            // Insert action_claims (unique constraint is final replay defense).
            sqlx::query(
                "INSERT INTO action_claims
                    (approval_id, run_id, action_id, parameter_hash, actor_id, claimed_at)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(approval_id.0)
            .bind(claim.run_id.0)
            .bind(claim.action_id.0)
            .bind(claim.parameter_hash.as_str())
            .bind(claim.actor.as_str())
            .bind(claim.claimed_at.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                if is_unique_violation(&e) {
                    approval_err("action was already claimed")
                } else {
                    internal_db(e)
                }
            })?;

            insert_audit_event_tx(&mut tx, &consume_event).await?;
            insert_audit_event_tx(&mut tx, &runtime_event).await?;
            tx.commit().await.map_err(internal_db)?;

            Ok(ApprovalConsumption {
                approval: updated_approval,
                action_claim: claim,
            })
        })
    }

    fn get_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<Option<ApprovalRequest>, OpenWorkError> {
        let approval_id = approval_id.clone();
        let pool = self.pool.clone();
        block_on(async move {
            sqlx::query(
                "SELECT ar.id, ar.run_id, ar.action_id, ar.parameter_hash::text,
                        ar.requested_by, ar.request_reason, ar.created_at, ar.expires_at,
                        ar.status::text, ar.revision, ar.consumed_at,
                        ad.decision, ad.actor_id AS decision_actor, ad.reason AS decision_reason,
                        ad.decided_at, ad.approval_revision
                 FROM approval_requests ar
                 LEFT JOIN approval_decisions ad ON ad.approval_id = ar.id
                 WHERE ar.id = $1",
            )
            .bind(approval_id.0)
            .fetch_optional(&pool)
            .await
            .map_err(internal_db)
        })?
        .map(|row| row_to_approval_request(&row))
        .transpose()
    }

    fn get_action_claim(&self, action_id: &ActionId) -> Result<Option<ActionClaim>, OpenWorkError> {
        let action_id = action_id.clone();
        let pool = self.pool.clone();
        block_on(async move {
            sqlx::query(
                "SELECT approval_id, run_id, action_id, parameter_hash::text, actor_id, claimed_at
                 FROM action_claims WHERE action_id = $1",
            )
            .bind(action_id.0)
            .fetch_optional(&pool)
            .await
            .map_err(internal_db)
        })?
        .as_ref()
        .map(row_to_action_claim)
        .transpose()
    }
}

// ---------------------------------------------------------------------------
// Transaction helpers -- run
// ---------------------------------------------------------------------------

async fn lock_run_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: &RunId,
) -> Result<Run, OpenWorkError> {
    let row = sqlx::query(
        "SELECT id, runtime, workspace, status::text, revision, actor_id, prompt_sha256::text,
                created_at, updated_at, started_at, completed_at, terminal_reason
         FROM runs WHERE id = $1 FOR UPDATE",
    )
    .bind(run_id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_db)?
    .ok_or_else(super::run_missing)?;
    row_to_run(&row)
}

/// Locks the lease after its run row has already been locked, preserving the
/// global `runs -> run_leases` lock order used by every worker operation.
async fn validate_current_lease_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease: &super::RunLease,
    current: &Run,
    expected_revision: u64,
    now: UtcTimestamp,
) -> Result<(), OpenWorkError> {
    if lease.run.revision != expected_revision || current.revision != expected_revision {
        return Err(super::state_error("run revision is stale"));
    }
    let row = sqlx::query(
        "SELECT lease_token, owner_id, acquired_at, expires_at
         FROM run_leases WHERE run_id=$1 FOR UPDATE",
    )
    .bind(lease.run.id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_db)?
    .ok_or_else(|| super::state_error("lease does not exist"))?;
    let token: Uuid = row.get("lease_token");
    let owner: String = row.get("owner_id");
    let acquired_at = UtcTimestamp(row.get::<time::OffsetDateTime, _>("acquired_at"));
    let expires_at = UtcTimestamp(row.get::<time::OffsetDateTime, _>("expires_at"));
    if token != lease.token.0
        || owner != lease.owner.as_str()
        || now < acquired_at
        || now >= expires_at
        || now < current.updated_at
    {
        return Err(super::state_error(
            "lease capability, revision, or time is not current",
        ));
    }
    Ok(())
}

async fn insert_run_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &Run,
) -> Result<(), OpenWorkError> {
    sqlx::query(
        "INSERT INTO runs
            (id, runtime, workspace, status, revision, actor_id, prompt_sha256, created_at, updated_at)
         VALUES ($1, $2, $3, 'queued'::run_status, 0, $4, $5, $6, $6)",
    )
    .bind(run.id.0)
    .bind(&run.runtime)
    .bind(run.workspace.to_str().unwrap_or(""))
    .bind(run.actor_id.as_str())
    .bind(run.prompt_sha256.as_str())
    .bind(run.created_at.0)
    .execute(&mut **tx)
    .await
    .map_err(internal_db)?;
    Ok(())
}

async fn update_run_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &Run,
    expected_revision: u64,
) -> Result<(), OpenWorkError> {
    let rows = sqlx::query(
        "UPDATE runs
         SET status = $2::run_status, revision = $3, updated_at = $4,
             started_at = $5, completed_at = $6, terminal_reason = $7
         WHERE id = $1 AND revision = $8",
    )
    .bind(run.id.0)
    .bind(run_status_str(run.status))
    .bind(i64::try_from(run.revision).map_err(|_| super::state_error("revision overflow"))?)
    .bind(run.updated_at.0)
    .bind(run.started_at.map(|t| t.0))
    .bind(run.completed_at.map(|t| t.0))
    .bind(&run.terminal_reason)
    .bind(i64::try_from(expected_revision).map_err(|_| super::state_error("revision overflow"))?)
    .execute(&mut **tx)
    .await
    .map_err(internal_db)?;
    if rows.rows_affected() == 0 {
        return Err(super::state_error("run revision is stale"));
    }
    Ok(())
}

fn apply_run_transition(
    current: &Run,
    next: RunStatus,
    reason: Option<&str>,
    timestamp: UtcTimestamp,
) -> Result<Run, OpenWorkError> {
    use openwork_core::redact_text;

    let mut updated = current.clone();
    updated.status = next;
    updated.revision = updated
        .revision
        .checked_add(1)
        .ok_or_else(|| super::state_error("run revision overflow"))?;
    updated.updated_at = timestamp;
    if next == RunStatus::Running && updated.started_at.is_none() {
        updated.started_at = Some(timestamp);
    }
    if next.is_terminal() {
        updated.completed_at = Some(timestamp);
        updated.terminal_reason = (next != RunStatus::Succeeded)
            .then(|| reason.map_or_else(|| "unspecified".to_owned(), redact_text));
    }
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Transaction helpers -- audit
// ---------------------------------------------------------------------------

async fn insert_audit_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: &AuditEvent,
) -> Result<(), OpenWorkError> {
    if event.timestamp != postgres_timestamp(event.timestamp) {
        return Err(super::internal_error(
            "audit timestamp was not normalized for Postgres",
        ));
    }
    let metadata_value = serde_json::to_value(event.metadata.as_map())
        .map_err(|_| super::internal_error("serialize metadata"))?;
    sqlx::query(
        "INSERT INTO audit_events
            (id, run_id, sequence, event_type, actor_id, occurred_at,
             redacted_metadata, previous_hash, event_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(event.id.0)
    .bind(event.run_id.0)
    .bind(i64::try_from(event.sequence).map_err(|_| super::internal_error("sequence overflow"))?)
    .bind(audit_event_type_name(event.event_type))
    .bind(event.actor.as_str())
    .bind(event.timestamp.0)
    .bind(&metadata_value)
    .bind(event.previous_hash.as_ref().map(Sha256Digest::as_str))
    .bind(event.event_hash().as_str())
    .execute(&mut **tx)
    .await
    .map_err(internal_db)?;
    Ok(())
}

async fn last_audit_hash_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: &RunId,
) -> Result<Option<Sha256Digest>, OpenWorkError> {
    let hash: Option<String> = sqlx::query_scalar(
        "SELECT event_hash::text FROM audit_events
         WHERE run_id = $1 ORDER BY sequence DESC LIMIT 1",
    )
    .bind(run_id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_db)?;
    hash.map(|s| Sha256Digest::parse(s).map_err(|e| super::internal_error(&e.to_string())))
        .transpose()
}

async fn last_audit_timestamp_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: &RunId,
) -> Result<Option<UtcTimestamp>, OpenWorkError> {
    let ts: Option<time::OffsetDateTime> = sqlx::query_scalar(
        "SELECT occurred_at FROM audit_events
         WHERE run_id = $1 ORDER BY sequence DESC LIMIT 1",
    )
    .bind(run_id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_db)?;
    Ok(ts.map(UtcTimestamp))
}

async fn ensure_audit_time_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: &RunId,
    timestamp: UtcTimestamp,
    updated_at: UtcTimestamp,
) -> Result<(), OpenWorkError> {
    if timestamp < updated_at
        || last_audit_timestamp_tx(tx, run_id)
            .await?
            .is_some_and(|last| timestamp < last)
    {
        return Err(super::state_error("audit timestamps cannot move backwards"));
    }
    Ok(())
}

async fn next_audit_sequence_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: &RunId,
) -> Result<u64, OpenWorkError> {
    let max_seq: Option<i64> =
        sqlx::query_scalar("SELECT MAX(sequence) FROM audit_events WHERE run_id = $1")
            .bind(run_id.0)
            .fetch_one(&mut **tx)
            .await
            .map_err(internal_db)?;
    let current: u64 = max_seq.map_or(0, |s| u64::try_from(s).unwrap_or(0));
    current
        .checked_add(1)
        .ok_or_else(|| super::internal_error("audit sequence overflow"))
}

// ---------------------------------------------------------------------------
// Transaction helpers -- approval
// ---------------------------------------------------------------------------

async fn lock_approval_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    approval_id: &ApprovalId,
) -> Result<ApprovalRequest, OpenWorkError> {
    let row = sqlx::query(
        "SELECT ar.id, ar.run_id, ar.action_id, ar.parameter_hash::text,
                ar.requested_by, ar.request_reason, ar.created_at, ar.expires_at,
                ar.status::text, ar.revision, ar.consumed_at,
                ad.decision, ad.actor_id AS decision_actor, ad.reason AS decision_reason,
                ad.decided_at, ad.approval_revision
         FROM approval_requests ar
         LEFT JOIN approval_decisions ad ON ad.approval_id = ar.id
         WHERE ar.id = $1
         FOR UPDATE OF ar",
    )
    .bind(approval_id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal_db)?
    .ok_or_else(approval_missing_err)?;
    row_to_approval_request(&row)
}

async fn verify_approval_window_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    approval: &ApprovalRequest,
) -> Result<(), OpenWorkError> {
    let run = lock_run_tx(tx, &approval.run_id).await?;
    let awaiting_run_revision = approval_window_revision_tx(tx, &approval.id).await?;
    verify_approval_window(awaiting_run_revision, &run)
}

async fn approval_window_revision_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    approval_id: &ApprovalId,
) -> Result<u64, OpenWorkError> {
    let revision: i64 =
        sqlx::query_scalar("SELECT awaiting_run_revision FROM approval_requests WHERE id = $1")
            .bind(approval_id.0)
            .fetch_one(&mut **tx)
            .await
            .map_err(internal_db)?;
    u64::try_from(revision).map_err(|_| approval_err("awaiting run revision is invalid"))
}

fn verify_approval_window(awaiting_run_revision: u64, run: &Run) -> Result<(), OpenWorkError> {
    if run.status != RunStatus::AwaitingApproval || run.revision != awaiting_run_revision {
        return Err(approval_err(
            "approval does not belong to the current awaiting-approval window",
        ));
    }
    Ok(())
}

async fn build_approval_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    approval: &ApprovalRequest,
    event_type: AuditEventType,
    actor: ActorId,
    timestamp: UtcTimestamp,
) -> Result<AuditEvent, OpenWorkError> {
    use serde_json::json;

    let last_audit_ts = last_audit_timestamp_tx(tx, &approval.run_id).await?;
    if last_audit_ts.is_some_and(|ts| timestamp < ts) {
        return Err(approval_err("approval audit timestamp moved backwards"));
    }
    let sequence = next_audit_sequence_tx(tx, &approval.run_id).await?;
    let previous_hash = last_audit_hash_tx(tx, &approval.run_id).await?;

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
        previous_hash,
    )
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

fn row_to_run(row: &sqlx::postgres::PgRow) -> Result<Run, OpenWorkError> {
    Ok(Run {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: RunId(row.get::<Uuid, _>("id")),
        runtime: row.get::<String, _>("runtime"),
        workspace: PathBuf::from(row.get::<String, _>("workspace")),
        status: parse_run_status(&row.get::<String, _>("status"))?,
        revision: u64::try_from(row.get::<i64, _>("revision"))
            .map_err(|_| super::state_error("revision out of range"))?,
        actor_id: ActorId::parse(row.get::<String, _>("actor_id"))
            .map_err(|_| super::state_error("invalid actor_id"))?,
        prompt_sha256: Sha256Digest::parse(row.get::<String, _>("prompt_sha256"))
            .map_err(|_| super::state_error("invalid prompt_sha256"))?,
        created_at: UtcTimestamp(row.get::<time::OffsetDateTime, _>("created_at")),
        updated_at: UtcTimestamp(row.get::<time::OffsetDateTime, _>("updated_at")),
        started_at: row
            .get::<Option<time::OffsetDateTime>, _>("started_at")
            .map(UtcTimestamp),
        completed_at: row
            .get::<Option<time::OffsetDateTime>, _>("completed_at")
            .map(UtcTimestamp),
        terminal_reason: row.get::<Option<String>, _>("terminal_reason"),
    })
}

fn row_to_audit_event(row: &sqlx::postgres::PgRow) -> Result<AuditEvent, OpenWorkError> {
    let metadata_raw: Value = row.get("redacted_metadata");
    let metadata_map: BTreeMap<String, Value> = if let Value::Object(map) = metadata_raw {
        map.into_iter().collect()
    } else {
        BTreeMap::new()
    };
    let previous_hash: Option<String> = row.get("previous_hash");
    let previous_hash = previous_hash
        .map(|s| Sha256Digest::parse(s).map_err(|e| super::internal_error(&e.to_string())))
        .transpose()?;
    let event_hash_str: String = row.get("event_hash");
    let event = AuditEvent::new(
        AuditEventId(row.get::<Uuid, _>("id")),
        RunId(row.get::<Uuid, _>("run_id")),
        u64::try_from(row.get::<i64, _>("sequence"))
            .map_err(|_| super::internal_error("sequence out of range"))?,
        parse_audit_event_type(&row.get::<String, _>("event_type"))?,
        ActorId::parse(row.get::<String, _>("actor_id"))
            .map_err(|_| super::internal_error("invalid actor"))?,
        UtcTimestamp(row.get::<time::OffsetDateTime, _>("occurred_at")),
        RedactedAuditMetadata::from_untrusted(&metadata_map),
        previous_hash,
    )?;
    // Verify the stored hash matches canonical computation.
    if event.event_hash().as_str() != event_hash_str {
        return Err(super::internal_error("audit event hash mismatch"));
    }
    Ok(event)
}

fn row_to_artifact(row: &sqlx::postgres::PgRow) -> Result<Artifact, OpenWorkError> {
    use crate::{ArtifactId, ArtifactSizeBytes, RelativeArtifactPath};

    Ok(Artifact {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: ArtifactId(row.get::<Uuid, _>("id")),
        run_id: RunId(row.get::<Uuid, _>("run_id")),
        path: RelativeArtifactPath::parse(row.get::<String, _>("path"))
            .map_err(|e| super::internal_error(&e.to_string()))?,
        media_type: row.get::<String, _>("media_type"),
        size_bytes: ArtifactSizeBytes::new(
            u64::try_from(row.get::<i64, _>("size_bytes")).unwrap_or(0),
        )
        .map_err(|e| super::internal_error(&e.to_string()))?,
        sha256: Sha256Digest::parse(row.get::<String, _>("sha256"))
            .map_err(|e| super::internal_error(&e.to_string()))?,
        created_at: UtcTimestamp(row.get::<time::OffsetDateTime, _>("created_at")),
    })
}

fn row_to_approval_request(row: &sqlx::postgres::PgRow) -> Result<ApprovalRequest, OpenWorkError> {
    let decision: Option<String> = row.get("decision");
    let decision_record = if let Some(decision_str) = decision {
        let decided_at: time::OffsetDateTime = row.get("decided_at");
        Some(ApprovalDecisionRecord {
            decision: match decision_str.as_str() {
                "approved" => ApprovalDecision::Approved,
                "denied" => ApprovalDecision::Denied,
                _ => return Err(approval_err("invalid decision value in database")),
            },
            actor: ActorId::parse(row.get::<String, _>("decision_actor"))
                .map_err(|_| approval_err("invalid decision actor"))?,
            reason: row.get::<Option<String>, _>("decision_reason"),
            decided_at: UtcTimestamp(decided_at),
        })
    } else {
        None
    };

    Ok(ApprovalRequest {
        schema_version: EXECUTION_SCHEMA_VERSION,
        id: ApprovalId(row.get::<Uuid, _>("id")),
        run_id: RunId(row.get::<Uuid, _>("run_id")),
        action_id: ActionId(row.get::<Uuid, _>("action_id")),
        parameter_hash: Sha256Digest::parse(row.get::<String, _>("parameter_hash"))
            .map_err(|_| approval_err("invalid parameter_hash"))?,
        requested_by: ActorId::parse(row.get::<String, _>("requested_by"))
            .map_err(|_| approval_err("invalid requested_by"))?,
        request_reason: row.get::<String, _>("request_reason"),
        created_at: UtcTimestamp(row.get::<time::OffsetDateTime, _>("created_at")),
        expires_at: UtcTimestamp(row.get::<time::OffsetDateTime, _>("expires_at")),
        status: parse_approval_status(&row.get::<String, _>("status"))?,
        revision: u64::try_from(row.get::<i64, _>("revision"))
            .map_err(|_| approval_err("revision out of range"))?,
        decision: decision_record,
        consumed_at: row
            .get::<Option<time::OffsetDateTime>, _>("consumed_at")
            .map(UtcTimestamp),
    })
}

fn row_to_action_claim(row: &sqlx::postgres::PgRow) -> Result<ActionClaim, OpenWorkError> {
    let claimed_at: time::OffsetDateTime = row.get("claimed_at");
    Ok(ActionClaim {
        run_id: RunId(row.get::<Uuid, _>("run_id")),
        action_id: ActionId(row.get::<Uuid, _>("action_id")),
        parameter_hash: Sha256Digest::parse(row.get::<String, _>("parameter_hash"))
            .map_err(|_| approval_err("invalid parameter_hash"))?,
        actor: ActorId::parse(row.get::<String, _>("actor_id"))
            .map_err(|_| approval_err("invalid actor_id"))?,
        claimed_at: UtcTimestamp(claimed_at),
    })
}

fn verify_audit_chain(events: &[AuditEvent]) -> Result<(), OpenWorkError> {
    let mut previous_hash: Option<Sha256Digest> = None;
    for (index, event) in events.iter().enumerate() {
        let sequence = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| super::internal_error("audit sequence overflow"))?;
        event.verify_integrity(sequence, previous_hash.as_ref())?;
        previous_hash = Some(event.event_hash().clone());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// String <-> enum helpers
// ---------------------------------------------------------------------------

const fn run_status_str(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Planning => "planning",
        RunStatus::AwaitingApproval => "awaiting_approval",
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::TimedOut => "timed_out",
    }
}

fn parse_run_status(s: &str) -> Result<RunStatus, OpenWorkError> {
    match s {
        "queued" => Ok(RunStatus::Queued),
        "planning" => Ok(RunStatus::Planning),
        "awaiting_approval" => Ok(RunStatus::AwaitingApproval),
        "running" => Ok(RunStatus::Running),
        "succeeded" => Ok(RunStatus::Succeeded),
        "failed" => Ok(RunStatus::Failed),
        "cancelled" => Ok(RunStatus::Cancelled),
        "timed_out" => Ok(RunStatus::TimedOut),
        _ => Err(super::state_error("unknown run status")),
    }
}

const fn approval_status_str(status: ApprovalStatus) -> &'static str {
    match status {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Approved => "approved",
        ApprovalStatus::Denied => "denied",
        ApprovalStatus::Expired => "expired",
        ApprovalStatus::Consumed => "consumed",
    }
}

fn parse_approval_status(s: &str) -> Result<ApprovalStatus, OpenWorkError> {
    match s {
        "pending" => Ok(ApprovalStatus::Pending),
        "approved" => Ok(ApprovalStatus::Approved),
        "denied" => Ok(ApprovalStatus::Denied),
        "expired" => Ok(ApprovalStatus::Expired),
        "consumed" => Ok(ApprovalStatus::Consumed),
        _ => Err(approval_err("unknown approval status")),
    }
}

fn parse_audit_event_type(s: &str) -> Result<AuditEventType, OpenWorkError> {
    match s {
        "run_created" => Ok(AuditEventType::RunCreated),
        "runtime_selected" => Ok(AuditEventType::RuntimeSelected),
        "sandbox_created" => Ok(AuditEventType::SandboxCreated),
        "action_requested" => Ok(AuditEventType::ActionRequested),
        "policy_allowed" => Ok(AuditEventType::PolicyAllowed),
        "policy_denied" => Ok(AuditEventType::PolicyDenied),
        "approval_requested" => Ok(AuditEventType::ApprovalRequested),
        "approval_approved" => Ok(AuditEventType::ApprovalApproved),
        "approval_denied" => Ok(AuditEventType::ApprovalDenied),
        "approval_expired" => Ok(AuditEventType::ApprovalExpired),
        "approval_consumed" => Ok(AuditEventType::ApprovalConsumed),
        "action_executed" => Ok(AuditEventType::ActionExecuted),
        "runtime_started" => Ok(AuditEventType::RuntimeStarted),
        "runtime_output" => Ok(AuditEventType::RuntimeOutput),
        "artifact_created" => Ok(AuditEventType::ArtifactCreated),
        "runtime_completed" => Ok(AuditEventType::RuntimeCompleted),
        "sandbox_destroyed" => Ok(AuditEventType::SandboxDestroyed),
        "run_completed" => Ok(AuditEventType::RunCompleted),
        "run_failed" => Ok(AuditEventType::RunFailed),
        "approval_binding_mismatch" => Ok(AuditEventType::ApprovalBindingMismatch),
        "cancel_requested" => Ok(AuditEventType::CancelRequested),
        "cancel_confirmed" => Ok(AuditEventType::CancelConfirmed),
        _ => Err(super::internal_error("unknown audit event type")),
    }
}

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

#[allow(clippy::needless_pass_by_value)]
fn internal_db(err: sqlx::Error) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::Internal, err.to_string())
}

fn approval_err(message: &str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ApprovalInvalid, message)
}

fn approval_missing_err() -> OpenWorkError {
    approval_err("approval does not exist")
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(db_err) if db_err.is_unique_violation()
    )
}

// ---------------------------------------------------------------------------
// Synchronous adapter (for trait methods that are not async)
// ---------------------------------------------------------------------------

/// Runs an owned database future from synchronous or Tokio-backed callers.
/// Multi-thread runtimes can safely use `block_in_place`; a current-thread
/// runtime delegates to a dedicated thread so its sole executor is not blocked.
fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        Ok(_) => std::thread::spawn(move || database_runtime().block_on(future))
            .join()
            .expect("database runtime thread panicked"),
        Err(_) => database_runtime().block_on(future),
    }
}

fn database_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("database runtime must initialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_window_requires_exact_run_revision() {
        let now = UtcTimestamp::parse("2026-08-21T00:00:00Z").expect("timestamp");
        let run = Run {
            schema_version: EXECUTION_SCHEMA_VERSION,
            id: RunId::generate(),
            runtime: "mock".to_owned(),
            workspace: PathBuf::from("/tmp/openwork-test"),
            status: RunStatus::AwaitingApproval,
            revision: 3,
            actor_id: ActorId::parse("test:actor").expect("actor"),
            prompt_sha256: Sha256Digest::parse("a".repeat(64)).expect("digest"),
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            terminal_reason: None,
        };

        assert!(verify_approval_window(3, &run).is_ok());
        assert!(verify_approval_window(2, &run).is_err());
        let mut later_window = run;
        later_window.revision = 5;
        assert!(verify_approval_window(3, &later_window).is_err());
    }

    #[test]
    fn audit_reads_require_a_complete_hash_chain() {
        let run_id = RunId::generate();
        let actor = ActorId::parse("test:auditor").expect("actor");
        let now = UtcTimestamp::parse("2026-08-21T00:00:00Z").expect("timestamp");
        let first = AuditEvent::new(
            AuditEventId::generate(),
            run_id.clone(),
            1,
            AuditEventType::RunCreated,
            actor.clone(),
            now,
            RedactedAuditMetadata::from_untrusted(&BTreeMap::new()),
            None,
        )
        .expect("first event");
        let second = AuditEvent::new(
            AuditEventId::generate(),
            run_id,
            2,
            AuditEventType::RuntimeSelected,
            actor,
            now,
            RedactedAuditMetadata::from_untrusted(&BTreeMap::new()),
            Some(first.event_hash().clone()),
        )
        .expect("second event");

        assert!(verify_audit_chain(&[first.clone(), second.clone()]).is_ok());
        assert!(verify_audit_chain(&[second, first]).is_err());
    }

    #[test]
    fn timestamp_precision_is_normalized_only_at_postgres_boundary() {
        let exact =
            UtcTimestamp::parse("2026-08-21T00:00:00.123456789Z").expect("nanosecond timestamp");
        let normalized = postgres_timestamp(exact);

        assert_eq!(exact.unix_timestamp_nanos() % 1_000, 789);
        assert_eq!(normalized.unix_timestamp_nanos() % 1_000, 0);
        assert_eq!(
            serde_json::to_string(&normalized).expect("serialize timestamp"),
            "\"2026-08-21T00:00:00.123456Z\""
        );
    }

    #[test]
    fn block_on_supports_synchronous_callers() {
        assert_eq!(block_on(async { 7_u8 }), 7);
    }

    #[test]
    fn block_on_supports_current_thread_runtime_callers() {
        let value = database_runtime().block_on(async { block_on(async { 11_u8 }) });
        assert_eq!(value, 11);
    }

    #[test]
    fn block_on_supports_multi_thread_runtime_callers() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let value = runtime.block_on(async { block_on(async { 13_u8 }) });
        assert_eq!(value, 13);
    }
}
