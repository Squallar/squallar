use super::*;
use crate::quality::{DESKTOP_PLATFORM_CEILING, MOBILE_PLATFORM_CEILING, WASM_PLATFORM_CEILING};

/// A profile for one shipped bracket, with every runtime field at its most
/// conservative reading.
fn shipped_profile(limits: BudgetLimits) -> DeviceProfile {
    DeviceProfile {
        platform: if limits.name == "wasm32" {
            Platform::Web
        } else {
            Platform::Native
        },
        limits,
        class: DeviceClass::Unknown,
        adapter: AdapterCeilings::WEBGL2_GUARANTEE,
        vram_bytes: None,
        system_ram_bytes: None,
        parallelism: 1,
        form_factor: None,
        memo: None,
    }
}

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
    assert_eq!(
        b.grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        VOLUME_GRID_FLOOR_SHAPE,
    );
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

/// The cross product the plan asks for: bracket × class × reported ceilings ×
/// VRAM reading × memo, with the shipped rows named so a regression on a real
/// target says which one.
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
                        out.push(DeviceProfile {
                            class,
                            adapter: AdapterCeilings {
                                max_texture_dimension_2d: two_d,
                                max_texture_dimension_3d: three_d,
                            },
                            vram_bytes: vram,
                            system_ram_bytes: vram.map(|v| v * 2),
                            parallelism: if limits.name == "wasm32" { 1 } else { 8 },
                            form_factor: if limits.name == "mobile" {
                                Some(FormFactor::Handheld)
                            } else {
                                Some(FormFactor::Desktop)
                            },
                            memo,
                            ..shipped_profile(limits)
                        });
                    }
                }
            }
        }
    }
    out
}

/// Every invariant a resolved budget must satisfy, whatever produced it.
fn check_invariants(profile: &DeviceProfile, from: &str) {
    let limits = &profile.limits;
    let b = resolve(profile);

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
    within(
        "loop_render_budget",
        b.loop_render_budget,
        limits.loop_render_budget,
    );
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
    assert_eq!(rows.len(), 720, "the matrix changed shape");
    for profile in &rows {
        check_invariants(profile, "matrix");
    }
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
            parallelism: rng.in_range(1, 256) as usize,
            form_factor: rng.pick(&[None, Some(FormFactor::Handheld), Some(FormFactor::Desktop)]),
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
    let desktop_class = web(
        DESKTOP_CLASS_REPORT.max_texture_dimension_2d,
        DESKTOP_CLASS_REPORT.max_texture_dimension_3d,
    );

    let floor = resolve(&at_the_guarantee);
    let promoted = resolve(&desktop_class);
    assert_eq!(floor.promotion, Promotion::Floor);
    assert_eq!(promoted.promotion, Promotion::Ceiling);
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

/// **The ladder is ordered, and lighting goes first.**
#[test]
fn the_ladder_surrenders_lighting_before_resolution_and_the_picture_last() {
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

    let one = stepped(1);
    assert_eq!(one.quality_ceiling.shading, GradientShading::Off);
    assert_eq!(one.quality_ceiling.resolution, ResolutionRung::Native);
    assert_eq!(one.offscreen_bytes, top.offscreen_bytes);
    assert_eq!(one.grid_cells, top.grid_cells);

    let two = stepped(2);
    assert_eq!(two.quality_ceiling.resolution, ResolutionRung::Half);
    assert_eq!(
        two.offscreen_bytes,
        BudgetLimits::DESKTOP.offscreen_bytes.floor
    );
    assert_eq!(two.grid_cells, top.grid_cells);
    assert_eq!(two.raster_side_ceiling_px, top.raster_side_ceiling_px);

    let deep = stepped(9);
    assert_eq!(deep.grid_cells, BudgetLimits::DESKTOP.grid_cells.floor);
    assert_eq!(
        deep.raster_side_ceiling_px,
        BudgetLimits::DESKTOP.long_range_image_side_px.floor,
    );
}

/// **A machine that keeps failing lands on the configuration this build already
/// shipped it, and stops.**
#[test]
fn no_number_of_back_offs_takes_a_machine_below_its_bracket_floor() {
    for limits in BudgetLimits::SHIPPED {
        let unreadable = resolve(&shipped_profile(limits));
        for steps in [0u32, 1, 2, 3, 4, 5, 8, 64, u32::MAX / 2] {
            // Capped so the test itself stays quick; `demote` loops per step.
            let steps = steps.min(64);
            let profile = DeviceProfile {
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
            check_invariants(&profile, &format!("{steps} steps back"));
            let b = resolve(&profile);
            assert!(
                b.offscreen_bytes >= limits.offscreen_bytes.floor
                    && b.grid_cells == limits.grid_cells.at(Promotion::Floor)
                    || steps < 3,
                "{}: {steps} steps took the grid off its floor",
                b.name,
            );
            if steps >= 4 {
                // Everything at its stop is the configuration a silent device got.
                assert_eq!(b.offscreen_bytes, unreadable.offscreen_bytes);
                assert_eq!(b.grid_cells, unreadable.grid_cells);
                assert_eq!(b.volume_texture_bytes, unreadable.volume_texture_bytes);
                assert_eq!(
                    b.app_texture_ceiling_bytes,
                    unreadable.app_texture_ceiling_bytes,
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
