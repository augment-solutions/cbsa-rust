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
    service::xfrfun::{self, XfrfunRequest, XfrfunResult},
    web::AppState,
};

const MAX_AMOUNT_MANTISSA: i64 = 999_999_999_999;

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/xfrfun", post(post_transfer))
}

#[axum::debug_handler(state = AppState)]
async fn post_transfer(
    State(state): State<AppState>,
    payload: Result<Json<XfrfunRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;

    let commarea = payload
        .xfrfun
        .ok_or_else(|| CbsaError::validation("XFRFUN is required"))?;
    let from_account_number = commarea
        .comm_faccno
        .ok_or_else(|| CbsaError::validation("CommFaccno is required"))?;
    let to_account_number = commarea
        .comm_taccno
        .ok_or_else(|| CbsaError::validation("CommTaccno is required"))?;
    let amount = parse_decimal_field(
        commarea
            .comm_amt
            .ok_or_else(|| CbsaError::validation("CommAmt is required"))?,
        "CommAmt",
    )?;

    let result = xfrfun::transfer(
        &state.pool,
        &state.sortcode,
        XfrfunRequest {
            from_account_number,
            to_account_number,
            amount,
        },
    )
    .await?;

    if result.transfer_success() {
        Ok(Json(XfrfunResponseDto::from_result(
            &result,
            &state.sortcode,
            from_account_number,
            to_account_number,
            amount,
        ))
        .into_response())
    } else {
        Ok(failure_response(&result))
    }
}

fn failure_response(result: &XfrfunResult) -> Response {
    let (status, title) = if result.is_from_account_not_found_failure() {
        (StatusCode::NOT_FOUND, "From account not found")
    } else if result.is_to_account_not_found_failure() {
        (StatusCode::NOT_FOUND, "To account not found")
    } else if result.is_insufficient_funds_failure() {
        (StatusCode::CONFLICT, "Insufficient funds")
    } else if result.is_invalid_amount_failure() {
        (StatusCode::BAD_REQUEST, "Invalid transfer amount")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Transfer failed")
    };

    let detail = result.message().unwrap_or("Transfer failed.");
    let problem_detail =
        ProblemDetail::new(status, title, detail).with_fail_code(result.fail_code());
    problem_response(status, &problem_detail)
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct XfrfunRequestDto {
    #[serde(rename = "XFRFUN")]
    #[validate(required, nested)]
    pub xfrfun: Option<XfrfunCommareaRequestDto>,
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct XfrfunCommareaRequestDto {
    #[serde(rename = "CommFaccno")]
    #[validate(
        required,
        range(
            min = 0,
            max = 99_999_999_i64,
            message = "CommFaccno must be between 0 and 99999999"
        )
    )]
    pub comm_faccno: Option<i64>,

    #[serde(rename = "CommFscode")]
    #[validate(required, custom(function = "validate_from_sortcode"))]
    pub comm_fscode: Option<String>,

    #[serde(rename = "CommTaccno")]
    #[validate(
        required,
        range(
            min = 0,
            max = 99_999_999_i64,
            message = "CommTaccno must be between 0 and 99999999"
        )
    )]
    pub comm_taccno: Option<i64>,

    #[serde(rename = "CommTscode")]
    #[validate(required, custom(function = "validate_to_sortcode"))]
    pub comm_tscode: Option<String>,

    #[serde(
        rename = "CommAmt",
        deserialize_with = "deserialize_optional_stringified"
    )]
    #[validate(required, custom(function = "validate_comm_amount"))]
    pub comm_amt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct XfrfunResponseDto {
    #[serde(rename = "XFRFUN")]
    pub xfrfun: XfrfunCommareaResponseDto,
}

#[derive(Debug, Serialize)]
pub struct XfrfunCommareaResponseDto {
    #[serde(rename = "CommFaccno")]
    pub comm_faccno: i64,
    #[serde(rename = "CommFscode")]
    pub comm_fscode: String,
    #[serde(rename = "CommTaccno")]
    pub comm_taccno: i64,
    #[serde(rename = "CommTscode")]
    pub comm_tscode: String,
    #[serde(rename = "CommAmt")]
    pub comm_amt: String,
    #[serde(rename = "CommFavbal")]
    pub comm_favbal: String,
    #[serde(rename = "CommFactbal")]
    pub comm_factbal: String,
    #[serde(rename = "CommTavbal")]
    pub comm_tavbal: String,
    #[serde(rename = "CommTactbal")]
    pub comm_tactbal: String,
    #[serde(rename = "CommFailCode")]
    pub comm_fail_code: &'static str,
    #[serde(rename = "CommSuccess")]
    pub comm_success: &'static str,
}

impl XfrfunResponseDto {
    fn from_result(
        result: &XfrfunResult,
        sortcode: &str,
        from_account_number: i64,
        to_account_number: i64,
        amount: Decimal,
    ) -> Self {
        Self {
            xfrfun: XfrfunCommareaResponseDto {
                comm_faccno: from_account_number,
                comm_fscode: sortcode.to_string(),
                comm_taccno: to_account_number,
                comm_tscode: sortcode.to_string(),
                comm_amt: decimal_string(amount),
                comm_favbal: decimal_string(
                    result
                        .from_available_balance()
                        .expect("successful XFRFUN result must include from available balance"),
                ),
                comm_factbal: decimal_string(
                    result
                        .from_actual_balance()
                        .expect("successful XFRFUN result must include from actual balance"),
                ),
                comm_tavbal: decimal_string(
                    result
                        .to_available_balance()
                        .expect("successful XFRFUN result must include to available balance"),
                ),
                comm_tactbal: decimal_string(
                    result
                        .to_actual_balance()
                        .expect("successful XFRFUN result must include to actual balance"),
                ),
                comm_fail_code: "0",
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

fn parse_decimal_field(value: String, field_name: &str) -> Result<Decimal, CbsaError> {
    Decimal::from_str(&value).map_err(|_| CbsaError::validation(format!("{field_name} is invalid")))
}

fn validate_from_sortcode(value: &str) -> Result<(), ValidationError> {
    validate_sortcode(value, "CommFscode must be exactly 6 ASCII digits")
}

fn validate_to_sortcode(value: &str) -> Result<(), ValidationError> {
    validate_sortcode(value, "CommTscode must be exactly 6 ASCII digits")
}

fn validate_sortcode(value: &str, message: &'static str) -> Result<(), ValidationError> {
    if is_six_ascii_digits(value) {
        Ok(())
    } else {
        let mut err = ValidationError::new("sortcode");
        err.message = Some(message.into());
        Err(err)
    }
}

fn validate_comm_amount(value: &str) -> Result<(), ValidationError> {
    validate_decimal(value, "CommAmt", |decimal| {
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
