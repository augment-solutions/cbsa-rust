use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::delete,
    Json, Router,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use validator::{Validate, ValidationError};

use crate::{
    config::is_six_ascii_digits,
    error::{problem_response, CbsaError, ProblemDetail},
    service::delacc::{self, DelaccRequest, DelaccResult},
    web::AppState,
};

const EYE_CATCHER: &str = "ACCT";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/delacc/:account_number", delete(delete_account))
}

#[axum::debug_handler(state = AppState)]
async fn delete_account(
    State(state): State<AppState>,
    Path(account_number): Path<String>,
    payload: Result<Json<DelaccRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;
    let path = DelaccPathDto::try_from(account_number)?;
    let request = build_request(path, payload, &state.sortcode)?;

    let result = delacc::delete(&state.pool, &state.sortcode, request).await?;

    if result.delete_success() {
        Ok(Json(DelaccResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn build_request(
    path: DelaccPathDto,
    payload: DelaccRequestDto,
    configured_sortcode: &str,
) -> Result<DelaccRequest, CbsaError> {
    let commarea = payload
        .delacc
        .ok_or_else(|| CbsaError::validation("DelAcc is required"))?;

    if let Some(body_account_number) = commarea.del_acc_accno {
        if body_account_number != path.account_number {
            return Err(CbsaError::validation(
                "Body DelAccAccno does not match path account_number.",
            ));
        }
    }

    if let Some(body_sortcode) = commarea.del_acc_scode.as_deref() {
        if !body_sortcode.is_empty() && body_sortcode != configured_sortcode {
            return Err(CbsaError::validation(
                "Body DelAccScode does not match the configured branch sortcode.",
            ));
        }
    }

    Ok(DelaccRequest {
        account_number: path.account_number,
    })
}

fn failure_response(result: &DelaccResult) -> Response {
    let (status, title) = if result.is_not_found_failure() {
        (StatusCode::NOT_FOUND, "Account not found")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Account deletion failed")
    };

    let detail = result.message().unwrap_or("Account deletion failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Validate)]
struct DelaccPathDto {
    #[validate(range(
        min = 0,
        max = 99_999_999_i64,
        message = "account_number must be between 0 and 99999999"
    ))]
    account_number: i64,
}

impl TryFrom<String> for DelaccPathDto {
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

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct DelaccRequestDto {
    #[serde(rename = "DelAcc")]
    #[validate(required, nested)]
    pub delacc: Option<DelaccCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct DelaccCommareaRequestDto {
    #[serde(rename = "DelAccEye")]
    #[validate(length(max = 4, message = "DelAccEye must be at most 4 characters"))]
    pub del_acc_eye: Option<String>,

    #[serde(rename = "DelAccCustno")]
    #[validate(custom(function = "validate_comm_custno"))]
    pub del_acc_custno: Option<String>,

    #[serde(rename = "DelAccScode")]
    #[validate(custom(function = "validate_optional_sortcode_or_empty"))]
    pub del_acc_scode: Option<String>,

    #[serde(rename = "DelAccAccno")]
    #[validate(range(
        min = 0,
        max = 99_999_999_i64,
        message = "DelAccAccno must be between 0 and 99999999"
    ))]
    pub del_acc_accno: Option<i64>,

    #[serde(rename = "DelAccAccType")]
    #[validate(length(max = 8, message = "DelAccAccType must be at most 8 characters"))]
    pub del_acc_acc_type: Option<String>,

    #[serde(
        rename = "DelAccIntRate",
        deserialize_with = "deserialize_optional_stringified"
    )]
    pub del_acc_int_rate: Option<String>,

    #[serde(rename = "DelAccOpened")]
    #[validate(range(
        min = 0,
        max = 99_999_999_i64,
        message = "DelAccOpened must be between 0 and 99999999"
    ))]
    pub del_acc_opened: Option<i64>,

    #[serde(
        rename = "DelAccOverdraft",
        deserialize_with = "deserialize_optional_stringified"
    )]
    pub del_acc_overdraft: Option<String>,

    #[serde(rename = "DelAccLastStmtDt")]
    #[validate(range(
        min = 0,
        max = 99_999_999_i64,
        message = "DelAccLastStmtDt must be between 0 and 99999999"
    ))]
    pub del_acc_last_stmt_dt: Option<i64>,

    #[serde(rename = "DelAccNextStmtDt")]
    #[validate(range(
        min = 0,
        max = 99_999_999_i64,
        message = "DelAccNextStmtDt must be between 0 and 99999999"
    ))]
    pub del_acc_next_stmt_dt: Option<i64>,

    #[serde(
        rename = "DelAccAvailBal",
        deserialize_with = "deserialize_optional_stringified"
    )]
    pub del_acc_avail_bal: Option<String>,

    #[serde(
        rename = "DelAccActualBal",
        deserialize_with = "deserialize_optional_stringified"
    )]
    pub del_acc_actual_bal: Option<String>,

    #[serde(rename = "DelAccSuccess")]
    #[validate(length(max = 1, message = "DelAccSuccess must be at most 1 character"))]
    pub del_acc_success: Option<String>,

    #[serde(rename = "DelAccFailCd")]
    #[validate(length(max = 1, message = "DelAccFailCd must be at most 1 character"))]
    pub del_acc_fail_cd: Option<String>,

    #[serde(rename = "DelAccDelSuccess")]
    #[validate(length(max = 1, message = "DelAccDelSuccess must be at most 1 character"))]
    pub del_acc_del_success: Option<String>,

    #[serde(rename = "DelAccDelFailCd")]
    #[validate(length(max = 1, message = "DelAccDelFailCd must be at most 1 character"))]
    pub del_acc_del_fail_cd: Option<String>,

    #[serde(rename = "DelAccDelApplid")]
    #[validate(length(max = 8, message = "DelAccDelApplid must be at most 8 characters"))]
    pub del_acc_del_applid: Option<String>,

    #[serde(rename = "DelAccDelPcb1")]
    #[validate(length(max = 4, message = "DelAccDelPcb1 must be at most 4 characters"))]
    pub del_acc_del_pcb1: Option<String>,

    #[serde(rename = "DelAccDelPcb2")]
    #[validate(length(max = 4, message = "DelAccDelPcb2 must be at most 4 characters"))]
    pub del_acc_del_pcb2: Option<String>,

    #[serde(rename = "DelAccDelPcb3")]
    #[validate(length(max = 4, message = "DelAccDelPcb3 must be at most 4 characters"))]
    pub del_acc_del_pcb3: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DelaccResponseDto {
    #[serde(rename = "DelAcc")]
    pub delacc: DelaccCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct DelaccCommareaResponseDto {
    #[serde(rename = "DelAccEye")]
    pub del_acc_eye: &'static str,
    #[serde(rename = "DelAccCustno")]
    pub del_acc_custno: String,
    #[serde(rename = "DelAccScode")]
    pub del_acc_scode: String,
    #[serde(rename = "DelAccAccno")]
    pub del_acc_accno: i64,
    #[serde(rename = "DelAccAccType")]
    pub del_acc_acc_type: String,
    #[serde(rename = "DelAccIntRate")]
    pub del_acc_int_rate: String,
    #[serde(rename = "DelAccOpened")]
    pub del_acc_opened: u32,
    #[serde(rename = "DelAccOverdraft")]
    pub del_acc_overdraft: String,
    #[serde(rename = "DelAccLastStmtDt")]
    pub del_acc_last_stmt_dt: u32,
    #[serde(rename = "DelAccNextStmtDt")]
    pub del_acc_next_stmt_dt: u32,
    #[serde(rename = "DelAccAvailBal")]
    pub del_acc_avail_bal: String,
    #[serde(rename = "DelAccActualBal")]
    pub del_acc_actual_bal: String,
    #[serde(rename = "DelAccSuccess")]
    pub del_acc_success: &'static str,
    #[serde(rename = "DelAccFailCd")]
    pub del_acc_fail_cd: &'static str,
    #[serde(rename = "DelAccDelSuccess")]
    pub del_acc_del_success: &'static str,
    #[serde(rename = "DelAccDelFailCd")]
    pub del_acc_del_fail_cd: &'static str,
    #[serde(rename = "DelAccDelApplid")]
    pub del_acc_del_applid: &'static str,
    #[serde(rename = "DelAccDelPcb1")]
    pub del_acc_del_pcb1: &'static str,
    #[serde(rename = "DelAccDelPcb2")]
    pub del_acc_del_pcb2: &'static str,
    #[serde(rename = "DelAccDelPcb3")]
    pub del_acc_del_pcb3: &'static str,
}

impl DelaccResponseDto {
    fn from_result(result: &DelaccResult) -> Self {
        let account = result
            .account()
            .expect("successful DELACC results must include an account");

        Self {
            delacc: DelaccCommareaResponseDto {
                del_acc_eye: EYE_CATCHER,
                del_acc_custno: format!("{:010}", account.customer_number()),
                del_acc_scode: account.sortcode().to_string(),
                del_acc_accno: account.account_number(),
                del_acc_acc_type: account.account_type().to_string(),
                del_acc_int_rate: decimal_string(account.interest_rate()),
                del_acc_opened: cobol_date(account.opened()),
                del_acc_overdraft: decimal_string(account.overdraft_limit()),
                del_acc_last_stmt_dt: account.last_statement_date().map(cobol_date).unwrap_or(0),
                del_acc_next_stmt_dt: account.next_statement_date().map(cobol_date).unwrap_or(0),
                del_acc_avail_bal: decimal_string(account.available_balance()),
                del_acc_actual_bal: decimal_string(account.actual_balance()),
                del_acc_success: "Y",
                del_acc_fail_cd: "0",
                del_acc_del_success: "Y",
                del_acc_del_fail_cd: "",
                del_acc_del_applid: "",
                del_acc_del_pcb1: "",
                del_acc_del_pcb2: "",
                del_acc_del_pcb3: "",
            },
        }
    }
}

fn decimal_string(value: Decimal) -> String {
    let mut value = value;
    value.rescale(2);
    value.to_string()
}

fn cobol_date(date: NaiveDate) -> u32 {
    date.format("%d%m%Y")
        .to_string()
        .parse()
        .expect("valid COBOL date")
}

fn validate_comm_custno(value: &str) -> Result<(), ValidationError> {
    if value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        let mut err = ValidationError::new("customer_number");
        err.message = Some("DelAccCustno must contain 0 to 10 ASCII digits".into());
        Err(err)
    }
}

fn validate_optional_sortcode_or_empty(value: &str) -> Result<(), ValidationError> {
    if value.is_empty() || is_six_ascii_digits(value) {
        Ok(())
    } else {
        let mut err = ValidationError::new("sortcode");
        err.message = Some("DelAccScode must contain 0 or 6 ASCII digits".into());
        Err(err)
    }
}

fn deserialize_optional_stringified<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<serde_json::Value>::deserialize(deserializer)?
        .map(|value| match value {
            serde_json::Value::String(value) => Ok(value),
            serde_json::Value::Number(value) => Ok(value.to_string()),
            _ => Err(D::Error::custom("expected string or number")),
        })
        .transpose()
}
