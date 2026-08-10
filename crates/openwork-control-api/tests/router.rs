use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use openwork_control_api::router;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN: &str = "01234567890123456789012345678901";

fn test_router() -> Router {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(10))
        .connect_lazy("postgresql://localhost/openwork")
        .unwrap();
    router(pool, TOKEN, "trusted-actor".to_owned())
}

async fn post(path: &str, body: &str, authenticated: bool) -> StatusCode {
    let mut request = Request::post(path).header("content-type", "application/json");
    if authenticated {
        request = request.header("authorization", format!("Bearer {TOKEN}"));
    }
    test_router()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn health_is_anonymous() {
    let response = test_router()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn mutation_requires_bearer_auth_before_database_access() {
    assert_eq!(
        post("/v1/runs", "{}", false).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn actor_cannot_be_supplied_by_request_body() {
    let body = r#"{"runtime":"mock","workspace":"sales","prompt":"x","actor_id":"attacker"}"#;
    assert_eq!(
        post("/v1/runs", body, true).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn authenticated_create_fails_closed_without_orchestrator() {
    let body = r#"{"runtime":"mock","workspace":"sales","prompt":"analyze"}"#;
    assert_eq!(
        post("/v1/runs", body, true).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn cancel_fails_closed_without_orchestrator() {
    let path = format!("/v1/runs/{}/cancel", Uuid::now_v7());
    assert_eq!(post(&path, "", true).await, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn workspace_ids_cannot_escape() {
    let body = r#"{"runtime":"mock","workspace":"../etc","prompt":"x"}"#;
    assert_eq!(
        post("/v1/runs", body, true).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
