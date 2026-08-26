//! Postgres-backed [`SecretStore`] owned by the control plane.
//!
//! Values are sealed with the shared authenticated cipher before they reach
//! Postgres; the column is only ever written through this module. Status and
//! observability queries select metadata columns only, so raw credential
//! material is never part of a normal API, audit, log, or serialization path.

use openwork_apps::{CredentialMetadata, ScopedCredential, SecretStore, SecretStoreEntry};
use openwork_core::crypto::{EncryptedValue, KeyProvider};
use openwork_core::{ErrorCode, OpenWorkError};
use openwork_execution::UtcTimestamp;
use sqlx::{PgPool, Row, postgres::PgRow};
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;

/// Context binding for every sealed credential value.
const CREDENTIAL_CONTEXT: &str = "credential-store-v1";

/// Postgres credential store with values sealed under the provided keyer.
#[derive(Clone)]
pub struct PostgresSecretStore {
    pool: PgPool,
    keys: Arc<dyn KeyProvider>,
}

impl PostgresSecretStore {
    /// Wraps a pool with the key provider used for every stored value.
    #[must_use]
    pub const fn new(pool: PgPool, keys: Arc<dyn KeyProvider>) -> Self {
        Self { pool, keys }
    }
}

impl SecretStore for PostgresSecretStore {
    fn resolve(&self, credential_ref: &str) -> Result<ScopedCredential, OpenWorkError> {
        validate_credential_ref(credential_ref)?;
        let owned_ref = credential_ref.to_owned();
        let pool = self.pool.clone();
        let row = block_on(async move {
            sqlx::query(
                "SELECT environment_variable, encrypted_value, key_id \
                 FROM credentials \
                 WHERE credential_ref = $1 \
                   AND disabled_at IS NULL \
                   AND (expires_at IS NULL OR expires_at > now())",
            )
            .bind(&owned_ref)
            .fetch_optional(&pool)
            .await
        })
        .map_err(|_| store_error("credential boundary is unavailable"))?
        .ok_or_else(|| credential_error("credential is not configured"))?;
        let environment_variable: String = row.get("environment_variable");
        let encrypted_value: Vec<u8> = row.get("encrypted_value");
        let key_id: String = row.get("key_id");
        let token = decode_value(&self.keys, credential_ref, &key_id, &encrypted_value)?;
        ScopedCredential::new(environment_variable, token)
    }

