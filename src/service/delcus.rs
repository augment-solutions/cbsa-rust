use chrono::{DateTime, NaiveTime, Timelike, Utc};
use sqlx::PgPool;

use crate::{
    config::is_six_ascii_digits,
    domain::CustomerDetails,
    error::CbsaError,
    repository::delcus::{self, DeleteCustomerCommand, DeleteCustomerOutcome},
};

const CUSTOMER_NUMBER_RANGE_MESSAGE: &str = "customer_number must be between 1 and 9999999999";
const SORTCODE_MESSAGE: &str = "sortcode must be exactly 6 ASCII digits";
const MAX_CUSTOMER_NUMBER: i64 = 9_999_999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelcusRequest {
    pub customer_number: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelcusResult {
    customer: Option<CustomerDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl DelcusResult {
    pub fn success(customer: CustomerDetails) -> Self {
        Self {
            customer: Some(customer),
            fail_code: " ",
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

    pub fn delete_success(&self) -> bool {
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
        !self.delete_success() && self.fail_code == "1"
    }
}

pub async fn delete(
    pool: &PgPool,
    sortcode: &str,
    request: DelcusRequest,
) -> Result<DelcusResult, CbsaError> {
    let clock = SystemClock;
    delete_with_dependencies(pool, sortcode, request, &clock).await
}

async fn delete_with_dependencies(
    pool: &PgPool,
    sortcode: &str,
    request: DelcusRequest,
    clock: &dyn Clock,
) -> Result<DelcusResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_request(request)?;

    let now = clock.now();
    let outcome = delcus::delete_customer(
        pool,
        DeleteCustomerCommand {
            sortcode: sortcode.to_string(),
            customer_number: request.customer_number,
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: now.date_naive(),
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        DeleteCustomerOutcome::Success(customer) => DelcusResult::success(customer),
        DeleteCustomerOutcome::Failure { fail_code, message } => {
            DelcusResult::failure(fail_code, message)
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

fn validate_request(request: DelcusRequest) -> Result<(), CbsaError> {
    if (1..=MAX_CUSTOMER_NUMBER).contains(&request.customer_number) {
        Ok(())
    } else {
        Err(CbsaError::validation(CUSTOMER_NUMBER_RANGE_MESSAGE))
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
    use super::{validate_request, DelcusRequest};

    #[test]
    fn rejects_random_customer_number_sentinel() {
        let err = validate_request(DelcusRequest { customer_number: 0 }).unwrap_err();

        assert_eq!(
            err.to_string(),
            "validation: customer_number must be between 1 and 9999999999"
        );
    }
}
