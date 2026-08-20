//! Shim: the abstract per-frame drawing primitives moved to
//! `rustdar_source::draw` at WO-M9, with the `SourceHandler` trait whose
//! `per_frame_points()`/`draw_point()`/`hover_text()` speak them.
//!
//! Glob-re-exported for the reason the sibling `fetch_policy` shim gives.
pub use rustdar_source::draw::*;
