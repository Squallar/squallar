use super::*;
use crate::fit::{fit, fit_holds, loop_ceiling, loop_pool_bytes, loop_room, need, need_terms};
use crate::quality::{DESKTOP_PLATFORM_CEILING, MOBILE_PLATFORM_CEILING, WASM_PLATFORM_CEILING};
use crate::scene::fixtures::{scene_table, shipped_profile, stand_in_grid_bytes};
use crate::scene::{Capacity, CapacitySource};

/// Every device class this workspace builds for, exactly once.
pub fn profiles() -> [DeviceProfile; 3] {
    BudgetLimits::SHIPPED.map(shipped_profile)
}

/// **The resolver reproduces every shipped constant, field for field.**
#[test]
fn the_resolver_reproduces_every_shipped_constant() {
    use crate::constants::*;
    use squallar_radar::types::{NATIVE_IMAGE_SIZE, WASM_IMAGE_SIZE};
    use squallar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

    let expected = [
        Budgets {
            name: "wasm32",
            promotion: Promotion::Floor,
            steps_back: 0,
            image_side_px: WASM_IMAGE_SIZE,
            long_range_image_side_px: WASM_LONG_RANGE_IMAGE_SIZE,
            loop_image_side_px: WASM_LOOP_IMAGE_SIZE,
            section_width_px: WASM_SECTION_WIDTH,
            concurrent_renders: WASM_MAX_CONCURRENT_RENDERS,
            concurrent_loop_downloads: NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
            loop_frames_held: WASM_MAX_LOOP_FRAMES,
            loop_span_secs: WASM_LOOP_SPAN_BUDGET_SECS,
            loop_render_budget: WASM_MAX_LOOP_RENDER_BUDGET,
            loop_pool_floor_bytes: WASM_LOOP_POOL_FLOOR_BYTES,
            loop_pool_ceiling_bytes: WASM_LOOP_POOL_CEILING_BYTES,
            grid_cells: WASM_VOLUME_GRID_CELLS,
            volume_texture_bytes: WASM_VOLUME_TEXTURE_BUDGET_BYTES,
            offscreen_bytes: WASM_VOLUME_OFFSCREEN_BUDGET_BYTES,
            mirror_bytes: WASM_VOLUME_MIRROR_BYTES_MAX,
            render_cache_entries: NON_MOBILE_MAX_RENDER_CACHE_ENTRIES,
            quality_ceiling: WASM_PLATFORM_CEILING,
            max_panes: MAX_PANES_DESKTOP,
            app_texture_ceiling_bytes: WASM_APP_TEXTURE_BUDGET_BYTES,
            raster_side_ceiling_px: WASM_RASTER_SIDE_CEILING,
            prism_vram_bytes: WASM_PRISM_GEOMETRY_BYTES,
            tile_whole_zoom: false,
            tile_styled_bytes: WASM_TILE_STYLED_BYTES[0],
            tile_parsed_bytes: WASM_TILE_PARSED_BYTES[0],
            tile_terrain_bytes: WASM_TILE_TERRAIN_BYTES[0],
            tile_host_ceiling_bytes: WASM_TILE_HOST_CEILING_BYTES[0],
        },
        Budgets {
            name: "mobile",
            promotion: Promotion::Floor,
            steps_back: 0,
            image_side_px: NATIVE_IMAGE_SIZE,
            long_range_image_side_px: MOBILE_LONG_RANGE_IMAGE_SIZE,
            loop_image_side_px: MOBILE_LOOP_IMAGE_SIZE,
            section_width_px: NATIVE_SECTION_WIDTH,
            concurrent_renders: MOBILE_MAX_CONCURRENT_RENDERS,
            concurrent_loop_downloads: MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
            loop_frames_held: MOBILE_MAX_LOOP_FRAMES,
            loop_span_secs: MOBILE_LOOP_SPAN_BUDGET_SECS,
            loop_render_budget: MOBILE_MAX_LOOP_RENDER_BUDGET,
            loop_pool_floor_bytes: MOBILE_LOOP_POOL_FLOOR_BYTES,
            loop_pool_ceiling_bytes: MOBILE_LOOP_POOL_CEILING_BYTES,
            grid_cells: MOBILE_VOLUME_GRID_CELLS,
            volume_texture_bytes: MOBILE_VOLUME_TEXTURE_BUDGET_BYTES,
            offscreen_bytes: MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES,
            mirror_bytes: MOBILE_VOLUME_MIRROR_BYTES_MAX,
            render_cache_entries: MOBILE_MAX_RENDER_CACHE_ENTRIES,
            quality_ceiling: MOBILE_PLATFORM_CEILING,
            max_panes: MAX_PANES_MOBILE,
            app_texture_ceiling_bytes: MOBILE_APP_TEXTURE_BUDGET_BYTES,
            raster_side_ceiling_px: MOBILE_RASTER_SIDE_CEILING,
            prism_vram_bytes: MOBILE_PRISM_GEOMETRY_BYTES,
            tile_whole_zoom: false,
            tile_styled_bytes: MOBILE_TILE_STYLED_BYTES,
            tile_parsed_bytes: MOBILE_TILE_PARSED_BYTES,
            tile_terrain_bytes: MOBILE_TILE_TERRAIN_BYTES,
            tile_host_ceiling_bytes: MOBILE_TILE_HOST_CEILING_BYTES,
        },
        Budgets {
            name: "desktop",
            promotion: Promotion::Floor,
            steps_back: 0,
            image_side_px: NATIVE_IMAGE_SIZE,
            long_range_image_side_px: DESKTOP_LONG_RANGE_IMAGE_SIZE,
            loop_image_side_px: DESKTOP_LOOP_IMAGE_SIZE,
            section_width_px: NATIVE_SECTION_WIDTH,
            concurrent_renders: DESKTOP_MAX_CONCURRENT_RENDERS,
            concurrent_loop_downloads: NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
            loop_frames_held: DESKTOP_MAX_LOOP_FRAMES,
            loop_span_secs: DESKTOP_LOOP_SPAN_BUDGET_SECS,
            loop_render_budget: DESKTOP_MAX_LOOP_RENDER_BUDGET,
            loop_pool_floor_bytes: DESKTOP_LOOP_POOL_FLOOR_BYTES,
            loop_pool_ceiling_bytes: DESKTOP_LOOP_POOL_CEILING_BYTES,
            grid_cells: DESKTOP_VOLUME_GRID_CELLS,
            volume_texture_bytes: DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES,
            offscreen_bytes: DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
            mirror_bytes: DESKTOP_VOLUME_MIRROR_BYTES_MAX,
            render_cache_entries: NON_MOBILE_MAX_RENDER_CACHE_ENTRIES,
            quality_ceiling: DESKTOP_PLATFORM_CEILING,
            max_panes: MAX_PANES_DESKTOP,
            app_texture_ceiling_bytes: DESKTOP_APP_TEXTURE_BUDGET_BYTES,
            raster_side_ceiling_px: DESKTOP_RASTER_SIDE_CEILING,
            prism_vram_bytes: DESKTOP_PRISM_GEOMETRY_BYTES,
            // The tile-sharpness rung's position: a class rung never snaps.
            tile_whole_zoom: false,
            tile_styled_bytes: DESKTOP_TILE_STYLED_BYTES[0],
            tile_parsed_bytes: DESKTOP_TILE_PARSED_BYTES[0],
            tile_terrain_bytes: DESKTOP_TILE_TERRAIN_BYTES[0],
            tile_host_ceiling_bytes: DESKTOP_TILE_HOST_CEILING_BYTES[0],
        },
    ];

    for (profile, want) in profiles().into_iter().zip(expected) {
        let got = resolve(&profile);
        assert_eq!(
            got, want,
            "{}: the resolver does not reproduce this target's shipped \
             constants, so extracting it was not the behaviour-free change it \
             is documented to be",
            want.name,
        );
    }
}

/// The compiled target's own budgets are the `cfg`-selected constants.
#[test]
fn the_compiled_targets_budgets_are_the_constants_this_build_selected() {
    use crate::constants::*;

    let b = resolve(&DeviceProfile::for_target());
    assert_eq!(b.image_side_px, squallar_radar::types::IMAGE_SIZE);
    assert_eq!(b.long_range_image_side_px, LONG_RANGE_IMAGE_SIZE);
    assert_eq!(b.loop_image_side_px, LOOP_IMAGE_SIZE);
    assert_eq!(b.section_width_px, squallar_radar::xsect::SECTION_WIDTH);
    assert_eq!(b.concurrent_renders, MAX_CONCURRENT_RENDERS);
    assert_eq!(b.concurrent_loop_downloads, MAX_CONCURRENT_LOOP_DOWNLOADS);
    assert_eq!(b.loop_frames_held, MAX_LOOP_FRAMES);
    assert_eq!(b.loop_span_secs, LOOP_SPAN_BUDGET_SECS);
    assert_eq!(b.loop_render_budget, MAX_LOOP_RENDER_BUDGET);
    assert_eq!(b.loop_pool_floor_bytes, LOOP_POOL_FLOOR_BYTES);
    assert_eq!(b.loop_pool_ceiling_bytes, LOOP_POOL_CEILING_BYTES);
    assert_eq!(b.volume_loop_bytes(), VOLUME_LOOP_TEXTURE_BUDGET_BYTES);
    assert_eq!(b.grid_cells, VOLUME_GRID_CELLS);
    assert_eq!(b.volume_texture_bytes, VOLUME_TEXTURE_BUDGET_BYTES);
    assert_eq!(b.offscreen_bytes, VOLUME_OFFSCREEN_BUDGET_BYTES);
    assert_eq!(b.mirror_bytes, VOLUME_MIRROR_BYTES_MAX);
    assert_eq!(b.render_cache_entries, MAX_RENDER_CACHE_ENTRIES);
    assert_eq!(b.quality_ceiling, crate::quality::PLATFORM_CEILING);
    assert_eq!(b.app_texture_ceiling_bytes, APP_TEXTURE_BUDGET_BYTES);
    assert_eq!(b.prism_vram_bytes, PRISM_GEOMETRY_BYTES);
    assert_eq!(
        b.grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        VOLUME_GRID_FLOOR_SHAPE,
    );
    assert!(
        !b.tile_whole_zoom,
        "a class rung snaps no tiles: only the ladder's tile-sharpness rung does",
    );
    // The tile allowances have no `cfg` cascade to compare against — that is
    // the point of them — so the target's own bracket at the floor is the
    // reference: a fresh profile has met no adapter and earns nothing.
    let limits = BudgetLimits::for_target();
    assert_eq!(b.tile_styled_bytes, limits.tile_styled_bytes.floor);
    assert_eq!(b.tile_parsed_bytes, limits.tile_parsed_bytes.floor);
    assert_eq!(b.tile_terrain_bytes, limits.tile_terrain_bytes.floor);
    assert_eq!(
        b.tile_cache(),
        TileCacheBudget {
            styled_bytes: limits.tile_styled_bytes.floor as u64,
            parsed_bytes: limits.tile_parsed_bytes.floor as u64,
            terrain_bytes: limits.tile_terrain_bytes.floor as u64,
            whole_zoom: false,
        }
    );
}

/// **The sharpness rung rides with the tile allowances, both ways.** The
/// `TileCacheBudget` a frame hands the tile caches says whether the ladder has
/// taken the tile rung — `true` from the step that takes it, `false` on a
/// fresh resolve — because it is the scene-level input to each source's snap
/// decision (`squallar_egui::tile_source::snap`), and a flag left `false` on a
/// stepped budget would leave that input dead while every byte figure arrived.
/// The rung moves sharpness and not bytes: the three allowances are the
/// figures they were.
#[test]
fn the_tile_allowances_carry_the_sharpness_rung() {
    for limits in BudgetLimits::SHIPPED {
        let fresh = resolve(&shipped_profile(limits));
        assert!(
            !fresh.tile_cache().whole_zoom,
            "{}: a fresh resolve is snapped",
            limits.name
        );

        let mut stepped = fresh;
        let mut steps = 0;
        while !stepped.tile_whole_zoom {
            assert!(
                step_down(&mut stepped, &limits),
                "{}: the ladder ended after {steps} steps without taking the tile rung",
                limits.name,
            );
            steps += 1;
            assert!(
                steps <= 16,
                "{}: the tile rung is out of reach",
                limits.name
            );
        }
        let handed = stepped.tile_cache();
        assert!(
            handed.whole_zoom,
            "{}: the rung was taken and the allowances did not say so",
            limits.name
        );
        assert_eq!(
            (
                handed.styled_bytes,
                handed.parsed_bytes,
                handed.terrain_bytes
            ),
            (
                fresh.tile_cache().styled_bytes,
                fresh.tile_cache().parsed_bytes,
                fresh.tile_cache().terrain_bytes,
            ),
            "{}: the sharpness rung moved a byte allowance",
            limits.name,
        );
    }
}

