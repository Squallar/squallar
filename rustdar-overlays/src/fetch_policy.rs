//! Shim: the fetch/retry policy lives in `rustdar_source::fetch_policy`,
//! re-exported wholesale so the shim cannot drift from what it shims.
pub use rustdar_source::fetch_policy::*;
