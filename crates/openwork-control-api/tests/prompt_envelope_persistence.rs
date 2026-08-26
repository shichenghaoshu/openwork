//! Real-Postgres envelope tests: restart survival, single-use consumption,
//! expiry, corruption fail-closed, and audit cover.
//!
//! Set `OPENWORK_TEST_DATABASE_URL` (loopback, database `openwork_test`) and
//! `OPENWORK_TEST_DATABASE_RESET=1` for a dedicated disposable test database.

use openwork_control_api::{DurablePromptDelivery, PromptDelivery};
use openwork_core::crypto::{KeyProvider, StaticKeyProvider};
use openwork_execution::{
    ActorId, AuditEventType, EXECUTION_SCHEMA_VERSION, Run, RunId, RunStatus, Sha256Digest,
    UtcTimestamp,
    audit::AuditAppend,
    store::{ExecutionStore, postgres::PostgresExecutionStore},
};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");
static DATABASE_TEST_LOCK: Mutex<()> = Mutex::new(());
static DATABASE_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn digest(prompt: &str) -> Sha256Digest {
    let hash: [u8; 32] = Sha256::digest(prompt.as_bytes()).into();
    Sha256Digest::parse(hex_digest(hash)).expect("valid SHA-256 digest")
}

fn hex_digest(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn actor() -> ActorId {
    ActorId::parse("test:envelope").expect("valid actor")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("valid timestamp")
}

fn uuid_of(run_id: &RunId) -> uuid::Uuid {
    uuid::Uuid::parse_str(&run_id.to_string()).expect("run IDs are always valid UUIDv7")
}

fn connect() -> Option<sqlx::PgPool> {
    let Ok(database_url) = env::var("OPENWORK_TEST_DATABASE_URL") else {
        eprintln!("skipping real Postgres test: OPENWORK_TEST_DATABASE_URL is not set");
        return None;
    };
    assert_eq!(
        env::var("OPENWORK_TEST_DATABASE_RESET").as_deref(),
        Ok("1"),
        "set OPENWORK_TEST_DATABASE_RESET=1 for the dedicated disposable test database"
    );
    assert!(
        (database_url.contains("@127.0.0.1:") || database_url.contains("@localhost:"))
            && database_url
                .split_once('?')
                .map_or(database_url.as_str(), |(base, _)| base)
                .ends_with("/openwork_test"),
        "envelope tests require a loopback database named openwork_test"
    );
    let runtime = DATABASE_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test database runtime")
    });
    let pool = runtime
        .block_on(async {
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .connect(&database_url)
                .await
                .map_err(|error| error.to_string())?;
            MIGRATIONS
                .run(&pool)
                .await
                .map_err(|error| error.to_string())?;
            sqlx::query("TRUNCATE TABLE runs CASCADE")
                .execute(&pool)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(pool)
        })
        .expect("connect and migrate test Postgres");
    Some(pool)
}

fn create_run(store: &PostgresExecutionStore, prompt: &str) -> Run {
    store
        .create_run(
            Run {
                schema_version: EXECUTION_SCHEMA_VERSION,
                id: RunId::generate(),
                runtime: "mock".to_owned(),
                workspace: PathBuf::from("/tmp/openwork-envelope-test"),
                status: RunStatus::Queued,
                revision: 0,
                actor_id: actor(),
                prompt_sha256: digest(prompt),
                created_at: timestamp("2026-08-22T00:00:00Z"),
                updated_at: timestamp("2026-08-22T00:00:00Z"),
                started_at: None,
                completed_at: None,
                terminal_reason: None,
            },
            AuditAppend::new(actor(), timestamp("2026-08-22T00:00:00Z")),
        )
        .expect("create run")
}

fn delivery(pool: sqlx::PgPool, store: PostgresExecutionStore) -> DurablePromptDelivery {
    let keys: Arc<dyn KeyProvider> = Arc::new(StaticKeyProvider::test_key());
    DurablePromptDelivery::new(pool, keys, store)
}

fn envelope_count(pool: &sqlx::PgPool) -> i64 {
    DATABASE_RUNTIME
        .get()
        .expect("database runtime")
        .block_on(async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM prompt_envelopes")
                .fetch_one(pool)
                .await
                .expect("envelope count")
        })
}

fn update_envelope(pool: &sqlx::PgPool, run_id: &RunId, statement: &str) {
    DATABASE_RUNTIME
        .get()
        .expect("database runtime")
        .block_on(async move {
            sqlx::query(&format!("{statement} WHERE run_id = $1"))
                .bind(uuid_of(run_id))
                .execute(pool)
                .await
                .expect("envelope update")
        });
}