/// The limits an adapter might really report, as `(2D, 3D)` pairs.
const REPORTED_CEILINGS: [(&str, u32, u32); 6] = [
    ("webgl2-guarantee", 2048, 256),
    ("downlevel-defaults", 2048, 512),
    ("metal-ish", 16384, 2048),
    ("wgpu-default", 8192, 2048),
    ("firefox_3090", 16384, 16384),
    ("chrome_890m", 16384, 8192),
];

/// Every device class, so the arms a browser and a phone take are both swept.
const CLASSES: [DeviceClass; 5] = [
    DeviceClass::Discrete,
    DeviceClass::Integrated,
    DeviceClass::Virtual,
    DeviceClass::Software,
    DeviceClass::Unknown,
];

/// Every form factor a bridge can report, the unknown arm included.
const FORM_FACTORS: [Option<FormFactor>; 3] =
    [None, Some(FormFactor::Handheld), Some(FormFactor::Desktop)];

/// The cross product the plan asks for: bracket × class × reported ceilings ×
/// VRAM reading × memo × form factor, with the shipped rows named so a
/// regression on a real target says which one. The host signals ride the VRAM
/// axis: a row that measures VRAM also measures RAM (twice the VRAM), declares
/// RAM (the same, capped at the 8 GiB `deviceMemory` bucket) and reports a
/// thread count.
fn synthetic_profiles() -> Vec<DeviceProfile> {
    let mut out = Vec::new();
    for limits in BudgetLimits::SHIPPED {
        for class in CLASSES {
            for (_, two_d, three_d) in REPORTED_CEILINGS {
                for vram in [None, Some(2 << 30), Some(8 << 30), Some(24 << 30)] {
                    for memo in [
                        None,
                        Some(BudgetMemo {
                            loop_pool_bytes: Some(limits.loop_pool_bytes.floor),
                            steps_back: 0,
                        }),
                    ] {
                        for form_factor in FORM_FACTORS {
                            out.push(DeviceProfile {
                                class,
                                adapter: AdapterCeilings {
                                    max_texture_dimension_2d: two_d,
                                    max_texture_dimension_3d: three_d,
                                },
                                vram_bytes: vram,
                                system_ram_bytes: vram.map(|v| v * 2),
                                declared_ram_bytes: vram.map(|v| (v * 2).min(8 << 30)),
                                parallelism: Some(if limits.name == "wasm32" { 1 } else { 8 }),
                                form_factor,
                                memo,
                                ..shipped_profile(limits)
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// `profile` with every host signal unread, exactly as a bridge that answers
/// nothing hands it over. A native bridge cannot answer nothing about the form
/// factor — it supplies one as a build fact — so on a native row that field
/// stays as it is; on a web row it is a pointer-media classification that can
/// fail, and it is stripped with the rest.
fn without_host_signals(profile: &DeviceProfile) -> DeviceProfile {
    DeviceProfile {
        system_ram_bytes: None,
        declared_ram_bytes: None,
        parallelism: None,
        form_factor: match profile.platform {
            Platform::Native => profile.form_factor,
            Platform::Web => None,
        },
        ..*profile
    }
}

/// Every invariant a resolved budget must satisfy, whatever produced it — and
/// the fit property on every arm the profile can stand on: for every scene in
/// the table, the budgets `fit` answers satisfy the same invariants, the
/// scene's need fits the capacity's allowance (or every rung is at its stop,
/// where the runtime clamps and logs), a scene that already fits sheds
/// nothing, and no rung was shed that the scene did not need. The presumed arm
/// is checked on every row; the measured arm on every row whose readings
/// amount to a measurement ([`DeviceProfile::gpu_capacity_bytes`]).
fn check_invariants(profile: &DeviceProfile, from: &str) {
    check_budgets(&resolve(profile), profile, from);
    check_fit_against(profile, &Capacity::presumed(&profile.limits), from);
    let measured = profile.capacity();
    if measured.source == CapacitySource::Measured {
        check_fit_against(profile, &measured, &format!("{from} / measured"));
    }
}

/// The fit property against one capacity, on whichever arm it is: every answer
/// passes [`check_budgets`]; need is under the allowance or every rung is at
/// its stop; a scene that fits at the class rung is left there; every rung
/// taken was needed; the pool is what the loops need capped by the room, and
/// the room is under the allowance; and **no field ends above the class
/// rung** — capacity moves the pool and the room and sheds rungs, and raises
/// nothing: the fill-rate and wire-capped fields stay where the class put
/// them on both arms.
fn check_fit_against(profile: &DeviceProfile, cap: &Capacity, from: &str) {
    let limits = &profile.limits;
    let b = resolve(profile);
    let allowance = cap.allowance();
    for (scene_name, scene) in scene_table() {
        let fitted = fit(&scene, profile, cap, stand_in_grid_bytes);
        check_budgets(
            &fitted,
            profile,
            &format!("{from} / fitted to {scene_name}"),
        );
        let after = need(&scene, &fitted, stand_in_grid_bytes);
        assert!(
            fit_holds(&scene, &fitted, limits, cap, stand_in_grid_bytes),
            "{from} / {} / {scene_name}: {} MiB of need against a {} MiB allowance \
             with rungs left to shed",
            b.name,
            after.gpu_bytes / (1024 * 1024),
            allowance / (1024 * 1024),
        );
        let at_the_class_rung = need(&scene, &b, stand_in_grid_bytes);
        if at_the_class_rung.gpu_bytes <= allowance {
            assert_eq!(
                fitted, b,
                "{from} / {} / {scene_name}: a scene that fits at the class rung was \
                 shed a rung anyway",
                b.name,
            );
        }
        // Every rung taken was taken for a reason: one fewer and the scene
        // would not have fitted.
        let extra = fitted.steps_back - b.steps_back;
        if extra > 0 {
            let mut one_less = b;
            demote(&mut one_less, limits, extra - 1);
            assert!(
                need(&scene, &one_less, stand_in_grid_bytes).gpu_bytes > allowance,
                "{from} / {} / {scene_name}: fit took {extra} rungs where {} would \
                 have done",
                b.name,
                extra - 1,
            );
        }
        // The pool: the room the rest of the scene leaves, capped at what the
        // loops could ever fill — their whole lookback at their cadence, no
        // loop past the list cap — and never less than what they need; the
        // room never past the allowance. `fit` charged the need, so the pool
        // can always pay it; the difference is what the planner balloons into.
        let terms = need_terms(&scene, &fitted, stand_in_grid_bytes);
        let room = loop_room(&scene, &fitted, cap, stand_in_grid_bytes);
        assert!(
            room <= allowance,
            "{from} / {} / {scene_name}: {room} B of room under a {allowance} B allowance",
            b.name,
        );
        let pool = loop_pool_bytes(&scene, &fitted, cap, stand_in_grid_bytes);
        assert_eq!(
            pool,
            loop_ceiling(&scene, &fitted, stand_in_grid_bytes).min(room),
            "{from} / {} / {scene_name}: the pool is not min(ceiling, room)",
            b.name,
        );
        assert!(
            pool >= terms.loops.min(room),
            "{from} / {} / {scene_name}: the pool ({pool} B) cannot pay the loops' need \
             ({} B) inside the room ({room} B)",
            b.name,
            terms.loops,
        );
        // Nothing above the class rung, on either arm.
        assert_eq!(fitted.promotion, b.promotion, "{from} / {scene_name}");
        assert!(
            fitted.offscreen_bytes <= b.offscreen_bytes,
            "{from} / {} / {scene_name}: capacity raised the offscreen past the class rung",
            b.name,
        );
        assert_eq!(
            fitted.quality_ceiling.capped_by(b.quality_ceiling),
            fitted.quality_ceiling,
            "{from} / {} / {scene_name}: capacity raised the 3D quality past the class rung",
            b.name,
        );
        assert!(fitted.loop_render_budget <= b.loop_render_budget);
        assert!(fitted.raster_side_ceiling_px <= b.raster_side_ceiling_px);
        assert!(fitted.app_texture_ceiling_bytes <= b.app_texture_ceiling_bytes);
        assert!(fitted.volume_texture_bytes <= b.volume_texture_bytes);
        for axis in 0..3 {
            assert!(
                fitted.grid_cells[axis] <= b.grid_cells[axis],
                "{from} / {} / {scene_name}: capacity raised grid axis {axis} past the \
                 class rung",
                b.name,
            );
        }
    }
}

/// The bracket invariants of one set of budgets, however it was produced.
fn check_budgets(b: &Budgets, profile: &DeviceProfile, from: &str) {
    let limits = &profile.limits;

    // Inside the bracket, both ends, on every field.
    let within = |name: &str, value: usize, bracket: Bracket| {
        assert!(
            value >= bracket.floor && value <= bracket.ceiling.max(bracket.floor),
            "{from} / {}: {name} resolved to {value}, outside [{}, {}]",
            b.name,
            bracket.floor,
            bracket.ceiling,
        );
    };
    within("image_side_px", b.image_side_px, limits.image_side_px);
    within(
        "long_range_image_side_px",
        b.long_range_image_side_px,
        limits.long_range_image_side_px,
    );
    within(
        "loop_image_side_px",
        b.loop_image_side_px,
        limits.loop_image_side_px,
    );
    within(
        "section_width_px",
        b.section_width_px,
        limits.section_width_px,
    );
    within(
        "concurrent_renders",
        b.concurrent_renders,
        limits.concurrent_renders,
    );
    within(
        "concurrent_loop_downloads",
        b.concurrent_loop_downloads,
        limits.concurrent_loop_downloads,
    );
    within(
        "loop_frames_held",
        b.loop_frames_held,
        limits.loop_frames_held,
    );
    within("loop_span_secs", b.loop_span_secs, limits.loop_span_secs);
    // The loop-history rung halves this one *down* past the bracket's floor
    // toward the two-frame minimum, so only the top is held to the bracket.
    assert!(
        b.loop_render_budget >= crate::constants::MIN_LOOP_FRAMES_PER_PANE
            && b.loop_render_budget <= limits.loop_render_budget.ceiling,
        "{from} / {}: loop_render_budget resolved to {}, outside [{}, {}]",
        b.name,
        b.loop_render_budget,
        crate::constants::MIN_LOOP_FRAMES_PER_PANE,
        limits.loop_render_budget.ceiling,
    );
    // The ladder is ordered: tiles snap only once the loop history is at its
    // floor, and the grid and raster move only once the tiles have snapped.
    if b.tile_whole_zoom {
        assert_eq!(
            b.loop_render_budget,
            crate::constants::MIN_LOOP_FRAMES_PER_PANE,
            "{from} / {}: tiles snapped while the loop history was still above \
             its floor — the tile rung ran before the loop rung",
            b.name,
        );
    }
    if b.grid_cells != limits.grid_cells.at(b.promotion)
        || b.raster_side_ceiling_px < limits.raster_side_ceiling_px.at(b.promotion)
    {
        assert!(
            b.tile_whole_zoom,
            "{from} / {}: the grid or the raster moved before the tiles snapped — \
             a detail rung ran before the sharpness rung",
            b.name,
        );
    }
    within(
        "loop_pool_floor_bytes",
        b.loop_pool_floor_bytes,
        limits.loop_pool_bytes,
    );
    within(
        "loop_pool_ceiling_bytes",
        b.loop_pool_ceiling_bytes,
        limits.loop_pool_bytes,
    );
    within(
        "volume_texture_bytes",
        b.volume_texture_bytes,
        limits.volume_texture_bytes,
    );
    within("offscreen_bytes", b.offscreen_bytes, limits.offscreen_bytes);
    within("mirror_bytes", b.mirror_bytes, limits.mirror_bytes);
    within(
        "render_cache_entries",
        b.render_cache_entries,
        limits.render_cache_entries,
    );
    within("max_panes", b.max_panes, limits.max_panes);
    within(
        "app_texture_ceiling_bytes",
        b.app_texture_ceiling_bytes,
        limits.app_texture_ceiling_bytes,
    );
    within(
        "prism_vram_bytes",
        b.prism_vram_bytes,
        limits.prism_geometry_bytes,
    );
    within(
        "tile_styled_bytes",
        b.tile_styled_bytes,
        limits.tile_styled_bytes,
    );
    within(
        "tile_parsed_bytes",
        b.tile_parsed_bytes,
        limits.tile_parsed_bytes,
    );
    within(
        "tile_terrain_bytes",
        b.tile_terrain_bytes,
        limits.tile_terrain_bytes,
    );
    within(
        "tile_host_ceiling_bytes",
        b.tile_host_ceiling_bytes,
        limits.tile_host_ceiling_bytes,
    );
    // The tile caches' own sum proof, on the host: the three allowances fit
    // their ceiling, and the ceiling is snug enough to catch one of them
    // silently doubling — the same 1.25x the GPU sum is held to.
    let tiles = b.tile_host_bytes();
    assert!(
        tiles <= b.tile_host_ceiling_bytes,
        "{from} / {}: {} MiB of tile allowances against a {} MiB host ceiling",
        b.name,
        tiles / (1024 * 1024),
        b.tile_host_ceiling_bytes / (1024 * 1024),
    );
    assert!(
        b.tile_host_ceiling_bytes * 4 <= tiles * 5,
        "{from} / {}: the {} MiB tile host ceiling is more than 1.25x the {} MiB it bounds",
        b.name,
        b.tile_host_ceiling_bytes / (1024 * 1024),
        tiles / (1024 * 1024),
    );
    assert_eq!(
        b.tile_cache(),
        TileCacheBudget {
            styled_bytes: b.tile_styled_bytes as u64,
            parsed_bytes: b.tile_parsed_bytes as u64,
            terrain_bytes: b.tile_terrain_bytes as u64,
            whole_zoom: b.tile_whole_zoom,
        },
        "{from} / {}: the tile caches are handed different figures from the ones resolved",
        b.name,
    );
    for axis in 0..3 {
        assert!(
            b.grid_cells[axis] >= limits.grid_cells.floor[axis]
                && b.grid_cells[axis] <= limits.grid_cells.ceiling[axis],
            "{from} / {}: grid axis {axis} resolved to {}, outside the bracket",
            b.name,
            b.grid_cells[axis],
        );
    }
    assert_eq!(
        b.quality_ceiling.capped_by(limits.quality_ceiling.ceiling),
        b.quality_ceiling,
        "{from} / {}: the resolved quality ceiling is above the bracket's",
        b.name,
    );

    // The sum proof, on every row rather than on the one this build compiled.
    let total = b.app_texture_bytes();
    assert!(
        total <= b.app_texture_ceiling_bytes,
        "{from} / {}: {} MiB of textures against a {} MiB ceiling",
        b.name,
        total / (1024 * 1024),
        b.app_texture_ceiling_bytes / (1024 * 1024),
    );
    // Snugness: a ceiling several times the real figure passes the line above
    // while admitting a silent doubling of any term.
    assert!(
        b.app_texture_ceiling_bytes * 4 <= total * 5,
        "{from} / {}: the {} MiB ceiling is more than 1.25x the {} MiB it bounds",
        b.name,
        b.app_texture_ceiling_bytes / (1024 * 1024),
        total / (1024 * 1024),
    );

    // The grid's byte budget is the raymarch's arithmetic
    // (`resident_grid_bytes`), so those proofs live in squallar-volumetric's
    // `raymarch::tests::budget_agreement`.

    // The raster ceiling is never a regression: a plan view may still reach the
    // size this build drew before any device was asked.
    assert!(
        b.raster_side_ceiling_px >= b.long_range_image_side_px,
        "{from} / {}: a {} px raster ceiling is below the {} px this build \
         already draws",
        b.name,
        b.raster_side_ceiling_px,
        b.long_range_image_side_px,
    );
    // And never above its own bracket. `demote` walks this one *down* past the
    // floor to `long_range_image_side_px.floor`, so only the top is checked.
    assert!(
        b.raster_side_ceiling_px <= limits.raster_side_ceiling_px.ceiling,
        "{from} / {}: a {} px raster ceiling is over the {} px the bracket \
         allows — a ceiling above the largest texture the class can hold is \
         every upload failing",
        b.name,
        b.raster_side_ceiling_px,
        limits.raster_side_ceiling_px.ceiling,
    );

    // A device is never asked for an axis it did not say it could hold, and
    // never for more cells than the budget every allocation was sized against.
    let max_axis = profile.adapter.max_texture_dimension_3d;
    let shape = b.grid_shape(max_axis);
    let budget_cells = b.grid_cells.iter().map(|&n| n as usize).product::<usize>();
    for (axis, n) in [("nx", shape.nx), ("ny", shape.ny), ("nz", shape.nz)] {
        assert!(
            n >= 1 && n as u32 <= max_axis,
            "{from} / {}: {axis} is {n} on a {max_axis}-reporting device — a \
             validation error inside a callback, where there is no Result to \
             check",
            b.name,
        );
    }
    assert!(
        shape.cells() <= budget_cells,
        "{from} / {}: a {max_axis}-reporting device is asked for {} cells \
         against the {budget_cells} this bracket budgeted",
        b.name,
        shape.cells(),
    );

    assert!(b.loop_render_budget >= crate::constants::MIN_LOOP_FRAMES_PER_PANE);
    assert!(b.loop_render_budget <= b.loop_frames_held);
    // Both ends of the clamp: a radar so slow one volume outlasts the budget
    // still gets two frames, and one so fast cannot buy unsized frames.
    assert!(b.loop_span_secs > 0, "{from} / {}: a zero span", b.name);
    assert_eq!(
        b.frames_for_span(Some(u32::MAX)),
        crate::constants::MIN_LOOP_FRAMES_PER_PANE,
        "{from} / {}: a very slow radar degrades a loop below the floor",
        b.name,
    );
    assert_eq!(
        b.frames_for_span(Some(1)),
        b.loop_render_budget,
        "{from} / {}: a very fast radar buys more than the render budget",
        b.name,
    );
    assert_eq!(
        b.frames_for_span(None),
        b.loop_render_budget,
        "{from} / {}: a loop with no cadence yet does not get the full budget",
        b.name,
    );
    assert!(b.concurrent_renders > 0);
    assert!(b.render_cache_entries > 0);
    assert!(b.concurrent_loop_downloads > 0);
    assert!(
        b.max_panes * crate::constants::MIN_LOOP_FRAMES_PER_PANE * b.loop_frame_bytes()
            <= b.loop_pool_floor_bytes,
        "{from} / {}: a full screen of loops does not fit the floor",
        b.name,
    );
}

/// **The whole matrix, not one row of three.**
#[test]
fn every_synthetic_profile_satisfies_every_invariant() {
    let rows = synthetic_profiles();
    assert_eq!(
        rows.len(),
        2160,
        "the matrix changed shape: 3 brackets x 5 classes x 6 reported \
         ceilings x 4 VRAM readings x 2 memos x 3 form factors = 2160 rows",
    );
    let mut measured = 0usize;
    for profile in &rows {
        check_invariants(profile, "matrix");
        if profile.capacity().source == CapacitySource::Measured {
            measured += 1;
        }
    }
    assert_eq!(
        measured, 504,
        "the measured arm changed shape: on each of the two native brackets, the \
         Discrete and Integrated rows with a reading (2 classes x 6 ceilings x 3 \
         readings x 2 memos x 3 form factors = 216) plus the Unknown rows at the two \
         desktop-class ceilings with a reading (2 x 3 x 2 x 3 = 36), so 252 twice",
    );
}

/// **The host signals reach the profile; on the presumed arm they move
/// nothing, and on the measured arm they move the pool and its room and
/// nothing else.** Re-argued from the proof that the signals changed no budget
/// when they landed: they now do, on purpose. A measured VRAM — or, on a
/// unified-memory part, a measured RAM — is the capacity the scene is fitted
/// to (ruling 3: no static pinned VRAM ceilings where capacity is measured),
/// and what it buys is room, never a bigger picture (ruling 5: a desktop does
/// not use more memory for the same scene because it has more).
///
/// Three statements, on every row of the matrix:
///
/// * `resolve` reads none of VRAM, RAM or threads — byte-identical with all
///   three unread, on every arm: the class rung is the adapter's and the form
///   factor's. Form factor and declared RAM do name the rung, so stripping
///   them may move `promotion`; on the web bracket that is the only field
///   that moves, because the step is the ceiling there
///   ([`the_web_step_is_todays_ceiling_until_a_desktop_browser_tier_is_measured`]),
///   and it moves at most one rung, never across the floor — the floor is the
///   adapter report's alone. On a native row the form factor is a build fact
///   the strip leaves in place ([`without_host_signals`]), and no native
///   bridge declares memory, so the whole set is an identity there.
/// * On the **presumed** arm — a browser, a software or virtual adapter, a
///   discrete card whose reader answered nothing, an unclassed adapter below
///   the desktop-class line — `fit`, the pool and the room are byte-identical
///   with the readings stripped, for every scene in the table.
/// * On the **measured** arm the class rung is the same budgets; what differs
///   from the stripped twin is the capacity, so the room, so the pool, and so
///   the rungs `fit` sheds — and each fitted answer on either arm is that one
///   class rung walked its own `steps_back` down the ladder, never a raised
///   field. `check_invariants` holds every such answer to the bracket.
///
/// Non-vacuity on each arm: more than half of the presumed rows carry a
/// reading to strip, and more than half of the measured rows differ from
/// their stripped twin in pool or room for at least one scene.
#[test]
fn the_signals_move_nothing_on_the_presumed_arm_and_only_the_pool_and_room_where_measured() {
    let rows = synthetic_profiles();
    let mut rows_with_a_signal = 0usize;
    let mut presumed_rows = 0usize;
    let mut presumed_rows_with_a_reading = 0usize;
    let mut measured_rows = 0usize;
    let mut measured_rows_whose_pool_or_room_moved = 0usize;
    for profile in &rows {
        let resolved = resolve(profile);
        let row = format!(
            "{} / {:?} / {:?} / vram {:?}",
            profile.limits.name, profile.class, profile.form_factor, profile.vram_bytes,
        );

        // VRAM, RAM and threads: `resolve` spends none of them.
        let unread = DeviceProfile {
            vram_bytes: None,
            system_ram_bytes: None,
            parallelism: None,
            ..*profile
        };
        assert_eq!(
            resolved,
            resolve(&unread),
            "{row}: a reading moved the class rung. Capacity is `fit`'s input and \
             never `resolve`'s",
        );

        let stripped = without_host_signals(profile);
        if stripped != *profile {
            rows_with_a_signal += 1;
        }
        let bare = resolve(&stripped);
        match profile.platform {
            Platform::Native => assert_eq!(
                resolved, bare,
                "{row}: a declared memory moved a native budget, and no native \
                 bridge declares one",
            ),
            Platform::Web => {
                assert_eq!(
                    Budgets {
                        promotion: bare.promotion,
                        ..resolved
                    },
                    bare,
                    "{row}: the form factor or the declared memory moved a web \
                     budget's value. They may move the rung's name, and the web \
                     step is the web ceiling, so the name is all that may move",
                );
                assert_eq!(
                    resolved.promotion == Promotion::Floor,
                    bare.promotion == Promotion::Floor,
                    "{row}: stripping the form factor moved a browser across \
                     the floor ({:?} to {:?}); the floor is the adapter \
                     report's alone",
                    resolved.promotion,
                    bare.promotion,
                );
            }
        }

        // The two arms. Stripping every reading always lands on the presumed
        // one, so `unread` is the presumed twin of whichever arm `profile` is on.
        let cap = profile.capacity();
        let bare = unread.capacity();
        assert_eq!(
            bare.source,
            CapacitySource::Presumed,
            "{row}: a profile with nothing read has a measured capacity",
        );
        let g = stand_in_grid_bytes;
        match cap.source {
            CapacitySource::Presumed => {
                presumed_rows += 1;
                if profile.vram_bytes.is_some() || profile.system_ram_bytes.is_some() {
                    presumed_rows_with_a_reading += 1;
                }
                assert_eq!(cap, bare, "{row}: a presumed capacity depends on a reading");
                for (name, scene) in scene_table() {
                    assert_eq!(
                        fit(&scene, profile, &cap, g),
                        fit(&scene, &unread, &bare, g),
                        "{row} / {name}: a reading moved a budget on the presumed arm, \
                         where nothing it carries is a measurement",
                    );
                    let with = fit(&scene, profile, &cap, g);
                    assert_eq!(
                        loop_pool_bytes(&scene, &with, &cap, g),
                        loop_pool_bytes(&scene, &with, &bare, g),
                        "{row} / {name}: a reading moved the pool on the presumed arm",
                    );
                    assert_eq!(
                        loop_room(&scene, &with, &cap, g),
                        loop_room(&scene, &with, &bare, g),
                        "{row} / {name}: a reading moved the room on the presumed arm",
                    );
                }
            }
            CapacitySource::Measured => {
                measured_rows += 1;
                assert_eq!(profile.platform, Platform::Native, "{row}");
                assert!(
                    !matches!(profile.class, DeviceClass::Software | DeviceClass::Virtual),
                    "{row}: a rasteriser's reading was believed",
                );
                let mut moved = false;
                for (name, scene) in scene_table() {
                    let with = fit(&scene, profile, &cap, g);
                    let without = fit(&scene, &unread, &bare, g);
                    // Every difference between the arms is a ladder position
                    // off the one class rung.
                    for (arm, fitted) in [("measured", with), ("presumed", without)] {
                        let mut walked = resolved;
                        demote(
                            &mut walked,
                            &profile.limits,
                            fitted.steps_back - resolved.steps_back,
                        );
                        assert_eq!(
                            Budgets {
                                steps_back: fitted.steps_back,
                                ..walked
                            },
                            fitted,
                            "{row} / {name} / {arm}: a fitted budget is not the class rung \
                             walked {} rungs down the ladder — capacity raised a field",
                            fitted.steps_back,
                        );
                    }
                    moved |= loop_pool_bytes(&scene, &with, &cap, g)
                        != loop_pool_bytes(&scene, &without, &bare, g)
                        || loop_room(&scene, &with, &cap, g)
                            != loop_room(&scene, &without, &bare, g);
                }
                if moved {
                    measured_rows_whose_pool_or_room_moved += 1;
                }
            }
            CapacitySource::Probed => {
                unreachable!("{row}: no profile produces a probed capacity")
            }
        }
    }
    assert!(
        rows_with_a_signal * 2 > rows.len(),
        "only {rows_with_a_signal} of {} rows carried a signal to strip, so \
         the identity above was mostly a row compared with itself",
        rows.len(),
    );
    assert_eq!(presumed_rows + measured_rows, rows.len());
    assert!(
        presumed_rows_with_a_reading * 2 > presumed_rows,
        "only {presumed_rows_with_a_reading} of {presumed_rows} presumed rows carried \
         a reading to strip, so the presumed-arm identity was mostly a row compared \
         with itself",
    );
    assert!(
        measured_rows_whose_pool_or_room_moved * 2 > measured_rows,
        "only {measured_rows_whose_pool_or_room_moved} of {measured_rows} measured rows \
         moved the pool or the room against their presumed twin, so the measured arm \
         was mostly not exercised",
    );
}

/// The shipped rows by name, so a regression on a real target says which one.
#[test]
fn every_shipped_profile_satisfies_every_invariant() {
    for profile in profiles() {
        check_invariants(&profile, "shipped");
    }
}

/// **Two browsers, one machine.**
#[test]
fn two_browser_profiles_on_one_machine_stay_inside_one_bracket() {
    let web = |two_d: u32, three_d: u32| DeviceProfile {
        adapter: AdapterCeilings {
            max_texture_dimension_2d: two_d,
            max_texture_dimension_3d: three_d,
        },
        ..shipped_profile(BudgetLimits::WASM)
    };
    // The Firefox row is entered at its *allocatable* 16384 rather than its
    // reported 32768.
    let firefox = web(16384, 16384);
    let chromium = web(16384, 8192);

    for profile in [&firefox, &chromium] {
        check_invariants(profile, "browser pair");
        assert_eq!(
            resolve(profile).name,
            BudgetLimits::WASM.name,
            "a browser resolved outside the web bracket",
        );
    }

    // Same bracket, so same budgets: nothing branches on browser identity.
    assert_eq!(
        resolve(&firefox),
        resolve(&chromium),
        "two browsers on one bracket resolved to different budgets, which can \
         only mean something is reading the report where it should be reading \
         the bracket",
    );

    // The same two browsers with the form factor the machine reports — one
    // mouse, read through the same pointer media by both. Nothing branches on
    // browser identity at this rung either.
    let shaped = |two_d, three_d| DeviceProfile {
        form_factor: Some(FormFactor::Desktop),
        ..web(two_d, three_d)
    };
    let (firefox_shaped, chromium_shaped) = (shaped(16384, 16384), shaped(16384, 8192));
    for profile in [&firefox_shaped, &chromium_shaped] {
        check_invariants(profile, "browser pair, desktop form factor");
    }
    assert_eq!(
        resolve(&firefox_shaped).promotion,
        Promotion::Ceiling,
        "the pair with a mouse is here to exercise the ceiling arm",
    );
    assert_eq!(
        resolve(&firefox_shaped),
        resolve(&chromium_shaped),
        "two browsers on one machine with one mouse resolved to different \
         budgets",
    );
    let b = resolve(&firefox);
    assert!(
        b.grid_shape(16384).cells() <= b.grid_cells.iter().map(|&n| n as usize).product(),
        "a browser reporting 16384 is asked for more cells than the bracket \
         budgeted",
    );
}

/// A deterministic 64-bit generator, so a failing sweep reproduces exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[(self.next() % from.len() as u64) as usize]
    }

    fn in_range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

/// **A profile nobody thought of is still covered.**
#[test]
fn a_random_sweep_of_profiles_satisfies_every_invariant() {
    let mut rng = Rng(0x5EED_B0D6_1234_5678);
    for row in 0..4096u32 {
        let limits = rng.pick(&BudgetLimits::SHIPPED);
        // Not held to powers of two, and allowed under the WebGL2 guarantee:
        // `shape_for_budget` is what has to cope.
        let three_d = rng.in_range(1, 32768) as u32;
        let two_d = rng.in_range(1, 32768) as u32;
        let profile = DeviceProfile {
            class: rng.pick(&CLASSES),
            adapter: AdapterCeilings {
                max_texture_dimension_2d: two_d,
                max_texture_dimension_3d: three_d,
            },
            vram_bytes: match rng.next() % 3 {
                0 => None,
                _ => Some(rng.in_range(1 << 28, 64u64 << 30)),
            },
            system_ram_bytes: match rng.next() % 3 {
                0 => None,
                _ => Some(rng.in_range(1 << 30, 256u64 << 30)),
            },
            // The `deviceMemory` buckets, and the unknown arm every browser
            // but Chromium's takes.
            declared_ram_bytes: match rng.next() % 3 {
                0 => None,
                _ => Some(rng.pick(&[1u64 << 28, 1 << 29, 1 << 30, 2 << 30, 4 << 30, 8 << 30])),
            },
            parallelism: match rng.next() % 3 {
                0 => None,
                _ => Some(rng.in_range(1, 256) as usize),
            },
            form_factor: rng.pick(&FORM_FACTORS),
            memo: match rng.next() % 2 {
                0 => None,
                _ => Some(BudgetMemo {
                    loop_pool_bytes: Some(rng.in_range(0, 8u64 << 30) as usize),
                    // Counts far past what a session can reach: `demote` has to
                    // be total, and a memo from a later build is untrusted data.
                    steps_back: rng.in_range(0, 12) as u32,
                }),
            },
            ..shipped_profile(limits)
        };
        check_invariants(&profile, &format!("random row {row}"));
    }
}

/// The measured inter-volume gaps, in seconds, and what each radar is.
const MEASURED_CADENCES: [(&str, u32); 4] = [
    ("TDWR VCP 80", 360),
    ("TDWR VCP 90", 360),
    ("WSR-88D precip (VCP 212/215)", 259),
    ("WSR-88D clear air (VCP 35)", 517),
];

/// **The same budget is the same wall clock on every radar.**
#[test]
fn a_loop_spans_its_budget_on_every_measured_radar() {
    for profile in profiles() {
        let b = resolve(&profile);
        for (radar, cadence) in MEASURED_CADENCES {
            let frames = b.frames_for_span(Some(cadence));
            assert!(
                frames >= crate::constants::MIN_LOOP_FRAMES_PER_PANE,
                "{} / {radar}: {frames} frames is not a loop",
                b.name,
            );
            if frames == b.loop_render_budget {
                // The arm ran out of frames before budget: the ceiling working.
                continue;
            }
            let covered = (frames - 1) * cadence as usize;
            assert!(
                covered <= b.loop_span_secs,
                "{} / {radar}: {frames} frames span {covered} s, over the {} s \
                 budget — the cap is not a cap",
                b.name,
                b.loop_span_secs,
            );
            assert!(
                covered + cadence as usize > b.loop_span_secs,
                "{} / {radar}: {frames} frames span {covered} s of a {} s \
                 budget with room for another whole volume, so the loop is \
                 shorter than it was paid for",
                b.name,
                b.loop_span_secs,
            );
        }
    }
}

/// **A radar that changes VCP mid-window moves the loop, and moves it by the
/// majority cadence.**
#[test]
fn a_vcp_change_moves_the_frame_count_between_the_two_cadences() {
    for profile in profiles() {
        let b = resolve(&profile);
        let precip = b.frames_for_span(Some(259));
        let clear = b.frames_for_span(Some(517));
        assert!(
            clear <= precip,
            "{}: clear air has longer volumes, so it cannot want more frames",
            b.name,
        );
        // A straddling window takes whichever VCP ran for most of it.
        for straddling in [259, 300, 400, 500, 517] {
            let frames = b.frames_for_span(Some(straddling));
            assert!(
                (clear..=precip).contains(&frames),
                "{}: a {straddling} s median resolves {frames} frames, outside \
                 the {clear}..={precip} the two VCPs bracket",
                b.name,
            );
        }
    }
}

/// **A desktop browser and a phone browser stop getting the same answer.**
#[test]
fn a_desktop_class_browser_is_promoted_and_a_spec_floor_browser_is_not() {
    let web = |two_d: u32, three_d: u32| DeviceProfile {
        adapter: AdapterCeilings {
            max_texture_dimension_2d: two_d,
            max_texture_dimension_3d: three_d,
        },
        ..shipped_profile(BudgetLimits::WASM)
    };
    let at_the_guarantee = web(
        AdapterCeilings::WEBGL2_GUARANTEE.max_texture_dimension_2d,
        AdapterCeilings::WEBGL2_GUARANTEE.max_texture_dimension_3d,
    );
    let desktop_class = DeviceProfile {
        // The ceiling asks for the form factor too: this is the machine with a
        // mouse.
        form_factor: Some(FormFactor::Desktop),
        ..web(
            DESKTOP_CLASS_REPORT.max_texture_dimension_2d,
            DESKTOP_CLASS_REPORT.max_texture_dimension_3d,
        )
    };

    let floor = resolve(&at_the_guarantee);
    let promoted = resolve(&desktop_class);
    assert_eq!(floor.promotion, Promotion::Floor);
    assert_eq!(promoted.promotion, Promotion::Ceiling);
    // The same report with the shape unclassified is the step, and the step is
    // these same numbers: the rung's name is the only thing the form factor
    // moved.
    let unshaped = resolve(&DeviceProfile {
        form_factor: None,
        ..desktop_class
    });
    assert_eq!(
        unshaped,
        Budgets {
            promotion: Promotion::Step,
            ..promoted
        },
        "a desktop-class browser whose shape nobody classified resolved \
         something other than the ceiling's numbers under the step's name",
    );
    assert_eq!(
        floor,
        resolve(&shipped_profile(BudgetLimits::WASM)),
        "a browser at the guarantee got something other than the shipped \
         wasm32 configuration",
    );

    let floor_cells: usize = floor.grid_cells.iter().map(|&n| n as usize).product();
    let promoted_cells: usize = promoted.grid_cells.iter().map(|&n| n as usize).product();
    assert!(
        promoted_cells > floor_cells,
        "a browser reporting a measured desktop machine's figures was handed \
         the same {floor_cells} cells as one reporting the spec floor — which \
         is the complaint this stage exists to answer",
    );
    assert_eq!(
        promoted.grid_cells,
        crate::constants::MOBILE_VOLUME_GRID_CELLS,
        "the web ceiling is the mobile tier, which is what bounds the cost of \
         getting this wrong: a handheld browser that reports desktop-class \
         figures is handed a budget handheld hardware already runs",
    );
    // The pool half lives in squallar-app's `loop_pool::tests::budget_agreement`.
    for profile in [&at_the_guarantee, &desktop_class] {
        check_invariants(profile, "browser separation");
    }
}

/// **A software rasteriser is never promoted, whatever it reports.**
#[test]
fn a_software_rasteriser_is_not_promoted_by_what_it_reports() {
    let profile = DeviceProfile {
        class: DeviceClass::Software,
        adapter: AdapterCeilings {
            max_texture_dimension_2d: 32768,
            max_texture_dimension_3d: 16384,
        },
        ..shipped_profile(BudgetLimits::DESKTOP)
    };
    assert_eq!(resolve(&profile).promotion, Promotion::Floor);
    assert_eq!(
        resolve(&profile),
        resolve(&shipped_profile(BudgetLimits::DESKTOP)),
        "a software rasteriser was handed a discrete GPU's budgets",
    );
}

/// **A reading is a measurement only where the platform and the class make it
/// one** — [`DeviceProfile::gpu_capacity_bytes`], cell by cell. Every
/// platform, every class, VRAM read or not, RAM read or not, an adapter at the
/// WebGL2 guarantee and one at the desktop-class line: 2 x 5 x 2 x 2 x 2 = 80
/// cells, every one carrying a 32 GiB `deviceMemory` declaration so that a
/// declaration counting as a measurement anywhere would show. The rows that
/// carry the argument are named first; the sweep after them holds the rule on
/// every cell and counts the 13 that measure.
#[test]
fn a_reading_is_a_measurement_only_where_the_platform_and_class_make_it_one() {
    const VRAM: u64 = 24 << 30;
    const RAM: u64 = 64 << 30;
    let cell = |platform, class, vram, ram, adapter| DeviceProfile {
        platform,
        class,
        adapter,
        vram_bytes: vram,
        system_ram_bytes: ram,
        declared_ram_bytes: Some(32 << 30),
        ..shipped_profile(if platform == Platform::Web {
            BudgetLimits::WASM
        } else {
            BudgetLimits::DESKTOP
        })
    };
    let guarantee = AdapterCeilings::WEBGL2_GUARANTEE;
    let desktop_class = DESKTOP_CLASS_REPORT;
    use DeviceClass::*;
    use Platform::*;

    // The lie-guard: a reading does not un-rasterise a rasteriser. This box's
    // llvmpipe lists 93.9 GiB of system RAM as device-local.
    let llvmpipe = cell(Native, Software, Some(VRAM), Some(RAM), desktop_class);
    assert_eq!(llvmpipe.gpu_capacity_bytes(), None);
    assert_eq!(
        llvmpipe.capacity(),
        Capacity::presumed(&BudgetLimits::DESKTOP),
        "a software rasteriser reading 24 GiB was believed",
    );
    assert_eq!(
        cell(Native, Virtual, Some(VRAM), Some(RAM), desktop_class).gpu_capacity_bytes(),
        None,
    );
    // Nothing a browser reports is a measurement: not a heap a WebGPU adapter
    // might one day list, not RAM, and never `deviceMemory`.
    let firefox = cell(Web, Unknown, Some(VRAM), Some(RAM), desktop_class);
    assert_eq!(firefox.gpu_capacity_bytes(), None);
    assert_eq!(firefox.capacity(), Capacity::presumed(&BudgetLimits::WASM));
    assert_eq!(
        cell(Web, Discrete, Some(VRAM), Some(RAM), desktop_class).gpu_capacity_bytes(),
        None,
        "a browser that names its class is still a browser",
    );
    // A native discrete card is its VRAM and nothing else: RAM is not VRAM
    // there, so a card whose reader answered nothing stays presumed.
    let rtx_3090 = cell(
        Native,
        Discrete,
        Some(24822 << 20),
        Some(RAM),
        desktop_class,
    );
    assert_eq!(rtx_3090.gpu_capacity_bytes(), Some(24822 << 20));
    assert_eq!(
        rtx_3090.capacity(),
        Capacity::measured(24822 << 20, Some(RAM)),
        "the host's RAM rides beside the GPU figure",
    );
    assert_eq!(
        cell(Native, Discrete, None, Some(RAM), desktop_class).gpu_capacity_bytes(),
        None,
        "a discrete card with no reading is not half the host's RAM",
    );
    // A native integrated part is unified memory: Metal's own figure where it
    // answered, else the host's RAM over the divisor.
    let radeon_890m = cell(Native, Integrated, None, Some(RAM), desktop_class);
    assert_eq!(radeon_890m.gpu_capacity_bytes(), Some(RAM / 2));
    assert_eq!(
        radeon_890m.gpu_capacity_bytes(),
        Some(RAM / crate::constants::UNIFIED_MEMORY_GPU_DIVISOR),
    );
    let m_series = cell(Native, Integrated, Some(48 << 30), Some(RAM), desktop_class);
    assert_eq!(
        m_series.gpu_capacity_bytes(),
        Some(48 << 30),
        "Metal's working set replaces the divisor wherever it answers",
    );
    assert_eq!(
        cell(Native, Integrated, None, None, guarantee).gpu_capacity_bytes(),
        None,
        "an integrated part on a host that would not say its RAM",
    );
    // An adapter the driver would not class: believed as unified memory only
    // at the desktop-class line — the 3090 over GL is `Other` to wgpu.
    let gl_3090 = cell(Native, Unknown, None, Some(RAM), desktop_class);
    assert_eq!(gl_3090.gpu_capacity_bytes(), Some(RAM / 2));
    assert_eq!(
        cell(Native, Unknown, Some(VRAM), Some(RAM), guarantee).gpu_capacity_bytes(),
        None,
        "an unclassed adapter below the desktop-class line, whatever it read",
    );

    // The sweep: the rule as stated, on every cell.
    let mut cells = 0usize;
    let mut measuring = 0usize;
    for platform in [Native, Web] {
        for class in CLASSES {
            for vram in [None, Some(VRAM)] {
                for ram in [None, Some(RAM)] {
                    for adapter in [guarantee, desktop_class] {
                        cells += 1;
                        let profile = cell(platform, class, vram, ram, adapter);
                        let unified = vram.or(ram.map(|r| r / 2));
                        let expected = match (platform, class) {
                            (Web, _) | (_, Software | Virtual) => None,
                            (Native, Discrete) => vram,
                            (Native, Integrated) => unified,
                            (Native, Unknown) if adapter == desktop_class => unified,
                            (Native, Unknown) => None,
                        };
                        let got = profile.gpu_capacity_bytes();
                        assert_eq!(
                            got, expected,
                            "{platform:?} / {class:?} / vram {vram:?} / ram {ram:?} / \
                             {adapter:?}",
                        );
                        match got {
                            Some(gpu_bytes) => {
                                measuring += 1;
                                assert_eq!(
                                    profile.capacity(),
                                    Capacity::measured(gpu_bytes, ram),
                                    "{platform:?} / {class:?}",
                                );
                            }
                            None => assert_eq!(
                                profile.capacity(),
                                Capacity::presumed(&profile.limits),
                                "{platform:?} / {class:?}",
                            ),
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cells, 80);
    assert_eq!(
        measuring, 13,
        "native Discrete with VRAM (2 RAM x 2 adapters = 4), native Integrated with \
         either reading (3 x 2 = 6), native Unknown at the desktop-class line with \
         either reading (3)",
    );
}

/// **The discrete desktop GPU stops eating the compromise it was still eating.**
#[test]
fn a_discrete_desktop_gpu_can_afford_a_4k_pane_at_native_resolution() {
    use crate::quality::{GroundPass, ResolutionRung, VolumeQuality};

    const FOUR_K: [u32; 2] = [3840, 2160];
    let unpromoted = resolve(&shipped_profile(BudgetLimits::DESKTOP));
    let discrete = resolve(&DeviceProfile {
        class: DeviceClass::Discrete,
        ..shipped_profile(BudgetLimits::DESKTOP)
    });
    assert_eq!(discrete.promotion, Promotion::Ceiling);

    let before = VolumeQuality::BEST.fit(FOUR_K, unpromoted.offscreen_bytes, GroundPass::Off);
    let after = VolumeQuality::BEST.fit(FOUR_K, discrete.offscreen_bytes, GroundPass::Off);
    assert_eq!(
        before.quality.resolution,
        ResolutionRung::Half,
        "precondition: the shipped desktop budget steps a 4K pane down",
    );
    assert_eq!(
        after.quality.resolution,
        ResolutionRung::Native,
        "a discrete GPU still cannot render a 4K pane at native resolution, \
         which is the compromise this stage is for",
    );
    assert_eq!(after.size, FOUR_K);
    check_invariants(
        &DeviceProfile {
            class: DeviceClass::Discrete,
            ..shipped_profile(BudgetLimits::DESKTOP)
        },
        "discrete desktop",
    );

    // An integrated desktop GPU stays put even when it reports desktop-class
    // ceilings: the cost model puts it at 12-23 ms for a 1440 x 900 pane, so a
    // 4K one is not a frame. Measured on a Radeon 890M reporting 16384/8192,
    // which clears `DESKTOP_CLASS_REPORT` outright.
    for adapter in [
        AdapterCeilings::WEBGL2_GUARANTEE,
        AdapterCeilings {
            max_texture_dimension_2d: 16384,
            max_texture_dimension_3d: 8192,
        },
    ] {
        let integrated = resolve(&DeviceProfile {
            class: DeviceClass::Integrated,
            adapter,
            ..shipped_profile(BudgetLimits::DESKTOP)
        });
        assert_eq!(integrated.promotion, Promotion::Step);
        assert_eq!(
            integrated.offscreen_bytes, unpromoted.offscreen_bytes,
            "an integrated GPU reporting {adapter:?} was promoted to a 4K \
             offscreen the cost model says it cannot fill in a frame",
        );
    }
}

/// **Every arm of the mobile bracket is pinned, and it is pinned on purpose.**
#[test]
fn the_mobile_bracket_promotes_nothing_until_somebody_measures_aarch64() {
    let pinned = |b: Bracket| b.floor == b.step && b.step == b.ceiling;
    let limits = BudgetLimits::MOBILE;
    assert!(pinned(limits.offscreen_bytes));
    assert!(pinned(limits.volume_texture_bytes));
    assert!(pinned(limits.app_texture_ceiling_bytes));
    assert!(pinned(limits.raster_side_ceiling_px));
    assert!(pinned(limits.prism_geometry_bytes));
    assert!(pinned(limits.tile_styled_bytes));
    assert!(pinned(limits.tile_parsed_bytes));
    assert!(pinned(limits.tile_terrain_bytes));
    assert!(pinned(limits.tile_host_ceiling_bytes));
    assert_eq!(limits.grid_cells.floor, limits.grid_cells.ceiling);
    for class in CLASSES {
        let profile = DeviceProfile {
            class,
            adapter: AdapterCeilings {
                max_texture_dimension_2d: 32768,
                max_texture_dimension_3d: 16384,
            },
            ..shipped_profile(BudgetLimits::MOBILE)
        };
        assert_eq!(
            resolve(&profile),
            Budgets {
                // Every field comes out the same, whatever rung was earned.
                promotion: profile.promotion(),
                ..resolve(&shipped_profile(BudgetLimits::MOBILE))
            },
            "a {class:?} adapter moved a mobile budget, on a target nobody has \
             run this on",
        );
    }
}

/// **The tile allowances are the written figures, in MiB, on every bracket
/// and at every rung.** The argument for each is on
/// `constants::WASM_TILE_STYLED_BYTES`; this holds the arithmetic it was
/// made from — styled / parsed / terrain at floor, step and ceiling — and the
/// two relations the argument rests on: the styled floor holds 2,400 typical
/// entries and not the 106-tile worst case, and desktop's parsed floor is
/// what a 1920x1200 restyle needs.
#[test]
fn the_tile_allowances_are_the_written_figures_on_every_bracket() {
    let mib = |n: usize| n * 1024 * 1024;
    let rungs = |b: Bracket| [b.floor, b.step, b.ceiling];
    for (limits, styled, parsed, terrain, ceiling) in [
        (
            BudgetLimits::WASM,
            [48, 64, 64],
            [48, 64, 64],
            [25, 32, 32],
            [128, 192, 192],
        ),
        (
            BudgetLimits::MOBILE,
            [48, 48, 48],
            [48, 48, 48],
            [25, 25, 25],
            [128, 128, 128],
        ),
        (
            BudgetLimits::DESKTOP,
            [160, 256, 512],
            [192, 256, 384],
            [64, 80, 128],
            [448, 640, 1024],
        ),
    ] {
        let name = limits.name;
        assert_eq!(
            rungs(limits.tile_styled_bytes),
            styled.map(mib),
            "{name} styled"
        );
        assert_eq!(
            rungs(limits.tile_parsed_bytes),
            parsed.map(mib),
            "{name} parsed"
        );
        assert_eq!(
            rungs(limits.tile_terrain_bytes),
            terrain.map(mib),
            "{name} terrain"
        );
        assert_eq!(
            rungs(limits.tile_host_ceiling_bytes),
            ceiling.map(mib),
            "{name} host ceiling"
        );
        // Ordered by rung on every axis: a step below its floor would resolve
        // as the floor and read as a promotion that did nothing.
        for bracket in [
            limits.tile_styled_bytes,
            limits.tile_parsed_bytes,
            limits.tile_terrain_bytes,
            limits.tile_host_ceiling_bytes,
        ] {
            assert!(
                bracket.floor <= bracket.step && bracket.step <= bracket.ceiling,
                "{name}: a tile bracket is out of order: {bracket:?}"
            );
        }
    }

    // The relations the figures were argued from, at the two measured entry
    // costs — the city-core tail (1,462,708 B, squallar-egui's
    // `MEASURED_STYLED_ENTRY_BYTES`, restated here because this crate sits
    // under that one) and the typical dense-city entry (~30 KB).
    const TAIL: usize = 1_462_708;
    const TYPICAL: usize = 30_000;
    const USER_WINDOW_WORST: usize = 106;
    let wasm_floor = BudgetLimits::WASM.tile_styled_bytes.floor;
    assert!(
        wasm_floor / TAIL < USER_WINDOW_WORST,
        "the wasm styled floor holds {} worst-case entries; the user's 2878x1651 window          wants {USER_WINDOW_WORST} between zooms. If this ever fits, the working-set floor          and the snapping rung are no longer what carries that window and the doc on          WASM_TILE_STYLED_BYTES is stale",
        wasm_floor / TAIL,
    );
    assert!(
        wasm_floor / TYPICAL >= 1_600,
        "the wasm styled floor holds {} typical entries, under the 1,600 its doc quotes",
        wasm_floor / TYPICAL,
    );
    // Desktop's floor holds the user's window at the tail; its step holds
    // 2560x1440 between zooms (144).
    let desktop_floor = BudgetLimits::DESKTOP.tile_styled_bytes.floor;
    assert!(
        desktop_floor / TAIL >= USER_WINDOW_WORST,
        "desktop's styled floor holds {} tail entries, under the user's {USER_WINDOW_WORST}",
        desktop_floor / TAIL,
    );
    let desktop_step = BudgetLimits::DESKTOP.tile_styled_bytes.step;
    assert!(
        desktop_step / TAIL >= 144,
        "desktop's styled step holds {} tail entries, under the 144 a 2560x1440 canvas keeps \
         between zooms",
        desktop_step / TAIL,
    );
    // Desktop's parsed floor restyles the common 1920x1200 canvas — 96 tiles
    // between zooms — from cache at the parsed tail (2.09 MB), to within the
    // rounding its doc states.
    const PARSED_TAIL: usize = 2_092_002;
    let desktop_parsed = BudgetLimits::DESKTOP.tile_parsed_bytes.floor;
    assert!(
        desktop_parsed / PARSED_TAIL >= 96,
        "desktop's parsed floor holds {} worst-case parses, under the 96 a 1920x1200          canvas keeps between zooms",
        desktop_parsed / PARSED_TAIL,
    );
    // The ceiling rung holds a 3840x2160 window between zooms — 299 tiles —
    // at the tail without the floor's help.
    let desktop_ceiling = BudgetLimits::DESKTOP.tile_styled_bytes.ceiling;
    assert!(
        desktop_ceiling / TAIL >= 299,
        "desktop's styled ceiling holds {} worst-case entries, under the 299 a 4K window          keeps between zooms",
        desktop_ceiling / TAIL,
    );
}

/// **Terrain rasters are GPU textures, and they are left out of
/// `app_texture_bytes` on purpose, by name.** This is the named omission: the
/// wasm GPU sum sits at 278 of its 288 MiB, so folding even the terrain
/// *floor* into it fails the snugness proof for a population the tile cache
/// already bounds in bytes. The day the sum has the room, this test is what
/// says so — and the omission should then be re-argued, not silently kept.
#[test]
fn the_terrain_rasters_are_omitted_from_the_gpu_sum_by_name() {
    for profile in profiles() {
        let b = resolve(&profile);
        // Two budgets differing only in their tile allowances price the GPU
        // sum identically: no tile figure is a term of it.
        let mut doubled = b;
        doubled.tile_styled_bytes *= 2;
        doubled.tile_parsed_bytes *= 2;
        doubled.tile_terrain_bytes *= 2;
        assert_eq!(
            doubled.app_texture_bytes(),
            b.app_texture_bytes(),
            "{}: a tile allowance moved the GPU sum",
            b.name,
        );
        assert_eq!(
            doubled.tile_host_bytes(),
            2 * b.tile_host_bytes(),
            "{}: the tile allowances are priced on the host sum and nowhere else",
            b.name,
        );
    }
    // And the omission is load-bearing on the arm it was made for.
    let wasm = resolve(&shipped_profile(BudgetLimits::WASM));
    assert!(
        wasm.app_texture_bytes() + wasm.tile_terrain_bytes > wasm.app_texture_ceiling_bytes,
        "wasm32: {} MiB of GPU textures plus the {} MiB terrain floor fits the {} MiB          ceiling after all, so the omission no longer needs to be one",
        wasm.app_texture_bytes() / (1024 * 1024),
        wasm.tile_terrain_bytes / (1024 * 1024),
        wasm.app_texture_ceiling_bytes / (1024 * 1024),
    );
}

/// Halvings from `frames` down to `MIN_LOOP_FRAMES_PER_PANE`, the way the
/// loop-history rung takes them: `max(n / 2, floor)` a step, until it stops
/// moving. 36 -> 18 -> 9 -> 4 -> 2 is four on the desktop bracket, 18 -> 9 -> 4
/// -> 2 three on mobile, 14 -> 7 -> 3 -> 2 three on wasm32.
fn halvings_to_the_floor(frames: usize) -> u32 {
    let floor = crate::constants::MIN_LOOP_FRAMES_PER_PANE;
    let mut n = frames;
    let mut steps = 0;
    while (n / 2).max(floor) < n {
        n = (n / 2).max(floor);
        steps += 1;
    }
    steps
}

/// Steps the resolution rung takes from `top` to its stop, the way the rung
/// takes them: one coarsening a step while there is a coarser rung or the
/// offscreen sits above its floor. Two from `Native` (the desktop class rung),
/// one from `Half` (mobile and wasm32).
fn resolution_steps_to_the_floor(top: &Budgets, limits: &BudgetLimits) -> u32 {
    let mut resolution = top.quality_ceiling.resolution;
    let mut offscreen = top.offscreen_bytes;
    let mut steps = 0;
    while resolution.next_coarser().is_some() || offscreen > limits.offscreen_bytes.floor {
        resolution = resolution.next_coarser().unwrap_or(resolution);
        offscreen = limits.offscreen_bytes.floor;
        steps += 1;
    }
    steps
}

/// **The ladder is ordered, and lighting goes first.**
///
/// Re-argued when the loop-history and tile-sharpness rungs were inserted
/// between resolution and grid (the design doc's §4.3 order: a shorter loop is
/// the least destructive thing in the application, and a softer basemap is a
/// softer picture where a coarser grid is a wrong-looking one). Steps 1 and 2
/// are what they were, and step 3 is the resolution rung's second coarsening
/// (Half to Quarter), as it always was. Step 4 is now the first halving of the
/// loop history, not the grid; the grid — pinned on this bracket, so it never
/// moves — is reached only after the history is at its two-frame floor and the
/// tiles have snapped, and the raster is last as before. `deep` still has the
/// grid at its floor and the raster at the long-range floor.
#[test]
fn the_ladder_surrenders_lighting_before_resolution_and_the_picture_last() {
    use crate::constants::{DESKTOP_MAX_LOOP_RENDER_BUDGET, MIN_LOOP_FRAMES_PER_PANE};
    use crate::quality::{GradientShading, ResolutionRung};

    let stepped = |steps: u32| {
        resolve(&DeviceProfile {
            class: DeviceClass::Discrete,
            memo: Some(BudgetMemo {
                loop_pool_bytes: None,
                steps_back: steps,
            }),
            ..shipped_profile(BudgetLimits::DESKTOP)
        })
    };
    let top = stepped(0);
    assert_eq!(top.quality_ceiling.shading, GradientShading::On);
    assert_eq!(top.quality_ceiling.resolution, ResolutionRung::Native);
    assert_eq!(top.loop_render_budget, DESKTOP_MAX_LOOP_RENDER_BUDGET);
    assert!(!top.tile_whole_zoom);

    let one = stepped(1);
    assert_eq!(one.quality_ceiling.shading, GradientShading::Off);
    assert_eq!(one.quality_ceiling.resolution, ResolutionRung::Native);
    assert_eq!(one.offscreen_bytes, top.offscreen_bytes);
    assert_eq!(one.grid_cells, top.grid_cells);
    assert_eq!(one.loop_render_budget, top.loop_render_budget);

    let two = stepped(2);
    assert_eq!(two.quality_ceiling.resolution, ResolutionRung::Half);
    assert_eq!(
        two.offscreen_bytes,
        BudgetLimits::DESKTOP.offscreen_bytes.floor
    );
    assert_eq!(two.grid_cells, top.grid_cells);
    assert_eq!(two.raster_side_ceiling_px, top.raster_side_ceiling_px);
    assert_eq!(two.loop_render_budget, top.loop_render_budget);
    assert!(!two.tile_whole_zoom);

    // Rung 2 again: the resolution rung is one coarsening a step, and the
    // desktop class starts at Native, so its second step is Half to Quarter.
    let three = stepped(3);
    assert_eq!(three.quality_ceiling.resolution, ResolutionRung::Quarter);
    assert_eq!(three.loop_render_budget, top.loop_render_budget);
    assert_eq!(three.grid_cells, top.grid_cells);
    assert!(!three.tile_whole_zoom);
    let shed_3d = 1 + resolution_steps_to_the_floor(&top, &BudgetLimits::DESKTOP);
    assert_eq!(shed_3d, 3, "lighting, then Native -> Half -> Quarter");

    // Rung 3: the loop's history, one halving a step, 2D before 3D.
    let four = stepped(shed_3d + 1);
    assert_eq!(
        four.loop_render_budget,
        DESKTOP_MAX_LOOP_RENDER_BUDGET / 2,
        "the fourth step is the first halving of the loop history — 36 to 18 \
         frames — and not the grid",
    );
    assert_eq!(four.grid_cells, top.grid_cells);
    assert_eq!(four.raster_side_ceiling_px, top.raster_side_ceiling_px);
    assert!(!four.tile_whole_zoom);

    let halvings = halvings_to_the_floor(DESKTOP_MAX_LOOP_RENDER_BUDGET);
    assert_eq!(halvings, 4, "36 -> 18 -> 9 -> 4 -> 2");
    let history_floor = stepped(shed_3d + halvings);
    assert_eq!(history_floor.loop_render_budget, MIN_LOOP_FRAMES_PER_PANE);
    assert!(
        !history_floor.tile_whole_zoom,
        "the tiles snapped before the loop history reached its floor",
    );
    assert_eq!(history_floor.grid_cells, top.grid_cells);

    // Rung 4: tile sharpness, after the history and before the grid.
    let snapped = stepped(shed_3d + halvings + 1);
    assert!(snapped.tile_whole_zoom);
    assert_eq!(snapped.grid_cells, top.grid_cells);
    assert_eq!(snapped.raster_side_ceiling_px, top.raster_side_ceiling_px);

    // Rungs 5 and 6, and past them: the grid at its floor, the picture last.
    let deep = stepped(shed_3d + halvings + 8);
    assert_eq!(deep.grid_cells, BudgetLimits::DESKTOP.grid_cells.floor);
    assert_eq!(
        deep.raster_side_ceiling_px,
        BudgetLimits::DESKTOP.long_range_image_side_px.floor,
    );
    assert_eq!(deep.loop_render_budget, MIN_LOOP_FRAMES_PER_PANE);
    assert!(deep.tile_whole_zoom);
}

/// **A machine that keeps failing lands on the configuration this build already
/// shipped it, and stops.**
///
/// Re-argued when the loop-history and tile-sharpness rungs were inserted
/// before the grid: the grid now reaches its floor, and the ladder its fixed
/// point, later than the 3 and 4 steps the four-rung ladder pinned — later by
/// exactly the halvings the bracket's render budget takes to reach the
/// two-frame floor plus the one tile step, with a rung that has nowhere to go
/// on a bracket costing no step at all. The count is derived from the
/// bracket's own constants below and named per bracket beside it, so a moved
/// constant is read here rather than inferred.
#[test]
fn no_number_of_back_offs_takes_a_machine_below_its_bracket_floor() {
    use crate::constants::MIN_LOOP_FRAMES_PER_PANE;
    use crate::quality::GradientShading;

    for limits in BudgetLimits::SHIPPED {
        let unreadable = resolve(&shipped_profile(limits));
        let discrete = |steps: u32| DeviceProfile {
            class: DeviceClass::Discrete,
            adapter: AdapterCeilings {
                max_texture_dimension_2d: 32768,
                max_texture_dimension_3d: 16384,
            },
            memo: Some(BudgetMemo {
                loop_pool_bytes: None,
                steps_back: steps,
            }),
            ..shipped_profile(limits)
        };

        // The ladder's length on this bracket, rung by rung.
        let top = resolve(&discrete(0));
        let shading = u32::from(top.quality_ceiling.shading == GradientShading::On);
        let resolution = resolution_steps_to_the_floor(&top, &limits);
        let halvings = halvings_to_the_floor(top.loop_render_budget);
        let tiles = 1;
        let grid = u32::from(top.grid_cells != limits.grid_cells.floor);
        let raster = u32::from(top.raster_side_ceiling_px > limits.long_range_image_side_px.floor);
        let grid_at_floor_from = shading + resolution + halvings + tiles + grid;
        let stop = grid_at_floor_from + raster;
        // Steps per rung — shading, resolution, history, tiles, grid, raster —
        // as the bracket's constants were read to give them, so a moved
        // constant fails on the rung that moved.
        let expected_rungs: [u32; 6] = match limits.name {
            // On, Native -> Half -> Quarter, 36 -> 2 in four, snap, a pinned
            // grid, 8192 -> 4096: nine steps.
            "desktop" => [1, 2, 4, 1, 0, 1],
            // Already Off, Half -> Quarter, 18 -> 2 in three, snap, pinned,
            // pinned: five steps.
            "mobile" => [0, 1, 3, 1, 0, 0],
            // Already Off, Half -> Quarter, 14 -> 2 in three, snap, the promoted
            // grid and the promoted raster both back to their floors: seven.
            "wasm32" => [0, 1, 3, 1, 1, 1],
            other => panic!("an unnamed bracket: {other}"),
        };
        assert_eq!(
            [shading, resolution, halvings, tiles, grid, raster],
            expected_rungs,
            "{}: the ladder's rungs take these steps here, not the steps its \
             constants were read to give — a rung moved or a bracket changed",
            limits.name,
        );
        assert_eq!(stop, expected_rungs.iter().sum::<u32>(), "{}", limits.name);

        for steps in [0u32, 1, 2, 3, 4, 5, 8, 64, u32::MAX / 2] {
            // Capped so the test itself stays quick; `demote` loops per step.
            let steps = steps.min(64);
            let profile = discrete(steps);
            check_invariants(&profile, &format!("{steps} steps back"));
            let b = resolve(&profile);
            assert!(
                b.offscreen_bytes >= limits.offscreen_bytes.floor
                    && b.grid_cells == limits.grid_cells.at(Promotion::Floor)
                    || steps < grid_at_floor_from,
                "{}: {steps} steps took the grid off its floor",
                b.name,
            );
            if steps >= stop {
                // Everything at its stop is the configuration a silent device got.
                assert_eq!(b.offscreen_bytes, unreadable.offscreen_bytes);
                assert_eq!(b.grid_cells, unreadable.grid_cells);
                assert_eq!(b.volume_texture_bytes, unreadable.volume_texture_bytes);
                assert_eq!(
                    b.app_texture_ceiling_bytes,
                    unreadable.app_texture_ceiling_bytes,
                );
                // And the two rungs the silent device never had: at their stops.
                assert_eq!(b.loop_render_budget, MIN_LOOP_FRAMES_PER_PANE);
                assert!(b.tile_whole_zoom);
                // The fixed point: one more step moves nothing.
                assert_eq!(
                    Budgets {
                        steps_back: b.steps_back,
                        ..resolve(&discrete(steps + 1))
                    },
                    b,
                    "{}: step {} still moved something past the stop",
                    b.name,
                    steps + 1,
                );
            } else {
                // Below the stop every step moves something: the ladder is
                // rungs, not a counter wearing one as a hat.
                assert_ne!(
                    Budgets {
                        steps_back: b.steps_back,
                        ..resolve(&discrete(steps + 1))
                    },
                    b,
                    "{}: step {} moved nothing, yet the stop is {stop}",
                    b.name,
                    steps + 1,
                );
            }
        }
    }
}

/// **A browser on a real driver draws a long-range sweep at twice the side a
/// browser on a software rasteriser does — and the software one keeps every
/// number it had.**
///
/// The rows are the four legs of
/// `.github/browser-rig/run_gpu_arm.sh --also-software`, run 2026-08-22 on one
/// build in one invocation, each naming the adapter that answered. Nothing here
/// is averaged across browsers or across arms: two browsers are two targets and
/// two arms are two machines. Firefox governs, so it is first.
///
/// | browser | adapter | `MAX_TEXTURE_SIZE` | `MAX_3D_TEXTURE_SIZE` |
/// |---|---|---:|---:|
/// | Firefox 153 | llvmpipe (Mesa), Xvfb | 16384 | 2048 |
/// | Firefox 153 | NVIDIA GeForce GTX 980, or similar | 32768 | 16384 |
/// | Chromium 151 | SwiftShader via ANGLE | 8192 | 2048 |
/// | Chromium 151 | RTX 3090 via ANGLE | 32768 | 16384 |
///
/// **The llvmpipe row is the one that earns the test.** It reports 16384 in 2D,
/// which clears [`DESKTOP_CLASS_REPORT`]'s 2D bar outright; a rule keyed on the
/// 2D cap alone would promote a software rasteriser. What holds it down is the
/// 3D cap, where two unrelated rasterisers independently answer 2048. A real
/// user reaches these figures through a blocklisted driver, not only through a
/// CI browser.
///
/// **Floor — `promote_on_the_2d_cap_alone`: drop the `max_texture_dimension_3d`
/// conjunct from `DeviceProfile::reported_promotion`.** The two software rows
/// then resolve 4096 and the software-unchanged block goes red on the Firefox
/// row while staying green on the Chromium one — which is why both software
/// adapters are here and why the assertion is per row rather than over the pair.
#[test]
fn a_real_driver_earns_a_wider_long_range_raster_and_a_software_one_keeps_its_own() {
    // (leg, 2D cap, 3D cap, driver-backed).
    const MEASURED: [(&str, u32, u32, bool); 4] = [
        ("firefox / llvmpipe, or similar (Mesa)", 16384, 2048, false),
        (
            "firefox / NVIDIA GeForce GTX 980, or similar",
            32768,
            16384,
            true,
        ),
        (
            "chromium / SwiftShader (Subzero) via ANGLE",
            8192,
            2048,
            false,
        ),
        (
            "chromium / NVIDIA GeForce RTX 3090 via ANGLE",
            32768,
            16384,
            true,
        ),
    ];

    // The bracket has somewhere to go. Without this the whole test passes on a
    // pinned axis by asserting the floor against itself, four times.
    let bracket = BudgetLimits::WASM.raster_side_ceiling_px;
    assert!(
        bracket.ceiling > bracket.floor,
        "the web raster ceiling is pinned at {}, so nothing below can fail",
        bracket.floor,
    );

    let shipped = resolve(&shipped_profile(BudgetLimits::WASM));
    let mut hardware_sides = Vec::new();

    for (leg, two_d, three_d, driver_backed) in MEASURED {
        let profile = DeviceProfile {
            // What wgpu reports through WebGL2 on **every** browser, driver or
            // not: `DeviceType::Other`. It is why the adapter's own numbers are
            // the only signal there is here.
            class: DeviceClass::Unknown,
            adapter: AdapterCeilings {
                max_texture_dimension_2d: two_d,
                max_texture_dimension_3d: three_d,
            },
            // The rig's Xvfb legs run with a mouse: form factor Desktop, on all
            // four rows.
            form_factor: Some(FormFactor::Desktop),
            ..shipped_profile(BudgetLimits::WASM)
        };
        check_invariants(&profile, leg);
        let b = resolve(&profile);
        let side = b.raster_side_for_adapter(two_d);

        if driver_backed {
            assert_eq!(b.promotion, Promotion::Ceiling, "{leg}");
            assert_eq!(
                side,
                crate::constants::WASM_RASTER_SIDE_CEILING_PROMOTED,
                "{leg}: a driver reporting {two_d} px 2D and {three_d} px 3D \
                 textures was held at the ceiling a phone browser gets",
            );
            hardware_sides.push(side);
        } else {
            assert_eq!(b.promotion, Promotion::Floor, "{leg}");
            assert_eq!(
                side,
                crate::constants::WASM_RASTER_SIDE_CEILING,
                "{leg}: a software rasteriser was handed a raster wider than \
                 the one it renders today — this is the arm a blocklisted \
                 driver lands a real user on",
            );
            // Not the raster axis alone: no field moved for this adapter.
            assert_eq!(
                b, shipped,
                "{leg}: a software rasteriser resolved something other than \
                 the shipped wasm32 configuration",
            );
        }
    }

    // The two outcomes are actually different numbers, said once and plainly.
    assert_eq!(
        hardware_sides.len(),
        2,
        "both hardware legs must be exercised"
    );
    for side in hardware_sides {
        assert!(
            side > shipped.raster_side_ceiling_px,
            "the promoted side {side} is not above the {} px floor, so the \
             promotion fires and changes nothing",
            shipped.raster_side_ceiling_px,
        );
    }
}

/// **The ceiling a browser can earn is a ceiling on a *long-range* sweep, and
/// nothing else moves.**
///
/// `squallar_radar::types::raster_side_px` returns `IMAGE_SIZE.min(ceiling)` at
/// or inside [`squallar_radar::types::BASE_EXTENT_KM`], so promoting this axis
/// must leave every ordinary tilt drawing exactly what it drew. That is the
/// half of the claim that bounds the cost — a browser rasterises on one thread,
/// and a blanket raise would be paid on every sweep rather than on the few that
/// carry the gates for it.
#[test]
fn the_promoted_ceiling_is_spent_on_long_range_sweeps_and_on_nothing_else() {
    use squallar_radar::types::{BASE_EXTENT_KM, IMAGE_SIZE, raster_side_px};

    let promoted = resolve(&DeviceProfile {
        adapter: AdapterCeilings {
            max_texture_dimension_2d: 32768,
            max_texture_dimension_3d: 16384,
        },
        ..shipped_profile(BudgetLimits::WASM)
    });
    let floor = resolve(&shipped_profile(BudgetLimits::WASM));
    let (wide, narrow) = (
        promoted.raster_side_for_adapter(32768),
        floor.raster_side_for_adapter(2048),
    );
    assert!(wide > narrow, "the arms under test are the same arm");

    // A gate spacing fine enough that the data never binds first, so what is
    // being read is the extent branch and not the Nyquist term.
    let dense_km = 0.05;
    for extent_km in [50.0, 150.0, BASE_EXTENT_KM] {
        assert_eq!(
            raster_side_px(extent_km, wide, dense_km),
            raster_side_px(extent_km, narrow, dense_km),
            "a {extent_km} km sweep drew differently on a promoted browser, \
             and every sweep at or inside {BASE_EXTENT_KM} km must not",
        );
        assert_eq!(raster_side_px(extent_km, wide, dense_km), IMAGE_SIZE);
    }
    // Past the base extent it is the ceiling that binds, and there the two arms
    // must part.
    for extent_km in [300.0, 460.0] {
        assert_eq!(raster_side_px(extent_km, wide, dense_km), wide);
        assert_eq!(raster_side_px(extent_km, narrow, dense_km), narrow);
    }
}

/// **On the web bracket the step is the ceiling, on every field that moves.**
/// The `Ceiling` rung is the slot a measured or probed desktop-browser tier
/// fills later; until it is filled, the two rungs above the floor resolve the
/// same numbers, and a browser held at the step by its form factor loses
/// nothing it had. Whoever fills the slot has to come past this line, and past
/// the web rows of [`the_signals_move_nothing_on_the_presumed_arm_and_only_the_pool_and_room_where_measured`], which stop being
/// an identity the moment the two rungs part.
#[test]
fn the_web_step_is_todays_ceiling_until_a_desktop_browser_tier_is_measured() {
    let w = BudgetLimits::WASM;
    for (name, bracket) in [
        ("image_side_px", w.image_side_px),
        ("long_range_image_side_px", w.long_range_image_side_px),
        ("loop_image_side_px", w.loop_image_side_px),
        ("section_width_px", w.section_width_px),
        ("concurrent_renders", w.concurrent_renders),
        ("concurrent_loop_downloads", w.concurrent_loop_downloads),
        ("loop_frames_held", w.loop_frames_held),
        ("loop_span_secs", w.loop_span_secs),
        ("loop_render_budget", w.loop_render_budget),
        ("loop_pool_bytes", w.loop_pool_bytes),
        ("volume_texture_bytes", w.volume_texture_bytes),
        ("offscreen_bytes", w.offscreen_bytes),
        ("mirror_bytes", w.mirror_bytes),
        ("render_cache_entries", w.render_cache_entries),
        ("max_panes", w.max_panes),
        ("app_texture_ceiling_bytes", w.app_texture_ceiling_bytes),
        ("raster_side_ceiling_px", w.raster_side_ceiling_px),
        ("tile_styled_bytes", w.tile_styled_bytes),
        ("tile_parsed_bytes", w.tile_parsed_bytes),
        ("tile_terrain_bytes", w.tile_terrain_bytes),
        ("tile_host_ceiling_bytes", w.tile_host_ceiling_bytes),
    ] {
        assert_eq!(
            bracket.step, bracket.ceiling,
            "wasm32 / {name}: the step ({}) and the ceiling ({}) parted, so a \
             browser whose form factor nobody classified now resolves less \
             than it did — a desktop-browser tier lands with its measurement, \
             and with this pin re-argued",
            bracket.step, bracket.ceiling,
        );
    }
    assert_eq!(
        w.grid_cells.step, w.grid_cells.ceiling,
        "wasm32 / grid_cells"
    );
    assert_eq!(
        w.quality_ceiling.step, w.quality_ceiling.ceiling,
        "wasm32 / quality_ceiling"
    );

    // And the rungs are real: the two promotable axes leave the floor, or the
    // step is a name with nothing behind it.
    assert_ne!(w.grid_cells.floor, w.grid_cells.step);
    assert_ne!(
        w.raster_side_ceiling_px.floor,
        w.raster_side_ceiling_px.step
    );
}

/// **The classifier's failure modes, one row each, and what the rule does with
/// them.**
///
/// The adapter report separates a driver from a software rasteriser and
/// nothing else. An iPad with a trackpad, a Chromebook and a phone docked to a
/// monitor all read `Desktop` from the pointer media — a fine pointer is
/// present — and the 3D conjunct of [`DESKTOP_CLASS_REPORT`] is the only thing
/// holding them at the floor. A handheld reporting desktop-class caps takes the
/// step, which on this bracket is the mobile tier and never above it. A
/// desktop-class browser whose shape nobody classified takes the step too, and
/// every value it resolves is the ceiling's — the behaviour-preservation proof,
/// as a row. `deviceMemory` lowers and never raises: 2 GiB beside a desktop
/// form factor is the step, 4 GiB leaves the ceiling where it was, 8 GiB on a
/// handheld is still the step, and no declaration at all is not a declaration
/// of plenty.
#[test]
fn the_form_factor_and_the_declared_memory_pick_between_the_step_and_the_ceiling() {
    use super::FormFactor::{Desktop, Handheld};
    use super::Promotion::{Ceiling, Floor, Step};
    const GIB: u64 = 1 << 30;

    // (leg, 2D cap, 3D cap, form factor, declared RAM, rung, why).
    type Row = (
        &'static str,
        u32,
        u32,
        Option<FormFactor>,
        Option<u64>,
        Promotion,
        &'static str,
    );
    const ROWS: [Row; 9] = [
        (
            "ipad + trackpad",
            16384,
            2048,
            Some(Desktop),
            None,
            Floor,
            "a trackpad reads Desktop and 16384 clears the 2D bar, so the 3D \
             conjunct is the only thing holding a tablet at the floor",
        ),
        (
            "chromebook",
            8192,
            2048,
            Some(Desktop),
            None,
            Floor,
            "below both bars; the form factor is never consulted at the floor",
        ),
        (
            "phone in DeX",
            16384,
            2048,
            Some(Desktop),
            None,
            Floor,
            "a docked phone reads Desktop from its mouse, and the 3D conjunct \
             holds it",
        ),
        (
            "touch laptop + dGPU",
            16384,
            16384,
            Some(Desktop),
            None,
            Ceiling,
            "any-pointer: fine wins over a coarse touchscreen, and a \
             desktop-class driver behind it earns the ceiling",
        ),
        (
            "handheld reporting desktop-class caps",
            16384,
            16384,
            Some(Handheld),
            None,
            Step,
            "a coarse-only pointer holds a desktop-class report at the step",
        ),
        (
            "desktop-class browser declaring 2 GiB",
            16384,
            16384,
            Some(Desktop),
            Some(2 * GIB),
            Step,
            "the declaration lowers the rung: 2 GiB is the handheld bucket",
        ),
        (
            "desktop-class browser declaring 4 GiB",
            16384,
            16384,
            Some(Desktop),
            Some(4 * GIB),
            Ceiling,
            "a declaration above the handheld bucket raises nothing and lowers \
             nothing",
        ),
        (
            "handheld declaring 8 GiB",
            16384,
            16384,
            Some(Handheld),
            Some(8 * GIB),
            Step,
            "a declaration never raises: 8 GiB on a coarse-only pointer is \
             still the step",
        ),
        (
            "desktop-class browser, shape unclassified",
            16384,
            16384,
            None,
            None,
            Step,
            "an unclassified shape is not a desktop; the step is what the \
             report alone earns",
        ),
    ];

    let web = |two_d, three_d, form_factor, declared_ram_bytes| DeviceProfile {
        adapter: AdapterCeilings {
            max_texture_dimension_2d: two_d,
            max_texture_dimension_3d: three_d,
        },
        form_factor,
        declared_ram_bytes,
        ..shipped_profile(BudgetLimits::WASM)
    };

    for (leg, two_d, three_d, form_factor, declared, rung, why) in ROWS {
        let profile = web(two_d, three_d, form_factor, declared);
        check_invariants(&profile, leg);
        assert_eq!(resolve(&profile).promotion, rung, "{leg}: {why}");
    }

    // The step on this bracket is the mobile tier, and never above it.
    let handheld = resolve(&web(16384, 16384, Some(Handheld), None));
    assert_eq!(
        handheld.grid_cells,
        crate::constants::MOBILE_VOLUME_GRID_CELLS,
        "a handheld reporting desktop-class caps was handed more than the \
         budget handheld hardware already runs",
    );

    // The behaviour-preservation proof: field for field, the rung name excepted.
    let shaped = resolve(&web(16384, 16384, Some(Desktop), None));
    let unshaped = resolve(&web(16384, 16384, None, None));
    assert_eq!(shaped.promotion, Ceiling);
    assert_eq!(
        unshaped,
        Budgets {
            promotion: Step,
            ..shaped
        },
        "a desktop-class browser whose shape nobody classified resolved a \
         value the same browser with a mouse does not",
    );
}

/// The 3D texture cap an Apple GPU reports through WebGL2. Measured 2026-09-02
/// by the browser rig's environment probe (its `A.safari.json` from a run at
/// tree c5673408), not by the app's own log, on the user's Mac mini M2 (10 GPU
/// cores, 8 GB unified) running macOS 26.4.1 and Safari 26.4: renderer string
/// `Apple GPU`, `MAX_TEXTURE_SIZE` 16384, `MAX_3D_TEXTURE_SIZE` 2048, and
/// WebGPU's `maxTextureDimension3D` on the same device also 2048. Firefox and
/// Chrome on the Mac were not run (no drivers); the WebGL2 caps come from the
/// GPU and its driver rather than the browser, so the same 2048 is presumed for
/// those two legs, unmeasured.
const APPLE_GPU_MAX_TEXTURE_DIMENSION_3D: u32 = 2048;

/// **A Mac in any browser resolves on its 3D cap, and the cap holds it at the
/// floor.** Every Mac browser reads `Desktop` (a trackpad is a fine pointer)
/// and Safari declares no memory, so the 3D conjunct alone decides between the
/// floor and the ceiling; the step is not reachable from this table. The
/// measured 2048 is a quarter of the 8192 the desktop-class line asks for, so
/// the conjunct fails and the form factor is never consulted: a Mac browser is
/// budgeted as a device that said nothing about itself.
#[test]
fn a_mac_browser_resolves_on_its_own_3d_cap() {
    let three_d = APPLE_GPU_MAX_TEXTURE_DIMENSION_3D;
    let desktop_class_three_d = DESKTOP_CLASS_REPORT.max_texture_dimension_3d;
    for leg in [
        "mac-safari-m-series",
        "mac-firefox-m-series",
        "mac-chrome-m-series",
    ] {
        let profile = DeviceProfile {
            adapter: AdapterCeilings {
                max_texture_dimension_2d: 16384,
                max_texture_dimension_3d: three_d,
            },
            form_factor: Some(FormFactor::Desktop),
            ..shipped_profile(BudgetLimits::WASM)
        };
        check_invariants(&profile, leg);
        let promotion = resolve(&profile).promotion;
        assert_eq!(
            promotion,
            Promotion::Floor,
            "{leg}: a 3D cap of {three_d} is under the desktop-class line's \
             {desktop_class_three_d}, so the 3D conjunct fails, the report is \
             not desktop-class, and the form factor is never asked; the floor \
             is what a device that said nothing about itself gets",
        );
        assert_ne!(
            promotion,
            Promotion::Step,
            "{leg}: a desktop form factor with no declaration never lands on \
             the step; the 3D conjunct alone decides",
        );
    }
}
