use super::*;
use crate::budget::{BudgetLimits, Promotion, demote};
use crate::constants::{
    DESKTOP_APP_TEXTURE_BUDGET_BYTES, DESKTOP_LOOP_IMAGE_SIZE, DESKTOP_MAX_LOOP_RENDER_BUDGET,
    DESKTOP_RASTER_SIDE_CEILING, DESKTOP_VOLUME_GRID_CELLS, MIN_LOOP_FRAMES_PER_PANE,
    MOBILE_APP_TEXTURE_BUDGET_BYTES, MOBILE_MAX_LOOP_RENDER_BUDGET, MOBILE_RASTER_SIDE_CEILING,
    MOBILE_VOLUME_GRID_CELLS, WASM_APP_TEXTURE_BUDGET_BYTES, WASM_LOOP_IMAGE_SIZE,
    WASM_MAX_LOOP_RENDER_BUDGET, WASM_RASTER_SIDE_CEILING, WASM_VOLUME_GRID_CELLS,
};
use crate::constants::{LOOP_SCAN_RESERVE_BYTES, OVERLAY_OVERSAMPLE_PERCENTS};
use crate::quality::{DeviceClass, GradientShading, ResolutionRung};
use crate::scene::fixtures::{
    HUGE_LEG_SCAN_BYTES, huge, huge_level3, huge_pending, plan_pane, scene_table, shipped_profile,
    stand_in_grid_bytes, two_panes_one_loop, two_panes_one_site, volume_pane,
};
use crate::scene::{CapacitySource, OverlayGridNeed, TileNeed};
use squallar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

const MIB: u64 = 1024 * 1024;
const HD: [u32; 2] = [1920, 1080];
const TWO_HOURS: usize = 2 * 60 * 60;
/// The WSR-88D precipitation cadence, measured.
const PRECIP: Option<u32> = Some(259);

fn scene_of(panes: Vec<PaneNeed>) -> Scene {
    Scene {
        panes,
        tile_sources: Vec::new(),
        mirror_px: [0, 0],
        overlay_grids: Vec::new(),
    }
}

fn desktop() -> Budgets {
    resolve(&shipped_profile(BudgetLimits::DESKTOP))
}

/// Nothing on screen costs nothing, and a scene that fits leaves the class rung
/// exactly where the resolver put it, on every bracket.
#[test]
fn an_empty_scene_costs_nothing_and_fits_at_the_class_rung() {
    for limits in BudgetLimits::SHIPPED {
        let profile = shipped_profile(limits);
        let b = resolve(&profile);
        assert_eq!(
            need(&Scene::empty(), &b, stand_in_grid_bytes),
            Need::default(),
            "{}",
            limits.name,
        );
        let cap = Capacity::presumed(&limits);
        assert_eq!(
            fit(&Scene::empty(), &profile, &cap, stand_in_grid_bytes),
            b,
            "{}: an empty scene moved the budgets off the class rung",
            limits.name,
        );
    }
}

/// **Every term is a call to a cost function the tree already had**, and
/// nothing else: each single-term scene prices to exactly that function's
/// answer, with every other term zero.
#[test]
fn every_term_is_the_cost_function_it_reuses() {
    let b = desktop();
    let grid = stand_in_grid_bytes(b.grid_cells).unwrap() as u64;
    let terms = |scene: &Scene| need_terms(scene, &b, stand_in_grid_bytes);

    // A plan-view pane's static render: the raster ceiling's worst case —
    // and the decoded volume it is parked at, which is what a 2D pane running
    // no radar loop is holding.
    let plan = terms(&scene_of(vec![plan_pane(HD, false, TWO_HOURS, None)]));
    assert_eq!(
        plan,
        NeedTerms {
            static_rasters: b.static_frame_cost().gpu as u64,
            render_peak_host: b.static_frame_cost().host_peak() as u64,
            still_scans_host: LOOP_SCAN_RESERVE_BYTES,
            ..NeedTerms::default()
        },
    );
    assert_eq!(
        plan.static_rasters,
        256 * MIB,
        "8192^2 x 4 B of Rgba8 texture on the desktop class"
    );
    // The other side of the same render, on the other memory: the raster with
    // the claim buffer that painted it still alive. 8192^2 x (4 + 8) B, and
    // the only term of the two the GPU never sees. It was 16 B a pixel until
    // 2026-09-08, when the value grid between them went for being written once
    // a pixel and never read.
    assert_eq!(
        plan.render_peak_host,
        768 * MIB,
        "8192^2 x 12 B of host at the instant the render runs"
    );
    assert_eq!(
        plan.total().host_bytes,
        plan.render_peak_host + plan.still_scans_host,
    );

    // A cross-section pane's static render: the section frame.
    let section = terms(&scene_of(vec![PaneNeed {
        view: RenderView::CrossSection,
        ..plan_pane(HD, false, TWO_HOURS, None)
    }]));
    assert_eq!(section.static_rasters, b.section_frame_cost().gpu as u64);
    assert_eq!(
        section.render_peak_host,
        b.section_frame_cost().host_peak() as u64,
    );

    // A radar loop: the pane's span at its cadence, held to the render budget,
    // at the loop frame's cost.
    let looping = terms(&scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]));
    let frames = b.frames_for_span_of(TWO_HOURS, PRECIP);
    assert_eq!(
        frames, 28,
        "1 + 7200 / 259 frames, under the 36 the budget caps at"
    );
    assert_eq!(
        looping.loops,
        frames as u64 * b.loop_frame_cost().gpu as u64,
    );
    assert_eq!(looping.static_rasters, plan.static_rasters);
    // And one decoded volume per frame on the host, at the reserve — a bare
    // loop's only host term, since it shows no picture and pans no tiles.
    assert_eq!(
        looping.loop_scans_host,
        frames as u64 * LOOP_SCAN_RESERVE_BYTES
    );
    assert_eq!(looping.loop_scans_host, 28 * 80 * MIB);
    // The render peak rides beside it: a looping pane still holds a static
    // render, and its host peak is the same one the still pane's is.
    assert_eq!(looping.render_peak_host, plan.render_peak_host);
    assert_eq!(
        looping.total().host_bytes,
        looping.loop_scans_host + looping.render_peak_host,
    );
    assert_eq!(plan.loop_scans_host, 0, "a still pane plays from no cache");
    // A pane asking for less than the budget's span gets less; one asking
    // for more is held to **what the capacity reaches**, not to the bracket's
    // compiled minutes; no cadence yet buys the whole render budget.
    assert_eq!(b.frames_for_span_of(30 * 60, PRECIP), 1 + 1800 / 259);
    // **Moved 2026-09-06, ruling 13.** This read `b.frames_for_span(PRECIP)`
    // — the pane's twenty-four hours cut to the bracket's two, 28 frames —
    // because `frames_for_span_of` opened `span_secs.min(self.loop_span_secs)`.
    // The user's span setting is tier 1 and no constant of this crate's
    // shortens it: the request is 1 + 86400 / 259 = 334 frames and what
    // answers is the reachable ceiling, which on this unmeasured profile is
    // the class figure.
    assert_eq!(
        b.frames_requested_for_span_of(24 * 60 * 60, PRECIP),
        1 + 86400 / 259
    );
    assert_eq!(
        b.frames_for_span_of(24 * 60 * 60, PRECIP),
        b.loop_render_budget
    );
    assert!(
        b.frames_for_span_of(24 * 60 * 60, PRECIP) > b.frames_for_span(PRECIP),
        "the bracket's own span is no longer a ceiling on the user's",
    );
    assert_eq!(b.frames_for_span_of(TWO_HOURS, None), b.loop_render_budget);

    // A loop of a layer that is not radar: the frame the pane measured.
    let overlay = terms(&scene_of(vec![PaneNeed {
        overlay_frame_bytes: 18_662_400,
        cadence_secs: Some(3600),
        ..plan_pane(HD, true, TWO_HOURS, None)
    }]));
    assert_eq!(
        overlay.loops,
        3 * 18_662_400,
        "three hourly frames cover two hours, at the planner's own 2880 x 1620 x 4 B",
    );
    assert_eq!(
        overlay.loop_scans_host, 0,
        "a loop of another layer holds its own rasters and no volume"
    );
    // Its loop frames cross the page heap on the way to those rasters: one
    // dispatch pass's whole burst, at this pane's frame.
    assert_eq!(
        overlay.loop_pictures_host,
        MAX_OVERLAY_LOOP_RENDERS_PER_PASS as u64 * 18_662_400,
    );
    // And radar is parked at a still behind it — the pane runs no radar loop,
    // so the scan term charges nothing and the still term charges the volume.
    assert_eq!(
        overlay.still_scans_host, LOOP_SCAN_RESERVE_BYTES,
        "a pane looping a satellite still has radar parked at a still"
    );

    // A 3D pane: its live grid, its loop as grids, and its offscreen fitted the
    // way the painter fits it. No static raster — the offscreen is its picture.
    let volume = terms(&scene_of(vec![PaneNeed {
        looping: true,
        loop_span_secs: TWO_HOURS,
        cadence_secs: PRECIP,
        ..volume_pane(HD, GroundPass::Off)
    }]));
    assert_eq!(volume.grids, grid);
    assert_eq!(volume.loops, frames as u64 * grid);
    assert_eq!(
        volume.loop_scans_host,
        frames as u64 * LOOP_SCAN_RESERVE_BYTES,
        "a 3D loop plays from the same decoded volumes",
    );
    assert_eq!(
        volume.offscreens,
        b.quality_ceiling
            .fit(HD, b.offscreen_bytes, GroundPass::Off)
            .bytes() as u64,
    );
    // `HD` here is a pane's own size, not a window's: the application reports
    // each 3D pane at what the painter last fitted its offscreen from, so a
    // pane this large is a lone pane filling the window.
    assert_eq!(
        volume.offscreens,
        1920 * 1080 * 4,
        "native resolution fits 20 MiB"
    );
    assert_eq!(volume.static_rasters, 0);
    // Ground quadruples the offscreen's bytes a pixel, and the fit steps the
    // resolution down to pay for it — the painter's own arithmetic.
    let grounded = terms(&scene_of(vec![volume_pane([2560, 1440], GroundPass::On)]));
    assert_eq!(
        grounded.offscreens,
        b.quality_ceiling
            .fit([2560, 1440], b.offscreen_bytes, GroundPass::On)
            .bytes() as u64,
    );
    assert!(grounded.offscreens <= b.offscreen_bytes as u64);

    // Buildings: the ceiling the prism ladder is fitted inside, once per pane
    // drawing them, and nothing for a pane that does not.
    let city = terms(&scene_of(vec![PaneNeed {
        buildings: true,
        ..volume_pane(HD, GroundPass::On)
    }]));
    assert_eq!(city.buildings, b.prism_vram_bytes as u64);
    assert_eq!(city.buildings, 16 * MIB, "the one machine's 16 MiB");
    assert_eq!(grounded.buildings, 0);
    assert_eq!(
        city.total().gpu_bytes,
        city.grids + city.offscreens + city.buildings,
        "the buildings term is in the GPU total",
    );

    // The mirror: a colour target of its size, held to the mirror budget.
    let mirror = terms(&Scene {
        mirror_px: [2048, 2048],
        ..Scene::empty()
    });
    assert_eq!(mirror.mirror, 2048 * 2048 * 4);
    let capped = terms(&Scene {
        mirror_px: [8192, 8192],
        ..Scene::empty()
    });
    assert_eq!(
        capped.mirror, b.mirror_bytes as u64,
        "64 MiB on the desktop class"
    );

    // Tiles: the working set at the measured entry cost, on the host.
    let tiles = terms(&Scene {
        tile_sources: vec![TileNeed {
            tiles_on_glass: 110,
            ancestor_net: 83,
            bytes_per_tile: 1_030_000,
        }],
        ..Scene::empty()
    });
    assert_eq!(tiles.tiles_host, 193 * 1_030_000);
    assert_eq!(tiles.total().gpu_bytes, 0);
    assert_eq!(tiles.total().host_bytes, 193 * 1_030_000);

    // Gridded overlays: each enabled layer's budgets as its handler states
    // them, once, on the host — MRMS's two desktop cache grids (49 MB apiece;
    // its cache keys by product and there are two) and GMGSI's four (15 MB
    // apiece, one byte a point, so 60 MB), plus what each stages beside its
    // cache while a loop runs (two grids apiece: the staged granule and the
    // pool's retained buffer).
    // Inputs, not reads of those constants: what is under test is that the
    // term sums both figures it is handed, once each.
    let gridded = terms(&Scene {
        overlay_grids: vec![
            OverlayGridNeed {
                budget_bytes: 98_000_000,
                staging_bytes: 98_000_000,
            },
            OverlayGridNeed {
                budget_bytes: 60_000_000,
                staging_bytes: 30_000_000,
            },
        ],
        ..Scene::empty()
    });
    assert_eq!(gridded.overlay_grids_host, 286_000_000);
    assert_eq!(gridded.total().gpu_bytes, 0);
    assert_eq!(gridded.total().host_bytes, 286_000_000);

    // **The staging term is summed, not absorbed into the cache budget.** The
    // same two layers with nothing staged price the cache alone, and the
    // difference between the two readings is exactly the staging handed in —
    // so a `staging_bytes` the fold dropped, or one it double-counted, moves
    // this equality rather than only the total above.
    let cache_only = terms(&Scene {
        overlay_grids: vec![
            OverlayGridNeed {
                budget_bytes: 98_000_000,
                staging_bytes: 0,
            },
            OverlayGridNeed {
                budget_bytes: 60_000_000,
                staging_bytes: 0,
            },
        ],
        ..Scene::empty()
    });
    assert_eq!(cache_only.overlay_grids_host, 158_000_000);
    assert_eq!(
        gridded.overlay_grids_host - cache_only.overlay_grids_host,
        128_000_000,
    );
}

/// **The staging term moves the ladder, and only where it should — both arms
/// of the same boundary.**
///
/// The figure is a real charge against the host allowance, so the test that
/// matters is not that the sum rose but that the ladder now answers a scene
/// differently. That has two failure directions and the second is the worse
/// one: a term that under-fires leaves a scene resident it cannot hold, a term
/// that **over**-fires sheds picture quality from a scene that always fitted,
/// on every start, with nothing to say why.
///
/// So the two scenes here differ in exactly one field. The wall is placed by
/// construction between their two prices — half a staged grid above the one
/// and half below the other — and both prices are asserted before either
/// verdict is read, so a term that stopped being summed at all would fail here
/// as a moved *premise* rather than as a passing control.
#[test]
fn the_staging_term_sheds_the_scene_it_pushes_over_and_leaves_the_one_that_fits() {
    // MRMS on the desktop arm as its handler states the two figures: a
    // two-grid key-space cache, and two more grids beside it while a loop of
    // the layer runs (the staged granule and the pool's retained buffer).
    // Inputs, not reads of that crate's constants — this crate sits under it.
    const CACHE: u64 = 98_000_000;
    const STAGING: u64 = 98_000_000;

    // The `huge` leg's canvas and tiles with its loop parked: thirteen overlay
    // pictures the ladder can actually shed, and no loop, so the two scenes
    // enter the ladder at identical budgets and the only difference between
    // their prices is the field under test.
    let base = {
        let mut scene = huge(13);
        scene.panes[0].looping = false;
        scene.panes[0].loop_scans_needed = false;
        scene.panes[0].loop_scans_resident_frames = 0;
        scene.panes[0].loop_scans_resident_bytes = 0;
        scene
    };
    let bare_scene = base.clone();
    let cache_only = Scene {
        overlay_grids: vec![OverlayGridNeed {
            budget_bytes: CACHE,
            staging_bytes: 0,
        }],
        ..base.clone()
    };
    let staged = Scene {
        overlay_grids: vec![OverlayGridNeed {
            budget_bytes: CACHE,
            staging_bytes: STAGING,
        }],
        ..base
    };

    let profile = shipped_profile(BudgetLimits::DESKTOP);
    let class = resolve(&profile);
    let priced = |scene: &Scene| need(scene, &class, stand_in_grid_bytes).host_bytes;

    // The premises, before any verdict, and both directions of them: the
    // control is its scene plus one cache budget and NOTHING else, and the
    // staged scene is the control plus the whole staging figure. The wall
    // below is placed from the bare scene's own price rather than from either
    // of these, so a term that over-charges cannot move the wall out of its
    // own way.
    let bare = priced(&bare_scene);
    assert_eq!(
        priced(&cache_only) - bare,
        CACHE,
        "the control is being charged something beside its cache budget",
    );
    assert_eq!(
        priced(&staged) - priced(&cache_only),
        STAGING,
        "the staging figure is not reaching the host total the ladder reads",
    );

    // A host wall half a staged grid above the control's price. The GPU pool
    // is large enough that nothing on that axis binds, so every verdict below
    // is the host axis's.
    let host_wall = {
        let inverse = Capacity::measured(0, None);
        inverse.gpu_bytes_for_allowance(bare + CACHE + STAGING / 2)
    };
    let cap = Capacity::measured(64 * 1024 * MIB, Some(host_wall));
    let allowance = cap.host_allowance().expect("a host wall was given");
    assert!(
        priced(&cache_only) <= allowance && allowance < priced(&staged),
        "the wall must sit between the two prices: {} <= {allowance} < {}",
        priced(&cache_only),
        priced(&staged),
    );

    // **Must not fire.** The control's price is unchanged by a staging figure
    // of zero, it is under the wall, and the ladder leaves it where the class
    // put it — no rung spent, on either axis.
    assert_eq!(
        over(&cache_only, &class, &cap, stand_in_grid_bytes),
        (false, false),
        "the control was over before the ladder ran",
    );
    let control_fit = fit(&cache_only, &profile, &cap, stand_in_grid_bytes);
    assert_eq!(control_fit.steps_back, 0, "the control shed a rung");
    assert_eq!(
        control_fit,
        admit(&cache_only, &profile, &cap, stand_in_grid_bytes),
        "the control came back off the rung admission put it on",
    );

    // **Must fire.** The same scene with the layer's staging charged is over
    // the same wall on the host axis alone, and the ladder answers it by
    // shedding — and by shedding enough, which is the point of charging it.
    assert_eq!(
        over(&staged, &class, &cap, stand_in_grid_bytes),
        (false, true),
        "the staged scene was not over the host wall at the class rung",
    );
    let shed_fit = fit(&staged, &profile, &cap, stand_in_grid_bytes);
    assert!(
        shed_fit.steps_back > 0,
        "the staged scene was priced over the wall and the ladder shed nothing",
    );
    assert_eq!(
        over(&staged, &shed_fit, &cap, stand_in_grid_bytes),
        (false, false),
        "the ladder stopped while the staged scene was still over the wall",
    );
}

