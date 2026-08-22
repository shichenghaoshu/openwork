//! Authenticated, Postgres-backed M1 control-plane HTTP API.

mod connector_runtime;
mod worker_runtime;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use openwork_core::ErrorCode;
use openwork_execution::{
    ActorId, ApprovalDecision, ApprovalId, ApprovalRequest, DEFAULT_MAX_ARTIFACT_BYTES, Run, RunId,
    Sha256Digest, UtcTimestamp,
    approval::ApprovalRepository,
    artifact::ArtifactScanner,
    orchestrator::ExecutionOrchestrator,
    store::{CancelRequest, ExecutionStore, RunQueueRepository, postgres::PostgresExecutionStore},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering, compiler_fence},
    },
    time::Duration,
};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

use connector_runtime::{ConnectorRegistry, LookupError};

const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");
const PROMPT_DELIVERY_CAPACITY: usize = 128;
const PROMPT_DELIVERY_MAX_BYTES: usize = 256 * 1024;
// Capacity is 128 and one worker run is bounded at ten minutes. A 24-hour
// process-local window covers the worst serial backlog plus recovery overhead.
const PROMPT_DELIVERY_TTL: Duration = Duration::from_hours(24);

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub database_url: String,
    pub api_token: String,
    pub actor_id: String,
    pub workspace_root: PathBuf,
    worker: Option<worker_runtime::WorkerRuntimeConfig>,
    connectors: connector_runtime::ConnectorRuntimeConfig,
}

impl Config {
    /// Loads server configuration. The default listener is loopback-only.
    ///
    /// # Errors
    ///
    /// Returns an error when required settings are missing or invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = env::var("OPENWORK_CONTROL_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()
            .map_err(|_| ConfigError("OPENWORK_CONTROL_BIND must be a socket address"))?;
        let database_url = required_env("OPENWORK_DATABASE_URL")?;
        let api_token = required_env("OPENWORK_API_TOKEN")?;
        if api_token.len() < 32 {
            return Err(ConfigError(
                "OPENWORK_API_TOKEN must contain at least 32 bytes",
            ));
        }
        let actor_id =
            env::var("OPENWORK_API_ACTOR").unwrap_or_else(|_| "control-admin".to_owned());
        ActorId::parse(actor_id.clone())
            .map_err(|_| ConfigError("OPENWORK_API_ACTOR is invalid"))?;
        let workspace_root = env::var_os("OPENWORK_WORKSPACE_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or(ConfigError("OPENWORK_WORKSPACE_ROOT is required"))?;
        let workspace_root = canonical_workspace_root(&workspace_root)?;
        let worker = worker_runtime::WorkerRuntimeConfig::from_env()?;
        let connectors = connector_runtime::ConnectorRuntimeConfig::from_env()?;
        Ok(Self {
            bind,
            database_url,
            api_token,
            actor_id,
            workspace_root,
            worker,
            connectors,
        })
    }
}

fn required_env(name: &'static str) -> Result<String, ConfigError> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(ConfigError(name))
}

fn canonical_workspace_root(path: &FsPath) -> Result<PathBuf, ConfigError> {
    let root = fs::canonicalize(path)
        .map_err(|_| ConfigError("OPENWORK_WORKSPACE_ROOT must be an existing directory"))?;
    if !fs::metadata(&root).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(ConfigError(
            "OPENWORK_WORKSPACE_ROOT must be an existing directory",
        ));
    }
    Ok(root)
}

#[derive(Debug)]
pub struct ConfigError(&'static str);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    store: PostgresExecutionStore,
    token_hash: [u8; 32],
    actor_id: String,
    workspace_root: PathBuf,
    cancellation: Arc<dyn CancellationCoordinator>,
    prompts: Arc<SharedPromptBoundary>,
    worker_runtimes: Arc<BTreeSet<String>>,
    worker_ready: Arc<AtomicBool>,
    connectors: Arc<ConnectorRegistry>,
}

struct RouterDependencies {
    cancellation: Arc<dyn CancellationCoordinator>,
    prompts: Arc<SharedPromptBoundary>,
    worker_runtimes: BTreeSet<String>,
    worker_ready: Arc<AtomicBool>,
    connectors: Arc<ConnectorRegistry>,
}

/// One-time, fail-closed prompt retrieval boundary for a durable worker.
///
/// This interface intentionally exposes no listing or retry operation. A
/// process restart creates an empty implementation, so a worker must fail
/// closed if its durable run has no matching prompt delivery entry.
pub trait PromptDelivery: Send + Sync {
    /// Takes one prompt only when its run ID and persisted digest both match.
    ///
    /// # Errors
    ///
    /// Returns an availability error for an expired, consumed, unknown, or
    /// restarted entry; or a binding error when the digest does not match.
    fn take_prompt(
        &self,
        run_id: &RunId,
        expected_sha256: &Sha256Digest,
        now: UtcTimestamp,
    ) -> Result<DeliveredPrompt, PromptDeliveryError>;

    /// Removes expired entries during worker startup/recovery.
    ///
    /// This cannot restore prompts after a process restart; that condition is
    /// intentionally represented by `take_prompt` returning unavailable.
    fn purge_expired(&self, now: UtcTimestamp) -> usize;
}

