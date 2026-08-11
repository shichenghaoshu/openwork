//! Approval persistence boundary for exact, single-use action authorization.

use crate::{
    ActionId, ActionRequest, ActorId, ApprovalDecision, ApprovalId, ApprovalRequest, RunId,
    Sha256Digest, UtcTimestamp,
};
use openwork_core::OpenWorkError;

/// Durable proof that one exact approved action was claimed for execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionClaim {
    pub approval_id: ApprovalId,
    pub run_id: RunId,
    pub action_id: ActionId,
    pub parameter_hash: Sha256Digest,
    pub actor: ActorId,
    pub claimed_at: UtcTimestamp,
}

/// Result of the atomic approval-consumption and action-claim transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalConsumption {
    pub approval: ApprovalRequest,
    pub action_claim: ActionClaim,
}

/// Compare-and-swap repository for approval state and its audit trail.
///
/// Implementations must obtain `trusted_now` from a server-controlled clock and
/// `trusted_actor` from authenticated context. Neither value may be decoded from
/// a model-authored action or public request body.
pub trait ApprovalRepository: Send + Sync {
    /// Persists a pending request, its current run-window revision binding, and
    /// `approval_requested` audit event atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid state, duplicate identity, actor mismatch,
    /// an absent run, or a storage failure.
    fn create_approval(
        &self,
        request: ApprovalRequest,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError>;

    /// CAS-transitions pending approval to approved or denied with one audit event.
    /// Approval requires the exact awaiting-approval window captured at create;
    /// denial remains available after a later transition for explicit cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error for expiry, stale revision, non-pending state, or storage failure.
    fn decide_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        decision: ApprovalDecision,
        trusted_actor: ActorId,
        reason: Option<&str>,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError>;

    /// CAS-transitions pending or approved approval to expired at the trusted deadline,
    /// including after a terminal run transition for scheduler cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error before expiry, for a stale revision, terminal state, or storage failure.
    fn expire_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalRequest, OpenWorkError>;

    /// Atomically consumes an exact approved binding from its original awaiting
    /// window, advances that run to running, and creates its action claim with
    /// both audit events.
    ///
    /// # Errors
    ///
    /// Returns an error for replay, expiry, stale revision, binding mismatch, or storage failure.
    fn consume_approval(
        &self,
        approval_id: &ApprovalId,
        expected_revision: u64,
        action: &ActionRequest,
        trusted_actor: ActorId,
        trusted_now: UtcTimestamp,
    ) -> Result<ApprovalConsumption, OpenWorkError>;

    /// Reads one approval.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be read.
    fn get_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<Option<ApprovalRequest>, OpenWorkError>;

    /// Reads an exact action claim if it was already consumed.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be read.
    fn get_action_claim(&self, action_id: &ActionId) -> Result<Option<ActionClaim>, OpenWorkError>;
}
