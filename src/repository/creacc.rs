use std::sync::Arc;

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{AccountDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const GLOBAL_CONTROL_ID: &str = "GLOBAL";
const COUNTER_ABEND_CODE: &str = "HNCS";
const ACCOUNT_WRITE_FAIL_CODE: &str = "7";
const MAX_ACCOUNT_NUMBER: i64 = 99_999_999;
const RETRY_EXHAUSTED_MESSAGE: &str =
    "CREACC aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAccountCommand {
    pub sortcode: String,
    pub customer_number: i64,
    pub account_type: String,
    pub interest_rate: Decimal,
    pub overdraft_limit: Decimal,
    pub available_balance: Decimal,
    pub actual_balance: Decimal,
    pub opened: NaiveDate,
    pub last_statement_date: NaiveDate,
    pub next_statement_date: NaiveDate,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateAccountOutcome {
    Success(AccountDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

pub async fn create_account(
    pool: &PgPool,
    command: CreateAccountCommand,
) -> Result<CreateAccountOutcome, CbsaError> {
    create_account_with_hook(pool, command, Arc::new(NoopCreateAccountHook)).await
}

#[doc(hidden)]
pub async fn create_account_with_hook(
    pool: &PgPool,
    command: CreateAccountCommand,
    hook: Arc<dyn CreateAccountHook>,
) -> Result<CreateAccountOutcome, CbsaError> {
    let hook_for_retry = Arc::clone(&hook);
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        let hook = Arc::clone(&hook_for_retry);
        async move {
            let mut tx = pool.begin().await?;
            let outcome = create_account_once(&mut tx, &command, hook.as_ref()).await?;
            if matches!(outcome, CreateAccountOnceOutcome::Success(_)) {
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
        CreateAccountOnceOutcome::Success(account) => Ok(CreateAccountOutcome::Success(account)),
        CreateAccountOnceOutcome::Failure { fail_code, message } => {
            Ok(CreateAccountOutcome::Failure { fail_code, message })
        }
        CreateAccountOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn create_account_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &CreateAccountCommand,
    hook: &dyn CreateAccountHook,
) -> Result<CreateAccountOnceOutcome, sqlx::Error> {
    hook.before_reserve(&mut *tx).await?;

    let account_number = match reserve_next_account_number(&mut *tx).await {
        Ok(Some(number)) => number,
        Ok(None) => {
            return Ok(CreateAccountOnceOutcome::Abend {
                code: COUNTER_ABEND_CODE,
                message: "CREACC account control record is missing.".to_string(),
            })
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(CreateAccountOnceOutcome::Abend {
                code: COUNTER_ABEND_CODE,
                message: format!("CREACC failed to reserve the next account number: {err}"),
            })
        }
    };

    if account_number > MAX_ACCOUNT_NUMBER {
        return Ok(CreateAccountOnceOutcome::Abend {
            code: COUNTER_ABEND_CODE,
            message: "CREACC account numbering has reached its maximum value.".to_string(),
        });
    }

    let account = match AccountDetails::new(
        command.sortcode.clone(),
        command.customer_number,
        account_number,
        command.account_type.clone(),
        command.interest_rate,
        command.opened,
        command.overdraft_limit,
        Some(command.last_statement_date),
        Some(command.next_statement_date),
        command.available_balance,
        command.actual_balance,
    ) {
        Ok(account) => account,
        Err(_) => {
            return Ok(CreateAccountOnceOutcome::failure(
                ACCOUNT_WRITE_FAIL_CODE,
                "Unable to create the account record.",
            ))
        }
    };

    match insert_account(&mut *tx, &account).await {
        Ok(()) => {}
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(_) => {
            return Ok(CreateAccountOnceOutcome::failure(
                ACCOUNT_WRITE_FAIL_CODE,
                "Unable to create the account record.",
            ))
        }
    }

    match insert_proctran(&mut *tx, &account, command).await {
        Ok(()) => Ok(CreateAccountOnceOutcome::Success(account)),
        Err(err) if db::is_serialization_failure(&err) => Err(err),
        Err(err) => Ok(CreateAccountOnceOutcome::Abend {
            code: PROCTRAN_ABEND_CODE,
            message: format!("CREACC failed to write the audit trail: {err}"),
        }),
    }
}

async fn reserve_next_account_number(conn: &mut PgConnection) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        r#"
        UPDATE control
        SET account_count = account_count + 1,
            account_last = account_last + 1
        WHERE id = $1
        RETURNING account_last
        "#,
    )
    .bind(GLOBAL_CONTROL_ID)
    .fetch_optional(conn)
    .await
}

async fn insert_account(
    conn: &mut PgConnection,
    account: &AccountDetails,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO account (
            sortcode,
            account_number,
            customer_number,
            account_type,
            interest_rate,
            opened,
            overdraft_limit,
            last_stmt_date,
            next_stmt_date,
            available_balance,
            actual_balance
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        "#,
    )
    .bind(account.sortcode())
    .bind(account.account_number())
    .bind(account.customer_number())
    .bind(account.account_type())
    .bind(account.interest_rate())
    .bind(account.opened())
    .bind(account.overdraft_limit())
    .bind(account.last_statement_date())
    .bind(account.next_statement_date())
    .bind(account.available_balance())
    .bind(account.actual_balance())
    .execute(conn)
    .await
    .map(|_| ())
}

async fn insert_proctran(
    conn: &mut PgConnection,
    account: &AccountDetails,
    command: &CreateAccountCommand,
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
    .bind(ProcTranType::AccountCreate.as_str())
    .bind(proctran_description(account))
    .bind(Decimal::ZERO)
    .execute(conn)
    .await
    .map(|_| ())
}

fn proctran_description(account: &AccountDetails) -> String {
    // CREACC.cbl WPD010 moves customer number, account type, the last statement
    // date, the next statement date, and six trailing spaces into
    // `HV-PROCTRAN-DESC(1:40)` before writing the OCA audit row (lines 960-964).
    format!(
        "{:010}{}{}{}      ",
        account.customer_number(),
        pad_or_truncate(account.account_type(), 8),
        account
            .last_statement_date()
            .expect("CREACC must populate last statement")
            .format("%d%m%Y"),
        account
            .next_statement_date()
            .expect("CREACC must populate next statement")
            .format("%d%m%Y"),
    )
}

fn pad_or_truncate(value: &str, width: usize) -> String {
    let truncated: String = value.chars().take(width).collect();
    let padding = width.saturating_sub(truncated.chars().count());
    format!("{truncated}{}", " ".repeat(padding))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CreateAccountOnceOutcome {
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

impl CreateAccountOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }
}

#[async_trait]
#[doc(hidden)]
pub trait CreateAccountHook: Send + Sync {
    async fn before_reserve(&self, conn: &mut PgConnection) -> Result<(), sqlx::Error>;
}

pub struct NoopCreateAccountHook;

#[async_trait]
impl CreateAccountHook for NoopCreateAccountHook {
    async fn before_reserve(&self, _conn: &mut PgConnection) -> Result<(), sqlx::Error> {
        Ok(())
    }
}