/// **A loop's decoded volumes are priced at what they measured where the
/// cache holds them and at the reserve where it does not.** The scan term is
/// `resident_bytes + pending x LOOP_SCAN_RESERVE_BYTES`, and the properties
/// that make it safe to charge a bound at all:
///
/// * it never under-prices what is live — with every frame resident the term
///   is exactly the measured bytes, however small each volume was, and a
///   loop holding MORE frames than its base charges every one of them rather
///   than the base's worth;
/// * it never charges a bound for a measured thing — a resident frame's
///   price is its own, and the reserve multiplies only the shortfall;
/// * it falls monotonically down the ladder, which is what `fit` needs: the
///   resident part is fixed and only the pending count moves.
#[test]
fn a_loops_volumes_are_priced_at_their_measured_size_and_the_reserve_for_the_rest() {
    let b = desktop();
    let scans =
        |pane: &PaneNeed| need_terms_for_pane(pane, &b, stand_in_grid_bytes).loop_scans_host;
    let base = plan_pane(HD, true, TWO_HOURS, PRECIP);
    assert_eq!(loop_frames(&base, &b), 28, "1 + 7200 / 259");

    // Nothing arrived: the whole base at the reserve.
    assert_eq!(scans(&base), 28 * LOOP_SCAN_RESERVE_BYTES);

    // Eleven arrived: those eleven at their price, the other seventeen at the
    // reserve. The 48,758,784 B below is an arbitrary resident size, not a
    // corpus figure — every property this test pins holds for any volume under
    // the reserve, which is why the `tiny` case further down uses 1,000 B.
    let settling = PaneNeed {
        loop_scans_resident_frames: 11,
        loop_scans_resident_bytes: 11 * 48_758_784,
        ..base
    };
    assert_eq!(
        scans(&settling),
        11 * 48_758_784 + 17 * LOOP_SCAN_RESERVE_BYTES,
    );
    assert!(
        scans(&settling) < scans(&base),
        "a measured volume must never cost more than the bound it replaced",
    );

    // Every frame arrived: the measured bytes exactly, and a bound nowhere.
    let settled = PaneNeed {
        loop_scans_resident_frames: 28,
        loop_scans_resident_bytes: 28 * 48_758_784,
        ..base
    };
    assert_eq!(scans(&settled), 28 * 48_758_784);
    assert_eq!(scans(&settled) % 48_758_784, 0, "no reserve is folded in");

    // Volumes far under the reserve are priced at what they are, not at a
    // fraction of a bound: the term follows the measurement all the way down.
    let tiny = PaneNeed {
        loop_scans_resident_bytes: 28 * 1_000,
        ..settled
    };
    assert_eq!(scans(&tiny), 28_000);

    // **A loop holding more than its base charges all of it.** The ladder
    // shedding the span does not free a volume that is resident now; the
    // eviction that will is `retain_scans`, later and elsewhere.
    let mut shed = b;
    shed.loop_render_budget = 9;
    assert_eq!(loop_frames(&settled, &shed), 9);
    assert_eq!(
        need_terms_for_pane(&settled, &shed, stand_in_grid_bytes).loop_scans_host,
        28 * 48_758_784,
        "a rung that shortened the loop wrote off volumes that are still held",
    );

    // Monotone down the ladder: fewer named frames never costs more.
    let mut previous = u64::MAX;
    for budget in (1..=28usize).rev() {
        let mut arm = b;
        arm.loop_render_budget = budget;
        let priced = need_terms_for_pane(&settling, &arm, stand_in_grid_bytes).loop_scans_host;
        assert!(priced <= previous, "the term rose as the loop shortened");
        assert!(
            priced >= settling.loop_scans_resident_bytes,
            "the term fell under what is resident",
        );
        previous = priced;
    }

    // **A loop whose frames are rendered from Level III objects reads no
    // volume**, so its site's are dropped: no reserve is charged for a fetch
    // that will never come, and what is left is the one volume a pane parked
    // at a still there is keeping — 47.99 MiB, the peer's measured median.
    // The term is that figure at every rung, because it is one resident
    // volume and not a function of the frame count.
    let level3 = PaneNeed {
        loop_scans_needed: false,
        loop_scan_reserve_bytes: 0,
        loop_scans_resident_frames: 0,
        loop_scans_resident_bytes: 50_320_343,
        ..base
    };
    assert_eq!(scans(&level3), 50_320_343);
    let mut shortest = b;
    shortest.loop_render_budget = 2;
    assert_eq!(
        need_terms_for_pane(&level3, &shortest, stand_in_grid_bytes).loop_scans_host,
        50_320_343,
        "a rung moved a figure that is one resident volume",
    );
    // Nothing parked there: nothing at all, however many frames the loop names.
    let bare_level3 = PaneNeed {
        loop_scans_resident_bytes: 0,
        ..level3
    };
    assert_eq!(scans(&bare_level3), 0);
    assert_eq!(
        need_terms_for_pane(&bare_level3, &shortest, stand_in_grid_bytes).loop_scans_host,
        0,
    );
    // The same pane with the flag the other way is the whole reserve: the
    // flag is what the difference rests on, and it is the retention's own
    // predicate that sets it.
    assert_eq!(
        scans(&PaneNeed {
            loop_scans_needed: true,
            loop_scan_reserve_bytes: 0,
            ..bare_level3
        }),
        28 * LOOP_SCAN_RESERVE_BYTES,
    );

    // A loop of a layer that is not radar plays from no volume, whatever the
    // cache holds, and so does a pane whose site another pane counts.
    assert_eq!(
        scans(&PaneNeed {
            overlay_frame_bytes: 18_662_400,
            ..settling
        }),
        0,
    );
    assert_eq!(
        scans(&PaneNeed {
            loop_scans_shared: true,
            ..settling
        }),
        0,
    );
}

/// **Per-pane terms fold back to the whole, bit for bit**, over every
/// fixture, every bracket and every rung of the ladder: each additive term of
/// `need_terms` is the plain sum of the panes' — checked for overflow, so a
/// saturation on one side could not hide on the other — the arrival is the
/// max of the panes' candidates, and the two totals are the panes' totals
/// plus exactly the scene-level terms (the mirror on the GPU; the tiles, the
/// arrival and the overlay grids on the host). Independent arithmetic, not
/// the fold re-run: `+` and `max` here, `saturating_add` there.
#[test]
fn per_pane_terms_fold_back_to_the_whole_bit_exactly() {
    let mut scenes = scene_table();
    scenes.push(("huge(13)", huge(13)));
    for limits in BudgetLimits::SHIPPED {
        let profile = DeviceProfile {
            class: DeviceClass::Discrete,
            ..shipped_profile(limits)
        };
        for steps in 0..=9u32 {
            let mut b = resolve(&profile);
            demote(&mut b, &limits, steps);
            for (name, scene) in &scenes {
                let ctx = format!("{} / {name} / rung {steps}", limits.name);
                let whole = need_terms(scene, &b, stand_in_grid_bytes);
                let parts: Vec<PaneTerms> = scene
                    .panes
                    .iter()
                    .map(|pane| need_terms_for_pane(pane, &b, stand_in_grid_bytes))
                    .collect();
                let sum = |term: fn(&PaneTerms) -> u64| {
                    parts
                        .iter()
                        .map(term)
                        .try_fold(0u64, |acc, x| acc.checked_add(x))
                        .expect("a fixture's terms overflowed u64")
                };
                assert_eq!(whole.static_rasters, sum(|p| p.static_rasters), "{ctx}");
                assert_eq!(whole.loops, sum(|p| p.loops), "{ctx}");
                assert_eq!(whole.grids, sum(|p| p.grids), "{ctx}");
                assert_eq!(whole.offscreens, sum(|p| p.offscreens), "{ctx}");
                assert_eq!(whole.buildings, sum(|p| p.buildings), "{ctx}");
                assert_eq!(whole.pictures_host, sum(|p| p.pictures_host), "{ctx}");
                assert_eq!(whole.loop_scans_host, sum(|p| p.loop_scans_host), "{ctx}");
                assert_eq!(
                    whole.volume_grids_host,
                    sum(|p| p.volume_grids_host),
                    "{ctx}: a grid's host half adds across panes exactly as its texture does",
                );
                assert_eq!(
                    whole.render_peak_host,
                    parts.iter().map(|p| p.render_peak_host).max().unwrap_or(0),
                    "{ctx}: the render peak is a max across panes, never a sum",
                );
                assert_eq!(
                    whole.picture_arrival_host,
                    parts.iter().map(|p| p.picture_host).max().unwrap_or(0),
                    "{ctx}: the arrival is a max across panes, never a sum",
                );
                let tiles: u64 = scene
                    .tile_sources
                    .iter()
                    .map(|t| (t.tiles_on_glass + t.ancestor_net) as u64 * t.bytes_per_tile as u64)
                    .sum();
                assert_eq!(whole.tiles_host, tiles, "{ctx}");
                assert_eq!(
                    whole.overlay_grids_host,
                    scene
                        .overlay_grids
                        .iter()
                        .map(|g| g.budget_bytes)
                        .sum::<u64>(),
                    "{ctx}",
                );
                assert_eq!(
                    whole.total().gpu_bytes,
                    sum(|p| p.gpu_bytes()) + whole.mirror,
                    "{ctx}: the GPU whole is the panes plus the mirror",
                );
                assert_eq!(
                    whole.total().host_bytes,
                    sum(|p| p.host_bytes())
                        + whole.tiles_host
                        + whole.picture_arrival_host
                        + whole.overlay_grids_host
                        + whole.render_peak_host,
                    "{ctx}: the host whole is the panes plus the tiles, the arrival, the grids \
                     and the one render's peak",
                );
                assert_eq!(need(scene, &b, stand_in_grid_bytes), whole.total(), "{ctx}");
            }
        }
    }
}

/// **A second pane on a shared loop prices at no loop cost** — ruling 8 as
/// the scene encodes it. An alias (same site, product and window) is written
/// down as not looping with no grid of its own, so its frames, its scans and
/// its grid price at zero while its static render and offscreen stay its
/// own; a second product on one site is its own texture set at the frames'
/// full price and no scans at all, because the decoded volumes are the
/// site's. The whole charges each set once.
#[test]
fn a_second_pane_on_a_shared_loop_prices_at_no_loop_cost() {
    let b = desktop();
    let terms = |pane: &PaneNeed| need_terms_for_pane(pane, &b, stand_in_grid_bytes);

    let one_loop = two_panes_one_loop();
    let (owner, alias) = (one_loop.panes[0], one_loop.panes[1]);
    let o = terms(&owner);
    let a = terms(&alias);
    assert_eq!(o.loops, 28 * 16 * MIB, "28 frames of two hours at 259 s");
    assert_eq!(o.loop_scans_host, 28 * 80 * MIB);
    assert_eq!(a.loops, 0, "the alias holds the owner's frames");
    assert_eq!(a.loop_scans_host, 0, "and plays from the owner's volumes");
    assert_eq!(a.grids, 0);
    assert_eq!(
        a.static_rasters, o.static_rasters,
        "its static render is still its own"
    );
    let whole = need_terms(&one_loop, &b, stand_in_grid_bytes);
    assert_eq!(whole.loops, o.loops, "one set, charged once");
    assert_eq!(whole.loop_scans_host, o.loop_scans_host);
    assert_eq!(whole.static_rasters, 2 * o.static_rasters);

    let one_site = two_panes_one_site();
    let second = terms(&one_site.panes[1]);
    assert_eq!(
        second.loops, o.loops,
        "a second product is a second texture set"
    );
    assert_eq!(second.loop_scans_host, 0, "over the same decoded volumes");
    let whole = need_terms(&one_site, &b, stand_in_grid_bytes);
    assert_eq!(whole.loops, 2 * o.loops);
    assert_eq!(whole.loop_scans_host, o.loop_scans_host);

    // The 3D shape: an alias of a volume loop holds no grid and no frames,
    // and raymarches into an offscreen of its own.
    let orbit = PaneNeed {
        looping: true,
        loop_span_secs: TWO_HOURS,
        cadence_secs: PRECIP,
        ..volume_pane(HD, GroundPass::Off)
    };
    let orbit_alias = PaneNeed {
        looping: false,
        volume_grids: 0,
        loop_scans_shared: true,
        ..orbit
    };
    let v = terms(&orbit_alias);
    assert_eq!((v.grids, v.loops, v.loop_scans_host), (0, 0, 0));
    assert_eq!(v.offscreens, terms(&orbit).offscreens);
    assert!(v.offscreens > 0);
}

/// **A desktop does not use more memory for the same scene because it has
/// more.** One scene, priced on every bracket: every byte that differs between
/// two brackets is a resolution constant — the raster ceiling, the section
/// width, the loop frame's side, the grid's cell budget, the quality ceiling's
/// resolution rung — and the expected difference is computed here from those
/// constants alone. The one figure that is not a resolution is the frame count
/// of a loop with no cadence yet, which is the bracket's span demand (2 h / 1 h
/// / 45 min): the plan flags it as a capacity presumption in disguise for a
/// later landing, and it is stated as such below rather than absorbed.
#[test]
fn the_same_scene_costs_the_same_bytes_on_every_bracket() {
    let desktop = desktop();
    let mobile = resolve(&shipped_profile(BudgetLimits::MOBILE));
    let wasm = resolve(&shipped_profile(BudgetLimits::WASM));
    let terms = |scene: &Scene, b: &Budgets| need_terms(scene, b, stand_in_grid_bytes);
    let squared = |side: usize| (side as u128) * (side as u128);
    let ruling = "a desktop does not use more memory for the same scene because it has more";

    // The static plan-view render differs by the raster ceiling squared, and
    // by nothing else.
    let plan = scene_of(vec![plan_pane(HD, false, TWO_HOURS, None)]);
    let (d, m, w) = (
        terms(&plan, &desktop).static_rasters as u128,
        terms(&plan, &mobile).static_rasters as u128,
        terms(&plan, &wasm).static_rasters as u128,
    );
    assert_eq!(
        d * squared(MOBILE_RASTER_SIDE_CEILING),
        m * squared(DESKTOP_RASTER_SIDE_CEILING),
        "{ruling}: the static render differs by more than the raster ceilings squared",
    );
    assert_eq!(
        d * squared(WASM_RASTER_SIDE_CEILING),
        w * squared(DESKTOP_RASTER_SIDE_CEILING)
    );

    // The section render differs by the section width squared — equal on the
    // two native brackets, whose width is the same constant.
    let section = scene_of(vec![PaneNeed {
        view: RenderView::CrossSection,
        ..plan_pane(HD, false, TWO_HOURS, None)
    }]);
    let (d, m, w) = (
        terms(&section, &desktop).static_rasters as u128,
        terms(&section, &mobile).static_rasters as u128,
        terms(&section, &wasm).static_rasters as u128,
    );
    assert_eq!(
        d, m,
        "{ruling}: two native brackets priced one section differently"
    );
    assert_eq!(
        d * squared(WASM_SECTION_WIDTH),
        w * squared(NATIVE_SECTION_WIDTH)
    );

    // A loop with no cadence yet: the bytes a frame differ by the loop side
    // squared alone; the frame counts are the brackets' span demand, named.
    let looping = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None)]);
    let per_frame = |b: &Budgets, frames: usize| {
        let loops = terms(&looping, b).loops;
        assert_eq!(
            loops % frames as u64,
            0,
            "{}: a loop of whole frames",
            b.name
        );
        (loops / frames as u64) as u128
    };
    let d = per_frame(&desktop, DESKTOP_MAX_LOOP_RENDER_BUDGET);
    let m = per_frame(&mobile, MOBILE_MAX_LOOP_RENDER_BUDGET);
    let w = per_frame(&wasm, WASM_MAX_LOOP_RENDER_BUDGET);
    assert_eq!(
        d, m,
        "{ruling}: a loop frame costs the two native brackets differently"
    );
    assert_eq!(
        d * squared(WASM_LOOP_IMAGE_SIZE),
        w * squared(DESKTOP_LOOP_IMAGE_SIZE)
    );
    // The same loop *with* a cadence wants the same frames wherever the span
    // covers them: 1 + 1800 / 259 = 7 on every bracket, so the same bytes on
    // the two native ones.
    let half_hour = scene_of(vec![plan_pane(HD, true, 30 * 60, PRECIP)]);
    assert_eq!(
        terms(&half_hour, &desktop).loops,
        terms(&half_hour, &mobile).loops,
        "{ruling}: a half-hour loop costs a desktop more than a tablet",
    );
    // The volume behind a frame is one reservation on every bracket: the scan
    // term differs by frame count alone, which is the span demand named above.
    for b in [&desktop, &mobile, &wasm] {
        let t = terms(&looping, b);
        assert_eq!(
            t.loop_scans_host,
            t.loops / b.loop_frame_bytes() as u64 * LOOP_SCAN_RESERVE_BYTES,
            "{ruling}: {} reserves a different volume per frame",
            b.name,
        );
    }
    assert_eq!(
        terms(&half_hour, &desktop).loop_scans_host,
        terms(&half_hour, &wasm).loop_scans_host,
        "{ruling}: seven frames of volumes cost a desktop and a browser the same",
    );

    // A 3D pane's grid is priced at the bracket's cell budget, a resolution
    // constant, by the one pricer. `HD` is the pane's own size — the figure
    // the application reports is the pane's, never the window's — and it is
    // the same figure on every bracket, which is what keeps the offscreen
    // difference below a pure resolution-rung ratio.
    let volume = scene_of(vec![volume_pane(HD, GroundPass::Off)]);
    for (b, cells) in [
        (&desktop, DESKTOP_VOLUME_GRID_CELLS),
        (&mobile, MOBILE_VOLUME_GRID_CELLS),
        (&wasm, WASM_VOLUME_GRID_CELLS),
    ] {
        assert_eq!(
            terms(&volume, b).grids,
            stand_in_grid_bytes(cells).unwrap() as u64,
            "{ruling}: {} prices a grid at something other than its cell budget",
            b.name,
        );
    }
    // Its offscreen differs by the quality ceiling's resolution rung squared:
    // Native on the desktop, Half on the other two.
    let divisor = |b: &Budgets| b.quality_ceiling.resolution.linear_divisor() as u128;
    let (d, m, w) = (
        terms(&volume, &desktop).offscreens as u128,
        terms(&volume, &mobile).offscreens as u128,
        terms(&volume, &wasm).offscreens as u128,
    );
    assert_eq!(divisor(&desktop), 1);
    assert_eq!(divisor(&mobile), 2);
    assert_eq!(
        d,
        m * divisor(&mobile).pow(2) / divisor(&desktop).pow(2),
        "{ruling}"
    );
    assert_eq!(
        m, w,
        "{ruling}: two Half-rung brackets priced one offscreen differently"
    );

    // Buildings: one number on every bracket, the one machine's measurement,
    // so the term has no bracket difference to account for.
    let city = scene_of(vec![PaneNeed {
        buildings: true,
        ..volume_pane(HD, GroundPass::Off)
    }]);
    for b in [&mobile, &wasm] {
        assert_eq!(
            terms(&city, &desktop).buildings,
            terms(&city, b).buildings,
            "{ruling}: {} prices a pane's buildings differently",
            b.name,
        );
    }
    assert_eq!(terms(&city, &desktop).buildings, 16 * MIB);

    // The mirror and the tiles have no bracket term at all.
    let shared = Scene {
        panes: Vec::new(),
        tile_sources: vec![TileNeed {
            tiles_on_glass: 193,
            ancestor_net: 0,
            bytes_per_tile: 1_030_000,
        }],
        mirror_px: [2048, 2048],
        overlay_grids: Vec::new(),
    };
    for b in [&mobile, &wasm] {
        assert_eq!(
            terms(&shared, &desktop).mirror,
            terms(&shared, b).mirror,
            "{ruling}"
        );
        assert_eq!(
            terms(&shared, &desktop).tiles_host,
            terms(&shared, b).tiles_host,
            "{ruling}",
        );
    }
}

/// The allowance rule: a presumed capacity is the bracket's constant and the
/// constant is the allowance; a measured or probed figure is raw hardware and
/// need may take three quarters of it.
#[test]
fn the_allowance_is_the_constant_when_presumed_and_three_quarters_when_measured() {
    for (limits, constant) in [
        (BudgetLimits::WASM, WASM_APP_TEXTURE_BUDGET_BYTES),
        (BudgetLimits::MOBILE, MOBILE_APP_TEXTURE_BUDGET_BYTES),
        (BudgetLimits::DESKTOP, DESKTOP_APP_TEXTURE_BUDGET_BYTES),
    ] {
        let cap = Capacity::presumed(&limits);
        assert_eq!(cap.source, CapacitySource::Presumed);
        assert_eq!(cap.gpu_bytes, constant as u64, "{}", limits.name);
        assert_eq!(
            cap.allowance(),
            constant as u64,
            "{}: the fraction was applied to a constant argued with its own headroom",
            limits.name,
        );
        // The host figure is the bracket's declared ceiling where it has one
        // — the bound the browser's module is LINKED with — and, unlike the
        // GPU presumption, the fraction IS applied to it: a wall the module
        // header declares has no headroom of its own. What a particular
        // browser instance was actually constructed with may be smaller and
        // outranks this; that is `DeviceProfile::capacity`'s job and is
        // pinned by `a_page_that_said_what_its_heap_was_built_with_outranks_the_bracket`.
        assert_eq!(
            cap.host_bytes,
            limits.presumed_host_bytes.map(|bytes| bytes as u64),
            "{}",
            limits.name,
        );
        assert_eq!(
            cap.host_allowance(),
            cap.host_bytes.map(|host| host / 4 * 3),
            "{}",
            limits.name,
        );
        if limits.name == "wasm32" {
            assert_eq!(cap.host_bytes, Some(1 << 30));
        } else {
            assert_eq!(cap.host_bytes, None);
        }
    }
    // The presumption is the bracket's floor constant whatever rung the class
    // earned: 3840 MiB on the desktop bracket, never the 4032 MiB ceiling.
    assert_eq!(
        Capacity::presumed(&BudgetLimits::DESKTOP).gpu_bytes,
        BudgetLimits::DESKTOP
            .app_texture_ceiling_bytes
            .at(Promotion::Floor) as u64,
    );
    assert_ne!(
        Capacity::presumed(&BudgetLimits::DESKTOP).gpu_bytes,
        BudgetLimits::DESKTOP
            .app_texture_ceiling_bytes
            .at(Promotion::Ceiling) as u64,
    );

    let measured = Capacity::measured(24 << 30, Some(64 << 30));
    assert_eq!(measured.source, CapacitySource::Measured);
    assert_eq!(
        measured.allowance(),
        18 << 30,
        "three quarters of a 24 GiB card"
    );
    assert_eq!(measured.host_bytes, Some(64 << 30));
    let probed = Capacity::probed(1 << 30);
    assert_eq!(probed.source, CapacitySource::Probed);
    assert_eq!(probed.allowance(), 768 * MIB);
    // Exact on figures the denominator does not divide.
    assert_eq!(Capacity::probed(7).allowance(), 5);
    assert_eq!(Capacity::probed(4).allowance(), 3);
}

