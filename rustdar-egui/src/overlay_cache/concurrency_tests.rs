//! How many rasters may be crossing at once, and what a result that arrives
//! for a dispatch the cache has moved past is worth.

use super::*;
use crate::overlay_cache::coverage_dispatch_tests::{PanRig, WARMUP};

/// A viewport for `east` degrees east of the origin, one degree square — the
/// same fixture geometry `coverage_dispatch_tests` pans over.
fn vp(east: f64) -> GeoBounds {
    GeoBounds {
        min_lat: 34.5,
        max_lat: 35.5,
        min_lon: -97.5 + east,
        max_lon: -96.5 + east,
    }
}

// ── The bound, and what it can never admit ───────────────────────────────

/// **A one-destination layer admits one raster at every budget, and that is the
/// livelock guard rather than an accident of today's default.**
///
/// [`OverlayTextureCache::hold`] replaces rather than queues, so a second
/// raster in flight for the same destination reaches the screen only by
/// destroying the first's upload — the freeze
/// `coverage_dispatch_tests::a_fling_the_pipeline_cannot_follow_still_puts_pictures_on_screen`
/// measures, where 300 rasters were spent, all 300 thrown away mid-upload and
/// none promoted. Raising `concurrent_renders` must therefore buy a
/// whole-picture layer nothing at all.
#[test]
fn one_destination_admits_one_raster_at_every_budget() {
    for limit in 1..=8 {
        let mut renders = RendersInFlight::default();
        assert!(
            renders.admits(RenderSlot::WHOLE, limit),
            "an empty cache refused the first raster at a budget of {limit}",
        );
        renders.record(RenderTicket::whole(1, vp(0.0)));
        assert!(
            !renders.admits(RenderSlot::WHOLE, limit),
            "a budget of {limit} admitted a second raster for the destination \
             that already had one: its arrival would throw away the first's \
             upload, and past the sustainable pan that closes into a freeze",
        );
        assert_eq!(
            renders.len(),
            1,
            "a one-destination cache reached {} outstanding rasters at a budget \
             of {limit}",
            renders.len(),
        );
    }
}

/// The second conjunct — the device budget — and it must actually bind: a
/// layer with more destinations than the budget cannot have them all out.
#[test]
fn the_budget_bounds_how_many_destinations_may_be_out() {
    for limit in 1..=6 {
        let mut renders = RendersInFlight::default();
        let mut admitted = 0;
        for n in 0..12u32 {
            let slot = RenderSlot::nth(n);
            if renders.admits(slot, limit) {
                renders.record(RenderTicket::for_slot(slot, 1, vp(f64::from(n) * 0.1)));
                admitted += 1;
            }
        }
        assert_eq!(
            admitted, limit,
            "a budget of {limit} admitted {admitted} of 12 distinct \
             destinations",
        );
        assert_eq!(renders.len(), limit, "and holds what it admitted");
    }
}

/// Retiring frees the slot it was holding, and only that one.
#[test]
fn retiring_one_destination_frees_exactly_one_place() {
    let limit = 3;
    let mut renders = RendersInFlight::default();
    let tickets: Vec<RenderTicket> = (0..3)
        .map(|n| RenderTicket::for_slot(RenderSlot::nth(n), 1, vp(f64::from(n) * 0.1)))
        .collect();
    for t in &tickets {
        renders.record(*t);
    }
    assert!(
        !renders.admits(RenderSlot::nth(9), limit),
        "fixture: the budget must be full, or the free below proves nothing",
    );

    assert!(renders.retire(&tickets[1]), "the cache was waiting for it");
    assert!(
        renders.admits(RenderSlot::nth(9), limit),
        "retiring one raster did not give its place back",
    );
    assert!(
        renders.holds(RenderSlot::nth(0)) && renders.holds(RenderSlot::nth(2)),
        "retiring one destination let go of another's mark too",
    );
}

// ── The stale-result policy ──────────────────────────────────────────────

