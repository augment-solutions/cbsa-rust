use std::{
    borrow::Cow,
    error::Error as StdError,
    fmt,
    sync::atomic::{AtomicU32, Ordering},
    sync::Arc,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cbsa::{
    db,
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
    repository::creacc::{self, CreateAccountCommand, CreateAccountHook},
    service::creacc::CreaccRequest,
    web::{router, AppState},
};
use chrono::{Days, NaiveDate, NaiveTime};
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::{
    error::{DatabaseError, ErrorKind},
    postgres::PgPoolOptions,
    PgConnection, PgPool, Row,
};
use testcontainers_modules::{
    cockroach_db::CockroachDb,
    testcontainers::{runners::AsyncRunner, ContainerAsync},
};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

static TEST_MUTEX: Mutex<()> = Mutex::const_new(());
static TEST_DATABASE: OnceCell<TestDatabase> = OnceCell::const_new();

struct TestDatabase {
    _container: Option<ContainerAsync<CockroachDb>>,
    database_url: String,
}

struct CustomerSeed<'a> {
    sortcode: &'a str,
    customer_number: i64,
}

struct ForceRetryHook {
    remaining_failures: AtomicU32,
}

#[derive(Debug)]
struct TestSerializationFailure;

impl fmt::Display for TestSerializationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "forced serialization failure")
    }
}

impl StdError for TestSerializationFailure {}

impl DatabaseError for TestSerializationFailure {
    fn message(&self) -> &str {
        "forced serialization failure"
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed("40001"))
    }

    fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

impl ForceRetryHook {
    fn new(remaining_failures: u32) -> Self {
        Self {
            remaining_failures: AtomicU32::new(remaining_failures),
        }
    }
}

#[async_trait]
impl CreateAccountHook for ForceRetryHook {
    async fn before_reserve(&self, _conn: &mut PgConnection) -> Result<(), sqlx::Error> {
        let should_fail = self
            .remaining_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                if value > 0 {
                    Some(value - 1)
                } else {
                    None
                }
            })
            .is_ok();

        if should_fail {
            return Err(sqlx::Error::database(TestSerializationFailure));
        }

        Ok(())
    }
}

#[tokio::test]
async fn creates_account_updates_control_and_writes_proctran() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;
    let sortcode = "961001";
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode,
            customer_number: 42,
        },
    )
    .await;

    let response = app(sortcode, &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/creacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    42, "ISA", "1.25", "250", "1500.25", "1499.75",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("application/json"));

    let body = response_json(response).await;
    assert_eq!(body["CreAcc"]["CommEyecatcher"], "ACCT");
    assert_eq!(body["CreAcc"]["CommCustno"], 42);
    assert_eq!(body["CreAcc"]["CommKey"]["CommSortcode"], sortcode);
    assert_eq!(body["CreAcc"]["CommKey"]["CommNumber"], 1);
    assert_eq!(body["CreAcc"]["CommAccType"], "ISA");
    assert_eq!(body["CreAcc"]["CommIntRt"], "1.25");
    assert_eq!(body["CreAcc"]["CommOverdrLim"], "250.00");
    assert_eq!(body["CreAcc"]["CommAvailBal"], "1500.25");
    assert_eq!(body["CreAcc"]["CommActBal"], "1499.75");
    assert_eq!(body["CreAcc"]["CommSuccess"], "Y");
    assert_eq!(body["CreAcc"]["CommFailCode"], "0");

    let opened = cobol_date(
        body["CreAcc"]["CommOpened"]
            .as_u64()
            .expect("opened date must be numeric"),
    );
    let next_stmt = cobol_date(
        body["CreAcc"]["CommNextStmtDt"]
            .as_u64()
            .expect("next statement date must be numeric"),
    );
    assert_eq!(
        opened
            .checked_add_days(Days::new(30))
            .expect("date must add"),
        next_stmt
    );

    let control =
        sqlx::query("SELECT account_count, account_last FROM control WHERE id = 'GLOBAL'")
            .fetch_one(&pool)
            .await
            .expect("control row must exist");
    assert_eq!(control.get::<i64, _>("account_count"), 1);
    assert_eq!(control.get::<i64, _>("account_last"), 1);

    let account = sqlx::query(
        "SELECT customer_number, account_type, overdraft_limit, available_balance, actual_balance FROM account WHERE sortcode = $1 AND account_number = 1",
    )
    .bind(sortcode)
    .fetch_one(&pool)
    .await
    .expect("account row must exist");
    assert_eq!(account.get::<i64, _>("customer_number"), 42);
    assert_eq!(account.get::<String, _>("account_type"), "ISA");

    let proctran = sqlx::query("SELECT tran_type, description FROM proctran WHERE sortcode = $1")
        .bind(sortcode)
        .fetch_one(&pool)
        .await
        .expect("proctran row must exist");
    assert_eq!(proctran.get::<String, _>("tran_type"), "OCA");
    assert_eq!(
        proctran.get::<String, _>("description"),
        format!(
            "{:010}{:<8}{:08}{:08}      ",
            42,
            "ISA",
            body["CreAcc"]["CommLastStmtDt"]
                .as_u64()
                .expect("last statement must be numeric"),
            body["CreAcc"]["CommNextStmtDt"]
                .as_u64()
                .expect("next statement must be numeric"),
        )
    );
}