/// A session's presumption only ever comes down.
#[test]
fn holding_a_capacity_to_a_session_only_lowers_it() {
    let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
    assert_eq!(cap.held_to(None), cap);
    assert_eq!(
        cap.held_to(Some(u64::MAX)),
        cap,
        "a session cannot raise the presumption"
    );
    assert_eq!(cap.held_to(Some(1 << 30)).gpu_bytes, 1 << 30);
    assert_eq!(
        cap.held_to(Some(1 << 30)).source,
        CapacitySource::Presumed,
        "lowering does not change how the figure was learned",
    );
}

/// **A modulation is the identity when it names nothing, and can only lower
/// when it does** — on both pools, on every source arm, with the source and
/// an absent host figure both left as they were. The third clamp term has to
/// be a no-op today (nothing produces one yet) and unable to promise more
/// than the hardware tomorrow.
#[test]
fn a_modulation_names_nothing_or_lowers_and_never_raises() {
    use crate::scene::Modulation;

    let arms = [
        Capacity::presumed(&BudgetLimits::DESKTOP),
        Capacity::presumed(&BudgetLimits::WASM),
        Capacity::measured(24 << 30, Some(64 << 30)),
        Capacity::measured(4 << 30, None),
        Capacity::probed(4032 << 20),
    ];
    for cap in arms {
        assert_eq!(cap.modulated_by(Modulation::NONE), cap, "{cap:?}");
        assert_eq!(cap.modulated_by(Modulation::default()), cap, "{cap:?}");
        assert_eq!(
            cap.modulated_by(Modulation {
                gpu_ceiling: Some(u64::MAX),
                host_ceiling: Some(u64::MAX),
            }),
            cap,
            "a ceiling above the figure raised it: {cap:?}"
        );

        let halved = Modulation {
            gpu_ceiling: Some(cap.gpu_bytes / 2),
            host_ceiling: cap.host_bytes.map(|host| host / 2),
        };
        let lowered = cap.modulated_by(halved);
        assert_eq!(lowered.gpu_bytes, cap.gpu_bytes / 2, "{cap:?}");
        assert_eq!(
            lowered.host_bytes,
            cap.host_bytes.map(|host| host / 2),
            "{cap:?}"
        );
        assert_eq!(
            lowered.source, cap.source,
            "lowering does not change how the figure was learned: {cap:?}"
        );
        assert!(
            lowered.allowance() <= cap.allowance(),
            "the allowance rose under a lower ceiling: {cap:?}"
        );

        // A host ceiling on a capacity with no host figure has nothing to
        // hold down, and must not invent one.
        let none = Capacity {
            host_bytes: None,
            ..cap
        };
        assert_eq!(
            none.modulated_by(Modulation {
                gpu_ceiling: None,
                host_ceiling: Some(1),
            })
            .host_bytes,
            None,
            "{cap:?}"
        );
    }
}

/// **`fit` sheds down the ladder only as far as the scene needs — and this
/// scene is now a refusal rather than a shed.**
///
/// Six two-hour loops on the desktop bracket cost
/// 6 x (36 x 16 MiB + 256 MiB) = 4992 MiB against a 3840 MiB presumption.
/// Until 2026-09-06 the fourth step, the loop history at 36 to 18 frames,
/// made it fit at 3264 MiB and the walk stopped there. Ruling 15 removed
/// that rung — *"frame DENSITY is tier 1 too: refuse, never decimate"* — so
/// the walk now takes every rung it has (lighting, resolution twice, the
/// overlay margin twice, the tiles, the raster ceiling: seven), lands on the
/// ladder's floor, where 6 x (36 x 16 + 64) MiB = 3840 MiB fits the
/// presumption **exactly** — every rung spent on a scene that used to cost
/// one. A pane more, or a card a byte smaller, and there is nothing left to
/// shed and the scene is refused at admission rather than quietly given half
/// its history.
///
/// **The product consequence, stated rather than discovered.** The rungs
/// this scene now takes are ones it never used to: the overlay margin goes
/// to 100 %, the tiles snap to the whole zoom and the raster ceiling falls
/// to 4096 px. A GPU-over looping scene goes from resolution straight to
/// oversampling, and the first rung a user calls "worse" arrives one step
/// sooner.
#[test]
fn fit_sheds_down_the_ladder_only_as_far_as_the_scene_needs() {
    let profile = DeviceProfile {
        class: DeviceClass::Discrete,
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let top = resolve(&profile);
    assert_eq!(top.promotion, Promotion::Ceiling);
    let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
    let six = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None); 6]);

    let before = need(&six, &top, stand_in_grid_bytes);
    assert_eq!(before.gpu_bytes, 6 * (36 * 16 + 256) * MIB);
    assert!(before.gpu_bytes > cap.allowance());

    let fitted = fit(&six, &profile, &cap, stand_in_grid_bytes);
    assert_eq!(
        fitted.steps_back, 7,
        "lighting, resolution twice, the margin twice, the tiles, the raster \
         ceiling — every rung the ladder has, because none of them is the \
         loop's history any more",
    );
    assert_eq!(fitted.quality_ceiling.shading, GradientShading::Off);
    assert_eq!(fitted.quality_ceiling.resolution, ResolutionRung::Quarter);
    assert_eq!(
        fitted.loop_render_budget, DESKTOP_MAX_LOOP_RENDER_BUDGET,
        "no governor path lowers a granted loop's frame count",
    );
    assert!(
        fitted.tile_whole_zoom,
        "the tiles are asked now: the history is not there to pay first",
    );
    assert_eq!(
        fitted.overlay_oversample_percent, 100,
        "and the margin is gone, one step sooner than it used to be",
    );
    assert_eq!(fitted.grid_cells, BudgetLimits::DESKTOP.grid_cells.floor);
    assert_eq!(
        fitted.raster_side_ceiling_px,
        BudgetLimits::DESKTOP.long_range_image_side_px.floor
    );
    let after = need(&six, &fitted, stand_in_grid_bytes);
    assert_eq!(after.gpu_bytes, 6 * (36 * 16 + 64) * MIB);
    assert_eq!(
        after.gpu_bytes,
        cap.allowance(),
        "at the ladder's floor the scene fits the 3840 MiB presumption \
         exactly — 6 x (36 x 16 + 64) MiB — with every rung spent and \
         nothing left over",
    );
    assert!(every_rung_at_its_stop(&fitted, &BudgetLimits::DESKTOP));
    assert!(fit_holds(
        &six,
        &fitted,
        &BudgetLimits::DESKTOP,
        &cap,
        stand_in_grid_bytes
    ));

    // The three 3D steps lower nothing for this scene: the first rung that
    // pays is the raster ceiling, and it pays 6 x 192 MiB.
    let mut three = top;
    demote(&mut three, &BudgetLimits::DESKTOP, 3);
    assert_eq!(need(&six, &three, stand_in_grid_bytes), before);

    // Fewer panes fit at the class rung and are left there.
    for panes in 1..=4 {
        let scene = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None); panes]);
        assert_eq!(
            fit(&scene, &profile, &cap, stand_in_grid_bytes),
            top,
            "{panes} two-hour loops fit the desktop presumption and were shed anyway",
        );
    }
}

/// When no rung can pay, `fit` hands back the floor and says so through
/// `every_rung_at_its_stop`, for the runtime to clamp and log.
#[test]
fn fit_returns_the_floor_when_no_rung_can_pay() {
    for limits in BudgetLimits::SHIPPED {
        let profile = DeviceProfile {
            class: DeviceClass::Discrete,
            ..shipped_profile(limits)
        };
        let one_byte = Capacity::probed(1);
        let scene = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None)]);
        let fitted = fit(&scene, &profile, &one_byte, stand_in_grid_bytes);
        assert!(every_rung_at_its_stop(&fitted, &limits), "{}", limits.name);
        let mut floor = resolve(&profile);
        demote(&mut floor, &limits, 64);
        assert_eq!(
            Budgets {
                steps_back: fitted.steps_back,
                // **The reachable frame count is admission's, not a rung's.**
                // A one-byte capacity reaches the two-frame floor and
                // `resolve` cannot know that: it sees a profile and no
                // capacity. Carried like `steps_back`, and checked on its own
                // line below.
                loop_frames_reachable: fitted.loop_frames_reachable,
                ..floor
            },
            fitted,
            "{}: the floor `fit` gives up at is not the ladder's floor",
            limits.name,
        );
        assert_eq!(
            fitted.loop_render_budget,
            resolve(&profile).loop_render_budget,
            "{}: a rung lowered a granted loop's frame count",
            limits.name,
        );
        assert_eq!(
            fitted.loop_frames_reachable, MIN_LOOP_FRAMES_PER_PANE,
            "{}: one byte of capacity reaches the two-frame floor and no more",
            limits.name,
        );
        assert!(
            !every_rung_at_its_stop(&resolve(&profile), &limits),
            "{}",
            limits.name
        );
    }
}

/// **`floor_need` is the scene's need at the ladder's floor** — the same
/// budgets `fit` gives up at, priced for the scene — and never more than the
/// need at the class rung, since every rung only sheds.
#[test]
fn the_floor_need_is_the_scenes_need_at_every_rungs_stop() {
    for limits in BudgetLimits::SHIPPED {
        let profile = DeviceProfile {
            class: DeviceClass::Discrete,
            ..shipped_profile(limits)
        };
        let scene = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None); 6]);
        let at_floor = floor_need(&scene, &profile, stand_in_grid_bytes);

        let mut floor = resolve(&profile);
        demote(&mut floor, &limits, 64);
        assert!(every_rung_at_its_stop(&floor, &limits), "{}", limits.name);
        assert_eq!(
            at_floor,
            need(&scene, &floor, stand_in_grid_bytes),
            "{}: the floor need is not the need at the ladder's floor",
            limits.name
        );

        let at_class_rung = need(&scene, &resolve(&profile), stand_in_grid_bytes);
        assert!(
            at_floor.gpu_bytes <= at_class_rung.gpu_bytes
                && at_floor.host_bytes <= at_class_rung.host_bytes,
            "{}: shedding every rung cost more, {at_floor:?} against {at_class_rung:?}",
            limits.name
        );
        assert!(
            at_floor.gpu_bytes > 0,
            "{}: six loops cost nothing at the floor",
            limits.name
        );
    }
}

/// **The capacity for an allowance is the smallest figure whose allowance
/// covers it**, on every source arm: one byte less allows one byte too few.
/// This is what the pressure decay's floor is spelled through — a need in
/// allowance terms, turned back into a capacity figure the arm can hold.
#[test]
fn a_capacity_for_an_allowance_is_the_smallest_that_covers_it() {
    let arms = [
        Capacity::presumed(&BudgetLimits::DESKTOP),
        Capacity::measured(24 << 30, None),
        Capacity::probed(4032 << 20),
    ];
    for cap in arms {
        for allowance in [
            1u64,
            2,
            3,
            4,
            5,
            6,
            7,
            100,
            576 << 20,
            3839 << 20,
            3840 << 20,
        ] {
            let figure = cap.gpu_bytes_for_allowance(allowance);
            let covers = |gpu_bytes: u64| Capacity { gpu_bytes, ..cap }.allowance();
            assert!(
                covers(figure) >= allowance,
                "{:?}: {figure} allows {} for a need of {allowance}",
                cap.source,
                covers(figure)
            );
            assert!(
                covers(figure - 1) < allowance,
                "{:?}: {figure} is not the smallest figure covering {allowance}",
                cap.source
            );
        }
        assert_eq!(cap.gpu_bytes_for_allowance(0), 0, "{:?}", cap.source);
        assert_eq!(
            cap.gpu_bytes_for_allowance(u64::MAX),
            u64::MAX,
            "{:?}: the top of u64 wrapped",
            cap.source
        );
    }
    // The presumed arm's constant is its own allowance, so the figure is the
    // need itself; the measured arm's is `NEED_FRACTION` of the card, so the
    // figure is the need over three quarters, rounded up: 5 needs 7, as the
    // allowance test above has it.
    assert_eq!(
        Capacity::presumed(&BudgetLimits::DESKTOP).gpu_bytes_for_allowance(5),
        5
    );
    assert_eq!(Capacity::probed(1).gpu_bytes_for_allowance(5), 7);
    assert_eq!(Capacity::measured(1, None).gpu_bytes_for_allowance(3), 4);
}

/// **The loop pool is what the loops need, capped by the room the rest of the
/// scene leaves** — never the class's ceiling. On the desktop bracket one
/// two-hour loop is 36 x 16 MiB = 576 MiB, not the 3072 MiB pool ceiling a
/// discrete card used to be handed; six are 3456 MiB against the 2304 MiB of
/// room six static renders leave under 3840, so 2304 at the class rung — and
/// once `fit` has halved the history, 1728, with room to spare.
#[test]
fn the_loop_pool_is_what_the_loops_need_capped_by_the_room() {
    let profile = DeviceProfile {
        class: DeviceClass::Discrete,
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let top = resolve(&profile);
    let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
    let pool = |scene: &Scene, b: &Budgets| loop_pool_bytes(scene, b, &cap, stand_in_grid_bytes);

    let one = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None)]);
    assert_eq!(loop_need(&one, &top, stand_in_grid_bytes), 576 * MIB);
    assert_eq!(
        loop_room(&one, &top, &cap, stand_in_grid_bytes),
        (3840 - 256) * MIB
    );
    assert_eq!(
        pool(&one, &top),
        576 * MIB,
        "one loop's span, not the 3072 MiB ceiling"
    );

    let six = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None); 6]);
    assert_eq!(loop_need(&six, &top, stand_in_grid_bytes), 3456 * MIB);
    assert_eq!(
        loop_room(&six, &top, &cap, stand_in_grid_bytes),
        (3840 - 6 * 256) * MIB
    );
    assert_eq!(pool(&six, &top), 2304 * MIB, "min(3456, 2304)");
    let fitted = fit(&six, &profile, &cap, stand_in_grid_bytes);
    // **Moved 2026-09-06, ruling 15.** This read 1728 MiB — `min(6 x 18 x 16,
    // 2304)` — because the ladder had halved the six loops to eighteen frames
    // to make the scene fit. No rung touches the frame count now, so the
    // loops still want all 36; what the walk shed instead is the raster
    // ceiling, which frees 6 x 192 MiB of room, and the ceiling rather than
    // the room is what binds.
    assert_eq!(
        pool(&six, &fitted),
        3456 * MIB,
        "min(6 x 36 x 16, 3840 - 6 x 64)"
    );

    // Nothing looping asks for nothing; the application's limits then hold the
    // pool at its floor.
    assert_eq!(pool(&Scene::empty(), &top), 0);

    // And the same one loop on the other brackets: its own span, at its own
    // frame side, and no more.
    let mobile = resolve(&shipped_profile(BudgetLimits::MOBILE));
    assert_eq!(
        loop_pool_bytes(
            &one,
            &mobile,
            &Capacity::presumed(&BudgetLimits::MOBILE),
            stand_in_grid_bytes
        ),
        18 * 16 * MIB,
    );
    let wasm = resolve(&shipped_profile(BudgetLimits::WASM));
    assert_eq!(
        loop_pool_bytes(
            &one,
            &wasm,
            &Capacity::presumed(&BudgetLimits::WASM),
            stand_in_grid_bytes
        ),
        14 * 4 * MIB,
    );
}

/// **The pool is the room, capped at the loops' ceiling — not their base.** A
/// pane whose listing has said 300 s over a six-hour lookback has a base of
/// 36 frames and a ceiling of 60 (`MAX_LOOP_FRAMES`; the lookback at that
/// cadence is 73). The pool is sized to the ceiling so the application's
/// planner has room to balloon into: 960 MiB for one such pane under the
/// presumption. `fit` asks whether the scene fits and charges the base; the
/// pool asks how much room is left. Where no cadence is known, the ceiling is
/// the base and nothing here moves.
///
/// **The base moved from 25 to 36 on 2026-09-06, ruling 13.** It used to be
/// two hours at 300 s — the *bracket's* span, not the pane's — because
/// `Budgets::frames_for_span_of` opened `span_secs.min(self.loop_span_secs)`.
/// A user who set a six-hour lookback on the desktop bracket was answered
/// with two hours of it and told nothing. The request is now the user's whole
/// six hours (73 frames) and what answers is the reachable ceiling, which on
/// this unmeasured profile is the class figure of 36 — three hours at 300 s,
/// and a clamp the readout names rather than a cut nothing recorded.
#[test]
fn the_pool_is_the_room_capped_at_the_loops_ceiling_not_their_base() {
    let top = desktop();
    let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
    const SIX_HOURS: usize = 6 * 60 * 60;
    let pane = plan_pane(HD, true, SIX_HOURS, Some(300));

    assert_eq!(
        loop_frames_requested(&pane, &top),
        73,
        "the request: the user's whole six hours at 300 s"
    );
    assert_eq!(
        loop_frames(&pane, &top),
        36,
        "the base: what this capacity reaches, three hours at 300 s"
    );
    assert_eq!(
        loop_frames_ceiling(&pane, &top),
        60,
        "the ceiling: min(1 + 21600 / 300 = 73, MAX_LOOP_FRAMES = 60)"
    );
    let one = scene_of(vec![pane]);
    assert_eq!(loop_need(&one, &top, stand_in_grid_bytes), 36 * 16 * MIB);
    assert_eq!(loop_ceiling(&one, &top, stand_in_grid_bytes), 60 * 16 * MIB);
    assert_eq!(
        loop_pool_bytes(&one, &top, &cap, stand_in_grid_bytes),
        960 * MIB,
        "min(60 x 16, 3840 - 256): the ceiling, not the base's 400 MiB",
    );

    let six = scene_of(vec![pane; 6]);
    assert_eq!(
        loop_pool_bytes(&six, &top, &cap, stand_in_grid_bytes),
        2304 * MIB,
        "min(6 x 960, 3840 - 6 x 256): the room",
    );
    assert_eq!(
        loop_pool_bytes(&six, &top, &cap, stand_in_grid_bytes),
        loop_room(&six, &top, &cap, stand_in_grid_bytes),
    );

    // No cadence: the ceiling is the base, and the pool is what it always was.
    let bare = plan_pane(HD, true, SIX_HOURS, None);
    assert_eq!(loop_frames_ceiling(&bare, &top), loop_frames(&bare, &top));
    assert_eq!(
        loop_pool_bytes(&scene_of(vec![bare]), &top, &cap, stand_in_grid_bytes),
        36 * 16 * MIB,
    );
    // A lookback inside the rung's span: the ceiling is the base too.
    let hour = plan_pane(HD, true, 3600, Some(300));
    assert_eq!(loop_frames(&hour, &top), 13);
    assert_eq!(loop_frames_ceiling(&hour, &top), 13);
    // Never below the base, whatever the cadence says.
    let coarse = plan_pane(HD, true, 600, Some(3600));
    assert!(loop_frames_ceiling(&coarse, &top) >= loop_frames(&coarse, &top));
}

