use crate::{
    AppMetadata, AppProvider, AvailableApp, CapabilityDescriptor, CapabilityId,
    CapabilityPermissions,
};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::RiskLevel;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CAPABILITY_PACK_API_VERSION: &str = "openwork.io/v1";
const MAX_MANIFEST_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPack {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: AppMetadata,
    pub provider: AppProvider,
    pub capabilities: Vec<ManifestCapability>,
    pub permissions: CapabilityPermissions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestCapability {
    pub id: CapabilityId,
    pub description: String,
    pub risk: RiskLevel,
    #[serde(default = "empty_schema")]
    pub input_schema: Value,
}

impl CapabilityPack {
    /// Parses and validates one bounded Capability Pack v1 YAML document.
    ///
    /// # Errors
    ///
    /// Returns a redacted configuration error for malformed or unsafe manifests.
    pub fn from_yaml(input: &str) -> Result<Self, OpenWorkError> {
        if input.len() > MAX_MANIFEST_BYTES {
            return Err(config_error("capability pack exceeds the 256 KiB limit"));
        }
        let pack = yaml_serde::from_str::<Self>(input)
            .map_err(|_| config_error("capability pack YAML is invalid"))?;
        pack.validate()?;
        Ok(pack)
    }

    /// Converts the manifest into the product-facing App model.
    #[must_use]
    pub fn into_available(self) -> AvailableApp {
        let capabilities = self
            .capabilities
            .into_iter()
            .map(|capability| CapabilityDescriptor {
                id: capability.id,
                app_id: self.metadata.id.clone(),
                provider: self.provider.clone(),
                description: capability.description,
                input_schema: capability.input_schema,
                risk: capability.risk,
                permissions: self.permissions.clone(),
            })
            .collect();
        AvailableApp {
            metadata: self.metadata,
            provider: self.provider,
            capabilities,
            status: crate::model::AppInstallStatus::Available,
        }
    }

    fn validate(&self) -> Result<(), OpenWorkError> {
        if self.api_version != CAPABILITY_PACK_API_VERSION
            || self.kind != "CapabilityPack"
            || self.metadata.name.trim().is_empty()
            || self.metadata.name.len() > 128
            || self.metadata.version.trim().is_empty()
            || self.metadata.description.len() > 4096
            || self.capabilities.is_empty()
            || self.capabilities.len() > 128
            || self.capabilities.iter().any(|capability| {
                capability.description.trim().is_empty()
                    || capability.description.len() > 4096
                    || serde_json::to_vec(&capability.input_schema)
                        .map_or(true, |encoded| encoded.len() > 64 * 1024)
            })
        {
            return Err(config_error("capability pack violates v1 invariants"));
        }
        let mut ids = self
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(config_error(
                "capability pack contains duplicate capabilities",
            ));
        }
        if self.permissions.filesystem.read.len() > 16
            || self.permissions.filesystem.write.len() > 16
            || self.permissions.credentials.len() > 16
        {
            return Err(config_error("capability pack permissions are oversized"));
        }
        Ok(())
    }
}

fn empty_schema() -> Value {
    json!({"type": "object", "additionalProperties": false})
}

fn config_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}
