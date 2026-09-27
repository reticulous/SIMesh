//! Propagation models. Every model is a pure profile→loss function behind
//! `planner_core::model::PathLossModel`; presets decide which one runs.
//!
//! P0 headline: `p1812` (ITU-R P.1812-8, implemented from the Recommendation
//! text — Apache-2.0-clean; validated black-box against eeveetza/Py1812, see
//! `tests/oracle/README.md`). `free_space` is the exact reference model used
//! for wiring, sanity floors, and tests. The optional ITM cross-check adapter
//! (vendored MIT port) comes only when community-tool comparisons are wanted.

pub mod free_space;
pub mod p1812;
pub mod near_field;
pub mod p2108;

pub use free_space::FreeSpace;
pub use p1812::P1812;
