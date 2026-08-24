//! The fold-straddle guard, scored against labelled truth on a fixture.

use super::*;

/// The fold seam the fixture folds at, m/s.
const FIXTURE_SEAM_MS: f64 = 11.5;

/// The fractions swept, spanning both shipped constants.
const SWEPT: [f64; 14] = [
    0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90,
];

/// The RDA's wrap: the true speed `v` reported inside `[−l, l)`.
fn wrap(v: f64, l: f64) -> f64 {
    (v + l).rem_euclid(2.0 * l) - l
}

/// Which Nyquist interval a true speed falls in — **this is the label**.
fn fold_index(v: f64, l: f64) -> i64 {
    ((v + l) / (2.0 * l)).floor() as i64
}

/// A deterministic stream, so the fixture is a fixture and not a sample.
struct Stream(u64);

impl Stream {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// The next value, uniform on `[-1, 1)`.
    fn signed(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // The top 53 bits are the well-mixed ones.
        f64::from_bits(0x3FF0_0000_0000_0000 | (bits >> 12)).mul_add(2.0, -3.0)
    }
}

/// One scored population.
#[derive(Default)]
struct Tally {
    candidates: u64,
    folded: u64,
    /// Per swept fraction: true positives, false positives.
    tp: [u64; SWEPT.len()],
    fp: [u64; SWEPT.len()],
}

impl Tally {
    fn recall(&self, i: usize) -> f64 {
        100.0 * self.tp[i] as f64 / self.folded as f64
    }

    fn false_fire(&self, i: usize) -> f64 {
        100.0 * self.fp[i] as f64 / (self.candidates - self.folded) as f64
    }

    /// The first band where raising the fraction gives up more real folds than
    /// it saves real shear — the break-even the shipped constants were placed
    /// at. Returns the fraction at the low side of that band.
    fn crossing(&self) -> f64 {
        for (i, band) in SWEPT.windows(2).enumerate() {
            let lost = self.tp[i].saturating_sub(self.tp[i + 1]) as f64;
            let saved = self.fp[i].saturating_sub(self.fp[i + 1]) as f64;
            if lost > 0.0 && saved / lost < 1.0 {
                return band[0];
            }
        }
        *SWEPT.last().unwrap()
    }

    fn index_of(f: f64) -> usize {
        SWEPT
            .iter()
            .position(|x| (x - f).abs() < 1e-9)
            .expect("the shipped fractions are on the swept grid")
    }
}

/// Draw a labelled population and score the shipped guard over it.
fn score(corners: usize, field_scale: f64, spread: f64, seed: u64) -> Tally {
    let l = FIXTURE_SEAM_MS;
    let mut rng = Stream::new(seed);
    let mut tally = Tally::default();

    for _ in 0..200_000 {
        // The tuple's own true speeds: a local level plus an excursion.
        let level = rng.signed() * field_scale;
        let mut truth = [0.0f64; 4];
        for slot in truth.iter_mut().take(corners) {
            *slot = level + rng.signed() * spread;
        }
        let truth = &truth[..corners];

        let folded: Vec<f64> = truth.iter().map(|v| wrap(*v, l)).collect();
        let lo = folded.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = folded.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // Only sign-crossing tuples are candidates: with both extremes on one
        // side of zero no fraction can fire, so scoring them would inflate
        // every denominator with cases the guard never sees.
        if !(lo < 0.0 && hi > 0.0) {
            continue;
        }

        // The label, from the pre-fold truth of the two extremes.
        let lo_i = folded
            .iter()
            .position(|v| *v == lo)
            .expect("lo came from this slice");
        let hi_i = folded
            .iter()
            .position(|v| *v == hi)
            .expect("hi came from this slice");
        let is_fold = fold_index(truth[lo_i], l) != fold_index(truth[hi_i], l);

        tally.candidates += 1;
        if is_fold {
            tally.folded += 1;
        }

        let samples: Vec<Sample> = folded.iter().map(|v| Sample::found(*v as f32)).collect();
        for (i, &f) in SWEPT.iter().enumerate() {
            // Reading only the folded values — exactly what the sampler sees.
            if straddles_at(&samples, f, l) {
                if is_fold {
                    tally.tp[i] += 1;
                } else {
                    tally.fp[i] += 1;
                }
            }
        }
    }
    tally
}

