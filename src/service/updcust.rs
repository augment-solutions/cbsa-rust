use chrono::{DateTime, NaiveTime, Timelike, Utc};
use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    domain::CustomerDetails,
    error::CbsaError,
    repository::updcust::{self, UpdateCustomerCommand, UpdateCustomerOutcome},
};

const INVALID_TITLE_CODE: &str = "T";
const INVALID_TITLE_MESSAGE: &str = "The customer title is invalid.";
const CUSTOMER_NUMBER_RANGE_MESSAGE: &str = "customer_number must be between 0 and 9999999999";
const CUSTOMER_SORTCODE_MESSAGE: &str = "sortcode must be exactly 6 ASCII digits";
const MAX_CUSTOMER_NUMBER: i64 = 9_999_999_999;
const MAX_NAME_CHARS: usize = 60;
const MAX_ADDRESS_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdcustRequest {
    pub customer_number: i64,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdcustResult {
    customer: Option<CustomerDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl UpdcustResult {
    pub fn success(customer: CustomerDetails) -> Self {
        Self {
            customer: Some(customer),
            fail_code: "0",
            message: None,
        }
    }

    pub fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            customer: None,
            fail_code,
            message: Some(message.into()),
        }
    }

    pub fn update_success(&self) -> bool {
        self.customer.is_some()
    }

    pub fn customer(&self) -> Option<&CustomerDetails> {
        self.customer.as_ref()
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
        !self.update_success() && matches!(self.fail_code, "4" | INVALID_TITLE_CODE)
    }
}

pub async fn update(
    pool: &PgPool,
    sortcode: &str,
    request: UpdcustRequest,
) -> Result<UpdcustResult, CbsaError> {
    let clock = SystemClock;
    update_with_dependencies(pool, sortcode, request, &clock).await
}

async fn update_with_dependencies(
    pool: &PgPool,
    sortcode: &str,
    request: UpdcustRequest,
    clock: &dyn Clock,
) -> Result<UpdcustResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_request(&request)?;

    if !valid_title(&request.name) {
        return Ok(UpdcustResult::failure(
            INVALID_TITLE_CODE,
            INVALID_TITLE_MESSAGE,
        ));
    }

    let now = clock.now();
    let outcome = updcust::update_customer(
        pool,
        UpdateCustomerCommand {
            sortcode: sortcode.to_string(),
            customer_number: request.customer_number,
            name: request.name,
            address: request.address,
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: now.date_naive(),
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        UpdateCustomerOutcome::Success(customer) => UpdcustResult::success(customer),
        UpdateCustomerOutcome::Failure { fail_code, message } => {
            UpdcustResult::failure(fail_code, message)
        }
    })
}

fn validate_sortcode(sortcode: &str) -> Result<(), CbsaError> {
    if is_six_ascii_digits(sortcode) {
        Ok(())
    } else {
        Err(CbsaError::validation(CUSTOMER_SORTCODE_MESSAGE))
    }
}

fn validate_request(request: &UpdcustRequest) -> Result<(), CbsaError> {
    if !(0..=MAX_CUSTOMER_NUMBER).contains(&request.customer_number) {
        return Err(CbsaError::validation(CUSTOMER_NUMBER_RANGE_MESSAGE));
    }

    if request.name.chars().count() > MAX_NAME_CHARS {
        return Err(CbsaError::validation(format!(
            "name must be at most {MAX_NAME_CHARS} characters"
        )));
    }

    if request.address.chars().count() > MAX_ADDRESS_CHARS {
        return Err(CbsaError::validation(format!(
            "address must be at most {MAX_ADDRESS_CHARS} characters"
        )));
    }

    Ok(())
}

fn valid_title(name: &str) -> bool {
    matches!(
        first_token(name),
        "Professor" | "Mr" | "Mrs" | "Miss" | "Ms" | "Dr" | "Drs" | "Lord" | "Sir" | "Lady" | ""
    )
}

fn first_token(name: &str) -> &str {
    let delimiter_index = name.find(' ').unwrap_or(name.len());
    &name[..delimiter_index]
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
    use super::{first_token, valid_title};

    #[test]
    fn accepts_blank_title_when_name_starts_with_space() {
        assert_eq!(first_token(" Dr Ada Example"), "");
        assert!(valid_title(" Dr Ada Example"));
    }

    #[test]
    fn rejects_unknown_titles() {
        assert!(!valid_title("Mx Ada Example"));
    }
}
