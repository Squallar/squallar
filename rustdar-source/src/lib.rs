//! Source-substrate vocabulary: origins, TLS, features, wire primitives, and
//! the job envelope + codec vocabulary. Contract/vocabulary only — no
//! per-source code; geodesy lives one floor lower, in `rustdar-geo` (the
//! WO-G1-era `geo` re-export module died at WO-G4). Sits below
//! `rustdar-radar` AND `rustdar-overlays`; ceiling + graph shape pinned by
//! `tests/charter.rs`.

pub mod feature;
pub mod id;
pub mod job;
pub mod origins;
pub mod tls;
pub mod wire;
