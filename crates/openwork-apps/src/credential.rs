//! Credential resolution stays on the provider side of the capability boundary.
//!
//! This module deliberately does not make credential bytes serializable or
//! printable. A provider receives a [`ScopedCredential`] only after the
//! gateway has produced an authorization for an installed capability.

use crate::gateway::CapabilityAuthorization;
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::UtcTimestamp;
use serde::Serialize;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Mutex;

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
    /// Constructs a provider-scoped value for one resolved store entry.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for an unusable environment variable or an
    /// empty secret.
    pub fn new(environment_variable: String, secret: String) -> Result<Self, OpenWorkError> {
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

/// Stable, one-way view of one stored credential. It deliberately contains no
/// secret value: every value owned by a [`SecretStore`] is sealable only by
/// that store's resolver and never participates in serialization, audit, or
/// API responses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialMetadata {
    pub credential_ref: String,
    pub provider: String,
    pub environment_variable: String,
    pub scope: String,
    pub owner: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub disabled_at: Option<String>,
    pub revision: u64,
}

/// Reviewed input for creating or replacing a stored credential.
///
/// The `value` field is intentionally not `Debug`, never serializable, and is
/// consumed by the store immediately.
#[derive(Clone)]
pub struct SecretStoreEntry {
    pub credential_ref: String,
    pub provider: String,
    pub environment_variable: String,
    pub scope: String,
    pub owner: String,
    pub value: String,
    pub expires_at: Option<UtcTimestamp>,
}

/// A credential boundary owned by the control plane.
///
/// Implementations store opaque values under reviewed references and expose
/// only metadata and a provider-scoped, environment-bound value. Standard
/// callers obtain credentials through a reviewed authorization object; direct
/// [`SecretStore`] calls are reserved for connector runtime configuration and
/// administration.
pub trait SecretStore: Send + Sync {
    /// Resolves the active credential `credential_ref` after enforcing
    /// disabled and expiry state. Failed resolution is fail-closed.
    ///
    /// # Errors
    ///
    /// Returns a redacted `ConfigInvalid` error when the credential does not
    /// exist, is disabled, expired, or cannot be decrypted.
    fn resolve(&self, credential_ref: &str) -> Result<ScopedCredential, OpenWorkError>;