/// Safe-to-log outcome for prompt delivery; it never contains prompt bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptDeliveryError {
    CapacityExceeded,
    DuplicateRun,
    DigestMismatch,
    Unavailable,
}

/// A consumed prompt whose backing bytes are cleared when dropped.
pub struct DeliveredPrompt(SecretBytes);

impl DeliveredPrompt {
    /// Returns the prompt bytes for immediate worker use.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0.0
    }

    /// Consumes the delivery guard without copying the UTF-8 prompt bytes.
    ///
    /// The returned `String` becomes the worker's responsibility to keep out
    /// of logs, argv, environment, and durable storage.
    ///
    /// # Panics
    ///
    /// Panics only if an internal invariant is violated and bytes that did not
    /// originate from the validated HTTP `String` reach this guard.
    #[must_use]
    pub fn into_string(self) -> String {
        String::from_utf8(self.0.into_vec()).expect("prompt was created from valid UTF-8")
    }
}

struct SecretBytes(Vec<u8>);

impl SecretBytes {
    fn from_string(value: String) -> Self {
        Self(value.into_bytes())
    }

    fn as_str(&self) -> &str {
        // Values originate from a Rust `String`; this check avoids unsafe code
        // while preserving the workspace-wide unsafe-code prohibition.
        std::str::from_utf8(&self.0).expect("prompt was created from valid UTF-8")
    }

    fn into_vec(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
        compiler_fence(Ordering::SeqCst);
    }
}

struct StoredPrompt {
    digest: Sha256Digest,
    expires_at: UtcTimestamp,
    bytes: SecretBytes,
}

/// Bounded process-local prompt handoff. It is deliberately not durable secret
/// storage: restart loss is fail-closed, and no prompt is ever written to disk.
pub struct InMemoryPromptDelivery {
    capacity: usize,
    max_bytes: usize,
    ttl: Duration,
    entries: Mutex<BTreeMap<RunId, StoredPrompt>>,
}

/// Serializes durable run creation plus prompt publication with worker claims.
///
/// A worker must use [`SharedPromptBoundary::with_worker_claim`] for its
/// claim-and-take closure. That closure cannot enter while Control API holds
/// the same gate from before durable creation until the raw prompt is present
/// in the broker, eliminating the persist-to-publish observation gap without
/// sleeps or retries. This remains process-local and does not survive restart.
pub struct SharedPromptBoundary {
    creation_gate: Mutex<()>,
    delivery: InMemoryPromptDelivery,
}

impl SharedPromptBoundary {
    /// Builds a shared creation gate around a bounded prompt broker.
    #[must_use]
    pub fn new(delivery: InMemoryPromptDelivery) -> Self {
        Self {
            creation_gate: Mutex::new(()),
            delivery,
        }
    }

    /// Creates the production shared gate and bounded prompt broker.
    #[must_use]
    pub fn production() -> Self {
        Self::new(InMemoryPromptDelivery::production())
    }

    /// Executes a worker claim-and-take closure only after publication is complete.
    ///
    /// A poisoned creation gate is treated as unavailable, so callers fail
    /// closed instead of claiming a run while prompt publication is uncertain.
    ///
    /// # Errors
    ///
    /// Returns `Unavailable` when the creation gate is poisoned.
    pub fn with_worker_claim<T>(
        &self,
        claim_and_take: impl FnOnce(&dyn PromptDelivery) -> T,
    ) -> Result<T, PromptDeliveryError> {
        let _gate = self
            .creation_gate
            .lock()
            .map_err(|_| PromptDeliveryError::Unavailable)?;
        Ok(claim_and_take(&self.delivery))
    }

    fn with_creation_gate<T>(
        &self,
        publish: impl FnOnce(&InMemoryPromptDelivery) -> T,
    ) -> Result<T, PromptDeliveryError> {
        let _gate = self
            .creation_gate
            .lock()
            .map_err(|_| PromptDeliveryError::Unavailable)?;
        Ok(publish(&self.delivery))
    }
}

impl InMemoryPromptDelivery {
    /// Creates the production M1 prompt handoff with bounded memory and TTL.
    #[must_use]
    pub fn production() -> Self {
        Self::new(
            PROMPT_DELIVERY_CAPACITY,
            PROMPT_DELIVERY_MAX_BYTES,
            PROMPT_DELIVERY_TTL,
        )
    }

