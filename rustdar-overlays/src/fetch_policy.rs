//! Shim: the fetch/retry policy moved to `rustdar_source::fetch_policy` at
//! WO-M9, with the `SourceHandler` trait whose `retry()` ledger it is.
//!
//! It is re-exported wholesale rather than re-listed so the shim cannot drift
//! from what it shims: a name added down there is published up here the same
//! day, and there is no list to forget to extend. `rustdar-egui` and
//! `rustdar-app` name this module in ~10 places and none of them moved.
pub use rustdar_source::fetch_policy::*;
