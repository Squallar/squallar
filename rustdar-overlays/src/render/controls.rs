//! Shim: the declarative control descriptors moved to
//! `rustdar_source::controls` at WO-M9, with the `SourceHandler` trait whose
//! `controls()`/`apply_control()` speak them.
//!
//! Glob-re-exported for the reason the sibling `fetch_policy` shim gives: a
//! list here would be a second thing to keep in step.
pub use rustdar_source::controls::*;
