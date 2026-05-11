use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    db,
    domain::AccountDetails,
    error::{CbsaError, RETRY_EXHAUSTED_ABEND_CODE},
    repository::inqacccu::{self, AccountRow},
};

const READ_FAILURE_ABEND_CODE: &str = "HACU";
const NOT_FOUND_CODE: &str = "1";
const RANDOM_CUSTOMER_NUMBER: i64 = 0;
const LAST_CUSTOMER_NUMBER: i64 = 9_999_999_999;
const CUSTOMER_NUMBER_RANGE_MESSAGE: &str = "customer_number must be between 0 and 9999999999";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InqacccuRequest {
    pub customer_number: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InqacccuResult {
    inquiry_success: bool,
    customer_number: i64,
    customer_found: bool,
    accounts: Vec<AccountDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl InqacccuResult {
    pub fn new(
        inquiry_success: bool,
        fail_code: &'static str,
        customer_number: i64,
        customer_found: bool,
        accounts: Vec<AccountDetails>,
        message: Option<String>,
    ) -> Result<Self, String> {
        if inquiry_success && fail_code != "0" {
            return Err("Successful results must use fail code 0".to_string());
        }

        if inquiry_success && !customer_found {
            return Err("Successful results must mark the customer as found".to_string());
        }

        if inquiry_success && message.is_some() {
            return Err("Successful results must not include a failure message".to_string());
        }

        if !inquiry_success && !accounts.is_empty() {
            return Err("Failure results must not include account data".to_string());
        }

        if !inquiry_success
            && message
                .as_deref()
                .is_none_or(|message| message.trim().is_empty())
        {
            return Err("Failure results must include a non-blank message".to_string());
        }

        Ok(Self {
            inquiry_success,
            customer_number,
            customer_found,
            accounts,
            fail_code,
            message,
        })
    }

    pub fn success(customer_number: i64, accounts: Vec<AccountDetails>) -> Self {
        Self::new(true, "0", customer_number, true, accounts, None)
            .expect("successful INQACCCU result invariants must hold")
    }

    pub fn failure(
        fail_code: &'static str,
        customer_number: i64,
        customer_found: bool,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            false,
            fail_code,
            customer_number,
            customer_found,
            Vec::new(),
            Some(message.into()),
        )
        .expect("failed INQACCCU result invariants must hold")
    }

    pub fn inquiry_success(&self) -> bool {
        self.inquiry_success
    }

    pub fn customer_number(&self) -> i64 {
        self.customer_number
    }

    pub fn customer_found(&self) -> bool {
        self.customer_found
    }

    pub fn accounts(&self) -> &[AccountDetails] {
        &self.accounts
    }

    pub fn fail_code(&self) -> &'static str {
        self.fail_code
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_not_found_failure(&self) -> bool {
        !self.inquiry_success && self.fail_code == NOT_FOUND_CODE
    }
}

#[derive(Debug)]
struct LoadedCustomerAccounts {
    customer_exists: bool,
    account_rows: Vec<AccountRow>,
}

impl LoadedCustomerAccounts {
    fn customer_not_found() -> Self {
        Self {
            customer_exists: false,
            account_rows: Vec::new(),
        }
    }

    fn customer_found(account_rows: Vec<AccountRow>) -> Self {
        Self {
            customer_exists: true,
            account_rows,
        }
    }
}

pub async fn inquire(
    pool: &PgPool,
    sortcode: &str,
    request: InqacccuRequest,
) -> Result<InqacccuResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_customer_number(request.customer_number)?;

    if is_reserved_customer_number(request.customer_number) {
        return Ok(customer_not_found(request.customer_number));
    }

    let customer_number = request.customer_number;
    let sortcode = sortcode.to_string();
    let loaded = db::with_retry(pool, move |pool| {
        let sortcode = sortcode.clone();
        async move {
            let mut tx = pool.begin().await?;

            if !inqacccu::customer_exists(&mut *tx, &sortcode, customer_number).await? {
                tx.commit().await?;
                return Ok(LoadedCustomerAccounts::customer_not_found());
            }

            let rows = inqacccu::find_by_sortcode_and_customer_number(
                &mut *tx,
                &sortcode,
                customer_number,
            )
            .await?;

            tx.commit().await?;
            Ok(LoadedCustomerAccounts::customer_found(rows))
        }
    })
    .await
    .map_err(read_failure)?;

    if !loaded.customer_exists {
        return Ok(customer_not_found(customer_number));
    }

    let accounts = loaded
        .account_rows
        .into_iter()
        .map(map_account_row)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(InqacccuResult::success(customer_number, accounts))
}