    /// Returns metadata for one stored credential without its value.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the reference does not exist.
    fn metadata(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError>;

    /// Lists stored credential metadata without any value.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the backing store is unavailable.
    fn list(&self) -> Result<Vec<CredentialMetadata>, OpenWorkError>;

    /// Stores a reviewed credential entry, creating or replacing its value.
    ///
    /// Replacing an existing entry keeps the original creation time, advances
    /// the one-way `revision`, and restores an enabled state.
    ///
    /// # Errors
    ///
    /// Returns a redacted validation error for an invalid entry.
    fn store(&self, entry: SecretStoreEntry) -> Result<CredentialMetadata, OpenWorkError>;

    /// Replaces only the value of an existing credential, advancing its
    /// revision without touching metadata or the owning App.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the reference does not exist.
    fn rotate(
        &self,
        credential_ref: &str,
        value: &str,
    ) -> Result<CredentialMetadata, OpenWorkError>;

    /// Revokes a credential. The resolved credential becomes unavailable for
    /// new executions while its metadata stays observable.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the reference does not exist.
    fn revoke(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError>;

    /// Re-enables a revoked credential.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the reference does not exist.
    fn enable(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError>;
}

/// Deterministic, process-local [`SecretStore`] backend for tests and local
/// demos. Values live only in memory, are dropped with the store, and never
/// appear in any serialized form.
pub struct MemorySecretStore {
    entries: Mutex<BTreeMap<String, MemoryCredential>>,
}

struct MemoryCredential {
    metadata: CredentialMetadata,
    value: String,
}

impl MemorySecretStore {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Default for MemorySecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for MemorySecretStore {
    fn resolve(&self, credential_ref: &str) -> Result<ScopedCredential, OpenWorkError> {
        validate_credential_ref(credential_ref)?;
        let entries = self
            .entries
            .lock()
            .map_err(|_| credential_error("credential boundary is unavailable"))?;
        let credential = entries
            .get(credential_ref)
            .ok_or_else(|| credential_error("credential is not configured"))?;
        require_active(credential)?;
        ScopedCredential::new(
            credential.metadata.environment_variable.clone(),
            credential.value.clone(),
        )
    }

    fn metadata(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError> {
        validate_credential_ref(credential_ref)?;
        let entries = self
            .entries
            .lock()
            .map_err(|_| credential_error("credential boundary is unavailable"))?;
        entries
            .get(credential_ref)
            .map(|credential| credential.metadata.clone())
            .ok_or_else(|| credential_error("credential is not configured"))
    }

    fn list(&self) -> Result<Vec<CredentialMetadata>, OpenWorkError> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| credential_error("credential boundary is unavailable"))?;
        Ok(entries
            .values()
            .map(|credential| credential.metadata.clone())
            .collect())
    }

    fn store(&self, entry: SecretStoreEntry) -> Result<CredentialMetadata, OpenWorkError> {
        let validated = validate_entry(&entry)?;
        let now = UtcTimestamp::now().to_rfc3339_string();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| credential_error("credential boundary is unavailable"))?;
        let existing = entries.get(&validated.credential_ref);
        let metadata = CredentialMetadata {
            credential_ref: validated.credential_ref.clone(),
            provider: validated.provider,
            environment_variable: validated.environment_variable,
            scope: validated.scope,
            owner: validated.owner,
            created_at: existing.map_or_else(
                || now.clone(),
                |credential| credential.metadata.created_at.clone(),
            ),
            expires_at: validated
                .expires_at
                .as_ref()
                .map(|expiry| expiry.to_rfc3339_string()),
            disabled_at: None,
            revision: existing.map_or(0, |credential| credential.metadata.revision + 1),
        };
        entries.insert(
            validated.credential_ref,
            MemoryCredential {
                metadata: metadata.clone(),
                value: validated.value,
            },
        );
        Ok(metadata)
    }

    fn rotate(
        &self,
        credential_ref: &str,
        value: &str,
    ) -> Result<CredentialMetadata, OpenWorkError> {
        validate_credential_ref(credential_ref)?;
        if value.is_empty() || value.len() > MAX_CREDENTIAL_BYTES {
            return Err(credential_error("credential value is invalid"));
        }
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| credential_error("credential boundary is unavailable"))?;
        let credential = entries
            .get_mut(credential_ref)
            .ok_or_else(|| credential_error("credential is not configured"))?;
        value.clone_into(&mut credential.value);
        credential.metadata.revision += 1;
        Ok(credential.metadata.clone())
    }

    fn revoke(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError> {
        self.set_disabled_at(
            credential_ref,
            Some(UtcTimestamp::now().to_rfc3339_string()),
        )
    }

    fn enable(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError> {
        self.set_disabled_at(credential_ref, None)
    }
}

impl MemorySecretStore {
    fn set_disabled_at(
        &self,
        credential_ref: &str,
        disabled_at: Option<String>,
    ) -> Result<CredentialMetadata, OpenWorkError> {
        validate_credential_ref(credential_ref)?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| credential_error("credential boundary is unavailable"))?;
        let credential = entries
            .get_mut(credential_ref)
            .ok_or_else(|| credential_error("credential is not configured"))?;
        credential.metadata.disabled_at = disabled_at;
        Ok(credential.metadata.clone())
    }
}

fn validate_entry(entry: &SecretStoreEntry) -> Result<SecretStoreEntry, OpenWorkError> {
    validate_credential_ref(&entry.credential_ref)?;
    if !valid_identifier(&entry.provider) || entry.provider.len() > 128 {
        return Err(credential_error("credential provider is invalid"));
    }
    if !valid_environment_variable(&entry.environment_variable) {
        return Err(credential_error(
            "credential environment variable is invalid",
        ));
    }
    if entry.scope.is_empty() || entry.scope.len() > 512 {
        return Err(credential_error("credential scope is invalid"));
    }
    if entry.owner.is_empty() || entry.owner.len() > 128 {
        return Err(credential_error("credential owner is invalid"));
    }
    if entry.value.is_empty() || entry.value.len() > MAX_CREDENTIAL_BYTES {
        return Err(credential_error("credential value is invalid"));
    }
    Ok(SecretStoreEntry {
        credential_ref: entry.credential_ref.clone(),
        provider: entry.provider.clone(),
        environment_variable: entry.environment_variable.clone(),
        scope: entry.scope.clone(),
        owner: entry.owner.clone(),
        value: entry.value.clone(),
        expires_at: entry.expires_at,
    })
}

fn require_active(credential: &MemoryCredential) -> Result<(), OpenWorkError> {
    if credential.metadata.disabled_at.is_some() {
        return Err(credential_error("credential is disabled"));
    }
    if let Some(expires_at) = &credential.metadata.expires_at
        && let Ok(expiry) = UtcTimestamp::parse(expires_at.as_str())
        && expiry < UtcTimestamp::now()
    {
        return Err(credential_error("credential is expired"));
    }
    Ok(())
}

fn validate_credential_ref(credential_ref: &str) -> Result<(), OpenWorkError> {
    if valid_identifier(credential_ref) {
        Ok(())
    } else {
        Err(credential_error("credential reference is invalid"))
    }
}

fn valid_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && value.len() <= 128
}

fn credential_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}

