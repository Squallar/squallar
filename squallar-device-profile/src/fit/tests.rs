use super::*;
use crate::budget::{BudgetLimits, Promotion, demote};
use crate::constants::{
    DESKTOP_APP_TEXTURE_BUDGET_BYTES, DESKTOP_LOOP_IMAGE_SIZE, DESKTOP_MAX_LOOP_RENDER_BUDGET,
    DESKTOP_RASTER_SIDE_CEILING, DESKTOP_VOLUME_GRID_CELLS, MIN_LOOP_FRAMES_PER_PANE,
    MOBILE_APP_TEXTURE_BUDGET_BYTES, MOBILE_MAX_LOOP_RENDER_BUDGET, MOBILE_RASTER_SIDE_CEILING,
    MOBILE_VOLUME_GRID_CELLS, WASM_APP_TEXTURE_BUDGET_BYTES, WASM_LOOP_IMAGE_SIZE,
    WASM_MAX_LOOP_RENDER_BUDGET, WASM_RASTER_SIDE_CEILING, WASM_VOLUME_GRID_CELLS,
};
use crate::quality::{DeviceClass, GradientShading, ResolutionRung};
use crate::scene::fixtures::{
    plan_pane, scene_table, shipped_profile, stand_in_grid_bytes, volume_pane,
};
use crate::scene::{CapacitySource, TileNeed};
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

    // A plan-view pane's static render: the raster ceiling's worst case.
    let plan = terms(&scene_of(vec![plan_pane(HD, false, TWO_HOURS, None)]));
    assert_eq!(
        plan,
        NeedTerms {
            static_rasters: b.static_frame_bytes() as u64,
            ..NeedTerms::default()
        },
    );
    assert_eq!(
        plan.static_rasters,
        256 * MIB,
        "8192^2 x 4 B on the desktop class"
    );

    // A cross-section pane's static render: the section frame.
    let section = terms(&scene_of(vec![PaneNeed {
        view: RenderView::CrossSection,
        ..plan_pane(HD, false, TWO_HOURS, None)
    }]));
    assert_eq!(section.static_rasters, b.section_frame_bytes() as u64);

    // A radar loop: the pane's span at its cadence, held to the render budget,
    // at the loop frame's cost.
    let looping = terms(&scene_of(vec![plan_pane(HD, true, TWO_HOURS, PRECIP)]));
    let frames = b.frames_for_span_of(TWO_HOURS, PRECIP);
    assert_eq!(
        frames, 28,
        "1 + 7200 / 259 frames, under the 36 the budget caps at"
    );
    assert_eq!(looping.loops, frames as u64 * b.loop_frame_bytes() as u64,);
    assert_eq!(looping.static_rasters, plan.static_rasters);
    // A pane asking for less than the budget's span gets less; one asking for
    // more is held to the budget's; no cadence yet buys the whole render budget.
    assert_eq!(b.frames_for_span_of(30 * 60, PRECIP), 1 + 1800 / 259);
    assert_eq!(
        b.frames_for_span_of(24 * 60 * 60, PRECIP),
        b.frames_for_span(PRECIP)
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
        volume.offscreens,
        b.quality_ceiling
            .fit(HD, b.offscreen_bytes, GroundPass::Off)
            .bytes() as u64,
    );
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

    // A 3D pane's grid is priced at the bracket's cell budget, a resolution
    // constant, by the one pricer.
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

    // The mirror and the tiles have no bracket term at all.
    let shared = Scene {
        panes: Vec::new(),
        tile_sources: vec![TileNeed {
            tiles_on_glass: 193,
            ancestor_net: 0,
            bytes_per_tile: 1_030_000,
        }],
        mirror_px: [2048, 2048],
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
        assert_eq!(cap.host_bytes, None);
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

/// **`fit` sheds down the ladder only as far as the scene needs.** Six two-hour
/// loops on the desktop bracket cost 6 x (36 x 16 MiB + 256 MiB) = 4992 MiB
/// against a 3840 MiB presumption; the first three steps are 3D rungs that take
/// nothing from a 2D scene but are the ladder's first rungs — lighting, then
/// resolution twice, Native to Half to Quarter, one coarsening a step — and the
/// fourth, the loop history at 36 to 18 frames, is what makes it fit:
/// 6 x (18 x 16 + 256) = 3264 MiB. Nothing further moves.
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
        fitted.steps_back, 4,
        "lighting, resolution twice, one halving of the history"
    );
    assert_eq!(fitted.quality_ceiling.shading, GradientShading::Off);
    assert_eq!(fitted.quality_ceiling.resolution, ResolutionRung::Quarter);
    assert_eq!(
        fitted.loop_render_budget,
        DESKTOP_MAX_LOOP_RENDER_BUDGET / 2
    );
    assert!(
        !fitted.tile_whole_zoom,
        "the tiles were not asked to give anything"
    );
    assert_eq!(fitted.grid_cells, top.grid_cells);
    assert_eq!(fitted.raster_side_ceiling_px, top.raster_side_ceiling_px);
    let after = need(&six, &fitted, stand_in_grid_bytes);
    assert_eq!(after.gpu_bytes, 6 * (18 * 16 + 256) * MIB);
    assert!(after.gpu_bytes <= cap.allowance());

    // The three 3D steps lowered nothing for this scene: the first rung that
    // paid was the loop history, which is the doc's "2D loops shed first".
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
                ..floor
            },
            fitted,
            "{}: the floor `fit` gives up at is not the ladder's floor",
            limits.name,
        );
        assert_eq!(fitted.loop_render_budget, MIN_LOOP_FRAMES_PER_PANE);
        assert!(
            !every_rung_at_its_stop(&resolve(&profile), &limits),
            "{}",
            limits.name
        );
    }
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
    assert_eq!(pool(&six, &fitted), 1728 * MIB, "min(6 x 18 x 16, 2304)");

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
