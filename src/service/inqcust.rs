use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{PgConnection, PgPool};

use crate::{
    config::is_six_ascii_digits, domain::CustomerDetails, error::CbsaError,
    repository::inqcust as repository,
};

const ABEND_CODE: &str = "CVR1";
const NO_CUSTOMERS_EXIST_MESSAGE: &str = "No customers exist.";
const NOT_FOUND_CODE: &str = "1";
const RANDOM_RETRY_EXHAUSTED_CODE: &str = "R";
const RANDOM_RETRY_EXHAUSTED_MESSAGE: &str =
    "Unable to find a random customer after exhausting retry attempts.";
const RANDOM_CUSTOMER_NUMBER: i64 = 0;
const LAST_CUSTOMER_NUMBER: i64 = 9_999_999_999;
const RANDOM_RETRY_LIMIT: usize = 1000;
const CUSTOMER_NUMBER_RANGE_MESSAGE: &str = "customer_number must be between 0 and 9999999999";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InqcustRequest {
    pub customer_number: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InqcustResult {
    customer: Option<CustomerDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl InqcustResult {
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

    pub fn inquiry_success(&self) -> bool {
        self.customer.is_some()
    }

    pub fn fail_code(&self) -> &str {
        self.fail_code
    }

    pub fn customer(&self) -> Option<&CustomerDetails> {
        self.customer.as_ref()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_not_found_failure(&self) -> bool {
        !self.inquiry_success() && self.fail_code == NOT_FOUND_CODE
    }

    pub fn is_random_retry_exhausted_failure(&self) -> bool {
        !self.inquiry_success() && self.fail_code == RANDOM_RETRY_EXHAUSTED_CODE
    }
}

pub trait RandomCustomerNumberGenerator: Send {
    fn next_customer_number(&mut self, highest_customer_number: i64) -> i64;
}

#[derive(Debug)]
pub struct SystemRandomCustomerNumberGenerator {
    state: u64,
}

impl Default for SystemRandomCustomerNumberGenerator {
    fn default() -> Self {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        Self {
            state: duration.as_secs() ^ u64::from(duration.subsec_nanos()),
        }
    }
}

impl RandomCustomerNumberGenerator for SystemRandomCustomerNumberGenerator {
    fn next_customer_number(&mut self, highest_customer_number: i64) -> i64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);

        let highest = u64::try_from(highest_customer_number).unwrap_or(1);
        let candidate = (self.state % highest) + 1;
        i64::try_from(candidate).unwrap_or(1)
    }
}

pub async fn inquire(
    pool: &PgPool,
    sortcode: &str,
    request: InqcustRequest,
    generator: &mut dyn RandomCustomerNumberGenerator,
) -> Result<InqcustResult, CbsaError> {
    validate_sortcode(sortcode)?;
    validate_customer_number(request.customer_number)?;

    let mut conn = pool
        .acquire()
        .await
        .map_err(CbsaError::from)
        .map_err(read_failure)?;

    if request.customer_number == RANDOM_CUSTOMER_NUMBER {
        return find_random_customer(&mut conn, sortcode, generator).await;
    }

    if request.customer_number == LAST_CUSTOMER_NUMBER {
        return find_last_customer(&mut conn, sortcode).await;
    }

    let customer = load_customer(&mut conn, sortcode, request.customer_number).await?;
    Ok(match customer {
        Some(customer) => InqcustResult::success(customer),
        None => InqcustResult::failure(
            NOT_FOUND_CODE,
            format!("Customer number {} was not found.", request.customer_number),
        ),
    })
}

async fn find_last_customer(
    conn: &mut PgConnection,
    sortcode: &str,
) -> Result<InqcustResult, CbsaError> {
    Ok(match load_last_customer(conn, sortcode).await? {
        Some(customer) => InqcustResult::success(customer),
        None => InqcustResult::failure(NOT_FOUND_CODE, NO_CUSTOMERS_EXIST_MESSAGE),
    })
}

async fn find_random_customer(
    conn: &mut PgConnection,
    sortcode: &str,
    generator: &mut dyn RandomCustomerNumberGenerator,
) -> Result<InqcustResult, CbsaError> {
    let Some(last_customer) = load_last_customer(conn, sortcode).await? else {
        return Ok(InqcustResult::failure(
            NOT_FOUND_CODE,
            NO_CUSTOMERS_EXIST_MESSAGE,
        ));
    };

    let highest_customer_number = last_customer.customer_number();
    if highest_customer_number < 1 {
        return Ok(InqcustResult::failure(
            NOT_FOUND_CODE,
            NO_CUSTOMERS_EXIST_MESSAGE,
        ));
    }

    for _ in 0..RANDOM_RETRY_LIMIT {
        let candidate = generator.next_customer_number(highest_customer_number);

        if let Some(customer) = load_customer(conn, sortcode, candidate).await? {
            return Ok(InqcustResult::success(customer));
        }
    }

    Ok(InqcustResult::failure(
        RANDOM_RETRY_EXHAUSTED_CODE,
        RANDOM_RETRY_EXHAUSTED_MESSAGE,
    ))
}

async fn load_last_customer(
    conn: &mut PgConnection,
    sortcode: &str,
) -> Result<Option<CustomerDetails>, CbsaError> {
    repository::find_last_by_sortcode(&mut *conn, sortcode)
        .await
        .map_err(read_failure)
}

async fn load_customer(
    conn: &mut PgConnection,
    sortcode: &str,
    customer_number: i64,
) -> Result<Option<CustomerDetails>, CbsaError> {
    repository::find_by_sortcode_and_customer_number(&mut *conn, sortcode, customer_number)
        .await
        .map_err(read_failure)
}

fn read_failure(err: CbsaError) -> CbsaError {
    CbsaError::abend(
        ABEND_CODE,
        format!("INQCUST failed to read the customer data: {err}"),
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

fn validate_customer_number(customer_number: i64) -> Result<(), CbsaError> {
    if (0..=LAST_CUSTOMER_NUMBER).contains(&customer_number) {
        Ok(())
    } else {
        Err(CbsaError::validation(CUSTOMER_NUMBER_RANGE_MESSAGE))
    }
}