const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: &str) -> SecretStoreEntry {
        SecretStoreEntry {
            credential_ref: "github.app".to_owned(),
            provider: "github".to_owned(),
            environment_variable: "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
            scope: "repos".to_owned(),
            owner: "control-admin".to_owned(),
            value: value.to_owned(),
            expires_at: None,
        }
    }

    #[test]
    fn memory_secret_store_resolves_and_never_exposes_the_value() {
        let store = MemorySecretStore::new();
        let stored = store.store(entry("ghp_test")).expect("stored");
        assert_eq!(stored.revision, 0);
        assert_eq!(stored.created_at, stored.created_at.clone());
        let resolved = store.resolve("github.app").expect("resolved");
        assert_eq!(
            resolved.environment_variable(),
            "GITHUB_PERSONAL_ACCESS_TOKEN"
        );
        // Metadata serialization never contains the value.
        let encoded = serde_json::to_string(&store.list().expect("list")).expect("json");
        assert!(!encoded.contains("ghp_test"));
        assert!(!encoded.contains("secret"));
        assert!(store.resolve("missing.app").is_err());
    }

    #[test]
    fn revoke_prevents_new_resolutions_and_enable_restores() {
        let store = MemorySecretStore::new();
        store.store(entry("v1")).expect("stored");
        let revoked = store.revoke("github.app").expect("revoked");
        assert!(revoked.disabled_at.is_some());
        assert!(store.resolve("github.app").is_err());
        let enabled = store.enable("github.app").expect("enabled");
        assert!(enabled.disabled_at.is_none());
        assert!(store.resolve("github.app").is_ok());
    }

    #[test]
    fn expiry_and_rotation_are_enforced() {
        let store = MemorySecretStore::new();
        let past = UtcTimestamp::parse("2000-01-01T00:00:00Z").expect("past timestamp");
        let mut entry = entry("old");
        entry.expires_at = Some(past);
        store.store(entry).expect("stored");
        assert!(store.resolve("github.app").is_err());
        let rotated = store.rotate("github.app", "new-value").expect("rotated");
        assert_eq!(rotated.revision, 1);
        assert_eq!(rotated.credential_ref, "github.app");
    }

    #[test]
    fn store_replaces_value_but_keeps_creation_time() {
        let store = MemorySecretStore::new();
        let first = store.store(entry("v1")).expect("stored");
        let second = store.store(entry("v2")).expect("replaced");
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(second.revision, 1);
        assert!(second.disabled_at.is_none());
    }

    #[test]
    fn invalid_entries_fail_closed() {
        let store = MemorySecretStore::new();
        let mut invalid = entry("x");
        invalid.credential_ref = "UPPERCASE".to_owned();
        assert!(store.store(invalid).is_err());
        assert!(store.resolve("UPPERCASE").is_err());
        let mut deep = entry("x");
        deep.scope = "x".repeat(600);
        assert!(store.store(deep).is_err());
    }
}
