//! The tile-sharpness rung, decided: whether one source draws at the whole
//! zoom below the fractional one, from two inputs and a dwell in both
//! directions.
//!
//! Pure and host-testable — integers in, a state out, no egui type — because
//! the frame thread runs [`snap_decision`] once per source per pass and must
//! do nothing else for it: the readings it takes are levels the cache already
//! keeps, and the one projection is a multiply.
//!
//! # What the rung trades
//!
//! Sharpness, and nothing else. `ui_map_overlays::draw_tile_layer` asks a
//! snapped source for `zoom.floor()` where it would have asked for
//! `zoom.round()`, so in the upper half of every zoom the tiles come from one
//! level up and are drawn at `1x`-`2x` their side instead of `0.7x`-`1x`
//! (`walkers::Projector::tile_rect` scales whatever level it is given). Fewer,
//! larger, softer tiles cover the same glass: the worst case between zooms
//! falls to the whole-zoom count. Placement, the ancestor net, the warm net,
//! the labels and every latency are untouched; the net in particular is asked
//! for at `WARM_ANCESTOR_STEPS` under whichever level is drawn, because a hole
//! is a wrong picture where a soft one is not.
//!
//! # The two inputs, either of which arms
//!
//! 1. **The scene-level rung** — `Budgets::tile_whole_zoom`, the ladder rung
//!    `squallar_device_profile::fit` takes when the scene's priced need does
//!    not fit the capacity, delivered per frame as
//!    `TileCacheBudget::whole_zoom`. A scene that does not fit at this rung
//!    must shed here before the grid and the raster are touched.
//! 2. **The measured working-set overrun** — the source's own styled cache
//!    holding, with every entry the floor does not protect already gone, more
//!    bytes than its allowance (`ByteLru::floor_overrun_bytes`). This is the
//!    pass's own cells costing more than the budget, measured where the
//!    styling ran; it is not the plain `overrun_bytes` level, which a shrink
//!    not yet paid also produces while history leaves one entry a pump, and
//!    an economy event must never shed a rung.
//!
//! Armed for [`TILE_SNAP_DWELL_PASSES`] consecutive passes, the source snaps.
//!
//! # Release, and why it is not read off the resident bytes
//!
//! Release needs **both** inputs gone for the same dwell: the rung off, and
//! the set the source would draw *unsnapped* projected to fit the allowance
//! with a quarter to spare ([`TILE_SNAP_RELEASE_HYSTERESIS`]) — the cells
//! the fractional zoom would want, priced at the cache's mean resident entry
//! — with no overrun of the snapped set either. The projection is the honest
//! reading here and the two levels one might reach for are not: resident
//! bytes fill to the budget with history by design, so a predicate on them
//! would never release a busy source; and the snapped set's own bytes fit by
//! construction the moment the snap lands, so a predicate on them would
//! release after one dwell and re-arm after the next, flapping with period
//! `2 x` the dwell. Both directions dwell so panning across a zoom boundary,
//! where the two levels agree and disagree by turns, does not flap either.
//!
//! The dwell and the margin are the loop pool's figures
//! (`LOOP_POOL_DWELL_FRAMES`, `LOOP_POOL_HYSTERESIS`) restated for a pass:
//! the same shape `LoopPoolState::observe` has, dwell then hysteresis.

/// Consecutive passes a condition must hold before the state flips, either
/// way. Fifteen passes is a quarter of a second at 60 Hz: long enough that a
/// frame of overrun while a pane resizes moves nothing, short enough that a
/// scene which really does not fit is soft before the second refetch.
pub const TILE_SNAP_DWELL_PASSES: u32 = 15;

/// Release needs the unsnapped set to fit with this much to spare, as a
/// fraction `numerator / denominator`: `bytes * 5 <= budget * 4`, i.e. the
/// set may take at most four fifths of the allowance. The margin is what
/// keeps a set that fits by a byte from snapping again a dwell later.
pub const TILE_SNAP_RELEASE_HYSTERESIS: (u64, u64) = (5, 4);

/// One source's position on the rung, and how long the opposite condition
/// has held. `Copy` and small: it is stored per source and stepped per pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnapState {
    /// Whether the source draws at the whole zoom.
    snapped: bool,
    /// Consecutive stepped passes the condition that would flip
    /// [`Self::snapped`] has held; zero the pass it fails.
    dwell: u32,
    /// The pass the state was last stepped for, so a second call in the same
    /// pass — one per pane drawing the source — steps nothing.
    last_pass: Option<u64>,
}

impl SnapState {
    /// Whether the source draws at the whole zoom this pass.
    pub const fn snapped(self) -> bool {
        self.snapped
    }

    /// How many consecutive passes the flipping condition has held so far.
    pub const fn dwell(self) -> u32 {
        self.dwell
    }

    /// A state already snapped, with no dwell — where a test starts a release.
    pub const fn snapped_at(pass_nr: u64) -> Self {
        Self {
            snapped: true,
            dwell: 0,
            last_pass: Some(pass_nr),
        }
    }
}

