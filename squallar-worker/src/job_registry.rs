//! The composed job-codec registry: the one module in this crate that names
//! the source crates' registries.

/// Every codec row this build composes, in order: the six radar rows
/// (radar, level3, level3/vild, section, voxels, decode), then the seven
/// overlay rows (sites, alerts, outlooks, discussions, reports, glm, model),
/// then the one elevation row (terrain/heights). Wire codes are indices into
/// this composition plus one (1..=14, 0 unallocated so a zeroed buffer never
/// decodes), so it must stay a single spelled-out expression.
///
/// **A new registry chains on the END.** A row inserted anywhere earlier —
/// including appended into `squallar_radar::jobs::JOB_CODECS`, which reads like
/// the natural home for a raster row — shifts every index after it, which
/// renumbers every wire code after it and moves the six radar labels
/// `squallar_radar::jobs`'s own `the_registry_is_the_six_rows_in_dispatch_order`
/// pins by value.
pub(crate) fn job_codecs() -> impl Iterator<Item = &'static squallar_source::job::JobCodec> {
    squallar_radar::jobs::JOB_CODECS
        .iter()
        .chain(squallar_overlays::render::jobs::JOB_CODECS.iter())
        .chain(squallar_elevation::jobs::JOB_CODECS.iter())
}