    fn metadata(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError> {
        validate_credential_ref(credential_ref)?;
        let owned_ref = credential_ref.to_owned();
        let pool = self.pool.clone();
        let row = block_on(async move {
            sqlx::query(
                "SELECT credential_ref, provider, environment_variable, scope, owner, \
                        created_at, expires_at, disabled_at, revision \
                 FROM credentials WHERE credential_ref = $1",
            )
            .bind(&owned_ref)
            .fetch_optional(&pool)
            .await
        })
        .map_err(|_| store_error("credential boundary is unavailable"))?
        .ok_or_else(|| credential_error("credential is not configured"))?;
        Ok(metadata_from_row(&row))
    }

    fn list(&self) -> Result<Vec<CredentialMetadata>, OpenWorkError> {
        let pool = self.pool.clone();
        let rows = block_on(async move {
            sqlx::query(
                "SELECT credential_ref, provider, environment_variable, scope, owner, \
                        created_at, expires_at, disabled_at, revision \
                 FROM credentials ORDER BY credential_ref",
            )
            .fetch_all(&pool)
            .await
        })
        .map_err(|_| store_error("credential boundary is unavailable"))?;
        Ok(rows.iter().map(metadata_from_row).collect())
    }

    fn store(&self, entry: SecretStoreEntry) -> Result<CredentialMetadata, OpenWorkError> {
        let validated = validate_entry(&entry)?;
        let sealed = seal_value(
            &self.keys,
            &validated.credential_ref,
            validated.value.as_bytes(),
        )?;
        let now = sqlx_timestamp(UtcTimestamp::now());
        let pool = self.pool.clone();
        let row = block_on(async move {
            sqlx::query(
                "INSERT INTO credentials \
                    (credential_ref, provider, environment_variable, scope, owner, \
                     encrypted_value, key_id, created_at, updated_at, expires_at, disabled_at, revision) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, $9, NULL, 0) \
                 ON CONFLICT (credential_ref) DO UPDATE SET \
                    provider = EXCLUDED.provider, \
                    environment_variable = EXCLUDED.environment_variable, \
                    scope = EXCLUDED.scope, \
                    owner = EXCLUDED.owner, \
                    encrypted_value = EXCLUDED.encrypted_value, \
                    key_id = EXCLUDED.key_id, \
                    updated_at = EXCLUDED.updated_at, \
                    expires_at = EXCLUDED.expires_at, \
                    disabled_at = NULL, \
                    revision = credentials.revision + 1 \
                 RETURNING credential_ref, provider, environment_variable, scope, owner, \
                           created_at, expires_at, disabled_at, revision",
            )
            .bind(&validated.credential_ref)
            .bind(&validated.provider)
            .bind(&validated.environment_variable)
            .bind(&validated.scope)
            .bind(&validated.owner)
            .bind(&sealed.ciphertext)
            .bind(&sealed.key_id)
            .bind(now)
            .bind(validated.expires_at.map(sqlx_timestamp))
            .fetch_one(&pool)
            .await
        })
        .map_err(|_| store_error("credential boundary is unavailable"))?;
        Ok(metadata_from_row(&row))
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
        let sealed = seal_value(&self.keys, credential_ref, value.as_bytes())?;
        let owned_ref = credential_ref.to_owned();
        let pool = self.pool.clone();
        let row = block_on(async move {
            sqlx::query(
                "UPDATE credentials SET \
                    encrypted_value = $2, key_id = $3, updated_at = now(), \
                    revision = revision + 1, disabled_at = NULL \
                 WHERE credential_ref = $1 \
                 RETURNING credential_ref, provider, environment_variable, scope, owner, \
                           created_at, expires_at, disabled_at, revision",
            )
            .bind(&owned_ref)
            .bind(&sealed.ciphertext)
            .bind(&sealed.key_id)
            .fetch_optional(&pool)
            .await
        })
        .map_err(|_| store_error("credential boundary is unavailable"))?
        .ok_or_else(|| credential_error("credential is not configured"))?;
        Ok(metadata_from_row(&row))
    }

    fn revoke(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError> {
        set_disabled_at(
            self.pool.clone(),
            credential_ref,
            Some(UtcTimestamp::now().to_rfc3339_string()),
        )
    }

    fn enable(&self, credential_ref: &str) -> Result<CredentialMetadata, OpenWorkError> {
        set_disabled_at(self.pool.clone(), credential_ref, None)
    }
}

/// The serialized ciphertext plus the key identifier used for sealing.
struct Sealed {
    key_id: String,
    ciphertext: Vec<u8>,
}

fn seal_value(
    keys: &Arc<dyn KeyProvider>,
    credential_ref: &str,
    value: &[u8],
) -> Result<Sealed, OpenWorkError> {
    let encrypted = EncryptedValue::seal(
        keys.as_ref(),
        CREDENTIAL_CONTEXT,
        "credential-store-v1",
        credential_ref.as_bytes(),
        value,
    )?;
    let mut ciphertext = Vec::with_capacity(12 + encrypted.ciphertext().len());
    ciphertext.extend_from_slice(encrypted.nonce());
    ciphertext.extend_from_slice(encrypted.ciphertext());
    Ok(Sealed {
        key_id: encrypted.key_id().to_owned(),
        ciphertext,
    })
}

fn decode_value(
    keys: &Arc<dyn KeyProvider>,
    credential_ref: &str,
    key_id: &str,
    ciphertext: &[u8],
) -> Result<String, OpenWorkError> {
    if ciphertext.len() < 12 {
        return Err(credential_error("credential is not configured"));
    }
    let (nonce, payload) = ciphertext.split_at(12);
    let encrypted = EncryptedValue::from_parts(key_id, nonce, payload)?;
    let value = encrypted
        .open(keys.as_ref(), CREDENTIAL_CONTEXT, credential_ref.as_bytes())
        .map_err(|_| credential_error("credential is not configured"))?;
    String::from_utf8(value).map_err(|_| credential_error("credential is not configured"))
}

fn metadata_from_row(row: &PgRow) -> CredentialMetadata {
    CredentialMetadata {
        credential_ref: row.get("credential_ref"),
        provider: row.get("provider"),
        environment_variable: row.get("environment_variable"),
        scope: row.get("scope"),
        owner: row.get("owner"),
        created_at: row
            .get::<time::OffsetDateTime, _>("created_at")
            .format(&Rfc3339)
            .expect("timestamptz always formats"),
        expires_at: row
            .get::<Option<time::OffsetDateTime>, _>("expires_at")
            .map(|value| value.format(&Rfc3339).expect("timestamptz always formats")),
        disabled_at: row
            .get::<Option<time::OffsetDateTime>, _>("disabled_at")
            .map(|value| value.format(&Rfc3339).expect("timestamptz always formats")),
        revision: row
            .get::<i64, _>("revision")
            .try_into()
            .expect("stored revision is non-negative"),
    }
}

fn set_disabled_at(
    pool: PgPool,
    credential_ref: &str,
    disabled_at: Option<String>,
) -> Result<CredentialMetadata, OpenWorkError> {
    validate_credential_ref(credential_ref)?;
    let owned_ref = credential_ref.to_owned();
    let row = block_on(async move {
        sqlx::query(
            "UPDATE credentials SET disabled_at = $2, updated_at = now() \
             WHERE credential_ref = $1 \
             RETURNING credential_ref, provider, environment_variable, scope, owner, \
                       created_at, expires_at, disabled_at, revision",
        )
        .bind(&owned_ref)
        .bind(disabled_at)
        .fetch_optional(&pool)
        .await
    })
    .map_err(|_| store_error("credential boundary is unavailable"))?
    .ok_or_else(|| credential_error("credential is not configured"))?;
    Ok(metadata_from_row(&row))
}

fn sqlx_timestamp(timestamp: UtcTimestamp) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(timestamp.unix_timestamp_nanos())
        .expect("control-plane timestamps are within range")
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

fn validate_credential_ref(credential_ref: &str) -> Result<(), OpenWorkError> {
    if credential_ref
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && credential_ref.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && credential_ref.len() <= 128
    {
        Ok(())
    } else {
        Err(credential_error("credential reference is invalid"))
    }
}

fn validate_entry(entry: &SecretStoreEntry) -> Result<SecretStoreEntry, OpenWorkError> {
    validate_credential_ref(&entry.credential_ref)?;
    if entry.provider.is_empty() || entry.provider.len() > 128 {
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

fn credential_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}

fn store_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::Internal, message)
}

const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

/// Runs an owned database future from synchronous or Tokio-backed callers.
fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        Ok(_) => std::thread::spawn(move || database_runtime().block_on(future))
            .join()
            .expect("database runtime thread panicked"),
        Err(_) => database_runtime().block_on(future),
    }
}

