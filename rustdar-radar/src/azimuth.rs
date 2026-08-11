//! One answer to "how far apart are this sweep's radials", for everything in
//! the crate that has to decide where a sweep stops.
//!
//! A radial is a sample, not an area, so every view of a sweep has to invent
//! the width it stands for: the sampler serves a half-step footprint either
//! side of each azimuth, the plan view paints a wedge around it, the voxel
//! builder inherits the sampler's. Those are three consumers of one
//! measurement, and the measurement is what this module holds. Derived per
//! consumer it drifted, and drifted in the way that matters: on a sparse sweep
//! the sampler reported the hole and the plan view painted across it.
//!
//! The measurement is [`median_azimuth_step_deg`], the rule built on it is
//! [`MAX_ADJACENT_GAP_STEPS`], and [`Rows`] is what a consumer holding a whole
//! grid asks: how far apart its rows are, and whether the last of them
//! neighbours the first. Nothing here knows about radar beyond the units:
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
/// The plan view reads it as a ceiling rather than as a bridge, because a
/// rasterizer has nothing to interpolate: `render::l2_wedge_width_deg` starts
/// from what each radial declares its own resolution to be and holds it under
/// this many median steps, so a radial that declares more sky than its
/// neighbours leave it is cut back to what the sweep supports. What it never
/// does is *widen* a radial to close a gap — the width does not depend on where
/// the next radial is at all — so the hole this constant refuses to bridge in
/// the sampler is the same hole the plan view leaves unpainted.
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
/// merely generous but absurd caps it at the call site: a wedge width does, at
/// `render::MAX_WEDGE_DEG`.
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

/// How much of the circle a grid's rows must account for, at their own measured
/// spacing, before [`Rows`] calls the grid closed.
///
/// The two answers [`Rows`] chooses between differ by exactly the fraction of
/// the circle that is missing, so this is what bounds the error the closed
/// branch can carry: 2%, which on an NROT of 1.0 — the magnitude its reference
/// calls a mesocyclone — is 0.02, half the 0.04 step that reference quantizes
/// its own output in, and so invisible in the only comparison the pipeline
/// reading this is calibrated against.
///
/// The margin on the other side is far wider than the noise it has to survive.
/// A complete cut's azimuths jitter by a few hundredths of a degree, which moves
/// the median of its 720 gaps by about a thousandth of one — 0.2% of a step,
/// against the 2% this leaves. The least uniform complete cut measured is a
/// TDWR's: every cut of TPIT volume 277 is 360 rows whose *median* gap is
/// 1.0107°, well off the 1.0° mean its 360 gaps must average to, and 360 of
/// those still accounts for 101.1% of the circle. Nor are the grids that do
/// fall through near cases: a 90° sector accounts for a quarter of the circle,
/// a half-received cut for half of it, and the abandoned 200° tail that
/// [`median_azimuth_step_deg`] exists for covers 55%.
const CLOSED_SWEEP_COVERAGE: f64 = 0.98;

