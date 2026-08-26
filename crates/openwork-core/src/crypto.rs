//! Authenticated encryption shared by the credential boundary and the prompt
//! envelope.
//!
//! Every ciphertext is bound to a caller-supplied context and additional
//! authenticated data. `OpenWork` never treats a reversible local transform as
//! encryption; this module is the only ciphertext path in the product.

use crate::{ErrorCode, OpenWorkError};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload},
};
use serde::{Deserialize, Serialize};

/// Length of an encryption key accepted by [`KeyProvider`].
pub const KEY_BYTES: usize = 32;
/// Length of the ChaCha20-Poly1305 nonce.
pub const NONCE_BYTES: usize = 12;

/// Resolves one authenticated encryption key by stable identifier.
///
/// Implementations must never return a key for an unknown identifier; a missing
/// key is a configuration error and callers fail closed.
pub trait KeyProvider: Send + Sync {
    /// Returns the 32-byte key for `key_id`.
    ///
    /// # Errors
    ///
    /// Returns a redacted configuration error when the key is not configured or
    /// cannot be read from the backing store.
    fn key(&self, key_id: &str) -> Result<[u8; KEY_BYTES], OpenWorkError>;
}

/// Reads keys from environment variables named `OPENWORK_CRYPTO_KEY_<ID>`.
///
/// The identifier is uppercased and every `.`/`-` is replaced by `_` to build
/// the variable name; the value must be 64 lowercase or uppercase hexadecimal
/// characters. This is a production-usable backend for small deployments and
/// the default for local demos; KMS implementations replace this provider.
#[derive(Clone, Copy, Default)]
pub struct EnvironmentKeyProvider;

impl EnvironmentKeyProvider {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl KeyProvider for EnvironmentKeyProvider {
    fn key(&self, key_id: &str) -> Result<[u8; KEY_BYTES], OpenWorkError> {
        let variable = env_variable_for(key_id)?;
        let encoded =
            std::env::var(&variable).map_err(|_| key_error("encryption key is not configured"))?;
        decode_key(&encoded).ok_or_else(|| key_error("encryption key format is invalid"))
    }
}

/// Deterministic fixed-key backend for tests and local fixtures.
pub struct StaticKeyProvider([u8; KEY_BYTES]);

impl StaticKeyProvider {
    /// Creates a provider for a caller-supplied key.
    #[must_use]
    pub const fn new(key: [u8; KEY_BYTES]) -> Self {
        Self(key)
    }

    /// Creates a provider for a known zero key for deterministic tests only.
    #[must_use]
    pub const fn test_key() -> Self {
        Self([0xA5; KEY_BYTES])
    }
}

impl KeyProvider for StaticKeyProvider {
    fn key(&self, _key_id: &str) -> Result<[u8; KEY_BYTES], OpenWorkError> {
        Ok(self.0)
    }
}

/// One sealed value: the key identifier used for sealing, the nonce, and the
/// authenticated ciphertext. The plaintext never appears in this type.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedValue {
    key_id: String,
    nonce: [u8; NONCE_BYTES],
    ciphertext: Vec<u8>,
}

impl EncryptedValue {
    /// Seals `plaintext` under `context` (a domain separator) and `aad`
    /// (the binding values, usually identifiers and a digest).
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the key cannot be resolved or a
    /// catastrophic cipher failure occurs.
    pub fn seal(
        provider: &dyn KeyProvider,
        context: &str,
        key_id: &str,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Self, OpenWorkError> {
        validate_key_id(key_id)?;
        let key_bytes = provider.key(key_id)?;
        let key = Key::from_slice(&key_bytes);
        let cipher = ChaCha20Poly1305::new(key);
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &bound_aad(context, aad),
                },
            )
            .map_err(|_| crypto_error("authenticated encryption failed"))?;
        Ok(Self {
            key_id: key_id.to_owned(),
            nonce: nonce.into(),
            ciphertext,
        })
    }

    /// Authenticates and reveals the sealed plaintext.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the key is missing, the ciphertext was
    /// tampered with, or the `context`/`aad` binding does not match.
    pub fn open(
        &self,
        provider: &dyn KeyProvider,
        context: &str,
        aad: &[u8],
    ) -> Result<Vec<u8>, OpenWorkError> {
        let key_bytes = provider.key(&self.key_id)?;
        let key = Key::from_slice(&key_bytes);
        let cipher = ChaCha20Poly1305::new(key);
        let nonce = Nonce::from_slice(&self.nonce);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &self.ciphertext,
                    aad: &bound_aad(context, aad),
                },
            )
            .map_err(|_| crypto_error("authenticated decryption failed"))
    }

    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Rebuilds an [`EncryptedValue`] from its persisted parts.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the nonce length is not exactly
    /// [`NONCE_BYTES`] or the key identifier is invalid.
    pub fn from_parts(
        key_id: &str,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Self, OpenWorkError> {
        validate_key_id(key_id)?;
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| crypto_error("sealed value nonce is invalid"))?;
        Ok(Self {
            key_id: key_id.to_owned(),
            nonce,
            ciphertext: ciphertext.to_vec(),
        })
    }

    #[must_use]
    pub fn nonce(&self) -> &[u8; NONCE_BYTES] {
        &self.nonce
    }

    #[must_use]
    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }
}