/// `INQACCCU` treats `0` and `9999999999` as reserved sentinels in
/// `CUSTOMER-CHECK`, returning `CUSTOMER-FOUND = 'N'` before linking to
/// `INQCUST`. See `INQACCCU.cbl` lines 835-844.
fn is_reserved_customer_number(customer_number: i64) -> bool {
    matches!(
        customer_number,
        RANDOM_CUSTOMER_NUMBER | LAST_CUSTOMER_NUMBER
    )
}

fn customer_not_found(customer_number: i64) -> InqacccuResult {
    InqacccuResult::failure(
        NOT_FOUND_CODE,
        customer_number,
        false,
        format!("Customer number {customer_number} was not found."),
    )
}

fn map_account_row(row: AccountRow) -> Result<AccountDetails, CbsaError> {
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
            READ_FAILURE_ABEND_CODE,
            format!("INQACCCU loaded invalid account data: {message}"),
        )
    })
}

fn read_failure(err: sqlx::Error) -> CbsaError {
    if db::is_serialization_failure(&err) {
        CbsaError::abend(
            RETRY_EXHAUSTED_ABEND_CODE,
            format!(
                "INQACCCU exhausted serialization retries while reading customer accounts: {err}"
            ),
        )
    } else {
        CbsaError::abend(
            READ_FAILURE_ABEND_CODE,
            format!("INQACCCU failed to read the customer accounts: {err}"),
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

fn validate_customer_number(customer_number: i64) -> Result<(), CbsaError> {
    if (0..=LAST_CUSTOMER_NUMBER).contains(&customer_number) {
        Ok(())
    } else {
        Err(CbsaError::validation(CUSTOMER_NUMBER_RANGE_MESSAGE))
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, fmt};

    use chrono::NaiveDate;
    use rust_decimal::Decimal;
    use sqlx::error::{DatabaseError, ErrorKind};

    use super::*;

    #[test]
    fn successful_results_allow_empty_account_lists() {
        let result = InqacccuResult::success(42, Vec::new());

        assert!(result.inquiry_success());
        assert!(result.customer_found());
        assert!(result.accounts().is_empty());
        assert_eq!(result.fail_code(), "0");
    }

    #[test]
    fn result_invariants_reject_success_without_customer_found() {
        let err = InqacccuResult::new(true, "0", 42, false, Vec::new(), None).unwrap_err();

        assert_eq!(err, "Successful results must mark the customer as found");
    }

    #[test]
    fn result_invariants_reject_failure_with_account_data() {
        let err = InqacccuResult::new(
            false,
            "1",
            42,
            false,
            vec![sample_account()],
            Some("boom".to_string()),
        )
        .unwrap_err();

        assert_eq!(err, "Failure results must not include account data");
    }

    #[test]
    fn result_invariants_reject_whitespace_only_failure_messages() {
        let err = InqacccuResult::new(false, "1", 42, false, Vec::new(), Some("   ".to_string()))
            .unwrap_err();

        assert_eq!(err, "Failure results must include a non-blank message");
    }

    #[test]
    fn reserved_customer_numbers_are_copybook_sentinels() {
        assert!(is_reserved_customer_number(RANDOM_CUSTOMER_NUMBER));
        assert!(is_reserved_customer_number(LAST_CUSTOMER_NUMBER));
        assert!(!is_reserved_customer_number(1));
    }

    #[test]
    fn read_failure_uses_retry_exhausted_abend_for_serialization_failures() {
        let error = sqlx::Error::Database(Box::new(FakeDbError::new("40001", "retry me")));

        let mapped = read_failure(error);

        assert!(matches!(
            mapped,
            CbsaError::Abend(RETRY_EXHAUSTED_ABEND_CODE, _)
        ));
    }

    #[test]
    fn read_failure_preserves_program_abend_for_non_retryable_errors() {
        let error = sqlx::Error::Database(Box::new(FakeDbError::new("23505", "boom")));

        let mapped = read_failure(error);

        assert!(matches!(
            mapped,
            CbsaError::Abend(READ_FAILURE_ABEND_CODE, _)
        ));
    }

    fn sample_account() -> AccountDetails {
        AccountDetails::new(
            "012345".to_string(),
            42,
            12_345_678,
            "ISA".to_string(),
            Decimal::new(150, 2),
            NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            Decimal::new(25000, 2),
            None,
            None,
            Decimal::new(150025, 2),
            Decimal::new(149975, 2),
        )
        .expect("sample account must be valid")
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
