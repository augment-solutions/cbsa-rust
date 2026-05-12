use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use cbsa::web::{router, AppState};
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

#[tokio::test]
async fn returns_successful_response_for_every_agency_route() {
    let app = app();

    for agency_number in 1..=5 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/crdtagy/{agency_number}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(request_json()))
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let score = body["CreCust"]["CommCreditScore"]
            .as_u64()
            .expect("credit score must be numeric");
        assert!((1..=998).contains(&score));
        assert_eq!(body["CreCust"]["CommKey"]["CommSortcode"], "987654");
        assert_eq!(body["CreCust"]["CommSuccess"], "Y");
        assert_eq!(body["CreCust"]["CommFailCode"], "0");
    }
}

#[tokio::test]
async fn rejects_out_of_range_agency_numbers_with_problem_detail() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/crdtagy/6")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_json()))
                .expect("request must build"),
        )
        .await
        .expect("router must respond");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(content_type(&response), Some("application/problem+json"));
    let body = response_json(response).await;
    assert_eq!(body["title"], "Validation failed");
}

fn app() -> axum::Router {
    router(AppState {
        pool: PgPoolOptions::new()
            .connect_lazy("postgres://root@localhost:26257/cbsa?sslmode=disable")
            .expect("lazy pool must be created"),
        sortcode: "987654".to_string(),
    })
}

fn request_json() -> String {
    r#"{"CreCust":{"CommEyecatcher":"CUST","CommKey":{"CommSortcode":"987654","CommNumber":42},"CommName":"Dr Alice Example","CommAddress":"1 Main Street","CommDateOfBirth":10012000,"CommCreditScore":0,"CommCsReviewDate":0,"CommSuccess":" ","CommFailCode":" "}}"#.to_string()
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
