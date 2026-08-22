use std::ffi::OsString;

/// Container engine selected for the shared sandbox policy and lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerEngineKind {
    Docker,
    Podman,
}

/// Adapter-level support. Host-dependent capabilities still require a real-host probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilitySupport {
    Supported,
    HostDependent,
}

/// Capabilities implemented by an engine adapter without claiming host verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerEngineCapabilities {
    pub hardened_create: CapabilitySupport,
    pub resource_limits: CapabilitySupport,
    pub stdin_attachment: CapabilitySupport,
}

/// Result of the most recent CLI health probe for this sandbox instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerEngineHealth {
    NotChecked,
    Available,
    Unavailable,
}

/// Observable adapter and health state. `Available` means only that the CLI probe passed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerEngineStatus {
    pub kind: ContainerEngineKind,
    pub capabilities: ContainerEngineCapabilities,
    pub health: ContainerEngineHealth,
}

/// Sealed command adapter for engines accepted by the shared sandbox implementation.
pub trait ContainerEngine: private::Sealed + Copy + Send + Sync {
    fn kind(self) -> ContainerEngineKind;
    fn capabilities(self) -> ContainerEngineCapabilities;
    fn health_arguments(self) -> Vec<OsString>;
    fn unavailable_message(self) -> &'static str;
    fn remove_failure_code(self) -> &'static str;

    fn create_arguments(self) -> Vec<OsString> {
        args(["create", "--read-only"])
    }

    fn start_arguments(self, container_id: &str) -> Vec<OsString> {
        args(["start", container_id])
    }

    fn attach_arguments(self, container_id: &str) -> Vec<OsString> {
        args(["attach", "--sig-proxy=false", container_id])
    }

    fn inspect_arguments(self, container_id: &str) -> Vec<OsString> {
        args(["inspect", "--format", "{{json .State}}", container_id])
    }

    fn logs_arguments(self, container_id: &str) -> Vec<OsString> {
        args(["logs", container_id])
    }

    fn kill_arguments(self, container_id: &str) -> Vec<OsString> {
        args(["kill", container_id])
    }

    fn remove_arguments(self, container_id: &str) -> Vec<OsString> {
        args(["rm", "--force", container_id])
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DockerEngine;

impl private::Sealed for DockerEngine {}

impl ContainerEngine for DockerEngine {
    fn kind(self) -> ContainerEngineKind {
        ContainerEngineKind::Docker
    }

    fn capabilities(self) -> ContainerEngineCapabilities {
        ContainerEngineCapabilities {
            hardened_create: CapabilitySupport::Supported,
            resource_limits: CapabilitySupport::Supported,
            stdin_attachment: CapabilitySupport::Supported,
        }
    }

    fn health_arguments(self) -> Vec<OsString> {
        args(["version", "--format", "{{.Server.Version}}"])
    }

    fn unavailable_message(self) -> &'static str {
        "Docker daemon is unavailable"
    }

    fn remove_failure_code(self) -> &'static str {
        "docker.remove_failed"
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PodmanEngine;

impl private::Sealed for PodmanEngine {}

impl ContainerEngine for PodmanEngine {
    fn kind(self) -> ContainerEngineKind {
        ContainerEngineKind::Podman
    }

    fn capabilities(self) -> ContainerEngineCapabilities {
        ContainerEngineCapabilities {
            hardened_create: CapabilitySupport::Supported,
            resource_limits: CapabilitySupport::HostDependent,
            stdin_attachment: CapabilitySupport::Supported,
        }
    }

    fn health_arguments(self) -> Vec<OsString> {
        args(["info", "--format", "{{.Version.Version}}"])
    }

    fn unavailable_message(self) -> &'static str {
        "Podman engine is unavailable"
    }

    fn remove_failure_code(self) -> &'static str {
        "podman.remove_failed"
    }
}

fn args<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

mod private {
    pub trait Sealed {}
}
