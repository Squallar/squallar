//! Shim: the fetch/retry policy lives in `squallar_source::fetch_policy`,
//! re-exported wholesale so the shim cannot drift from what it shims.
pub use squallar_source::fetch_policy::*;
