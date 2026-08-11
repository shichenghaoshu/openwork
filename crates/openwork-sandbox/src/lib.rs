//! Secure process and filesystem primitives for the M1 Docker sandbox.

mod cli;

// These backend-owned filesystem primitives are wired into the lifecycle in the
// next stacked commit. Keeping them private prevents callers granting mounts.
#[allow(dead_code)]
mod filesystem;

pub use cli::{CliOutput, DockerCli, SystemDockerCli};

use openwork_core::{ErrorCode, OpenWorkError};

pub(crate) fn sandbox_error(code: ErrorCode, message: &'static str) -> OpenWorkError {
    OpenWorkError::new(code, message)
}
