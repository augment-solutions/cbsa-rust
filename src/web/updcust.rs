use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::put,
    Json, Router,
};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};

use crate::{
    config::is_six_ascii_digits,
    error::{problem_response, CbsaError, ProblemDetail},
    service::updcust::{self, UpdcustRequest, UpdcustResult},
    web::AppState,
};

const EYE_CATCHER: &str = "CUST";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/updcust", put(update))
}

#[axum::debug_handler(state = AppState)]
async fn update(
    State(state): State<AppState>,
    payload: Result<Json<UpdcustRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;
    let request = UpdcustRequest::try_from(payload)?;

    let result = updcust::update(&state.pool, &state.sortcode, request).await?;

    if result.update_success() {
        Ok(Json(UpdcustResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &UpdcustResult) -> Response {
    let status = if result.is_not_found_failure() {
        StatusCode::NOT_FOUND
    } else if result.is_validation_failure() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };

    let title = match result.fail_code() {
        "1" => "Customer not found",
        "2" => "Customer datastore read failed",
        "4" => "Customer name and address required",
        "T" => "Invalid customer title",
        _ => "Customer update failed",
    };

    let detail = result.message().unwrap_or("Customer update failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct UpdcustRequestDto {
    #[serde(rename = "UpdCust")]
    #[validate(required, nested)]
    pub updcust: Option<UpdcustCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct UpdcustCommareaRequestDto {
    #[serde(rename = "CommEye")]
    #[validate(length(max = 4, message = "CommEye must be at most 4 characters"))]
    pub comm_eye: Option<String>,

    #[serde(rename = "CommScode")]
    #[validate(required, custom(function = "validate_comm_sortcode"))]
    pub comm_scode: Option<String>,

    #[serde(rename = "CommCustno")]
    #[validate(required, custom(function = "validate_comm_custno"))]
    pub comm_custno: Option<String>,

    #[serde(rename = "CommName")]
    #[validate(
        required,
        length(max = 60, message = "CommName must be at most 60 characters")
    )]
    pub comm_name: Option<String>,

    #[serde(rename = "CommAddress")]
    #[validate(
        required,
        length(max = 160, message = "CommAddress must be at most 160 characters")
    )]
    pub comm_address: Option<String>,

    #[serde(rename = "CommDob")]
    #[validate(
        required,
        range(
            min = 0,
            max = 99_999_999,
            message = "CommDob must be between 0 and 99999999"
        )
    )]
    pub comm_dob: Option<i64>,

    #[serde(rename = "CommCreditScore")]
    #[validate(
        required,
        range(
            min = 0,
            max = 999,
            message = "CommCreditScore must be between 0 and 999"
        )
    )]
    pub comm_credit_score: Option<i64>,

    #[serde(rename = "CommCsReviewDate")]
    #[validate(
        required,
        range(
            min = 0,
            max = 99_999_999,
            message = "CommCsReviewDate must be between 0 and 99999999"
        )
    )]
    pub comm_cs_review_date: Option<i64>,

    #[serde(rename = "CommUpdSuccess")]
    #[validate(length(max = 1, message = "CommUpdSuccess must be at most 1 character"))]
    pub comm_upd_success: Option<String>,

    #[serde(rename = "CommUpdFailCd")]
    #[validate(length(max = 1, message = "CommUpdFailCd must be at most 1 character"))]
    pub comm_upd_fail_cd: Option<String>,
}

fn validate_comm_sortcode(value: &str) -> Result<(), ValidationError> {
    if is_six_ascii_digits(value) {
        Ok(())
    } else {
        let mut err = ValidationError::new("sortcode");
        err.message = Some("CommScode must be exactly 6 ASCII digits".into());
        Err(err)
    }
}

fn validate_comm_custno(value: &str) -> Result<(), ValidationError> {
    if !value.is_empty() && value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        let mut err = ValidationError::new("customer_number");
        err.message = Some("CommCustno must contain 1 to 10 ASCII digits".into());
        Err(err)
    }
}

impl TryFrom<UpdcustRequestDto> for UpdcustRequest {
    type Error = CbsaError;

    fn try_from(value: UpdcustRequestDto) -> Result<Self, Self::Error> {
        let commarea = value
            .updcust
            .ok_or_else(|| CbsaError::validation("UpdCust is required"))?;

        let customer_number = commarea
            .comm_custno
            .ok_or_else(|| CbsaError::validation("CommCustno is required"))?
            .parse()
            .map_err(|_| CbsaError::validation("CommCustno must contain 1 to 10 ASCII digits"))?;

        Ok(Self {
            customer_number,
            name: commarea
                .comm_name
                .ok_or_else(|| CbsaError::validation("CommName is required"))?,
            address: commarea
                .comm_address
                .ok_or_else(|| CbsaError::validation("CommAddress is required"))?,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct UpdcustResponseDto {
    #[serde(rename = "UpdCust")]
    pub updcust: UpdcustCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct UpdcustCommareaResponseDto {
    #[serde(rename = "CommEye")]
    pub comm_eye: &'static str,
    #[serde(rename = "CommScode")]
    pub comm_scode: String,
    #[serde(rename = "CommCustno")]
    pub comm_custno: String,
    #[serde(rename = "CommName")]
    pub comm_name: String,
    #[serde(rename = "CommAddress")]
    pub comm_address: String,
    #[serde(rename = "CommDob")]
    pub comm_dob: u32,
    #[serde(rename = "CommCreditScore")]
    pub comm_credit_score: u16,
    #[serde(rename = "CommCsReviewDate")]
    pub comm_cs_review_date: u32,
    #[serde(rename = "CommUpdSuccess")]
    pub comm_upd_success: &'static str,
    #[serde(rename = "CommUpdFailCd")]
    pub comm_upd_fail_cd: &'static str,
}

impl UpdcustResponseDto {
    fn from_result(result: &UpdcustResult) -> Self {
        let customer = result
            .customer()
            .expect("successful UPDCUST results must include a customer");

        Self {
            updcust: UpdcustCommareaResponseDto {
                comm_eye: EYE_CATCHER,
                comm_scode: customer.sortcode().to_string(),
                comm_custno: format!("{:010}", customer.customer_number()),
                comm_name: customer.name().to_string(),
                comm_address: customer.address().to_string(),
                comm_dob: cobol_date(customer.date_of_birth()),
                comm_credit_score: customer.credit_score(),
                comm_cs_review_date: customer
                    .credit_score_review_date()
                    .map(cobol_date)
                    .unwrap_or(0),
                comm_upd_success: "Y",
                comm_upd_fail_cd: "0",
            },
        }
    }
}

fn cobol_date(date: chrono::NaiveDate) -> u32 {
    (date.day() * 1_000_000)
        + (date.month() * 10_000)
        + u32::try_from(date.year()).unwrap_or_default()
}
