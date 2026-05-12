use chrono::{DateTime, NaiveTime, Timelike, Utc};
use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    domain::AccountDetails,
    error::CbsaError,
    repository::delacc::{self, DeleteAccountCommand, DeleteAccountOutcome},
};

const ACCOUNT_NUMBER_RANGE_MESSAGE: &str = "account_number must be between 0 and 99999999";
const SORTCODE_MESSAGE: &str = "sortcode must be exactly 6 ASCII digits";
const MAX_ACCOUNT_NUMBER: i64 = 99_999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelaccRequest {
    pub account_number: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelaccResult {
    account: Option<AccountDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl DelaccResult {
    pub fn success(account: AccountDetails) -> Self {
        Self {
            account: Some(account),
            fail_code: " ",
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

    pub fn delete_success(&self) -> bool {
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
        !self.delete_success() && self.fail_code == "1"
    }
}

pub async fn delete(
    pool: &PgPool,
    sortcode: &str,
    request: DelaccRequest,
) -> Result<DelaccResult, CbsaError> {
    let clock = SystemClock;
    delete_with_dependencies(pool, sortcode, request, &clock).await
}

async fn delete_with_dependencies(
    pool: &PgPool,
    sortcode: &str,
    request: DelaccRequest,
    clock: &dyn Clock,
) -> Result<DelaccResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_request(request)?;

    let now = clock.now();
    let outcome = delacc::delete_account(
        pool,
        DeleteAccountCommand {
            sortcode: sortcode.to_string(),
            account_number: request.account_number,
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: now.date_naive(),
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        DeleteAccountOutcome::Success(account) => DelaccResult::success(account),
        DeleteAccountOutcome::Failure { fail_code, message } => {
            DelaccResult::failure(fail_code, message)
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

fn validate_request(request: DelaccRequest) -> Result<(), CbsaError> {
    if (0..=MAX_ACCOUNT_NUMBER).contains(&request.account_number) {
        Ok(())
    } else {
        Err(CbsaError::validation(ACCOUNT_NUMBER_RANGE_MESSAGE))
    }
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
    use super::{validate_request, DelaccRequest};

    #[test]
    fn rejects_account_numbers_above_copybook_width() {
        let err = validate_request(DelaccRequest {
            account_number: 100_000_000,
        })
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "validation: account_number must be between 0 and 99999999"
        );
    }
}
