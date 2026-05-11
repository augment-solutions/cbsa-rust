use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{Executor, FromRow, Postgres};

#[derive(Debug, Clone, FromRow)]
pub struct AccountRow {
    pub customer_number: i64,
    pub sortcode: String,
    pub account_number: i64,
    pub account_type: String,
    pub interest_rate: Decimal,
    pub opened: NaiveDate,
    pub overdraft_limit: Decimal,
    pub last_stmt_date: Option<NaiveDate>,
    pub next_stmt_date: Option<NaiveDate>,
    pub available_balance: Decimal,
    pub actual_balance: Decimal,
}

pub async fn find_by_sortcode_and_account_number(
    executor: impl Executor<'_, Database = Postgres>,
    sortcode: &str,
    account_number: i64,
) -> Result<Option<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT customer_number, sortcode, account_number, account_type, interest_rate, opened,
               overdraft_limit, last_stmt_date, next_stmt_date, available_balance, actual_balance
        FROM account
        WHERE sortcode = $1 AND account_number = $2
        "#,
    )
    .bind(sortcode)
    .bind(account_number)
    .fetch_optional(executor)
    .await
}

pub async fn find_last_by_sortcode(
    executor: impl Executor<'_, Database = Postgres>,
    sortcode: &str,
) -> Result<Option<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT customer_number, sortcode, account_number, account_type, interest_rate, opened,
               overdraft_limit, last_stmt_date, next_stmt_date, available_balance, actual_balance
        FROM account
        WHERE sortcode = $1
        ORDER BY account_number DESC
        LIMIT 1
        "#,
    )
    .bind(sortcode)
    .fetch_optional(executor)
    .await
}