/// What one source reads at the start of a pass, in the order the module doc
/// argues them. Every field is a level or a projection the source already
/// had; nothing is measured to build one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnapReading {
    /// `Budgets::tile_whole_zoom` as the frame delivered it.
    pub whole_zoom_rung: bool,
    /// `ByteLru::floor_overrun_bytes` of the source's styled cache: what the
    /// working set alone holds past the allowance, zero while history remains.
    pub working_set_overrun_bytes: u64,
    /// What the source would hold if it drew unsnapped this pass: the cells
    /// the fractional zoom wants — glass and net — at the cache's mean
    /// resident entry. Equal to the working set's own price while unsnapped.
    pub unsnapped_bytes: u64,
    /// The source's allowance for its styled (or raster) entries.
    pub budget_bytes: u64,
}

/// Whether `bytes` fits `budget` with the release margin to spare.
pub const fn fits_with_margin(bytes: u64, budget: u64) -> bool {
    let (numerator, denominator) = TILE_SNAP_RELEASE_HYSTERESIS;
    bytes.saturating_mul(numerator) <= budget.saturating_mul(denominator)
}

/// Step one source's state for `pass_nr` on `reading`.
///
/// A pass already stepped — the same or an earlier number — returns `prev`
/// unchanged, so the dwell counts passes and never panes. Unsnapped, the
/// dwell counts passes on which either input arms; snapped, passes on which
/// the rung is off, nothing overruns and the unsnapped set fits with margin.
/// A pass on which the condition fails resets the dwell to zero; the pass on
/// which it reaches [`TILE_SNAP_DWELL_PASSES`] flips the state and starts the
/// other direction's dwell from zero.
pub fn snap_decision(prev: SnapState, reading: SnapReading, pass_nr: u64) -> SnapState {
    if prev.last_pass.is_some_and(|last| pass_nr <= last) {
        return prev;
    }
    let toward_flip = if prev.snapped {
        !reading.whole_zoom_rung
            && reading.working_set_overrun_bytes == 0
            && fits_with_margin(reading.unsnapped_bytes, reading.budget_bytes)
    } else {
        reading.whole_zoom_rung || reading.working_set_overrun_bytes > 0
    };
    let dwell = if toward_flip {
        prev.dwell.saturating_add(1)
    } else {
        0
    };
    let flip = dwell >= TILE_SNAP_DWELL_PASSES;
    SnapState {
        snapped: prev.snapped ^ flip,
        dwell: if flip { 0 } else { dwell },
        last_pass: Some(pass_nr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: u64 = 48 << 20;

    /// Step `state` over `passes` consecutive passes starting after its last
    /// one, on the same reading.
    fn run(mut state: SnapState, reading: SnapReading, passes: u32) -> SnapState {
        let first = state.last_pass.map_or(1, |p| p + 1);
        for pass in first..first + u64::from(passes) {
            state = snap_decision(state, reading, pass);
        }
        state
    }

    fn calm() -> SnapReading {
        SnapReading {
            whole_zoom_rung: false,
            working_set_overrun_bytes: 0,
            unsnapped_bytes: BUDGET / 2,
            budget_bytes: BUDGET,
        }
    }

    /// **The rung alone arms, and the dwell is the whole delay.** Fourteen
    /// passes of the rung leave the source drawing sharp; the fifteenth snaps.
    #[test]
    fn the_scene_rung_arms_and_snaps_after_the_dwell() {
        let rung = SnapReading {
            whole_zoom_rung: true,
            ..calm()
        };
        let short = run(SnapState::default(), rung, TILE_SNAP_DWELL_PASSES - 1);
        assert!(!short.snapped(), "snapped a pass early: {short:?}");
        assert_eq!(short.dwell(), TILE_SNAP_DWELL_PASSES - 1);
        let full = run(short, rung, 1);
        assert!(
            full.snapped(),
            "the dwell passed and nothing snapped: {full:?}"
        );
        assert_eq!(full.dwell(), 0, "the release dwell starts from zero");
    }

    /// **A measured overrun arms on its own**, with the rung off — a byte of
    /// working set past the allowance is enough, held for the dwell.
    #[test]
    fn a_measured_overrun_arms_and_snaps_after_the_dwell() {
        let overrun = SnapReading {
            working_set_overrun_bytes: 1,
            unsnapped_bytes: BUDGET + 1,
            ..calm()
        };
        let short = run(SnapState::default(), overrun, TILE_SNAP_DWELL_PASSES - 1);
        assert!(!short.snapped());
        assert!(run(short, overrun, 1).snapped());
    }

    /// **An interrupted dwell starts over.** Ten passes armed, one calm, ten
    /// armed: twenty of twenty-one passes wanted the snap and none landed,
    /// because the condition has to hold on every one of fifteen in a row.
    #[test]
    fn a_pass_without_the_condition_resets_the_dwell() {
        let rung = SnapReading {
            whole_zoom_rung: true,
            ..calm()
        };
        let ten = run(SnapState::default(), rung, 10);
        assert_eq!(ten.dwell(), 10);
        let broken = run(ten, calm(), 1);
        assert_eq!(broken.dwell(), 0, "one calm pass did not reset the dwell");
        let again = run(broken, rung, 10);
        assert!(!again.snapped(), "a broken dwell of 10 + 10 counted as 15");
        assert_eq!(again.dwell(), 10);
    }

    /// **Release needs both inputs gone, and the dwell.** With the rung still
    /// on the fitting projection releases nothing; with the rung off and the
    /// projection over the margin, nothing; with both, fourteen passes hold
    /// and the fifteenth releases.
    #[test]
    fn release_needs_the_rung_off_and_the_unsnapped_set_fitting_for_the_dwell() {
        let start = SnapState::snapped_at(100);

        let rung_still_on = SnapReading {
            whole_zoom_rung: true,
            ..calm()
        };
        let held = run(start, rung_still_on, 3 * TILE_SNAP_DWELL_PASSES);
        assert!(held.snapped(), "released while the rung was still taken");
        assert_eq!(held.dwell(), 0);

        let still_too_big = SnapReading {
            unsnapped_bytes: BUDGET,
            ..calm()
        };
        let held = run(start, still_too_big, 3 * TILE_SNAP_DWELL_PASSES);
        assert!(held.snapped(), "released a set that fits with no margin");

        let short = run(start, calm(), TILE_SNAP_DWELL_PASSES - 1);
        assert!(short.snapped(), "released a pass early");
        assert_eq!(short.dwell(), TILE_SNAP_DWELL_PASSES - 1);
        let released = run(short, calm(), 1);
        assert!(
            !released.snapped(),
            "both conditions held for the dwell and nothing released"
        );
        assert_eq!(released.dwell(), 0);
    }

    /// **An overrun of the snapped set itself holds the snap**, whatever the
    /// projection says — the projection is an estimate and the overrun is a
    /// measurement, and the measurement wins.
    #[test]
    fn an_overrun_while_snapped_blocks_release() {
        let overrun_but_projected_to_fit = SnapReading {
            working_set_overrun_bytes: 1,
            ..calm()
        };
        let held = run(
            SnapState::snapped_at(0),
            overrun_but_projected_to_fit,
            3 * TILE_SNAP_DWELL_PASSES,
        );
        assert!(held.snapped());
        assert_eq!(held.dwell(), 0);
    }

    /// **The margin is a quarter, on the budget's side.** Four fifths of the
    /// allowance fits; one byte more does not; the whole allowance does not.
    /// The boundary is `budget * 4 / 5` exactly -- 48 MiB is not a multiple of
    /// five, so it is computed and not restated as `budget / 5 * 4`.
    #[test]
    fn the_release_margin_is_four_fifths_of_the_allowance() {
        let four_fifths = BUDGET * 4 / 5;
        assert!(fits_with_margin(four_fifths, BUDGET));
        assert!(!fits_with_margin(four_fifths + 1, BUDGET));
        assert!(!fits_with_margin(BUDGET, BUDGET));
        assert!(
            fits_with_margin(0, 0),
            "an empty set fits an empty allowance"
        );
        assert!(
            !fits_with_margin(u64::MAX, BUDGET),
            "a set too large to price saturated into a fit against a real allowance"
        );
        // And the constant is the loop pool's 1.25, restated as a fraction.
        let (n, d) = TILE_SNAP_RELEASE_HYSTERESIS;
        assert_eq!(n * 100 / d, 125);
        assert_eq!(
            TILE_SNAP_DWELL_PASSES,
            squallar_device_profile::constants::LOOP_POOL_DWELL_FRAMES,
            "the two dwells are the same figure by design; move both or argue the split"
        );
    }

    /// **A pass is stepped once.** Every pane drawing the source calls the
    /// decision with the same pass number; the second and later calls return
    /// the state unchanged, and a pass number that goes backwards is ignored
    /// rather than counted as a fresh pass.
    #[test]
    fn a_repeated_or_earlier_pass_number_steps_nothing() {
        let rung = SnapReading {
            whole_zoom_rung: true,
            ..calm()
        };
        let mut state = SnapState::default();
        for _ in 0..(3 * TILE_SNAP_DWELL_PASSES) {
            state = snap_decision(state, rung, 7);
        }
        assert_eq!(
            state.dwell(),
            1,
            "three panes in one pass counted as three passes"
        );
        assert!(!state.snapped());

        let earlier = snap_decision(state, rung, 3);
        assert_eq!(earlier, state, "an earlier pass number moved the state");

        let next = snap_decision(state, rung, 8);
        assert_eq!(next.dwell(), 2);
    }

    /// **A fresh state has no pass yet**, so its first step counts whatever
    /// number egui hands it, including zero.
    #[test]
    fn the_first_step_counts_whatever_pass_number_arrives() {
        let rung = SnapReading {
            whole_zoom_rung: true,
            ..calm()
        };
        assert_eq!(snap_decision(SnapState::default(), rung, 0).dwell(), 1);
    }
}
