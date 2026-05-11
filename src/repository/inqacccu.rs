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

pub async fn customer_exists(
    executor: impl Executor<'_, Database = Postgres>,
    sortcode: &str,
    customer_number: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM customer
            WHERE sortcode = $1 AND customer_number = $2
        )
        "#,
    )
    .bind(sortcode)
    .bind(customer_number)
    .fetch_one(executor)
    .await
}

pub async fn find_by_sortcode_and_customer_number(
    executor: impl Executor<'_, Database = Postgres>,
    sortcode: &str,
    customer_number: i64,
) -> Result<Vec<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT customer_number, sortcode, account_number, account_type, interest_rate, opened,
               overdraft_limit, last_stmt_date, next_stmt_date, available_balance, actual_balance
        FROM account
        WHERE sortcode = $1 AND customer_number = $2
        ORDER BY account_number ASC
        LIMIT 20
        "#,
    )
    .bind(sortcode)
    .bind(customer_number)
    .fetch_all(executor)
    .await
}
