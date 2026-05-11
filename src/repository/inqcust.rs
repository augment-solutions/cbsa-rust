use chrono::NaiveDate;
use sqlx::{Executor, FromRow, Postgres};

use crate::{domain::CustomerDetails, error::CbsaError};

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

pub async fn find_by_sortcode_and_customer_number(
    executor: impl Executor<'_, Database = Postgres>,
    sortcode: &str,
    customer_number: i64,
) -> Result<Option<CustomerDetails>, CbsaError> {
    let row = sqlx::query_as::<_, CustomerRow>(
        r#"
        SELECT sortcode, customer_number, name, address, date_of_birth, credit_score, cs_review_date
        FROM customer
        WHERE sortcode = $1 AND customer_number = $2
        "#,
    )
    .bind(sortcode)
    .bind(customer_number)
    .fetch_optional(executor)
    .await?;

    row.map(TryInto::try_into).transpose()
}

pub async fn find_last_by_sortcode(
    executor: impl Executor<'_, Database = Postgres>,
    sortcode: &str,
) -> Result<Option<CustomerDetails>, CbsaError> {
    let row = sqlx::query_as::<_, CustomerRow>(
        r#"
        SELECT sortcode, customer_number, name, address, date_of_birth, credit_score, cs_review_date
        FROM customer
        WHERE sortcode = $1
        ORDER BY customer_number DESC
        LIMIT 1
        "#,
    )
    .bind(sortcode)
    .fetch_optional(executor)
    .await?;

    row.map(TryInto::try_into).transpose()
}
