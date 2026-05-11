//! Persistence layer. Each program PR adds one submodule here owning the
//! sqlx queries against its aggregate (customer, account, proctran, control).
//! Empty in the bootstrap commit.

pub mod creacc;
pub mod crecust;
pub mod delcus;
pub mod inqacc;
pub mod inqacccu;
pub mod inqcust;
pub mod updacc;
pub mod updcust;
