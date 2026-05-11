use chrono::{NaiveDate, NaiveTime};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{AccountDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const NOT_FOUND_CODE: &str = "1";
const READ_ABEND_CODE: &str = "HRAC";
const UPDATE_ABEND_CODE: &str = "HUAC";
const RETRY_EXHAUSTED_MESSAGE: &str =
    "UPDACC aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAccountCommand {
    pub sortcode: String,
    pub account_number: i64,
    pub account_type: String,
    pub interest_rate: Decimal,
    pub overdraft_limit: Decimal,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAccountOutcome {
    Success(AccountDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

pub async fn update_account(
    pool: &PgPool,
    command: UpdateAccountCommand,
) -> Result<UpdateAccountOutcome, CbsaError> {
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        async move {
            let mut tx = pool.begin().await?;
            let outcome = update_account_once(&mut tx, &command).await?;
            if matches!(outcome, UpdateAccountOnceOutcome::Success(_)) {
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
        UpdateAccountOnceOutcome::Success(account) => Ok(UpdateAccountOutcome::Success(account)),
        UpdateAccountOnceOutcome::Failure { fail_code, message } => {
            Ok(UpdateAccountOutcome::Failure { fail_code, message })
        }
        UpdateAccountOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn update_account_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &UpdateAccountCommand,
) -> Result<UpdateAccountOnceOutcome, sqlx::Error> {
    let existing_account =
        match load_account_for_update(&mut *tx, &command.sortcode, command.account_number).await {
            Ok(Some(account)) => account,
            Ok(None) => {
                return Ok(UpdateAccountOnceOutcome::failure(
                    NOT_FOUND_CODE,
                    format!("Account number {} was not found.", command.account_number),
                ))
            }
            Err(CbsaError::Database(err)) if db::is_serialization_failure(&err) => return Err(err),
            Err(CbsaError::Abend(code, message)) => {
                return Ok(UpdateAccountOnceOutcome::Abend { code, message })
            }
            Err(err) => {
                return Ok(UpdateAccountOnceOutcome::Abend {
                    code: READ_ABEND_CODE,
                    message: format!("UPDACC failed to read the account data: {err}"),
                })
            }
        };

    let updated_account = match AccountDetails::new(
        existing_account.sortcode().to_string(),
        existing_account.customer_number(),
        existing_account.account_number(),
        command.account_type.clone(),
        command.interest_rate,
        existing_account.opened(),
        command.overdraft_limit,
        existing_account.last_statement_date(),
        existing_account.next_statement_date(),
        existing_account.available_balance(),
        existing_account.actual_balance(),
    ) {
        Ok(account) => account,
        Err(message) => {
            return Ok(UpdateAccountOnceOutcome::Abend {
                code: UPDATE_ABEND_CODE,
                message: format!("UPDACC built invalid updated account data: {message}"),
            })
        }
    };

    match update_account_row(&mut *tx, &updated_account).await {
        Ok(1) => {}
        Ok(rows) => {
            return Ok(UpdateAccountOnceOutcome::Abend {
                code: UPDATE_ABEND_CODE,
                message: format!("UPDACC updated {rows} rows instead of 1."),
            })
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(UpdateAccountOnceOutcome::Abend {
                code: UPDATE_ABEND_CODE,
                message: format!("UPDACC failed to update the account data: {err}"),
            })
        }
    }

    match insert_proctran(&mut *tx, &updated_account, command).await {
        Ok(()) => Ok(UpdateAccountOnceOutcome::Success(updated_account)),
        Err(err) if db::is_serialization_failure(&err) => Err(err),
        Err(err) => Ok(UpdateAccountOnceOutcome::Abend {
            code: PROCTRAN_ABEND_CODE,
            message: format!("UPDACC failed to write the audit trail: {err}"),
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
                format!("UPDACC loaded invalid account data: {message}"),
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

async fn update_account_row(
    conn: &mut PgConnection,
    account: &AccountDetails,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE account
        SET account_type = $1,
            interest_rate = $2,
            overdraft_limit = $3
        WHERE sortcode = $4 AND account_number = $5
        "#,
    )
    .bind(account.account_type())
    .bind(account.interest_rate())
    .bind(account.overdraft_limit())
    .bind(account.sortcode())
    .bind(account.account_number())
    .execute(conn)
    .await
    .map(|result| result.rows_affected())
}

async fn insert_proctran(
    conn: &mut PgConnection,
    account: &AccountDetails,
    command: &UpdateAccountCommand,
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
    .bind(ProcTranType::AccountUpdate.as_str())
    .bind(proctran_description(account))
    .bind(Decimal::ZERO)
    .execute(conn)
    .await
    .map(|_| ())
}

fn proctran_description(account: &AccountDetails) -> String {
    format!(
        "{:08}{}{}{}          ",
        account.account_number(),
        pad_or_truncate(account.account_type(), 8),
        unsigned_decimal_digits(account.interest_rate(), 6),
        unsigned_integer_digits(account.overdraft_limit(), 8),
    )
}

fn unsigned_decimal_digits(value: Decimal, width: usize) -> String {
    let scaled = (value.round_dp(2) * Decimal::new(100, 0))
        .to_i128()
        .expect("decimal must fit into i128");
    format!("{:0width$}", scaled, width = width)
}

fn unsigned_integer_digits(value: Decimal, width: usize) -> String {
    let integer = value.trunc().to_i128().expect("decimal must fit into i128");
    format!("{:0width$}", integer, width = width)
}

fn pad_or_truncate(value: &str, width: usize) -> String {
    let truncated: String = value.chars().take(width).collect();
    let padding = width.saturating_sub(truncated.chars().count());
    format!("{truncated}{}", " ".repeat(padding))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateAccountOnceOutcome {
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

impl UpdateAccountOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }
}
