use std::str::FromStr;

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};

use crate::{
    config::is_six_ascii_digits,
    error::{problem_response, CbsaError, ProblemDetail},
    service::creacc::{self, CreaccRequest, CreaccResult},
    web::AppState,
};

const EYE_CATCHER: &str = "ACCT";

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/creacc", post(create))
}

#[axum::debug_handler(state = AppState)]
async fn create(
    State(state): State<AppState>,
    payload: Result<Json<CreaccRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;
    let request = CreaccRequest::try_from(payload)?;
    let result = creacc::create(&state.pool, &state.sortcode, request).await?;

    if result.creation_success() {
        Ok(Json(CreaccResponseDto::from_result(&result)).into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &CreaccResult) -> Response {
    let status = if result.is_validation_failure() {
        StatusCode::BAD_REQUEST
    } else if result.is_not_found_failure() {
        StatusCode::NOT_FOUND
    } else if result.is_capacity_failure() {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };

    let title = match result.fail_code() {
        "1" => "Customer not found",
        "8" => "Maximum account count reached",
        "A" => "Invalid account type",
        _ => "Account creation failed",
    };

    let detail = result.message().unwrap_or("Account creation failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CreaccRequestDto {
    #[serde(rename = "CreAcc")]
    #[validate(required, nested)]
    pub creacc: Option<CreaccCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CreaccCommareaRequestDto {
    #[serde(rename = "CommEyecatcher")]
    #[validate(length(max = 4, message = "CommEyecatcher must be at most 4 characters"))]
    pub comm_eyecatcher: Option<String>,

    #[serde(rename = "CommCustno")]
    #[validate(
        required,
        range(
            min = 0,
            max = 9_999_999_999_i64,
            message = "CommCustno must be between 0 and 9999999999"
        )
    )]
    pub comm_custno: Option<i64>,

    #[serde(rename = "CommKey")]
    #[validate(required, nested)]
    pub comm_key: Option<CreaccKeyDto>,

    #[serde(rename = "CommAccType")]
    #[validate(
        required,
        length(max = 8, message = "CommAccType must be at most 8 characters"),
        custom(function = "validate_non_blank_account_type")
    )]
    pub comm_acc_type: Option<String>,

    #[serde(rename = "CommIntRt")]
    #[validate(required, custom(function = "validate_comm_int_rt"))]
    pub comm_int_rt: Option<String>,

    #[serde(rename = "CommOpened")]
    #[validate(range(
        min = 0,
        max = 99_999_999,
        message = "CommOpened must be between 0 and 99999999"
    ))]
    pub comm_opened: Option<i64>,

    #[serde(rename = "CommOverdrLim")]
    #[validate(required, custom(function = "validate_comm_overdr_lim"))]
    pub comm_overdr_lim: Option<String>,

    #[serde(rename = "CommLastStmtDt")]
    #[validate(range(
        min = 0,
        max = 99_999_999,
        message = "CommLastStmtDt must be between 0 and 99999999"
    ))]
    pub comm_last_stmt_dt: Option<i64>,

    #[serde(rename = "CommNextStmtDt")]
    #[validate(range(
        min = 0,
        max = 99_999_999,
        message = "CommNextStmtDt must be between 0 and 99999999"
    ))]
    pub comm_next_stmt_dt: Option<i64>,

    #[serde(rename = "CommAvailBal")]
    #[validate(required, custom(function = "validate_comm_balance"))]
    pub comm_avail_bal: Option<String>,

    #[serde(rename = "CommActBal")]
    #[validate(required, custom(function = "validate_comm_balance"))]
    pub comm_act_bal: Option<String>,

    #[serde(rename = "CommSuccess")]
    #[validate(length(max = 1, message = "CommSuccess must be at most 1 character"))]
    pub comm_success: Option<String>,

    #[serde(rename = "CommFailCode")]
    #[validate(length(max = 1, message = "CommFailCode must be at most 1 character"))]
    pub comm_fail_code: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct CreaccKeyDto {
    #[serde(rename = "CommSortcode")]
    #[validate(required, custom(function = "validate_comm_sortcode"))]
    pub comm_sortcode: Option<String>,

    #[serde(rename = "CommNumber")]
    #[validate(
        required,
        range(
            min = 0,
            max = 99_999_999,
            message = "CommNumber must be between 0 and 99999999"
        )
    )]
    pub comm_number: Option<i64>,
}

impl TryFrom<CreaccRequestDto> for CreaccRequest {
    type Error = CbsaError;

