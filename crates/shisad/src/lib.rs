//! shisad internals. The binary is a thin wrapper; the integration tests use
//! these modules directly so the bus side can be exercised without root.

pub mod activity;
pub mod bus;
mod clock;
pub mod diag;
pub mod events;
pub mod gate;
pub mod load;
pub mod logind;
pub mod notify;
pub mod policy;
pub mod polkit;
pub mod procfs;