    /// Creates a bounded handoff. This is public for worker injection and tests.
    #[must_use]
    pub fn new(capacity: usize, max_bytes: usize, ttl: Duration) -> Self {
        Self {
            capacity,
            max_bytes,
            ttl,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Registers the raw prompt only after its run has been durably created.
    ///
    /// # Errors
    ///
    /// Returns a safe-to-log error without retaining prompt bytes on failure.
    fn register_prompt(
        &self,
        run_id: RunId,
        expected_sha256: Sha256Digest,
        bytes: SecretBytes,
        now: UtcTimestamp,
    ) -> Result<(), PromptDeliveryError> {
        if self.capacity == 0 || bytes.0.len() > self.max_bytes {
            return Err(PromptDeliveryError::CapacityExceeded);
        }
        let actual: [u8; 32] = Sha256::digest(&bytes.0).into();
        let actual = Sha256Digest::parse(hex_digest(actual))
            .expect("SHA-256 output is always a valid lowercase digest");
        if !digest_matches(&actual, &expected_sha256) {
            return Err(PromptDeliveryError::DigestMismatch);
        }
        let expires_at = expiry(now, self.ttl).ok_or(PromptDeliveryError::Unavailable)?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| PromptDeliveryError::Unavailable)?;
        purge_expired_locked(&mut entries, now);
        if entries.contains_key(&run_id) {
            return Err(PromptDeliveryError::DuplicateRun);
        }
        if entries.len() >= self.capacity {
            return Err(PromptDeliveryError::CapacityExceeded);
        }
        entries.insert(
            run_id,
            StoredPrompt {
                digest: expected_sha256,
                expires_at,
                bytes,
            },
        );
        Ok(())
    }
}

impl PromptDelivery for InMemoryPromptDelivery {
    fn take_prompt(
        &self,
        run_id: &RunId,
        expected_sha256: &Sha256Digest,
        now: UtcTimestamp,
    ) -> Result<DeliveredPrompt, PromptDeliveryError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| PromptDeliveryError::Unavailable)?;
        purge_expired_locked(&mut entries, now);
        let matches = entries
            .get(run_id)
            .is_some_and(|entry| digest_matches(&entry.digest, expected_sha256));
        if !matches {
            return if entries.contains_key(run_id) {
                Err(PromptDeliveryError::DigestMismatch)
            } else {
                Err(PromptDeliveryError::Unavailable)
            };
        }
        let entry = entries
            .remove(run_id)
            .expect("matching prompt entry still exists while mutex is held");
        Ok(DeliveredPrompt(entry.bytes))
    }

    fn purge_expired(&self, now: UtcTimestamp) -> usize {
        self.entries.lock().map_or(0, |mut entries| {
            let before = entries.len();
            purge_expired_locked(&mut entries, now);
            before - entries.len()
        })
    }
}

fn purge_expired_locked(entries: &mut BTreeMap<RunId, StoredPrompt>, now: UtcTimestamp) {
    entries.retain(|_, entry| entry.expires_at > now);
}

fn expiry(now: UtcTimestamp, ttl: Duration) -> Option<UtcTimestamp> {
    let nanos = i128::try_from(ttl.as_nanos()).ok()?;
    let timestamp = now.unix_timestamp_nanos().checked_add(nanos)?;
    UtcTimestamp::parse(format_timestamp_nanos(timestamp)).ok()
}

