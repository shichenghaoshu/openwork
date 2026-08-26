//! Real-Postgres credential store tests: sealed-at-rest values, metadata-only
//! reads, revoke/rotate/enable lifecycle, and expiry enforcement.
//!
//! Set `OPENWORK_TEST_DATABASE_URL` (loopback, database `openwork_test`) and
//! `OPENWORK_TEST_DATABASE_RESET=1` for a dedicated disposable test database.

use openwork_apps::{SecretStore, SecretStoreEntry};
use openwork_core::crypto::{KeyProvider, StaticKeyProvider};
use openwork_execution::UtcTimestamp;
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::sync::{Mutex, OnceLock};

const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");
static DATABASE_TEST_LOCK: Mutex<()> = Mutex::new(());
static DATABASE_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

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
        "credential tests require a loopback database named openwork_test"
    );
    let runtime = DATABASE_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("test database runtime")
    });
    runtime
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
            sqlx::query("DELETE FROM credentials")
                .execute(&pool)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(pool)
        })
        .ok()
}

fn store(pool: sqlx::PgPool) -> openwork_control_api::secret_store::PostgresSecretStore {
    let keys: std::sync::Arc<dyn KeyProvider> = std::sync::Arc::new(StaticKeyProvider::test_key());
    openwork_control_api::secret_store::PostgresSecretStore::new(pool, keys)
}

fn github_entry(value: &str) -> SecretStoreEntry {
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

fn ciphertexts(pool: &sqlx::PgPool) -> Vec<Vec<u8>> {
    DATABASE_RUNTIME
        .get()
        .expect("database runtime")
        .block_on(async move {
            sqlx::query_scalar::<_, Vec<u8>>("SELECT encrypted_value FROM credentials")
                .fetch_all(pool)
                .await
                .expect("credential ciphertexts")
        })
}

#[test]
fn postgres_secret_store_seals_values_and_exposes_metadata_only() {
    let _gate = DATABASE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(pool) = connect() else {
        return;
    };
    let credential_store = store(pool.clone());
    let stored = credential_store
        .store(github_entry("ghp_do_not_store_in_plaintext"))
        .expect("stored credential");
    assert_eq!(stored.revision, 0);
    assert!(stored.disabled_at.is_none());

    // At rest the value is sealed; plaintext never appears in the row.
    let raw = ciphertexts(&pool);
    assert!(!raw.is_empty());
    let joined = String::from_utf8_lossy(&raw[0]);
    assert!(!joined.contains("ghp_do_not_store_in_plaintext"));

    let resolved = credential_store
        .resolve("github.app")
        .expect("resolve stored credential");
    assert_eq!(
        resolved.environment_variable(),
        "GITHUB_PERSONAL_ACCESS_TOKEN"
    );

    let listed = credential_store.list().expect("list credentials");
    assert_eq!(listed.len(), 1);
    let encoded = serde_json::to_string(&listed).expect("serialize metadata");
    assert!(!encoded.contains("ghp_do_not_store_in_plaintext"));
}

#[test]
fn postgres_secret_store_enforces_revoke_expiry_and_rotation() {
    let _gate = DATABASE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(pool) = connect() else {
        return;
    };
    let credential_store = store(pool.clone());
    credential_store
        .store(github_entry("value-1"))
        .expect("stored credential");

    // Revocation blocks new resolutions while metadata stays observable.
    let revoked = credential_store
        .revoke("github.app")
        .expect("revoke credential");
    assert!(revoked.disabled_at.is_some());
    assert!(credential_store.resolve("github.app").is_err());
    assert_eq!(
        credential_store
            .metadata("github.app")
            .expect("metadata")
            .revision,
        0
    );

    // Rotation replaces the value and advances the revision, and still fails
    // closed unless the credential is re-enabled.
    credential_store
        .rotate("github.app", "value-2")
        .expect("rotate credential");
    assert_eq!(
        credential_store
            .metadata("github.app")
            .expect("metadata")
            .revision,
        1,
        "rotate advances revision"
    );
    assert!(
        credential_store.resolve("github.app").is_err(),
        "revoked rotation stays closed"
    );

    let enabled = credential_store
        .enable("github.app")
        .expect("enable credential");
    assert!(enabled.disabled_at.is_none());
    assert!(credential_store.resolve("github.app").is_ok());

    // Expiry is enforced at resolution time.
    credential_store
        .store(SecretStoreEntry {
            expires_at: Some(UtcTimestamp::parse("2000-01-01T00:00:00Z").expect("past")),
            ..github_entry("value-3")
        })
        .expect("stored expiring credential");
    assert!(
        credential_store.resolve("github.app").is_err(),
        "expired credential fails closed"
    );

    // Missing references fail closed with a redacted error.
    assert!(credential_store.resolve("missing.app").is_err());
    assert!(credential_store.rotate("missing.app", "x").is_err());
    assert!(credential_store.revoke("missing.app").is_err());
}
