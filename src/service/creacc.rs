use chrono::{DateTime, Days, NaiveTime, Timelike, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    domain::AccountDetails,
    error::CbsaError,
    repository::creacc::{self, CreateAccountCommand, CreateAccountOutcome},
    service::{
        inqacccu::{self, InqacccuRequest},
        inqcust::{self, InqcustRequest, SystemRandomCustomerNumberGenerator},
    },
};

const INVALID_ACCOUNT_TYPE_CODE: &str = "A";
const NOT_FOUND_CODE: &str = "1";
const CAPACITY_FAIL_CODE: &str = "8";
const MAX_ACCOUNTS_PER_CUSTOMER: usize = 9;
const RANDOM_CUSTOMER_NUMBER: i64 = 0;
const LAST_CUSTOMER_NUMBER: i64 = 9_999_999_999;
const CUSTOMER_NUMBER_RANGE_MESSAGE: &str = "customer_number must be between 0 and 9999999999";
const ACCOUNT_TYPE_LENGTH_MESSAGE: &str = "account_type must be at most 8 characters";
const ACCOUNT_TYPE_BLANK_MESSAGE: &str = "account_type must not be blank";
const INTEREST_RATE_RANGE_MESSAGE: &str = "interest_rate must be between 0.00 and 9999.99";
const OVERDRAFT_LIMIT_RANGE_MESSAGE: &str = "overdraft_limit must be between 0 and 99999999";
const OVERDRAFT_LIMIT_SCALE_MESSAGE: &str =
    "overdraft_limit must not include non-zero fractional digits";
const BALANCE_RANGE_MESSAGE: &str = "balances must be between -9999999999.99 and 9999999999.99";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreaccRequest {
    customer_number: i64,
    account_type: String,
    interest_rate: Decimal,
    overdraft_limit: Decimal,
    available_balance: Decimal,
    actual_balance: Decimal,
}

impl CreaccRequest {
    pub fn new(
        customer_number: i64,
        account_type: String,
        interest_rate: Decimal,
        overdraft_limit: Decimal,
        available_balance: Decimal,
        actual_balance: Decimal,
    ) -> Result<Self, String> {
        if !(0..=LAST_CUSTOMER_NUMBER).contains(&customer_number) {
            return Err(CUSTOMER_NUMBER_RANGE_MESSAGE.to_string());
        }

        if account_type.chars().count() > 8 {
            return Err(ACCOUNT_TYPE_LENGTH_MESSAGE.to_string());
        }

        let account_type = account_type.trim().to_string();
        if account_type.is_empty() {
            return Err(ACCOUNT_TYPE_BLANK_MESSAGE.to_string());
        }

        validate_money_range(
            interest_rate,
            Decimal::ZERO,
            Decimal::new(999_999, 2),
            INTEREST_RATE_RANGE_MESSAGE,
        )?;
        validate_money_range(
            overdraft_limit,
            Decimal::ZERO,
            Decimal::new(99_999_999, 0),
            OVERDRAFT_LIMIT_RANGE_MESSAGE,
        )?;
        if !overdraft_limit.fract().is_zero() {
            return Err(OVERDRAFT_LIMIT_SCALE_MESSAGE.to_string());
        }
        validate_money_range(
            available_balance,
            Decimal::new(-999_999_999_999, 2),
            Decimal::new(999_999_999_999, 2),
            BALANCE_RANGE_MESSAGE,
        )?;
        validate_money_range(
            actual_balance,
            Decimal::new(-999_999_999_999, 2),
            Decimal::new(999_999_999_999, 2),
            BALANCE_RANGE_MESSAGE,
        )?;

        Ok(Self {
            customer_number,
            account_type,
            interest_rate: round_money(interest_rate),
            overdraft_limit: round_money(overdraft_limit),
            available_balance: round_money(available_balance),
            actual_balance: round_money(actual_balance),
        })
    }

    pub fn customer_number(&self) -> i64 {
        self.customer_number
    }

    pub fn account_type(&self) -> &str {
        &self.account_type
    }

    pub fn interest_rate(&self) -> Decimal {
        self.interest_rate
    }

    pub fn overdraft_limit(&self) -> Decimal {
        self.overdraft_limit
    }

    pub fn available_balance(&self) -> Decimal {
        self.available_balance
    }

