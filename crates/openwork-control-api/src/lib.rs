//! Authenticated, Postgres-backed M1 control-plane HTTP API.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use openwork_execution::ActorId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use std::{env, net::SocketAddr, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub database_url: String,
    pub api_token: String,
    pub actor_id: String,
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
        Ok(Self {
            bind,
            database_url,
            api_token,
            actor_id,
        })
    }
}

fn required_env(name: &'static str) -> Result<String, ConfigError> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(ConfigError(name))
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
    token_hash: [u8; 32],
    actor_id: String,
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
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    axum::serve(listener, router(pool, &config.api_token, config.actor_id)).await?;
    Ok(())
}

pub fn router(pool: PgPool, api_token: &str, actor_id: String) -> Router {
    let state = Arc::new(AppState {
        pool,
        token_hash: Sha256::digest(api_token.as_bytes()).into(),
        actor_id,
    });
    let protected = Router::new()
        .route("/runs", post(create_run))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/events", get(list_events))
        .route("/runs/{id}/artifacts", get(list_artifacts))
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}/approve", post(approve))
        .route("/approvals/{id}/deny", post(deny))
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

#[derive(Serialize, FromRow)]
struct RunResponse {
    schema_version: i32,
    id: Uuid,
    runtime: String,
    workspace: String,
    status: String,
    revision: i64,
    actor_id: String,
    prompt_sha256: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    completed_at: Option<OffsetDateTime>,
    terminal_reason: Option<String>,
}

async fn create_run(
    Extension(_actor): Extension<AuthActor>,
    Json(request): Json<CreateRun>,
) -> Result<Json<Value>, ApiError> {
    validate_create_run(&request)?;
    // Run creation must atomically persist the genesis audit event. The M1
    // orchestrator owns that transaction and is injected in the integration wave.
    Err(ApiError::unavailable("run_orchestrator_unavailable"))
}

fn validate_create_run(request: &CreateRun) -> Result<(), ApiError> {
    if request.runtime.trim().is_empty()
        || request.runtime.len() > 128
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

async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<RunResponse>, ApiError> {
    let id = validate_execution_id(id)?;
    fetch_run(&state.pool, id).await.map(Json)
}

async fn fetch_run(pool: &PgPool, id: Uuid) -> Result<RunResponse, ApiError> {
    sqlx::query_as::<_, RunResponse>(concat!(
        "SELECT 1 AS schema_version,id,runtime,workspace,status::text AS status,revision,actor_id,",
        "prompt_sha256::text,created_at,updated_at,started_at,completed_at,terminal_reason ",
        "FROM runs WHERE id=$1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(ApiError::not_found("run_not_found"))
}

async fn cancel_run(
    Extension(_actor): Extension<AuthActor>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let _id = validate_execution_id(id)?;
    // Never claim cancellation until the orchestrator has stopped runtime and sandbox.
    Err(ApiError::unavailable("run_orchestrator_unavailable"))
}

#[derive(Serialize, FromRow)]
struct ArtifactResponse {
    schema_version: i32,
    id: Uuid,
    run_id: Uuid,
    path: String,
    media_type: String,
    size_bytes: i64,
    sha256: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

async fn list_artifacts(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ArtifactResponse>>, ApiError> {
    let id = validate_execution_id(id)?;
    let rows = sqlx::query_as::<_, ArtifactResponse>(
        "SELECT 1 AS schema_version,id,run_id,path,media_type,size_bytes,sha256::text,created_at FROM artifacts WHERE run_id=$1 ORDER BY created_at,id"
    ).bind(id).fetch_all(&state.pool).await?;
    Ok(Json(rows))
}

#[derive(Serialize, FromRow)]
struct EventResponse {
    schema_version: i32,
    id: Uuid,
    run_id: Uuid,
    sequence: i64,
    event_type: String,
    actor: String,
    #[serde(with = "time::serde::rfc3339")]
    timestamp: OffsetDateTime,
    metadata: Value,
    previous_hash: Option<String>,
    event_hash: String,
}

async fn list_events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<EventResponse>>, ApiError> {
    let id = validate_execution_id(id)?;
    let rows = sqlx::query_as::<_, EventResponse>(
        "SELECT 1 AS schema_version,id,run_id,sequence,event_type,actor_id AS actor,occurred_at AS timestamp,redacted_metadata AS metadata,previous_hash::text,event_hash::text FROM audit_events WHERE run_id=$1 ORDER BY sequence"
    ).bind(id).fetch_all(&state.pool).await?;
    Ok(Json(rows))
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequest {
    expected_revision: i64,
    reason: Option<String>,
}

async fn approve(
    _state: State<Arc<AppState>>,
    actor: Extension<AuthActor>,
    path: Path<Uuid>,
    body: Json<DecisionRequest>,
) -> Result<Json<Value>, ApiError> {
    unavailable_decision(actor, path, body)
}

async fn deny(
    _state: State<Arc<AppState>>,
    actor: Extension<AuthActor>,
    path: Path<Uuid>,
    body: Json<DecisionRequest>,
) -> Result<Json<Value>, ApiError> {
    unavailable_decision(actor, path, body)
}

fn unavailable_decision(
    Extension(actor): Extension<AuthActor>,
    Path(id): Path<Uuid>,
    Json(request): Json<DecisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let _id = validate_execution_id(id)?;
    if request.expected_revision < 0
        || request
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > 2048)
        || actor.0.is_empty()
    {
        return Err(ApiError::invalid("invalid_revision"));
    }
    // Decision and matching audit event are one policy-owned transaction.
    Err(ApiError::unavailable("approval_service_unavailable"))
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