/// **A raster is accepted only while it is still the one the cache asked for.**
///
/// With more than one outstanding, results arrive out of order and for
/// viewports the pane has left. This is the rule that decides them, and it is
/// the same rule at one: a dispatch the cache has moved past is refused, and
/// the caller drops the picture rather than holding it.
#[test]
fn a_raster_for_a_dispatch_the_cache_has_moved_past_is_refused() {
    let mut renders = RendersInFlight::default();
    let first = RenderTicket::whole(1, vp(0.0));
    renders.record(first);

    // The pane moved and was dispatched for again — which the app does by
    // marking unconditionally, so this is `record`, not a second `admits`.
    let second = RenderTicket::whole(1, vp(0.5));
    renders.record(second);

    assert!(
        !renders.retire(&first),
        "the raster for the viewport the pane has left was accepted: it would \
         be held, and its upload would then be thrown away by the newer one — \
         which is the supersede the coverage brake exists to ration",
    );
    assert!(
        renders.holds(RenderSlot::WHOLE),
        "refusing the stale raster took the live dispatch's mark with it: the \
         destination is now free to be dispatched for a second time while its \
         real raster is still flying",
    );
    assert!(
        renders.retire(&second),
        "and the raster the cache is actually waiting for is still accepted",
    );
    assert!(renders.is_empty(), "which ends the dispatch");
}

/// A raster whose mark was **abandoned** while it flew is refused the same way.
/// That is the pane-moved-index case and the renderer-rebuilt case: the answer
/// cannot be filed, because what asked for it is gone.
#[test]
fn a_raster_whose_mark_was_abandoned_is_refused() {
    let mut renders = RendersInFlight::default();
    let ticket = RenderTicket::whole(1, vp(0.0));
    renders.record(ticket);
    renders.abandon_all();
    assert!(
        !renders.retire(&ticket),
        "a raster nothing was waiting for was accepted",
    );
}

/// The token is part of the identity too, not only the ground. A rebuild for
/// *new data* over the same viewport supersedes the old dispatch, and the old
/// raster must not be filed as the answer to it.
#[test]
fn the_token_is_part_of_the_dispatch_identity() {
    let mut renders = RendersInFlight::default();
    let old = RenderTicket::whole(1, vp(0.0));
    renders.record(old);
    let fresh = RenderTicket::whole(2, vp(0.0));
    renders.record(fresh);
    assert!(
        !renders.retire(&old),
        "a raster of the previous round's data was accepted for a dispatch \
         made at a newer token: the pane would draw the old picture and stop \
         asking",
    );
    assert!(renders.retire(&fresh), "and the fresh one still is");
}

// ── No regression at the default ─────────────────────────────────────────

/// The speed axis `PAN_REBUILD_THRESHOLD`'s own note is swept on: 56 speeds
/// from 0.25 to 3.0 viewports per second, at 60 Hz.
fn speeds() -> Vec<f64> {
    (0..56)
        .map(|i| 0.25 + (3.0 - 0.25) * f64::from(i) / 55.0)
        .collect()
}

/// One viewport is one degree of the fixture geometry, so a speed in viewports
/// per second is degrees per frame at 60 Hz.
fn step_for(viewports_per_sec: f64) -> f64 {
    viewports_per_sec / 60.0
}

/// Frames counted per speed, after the warm-up the rig discards.
const COUNTED: u32 = 600;

/// What one whole sweep at `limit` costs, over 56 speeds x 600 counted frames
/// = 33,600 frames: dry frames, rasters spent, pictures promoted, uploads
/// thrown away.
fn sweep(ctx: &egui::Context, limit: usize, raster_frames: u32) -> Sweep {
    let mut total = Sweep::default();
    for s in speeds() {
        let mut rig =
            PanRig::at_limit(ctx, step_for(s), 3, limit).with_raster_frames(raster_frames);
        rig.run(WARMUP + COUNTED);
        total.dry += rig.dry;
        total.counted += rig.counted;
        total.dispatches += rig.dispatches;
        total.promotions += rig.promotions;
        total.superseded += rig.superseded;
        total.discarded += rig.discarded;
    }
    total
}

/// Everything one sweep counts. Compared whole: a budget that changed *any* of
/// these changed what the layer does.
#[derive(Default, PartialEq, Eq, Debug)]
struct Sweep {
    dry: u32,
    counted: u32,
    dispatches: u32,
    promotions: u32,
    /// Uploads thrown away by a later raster landing on top of them.
    superseded: u32,
    /// Rasters dropped on arrival because the cache had moved past them.
    discarded: u32,
}

