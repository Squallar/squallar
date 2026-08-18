//! Source-substrate vocabulary: origins, TLS, geodesy, features, wire
//! primitives, and the job envelope + codec vocabulary. Contract/vocabulary
//! only — no per-source code. Sits below `rustdar-radar` AND
//! `rustdar-overlays`; ceiling + graph shape pinned by `tests/charter.rs`.

pub mod feature;
pub mod geo;
pub mod job;
pub mod origins;
pub mod tls;
pub mod wire;
