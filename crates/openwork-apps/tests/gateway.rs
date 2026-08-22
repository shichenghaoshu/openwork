use openwork_apps::credential::{CredentialBroker, EnvCredentialProvider};
use openwork_apps::gateway::{CapabilityAuthorizationResult, CapabilityGateway};
use openwork_apps::{
    AppId, CapabilityGrant, CapabilityId, CapabilityInvocation, CapabilityPack, InstalledApp,
};
use openwork_execution::{
    ActionPolicy, ActionRequest, ActorId, PolicyDecision, PolicyEvaluation, RiskLevel, RunId,
    UtcTimestamp,
};
use serde_json::json;
use std::collections::BTreeSet;

struct StaticPolicy(PolicyDecision);

impl ActionPolicy for StaticPolicy {
    fn evaluate(&self, request: &ActionRequest) -> PolicyEvaluation {
        PolicyEvaluation {
            schema_version: request.schema_version,
            run_id: request.run_id.clone(),
            action_id: request.id.clone(),
            parameter_hash: request.parameter_hash().clone(),
            decision: self.0,
            effective_risk: RiskLevel::ExternalEffect,
            policy_version: "test".to_owned(),
            rule_id: "test".to_owned(),
            reason_code: "test".to_owned(),
            evaluated_at: UtcTimestamp::now(),
        }
    }
}

fn app_id() -> AppId {
    AppId::parse("org.openwork.github").expect("app ID")
}

fn capability_id() -> CapabilityId {
    CapabilityId::parse("github.issue.create").expect("capability ID")
}

fn available_app() -> openwork_apps::AvailableApp {
    CapabilityPack::from_yaml(include_str!("../../../registry/apps/github.yaml"))
        .expect("manifest")
        .into_available()
}

fn invocation() -> CapabilityInvocation {
    CapabilityInvocation::new(
        ActorId::parse("alice").expect("actor"),
        RunId::generate(),
        app_id(),
        capability_id(),
        "github://acme/openwork/issues",
        json!({"owner": "acme", "repo": "openwork", "title": "x", "body": "y"}),
    )
    .expect("invocation")
}

fn gateway(policy: PolicyDecision) -> CapabilityGateway<StaticPolicy> {
    CapabilityGateway::new(
        StaticPolicy(policy),
        [available_app()],
        [InstalledApp {
            app_id: app_id(),
            enabled: true,
            credential_ref: Some("OPENWORK_TEST_MISSING_SECRET_7E9A".to_owned()),
        }],
        [CapabilityGrant {
            principal: "alice".to_owned(),
            app_id: app_id(),
            capabilities: BTreeSet::from([capability_id()]),
        }],
    )
}

#[test]
fn approval_required_never_issues_provider_authorization() {
    let result = gateway(PolicyDecision::RequireApproval)
        .authorize(invocation())
        .expect("approval result");

    match result {
        CapabilityAuthorizationResult::PendingApproval(pending) => {
            assert_eq!(
                pending.evaluation().decision,
                PolicyDecision::RequireApproval
            );
            assert_eq!(pending.invocation().capability, capability_id());
        }
        CapabilityAuthorizationResult::Authorized(_) => {
            panic!("approval-required action bypassed the approval boundary")
        }
    }
}

#[test]
fn credential_resolution_does_not_expose_secret_through_authorization() {
    let authorized = match gateway(PolicyDecision::Allow)
        .authorize(invocation())
        .expect("authorization")
    {
        CapabilityAuthorizationResult::Authorized(authorized) => authorized,
        CapabilityAuthorizationResult::PendingApproval(_) => panic!("unexpected approval"),
    };
    let Err(error) =
        CredentialBroker::new(EnvCredentialProvider::new()).credential_for_provider(&authorized)
    else {
        panic!("test credential unexpectedly configured");
    };

    assert!(!error.message.contains("OPENWORK_TEST_MISSING_SECRET_7E9A"));
    // `ScopedCredential` intentionally has no Debug, Serialize, or secret getter;
    // this test can only observe the safe provider-facing API at compile time.
    assert_eq!(authorized.descriptor().id, capability_id());
}
