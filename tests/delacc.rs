use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cbsa::{
    db,
    error::{CbsaError, PROCTRAN_ABEND_CODE},
    service::delacc::{self, DelaccRequest},
    web::{router, AppState},
};
use chrono::NaiveDate;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
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

struct AccountSeed<'a> {
    sortcode: &'a str,
    customer_number: i64,
    account_number: i64,
    account_type: &'a str,
    interest_rate: Decimal,
    opened: NaiveDate,
    overdraft_limit: Decimal,
    last_stmt_date: Option<NaiveDate>,
    next_stmt_date: Option<NaiveDate>,
    available_balance: Decimal,
    actual_balance: Decimal,
}

#[tokio::test]
async fn deletes_account_and_writes_expected_proctran_row() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 101,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 101,
            account_number: 12_345_678,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::new(250, 0),
            last_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 2, 3).expect("valid date")),
            next_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 3, 4).expect("valid date")),
            available_balance: Decimal::new(150_025, 2),
            actual_balance: Decimal::new(149_975, 2),
        },
    )
    .await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/delacc/12345678")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(Some(12_345_678), Some("987654"))))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("application/json"));

    let body = response_json(response).await;
    assert_eq!(body["DelAcc"]["DelAccEye"], "ACCT");
    assert_eq!(body["DelAcc"]["DelAccCustno"], "0000000101");
    assert_eq!(body["DelAcc"]["DelAccScode"], "987654");
    assert_eq!(body["DelAcc"]["DelAccAccno"], 12_345_678);
    assert_eq!(body["DelAcc"]["DelAccAccType"], "ISA");
    assert_eq!(body["DelAcc"]["DelAccIntRate"], "1.50");
    assert_eq!(body["DelAcc"]["DelAccOpened"], 2_012_024);
    assert_eq!(body["DelAcc"]["DelAccOverdraft"], "250.00");
    assert_eq!(body["DelAcc"]["DelAccLastStmtDt"], 3_022_024);
    assert_eq!(body["DelAcc"]["DelAccNextStmtDt"], 4_032_024);
    assert_eq!(body["DelAcc"]["DelAccAvailBal"], "1500.25");
    assert_eq!(body["DelAcc"]["DelAccActualBal"], "1499.75");
    assert_eq!(body["DelAcc"]["DelAccSuccess"], "Y");
    assert_eq!(body["DelAcc"]["DelAccFailCd"], "0");
    assert_eq!(body["DelAcc"]["DelAccDelSuccess"], "Y");

    let account_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM account WHERE sortcode = $1 AND account_number = $2",
    )
    .bind("987654")
    .bind(12_345_678_i64)
    .fetch_one(&pool)
    .await
    .expect("account count query must succeed");
    assert_eq!(account_count, 0);

    let proctran = sqlx::query("SELECT tran_type, description, amount FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran row must exist");
    assert_eq!(proctran.get::<String, _>("tran_type"), "ODA");
    assert_eq!(
        proctran.get::<String, _>("description"),
        "0000000101ISA     0302202404032024DELETE"
    );
    assert_eq!(
        proctran.get::<Decimal, _>("amount"),
        Decimal::new(149_975, 2)
    );
}

#[tokio::test]
async fn returns_not_found_problem_detail_for_missing_account() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/delacc/12345678")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(Some(12_345_678), Some("987654"))))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Account not found");
    assert_eq!(body["failCode"], "1");
}

#[tokio::test]
async fn returns_problem_detail_for_body_sortcode_mismatch() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/delacc/12345678")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(Some(12_345_678), Some("123456"))))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Validation failed");
    assert_eq!(
        body["detail"],
        "Body DelAccScode does not match the configured branch sortcode."
    );
}

#[tokio::test]
async fn missing_statement_dates_are_zero_filled_in_oda_description() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 202,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 202,
            account_number: 23_456_789,
            account_type: "SAVING",
            interest_rate: Decimal::new(125, 2),
            opened: NaiveDate::from_ymd_opt(2024, 4, 5).expect("valid date"),
            overdraft_limit: Decimal::new(100, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(100_000, 2),
            actual_balance: Decimal::new(99_500, 2),
        },
    )
    .await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/delacc/23456789")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(Some(23_456_789), Some(""))))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);

    let description = sqlx::query_scalar::<_, String>("SELECT description FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran row must exist");
    assert_eq!(description, "0000000202SAVING  0000000000000000DELETE");
}

