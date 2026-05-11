//! HTTP layer. The bootstrap router exposes only `/health`; per-program
//! routers are merged in by the modules added in subsequent migration PRs.

use axum::{routing::get, Json, Router};
use serde::Serialize;
use sqlx::PgPool;

pub mod inqacc;
pub mod inqcust;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub sortcode: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(inqcust::router())
        .merge(inqacc::router())
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}
