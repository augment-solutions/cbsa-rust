use chrono::{DateTime, NaiveTime, Timelike, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    domain::AccountDetails,
    error::CbsaError,
    repository::updacc::{self, UpdateAccountCommand, UpdateAccountOutcome},
};

const INVALID_ACCOUNT_TYPE_CODE: &str = "2";
const ACCOUNT_NUMBER_RANGE_MESSAGE: &str = "account_number must be between 0 and 99999999";
const ACCOUNT_TYPE_LENGTH_MESSAGE: &str = "account_type must be at most 8 characters";
const INTEREST_RATE_RANGE_MESSAGE: &str = "interest_rate must be between 0.00 and 9999.99";
const OVERDRAFT_LIMIT_RANGE_MESSAGE: &str = "overdraft_limit must be between 0 and 99999999";
const OVERDRAFT_LIMIT_SCALE_MESSAGE: &str =
    "overdraft_limit must not include non-zero fractional digits";
const SORTCODE_MESSAGE: &str = "sortcode must be exactly 6 ASCII digits";
const MAX_ACCOUNT_NUMBER: i64 = 99_999_999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaccRequest {
    pub account_number: i64,
    pub account_type: String,
    pub interest_rate: Decimal,
    pub overdraft_limit: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaccResult {
    account: Option<AccountDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl UpdaccResult {
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

    pub fn update_success(&self) -> bool {
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
        !self.update_success() && self.fail_code == "1"
    }

    pub fn is_validation_failure(&self) -> bool {
        !self.update_success() && self.fail_code == INVALID_ACCOUNT_TYPE_CODE
    }
}

pub async fn update(
    pool: &PgPool,
    sortcode: &str,
    request: UpdaccRequest,
) -> Result<UpdaccResult, CbsaError> {
    let clock = SystemClock;
    update_with_dependencies(pool, sortcode, request, &clock).await
}

async fn update_with_dependencies(
    pool: &PgPool,
    sortcode: &str,
    request: UpdaccRequest,
    clock: &dyn Clock,
) -> Result<UpdaccResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_request(&request)?;

    if invalid_account_type(&request.account_type) {
        return Ok(UpdaccResult::failure(
            INVALID_ACCOUNT_TYPE_CODE,
            "Account type must not be blank or start with a space.",
        ));
    }

    let now = clock.now();
    let outcome = updacc::update_account(
        pool,
        UpdateAccountCommand {
            sortcode: sortcode.to_string(),
            account_number: request.account_number,
            account_type: request.account_type,
            interest_rate: round_money(request.interest_rate),
            overdraft_limit: round_money(request.overdraft_limit),
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: now.date_naive(),
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        UpdateAccountOutcome::Success(account) => UpdaccResult::success(account),
        UpdateAccountOutcome::Failure { fail_code, message } => {
            UpdaccResult::failure(fail_code, message)
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

fn validate_request(request: &UpdaccRequest) -> Result<(), CbsaError> {
    if !(0..=MAX_ACCOUNT_NUMBER).contains(&request.account_number) {
        return Err(CbsaError::validation(ACCOUNT_NUMBER_RANGE_MESSAGE));
    }

    if request.account_type.chars().count() > 8 {
        return Err(CbsaError::validation(ACCOUNT_TYPE_LENGTH_MESSAGE));
    }

    validate_money_range(
        request.interest_rate,
        Decimal::ZERO,
        Decimal::new(999_999, 2),
        INTEREST_RATE_RANGE_MESSAGE,
    )
    .map_err(CbsaError::validation)?;

    validate_money_range(
        request.overdraft_limit,
        Decimal::ZERO,
        Decimal::new(99_999_999, 0),
        OVERDRAFT_LIMIT_RANGE_MESSAGE,
    )
    .map_err(CbsaError::validation)?;

    if !request.overdraft_limit.fract().is_zero() {
        return Err(CbsaError::validation(OVERDRAFT_LIMIT_SCALE_MESSAGE));
    }

    Ok(())
}

fn invalid_account_type(account_type: &str) -> bool {
    account_type.trim().is_empty() || account_type.starts_with(' ')
}

fn validate_money_range(
    value: Decimal,
    min: Decimal,
    max: Decimal,
    message: &str,
) -> Result<(), String> {
    if value.scale() > 2 || value < min || value > max {
        Err(message.to_string())
    } else {
        Ok(())
    }
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
    use super::{invalid_account_type, validate_request, UpdaccRequest};
    use rust_decimal::Decimal;

    #[test]
    fn leading_space_account_types_are_business_failures_not_validation_errors() {
        let request = UpdaccRequest {
            account_number: 12_345_678,
            account_type: " ISA".to_string(),
            interest_rate: Decimal::new(225, 2),
            overdraft_limit: Decimal::new(500, 0),
        };

        assert!(validate_request(&request).is_ok());
        assert!(invalid_account_type(&request.account_type));
    }

    #[test]
    fn rejects_fractional_overdraft_limits() {
        let request = UpdaccRequest {
            account_number: 12_345_678,
            account_type: "ISA".to_string(),
            interest_rate: Decimal::new(225, 2),
            overdraft_limit: Decimal::new(5005, 1),
        };

        assert_eq!(
            validate_request(&request).unwrap_err().to_string(),
            "validation: overdraft_limit must not include non-zero fractional digits"
        );
    }
}
