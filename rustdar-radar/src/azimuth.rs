//! One answer to "how far apart are this sweep's radials", for everything in
//! the crate that has to decide where a sweep stops.
//!
//! A radial is a sample, not an area, so every view of a sweep has to invent
//! the width it stands for: the sampler serves a half-step footprint either
//! side of each azimuth, the plan view paints a wedge around it, the voxel
//! builder inherits the sampler's. Those are three consumers of one
//! measurement, and the measurement is what this module holds. Derived per
//! consumer it drifted, and drifted in the way that matters: on a sparse sweep
//! the sampler reports the hole and the plan view — still reading the sweep
//! its own way, see [`MAX_ADJACENT_GAP_STEPS`] — paints across it.
//!
//! The measurement is [`median_azimuth_step_deg`] and the rule built on it is
//! [`MAX_ADJACENT_GAP_STEPS`]. Nothing here knows about radar beyond the units:
//! azimuths are degrees, and gaps run forward around a circle that closes at
//! 360° rather than ending there.

/// How many median azimuth steps two radials may be apart and still count as
/// adjacent — i.e. as a pair worth interpolating between, or a pair whose
/// wedges may meet.
///
/// One step is what consecutive radials are apart by construction, and a real
/// sweep's jitter is a few hundredths of a step, so 1.5 is bracketed from both
/// sides: it is wide enough that a jittered sweep stays one continuous ladder
/// (`sampler::tests::azimuth_jitter_does_not_open_a_hole`), and narrow enough
/// that one dropped radial — a gap of **two** steps — falls outside it and is
/// therefore *not* bridged
/// (`sampler::tests::an_azimuth_hole_is_reported_rather_than_painted_across`).
/// What happens past it is not a fallback to nearest-across-the-hole, which is
/// how a renderer shows data where the radar never looked: past it a consumer
/// serves only the azimuths inside a surviving radial's own half-step
/// footprint, and says nothing between them —
/// [`crate::sampler::SampleStatus::NoCoverage`] from the sampler, unpainted
/// raster from a rasterizer.
///
/// The step it counts is [`median_azimuth_step_deg`], and both halves of that
/// sentence are load-bearing. *Median*, because the sweeps this rule exists for
/// are exactly the ones with one enormous gap in them: a 400-radial abandoned
/// tail spanning 200° has a mean step of 0.5° only if you already ignore the
/// 160° hole, while its median step is 0.5° whether you noticed the hole or
/// not. *Shared*, because a hole in a sweep is a hole in every view of that
/// sweep, and an abandoned tail should leave the same shaped absence in the
/// plan view that it leaves in a cross-section or a voxel column.
///
/// **The plan view does not honour this yet.**
/// `render::render_radar_to_image_full` still hands every radial one global
/// half-width taken from the arithmetic mean of signed collection-order
/// differences, so each surviving radial fans across the gap to its neighbour
/// and a sparse sweep comes out as filled chords rather than as holes. That
/// divergence is why this module exists; the contract above is what it is being
/// moved onto, and this constant is the number it will be moved onto.
pub(crate) const MAX_ADJACENT_GAP_STEPS: f64 = 1.5;

