//! Exact managed-action scenario fixtures for later policy integration.

use crate::FixtureError;
use openwork_execution::{ActionId, ActionRequest, PolicyDecision, RiskLevel, RunId, Sha256Digest};
use serde::Deserialize;
use serde_json::Value;

const SCENARIO_VERSION: u32 = 1;

/// A validated action plus the policy outcome a later pipeline must prove.
#[derive(Clone)]
pub struct ScenarioFixture {
    name: String,
    kind: ScenarioKind,
    request: ActionRequest,
    expected_risk: RiskLevel,
    expected_decision: PolicyDecision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScenarioKind {
    RiskyExternal,
    Destructive,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioWire {
    schema_version: u32,
    name: String,
    kind: ScenarioKind,
    run_id: String,
    action_id: String,
    action: String,
    resource: String,
    parameters: Value,
    expected: ExpectedWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedWire {
    effective_risk: RiskLevel,
    decision: PolicyDecision,
    parameter_hash: String,
}

impl ScenarioFixture {
    /// Parses one scenario and verifies its exact frozen action binding.
    ///
    /// # Errors
    ///
    /// Returns a content-free error for malformed, mislabeled, or tampered fixtures.
    pub fn from_json(input: &str) -> Result<Self, FixtureError> {
        let wire = serde_json::from_str::<ScenarioWire>(input)
            .map_err(|_| FixtureError("scenario JSON is invalid"))?;
        if wire.schema_version != SCENARIO_VERSION
            || !valid_name(&wire.name)
            || !wire.parameters.is_object()
        {
            return Err(FixtureError("scenario invariants are invalid"));
        }
        let request = ActionRequest::new(
            ActionId::parse(&wire.action_id)
                .map_err(|_| FixtureError("scenario action ID is invalid"))?,
            RunId::parse(&wire.run_id).map_err(|_| FixtureError("scenario run ID is invalid"))?,
            wire.action,
            wire.resource,
            wire.parameters,
        )
        .map_err(|_| FixtureError("scenario action is invalid"))?;
        let expected_hash = Sha256Digest::parse(wire.expected.parameter_hash)
            .map_err(|_| FixtureError("scenario binding is invalid"))?;
        if request.parameter_hash() != &expected_hash
            || !expected_semantics(
                wire.kind,
                &request.action,
                wire.expected.effective_risk,
                wire.expected.decision,
            )
        {
            return Err(FixtureError("scenario binding or expectation mismatch"));
        }
        Ok(Self {
            name: wire.name,
            kind: wire.kind,
            request,
            expected_risk: wire.expected.effective_risk,
            expected_decision: wire.expected.decision,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn action_request(&self) -> &ActionRequest {
        &self.request
    }

    #[must_use]
    pub const fn expected_risk(&self) -> RiskLevel {
        self.expected_risk
    }

    #[must_use]
    pub const fn expected_decision(&self) -> PolicyDecision {
        self.expected_decision
    }

    #[must_use]
    pub const fn is_destructive(&self) -> bool {
        matches!(self.kind, ScenarioKind::Destructive)
    }
}

fn expected_semantics(
    kind: ScenarioKind,
    action: &str,
    risk: RiskLevel,
    decision: PolicyDecision,
) -> bool {
    match kind {
        ScenarioKind::RiskyExternal => {
            action == "email.send"
                && risk == RiskLevel::ExternalEffect
                && decision == PolicyDecision::RequireApproval
        }
        ScenarioKind::Destructive => {
            action == "database.delete"
                && risk == RiskLevel::DestructiveOrFinancial
                && decision == PolicyDecision::Deny
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}
