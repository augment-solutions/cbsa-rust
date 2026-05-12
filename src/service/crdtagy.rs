use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::time::{sleep, Duration};

use crate::error::CbsaError;

const MIN_DELAY_SECONDS: u64 = 1;
const MAX_DELAY_SECONDS_EXCLUSIVE: u64 = 3;
const MAX_CREDIT_SCORE_EXCLUSIVE: u16 = 999;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditAgencyRequest {
    pub name: String,
    pub address: String,
    pub date_of_birth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditAgency {
    One = 1,
    Two = 2,
    Three = 3,
    Four = 4,
    Five = 5,
}

impl CreditAgency {
    pub const ALL: [Self; 5] = [Self::One, Self::Two, Self::Three, Self::Four, Self::Five];

    pub fn number(self) -> u8 {
        self as u8
    }

    pub fn program_name(self) -> &'static str {
        match self {
            Self::One => "CRDTAGY1",
            Self::Two => "CRDTAGY2",
            Self::Three => "CRDTAGY3",
            Self::Four => "CRDTAGY4",
            Self::Five => "CRDTAGY5",
        }
    }

    pub fn container_name(self) -> &'static str {
        match self {
            Self::One => "CIPA",
            Self::Two => "CIPB",
            Self::Three => "CIPC",
            Self::Four => "CIPD",
            Self::Five => "CIPE",
        }
    }
}

impl TryFrom<u8> for CreditAgency {
    type Error = CbsaError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::One),
            2 => Ok(Self::Two),
            3 => Ok(Self::Three),
            4 => Ok(Self::Four),
            5 => Ok(Self::Five),
            _ => Err(CbsaError::validation(
                "agency number must be between 1 and 5",
            )),
        }
    }
}

#[async_trait]
pub trait CreditAgencyClient: Send + Sync {
    async fn request_score(
        &self,
        agency: CreditAgency,
        request: CreditAgencyRequest,
    ) -> Result<u16, CbsaError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCreditAgencyClient;

#[async_trait]
impl CreditAgencyClient for SystemCreditAgencyClient {
    async fn request_score(
        &self,
        agency: CreditAgency,
        request: CreditAgencyRequest,
    ) -> Result<u16, CbsaError> {
        request_score(agency, request).await
    }
}

pub async fn request_score_by_number(
    agency_number: u8,
    request: CreditAgencyRequest,
) -> Result<u16, CbsaError> {
    let agency = CreditAgency::try_from(agency_number)?;
    request_score(agency, request).await
}

pub async fn request_score(
    agency: CreditAgency,
    request: CreditAgencyRequest,
) -> Result<u16, CbsaError> {
    validate_request(&request)?;
    let seed = runtime_seed(agency);
    request_score_with_seed(agency, request, seed).await
}

pub async fn request_score_with_seed(
    agency: CreditAgency,
    request: CreditAgencyRequest,
    seed: u64,
) -> Result<u16, CbsaError> {
    let outcome = seeded_outcome(agency, &request, seed)?;
    sleep(outcome.delay).await;
    Ok(outcome.score)
}

fn validate_request(request: &CreditAgencyRequest) -> Result<(), CbsaError> {
    if request.name.trim().is_empty() {
        return Err(CbsaError::validation("name must not be blank"));
    }
    if request.name.chars().count() > 60 {
        return Err(CbsaError::validation(
            "name must be at most 60 characters long",
        ));
    }
    if request.address.chars().count() > 160 {
        return Err(CbsaError::validation(
            "address must be at most 160 characters long",
        ));
    }
    if request.date_of_birth > 99_999_999 {
        return Err(CbsaError::validation(
            "date_of_birth must be between 0 and 99999999",
        ));
    }
    Ok(())
}

fn runtime_seed(agency: CreditAgency) -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_secs() ^ u64::from(duration.subsec_nanos()) ^ u64::from(agency.number())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeededOutcome {
    delay: Duration,
    score: u16,
}

fn seeded_outcome(
    agency: CreditAgency,
    request: &CreditAgencyRequest,
    seed: u64,
) -> Result<SeededOutcome, CbsaError> {
    validate_request(request)?;
    let mut rng = Lcg64::new(seed ^ u64::from(agency.number()));
    Ok(SeededOutcome {
        delay: Duration::from_secs(
            (rng.next_u64() % (MAX_DELAY_SECONDS_EXCLUSIVE - MIN_DELAY_SECONDS))
                + MIN_DELAY_SECONDS,
        ),
        score: ((rng.next_u64() % u64::from(MAX_CREDIT_SCORE_EXCLUSIVE - 1)) + 1) as u16,
    })
}

#[derive(Debug, Clone, Copy)]
struct Lcg64 {
    state: u64,
}

impl Lcg64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> CreditAgencyRequest {
        CreditAgencyRequest {
            name: "Dr Alice Example".to_string(),
            address: "1 Main Street".to_string(),
            date_of_birth: 10_012_000,
        }
    }

    async fn assert_seeded_agency_behavior(agency: CreditAgency, seed: u64) {
        let request = sample_request();
        let expected = seeded_outcome(agency, &request, seed).expect("seeded outcome must exist");
        let handle = tokio::spawn(request_score_with_seed(agency, request, seed));

        tokio::task::yield_now().await;

        let just_before = expected
            .delay
            .checked_sub(Duration::from_millis(1))
            .unwrap_or_default();
        if !just_before.is_zero() {
            tokio::time::advance(just_before).await;
            assert!(!handle.is_finished());
        }

        tokio::time::advance(expected.delay - just_before).await;
        let score = handle
            .await
            .expect("task must complete")
            .expect("request must succeed");

        assert_eq!(score, expected.score);
        assert!((1..=998).contains(&score));
    }

    #[tokio::test(start_paused = true)]
    async fn crdtagy1_returns_seeded_score_in_range_after_seeded_delay() {
        assert_seeded_agency_behavior(CreditAgency::One, 101).await;
    }

    #[tokio::test(start_paused = true)]
    async fn crdtagy2_returns_seeded_score_in_range_after_seeded_delay() {
        assert_seeded_agency_behavior(CreditAgency::Two, 202).await;
    }

    #[tokio::test(start_paused = true)]
    async fn crdtagy3_returns_seeded_score_in_range_after_seeded_delay() {
        assert_seeded_agency_behavior(CreditAgency::Three, 303).await;
    }

    #[tokio::test(start_paused = true)]
    async fn crdtagy4_returns_seeded_score_in_range_after_seeded_delay() {
        assert_seeded_agency_behavior(CreditAgency::Four, 404).await;
    }

    #[tokio::test(start_paused = true)]
    async fn crdtagy5_returns_seeded_score_in_range_after_seeded_delay() {
        assert_seeded_agency_behavior(CreditAgency::Five, 505).await;
    }

    #[test]
    fn rejects_invalid_agency_numbers() {
        let err = CreditAgency::try_from(6).expect_err("agency 6 must fail");
        match err {
            CbsaError::Validation(message) => {
                assert_eq!(message, "agency number must be between 1 and 5")
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }
}