/// **Raising the budget changes nothing a whole-picture layer does**, frame for
/// frame, over the whole published speed axis.
///
/// This is the no-regression proof and the livelock proof in one measurement.
/// The rig is `coverage_dispatch_tests`' own, rewired onto
/// [`RendersInFlight::admits`] — so it is the real gate deciding every
/// dispatch, at four budgets including the one the wasm arm ships — and every
/// counted quantity is identical, not merely close. Denominator: 56 pan speeds
/// from 0.25 to 3.0 viewports/second at 60 Hz, 600 counted frames each =
/// 33,600 frames per budget, one texture layer on one pane, raster one frame,
/// upload three.
///
/// Measured 2026-08-22, at budgets 1, 2, 4 and 8 alike: **3200 dry frames of
/// 33,600 (9.5238%)**, 6473 rasters spent, 6470 pictures promoted, 0 uploads
/// thrown away. The dry figure is the one `PAN_REBUILD_THRESHOLD`'s own table
/// carries for a half at a three-frame upload — 9.5% — which is corroboration
/// and not a restatement: that sweep was run at the real 2880x1620 plan and
/// this one at this module's 8x5 fixture.
///
/// **Tampered 2026-08-22**, by deleting the `!self.holds(slot)` conjunct from
/// [`RendersInFlight::admits`] and leaving everything else alone. On the
/// two-frame arm at a budget of 2 the pane went from 7630 dry frames of 33,600
/// and 6470 pictures promoted to **33,441 dry and none promoted at all**,
/// spending 33,581 rasters and dropping 33,574 of them on arrival. That is the
/// freeze, measured: the stale-result policy on [`RendersInFlight::retire`]
/// saves the *upload* — nothing is superseded — but it cannot save the screen,
/// because every raster is superseded before it lands. The admission conjunct
/// is what keeps the pane drawing, and this is the number that says so.
#[test]
fn the_published_pan_sweep_is_identical_at_every_budget() {
    let ctx = egui::Context::default();
    let base = sweep(&ctx, 1, 1);
    let Sweep {
        dry,
        counted,
        dispatches,
        promotions,
        ..
    } = base;

    // Controls first, or four equal numbers below are four zeros agreeing.
    assert_eq!(
        counted,
        56 * COUNTED,
        "the denominator moved: {counted} frames counted where 56 x {COUNTED} \
         were asked for",
    );
    assert!(
        dry > 0,
        "control: this axis must contain speeds the pipeline cannot follow, or \
         the equality below is 'nothing happened' four times",
    );
    assert!(dry < counted, "control: and speeds it can, or the same",);
    assert!(
        dispatches > 0 && promotions > 0,
        "control: the rig must spend rasters and put pictures on screen",
    );

    // **A pipeline the gate is actually consulted on.** At a one-frame raster
    // nothing is ever still outstanding when the next frame asks, so `admits`
    // is answered against an empty set every time and the equality below would
    // hold whatever it returned. Two frames is the shallowest pipeline where a
    // raster is in flight at gate time, and it is where the per-destination
    // conjunct is the only thing standing between this layer and a second
    // raster whose arrival throws away the first's upload.
    let deep = sweep(&ctx, 1, 2);
    assert!(
        deep.dispatches > 0 && deep.promotions > 0,
        "control: the two-frame pipeline must spend rasters and promote \
         pictures, or the equality below is about a rig that does nothing",
    );

    for limit in [2, 4, 8] {
        assert_eq!(
            sweep(&ctx, limit, 2),
            deep,
            "a budget of {limit} changed what a whole-picture layer does on a \
             two-frame raster, where the gate really is asked with one in \
             flight. Baseline at 1: {deep:?}",
        );
        assert_eq!(
            sweep(&ctx, limit, 1),
            base,
            "a budget of {limit} changed what a whole-picture layer does. It \
             has one destination, so every extra place the budget offers is one \
             the livelock guard must refuse — see `RenderSlot`. Baseline at 1: \
             {dry} dry of {counted}, {dispatches} rasters, {promotions} \
             promoted.",
        );
    }
}

// ── What the budget is for: more than one destination ────────────────────

/// A destination's picture in the rig below: whether it has reached the screen,
/// and the frame its raster lands on if one is out.
///
/// Deliberately not an [`OverlayTextureCache`]: the cache holds one destination
/// and the grid that will hold many does not exist yet, so what is measured
/// here is the **admission bound**, not the promote protocol. The promote
/// protocol is measured at one destination by
/// `the_published_pan_sweep_is_identical_at_every_budget` above.
#[derive(Clone, Copy, Default)]
struct GridCell {
    drawn: bool,
    arrives: Option<u32>,
}