/// The median circular gap between a sweep's adjacent azimuths, degrees, or
/// `None` when the sweep has no two distinct azimuths to measure between.
///
/// `azimuths` may arrive in any order and in any range — collection order
/// starts wherever the antenna was, and a sweep may be handed over as raw
/// declared angles — so they are wrapped into `[0, 360)`, sorted, and walked
/// **circularly**: the gap from the last azimuth back to the first counts as
/// one step of a complete sweep rather than as the sweep's one big hole. Gaps
/// of exactly zero are dropped, so a duplicated azimuth neither halves the
/// median nor makes a complete sweep look like it has no step at all.
///
/// The wrap into `[0, 360)` goes through an `f64` `rem_euclid` and back down to
/// `f32` before the gaps are taken, which looks gratuitous and is not: that
/// round-trip is the quantization the sampler's azimuth index has always used,
/// and the sampler's hole test is a `<=` against a threshold scaled by the
/// number returned here — a jittered sweep sits a few hundredths of a step from
/// that line, so an upgrade to end-to-end `f64` would move the answer's last
/// ulp under tests measured on the old bits, for no gain worth having.
///
/// `None` rather than a fallback: the right stand-in is the caller's to pick
/// — the sampler's 1.0° serves half a degree either side and no more, which is
/// a statement about footprints, not about this measurement.
///
/// # Two radials
///
/// With exactly two distinct azimuths there are two gaps, the short way and
/// the long way, and the "median" of a two-element list is its **upper**
/// element — so two radials 10° apart report a step of 350°, not 10°. This is
/// deliberate and inherited rather than fixed here: it is what the sampler has
/// always computed, where the effect is only that a two-radial rung serves its
/// two footprints generously, and changing it would change sampler behaviour
/// under the banner of a refactor. A consumer for which a 350° step is not
/// merely generous but absurd — a wedge width, say — caps it at the call site.
pub(crate) fn median_azimuth_step_deg(azimuths: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut sorted: Vec<f32> = azimuths
        .into_iter()
        .map(|az| az.rem_euclid(360.0) as f32)
        .collect();
    sorted.sort_by(f32::total_cmp);

    let mut gaps: Vec<f64> = Vec::with_capacity(sorted.len());
    for i in 0..sorted.len() {
        let a = f64::from(sorted[i]);
        let b = f64::from(sorted[(i + 1) % sorted.len()]);
        let gap = (b - a).rem_euclid(360.0);
        if gap > 0.0 {
            gaps.push(gap);
        }
    }
    gaps.sort_by(f64::total_cmp);
    gaps.get(gaps.len() / 2).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every gap equal: the median is that gap, and the seam closing the
    /// circle is one of them rather than the sweep's one big hole.
    #[test]
    fn a_complete_sweep_measures_its_own_spacing() {
        let half = (0..720).map(|i| f64::from(i) * 0.5);
        assert_eq!(median_azimuth_step_deg(half), Some(0.5));

        let whole = (0..360).map(f64::from);
        assert_eq!(median_azimuth_step_deg(whole), Some(1.0));
    }

    /// Collection order starts wherever the antenna was, and a sweep may
    /// declare angles outside `[0, 360)`. Neither changes the answer.
    #[test]
    fn order_and_range_do_not_change_the_measurement() {
        let rolled = (0..720).map(|i| f64::from((i + 431) % 720) * 0.5);
        assert_eq!(median_azimuth_step_deg(rolled), Some(0.5));

        let unwrapped = (0..720).map(|i| f64::from(i) * 0.5 - 360.0);
        assert_eq!(median_azimuth_step_deg(unwrapped), Some(0.5));
    }

    /// The reason this is a median and not a mean: a 200° arc of 0.5° radials
    /// with a 160° hole in it still has a step of 0.5°, so the hole stays
    /// visible as a hole instead of being averaged into everyone's footprint.
    #[test]
    fn one_enormous_hole_does_not_move_the_median() {
        let tail = (0..400).map(|i| f64::from(i) * 0.5);
        assert_eq!(median_azimuth_step_deg(tail), Some(0.5));
    }

    /// Duplicated azimuths contribute zero gaps, which are dropped: a sweep
    /// that reported some radial twice still measures its real spacing.
    #[test]
    fn duplicate_azimuths_are_not_zero_steps() {
        let doubled = (0..720).flat_map(|i| [f64::from(i) * 0.5; 2]);
        assert_eq!(median_azimuth_step_deg(doubled), Some(0.5));
    }

    /// No positive gap anywhere means nothing was measured, and the fallback
    /// belongs to whoever asked.
    #[test]
    fn nothing_to_measure_reports_nothing() {
        assert_eq!(median_azimuth_step_deg([]), None);
        assert_eq!(median_azimuth_step_deg([37.5]), None);
        assert_eq!(median_azimuth_step_deg([37.5, 37.5, 397.5]), None);
    }

    /// The documented two-radial quirk, pinned so it is changed on purpose or
    /// not at all: two gaps, and the median of two is the larger.
    #[test]
    fn two_radials_report_the_larger_circular_gap() {
        assert_eq!(median_azimuth_step_deg([0.0, 10.0]), Some(350.0));
        assert_eq!(median_azimuth_step_deg([10.0, 0.0]), Some(350.0));
    }
}
