//! One answer to "how far apart are this sweep's radials", for everything in
//! the crate that has to decide where a sweep stops.

/// How many median azimuth steps two radials may be apart and still count as
/// adjacent — i.e. as a pair worth interpolating between, or a pair whose
/// wedges may meet.
///
/// One step is what consecutive radials are apart by construction, and a real
/// sweep's jitter is a few hundredths of a step, so 1.5 admits a jittered
/// sweep and excludes a gap of two steps (one dropped radial), never bridged.
pub(crate) const MAX_ADJACENT_GAP_STEPS: f64 = 1.5;

/// The median circular gap between a sweep's adjacent azimuths, degrees, or
/// `None` when the sweep has no two distinct azimuths to measure between.
///
/// `azimuths` may arrive in any order or range: they are wrapped into
/// `[0, 360)`, sorted, and walked circularly, so the gap from the last back
/// to the first is one step rather than the sweep's one big hole. Gaps of
/// exactly zero are dropped.
///
/// The wrap goes through `f64` `rem_euclid` and back to `f32` before the gaps
/// are taken: that round-trip is the quantization the sampler's azimuth index
/// uses, and its hole test is a `<=` against a threshold scaled by this.
///
/// With exactly two distinct azimuths the median of the two gaps is the
/// **upper** one, so two radials 10° apart report a step of 350°. Callers cap
/// it (`render::MAX_WEDGE_DEG`).
pub(crate) fn median_azimuth_step_deg(azimuths: impl IntoIterator<Item = f64>) -> Option<f64> {
    let gaps = circular_gaps_deg(azimuths);
    gaps.get(gaps.len() / 2).copied()
}

/// Every positive circular gap between adjacent azimuths, ascending.
fn circular_gaps_deg(azimuths: impl IntoIterator<Item = f64>) -> Vec<f64> {
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
    gaps
}

/// The widest circular gap between adjacent azimuths, degrees, or `None` when
/// there are no two distinct azimuths to measure between.
pub(crate) fn largest_azimuth_gap_deg(azimuths: impl IntoIterator<Item = f64>) -> Option<f64> {
    circular_gaps_deg(azimuths).last().copied()
}

/// Whether a sweep's azimuths cover the circle without an interior hole.
///
/// [`MAX_ADJACENT_GAP_STEPS`] applied to the sweep as a whole: whole when its
/// widest gap is one the sampler would interpolate across. Scale-free — the
/// comparison is against the sweep's own median step.
pub(crate) fn covers_the_circle(azimuths: &[f64]) -> bool {
    let gaps = circular_gaps_deg(azimuths.iter().copied());
    let (Some(median), Some(largest)) = (gaps.get(gaps.len() / 2), gaps.last()) else {
        return true;
    };
    largest <= &(median * MAX_ADJACENT_GAP_STEPS)
}

/// How much of the circle a grid's rows must account for, at their own measured
/// spacing, before [`Rows`] calls the grid closed.
///
/// The two branches differ by the fraction of the circle that is missing, so
/// this bounds the closed branch's error: 2%, which on an NROT of 1.0 is
/// 0.02, half the 0.04 step the reference quantizes its own output in.
///
/// Observations, from a corpus not in this tree: a complete cut's azimuths
/// jitter about 0.2% of a step, and the least uniform measured (TPIT volume
/// 277, median gap 1.0107°) still accounts for 101.1% of the circle. The
/// grids that fall through are not near cases — a 90° sector is 25%, a
/// half-received cut 50%, an abandoned 200° tail 55%.
const CLOSED_SWEEP_COVERAGE: f64 = 0.98;