fn format_timestamp_nanos(timestamp: i128) -> String {
    // `UtcTimestamp` owns the canonical parser. This helper is only used for
    // bounded, positive TTL arithmetic and remains independent of storage.
    time::OffsetDateTime::from_unix_timestamp_nanos(timestamp)
        .expect("valid timestamp arithmetic")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("UTC timestamps always format")
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

fn digest_matches(left: &Sha256Digest, right: &Sha256Digest) -> bool {
    bool::from(left.as_str().as_bytes().ct_eq(right.as_str().as_bytes()))
}

/// Boundary between HTTP cancellation intent and the durable execution worker.
///
/// The adapter persists an active-run cancellation request but only a worker
/// with a current lease may later confirm the terminal state after runtime and
/// sandbox cleanup evidence has been verified.
trait CancellationCoordinator: Send + Sync {
    fn request_cancel(
        &self,
        run_id: &RunId,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<CancellationOutcome, CancellationError>;
}

#[derive(Clone, Copy)]
enum CancellationOutcome {
    Cancelled,
    Accepted,
    AlreadyCancelled,
    TerminalConflict,
}

#[derive(Clone, Copy)]
enum CancellationError {
    NotFound,
    Conflict,
    Internal,
}

#[derive(Clone)]
struct DurableCancellationCoordinator {
    store: PostgresExecutionStore,
}

impl CancellationCoordinator for DurableCancellationCoordinator {
    fn request_cancel(
        &self,
        run_id: &RunId,
        actor: ActorId,
        now: UtcTimestamp,
    ) -> Result<CancellationOutcome, CancellationError> {
        self.store
            .get_run(run_id)
            .map_err(|_| CancellationError::Internal)?
            .ok_or(CancellationError::NotFound)?;
        match self
            .store
            .request_cancel(run_id, actor, now)
            .map_err(|error| match error.code {
                ErrorCode::InvalidStateTransition => CancellationError::Conflict,
                _ => CancellationError::Internal,
            })? {
            CancelRequest::Cancelled => Ok(CancellationOutcome::Cancelled),
            CancelRequest::Requested => Ok(CancellationOutcome::Accepted),
            CancelRequest::AlreadyTerminal(openwork_execution::RunStatus::Cancelled) => {
                Ok(CancellationOutcome::AlreadyCancelled)
            }
            CancelRequest::AlreadyTerminal(_) => Ok(CancellationOutcome::TerminalConflict),
        }
    }
}

/// Runs migrations and serves the Control API.
///
/// # Errors
///
/// Returns connection, migration, bind, and serving failures.
pub async fn serve(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(2))
        .max_connections(8)
        .connect(&config.database_url)
        .await?;
    MIGRATIONS.run(&pool).await?;
    let recovery_actor = ActorId::parse(config.actor_id.clone())?;
    let store = PostgresExecutionStore::new(pool.clone());
    let recovery_time = UtcTimestamp::now();
    store.recover_expired_leases(recovery_actor.clone(), recovery_time)?;
    store.recover_interrupted_runs(recovery_actor, recovery_time)?;
    let prompts = Arc::new(SharedPromptBoundary::production());
    let connectors = Arc::new(ConnectorRegistry::new(config.connectors));
    let worker = config
        .worker
        .map(|worker_config| {
            worker_runtime::WorkerRuntime::start(
                worker_config,
                store.clone(),
                Arc::clone(&prompts),
                config.workspace_root.clone(),
            )
        })
        .transpose()?;
    let worker_runtimes = worker
        .as_ref()
        .map_or_else(BTreeSet::new, |worker| worker.runtimes().clone());
    let worker_ready = worker.as_ref().map_or_else(
        || Arc::new(AtomicBool::new(false)),
        worker_runtime::WorkerRuntime::readiness,
    );
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let result = axum::serve(
        listener,
        router_with_dependencies(
            pool,
            &config.api_token,
            config.actor_id,
            config.workspace_root,
            RouterDependencies {
                cancellation: Arc::new(DurableCancellationCoordinator {
                    store: store.clone(),
                }),
                prompts,
                worker_runtimes,
                worker_ready,
                connectors,
            },
        ),
    )
    .await;
    drop(worker);
    result.map_err(Into::into)
}

/// Builds the authenticated API around one Postgres store and trusted workspace root.
///
/// `workspace_root` must already be canonical. Production callers should use
/// [`Config::from_env`], which enforces that invariant.
pub fn router(pool: PgPool, api_token: &str, actor_id: String, workspace_root: PathBuf) -> Router {
    router_with_prompt_boundary(
        pool,
        api_token,
        actor_id,
        workspace_root,
        Arc::new(SharedPromptBoundary::production()),
    )
}

/// Builds the API with an explicitly shared prompt handoff for a future worker.
///
/// The caller must keep the same [`PromptDelivery`] implementation process-local;
/// this M1 boundary intentionally has no durable-secret-storage fallback.
pub fn router_with_prompt_boundary(
    pool: PgPool,
    api_token: &str,
    actor_id: String,
    workspace_root: PathBuf,
    prompts: Arc<SharedPromptBoundary>,
) -> Router {
    let cancellation = Arc::new(DurableCancellationCoordinator {
        store: PostgresExecutionStore::new(pool.clone()),
    });
    router_with_dependencies(
        pool,
        api_token,
        actor_id,
        workspace_root,
        RouterDependencies {
            cancellation,
            prompts,
            worker_runtimes: BTreeSet::new(),
            worker_ready: Arc::new(AtomicBool::new(false)),
            connectors: Arc::new(ConnectorRegistry::empty()),
        },
    )
}

#[cfg(test)]
fn router_with_cancellation(
    pool: PgPool,
    api_token: &str,
    actor_id: String,
    workspace_root: PathBuf,
    cancellation: Arc<dyn CancellationCoordinator>,
) -> Router {
    router_with_dependencies(
        pool,
        api_token,
        actor_id,
        workspace_root,
        RouterDependencies {
            cancellation,
            prompts: Arc::new(SharedPromptBoundary::production()),
            worker_runtimes: BTreeSet::new(),
            worker_ready: Arc::new(AtomicBool::new(false)),
            connectors: Arc::new(ConnectorRegistry::empty()),
        },
    )
}

fn router_with_dependencies(
    pool: PgPool,
    api_token: &str,
    actor_id: String,
    workspace_root: PathBuf,
    dependencies: RouterDependencies,
) -> Router {
    let state = Arc::new(AppState {
        store: PostgresExecutionStore::new(pool.clone()),
        pool,
        token_hash: Sha256::digest(api_token.as_bytes()).into(),
        actor_id,
        workspace_root,
        cancellation: dependencies.cancellation,
        prompts: dependencies.prompts,
        worker_runtimes: Arc::new(dependencies.worker_runtimes),
        worker_ready: dependencies.worker_ready,
        connectors: dependencies.connectors,
    });
    let protected = Router::new()
        .route("/runs", post(create_run))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/events", get(list_events))
        .route("/runs/{id}/artifacts", get(list_artifacts))
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}", get(get_approval))
        .route("/approvals/{id}/approve", post(approve))
        .route("/approvals/{id}/deny", post(deny))
        .route("/connectors", get(list_connectors))
        .route("/connectors/{id}/tools", get(list_connector_tools))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/health", get(health))
        .nest("/v1", protected)
        .with_state(state)
}

async fn authenticate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, ApiError> {
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(ApiError::unauthorized())?;
    let supplied_hash: [u8; 32] = Sha256::digest(supplied.as_bytes()).into();
    if !bool::from(supplied_hash.ct_eq(&state.token_hash)) {
        return Err(ApiError::unauthorized());
    }
    request
        .extensions_mut()
        .insert(AuthActor(state.actor_id.clone()));
    Ok(next.run(request).await)
}

#[derive(Clone)]
struct AuthActor(String);

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    postgres: &'static str,
}

