//! Domain value types shared across programs.
//!
//! Per-program domain records (e.g. `CustomerDetails`, `AccountDetails`) are
//! added by their owning migration PR. This file is intentionally empty in
//! the bootstrap commit beyond the COBOL transaction-type enum, which is
//! shared between every program that writes a `PROCTRAN` row.

use serde::{Deserialize, Serialize};

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
