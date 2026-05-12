//! Domain value types shared across programs.
//!
//! Per-program domain records (e.g. `CustomerDetails`, `AccountDetails`) are
//! added by their owning migration PR. This file is intentionally empty in
//! the bootstrap commit beyond the COBOL transaction-type enum, which is
//! shared between every program that writes a `PROCTRAN` row.

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::config::is_six_ascii_digits;

const MIN_CUSTOMER_NUMBER: i64 = 0;
const MAX_CUSTOMER_NUMBER: i64 = 9_999_999_999;
const CUSTOMER_NUMBER_RANGE_MESSAGE: &str = "customer_number must be between 0 and 9999999999";
const MAX_CUSTOMER_NAME_CHARS: usize = 60;
const MAX_CUSTOMER_ADDRESS_CHARS: usize = 160;
const MIN_ACCOUNT_NUMBER: i64 = 0;
const MAX_ACCOUNT_NUMBER: i64 = 99_999_999;
const ACCOUNT_NUMBER_RANGE_MESSAGE: &str = "account_number must be between 0 and 99999999";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerProfile {
    name: String,
    address: String,
}

impl CustomerProfile {
    pub fn new(name: String, address: String) -> Result<Self, String> {
        if name.trim().is_empty() {
            return Err("name must not be blank".to_string());
        }

        if name.chars().count() > MAX_CUSTOMER_NAME_CHARS {
            return Err(format!(
                "name must be at most {MAX_CUSTOMER_NAME_CHARS} characters"
            ));
        }

        if address.chars().count() > MAX_CUSTOMER_ADDRESS_CHARS {
            return Err(format!(
                "address must be at most {MAX_CUSTOMER_ADDRESS_CHARS} characters"
            ));
        }

        Ok(Self { name, address })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

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
    #[serde(rename = "OUA")]
    AccountUpdate,
    #[serde(rename = "OUC")]
    CustomerUpdate,
    #[serde(rename = "ODC")]
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
            Self::AccountUpdate => "OUA",
            Self::CustomerUpdate => "OUC",
            Self::CustomerDelete => "ODC",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDetails {
    sortcode: String,
    customer_number: i64,
    account_number: i64,
    account_type: String,
    interest_rate: Decimal,
    opened: NaiveDate,
    overdraft_limit: Decimal,
    last_statement_date: Option<NaiveDate>,
    next_statement_date: Option<NaiveDate>,
    available_balance: Decimal,
    actual_balance: Decimal,
}

impl AccountDetails {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sortcode: String,
        customer_number: i64,
        account_number: i64,
        account_type: String,
        interest_rate: Decimal,
        opened: NaiveDate,
        overdraft_limit: Decimal,
        last_statement_date: Option<NaiveDate>,
        next_statement_date: Option<NaiveDate>,
        available_balance: Decimal,
        actual_balance: Decimal,
    ) -> Result<Self, String> {
        if !is_six_ascii_digits(&sortcode) {
            return Err("sortcode must be exactly 6 ASCII digits".to_string());
        }

        if !(MIN_CUSTOMER_NUMBER..=MAX_CUSTOMER_NUMBER).contains(&customer_number) {
            return Err(CUSTOMER_NUMBER_RANGE_MESSAGE.to_string());
        }

        if !(MIN_ACCOUNT_NUMBER..=MAX_ACCOUNT_NUMBER).contains(&account_number) {
            return Err(ACCOUNT_NUMBER_RANGE_MESSAGE.to_string());
        }

        if account_type.chars().count() > 8 {
            return Err("account_type must be at most 8 characters".to_string());
        }

        Ok(Self {
            sortcode,
            customer_number,
            account_number,
            account_type,
            interest_rate: round_money(interest_rate),
            opened,
            overdraft_limit: round_money(overdraft_limit),
            last_statement_date,
            next_statement_date,
            available_balance: round_money(available_balance),
            actual_balance: round_money(actual_balance),
        })
    }

    pub fn sortcode(&self) -> &str {
        &self.sortcode
    }

    pub fn customer_number(&self) -> i64 {
        self.customer_number
    }

    pub fn account_number(&self) -> i64 {
        self.account_number
    }

    pub fn account_type(&self) -> &str {
        &self.account_type
    }

    pub fn interest_rate(&self) -> Decimal {
        self.interest_rate
    }

    pub fn opened(&self) -> NaiveDate {
        self.opened
    }

    pub fn overdraft_limit(&self) -> Decimal {
        self.overdraft_limit
    }

    pub fn last_statement_date(&self) -> Option<NaiveDate> {
        self.last_statement_date
    }

    pub fn next_statement_date(&self) -> Option<NaiveDate> {
        self.next_statement_date
    }

    pub fn available_balance(&self) -> Decimal {
        self.available_balance
    }

    pub fn actual_balance(&self) -> Decimal {
        self.actual_balance
    }
}

fn round_money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
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