/// How a polar grid's rows sit in azimuth: the angle between adjacent rows, and
/// whether the last row neighbours the first.
///
/// On a grid that closes the circle, `360 / count` is exact: n rows leave n
/// gaps summing to 360°. On a grid that stops short it is the spacing of
/// nothing — a 36° sector of 72 rows is 0.5° apart and reads 5° — so there
/// the step is measured by [`median_azimuth_step_deg`], median so one
/// abandoned arc is not averaged into everyone's spacing.
///
/// It does not say whether the rows are *dense*: a cut that lost one chunk of
/// six still ends where a complete sweep ends, and its interior 60° seam is
/// invisible here. [`crate::chunks`] keeps such a cut out of every snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rows {
    /// Rows in the grid, from the caller rather than the azimuth slice.
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
            // Nothing to measure between — none of these is a sector.
            _ => closed,
        }
    }

    /// The row `d` rows around from row `i`, or `None` when that row lies past
    /// the end of an arc that does not close.
    ///
    /// Closed grids wrap: `(i + d).rem_euclid(count)`. Past the edge of a
    /// sector it is `None` rather than a wrap, which would hand row 0 of a 36°
    /// sector the sample the antenna took 324° away.
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

    #[test]
    fn a_complete_sweep_measures_its_own_spacing() {
        let half = (0..720).map(|i| f64::from(i) * 0.5);
        assert_eq!(median_azimuth_step_deg(half), Some(0.5));

        let whole = (0..360).map(f64::from);
        assert_eq!(median_azimuth_step_deg(whole), Some(1.0));
    }

    #[test]
    fn order_and_range_do_not_change_the_measurement() {
        let rolled = (0..720).map(|i| f64::from((i + 431) % 720) * 0.5);
        assert_eq!(median_azimuth_step_deg(rolled), Some(0.5));

        let unwrapped = (0..720).map(|i| f64::from(i) * 0.5 - 360.0);
        assert_eq!(median_azimuth_step_deg(unwrapped), Some(0.5));
    }

    #[test]
    fn one_enormous_hole_does_not_move_the_median() {
        let tail = (0..400).map(|i| f64::from(i) * 0.5);
        assert_eq!(median_azimuth_step_deg(tail), Some(0.5));
    }

    #[test]
    fn duplicate_azimuths_are_not_zero_steps() {
        let doubled = (0..720).flat_map(|i| [f64::from(i) * 0.5; 2]);
        assert_eq!(median_azimuth_step_deg(doubled), Some(0.5));
    }

    #[test]
    fn nothing_to_measure_reports_nothing() {
        assert_eq!(median_azimuth_step_deg([]), None);
        assert_eq!(median_azimuth_step_deg([37.5]), None);
        assert_eq!(median_azimuth_step_deg([37.5, 37.5, 397.5]), None);
    }

    #[test]
    fn two_radials_report_the_larger_circular_gap() {
        assert_eq!(median_azimuth_step_deg([0.0, 10.0]), Some(350.0));
        assert_eq!(median_azimuth_step_deg([10.0, 0.0]), Some(350.0));
    }

    #[test]
    fn the_hole_the_median_ignores_is_what_the_largest_gap_reports() {
        let tail: Vec<f64> = (0..400).map(|i| f64::from(i) * 0.5).collect();
        assert_eq!(median_azimuth_step_deg(tail.iter().copied()), Some(0.5));
        // 400 radials at 0.5° reach 199.5°, so the closing hole is 160.5°.
        assert_eq!(largest_azimuth_gap_deg(tail.iter().copied()), Some(160.5));

        let whole: Vec<f64> = (0..720).map(|i| f64::from(i) * 0.5).collect();
        assert_eq!(largest_azimuth_gap_deg(whole), Some(0.5));

        assert_eq!(largest_azimuth_gap_deg([]), None);
        assert_eq!(largest_azimuth_gap_deg([37.5]), None);
    }

    #[test]
    fn a_lost_chunk_measures_the_same_wherever_it_falls() {
        let present = |keep: &dyn Fn(usize) -> bool| -> Vec<f64> {
            (0..720)
                .filter(|i| keep(*i))
                .map(|i| i as f64 * 0.5)
                .collect()
        };
        let lost_middle = present(&|i| !(240..360).contains(&i));
        let lost_tail = present(&|i| i < 600);

        for azimuths in [&lost_middle, &lost_tail] {
            assert_eq!(azimuths.len(), 600);
            assert_eq!(
                largest_azimuth_gap_deg(azimuths.iter().copied()),
                Some(60.5)
            );
            assert_eq!(median_azimuth_step_deg(azimuths.iter().copied()), Some(0.5));
            assert!(!covers_the_circle(azimuths));
        }
    }

    #[test]
    fn wholeness_is_measured_against_the_sweeps_own_spacing() {
        let super_res: Vec<f64> = (0..720).map(|i| i as f64 * 0.5).collect();
        let standard: Vec<f64> = (0..360).map(|i| i as f64).collect();
        assert!(covers_the_circle(&super_res));
        assert!(covers_the_circle(&standard));

        // One chunk short of each: 600 of 720 and 240 of 360.
        let short_super: Vec<f64> = (0..600).map(|i| i as f64 * 0.5).collect();
        let short_standard: Vec<f64> = (0..240).map(|i| i as f64).collect();
        assert!(!covers_the_circle(&short_super));
        assert!(!covers_the_circle(&short_standard));

        // Jitter is not a hole.
        let jittered: Vec<f64> = (0..720)
            .map(|i| i as f64 * 0.5 + 0.02 * (i as f64 * 1.7).sin())
            .collect();
        assert!(covers_the_circle(&jittered));

        assert!(covers_the_circle(&[]));
        assert!(covers_the_circle(&[37.5]));
    }

    #[test]
    fn one_dropped_radial_is_already_a_hole() {
        let mut azimuths: Vec<f64> = (0..720).map(|i| i as f64 * 0.5).collect();
        azimuths.remove(300);
        assert_eq!(largest_azimuth_gap_deg(azimuths.iter().copied()), Some(1.0));
        assert!(!covers_the_circle(&azimuths));
    }

    fn ring(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 * 360.0 / n as f64).collect()
    }

    #[test]
    fn a_closed_grid_neighbours_exactly_as_it_wrapped() {
        for n in [360usize, 720] {
            let rows = Rows::of(&ring(n), n);
            assert!(rows.closed);
            assert_eq!(rows.step_deg, 360.0 / n as f64);
            for i in 0..n {
               // ±11 is the widest reach any consumer here has: the width-4
               // NROT bank kernel's tap list.
                for d in -11i32..=11 {
                    assert_eq!(
                        rows.neighbour(i, d),
                        Some((i as i32 + d).rem_euclid(n as i32) as usize),
                        "row {i} offset {d}",
                    );
                }
            }
        }

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
        assert_eq!(Rows::of(&[], 0).neighbour(0, 0), None);
    }

    #[test]
    fn the_closed_test_is_about_the_arc_not_the_count() {
        let dropped: Vec<f64> = ring(720).into_iter().filter(|a| *a != 100.0).collect();
        let rows = Rows::of(&dropped, 719);
        assert!(rows.closed);
        assert_eq!(rows.step_deg, 360.0 / 719.0);

        // The same sweep with a 10° bite: 700 rows claim 0.5° × 700 = 350° of
        // the circle, 97.2%, and fall through.
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
