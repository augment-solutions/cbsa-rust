use axum::{body::Body, http::Request, http::StatusCode};
use cbsa::{
    db,
    service::inqcust,
    web::{router, AppState},
};
use chrono::NaiveDate;
use http_body_util::BodyExt;
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
    name: &'a str,
    address: &'a str,
    date_of_birth: NaiveDate,
    credit_score: i16,
    cs_review_date: Option<NaiveDate>,
}

static TEST_DATABASE: OnceCell<TestDatabase> = OnceCell::const_new();
static TEST_MUTEX: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn returns_customer_for_successful_lookup() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let sortcode = "012345";
    insert_customer(
        &pool,
        CustomerSeed {
            sortcode,
            customer_number: 10,
            name: "Dr William Q Price",
            address: "19 Nutmeg Grove, Durham",
            date_of_birth: NaiveDate::from_ymd_opt(1936, 9, 24).expect("valid date"),
            credit_score: 263,
            cs_review_date: Some(NaiveDate::from_ymd_opt(2022, 2, 9).expect("valid date")),
        },
    )
    .await;

    let response = app(sortcode, &pool)
        .oneshot(
            Request::builder()
                .uri("/api/v1/inqcust/10")
                .body(Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json"),
    );

    let body = response_json(response).await;
    assert_eq!(body["eye"], "CUST");
    assert_eq!(body["sortcode"], sortcode);
    assert_eq!(body["customer_number"], 10);
    assert_eq!(body["name"], "Dr William Q Price");
    assert_eq!(body["date_of_birth"]["day"], 24);
    assert_eq!(body["credit_score_review_date"]["year"], 2022);
    assert_eq!(body["inquiry_success"], "Y");
    assert_eq!(body["fail_code"], "0");
    assert_eq!(body["pcb_pointer"], "");
}

#[tokio::test]
async fn returns_not_found_for_missing_customer() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let response = app("123456", &pool)
        .oneshot(
            Request::builder()
                .uri("/api/v1/inqcust/1234567890")
                .body(Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json"),
    );

    let body = response_json(response).await;
    assert_eq!(body["title"], "Customer not found");
    assert_eq!(body["failCode"], "1");
    assert_eq!(body["detail"], "Customer number 1234567890 was not found.");
}

#[tokio::test]
async fn returns_not_found_when_no_customers_exist_for_special_lookup_modes() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;

    for customer_number in [0, 9_999_999_999_i64] {
        let response = app("223344", &pool)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/inqcust/{customer_number}"))
                    .body(Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response_json(response).await;
        assert_eq!(body["failCode"], "1");
        assert_eq!(body["detail"], "No customers exist.");
    }
}

#[tokio::test]
async fn rejects_customer_numbers_outside_the_copybook_range() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    let response = app("334455", &pool)
        .oneshot(
            Request::builder()
                .uri("/api/v1/inqcust/10000000000")
                .body(Body::empty())
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_json(response).await;
    assert_eq!(body["title"], "Validation failed");
    assert_eq!(body["status"], 400);
}

#[test]
fn marks_random_retry_exhausted_failures() {
    let result = inqcust::InqcustResult::failure(
        "R",
        "Unable to find a random customer after exhausting retry attempts.",
    );

    assert!(!result.inquiry_success());
    assert!(result.is_random_retry_exhausted_failure());
    assert_eq!(result.fail_code(), "R");
    assert_eq!(
        result.message(),
        Some("Unable to find a random customer after exhausting retry attempts."),
    );
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
        ON CONFLICT (sortcode, customer_number) DO UPDATE
        SET name = EXCLUDED.name,
            address = EXCLUDED.address,
            date_of_birth = EXCLUDED.date_of_birth,
            credit_score = EXCLUDED.credit_score,
            cs_review_date = EXCLUDED.cs_review_date
        "#,
    )
    .bind(customer.sortcode)
    .bind(customer.customer_number)
    .bind(customer.name)
    .bind(customer.address)
    .bind(customer.date_of_birth)
    .bind(customer.credit_score)
    .bind(customer.cs_review_date)
    .execute(pool)
    .await
    .expect("customer insert must succeed");
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
