use axum::{body::Body, http::Request, http::StatusCode};
use cbsa::{
    db,
    web::{router, AppState},
};
use chrono::NaiveDate;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use testcontainers_modules::{
    cockroach_db::CockroachDb,
    testcontainers::{runners::AsyncRunner, ContainerAsync},
};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

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

static TEST_DATABASE: OnceCell<TestDatabase> = OnceCell::const_new();
static TEST_MUTEX: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn returns_all_accounts_for_a_customer_in_account_number_order() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let sortcode = "870707";
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode,
            customer_number: 70,
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode,
            customer_number: 70,
            account_number: 12_000_002,
            account_type: "SAVER",
            interest_rate: Decimal::new(175, 2),
            opened: NaiveDate::from_ymd_opt(2024, 4, 5).expect("valid date"),
            overdraft_limit: Decimal::new(0, 0),
            last_stmt_date: None,
            next_stmt_date: None,
            available_balance: Decimal::new(245055, 2),
            actual_balance: Decimal::new(245055, 2),
        },
    )
    .await;
    insert_account(
        &pool,
        AccountSeed {
            sortcode,
            customer_number: 70,
            account_number: 12_000_001,
            account_type: "ISA",
            interest_rate: Decimal::new(150, 2),
            opened: NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            overdraft_limit: Decimal::new(25000, 2),
            last_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 2, 3).expect("valid date")),
            next_stmt_date: Some(NaiveDate::from_ymd_opt(2024, 3, 4).expect("valid date")),
            available_balance: Decimal::new(150025, 2),
            actual_balance: Decimal::new(149975, 2),
        },
    )
    .await;

    let response = app(sortcode, &pool)
        .oneshot(
            Request::builder()
                .uri("/api/v1/inqacccu/70")
                .body(Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("application/json"));

    let body = response_json(response).await;
    assert_eq!(body["customer_number"], 70);
    assert_eq!(body["inquiry_success"], "Y");
    assert_eq!(body["fail_code"], "0");
    assert_eq!(body["customer_found"], "Y");
    assert_eq!(body["pcb_pointer"], "");

    let accounts = body["account_details"]
        .as_array()
        .expect("account_details must be an array");
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0]["account_number"], 12_000_001);
    assert_eq!(accounts[0]["interest_rate"], "1.50");
    assert_eq!(accounts[0]["overdraft"], "250.00");
    assert_eq!(accounts[0]["available_balance"], "1500.25");
    assert_eq!(accounts[0]["actual_balance"], "1499.75");
    assert_eq!(accounts[0]["opened"]["day"], 2);
    assert_eq!(accounts[0]["last_statement_date"]["month"], 2);
    assert_eq!(accounts[0]["next_statement_date"]["year"], 2024);
    assert_eq!(accounts[1]["account_number"], 12_000_002);
    assert_eq!(accounts[1]["eye"], "ACCT");
    assert_eq!(accounts[1]["last_statement_date"]["year"], 0);
    assert_eq!(accounts[1]["next_statement_date"]["year"], 0);
}

#[tokio::test]
async fn returns_empty_account_list_when_customer_has_no_accounts() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let sortcode = "880808";
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode,
            customer_number: 80,
        },
    )
    .await;

    let response = app(sortcode, &pool)
        .oneshot(
            Request::builder()
                .uri("/api/v1/inqacccu/80")
                .body(Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["inquiry_success"], "Y");
    assert_eq!(body["fail_code"], "0");
    assert_eq!(body["customer_found"], "Y");
    assert_eq!(body["account_details"], Value::Array(vec![]));
}

#[tokio::test]
async fn returns_not_found_when_customer_does_not_exist() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let response = app("890909", &pool)
        .oneshot(
            Request::builder()
                .uri("/api/v1/inqacccu/999")
                .body(Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(content_type(&response), Some("application/problem+json"));

    let body = response_json(response).await;
    assert_eq!(body["title"], "Customer not found");
    assert_eq!(body["failCode"], "1");
    assert_eq!(body["detail"], "Customer number 999 was not found.");
}

#[tokio::test]
async fn reserved_boundary_customer_numbers_return_not_found_even_when_present() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let sortcode = "891891";

    for (customer_number, account_number) in [(0, 12_300_000), (9_999_999_999, 12_300_001)] {
        insert_customer(
            &pool,
            CustomerSeed {
                sortcode,
                customer_number,
            },
        )
        .await;
        insert_account(
            &pool,
            AccountSeed {
                sortcode,
                customer_number,
                account_number,
                account_type: "CHKING",
                interest_rate: Decimal::new(25, 2),
                opened: NaiveDate::from_ymd_opt(2024, 5, 6).expect("valid date"),
                overdraft_limit: Decimal::new(10000, 2),
                last_stmt_date: None,
                next_stmt_date: None,
                available_balance: Decimal::new(5000, 2),
                actual_balance: Decimal::new(5000, 2),
            },
        )
        .await;

        let response = app(sortcode, &pool)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/inqacccu/{customer_number}"))
                    .body(Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(content_type(&response), Some("application/problem+json"));

        let body = response_json(response).await;
        assert_eq!(body["title"], "Customer not found");
        assert_eq!(body["failCode"], "1");
        assert_eq!(
            body["detail"],
            format!("Customer number {customer_number} was not found.")
        );
    }
}

#[tokio::test]
async fn rejects_customer_numbers_outside_the_copybook_range() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let response = app("900900", &pool)
        .oneshot(
            Request::builder()
                .uri("/api/v1/inqacccu/10000000000")
                .body(Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));

    let body = response_json(response).await;
    assert_eq!(body["title"], "Validation failed");
    assert_eq!(body["status"], 400);
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
                        .expect("container host must resolve")
                        .to_string(),
                    container
                        .get_host_port_ipv4(26257)
                        .await
                        .expect("cockroach sql port must be exposed"),
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
        INSERT INTO customer (
            sortcode,
            customer_number,
            name,
            address,
            date_of_birth,
            credit_score,
            cs_review_date
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (sortcode, customer_number) DO NOTHING
        "#,
    )
    .bind(customer.sortcode)
    .bind(customer.customer_number)
    .bind(format!("Example Customer {}", customer.customer_number))
    .bind(format!("{} Example Road", customer.customer_number))
    .bind(NaiveDate::from_ymd_opt(1990, 1, 1).expect("valid date"))
    .bind(500_i16)
    .bind(Some(
        NaiveDate::from_ymd_opt(2025, 1, 1).expect("valid date"),
    ))
    .execute(pool)
    .await
    .expect("customer insert must succeed");
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
        ON CONFLICT (sortcode, account_number) DO UPDATE
        SET customer_number = EXCLUDED.customer_number,
            account_type = EXCLUDED.account_type,
            interest_rate = EXCLUDED.interest_rate,
            opened = EXCLUDED.opened,
            overdraft_limit = EXCLUDED.overdraft_limit,
            last_stmt_date = EXCLUDED.last_stmt_date,
            next_stmt_date = EXCLUDED.next_stmt_date,
            available_balance = EXCLUDED.available_balance,
            actual_balance = EXCLUDED.actual_balance
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
    .expect("account insert must succeed");
}

fn content_type(response: &axum::response::Response) -> Option<&str> {
    response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body must be readable")
        .to_bytes();

    serde_json::from_slice(&body).expect("response body must be valid json")
}
