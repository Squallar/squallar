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
    // And one decoded volume per frame on the host, at the reserve — a bare
    // loop's only host term, since it shows no picture and pans no tiles.
    assert_eq!(
        looping.loop_scans_host,
        frames as u64 * LOOP_SCAN_RESERVE_BYTES
    );
    assert_eq!(looping.loop_scans_host, 28 * 64 * MIB);
    assert_eq!(looping.total().host_bytes, looping.loop_scans_host);
    assert_eq!(plan.loop_scans_host, 0, "a still pane plays from no cache");
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
    assert_eq!(
        overlay.loop_scans_host, 0,
        "a loop of another layer holds its own rasters and no volume"
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

    // Gridded overlays: each enabled layer's budget as its handler states
    // it, once, on the host — MRMS's four desktop grids and GMGSI's four.
    let gridded = terms(&Scene {
        overlay_grids: vec![
            OverlayGridNeed {
                budget_bytes: 196_000_000,
            },
            OverlayGridNeed {
                budget_bytes: 240_000_000,
            },
        ],
        ..Scene::empty()
    });
    assert_eq!(gridded.overlay_grids_host, 436_000_000);
    assert_eq!(gridded.total().gpu_bytes, 0);
    assert_eq!(gridded.total().host_bytes, 436_000_000);
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

    // Eleven arrived at 46.5 MiB apiece: those eleven at their price, the
    // other seventeen at the reserve.
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
                        + whole.overlay_grids_host,
                    "{ctx}: the host whole is the panes plus the tiles, the arrival and the grids",
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
    assert_eq!(o.loop_scans_host, 28 * 64 * MIB);
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
    assert_eq!(
        fitted.overlay_oversample_percent, OVERLAY_OVERSAMPLE_PERCENTS[0],
        "a card over its allowance thinned the overlay margin before the history \
         had paid, or for a byte the GPU model does not price",
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

/// **The pool is the room, capped at the loops' ceiling — not their base.** A
/// pane whose listing has said 300 s over a six-hour lookback has a base of
/// 25 frames (two hours at 300 s, the rung's span: what `fit` charges) and a
/// ceiling of 60 (`MAX_LOOP_FRAMES`; the lookback at that cadence is 73). The
/// pool is sized to the ceiling so the application's planner has room to
/// balloon into: 960 MiB for one such pane under the presumption, and six of
/// them are held to the 2304 MiB of room — the same room six two-hour loops
/// with no cadence get, because the room does not depend on what the loops
/// ask. `fit` asks whether the scene fits and charges the base; the pool asks
/// how much room is left. Where no cadence is known, the ceiling is the base
/// and nothing here moves.
#[test]
fn the_pool_is_the_room_capped_at_the_loops_ceiling_not_their_base() {
    let top = desktop();
    let cap = Capacity::presumed(&BudgetLimits::DESKTOP);
    const SIX_HOURS: usize = 6 * 60 * 60;
    let pane = plan_pane(HD, true, SIX_HOURS, Some(300));

    assert_eq!(loop_frames(&pane, &top), 25, "the base: two hours at 300 s");
    assert_eq!(
        loop_frames_ceiling(&pane, &top),
        60,
        "the ceiling: min(1 + 21600 / 300 = 73, MAX_LOOP_FRAMES = 60)"
    );
    let one = scene_of(vec![pane]);
    assert_eq!(loop_need(&one, &top, stand_in_grid_bytes), 25 * 16 * MIB);
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
    assert_eq!(
        presumed.loop_render_budget,
        DESKTOP_MAX_LOOP_RENDER_BUDGET / 2
    );
    assert_eq!(
        Budgets {
            steps_back: 0,
            quality_ceiling: top.quality_ceiling,
            offscreen_bytes: top.offscreen_bytes,
            app_texture_ceiling_bytes: top.app_texture_ceiling_bytes,
            loop_render_budget: top.loop_render_budget,
            ..presumed
        },
        top,
        "the two arms differ by ladder rungs and nothing else",
    );

    let four_gib = discrete(4096);
    let cap = four_gib.capacity();
    assert_eq!(cap.allowance(), 3072 * MIB);
    let fitted = fit(&six, &four_gib, &cap, stand_in_grid_bytes);
    assert_eq!(
        fitted.steps_back, 5,
        "lighting, resolution twice, two halvings of the history"
    );
    assert_eq!(fitted.loop_render_budget, 9);
    assert_eq!(
        need(&six, &fitted, stand_in_grid_bytes).gpu_bytes,
        2400 * MIB
    );
    let mut one_less = resolve(&four_gib);
    demote(&mut one_less, &BudgetLimits::DESKTOP, 4);
    assert_eq!(
        need(&six, &one_less, stand_in_grid_bytes).gpu_bytes,
        3264 * MIB
    );
    assert!(need(&six, &one_less, stand_in_grid_bytes).gpu_bytes > cap.allowance());
    assert_eq!(
        loop_pool_bytes(&six, &fitted, &cap, stand_in_grid_bytes),
        6 * 9 * 16 * MIB,
        "min(864, 3072 - 1536)",
    );
    assert_eq!(
        fitted.frames_for_span_of(TWO_HOURS, PRECIP),
        9,
        "the pane asked for 28 frames of two hours and holds nine: 2072 s",
    );
    assert_eq!(fitted.grid_cells, one_less.grid_cells);
    assert_eq!(
        fitted.raster_side_ceiling_px,
        one_less.raster_side_ceiling_px
    );
    assert!(!fitted.tile_whole_zoom);

    // A unified-memory part on a 64 GiB host: 32 GiB stands in for the GPU,
    // one loop's pool is its need, and the offscreen stays at the Step the
    // class earns — memory says nothing about fill rate.
    let integrated = DeviceProfile {
        class: DeviceClass::Integrated,
        vram_bytes: None,
        system_ram_bytes: Some(64 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let cap = integrated.capacity();
    assert_eq!(cap.source, CapacitySource::Measured);
    assert_eq!(cap.gpu_bytes, 32 << 30);
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
    assert_eq!(
        need(&six, &fitted, stand_in_grid_bytes).gpu_bytes,
        3264 * MIB
    );
    assert_eq!(
        economy_allowance(&six, &fitted, &presumed, stand_in_grid_bytes),
        (3456 - 3264) * MIB,
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

    // Between the two the shares are what they say: pick an economy that
    // lands every population strictly inside its bracket.
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

/// **The `huge` leg's pictures fit the page heap after one step of the
/// oversampling rung, and its loop does not fit at any host rung.** Two
/// claims, on two shapes of the same fixture.
///
/// **The still shape** — the leg's pane with its loop stopped — is the
/// picture arithmetic: thirteen pictures at 1.5x on the leg's own 2878 x 1611
/// pane plus the 193-tile working set plus one arrival are 866,375,476 B of
/// host need against three quarters of a 1 GiB page heap (805,306,368 B) —
/// over, which is the trap of 2026-09-02 priced. At 1.25x the same scene is
/// 687,785,260 B and fits, so `fit` takes exactly that one step: the 3D
/// ceiling, grid and raster side stay at the class rung, the tiles do not
/// snap. The same bytes on the desktop bracket with a measured 1 GiB of RAM
/// take the same one step. Under the session presumptions the watermark
/// lowers to — nine tenths, then eighty-one hundredths — the rung stays at
/// 1.25x and then goes to 1x, where the scene is 541,944,292 B against
/// 652,298,157 B and fits again.
///
/// **This is the arithmetic that failed to run on the leg**, and the reason
/// is one figure: the need was priced at ONE picture per pane, not thirteen.
/// 41,719,488 B plus an arrival plus the tiles is 365,741,620 B, well inside
/// the 805,306,368 B allowance, so `fit` correctly answered "nothing to
/// shed" to a question that was 500 MB short of the scene. The leg's last
/// telemetry read `steps 0` and `oversample 150` at 1011 of 1024 MiB of
/// page heap, which is that answer, printed.
///
/// **The looping shape is the leg itself**, and an earlier version of this
/// test claimed it fit after the one step too. It did not: the loop's
/// decoded volumes were priced at nothing, the same undercount as the
/// pictures one paragraph up, of the same order. On the web bracket the
/// leg's two-hour loop at 259 s is held to the 45-minute span, 1 + 2700 / 259
/// = 11 frames, and at the 64 MiB reserve apiece that is 738,197,504 B —
/// 91.7 % of the allowance on its own, before a tile or a picture. So the
/// host rungs go to their stops (1.25x, 1x, tiles snapped — three steps) and
/// the scene is still 1,280,141,796 B over an 805,306,368 B allowance;
/// `fit_holds`, because nothing is left to shed on that axis, and no GPU
/// rung moves for it. The desktop bracket's 28 frames are 1,879,048,192 B
/// and the same three steps. Under ruling 13 no host rung shortens the loop;
/// what makes this scene fit is the user's span or, later, a refusal at the
/// door — not a rung.
#[test]
fn the_huge_legs_pictures_fit_after_one_oversampling_step_and_its_loop_fits_at_no_host_rung() {
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

    // **The undercount, priced.** One picture per pane — the figure a walk
    // over panes produces, and the figure the leg was fitted at — is
    // 365,741,620 B and fits the same allowance with 440 MB to spare, so the
    // ladder never moves. The difference between these two lines is the
    // whole defect; neither the allowance nor the tile term is in it.
    let undercounted = need(&still(1), &top, stand_in_grid_bytes).host_bytes;
    assert_eq!(undercounted, 365_741_620);
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
    assert_eq!(at_top.picture_arrival_host, 41_719_488);
    assert_eq!(at_top.loop_scans_host, 0, "the still shape plays no loop");
    assert_eq!(at_top.total().host_bytes, 866_375_476);
    assert_eq!(
        over(&scene, &top, &presumed, stand_in_grid_bytes),
        (false, true)
    );

    let fitted = fit(&scene, &wasm, &presumed, stand_in_grid_bytes);
    assert_eq!(fitted.steps_back, 1, "one step of the oversampling rung");
    assert_eq!(fitted.overlay_oversample_percent, 125);
    assert_eq!(
        need(&scene, &fitted, stand_in_grid_bytes).host_bytes,
        687_785_260
    );
    assert_eq!(
        over(&scene, &fitted, &presumed, stand_in_grid_bytes),
        (false, false)
    );
    assert_eq!(
        Budgets {
            steps_back: 0,
            overlay_oversample_percent: 150,
            ..fitted
        },
        top,
        "a page heap over its allowance moved something other than the margin",
    );
    assert_eq!(fitted.loop_render_budget, top.loop_render_budget);
    assert!(!fitted.tile_whole_zoom);
    assert!(fit_holds(
        &scene,
        &fitted,
        &wasm.limits,
        &presumed,
        stand_in_grid_bytes
    ));

    // The same scene, the same host, the desktop bracket: the same step.
    let desktop = DeviceProfile {
        class: DeviceClass::Discrete,
        vram_bytes: Some(24 << 30),
        system_ram_bytes: Some(1 << 30),
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    let measured = desktop.capacity();
    assert_eq!(measured.source, CapacitySource::Measured);
    assert_eq!(measured.host_allowance(), Some(805_306_368));
    let on_desktop = fit(&scene, &desktop, &measured, stand_in_grid_bytes);
    assert_eq!(on_desktop.steps_back, 1);
    assert_eq!(on_desktop.overlay_oversample_percent, 125);
    assert_eq!(
        need(&scene, &on_desktop, stand_in_grid_bytes).host_bytes,
        need(&scene, &fitted, stand_in_grid_bytes).host_bytes,
        "the same scene costs different host bytes on two brackets",
    );

    // The presumption the watermark lowers to, once and twice.
    let lowered = |tenths: u64| presumed.host_held_to(Some((1u64 << 30) * tenths / 100));
    let once = fit(&scene, &wasm, &lowered(90), stand_in_grid_bytes);
    assert_eq!(
        once.overlay_oversample_percent, 125,
        "nine tenths still holds 1.25x"
    );
    let twice = fit(&scene, &wasm, &lowered(81), stand_in_grid_bytes);
    assert_eq!(twice.steps_back, 2);
    assert_eq!(twice.overlay_oversample_percent, 100);
    assert_eq!(
        need(&scene, &twice, stand_in_grid_bytes).host_bytes,
        541_944_292
    );
    assert_eq!(lowered(81).host_allowance(), Some(652_298_157));
    assert!(
        !twice.tile_whole_zoom,
        "the tiles snapped while the margin could still pay"
    );

    // A scene the host rungs cannot pay for stops at their stops and holds:
    // the tiles snap after the margin is gone, and no GPU rung is touched.
    let wall = presumed.host_held_to(Some(64 << 20));
    let floor = fit(&scene, &wasm, &wall, stand_in_grid_bytes);
    assert_eq!(floor.overlay_oversample_percent, 100);
    assert!(floor.tile_whole_zoom);
    assert_eq!(floor.steps_back, 3);
    assert_eq!(floor.loop_render_budget, top.loop_render_budget);
    assert_eq!(floor.grid_cells, top.grid_cells);
    assert_eq!(floor.raster_side_ceiling_px, top.raster_side_ceiling_px);
    assert!(every_host_rung_at_its_stop(&floor, &wasm.limits));
    assert!(!every_rung_at_its_stop(&floor, &wasm.limits));
    assert!(fit_holds(
        &scene,
        &floor,
        &wasm.limits,
        &wall,
        stand_in_grid_bytes
    ));

    // **The leg itself, loop playing** — and its scans reconciled: the
    // eleven frames the web arm names had all arrived, so they are priced at
    // what they measured (46.5 MiB apiece, the fixture's modelled median),
    // not at the 64 MiB reserve, and nothing is pending.
    assert_eq!(loop_frames(&leg.panes[0], &top), 11, "1 + 2700 / 259");
    assert_eq!(leg.panes[0].loop_scans_resident_frames, 11);
    let playing = need_terms(&leg, &top, stand_in_grid_bytes);
    assert_eq!(playing.loop_scans_host, 11 * HUGE_LEG_SCAN_BYTES);
    assert_eq!(playing.loop_scans_host, 536_346_624);
    assert_eq!(
        playing.total().host_bytes,
        866_375_476 + 536_346_624,
        "the still shape's bytes plus the volumes"
    );
    assert_eq!(playing.total().host_bytes, 1_402_722_100);
    assert_eq!(
        playing.total().host_bytes - playing.loop_scans_host,
        at_top.total().host_bytes,
        "on the host the loop adds its volumes to the still shape and nothing else",
    );
    assert_eq!(
        playing.total().gpu_bytes,
        at_top.total().gpu_bytes + playing.loops,
        "on the GPU it adds its frames: 11 textures at the web loop side",
    );
    assert_eq!(playing.loops, 11 * 4 * MIB);

    // **The same leg before its first volume arrived** is the admission
    // price: every named frame pending, at the reserve. The difference
    // between the two lines is what reconciliation is worth on this scene —
    // 738,197,504 - 536,346,624 = 201,850,880 B, 27.3 % of the charge — and
    // the reserve is only ever the larger of the two, which is the direction
    // a bound must err.
    let pending = need_terms(&huge_pending(13), &top, stand_in_grid_bytes);
    assert_eq!(pending.loop_scans_host, 11 * LOOP_SCAN_RESERVE_BYTES);
    assert_eq!(pending.loop_scans_host, 738_197_504);
    assert_eq!(
        pending.loop_scans_host - playing.loop_scans_host,
        201_850_880
    );
    assert_eq!(pending.total().host_bytes, 1_604_572_980);

    let leg_fitted = fit(&leg, &wasm, &presumed, stand_in_grid_bytes);
    assert_eq!(
        leg_fitted.steps_back, 3,
        "both oversampling steps and the tile snap: every host rung"
    );
    assert_eq!(leg_fitted.overlay_oversample_percent, 100);
    assert!(leg_fitted.tile_whole_zoom);
    assert_eq!(
        need(&leg, &leg_fitted, stand_in_grid_bytes).host_bytes,
        541_944_292 + 536_346_624
    );
    assert_eq!(
        need(&leg, &leg_fitted, stand_in_grid_bytes).host_bytes,
        1_078_290_916
    );
    assert_eq!(
        over(&leg, &leg_fitted, &presumed, stand_in_grid_bytes),
        (false, true),
        "still over: no host rung reaches the volumes"
    );
    assert!(every_host_rung_at_its_stop(&leg_fitted, &wasm.limits));
    assert!(fit_holds(
        &leg,
        &leg_fitted,
        &wasm.limits,
        &presumed,
        stand_in_grid_bytes
    ));
    assert_eq!(
        leg_fitted.loop_render_budget, top.loop_render_budget,
        "no host rung shortens the loop"
    );
    assert_eq!(leg_fitted.grid_cells, top.grid_cells);
    assert_eq!(
        leg_fitted.raster_side_ceiling_px,
        top.raster_side_ceiling_px
    );
    assert_eq!(leg_fitted.quality_ceiling, top.quality_ceiling);
    // The pending shape is over by more and sheds the same rungs: the
    // reconciliation changes what the scene costs, never which rungs answer.
    assert_eq!(
        fit(&huge_pending(13), &wasm, &presumed, stand_in_grid_bytes),
        leg_fitted
    );

    // **The desktop bracket names 28 frames and holds eleven of them**, so
    // the term is the mixed case: 11 x 46.5 MiB measured + 17 x 64 MiB
    // reserved = 536,346,624 + 1,140,850,688. The volumes are a host figure
    // the bracket does not change; what the bracket changes is how many
    // frames the loop names, and every frame it names past what has arrived
    // is charged the bound.
    let desktop_top = resolve(&desktop);
    assert_eq!(loop_frames(&leg.panes[0], &desktop_top), 28);
    let on_desktop = need_terms(&leg, &desktop_top, stand_in_grid_bytes);
    assert_eq!(
        on_desktop.loop_scans_host,
        11 * HUGE_LEG_SCAN_BYTES + 17 * LOOP_SCAN_RESERVE_BYTES,
    );
    assert_eq!(on_desktop.loop_scans_host, 1_677_197_312);
    let leg_on_desktop = fit(&leg, &desktop, &measured, stand_in_grid_bytes);
    assert_eq!(leg_on_desktop.steps_back, 3);
    assert_eq!(leg_on_desktop.overlay_oversample_percent, 100);
    assert!(leg_on_desktop.tile_whole_zoom);
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

    // **The same leg playing a Level III product fits after the one
    // oversampling step**, exactly as the still shape does. Its frames are
    // rendered from paired objects, so its site's decoded volumes are dropped
    // and the scan term is nothing: the scene is the pictures and the tiles
    // again, 866,375,476 B over at 1.5x and 687,785,260 B fitting at 1.25x.
    // That is the whole distance between the two loops on this scene — one
    // rung and a fit, against three rungs and 272,984,548 B still over — and
    // it is why the price asks the retention's own predicate rather than
    // charging every loop for volumes.
    let l3 = huge_level3(13);
    let l3_terms = need_terms(&l3, &top, stand_in_grid_bytes);
    assert_eq!(l3_terms.loop_scans_host, 0);
    assert_eq!(l3_terms.total().host_bytes, 866_375_476);
    assert_eq!(
        l3_terms.total().gpu_bytes,
        playing.total().gpu_bytes,
        "a Level III loop still holds its frames' textures",
    );
    let l3_fitted = fit(&l3, &wasm, &presumed, stand_in_grid_bytes);
    assert_eq!(l3_fitted.steps_back, 1);
    assert_eq!(l3_fitted.overlay_oversample_percent, 125);
    assert!(!l3_fitted.tile_whole_zoom);
    assert_eq!(
        over(&l3, &l3_fitted, &presumed, stand_in_grid_bytes),
        (false, false),
        "the Level III leg fits where the Level II leg cannot",
    );
    assert_eq!(
        need(&leg, &leg_fitted, stand_in_grid_bytes).host_bytes
            - presumed.host_allowance().unwrap(),
        272_984_548,
        "what the Level II leg is over by at every host rung",
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
