#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! rustdar-worker is the job engine: the offload funnel, the wire identity,
//! the composed registry, the native pool. Job vocabulary lives in
//! rustdar-source; codec rows live beside their pipelines in
//! rustdar-radar/rustdar-overlays; this crate composes and runs them.

/// The composed job-codec registry — the one module that names the source
/// crates' `JOB_CODECS`.
pub(crate) mod job_registry;
pub mod offload;
pub mod wire_identity;
