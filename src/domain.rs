//! Domain value types shared across programs.
//!
//! Per-program domain records (e.g. `CustomerDetails`, `AccountDetails`) are
//! added by their owning migration PR. This file is intentionally empty in
//! the bootstrap commit beyond the COBOL transaction-type enum, which is
//! shared between every program that writes a `PROCTRAN` row.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::config::is_six_ascii_digits;

const MIN_CUSTOMER_NUMBER: i64 = 0;
const MAX_CUSTOMER_NUMBER: i64 = 9_999_999_999;
const CUSTOMER_NUMBER_RANGE_MESSAGE: &str = "customer_number must be between 0 and 9999999999";

/// `PROC-TRAN-TYPE` (PIC X(3)) values used in `proctran.tran_type`. Every
/// program that writes a PROCTRAN row picks one of these. The string form
/// matches the COBOL `88 PROC-TY-*` level-88 values exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcTranType {
    #[serde(rename = "CHA")]
    ChequeAcctOpened,
    #[serde(rename = "CHI")]
    ChequePaidIn,
    #[serde(rename = "CHO")]
    ChequePaidOut,
    #[serde(rename = "CRE")]
    Credit,
    #[serde(rename = "DEB")]
    Debit,
    #[serde(rename = "ICL")]
    BranchCreate,
    #[serde(rename = "OCC")]
    CustomerCreate,
    #[serde(rename = "OCA")]
    AccountCreate,
    #[serde(rename = "OCD")]
    CustomerDelete,
    #[serde(rename = "ODA")]
    AccountDelete,
    #[serde(rename = "TFR")]
    Transfer,
}

impl ProcTranType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ChequeAcctOpened => "CHA",
            Self::ChequePaidIn => "CHI",
            Self::ChequePaidOut => "CHO",
            Self::Credit => "CRE",
            Self::Debit => "DEB",
            Self::BranchCreate => "ICL",
            Self::CustomerCreate => "OCC",
            Self::AccountCreate => "OCA",
            Self::CustomerDelete => "OCD",
            Self::AccountDelete => "ODA",
            Self::Transfer => "TFR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerDetails {
    sortcode: String,
    customer_number: i64,
    name: String,
    address: String,
    date_of_birth: NaiveDate,
    credit_score: u16,
    credit_score_review_date: Option<NaiveDate>,
}

impl CustomerDetails {
    pub fn new(
        sortcode: String,
        customer_number: i64,
        name: String,
        address: String,
        date_of_birth: NaiveDate,
        credit_score: u16,
        credit_score_review_date: Option<NaiveDate>,
    ) -> Result<Self, String> {
        if !is_six_ascii_digits(&sortcode) {
            return Err("sortcode must be exactly 6 ASCII digits".to_string());
        }

        if !(MIN_CUSTOMER_NUMBER..=MAX_CUSTOMER_NUMBER).contains(&customer_number) {
            return Err(CUSTOMER_NUMBER_RANGE_MESSAGE.to_string());
        }

        if credit_score > 999 {
            return Err("credit_score must be between 0 and 999".to_string());
        }

        Ok(Self {
            sortcode,
            customer_number,
            name,
            address,
            date_of_birth,
            credit_score,
            credit_score_review_date,
        })
    }

    pub fn sortcode(&self) -> &str {
        &self.sortcode
    }

    pub fn customer_number(&self) -> i64 {
        self.customer_number
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn date_of_birth(&self) -> NaiveDate {
        self.date_of_birth
    }

    pub fn credit_score(&self) -> u16 {
        self.credit_score
    }

    pub fn credit_score_review_date(&self) -> Option<NaiveDate> {
        self.credit_score_review_date
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn customer(customer_number: i64) -> Result<CustomerDetails, String> {
        CustomerDetails::new(
            "012345".to_string(),
            customer_number,
            "Jane Doe".to_string(),
            "1 Main Street".to_string(),
            NaiveDate::from_ymd_opt(1990, 1, 2).expect("valid date"),
            450,
            Some(NaiveDate::from_ymd_opt(2025, 3, 4).expect("valid date")),
        )
    }

    #[test]
    fn customer_details_accepts_copybook_customer_number_bounds() {
        for customer_number in [MIN_CUSTOMER_NUMBER, MAX_CUSTOMER_NUMBER] {
            assert_eq!(
                customer(customer_number).unwrap().customer_number(),
                customer_number
            );
        }
    }

    #[test]
    fn customer_details_rejects_out_of_range_customer_number() {
        assert_eq!(
            customer(MAX_CUSTOMER_NUMBER + 1).unwrap_err(),
            CUSTOMER_NUMBER_RANGE_MESSAGE
        );
    }
}
