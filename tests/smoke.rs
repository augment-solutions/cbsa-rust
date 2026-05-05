//! Smoke test: boot the axum router from `cbsa::web::router` and assert that
//! `GET /health` returns `200 {"status":"ok"}`. The pool is constructed
//! lazily so the test does not require a running CockroachDB — `/health` is
//! the only route registered by the bootstrap and it does not touch the
//! database. Per-program PRs add DB-backed integration tests under their own
//! `tests/<program>.rs` files using `testcontainers-modules`.

use axum::body::Body;
use axum::http::{header, HeaderValue, Request, StatusCode};
use cbsa::web::{router, AppState};
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://cbsa:cbsa@127.0.0.1:1/cbsa")
        .expect("lazy pool construction must succeed for a well-formed url");

    let app = router(AppState { pool });

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router must respond to /health");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/json")),
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], br#"{"status":"ok"}"#);
}
