use chrono::{NaiveDate, NaiveTime};
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{AccountDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const FROM_NOT_FOUND_CODE: &str = "1";
const TO_NOT_FOUND_CODE: &str = "2";
const INSUFFICIENT_FUNDS_CODE: &str = "3";
const FROM_ACCOUNT_ABEND_CODE: &str = "FROM";
const TO_ACCOUNT_ABEND_CODE: &str = "TO  ";
const RETRY_EXHAUSTED_MESSAGE: &str =
    "XFRFUN aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XfrfunCommand {
    pub sortcode: String,
    pub from_account_number: i64,
    pub to_account_number: i64,
    pub amount: Decimal,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XfrfunOutcome {
    Success(Box<SuccessfulTransfer>),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessfulTransfer {
    pub from_account: AccountDetails,
    pub to_account: AccountDetails,
}

pub async fn transfer_funds(
    pool: &PgPool,
    command: XfrfunCommand,
) -> Result<XfrfunOutcome, CbsaError> {
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        async move {
            let mut tx = pool.begin().await?;
            let outcome = transfer_funds_once(&mut tx, &command).await?;
            if matches!(outcome, XfrfunOnceOutcome::Success(_)) {
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
        XfrfunOnceOutcome::Success(transfer) => {
            let SuccessfulTransfer {
                from_account,
                to_account,
            } = *transfer;

            Ok(XfrfunOutcome::Success(Box::new(SuccessfulTransfer {
                from_account,
                to_account,
            })))
        }
        XfrfunOnceOutcome::Failure { fail_code, message } => {
            Ok(XfrfunOutcome::Failure { fail_code, message })
        }
        XfrfunOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn transfer_funds_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &XfrfunCommand,
) -> Result<XfrfunOnceOutcome, sqlx::Error> {
    let accounts = match lock_accounts_in_cobol_order(&mut *tx, command).await? {
        LockedAccountsOutcome::Success(accounts) => *accounts,
        LockedAccountsOutcome::Failure { fail_code, message } => {
            return Ok(XfrfunOnceOutcome::Failure { fail_code, message })
        }
        LockedAccountsOutcome::Abend { code, message } => {
            return Ok(XfrfunOnceOutcome::Abend { code, message })
        }
    };

    let updated_from_available_balance =
        round_money(accounts.from_account.available_balance() - command.amount);
    let updated_from_actual_balance =
        round_money(accounts.from_account.actual_balance() - command.amount);

    if updated_from_available_balance < Decimal::ZERO
        || updated_from_actual_balance < -accounts.from_account.overdraft_limit()
    {
        return Ok(XfrfunOnceOutcome::failure(
            INSUFFICIENT_FUNDS_CODE,
            format!(
                "From account number {} does not have sufficient funds for this transfer.",
                command.from_account_number
            ),
        ));
    }

    let updated_from_account = match AccountDetails::new(
        accounts.from_account.sortcode().to_string(),
        accounts.from_account.customer_number(),
        accounts.from_account.account_number(),
        accounts.from_account.account_type().to_string(),
        accounts.from_account.interest_rate(),
        accounts.from_account.opened(),
        accounts.from_account.overdraft_limit(),
        accounts.from_account.last_statement_date(),
        accounts.from_account.next_statement_date(),
        updated_from_available_balance,
        updated_from_actual_balance,
    ) {
        Ok(account) => account,
        Err(message) => {
            return Ok(XfrfunOnceOutcome::Abend {
                code: FROM_ACCOUNT_ABEND_CODE,
                message: format!("XFRFUN built invalid updated FROM account data: {message}"),
            })
        }
    };

    let updated_to_account = match AccountDetails::new(
        accounts.to_account.sortcode().to_string(),
        accounts.to_account.customer_number(),
        accounts.to_account.account_number(),
        accounts.to_account.account_type().to_string(),
        accounts.to_account.interest_rate(),
        accounts.to_account.opened(),
        accounts.to_account.overdraft_limit(),
        accounts.to_account.last_statement_date(),
        accounts.to_account.next_statement_date(),
        round_money(accounts.to_account.available_balance() + command.amount),
        round_money(accounts.to_account.actual_balance() + command.amount),
    ) {
        Ok(account) => account,
        Err(message) => {
            return Ok(XfrfunOnceOutcome::Abend {
                code: TO_ACCOUNT_ABEND_CODE,
                message: format!("XFRFUN built invalid updated TO account data: {message}"),
            })
        }
    };

    match update_account_row(&mut *tx, &updated_from_account).await {
        Ok(1) => {}
        Ok(rows) => {
            return Ok(XfrfunOnceOutcome::Abend {
                code: FROM_ACCOUNT_ABEND_CODE,
                message: format!("XFRFUN updated {rows} FROM account rows instead of 1."),
            })
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(XfrfunOnceOutcome::Abend {
                code: FROM_ACCOUNT_ABEND_CODE,
                message: format!(
                    "XFRFUN failed to update the FROM account {}: {err}",
                    command.from_account_number
                ),
            })
        }
    }

    match update_account_row(&mut *tx, &updated_to_account).await {
        Ok(1) => {}
        Ok(rows) => {
            return Ok(XfrfunOnceOutcome::Abend {
                code: TO_ACCOUNT_ABEND_CODE,
                message: format!("XFRFUN updated {rows} TO account rows instead of 1."),
            })
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(XfrfunOnceOutcome::Abend {
                code: TO_ACCOUNT_ABEND_CODE,
                message: format!(
                    "XFRFUN failed to update the TO account {}: {err}",
                    command.to_account_number
                ),
            })
        }
    }

    match insert_proctran(
        &mut *tx,
        command,
        ProctranInsert {
            description: transfer_description(&command.sortcode, command.to_account_number),
            amount: command.amount,
        },
    )
    .await
    {
        Ok(()) => {}
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(err) => {
            return Ok(XfrfunOnceOutcome::Abend {
                code: PROCTRAN_ABEND_CODE,
                message: format!("XFRFUN failed to write the audit trail: {err}"),
            })
        }
    }

    Ok(XfrfunOnceOutcome::Success(Box::new(SuccessfulTransfer {
        from_account: updated_from_account,
        to_account: updated_to_account,
    })))
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

impl AccountRow {
    fn into_account(self) -> Result<AccountDetails, String> {
        AccountDetails::new(
            self.sortcode,
            self.customer_number,
            self.account_number,
            self.account_type.trim_end().to_string(),
            self.interest_rate,
            self.opened,
            self.overdraft_limit,
            self.last_stmt_date,
            self.next_stmt_date,
            self.available_balance,
            self.actual_balance,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockedAccounts {
    from_account: AccountDetails,
    to_account: AccountDetails,
}

enum LockedAccountsOutcome {
    Success(Box<LockedAccounts>),
    Failure {
        fail_code: &'static str,
        message: String,
    },
    Abend {
        code: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountRole {
    From,
    To,
}

async fn lock_accounts_in_cobol_order(
    conn: &mut PgConnection,
    command: &XfrfunCommand,
) -> Result<LockedAccountsOutcome, sqlx::Error> {
    let mut from_account = None;
    let mut to_account = None;

    for role in ordered_roles(command) {
        let account_number = account_number_for(role, command);
        let row = match load_account_row_for_update(conn, &command.sortcode, account_number).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return Ok(LockedAccountsOutcome::Failure {
                    fail_code: not_found_code(role),
                    message: format!(
                        "{} account number {} was not found.",
                        role_title(role),
                        account_number
                    ),
                })
            }
            Err(err) if db::is_serialization_failure(&err) => return Err(err),
            Err(err) => {
                return Ok(LockedAccountsOutcome::Abend {
                    code: abend_code(role),
                    message: format!(
                        "XFRFUN failed to read the {} account {}: {err}",
                        role_upper(role),
                        account_number
                    ),
                })
            }
        };

        let account = match row.into_account() {
            Ok(account) => account,
            Err(message) => {
                return Ok(LockedAccountsOutcome::Abend {
                    code: abend_code(role),
                    message: format!(
                        "XFRFUN loaded invalid {} account data: {message}",
                        role_upper(role)
                    ),
                })
            }
        };

        match role {
            AccountRole::From => from_account = Some(account),
            AccountRole::To => to_account = Some(account),
        }
    }

    Ok(LockedAccountsOutcome::Success(Box::new(LockedAccounts {
        from_account: from_account.expect("FROM account must be locked before success"),
        to_account: to_account.expect("TO account must be locked before success"),
    })))
}

fn ordered_roles(command: &XfrfunCommand) -> [AccountRole; 2] {
    if command.from_account_number < command.to_account_number {
        [AccountRole::From, AccountRole::To]
    } else {
        [AccountRole::To, AccountRole::From]
    }
}

fn account_number_for(role: AccountRole, command: &XfrfunCommand) -> i64 {
    match role {
        AccountRole::From => command.from_account_number,
        AccountRole::To => command.to_account_number,
    }
}

fn not_found_code(role: AccountRole) -> &'static str {
    match role {
        AccountRole::From => FROM_NOT_FOUND_CODE,
        AccountRole::To => TO_NOT_FOUND_CODE,
    }
}

fn abend_code(role: AccountRole) -> &'static str {
    match role {
        AccountRole::From => FROM_ACCOUNT_ABEND_CODE,
        AccountRole::To => TO_ACCOUNT_ABEND_CODE,
    }
}

fn role_title(role: AccountRole) -> &'static str {
    match role {
        AccountRole::From => "From",
        AccountRole::To => "To",
    }
}

fn role_upper(role: AccountRole) -> &'static str {
    match role {
        AccountRole::From => "FROM",
        AccountRole::To => "TO",
    }
}

async fn load_account_row_for_update(
    conn: &mut PgConnection,
    sortcode: &str,
    account_number: i64,
) -> Result<Option<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(
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
    .await
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

struct ProctranInsert {
    description: String,
    amount: Decimal,
}

async fn insert_proctran(
    conn: &mut PgConnection,
    command: &XfrfunCommand,
    row: ProctranInsert,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO proctran (sortcode, logical_delete, tran_date, tran_time, tran_ref, tran_type, description, amount)
        VALUES ($1, FALSE, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(&command.sortcode)
    .bind(command.transaction_date)
    .bind(command.transaction_time)
    .bind(command.transaction_reference)
    .bind(ProcTranType::Transfer.as_str())
    .bind(row.description)
    .bind(row.amount)
    .execute(conn)
    .await
    .map(|_| ())
}

fn transfer_description(counterparty_sortcode: &str, counterparty_account_number: i64) -> String {
    format!(
        "{}{counterparty_sortcode}{counterparty_account_number:08}",
        pad_or_truncate("TRANSFER", 26),
    )
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
enum XfrfunOnceOutcome {
    Success(Box<SuccessfulTransfer>),
    Failure {
        fail_code: &'static str,
        message: String,
    },
    Abend {
        code: &'static str,
        message: String,
    },
}

impl XfrfunOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }
}
