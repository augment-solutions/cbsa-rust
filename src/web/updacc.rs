use std::str::FromStr;

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::put,
    Json, Router,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use validator::{Validate, ValidationError};

use crate::{
    config::is_six_ascii_digits,
    error::{problem_response, CbsaError, ProblemDetail},
    service::updacc::{self, UpdaccRequest, UpdaccResult},
    web::AppState,
};

const EYE_CATCHER: &str = "ACCT";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/updacc", put(update))
}

#[axum::debug_handler(state = AppState)]
async fn update(
    State(state): State<AppState>,
    payload: Result<Json<UpdaccRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;
    let request = UpdaccRequest::try_from(payload)?;

    let result = updacc::update(&state.pool, &state.sortcode, request).await?;

    if result.update_success() {
        Ok(Json(UpdaccResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &UpdaccResult) -> Response {
    let status = if result.is_not_found_failure() {
        StatusCode::NOT_FOUND
    } else if result.is_validation_failure() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };

    let title = match result.fail_code() {
        "1" => "Account not found",
        "2" => "Invalid account type",
        _ => "Account update failed",
    };

    let detail = result.message().unwrap_or("Account update failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct UpdaccRequestDto {
    #[serde(rename = "UpdAcc")]
    #[validate(required, nested)]
    pub updacc: Option<UpdaccCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct UpdaccCommareaRequestDto {
    #[serde(rename = "CommEye")]
    #[validate(length(max = 4, message = "CommEye must be at most 4 characters"))]
    pub comm_eye: Option<String>,

    #[serde(rename = "CommCustno")]
    #[validate(custom(function = "validate_comm_custno"))]
    pub comm_custno: Option<String>,

    #[serde(rename = "CommScode")]
    #[validate(custom(function = "validate_optional_sortcode"))]
    pub comm_scode: Option<String>,

    #[serde(rename = "CommAccno")]
    #[validate(
        required,
        range(
            min = 0,
            max = 99_999_999,
            message = "CommAccno must be between 0 and 99999999"
        )
    )]
    pub comm_accno: Option<i64>,

    #[serde(rename = "CommAccType")]
    #[validate(
        required,
        length(max = 8, message = "CommAccType must be at most 8 characters")
    )]
    pub comm_acc_type: Option<String>,

    #[serde(
        rename = "CommIntRate",
        deserialize_with = "deserialize_optional_stringified"
    )]
    #[validate(required, custom(function = "validate_comm_int_rate"))]
    pub comm_int_rate: Option<String>,

    #[serde(rename = "CommOpened")]
    pub comm_opened: Option<i64>,

    #[serde(
        rename = "CommOverdraft",
        deserialize_with = "deserialize_optional_stringified"
    )]
    #[validate(required, custom(function = "validate_comm_overdraft"))]
    pub comm_overdraft: Option<String>,

    #[serde(rename = "CommLastStmtDt")]
    pub comm_last_stmt_dt: Option<i64>,

    #[serde(rename = "CommNextStmtDt")]
    pub comm_next_stmt_dt: Option<i64>,

    #[serde(
        rename = "CommAvailBal",
        deserialize_with = "deserialize_optional_stringified"
    )]
    pub comm_avail_bal: Option<String>,

    #[serde(
        rename = "CommActualBal",
        deserialize_with = "deserialize_optional_stringified"
    )]
    pub comm_actual_bal: Option<String>,

    #[serde(rename = "CommSuccess")]
    #[validate(length(max = 1, message = "CommSuccess must be at most 1 character"))]
    pub comm_success: Option<String>,
}

impl TryFrom<UpdaccRequestDto> for UpdaccRequest {
    type Error = CbsaError;