/// How a polar grid's rows sit in azimuth: the angle between adjacent rows, and
/// whether the last row neighbours the first.
///
/// Both are one question — do the rows close the circle — asked once, because
/// the two answers have to agree. A grid differentiated over the arc its rows
/// really cover but still indexed as though that arc met itself would read the
/// jump between two rows 324° apart at the scale of two rows half a degree
/// apart, which is the largest wrong number either mistake can produce.
///
/// # The step
///
/// On a grid that closes the circle, `360 / count` is not an estimate of the
/// step: n rows laid around a circle leave n gaps summing to exactly 360°, so
/// their mean is exactly `360 / n` however much the antenna jittered on the way
/// round. A complete cut therefore takes it unmeasured — and every WSR-88D VCP
/// cut is a complete cut, as is every TDWR cut in the volumes measured here.
///
/// On a grid that stops short, `360 / count` is the spacing of nothing: a 36°
/// sector of 72 rows is 0.5° apart and reads 5°, ten times the arc it has.
/// There the step is measured, by [`median_azimuth_step_deg`] — **median**, so
/// the one abandoned arc in a half-received cut is not averaged into everyone's
/// spacing, and **shared**, so a grid is read at the same spacing the sampler
/// serves it and the plan view paints it at.
///
/// What a radial declares on the wire is the other candidate and answers a
/// different question. What a consumer indexing a grid needs is the angle
/// between rows `i` and `i + 1` of the grid in front of it, which is a property
/// of how that grid was assembled — a sweep of 0.5° radials handed over every
/// other radial has 1.0° rows whatever those radials declare — and a
/// declaration of zero would divide an arc down to nothing. This measurement
/// cannot return zero, because it drops zero gaps.
///
/// # What it does not answer
///
/// Whether the rows are *dense*. A cut that lost one chunk of six holds 300
/// rows spanning 60°..360°, and rows 119 and 120 of that grid are 60° apart
/// with nothing between them — an interior seam, invisible here, because this
/// knows only where a grid ends and not where it is sparse. That is why
/// [`crate::chunks`] keeps such a cut out of every snapshot rather than
/// leaving the reader to notice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rows {
    /// Rows in the grid. Taken from the caller rather than from the azimuth
    /// slice, because the grid is what gets indexed.
    pub count: usize,
    /// The angle between rows `i` and `i + 1`, degrees.
    pub step_deg: f64,
    /// Whether row `count - 1` neighbours row `0`.
    pub closed: bool,
}

impl Rows {
    pub(crate) fn of(azimuths_deg: &[f64], count: usize) -> Self {
        let closed = Self {
            count,
            step_deg: 360.0 / count.max(1) as f64,
            closed: true,
        };
        match median_azimuth_step_deg(azimuths_deg.iter().copied()) {
            Some(step) if step * (count as f64) < CLOSED_SWEEP_COVERAGE * 360.0 => Self {
                step_deg: step,
                closed: false,
                ..closed
            },
            // Nothing to measure between — no rows, one row, or a sweep that
            // reported one azimuth n times. None of them is a sector.
            _ => closed,
        }
    }

    /// The row `d` rows around from row `i`, or `None` when that row lies past
    /// the end of an arc that does not close.
    ///
    /// On a closed grid this is the wrap it has always been, to the index:
    /// `(i + d).rem_euclid(count)`, the last row bordering the first because on
    /// a full rotation it does.
    ///
    /// Past the edge of a sector it is `None`, which is the answer every
    /// consumer here already has a branch for, because it is the same absence
    /// their *other* axis produces at its own ends: a stencil whose tap cell
    /// holds no data reads ND, a median window counts only the cells that
    /// exist, a connected component stops. Wrapping instead would hand row 0 of
    /// a 36° sector the velocities of row 71 — a sample the antenna took 324°
    /// away, arriving as a step of a hundred-odd m/s divided by half a degree
    /// of arc, which is how both ends of a sector paint the ±5 clamp with no
    /// rotation anywhere in the data.
    pub(crate) fn neighbour(self, i: usize, d: i32) -> Option<usize> {
        let n = self.count as i32;
        if n == 0 {
            return None;
        }
        let k = i as i32 + d;
        if self.closed {
            Some(k.rem_euclid(n) as usize)
        } else {
            (0..n).contains(&k).then_some(k as usize)
        }
    }
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

    fn ring(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
    }