#[tokio::test]
async fn rejects_missing_and_blank_fields_with_problem_detail() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    for payload in [
        json!({"CreAcc":{"CommCustno":42,"CommKey":{"CommSortcode":"000000","CommNumber":0},"CommIntRt":"1.25","CommOverdrLim":"250","CommAvailBal":"1500.25","CommActBal":"1499.75"}}).to_string(),
        request_json(42, "   ", "1.25", "250", "1500.25", "1499.75"),
    ] {
        let response = app("961002", &pool)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/creacc")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload))
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(content_type(&response), Some("application/problem+json"));
        let body = response_json(response).await;
        assert_eq!(body["title"], "Validation failed");
    }
}

#[tokio::test]
async fn rejects_range_errors_and_unknown_account_type() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let range_response = app("961003", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/creacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    42, "ISA", "10000.00", "250", "1500.25", "1499.75",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");
    assert_eq!(range_response.status(), StatusCode::BAD_REQUEST);

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "961003",
            customer_number: 42,
        },
    )
    .await;
    let invalid_type_response = app("961003", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/creacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    42, "SAVINGS", "1.25", "250", "1500.25", "1499.75",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(invalid_type_response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(invalid_type_response).await;
    assert_eq!(body["title"], "Invalid account type");
    assert_eq!(body["failCode"], "A");
}

#[tokio::test]
async fn returns_not_found_when_parent_customer_does_not_exist() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("961004", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/creacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    42, "ISA", "1.25", "250", "1500.25", "1499.75",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Customer not found");
    assert_eq!(body["failCode"], "1");
}

#[tokio::test]
async fn retries_serialization_failures_and_then_succeeds() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "961005",
            customer_number: 45,
        },
    )
    .await;

    let outcome = creacc::create_account_with_hook(
        &pool,
        command("961005", 45),
        Arc::new(ForceRetryHook::new(1)),
    )
    .await
    .expect("retry should eventually succeed");

    let account = match outcome {
        creacc::CreateAccountOutcome::Success(account) => account,
        other => panic!("expected success outcome, got {other:?}"),
    };

    assert_eq!(account.account_number(), 1);
    let control =
        sqlx::query("SELECT account_count, account_last FROM control WHERE id = 'GLOBAL'")
            .fetch_one(&pool)
            .await
            .expect("control row must exist");
    assert_eq!(control.get::<i64, _>("account_count"), 1);
    assert_eq!(control.get::<i64, _>("account_last"), 1);
}

#[tokio::test]
async fn serialization_retry_exhaustion_surfaces_xrty() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "961006",
            customer_number: 46,
        },
    )
    .await;

    let error = creacc::create_account_with_hook(
        &pool,
        command("961006", 46),
        Arc::new(ForceRetryHook::new(db::DEFAULT_RETRY_ATTEMPTS)),
    )
    .await
    .expect_err("retry exhaustion must surface as an abend");

    match error {
        CbsaError::Abend(code, _) => assert_eq!(code, RETRY_EXHAUSTED_ABEND_CODE),
        other => panic!("expected XRTY abend, got {other:?}"),
    }
}

#[tokio::test]
async fn proctran_insert_failure_surfaces_hwpt_abend_and_rolls_back() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;
    let sortcode = "961007";
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode,
            customer_number: 47,
        },
    )
    .await;

    sqlx::query("ALTER TABLE proctran DROP CONSTRAINT IF EXISTS proctran_block_inserts")
        .execute(&pool)
        .await
        .expect("cleanup must succeed");

    let result = async {
        sqlx::query(
            "ALTER TABLE proctran ADD CONSTRAINT proctran_block_inserts CHECK (false) NOT VALID",
        )
        .execute(&pool)
        .await
        .expect("constraint must be added");

        cbsa::service::creacc::create(
            &pool,
            sortcode,
            CreaccRequest::new(
                47,
                "ISA".to_string(),
                Decimal::new(125, 2),
                Decimal::new(250, 0),
                Decimal::new(150_025, 2),
                Decimal::new(149_975, 2),
            )
            .expect("request must be valid"),
        )
        .await
    }
    .await;

    sqlx::query("ALTER TABLE proctran DROP CONSTRAINT IF EXISTS proctran_block_inserts")
        .execute(&pool)
        .await
        .expect("cleanup must succeed");

    match result.expect_err("PROCTRAN failure must return an error") {
        CbsaError::Abend(code, _) => assert_eq!(code, PROCTRAN_ABEND_CODE),
        other => panic!("expected HWPT abend, got {other:?}"),
    }

    let account_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM account")
        .fetch_one(&pool)
        .await
        .expect("account count query must succeed");
    let proctran_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran count query must succeed");
    let control =
        sqlx::query("SELECT account_count, account_last FROM control WHERE id = 'GLOBAL'")
            .fetch_one(&pool)
            .await
            .expect("control row must exist");

    assert_eq!(account_count, 0);
    assert_eq!(proctran_count, 0);
    assert_eq!(control.get::<i64, _>("account_count"), 0);
    assert_eq!(control.get::<i64, _>("account_last"), 0);
}

