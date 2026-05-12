use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cbsa::db;
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
async fn transfers_funds_and_writes_one_transfer_audit_row() {
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
            customer_number: 101,
            account_number: 12_345_678,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::new(250, 0),
            last_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 2, 3).expect("valid date")),
            next_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 3, 4).expect("valid date")),
            available_balance: Decimal::new(50_000, 2),
            actual_balance: Decimal::new(50_000, 2),
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 202,
            account_number: 87_654_321,
            account_type: "SAVING",
            interest_rate: Decimal::new(125, 2),
            opened: NaiveDate::from_ymd_opt(2024, 4, 5).expect("valid date"),
            overdraft_limit: Decimal::new(100, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(15_000, 2),
            actual_balance: Decimal::new(15_000, 2),
        },
    )
    .await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/xfrfun")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    12_345_678, "987654", 87_654_321, "987654", "25.00",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("application/json"));

    let body = response_json(response).await;
    assert_eq!(body["XFRFUN"]["CommFaccno"], 12_345_678);
    assert_eq!(body["XFRFUN"]["CommFscode"], "987654");
    assert_eq!(body["XFRFUN"]["CommTaccno"], 87_654_321);
    assert_eq!(body["XFRFUN"]["CommTscode"], "987654");
    assert_eq!(body["XFRFUN"]["CommAmt"], "25.00");
    assert_eq!(body["XFRFUN"]["CommFavbal"], "475.00");
    assert_eq!(body["XFRFUN"]["CommFactbal"], "475.00");
    assert_eq!(body["XFRFUN"]["CommTavbal"], "175.00");
    assert_eq!(body["XFRFUN"]["CommTactbal"], "175.00");
    assert_eq!(body["XFRFUN"]["CommSuccess"], "Y");
    assert_eq!(body["XFRFUN"]["CommFailCode"], "0");

    assert_account_balances(&pool, 12_345_678, Decimal::new(47_500, 2)).await;
    assert_account_balances(&pool, 87_654_321, Decimal::new(17_500, 2)).await;

    let audit_rows =
        sqlx::query("SELECT tran_type, description, amount FROM proctran ORDER BY counter ASC")
            .fetch_all(&pool)
            .await
            .expect("audit rows must be readable");

    assert_eq!(audit_rows.len(), 1);
    assert_eq!(audit_rows[0].get::<String, _>("tran_type"), "TFR");
    assert_eq!(
        audit_rows[0].get::<String, _>("description"),
        format!("{:<26}98765487654321", "TRANSFER")
    );
    assert_eq!(
        audit_rows[0].get::<Decimal, _>("amount"),
        Decimal::new(2_500, 2)
    );
}

#[tokio::test]
async fn returns_not_found_problem_detail_for_missing_from_account() {
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
            account_number: 87_654_321,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::new(250, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(15_000, 2),
            actual_balance: Decimal::new(15_000, 2),
        },
    )
    .await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/xfrfun")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    12_345_678, "987654", 87_654_321, "987654", "25.00",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "From account not found");
    assert_eq!(body["failCode"], "1");

    let audit_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("audit count query must succeed");
    assert_eq!(audit_count, 0);
}

#[tokio::test]
async fn returns_not_found_problem_detail_for_missing_to_account_when_cobol_order_locks_to_first() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 404,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 404,
            account_number: 87_654_321,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::new(250, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(50_000, 2),
            actual_balance: Decimal::new(50_000, 2),
        },
    )
    .await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/xfrfun")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    87_654_321, "987654", 12_345_678, "987654", "25.00",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "To account not found");
    assert_eq!(body["failCode"], "2");
    assert_account_balances(&pool, 87_654_321, Decimal::new(50_000, 2)).await;
}

#[tokio::test]
async fn returns_conflict_problem_detail_for_insufficient_funds() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 505,
        },
    )
    .await;
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 606,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 505,
            account_number: 11_111_111,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::ZERO,
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(1_000, 2),
            actual_balance: Decimal::new(1_000, 2),
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 606,
            account_number: 22_222_222,
            account_type: "SAVING",
            interest_rate: Decimal::new(125, 2),
            opened: NaiveDate::from_ymd_opt(2024, 4, 5).expect("valid date"),
            overdraft_limit: Decimal::new(100, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(15_000, 2),
            actual_balance: Decimal::new(15_000, 2),
        },
    )
    .await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/xfrfun")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    11_111_111, "987654", 22_222_222, "987654", "25.00",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Insufficient funds");
    assert_eq!(body["failCode"], "3");
    assert_account_balances(&pool, 11_111_111, Decimal::new(1_000, 2)).await;
    assert_account_balances(&pool, 22_222_222, Decimal::new(15_000, 2)).await;

    let audit_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("audit count query must succeed");
    assert_eq!(audit_count, 0);
}

