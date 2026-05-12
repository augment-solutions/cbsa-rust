use axum::{
    extract::{rejection::JsonRejection, Path},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Serialize;
use validator::Validate;

use crate::{
    error::CbsaError,
    service::crdtagy::{self, CreditAgencyRequest},
    web::{
        crecust::{CrecustCommareaRequestDto, CrecustKeyDto, CrecustRequestDto},
        AppState,
    },
};

const CREDIT_AGENCY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub fn router() -> Router<AppState> {
    Router::<AppState>::new().route("/api/v1/crdtagy/:agency_number", post(process))
}

async fn process(
    Path(agency_number): Path<u8>,
    payload: Result<Json<CrecustRequestDto>, JsonRejection>,
) -> Result<Response, CbsaError> {
    let Json(payload) = payload.map_err(|err| CbsaError::validation(err.body_text()))?;
    payload
        .validate()
        .map_err(|err| CbsaError::validation(err.to_string()))?;

    let commarea = payload
        .crecust
        .ok_or_else(|| CbsaError::validation("CreCust is required"))?;
    let request = CreditAgencyRequest {
        name: commarea
            .comm_name
            .clone()
            .ok_or_else(|| CbsaError::validation("CommName is required"))?,
        address: commarea
            .comm_address
            .clone()
            .ok_or_else(|| CbsaError::validation("CommAddress is required"))?,
        date_of_birth: u32::try_from(
            commarea
                .comm_date_of_birth
                .ok_or_else(|| CbsaError::validation("CommDateOfBirth is required"))?,
        )
        .map_err(|_| CbsaError::validation("CommDateOfBirth must be between 0 and 99999999"))?,
    };

    let credit_score = tokio::time::timeout(
        CREDIT_AGENCY_TIMEOUT,
        crdtagy::request_score_by_number(agency_number, request),
    )
    .await
    .map_err(|_| CbsaError::abend("PLOP", "Credit agency processing timed out."))??;

    Ok(Json(CrdtagyResponseDto::success(commarea, credit_score)).into_response())
}

#[derive(Debug, Serialize)]
struct CrdtagyResponseDto {
    #[serde(rename = "CreCust")]
    crecust: CrdtagyCommareaResponseDto,
}

#[derive(Debug, Serialize)]
struct CrdtagyCommareaResponseDto {
    #[serde(rename = "CommEyecatcher")]
    comm_eyecatcher: String,
    #[serde(rename = "CommKey")]
    comm_key: CrdtagyKeyResponseDto,
    #[serde(rename = "CommName")]
    comm_name: String,
    #[serde(rename = "CommAddress")]
    comm_address: String,
    #[serde(rename = "CommDateOfBirth")]
    comm_date_of_birth: u32,
    #[serde(rename = "CommCreditScore")]
    comm_credit_score: u16,
    #[serde(rename = "CommCsReviewDate")]
    comm_cs_review_date: u32,
    #[serde(rename = "CommSuccess")]
    comm_success: &'static str,
    #[serde(rename = "CommFailCode")]
    comm_fail_code: &'static str,
}

#[derive(Debug, Serialize)]
struct CrdtagyKeyResponseDto {
    #[serde(rename = "CommSortcode")]
    comm_sortcode: String,
    #[serde(rename = "CommNumber")]
    comm_number: i64,
}

impl CrdtagyResponseDto {
    fn success(commarea: CrecustCommareaRequestDto, credit_score: u16) -> Self {
        let key = commarea.comm_key.unwrap_or(CrecustKeyDto {
            comm_sortcode: Some(String::new()),
            comm_number: Some(0),
        });
        Self {
            crecust: CrdtagyCommareaResponseDto {
                comm_eyecatcher: commarea.comm_eyecatcher.unwrap_or_default(),
                comm_key: CrdtagyKeyResponseDto {
                    comm_sortcode: key.comm_sortcode.unwrap_or_default(),
                    comm_number: key.comm_number.unwrap_or_default(),
                },
                comm_name: commarea.comm_name.unwrap_or_default(),
                comm_address: commarea.comm_address.unwrap_or_default(),
                comm_date_of_birth: commarea
                    .comm_date_of_birth
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
                comm_credit_score: credit_score,
                comm_cs_review_date: commarea
                    .comm_cs_review_date
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
                comm_success: "Y",
                comm_fail_code: "0",
            },
        }
    }
}
