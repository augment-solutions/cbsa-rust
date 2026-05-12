use chrono::{DateTime, NaiveTime, Timelike, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    error::CbsaError,
    repository::xfrfun::{self, XfrfunCommand, XfrfunOutcome},
};

const ACCOUNT_NUMBER_RANGE_MESSAGE: &str = "account_number must be between 0 and 99999999";
const AMOUNT_RANGE_MESSAGE: &str = "amount must be between -9999999999.99 and 9999999999.99";
const AMOUNT_SCALE_MESSAGE: &str = "amount must have at most 2 fractional digits";
const SORTCODE_MESSAGE: &str = "sortcode must be exactly 6 ASCII digits";
const INVALID_AMOUNT_CODE: &str = "4";
const INVALID_AMOUNT_MESSAGE: &str = "Please supply an amount greater than zero.";
const SAME_ACCOUNT_ABEND_CODE: &str = "SAME";
const MAX_ACCOUNT_NUMBER: i64 = 99_999_999;
const MAX_AMOUNT_MANTISSA: i64 = 999_999_999_999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XfrfunRequest {
    pub from_account_number: i64,
    pub to_account_number: i64,
    pub amount: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XfrfunResult {
    from_available_balance: Option<Decimal>,
    from_actual_balance: Option<Decimal>,
    to_available_balance: Option<Decimal>,
    to_actual_balance: Option<Decimal>,
    fail_code: &'static str,
    message: Option<String>,
}

impl XfrfunResult {
    pub fn success(
        from_available_balance: Decimal,
        from_actual_balance: Decimal,
        to_available_balance: Decimal,
        to_actual_balance: Decimal,
    ) -> Self {
        Self {
            from_available_balance: Some(from_available_balance),
            from_actual_balance: Some(from_actual_balance),
            to_available_balance: Some(to_available_balance),
            to_actual_balance: Some(to_actual_balance),
            fail_code: "0",
            message: None,
        }
    }

    pub fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            from_available_balance: None,
            from_actual_balance: None,
            to_available_balance: None,
            to_actual_balance: None,
            fail_code,
            message: Some(message.into()),
        }
    }

    pub fn transfer_success(&self) -> bool {
        self.from_available_balance.is_some()
    }

    pub fn from_available_balance(&self) -> Option<Decimal> {
        self.from_available_balance
    }

    pub fn from_actual_balance(&self) -> Option<Decimal> {
        self.from_actual_balance
    }

    pub fn to_available_balance(&self) -> Option<Decimal> {
        self.to_available_balance
    }

    pub fn to_actual_balance(&self) -> Option<Decimal> {
        self.to_actual_balance
    }

    pub fn fail_code(&self) -> &str {
        self.fail_code
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_from_account_not_found_failure(&self) -> bool {
        !self.transfer_success() && self.fail_code == "1"
    }

    pub fn is_to_account_not_found_failure(&self) -> bool {
        !self.transfer_success() && self.fail_code == "2"
    }

    pub fn is_insufficient_funds_failure(&self) -> bool {
        !self.transfer_success() && self.fail_code == "3"
    }

    pub fn is_invalid_amount_failure(&self) -> bool {
        !self.transfer_success() && self.fail_code == INVALID_AMOUNT_CODE
    }
}

pub async fn transfer(
    pool: &PgPool,
    sortcode: &str,
    request: XfrfunRequest,
) -> Result<XfrfunResult, CbsaError> {
    let clock = SystemClock;
    transfer_with_dependencies(pool, sortcode, request, &clock).await
}

async fn transfer_with_dependencies(
    pool: &PgPool,
    sortcode: &str,
    request: XfrfunRequest,
    clock: &dyn Clock,
) -> Result<XfrfunResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_request(&request)?;

    if request.amount <= Decimal::ZERO {
        return Ok(XfrfunResult::failure(
            INVALID_AMOUNT_CODE,
            INVALID_AMOUNT_MESSAGE,
        ));
    }

    if request.from_account_number == request.to_account_number {
        return Err(CbsaError::abend(
            SAME_ACCOUNT_ABEND_CODE,
            "XFRFUN cannot transfer funds to the same account.",
        ));
    }

    let now = clock.now();
    let outcome = xfrfun::transfer_funds(
        pool,
        XfrfunCommand {
            sortcode: sortcode.to_string(),
            from_account_number: request.from_account_number,
            to_account_number: request.to_account_number,
            amount: round_money(request.amount),
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: now.date_naive(),
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        XfrfunOutcome::Success(transfer) => XfrfunResult::success(
            transfer.from_account.available_balance(),
            transfer.from_account.actual_balance(),
            transfer.to_account.available_balance(),
            transfer.to_account.actual_balance(),
        ),
        XfrfunOutcome::Failure { fail_code, message } => XfrfunResult::failure(fail_code, message),
    })
}

fn validate_sortcode(sortcode: &str) -> Result<(), CbsaError> {
    if is_six_ascii_digits(sortcode) {
        Ok(())
    } else {
        Err(CbsaError::validation(SORTCODE_MESSAGE))
    }
}

fn validate_request(request: &XfrfunRequest) -> Result<(), CbsaError> {
    for account_number in [request.from_account_number, request.to_account_number] {
        if !(0..=MAX_ACCOUNT_NUMBER).contains(&account_number) {
            return Err(CbsaError::validation(ACCOUNT_NUMBER_RANGE_MESSAGE));
        }
    }

    if request.amount.scale() > 2 {
        return Err(CbsaError::validation(AMOUNT_SCALE_MESSAGE));
    }

    if request.amount < min_amount() || request.amount > max_amount() {
        return Err(CbsaError::validation(AMOUNT_RANGE_MESSAGE));
    }

    Ok(())
}

fn min_amount() -> Decimal {
    Decimal::from_i128_with_scale(-(MAX_AMOUNT_MANTISSA as i128), 2)
}

fn max_amount() -> Decimal {
    Decimal::from_i128_with_scale(MAX_AMOUNT_MANTISSA as i128, 2)
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
    use super::{validate_request, XfrfunRequest};
    use rust_decimal::Decimal;

    #[test]
    fn rejects_amounts_with_more_than_two_fractional_digits() {
        let request = XfrfunRequest {
            from_account_number: 12_345_678,
            to_account_number: 87_654_321,
            amount: Decimal::new(12_345, 3),
        };

        assert_eq!(
            validate_request(&request).unwrap_err().to_string(),
            "validation: amount must have at most 2 fractional digits"
        );
    }
}