/// Fills a caller-owned buffer with randomness for a one-time nonce.
#[must_use]
pub fn random_nonce() -> [u8; NONCE_BYTES] {
    let mut nonce = [0_u8; NONCE_BYTES];
    rand::fill(&mut nonce);
    nonce
}

fn bound_aad(context: &str, aad: &[u8]) -> Vec<u8> {
    let mut bound = b"openwork-aead-v1".to_vec();
    bound.push(0);
    bound.extend_from_slice(context.as_bytes());
    bound.push(0);
    bound.extend_from_slice(aad);
    bound
}

fn validate_key_id(key_id: &str) -> Result<(), OpenWorkError> {
    let mut bytes = key_id.bytes();
    let Some(first) = bytes.next() else {
        return Err(key_error("encryption key identifier is invalid"));
    };
    if !first.is_ascii_lowercase() {
        return Err(key_error("encryption key identifier is invalid"));
    }
    if !bytes
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' || byte == b'_')
        || key_id.len() > 128
    {
        return Err(key_error("encryption key identifier is invalid"));
    }
    Ok(())
}

fn env_variable_for(key_id: &str) -> Result<String, OpenWorkError> {
    validate_key_id(key_id)?;
    let mut variable = String::with_capacity(key_id.len() + 17);
    variable.push_str("OPENWORK_CRYPTO_KEY_");
    for byte in key_id.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => variable.push(char::from(byte)),
            b'.' | b'-' => variable.push('_'),
            _ => return Err(key_error("encryption key identifier is invalid")),
        }
    }
    Ok(variable.to_uppercase())
}

fn decode_key(encoded: &str) -> Option<[u8; KEY_BYTES]> {
    if encoded.len() != KEY_BYTES * 2 {
        return None;
    }
    let mut key = [0_u8; KEY_BYTES];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        key[index] = (high << 4) | low;
    }
    Some(key)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn key_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}

fn crypto_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, OpenWorkError};

    #[test]
    fn seal_and_open_round_trip_binds_aad() {
        let provider = StaticKeyProvider::test_key();
        let value = EncryptedValue::seal(&provider, "test", "env-v1", b"run:a", b"plain secret")
            .expect("seal succeeds");
        assert_eq!(value.key_id(), "env-v1");
        assert!(value.ciphertext.contains(&0x12) || value.ciphertext != b"plain secret");
        assert_eq!(
            value
                .open(&provider, "test", b"run:a")
                .expect("open succeeds"),
            b"plain secret"
        );
        // The same ciphertext never opens under a different binding.
        assert!(value.open(&provider, "test", b"run:b").is_err());
        assert!(value.open(&provider, "other", b"run:a").is_err());
    }

    #[test]
    fn tampered_or_wrong_key_ciphertext_fails_closed() {
        let provider = StaticKeyProvider::test_key();
        let mut value =
            EncryptedValue::seal(&provider, "test", "env-v1", b"aad", b"payload").expect("seal");
        value.ciphertext[0] ^= 0xFF;
        assert!(value.open(&provider, "test", b"aad").is_err());

        let other = StaticKeyProvider::new([0x51; KEY_BYTES]);
        let intact =
            EncryptedValue::seal(&provider, "test", "env-v1", b"aad", b"payload").expect("seal");
        assert!(intact.open(&other, "test", b"aad").is_err());
    }

    #[test]
    fn environment_provider_requires_well_formed_key() {
        let key_id = "demo.v1";
        let variable = env_variable_for(key_id).expect("variable name");
        assert_eq!(variable, "OPENWORK_CRYPTO_KEY_DEMO_V1");
        assert!(env_variable_for("UPPER").is_err());
        // The provider never reads a variable that is not configured in-process.
        assert!(EnvironmentKeyProvider.key(key_id).is_err());
        assert_eq!(decode_key("00ffA5"), None);
        let hex = "00".repeat(KEY_BYTES);
        assert_eq!(decode_key(&hex), Some([0_u8; KEY_BYTES]));
    }

    #[test]
    fn key_id_validation_is_strict_and_errors_are_redacted() {
        assert!(env_variable_for("UPPER").is_err());
        assert!(env_variable_for("").is_err());
        let error: OpenWorkError = OpenWorkError::new(ErrorCode::ConfigInvalid, "TOKEN=hidden");
        assert!(!error.message.contains("hidden"));
    }
}
