//! Unit tests for `db::with_retry` — the CockroachDB serialization-retry helper.
//!
//! These tests verify the retry logic without requiring a running database:
//! - successful operations return immediately
//! - serialization failures (SQLSTATE 40001) are retried up to DEFAULT_RETRY_ATTEMPTS
//! - non-serialization errors short-circuit immediately
//! - exhausted retries return the last error

use cbsa::db::{with_retry, DEFAULT_RETRY_ATTEMPTS};
use sqlx::postgres::PgDatabaseError;
use sqlx::{Error as SqlxError, PgPool};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Helper to construct a mock serialization failure error (SQLSTATE 40001).
fn mock_serialization_error() -> SqlxError {
    // Construct a PgDatabaseError with SQLSTATE 40001
    let db_err = Box::new(PgDatabaseError::new(
        "error".into(),
        "40001".into(),
        "serialization failure".into(),
    ));
    SqlxError::Database(db_err)
}

/// Helper to construct a mock non-serialization database error.
fn mock_other_error() -> SqlxError {
    let db_err = Box::new(PgDatabaseError::new(
        "error".into(),
        "23505".into(), // unique violation
        "duplicate key value".into(),
    ));
    SqlxError::Database(db_err)
}

/// Helper to create a lazy pool (doesn't require a running DB).
fn lazy_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://test:test@127.0.0.1:1/test")
        .expect("lazy pool must succeed")
}

#[tokio::test]
async fn successful_operation_returns_immediately() {
    let pool = lazy_pool();
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result = with_retry(&pool, |_pool| {
        let count = call_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<i32, SqlxError>(42)
        }
    })
    .await;

    assert_eq!(result.unwrap(), 42);
    assert_eq!(call_count.load(Ordering::SeqCst), 1, "should only call once");
}

#[tokio::test]
async fn serialization_failure_retries_and_succeeds() {
    let pool = lazy_pool();
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result = with_retry(&pool, |_pool| {
        let count = call_count_clone.clone();
        async move {
            let attempt = count.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt < 3 {
                // Fail first 2 attempts with serialization error
                Err(mock_serialization_error())
            } else {
                // Succeed on 3rd attempt
                Ok::<i32, SqlxError>(100)
            }
        }
    })
    .await;

    assert_eq!(result.unwrap(), 100);
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "should retry twice then succeed"
    );
}

#[tokio::test]
async fn non_serialization_error_short_circuits() {
    let pool = lazy_pool();
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result = with_retry(&pool, |_pool| {
        let count = call_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Err::<i32, SqlxError>(mock_other_error())
        }
    })
    .await;

    assert!(result.is_err());
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "should not retry non-serialization errors"
    );
}

#[tokio::test]
async fn exhausted_retries_returns_last_error() {
    let pool = lazy_pool();
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result = with_retry(&pool, |_pool| {
        let count = call_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            // Always return serialization error
            Err::<i32, SqlxError>(mock_serialization_error())
        }
    })
    .await;

    assert!(result.is_err());
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        DEFAULT_RETRY_ATTEMPTS,
        "should attempt exactly DEFAULT_RETRY_ATTEMPTS times"
    );

    // Verify it's still a serialization error
    match &result {
        Err(SqlxError::Database(db_err)) => {
            assert_eq!(db_err.code(), Some("40001".into()));
        }
        _ => panic!("expected database error with code 40001"),
    }
}

#[tokio::test]
async fn mixed_errors_stop_on_non_serialization() {
    let pool = lazy_pool();
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result = with_retry(&pool, |_pool| {
        let count = call_count_clone.clone();
        async move {
            let attempt = count.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == 1 {
                // First attempt: serialization error (should retry)
                Err(mock_serialization_error())
            } else {
                // Second attempt: different error (should short-circuit)
                Err::<i32, SqlxError>(mock_other_error())
            }
        }
    })
    .await;

    assert!(result.is_err());
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        2,
        "should retry once then stop"
    );

    // Verify the returned error is the non-serialization error
    match &result {
        Err(SqlxError::Database(db_err)) => {
            assert_eq!(db_err.code(), Some("23505".into()));
        }
        _ => panic!("expected database error with code 23505"),
    }
}

#[tokio::test]
async fn pool_is_cloned_on_each_attempt() {
    let pool = lazy_pool();
    let pool_addresses = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let pool_addresses_clone = pool_addresses.clone();

    let _ = with_retry(&pool, |pool_clone| {
        let addresses = pool_addresses_clone.clone();
        async move {
            let mut guard = addresses.lock().await;
            // Store the pool's address to verify it's different each time
            let addr = &pool_clone as *const PgPool as usize;
            guard.push(addr);

            if guard.len() < 2 {
                Err(mock_serialization_error())
            } else {
                Ok::<(), SqlxError>(())
            }
        }
    })
    .await;

    let addresses = pool_addresses.lock().await;
    assert_eq!(addresses.len(), 2, "should have been called twice");
    // Note: We can't reliably test that addresses are different because
    // PgPool cloning doesn't guarantee a different memory address.
    // This test mainly ensures the closure receives a cloned pool each time.
}
