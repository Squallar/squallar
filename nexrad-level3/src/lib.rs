//! # nexrad-level3
//!
//! Decoder for NEXRAD Level III products (ICD 2620001) — the Radar Product
//! Generator's derived outputs, not the raw base moments of Level II.
//!
//! Byte slices in, model types out: no network, no filesystem, no rendering.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![warn(clippy::correctness)]
#![allow(clippy::too_many_arguments)]
#![deny(missing_docs)]

pub mod decode;
pub mod model;
pub mod result;