async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.pool),
    )
    .await
    .is_ok_and(|result| result.is_ok())
    {
        (
            StatusCode::OK,
            Json(HealthResponse {
                status: "ok",
                postgres: "ok",
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "degraded",
                postgres: "unavailable",
            }),
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRun {
    runtime: String,
    workspace: String,
    prompt: String,
}

async fn create_run(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<AuthActor>,
    Json(mut request): Json<CreateRun>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    validate_create_run(&request)?;
    let workspace = resolve_workspace(&state.workspace_root, &request.workspace)?;
    if !state.worker_ready.load(Ordering::Acquire)
        || !state.worker_runtimes.contains(&request.runtime)
    {
        return Err(ApiError::unavailable("runtime_worker_unavailable"));
    }
    let prompt = SecretBytes::from_string(std::mem::take(&mut request.prompt));
    let trusted_actor = ActorId::parse(actor.0.clone()).map_err(|_| ApiError::internal())?;
    let cancellation_actor = ActorId::parse(actor.0).map_err(|_| ApiError::internal())?;
    let scanner =
        ArtifactScanner::new(DEFAULT_MAX_ARTIFACT_BYTES).map_err(|_| ApiError::internal())?;
    let orchestrator = ExecutionOrchestrator::new(state.store.clone(), scanner);
    let run = state
        .prompts
        .with_creation_gate(|delivery| {
            let run = orchestrator
                .create_run(
                    &request.runtime,
                    &workspace,
                    trusted_actor,
                    prompt.as_str(),
                    UtcTimestamp::now(),
                )
                .map_err(|_| ApiError::internal())?;
            if delivery
                .register_prompt(
                    run.id.clone(),
                    run.prompt_sha256.clone(),
                    prompt,
                    UtcTimestamp::now(),
                )
                .is_err()
            {
                // Registration retains no bytes on error. Keep the gate held
                // while cancelling so a worker cannot claim this run before
                // the missing-prompt condition is durably fail-closed.
                let _ =
                    state
                        .store
                        .request_cancel(&run.id, cancellation_actor, UtcTimestamp::now());
                return Err(ApiError::internal());
            }
            Ok(run)
        })
        .map_err(|_| ApiError::internal())??;
    Ok((StatusCode::CREATED, Json(run)))
}

fn validate_create_run(request: &CreateRun) -> Result<(), ApiError> {
    if !matches!(request.runtime.as_str(), "codex" | "claude-code")
        || request.prompt.is_empty()
        || request.prompt.len() > 256 * 1024
    {
        return Err(ApiError::invalid("invalid_run_request"));
    }
    let workspace = request.workspace.as_str();
    if workspace.is_empty()
        || workspace.len() > 256
        || workspace.starts_with('/')
        || workspace.contains('\\')
        || !workspace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
        || workspace
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(ApiError::invalid("invalid_workspace_id"));
    }
    Ok(())
}

fn resolve_workspace(root: &FsPath, workspace: &str) -> Result<PathBuf, ApiError> {
    let candidate = fs::canonicalize(root.join(workspace))
        .map_err(|_| ApiError::invalid("workspace_unavailable"))?;
    if candidate == root
        || !candidate.starts_with(root)
        || !fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(ApiError::invalid("invalid_workspace_id"));
    }
    Ok(candidate)
}

async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Run>, ApiError> {
    let id = validate_execution_id(id)?;
    let run_id =
        RunId::parse(&id.to_string()).map_err(|_| ApiError::invalid("invalid_execution_id"))?;
    state
        .store
        .get_run(&run_id)
        .map_err(|_| ApiError::internal())?
        .map(Json)
        .ok_or(ApiError::not_found("run_not_found"))
}

async fn cancel_run(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<AuthActor>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = validate_execution_id(id)?;
    let run_id =
        RunId::parse(&id.to_string()).map_err(|_| ApiError::invalid("invalid_execution_id"))?;
    let actor = ActorId::parse(actor.0).map_err(|_| ApiError::internal())?;
    match state
        .cancellation
        .request_cancel(&run_id, actor, UtcTimestamp::now())
    {
        Ok(CancellationOutcome::Cancelled | CancellationOutcome::AlreadyCancelled) => Ok((
            StatusCode::OK,
            Json(json!({"status":"cancelled", "confirmed":true})),
        )),
        Ok(CancellationOutcome::Accepted) => Ok((
            StatusCode::ACCEPTED,
            Json(json!({"status":"cancelling", "confirmed":false})),
        )),
        Ok(CancellationOutcome::TerminalConflict) => {
            Err(ApiError::conflict("run_already_terminal"))
        }
        Err(CancellationError::NotFound) => Err(ApiError::not_found("run_not_found")),
        Err(CancellationError::Conflict) => Err(ApiError::conflict("run_cancellation_conflict")),
        Err(CancellationError::Internal) => Err(ApiError::internal()),
    }
}

