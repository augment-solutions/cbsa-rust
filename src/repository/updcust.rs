use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{CustomerDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const NOT_FOUND_CODE: &str = "1";
const READ_FAIL_CODE: &str = "2";
const UPDATE_FAIL_CODE: &str = "3";
const BLANK_UPDATE_FAIL_CODE: &str = "4";
const RETRY_EXHAUSTED_MESSAGE: &str =
    "UPDCUST aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCustomerCommand {
    pub sortcode: String,
    pub customer_number: i64,
    pub name: String,
    pub address: String,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCustomerOutcome {
    Success(CustomerDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

pub async fn update_customer(
    pool: &PgPool,
    command: UpdateCustomerCommand,
) -> Result<UpdateCustomerOutcome, CbsaError> {
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        async move {
            let mut tx = pool.begin().await?;
            let outcome = update_customer_once(&mut tx, &command).await?;
            if matches!(outcome, UpdateCustomerOnceOutcome::Success(_)) {
                tx.commit().await?;
            }
            Ok(outcome)
        }
    })
    .await
    .map_err(|err| {
        if db::is_serialization_failure(&err) {
            CbsaError::abend(RETRY_EXHAUSTED_ABEND_CODE, RETRY_EXHAUSTED_MESSAGE)
        } else {
            CbsaError::from(err)
        }
    })?;

    match outcome {
        UpdateCustomerOnceOutcome::Success(customer) => {
            Ok(UpdateCustomerOutcome::Success(customer))
        }
        UpdateCustomerOnceOutcome::Failure { fail_code, message } => {
            Ok(UpdateCustomerOutcome::Failure { fail_code, message })
        }
        UpdateCustomerOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn update_customer_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &UpdateCustomerCommand,
) -> Result<UpdateCustomerOnceOutcome, sqlx::Error> {
    let existing_customer = match load_customer_for_update(
        &mut *tx,
        &command.sortcode,
        command.customer_number,
    )
    .await
    {
        Ok(Some(customer)) => customer,
        Ok(None) => {
            return Ok(UpdateCustomerOnceOutcome::failure(
                NOT_FOUND_CODE,
                format!("Customer number {} was not found.", command.customer_number),
            ))
        }
        Err(CbsaError::Database(err)) if db::is_serialization_failure(&err) => return Err(err),
        Err(_) => {
            return Ok(UpdateCustomerOnceOutcome::failure(
                READ_FAIL_CODE,
                "Unable to read the customer record.",
            ))
        }
    };

    if is_blankish(&command.name) && is_blankish(&command.address) {
        return Ok(UpdateCustomerOnceOutcome::failure(
            BLANK_UPDATE_FAIL_CODE,
            "Customer name and address must not both be blank.",
        ));
    }

    let (updated_name, updated_address) = resolved_fields(&existing_customer, command);

    match update_customer_row(
        &mut *tx,
        &command.sortcode,
        command.customer_number,
        &updated_name,
        &updated_address,
    )
    .await
    {
        Ok(1) => {}
        Ok(_) => {
            return Ok(UpdateCustomerOnceOutcome::failure(
                UPDATE_FAIL_CODE,
                "Unable to update the customer record.",
            ))
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(_) => {
            return Ok(UpdateCustomerOnceOutcome::failure(
                UPDATE_FAIL_CODE,
                "Unable to update the customer record.",
            ))
        }
    }

    let customer = match CustomerDetails::new(
        command.sortcode.clone(),
        command.customer_number,
        updated_name,
        updated_address,
        existing_customer.date_of_birth(),
        existing_customer.credit_score(),
        existing_customer.credit_score_review_date(),
    ) {
        Ok(customer) => customer,
        Err(_) => {
            return Ok(UpdateCustomerOnceOutcome::failure(
                UPDATE_FAIL_CODE,
                "Unable to update the customer record.",
            ))
        }
    };

    match insert_proctran(&mut *tx, &customer, command).await {
        Ok(()) => Ok(UpdateCustomerOnceOutcome::Success(customer)),
        Err(err) if db::is_serialization_failure(&err) => Err(err),
        Err(err) => Ok(UpdateCustomerOnceOutcome::Abend {
            code: PROCTRAN_ABEND_CODE,
            message: format!("UPDCUST failed to write the audit trail: {err}"),
        }),
    }
}

fn resolved_fields(
    existing_customer: &CustomerDetails,
    command: &UpdateCustomerCommand,
) -> (String, String) {
    let mut updated_name = existing_customer.name().to_string();
    let mut updated_address = existing_customer.address().to_string();

    if is_blankish(&command.name) && is_provided_for_single_field_update(&command.address) {
        updated_address = command.address.clone();
    }

    if is_blankish(&command.address) && is_provided_for_single_field_update(&command.name) {
        updated_name = command.name.clone();
    }

    if starts_with_non_space(&command.name) && starts_with_non_space(&command.address) {
        updated_name = command.name.clone();
        updated_address = command.address.clone();
    }

    (updated_name, updated_address)
}

#[derive(Debug, FromRow)]
struct CustomerRow {
    sortcode: String,
    customer_number: i64,
    name: String,
    address: String,
    date_of_birth: NaiveDate,
    credit_score: i16,
    cs_review_date: Option<NaiveDate>,
}

impl TryFrom<CustomerRow> for CustomerDetails {
    type Error = CbsaError;

    fn try_from(row: CustomerRow) -> Result<Self, Self::Error> {
        let credit_score = u16::try_from(row.credit_score)
            .map_err(|_| CbsaError::validation("credit_score must be between 0 and 999"))?;

        CustomerDetails::new(
            row.sortcode,
            row.customer_number,
            row.name.trim_end().to_string(),
            row.address.trim_end().to_string(),
            row.date_of_birth,
            credit_score,
            row.cs_review_date,
        )
        .map_err(CbsaError::validation)
    }
}

async fn load_customer_for_update(
    conn: &mut PgConnection,
    sortcode: &str,
    customer_number: i64,
) -> Result<Option<CustomerDetails>, CbsaError> {
    let row = sqlx::query_as::<_, CustomerRow>(
        r#"
        SELECT sortcode, customer_number, name, address, date_of_birth, credit_score, cs_review_date
        FROM customer
        WHERE sortcode = $1 AND customer_number = $2
        FOR UPDATE
        "#,
    )
    .bind(sortcode)
    .bind(customer_number)
    .fetch_optional(conn)
    .await?;

    row.map(TryInto::try_into).transpose()
}

async fn update_customer_row(
    conn: &mut PgConnection,
    sortcode: &str,
    customer_number: i64,
    name: &str,
    address: &str,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE customer
        SET name = $1,
            address = $2
        WHERE sortcode = $3 AND customer_number = $4
        "#,
    )
    .bind(name)
    .bind(address)
    .bind(sortcode)
    .bind(customer_number)
    .execute(conn)
    .await
    .map(|result| result.rows_affected())
}

async fn insert_proctran(
    conn: &mut PgConnection,
    customer: &CustomerDetails,
    command: &UpdateCustomerCommand,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO proctran (sortcode, logical_delete, tran_date, tran_time, tran_ref, tran_type, description, amount)
        VALUES ($1, FALSE, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(customer.sortcode())
    .bind(command.transaction_date)
    .bind(command.transaction_time)
    .bind(command.transaction_reference)
    .bind(ProcTranType::CustomerUpdate.as_str())
    .bind(proctran_description(customer))
    .bind(Decimal::ZERO)
    .execute(conn)
    .await
    .map(|_| ())
}

fn proctran_description(customer: &CustomerDetails) -> String {
    format!(
        "{}{:010}{}{}",
        customer.sortcode(),
        customer.customer_number(),
        pad_or_truncate(customer.name(), 14),
        customer.date_of_birth().format("%d/%m/%Y"),
    )
}

fn pad_or_truncate(value: &str, width: usize) -> String {
    let truncated: String = value.chars().take(width).collect();
    let padding = width.saturating_sub(truncated.chars().count());
    format!("{truncated}{}", " ".repeat(padding))
}

fn is_blankish(value: &str) -> bool {
    value.is_empty() || value.chars().all(|ch| ch == ' ') || value.starts_with(' ')
}

fn is_provided_for_single_field_update(value: &str) -> bool {
    !is_blankish(value)
}

fn starts_with_non_space(value: &str) -> bool {
    !value.is_empty() && !value.starts_with(' ')
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateCustomerOnceOutcome {
    Success(CustomerDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
    Abend {
        code: &'static str,
        message: String,
    },
}

impl UpdateCustomerOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }
}
