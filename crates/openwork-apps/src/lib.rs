//! Unified `OpenWork` App and capability contracts.

mod manifest;
mod model;
mod registry;

pub mod credential;
pub mod gateway;
pub mod importer;
pub mod provider;

pub use gateway::{
    CapabilityAuthorization, CapabilityAuthorizationResult, CapabilityGateway,
    PendingCapabilityApproval,
};
pub use importer::{CliAnythingCapabilityDraft, CliAnythingImportRequest, import_cli_anything};
pub use manifest::{CAPABILITY_PACK_API_VERSION, CapabilityPack};
pub use model::{
    AppId, AppInstallStatus, AppMetadata, AppProvider, AvailableApp, CapabilityDescriptor,
    CapabilityGrant, CapabilityId, CapabilityInvocation, CapabilityPermissions,
    FilesystemPermissions, InstalledApp, NetworkPermission,
};
pub use provider::{CliAppConfig, CliCapabilityProvider};
pub use registry::AppRegistry;
