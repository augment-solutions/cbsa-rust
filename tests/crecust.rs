use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cbsa::{
    db,
    error::{CbsaError, PROCTRAN_ABEND_CODE},
    service::crecust::{self, CrecustRequest},
    web::{router, AppState},
};
use http_body_util::BodyExt;
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

#[tokio::test]
async fn creates_customer_updates_control_and_writes_proctran() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;
    let sortcode = "987654";

    let response = app(sortcode, &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/crecust")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    "Dr Alice Example",
                    "1 Main Street",
                    10_012_000,
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), Some("application/json"));

    let body = response_json(response).await;
    assert_eq!(body["CreCust"]["CommEyecatcher"], "CUST");
    assert_eq!(body["CreCust"]["CommKey"]["CommSortcode"], sortcode);
    assert_eq!(body["CreCust"]["CommKey"]["CommNumber"], 1);
    assert_eq!(body["CreCust"]["CommName"], "Dr Alice Example");
    assert_eq!(body["CreCust"]["CommAddress"], "1 Main Street");

    let credit_score = body["CreCust"]["CommCreditScore"]
        .as_u64()
        .expect("credit score must be numeric");
    assert!((1..=998).contains(&credit_score));

    let customer_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM customer")
        .fetch_one(&pool)
        .await
        .expect("customer count query must succeed");
    let proctran_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran count query must succeed");
    assert_eq!(customer_count, 1);
    assert_eq!(proctran_count, 1);

    let control =
        sqlx::query("SELECT customer_count, customer_last FROM control WHERE id = 'GLOBAL'")
            .fetch_one(&pool)
            .await
            .expect("control row must exist");
    assert_eq!(control.get::<i64, _>("customer_count"), 1);
    assert_eq!(control.get::<i64, _>("customer_last"), 1);

    let proctran = sqlx::query("SELECT tran_type, description FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran row must exist");
    assert_eq!(proctran.get::<String, _>("tran_type"), "OCC");
    assert!(proctran
        .get::<String, _>("description")
        .starts_with("9876540000000001Dr Alice Examp10/01/2000"));
}

#[tokio::test]
async fn rejects_missing_and_blank_name_with_problem_detail() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    for payload in [
        r#"{"CreCust":{"CommKey":{"CommSortcode":"000000","CommNumber":0},"CommAddress":"1 Main Street","CommDateOfBirth":10012000}}"#,
        &request_json("   ", "1 Main Street", 10_012_000),
    ] {
        let response = app("987654", &pool)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/crecust")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
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
async fn rejects_out_of_range_request_fields_with_problem_detail() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/crecust")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"CreCust":{"CommKey":{"CommSortcode":"000000","CommNumber":0},"CommName":"Dr Alice Example","CommAddress":"1 Main Street","CommDateOfBirth":100000000}}"#,
                ))
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
async fn returns_problem_detail_for_invalid_customer_title() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/crecust")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    "Alice Example",
                    "1 Main Street",
                    10_012_000,
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Invalid customer title");
    assert_eq!(body["failCode"], "T");
}

#[tokio::test]
async fn returns_problem_detail_for_invalid_calendar_date() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

    let response = app("987654", &pool)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/crecust")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json(
                    "Dr Alice Example",
                    "1 Main Street",
                    31_022_000,
                )))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Invalid date of birth");
    assert_eq!(body["failCode"], "Z");
}

#[tokio::test]
async fn proctran_insert_failure_surfaces_hwpt_abend_and_rolls_back() {
    let _guard = TEST_MUTEX.lock().await;
    let pool = test_pool().await;
    clean_database(&pool).await;

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

        crecust::create(
            &pool,
            "987654",
            CrecustRequest {
                name: "Dr Alice Example".to_string(),
                address: "1 Main Street".to_string(),
                date_of_birth: 10_012_000,
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

    let customer_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM customer")
        .fetch_one(&pool)
        .await
        .expect("customer count query must succeed");
    let proctran_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proctran")
        .fetch_one(&pool)
        .await
        .expect("proctran count query must succeed");
    let control =
        sqlx::query("SELECT customer_count, customer_last FROM control WHERE id = 'GLOBAL'")
            .fetch_one(&pool)
            .await
            .expect("control row must exist");

    assert_eq!(customer_count, 0);
    assert_eq!(proctran_count, 0);
    assert_eq!(control.get::<i64, _>("customer_count"), 0);
    assert_eq!(control.get::<i64, _>("customer_last"), 0);
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

fn request_json(name: &str, address: &str, date_of_birth: i32) -> String {
    format!(
        r#"{{"CreCust":{{"CommEyecatcher":"CUST","CommKey":{{"CommSortcode":"000000","CommNumber":0}},"CommName":"{name}","CommAddress":"{address}","CommDateOfBirth":{date_of_birth},"CommCreditScore":0,"CommCsReviewDate":0,"CommSuccess":" ","CommFailCode":" "}}}}"#
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
