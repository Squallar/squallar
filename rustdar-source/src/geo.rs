//! Shared horizontal geodesy, re-exported wholesale from [`rustdar_geo`] —
//! the workspace's geometry floor since WO-G1. The definitions (and the
//! load-bearing `#[inline]` on the two Mercator helpers) live there; this
//! module is the path the substrate always published them at, so every
//! `rustdar_source::geo::` spelling above keeps resolving.
//!
//! The glob is exact: `rustdar-geo`'s root is flat and its names are the ones
//! this module used to define, plus the sphere constants and great-circle
//! operations that moved down from `rustdar-radar` in the same land.

pub use rustdar_geo::*;
