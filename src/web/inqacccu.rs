use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::Datelike;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::{
    error::{problem_response, CbsaError, ProblemDetail},
    service::inqacccu::{self, InqacccuRequest, InqacccuResult},
    web::AppState,
};

const EYE_CATCHER: &str = "ACCT";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/inqacccu/:customer_number", get(inquire))
}

#[axum::debug_handler(state = AppState)]
async fn inquire(
    State(state): State<AppState>,
    Path(customer_number): Path<String>,
) -> Result<Response, CbsaError> {
    let request = InqacccuRequestDto::try_from(customer_number)?;
    let result = inqacccu::inquire(&state.pool, &state.sortcode, request.into()).await?;

    if result.inquiry_success() {
        Ok(Json(InqacccuResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &InqacccuResult) -> Response {
    let (status, title) = if result.is_not_found_failure() {
        (StatusCode::NOT_FOUND, "Customer not found")
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Customer account inquiry failed",
        )
    };

    let detail = result
        .message()
        .unwrap_or("Customer account inquiry failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());

    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Validate)]
pub struct InqacccuRequestDto {
    #[validate(range(
        min = 0,
        max = 9_999_999_999_i64,
        message = "customer_number must be between 0 and 9999999999"
    ))]
    pub customer_number: i64,
}

impl TryFrom<String> for InqacccuRequestDto {
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

impl From<InqacccuRequestDto> for InqacccuRequest {
    fn from(value: InqacccuRequestDto) -> Self {
        Self {
            customer_number: value.customer_number,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct InqacccuDateDto {
    pub day: u32,
    pub month: u32,
    pub year: i32,
}

impl InqacccuDateDto {
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
pub struct InqacccuAccountDto {
    pub eye: &'static str,
    pub customer_number: i64,
    pub sortcode: String,
    pub account_number: i64,
    pub account_type: String,
    pub interest_rate: String,
    pub opened: InqacccuDateDto,
    pub overdraft: String,
    pub last_statement_date: InqacccuDateDto,
    pub next_statement_date: InqacccuDateDto,
    pub available_balance: String,
    pub actual_balance: String,
}

#[derive(Debug, Serialize)]
pub struct InqacccuResponseDto {
    pub customer_number: i64,
    pub inquiry_success: &'static str,
    pub fail_code: &'static str,
    pub customer_found: &'static str,
    pub pcb_pointer: &'static str,
    pub account_details: Vec<InqacccuAccountDto>,
}

impl InqacccuResponseDto {
    fn from_result(result: &InqacccuResult) -> Self {
        Self {
            customer_number: result.customer_number(),
            inquiry_success: "Y",
            fail_code: result.fail_code(),
            customer_found: if result.customer_found() { "Y" } else { "N" },
            pcb_pointer: "",
            account_details: result
                .accounts()
                .iter()
                .map(|account| InqacccuAccountDto {
                    eye: EYE_CATCHER,
                    customer_number: account.customer_number(),
                    sortcode: account.sortcode().to_string(),
                    account_number: account.account_number(),
                    account_type: account.account_type().to_string(),
                    interest_rate: decimal_value(account.interest_rate()),
                    opened: InqacccuDateDto::from_date(account.opened()),
                    overdraft: decimal_value(account.overdraft_limit()),
                    last_statement_date: account
                        .last_statement_date()
                        .map(InqacccuDateDto::from_date)
                        .unwrap_or_else(InqacccuDateDto::zero),
                    next_statement_date: account
                        .next_statement_date()
                        .map(InqacccuDateDto::from_date)
                        .unwrap_or_else(InqacccuDateDto::zero),
                    available_balance: decimal_value(account.available_balance()),
                    actual_balance: decimal_value(account.actual_balance()),
                })
                .collect(),
        }
    }
}

fn decimal_value(value: Decimal) -> String {
    value.to_string()
}