    fn try_from(value: CreaccRequestDto) -> Result<Self, Self::Error> {
        let commarea = value
            .creacc
            .ok_or_else(|| CbsaError::validation("CreAcc is required"))?;
        CreaccRequest::new(
            commarea
                .comm_custno
                .ok_or_else(|| CbsaError::validation("CommCustno is required"))?,
            commarea
                .comm_acc_type
                .ok_or_else(|| CbsaError::validation("CommAccType is required"))?,
            parse_decimal_field(
                commarea
                    .comm_int_rt
                    .ok_or_else(|| CbsaError::validation("CommIntRt is required"))?,
                "CommIntRt",
            )?,
            parse_decimal_field(
                commarea
                    .comm_overdr_lim
                    .ok_or_else(|| CbsaError::validation("CommOverdrLim is required"))?,
                "CommOverdrLim",
            )?,
            parse_decimal_field(
                commarea
                    .comm_avail_bal
                    .ok_or_else(|| CbsaError::validation("CommAvailBal is required"))?,
                "CommAvailBal",
            )?,
            parse_decimal_field(
                commarea
                    .comm_act_bal
                    .ok_or_else(|| CbsaError::validation("CommActBal is required"))?,
                "CommActBal",
            )?,
        )
        .map_err(CbsaError::validation)
    }
}

#[derive(Debug, Serialize)]
pub struct CreaccResponseDto {
    #[serde(rename = "CreAcc")]
    pub creacc: CreaccCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct CreaccCommareaResponseDto {
    #[serde(rename = "CommEyecatcher")]
    pub comm_eyecatcher: &'static str,
    #[serde(rename = "CommCustno")]
    pub comm_custno: i64,
    #[serde(rename = "CommKey")]
    pub comm_key: CreaccKeyResponseDto,
    #[serde(rename = "CommAccType")]
    pub comm_acc_type: String,
    #[serde(rename = "CommIntRt")]
    pub comm_int_rt: String,
    #[serde(rename = "CommOpened")]
    pub comm_opened: u32,
    #[serde(rename = "CommOverdrLim")]
    pub comm_overdr_lim: String,
    #[serde(rename = "CommLastStmtDt")]
    pub comm_last_stmt_dt: u32,
    #[serde(rename = "CommNextStmtDt")]
    pub comm_next_stmt_dt: u32,
    #[serde(rename = "CommAvailBal")]
    pub comm_avail_bal: String,
    #[serde(rename = "CommActBal")]
    pub comm_act_bal: String,
    #[serde(rename = "CommSuccess")]
    pub comm_success: &'static str,
    #[serde(rename = "CommFailCode")]
    pub comm_fail_code: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CreaccKeyResponseDto {
    #[serde(rename = "CommSortcode")]
    pub comm_sortcode: String,
    #[serde(rename = "CommNumber")]
    pub comm_number: i64,
}

impl CreaccResponseDto {
    fn from_result(result: &CreaccResult) -> Self {
        let account = result
            .account()
            .expect("successful CREACC results must include an account");

        Self {
            creacc: CreaccCommareaResponseDto {
                comm_eyecatcher: EYE_CATCHER,
                comm_custno: account.customer_number(),
                comm_key: CreaccKeyResponseDto {
                    comm_sortcode: account.sortcode().to_string(),
                    comm_number: account.account_number(),
                },
                comm_acc_type: account.account_type().to_string(),
                comm_int_rt: decimal_string(account.interest_rate()),
                comm_opened: cobol_date(account.opened()),
                comm_overdr_lim: decimal_string(account.overdraft_limit()),
                comm_last_stmt_dt: cobol_date(
                    account
                        .last_statement_date()
                        .expect("CREACC must populate last statement date"),
                ),
                comm_next_stmt_dt: cobol_date(
                    account
                        .next_statement_date()
                        .expect("CREACC must populate next statement date"),
                ),
                comm_avail_bal: decimal_string(account.available_balance()),
                comm_act_bal: decimal_string(account.actual_balance()),
                comm_success: "Y",
                comm_fail_code: "0",
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

fn validate_non_blank_account_type(value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        let mut err = ValidationError::new("non_blank");
        err.message = Some("CommAccType must not be blank".into());
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

fn validate_comm_int_rt(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "CommIntRt", |decimal| {
        decimal.scale() <= 2 && *decimal >= Decimal::ZERO && *decimal <= Decimal::new(999_999, 2)
    })
}

fn validate_comm_overdr_lim(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "CommOverdrLim", |decimal| {
        decimal.scale() <= 2
            && *decimal >= Decimal::ZERO
            && *decimal <= Decimal::new(99_999_999, 0)
            && decimal.fract().is_zero()
    })
}

fn validate_comm_balance(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "balance field", |decimal| {
        decimal.scale() <= 2
            && *decimal >= Decimal::new(-999_999_999_999, 2)
            && *decimal <= Decimal::new(999_999_999_999, 2)
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