/// The guard's own fraction, threaded through a [`Seam`] the fixture picks.
fn straddles_at(corners: &[Sample], fraction: f64, limit: f64) -> bool {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for corner in corners {
        let value = f64::from(corner.value);
        lo = lo.min(value);
        hi = hi.max(value);
    }
    lo < -fraction * limit && hi > fraction * limit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep's re-implementation of the rule agrees with the shipped one at
    /// both shipped fractions.
    #[test]
    fn the_swept_rule_is_the_shipped_rule_at_the_shipped_fractions() {
        let limit = FIXTURE_SEAM_MS;
        let mut checked = 0u32;
        let mut fired = 0u32;
        for a in -46..=46 {
            for b in -46..=46 {
                let lo = f64::from(a) * 0.25 * limit / 11.5;
                let hi = f64::from(b) * 0.25 * limit / 11.5;
                let corners = [Sample::found(lo as f32), Sample::found(hi as f32)];
                for (fraction, seam) in [
                    (SEAM_PROXIMITY_ACROSS_GATES, Seam::AcrossGates(limit)),
                    (SEAM_PROXIMITY_ACROSS_TILTS, Seam::AcrossTilts(limit)),
                ] {
                    let shipped = straddles_fold(&corners, seam);
                    assert_eq!(
                        shipped,
                        straddles_at(&corners, fraction, limit),
                        "the swept rule and the shipped rule disagree at f={fraction} \
                         on ({lo:.4}, {hi:.4})",
                    );
                    checked += 1;
                    fired += u32::from(shipped);
                }
            }
        }
        assert!(checked > 8_000, "the grid shrank to {checked} comparisons");
        assert!(
            fired > 500,
            "only {fired} of {checked} cases fired — a grid that never exercises the \
             positive branch would pass this test with the rule inverted",
        );
    }

    /// Raising the fraction gives up folds and saves shear, never the reverse.
    #[test]
    fn the_fraction_trades_recall_against_false_fires_in_one_direction() {
        for (corners, name) in [(4usize, "quad"), (2usize, "rung pair")] {
            let t = score(corners, 4.0 * FIXTURE_SEAM_MS, 8.0, 20_250_401);
            assert!(
                t.candidates > 1_000,
                "{name}: only {} candidates",
                t.candidates
            );
            assert!(t.folded > 100, "{name}: only {} labelled folds", t.folded);
            for i in 0..SWEPT.len() - 1 {
                assert!(
                    t.tp[i] >= t.tp[i + 1],
                    "{name}: recall rose from f={} to f={} ({} → {} folds caught)",
                    SWEPT[i],
                    SWEPT[i + 1],
                    t.tp[i],
                    t.tp[i + 1],
                );
                assert!(
                    t.fp[i] >= t.fp[i + 1],
                    "{name}: false fires rose from f={} to f={} ({} → {})",
                    SWEPT[i],
                    SWEPT[i + 1],
                    t.fp[i],
                    t.fp[i + 1],
                );
            }
        }
    }

    /// The break-even moves **up** as the fold base rate falls.
    #[test]
    fn the_break_even_moves_up_as_folds_get_rarer() {
        let rich = score(2, 4.0 * FIXTURE_SEAM_MS, 3.0, 20_250_402);
        let poor = score(2, 0.7 * FIXTURE_SEAM_MS, 3.0, 20_250_403);

        let rich_rate = 100.0 * rich.folded as f64 / rich.candidates as f64;
        let poor_rate = 100.0 * poor.folded as f64 / poor.candidates as f64;
        assert!(
            rich_rate > poor_rate + 20.0,
            "the two populations are not separated in base rate: {rich_rate:.1}% and \
             {poor_rate:.1}% — the test cannot say anything about base rate",
        );
        assert!(
            poor.crossing() > rich.crossing(),
            "the fold-poor population ({poor_rate:.1}% folds) crossed at {:.2} and the \
             fold-rich one ({rich_rate:.1}%) at {:.2}. The corpus campaign measured this \
             relationship as monotone over five populations and left both constants where \
             they are because of it.",
            poor.crossing(),
            rich.crossing(),
        );
    }

    /// Recall collapses once the tuple's own true change reaches the guard's
    /// line — the rule's premise failing, on demand.
    #[test]
    fn recall_falls_away_as_the_true_change_reaches_the_guards_own_line() {
        let line = SEAM_PROXIMITY_ACROSS_TILTS * FIXTURE_SEAM_MS;
        let at = Tally::index_of(SEAM_PROXIMITY_ACROSS_TILTS);

        let gentle = score(2, 4.0 * FIXTURE_SEAM_MS, 0.25 * line, 20_250_404);
        let atline = score(2, 4.0 * FIXTURE_SEAM_MS, line, 20_250_405);
        let beyond = score(2, 4.0 * FIXTURE_SEAM_MS, 2.0 * line, 20_250_406);

        let (g, a, b) = (gentle.recall(at), atline.recall(at), beyond.recall(at));
        assert!(
            g > a && a > b,
            "recall did not fall as the true change grew towards and past the guard's \
             line ({line:.2} m/s): {g:.1}% at a quarter of it, {a:.1}% at it, {b:.1}% at \
             twice it. This is the premise straddles_fold rests on, and the vertical \
             constant's whole failure profile is this curve.",
        );
        assert!(
            g - b > 25.0,
            "the collapse is only {:.1} points ({g:.1}% → {b:.1}%). The corpus measured \
             80% → 27% across the same transition; a fixture showing almost no effect \
             means the generator stopped exercising the premise.",
            g - b,
        );
    }

    /// Both shipped fractions, on one fixture, with their costs printed.
    #[test]
    fn the_shipped_fractions_land_where_this_fixture_says_they_do() {
        let quads = score(4, 4.0 * FIXTURE_SEAM_MS, 8.0, 20_250_407);
        let rungs = score(2, 4.0 * FIXTURE_SEAM_MS, 8.0, 20_250_408);

        let qi = Tally::index_of(SEAM_PROXIMITY_ACROSS_GATES);
        let ri = Tally::index_of(SEAM_PROXIMITY_ACROSS_TILTS);

        println!(
            "  quads  @ {:.2}: recall {:.2}%  false-fire {:.3}%  ({} candidates, {:.1}% folds)",
            SEAM_PROXIMITY_ACROSS_GATES,
            quads.recall(qi),
            quads.false_fire(qi),
            quads.candidates,
            100.0 * quads.folded as f64 / quads.candidates as f64,
        );
        println!(
            "  rungs  @ {:.2}: recall {:.2}%  false-fire {:.3}%  ({} candidates, {:.1}% folds)",
            SEAM_PROXIMITY_ACROSS_TILTS,
            rungs.recall(ri),
            rungs.false_fire(ri),
            rungs.candidates,
            100.0 * rungs.folded as f64 / rungs.candidates as f64,
        );

        // A neighbouring grid step must fall outside the band, or the band is
        // not pinning the constant — it is pinning the neighbourhood.
        for (name, t, i, lo, hi) in [
            ("quads", &quads, qi, 60.0, 70.0),
            ("rungs", &rungs, ri, 47.0, 57.0),
        ] {
            let r = t.recall(i);
            assert!(
                (lo..hi).contains(&r),
                "{name} recall {r:.2}% left the pinned band {lo}–{hi}%",
            );
            let neighbour = t.recall(i + 1);
            assert!(
                (r - neighbour).abs() > 1.0,
                "{name}: the next grid step reads {neighbour:.2}% against {r:.2}% — too \
                 close for this pin to detect a one-step change in the constant",
            );
        }
    }
}
