use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    db,
    domain::AccountDetails,
    error::{CbsaError, RETRY_EXHAUSTED_ABEND_CODE},
    repository::inqacc::{self, AccountRow},
};

const EXACT_LOOKUP_ABEND_CODE: &str = "HRAC";
const LAST_LOOKUP_ABEND_CODE: &str = "HNCS";
const NO_ACCOUNTS_EXIST_MESSAGE: &str = "No accounts exist.";
const NOT_FOUND_CODE: &str = "1";
const LAST_ACCOUNT_NUMBER: i64 = 99_999_999;
const ACCOUNT_NUMBER_RANGE_MESSAGE: &str = "account_number must be between 0 and 99999999";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InqaccRequest {
    pub account_number: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InqaccResult {
    account: Option<AccountDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl InqaccResult {
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

    pub fn inquiry_success(&self) -> bool {
        self.account.is_some()
    }

    pub fn fail_code(&self) -> &str {
        self.fail_code
    }

    pub fn account(&self) -> Option<&AccountDetails> {
        self.account.as_ref()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_not_found_failure(&self) -> bool {
        !self.inquiry_success() && self.fail_code == NOT_FOUND_CODE
    }
}

pub async fn inquire(
    pool: &PgPool,
    sortcode: &str,
    request: InqaccRequest,
) -> Result<InqaccResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_account_number(request.account_number)?;

    if request.account_number == LAST_ACCOUNT_NUMBER {
        return find_last_account(pool, sortcode).await;
    }

    let account = load_account(pool, sortcode, request.account_number).await?;
    Ok(match account {
        Some(account) => InqaccResult::success(account),
        None => InqaccResult::failure(
            NOT_FOUND_CODE,
            format!("Account number {} was not found.", request.account_number),
        ),
    })
}

async fn find_last_account(pool: &PgPool, sortcode: &str) -> Result<InqaccResult, CbsaError> {
    Ok(match load_last_account(pool, sortcode).await? {
        Some(account) => InqaccResult::success(account),
        None => InqaccResult::failure(NOT_FOUND_CODE, NO_ACCOUNTS_EXIST_MESSAGE),
    })
}

async fn load_account(
    pool: &PgPool,
    sortcode: &str,
    account_number: i64,
) -> Result<Option<AccountDetails>, CbsaError> {
    let sortcode = sortcode.to_string();
    let row = db::with_retry(pool, move |pool| {
        let sortcode = sortcode.clone();
        async move {
            inqacc::find_by_sortcode_and_account_number(&pool, &sortcode, account_number).await
        }
    })
    .await
    .map_err(|err| read_failure(err, EXACT_LOOKUP_ABEND_CODE))?;

    row.map(|row| map_account_row(row, EXACT_LOOKUP_ABEND_CODE))
        .transpose()
}

async fn load_last_account(
    pool: &PgPool,
    sortcode: &str,
) -> Result<Option<AccountDetails>, CbsaError> {
    let sortcode = sortcode.to_string();
    let row = db::with_retry(pool, move |pool| {
        let sortcode = sortcode.clone();
        async move { inqacc::find_last_by_sortcode(&pool, &sortcode).await }
    })
    .await
    .map_err(|err| read_failure(err, LAST_LOOKUP_ABEND_CODE))?;

    row.map(|row| map_account_row(row, LAST_LOOKUP_ABEND_CODE))
        .transpose()
}

fn map_account_row(row: AccountRow, abend_code: &'static str) -> Result<AccountDetails, CbsaError> {
    AccountDetails::new(
        row.sortcode,
        row.customer_number,
        row.account_number,
        row.account_type,
        row.interest_rate,
        row.opened,
        row.overdraft_limit,
        row.last_stmt_date,
        row.next_stmt_date,
        row.available_balance,
        row.actual_balance,
    )
    .map_err(|message| {
        CbsaError::abend(
            abend_code,
            format!("INQACC loaded invalid account data: {message}"),
        )
    })
}

fn read_failure(err: sqlx::Error, abend_code: &'static str) -> CbsaError {
    if db::is_serialization_failure(&err) {
        CbsaError::abend(
            RETRY_EXHAUSTED_ABEND_CODE,
            format!("INQACC exhausted serialization retries while reading account data: {err}"),
        )
    } else {
        CbsaError::abend(
            abend_code,
            format!("INQACC failed to read the account data: {err}"),
        )
    }
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

fn validate_account_number(account_number: i64) -> Result<(), CbsaError> {
    if (0..=LAST_ACCOUNT_NUMBER).contains(&account_number) {
        Ok(())
    } else {
        Err(CbsaError::validation(ACCOUNT_NUMBER_RANGE_MESSAGE))
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, fmt};

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    #[test]
    fn read_failure_uses_retry_exhausted_abend_for_serialization_failures() {
        let error = sqlx::Error::Database(Box::new(FakeDbError::new("40001", "retry me")));

        let mapped = read_failure(error, EXACT_LOOKUP_ABEND_CODE);

        assert!(matches!(
            mapped,
            CbsaError::Abend(RETRY_EXHAUSTED_ABEND_CODE, _)
        ));
    }

    #[test]
    fn read_failure_preserves_program_abend_for_non_retryable_errors() {
        let error = sqlx::Error::Database(Box::new(FakeDbError::new("23505", "boom")));

        let mapped = read_failure(error, LAST_LOOKUP_ABEND_CODE);

        assert!(matches!(
            mapped,
            CbsaError::Abend(LAST_LOOKUP_ABEND_CODE, _)
        ));
    }

    #[derive(Debug)]
    struct FakeDbError {
        code: &'static str,
        message: &'static str,
    }

    impl FakeDbError {
        fn new(code: &'static str, message: &'static str) -> Self {
            Self { code, message }
        }
    }

    impl fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{} ({})", self.message, self.code)
        }
    }

    impl std::error::Error for FakeDbError {}

    impl DatabaseError for FakeDbError {
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn message(&self) -> &str {
            self.message
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }
    }
}
