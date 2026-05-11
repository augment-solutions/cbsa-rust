use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, Timelike, Utc};
use sqlx::PgPool;
use tokio::{
    task::JoinSet,
    time::{Duration, Instant},
};

use crate::{
    config::is_six_ascii_digits,
    domain::{CustomerDetails, CustomerProfile},
    error::CbsaError,
    repository::crecust::{self, CreateCustomerCommand, CreateCustomerOutcome},
};

const INVALID_TITLE_CODE: &str = "T";
const DATE_TOO_OLD_CODE: &str = "O";
const FUTURE_DATE_CODE: &str = "Y";
const INVALID_DATE_CODE: &str = "Z";
const CREDIT_FAILURE_CODE: &str = "G";
const CUSTOMER_TITLE_MESSAGE: &str = "The customer title is invalid.";
const CREDIT_FAILURE_MESSAGE: &str = "Credit check could not be completed.";
const CUSTOMER_SORTCODE_MESSAGE: &str = "sortcode must be exactly 6 ASCII digits";
const CREDIT_REPLY_WINDOW: Duration = Duration::from_secs(3);
const REVIEW_DATE_BOUND_DAYS: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateOfBirthParseFailure {
    TooOld,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrecustRequest {
    pub name: String,
    pub address: String,
    pub date_of_birth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrecustResult {
    customer: Option<CustomerDetails>,
    fail_code: &'static str,
    message: Option<String>,
}

impl CrecustResult {
    pub fn success(customer: CustomerDetails) -> Self {
        Self {
            customer: Some(customer),
            fail_code: "0",
            message: None,
        }
    }

    pub fn failure(fail_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            customer: None,
            fail_code,
            message: Some(message.into()),
        }
    }

    pub fn creation_success(&self) -> bool {
        self.customer.is_some()
    }

    pub fn customer(&self) -> Option<&CustomerDetails> {
        self.customer.as_ref()
    }

    pub fn fail_code(&self) -> &str {
        self.fail_code
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn is_validation_failure(&self) -> bool {
        matches!(
            self.fail_code,
            INVALID_TITLE_CODE | DATE_TOO_OLD_CODE | FUTURE_DATE_CODE | INVALID_DATE_CODE
        )
    }

    pub fn is_credit_failure(&self) -> bool {
        self.fail_code == CREDIT_FAILURE_CODE
    }
}

pub async fn create(
    pool: &PgPool,
    sortcode: &str,
    request: CrecustRequest,
) -> Result<CrecustResult, CbsaError> {
    let clock = SystemClock;
    let mut review_date_generator = SystemReviewDateGenerator::default();
    let credit_client: Arc<dyn CreditAgencyClient> = Arc::new(SimulatedCreditAgencyClient);
    create_with_dependencies(
        pool,
        sortcode,
        request,
        &clock,
        &mut review_date_generator,
        credit_client,
    )
    .await
}

async fn create_with_dependencies(
    pool: &PgPool,
    sortcode: &str,
    request: CrecustRequest,
    clock: &dyn Clock,
    review_date_generator: &mut dyn ReviewDateGenerator,
    credit_client: Arc<dyn CreditAgencyClient>,
) -> Result<CrecustResult, CbsaError> {
    validate_sortcode(sortcode)?;

    let profile =
        CustomerProfile::new(request.name, request.address).map_err(CbsaError::validation)?;
    if !valid_title(profile.name()) {
        return Ok(CrecustResult::failure(
            INVALID_TITLE_CODE,
            CUSTOMER_TITLE_MESSAGE,
        ));
    }

    let date_of_birth = match parse_date_of_birth(request.date_of_birth) {
        Ok(date_of_birth) => date_of_birth,
        Err(DateOfBirthParseFailure::TooOld) => {
            return Ok(CrecustResult::failure(
                DATE_TOO_OLD_CODE,
                "Date of birth must not be earlier than 1601.",
            ))
        }
        Err(DateOfBirthParseFailure::Invalid) => {
            return Ok(CrecustResult::failure(
                INVALID_DATE_CODE,
                "Date of birth is invalid.",
            ))
        }
    };
    let now = clock.now();
    let today = now.date_naive();
    if let Some(failure) = validate_date_of_birth(today, date_of_birth) {
        return Ok(failure);
    }

    let Some(credit_decision) = evaluate_credit(
        credit_client,
        CreditAgencyRequest {
            name: profile.name().to_string(),
            address: profile.address().to_string(),
            date_of_birth: request.date_of_birth,
        },
        today,
        review_date_generator,
    )
    .await
    else {
        return Ok(CrecustResult::failure(
            CREDIT_FAILURE_CODE,
            CREDIT_FAILURE_MESSAGE,
        ));
    };

    let outcome = crecust::create_customer(
        pool,
        CreateCustomerCommand {
            sortcode: sortcode.to_string(),
            name: profile.name().to_string(),
            address: profile.address().to_string(),
            date_of_birth,
            credit_score: credit_decision.credit_score,
            credit_score_review_date: credit_decision.review_date,
            transaction_reference: now.timestamp_millis().max(0),
            transaction_date: today,
            transaction_time: NaiveTime::from_hms_opt(now.hour(), now.minute(), now.second())
                .expect("valid UTC wall-clock time"),
        },
    )
    .await?;

    Ok(match outcome {
        CreateCustomerOutcome::Success(customer) => CrecustResult::success(customer),
        CreateCustomerOutcome::Failure { fail_code, message } => {
            CrecustResult::failure(fail_code, message)
        }
    })
}

fn validate_sortcode(sortcode: &str) -> Result<(), CbsaError> {
    if is_six_ascii_digits(sortcode) {
        Ok(())
    } else {
        Err(CbsaError::validation(CUSTOMER_SORTCODE_MESSAGE))
    }
}

fn parse_date_of_birth(raw: u32) -> Result<NaiveDate, DateOfBirthParseFailure> {
    let normalized = format!("{raw:08}");
    NaiveDate::parse_from_str(&normalized, "%d%m%Y").map_err(|_| {
        let year = raw % 10_000;
        if year < 1601 {
            DateOfBirthParseFailure::TooOld
        } else {
            DateOfBirthParseFailure::Invalid
        }
    })
}

fn validate_date_of_birth(today: NaiveDate, date_of_birth: NaiveDate) -> Option<CrecustResult> {
    if date_of_birth.year() < 1601 {
        return Some(CrecustResult::failure(
            DATE_TOO_OLD_CODE,
            "Date of birth must not be earlier than 1601.",
        ));
    }

    if today.year() - date_of_birth.year() > 150 {
        return Some(CrecustResult::failure(
            DATE_TOO_OLD_CODE,
            "Date of birth must not be more than 150 years ago.",
        ));
    }

    if date_of_birth > today {
        return Some(CrecustResult::failure(
            FUTURE_DATE_CODE,
            "Date of birth must not be in the future.",
        ));
    }

    None
}

fn valid_title(name: &str) -> bool {
    matches!(
        first_token(name),
        "Professor" | "Mr" | "Mrs" | "Miss" | "Ms" | "Dr" | "Drs" | "Lord" | "Sir" | "Lady"
    )
}

fn first_token(name: &str) -> &str {
    let trimmed = name.trim_start();
    if trimmed.is_empty() {
        return "";
    }
    trimmed.split_once(' ').map_or(trimmed, |(first, _)| first)
}

async fn evaluate_credit(
    credit_client: Arc<dyn CreditAgencyClient>,
    request: CreditAgencyRequest,
    today: NaiveDate,
    review_date_generator: &mut dyn ReviewDateGenerator,
) -> Option<CreditDecision> {
    let mut join_set = JoinSet::new();
    for agency in CreditAgency::ALL {
        let client = Arc::clone(&credit_client);
        let request = request.clone();
        join_set.spawn(async move { client.request_score(agency, request).await });
    }

    let deadline = Instant::now() + CREDIT_REPLY_WINDOW;
    let mut total_score = 0u32;
    let mut returned_scores = 0u32;

    while !join_set.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            join_set.abort_all();
            break;
        }

        match tokio::time::timeout(remaining, join_set.join_next()).await {
            Ok(Some(Ok(Ok(score)))) if (1..=998).contains(&score) => {
                total_score += u32::from(score);
                returned_scores += 1;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                join_set.abort_all();
                break;
            }
        }
    }

    if returned_scores == 0 {
        return None;
    }

    let review_offset_days = review_date_generator.next_offset_days();
    let review_date = today.checked_add_days(Days::new(u64::from(review_offset_days)))?;
    Some(CreditDecision {
        credit_score: (total_score / returned_scores) as u16,
        review_date,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CreditDecision {
    credit_score: u16,
    review_date: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreditAgencyRequest {
    name: String,
    address: String,
    date_of_birth: u32,
}

// CRECUST.cbl CREDIT-CHECK issues five async child programs (OCR1..OCR5) and
// binds them to fixed container slots CIPA..CIPE. The Rust translation models
// those reserved slots as five stable agency identifiers until CRDTAGY itself
// is migrated and wired in as a separate program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreditAgency {
    One = 1,
    Two = 2,
    Three = 3,
    Four = 4,
    Five = 5,
}

impl CreditAgency {
    const ALL: [Self; 5] = [Self::One, Self::Two, Self::Three, Self::Four, Self::Five];

    fn number(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreditAgencyError;

#[async_trait]
trait CreditAgencyClient: Send + Sync {
    async fn request_score(
        &self,
        agency: CreditAgency,
        request: CreditAgencyRequest,
    ) -> Result<u16, CreditAgencyError>;
}

trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

trait ReviewDateGenerator: Send {
    fn next_offset_days(&mut self) -> u32;
}

struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug)]
struct SystemReviewDateGenerator {
    state: u64,
}

impl Default for SystemReviewDateGenerator {
    fn default() -> Self {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            state: duration.as_secs() ^ u64::from(duration.subsec_nanos()),
        }
    }
}

impl ReviewDateGenerator for SystemReviewDateGenerator {
    fn next_offset_days(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((self.state % u64::from(REVIEW_DATE_BOUND_DAYS)) + 1) as u32
    }
}

struct SimulatedCreditAgencyClient;

#[async_trait]
impl CreditAgencyClient for SimulatedCreditAgencyClient {
    async fn request_score(
        &self,
        agency: CreditAgency,
        request: CreditAgencyRequest,
    ) -> Result<u16, CreditAgencyError> {
        tokio::task::yield_now().await;
        Ok(simulated_credit_score(agency, &request))
    }
}

fn simulated_credit_score(agency: CreditAgency, request: &CreditAgencyRequest) -> u16 {
    let mut hash = 14_695_981_039_346_656_037u64;
    for byte in request
        .name
        .bytes()
        .chain(request.address.bytes())
        .chain(format!("{:08}", request.date_of_birth).bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    (((hash ^ u64::from(agency.number())) % 998) + 1) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    struct FixedReviewDateGenerator(u32);
    impl ReviewDateGenerator for FixedReviewDateGenerator {
        fn next_offset_days(&mut self) -> u32 {
            self.0
        }
    }

    struct FakeCreditAgencyClient {
        responses: Vec<Result<u16, CreditAgencyError>>,
    }

    #[async_trait]
    impl CreditAgencyClient for FakeCreditAgencyClient {
        async fn request_score(
            &self,
            agency: CreditAgency,
            _request: CreditAgencyRequest,
        ) -> Result<u16, CreditAgencyError> {
            self.responses[usize::from(agency.number() - 1)].clone()
        }
    }

    #[test]
    fn first_token_ignores_leading_whitespace() {
        assert_eq!(first_token("   Dr Alice Example"), "Dr");
    }

    #[test]
    fn validate_date_of_birth_rejects_future_and_ancient_dates() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 1).expect("valid date");
        assert_eq!(
            validate_date_of_birth(
                today,
                NaiveDate::from_ymd_opt(1800, 1, 10).expect("valid date"),
            )
            .expect("ancient DOB must fail")
            .fail_code(),
            DATE_TOO_OLD_CODE
        );
        assert_eq!(
            validate_date_of_birth(
                today,
                NaiveDate::from_ymd_opt(2030, 1, 10).expect("valid date")
            )
            .expect("future DOB must fail")
            .fail_code(),
            FUTURE_DATE_CODE
        );
    }

    #[test]
    fn parse_date_of_birth_maps_invalid_calendar_dates_to_domain_fail_codes() {
        assert_eq!(
            parse_date_of_birth(31_022_000).expect_err("invalid calendar DOB must fail"),
            DateOfBirthParseFailure::Invalid
        );
    }

    #[tokio::test]
    async fn evaluate_credit_averages_successful_scores_and_applies_review_offset() {
        let mut review_date_generator = FixedReviewDateGenerator(7);
        let decision = evaluate_credit(
            Arc::new(FakeCreditAgencyClient {
                responses: vec![
                    Ok(410),
                    Ok(420),
                    Err(CreditAgencyError),
                    Ok(460),
                    Err(CreditAgencyError),
                ],
            }),
            CreditAgencyRequest {
                name: "Dr Alice Example".to_string(),
                address: "1 Main Street".to_string(),
                date_of_birth: 10_012_000,
            },
            NaiveDate::from_ymd_opt(2026, 5, 1).expect("valid date"),
            &mut review_date_generator,
        )
        .await
        .expect("credit decision must be produced");

        assert_eq!(decision.credit_score, 430);
        assert_eq!(
            decision.review_date,
            NaiveDate::from_ymd_opt(2026, 5, 8).expect("valid date")
        );
    }

    #[tokio::test]
    async fn evaluate_credit_returns_none_when_no_agency_responds() {
        let mut review_date_generator = FixedReviewDateGenerator(1);
        let decision = evaluate_credit(
            Arc::new(FakeCreditAgencyClient {
                responses: vec![
                    Err(CreditAgencyError),
                    Err(CreditAgencyError),
                    Err(CreditAgencyError),
                    Err(CreditAgencyError),
                    Err(CreditAgencyError),
                ],
            }),
            CreditAgencyRequest {
                name: "Dr Alice Example".to_string(),
                address: "1 Main Street".to_string(),
                date_of_birth: 10_012_000,
            },
            FixedClock(
                DateTime::parse_from_rfc3339("2026-05-01T10:15:30Z")
                    .expect("valid timestamp")
                    .with_timezone(&Utc),
            )
            .now()
            .date_naive(),
            &mut review_date_generator,
        )
        .await;

        assert!(decision.is_none());
    }
}
