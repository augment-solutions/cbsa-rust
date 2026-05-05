//! Application configuration. Bound from `application.toml` and overlaid with
//! `CBSA_*` environment variables. The sortcode is kept as a String end-to-end
//! and validated `^[0-9]{6}$` at startup so malformed values fail fast.

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub server: Server,
    pub database: Database,
    pub cbsa: Cbsa,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Server {
    pub bind: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Database {
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    10
}

#[derive(Debug, Deserialize, Clone)]
pub struct Cbsa {
    pub sortcode: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration: {0}")]
    Figment(Box<figment::Error>),
    #[error("invalid sortcode {0:?}: must be exactly six ASCII digits")]
    InvalidSortcode(String),
}

impl From<figment::Error> for ConfigError {
    fn from(err: figment::Error) -> Self {
        Self::Figment(Box::new(err))
    }
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let cfg: AppConfig = Figment::new()
            .merge(Toml::file("application.toml"))
            .merge(Env::prefixed("CBSA_").split("__"))
            .extract()?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_six_ascii_digits(&self.cbsa.sortcode) {
            return Err(ConfigError::InvalidSortcode(self.cbsa.sortcode.clone()));
        }
        Ok(())
    }
}

fn is_six_ascii_digits(s: &str) -> bool {
    s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_ascii_digits_accepts_padded() {
        assert!(is_six_ascii_digits("987654"));
        assert!(is_six_ascii_digits("000001"));
    }

    #[test]
    fn six_ascii_digits_rejects_short_or_non_ascii() {
        assert!(!is_six_ascii_digits("12345"));
        assert!(!is_six_ascii_digits("1234567"));
        assert!(!is_six_ascii_digits("12345a"));
        assert!(!is_six_ascii_digits(
            "\u{ff10}\u{ff11}\u{ff12}\u{ff13}\u{ff14}\u{ff15}"
        ));
    }
}
