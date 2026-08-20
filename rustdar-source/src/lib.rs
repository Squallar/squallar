//! Source-substrate vocabulary: origins, TLS, features, wire primitives, the
//! job envelope + codec vocabulary, and — since WO-M9 — the
//! [`SourceHandler`](handler::SourceHandler) contract every layer implements
//! together with the vocabulary its methods speak (controls, drawing
//! primitives, fetch policy). Contract/vocabulary only — no per-source code;
//! geodesy lives one floor lower, in `rustdar-geo` (the WO-G1-era `geo`
//! re-export module died at WO-G4). Sits below `rustdar-radar` AND
//! `rustdar-overlays`; ceiling + graph shape pinned by `tests/charter.rs`.

pub mod controls;
pub mod draw;
pub mod feature;
pub mod fetch_policy;
pub mod handler;
pub mod id;
pub mod job;
pub mod origins;
pub mod tls;
pub mod wire;