/// `fit` is pure: the same scene against the same capacity fits to the same
/// budgets every time, which is what makes a reopen 1:1 without a memo.
#[test]
fn the_same_scene_against_the_same_capacity_fits_the_same_twice() {
    for limits in BudgetLimits::SHIPPED {
        let profile = DeviceProfile {
            class: DeviceClass::Discrete,
            ..shipped_profile(limits)
        };
        let cap = Capacity::presumed(&limits);
        for (name, scene) in scene_table() {
            let first = fit(&scene, &profile, &cap, stand_in_grid_bytes);
            let second = fit(&scene, &profile, &cap, stand_in_grid_bytes);
            assert_eq!(first, second, "{} / {name}", limits.name);
        }
    }
}

/// **A measured capacity is the allowance the scene is fitted to, and no
/// bracket constant binds.** The box's own RTX 3090 reads 24822 MiB, so need
/// may take three quarters of it, 18616.5 MiB: six two-hour loops beside their
/// static renders cost 6 x (36 x 16 + 256) = 4992 MiB and fit at the class
/// rung with every frame — where the 3840 MiB presumption halves the history
/// to 18 ([`fit_sheds_down_the_ladder_only_as_far_as_the_scene_needs`]). The
/// pool is what the loops need, 3456 MiB, and the room beside it is
/// 18616.5 - 1536 = 17080.5 MiB, stated in bytes because the halves are real.
/// A 4 GiB card allows 3072 MiB: the same scene sheds the three 3D rungs that
/// cost a 2D scene nothing and then two halvings, 36 to 18 to 9 frames —
/// 6 x (18 x 16 + 256) = 3264 is still over, 6 x (9 x 16 + 256) = 2400 fits —
/// and at the 259 s precipitation cadence nine frames are 8 x 259 = 2072 s of
/// lookback, thirty-four minutes of the two hours asked for.
#[test]
fn a_measured_capacity_is_the_allowance_the_scene_is_fitted_to() {
    let discrete = |vram_mib: u64| DeviceProfile {
        class: DeviceClass::Discrete,
        vram_bytes: Some(vram_mib * MIB),
        system_ram_bytes: Some(64 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let six = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None); 6]);

    let rtx_3090 = discrete(24822);
    let cap = rtx_3090.capacity();
    assert_eq!(cap.source, CapacitySource::Measured);
    assert_eq!(cap.gpu_bytes, 24822 * MIB);
    assert_eq!(cap.host_bytes, Some(64 << 30));
    assert_eq!(cap.allowance(), 19_520_815_104, "18616.5 MiB, exactly");
    let top = resolve(&rtx_3090);
    let fitted = fit(&six, &rtx_3090, &cap, stand_in_grid_bytes);
    assert_eq!(fitted, top, "a scene that fits the card was shed anyway");
    assert_eq!(fitted.loop_render_budget, DESKTOP_MAX_LOOP_RENDER_BUDGET);
    assert_eq!(
        need(&six, &fitted, stand_in_grid_bytes).gpu_bytes,
        4992 * MIB
    );
    assert_eq!(
        loop_pool_bytes(&six, &fitted, &cap, stand_in_grid_bytes),
        3456 * MIB,
        "the pool is what six two-hour loops need, past the 3072 MiB pool ceiling",
    );
    assert_eq!(
        loop_room(&six, &fitted, &cap, stand_in_grid_bytes),
        19_520_815_104 - 1536 * MIB,
        "17080.5 MiB of room",
    );
    // The same scene against the presumption is shed: this is the difference
    // a measurement makes, and the only one.
    let presumed = fit(
        &six,
        &rtx_3090,
        &Capacity::presumed(&BudgetLimits::DESKTOP),
        stand_in_grid_bytes,
    );
    // **Moved 2026-09-06, ruling 15.** This read
    // `DESKTOP_MAX_LOOP_RENDER_BUDGET / 2` — the presumed arm halved the
    // loop's history to fit and the measured arm did not, which was the
    // difference a measurement made. No rung halves it now; what the
    // presumed arm sheds instead is the picture.
    assert_eq!(
        presumed.loop_render_budget, DESKTOP_MAX_LOOP_RENDER_BUDGET,
        "no governor path lowers a granted loop's frame count",
    );
    assert_eq!(
        presumed.overlay_oversample_percent, 100,
        "the presumption pays with the picture's margin instead",
    );
    assert_eq!(
        Budgets {
            steps_back: 0,
            quality_ceiling: top.quality_ceiling,
            offscreen_bytes: top.offscreen_bytes,
            app_texture_ceiling_bytes: top.app_texture_ceiling_bytes,
            overlay_oversample_percent: top.overlay_oversample_percent,
            tile_whole_zoom: top.tile_whole_zoom,
            raster_side_ceiling_px: top.raster_side_ceiling_px,
            grid_cells: top.grid_cells,
            volume_texture_bytes: top.volume_texture_bytes,
            ..presumed
        },
        top,
        "the two arms differ by ladder rungs and nothing else",
    );

    let four_gib = discrete(4096);
    let cap = four_gib.capacity();
    assert_eq!(cap.allowance(), 3072 * MIB);
    let fitted = fit(&six, &four_gib, &cap, stand_in_grid_bytes);
    // **Until 2026-09-06 this card's answer was five ladder steps — lighting,
    // resolution twice, two halvings of the history — and a
    // `loop_render_budget` of 9.** Ruling 15 took the halvings away, so the
    // frame count is the class figure whatever this card holds and the walk
    // pays with the picture instead. 3072 MiB less the 1536 MiB the six
    // panes' static rasters take is 96 frames of 16 MiB, which is over the
    // class figure, so this card reaches every frame the class offers.
    assert_eq!(
        fitted.loop_render_budget, DESKTOP_MAX_LOOP_RENDER_BUDGET,
        "no governor path lowers a granted loop's frame count",
    );
    assert_eq!(
        fitted.loop_frames_reachable, DESKTOP_MAX_LOOP_RENDER_BUDGET,
        "4 GiB reaches every frame the class offers",
    );
    assert_eq!(
        fitted.frames_for_span_of(TWO_HOURS, PRECIP),
        28,
        "so the pane's two hours at 259 s are answered whole",
    );

    // **A card small enough to bite, which is item 2's other half.** A probed
    // 1 GiB capacity allows 768 MiB; the six panes' static rasters take
    // 384 MiB of it at the ladder's floor, and what is left buys 24 frames of
    // 16 MiB — under the class figure, so the request is CLAMPED and the pair
    // says so. This is the figure that used to be a compiled constant.
    let one_gib = discrete(1024);
    let small = one_gib.capacity();
    let fitted = fit(&six, &one_gib, &small, stand_in_grid_bytes);
    assert_eq!(small.allowance(), 768 * MIB);
    assert_eq!(
        fitted.loop_render_budget, DESKTOP_MAX_LOOP_RENDER_BUDGET,
        "no governor path lowers a granted loop's frame count",
    );
    assert!(
        fitted.loop_frames_reachable < DESKTOP_MAX_LOOP_RENDER_BUDGET,
        "a 1 GiB card reaches the whole class figure, so nothing is clamped \
         here and this arm proves nothing: {}",
        fitted.loop_frames_reachable,
    );
    assert_eq!(
        fitted.frames_requested_for_span_of(TWO_HOURS, PRECIP),
        28,
        "the ask: two hours at 259 s",
    );
    assert_eq!(
        fitted.frames_for_span_of(TWO_HOURS, PRECIP),
        fitted.loop_frames_reachable,
        "and what this card reaches, which is what the readout names beside it",
    );
    assert!(
        fitted.tile_whole_zoom,
        "a 1 GiB card leaves this scene room to spare after all",
    );

    // A unified-memory part on a 64 GiB host: the pool is cut in two, 32 GiB
    // to each side, one loop's pool is its need, and the offscreen stays at
    // the Step the class earns — memory says nothing about fill rate.
    let integrated = DeviceProfile {
        class: DeviceClass::Integrated,
        vram_bytes: None,
        system_ram_bytes: Some(64 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let cap = integrated.capacity();
    // Derived, not measured: no API was asked about this GPU. The figure and
    // everything the fit does with it are the same as before that word
    // existed — what changed is the ceiling `LoopPoolLimits::on` leaves in
    // place and the host share, not the arithmetic here.
    assert_eq!(cap.source, CapacitySource::Derived);
    assert_eq!(cap.pools, crate::scene::Pools::Unified);
    assert_eq!(cap.gpu_bytes, 32 << 30);
    assert_eq!(cap.host_bytes, Some(32 << 30));
    let one = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None)]);
    let fitted = fit(&one, &integrated, &cap, stand_in_grid_bytes);
    assert_eq!(fitted, resolve(&integrated));
    assert_eq!(fitted.promotion, Promotion::Step);
    assert_eq!(fitted.offscreen_bytes as u64, 20 * MIB);
    assert_eq!(
        loop_pool_bytes(&one, &fitted, &cap, stand_in_grid_bytes),
        576 * MIB
    );
    assert_eq!(
        loop_room(&one, &fitted, &cap, stand_in_grid_bytes),
        (24 << 30) - 256 * MIB
    );
}

/// **The economy allowance is what is left under nine tenths of the capacity
/// once need is paid**, on every arm, and never negative. Under the 3090's
/// measurement six two-hour loops leave 0.9 x 24822 - 4992 = 17347.8 MiB for
/// tiles panned away from, parsed geometry and the render cache; under the
/// 3840 MiB presumption the same scene, shed to 18 frames, leaves 3456 - 3264
/// = 192 MiB, and a scene at the presumption's whole allowance leaves nothing.
#[test]
fn the_economy_allowance_is_what_is_left_under_nine_tenths_of_the_capacity() {
    // Exact on small figures the denominator does not divide.
    let thousand = Capacity::probed(1000);
    let gpu = |gpu_bytes: u64| Need {
        gpu_bytes,
        host_bytes: 0,
    };
    assert_eq!(thousand.economy_allowance(Need::default()), 900);
    assert_eq!(thousand.economy_allowance(gpu(100)), 800);
    assert_eq!(thousand.economy_allowance(gpu(900)), 0);
    assert_eq!(
        thousand.economy_allowance(gpu(950)),
        0,
        "a need past the line saturates rather than wrapping",
    );
    assert_eq!(Capacity::probed(7).economy_allowance(Need::default()), 6);
    assert_eq!(
        Capacity::probed(u64::MAX).economy_allowance(Need::default()),
        u64::MAX / 10 * 9 + (u64::MAX % 10) * 9 / 10,
        "no overflow at the top of the range",
    );

    let rtx_3090 = DeviceProfile {
        class: DeviceClass::Discrete,
        vram_bytes: Some(24822 * MIB),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let six = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None); 6]);
    let cap = rtx_3090.capacity();
    let fitted = fit(&six, &rtx_3090, &cap, stand_in_grid_bytes);
    let economy = economy_allowance(&six, &fitted, &cap, stand_in_grid_bytes);
    assert_eq!(
        economy,
        24822 * MIB / 10 * 9 + (24822 * MIB % 10) * 9 / 10 - 4992 * MIB,
    );
    assert_eq!(economy / MIB, 17347, "17347.8 MiB, by integer division");
    assert_eq!(
        economy,
        cap.economy_allowance(need(&six, &fitted, stand_in_grid_bytes)),
        "the free function is the method at the scene's price",
    );

    let presumed = Capacity::presumed(&BudgetLimits::DESKTOP);
    let profile = DeviceProfile {
        class: DeviceClass::Discrete,
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let fitted = fit(&six, &profile, &presumed, stand_in_grid_bytes);
    // **Moved 2026-09-06, ruling 15.** The need read 3264 MiB while the
    // ladder could halve the six loops to eighteen frames. It cannot, so the
    // loops still cost 6 x 36 x 16 MiB and what the walk sheds instead is the
    // raster ceiling: 6 x (36 x 16 + 64) = 3840 MiB, the whole presumption,
    // and nothing is left under the nine-tenths line.
    assert_eq!(
        need(&six, &fitted, stand_in_grid_bytes).gpu_bytes,
        3840 * MIB
    );
    assert_eq!(
        economy_allowance(&six, &fitted, &presumed, stand_in_grid_bytes),
        0,
        "a scene at the whole allowance leaves no economy",
    );
    // Four two-hour loops beside two still panes cost the whole 3840 MiB
    // allowance and fit it exactly; they are past the nine-tenths line, so
    // nothing may sit beyond them.
    let mut exact = vec![plan_pane(HD, true, TWO_HOURS, None); 4];
    exact.extend([plan_pane(HD, false, TWO_HOURS, None); 2]);
    let exact = scene_of(exact);
    let fitted = fit(&exact, &profile, &presumed, stand_in_grid_bytes);
    assert_eq!(
        need(&exact, &fitted, stand_in_grid_bytes).gpu_bytes,
        3840 * MIB
    );
    assert_eq!(
        economy_allowance(&exact, &fitted, &presumed, stand_in_grid_bytes),
        0
    );
}

/// **`fit_holds` is the invariant `fit` promises, and it can say no.** Every
/// answer `fit` gives on either arm holds; the class rung handed a capacity it
/// does not fit, with rungs left to shed, does not — that is the answer the
/// runtime clamps and logs on rather than trusting.
#[test]
fn fit_holds_for_every_answer_fit_gives_and_refuses_a_budget_that_was_not_fitted() {
    let six = scene_of(vec![plan_pane(HD, true, TWO_HOURS, None); 6]);
    for limits in BudgetLimits::SHIPPED {
        let profile = DeviceProfile {
            class: DeviceClass::Discrete,
            vram_bytes: Some(4 << 30),
            ..shipped_profile(limits)
        };
        for cap in [
            Capacity::presumed(&limits),
            profile.capacity(),
            Capacity::probed(1),
        ] {
            for (name, scene) in scene_table() {
                let fitted = fit(&scene, &profile, &cap, stand_in_grid_bytes);
                assert!(
                    fit_holds(&scene, &fitted, &limits, &cap, stand_in_grid_bytes),
                    "{} / {name} / {:?}: fit's own answer does not hold",
                    limits.name,
                    cap.source,
                );
            }
        }
        // The class rung against one byte: over the allowance, rungs to spare.
        let top = resolve(&profile);
        let one_byte = Capacity::probed(1);
        assert!(
            !fit_holds(&six, &top, &limits, &one_byte, stand_in_grid_bytes),
            "{}: a budget nothing fitted was accepted",
            limits.name,
        );
        // The floor against one byte: still over, but nothing left to shed.
        let mut floor = top;
        demote(&mut floor, &limits, 64);
        assert!(fit_holds(
            &six,
            &floor,
            &limits,
            &one_byte,
            stand_in_grid_bytes
        ));
    }
}

/// **The tile allowance on the measured arm is the economy split, held inside
/// the bracket.** Presumed: the class rung's figures, untouched. Measured
/// with room: every population at its ceiling, whatever rung the class earned
/// — a card that can hold more history holds more, up to the generous cap.
/// Measured without room — a card the scene has nearly filled — the floor,
/// never below it. The shares are 2:2:1 and each is clamped on its own.
#[test]
fn the_tile_allowance_follows_the_economy_on_the_measured_arm_and_the_bracket_otherwise() {
    use crate::fit::{TILE_ECONOMY_SHARES, tile_cache_budget};
    use crate::scene::{Capacity, CapacitySource};

    let limits = BudgetLimits::DESKTOP;
    let profile = shipped_profile(limits);
    let budgets = resolve(&profile);
    let scene = scene_of(vec![plan_pane(HD, false, 0, None)]);

    // Presumed: the class rung's own figures.
    let presumed = Capacity::presumed(&limits);
    assert_eq!(
        tile_cache_budget(&scene, &budgets, &limits, &presumed, stand_in_grid_bytes),
        budgets.tile_cache(),
        "the presumed arm reads the bracket, as every presumed allowance does"
    );

    // Measured, with a card that has room: the ceiling on every population.
    let roomy = Capacity::measured(24 << 30, None);
    assert_eq!(roomy.source, CapacitySource::Measured);
    let at_ceiling = tile_cache_budget(&scene, &budgets, &limits, &roomy, stand_in_grid_bytes);
    assert_eq!(
        at_ceiling,
        TileCacheBudget {
            styled_bytes: limits.tile_styled_bytes.ceiling as u64,
            parsed_bytes: limits.tile_parsed_bytes.ceiling as u64,
            terrain_bytes: limits.tile_terrain_bytes.ceiling as u64,
            whole_zoom: false,
        },
        "a 24 GiB card holds the ceiling and not a byte more"
    );

    // Measured, with a card the scene has nearly filled: the floor, whatever
    // the class rung was.
    let scene_need = need(&scene, &budgets, stand_in_grid_bytes).gpu_bytes;
    let tight = Capacity::measured(scene_need + 1, None);
    let at_floor = tile_cache_budget(&scene, &budgets, &limits, &tight, stand_in_grid_bytes);
    assert_eq!(
        at_floor,
        TileCacheBudget {
            styled_bytes: limits.tile_styled_bytes.floor as u64,
            parsed_bytes: limits.tile_parsed_bytes.floor as u64,
            terrain_bytes: limits.tile_terrain_bytes.floor as u64,
            whole_zoom: false,
        },
        "a card with no economy left still holds the floor"
    );

    // Between the two the shares are what they say — **for styled, and only
    // styled**. At this economy the three shares are 400 / 400 / 200 MiB, and
    // only styled (bracket 160..512) lands strictly inside one. Parsed clamps
    // at its ceiling and so does terrain (200 MiB against 128), so two of the
    // three rows below are `hold` against `hold`: they re-prove the ceiling
    // the `at_ceiling` case above already holds, not the split.
    //
    // Both clamps predate the parsed re-derivation — on the brackets before
    // it, parsed was 400 over a 384 ceiling and terrain 200 over the same 128
    // — so the count of clamped rows is two either way, and re-deriving the
    // parsed bracket only deepened one of them. A standing gap, not one this
    // change opened. An economy of 440 MiB puts all three strictly inside
    // (176 / 176 / 88); that is the fix, and it wants a run of this suite.
    let parts: u64 = TILE_ECONOMY_SHARES.iter().sum();
    let economy = 5 * (200u64 << 20);
    let cap = Capacity::measured(
        (economy + scene_need) * crate::constants::ECONOMY_FRACTION.1
            / crate::constants::ECONOMY_FRACTION.0,
        None,
    );
    let inside = tile_cache_budget(&scene, &budgets, &limits, &cap, stand_in_grid_bytes);
    let e = crate::fit::economy_allowance(&scene, &budgets, &cap, stand_in_grid_bytes);
    assert_eq!(
        inside,
        TileCacheBudget {
            styled_bytes: limits.tile_styled_bytes.hold((e / parts * 2) as usize) as u64,
            parsed_bytes: limits.tile_parsed_bytes.hold((e / parts * 2) as usize) as u64,
            terrain_bytes: limits.tile_terrain_bytes.hold((e / parts) as usize) as u64,
            whole_zoom: false,
        }
    );

    // The sharpness rung rides on both arms: step the budgets to the tile rung
    // and the measured arm's allowance carries it as the presumed arm's does.
    let mut snapped = budgets;
    while !snapped.tile_whole_zoom {
        assert!(
            crate::budget::step_down(&mut snapped, &limits),
            "the ladder ended before the tile rung"
        );
    }
    assert!(
        tile_cache_budget(&scene, &snapped, &limits, &presumed, stand_in_grid_bytes).whole_zoom
    );
    assert!(tile_cache_budget(&scene, &snapped, &limits, &roomy, stand_in_grid_bytes).whole_zoom);
    assert_eq!(
        tile_cache_budget(&scene, &snapped, &limits, &roomy, stand_in_grid_bytes).styled_bytes,
        at_ceiling.styled_bytes,
        "the sharpness rung moved the styled allowance"
    );
    assert!(
        inside.styled_bytes > limits.tile_styled_bytes.floor as u64
            && inside.styled_bytes < limits.tile_styled_bytes.ceiling as u64,
        "fixture: the styled share must land strictly inside the bracket to prove the \
         arithmetic, not a clamp: {inside:?}"
    );
}