fn database_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("database runtime must initialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_value_round_trips_through_the_boundary_helpers() {
        let keys: Arc<dyn KeyProvider> =
            Arc::new(openwork_core::crypto::StaticKeyProvider::test_key());
        let sealed = seal_value(&keys, "github.app", b"ghp_secret").expect("sealed");
        assert_eq!(sealed.key_id, "credential-store-v1");
        assert_eq!(
            decode_value(&keys, "github.app", &sealed.key_id, &sealed.ciphertext).expect("decoded"),
            "ghp_secret"
        );
        // A misbound credential reference never decodes.
        assert!(decode_value(&keys, "lark.app", &sealed.key_id, &sealed.ciphertext).is_err());
    }

    #[test]
    fn corrupted_or_truncated_ciphertext_fails_closed() {
        let keys: Arc<dyn KeyProvider> =
            Arc::new(openwork_core::crypto::StaticKeyProvider::test_key());
        let sealed = seal_value(&keys, "github.app", b"value").expect("sealed");
        let mut truncated = sealed.ciphertext.clone();
        truncated.truncate(11);
        assert!(decode_value(&keys, "github.app", &sealed.key_id, &truncated).is_err());
        let mut corrupted = sealed.ciphertext.clone();
        corrupted[12] ^= 0xFF;
        assert!(decode_value(&keys, "github.app", &sealed.key_id, &corrupted).is_err());
    }

    #[test]
    fn standalone_validation_rejects_unsafe_input() {
        assert!(validate_credential_ref("UPPER").is_err());
        assert!(validate_credential_ref("").is_err());
        let key: Arc<dyn KeyProvider> =
            Arc::new(openwork_core::crypto::StaticKeyProvider::test_key());
        let value = "64K+1".to_owned();
        let mut entry = SecretStoreEntry {
            credential_ref: "app.v1".to_owned(),
            provider: "github".to_owned(),
            environment_variable: "TOKEN".to_owned(),
            scope: "ro".to_owned(),
            owner: "admin".to_owned(),
            value,
            expires_at: None,
        };
        entry.value = "x".repeat(MAX_CREDENTIAL_BYTES + 1);
        assert!(validate_entry(&entry).is_err());
        assert!(
            validate_entry(&SecretStoreEntry {
                credential_ref: "app.v1".to_owned(),
                provider: "github".to_owned(),
                environment_variable: "1bad".to_owned(),
                scope: "ro".to_owned(),
                owner: "admin".to_owned(),
                value: "x".to_owned(),
                expires_at: None,
            })
            .is_err()
        );
        drop(key);
    }
}