/// A 512-pixel grid under a 1920x1080 pane panned east, with one cell of
/// prefetch — the shape WS2 measured the tile overlay at.
struct GridRig {
    cells: std::collections::HashMap<(i32, i32), GridCell>,
    renders: RendersInFlight,
    limit: usize,
    /// Viewport widths per frame, in tile units.
    step: f64,
    /// Frames between a dispatch and its raster landing: one to rasterise,
    /// three to upload — the depth the published dry table's third row is at.
    pipeline: u32,
    pub dry: u32,
    pub counted: u32,
    pub dispatches: u32,
    /// The most destinations outstanding at any one moment.
    pub peak_outstanding: usize,
}

/// A 1920-point pane at 512-pixel tiles.
const COLS_PER_VIEWPORT: f64 = 1920.0 / 512.0;
/// A 1080-point pane at the same: 2.11 tiles, so three rows are on screen.
const ROWS: i32 = 3;
/// Cells reachable per axis beyond the viewport — WS2's "prefetch 1".
const PREFETCH: i32 = 1;
/// Frames run before anything is counted, so a cold grid is not a dry read.
const GRID_WARMUP: u32 = 120;

impl GridRig {
    fn new(viewports_per_sec: f64, limit: usize) -> Self {
        Self {
            cells: std::collections::HashMap::new(),
            renders: RendersInFlight::default(),
            limit,
            step: viewports_per_sec * COLS_PER_VIEWPORT / 60.0,
            pipeline: 4,
            dry: 0,
            counted: 0,
            dispatches: 0,
            peak_outstanding: 0,
        }
    }

    /// A cell's slot index. Columns are unbounded eastward, so the index is the
    /// column strided by the row count — distinct per cell, which is all the
    /// bound needs.
    fn slot(col: i32, row: i32) -> RenderSlot {
        RenderSlot::nth((col.rem_euclid(1 << 20) * ROWS + row) as u32)
    }

    fn ticket(col: i32, row: i32) -> RenderTicket {
        RenderTicket::for_slot(Self::slot(col, row), 1, vp(f64::from(col)))
    }

    fn run(&mut self, frames: u32) {
        for f in 0..frames {
            let west = f as f64 * self.step;
            let first = west.floor() as i32;
            let last = (west + COLS_PER_VIEWPORT).floor() as i32;

            // Arrivals, before anything is asked for — the order the app's
            // frame runs them in.
            for col in (first - PREFETCH)..=(last + PREFETCH) {
                for row in 0..ROWS {
                    let Some(cell) = self.cells.get_mut(&(col, row)) else {
                        continue;
                    };
                    if cell.arrives.is_some_and(|at| f >= at) {
                        cell.arrives = None;
                        let landed = self.renders.retire(&Self::ticket(col, row));
                        assert!(
                            landed,
                            "the rig's own raster for ({col},{row}) was refused \
                             as stale, and nothing here abandons a mark",
                        );
                        cell.drawn = true;
                    }
                }
            }

            // Ask, nearest the viewport first, so a starved grid spends its
            // places on what is about to be looked at.
            for col in (first - PREFETCH)..=(last + PREFETCH) {
                for row in 0..ROWS {
                    let cell = self.cells.entry((col, row)).or_default();
                    if cell.drawn || cell.arrives.is_some() {
                        continue;
                    }
                    let slot = Self::slot(col, row);
                    if !self.renders.admits(slot, self.limit) {
                        continue;
                    }
                    self.renders.record(Self::ticket(col, row));
                    cell.arrives = Some(f + self.pipeline);
                    if f >= GRID_WARMUP {
                        self.dispatches += 1;
                    }
                }
            }
            self.peak_outstanding = self.peak_outstanding.max(self.renders.len());
            assert!(
                self.renders.len() <= self.limit,
                "{} rasters outstanding against a budget of {}",
                self.renders.len(),
                self.limit,
            );

            if f >= GRID_WARMUP {
                self.counted += 1;
                // Dry: some cell the viewer is looking at has no picture. The
                // prefetch ring is not counted — it is not on screen.
                let blank =
                    (first..=last).any(|col| (0..ROWS).any(|row| !self.cells[&(col, row)].drawn));
                if blank {
                    self.dry += 1;
                }
            }
        }
    }
}

