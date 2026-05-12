use chrono::{NaiveDate, NaiveTime};
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{AccountDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const NOT_FOUND_CODE: &str = "1";
const UPDATE_FAILURE_CODE: &str = "2";
const INSUFFICIENT_FUNDS_CODE: &str = "3";
const DISALLOWED_ACCOUNT_TYPE_CODE: &str = "4";
const READ_ABEND_CODE: &str = "HRAC";
const UPDATE_ABEND_CODE: &str = "HUAC";
const PAYMENT_FACILITY_TYPE: i32 = 496;
const RETRY_EXHAUSTED_MESSAGE: &str =
    "DBCRFUN aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbcrfunCommand {
    pub sortcode: String,
    pub account_number: i64,
    pub amount: Decimal,
    pub facility_type: i32,
    pub payment_origin_description: String,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbcrfunOutcome {
    Success(AccountDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

pub async fn post_transaction(
    pool: &PgPool,
    command: DbcrfunCommand,
) -> Result<DbcrfunOutcome, CbsaError> {
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        async move {
            let mut tx = pool.begin().await?;
            let outcome = post_transaction_once(&mut tx, &command).await?;
            if matches!(outcome, DbcrfunOnceOutcome::Success(_)) {
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
        DbcrfunOnceOutcome::Success(account) => Ok(DbcrfunOutcome::Success(account)),
        DbcrfunOnceOutcome::Failure { fail_code, message } => {
            Ok(DbcrfunOutcome::Failure { fail_code, message })
        }
        DbcrfunOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn post_transaction_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &DbcrfunCommand,
) -> Result<DbcrfunOnceOutcome, sqlx::Error> {
    let account =
        match load_account_for_update(&mut *tx, &command.sortcode, command.account_number).await {
            Ok(Some(account)) => account,
            Ok(None) => {
                return Ok(DbcrfunOnceOutcome::failure(
                    NOT_FOUND_CODE,
                    format!("Account number {} was not found.", command.account_number),
                ))
            }
            Err(CbsaError::Database(err)) if db::is_serialization_failure(&err) => return Err(err),
            Err(CbsaError::Abend(code, message)) => {
                return Ok(DbcrfunOnceOutcome::Abend { code, message })
            }
            Err(err) => {
                return Ok(DbcrfunOnceOutcome::Abend {
                    code: READ_ABEND_CODE,
                    message: format!("DBCRFUN failed to read the account data: {err}"),
                })
            }
        };

    if is_payment_facility(command.facility_type)
        && is_restricted_account_type(account.account_type())
    {
        return Ok(DbcrfunOnceOutcome::failure(
            DISALLOWED_ACCOUNT_TYPE_CODE,
            format!(
                "Payments are not supported for account type {}.",
                account.account_type()
            ),
        ));
    }

    let updated_available_balance = round_money(account.available_balance() + command.amount);
    if command.amount < Decimal::ZERO
        && is_payment_facility(command.facility_type)
        && updated_available_balance < Decimal::ZERO
    {
        return Ok(DbcrfunOnceOutcome::failure(
            INSUFFICIENT_FUNDS_CODE,
            format!(
                "Account number {} does not have sufficient available funds.",
                command.account_number
            ),
        ));
    }

    let updated_actual_balance = round_money(account.actual_balance() + command.amount);
    let updated_account = match AccountDetails::new(
        account.sortcode().to_string(),
        account.customer_number(),
        account.account_number(),
        account.account_type().to_string(),
        account.interest_rate(),
        account.opened(),
        account.overdraft_limit(),
        account.last_statement_date(),
        account.next_statement_date(),
        updated_available_balance,
        updated_actual_balance,
    ) {
        Ok(account) => account,
        Err(message) => {
            return Ok(DbcrfunOnceOutcome::Abend {
                code: UPDATE_ABEND_CODE,
                message: format!("DBCRFUN built invalid updated account data: {message}"),
            })
        }
    };

    match update_account_row(&mut *tx, &updated_account).await {
        Ok(1) => {}
        Ok(_) => {
            return Ok(DbcrfunOnceOutcome::failure(
                UPDATE_FAILURE_CODE,
                "DBCRFUN failed to update the account record.",
            ))
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(DbcrfunOnceOutcome::Abend {
                code: UPDATE_ABEND_CODE,
                message: format!("DBCRFUN failed to update the account data: {err}"),
            })
        }
    }

    match insert_proctran(&mut *tx, &updated_account, command).await {
        Ok(()) => Ok(DbcrfunOnceOutcome::Success(updated_account)),
        Err(err) if db::is_serialization_failure(&err) => Err(err),
        Err(err) => Ok(DbcrfunOnceOutcome::Abend {
            code: PROCTRAN_ABEND_CODE,
            message: format!("DBCRFUN failed to write the audit trail: {err}"),
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
                format!("DBCRFUN loaded invalid account data: {message}"),
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
        SET available_balance = $1,
            actual_balance = $2
        WHERE sortcode = $3 AND account_number = $4
        "#,
    )
    .bind(account.available_balance())
    .bind(account.actual_balance())
    .bind(account.sortcode())
    .bind(account.account_number())
    .execute(conn)
    .await
    .map(|result| result.rows_affected())
}

async fn insert_proctran(
    conn: &mut PgConnection,
    account: &AccountDetails,
    command: &DbcrfunCommand,
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
    .bind(proctran_type(command).as_str())
    .bind(proctran_description(command))
    .bind(command.amount)
    .execute(conn)
    .await
    .map(|_| ())
}

fn proctran_type(command: &DbcrfunCommand) -> ProcTranType {
    if command.amount < Decimal::ZERO {
        if is_payment_facility(command.facility_type) {
            ProcTranType::PaymentDebit
        } else {
            ProcTranType::Debit
        }
    } else if is_payment_facility(command.facility_type) {
        ProcTranType::PaymentCredit
    } else {
        ProcTranType::Credit
    }
}

fn proctran_description(command: &DbcrfunCommand) -> String {
    let description = if is_payment_facility(command.facility_type) {
        pad_or_truncate(&command.payment_origin_description, 14)
    } else if command.amount < Decimal::ZERO {
        "COUNTER WTHDRW".to_string()
    } else {
        "COUNTER RECVED".to_string()
    };
    format!(
        "{}{}",
        description,
        " ".repeat(40 - description.chars().count())
    )
}

fn is_payment_facility(facility_type: i32) -> bool {
    facility_type == PAYMENT_FACILITY_TYPE
}

fn is_restricted_account_type(account_type: &str) -> bool {
    matches!(account_type.trim_end(), "MORTGAGE" | "LOAN")
}

fn pad_or_truncate(value: &str, width: usize) -> String {
    let truncated: String = value.chars().take(width).collect();
    let padding = width.saturating_sub(truncated.chars().count());
    format!("{truncated}{}", " ".repeat(padding))
}

fn round_money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DbcrfunOnceOutcome {
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

impl DbcrfunOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }
}