#[tokio::test]
async fn proctran_insert_failure_surfaces_hwpt_abend_and_rolls_back_delete() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 303,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 303,
            account_number: 34_567_890,
            account_type: "SAVING",
            interest_rate: Decimal::new(175, 2),
            opened: NaiveDate::from_ymd_opt(2024, 5, 6).expect("valid date"),
            overdraft_limit: Decimal::new(100, 0),
            last_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 6, 7).expect("valid date")),
            next_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 7, 8).expect("valid date")),
            available_balance: Decimal::new(250_000, 2),
            actual_balance: Decimal::new(249_500, 2),
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

        delacc::delete(
            &pool,
            "987654",
            DelaccRequest {
                account_number: 34_567_890,
            },
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

    let account = sqlx::query(
        "SELECT customer_number, account_type FROM account WHERE sortcode = $1 AND account_number = $2",
    )
    .bind("987654")
    .bind(34_567_890_i64)
    .fetch_one(&pool)
    .await
    .expect("seeded account must remain");
    assert_eq!(account.get::<i64, _>("customer_number"), 303);
    assert_eq!(account.get::<String, _>("account_type"), "SAVING");

    let proctran_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran count query must succeed");
    assert_eq!(proctran_count, 0);
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
    sqlx::query("UPDATE control SET customer_count = 0, customer_last = 0, account_count = 0, account_last = 0 WHERE id = 'GLOBAL'")
        .execute(pool)
        .await
        .expect("control reset must succeed");
}

async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body must collect")
        .to_bytes();
    serde_json::from_slice(&body).expect("response body must be valid json")
}

fn content_type(response: &axum::response::Response) -> Option<&str> {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
}

fn request_json(account_number: Option<i64>, sortcode: Option<&str>) -> String {
    let account_number = account_number
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let sortcode = match sortcode {
        Some(value) => format!("\"{value}\""),
        None => "null".to_string(),
    };

    format!(
        r#"{{"DelAcc":{{"DelAccEye":"ACCT","DelAccCustno":"0000000101","DelAccScode":{sortcode},"DelAccAccno":{account_number},"DelAccAccType":"ISA","DelAccIntRate":"1.50","DelAccOpened":2012024,"DelAccOverdraft":"250.00","DelAccLastStmtDt":3022024,"DelAccNextStmtDt":4032024,"DelAccAvailBal":"1500.25","DelAccActualBal":"1499.75","DelAccSuccess":" ","DelAccFailCd":" ","DelAccDelSuccess":" ","DelAccDelFailCd":" ","DelAccDelApplid":"","DelAccDelPcb1":"","DelAccDelPcb2":"","DelAccDelPcb3":""}}}}"#
    )
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

async fn insert_customer(pool: &PgPool, customer: CustomerSeed<'_>) {
    sqlx::query(
        r#"
        INSERT INTO customer (sortcode, customer_number, name, address, date_of_birth, credit_score, cs_review_date)
        VALUES ($1, $2, 'Test Customer', '1 Test Street', '1990-01-02', 700, '2025-01-03')
        "#,
    )
    .bind(customer.sortcode)
    .bind(customer.customer_number)
    .execute(pool)
    .await
    .expect("customer seed insert must succeed");
}

async fn insert_account(pool: &PgPool, account: AccountSeed<'_>) {
    sqlx::query(
        r#"
        INSERT INTO account (
            sortcode,
            account_number,
            customer_number,
            account_type,
            interest_rate,
            opened,
            overdraft_limit,
            last_stmt_date,
            next_stmt_date,
            available_balance,
            actual_balance
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        "#,
    )
    .bind(account.sortcode)
    .bind(account.account_number)
    .bind(account.customer_number)
    .bind(account.account_type)
    .bind(account.interest_rate)
    .bind(account.opened)
    .bind(account.overdraft_limit)
    .bind(account.last_stmt_date)
    .bind(account.next_stmt_date)
    .bind(account.available_balance)
    .bind(account.actual_balance)
    .execute(pool)
    .await
    .expect("account seed insert must succeed");
}
