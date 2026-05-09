use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::{
    error::{problem_response, CbsaError, ProblemDetail},
    service::inqcust::{self, InqcustRequest, InqcustResult, SystemRandomCustomerNumberGenerator},
    web::AppState,
};

const EYE_CATCHER: &str = "CUST";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/inqcust/:customer_number", get(inquire))
}

#[axum::debug_handler(state = AppState)]
async fn inquire(
    State(state): State<AppState>,
    Path(customer_number): Path<String>,
) -> Result<Response, CbsaError> {
    let request = InqcustRequestDto::try_from(customer_number)?;

    let mut generator = SystemRandomCustomerNumberGenerator::default();
    let result =
        inqcust::inquire(&state.pool, &state.sortcode, request.into(), &mut generator).await?;

    if result.inquiry_success() {
        Ok(Json(InqcustResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &InqcustResult) -> Response {
    let (status, title) = if result.is_not_found_failure() {
        (StatusCode::NOT_FOUND, "Customer not found")
    } else if result.is_random_retry_exhausted_failure() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Customer inquiry retry exhausted",
        )
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Customer inquiry failed")
    };

    let detail = result.message().unwrap_or("Customer inquiry failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());

    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Validate)]
pub struct InqcustRequestDto {
    #[validate(range(
        min = 0,
        max = 9_999_999_999_i64,
        message = "customer_number must be between 0 and 9999999999"
    ))]
    pub customer_number: i64,
}

impl TryFrom<String> for InqcustRequestDto {
    type Error = CbsaError;

    fn try_from(customer_number: String) -> Result<Self, Self::Error> {
        let customer_number = customer_number.parse().map_err(|_| {
            CbsaError::validation(
                "customer_number must be a base-10 integer between 0 and 9999999999",
            )
        })?;

        let request = Self { customer_number };
        request
            .validate()
            .map_err(|err| CbsaError::validation(err.to_string()))?;

        Ok(request)
    }
}

impl From<InqcustRequestDto> for InqcustRequest {
    fn from(value: InqcustRequestDto) -> Self {
        Self {
            customer_number: value.customer_number,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct InqcustDateDto {
    pub day: u32,
    pub month: u32,
    pub year: i32,
}

impl InqcustDateDto {
    fn from_date(date: chrono::NaiveDate) -> Self {
        Self {
            day: date.day(),
            month: date.month(),
            year: date.year(),
        }
    }

    fn zero() -> Self {
        Self {
            day: 0,
            month: 0,
            year: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InqcustResponseDto {
    pub eye: &'static str,
    pub sortcode: String,
    pub customer_number: i64,
    pub name: String,
    pub address: String,
    pub date_of_birth: InqcustDateDto,
    pub credit_score: u16,
    pub credit_score_review_date: InqcustDateDto,
    pub inquiry_success: &'static str,
    pub fail_code: &'static str,
    pub pcb_pointer: &'static str,
}

impl InqcustResponseDto {
    fn from_result(result: &InqcustResult) -> Self {
        let customer = result
            .customer()
            .expect("successful INQCUST results must include a customer");

        Self {
            eye: EYE_CATCHER,
            sortcode: customer.sortcode().to_string(),
            customer_number: customer.customer_number(),
            name: customer.name().to_string(),
            address: customer.address().to_string(),
            date_of_birth: InqcustDateDto::from_date(customer.date_of_birth()),
            credit_score: customer.credit_score(),
            credit_score_review_date: customer
                .credit_score_review_date()
                .map(InqcustDateDto::from_date)
                .unwrap_or_else(InqcustDateDto::zero),
            inquiry_success: "Y",
            fail_code: "0",
            pcb_pointer: "",
        }
    }
}