#[tokio::test]
async fn same_account_transfer_returns_same_abend_problem_detail() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/xfrfun")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    12_345_678, "987654", 12_345_678, "987654", "25.00",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Abend");
    assert_eq!(body["abendCode"], "SAME");
}

#[tokio::test]
async fn zero_amount_returns_business_validation_problem_detail() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/xfrfun")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    12_345_678, "987654", 87_654_321, "987654", "0.00",
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Invalid transfer amount");
    assert_eq!(body["failCode"], "4");
}

#[tokio::test]
async fn amount_scale_validation_failures_remain_problem_details() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/xfrfun")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    12_345_678, "987654", 87_654_321, "987654", "25.001",
                )))
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
async fn proctran_insert_failure_returns_hwpt_problem_and_rolls_back_both_accounts() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 707,
        },
    )
    .await;
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode: "987654",
            customer_number: 808,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 707,
            account_number: 33_333_333,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::new(250, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(50_000, 2),
            actual_balance: Decimal::new(50_000, 2),
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode: "987654",
            customer_number: 808,
            account_number: 44_444_444,
            account_type: "SAVING",
            interest_rate: Decimal::new(125, 2),
            opened: NaiveDate::from_ymd_opt(2024, 4, 5).expect("valid date"),
            overdraft_limit: Decimal::new(100, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(15_000, 2),
            actual_balance: Decimal::new(15_000, 2),
        },
    )
    .await;

    sqlx::query("ALTER TABLE proctran DROP CONSTRAINT IF EXISTS proctran_block_inserts")
        .execute(&pool)
        .await
        .expect("cleanup must succeed");

    let response = async {
        sqlx::query(
            "ALTER TABLE proctran ADD CONSTRAINT proctran_block_inserts CHECK (false) NOT VALID",
        )
        .execute(&pool)
        .await
        .expect("constraint must be added");

        app("987654", &pool)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/xfrfun")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(request_json(
                        33_333_333, "987654", 44_444_444, "987654", "25.00",
                    )))
                    .expect("request must build"),
            )
            .await
            .expect("router must respond")
    }
    .await;

    sqlx::query("ALTER TABLE proctran DROP CONSTRAINT IF EXISTS proctran_block_inserts")
        .execute(&pool)
        .await
        .expect("cleanup must succeed");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Abend");
    assert_eq!(body["abendCode"], "HWPT");

    assert_account_balances(&pool, 33_333_333, Decimal::new(50_000, 2)).await;
    assert_account_balances(&pool, 44_444_444, Decimal::new(15_000, 2)).await;

    let audit_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("audit count query must succeed");
    assert_eq!(audit_count, 0);
}

async fn assert_account_balances(pool: &PgPool, account_number: i64, expected_balance: Decimal) {
    let account = sqlx::query(
        "SELECT available_balance, actual_balance FROM account WHERE sortcode = $1 AND account_number = $2",
    )
    .bind("987654")
    .bind(account_number)
    .fetch_one(pool)
    .await
    .expect("account query must succeed");

    assert_eq!(
        account.get::<Decimal, _>("available_balance"),
        expected_balance
    );
    assert_eq!(
        account.get::<Decimal, _>("actual_balance"),
        expected_balance
    );
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

fn request_json(
    from_account_number: i64,
    from_sortcode: &str,
    to_account_number: i64,
    to_sortcode: &str,
    amount: &str,
) -> String {
    format!(
        r#"{{"XFRFUN":{{"CommFaccno":{from_account_number},"CommFscode":"{from_sortcode}","CommTaccno":{to_account_number},"CommTscode":"{to_sortcode}","CommAmt":{amount}}}}}"#
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
    cbsa::web::router(cbsa::web::AppState {
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
