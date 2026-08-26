//! Durable encrypted prompt envelopes over Postgres.
//!
//! The envelope binds a run and its prompt digest, encrypts the prompt with the
//! authenticated cipher, and is consumed exactly once inside the trusted
//! worker boundary immediately before provider stdin delivery. Plaintext never
//! reaches database rows, API responses, logs, audit metadata, process argv, or
//! the Electron renderer.

use crate::{DeliveredPrompt, PromptDelivery, PromptDeliveryError, SecretBytes, block_on, expiry};
use openwork_core::crypto::{EncryptedValue, KeyProvider};
use openwork_execution::{
    ActorId, AuditEventType, RunId, Sha256Digest, UtcTimestamp,
    audit::AuditAppend,
    store::{ExecutionStore, postgres::PostgresExecutionStore},
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Duration;

/// Context binding for every sealed prompt envelope.
const PROMPT_CONTEXT: &str = "prompt-envelope-v1";
/// Key identifier used to seal envelopes.
const PROMPT_KEY_ID: &str = "prompt-envelope-v1";
/// Matches the process-local broker window: 24 hours covers a serial backlog.
const ENVELOPE_TTL: Duration = Duration::from_hours(24);

/// Postgres-backed, single-use envelope broker.
#[derive(Clone)]
pub struct DurablePromptDelivery {
    pool: PgPool,
    keys: Arc<dyn KeyProvider>,
    store: PostgresExecutionStore,
}

impl DurablePromptDelivery {
    /// Wraps a pool and key provider while keeping the store for audit append.
    #[must_use]
    pub const fn new(
        pool: PgPool,
        keys: Arc<dyn KeyProvider>,
        store: PostgresExecutionStore,
    ) -> Self {
        Self { pool, keys, store }
    }
}

impl PromptDelivery for DurablePromptDelivery {
    fn register_prompt(
        &self,
        run_id: &RunId,
        expected_sha256: &Sha256Digest,
        prompt: &[u8],
        now: UtcTimestamp,
    ) -> Result<(), PromptDeliveryError> {
        let actual: [u8; 32] = Sha256::digest(prompt).into();
        let actual = Sha256Digest::parse(crate::hex_digest(actual))
            .expect("SHA-256 output is always a valid lowercase digest");
        if !crate::digest_matches(&actual, expected_sha256) {
            return Err(PromptDeliveryError::DigestMismatch);
        }
        let sealed = EncryptedValue::seal(
            self.keys.as_ref(),
            PROMPT_CONTEXT,
            PROMPT_KEY_ID,
            &envelope_aad(run_id, expected_sha256),
            prompt,
        )
        .map_err(|_| PromptDeliveryError::Unavailable)?;
        let expires_at = expiry(now, ENVELOPE_TTL).ok_or(PromptDeliveryError::Unavailable)?;
        let pool = self.pool.clone();
        let owned_run = run_id.to_owned();
        let owned_digest = expected_sha256.to_owned();
        let consumed = block_on(async move {
            sqlx::query(
                "INSERT INTO prompt_envelopes \
                   (run_id, prompt_sha256, key_id, nonce, ciphertext, created_at, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (run_id) DO NOTHING \
                 RETURNING run_id",
            )
            .bind(uuid_of(&owned_run))
            .bind(owned_digest.as_str())
            .bind(sealed.key_id())
            .bind(sealed.nonce())
            .bind(sealed.ciphertext())
            .bind(sqlx_now(now))
            .bind(sqlx_now(expires_at))
            .fetch_optional(&pool)
            .await
        })
        .map_err(|_| PromptDeliveryError::Unavailable)?;
        if consumed.is_none() {
            return Err(PromptDeliveryError::DuplicateRun);
        }
        self.audit(run_id, AuditEventType::PromptEnvelopeStored);
        Ok(())
    }

    fn take_prompt(
        &self,
        run_id: &RunId,
        expected_sha256: &Sha256Digest,
        now: UtcTimestamp,
    ) -> Result<DeliveredPrompt, PromptDeliveryError> {
        let pool = self.pool.clone();
        let owned_run = run_id.to_owned();
        let row = block_on(async move {
            sqlx::query(
                "SELECT prompt_sha256, key_id, nonce, ciphertext, expires_at \
                 FROM prompt_envelopes WHERE run_id = $1",
            )
            .bind(uuid_of(&owned_run))
            .fetch_optional(&pool)
            .await
        })
        .map_err(|_| PromptDeliveryError::Unavailable)?;
        let Some(row) = row else {
            return Err(PromptDeliveryError::Unavailable);
        };
        let stored_digest: String = row.get("prompt_sha256");
        let key_id: String = row.get("key_id");
        let nonce: Vec<u8> = row.get("nonce");
        let ciphertext: Vec<u8> = row.get("ciphertext");
        let expires_at: time::OffsetDateTime = row.get("expires_at");
        if expires_at <= sqlx_now(now) {
            let pool = self.pool.clone();
            let owned_run = run_id.to_owned();
            let _ = block_on(async move {
                sqlx::query("DELETE FROM prompt_envelopes WHERE run_id = $1")
                    .bind(uuid_of(&owned_run))
                    .execute(&pool)
                    .await
            });
            self.audit(run_id, AuditEventType::PromptEnvelopeExpired);
            return Err(PromptDeliveryError::Unavailable);
        }
        if !crate::digest_matches(
            &Sha256Digest::parse(stored_digest).map_err(|_| PromptDeliveryError::Corrupt)?,
            expected_sha256,
        ) {
            return Err(PromptDeliveryError::DigestMismatch);
        }
        // Single-use consumption is an atomic delete; losing the race means the
        // run is not ours and we fail closed as unavailable.
        let pool = self.pool.clone();
        let owned_run = run_id.to_owned();
        let consumed = block_on(async move {
            sqlx::query("DELETE FROM prompt_envelopes WHERE run_id = $1")
                .bind(uuid_of(&owned_run))
                .execute(&pool)
                .await
        })
        .map_err(|_| PromptDeliveryError::Unavailable)?;
        if consumed.rows_affected() == 0 {
            return Err(PromptDeliveryError::Unavailable);
        }
        let sealed = EncryptedValue::from_parts(&key_id, &nonce, &ciphertext)
            .map_err(|_| PromptDeliveryError::Corrupt)?;
        let plaintext = sealed
            .open(
                self.keys.as_ref(),
                PROMPT_CONTEXT,
                &envelope_aad(run_id, expected_sha256),
            )
            .map_err(|_| {
                self.audit(run_id, AuditEventType::PromptEnvelopeCorrupt);
                PromptDeliveryError::Corrupt
            })?;
        self.audit(run_id, AuditEventType::PromptEnvelopeConsumed);
        Ok(DeliveredPrompt(SecretBytes::from_vec(plaintext)))
    }

    fn purge_expired(&self, now: UtcTimestamp) -> usize {
        let pool = self.pool.clone();
        block_on(async move {
            sqlx::query("DELETE FROM prompt_envelopes WHERE expires_at <= $1")
                .bind(sqlx_now(now))
                .execute(&pool)
                .await
        })
        .map_or(0, |result| {
            usize::try_from(result.rows_affected()).unwrap_or_default()
        })
    }
}

impl DurablePromptDelivery {
    /// Appends one envelope event without degrading prompt delivery on an
    /// audit failure; the persistence boundary remains authoritative.
    fn audit(&self, run_id: &RunId, event_type: AuditEventType) {
        let actor = ActorId::parse("control-plane").expect("control-plane actor is valid");
        let audit = AuditAppend::new(actor, UtcTimestamp::now());
        let _ = self.store.append_audit(run_id, event_type, audit);
    }
}

/// Binds ciphertext to the run and the persisted prompt digest so a sealed
/// envelope cannot be replayed under another run or prompt.
fn envelope_aad(run_id: &RunId, prompt_sha256: &Sha256Digest) -> Vec<u8> {
    let mut aad = b"openwork-prompt-envelope-v1".to_vec();
    aad.push(0);
    aad.extend_from_slice(run_id.to_string().as_bytes());
    aad.push(0);
    aad.extend_from_slice(prompt_sha256.as_str().as_bytes());
    aad
}

fn uuid_of(run_id: &RunId) -> uuid::Uuid {
    uuid::Uuid::parse_str(&run_id.to_string()).expect("run IDs are always valid UUIDv7")
}

fn sqlx_now(timestamp: UtcTimestamp) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(timestamp.unix_timestamp_nanos())
        .expect("prompt envelope timestamps are within range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_aad_binds_run_id_and_digest() {
        let run = RunId::generate();
        let left = Sha256Digest::parse("a".repeat(64)).expect("digest");
        let right = Sha256Digest::parse("b".repeat(64)).expect("digest");
        assert_eq!(envelope_aad(&run, &left), envelope_aad(&run, &left));
        assert_ne!(envelope_aad(&run, &left), envelope_aad(&run, &right));
        assert_ne!(
            envelope_aad(&run, &left),
            envelope_aad(&RunId::generate(), &left)
        );
    }
}
