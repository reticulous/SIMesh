//! Schema and traits shared by every planner crate.
//!
//! The load-bearing decisions encoded here (review §8.1/§8.3):
//! terrain and clutter heights are SEPARATE layers everywhere (a DSM is a
//! view, never a stored input); propagation models are pure profile→loss
//! functions behind [`model::PathLossModel`]; scenarios are cell-first (a
//! cell = one flood/airtime domain with one radio config).

pub mod entry_loss;
pub mod geo;
pub mod hex;
pub mod model;
pub mod preset;
pub mod profile;
pub mod scenario;
