use std::sync::Arc;

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::{
    db,
    domain::{CustomerDetails, ProcTranType},
    error::{CbsaError, PROCTRAN_ABEND_CODE, RETRY_EXHAUSTED_ABEND_CODE},
};

const GLOBAL_CONTROL_ID: &str = "GLOBAL";
const MAX_CUSTOMER_NUMBER: i64 = 9_999_999_999;
const CUSTOMER_WRITE_FAIL_CODE: &str = "1";
const COUNTER_RESERVE_FAIL_CODE: &str = "3";
const COUNTER_UPDATE_FAIL_CODE: &str = "4";
const RETRY_EXHAUSTED_MESSAGE: &str =
    "CRECUST aborted after exhausting Cockroach serialization retries.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCustomerCommand {
    pub sortcode: String,
    pub name: String,
    pub address: String,
    pub date_of_birth: NaiveDate,
    pub credit_score: u16,
    pub credit_score_review_date: NaiveDate,
    pub transaction_reference: i64,
    pub transaction_date: NaiveDate,
    pub transaction_time: NaiveTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateCustomerOutcome {
    Success(CustomerDetails),
    Failure {
        fail_code: &'static str,
        message: String,
    },
}

pub async fn create_customer(
    pool: &PgPool,
    command: CreateCustomerCommand,
) -> Result<CreateCustomerOutcome, CbsaError> {
    create_customer_with_hook(pool, command, Arc::new(NoopCreateCustomerHook)).await
}

