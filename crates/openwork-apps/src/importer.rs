//! Bounded CLI-Anything metadata import into an administrator-reviewed draft.

use crate::{
    AppId, AppMetadata, AppProvider, CapabilityId, CapabilityPack, CapabilityPermissions,
    FilesystemPermissions, NetworkPermission,
};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::RiskLevel;
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_SKILL_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliAnythingCapabilityDraft {
    pub id: CapabilityId,
    pub risk: RiskLevel,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliAnythingImportRequest {
    pub app_id: AppId,
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<CliAnythingCapabilityDraft>,
}

/// Generates a Capability Pack draft. SKILL text may enrich descriptions but
/// can never add permissions, credentials, network, or risk overrides.
///
/// # Errors
///
/// Returns an error for oversized SKILL text or invalid generated manifests.
pub fn import_cli_anything(
    request: CliAnythingImportRequest,
    skill_markdown: &str,
) -> Result<CapabilityPack, OpenWorkError> {
    if skill_markdown.len() > MAX_SKILL_BYTES {
        return Err(import_error("CLI-Anything SKILL exceeds the import limit"));
    }
    let skill_summary = skill_markdown
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("---"))
        .unwrap_or("");
    let capabilities = request
        .capabilities
        .into_iter()
        .map(|capability| crate::manifest::ManifestCapability {
            id: capability.id,
            description: if capability.description.is_empty() {
                skill_summary.chars().take(512).collect()
            } else {
                capability.description
            },
            risk: capability.risk,
            input_schema: json!({"type": "object"}),
        })
        .collect();
    let draft = CapabilityPack {
        api_version: crate::CAPABILITY_PACK_API_VERSION.to_owned(),
        kind: "CapabilityPack".to_owned(),
        metadata: AppMetadata {
            id: request.app_id,
            name: request.name,
            version: request.version,
            description: request.description,
        },
        provider: AppProvider::Cli {
            adapter: "cli-anything".to_owned(),
        },
        capabilities,
        permissions: CapabilityPermissions {
            filesystem: FilesystemPermissions {
                read: vec!["workspace".to_owned()],
                write: vec!["artifacts".to_owned()],
            },
            network: NetworkPermission::None,
            credentials: Vec::new(),
        },
    };
    let encoded = yaml_serde::to_string(&draft)
        .map_err(|_| import_error("CLI-Anything draft could not be encoded"))?;
    CapabilityPack::from_yaml(&encoded)
}

fn import_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}
