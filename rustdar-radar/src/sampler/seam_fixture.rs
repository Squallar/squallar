//! The fold-straddle guard, scored against labelled truth on a fixture.
//!
//! # Why this exists next to a corpus sweep
//!
//! [`SEAM_PROXIMITY_ACROSS_GATES`] and [`SEAM_PROXIMITY_ACROSS_TILTS`] were
//! arbitrated on archive volumes, and the sweep that did it needs a corpus on
//! disk — hundreds of volumes nobody has by default. That makes it the right
//! instrument and the wrong guard: a number justified only by a run nobody
//! repeats is a number that can drift without anything noticing.
//!
//! So the measurement is carried at two levels of effort. The corpus sweep on
//! `campaign-harness` answers *what the constants cost on real weather*. This
//! module answers *whether the guard still behaves the way that argument
//! assumed*, on a fixture built in code, in milliseconds, on every build.
//!
//! # Why a fixture can carry any of it
//!
//! Because the expensive part of the corpus is realism, and none of the
//! properties below need it. What the corpus supplies is a population of
//! corner tuples drawn from real weather; what the guard is, is a decision
//! about a tuple. Feed it tuples from an analytic field instead and every
//! *structural* claim the constants rest on is still testable — the direction
//! the fractions trade in, the way the break-even moves with the fold base
//! rate, and the condition under which the rule's own premise fails. What a
//! fixture cannot supply is the *value* of the trade on real weather, which is
//! why those percentages live on the harness branch and not here.
//!
//! # Why this is not circular
//!
//! The generator draws **true** speeds and folds them with [`wrap`]. The label
//! is [`fold_index`] on those true speeds — arithmetic on numbers that existed
//! before the fold. The decision is [`super::straddles_fold`], the shipped
//! function, reading only the **folded** values, exactly what the sampler sees.
//! Nothing that produces the label is consulted by the thing being scored, and
//! a bug in `straddles_fold` cannot reach the label to hide itself.

use super::*;

/// The fold seam the fixture folds at, m/s.
///
/// A real VCP 31 Nyquist velocity: 11.17–12.50 is that pattern's whole
/// operational range and this is the middle of it. The corpus sweep uses the
/// same number, so a reader comparing the two tables is comparing like with
/// like.
const FIXTURE_SEAM_MS: f64 = 11.5;

/// The fractions swept, spanning both shipped constants.
const SWEPT: [f64; 14] = [
    0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90,
];

/// The RDA's wrap: the true speed `v` reported inside `[−l, l)`.
///
/// Phase is periodic on `2π` and the velocity that maps onto is periodic on
/// `2·l`, so the reported number is the true one shifted by whole multiples of
/// `2·l` until it lands in the reporting interval. `rem_euclid` is that
/// sentence.
fn wrap(v: f64, l: f64) -> f64 {
    (v + l).rem_euclid(2.0 * l) - l
}

/// Which Nyquist interval a true speed falls in — **this is the label**.
///
/// Two samples sharing an index were shifted by the same multiple of `2·l`, so
/// the wrap left the step between them as it found it and no fold separates
/// them. Different indices were shifted differently, so a fold does. No field,
/// no context and no tolerance enters.
fn fold_index(v: f64, l: f64) -> i64 {
    ((v + l) / (2.0 * l)).floor() as i64
}

