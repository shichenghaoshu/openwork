//! Credential resolution stays on the provider side of the capability boundary.
//!
//! This module deliberately does not make credential bytes serializable or
//! printable. A provider receives a [`ScopedCredential`] only after the
//! gateway has produced an authorization for an installed capability.

use crate::gateway::CapabilityAuthorization;
use openwork_core::{ErrorCode, OpenWorkError};
use std::collections::BTreeMap;
use std::process::Command;

/// A credential source owned by the control plane.
pub trait CredentialProvider: Send + Sync {
    /// Resolves one opaque credential reference.
    ///
    /// # Errors
    ///
    /// Returns a redacted configuration error when the reference is unavailable.
    fn resolve(&self, credential_ref: &str) -> Result<ScopedCredential, OpenWorkError>;
}

/// Binds credential lookup to an authorization issued by [`crate::gateway`].
///
/// The broker will not resolve a credential for an uninstalled capability, a
/// capability without a credential permission, or an authorization that was
/// not issued by the gateway.
pub struct CredentialBroker<P> {
    provider: P,
}

impl<P: CredentialProvider> CredentialBroker<P> {
    #[must_use]
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }

    /// Resolves the credential selected by the installed application.
    ///
    /// Providers should inject the returned value directly into their child
    /// process and must not place it in logs, audit records, or API responses.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the authorization does not select a credential
    /// or the configured provider cannot resolve it.
    pub fn credential_for_provider(
        &self,
        authorization: &CapabilityAuthorization,
    ) -> Result<ScopedCredential, OpenWorkError> {
        let credential_ref = authorization
            .credential_ref()
            .ok_or_else(|| credential_error("installed capability has no credential reference"))?;
        self.provider.resolve(credential_ref)
    }
}

/// A provider-scoped secret. It intentionally implements neither `Debug` nor
/// `Serialize` and exposes no accessor for the underlying bytes.
pub struct ScopedCredential {
    environment_variable: String,
    secret: String,
}

impl ScopedCredential {
    pub(crate) fn new(environment_variable: String, secret: String) -> Result<Self, OpenWorkError> {
        if !valid_environment_variable(&environment_variable) || secret.is_empty() {
            return Err(credential_error("credential configuration is invalid"));
        }
        Ok(Self {
            environment_variable,
            secret,
        })
    }

    /// Supplies the secret directly to the provider process environment.
    ///
    /// Callers must treat the resulting command as sensitive: its debug
    /// representation can include environment values on some platforms.
    pub fn inject_into_provider(&self, command: &mut Command) {
        command.env(&self.environment_variable, &self.secret);
    }

    #[must_use]
    pub fn environment_variable(&self) -> &str {
        &self.environment_variable
    }
}

/// Reads a credential from a named environment variable at provider launch.
#[derive(Clone, Copy, Default)]
pub struct EnvCredentialProvider;

impl EnvCredentialProvider {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CredentialProvider for EnvCredentialProvider {
    fn resolve(&self, credential_ref: &str) -> Result<ScopedCredential, OpenWorkError> {
        if !valid_environment_variable(credential_ref) {
            return Err(credential_error("credential reference is invalid"));
        }
        let secret = std::env::var(credential_ref)
            .map_err(|_| credential_error("credential is not configured"))?;
        ScopedCredential::new(credential_ref.to_owned(), secret)
    }
}

/// Pluggable decryption boundary for encrypted configuration at rest.
///
/// `OpenWork` intentionally does not pretend that base64, XOR, or a reversible
/// local transform is encryption. Deployments provide a KMS/keychain-backed
/// implementation of this trait.
pub trait EncryptedConfigDecryptor: Send + Sync {
    /// Decrypts authenticated ciphertext into a provider environment value.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when ciphertext authentication or key access
    /// fails.
    fn decrypt(&self, ciphertext: &[u8]) -> Result<String, OpenWorkError>;
}

/// Credential provider backed by opaque, already-encrypted configuration.
pub struct EncryptedConfigCredentialProvider<D> {
    decryptor: D,
    ciphertext_by_ref: BTreeMap<String, Vec<u8>>,
    environment_by_ref: BTreeMap<String, String>,
}

impl<D: EncryptedConfigDecryptor> EncryptedConfigCredentialProvider<D> {
    #[must_use]
    pub fn new(
        decryptor: D,
        ciphertext_by_ref: BTreeMap<String, Vec<u8>>,
        environment_by_ref: BTreeMap<String, String>,
    ) -> Self {
        Self {
            decryptor,
            ciphertext_by_ref,
            environment_by_ref,
        }
    }
}

impl<D: EncryptedConfigDecryptor> CredentialProvider for EncryptedConfigCredentialProvider<D> {
    fn resolve(&self, credential_ref: &str) -> Result<ScopedCredential, OpenWorkError> {
        let ciphertext = self
            .ciphertext_by_ref
            .get(credential_ref)
            .ok_or_else(|| credential_error("credential is not configured"))?;
        let environment_variable = self
            .environment_by_ref
            .get(credential_ref)
            .ok_or_else(|| credential_error("credential environment is not configured"))?;
        let secret = self.decryptor.decrypt(ciphertext)?;
        ScopedCredential::new(environment_variable.clone(), secret)
    }
}

fn valid_environment_variable(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && value.len() <= 128
}

fn credential_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}