#[test]
fn queued_prompt_survives_service_restart_and_is_single_use() {
    let _gate = DATABASE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(pool) = connect() else {
        return;
    };
    let store = PostgresExecutionStore::new(pool.clone());
    let run = create_run(&store, "durable prompt across restart");

    // A first process publishes the envelope.
    delivery(pool.clone(), store.clone())
        .register_prompt(
            &run.id,
            &run.prompt_sha256,
            b"durable prompt across restart",
            timestamp("2026-08-22T00:00:01Z"),
        )
        .expect("publish envelope");

    // A second process (fresh broker, same database) recovers the prompt; this
    // simulates a full service restart before worker claim.
    let recovered = delivery(pool.clone(), store.clone())
        .take_prompt(
            &run.id,
            &run.prompt_sha256,
            timestamp("2026-08-22T00:00:02Z"),
        )
        .expect("restart recovery delivers prompt");
    assert_eq!(recovered.as_bytes(), b"durable prompt across restart");
    drop(recovered);

    // Single-use: a second consumption attempt fails closed.
    assert_eq!(
        delivery(pool.clone(), store.clone())
            .take_prompt(
                &run.id,
                &run.prompt_sha256,
                timestamp("2026-08-22T00:00:03Z")
            )
            .map(drop),
        Err(openwork_control_api::PromptDeliveryError::Unavailable)
    );

    // Durable rows never contain plaintext.
    assert_eq!(envelope_count(&pool), 0);

    // Audit records both lifecycle events without prompt material.
    let events = store.audit_events(&run.id).expect("audit chain");
    let stored = events
        .iter()
        .any(|event| event.event_type == AuditEventType::PromptEnvelopeStored);
    let consumed = events
        .iter()
        .any(|event| event.event_type == AuditEventType::PromptEnvelopeConsumed);
    assert!(stored, "expected prompt_envelope_stored audit event");
    assert!(consumed, "expected prompt_envelope_consumed audit event");
    assert!(
        !serde_json::to_string(&events)
            .expect("audit json")
            .contains("durable prompt")
    );
}

#[test]
fn envelope_rejects_tampered_input_and_duplicate_publication() {
    let _gate = DATABASE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(pool) = connect() else {
        return;
    };
    let store = PostgresExecutionStore::new(pool.clone());
    let run = create_run(&store, "tamper target");
    let broker = delivery(pool.clone(), store.clone());

    broker
        .register_prompt(
            &run.id,
            &run.prompt_sha256,
            b"tamper target",
            timestamp("2026-08-22T00:00:01Z"),
        )
        .expect("publish envelope");

    // Duplicate publication is rejected without replacing the envelope.
    assert_eq!(
        broker
            .register_prompt(
                &run.id,
                &run.prompt_sha256,
                b"another",
                timestamp("2026-08-22T00:00:02Z"),
            )
            .map_err(|_| "duplicate"),
        Err("duplicate")
    );

    // Digest substitution is rejected without consuming the envelope.
    let wrong = digest("different prompt");
    assert_eq!(
        broker
            .take_prompt(&run.id, &wrong, timestamp("2026-08-22T00:00:03Z"))
            .map(drop),
        Err(openwork_control_api::PromptDeliveryError::DigestMismatch)
    );

    // Corrupted ciphertext fails closed and leaves a corrupt audit mark.
    update_envelope(
        &pool,
        &run.id,
        "UPDATE prompt_envelopes SET ciphertext = ciphertext || decode('00', 'hex')",
    );
    let outcome = delivery(pool.clone(), store.clone())
        .take_prompt(
            &run.id,
            &run.prompt_sha256,
            timestamp("2026-08-22T00:00:04Z"),
        )
        .map(drop);
    assert_eq!(
        outcome,
        Err(openwork_control_api::PromptDeliveryError::Corrupt)
    );
    let events = store.audit_events(&run.id).expect("audit chain");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == AuditEventType::PromptEnvelopeCorrupt)
    );
    assert!(
        !serde_json::to_string(&events)
            .expect("audit json")
            .contains("tamper target")
    );
}

#[test]
fn expired_envelope_is_removed_and_fails_closed() {
    let _gate = DATABASE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(pool) = connect() else {
        return;
    };
    let store = PostgresExecutionStore::new(pool.clone());
    let run = create_run(&store, "expired target");
    let broker = delivery(pool.clone(), store.clone());
    broker
        .register_prompt(
            &run.id,
            &run.prompt_sha256,
            b"expired target",
            timestamp("2026-08-22T00:00:01Z"),
        )
        .expect("publish envelope");

    // Force the envelope into the past without touching the ciphertext.
    update_envelope(
        &pool,
        &run.id,
        "UPDATE prompt_envelopes SET expires_at = '2000-01-01T00:00:00Z', \
         created_at = '1999-12-31T00:00:00Z'",
    );

    assert_eq!(
        delivery(pool.clone(), store.clone())
            .take_prompt(
                &run.id,
                &run.prompt_sha256,
                timestamp("2026-08-22T00:00:05Z")
            )
            .map(drop),
        Err(openwork_control_api::PromptDeliveryError::Unavailable)
    );
    assert_eq!(envelope_count(&pool), 0, "expired envelopes are removed");
}