/// One whole sweep of the grid at `limit`: dry frames, counted frames, and the
/// most destinations that were ever outstanding at once.
fn grid_sweep(limit: usize) -> (u32, u32, usize) {
    let (mut dry, mut counted, mut peak) = (0, 0, 0);
    for s in speeds() {
        let mut rig = GridRig::new(s, limit);
        rig.run(GRID_WARMUP + COUNTED);
        dry += rig.dry;
        counted += rig.counted;
        peak = peak.max(rig.peak_outstanding);
    }
    (dry, counted, peak)
}

/// **What the bound is for.** A layer with one destination per tile is starved
/// by a one-raster ceiling and is not starved by four, and the *only* thing
/// that differs between the two runs is the number the gate is handed.
///
/// Denominator: 56 pan speeds from 0.25 to 3.0 viewports/second at 60 Hz, 600
/// counted frames each = 33,600 frames per budget; a 1920x1080 pane at
/// 512-pixel tiles (3.75 x 2.11, so three rows), one tile of prefetch, four
/// frames from dispatch to pixels — one to rasterise, three to upload.
///
/// The figures are this rig's, not WS2's: what is shared with the published
/// table is the geometry and the axis, not the dispatcher, and no number here
/// is a restatement of one measured elsewhere. Measured 2026-08-22, dry frames
/// of 33,600:
///
/// | budget | 1     | 2     | 3    | 4    | 6    |
/// |--------|-------|-------|------|------|------|
/// | dry %  | 69.30 | 37.51 | 0.00 | 0.00 | 0.00 |
///
/// **Three is where this grid stops starving**, and the ceiling the tree ships
/// on wasm is one. WS2's own sweep of the real predicate put a 512-pixel grid
/// at 56.32% dry at a concurrency of 1 and 0.00% at 4, on a rig with a
/// different pipeline; the two agree on the shape and on the conclusion, and
/// their percentages are not comparable.
#[test]
fn a_tiled_layer_is_starved_at_one_raster_and_is_not_at_four() {
    let (dry_1, counted, peak_1) = grid_sweep(1);
    let (dry_4, counted_4, peak_4) = grid_sweep(4);

    assert_eq!(counted, 56 * COUNTED, "the denominator moved");
    assert_eq!(
        counted_4, counted,
        "and both budgets counted the same frames"
    );

    // The bound is real at both, and it binds at both — a budget nothing ever
    // reaches would make the comparison below meaningless.
    assert_eq!(peak_1, 1, "a budget of 1 reached {peak_1} outstanding");
    assert_eq!(peak_4, 4, "a budget of 4 never reached its own ceiling");

    // Control: the ceiling under test must actually starve the pane, or the
    // improvement is measured against a rig that was never in trouble.
    assert!(
        dry_1 * 10 > counted,
        "control: a one-raster ceiling left the grid dry on only {dry_1} of \
         {counted} frames, so this axis does not reach the speeds the ceiling \
         cannot follow and the comparison below is not about starvation",
    );

    assert!(
        dry_4 * 4 < dry_1,
        "four concurrent destinations bought less than a 4x reduction in dry \
         frames: {dry_1} of {counted} at a budget of 1, {dry_4} at 4. The tile \
         grid cannot land on a dispatcher that admits one raster per pane and \
         layer, and this is the measurement that says so.",
    );
}

// ── Where the number comes from ──────────────────────────────────────────

/// **The budget the UI admits against is the one the device resolved**, and a
/// `Gui` nobody has pushed facts into is not a `Gui` with no render budget.
///
/// The frontend pushes `Budgets::concurrent_renders` through `FrameInputs`
/// every frame; the default underneath it is the compile-time arm of the same
/// axis. The two are pinned together here through a *different expression* than
/// either initialiser uses — the bracket resolved off this target's own
/// `DeviceProfile` — so this cannot pass by both sides reading the same
/// constant.
#[test]
fn the_default_render_budget_is_this_device_s_own() {
    let resolved = rustdar_device_profile::budget::resolve(
        &rustdar_device_profile::budget::DeviceProfile::for_target(),
    )
    .concurrent_renders;
    assert!(
        resolved > 0,
        "this target resolved a render budget of zero, which admits no overlay \
         raster at all: the pane would never redraw",
    );
    assert_eq!(
        crate::input_harness::InputHarness::new()
            .gui()
            .concurrent_renders_for_test(),
        resolved,
        "the `Gui` default and the budget the App pushes are different numbers, \
         so a frame before the first push admits a different count than every \
         frame after it",
    );
}