/// **A pane is priced at its own size, not the window's.** Six 3D panes on a
/// 1920 x 1080 window are six 640 x 540 offscreens, which together cost what
/// one window-sized offscreen does; priced at the window they cost six times
/// that. With ground on, the window figure takes the `Half` rung that no
/// pane-sized offscreen needs — the ladder stepping down for a scene that
/// never asked it to.
#[test]
fn six_pane_sized_offscreens_cost_a_sixth_of_six_window_sized_ones() {
    let b = desktop();
    assert_eq!(
        b.offscreen_bytes,
        20 * MIB as usize,
        "fixture: the class rung"
    );
    let offscreens = |scene: Scene| need_terms(&scene, &b, stand_in_grid_bytes).offscreens;
    const PANE: [u32; 2] = [640, 540];

    let pane_priced = offscreens(scene_of(vec![volume_pane(PANE, GroundPass::Off); 6]));
    let window_priced = offscreens(scene_of(vec![volume_pane(HD, GroundPass::Off); 6]));
    assert_eq!(pane_priced, 6 * 640 * 540 * 4);
    assert_eq!(pane_priced, 8_294_400);
    assert_eq!(window_priced, 6 * 1920 * 1080 * 4);
    assert_eq!(window_priced, 49_766_400);
    assert_eq!(window_priced - pane_priced, 41_472_000);

    let grounded = offscreens(scene_of(vec![volume_pane(PANE, GroundPass::On); 6]));
    assert_eq!(
        grounded,
        6 * 640 * 540 * 16,
        "six native pane-sized grounds"
    );
    assert_eq!(grounded, 33_177_600);
    assert_eq!(
        b.quality_ceiling
            .fit(PANE, b.offscreen_bytes, GroundPass::On)
            .quality
            .resolution,
        ResolutionRung::Native,
    );
    assert_eq!(
        b.quality_ceiling
            .fit(HD, b.offscreen_bytes, GroundPass::On)
            .quality
            .resolution,
        ResolutionRung::Half,
        "the window figure with ground takes a rung a pane-sized one never needs",
    );
}

/// **A shown overlay picture is priced at the planner's own arithmetic.** The
/// planner (`squallar_egui::overlay_cache::plan_overlay_texture`) sizes a
/// side as `(side * scale) as u32` in `f32`; this crate sizes it as
/// `side * percent / 100` in integers. For every entry of the oversampling
/// table — 3/2, 5/4, 1/1, each exactly representable — and every side up to
/// the largest 2D texture any adapter reports, the two truncate to the same
/// pixel. On the user's own 2878 x 1651 window that is 42,755,568 B at 1.5x,
/// 29,682,444 B at 1.25x and 19,006,312 B at 1x; on the 2878 x 1611 PANE
/// inside it — the window less its forty-point top bar, and the rect the
/// planner is actually handed — 41,719,488 B, which is what the
/// `overlay pictures:` line reported on both Tier-2 `huge` legs.
#[test]
fn a_shown_picture_is_priced_at_the_planners_own_arithmetic() {
    let planner =
        |side: u32, percent: u16| ((side as f32 * (f32::from(percent) / 100.0)) as u32) as u64;
    for percent in OVERLAY_OVERSAMPLE_PERCENTS {
        for side in (1..=16384u32).chain([2878, 1651, 32767, 32768]) {
            assert_eq!(
                picture_bytes([side, 1], percent) / 4,
                planner(side, percent),
                "side {side} at {percent}%: the integer side and the planner's f32 side \
                 truncate to different pixels",
            );
        }
    }
    assert_eq!(picture_bytes([2878, 1651], 150), 42_755_568);
    assert_eq!(picture_bytes([2878, 1651], 125), 29_682_444);
    assert_eq!(picture_bytes([2878, 1651], 100), 19_006_312);
    assert_eq!(
        picture_bytes([2878, 1611], 150),
        41_719_488,
        "the leg's own"
    );
    assert_eq!(picture_bytes([0, 1651], 150), 0);
    assert_eq!(
        picture_bytes([u32::MAX, u32::MAX], 150),
        u64::MAX,
        "saturates"
    );
}

/// **The `huge` leg fits the page heap at no host rung, on any of its three
/// shapes**, and the one-picture undercount that was fitted in its place fits
/// with room to spare. Four shapes of one fixture, and the distance between
/// the first and the last is the defect.
///
/// **The still shape** — the leg's pane with its loop stopped — is the
/// picture arithmetic. Thirteen pictures at 1.5x on the leg's own 2878 x 1611
/// pane are 542,353,344 B, and **the renderer's upload queue holds the same
/// batch again**: the queue drains one `BLOCKING_BAND_BYTES` band a frame for
/// the whole queue on every device without a staging ring, so a batch that
/// size is some 135 frames of draining while the shown layers re-rasterise
/// together on every move. That is 1,084,706,688 B of pictures before a tile,
/// an arrival, a parked volume or a render peak joins, against three quarters
/// of a 1 GiB page heap (805,306,368 B). At 1x — every host rung at its stop
/// — the same scene is still 128,728,684 B over. The census measured the two
/// picture families separately on one tick of the leg (`overlay pictures`
/// 167 – 215 MB, `upload pending` 424 – 527 MB) and the model priced the
/// second at zero until 2026-09-06.
///
/// **The Level III shape is the one this correction turns around**, and it is
/// the cleanest evidence because it isolates the picture terms: the same
/// thirteen pictures, playing a product derived from paired objects, so its
/// site's decoded volumes are dropped and the scan term is nothing at all.
/// Before the upload term it fitted after one oversampling step with
/// 50,412,244 B to spare, on a page the leg measured at 1,095 – 1,174 MB
/// resident. It now goes to the stops and is 44,842,604 B over.
///
/// **The Level II shape** is the leg itself and was already refused: its
/// eleven named frames' decoded volumes are 563,798,345 B on top.
///
/// **The undercount, which is what actually ran on the leg**: one picture per
/// pane — the figure a walk over panes produced before the roster was counted
/// — is 558,456,052 B and fits the same allowance, so `fit` correctly
/// answered "nothing to shed" to a question that was a batch and a half short
/// of the scene. The leg's last telemetry read `steps 0` and `oversample 150`
/// at 1011 of 1024 MiB of page heap, which is that answer, printed.
///
/// **The desktop bracket is over by more and sheds further**, and that is not
/// a bracket disagreeing with itself: its render peak is 8192^2 x 16 B = 1.00
/// GiB against the web's 64.00 MiB, the same class figure `static_rasters`
/// has always carried on the GPU axis reaching the other memory. So a host
/// need is not equal across brackets at the same scene and the same RAM.
#[test]
fn the_huge_leg_fits_at_no_host_rung_and_the_one_picture_undercount_fitted() {
    let leg = huge(13);
    let still = |pictures: usize| {
        let mut scene = huge(pictures);
        scene.panes[0].looping = false;
        scene
    };
    let wasm = shipped_profile(BudgetLimits::WASM);
    let top = resolve(&wasm);
    let presumed = Capacity::presumed(&BudgetLimits::WASM);
    assert_eq!(
        presumed.host_bytes,
        Some(1 << 30),
        "the page's declared ceiling"
    );
    assert_eq!(presumed.host_allowance(), Some(805_306_368));

    // **The undercount, priced.** One picture per pane is the figure the leg
    // was fitted at, and it fits: the difference between this line and the
    // one below is the whole defect, and neither the allowance nor the tile
    // term is in it.
    let undercounted = need(&still(1), &top, stand_in_grid_bytes).host_bytes;
    // 558,456,052 until 2026-09-08; the 16,777,216 B difference is this pane's
    // 2048^2 value grid, which the render no longer allocates.
    assert_eq!(undercounted, 541_678_836);
    assert_eq!(
        over(&still(1), &top, &presumed, stand_in_grid_bytes),
        (false, false),
        "counting a pane's pictures as one is what let the `huge` leg fit at \
         the top rung and then trap at 1011 of 1024 MiB",
    );

    let scene = still(13);
    let at_top = need_terms(&scene, &top, stand_in_grid_bytes);
    assert_eq!(at_top.tiles_host, 193 * 1_462_708);
    assert_eq!(at_top.pictures_host, 13 * 41_719_488);
    // The batch the dispatch holds and the same batch in the upload queue:
    // two generations of one set of layers, both resident, both measured.
    assert_eq!(at_top.upload_pending_host, at_top.pictures_host);
    assert_eq!(at_top.picture_arrival_host, 41_719_488);
    assert_eq!(at_top.loop_scans_host, 0, "the still shape plays no loop");
    // The volume the still it is showing was decoded from — the term the
    // radar-loop arm does not reach, because this pane runs no radar loop.
    assert_eq!(at_top.still_scans_host, LOOP_SCAN_RESERVE_BYTES);
    // The web bracket's whole 2048^2 raster with the claim buffer that painted
    // it, both alive together at the instant the render runs. It was sixteen
    // bytes a pixel until 2026-09-08, when the value grid between them went.
    assert_eq!(
        at_top.render_peak_host,
        top.static_frame_cost().host_peak() as u64
    );
    assert_eq!(at_top.render_peak_host, 2048 * 2048 * 12);
    // Every host term of the shape, summed here rather than re-typed as one
    // literal, so a term that moves names itself.
    assert_eq!(
        at_top.total().host_bytes,
        at_top.tiles_host
            + at_top.pictures_host
            + at_top.upload_pending_host
            + at_top.picture_arrival_host
            + at_top.still_scans_host
            + at_top.render_peak_host,
    );
    // 1,559,723,764 until 2026-09-08; the 16,777,216 B difference is the
    // 2048^2 value grid this render no longer allocates, and it arrives here
    // through `render_peak_host` in the sum above.
    assert_eq!(at_top.total().host_bytes, 1_542_946_548);
    assert_eq!(
        over(&scene, &top, &presumed, stand_in_grid_bytes),
        (false, true)
    );

    // **At every host rung's stop it is still over.** The rungs that answer
    // the host axis are the margin twice and the tile snap; the raster rung
    // moves nothing on a bracket whose ceiling already is its floor, and the
    // loop-history rung is off this axis by ruling 15.
    let fitted = fit(&scene, &wasm, &presumed, stand_in_grid_bytes);
    assert_eq!(fitted.steps_back, 3);
    assert_eq!(fitted.overlay_oversample_percent, 100);
    assert!(fitted.tile_whole_zoom);
    assert_eq!(fitted.loop_render_budget, top.loop_render_budget);
    assert_eq!(fitted.raster_side_ceiling_px, top.raster_side_ceiling_px);
    assert_eq!(fitted.grid_cells, top.grid_cells);
    assert_eq!(fitted.quality_ceiling, top.quality_ceiling);
    let at_floor = need_terms(&scene, &fitted, stand_in_grid_bytes);
    assert_eq!(at_floor.pictures_host, 13 * 18_545_832);
    assert_eq!(at_floor.upload_pending_host, at_floor.pictures_host);
    // 934,035,052 until 2026-09-08, less the 16,777,216 B of value grid the
    // render no longer allocates. It is still over — the ladder still runs out
    // with the shape above the allowance — so the ruling this test records is
    // unchanged and only the margin moved.
    assert_eq!(at_floor.total().host_bytes, 917_257_836);
    assert_eq!(
        at_floor.total().host_bytes - presumed.host_allowance().unwrap(),
        111_951_468,
        "what the still shape is over by once the ladder has nothing left",
    );
    assert_eq!(
        over(&scene, &fitted, &presumed, stand_in_grid_bytes),
        (false, true)
    );
    assert!(every_host_rung_at_its_stop(&fitted, &wasm.limits));
    assert!(fit_holds(
        &scene,
        &fitted,
        &wasm.limits,
        &presumed,
        stand_in_grid_bytes
    ));
    // A shape over at every host rung is over under every presumption below
    // the one it was tested against, so the watermark's steps buy nothing
    // here — which is what `floor_need` exists to say.
    let lowered = |tenths: u64| presumed.host_held_to(Some((1u64 << 30) * tenths / 100));
    for tenths in [90u64, 81] {
        let under = fit(&scene, &wasm, &lowered(tenths), stand_in_grid_bytes);
        assert_eq!(
            under, fitted,
            "a lower presumption cannot move a shape already at the stops",
        );
    }
    assert_eq!(
        floor_need(&scene, &wasm, stand_in_grid_bytes).host_bytes,
        at_floor.total().host_bytes,
        "the ladder's floor is where `fit` left it",
    );

    // **The same scene, the same host, the desktop bracket: further, and
    // still over.** Its render peak is 8192^2 x 16 B against the web's
    // 2048^2 x 12 B, so the raster rung is a host lever there and is taken.
    let desktop = DeviceProfile {
        class: DeviceClass::Discrete,
        vram_bytes: Some(24 << 30),
        system_ram_bytes: Some(1 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let measured = desktop.capacity();
    assert_eq!(measured.source, CapacitySource::Measured);
    assert_eq!(measured.host_allowance(), Some(805_306_368));
    assert_eq!(
        resolve(&desktop).static_frame_cost().host_peak(),
        8192 * 8192 * 12,
        "0.75 GiB, still sixteen times the web bracket's peak - both fell by \
         the same quarter when the value grid left the render",
    );
    let on_desktop = fit(&scene, &desktop, &measured, stand_in_grid_bytes);
    assert_eq!(on_desktop.overlay_oversample_percent, 100);
    assert!(on_desktop.tile_whole_zoom);
    assert_eq!(
        on_desktop.raster_side_ceiling_px,
        BudgetLimits::DESKTOP.long_range_image_side_px.floor,
        "the raster rung is reachable from the host axis now that a host term \
         is sized from it",
    );
    assert!(
        need(&scene, &on_desktop, stand_in_grid_bytes).host_bytes
            > measured.host_allowance().unwrap(),
        "the desktop bracket cannot pay for this scene at any host rung",
    );
    assert!(every_host_rung_at_its_stop(&on_desktop, &desktop.limits));
    assert!(fit_holds(
        &scene,
        &on_desktop,
        &desktop.limits,
        &measured,
        stand_in_grid_bytes
    ));

    // **The leg itself, loop playing** — and its scans reconciled: the
    // eleven frames that had arrived are priced at what they measured
    // (48.88 MiB apiece, the fixture's modelled median) and the rest at the
    // 80 MiB reserve.
    //
    // **The web arm names fourteen frames now, and it named eleven before
    // 2026-09-06 (ruling 13).** The pane's lookback is two hours and the web
    // bracket budgets forty-five minutes, and `frames_for_span_of` used to
    // open `span_secs.min(self.loop_span_secs)`: the user's two hours were
    // cut to the bracket's 2700 s and 1 + 2700 / 259 = 11 frames, with
    // nothing said. The request is now the user's whole two hours —
    // 1 + 7200 / 259 = 28 frames — and what answers is the reachable
    // ceiling, which on this unmeasured bracket is the class figure of 14.
    // Three of those fourteen have not arrived, so they are charged the
    // reserve.
    assert_eq!(
        top.frames_requested_for_span_of(TWO_HOURS, PRECIP),
        28,
        "1 + 7200 / 259: the user's own lookback, unshortened"
    );
    assert_eq!(loop_frames(&leg.panes[0], &top), 14, "what the web reaches");
    assert_eq!(leg.panes[0].loop_scans_resident_frames, 11);
    let playing = need_terms(&leg, &top, stand_in_grid_bytes);
    assert_eq!(
        playing.loop_scans_host,
        11 * HUGE_LEG_SCAN_BYTES + 3 * LOOP_SCAN_RESERVE_BYTES,
    );
    assert_eq!(playing.loop_scans_host, 815_456_585);
    assert_eq!(
        playing.still_scans_host, 0,
        "a pane running a radar loop pays for its volumes through the loop",
    );
    assert_eq!(
        playing.total().host_bytes,
        at_top.total().host_bytes - at_top.still_scans_host + playing.loop_scans_host,
        "on the host the loop swaps the parked still for its own volumes",
    );
    // 2,291,294,269 until 2026-09-08, less this render's 2048^2 value grid.
    // The relation above it is what checks the figure; this literal only
    // records where the relation lands.
    assert_eq!(playing.total().host_bytes, 2_274_517_053);
    assert_eq!(
        playing.total().gpu_bytes,
        at_top.total().gpu_bytes + playing.loops,
        "on the GPU it adds its frames: 14 textures at the web loop side",
    );
    assert_eq!(playing.loops, 14 * 4 * MIB);

    // **The same leg before its first volume arrived** is the admission
    // price: every named frame pending, at the reserve. The difference
    // between the two lines is what reconciliation is worth on this scene —
    // 1,174,405,120 - 815,456,585 = 358,948,535 B, 30.6 % of the charge —
    // and the reserve is only ever the larger of the two, which is the
    // direction a bound must err.
    //
    // **The difference itself did not move when the frame count went from
    // eleven to fourteen** (ruling 13, above), and that is the arithmetic
    // saying the right thing rather than a coincidence: reconciliation is
    // worth `resident x (reserve - measured)`, and the three frames the
    // wider span added are pending on both lines. What fell is the share:
    // 38.9 % of a smaller charge, 30.6 % of this one.
    //
    // **Both sides of that subtraction moved, and neither figure published
    // while they moved separately survived.** The reserve rose 64 -> 80 MiB
    // when the corpus maximum was corrected; the measured frames rose
    // 46.5 -> 48.88 MiB when `scan_bytes` stopped charging vector length for
    // vector capacity. Holding either side at its old value gives an answer
    // that looks reasonable and is wrong — 41.9 % with the frames held, 23.6 %
    // with the reserve held — so the figure is re-derived from the merged pair
    // rather than from either lane's arithmetic. Reconciliation is worth more
    // than it was, but by less than the reserve's rise alone implies, because
    // what it discharges to rose too.
    let pending = need_terms(&huge_pending(13), &top, stand_in_grid_bytes);
    assert_eq!(pending.loop_scans_host, 14 * LOOP_SCAN_RESERVE_BYTES);
    assert_eq!(pending.loop_scans_host, 1_174_405_120);
    assert_eq!(
        pending.loop_scans_host - playing.loop_scans_host,
        11 * (LOOP_SCAN_RESERVE_BYTES - HUGE_LEG_SCAN_BYTES),
    );
    assert_eq!(
        pending.loop_scans_host - playing.loop_scans_host,
        358_948_535
    );
    // 2,650,242,804 until 2026-09-08, less this render's 2048^2 value grid.
    assert_eq!(pending.total().host_bytes, 2_633_465_588);

    let leg_fitted = fit(&leg, &wasm, &presumed, stand_in_grid_bytes);
    assert_eq!(leg_fitted.overlay_oversample_percent, 100);
    assert!(leg_fitted.tile_whole_zoom);
    assert_eq!(
        leg_fitted.loop_render_budget, top.loop_render_budget,
        "ruling 15 keeps every host rung off the loop's frame count: this \
         scene is a refusal, not a shorter loop",
    );
    assert_eq!(
        need(&leg, &leg_fitted, stand_in_grid_bytes).host_bytes,
        at_floor.total().host_bytes - at_floor.still_scans_host + playing.loop_scans_host,
    );
    // 1,665,605,557 until 2026-09-08, less this render's 2048^2 value grid.
    // The relation above is the check; this records where it lands.
    assert_eq!(
        need(&leg, &leg_fitted, stand_in_grid_bytes).host_bytes,
        1_648_828_341
    );
    assert_eq!(
        over(&leg, &leg_fitted, &presumed, stand_in_grid_bytes),
        (false, true),
        "still over: eleven volumes that have arrived cost what they measured \
         whatever the rung, and the three the wider span named cost the \
         reserve at every rung too",
    );
    assert!(every_host_rung_at_its_stop(&leg_fitted, &wasm.limits));
    assert!(fit_holds(
        &leg,
        &leg_fitted,
        &wasm.limits,
        &presumed,
        stand_in_grid_bytes
    ));
    assert_eq!(leg_fitted.grid_cells, top.grid_cells);
    assert_eq!(
        leg_fitted.raster_side_ceiling_px,
        top.raster_side_ceiling_px
    );
    assert_eq!(leg_fitted.quality_ceiling, top.quality_ceiling);
    // The pending shape sheds the same rungs and is over by more: the
    // reconciliation changes what the scene costs, never which rungs answer.
    let pending_fitted = fit(&huge_pending(13), &wasm, &presumed, stand_in_grid_bytes);
    assert_eq!(pending_fitted, leg_fitted);
    assert_eq!(
        need_terms(&huge_pending(13), &pending_fitted, stand_in_grid_bytes).loop_scans_host,
        pending.loop_scans_host,
        "no rung reaches the frame count, so the reserve stands at every rung",
    );
    assert_eq!(
        need(&huge_pending(13), &pending_fitted, stand_in_grid_bytes).host_bytes
            - need(&leg, &leg_fitted, stand_in_grid_bytes).host_bytes,
        358_948_535,
        "the same reconciliation difference, at the stops",
    );

    // **The desktop bracket names 28 frames and holds eleven of them**, so
    // the term is the mixed case: 11 x 48.88 MiB measured + 17 x 80 MiB
    // reserved = 563,798,345 + 1,426,063,360. The volumes are a host figure
    // the bracket does not change; what the bracket changes is how many
    // frames the loop names, and every frame it names past what has arrived
    // is charged the bound.
    let desktop_top = resolve(&desktop);
    assert_eq!(loop_frames(&leg.panes[0], &desktop_top), 28);
    let on_desktop_terms = need_terms(&leg, &desktop_top, stand_in_grid_bytes);
    assert_eq!(
        on_desktop_terms.loop_scans_host,
        11 * HUGE_LEG_SCAN_BYTES + 17 * LOOP_SCAN_RESERVE_BYTES,
    );
    assert_eq!(on_desktop_terms.loop_scans_host, 1_989_861_705);
    let leg_on_desktop = fit(&leg, &desktop, &measured, stand_in_grid_bytes);
    assert_eq!(leg_on_desktop.overlay_oversample_percent, 100);
    assert!(leg_on_desktop.tile_whole_zoom);
    assert_eq!(
        leg_on_desktop.raster_side_ceiling_px,
        BudgetLimits::DESKTOP.long_range_image_side_px.floor,
    );
    assert!(every_host_rung_at_its_stop(
        &leg_on_desktop,
        &desktop.limits
    ));
    assert!(fit_holds(
        &leg,
        &leg_on_desktop,
        &desktop.limits,
        &measured,
        stand_in_grid_bytes
    ));

    // **The Level III shape, which is the one this correction turns
    // around.** Its frames are rendered from paired objects, so the scan term
    // is nothing and the scene is the pictures, the tiles and the render:
    // before the upload queue was priced it fitted after one oversampling
    // step with 50,412,244 B to spare, on a page the leg measured at
    // 1,095 - 1,174 MB. It now goes to the stops and is over.
    let l3 = huge_level3(13);
    let l3_terms = need_terms(&l3, &top, stand_in_grid_bytes);
    assert_eq!(l3_terms.loop_scans_host, 0);
    assert_eq!(l3_terms.still_scans_host, 0, "the pane is looping radar");
    assert_eq!(
        l3_terms.total().host_bytes,
        at_top.total().host_bytes - at_top.still_scans_host,
        "the still shape without the volume it was parked at",
    );
    // 1,475,837,684 until 2026-09-08, less this render's 2048^2 value grid.
    assert_eq!(l3_terms.total().host_bytes, 1_459_060_468);
    assert_eq!(
        l3_terms.total().gpu_bytes,
        playing.total().gpu_bytes,
        "a Level III loop still holds its frames' textures",
    );
    let l3_fitted = fit(&l3, &wasm, &presumed, stand_in_grid_bytes);
    assert_eq!(l3_fitted.overlay_oversample_percent, 100);
    assert!(l3_fitted.tile_whole_zoom);
    let l3_at_floor = need(&l3, &l3_fitted, stand_in_grid_bytes).host_bytes;
    // 850,148,972 until 2026-09-08, less this render's 2048^2 value grid.
    assert_eq!(l3_at_floor, 833_371_756);
    assert_eq!(
        over(&l3, &l3_fitted, &presumed, stand_in_grid_bytes),
        (false, true),
        "the Level III leg no longer fits either: the upload queue is the \
         term that turned this shape around",
    );
    assert_eq!(
        l3_at_floor - presumed.host_allowance().unwrap(),
        28_065_388,
        "what the Level III leg is over by at every host rung",
    );
    // Priced as it was before the upload term, this shape fitted with room —
    // spelled as subtraction on one figure rather than as a second build, so
    // the claim "this term is what turned it" is arithmetic and not a
    // comparison of two programs.
    let l3_without_upload =
        l3_at_floor - need_terms(&l3, &l3_fitted, stand_in_grid_bytes).upload_pending_host;
    assert!(
        l3_without_upload < presumed.host_allowance().unwrap(),
        "{l3_without_upload} against {:?}",
        presumed.host_allowance(),
    );
    assert_eq!(
        need(&leg, &leg_fitted, stand_in_grid_bytes).host_bytes
            - presumed.host_allowance().unwrap(),
        843_521_973,
        "what the Level II leg is over by at every host rung — 860,299,189 B \
         until the value grid left the render on 2026-09-08, and 608,640,949 B \
         until ruling 13 stopped cutting the pane's two-hour lookback to the \
         bracket's forty-five minutes, which is three more frames at the \
         reserve",
    );
}

/// **A host figure nobody reads bounds nothing.** The native presumed arm
/// carries no host capacity, so the same thirteen pictures are fitted on
/// the GPU axis alone and stay at the class rung — exactly as before the
/// host term existed — while the need itself is still priced.
#[test]
fn a_capacity_with_no_host_figure_never_sheds_for_the_host() {
    let scene = huge(13);
    for limits in [BudgetLimits::DESKTOP, BudgetLimits::MOBILE] {
        let profile = shipped_profile(limits);
        let cap = Capacity::presumed(&limits);
        assert_eq!(cap.host_bytes, None, "{}", limits.name);
        assert_eq!(cap.host_allowance(), None);
        assert_eq!(cap.host_held_to(Some(1)).host_bytes, None);
        let fitted = fit(&scene, &profile, &cap, stand_in_grid_bytes);
        assert_eq!(fitted, resolve(&profile), "{}", limits.name);
        assert!(need(&scene, &fitted, stand_in_grid_bytes).host_bytes > 800_000_000);
    }
}

/// The Framework 13 that froze, in the terms the profile carries: an AMD
/// Radeon 890M — integrated, and no Vulkan reader is believed for one — beside
/// 86.2 GiB of RAM. The GPU reading is `None` because
/// `squallar::capacity::trust_local_heaps` answers `false` for every device
/// type but `DiscreteGpu`, so nothing on that machine ever asks the driver.
const FRAMEWORK_13_RAM: u64 = 862 * (1 << 30) / 10;

/// That machine's profile, with `available` as the caller names it.
fn framework_13(available: Option<u64>) -> DeviceProfile {
    DeviceProfile {
        class: DeviceClass::Integrated,
        vram_bytes: None,
        system_ram_bytes: Some(FRAMEWORK_13_RAM),
        host_pool_bytes: available,
        ..shipped_profile(BudgetLimits::DESKTOP)
    }
}

/// **What each arm of [`CapacitySource`] buys, one row each.**
///
/// The enum is provenance and every consumer decides on it, so the table is
/// what those decisions add up to at one figure. The fraction separates the
/// arms that name real memory from the bracket constant that does not; the
/// tile caches separate the arms a reader answered on from the arms nothing
/// measured. The loop pool's ceiling is the third consumer and lives in
/// `squallar-app` beside the pool it bounds
/// (`loop_pool::tests::what_each_capacity_source_arm_buys_the_loop_pool`).
#[test]
fn what_each_capacity_source_arm_buys() {
    const GPU: u64 = 24 << 30;
    let limits = BudgetLimits::DESKTOP;
    let profile = shipped_profile(limits);
    let budgets = resolve(&profile);
    let scene = scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]);

    // source | allowance | the tile caches are the bracket's, not the economy's
    let table = [
        (CapacitySource::Measured, GPU / 4 * 3, false),
        (CapacitySource::Probed, GPU / 4 * 3, false),
        (CapacitySource::Derived, GPU / 4 * 3, true),
        (CapacitySource::Presumed, GPU, true),
    ];
    for (source, allowance, bracket_tiles) in table {
        let cap = Capacity {
            gpu_bytes: GPU,
            host_bytes: None,
            source,
            pools: Pools::Split,
        };
        assert_eq!(cap.allowance(), allowance, "{source:?}: allowance");
        // The inverse closes on every arm, which is what keeps the two
        // matches spelling the same two sets.
        assert_eq!(
            cap.gpu_bytes_for_allowance(cap.allowance()),
            GPU,
            "{source:?}: the round trip",
        );
        assert_eq!(
            tile_cache_budget(&scene, &budgets, &limits, &cap, stand_in_grid_bytes)
                == budgets.tile_cache(),
            bracket_tiles,
            "{source:?}: the tile caches",
        );
    }
    // Not a table of one value wearing four labels: the two groups really do
    // differ, on both columns.
    assert_ne!(GPU / 4 * 3, GPU);
    let economy_split = tile_cache_budget(
        &scene,
        &budgets,
        &limits,
        &Capacity::measured(GPU, None),
        stand_in_grid_bytes,
    );
    assert_ne!(economy_split, budgets.tile_cache());
    assert_eq!(
        economy_split.styled_bytes, limits.tile_styled_bytes.ceiling as u64,
        "a 24 GiB card with a one-pane scene resolves the styled ceiling",
    );
}

