//! Authenticated, Postgres-backed M1 control-plane HTTP API.

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
    UtcTimestamp,
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
    env, fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};
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
    pub workspace_root: PathBuf,
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
        Ok(Self {
            bind,
            database_url,
            api_token,
            actor_id,
            workspace_root,
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
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    axum::serve(
        listener,
        router(
            pool,
            &config.api_token,
            config.actor_id,
            config.workspace_root,
        ),
    )
    .await?;
    Ok(())
}

/// Builds the authenticated API around one Postgres store and trusted workspace root.
///
/// `workspace_root` must already be canonical. Production callers should use
/// [`Config::from_env`], which enforces that invariant.
pub fn router(pool: PgPool, api_token: &str, actor_id: String, workspace_root: PathBuf) -> Router {
    let cancellation = Arc::new(DurableCancellationCoordinator {
        store: PostgresExecutionStore::new(pool.clone()),
    });
    router_with_cancellation(pool, api_token, actor_id, workspace_root, cancellation)
}

fn router_with_cancellation(
    pool: PgPool,
    api_token: &str,
    actor_id: String,
    workspace_root: PathBuf,
    cancellation: Arc<dyn CancellationCoordinator>,
) -> Router {
    let state = Arc::new(AppState {
        store: PostgresExecutionStore::new(pool.clone()),
        pool,
        token_hash: Sha256::digest(api_token.as_bytes()).into(),
        actor_id,
        workspace_root,
        cancellation,
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
    Json(request): Json<CreateRun>,
) -> Result<(StatusCode, Json<Run>), ApiError> {
    validate_create_run(&request)?;
    let workspace = resolve_workspace(&state.workspace_root, &request.workspace)?;
    let trusted_actor = ActorId::parse(actor.0).map_err(|_| ApiError::internal())?;
    let scanner =
        ArtifactScanner::new(DEFAULT_MAX_ARTIFACT_BYTES).map_err(|_| ApiError::internal())?;
    let orchestrator = ExecutionOrchestrator::new(state.store.clone(), scanner);
    let run = orchestrator
        .create_run(
            &request.runtime,
            &workspace,
            trusted_actor,
            &request.prompt,
            UtcTimestamp::now(),
        )
        .map_err(|_| ApiError::internal())?;
    Ok((StatusCode::CREATED, Json(run)))
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