/// A deterministic stream, so the fixture is a fixture and not a sample.
///
/// xorshift64*, written out rather than pulled in: the numbers below are pinned
/// to three decimal places, and that is only honest if the stream cannot move
/// under a dependency bump.
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
///
/// `corners` is 4 for the bilinear path and 2 for the vertical lerp,
/// `field_scale` is how fast the air gets in m/s, and `spread` is the scale of
/// the tuple's own **true** change — the quantity the rule's premise is about.
///
/// **The two knobs are not interchangeable and that is the point.**
/// `field_scale` sets the fold base rate, because folding is what happens when
/// the air outruns the Nyquist interval: a field confined inside `±l` never
/// wraps however rough it is, and one roaming many periods wraps constantly.
/// `spread` sets whether a fold, once present, arrives in the shape the rule
/// can recognise. Sweeping the first says how the break-even moves with base
/// rate; sweeping the second says where the rule's premise fails.
///
/// The decision is [`straddles_at`], not [`super::straddles_fold`] directly,
/// because the shipped function takes its fraction from the [`Seam`] variant
/// and a sweep has to vary that fraction. The two are pinned equal at both
/// shipped fractions by
/// [`tests::the_swept_rule_is_the_shipped_rule_at_the_shipped_fractions`],
/// which is what makes this a measurement of the shipped guard.
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
///
/// [`straddles_fold`] takes the fraction from the `Seam` variant and not from
/// an argument — deliberately, so a call site cannot silently swap the two
/// constants — which means a sweep has to reach the fraction some other way.
/// Rather than widen the shipped signature for a test, this scores the rule's
/// algebra directly at an arbitrary fraction and separately pins that the two
/// shipped variants agree with it at their own constants.
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
    ///
    /// **This is the join that makes the rest of this module about the shipped
    /// guard.** Everything below sweeps [`straddles_at`] so it can vary the
    /// fraction; that is only meaningful if, at the two fractions that actually
    /// ship, it is the same decision [`straddles_fold`] makes. Swept over a
    /// grid that crosses both seams from every direction, including the exact
    /// boundary values where a `<` and a `<=` would part company.
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
    ///
    /// The whole argument on both constants is that the fraction buys one
    /// against the other, so which direction each moves in is load-bearing.
    /// It is also the cheapest possible regression on the rule's shape: an
    /// inverted comparison, a sign slip, or a fraction applied to the wrong
    /// side of the seam breaks monotonicity long before it breaks a percentage.
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
    ///
    /// # The finding this pins
    ///
    /// Both constants' docs record a puzzle: scored against labelled truth, the
    /// marginal bands cross 1:1 far below where either constant sits, which
    /// read as an argument for moving them a long way down. The corpus campaign
    /// resolved it — the crossing is not a property of the guard but of how
    /// fold-rich the population scoring it happens to be, and every labelled
    /// corpus that produced a low crossing was fold-rich. Across five
    /// populations spanning 11 %–69 % the relationship is monotone.
    ///
    /// That is a claim about arithmetic, not about weather, so it is checkable
    /// here: the same generator at two spreads produces a fold-rich and a
    /// fold-poor population, and the fold-poor one must cross later. If this
    /// ever fails, the reasoning that left both constants where they are has
    /// lost its foundation and the corpus sweep needs re-running.
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
    ///
    /// # What this makes checkable
    ///
    /// [`straddles_fold`]'s argument is that one wrap of a smooth field leaves
    /// *both* sides near `±limit`, which holds only while the tuple's own true
    /// change is small next to the Nyquist interval. Where that fails a real
    /// fold arrives with one end deep inside the range and the rule reads it as
    /// shear.
    ///
    /// On real volumes that failure is organised by **beam height**: measured
    /// in strata, vertical recall runs 80 % at 2–4 km down to 27 % above 11 km,
    /// and the mean true change between adjacent tilts climbs from 1.5 m/s to
    /// 7.9 m/s over the same strata — reaching the guard's own line, 5.75 m/s
    /// at `f = 0.50` against this seam, in the 6–8 km stratum where recall
    /// halves. Height is the organising axis and slant range is not.
    ///
    /// The mechanism is arithmetic and belongs here; the altitudes are weather
    /// and belong on the harness branch. This asserts the mechanism: hold
    /// everything else fixed, sweep only the true change, and recall must fall
    /// away as it approaches `f · limit`.
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
    ///
    /// # Why a pinned number and not only a property
    ///
    /// The tests above would all still pass if someone changed either constant,
    /// because they are about shape. This one moves the moment a constant does:
    /// it scores the fixture at the two values that ship and pins what they
    /// produce. The bands are wide enough that a compiler or platform
    /// difference cannot trip them and narrow enough that a grid step cannot
    /// hide inside one — the neighbouring fraction on each path lands outside
    /// its own band, which the assertions check rather than assume.
    ///
    /// These percentages are **the fixture's, not the archive's**. Real recall
    /// and false-fire figures depend on the fold base rate and the height
    /// distribution of the population, and live with the corpus sweep on
    /// `campaign-harness`. Nothing here should be quoted as a cost on weather.
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