/// **A unified adapter's two allowances never sum past its one pool.**
///
/// The property [`Capacity::unified`] exists for, held by construction rather
/// than by a constant: the GPU figure is a share of the pool and the host
/// figure is the rest of it, so the two sum to the pool exactly and their
/// allowances to `NEED_FRACTION` of it — for a reader's figure anywhere in the
/// pool as well as for the divisor's. Swept rather than spot-checked, because
/// the claim is about every input and not about one machine.
#[test]
fn a_unified_adapters_two_allowances_never_sum_past_its_one_pool() {
    let pools = [
        1u64,
        3,
        4 << 30,
        16 << 30,
        FRAMEWORK_13_RAM,
        64 << 30,
        u64::MAX / 2,
    ];
    let mut derived_rows = 0usize;
    let mut measured_rows = 0usize;
    for pool in pools {
        for measured in [
            None,
            Some(0),
            Some(pool / 4),
            Some(pool),
            // A reader that answered more than the pool: held to it, never
            // over it, so the residual can never go negative.
            Some(pool.saturating_add(1)),
            Some(u64::MAX),
        ] {
            let cap = Capacity::unified(pool, measured);
            assert_eq!(
                cap.pools,
                Pools::Unified,
                "pool {pool} measured {measured:?}"
            );
            let host = cap.host_bytes.expect("a unified capacity always has one");
            assert_eq!(
                cap.gpu_bytes + host,
                pool,
                "pool {pool} measured {measured:?}: the shares are not a partition",
            );
            let spent = cap.allowance() + cap.host_allowance().unwrap();
            assert_eq!(spent, cap.joint_allowance());
            assert!(
                spent <= pool,
                "pool {pool} measured {measured:?}: {spent} authorised over {pool}",
            );
            // Three quarters of the pool, to the rounding of two independent
            // shares: never more, and never more than a byte per share less.
            // Spelled as the code spells it — floor(3n/4) without overflowing
            // — because `pool / 4 * 3` is a different, smaller number.
            use crate::constants::NEED_FRACTION as FRAC;
            let three_quarters = |n: u64| n / FRAC.1 * FRAC.0 + (n % FRAC.1) * FRAC.0 / FRAC.1;
            assert!(
                spent <= three_quarters(pool) && spent + 2 >= three_quarters(pool),
                "pool {pool} measured {measured:?}: {spent} against {}",
                three_quarters(pool),
            );
            match cap.source {
                CapacitySource::Derived => derived_rows += 1,
                CapacitySource::Measured => measured_rows += 1,
                other => panic!("{other:?} from Capacity::unified"),
            }
        }
    }
    assert_eq!(derived_rows, pools.len(), "one derived row per pool");
    assert_eq!(measured_rows, pools.len() * 5);
}

/// **On one pool the two needs face one allowance** — against the shape the
/// incident had, which is the only honest comparison.
///
/// Taken alone the collapse is the weaker test: two halves each under their
/// own allowance are always under the sum, so a joint refusal implies an axis
/// was over. What the pair of changes buys is visible only against what was
/// there before — a GPU figure of half the machine's RAM beside a host figure
/// of that same RAM **again**, each tested on its own. Here is one scene and
/// one machine, read both ways: the old shape admits it, the partitioned pool
/// with one admission test refuses it, and a machine with room admits it on
/// either shape, which is the other arm.
#[test]
fn on_one_pool_the_two_needs_face_one_allowance() {
    let limits = BudgetLimits::DESKTOP;
    let profile = shipped_profile(limits);
    let budgets = resolve(&profile);
    let scene = huge(13);
    let need = need(&scene, &budgets, stand_in_grid_bytes);
    let together = need.gpu_bytes + need.host_bytes;

    // A machine whose RAM is four thirds of the scene's host half — so the
    // host reading whole is exactly its allowance, and the pair is over it.
    let pool = need.host_bytes * 4 / 3 + 4;
    let as_the_incident_read_it = Capacity {
        gpu_bytes: pool / 2,
        host_bytes: Some(pool),
        source: CapacitySource::Measured,
        pools: Pools::Split,
    };
    // The two allowances over one machine, which is the defect in one line.
    assert!(
        as_the_incident_read_it.allowance() + as_the_incident_read_it.host_allowance().unwrap()
            > pool,
    );
    assert_eq!(
        over(
            &scene,
            &budgets,
            &as_the_incident_read_it,
            stand_in_grid_bytes
        ),
        (false, false),
        "the shape that froze the machine admitted this scene",
    );

    let partitioned = Capacity::unified(pool, None);
    assert_eq!(
        partitioned.gpu_bytes + partitioned.host_bytes.unwrap(),
        pool
    );
    assert!(together > partitioned.joint_allowance());
    assert_eq!(
        over(&scene, &budgets, &partitioned, stand_in_grid_bytes),
        (true, true),
        "one pool admitted a scene that does not fit it",
    );

    // The other arm, and it has to be here: a unified machine with room
    // admits the same scene. The collapse refuses what does not fit, not
    // everything.
    let roomy = Capacity::unified(together * 4, None);
    assert!(together < roomy.joint_allowance());
    assert_eq!(
        over(&scene, &budgets, &roomy, stand_in_grid_bytes),
        (false, false),
    );
    assert_eq!(
        over(
            &scene,
            &budgets,
            &Capacity {
                pools: Pools::Split,
                ..roomy
            },
            stand_in_grid_bytes,
        ),
        (false, false),
    );
}

/// **A falling pool takes the GPU share down with it** — the property whose
/// absence made the Framework 13 a freeze rather than a shed.
///
/// The host figure was always the receding one, re-read from `MemAvailable`
/// on the telemetry tick. The GPU figure was half of `MemTotal`, which never
/// moves, and nothing in this tree measures GPU residency — so on an
/// integrated part the ladder could not find a reason to demote however tight
/// RAM got. Cut from the same available reading, both halves fall together.
#[test]
fn a_falling_pool_takes_the_gpu_share_down_with_it() {
    let mut previous: Option<(u64, u64, u64)> = None;
    for available in [80u64 << 30, 40 << 30, 16 << 30, 8 << 30, 2 << 30] {
        let cap = framework_13(Some(available)).capacity();
        assert_eq!(cap.source, CapacitySource::Derived);
        assert_eq!(cap.pools, Pools::Unified);
        assert_eq!(cap.gpu_bytes, available / 2, "{available}");
        assert_eq!(cap.host_bytes, Some(available / 2), "{available}");
        let row = (cap.gpu_bytes, cap.allowance(), cap.joint_allowance());
        if let Some(before) = previous {
            assert!(row.0 < before.0, "{available}: the GPU figure stood still");
            assert!(
                row.1 < before.1,
                "{available}: the GPU allowance stood still"
            );
            assert!(
                row.2 < before.2,
                "{available}: the joint allowance stood still"
            );
        }
        previous = Some(row);
    }
    // The total is the last resort and says so, and it is the figure that
    // does not move: a machine with no available reader is exactly the one
    // this arm was named for.
    let unread = framework_13(None).capacity();
    assert_eq!(unread.source, CapacitySource::Derived);
    assert_eq!(unread.gpu_bytes, FRAMEWORK_13_RAM / 2);
}

/// **The Framework 13 sheds as its available memory falls.** The regression
/// test for the incident: the same scene on the same machine, admitted at the
/// class rung with 80 GiB available and walked down the ladder at 512 MiB.
/// Before the partition was cut from the receding figure, every column of
/// this test read the same at both ends.
#[test]
fn the_framework_13_sheds_as_its_available_memory_falls() {
    let scene = huge(13);
    let roomy = framework_13(Some(80 << 30));
    let roomy_cap = roomy.capacity();
    let fitted = fit(&scene, &roomy, &roomy_cap, stand_in_grid_bytes);
    assert_eq!(
        fitted,
        resolve(&roomy),
        "with 80 GiB available the scene fits at the class rung",
    );
    assert_eq!(
        over(&scene, &fitted, &roomy_cap, stand_in_grid_bytes),
        (false, false),
    );

    let tight = framework_13(Some(512 << 20));
    let tight_cap = tight.capacity();
    assert_eq!(
        over(&scene, &resolve(&tight), &tight_cap, stand_in_grid_bytes),
        (true, true),
        "at 512 MiB available the class rung is over the one pool",
    );
    let shed = fit(&scene, &tight, &tight_cap, stand_in_grid_bytes);
    assert!(
        shed.steps_back > fitted.steps_back,
        "the ladder did not move as the machine filled: {} rungs both times",
        shed.steps_back,
    );
    // Over with every rung spent is the runtime's clamp-and-log, not a broken
    // ladder: `fit_holds` still answers true.
    assert!(fit_holds(
        &scene,
        &shed,
        &tight.limits,
        &tight_cap,
        stand_in_grid_bytes
    ));
}

/// **The control: a genuinely measured discrete card is unchanged, in every
/// figure.** The regression that would cost the desktop user real capacity.
/// Every number here is the 24 GiB card's from before the derived arm, the
/// partition and the collapsed admission test existed — two memories, two
/// allowances, two independent tests, and the economy split the reading buys.
#[test]
fn a_measured_discrete_card_is_unchanged_in_every_figure() {
    const VRAM: u64 = 24 << 30;
    const RAM: u64 = 64 << 30;
    let limits = BudgetLimits::DESKTOP;
    let profile = DeviceProfile {
        class: DeviceClass::Discrete,
        vram_bytes: Some(VRAM),
        system_ram_bytes: Some(RAM),
        ..shipped_profile(limits)
    };
    let cap = profile.capacity();
    assert_eq!(cap, Capacity::measured(VRAM, Some(RAM)));
    assert_eq!(cap.source, CapacitySource::Measured);
    assert_eq!(cap.pools, Pools::Split, "a card's memory is its own");
    assert_eq!(cap.gpu_bytes, VRAM);
    assert_eq!(cap.host_bytes, Some(RAM));
    assert_eq!(cap.allowance(), VRAM / 4 * 3);
    assert_eq!(cap.host_allowance(), Some(RAM / 4 * 3));
    assert_eq!(cap.gpu_bytes_for_allowance(cap.allowance()), VRAM);
    // The two pools are tested apart, and neither figure was cut from the
    // other: the host keeps the whole RAM reading.
    let scene = huge(13);
    let budgets = resolve(&profile);
    assert_eq!(
        over(&scene, &budgets, &cap, stand_in_grid_bytes),
        (false, false),
    );
    assert_eq!(fit(&scene, &profile, &cap, stand_in_grid_bytes), budgets);
    assert_eq!(cap.gpu_bytes + cap.host_bytes.unwrap(), VRAM + RAM);
    // The economy split, which is what a reading buys. A card this size
    // resolves every share's ceiling, which on a Discrete row is also the
    // class rung — so the ceilings alone would not show the split is live.
    // A 1 GiB reading of the same card is what shows it.
    let tiles = tile_cache_budget(&scene, &budgets, &limits, &cap, stand_in_grid_bytes);
    assert_eq!(tiles.styled_bytes, limits.tile_styled_bytes.ceiling as u64);
    assert_eq!(tiles.parsed_bytes, limits.tile_parsed_bytes.ceiling as u64);
    assert_eq!(
        tiles.terrain_bytes,
        limits.tile_terrain_bytes.ceiling as u64
    );
    let small = Capacity::measured(1 << 30, Some(RAM));
    assert_ne!(
        tile_cache_budget(&scene, &budgets, &limits, &small, stand_in_grid_bytes),
        budgets.tile_cache(),
        "the economy split is not in force on the measured arm",
    );
}

