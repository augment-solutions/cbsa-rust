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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_tran_type_as_str() {
        assert_eq!(ProcTranType::ChequeAcctOpened.as_str(), "CHA");
        assert_eq!(ProcTranType::ChequePaidIn.as_str(), "CHI");
        assert_eq!(ProcTranType::ChequePaidOut.as_str(), "CHO");
        assert_eq!(ProcTranType::Credit.as_str(), "CRE");
        assert_eq!(ProcTranType::Debit.as_str(), "DEB");
        assert_eq!(ProcTranType::BranchCreate.as_str(), "ICL");
        assert_eq!(ProcTranType::CustomerCreate.as_str(), "OCC");
        assert_eq!(ProcTranType::AccountCreate.as_str(), "OCA");
        assert_eq!(ProcTranType::CustomerDelete.as_str(), "OCD");
        assert_eq!(ProcTranType::AccountDelete.as_str(), "ODA");
        assert_eq!(ProcTranType::Transfer.as_str(), "TFR");
    }

    #[test]
    fn proc_tran_type_serde_roundtrip() {
        let types = vec![
            ProcTranType::ChequeAcctOpened,
            ProcTranType::Credit,
            ProcTranType::Transfer,
        ];

        for ty in types {
            let json = serde_json::to_string(&ty).unwrap();
            let roundtrip: ProcTranType = serde_json::from_str(&json).unwrap();
            assert_eq!(roundtrip, ty);
        }
    }

    #[test]
    fn proc_tran_type_deserializes_from_cobol_value() {
        let json = r#""CRE""#;
        let ty: ProcTranType = serde_json::from_str(json).unwrap();
        assert_eq!(ty, ProcTranType::Credit);

        let json = r#""TFR""#;
        let ty: ProcTranType = serde_json::from_str(json).unwrap();
        assert_eq!(ty, ProcTranType::Transfer);
    }
}
