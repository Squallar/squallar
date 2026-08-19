//! The composed job-codec registry: the one frontend module that names the
//! source crates' registries.
//!
//! Since WO-M7.2 the REQUEST direction of [`crate::offload`] is
//! source-type-free — the funnel frames, routes and runs jobs entirely
//! through [`rustdar_source::job::JobCodec`] rows and never names a kind's
//! input type or its crate. Somebody still has to say *which* rows this
//! build composes, and that statement is this module: the radar six and the
//! overlay seven, each half published beside the pipeline it runs
//! (`rustdar_radar::jobs`, `rustdar_overlays::render::jobs`).
//!
//! **ONE explicit expression, radar first, deliberately**: WO-M7b's dense
//! code flip assigns wire codes by index into exactly this composition
//! (codes 1..=13, `radar` = 1 … `overlay/model` = 13), so the composition
//! must stay a single spelled-out expression rather than something assembled
//! in pieces. Until that flip the wire codes are `offload`'s frontend-owned
//! sparse `LegacyCode` map, pinned literal-by-literal in `offload::tests`.

/// Every codec row this build composes, in the load-bearing order: the six
/// radar rows (**radar, level3, level3/vild, section, voxels, decode**) and
/// then the seven overlay rows (**sites, alerts, outlooks, discussions,
/// reports, glm, model**).
///
/// An iterator over the two statics rather than a materialised slice:
/// [`rustdar_source::job::JobCodec`] is a row of function pointers built by
/// `const` constructors and deliberately not `Clone`, so the one way to
/// compose the halves without restating any row is to chain them — which is
/// also the one-expression shape WO-M7b's index assignment depends on.
pub(crate) fn job_codecs() -> impl Iterator<Item = &'static rustdar_source::job::JobCodec> {
    rustdar_radar::jobs::JOB_CODECS
        .iter()
        .chain(rustdar_overlays::render::jobs::JOB_CODECS.iter())
}
