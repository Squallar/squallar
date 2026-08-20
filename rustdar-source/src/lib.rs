//! Source-substrate vocabulary: origins, TLS, features, wire primitives, the
//! job envelope + codec vocabulary, and the
//! [`SourceHandler`](handler::SourceHandler) contract every layer implements,
//! with the vocabulary its methods speak. No per-source code; geodesy lives one
//! floor lower, in `rustdar-geo`. Ceiling + graph shape pinned by
//! `tests/charter.rs`.
//! AND `rustdar-overlays`; ceiling + graph shape pinned by `tests/charter.rs`.

pub mod controls;
pub mod draw;
pub mod feature;
pub mod fetch_policy;
pub mod handler;
pub mod id;
pub mod job;
pub mod liveness;
pub mod origins;
pub mod product;
pub mod time;
pub mod tls;
pub mod wire;
