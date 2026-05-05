//! Database pool, schema migration, and the CockroachDB serialization-retry
//! helper.
//!
//! All persistence in this crate goes through `sqlx::PgPool` against
//! CockroachDB (PostgreSQL wire protocol). The retry helper wraps multi-
//! statement transactions so SQLSTATE 40001 (serialization failure) is
//! retried transparently — see `docs/translation-rules.md` §5.

use std::time::Duration;

use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};

use crate::config::AppConfig;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database connect: {0}")]
    Connect(#[from] sqlx::Error),
    #[error("database migrate: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("invalid database url: {0}")]
    InvalidUrl(String),
}

pub async fn connect(cfg: &AppConfig) -> Result<PgPool, DbError> {
    let opts: PgConnectOptions = cfg
        .database
        .url
        .parse()
        .map_err(|e: sqlx::Error| DbError::InvalidUrl(e.to_string()))?;
    let pool = PgPoolOptions::new()
        .max_connections(cfg.database.max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts)
        .await?;
    Ok(pool)
}

pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// `true` if the error is a CockroachDB serialization failure (SQLSTATE
/// 40001). Such errors must be retried by the surrounding transaction
/// wrapper (`db::with_retry`, added by the first program PR that needs it
/// — see `docs/translation-rules.md` §5) and never surfaced to the caller.
pub fn is_serialization_failure(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|c| c == "40001")
}

/// Default retry budget for the CockroachDB serialization-retry helper.
/// Kept small because CockroachDB performs its own internal retries before
/// surfacing 40001 to the client.
pub const DEFAULT_RETRY_ATTEMPTS: u32 = 5;