    /// A grid whose rows close the circle indexes exactly as a wrap: the last
    /// row borders the first, at every row and every offset. This is the whole
    /// of what says no full-rotation value moves — every consumer's neighbour
    /// lookup goes through here, so if this is `rem_euclid` on a complete cut
    /// then so is every one of them.
    #[test]
    fn a_closed_grid_neighbours_exactly_as_it_wrapped() {
        for n in [360usize, 720] {
            let rows = Rows::of(&ring(n), n);
            assert!(rows.closed);
            assert_eq!(rows.step_deg, 360.0 / n as f64);
            for i in 0..n {
                // ±11 is the widest reach any consumer in this crate has: the
                // width-4 NROT bank kernel's tap list.
                for d in -11i32..=11 {
                    assert_eq!(
                        rows.neighbour(i, d),
                        Some((i as i32 + d).rem_euclid(n as i32) as usize),
                        "row {i} offset {d}",
                    );
                }
            }
        }

        // Neither of the two things a real cut's azimuths do to it moves the
        // answer: collection order starts wherever the antenna was, and the
        // azimuths jitter by a few hundredths of a step around it.
        let rolled: Vec<f64> = (0..720).map(|i| f64::from((i + 431) % 720) * 0.5).collect();
        let jittered: Vec<f64> = (0..720)
            .map(|i| i as f64 * 0.5 + 0.02 * (i as f64 * 1.7).sin())
            .collect();
        for azimuths in [rolled, jittered] {
            let rows = Rows::of(&azimuths, 720);
            assert!(rows.closed);
            assert_eq!(rows.step_deg, 0.5);
        }
    }

    /// A sector's rows stop where the antenna stopped. Inside the arc the
    /// lookup is plain addition; past either end there is no row, and the
    /// caller gets the absence rather than a sample from the far side.
    #[test]
    fn a_sector_has_no_row_past_either_end() {
        // 36° of 0.5° rows: a tenth of the circle, so nowhere near the line.
        let sector: Vec<f64> = (0..72).map(|i| f64::from(i) * 0.5).collect();
        let rows = Rows::of(&sector, 72);
        assert!(!rows.closed);
        assert_eq!(rows.step_deg, 0.5);

        assert_eq!(rows.neighbour(0, 0), Some(0));
        assert_eq!(rows.neighbour(0, 5), Some(5));
        assert_eq!(rows.neighbour(0, -1), None);
        assert_eq!(rows.neighbour(4, -5), None);
        assert_eq!(rows.neighbour(71, 1), None);
        assert_eq!(rows.neighbour(67, 5), None);
        assert_eq!(rows.neighbour(71, -71), Some(0));
    }

    /// The degenerate grids produce a step rather than an infinity or a zero,
    /// and are read as closed: an empty grid and a one-row grid have nothing to
    /// measure between, a grid that reported one azimuth n times has no gap at
    /// all, and two rows 10° apart measure 350° by the documented quirk above —
    /// which is more circle than there is, so not a sector either.
    #[test]
    fn a_grid_with_nothing_to_measure_is_closed() {
        for (azimuths, count, step) in [
            (vec![], 0usize, 360.0),
            (vec![37.5], 1, 360.0),
            (vec![12.0; 8], 8, 45.0),
            (vec![0.0, 10.0], 2, 180.0),
        ] {
            let rows = Rows::of(&azimuths, count);
            assert!(rows.closed, "{azimuths:?}");
            assert_eq!(rows.step_deg, step, "{azimuths:?}");
        }
        // No row to reach even at offset zero, rather than a division by the
        // zero row count the wrap would be.
        assert_eq!(Rows::of(&[], 0).neighbour(0, 0), None);
    }

    /// Where the two branches meet, from both sides. A cut missing one radial
    /// of 720 covers 359.5° and is still a rotation; a cut missing 2% of its
    /// radials in one piece is not.
    #[test]
    fn the_closed_test_is_about_the_arc_not_the_count() {
        let dropped: Vec<f64> = ring(720).into_iter().filter(|a| *a != 100.0).collect();
        let rows = Rows::of(&dropped, 719);
        assert!(rows.closed);
        assert_eq!(rows.step_deg, 360.0 / 719.0);

        // The same sweep with a 10° bite out of it — twenty radials in one
        // piece rather than one anywhere. Its 700 rows claim 0.5° × 700 = 350°
        // of the circle, 97.2%, and fall through.
        let bitten: Vec<f64> = ring(720)
            .into_iter()
            .filter(|a| !(100.0..110.0).contains(a))
            .collect();
        assert_eq!(bitten.len(), 700);
        let rows = Rows::of(&bitten, bitten.len());
        assert!(!rows.closed);
        assert_eq!(rows.step_deg, 0.5);
    }
}
