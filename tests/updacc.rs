use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cbsa::{
    db,
    error::{CbsaError, PROCTRAN_ABEND_CODE},
    service::updacc::{self, UpdaccRequest},
    web::{router, updacc::UpdaccRequestDto, AppState},
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
async fn updates_account_and_writes_proctran() {
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
                .method("PUT")
                .uri("/api/v1/updacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json("MORTGAGE", 2.25, 500)))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("application/json"));

    let body = response_json(response).await;
    assert_eq!(body["UpdAcc"]["CommEye"], "ACCT");
    assert_eq!(body["UpdAcc"]["CommCustno"], "0000000101");
    assert_eq!(body["UpdAcc"]["CommScode"], "987654");
    assert_eq!(body["UpdAcc"]["CommAccno"], 12_345_678);
    assert_eq!(body["UpdAcc"]["CommAccType"], "MORTGAGE");
    assert_eq!(body["UpdAcc"]["CommIntRate"], "2.25");
    assert_eq!(body["UpdAcc"]["CommOverdraft"], "500.00");
    assert_eq!(body["UpdAcc"]["CommOpened"], 2_012_024);
    assert_eq!(body["UpdAcc"]["CommLastStmtDt"], 3_022_024);
    assert_eq!(body["UpdAcc"]["CommNextStmtDt"], 4_032_024);
    assert_eq!(body["UpdAcc"]["CommAvailBal"], "1500.25");
    assert_eq!(body["UpdAcc"]["CommActualBal"], "1499.75");
    assert_eq!(body["UpdAcc"]["CommSuccess"], "Y");

    let account = sqlx::query(
        "SELECT account_type, interest_rate, overdraft_limit, available_balance, actual_balance FROM account WHERE sortcode = $1 AND account_number = $2",
    )
    .bind("987654")
    .bind(12_345_678_i64)
    .fetch_one(&pool)
    .await
    .expect("updated account must exist");
    assert_eq!(account.get::<String, _>("account_type"), "MORTGAGE");
    assert_eq!(
        account.get::<Decimal, _>("interest_rate"),
        Decimal::new(225, 2)
    );
    assert_eq!(
        account.get::<Decimal, _>("overdraft_limit"),
        Decimal::new(50000, 2)
    );
    assert_eq!(
        account.get::<Decimal, _>("available_balance"),
        Decimal::new(150_025, 2)
    );
    assert_eq!(
        account.get::<Decimal, _>("actual_balance"),
        Decimal::new(149_975, 2)
    );

    let proctran = sqlx::query("SELECT tran_type, description, amount FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran row must exist");
    assert_eq!(proctran.get::<String, _>("tran_type"), "OUA");
    assert_eq!(proctran.get::<Decimal, _>("amount"), Decimal::ZERO);
    assert_eq!(proctran.get::<String, _>("description").len(), 40);
}

#[tokio::test]
async fn max_overdraft_limit_keeps_oua_description_within_copybook_width() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 111,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 111,
            account_number: 12_345_678,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::new(0, 0),
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
                .method("PUT")
                .uri("/api/v1/updacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json("MORTGAGE", 2.25, 99_999_999)))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);

    let description = sqlx::query_scalar::<_, String>("SELECT description FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran row must exist");
    assert_eq!(description.len(), 40);
    assert_eq!(description, "12345678MORTGAGE00022599999999          ");
}

#[test]
fn json_number_money_fields_preserve_decimal_precision() {
    let request: UpdaccRequestDto = serde_json::from_str(
        r#"{"UpdAcc":{"CommEye":"ACCT","CommCustno":"0000000101","CommScode":"987654","CommAccno":12345678,"CommAccType":"MORTGAGE","CommIntRate":0.1,"CommOpened":2012024,"CommOverdraft":0.1,"CommLastStmtDt":3022024,"CommNextStmtDt":4032024,"CommAvailBal":1500.25,"CommActualBal":1499.75,"CommSuccess":" "}}"#,
    )
    .expect("request must deserialize");

    let commarea = request.updacc.expect("UpdAcc must be present");
    assert_eq!(commarea.comm_int_rate.as_deref(), Some("0.1"));
    assert_eq!(commarea.comm_overdraft.as_deref(), Some("0.1"));
    assert_eq!(commarea.comm_avail_bal.as_deref(), Some("1500.25"));
    assert_eq!(commarea.comm_actual_bal.as_deref(), Some("1499.75"));
}

#[tokio::test]
async fn returns_not_found_problem_detail_for_missing_account() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/updacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json("MORTGAGE", 2.25, 500)))
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
async fn returns_problem_detail_for_invalid_account_type() {
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
            account_type: "CURR",
            interest_rate: Decimal::new(125, 2),
            opened: NaiveDate::from_ymd_opt(2024, 4, 5).expect("valid date"),
            overdraft_limit: Decimal::new(0, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(100_000, 2),
            actual_balance: Decimal::new(100_000, 2),
        },
    )
    .await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/updacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(" ISA", 2.25, 500)))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Invalid account type");
    assert_eq!(body["failCode"], "2");
}

#[tokio::test]
async fn request_validation_failures_remain_problem_details() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/updacc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(missing_account_number_json()))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Validation failed");
}

#[tokio::test]
async fn proctran_insert_failure_surfaces_hwpt_abend_and_rolls_back_update() {
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

        updacc::update(
            &pool,
            "987654",
            UpdaccRequest {
                account_number: 34_567_890,
                account_type: "MORTGAGE".to_string(),
                interest_rate: Decimal::new(225, 2),
                overdraft_limit: Decimal::new(500, 0),
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
        "SELECT account_type, interest_rate, overdraft_limit FROM account WHERE sortcode = $1 AND account_number = $2",
    )
    .bind("987654")
    .bind(34_567_890_i64)
    .fetch_one(&pool)
    .await
    .expect("seeded account must remain");
    assert_eq!(account.get::<String, _>("account_type"), "SAVING");
    assert_eq!(
        account.get::<Decimal, _>("interest_rate"),
        Decimal::new(175, 2)
    );
    assert_eq!(
        account.get::<Decimal, _>("overdraft_limit"),
        Decimal::new(10000, 2)
    );

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

fn request_json(account_type: &str, interest_rate: f64, overdraft: i64) -> String {
    format!(
        r#"{{"UpdAcc":{{"CommEye":"ACCT","CommCustno":"0000000101","CommScode":"987654","CommAccno":12345678,"CommAccType":"{account_type}","CommIntRate":{interest_rate},"CommOpened":2012024,"CommOverdraft":{overdraft},"CommLastStmtDt":3022024,"CommNextStmtDt":4032024,"CommAvailBal":1500.25,"CommActualBal":1499.75,"CommSuccess":" "}}}}"#
    )
}

fn missing_account_number_json() -> &'static str {
    r#"{"UpdAcc":{"CommAccType":"MORTGAGE","CommIntRate":2.25,"CommOverdraft":500}}"#
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
