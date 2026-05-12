use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{AccountDetails, CustomerDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const NOT_FOUND_CODE: &str = "1";
const READ_ABEND_CODE: &str = "WPV6";
const DELETE_ABEND_CODE: &str = "WPV7";
const RETRY_EXHAUSTED_MESSAGE: &str =
    "DELCUS aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteCustomerCommand {
    pub sortcode: String,
    pub customer_number: i64,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteCustomerOutcome {
    Success(CustomerDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

pub async fn delete_customer(
    pool: &PgPool,
    command: DeleteCustomerCommand,
) -> Result<DeleteCustomerOutcome, CbsaError> {
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        async move {
            let mut tx = pool.begin().await?;
            let outcome = delete_customer_once(&mut tx, &command).await?;
            if matches!(outcome, DeleteCustomerOnceOutcome::Success(_)) {
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
        DeleteCustomerOnceOutcome::Success(customer) => {
            Ok(DeleteCustomerOutcome::Success(customer))
        }
        DeleteCustomerOnceOutcome::Failure { fail_code, message } => {
            Ok(DeleteCustomerOutcome::Failure { fail_code, message })
        }
        DeleteCustomerOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn delete_customer_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &DeleteCustomerCommand,
) -> Result<DeleteCustomerOnceOutcome, sqlx::Error> {
    let customer = match load_customer_for_update(
        &mut *tx,
        &command.sortcode,
        command.customer_number,
    )
    .await
    {
        Ok(Some(customer)) => customer,
        Ok(None) => {
            return Ok(DeleteCustomerOnceOutcome::failure(
                NOT_FOUND_CODE,
                format!("Customer number {} was not found.", command.customer_number),
            ))
        }
        Err(CbsaError::Database(err)) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(DeleteCustomerOnceOutcome::abend(
                READ_ABEND_CODE,
                format!("DELCUS failed to read the customer data: {err}"),
            ))
        }
    };

    let accounts = match load_accounts_for_customer(
        &mut *tx,
        &command.sortcode,
        command.customer_number,
    )
    .await
    {
        Ok(accounts) => accounts,
        Err(CbsaError::Database(err)) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(DeleteCustomerOnceOutcome::abend(
                READ_ABEND_CODE,
                format!("DELCUS failed to read the account data: {err}"),
            ))
        }
    };

    for account in &accounts {
        match delete_account_row(&mut *tx, &command.sortcode, account.account_number()).await {
            Ok(0) => continue,
            Ok(1) => {}
            Ok(rows) => {
                return Ok(DeleteCustomerOnceOutcome::abend(
                    DELETE_ABEND_CODE,
                    format!(
                        "DELCUS deleted an unexpected number of rows for account {}: {rows}.",
                        account.account_number()
                    ),
                ))
            }
            Err(err) if db::is_serialization_failure(&err) => return Err(err),
            Err(err) => {
                return Ok(DeleteCustomerOnceOutcome::abend(
                    DELETE_ABEND_CODE,
                    format!(
                        "DELCUS failed to delete account {}: {err}",
                        account.account_number()
                    ),
                ))
            }
        }

        match insert_account_proctran(&mut *tx, account, command).await {
            Ok(()) => {}
            Err(err) if db::is_serialization_failure(&err) => return Err(err),
            Err(err) => {
                return Ok(DeleteCustomerOnceOutcome::abend(
                    PROCTRAN_ABEND_CODE,
                    format!("DELCUS failed to write the account deletion audit trail: {err}"),
                ))
            }
        }
    }

    match delete_customer_row(&mut *tx, &command.sortcode, command.customer_number).await {
        Ok(1) => {}
        Ok(0) => {
            return Ok(DeleteCustomerOnceOutcome::abend(
                DELETE_ABEND_CODE,
                format!(
                    "DELCUS could not delete customer {} (concurrently removed).",
                    command.customer_number
                ),
            ))
        }
        Ok(rows) => {
            return Ok(DeleteCustomerOnceOutcome::abend(
                DELETE_ABEND_CODE,
                format!(
                    "DELCUS deleted an unexpected number of rows for customer {}: {rows}.",
                    command.customer_number
                ),
            ))
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(DeleteCustomerOnceOutcome::abend(
                DELETE_ABEND_CODE,
                format!(
                    "DELCUS failed to delete customer {}: {err}",
                    command.customer_number
                ),
            ))
        }
    }

    match insert_customer_proctran(&mut *tx, &customer, command).await {
        Ok(()) => Ok(DeleteCustomerOnceOutcome::Success(customer)),
        Err(err) if db::is_serialization_failure(&err) => Err(err),
        Err(err) => Ok(DeleteCustomerOnceOutcome::abend(
            PROCTRAN_ABEND_CODE,
            format!("DELCUS failed to write the customer deletion audit trail: {err}"),
        )),
    }
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

#[derive(Debug, FromRow)]
struct AccountRow {
    sortcode: String,
    customer_number: i64,
    account_number: i64,
    account_type: String,
    interest_rate: Decimal,
    opened: NaiveDate,
    overdraft_limit: Decimal,
    last_stmt_date: Option<NaiveDate>,
    next_stmt_date: Option<NaiveDate>,
    available_balance: Decimal,
    actual_balance: Decimal,
}

impl TryFrom<AccountRow> for AccountDetails {
    type Error = CbsaError;

    fn try_from(row: AccountRow) -> Result<Self, Self::Error> {
        AccountDetails::new(
            row.sortcode,
            row.customer_number,
            row.account_number,
            row.account_type.trim_end().to_string(),
            row.interest_rate,
            row.opened,
            row.overdraft_limit,
            row.last_stmt_date,
            row.next_stmt_date,
            row.available_balance,
            row.actual_balance,
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

async fn load_accounts_for_customer(
    conn: &mut PgConnection,
    sortcode: &str,
    customer_number: i64,
) -> Result<Vec<AccountDetails>, CbsaError> {
    let rows = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT sortcode, customer_number, account_number, account_type, interest_rate, opened,
               overdraft_limit, last_stmt_date, next_stmt_date, available_balance, actual_balance
        FROM account
        WHERE sortcode = $1 AND customer_number = $2
        ORDER BY account_number ASC
        LIMIT 20
        "#,
    )
    .bind(sortcode)
    .bind(customer_number)
    .fetch_all(conn)
    .await?;

    rows.into_iter().map(TryInto::try_into).collect()
}

async fn delete_account_row(
    conn: &mut PgConnection,
    sortcode: &str,
    account_number: i64,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        r#"
        DELETE FROM account
        WHERE sortcode = $1 AND account_number = $2
        "#,
    )
    .bind(sortcode)
    .bind(account_number)
    .execute(conn)
    .await
    .map(|result| result.rows_affected())
}

async fn delete_customer_row(
    conn: &mut PgConnection,
    sortcode: &str,
    customer_number: i64,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        r#"
        DELETE FROM customer
        WHERE sortcode = $1 AND customer_number = $2
        "#,
    )
    .bind(sortcode)
    .bind(customer_number)
    .execute(conn)
    .await
    .map(|result| result.rows_affected())
}

async fn insert_account_proctran(
    conn: &mut PgConnection,
    account: &AccountDetails,
    command: &DeleteCustomerCommand,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO proctran (sortcode, logical_delete, tran_date, tran_time, tran_ref, tran_type, description, amount)
        VALUES ($1, FALSE, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(account.sortcode())
    .bind(command.transaction_date)
    .bind(command.transaction_time)
    .bind(command.transaction_reference)
    .bind(ProcTranType::AccountDelete.as_str())
    .bind(account_proctran_description(account))
    .bind(account.actual_balance())
    .execute(conn)
    .await
    .map(|_| ())
}

async fn insert_customer_proctran(
    conn: &mut PgConnection,
    customer: &CustomerDetails,
    command: &DeleteCustomerCommand,
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
    .bind(ProcTranType::CustomerDelete.as_str())
    .bind(customer_proctran_description(customer))
    .bind(Decimal::ZERO)
    .execute(conn)
    .await
    .map(|_| ())
}

fn account_proctran_description(account: &AccountDetails) -> String {
    format!(
        "{:010}{}{}{}DELETE",
        account.customer_number(),
        pad_or_truncate(account.account_type(), 8),
        optional_cobol_date(account.last_statement_date()),
        optional_cobol_date(account.next_statement_date()),
    )
}

fn customer_proctran_description(customer: &CustomerDetails) -> String {
    format!(
        "{}{:010}{}{}",
        customer.sortcode(),
        customer.customer_number(),
        pad_or_truncate(customer.name(), 14),
        customer.date_of_birth().format("%d/%m/%Y"),
    )
}

fn optional_cobol_date(date: Option<NaiveDate>) -> String {
    date.map(|date| date.format("%d%m%Y").to_string())
        .unwrap_or_else(|| "00000000".to_string())
}

fn pad_or_truncate(value: &str, width: usize) -> String {
    let truncated: String = value.chars().take(width).collect();
    let padding = width.saturating_sub(truncated.chars().count());
    format!("{truncated}{}", " ".repeat(padding))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeleteCustomerOnceOutcome {
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

impl DeleteCustomerOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }

    fn abend(code: &'static str, message: impl Into<String>) -> Self {
        Self::Abend {
            code,
            message: message.into(),
        }
    }
}
