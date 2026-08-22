//! Capability authorization before routing work to a provider.

use crate::model::AppInstallStatus;
use crate::{
    AppId, AvailableApp, CapabilityDescriptor, CapabilityGrant, CapabilityInvocation, InstalledApp,
};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::{ActionPolicy, PolicyDecision, PolicyEvaluation};
use std::collections::BTreeMap;

/// Result of checking an invocation. This type never executes a provider.
pub enum CapabilityAuthorizationResult {
    /// The exact invocation needs a durable approval before it can be executed.
    PendingApproval(Box<PendingCapabilityApproval>),
    /// The gateway has authorized this exact invocation for provider routing.
    Authorized(Box<CapabilityAuthorization>),
}

/// Exact action and policy evaluation to hand to the approval service.
///
/// Its invocation is intentionally private, preventing a caller from swapping
/// parameters after policy evaluation and before approval creation.
pub struct PendingCapabilityApproval {
    invocation: CapabilityInvocation,
    evaluation: PolicyEvaluation,
}

impl PendingCapabilityApproval {
    #[must_use]
    pub const fn invocation(&self) -> &CapabilityInvocation {
        &self.invocation
    }

    #[must_use]
    pub const fn evaluation(&self) -> &PolicyEvaluation {
        &self.evaluation
    }
}

/// Opaque, exact-bound capability authorization.
///
/// It has no public constructor, serialization implementation, or mutation
/// API. Providers can inspect only the invocation and descriptor needed for
/// execution, while the credential reference remains broker-private.
pub struct CapabilityAuthorization {
    invocation: CapabilityInvocation,
    descriptor: CapabilityDescriptor,
    credential_ref: Option<String>,
    evaluation: PolicyEvaluation,
}

impl CapabilityAuthorization {
    #[must_use]
    pub const fn invocation(&self) -> &CapabilityInvocation {
        &self.invocation
    }

    #[must_use]
    pub const fn descriptor(&self) -> &CapabilityDescriptor {
        &self.descriptor
    }

    #[must_use]
    pub const fn evaluation(&self) -> &PolicyEvaluation {
        &self.evaluation
    }

    pub(crate) fn credential_ref(&self) -> Option<&str> {
        self.credential_ref.as_deref()
    }
}

/// In-memory view of installed packs, grants, and policy for one control-plane
/// process. Durable installation/grant persistence belongs outside this
/// boundary; callers replace this gateway when that state changes.
pub struct CapabilityGateway<P> {
    policy: P,
    apps: BTreeMap<AppId, AvailableApp>,
    installed: BTreeMap<AppId, InstalledApp>,
    grants: Vec<CapabilityGrant>,
}

impl<P: ActionPolicy> CapabilityGateway<P> {
    #[must_use]
    pub fn new(
        policy: P,
        available: impl IntoIterator<Item = AvailableApp>,
        installed: impl IntoIterator<Item = InstalledApp>,
        grants: impl IntoIterator<Item = CapabilityGrant>,
    ) -> Self {
        Self {
            policy,
            apps: available
                .into_iter()
                .map(|app| (app.metadata.id.clone(), app))
                .collect(),
            installed: installed
                .into_iter()
                .map(|app| (app.app_id.clone(), app))
                .collect(),
            grants: grants.into_iter().collect(),
        }
    }

    /// Checks registration, enablement, the actor grant, exact descriptor
    /// binding, and policy. It never treats an approval-required decision as
    /// executable authorization.
    ///
    /// # Errors
    ///
    /// Returns `PolicyDenied` for an unknown, disabled, ungranted, or malformed
    /// capability invocation, and for an explicit policy denial.
    pub fn authorize(
        &self,
        invocation: CapabilityInvocation,
    ) -> Result<CapabilityAuthorizationResult, OpenWorkError> {
        let descriptor = self.find_exact_descriptor(&invocation)?;
        let installed = self.require_enabled_installation(&invocation.app_id)?;
        self.require_grant(&invocation)?;

        let evaluation = self.policy.evaluate(invocation.action());
        match evaluation.decision {
            PolicyDecision::Deny => Err(policy_error("capability action denied by policy")),
            PolicyDecision::RequireApproval => Ok(CapabilityAuthorizationResult::PendingApproval(
                Box::new(PendingCapabilityApproval {
                    invocation,
                    evaluation,
                }),
            )),
            PolicyDecision::Allow => {
                let credential_ref = credential_ref_for(&descriptor, installed);
                Ok(CapabilityAuthorizationResult::Authorized(Box::new(
                    CapabilityAuthorization {
                        invocation,
                        descriptor,
                        credential_ref,
                        evaluation,
                    },
                )))
            }
        }
    }

    fn find_exact_descriptor(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<CapabilityDescriptor, OpenWorkError> {
        if !invocation.action().parameters_match_hash()
            || invocation.action().run_id != invocation.run_id
            || invocation.action().action != invocation.capability.as_str()
            || invocation.action().resource != invocation.resource
            || invocation.action().parameters != invocation.parameters
            || invocation.action().parameter_hash() != &invocation.parameter_hash
        {
            return Err(policy_error("capability invocation binding is invalid"));
        }
        let app = self
            .apps
            .get(&invocation.app_id)
            .ok_or_else(|| policy_error("capability application is not registered"))?;
        if app.status == AppInstallStatus::Disabled {
            return Err(policy_error("capability application is disabled"));
        }
        app.capabilities
            .iter()
            .find(|descriptor| {
                descriptor.app_id == invocation.app_id && descriptor.id == invocation.capability
            })
            .cloned()
            .ok_or_else(|| policy_error("capability descriptor does not match registered app"))
    }

    fn require_enabled_installation(&self, app_id: &AppId) -> Result<&InstalledApp, OpenWorkError> {
        let installed = self
            .installed
            .get(app_id)
            .ok_or_else(|| policy_error("capability application is not installed"))?;
        if !installed.enabled {
            return Err(policy_error("capability application is disabled"));
        }
        Ok(installed)
    }

    fn require_grant(&self, invocation: &CapabilityInvocation) -> Result<(), OpenWorkError> {
        if self.grants.iter().any(|grant| {
            grant.principal == invocation.actor.as_str()
                && grant.app_id == invocation.app_id
                && grant.capabilities.contains(&invocation.capability)
        }) {
            Ok(())
        } else {
            Err(policy_error("principal lacks capability grant"))
        }
    }
}

fn credential_ref_for(
    descriptor: &CapabilityDescriptor,
    installed: &InstalledApp,
) -> Option<String> {
    (!descriptor.permissions.credentials.is_empty())
        .then(|| installed.credential_ref.clone())
        .flatten()
}

fn policy_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::PolicyDenied, message)
}
