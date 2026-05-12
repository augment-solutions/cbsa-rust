use std::str::FromStr;

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use rust_decimal::Decimal;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use validator::{Validate, ValidationError};

use crate::{
    config::is_six_ascii_digits,
    error::{problem_response, CbsaError, ProblemDetail},
    service::dbcrfun::{self, DbcrfunOrigin, DbcrfunRequest, DbcrfunResult},
    web::AppState,
};

const MAX_AMOUNT_MANTISSA: i64 = 999_999_999_999;

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/dbcrfun", post(post_payment))
}

#[axum::debug_handler(state = AppState)]
async fn post_payment(
    State(state): State<AppState>,
    payload: Result<Json<DbcrfunRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;

    let commarea = payload
        .paydbcr
        .ok_or_else(|| CbsaError::validation("PAYDBCR is required"))?;
    let comm_accno = commarea
        .comm_accno
        .ok_or_else(|| CbsaError::validation("CommAccno is required"))?;
    let comm_amt = parse_decimal_field(
        commarea
            .comm_amt
            .ok_or_else(|| CbsaError::validation("CommAmt is required"))?,
        "CommAmt",
    )?;
    let origin_payload = commarea
        .comm_origin
        .ok_or_else(|| CbsaError::validation("CommOrigin is required"))?;

    let origin = DbcrfunOrigin {
        applid: origin_payload.comm_applid.unwrap_or_default(),
        userid: origin_payload.comm_userid.unwrap_or_default(),
        facility_name: origin_payload.comm_facility_name.unwrap_or_default(),
        netwrk_id: origin_payload.comm_netwrk_id.unwrap_or_default(),
        facility_type: origin_payload.comm_faciltype.unwrap_or_default(),
        fill0: origin_payload.fill0.unwrap_or_default(),
    };

    let request = DbcrfunRequest {
        account_number: parse_account_number(&comm_accno)?,
        amount: comm_amt,
        origin: origin.clone(),
    };

    let result = dbcrfun::process(&state.pool, &state.sortcode, request).await?;

    if result.payment_success() {
        Ok(Json(DbcrfunResponseDto::from_result(
            &result,
            &comm_accno,
            comm_amt,
            &state.sortcode,
            &origin,
        ))
        .into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &DbcrfunResult) -> Response {
    let (status, title) = if result.is_not_found_failure() {
        (StatusCode::NOT_FOUND, "Account not found")
    } else if result.is_insufficient_funds_failure() {
        (StatusCode::CONFLICT, "Insufficient funds")
    } else if result.is_disallowed_account_type_failure() {
        (StatusCode::CONFLICT, "Payment not permitted")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Payment failed")
    };

    let detail = result.message().unwrap_or("Payment failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct DbcrfunRequestDto {
    #[serde(rename = "PAYDBCR")]
    #[validate(required, nested)]
    pub paydbcr: Option<DbcrfunCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct DbcrfunCommareaRequestDto {
    #[serde(rename = "CommAccno")]
    #[validate(required, custom(function = "validate_comm_accno"))]
    pub comm_accno: Option<String>,

    #[serde(
        rename = "CommAmt",
        deserialize_with = "deserialize_optional_stringified"
    )]
    #[validate(required, custom(function = "validate_comm_amount"))]
    pub comm_amt: Option<String>,

    #[serde(rename = "mSortC")]
    #[validate(custom(function = "validate_optional_sortcode"))]
    pub m_sort_c: Option<String>,

    #[serde(
        rename = "CommAvBal",
        deserialize_with = "deserialize_optional_stringified"
    )]
    #[validate(custom(function = "validate_optional_balance"))]
    pub comm_av_bal: Option<String>,

    #[serde(
        rename = "CommActBal",
        deserialize_with = "deserialize_optional_stringified"
    )]
    #[validate(custom(function = "validate_optional_balance"))]
    pub comm_act_bal: Option<String>,

    #[serde(rename = "CommOrigin")]
    #[validate(required, nested)]
    pub comm_origin: Option<DbcrfunOriginRequestDto>,

    #[serde(rename = "CommSuccess")]
    #[validate(length(max = 1, message = "CommSuccess must be at most 1 character"))]
    pub comm_success: Option<String>,

    #[serde(rename = "CommFailCode")]
    #[validate(length(max = 1, message = "CommFailCode must be at most 1 character"))]
    pub comm_fail_code: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct DbcrfunOriginRequestDto {
    #[serde(rename = "CommApplid")]
    #[validate(length(max = 8, message = "CommApplid must be at most 8 characters"))]
    pub comm_applid: Option<String>,

    #[serde(rename = "CommUserid")]
    #[validate(length(max = 8, message = "CommUserid must be at most 8 characters"))]
    pub comm_userid: Option<String>,

    #[serde(rename = "CommFacilityName")]
    #[validate(length(max = 8, message = "CommFacilityName must be at most 8 characters"))]
    pub comm_facility_name: Option<String>,

    #[serde(rename = "CommNetwrkId")]
    #[validate(length(max = 8, message = "CommNetwrkId must be at most 8 characters"))]
    pub comm_netwrk_id: Option<String>,

    #[serde(rename = "CommFaciltype")]
    #[validate(
        required,
        range(
            min = -99_999_999,
            max = 99_999_999,
            message = "CommFaciltype must be between -99999999 and 99999999"
        )
    )]
    pub comm_faciltype: Option<i32>,

    #[serde(rename = "Fill0")]
    #[validate(length(max = 4, message = "Fill0 must be at most 4 characters"))]
    pub fill0: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DbcrfunResponseDto {
    #[serde(rename = "PAYDBCR")]
    pub paydbcr: DbcrfunCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct DbcrfunCommareaResponseDto {
    #[serde(rename = "CommAccno")]
    pub comm_accno: String,
    #[serde(rename = "CommAmt")]
    pub comm_amt: String,
    #[serde(rename = "mSortC")]
    pub m_sort_c: String,
    #[serde(rename = "CommAvBal")]
    pub comm_av_bal: String,
    #[serde(rename = "CommActBal")]
    pub comm_act_bal: String,
    #[serde(rename = "CommOrigin")]
    pub comm_origin: DbcrfunOriginResponseDto,
    #[serde(rename = "CommSuccess")]
    pub comm_success: &'static str,
    #[serde(rename = "CommFailCode")]
    pub comm_fail_code: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DbcrfunOriginResponseDto {
    #[serde(rename = "CommApplid")]
    pub comm_applid: String,
    #[serde(rename = "CommUserid")]
    pub comm_userid: String,
    #[serde(rename = "CommFacilityName")]
    pub comm_facility_name: String,
    #[serde(rename = "CommNetwrkId")]
    pub comm_netwrk_id: String,
    #[serde(rename = "CommFaciltype")]
    pub comm_faciltype: i32,
    #[serde(rename = "Fill0")]
    pub fill0: String,
}

impl DbcrfunResponseDto {
    fn from_result(
        result: &DbcrfunResult,
        comm_accno: &str,
        comm_amt: Decimal,
        sortcode: &str,
        origin: &DbcrfunOrigin,
    ) -> Self {
        let account = result
            .account()
            .expect("successful DBCRFUN results must include an account");

        Self {
            paydbcr: DbcrfunCommareaResponseDto {
                comm_accno: comm_accno.to_string(),
                comm_amt: decimal_string(comm_amt),
                m_sort_c: sortcode.to_string(),
                comm_av_bal: decimal_string(account.available_balance()),
                comm_act_bal: decimal_string(account.actual_balance()),
                comm_origin: DbcrfunOriginResponseDto {
                    comm_applid: origin.applid.clone(),
                    comm_userid: origin.userid.clone(),
                    comm_facility_name: origin.facility_name.clone(),
                    comm_netwrk_id: origin.netwrk_id.clone(),
                    comm_faciltype: origin.facility_type,
                    fill0: origin.fill0.clone(),
                },
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

fn parse_decimal_field(value: String, field_name: &str) -> Result<Decimal, CbsaError> {
    Decimal::from_str(&value).map_err(|_| CbsaError::validation(format!("{field_name} is invalid")))
}

fn parse_account_number(value: &str) -> Result<i64, CbsaError> {
    value
        .parse()
        .map_err(|_| CbsaError::validation("CommAccno must contain 1 to 8 ASCII digits"))
}

fn validate_comm_accno(value: &str) -> Result<(), ValidationError> {
    if (1..=8).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        let mut err = ValidationError::new("account_number");
        err.message = Some("CommAccno must contain 1 to 8 ASCII digits".into());
        Err(err)
    }
}

fn validate_comm_amount(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "CommAmt", |decimal| {
        decimal.scale() <= 2 && *decimal >= min_amount() && *decimal <= max_amount()
    })
}

fn validate_optional_sortcode(value: &str) -> Result<(), ValidationError> {
    if is_six_ascii_digits(value) {
        Ok(())
    } else {
        let mut err = ValidationError::new("sortcode");
        err.message = Some("mSortC must be exactly 6 ASCII digits".into());
        Err(err)
    }
}

fn validate_optional_balance(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "balance field", |decimal| {
        decimal.scale() <= 2 && *decimal >= min_amount() && *decimal <= max_amount()
    })
}

fn min_amount() -> Decimal {
    Decimal::from_i128_with_scale(-(MAX_AMOUNT_MANTISSA as i128), 2)
}

fn max_amount() -> Decimal {
    Decimal::from_i128_with_scale(MAX_AMOUNT_MANTISSA as i128, 2)
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
