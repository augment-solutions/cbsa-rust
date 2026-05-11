use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};

use crate::{
    config::is_six_ascii_digits,
    error::{problem_response, CbsaError, ProblemDetail},
    service::crecust::{self, CrecustRequest, CrecustResult},
    web::AppState,
};

const EYE_CATCHER: &str = "CUST";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/crecust", post(create))
}

#[axum::debug_handler(state = AppState)]
async fn create(
    State(state): State<AppState>,
    payload: Result<Json<CrecustRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;
    let request = CrecustRequest::try_from(payload)?;
    let result = crecust::create(&state.pool, &state.sortcode, request).await?;

    if result.creation_success() {
        Ok(Json(CrecustResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &CrecustResult) -> Response {
    let status = if result.is_validation_failure() {
        StatusCode::BAD_REQUEST
    } else if result.is_credit_failure() {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };

    let title = match result.fail_code() {
        "T" => "Invalid customer title",
        "O" | "Y" | "Z" => "Invalid date of birth",
        "G" => "Credit check unavailable",
        _ => "Customer creation failed",
    };

    let detail = result.message().unwrap_or("Customer creation failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CrecustRequestDto {
    #[serde(rename = "CreCust")]
    #[validate(required, nested)]
    pub crecust: Option<CrecustCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CrecustCommareaRequestDto {
    #[serde(rename = "CommEyecatcher")]
    #[validate(length(max = 4, message = "CommEyecatcher must be at most 4 characters"))]
    pub comm_eyecatcher: Option<String>,

    // CRECUST overwrites the request placeholder key on success (COMM-SORTCODE /
    // COMM-NUMBER are populated only after the customer row has been written).
    #[serde(rename = "CommKey")]
    #[validate(required, nested)]
    pub comm_key: Option<CrecustKeyDto>,

    #[serde(rename = "CommName")]
    #[validate(
        required,
        length(max = 60, message = "CommName must be at most 60 characters"),
        custom(function = "validate_non_blank_name")
    )]
    pub comm_name: Option<String>,

    #[serde(rename = "CommAddress")]
    #[validate(
        required,
        length(max = 160, message = "CommAddress must be at most 160 characters")
    )]
    pub comm_address: Option<String>,

    #[serde(rename = "CommDateOfBirth")]
    #[validate(
        required,
        range(
            min = 0,
            max = 99_999_999,
            message = "CommDateOfBirth must be between 0 and 99999999"
        )
    )]
    pub comm_date_of_birth: Option<i64>,

    #[serde(rename = "CommCreditScore")]
    #[validate(range(
        min = 0,
        max = 999,
        message = "CommCreditScore must be between 0 and 999"
    ))]
    pub comm_credit_score: Option<i64>,

    #[serde(rename = "CommCsReviewDate")]
    #[validate(range(
        min = 0,
        max = 99_999_999,
        message = "CommCsReviewDate must be between 0 and 99999999"
    ))]
    pub comm_cs_review_date: Option<i64>,

    #[serde(rename = "CommSuccess")]
    #[validate(length(max = 1, message = "CommSuccess must be at most 1 character"))]
    pub comm_success: Option<String>,

    #[serde(rename = "CommFailCode")]
    #[validate(length(max = 1, message = "CommFailCode must be at most 1 character"))]
    pub comm_fail_code: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CrecustKeyDto {
    #[serde(rename = "CommSortcode")]
    #[validate(required, custom(function = "validate_comm_sortcode"))]
    pub comm_sortcode: Option<String>,

    #[serde(rename = "CommNumber")]
    #[validate(
        required,
        range(
            min = 0,
            max = 9_999_999_999_i64,
            message = "CommNumber must be between 0 and 9999999999"
        )
    )]
    pub comm_number: Option<i64>,
}

fn validate_non_blank_name(value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        let mut err = ValidationError::new("non_blank");
        err.message = Some("CommName must not be blank".into());
        return Err(err);
    }
    Ok(())
}

fn validate_comm_sortcode(value: &str) -> Result<(), ValidationError> {
    if is_six_ascii_digits(value) {
        Ok(())
    } else {
        let mut err = ValidationError::new("sortcode");
        err.message = Some("CommSortcode must be exactly 6 ASCII digits".into());
        Err(err)
    }
}

impl TryFrom<CrecustRequestDto> for CrecustRequest {
    type Error = CbsaError;

    fn try_from(value: CrecustRequestDto) -> Result<Self, Self::Error> {
        let commarea = value
            .crecust
            .ok_or_else(|| CbsaError::validation("CreCust is required"))?;
        Ok(Self {
            name: commarea
                .comm_name
                .ok_or_else(|| CbsaError::validation("CommName is required"))?,
            address: commarea
                .comm_address
                .ok_or_else(|| CbsaError::validation("CommAddress is required"))?,
            date_of_birth: u32::try_from(
                commarea
                    .comm_date_of_birth
                    .ok_or_else(|| CbsaError::validation("CommDateOfBirth is required"))?,
            )
            .map_err(|_| CbsaError::validation("CommDateOfBirth must be between 0 and 99999999"))?,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct CrecustResponseDto {
    #[serde(rename = "CreCust")]
    pub crecust: CrecustCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct CrecustCommareaResponseDto {
    #[serde(rename = "CommEyecatcher")]
    pub comm_eyecatcher: &'static str,
    #[serde(rename = "CommKey")]
    pub comm_key: CrecustKeyResponseDto,
    #[serde(rename = "CommName")]
    pub comm_name: String,
    #[serde(rename = "CommAddress")]
    pub comm_address: String,
    #[serde(rename = "CommDateOfBirth")]
    pub comm_date_of_birth: u32,
    #[serde(rename = "CommCreditScore")]
    pub comm_credit_score: u16,
    #[serde(rename = "CommCsReviewDate")]
    pub comm_cs_review_date: u32,
    #[serde(rename = "CommSuccess")]
    pub comm_success: &'static str,
    #[serde(rename = "CommFailCode")]
    pub comm_fail_code: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CrecustKeyResponseDto {
    #[serde(rename = "CommSortcode")]
    pub comm_sortcode: String,
    #[serde(rename = "CommNumber")]
    pub comm_number: i64,
}

impl CrecustResponseDto {
    fn from_result(result: &CrecustResult) -> Self {
        let customer = result
            .customer()
            .expect("successful CRECUST results must include a customer");
        Self {
            crecust: CrecustCommareaResponseDto {
                comm_eyecatcher: EYE_CATCHER,
                comm_key: CrecustKeyResponseDto {
                    comm_sortcode: customer.sortcode().to_string(),
                    comm_number: customer.customer_number(),
                },
                comm_name: customer.name().to_string(),
                comm_address: customer.address().to_string(),
                comm_date_of_birth: cobol_date(customer.date_of_birth()),
                comm_credit_score: customer.credit_score(),
                comm_cs_review_date: customer
                    .credit_score_review_date()
                    .map(cobol_date)
                    .unwrap_or(0),
                comm_success: "Y",
                comm_fail_code: "",
            },
        }
    }
}

fn cobol_date(date: chrono::NaiveDate) -> u32 {
    (date.day() * 1_000_000)
        + (date.month() * 10_000)
        + u32::try_from(date.year()).unwrap_or_default()
}