async fn list_artifacts(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<openwork_execution::Artifact>>, ApiError> {
    let id = validate_execution_id(id)?;
    let run_id =
        RunId::parse(&id.to_string()).map_err(|_| ApiError::invalid("invalid_execution_id"))?;
    state
        .store
        .artifacts(&run_id)
        .map(Json)
        .map_err(|_| ApiError::internal())
}

async fn list_events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<openwork_execution::AuditEvent>>, ApiError> {
    let id = validate_execution_id(id)?;
    let run_id =
        RunId::parse(&id.to_string()).map_err(|_| ApiError::invalid("invalid_execution_id"))?;
    state
        .store
        .audit_events(&run_id)
        .map(Json)
        .map_err(|_| ApiError::internal())
}

#[derive(Serialize, FromRow)]
struct ApprovalResponse {
    schema_version: i32,
    id: Uuid,
    run_id: Uuid,
    action_id: Uuid,
    parameter_hash: String,
    requested_by: String,
    request_reason: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    status: String,
    revision: i64,
    decision: Option<Value>,
    #[serde(with = "time::serde::rfc3339::option")]
    consumed_at: Option<OffsetDateTime>,
}

async fn list_approvals(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ApprovalResponse>>, ApiError> {
    let rows = sqlx::query_as::<_, ApprovalResponse>(concat!(
        "SELECT 1 AS schema_version,ar.id,run_id,action_id,parameter_hash::text,requested_by,request_reason,created_at,",
        "expires_at,status::text AS status,revision,CASE WHEN ad.id IS NULL THEN NULL ELSE jsonb_build_object(",
        "'decision',ad.decision,'actor',ad.actor_id,'reason',ad.reason,'decided_at',to_char(ad.decided_at AT TIME ZONE 'UTC',",
        "'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) END AS decision,consumed_at FROM approval_requests ar ",
        "LEFT JOIN approval_decisions ad ON ad.approval_id=ar.id ORDER BY created_at"
    )).fetch_all(&state.pool).await?;
    Ok(Json(rows))
}

async fn list_connectors(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<connector_runtime::ConnectorSummary>> {
    Json(state.connectors.summaries())
}

async fn list_connector_tools(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<connector_runtime::ConnectorTool>>, ApiError> {
    let connectors = Arc::clone(&state.connectors);
    let tools = tokio::task::spawn_blocking(move || connectors.tools(&id))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|error| match error {
            LookupError::NotFound => ApiError::not_found("connector_not_found"),
            LookupError::NotConfigured => ApiError::unavailable("connector_not_configured"),
            LookupError::Unavailable => ApiError::unavailable("connector_unavailable"),
        })?;
    Ok(Json(tools))
}

async fn get_approval(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApprovalRequest>, ApiError> {
    let approval_id = parse_approval_id(id)?;
    state
        .store
        .get_approval(&approval_id)
        .map_err(|_| ApiError::internal())?
        .map(Json)
        .ok_or(ApiError::not_found("approval_not_found"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequest {
    expected_revision: i64,
    reason: Option<String>,
}

async fn approve(
    state: State<Arc<AppState>>,
    actor: Extension<AuthActor>,
    path: Path<Uuid>,
    body: Json<DecisionRequest>,
) -> Result<Json<ApprovalRequest>, ApiError> {
    decide_approval(state, actor, path, body, ApprovalDecision::Approved)
}

async fn deny(
    state: State<Arc<AppState>>,
    actor: Extension<AuthActor>,
    path: Path<Uuid>,
    body: Json<DecisionRequest>,
) -> Result<Json<ApprovalRequest>, ApiError> {
    decide_approval(state, actor, path, body, ApprovalDecision::Denied)
}

fn decide_approval(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<AuthActor>,
    Path(id): Path<Uuid>,
    Json(request): Json<DecisionRequest>,
    decision: ApprovalDecision,
) -> Result<Json<ApprovalRequest>, ApiError> {
    let approval_id = parse_approval_id(id)?;
    let expected_revision = u64::try_from(request.expected_revision)
        .map_err(|_| ApiError::invalid("invalid_revision"))?;
    if request
        .reason
        .as_ref()
        .is_some_and(|reason| reason.len() > 2048)
        || actor.0.is_empty()
    {
        return Err(ApiError::invalid("invalid_decision_request"));
    }
    if state
        .store
        .get_approval(&approval_id)
        .map_err(|_| ApiError::internal())?
        .is_none()
    {
        return Err(ApiError::not_found("approval_not_found"));
    }
    let trusted_actor = ActorId::parse(actor.0).map_err(|_| ApiError::internal())?;
    state
        .store
        .decide_approval(
            &approval_id,
            expected_revision,
            decision,
            trusted_actor,
            request.reason.as_deref(),
            UtcTimestamp::now(),
        )
        .map(Json)
        .map_err(|_| ApiError::conflict("approval_decision_rejected"))
}

fn parse_approval_id(id: Uuid) -> Result<ApprovalId, ApiError> {
    let id = validate_execution_id(id)?;
    ApprovalId::parse(&id.to_string()).map_err(|_| ApiError::invalid("invalid_execution_id"))
}

fn validate_execution_id(id: Uuid) -> Result<Uuid, ApiError> {
    if id.get_version_num() == 7 {
        Ok(id)
    } else {
        Err(ApiError::invalid("invalid_execution_id"))
    }
}

#[derive(Debug)]
struct ApiError(StatusCode, &'static str);

impl ApiError {
    const fn unauthorized() -> Self {
        Self(StatusCode::UNAUTHORIZED, "unauthorized")
    }
    const fn not_found(code: &'static str) -> Self {
        Self(StatusCode::NOT_FOUND, code)
    }
    const fn invalid(code: &'static str) -> Self {
        Self(StatusCode::UNPROCESSABLE_ENTITY, code)
    }
    const fn conflict(code: &'static str) -> Self {
        Self(StatusCode::CONFLICT, code)
    }
    const fn unavailable(code: &'static str) -> Self {
        Self(StatusCode::SERVICE_UNAVAILABLE, code)
    }
    const fn internal() -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(_: sqlx::Error) -> Self {
        Self::internal()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Method, Request},
    };
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::parse(value).expect("valid test timestamp")
    }

    fn digest(prompt: &str) -> Sha256Digest {
        let hash: [u8; 32] = Sha256::digest(prompt.as_bytes()).into();
        Sha256Digest::parse(hex_digest(hash)).expect("valid SHA-256 digest")
    }

    #[test]
    fn prompt_delivery_is_exactly_bound_one_time_and_safe_to_log() {
        let delivery = InMemoryPromptDelivery::new(2, 64, Duration::from_mins(1));
        let run_id = RunId::generate();
        let prompt = "do not log this raw prompt";
        let expected = digest(prompt);
        let wrong = digest("tampered");
        let now = timestamp("2026-08-22T00:00:00Z");

        delivery
            .register_prompt(
                run_id.clone(),
                expected.clone(),
                SecretBytes::from_string(prompt.to_owned()),
                now,
            )
            .expect("register prompt");
        assert!(matches!(
            delivery.take_prompt(&run_id, &wrong, now),
            Err(PromptDeliveryError::DigestMismatch)
        ));
        let consumed = delivery
            .take_prompt(&run_id, &expected, now)
            .expect("exact binding consumes prompt");
        assert_eq!(consumed.as_bytes(), prompt.as_bytes());
        drop(consumed);
        assert!(matches!(
            delivery.take_prompt(&run_id, &expected, now),
            Err(PromptDeliveryError::Unavailable)
        ));
        assert!(!format!("{:?}", PromptDeliveryError::DigestMismatch).contains(prompt));
    }

    #[test]
    fn prompt_delivery_rejects_expiry_duplicate_capacity_and_restart_loss() {
        let now = timestamp("2026-08-22T00:00:00Z");
        let prompt = "bounded";
        let expected = digest(prompt);
        let first = RunId::generate();
        let second = RunId::generate();
        let delivery = InMemoryPromptDelivery::new(1, 16, Duration::from_secs(1));

        delivery
            .register_prompt(
                first.clone(),
                expected.clone(),
                SecretBytes::from_string(prompt.to_owned()),
                now,
            )
            .expect("first prompt fits");
        assert_eq!(
            delivery.register_prompt(
                first.clone(),
                expected.clone(),
                SecretBytes::from_string(prompt.to_owned()),
                now,
            ),
            Err(PromptDeliveryError::DuplicateRun)
        );
        assert_eq!(
            delivery.register_prompt(
                second,
                expected.clone(),
                SecretBytes::from_string(prompt.to_owned()),
                now,
            ),
            Err(PromptDeliveryError::CapacityExceeded)
        );
        let expired = timestamp("2026-08-22T00:00:02Z");
        assert_eq!(delivery.purge_expired(expired), 1);
        assert!(matches!(
            delivery.take_prompt(&first, &expected, expired),
            Err(PromptDeliveryError::Unavailable)
        ));

        let restarted = InMemoryPromptDelivery::new(1, 16, Duration::from_secs(1));
        assert!(matches!(
            restarted.take_prompt(&first, &expected, now),
            Err(PromptDeliveryError::Unavailable)
        ));
    }

    #[test]
    fn prompt_registration_rejects_tampered_or_oversized_values_without_delivery() {
        let now = timestamp("2026-08-22T00:00:00Z");
        let delivery = InMemoryPromptDelivery::new(1, 16, Duration::from_mins(1));
        let tampered = RunId::generate();
        assert_eq!(
            delivery.register_prompt(
                tampered.clone(),
                digest("expected"),
                SecretBytes::from_string("actual".to_owned()),
                now,
            ),
            Err(PromptDeliveryError::DigestMismatch)
        );
        assert!(matches!(
            delivery.take_prompt(&tampered, &digest("expected"), now),
            Err(PromptDeliveryError::Unavailable)
        ));
        let oversized = RunId::generate();
        assert_eq!(
            delivery.register_prompt(
                oversized,
                digest("this value is larger than the bounded prompt slot"),
                SecretBytes::from_string(
                    "this value is larger than the bounded prompt slot".to_owned(),
                ),
                now,
            ),
            Err(PromptDeliveryError::CapacityExceeded)
        );
    }

    #[test]
    fn worker_claim_gate_cannot_enter_before_persist_and_publish_complete() {
        let boundary = Arc::new(SharedPromptBoundary::new(InMemoryPromptDelivery::new(
            1,
            64,
            Duration::from_mins(1),
        )));
        let run_id = RunId::generate();
        let prompt = "publish-before-claim";
        let expected = digest(prompt);
        let now = timestamp("2026-08-22T00:00:00Z");
        let (creation_entered_tx, creation_entered_rx) = std::sync::mpsc::channel();
        let (allow_publish_tx, allow_publish_rx) = std::sync::mpsc::channel();
        let creating = Arc::clone(&boundary);
        let publish_run_id = run_id.clone();
        let publish_digest = expected.clone();
        let creator = std::thread::spawn(move || {
            creating
                .with_creation_gate(|delivery| {
                    creation_entered_tx.send(()).expect("signal gate held");
                    allow_publish_rx.recv().expect("allow publication");
                    delivery
                        .register_prompt(
                            publish_run_id,
                            publish_digest,
                            SecretBytes::from_string(prompt.to_owned()),
                            now,
                        )
                        .expect("publish prompt before gate release");
                })
                .expect("creation gate is healthy");
        });
        creation_entered_rx.recv().expect("creation holds gate");

        let (claim_entered_tx, claim_entered_rx) = std::sync::mpsc::channel();
        let claiming = Arc::clone(&boundary);
        let worker = std::thread::spawn(move || {
            claiming
                .with_worker_claim(|delivery| {
                    let delivered = delivery
                        .take_prompt(&run_id, &expected, now)
                        .expect("worker sees published prompt");
                    assert_eq!(delivered.into_string(), prompt);
                    claim_entered_tx.send(()).expect("claim entered");
                })
                .expect("worker gate is healthy");
        });
        assert!(
            claim_entered_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );

        allow_publish_tx.send(()).expect("release publication");
        creator.join().expect("creator joins");
        claim_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("claim enters after publication");
        worker.join().expect("worker joins");
    }

    fn test_router(root: PathBuf) -> Router {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://openwork:openwork@127.0.0.1:9/openwork")
            .expect("valid test database URL");
        router(pool, TOKEN, "test:control".to_owned(), root)
    }

    struct AcceptedCancellation;

    impl CancellationCoordinator for AcceptedCancellation {
        fn request_cancel(
            &self,
            _run_id: &RunId,
            _actor: ActorId,
            _now: UtcTimestamp,
        ) -> Result<CancellationOutcome, CancellationError> {
            Ok(CancellationOutcome::Accepted)
        }
    }

    fn accepted_cancellation_router(root: PathBuf) -> Router {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://openwork:openwork@127.0.0.1:9/openwork")
            .expect("valid test database URL");
        router_with_cancellation(
            pool,
            TOKEN,
            "test:control".to_owned(),
            root,
            Arc::new(AcceptedCancellation),
        )
    }

    fn request(method: Method, uri: &str, body: &str, authenticated: bool) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if authenticated {
            builder = builder.header("authorization", format!("Bearer {TOKEN}"));
        }
        if !body.is_empty() {
            builder = builder.header("content-type", "application/json");
        }
        builder
            .body(Body::from(body.to_owned()))
            .expect("valid request")
    }

    #[tokio::test]
    async fn protected_routes_require_bearer_authentication_before_database_access() {
        let response = test_router(PathBuf::from("."))
            .oneshot(request(Method::GET, "/v1/approvals", "", false))
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn connector_catalog_is_authenticated_and_does_not_require_database_access() {
        let response = test_router(PathBuf::from("."))
            .oneshot(request(Method::GET, "/v1/connectors", "", true))
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_run_rejects_unsafe_workspace_before_database_access() {
        let response = test_router(PathBuf::from("."))
            .oneshot(request(
                Method::POST,
                "/v1/runs",
                r#"{"runtime":"mock","workspace":"../escape","prompt":"secret"}"#,
                true,
            ))
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn create_run_is_unavailable_without_an_owned_worker() {
        let root = tempfile::tempdir().expect("workspace root");
        fs::create_dir(root.path().join("safe")).expect("workspace");
        let canonical_root = fs::canonicalize(root.path()).expect("canonical workspace root");
        let response = test_router(canonical_root)
            .oneshot(request(
                Method::POST,
                "/v1/runs",
                r#"{"runtime":"codex","workspace":"safe","prompt":"secret"}"#,
                true,
            ))
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn active_cancellation_returns_intent_not_a_false_terminal_claim() {
        let run_id = Uuid::now_v7();
        let response = accepted_cancellation_router(PathBuf::from("."))
            .oneshot(request(
                Method::POST,
                &format!("/v1/runs/{run_id}/cancel"),
                "",
                true,
            ))
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn approval_detail_and_decision_reject_non_v7_ids_before_database_access() {
        let invalid_id = Uuid::nil();
        for (method, uri, body) in [
            (
                Method::GET,
                format!("/v1/approvals/{invalid_id}"),
                String::new(),
            ),
            (
                Method::POST,
                format!("/v1/approvals/{invalid_id}/approve"),
                r#"{"expected_revision":0}"#.to_owned(),
            ),
        ] {
            let response = test_router(PathBuf::from("."))
                .oneshot(request(method, &uri, &body, true))
                .await
                .expect("router response");
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
    }

    #[test]
    fn workspace_resolution_is_root_bounded_and_requires_a_directory() {
        let root = env::temp_dir().join(format!("openwork-control-api-{}", Uuid::now_v7()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace fixture");
        let canonical_root = fs::canonicalize(&root).expect("canonical root");

        assert_eq!(
            resolve_workspace(&canonical_root, "workspace").expect("valid workspace"),
            fs::canonicalize(&workspace).expect("canonical workspace")
        );
        assert!(resolve_workspace(&canonical_root, "missing").is_err());

        fs::remove_dir_all(&root).expect("remove workspace fixture");
    }
}