    #[test]
    fn customer_profile_accepts_character_count_limits() {
        let name = "é".repeat(MAX_CUSTOMER_NAME_CHARS);
        let address = "ø".repeat(MAX_CUSTOMER_ADDRESS_CHARS);

        let profile =
            CustomerProfile::new(name.clone(), address.clone()).expect("profile must be valid");

        assert_eq!(profile.name(), name);
        assert_eq!(profile.address(), address);
    }

    #[test]
    fn customer_profile_rejects_blank_name() {
        assert_eq!(
            CustomerProfile::new("   ".to_string(), "1 Main Street".to_string()).unwrap_err(),
            "name must not be blank"
        );
    }

    #[test]
    fn customer_profile_rejects_name_longer_than_copybook_limit() {
        assert_eq!(
            CustomerProfile::new(
                "é".repeat(MAX_CUSTOMER_NAME_CHARS + 1),
                "1 Main Street".to_string()
            )
            .unwrap_err(),
            format!("name must be at most {MAX_CUSTOMER_NAME_CHARS} characters")
        );
    }

    fn account(account_number: i64) -> Result<AccountDetails, String> {
        AccountDetails::new(
            "012345".to_string(),
            42,
            account_number,
            "ISA".to_string(),
            Decimal::new(125, 2),
            NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            Decimal::new(250, 0),
            Some(NaiveDate::from_ymd_opt(2024, 2, 3).expect("valid date")),
            Some(NaiveDate::from_ymd_opt(2024, 3, 4).expect("valid date")),
            Decimal::new(150_025, 2),
            Decimal::new(149_975, 2),
        )
    }

    #[test]
    fn account_details_accepts_copybook_account_number_bounds() {
        for account_number in [MIN_ACCOUNT_NUMBER, MAX_ACCOUNT_NUMBER] {
            assert_eq!(
                account(account_number).unwrap().account_number(),
                account_number
            );
        }
    }

    #[test]
    fn account_details_rejects_out_of_range_account_number() {
        assert_eq!(
            account(MAX_ACCOUNT_NUMBER + 1).unwrap_err(),
            ACCOUNT_NUMBER_RANGE_MESSAGE
        );
    }

    #[test]
    fn account_details_accepts_eight_non_ascii_account_type_characters() {
        let account = AccountDetails::new(
            "012345".to_string(),
            42,
            12_345_678,
            "éééééééé".to_string(),
            Decimal::new(125, 2),
            NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            Decimal::new(250, 0),
            None,
            None,
            Decimal::new(150_025, 2),
            Decimal::new(149_975, 2),
        )
        .expect("eight characters should be accepted regardless of byte width");

        assert_eq!(account.account_type(), "éééééééé");
    }

    #[test]
    fn account_details_rounds_money_fields_with_bankers_rounding() {
        let account = AccountDetails::new(
            "012345".to_string(),
            42,
            12_345_678,
            "SAVINGS".to_string(),
            Decimal::new(125, 3),
            NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date"),
            Decimal::new(250_005, 3),
            None,
            None,
            Decimal::new(150_025, 2),
            Decimal::new(123_445, 3),
        )
        .expect("account must be valid");

        assert_eq!(account.interest_rate(), Decimal::new(12, 2));
        assert_eq!(account.overdraft_limit(), Decimal::new(250, 0));
        assert_eq!(account.actual_balance(), Decimal::new(12_344, 2));
    }
}