fn command(sortcode: &str, customer_number: i64) -> CreateAccountCommand {
    CreateAccountCommand {
        sortcode: sortcode.to_string(),
        customer_number,
        account_type: "ISA".to_string(),
        interest_rate: Decimal::new(125, 2),
        overdraft_limit: Decimal::new(250, 0),
        available_balance: Decimal::new(150_025, 2),
        actual_balance: Decimal::new(149_975, 2),
        opened: NaiveDate::from_ymd_opt(2026, 5, 1).expect("valid date"),
        last_statement_date: NaiveDate::from_ymd_opt(2026, 5, 1).expect("valid date"),
        next_statement_date: NaiveDate::from_ymd_opt(2026, 5, 31).expect("valid date"),
        transaction_reference: 1234567890,
        transaction_date: NaiveDate::from_ymd_opt(2026, 5, 1).expect("valid date"),
        transaction_time: NaiveTime::from_hms_opt(10, 15, 30).expect("valid time"),
    }
}

fn request_json(
    customer_number: i64,
    account_type: &str,
    interest_rate: &str,
    overdraft_limit: &str,
    available_balance: &str,
    actual_balance: &str,
) -> String {
    json!({
        "CreAcc": {
            "CommEyecatcher": "",
            "CommCustno": customer_number,
            "CommKey": {
                "CommSortcode": "000000",
                "CommNumber": 0
            },
            "CommAccType": account_type,
            "CommIntRt": interest_rate,
            "CommOpened": 0,
            "CommOverdrLim": overdraft_limit,
            "CommLastStmtDt": 0,
            "CommNextStmtDt": 0,
            "CommAvailBal": available_balance,
            "CommActBal": actual_balance,
            "CommSuccess": "",
            "CommFailCode": ""
        }
    })
    .to_string()
}

async fn insert_customer(pool: &PgPool, seed: CustomerSeed<'_>) {
    sqlx::query(
        r#"
        INSERT INTO customer (sortcode, customer_number, name, address, date_of_birth, credit_score, cs_review_date)
        VALUES ($1, $2, 'Dr Alice Example', '1 Main Street', '2000-01-10', 430, '2026-05-08')
        "#,
    )
    .bind(seed.sortcode)
    .bind(seed.customer_number)
    .execute(pool)
    .await
    .expect("customer insert must succeed");
}

async fn clean_database(pool: &PgPool) {
    sqlx::query("DELETE FROM account")
        .execute(pool)
        .await
        .expect("account cleanup must succeed");
    sqlx::query("DELETE FROM proctran")
        .execute(pool)
        .await
        .expect("proctran cleanup must succeed");
    sqlx::query("DELETE FROM customer")
        .execute(pool)
        .await
        .expect("customer cleanup must succeed");
    sqlx::query(
        "UPDATE control SET customer_count = 0, customer_last = 0, account_count = 0, account_last = 0 WHERE id = 'GLOBAL'",
    )
    .execute(pool)
    .await
    .expect("control reset must succeed");
}

async fn test_database() -> &'static TestDatabase {
    TEST_DATABASE
        .get_or_init(|| async {
            let container = CockroachDb::default().start().await.ok();
            let (host, port) = if let Some(container) = &container {
                (
                    container
                        .get_host()
                        .await
                        .expect("host must resolve")
                        .to_string(),
                    container
                        .get_host_port_ipv4(26257)
                        .await
                        .expect("port must resolve"),
                )
            } else {
                ("localhost".to_string(), 26257)
            };

            let admin_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&format!(
                    "postgres://root@{host}:{port}/defaultdb?sslmode=disable"
                ))
                .await
                .expect("admin pool must connect");
            sqlx::query("CREATE DATABASE IF NOT EXISTS cbsa")
                .execute(&admin_pool)
                .await
                .expect("cbsa database must exist");
            drop(admin_pool);

            let pool = PgPoolOptions::new()
                .max_connections(50)
                .connect(&format!(
                    "postgres://root@{host}:{port}/cbsa?sslmode=disable"
                ))
                .await
                .expect("application pool must connect");
            db::migrate(&pool).await.expect("migrations must apply");
            let database_url = format!("postgres://root@{host}:{port}/cbsa?sslmode=disable");
            drop(pool);

            TestDatabase {
                _container: container,
                database_url,
            }
        })
        .await
}

async fn test_pool() -> PgPool {
    let database = test_database().await;
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&database.database_url)
        .await
        .expect("test pool must connect")
}

fn app(sortcode: &str, pool: &PgPool) -> axum::Router {
    router(AppState {
        pool: pool.clone(),
        sortcode: sortcode.to_string(),
    })
}

fn content_type(response: &axum::response::Response) -> Option<&str> {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must be readable")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("body must be valid json")
}

fn cobol_date(raw: u64) -> NaiveDate {
    NaiveDate::parse_from_str(&format!("{raw:08}"), "%d%m%Y").expect("valid COBOL date")
}
