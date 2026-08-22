//! Provider adapter contracts and sandbox-only CLI execution plans.

use crate::{AppId, CapabilityAuthorization};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::{
    ApprovedMountDirectory, DigestPinnedImageRef, SandboxCommand, SandboxLimits, SandboxRequest,
    SandboxUser,
};
use std::collections::BTreeMap;

/// One pinned CLI App image. It never runs a host executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliAppConfig {
    pub app_id: AppId,
    pub image: DigestPinnedImageRef,
    pub program: String,
    pub fixed_arguments: Vec<String>,
    pub user: SandboxUser,
    pub limits: SandboxLimits,
}

impl CliAppConfig {
    /// Validates a shell-free CLI App entrypoint.
    ///
    /// # Errors
    ///
    /// Returns an error unless the executable is an absolute in-container path
    /// and every fixed argument is bounded and contains no NUL byte.
    pub fn validate(&self) -> Result<(), OpenWorkError> {
        if !self.program.starts_with('/')
            || self.program.len() > 4096
            || self.fixed_arguments.len() > 128
            || self
                .fixed_arguments
                .iter()
                .any(|argument| argument.len() > 4096 || argument.contains('\0'))
        {
            return Err(provider_error("CLI App configuration is invalid"));
        }
        Ok(())
    }
}

/// Converts an authorized invocation into a sandbox request for one pinned App image.
pub struct CliCapabilityProvider {
    apps: BTreeMap<AppId, CliAppConfig>,
}

impl CliCapabilityProvider {
    /// Builds a provider from exact administrator-owned App configurations.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or duplicate configurations.
    pub fn new(configs: Vec<CliAppConfig>) -> Result<Self, OpenWorkError> {
        let mut apps = BTreeMap::new();
        for config in configs {
            config.validate()?;
            if apps.insert(config.app_id.clone(), config).is_some() {
                return Err(provider_error("duplicate CLI App configuration"));
            }
        }
        Ok(Self { apps })
    }

    /// Produces a network-disabled, stdin-driven sandbox request.
    ///
    /// Dynamic model parameters are serialized only to bounded stdin. They
    /// never become executable names, argv fragments, environment variables,
    /// or shell source.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown App or an invalid sandbox contract.
    pub fn prepare(
        &self,
        authorization: &CapabilityAuthorization,
        input_directory: ApprovedMountDirectory,
        output_directory: ApprovedMountDirectory,
    ) -> Result<SandboxRequest, OpenWorkError> {
        let invocation = authorization.invocation();
        if !matches!(
            authorization.descriptor().provider,
            crate::AppProvider::Cli { .. }
        ) {
            return Err(provider_error("capability is not routed to a CLI App"));
        }
        let config = self
            .apps
            .get(&invocation.app_id)
            .ok_or_else(|| provider_error("CLI App is not configured"))?;
        let stdin = serde_json::to_vec(&invocation.parameters)
            .map_err(|_| provider_error("capability parameters could not be encoded"))?;
        let command = SandboxCommand::with_stdin(
            config.program.clone(),
            config.fixed_arguments.clone(),
            BTreeMap::new(),
            stdin,
        )?;
        SandboxRequest::new(
            invocation.run_id.clone(),
            config.image.clone(),
            command,
            config.user,
            input_directory,
            output_directory,
            config.limits,
        )
    }
}

fn provider_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ExecutionFailed, message)
}
