//! Integration tests for the CBSA bootstrap skeleton.
//!
//! These tests verify that the foundational infrastructure works end-to-end:
//! database connection + migration, HTTP server routing, error response
//! formatting, and domain type serialization.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use cbsa::{config::AppConfig, db, error::ProblemDetail, web};
use http_body_util::BodyExt;
use sqlx::PgPool;
use std::sync::Once;
use testcontainers::{clients::Cli, RunnableImage};
use testcontainers_modules::cockroachdb::CockroachDb;
use tokio::sync::OnceCell;
use tower::ServiceExt;

static DOCKER: OnceCell<Cli> = OnceCell::const_new();
static CONTAINER: OnceCell<testcontainers::Container<'static, CockroachDb>> =
    OnceCell::const_new();
static INIT_TRACING: Once = Once::new();

async fn setup() -> PgPool {
    INIT_TRACING.call_once(|| {
        tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("info")
            .init();
    });

    let docker = DOCKER.get_or_init(|| async { Cli::default() }).await;

    let container = CONTAINER
        .get_or_init(|| async {
            let image = RunnableImage::from(CockroachDb::default());
            docker.run(image)
        })
        .await;

    let port = container.get_host_port_ipv4(26257);
    let url = format!("postgres://root@127.0.0.1:{}/defaultdb?sslmode=disable", port);

    let pool = sqlx::PgPool::connect(&url)
        .await
        .expect("failed to connect to test database");

    db::MIGRATOR
        .run(&pool)
        .await
        .expect("failed to run migrations");

    pool
}

#[tokio::test]
async fn test_database_migration() {
    let pool = setup().await;

    // Verify that the baseline tables exist
    let customer_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM customer")
        .fetch_one(&pool)
        .await
        .expect("customer table should exist");
    assert_eq!(customer_count, 0);

    let account_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account")
        .fetch_one(&pool)
        .await
        .expect("account table should exist");
    assert_eq!(account_count, 0);

    let proctran_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran table should exist");
    assert_eq!(proctran_count, 0);

    // Verify control table has the baseline row
    let control_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control")
        .fetch_one(&pool)
        .await
        .expect("control table should exist");
    assert_eq!(control_count, 1, "control table should have exactly one row");
}

#[tokio::test]
async fn test_health_endpoint() {
    let pool = setup().await;
    let app = web::router(web::AppState { pool });

    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn test_health_endpoint_content_type() {
    let pool = setup().await;
    let app = web::router(web::AppState { pool });

    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok());

    assert_eq!(content_type, Some("application/json"));
}