/// **Both arms of the render-peak term**: a scene that owed it and now sheds
/// for it, and one with room that must not move at all.
///
/// The firing arm is the incident's own shape — a `Pools::Unified` part, where
/// the GPU and host needs face one allowance, so a host term priced at zero is
/// a byte the admission decision cannot see. One still plan-view pane on a
/// 1 GiB pool: 256 MiB of texture and, at the desktop ceiling, 1.00 GiB of
/// host at the instant the render runs. The counterfactual is spelled as
/// subtraction rather than as a second build — the same need with
/// `render_peak_host` taken back out — so the claim "this term is what fires"
/// is arithmetic on one figure and not a comparison of two.
///
/// The control arm is the one that matters more. Over-firing costs a scene
/// rungs it does not owe, and a term that fires on everything is worse than
/// one that fires on nothing: the same scene against a pool with room stays
/// at the class rung, every field of it, and `steps_back` is zero.
#[test]
fn the_render_peak_sheds_a_scene_that_owed_it_and_leaves_one_with_room_alone() {
    let profile = shipped_profile(BudgetLimits::DESKTOP);
    let top = resolve(&profile);
    let scene = scene_of(vec![plan_pane(HD, false, TWO_HOURS, None)]);
    let terms = need_terms(&scene, &top, stand_in_grid_bytes);
    assert_eq!(terms.render_peak_host, 768 * MIB, "8192^2 x 12 B");
    assert_eq!(terms.total().gpu_bytes, 256 * MIB, "8192^2 x 4 B");
    assert_eq!(
        terms.still_scans_host, LOOP_SCAN_RESERVE_BYTES,
        "the volume the still it is showing was decoded from",
    );
    assert_eq!(
        terms.total().host_bytes,
        terms.render_peak_host + terms.still_scans_host,
        "a bare still pane's whole host cost is the render it is showing and \
         the volume that render read",
    );

    // The firing arm.
    let tight = Capacity::unified(1 << 30, None);
    assert_eq!(tight.pools, Pools::Unified);
    let joint = terms.total().gpu_bytes + terms.total().host_bytes;
    assert!(
        joint > tight.joint_allowance(),
        "{joint} against {}",
        tight.joint_allowance(),
    );
    assert!(
        joint - terms.render_peak_host < tight.joint_allowance(),
        "priced as it was before the term, this scene fit with room to spare, \
         which is what makes the term the thing that fires",
    );
    let fitted = fit(&scene, &profile, &tight, stand_in_grid_bytes);
    assert!(fitted.steps_back > 0, "the scene sheds for the term");
    assert_eq!(
        fitted.raster_side_ceiling_px,
        BudgetLimits::DESKTOP.long_range_image_side_px.floor,
        "and the rung it ends on is the raster side, which only answers the \
         host axis because this term is sized from it",
    );
    let after = need(&scene, &fitted, stand_in_grid_bytes);
    assert!(
        after.gpu_bytes + after.host_bytes <= tight.joint_allowance(),
        "and having shed, it fits",
    );
    assert!(fit_holds(
        &scene,
        &fitted,
        &profile.limits,
        &tight,
        stand_in_grid_bytes
    ));

    // The control arm: the same scene, a pool with room, nothing moves.
    let roomy = Capacity::unified(8 << 30, None);
    assert!(joint < roomy.joint_allowance());
    let unshed = fit(&scene, &profile, &roomy, stand_in_grid_bytes);
    assert_eq!(
        unshed, top,
        "a scene that fits must not shed a rung for a term it can pay",
    );
    assert_eq!(unshed.steps_back, 0);
}

/// **The upload queue holds one more of every shown picture**, and the term
/// that prices it is the batch again — not a fraction of it and not a max.
///
/// The measurement is `squallar_egui::heap_census`'s `upload pending`, which
/// sums the `pending` queue's distinct `Arc`s: 424,260,388 – 527,142,364 B on
/// the Tier-2 `huge` leg, against the 542,353,344 B batch that leg's thirteen
/// layers make at 1.5x — 78 to 97 % of a whole batch. The reason it is a
/// whole batch rather than one picture is the drain rate: one
/// `BLOCKING_BAND_BYTES` band a frame for the entire queue on every device
/// without a staging ring, so that batch is some 135 frames of draining while
/// the shown layers re-rasterise together on every move.
///
/// The two arms below are the ones that matter: it is **zero** where no
/// picture is shown, so a scene without overlays cannot shed for it, and it
/// falls with the oversampling rung, so it is monotone down the ladder like
/// every other term.
#[test]
fn the_upload_queue_holds_one_more_of_every_shown_picture() {
    let wasm = resolve(&shipped_profile(BudgetLimits::WASM));
    let bare = need_terms(
        &scene_of(vec![plan_pane(HD, false, TWO_HOURS, None)]),
        &wasm,
        stand_in_grid_bytes,
    );
    assert_eq!(
        bare.upload_pending_host, 0,
        "a scene showing no picture queues none",
    );

    let scene = huge(13);
    let terms = need_terms(&scene, &wasm, stand_in_grid_bytes);
    assert_eq!(terms.pictures_host, 13 * 41_719_488);
    assert_eq!(
        terms.upload_pending_host, terms.pictures_host,
        "the same batch again, a sum and not a max",
    );
    assert_eq!(terms.upload_pending_host, 542_353_344);
    // The measured range, held around the figure rather than inside a
    // comment: the census read at least this and at most a whole batch.
    assert!(
        (424_260_388..=terms.upload_pending_host).contains(&527_142_364),
        "the leg's own reading no longer sits inside the batch this prices",
    );
    // It is in the total, and it is the whole of the difference from the
    // total without it.
    assert_eq!(
        terms.total().host_bytes - terms.upload_pending_host,
        terms.tiles_host
            + terms.pictures_host
            + terms.picture_arrival_host
            + terms.loop_scans_host
            + terms.render_peak_host,
    );
    assert_eq!(terms.total().gpu_bytes, {
        let mut without = terms;
        without.upload_pending_host = 0;
        without.total().gpu_bytes
    });

    // Monotone down the ladder: every rung of the oversampling table prices
    // it at that rung's batch, and each is smaller than the last.
    let mut previous = u64::MAX;
    for percent in OVERLAY_OVERSAMPLE_PERCENTS {
        let at = need_terms(
            &scene,
            &Budgets {
                overlay_oversample_percent: percent,
                ..wasm
            },
            stand_in_grid_bytes,
        );
        assert_eq!(at.upload_pending_host, at.pictures_host, "{percent}%");
        assert!(at.upload_pending_host < previous, "{percent}%");
        previous = at.upload_pending_host;
    }

    // Two panes queue two batches: the renderer's queue is one for the
    // application and holds every band anyone has filed, so this adds where
    // the arrival takes a max.
    let two = Scene {
        panes: vec![scene.panes[0], scene.panes[0]],
        ..scene.clone()
    };
    let both = need_terms(&two, &wasm, stand_in_grid_bytes);
    assert_eq!(both.upload_pending_host, 2 * terms.upload_pending_host);
    assert_eq!(
        both.picture_arrival_host, terms.picture_arrival_host,
        "the arrival is one buffer for the application and stays a max",
    );
}

/// **A pane that is not running a radar loop is parked at a still, and the
/// still is a decoded volume.** The predicate is the exact complement of the
/// scan term's, which is what makes the two add rather than double-charge.
///
/// Five arms, one per shape the scene can describe: a plain 2D still pane
/// pays one reserve; a pane running a radar loop pays through
/// `NeedTerms::loop_scans_host` and nothing here; a pane whose site another
/// pane counts pays neither; a 3D pane rasterises no still and pays nothing;
/// and a pane looping a satellite or a forecast pays here, because its radar
/// is parked even though `looping` is true.
///
/// The census family `still scans` read 94 – 99 MB on the `huge` leg against
/// the 83,886,080 B one reserve is, and the gap is the per-site latest cache,
/// which no scene field names.
#[test]
fn a_pane_not_running_a_radar_loop_pays_for_the_still_it_is_parked_at() {
    let b = desktop();
    let terms = |pane: PaneNeed| need_terms(&scene_of(vec![pane]), &b, stand_in_grid_bytes);

    let still = terms(plan_pane(HD, false, TWO_HOURS, None));
    assert_eq!(still.still_scans_host, LOOP_SCAN_RESERVE_BYTES);
    assert_eq!(still.loop_scans_host, 0);

    let section = terms(PaneNeed {
        view: RenderView::CrossSection,
        ..plan_pane(HD, false, TWO_HOURS, None)
    });
    assert_eq!(section.still_scans_host, LOOP_SCAN_RESERVE_BYTES);

    let radar_loop = terms(plan_pane(HD, true, TWO_HOURS, PRECIP));
    assert_eq!(
        radar_loop.still_scans_host, 0,
        "a radar loop's volumes are the scan term's, charged once",
    );
    assert!(radar_loop.loop_scans_host > 0);

    let shared = terms(PaneNeed {
        loop_scans_shared: true,
        ..plan_pane(HD, false, TWO_HOURS, None)
    });
    assert_eq!(
        shared.still_scans_host, 0,
        "a pane whose site another pane counts holds that pane's volume",
    );

    let volume = terms(volume_pane(HD, GroundPass::Off));
    assert_eq!(volume.still_scans_host, 0, "a 3D pane rasterises no still",);

    let overlay_loop = terms(PaneNeed {
        overlay_frame_bytes: 18_662_400,
        ..plan_pane(HD, true, TWO_HOURS, PRECIP)
    });
    assert_eq!(
        overlay_loop.still_scans_host, LOOP_SCAN_RESERVE_BYTES,
        "a pane looping a satellite has radar parked at a still behind it",
    );
    assert_eq!(overlay_loop.loop_scans_host, 0);

    // A sum, not a max: two still panes are two sites' worth of volume in
    // the worst case, and the scene cannot tell whether they share one.
    let pair = need_terms(
        &scene_of(vec![
            plan_pane(HD, false, TWO_HOURS, None),
            plan_pane(HD, false, TWO_HOURS, None),
        ]),
        &b,
        stand_in_grid_bytes,
    );
    assert_eq!(pair.still_scans_host, 2 * LOOP_SCAN_RESERVE_BYTES);

    // The two fixtures whose second pane is an alias: neither pays twice.
    for scene in [two_panes_one_loop(), two_panes_one_site()] {
        let both = need_terms(&scene, &b, stand_in_grid_bytes);
        assert_eq!(
            both.still_scans_host, 0,
            "both panes are on one site's loop",
        );
    }
}

/// **An overlay loop charges one dispatch pass of its frames**, at the widest
/// looping pane's frame — the burst `App::dispatch_overlay_loop_renders`
/// collects before it spends, whose cap is application-wide because its `asks`
/// list is declared outside the pane walk.
///
/// So it is a **max across panes times the cap**, not a sum: two panes
/// looping two satellite layers still share one pass.
#[test]
fn an_overlay_loop_charges_one_dispatch_pass_of_its_frames() {
    let b = desktop();
    let small = PaneNeed {
        overlay_frame_bytes: 11_059_200,
        cadence_secs: Some(3600),
        ..plan_pane(HD, true, TWO_HOURS, None)
    };
    let large = PaneNeed {
        overlay_frame_bytes: 44_236_800,
        ..small
    };
    let one = need_terms(&scene_of(vec![large]), &b, stand_in_grid_bytes);
    assert_eq!(
        one.loop_pictures_host,
        MAX_OVERLAY_LOOP_RENDERS_PER_PASS as u64 * 44_236_800,
    );
    let both = need_terms(&scene_of(vec![small, large]), &b, stand_in_grid_bytes);
    assert_eq!(
        both.loop_pictures_host, one.loop_pictures_host,
        "one pass for the application, at the widest pane's frame",
    );
    // A radar loop rasterises through its own pipeline and charges nothing
    // here; so does a pane that is not looping at all.
    for pane in [
        plan_pane(HD, true, TWO_HOURS, PRECIP),
        PaneNeed {
            looping: false,
            ..large
        },
    ] {
        assert_eq!(
            need_terms(&scene_of(vec![pane]), &b, stand_in_grid_bytes).loop_pictures_host,
            0,
        );
    }
    // It is a host term and reaches no GPU total.
    assert_eq!(
        one.total().gpu_bytes,
        one.static_rasters + one.loops,
        "the burst is host bytes in transit, not a texture",
    );
}

/// **A loop with no cadence yet prices its whole render budget at the
/// reserve, the ladder cannot touch it, and that is the correct answer** —
/// the shape an admission door reads, and the reason it has to be a door and
/// not a rung.
///
/// Before a listing lands `cadence_secs` is `None`, so
/// `Budgets::frames_for_span_of` answers `loop_render_budget` outright and
/// every one of those frames is pending: on the web bracket that is
/// 14 x 80 MiB = 1,174,405,120 B, **1.46x the whole host allowance**, before
/// a picture or a tile is counted. The only rung whose knob is in that
/// arithmetic is the loop-history rung, and **ruling 15 keeps it off the host
/// axis**: *"frame density is tier 1 too: refuse, never decimate"*, with the
/// negative property *"no governor path lowers a granted loop's frame count
/// or span"*. So `every_host_rung_at_its_stop` answering "nothing left to
/// shed" here is not a gap in the ladder — it is the ladder saying the thing
/// the ruling requires, and what has to happen next is a refusal with text,
/// not a quieter loop.
///
/// What this test therefore pins is the **door's input**: the term is priced,
/// `over` says the host axis and only the host axis, and the ladder reports
/// itself finished. Widening the rung to `Lowers::BOTH` was measured on
/// 2026-09-06 and declined; `budget`'s `LADDER` carries the figures.
#[test]
fn a_loop_with_no_cadence_prices_its_whole_render_budget_and_no_rung_moves_it() {
    let wasm = shipped_profile(BudgetLimits::WASM);
    let top = resolve(&wasm);
    let presumed = Capacity::presumed(&BudgetLimits::WASM);
    let scene = scene_of(vec![PaneNeed {
        loop_scans_needed: true,
        loop_scan_reserve_bytes: 0,
        ..plan_pane([0, 0], true, TWO_HOURS, None)
    }]);
    let at_top = need_terms(&scene, &top, stand_in_grid_bytes);
    assert_eq!(loop_frames(&scene.panes[0], &top), top.loop_render_budget);
    assert_eq!(top.loop_render_budget, WASM_MAX_LOOP_RENDER_BUDGET);
    assert_eq!(
        at_top.loop_scans_host,
        WASM_MAX_LOOP_RENDER_BUDGET as u64 * LOOP_SCAN_RESERVE_BYTES,
    );
    assert_eq!(at_top.loop_scans_host, 1_174_405_120);
    assert!(
        at_top.loop_scans_host > presumed.host_allowance().unwrap(),
        "one term over the whole allowance on its own",
    );
    assert_eq!(
        over(&scene, &top, &presumed, stand_in_grid_bytes),
        (false, true),
        "the host axis alone",
    );

    // The ladder reports itself finished at the class rung, because the one
    // knob in this term's arithmetic is off the host axis by ruling 15.
    let fitted = fit(&scene, &wasm, &presumed, stand_in_grid_bytes);
    assert!(every_host_rung_at_its_stop(&fitted, &wasm.limits));
    assert_eq!(
        fitted.loop_render_budget, top.loop_render_budget,
        "no governor path lowers a granted loop's frame count",
    );
    assert_eq!(
        need_terms(&scene, &fitted, stand_in_grid_bytes).loop_scans_host,
        at_top.loop_scans_host,
        "and the term is unmoved by every rung the walk did take",
    );
    assert_eq!(
        over(&scene, &fitted, &presumed, stand_in_grid_bytes),
        (false, true),
        "still over: this scene is a refusal, not a shed",
    );
    assert!(fit_holds(
        &scene,
        &fitted,
        &wasm.limits,
        &presumed,
        stand_in_grid_bytes
    ));

    // **Walked to its stop, NEITHER axis reaches the frame count.** This arm
    // was written on 2026-09-06 as a witness with the second half inverted —
    // the GPU walk then ended at `MIN_LOOP_FRAMES_PER_PANE`, the standing
    // ruling-15 violation `budget`'s `LADDER` recorded — so that removing the
    // rung had something to turn green rather than a silent pass. The rung is
    // gone and both halves now say the same thing.
    let mut host_probe = top;
    while step_down_for(&mut host_probe, &wasm.limits, false, true) {}
    assert_eq!(
        host_probe.loop_render_budget, top.loop_render_budget,
        "a host walk taken to its stop lowered a granted frame count",
    );
    let mut gpu_probe = top;
    while step_down_for(&mut gpu_probe, &wasm.limits, true, false) {}
    assert_eq!(
        gpu_probe.loop_render_budget, top.loop_render_budget,
        "a GPU walk taken to its stop lowered a granted frame count",
    );
}

/// **The negative property, on every governor path this crate has.**
///
/// Ruling 15's *"no governor path lowers a granted loop's frame count or
/// span"*, checked by driving each path to its stop rather than by reading
/// the ladder's table:
///
/// * **The ladder** — `step_down`, and `step_down_for` on each axis alone,
///   walked until nothing moves, on every shipped bracket and from every
///   promotion.
/// * **`fit`** — the whole walk, against capacities from one byte to a
///   3090's, on scenes that fit and scenes that cannot.
/// * **`refit_under_pressure` and `refit_to_scene`** — the application's two
///   re-fit paths, which are `fit` against a lowered capacity and `fit`
///   against a changed scene; the lowered capacity is modelled here by
///   `Capacity::probed` at each of a sequence of falling figures, which is
///   exactly what `App::refit_under_pressure` hands `fit`.
///
/// The span half is checked with the frame count: `frames_for_span_of` is
/// the only place a span becomes frames, and `loop_span_secs` is not a knob
/// of any rung, so a frame count that never falls is a span that never
/// shortens. `LoopPool::plan`'s half of the property lives beside the pool,
/// in `squallar-app`.
#[test]
fn no_governor_path_lowers_a_granted_loops_frames_or_span() {
    let scenes = [
        ("empty", Scene::empty()),
        (
            "one loop",
            scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]),
        ),
        (
            "one loop, no cadence",
            scene_of(vec![plan_pane(HD, true, TWO_HOURS, None)]),
        ),
        (
            "six loops",
            scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP); 6]),
        ),
        (
            "six loops, a day of lookback",
            scene_of(vec![plan_pane(HD, true, 24 * 60 * 60, PRECIP); 6]),
        ),
    ];
    for limits in BudgetLimits::SHIPPED {
        for class in [DeviceClass::Discrete, DeviceClass::Integrated] {
            let profile = DeviceProfile {
                class,
                ..shipped_profile(limits)
            };
            let top = resolve(&profile);

            // The ladder, on each axis and on both.
            for (gpu, host) in [(true, true), (true, false), (false, true)] {
                let mut probe = top;
                let mut steps = 0;
                while step_down_for(&mut probe, &limits, gpu, host) {
                    steps += 1;
                    assert!(steps < 64, "{}: the ladder did not stop", limits.name);
                    assert_eq!(
                        probe.loop_render_budget, top.loop_render_budget,
                        "{} / gpu {gpu} host {host}: step {steps} lowered a \
                         granted loop's frame count",
                        limits.name,
                    );
                    assert_eq!(
                        probe.loop_span_secs, top.loop_span_secs,
                        "{} / gpu {gpu} host {host}: step {steps} shortened a \
                         granted loop's span",
                        limits.name,
                    );
                }
            }

            // `fit`, and the two re-fit paths, which are `fit` against a
            // capacity or a scene that moved. A falling sequence of probed
            // figures is what `App::refit_under_pressure` hands it.
            for (scene_name, scene) in &scenes {
                let mut previous: Option<usize> = None;
                for gpu in [24822u64 << 20, 4096 << 20, 1024 << 20, 256 << 20, 1] {
                    let cap = Capacity::probed(gpu);
                    let admitted = admit(scene, &profile, &cap, stand_in_grid_bytes);
                    let fitted = fit(scene, &profile, &cap, stand_in_grid_bytes);
                    assert_eq!(
                        fitted.loop_render_budget, top.loop_render_budget,
                        "{} / {scene_name} / {gpu} B: the ladder lowered a \
                         granted loop's frame count",
                        limits.name,
                    );
                    assert_eq!(
                        fitted.loop_span_secs, top.loop_span_secs,
                        "{} / {scene_name} / {gpu} B: the ladder shortened a \
                         granted loop's span",
                        limits.name,
                    );
                    // **The one figure a falling capacity may lower is the
                    // one admission set, and it is lowered before the walk
                    // and never during it.** The pair is what the readout
                    // shows; the ladder is what may not move it.
                    assert_eq!(
                        fitted.loop_frames_reachable, admitted.loop_frames_reachable,
                        "{} / {scene_name} / {gpu} B: a rung moved the \
                         reachable count",
                        limits.name,
                    );
                    if let Some(before) = previous {
                        assert!(
                            fitted.loop_frames_reachable <= before,
                            "{} / {scene_name}: a smaller capacity reached MORE \
                             frames",
                            limits.name,
                        );
                    }
                    previous = Some(fitted.loop_frames_reachable);
                }
            }
        }
    }
}

