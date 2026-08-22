use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::{ActionId, ActionRequest, ActorId, RiskLevel, RunId, Sha256Digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppId(String);

impl AppId {
    /// Parses a reverse-domain style app identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty, oversized, or non-portable.
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        parse_id(value.into(), "app identifier is invalid").map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityId(String);

impl CapabilityId {
    /// Parses a stable capability identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty, oversized, or non-portable.
    pub fn parse(value: impl Into<String>) -> Result<Self, OpenWorkError> {
        parse_id(value.into(), "capability identifier is invalid").map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppMetadata {
    pub id: AppId,
    pub name: String,
    pub version: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppProvider {
    Mcp { connector: String },
    Cli { adapter: String },
    Native { adapter: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPermission {
    None,
    Restricted,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPermissions {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPermissions {
    #[serde(default)]
    pub filesystem: FilesystemPermissions,
    pub network: NetworkPermission,
    #[serde(default)]
    pub credentials: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub app_id: AppId,
    pub provider: AppProvider,
    pub description: String,
    pub input_schema: Value,
    pub risk: RiskLevel,
    pub permissions: CapabilityPermissions,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppInstallStatus {
    Available,
    Installed,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailableApp {
    pub metadata: AppMetadata,
    pub provider: AppProvider,
    pub capabilities: Vec<CapabilityDescriptor>,
    pub status: AppInstallStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledApp {
    pub app_id: AppId,
    pub enabled: bool,
    pub credential_ref: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrant {
    pub principal: String,
    pub app_id: AppId,
    pub capabilities: BTreeSet<CapabilityId>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInvocation {
    pub actor: ActorId,
    pub run_id: RunId,
    pub app_id: AppId,
    pub capability: CapabilityId,
    pub resource: String,
    pub parameters: Value,
    pub parameter_hash: Sha256Digest,
    #[serde(skip)]
    action: ActionRequest,
}

impl CapabilityInvocation {
    /// Builds one exact-bound invocation suitable for policy and approval evaluation.
    ///
    /// # Errors
    ///
    /// Returns an error when identifiers, resource, or parameters violate the action contract.
    pub fn new(
        actor: ActorId,
        run_id: RunId,
        app_id: AppId,
        capability: CapabilityId,
        resource: impl Into<String>,
        parameters: Value,
    ) -> Result<Self, OpenWorkError> {
        let action = ActionRequest::new(
            ActionId::generate(),
            run_id.clone(),
            capability.as_str(),
            resource,
            parameters.clone(),
        )?;
        Ok(Self {
            actor,
            run_id,
            app_id,
            capability,
            resource: action.resource.clone(),
            parameters,
            parameter_hash: action.parameter_hash().clone(),
            action,
        })
    }

    #[must_use]
    pub const fn action(&self) -> &ActionRequest {
        &self.action
    }
}

fn parse_id(value: String, message: &'static str) -> Result<String, OpenWorkError> {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(invalid(message));
    };
    if value.len() > 128
        || !first.is_ascii_lowercase()
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        return Err(invalid(message));
    }
    Ok(value)
}

fn invalid(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}