    fn try_from(value: UpdaccRequestDto) -> Result<Self, Self::Error> {
        let commarea = value
            .updacc
            .ok_or_else(|| CbsaError::validation("UpdAcc is required"))?;

        Ok(Self {
            account_number: commarea
                .comm_accno
                .ok_or_else(|| CbsaError::validation("CommAccno is required"))?,
            account_type: commarea
                .comm_acc_type
                .ok_or_else(|| CbsaError::validation("CommAccType is required"))?,
            interest_rate: parse_decimal_field(
                commarea
                    .comm_int_rate
                    .ok_or_else(|| CbsaError::validation("CommIntRate is required"))?,
                "CommIntRate",
            )?,
            overdraft_limit: parse_decimal_field(
                commarea
                    .comm_overdraft
                    .ok_or_else(|| CbsaError::validation("CommOverdraft is required"))?,
                "CommOverdraft",
            )?,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct UpdaccResponseDto {
    #[serde(rename = "UpdAcc")]
    pub updacc: UpdaccCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct UpdaccCommareaResponseDto {
    #[serde(rename = "CommEye")]
    pub comm_eye: &'static str,
    #[serde(rename = "CommCustno")]
    pub comm_custno: String,
    #[serde(rename = "CommScode")]
    pub comm_scode: String,
    #[serde(rename = "CommAccno")]
    pub comm_accno: i64,
    #[serde(rename = "CommAccType")]
    pub comm_acc_type: String,
    #[serde(rename = "CommIntRate")]
    pub comm_int_rate: String,
    #[serde(rename = "CommOpened")]
    pub comm_opened: u32,
    #[serde(rename = "CommOverdraft")]
    pub comm_overdraft: String,
    #[serde(rename = "CommLastStmtDt")]
    pub comm_last_stmt_dt: u32,
    #[serde(rename = "CommNextStmtDt")]
    pub comm_next_stmt_dt: u32,
    #[serde(rename = "CommAvailBal")]
    pub comm_avail_bal: String,
    #[serde(rename = "CommActualBal")]
    pub comm_actual_bal: String,
    #[serde(rename = "CommSuccess")]
    pub comm_success: &'static str,
}

impl UpdaccResponseDto {
    fn from_result(result: &UpdaccResult) -> Self {
        let account = result
            .account()
            .expect("successful UPDACC results must include an account");

        Self {
            updacc: UpdaccCommareaResponseDto {
                comm_eye: EYE_CATCHER,
                comm_custno: format!("{:010}", account.customer_number()),
                comm_scode: account.sortcode().to_string(),
                comm_accno: account.account_number(),
                comm_acc_type: account.account_type().to_string(),
                comm_int_rate: decimal_string(account.interest_rate()),
                comm_opened: cobol_date(account.opened()),
                comm_overdraft: decimal_string(account.overdraft_limit()),
                comm_last_stmt_dt: account.last_statement_date().map(cobol_date).unwrap_or(0),
                comm_next_stmt_dt: account.next_statement_date().map(cobol_date).unwrap_or(0),
                comm_avail_bal: decimal_string(account.available_balance()),
                comm_actual_bal: decimal_string(account.actual_balance()),
                comm_success: "Y",
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
    if !value.is_empty() && value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        let mut err = ValidationError::new("customer_number");
        err.message = Some("CommCustno must contain 1 to 10 ASCII digits".into());
        Err(err)
    }
}

fn validate_optional_sortcode(value: &str) -> Result<(), ValidationError> {
    if is_six_ascii_digits(value) {
        Ok(())
    } else {
        let mut err = ValidationError::new("sortcode");
        err.message = Some("CommScode must be exactly 6 ASCII digits".into());
        Err(err)
    }
}

fn validate_comm_int_rate(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "CommIntRate", |decimal| {
        decimal.scale() <= 2 && *decimal >= Decimal::ZERO && *decimal <= Decimal::new(999_999, 2)
    })
}

fn validate_comm_overdraft(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "CommOverdraft", |decimal| {
        decimal.scale() <= 2
            && decimal.fract().is_zero()
            && *decimal >= Decimal::ZERO
            && *decimal <= Decimal::new(99_999_999, 0)
    })
}

fn validate_decimal(
    value: &str,
    field: &'static str,
    predicate: impl Fn(&Decimal) -> bool,
) -> Result<(), ValidationError> {
    match Decimal::from_str(value) {
        Ok(decimal) if predicate(&decimal) => Ok(()),
        _ => {
            let mut err = ValidationError::new("decimal");
            err.message = Some(format!("{field} is invalid").into());
            Err(err)
        }
    }
}

fn parse_decimal_field(value: String, field_name: &str) -> Result<Decimal, CbsaError> {
    Decimal::from_str(&value).map_err(|_| CbsaError::validation(format!("{field_name} is invalid")))
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