/// **A pane's own per-site reserve is what its unfetched frames are priced
/// at**, and the sentinel still prices at the class bootstrap.
///
/// Both arms, and the second is the one that protects everybody: every
/// construction site that predates this reserve writes `0` and means "the
/// class figure", so the sentinel arm must price byte-for-byte what this
/// crate priced before the field existed.
///
/// A decoded radar volume is the one scene term whose size is not knowable in
/// advance — no `Content-Length` is read and S3's `Size` sits in a listing
/// document nothing parses — so what is checked here is that the *reserve*
/// reaches the arithmetic, not that any measurement does.
#[test]
fn a_panes_own_scan_reserve_prices_the_frames_it_has_not_fetched() {
    let b = desktop();
    let priced = |pane: PaneNeed| need_terms(&scene_of(vec![pane]), &b, stand_in_grid_bytes);

    let base = plan_pane(HD, true, TWO_HOURS, PRECIP);
    let frames = loop_frames(&base, &b) as u64;
    assert!(frames > 0, "the fixture loop asks for no frames");

    // The sentinel: silence means the class bootstrap, exactly as before.
    let at_sentinel = priced(PaneNeed {
        loop_scan_reserve_bytes: 0,
        ..base
    });
    assert_eq!(
        at_sentinel.loop_scans_host,
        frames * LOOP_SCAN_RESERVE_BYTES,
        "the sentinel stopped pricing at the class bootstrap",
    );

    // A site this session has seen produce larger volumes prices its
    // unfetched frames at what that site actually produces.
    let calibrated = LOOP_SCAN_RESERVE_BYTES + 12 * 1024 * 1024;
    let at_site = priced(PaneNeed {
        loop_scan_reserve_bytes: calibrated,
        ..base
    });
    assert_eq!(
        at_site.loop_scans_host,
        frames * calibrated,
        "the pane's own reserve did not reach the price of its frames",
    );
    assert!(
        at_site.loop_scans_host > at_sentinel.loop_scans_host,
        "calibrating upward did not raise the price",
    );

    // **Evidence never lowers it.** A site whose volumes have all been small
    // has shown that its volumes can be small, not that they cannot be large,
    // and the bootstrap is a 208-volume maximum rather than a guess.
    let at_tiny = priced(PaneNeed {
        loop_scan_reserve_bytes: 1,
        ..base
    });
    assert_eq!(
        at_tiny.loop_scans_host, at_sentinel.loop_scans_host,
        "a small per-site figure priced below the class bootstrap",
    );
}

/// **A resident voxel grid is charged on BOTH memories, and the host half is
/// the index plane and its table.**
///
/// The device holds the widened texture (`stand_in_grid_bytes`, four bytes a
/// cell here as the raymarch's own arithmetic is four bytes a cell plus its
/// layout); the host holds `VolumeGrid::indices` at one byte a cell and the
/// transfer table beside it. Every figure below is spelled from the cell
/// budget rather than as a literal, so a bracket that re-spends its cells
/// costs this test nothing.
#[test]
fn a_voxel_grid_is_priced_on_the_host_as_well_as_the_device() {
    for limits in BudgetLimits::SHIPPED {
        let b = resolve(&shipped_profile(limits));
        let cells =
            u64::from(b.grid_cells[0]) * u64::from(b.grid_cells[1]) * u64::from(b.grid_cells[2]);
        let host = crate::constants::volume_grid_host_bytes(b.grid_cells);
        assert_eq!(
            host,
            cells * crate::constants::HOST_GRID_BYTES_PER_CELL as u64
                + crate::constants::VOLUME_LUT_BYTES as u64,
            "{}: the index plane and the table, and nothing else",
            limits.name,
        );

        let still = need_terms_for_pane(
            &volume_pane([2560, 1440], GroundPass::On),
            &b,
            stand_in_grid_bytes,
        );
        assert_eq!(still.grids, cells * 4, "{}: the texture", limits.name);
        assert_eq!(
            still.volume_grids_host, host,
            "{}: one live grid, one index plane",
            limits.name,
        );
        assert!(
            still.host_bytes() >= host,
            "{}: and it reaches the pane's own host total",
            limits.name,
        );

        // A 2D pane builds no grid on either memory.
        let plan = need_terms_for_pane(
            &plan_pane(HD, true, TWO_HOURS, PRECIP),
            &b,
            stand_in_grid_bytes,
        );
        assert_eq!(plan.grids, 0, "{}", limits.name);
        assert_eq!(
            plan.volume_grids_host, 0,
            "{}: a plan view rasterises, it does not resample",
            limits.name,
        );
    }
}

/// **A 3D loop holds one grid per frame on the host, exactly as it does on the
/// device** — `VolumeStore` keeps an `Arc<VolumeGrid>` for every frame the
/// pass attaches under `Hold::Set`, so the host term is the frame count times
/// one grid's index plane, the same frame count the `loops` term buys textures
/// for. And the two 2D loop arms stay at zero, because their host buffers
/// belong to the render that is running (`render_peak_host`) and not to the
/// frames that are held.
#[test]
fn a_three_d_loop_charges_one_index_plane_per_frame() {
    for limits in BudgetLimits::SHIPPED {
        let b = resolve(&shipped_profile(limits));
        let host = crate::constants::volume_grid_host_bytes(b.grid_cells);
        let cells = host - crate::constants::VOLUME_LUT_BYTES as u64;

        let looping = PaneNeed {
            looping: true,
            loop_span_secs: TWO_HOURS,
            cadence_secs: PRECIP,
            ..volume_pane([2560, 1440], GroundPass::On)
        };
        let terms = need_terms_for_pane(&looping, &b, stand_in_grid_bytes);
        let frames = loop_frames(&looping, &b) as u64;
        assert!(frames > 0, "{}: the fixture has to loop", limits.name);
        // The live grid plus one per frame — the same shape as `grids` plus
        // `loops`, spelled off the frame count rather than pinned.
        assert_eq!(
            terms.volume_grids_host,
            host * (frames + 1),
            "{}: one grid per frame, and the live one beside them",
            limits.name,
        );
        assert_eq!(
            terms.loops,
            cells * 4 * frames,
            "{}: their textures",
            limits.name
        );

        // A plan-view loop and a cross-section loop hold no grid at all.
        for pane in [
            plan_pane(HD, true, TWO_HOURS, PRECIP),
            PaneNeed {
                view: RenderView::CrossSection,
                ..plan_pane(HD, true, TWO_HOURS, PRECIP)
            },
        ] {
            assert_eq!(
                need_terms_for_pane(&pane, &b, stand_in_grid_bytes).volume_grids_host,
                0,
                "{}: a 2D loop's host buffers are the render's, priced at render_peak_host",
                limits.name,
            );
        }

        // An overlay loop rasterises a picture and resamples nothing.
        let overlay = PaneNeed {
            overlay_frame_bytes: 4 << 20,
            ..plan_pane(HD, true, TWO_HOURS, PRECIP)
        };
        assert_eq!(
            need_terms_for_pane(&overlay, &b, stand_in_grid_bytes).volume_grids_host,
            0,
            "{}: an overlay loop builds no grid",
            limits.name,
        );
    }
}

/// **On one pool the grid's host half is inside the only sum there is.**
///
/// The freeze's shape, as a test rather than as a sentence: a `Pools::Unified`
/// capacity tests `gpu + host` against one allowance, so a term that reaches
/// no total escapes the test entirely. Take a joint allowance sized to admit
/// the scene *without* the new term and show the scene is over it *with* one —
/// which can only be true if the term is in the sum.
#[test]
fn on_one_pool_a_grids_host_half_is_inside_the_joint_test() {
    let b = desktop();
    let scene = scene_of(vec![volume_pane([2560, 1440], GroundPass::On)]);
    let terms = need_terms(&scene, &b, stand_in_grid_bytes);
    assert!(
        terms.volume_grids_host > 0,
        "the fixture has to hold a grid",
    );

    let joint = terms.total().gpu_bytes + terms.total().host_bytes;

    // A pool whose allowance lands between the need with the term and the
    // need without it. Sized from the need rather than pinned, so a bracket
    // that re-spends its cells costs this test nothing; the interval it has
    // to land in is asserted rather than assumed, so a rounding that moved it
    // would fail here instead of making the test vacuous.
    // `Capacity::unified` splits the pool in two and `NEED_FRACTION` takes
    // three quarters of each, so the joint allowance is three quarters of the
    // pool: aim it at the need less HALF the new term, which lands strictly
    // inside the interval from either end.
    let aim = joint - terms.volume_grids_host / 2;
    let tight = Capacity::unified(aim * 4 / 3, None);
    assert_eq!(tight.pools, Pools::Unified);
    let allowance = tight.joint_allowance();
    assert!(
        joint > allowance && joint - terms.volume_grids_host < allowance,
        "the fixture must be over WITH the term and under WITHOUT it: \
         {joint} B of need, {allowance} B of allowance, {} B of index plane",
        terms.volume_grids_host,
    );

    assert_eq!(
        over(&scene, &b, &tight, stand_in_grid_bytes),
        (true, true),
        "one pool, one need, one allowance — and the index plane is in it",
    );
    // The counterfactual as subtraction on one figure, not as a second build:
    // priced as it was before this term, the same scene fitted.
    assert!(
        joint - terms.volume_grids_host < allowance,
        "{} B of index plane is the whole of the difference",
        terms.volume_grids_host,
    );
}

/// **The render cache's cap prices what `RenderCache::entry_bytes` measures**:
/// entries times one converted `Color32` raster, four bytes a pixel.
///
/// It is a **cap**, not a need term, and that is the model's own distinction
/// rather than an omission. `crate::scene`'s charter puts what the scene costs
/// in *need* — "a function of what is shown and at what resolution, never of
/// the machine" — and what is held beyond it, evictable first under pressure,
/// in *economy*. A cache of finished rasters is the second kind, which is why
/// the tile caches are sized by [`tile_cache_budget`] from the economy
/// allowance and are absent from [`NeedTerms`] too; `tiles_host` prices only
/// the working set on the glass.
#[test]
fn the_render_cache_cap_prices_a_converted_raster_and_not_a_render() {
    for limits in BudgetLimits::SHIPPED {
        let b = resolve(&shipped_profile(limits));
        assert_eq!(
            b.render_cache_budget_bytes(),
            b.render_cache_entries
                * crate::constants::converted_raster_bytes(b.long_range_image_side_px),
            "{}: entries times one converted raster",
            limits.name,
        );
        // **These were a factor of two apart, and the factor was the value
        // grid.** `host_held` priced the render's RGBA *and* a `Vec<f32>` of
        // the same pixel count, so an entry was half what a render allocated.
        // The grid went on 2026-09-08 — written once a pixel, never read — and
        // a render now holds its raster and nothing else, so the two agree.
        //
        // **This pair therefore no longer tells the two prices apart by
        // value**, and that is worth stating rather than leaving as a passing
        // line: a cap wrongly spelled `host_held` would read the same as one
        // spelled `converted_raster_bytes` today. What still discriminates is
        // `squallar_app`'s `the_budget_and_the_price_are_one_expression`, and
        // the reachability half of the test below.
        assert_eq!(
            crate::constants::converted_raster_bytes(b.long_range_image_side_px),
            crate::constants::plan_view_frame_cost(b.long_range_image_side_px).host_held,
            "{}",
            limits.name,
        );
    }
}

/// **The render cache cap prices the entry and not the render, on every
/// bracket, and its byte bound is reachable wherever the bracket leaves room
/// for one.**
///
/// This test was `..._is_half_what_it_was_...` and asserted a factor of two:
/// the cap had been priced at `entries x host_held` and was corrected to the
/// same side at the entry's own four bytes a pixel, which halved it. **The
/// factor was the value grid.** `host_held` carried a `Vec<f32>` of the same
/// pixel count beside the RGBA until 2026-09-08, when it went for being
/// written once a pixel and never read, and the two prices have been equal
/// since. The relation is spelled as it now stands rather than as a halving
/// that no longer happens; the history is here so the change reads as the
/// grid going rather than as a guard being loosened.
///
/// The second half is the guard against re-introducing the defect the
/// original fix removed, and it is untouched by any of that: a cap at or
/// above `entries x` the largest entry an arm can build is an entry count
/// wearing a byte cap's name.
#[test]
fn the_render_cache_cap_prices_the_entry_and_stays_reachable() {
    for limits in BudgetLimits::SHIPPED {
        let b = resolve(&shipped_profile(limits));
        let a_render_holds = b.render_cache_entries
            * crate::constants::plan_view_frame_cost(b.long_range_image_side_px).host_held;
        assert_eq!(
            b.render_cache_budget_bytes(),
            a_render_holds,
            "{}: the same side, the object's true four bytes a pixel",
            limits.name,
        );

        // **Reachable — where the bracket leaves room for it to be.** The
        // largest entry an arm can build is a raster at its raster ceiling.
        // Where that ceiling is above the long-range side the cap is
        // genuinely below `entries x` the largest entry and the byte bound
        // fires before the count does; where the two sides are EQUAL the cap
        // is exactly `entries x` the largest entry, and no pricing of this
        // side can make it bind — only the hover residual can push a scene
        // over it. That is a property of the bracket, not of this function,
        // and it is asserted in both directions rather than assumed in one.
        let biggest = crate::constants::converted_raster_bytes(b.raster_side_ceiling_px);
        let reach = b.render_cache_entries * biggest;
        if b.raster_side_ceiling_px > b.long_range_image_side_px {
            assert!(
                reach > b.render_cache_budget_bytes(),
                "{}: {} entries of {biggest} B must be able to exceed the {} B cap",
                limits.name,
                b.render_cache_entries,
                b.render_cache_budget_bytes(),
            );
        } else {
            assert_eq!(
                b.raster_side_ceiling_px, b.long_range_image_side_px,
                "{}: a ceiling BELOW the long-range side would invert the bracket",
                limits.name,
            );
            assert_eq!(
                reach,
                b.render_cache_budget_bytes(),
                "{}: with one side the cap IS the entry count, and the byte \
                 bound is reachable only by the hover residual",
                limits.name,
            );
        }
    }
}

/// **The cap falls with the user's own texture ceiling** — the one control
/// that reaches it today, and the reason it is a budget rather than a
/// constant. `TextureCeiling::hold_all` lowers every raster side including
/// the long-range side too, so a user who caps their rasters caps the
/// cache with them.
///
/// **The ladder does NOT lower it, and that is stated rather than asserted
/// away**: the rung that lowers a raster side lowers `raster_side_ceiling_px`
/// and leaves the long-range side alone. So this asserts monotonicity
/// down the ladder — a rung may never *raise* it — and the real fall against
/// the setting.
#[test]
fn the_render_cache_cap_falls_with_the_setting_and_never_rises_on_a_rung() {
    for limits in BudgetLimits::SHIPPED {
        let top = resolve(&shipped_profile(limits)).render_cache_budget_bytes();

        let mut b = resolve(&shipped_profile(limits));
        for step in 0..=9u32 {
            demote(&mut b, &limits, step);
            assert!(
                b.render_cache_budget_bytes() <= top,
                "{}: rung {step} raised the cap",
                limits.name,
            );
        }

        let floor_px = crate::budget::TextureCeiling::FLOOR_PX as usize;
        let held = crate::budget::TextureCeiling::clamped(crate::budget::TextureCeiling::FLOOR_PX)
            .hold_all(resolve(&shipped_profile(limits)));
        assert_eq!(
            held.render_cache_budget_bytes(),
            held.render_cache_entries * crate::constants::converted_raster_bytes(floor_px),
            "{}: the user's floor is what the cache is priced at",
            limits.name,
        );
        assert!(
            held.render_cache_budget_bytes() < top,
            "{}: and it is a real fall, not the identity",
            limits.name,
        );
    }
}

/// **A pane holding polar frames is charged for polar frames, and the
/// substitution reaches `NeedTerms::loops` rather than stopping at the
/// selector.**
///
/// The whole of the budget switch on this side. `PaneNeed::radar_frame_bytes`
/// is what the application measured off the frames the pane is actually
/// holding, and a plan-view loop is charged that instead of
/// `Budgets::loop_frame_cost().gpu` — never the other way round, and never on
/// any signal but the frames themselves.
///
/// The scene is held identical between the arms except for the one field, so
/// the difference in `loops` is that field's and nothing else's; and the frame
/// count is recovered from the raster arm rather than assumed, so this states
/// the substitution without restating `frames_for_span`'s arithmetic.
///
/// TAMPER: delete the measured arm in `loop_frame_bytes` and the polar arm
/// reads the raster's total; make it unconditional and the zero arm goes red.
#[test]
fn a_pane_holding_polar_frames_is_charged_for_polar_frames() {
    let budgets = desktop();
    let raster_frame = budgets.loop_frame_cost().gpu as u64;
    // A real surveillance plane's whole chain, which is what a pane holding
    // one measures. Stated here rather than imported: this test is about the
    // substitution, and it must fail if the substitution stops happening
    // whatever that figure becomes.
    const POLAR_FRAME: usize = 1_758_832;
    assert!(
        (POLAR_FRAME as u64) < raster_frame,
        "premise: the polar figure is the smaller of the two ({POLAR_FRAME} vs \
         {raster_frame}), or the arms below could not tell them apart"
    );

    let terms = |scene: &Scene| need_terms(scene, &budgets, stand_in_grid_bytes);
    let as_raster = terms(&scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]));
    let mut polar_pane = plan_pane(HD, true, TWO_HOURS, PRECIP);
    polar_pane.radar_frame_bytes = POLAR_FRAME;
    let as_polar = terms(&scene_of(vec![polar_pane]));

    // The frame count is the raster arm's own, recovered rather than assumed.
    assert!(
        as_raster.loops > 0,
        "premise: the raster arm charges a loop"
    );
    assert_eq!(as_raster.loops % raster_frame, 0);
    let frames = as_raster.loops / raster_frame;
    assert!(
        frames > 1,
        "premise: more than one frame, or the two arms could \
         differ by a rounding"
    );

    assert_eq!(
        as_polar.loops,
        frames * POLAR_FRAME as u64,
        "the polar pane was charged {} for {frames} frames, which is neither \
         its own frames' price nor the raster's",
        as_polar.loops
    );
    assert!(as_polar.loops < as_raster.loops);
}
