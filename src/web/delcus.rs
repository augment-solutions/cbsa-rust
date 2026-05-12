use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::delete,
    Json, Router,
};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::{
    config::is_six_ascii_digits,
    error::{problem_response, CbsaError, ProblemDetail},
    service::delcus::{self, DelcusRequest, DelcusResult},
    web::AppState,
};

const EYE_CATCHER: &str = "CUST";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/delcus/:customer_number", delete(delete_customer))
}

#[axum::debug_handler(state = AppState)]
async fn delete_customer(
    State(state): State<AppState>,
    Path(customer_number): Path<String>,
    payload: Result<Json<DelcusRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let path = DelcusPathDto::try_from(customer_number)?;
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;
    let request = DelcusRequest::try_from((path, payload))?;

    let result = delcus::delete(&state.pool, &state.sortcode, request).await?;

    if result.delete_success() {
        Ok(Json(DelcusResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &DelcusResult) -> Response {
    let (status, title) = if result.is_not_found_failure() {
        (StatusCode::NOT_FOUND, "Customer not found")
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Customer deletion failed",
        )
    };

    let detail = result.message().unwrap_or("Customer deletion failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Validate)]
struct DelcusPathDto {
    #[validate(range(
        min = 1,
        max = 9_999_999_999_i64,
        message = "customer_number must be between 1 and 9999999999"
    ))]
    customer_number: i64,
}

impl TryFrom<String> for DelcusPathDto {
    type Error = CbsaError;

    fn try_from(customer_number: String) -> Result<Self, Self::Error> {
        let customer_number = customer_number.parse().map_err(|_| {
            CbsaError::validation(
                "customer_number must be a base-10 integer between 1 and 9999999999",
            )
        })?;

        let request = Self { customer_number };
        request
            .validate()
            .map_err(|err| CbsaError::validation(err.to_string()))?;

        Ok(request)
    }
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct DelcusRequestDto {
    #[serde(rename = "DelCus")]
    #[validate(required, nested)]
    pub delcus: Option<DelcusCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct DelcusCommareaRequestDto {
    #[serde(rename = "CommEye")]
    #[validate(length(max = 4, message = "CommEye must be at most 4 characters"))]
    pub comm_eye: Option<String>,

    #[serde(rename = "CommScode")]
    #[validate(length(max = 6, message = "CommScode must be at most 6 characters"))]
    pub comm_scode: Option<String>,

    #[serde(rename = "CommCustno")]
    #[validate(length(max = 10, message = "CommCustno must be at most 10 characters"))]
    pub comm_custno: Option<String>,

    #[serde(rename = "CommName")]
    #[validate(length(max = 60, message = "CommName must be at most 60 characters"))]
    pub comm_name: Option<String>,

    #[serde(rename = "CommAddr")]
    #[validate(length(max = 160, message = "CommAddr must be at most 160 characters"))]
    pub comm_addr: Option<String>,

    #[serde(rename = "CommDob")]
    pub comm_dob: Option<i64>,

    #[serde(rename = "CommCreditScore")]
    pub comm_credit_score: Option<i64>,

    #[serde(rename = "CommCsReviewDate")]
    pub comm_cs_review_date: Option<i64>,

    #[serde(rename = "CommDelSuccess")]
    #[validate(length(max = 1, message = "CommDelSuccess must be at most 1 character"))]
    pub comm_del_success: Option<String>,

    #[serde(rename = "CommDelFailCd")]
    #[validate(length(max = 1, message = "CommDelFailCd must be at most 1 character"))]
    pub comm_del_fail_cd: Option<String>,
}

impl TryFrom<(DelcusPathDto, DelcusRequestDto)> for DelcusRequest {
    type Error = CbsaError;

    fn try_from(value: (DelcusPathDto, DelcusRequestDto)) -> Result<Self, Self::Error> {
        let (path, payload) = value;
        let commarea = payload
            .delcus
            .ok_or_else(|| CbsaError::validation("DelCus is required"))?;

        if let Some(sortcode) = commarea.comm_scode.as_deref() {
            if !sortcode.is_empty() && !is_six_ascii_digits(sortcode) {
                return Err(CbsaError::validation(
                    "CommScode must contain 0 or 6 ASCII digits",
                ));
            }
        }

        if let Some(body_customer_number) =
            parse_optional_customer_number(commarea.comm_custno.as_deref())?
        {
            if body_customer_number != path.customer_number {
                return Err(CbsaError::validation(
                    "Body CommCustno does not match path customer_number.",
                ));
            }
        }

        Ok(Self {
            customer_number: path.customer_number,
        })
    }
}

fn parse_optional_customer_number(value: Option<&str>) -> Result<Option<i64>, CbsaError> {
    let Some(value) = value else {
        return Ok(None);
    };

    if value.is_empty() {
        return Ok(None);
    }

    if value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        value
            .parse()
            .map(Some)
            .map_err(|_| CbsaError::validation("CommCustno must contain 0 to 10 ASCII digits"))
    } else {
        Err(CbsaError::validation(
            "CommCustno must contain 0 to 10 ASCII digits",
        ))
    }
}

#[derive(Debug, Serialize)]
pub struct DelcusResponseDto {
    #[serde(rename = "DelCus")]
    pub delcus: DelcusCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct DelcusCommareaResponseDto {
    #[serde(rename = "CommEye")]
    pub comm_eye: &'static str,
    #[serde(rename = "CommScode")]
    pub comm_scode: String,
    #[serde(rename = "CommCustno")]
    pub comm_custno: String,
    #[serde(rename = "CommName")]
    pub comm_name: String,
    #[serde(rename = "CommAddr")]
    pub comm_addr: String,
    #[serde(rename = "CommDob")]
    pub comm_dob: u32,
    #[serde(rename = "CommCreditScore")]
    pub comm_credit_score: u16,
    #[serde(rename = "CommCsReviewDate")]
    pub comm_cs_review_date: u32,
    #[serde(rename = "CommDelSuccess")]
    pub comm_del_success: &'static str,
    #[serde(rename = "CommDelFailCd")]
    pub comm_del_fail_cd: &'static str,
}

impl DelcusResponseDto {
    fn from_result(result: &DelcusResult) -> Self {
        let customer = result
            .customer()
            .expect("successful DELCUS results must include a customer");

        Self {
            delcus: DelcusCommareaResponseDto {
                comm_eye: EYE_CATCHER,
                comm_scode: customer.sortcode().to_string(),
                comm_custno: format!("{:010}", customer.customer_number()),
                comm_name: customer.name().to_string(),
                comm_addr: customer.address().to_string(),
                comm_dob: cobol_date(customer.date_of_birth()),
                comm_credit_score: customer.credit_score(),
                comm_cs_review_date: customer
                    .credit_score_review_date()
                    .map(cobol_date)
                    .unwrap_or(0),
                comm_del_success: "Y",
                comm_del_fail_cd: " ",
            },
        }
    }
}

fn cobol_date(date: chrono::NaiveDate) -> u32 {
    (date.day() * 1_000_000)
        + (date.month() * 10_000)
        + u32::try_from(date.year()).unwrap_or_default()
}