async fn create_customer_with_hook(
    pool: &PgPool,
    command: CreateCustomerCommand,
    hook: Arc<dyn CreateCustomerHook>,
) -> Result<CreateCustomerOutcome, CbsaError> {
    let hook_for_retry = Arc::clone(&hook);
    let outcome = db::with_retry(pool, move |pool| {
        let command = command.clone();
        let hook = Arc::clone(&hook_for_retry);
        async move {
            let mut tx = pool.begin().await?;
            let outcome = create_customer_once(&mut tx, &command, hook.as_ref()).await?;
            if matches!(outcome, CreateCustomerOnceOutcome::Success(_)) {
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
        CreateCustomerOnceOutcome::Success(customer) => {
            Ok(CreateCustomerOutcome::Success(customer))
        }
        CreateCustomerOnceOutcome::Failure { fail_code, message } => {
            Ok(CreateCustomerOutcome::Failure { fail_code, message })
        }
        CreateCustomerOnceOutcome::Abend { code, message } => Err(CbsaError::abend(code, message)),
    }
}

async fn create_customer_once(
    tx: &mut Transaction<'_, Postgres>,
    command: &CreateCustomerCommand,
    hook: &dyn CreateCustomerHook,
) -> Result<CreateCustomerOnceOutcome, sqlx::Error> {
    hook.before_reserve(&mut *tx).await?;

    let allocated_number = match reserve_next_customer_number(&mut *tx).await {
        Ok(Some(number)) => number,
        Ok(None) => {
            return Ok(CreateCustomerOnceOutcome::failure(
                COUNTER_UPDATE_FAIL_CODE,
                "Customer control record is missing.",
            ));
        }
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(_) => {
            return Ok(CreateCustomerOnceOutcome::failure(
                COUNTER_RESERVE_FAIL_CODE,
                "Unable to reserve the next customer number.",
            ));
        }
    };

    if allocated_number > MAX_CUSTOMER_NUMBER {
        return Ok(CreateCustomerOnceOutcome::failure(
            COUNTER_UPDATE_FAIL_CODE,
            "Customer numbering has reached its maximum value.",
        ));
    }

    let customer = match CustomerDetails::new(
        command.sortcode.clone(),
        allocated_number,
        command.name.clone(),
        command.address.clone(),
        command.date_of_birth,
        command.credit_score,
        Some(command.credit_score_review_date),
    ) {
        Ok(customer) => customer,
        Err(_) => {
            return Ok(CreateCustomerOnceOutcome::failure(
                CUSTOMER_WRITE_FAIL_CODE,
                "Unable to create the customer record.",
            ));
        }
    };

    match insert_customer(&mut *tx, &customer).await {
        Ok(()) => {}
        Err(err) if db::is_serialization_failure(&err) => return Err(err),
        Err(_) => {
            return Ok(CreateCustomerOnceOutcome::failure(
                CUSTOMER_WRITE_FAIL_CODE,
                "Unable to create the customer record.",
            ));
        }
    }

    match insert_proctran(&mut *tx, &customer, command).await {
        Ok(()) => Ok(CreateCustomerOnceOutcome::Success(customer)),
        Err(err) if db::is_serialization_failure(&err) => Err(err),
        Err(err) => Ok(CreateCustomerOnceOutcome::Abend {
            code: PROCTRAN_ABEND_CODE,
            message: format!("CRECUST failed to write the audit trail: {err}"),
        }),
    }
}

async fn reserve_next_customer_number(conn: &mut PgConnection) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        r#"
        UPDATE control
        SET customer_count = customer_count + 1,
            customer_last = customer_last + 1
        WHERE id = $1
        RETURNING customer_last
        "#,
    )
    .bind(GLOBAL_CONTROL_ID)
    .fetch_optional(conn)
    .await
}

async fn insert_customer(
    conn: &mut PgConnection,
    customer: &CustomerDetails,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO customer (sortcode, customer_number, name, address, date_of_birth, credit_score, cs_review_date)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(customer.sortcode())
    .bind(customer.customer_number())
    .bind(customer.name())
    .bind(customer.address())
    .bind(customer.date_of_birth())
    .bind(i16::try_from(customer.credit_score()).expect("credit_score already validated to fit SMALLINT"))
    .bind(customer.credit_score_review_date())
    .execute(conn)
    .await
    .map(|_| ())
}

async fn insert_proctran(
    conn: &mut PgConnection,
    customer: &CustomerDetails,
    command: &CreateCustomerCommand,
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
    .bind(ProcTranType::CustomerCreate.as_str())
    .bind(proctran_description(customer))
    .bind(Decimal::ZERO)
    .execute(conn)
    .await
    .map(|_| ())
}

fn proctran_description(customer: &CustomerDetails) -> String {
    // CRECUST.cbl WCV010/WPD010 copies the sortcode, zero-padded customer
    // number, first 14 chars of the name, and a DD/MM/YYYY DOB string into the
    // 40-byte PROCTRAN description field before writing the OCC audit row.
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum CreateCustomerOnceOutcome {
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

impl CreateCustomerOnceOutcome {
    fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self::Failure {
            fail_code,
            message: message.into(),
        }
    }
}

#[async_trait]
trait CreateCustomerHook: Send + Sync {
    async fn before_reserve(&self, conn: &mut PgConnection) -> Result<(), sqlx::Error>;
}

struct NoopCreateCustomerHook;

#[async_trait]
impl CreateCustomerHook for NoopCreateCustomerHook {
    async fn before_reserve(&self, _conn: &mut PgConnection) -> Result<(), sqlx::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        error::Error as StdError,
        fmt,
        sync::atomic::{AtomicU32, Ordering},
    };

    use super::*;
    use crate::db;
    use sqlx::{
        error::{DatabaseError, ErrorKind},
        postgres::PgPoolOptions,
        Row,
    };
    use testcontainers_modules::{
        cockroach_db::CockroachDb,
        testcontainers::{runners::AsyncRunner, ContainerAsync},
    };
    use tokio::sync::{Mutex, OnceCell};

    static TEST_MUTEX: Mutex<()> = Mutex::const_new(());
    static TEST_DATABASE: OnceCell<TestDatabase> = OnceCell::const_new();

    struct TestDatabase {
        _container: Option<ContainerAsync<CockroachDb>>,
        database_url: String,
    }

    struct ForceRetryHook {
        remaining_failures: AtomicU32,
    }

    #[derive(Debug)]
    struct TestSerializationFailure;

    impl fmt::Display for TestSerializationFailure {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "forced serialization failure")
        }
    }

    impl StdError for TestSerializationFailure {}

    impl DatabaseError for TestSerializationFailure {
        fn message(&self) -> &str {
            "forced serialization failure"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed("40001"))
        }

        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    impl ForceRetryHook {
        fn new(remaining_failures: u32) -> Self {
            Self {
                remaining_failures: AtomicU32::new(remaining_failures),
            }
        }
    }

    #[async_trait]
    impl CreateCustomerHook for ForceRetryHook {
        async fn before_reserve(&self, _conn: &mut PgConnection) -> Result<(), sqlx::Error> {
            let should_fail = self
                .remaining_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    if value > 0 {
                        Some(value - 1)
                    } else {
                        None
                    }
                })
                .is_ok();

            if should_fail {
                return Err(sqlx::Error::database(TestSerializationFailure));
            }

            Ok(())
        }
    }

    #[tokio::test]
    async fn retries_serialization_failure_and_then_succeeds() {
        let _guard = TEST_MUTEX.lock().await;
        let pool = test_pool().await;
        clean_database(&pool).await;

        let outcome = create_customer_with_hook(&pool, command(), Arc::new(ForceRetryHook::new(1)))
            .await
            .expect("retry should eventually succeed");

        let customer = match outcome {
            CreateCustomerOutcome::Success(customer) => customer,
            other => panic!("expected success outcome, got {other:?}"),
        };

        assert_eq!(customer.customer_number(), 1);
        assert_eq!(customer.sortcode(), "987654");

        let control =
            sqlx::query("SELECT customer_count, customer_last FROM control WHERE id = 'GLOBAL'")
                .fetch_one(&pool)
                .await
                .expect("control row must exist");
        assert_eq!(control.get::<i64, _>("customer_count"), 1);
        assert_eq!(control.get::<i64, _>("customer_last"), 1);
    }

    #[tokio::test]
    async fn serialization_retry_exhaustion_maps_to_xrty() {
        let _guard = TEST_MUTEX.lock().await;
        let pool = test_pool().await;
        clean_database(&pool).await;

        let error = create_customer_with_hook(
            &pool,
            command(),
            Arc::new(ForceRetryHook::new(db::DEFAULT_RETRY_ATTEMPTS)),
        )
        .await
        .expect_err("retry exhaustion must surface as an abend");

        match error {
            CbsaError::Abend(code, _) => assert_eq!(code, RETRY_EXHAUSTED_ABEND_CODE),
            other => panic!("expected retry exhaustion abend, got {other:?}"),
        }
    }

    fn command() -> CreateCustomerCommand {
        CreateCustomerCommand {
            sortcode: "987654".to_string(),
            name: "Dr Alice Example".to_string(),
            address: "1 Main Street".to_string(),
            date_of_birth: NaiveDate::from_ymd_opt(2000, 1, 10).expect("valid date"),
            credit_score: 430,
            credit_score_review_date: NaiveDate::from_ymd_opt(2026, 5, 8).expect("valid date"),
            transaction_reference: 1234567890,
            transaction_date: NaiveDate::from_ymd_opt(2026, 5, 1).expect("valid date"),
            transaction_time: NaiveTime::from_hms_opt(10, 15, 30).expect("valid time"),
        }
    }

    async fn clean_database(pool: &PgPool) {
        sqlx::query("DELETE FROM account")
            .execute(pool)
            .await
            .expect("account cleanup must succeed");
        sqlx::query("DELETE FROM proctran")
            .execute(pool)
            .await
            .expect("proctran cleanup must succeed");
        sqlx::query("DELETE FROM customer")
            .execute(pool)
            .await
            .expect("customer cleanup must succeed");
        sqlx::query(
            "UPDATE control SET customer_count = 0, customer_last = 0, account_count = 0, account_last = 0 WHERE id = 'GLOBAL'",
        )
        .execute(pool)
        .await
        .expect("control reset must succeed");
    }

    async fn test_database() -> &'static TestDatabase {
        TEST_DATABASE
            .get_or_init(|| async {
                let container = CockroachDb::default().start().await.ok();
                let (host, port) = if let Some(container) = &container {
                    (
                        container
                            .get_host()
                            .await
                            .expect("host must resolve")
                            .to_string(),
                        container
                            .get_host_port_ipv4(26257)
                            .await
                            .expect("port must resolve"),
                    )
                } else {
                    ("localhost".to_string(), 26257)
                };

                let admin_pool = PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&format!(
                        "postgres://root@{host}:{port}/defaultdb?sslmode=disable"
                    ))
                    .await
                    .expect("admin pool must connect");
                sqlx::query("CREATE DATABASE IF NOT EXISTS cbsa")
                    .execute(&admin_pool)
                    .await
                    .expect("cbsa database must exist");
                drop(admin_pool);

                let pool = PgPoolOptions::new()
                    .max_connections(50)
                    .connect(&format!(
                        "postgres://root@{host}:{port}/cbsa?sslmode=disable"
                    ))
                    .await
                    .expect("application pool must connect");
                db::migrate(&pool).await.expect("migrations must apply");
                let database_url = format!("postgres://root@{host}:{port}/cbsa?sslmode=disable");
                drop(pool);

                TestDatabase {
                    _container: container,
                    database_url,
                }
            })
            .await
    }

    async fn test_pool() -> PgPool {
        let database = test_database().await;
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database.database_url)
            .await
            .expect("test pool must connect")
    }
}
