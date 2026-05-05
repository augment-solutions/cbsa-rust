//! CBSA migration target.
//!
//! Public modules are populated as COBOL programs are migrated. The bootstrap
//! commit only wires up cross-cutting infrastructure: configuration loading,
//! database pool, error mapping, and the axum router. Each program PR adds
//! one submodule under `service` / `repository` / `web`.

pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod repository;
pub mod service;
pub mod web;