    pub fn actual_balance(&self) -> Decimal {
        self.actual_balance
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreaccResult {
    account: Option<AccountDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl CreaccResult {
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

    pub fn creation_success(&self) -> bool {
        self.account.is_some()
    }

    pub fn account(&self) -> Option<&AccountDetails> {
        self.account.as_ref()
    }

    pub fn fail_code(&self) -> &'static str {
        self.fail_code
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_validation_failure(&self) -> bool {
        self.fail_code == INVALID_ACCOUNT_TYPE_CODE
    }

    pub fn is_not_found_failure(&self) -> bool {
        self.fail_code == NOT_FOUND_CODE
    }

    pub fn is_capacity_failure(&self) -> bool {
        self.fail_code == CAPACITY_FAIL_CODE
    }
}

pub async fn create(
    pool: &PgPool,
    sortcode: &str,
    request: CreaccRequest,
) -> Result<CreaccResult, CbsaError> {
    let clock = SystemClock;
    create_with_clock(pool, sortcode, request, &clock).await
}

async fn create_with_clock(
    pool: &PgPool,
    sortcode: &str,
    request: CreaccRequest,
    clock: &dyn Clock,
) -> Result<CreaccResult, CbsaError> {
    validate_sortcode(sortcode)?;

    if !is_supported_account_type(request.account_type()) {
        return Ok(CreaccResult::failure(
            INVALID_ACCOUNT_TYPE_CODE,
            "Account type must be ISA, MORTGAGE, SAVING, CURRENT, or LOAN.",
        ));
    }

    if is_reserved_customer_number(request.customer_number()) {
        return Ok(customer_not_found(request.customer_number()));
    }

    let mut generator = SystemRandomCustomerNumberGenerator::default();
    let customer_result = inqcust::inquire(
        pool,
        sortcode,
        InqcustRequest {
            customer_number: request.customer_number(),
        },
        &mut generator,
    )
    .await?;

    if !customer_result.inquiry_success() {
        return Ok(CreaccResult::failure(
            NOT_FOUND_CODE,
            customer_result
                .message()
                .unwrap_or("Customer was not found."),
        ));
    }

    let account_count_result = inqacccu::inquire(
        pool,
        sortcode,
        InqacccuRequest {
            customer_number: request.customer_number(),
        },
    )
    .await?;

    if !account_count_result.inquiry_success() {
        return Ok(customer_not_found(request.customer_number()));
    }

    if account_count_result.accounts().len() > MAX_ACCOUNTS_PER_CUSTOMER {
        return Ok(CreaccResult::failure(
            CAPACITY_FAIL_CODE,
            format!(
                "Customer number {} already has the maximum number of accounts.",
                request.customer_number()
            ),
        ));
    }

    let now = clock.now();
    let today = now.date_naive();
    let next_statement_date = today
        .checked_add_days(Days::new(30))
        .expect("today plus 30 days must be representable");
    let outcome = creacc::create_account(
        pool,
        CreateAccountCommand {
            sortcode: sortcode.to_string(),
            customer_number: request.customer_number(),
            account_type: request.account_type().to_string(),
            interest_rate: request.interest_rate(),
            overdraft_limit: request.overdraft_limit(),
            available_balance: request.available_balance(),
            actual_balance: request.actual_balance(),
            opened: today,
            last_statement_date: today,
            next_statement_date,
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: today,
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        CreateAccountOutcome::Success(account) => CreaccResult::success(account),
        CreateAccountOutcome::Failure { fail_code, message } => {
            CreaccResult::failure(fail_code, message)
        }
    })
}

// CREACC links to INQCUST and INQACCCU with the raw customer number, but
// those programs reserve `0` for random-customer mode and `9999999999` for
// last-customer mode (`INQCUST.cbl` lines 190-205; `INQACCCU.cbl` lines
// 836-842). Treat the sentinels as non-addressable parent customers here.
fn is_reserved_customer_number(customer_number: i64) -> bool {
    matches!(
        customer_number,
        RANDOM_CUSTOMER_NUMBER | LAST_CUSTOMER_NUMBER
    )
}

fn customer_not_found(customer_number: i64) -> CreaccResult {
    CreaccResult::failure(
        NOT_FOUND_CODE,
        format!("Customer number {customer_number} was not found."),
    )
}

fn validate_sortcode(sortcode: &str) -> Result<(), CbsaError> {
    if is_six_ascii_digits(sortcode) {
        Ok(())
    } else {
        Err(CbsaError::validation(
            "sortcode must be exactly 6 ASCII digits",
        ))
    }
}

fn is_supported_account_type(account_type: &str) -> bool {
    matches!(
        account_type,
        "ISA" | "MORTGAGE" | "SAVING" | "CURRENT" | "LOAN"
    )
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
    use super::*;

    fn valid_request(account_type: &str) -> Result<CreaccRequest, String> {
        CreaccRequest::new(
            42,
            account_type.to_string(),
            Decimal::new(125, 2),
            Decimal::new(250, 0),
            Decimal::new(150_025, 2),
            Decimal::new(149_975, 2),
        )
    }

    #[test]
    fn request_accepts_eight_non_ascii_account_type_characters() {
        let request = valid_request("éééééééé").expect("eight characters should be valid");
        assert_eq!(request.account_type(), "éééééééé");
    }

    #[test]
    fn request_rejects_blank_account_type() {
        assert_eq!(
            valid_request("   ").unwrap_err(),
            ACCOUNT_TYPE_BLANK_MESSAGE
        );
    }

    #[test]
    fn request_rejects_interest_rate_outside_copybook_range() {
        let err = CreaccRequest::new(
            42,
            "ISA".to_string(),
            Decimal::new(1_000_000, 2),
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
        )
        .unwrap_err();

        assert_eq!(err, INTEREST_RATE_RANGE_MESSAGE);
    }

    #[test]
    fn request_rejects_fractional_overdraft_limit() {
        let err = CreaccRequest::new(
            42,
            "ISA".to_string(),
            Decimal::new(125, 2),
            Decimal::new(2505, 1),
            Decimal::ZERO,
            Decimal::ZERO,
        )
        .unwrap_err();

        assert_eq!(err, OVERDRAFT_LIMIT_SCALE_MESSAGE);
    }
}
