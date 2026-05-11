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
    service::inqacc::{self, InqaccRequest, InqaccResult},
    web::AppState,
};

const EYE_CATCHER: &str = "ACCT";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/inqacc/:account_number", get(inquire))
}

#[axum::debug_handler(state = AppState)]
async fn inquire(
    State(state): State<AppState>,
    Path(account_number): Path<String>,
) -> Result<Response, CbsaError> {
    let request = InqaccRequestDto::try_from(account_number)?;
    let result = inqacc::inquire(&state.pool, &state.sortcode, request.into()).await?;

    if result.inquiry_success() {
        Ok(Json(InqaccResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &InqaccResult) -> Response {
    let (status, title) = if result.is_not_found_failure() {
        (StatusCode::NOT_FOUND, "Account not found")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Account inquiry failed")
    };

    let detail = result.message().unwrap_or("Account inquiry failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());

    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Validate)]
pub struct InqaccRequestDto {
    #[validate(range(
        min = 0,
        max = 99_999_999_i64,
        message = "account_number must be between 0 and 99999999"
    ))]
    pub account_number: i64,
}

impl TryFrom<String> for InqaccRequestDto {
    type Error = CbsaError;

    fn try_from(account_number: String) -> Result<Self, Self::Error> {
        let account_number = account_number.parse().map_err(|_| {
            CbsaError::validation("account_number must be a base-10 integer between 0 and 99999999")
        })?;

        let request = Self { account_number };
        request
            .validate()
            .map_err(|err| CbsaError::validation(err.to_string()))?;

        Ok(request)
    }
}

impl From<InqaccRequestDto> for InqaccRequest {
    fn from(value: InqaccRequestDto) -> Self {
        Self {
            account_number: value.account_number,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct InqaccDateDto {
    pub day: u32,
    pub month: u32,
    pub year: i32,
}

impl InqaccDateDto {
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
pub struct InqaccResponseDto {
    pub eye: &'static str,
    pub customer_number: i64,
    pub sortcode: String,
    pub account_number: i64,
    pub account_type: String,
    pub interest_rate: String,
    pub opened: InqaccDateDto,
    pub overdraft: String,
    pub last_statement_date: InqaccDateDto,
    pub next_statement_date: InqaccDateDto,
    pub available_balance: String,
    pub actual_balance: String,
    pub inquiry_success: &'static str,
    pub pcb1_pointer: &'static str,
}

impl InqaccResponseDto {
    fn from_result(result: &InqaccResult) -> Self {
        let account = result
            .account()
            .expect("successful INQACC results must include an account");

        Self {
            eye: EYE_CATCHER,
            customer_number: account.customer_number(),
            sortcode: account.sortcode().to_string(),
            account_number: account.account_number(),
            account_type: account.account_type().to_string(),
            interest_rate: decimal_value(account.interest_rate()),
            opened: InqaccDateDto::from_date(account.opened()),
            overdraft: decimal_value(account.overdraft_limit()),
            last_statement_date: account
                .last_statement_date()
                .map(InqaccDateDto::from_date)
                .unwrap_or_else(InqaccDateDto::zero),
            next_statement_date: account
                .next_statement_date()
                .map(InqaccDateDto::from_date)
                .unwrap_or_else(InqaccDateDto::zero),
            available_balance: decimal_value(account.available_balance()),
            actual_balance: decimal_value(account.actual_balance()),
            inquiry_success: "Y",
            pcb1_pointer: "",
        }
    }
}

fn decimal_value(value: Decimal) -> String {
    value.to_string()
}
