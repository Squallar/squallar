#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! squallar-worker is the job engine: the offload funnel, the wire identity,
//! the composed registry, the native pool. Job vocabulary lives in
//! squallar-source; codec rows live beside their pipelines in
//! squallar-radar/squallar-overlays/squallar-elevation; this crate composes and
//! runs them.

/// The composed job-codec registry — the one module that names the source
/// crates' `JOB_CODECS`.
pub(crate) mod job_registry;
pub mod offload;
pub mod wire_identity;
