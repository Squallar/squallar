//! The composed job-codec registry: the one module in this crate that names
//! the source crates' registries.

/// Every codec row this build composes, in order: the six radar rows
/// (radar, level3, level3/vild, section, voxels, decode) then the seven
/// overlay rows (sites, alerts, outlooks, discussions, reports, glm, model).
/// Wire codes are indices into this composition plus one (1..=13, 0
/// unallocated so a zeroed buffer never decodes), so it must stay a single
/// spelled-out expression.
pub(crate) fn job_codecs() -> impl Iterator<Item = &'static squallar_source::job::JobCodec> {
    squallar_radar::jobs::JOB_CODECS
        .iter()
        .chain(squallar_overlays::render::jobs::JOB_CODECS.iter())
}
