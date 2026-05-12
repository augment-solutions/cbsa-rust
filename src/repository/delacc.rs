use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{AccountDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const NOT_FOUND_CODE: &str = "1";
const DELETE_FAILURE_CODE: &str = "3";
const READ_ABEND_CODE: &str = "HRAC";
const RETRY_EXHAUSTED_MESSAGE: &str =
    "DELACC aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAccountCommand {
    pub sortcode: String,
    pub account_number: i64,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteAccountOutcome {
    Success(AccountDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

pub async fn delete_account(
    pool: &PgPool,
    command: DeleteAccountCommand,
) -> Result<DeleteAccountOutcome, CbsaError> {
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        async move {
            let mut tx = pool.begin().await?;
            let outcome = delete_account_once(&mut tx, &command).await?;
            if matches!(outcome, DeleteAccountOnceOutcome::Success(_)) {
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
        DeleteAccountOnceOutcome::Success(account) => Ok(DeleteAccountOutcome::Success(account)),
        DeleteAccountOnceOutcome::Failure { fail_code, message } => {
            Ok(DeleteAccountOutcome::Failure { fail_code, message })
        }
        DeleteAccountOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn delete_account_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &DeleteAccountCommand,
) -> Result<DeleteAccountOnceOutcome, sqlx::Error> {
    let account =
        match load_account_for_update(&mut *tx, &command.sortcode, command.account_number).await {
            Ok(Some(account)) => account,
            Ok(None) => {
                return Ok(DeleteAccountOnceOutcome::failure(
                    NOT_FOUND_CODE,
                    format!("Account number {} was not found.", command.account_number),
                ))
            }
            Err(CbsaError::Database(err)) if db::is_serialization_failure(&err) => return Err(err),
            Err(CbsaError::Abend(code, message)) => {
                return Ok(DeleteAccountOnceOutcome::Abend { code, message })
            }
            Err(err) => {
                return Ok(DeleteAccountOnceOutcome::Abend {
                    code: READ_ABEND_CODE,
                    message: format!("DELACC failed to read the account data: {err}"),
                })
            }
        };

    match delete_account_row(&mut *tx, &command.sortcode, command.account_number).await {
        Ok(1) => {}
        Ok(_) => {
            return Ok(DeleteAccountOnceOutcome::failure(
                DELETE_FAILURE_CODE,
                format!(
                    "Account number {} could not be deleted.",
                    command.account_number
                ),
            ))
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(_) => {
            return Ok(DeleteAccountOnceOutcome::failure(
                DELETE_FAILURE_CODE,
                format!(
                    "Account number {} could not be deleted.",
                    command.account_number
                ),
            ))
        }
    }

    match insert_proctran(&mut *tx, &account, command).await {
        Ok(()) => Ok(DeleteAccountOnceOutcome::Success(account)),
        Err(err) if db::is_serialization_failure(&err) => Err(err),
        Err(err) => Ok(DeleteAccountOnceOutcome::Abend {
            code: PROCTRAN_ABEND_CODE,
            message: format!("DELACC failed to write the audit trail: {err}"),
        }),
    }
}

#[derive(Debug, Clone, FromRow)]
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
        .map_err(|message| {
            CbsaError::abend(
                READ_ABEND_CODE,
                format!("DELACC loaded invalid account data: {message}"),
            )
        })
    }
}

async fn load_account_for_update(
    conn: &mut PgConnection,
    sortcode: &str,
    account_number: i64,
) -> Result<Option<AccountDetails>, CbsaError> {
    let row = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT sortcode, customer_number, account_number, account_type, interest_rate, opened,
               overdraft_limit, last_stmt_date, next_stmt_date, available_balance, actual_balance
        FROM account
        WHERE sortcode = $1 AND account_number = $2
        FOR UPDATE
        "#,
    )
    .bind(sortcode)
    .bind(account_number)
    .fetch_optional(conn)
    .await?;

    row.map(TryInto::try_into).transpose()
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

async fn insert_proctran(
    conn: &mut PgConnection,
    account: &AccountDetails,
    command: &DeleteAccountCommand,
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
    .bind(proctran_description(account))
    .bind(account.actual_balance())
    .execute(conn)
    .await
    .map(|_| ())
}

fn proctran_description(account: &AccountDetails) -> String {
    format!(
        "{:010}{}{}{}DELETE",
        account.customer_number(),
        pad_or_truncate(account.account_type(), 8),
        optional_cobol_date(account.last_statement_date()),
        optional_cobol_date(account.next_statement_date()),
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
enum DeleteAccountOnceOutcome {
    Success(AccountDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
    Abend {
        code: &'static str,
        message: String,
    },
}

impl DeleteAccountOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }
}
