use chrono::{DateTime, NaiveTime, Timelike, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    domain::AccountDetails,
    error::CbsaError,
    repository::dbcrfun::{self, DbcrfunCommand, DbcrfunOutcome},
};

const ACCOUNT_NUMBER_RANGE_MESSAGE: &str = "account_number must be between 0 and 99999999";
const AMOUNT_RANGE_MESSAGE: &str = "amount must be between -9999999999.99 and 9999999999.99";
const AMOUNT_SCALE_MESSAGE: &str = "amount must have at most 2 fractional digits";
const SORTCODE_MESSAGE: &str = "sortcode must be exactly 6 ASCII digits";
const FACILITY_TYPE_RANGE_MESSAGE: &str = "facility_type must be between -99999999 and 99999999";
const MAX_ACCOUNT_NUMBER: i64 = 99_999_999;
const MAX_AMOUNT_MANTISSA: i64 = 999_999_999_999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbcrfunRequest {
    pub account_number: i64,
    pub amount: Decimal,
    pub origin: DbcrfunOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbcrfunOrigin {
    pub applid: String,
    pub userid: String,
    pub facility_name: String,
    pub netwrk_id: String,
    pub facility_type: i32,
    pub fill0: String,
}

impl DbcrfunOrigin {
    pub fn payment_description(&self) -> String {
        let header = format!(
            "{}{}",
            pad_or_truncate(&self.applid, 8),
            pad_or_truncate(&self.userid, 8)
        );
        header.chars().take(14).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbcrfunResult {
    account: Option<AccountDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl DbcrfunResult {
    pub fn success(account: AccountDetails) -> Self {
        Self {
            account: Some(account),
            fail_code: "0",
            message: None,
        }
    }

    pub fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            account: None,
            fail_code,
            message: Some(message.into()),
        }
    }

    pub fn payment_success(&self) -> bool {
        self.account.is_some()
    }

    pub fn account(&self) -> Option<&AccountDetails> {
        self.account.as_ref()
    }

    pub fn fail_code(&self) -> &str {
        self.fail_code
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_not_found_failure(&self) -> bool {
        !self.payment_success() && self.fail_code == "1"
    }

    pub fn is_insufficient_funds_failure(&self) -> bool {
        !self.payment_success() && self.fail_code == "3"
    }

    pub fn is_disallowed_account_type_failure(&self) -> bool {
        !self.payment_success() && self.fail_code == "4"
    }
}

pub async fn process(
    pool: &PgPool,
    sortcode: &str,
    request: DbcrfunRequest,
) -> Result<DbcrfunResult, CbsaError> {
    let clock = SystemClock;
    process_with_dependencies(pool, sortcode, request, &clock).await
}

async fn process_with_dependencies(
    pool: &PgPool,
    sortcode: &str,
    request: DbcrfunRequest,
    clock: &dyn Clock,
) -> Result<DbcrfunResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_request(&request)?;

    let now = clock.now();
    let outcome = dbcrfun::post_transaction(
        pool,
        DbcrfunCommand {
            sortcode: sortcode.to_string(),
            account_number: request.account_number,
            amount: round_money(request.amount),
            facility_type: request.origin.facility_type,
            payment_origin_description: request.origin.payment_description(),
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: now.date_naive(),
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        DbcrfunOutcome::Success(account) => DbcrfunResult::success(account),
        DbcrfunOutcome::Failure { fail_code, message } => {
            DbcrfunResult::failure(fail_code, message)
        }
    })
}

fn validate_sortcode(sortcode: &str) -> Result<(), CbsaError> {
    if is_six_ascii_digits(sortcode) {
        Ok(())
    } else {
        Err(CbsaError::validation(SORTCODE_MESSAGE))
    }
}

fn validate_request(request: &DbcrfunRequest) -> Result<(), CbsaError> {
    if !(0..=MAX_ACCOUNT_NUMBER).contains(&request.account_number) {
        return Err(CbsaError::validation(ACCOUNT_NUMBER_RANGE_MESSAGE));
    }

    if request.amount.scale() > 2 {
        return Err(CbsaError::validation(AMOUNT_SCALE_MESSAGE));
    }

    if request.amount < min_amount() || request.amount > max_amount() {
        return Err(CbsaError::validation(AMOUNT_RANGE_MESSAGE));
    }

    validate_origin(&request.origin)
}

fn validate_origin(origin: &DbcrfunOrigin) -> Result<(), CbsaError> {
    validate_length(&origin.applid, 8, "applid")?;
    validate_length(&origin.userid, 8, "userid")?;
    validate_length(&origin.facility_name, 8, "facility_name")?;
    validate_length(&origin.netwrk_id, 8, "netwrk_id")?;
    validate_length(&origin.fill0, 4, "fill0")?;

    if !(-99_999_999..=99_999_999).contains(&origin.facility_type) {
        return Err(CbsaError::validation(FACILITY_TYPE_RANGE_MESSAGE));
    }

    Ok(())
}

fn min_amount() -> Decimal {
    Decimal::from_i128_with_scale(-(MAX_AMOUNT_MANTISSA as i128), 2)
}

fn max_amount() -> Decimal {
    Decimal::from_i128_with_scale(MAX_AMOUNT_MANTISSA as i128, 2)
}

fn validate_length(value: &str, max_chars: usize, field_name: &str) -> Result<(), CbsaError> {
    if value.chars().count() <= max_chars {
        Ok(())
    } else {
        Err(CbsaError::validation(format!(
            "{field_name} must be at most {max_chars} characters"
        )))
    }
}

fn pad_or_truncate(value: &str, width: usize) -> String {
    let truncated: String = value.chars().take(width).collect();
    let padding = width.saturating_sub(truncated.chars().count());
    format!("{truncated}{}", " ".repeat(padding))
}

fn round_money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
}

trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_request, DbcrfunOrigin, DbcrfunRequest};
    use rust_decimal::Decimal;

    #[test]
    fn payment_description_uses_first_fourteen_characters_of_applid_and_userid() {
        let origin = DbcrfunOrigin {
            applid: "ABCDEFGH".to_string(),
            userid: "12345678".to_string(),
            facility_name: "PAYAPI".to_string(),
            netwrk_id: "NET00001".to_string(),
            facility_type: 496,
            fill0: String::new(),
        };

        assert_eq!(origin.payment_description(), "ABCDEFGH123456");
    }

    #[test]
    fn rejects_amounts_with_more_than_two_fractional_digits() {
        let request = DbcrfunRequest {
            account_number: 12_345_678,
            amount: Decimal::new(12_345, 3),
            origin: DbcrfunOrigin {
                applid: String::new(),
                userid: String::new(),
                facility_name: String::new(),
                netwrk_id: String::new(),
                facility_type: 0,
                fill0: String::new(),
            },
        };

        assert_eq!(
            validate_request(&request).unwrap_err().to_string(),
            "validation: amount must have at most 2 fractional digits"
        );
    }
}
