use super::*;
use crate::volume::quality::{
    DESKTOP_PLATFORM_CEILING, MOBILE_PLATFORM_CEILING, WASM_PLATFORM_CEILING,
};

/// A profile for one shipped bracket, with every runtime field at its most
/// conservative reading.
///
/// The three of these are what `resolve` has to reproduce the shipped constants
/// for; everything else in this file varies around them.
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
///
/// The successor to `constants::tests::arms()`, and the same table read the
/// other way round: that one *was* a struct of budgets per class, which is what
/// made the conversion less invention than it sounds. Here the rows are
/// profiles and the budgets come out of [`resolve`].
pub fn profiles() -> [DeviceProfile; 3] {
    BudgetLimits::SHIPPED.map(shipped_profile)
}

/// **The resolver reproduces every shipped constant, field for field.**
///
/// The claim the whole extraction rests on, and it is checked rather than
/// argued: if a single field of a single arm came out different, the app would
/// have changed behaviour on that target the moment the constant stopped being
/// read directly — on two of three arms that no build here compiles.
///
/// Read against the **named arms** rather than against the `cfg`-selected
/// constants, because the `cfg`-selected ones are one row out of three and this
/// has to cover all three. The selection itself is covered by
/// `the_compiled_targets_budgets_are_the_constants_this_build_selected` below,
/// which is the one assertion that has to be `cfg`-gated.
#[test]
fn the_resolver_reproduces_every_shipped_constant() {
    use crate::constants::*;
    use rustdar_radar::types::{NATIVE_IMAGE_SIZE, WASM_IMAGE_SIZE};
    use rustdar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

    let expected = [
        Budgets {
            name: "wasm32",
            image_side_px: WASM_IMAGE_SIZE,
            long_range_image_side_px: WASM_LONG_RANGE_IMAGE_SIZE,
            loop_image_side_px: WASM_LOOP_IMAGE_SIZE,
            section_width_px: WASM_SECTION_WIDTH,
            concurrent_renders: WASM_MAX_CONCURRENT_RENDERS,
            concurrent_loop_downloads: NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
            loop_frames_held: WASM_MAX_LOOP_FRAMES,
            loop_render_budget: WASM_MAX_LOOP_RENDER_BUDGET,
            loop_pool_floor_bytes: WASM_LOOP_POOL_FLOOR_BYTES,
            loop_pool_ceiling_bytes: WASM_LOOP_POOL_CEILING_BYTES,
            grid_cells: WASM_VOLUME_GRID_CELLS,
            volume_texture_bytes: WASM_VOLUME_TEXTURE_BUDGET_BYTES,
            offscreen_bytes: WASM_VOLUME_OFFSCREEN_BUDGET_BYTES,
            mirror_bytes: WASM_VOLUME_MIRROR_BYTES_MAX,
            render_cache_entries: NON_MOBILE_MAX_RENDER_CACHE_ENTRIES,
            quality_ceiling: WASM_PLATFORM_CEILING,
            max_panes: rustdar_egui::pane::MAX_PANES_DESKTOP,
            app_texture_ceiling_bytes: WASM_APP_TEXTURE_BUDGET_BYTES,
            raster_side_ceiling_px: WASM_RASTER_SIDE_CEILING,
        },
        Budgets {
            name: "mobile",
            image_side_px: NATIVE_IMAGE_SIZE,
            long_range_image_side_px: MOBILE_LONG_RANGE_IMAGE_SIZE,
            loop_image_side_px: MOBILE_LOOP_IMAGE_SIZE,
            section_width_px: NATIVE_SECTION_WIDTH,
            concurrent_renders: MOBILE_MAX_CONCURRENT_RENDERS,
            concurrent_loop_downloads: MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
            loop_frames_held: MOBILE_MAX_LOOP_FRAMES,
            loop_render_budget: MOBILE_MAX_LOOP_RENDER_BUDGET,
            loop_pool_floor_bytes: MOBILE_LOOP_POOL_FLOOR_BYTES,
            loop_pool_ceiling_bytes: MOBILE_LOOP_POOL_CEILING_BYTES,
            grid_cells: MOBILE_VOLUME_GRID_CELLS,
            volume_texture_bytes: MOBILE_VOLUME_TEXTURE_BUDGET_BYTES,
            offscreen_bytes: MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES,
            mirror_bytes: MOBILE_VOLUME_MIRROR_BYTES_MAX,
            render_cache_entries: MOBILE_MAX_RENDER_CACHE_ENTRIES,
            quality_ceiling: MOBILE_PLATFORM_CEILING,
            max_panes: rustdar_egui::pane::MAX_PANES_MOBILE,
            app_texture_ceiling_bytes: MOBILE_APP_TEXTURE_BUDGET_BYTES,
            raster_side_ceiling_px: MOBILE_RASTER_SIDE_CEILING,
        },
        Budgets {
            name: "desktop",
            image_side_px: NATIVE_IMAGE_SIZE,
            long_range_image_side_px: DESKTOP_LONG_RANGE_IMAGE_SIZE,
            loop_image_side_px: DESKTOP_LOOP_IMAGE_SIZE,
            section_width_px: NATIVE_SECTION_WIDTH,
            concurrent_renders: DESKTOP_MAX_CONCURRENT_RENDERS,
            concurrent_loop_downloads: NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
            loop_frames_held: DESKTOP_MAX_LOOP_FRAMES,
            loop_render_budget: DESKTOP_MAX_LOOP_RENDER_BUDGET,
            loop_pool_floor_bytes: DESKTOP_LOOP_POOL_FLOOR_BYTES,
            loop_pool_ceiling_bytes: DESKTOP_LOOP_POOL_CEILING_BYTES,
            grid_cells: DESKTOP_VOLUME_GRID_CELLS,
            volume_texture_bytes: DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES,
            offscreen_bytes: DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
            mirror_bytes: DESKTOP_VOLUME_MIRROR_BYTES_MAX,
            render_cache_entries: NON_MOBILE_MAX_RENDER_CACHE_ENTRIES,
            quality_ceiling: DESKTOP_PLATFORM_CEILING,
            max_panes: rustdar_egui::pane::MAX_PANES_DESKTOP,
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
///
/// The one thing no other target can check on this one's behalf: the test above
/// covers all three bracket sets and says nothing about which one this build
/// took. `constants::tests::every_cascade_in_this_file_selected_the_same_arm`
/// is the same idea for the constants themselves, and this extends it across
/// the seam — a `BudgetLimits::for_target` arm pointing at the wrong bracket
/// would leave every other test in this file green.
#[test]
fn the_compiled_targets_budgets_are_the_constants_this_build_selected() {
    use crate::constants::*;

    let b = resolve(&DeviceProfile::for_target());
    assert_eq!(b.image_side_px, rustdar_radar::types::IMAGE_SIZE);
    assert_eq!(b.long_range_image_side_px, LONG_RANGE_IMAGE_SIZE);
    assert_eq!(b.loop_image_side_px, LOOP_IMAGE_SIZE);
    assert_eq!(b.section_width_px, rustdar_radar::xsect::SECTION_WIDTH);
    assert_eq!(b.concurrent_renders, MAX_CONCURRENT_RENDERS);
    assert_eq!(b.concurrent_loop_downloads, MAX_CONCURRENT_LOOP_DOWNLOADS);
    assert_eq!(b.loop_frames_held, MAX_LOOP_FRAMES);
    assert_eq!(b.loop_render_budget, MAX_LOOP_RENDER_BUDGET);
    assert_eq!(b.loop_pool_floor_bytes, LOOP_POOL_FLOOR_BYTES);
    assert_eq!(b.loop_pool_ceiling_bytes, LOOP_POOL_CEILING_BYTES);
    assert_eq!(b.volume_loop_bytes(), VOLUME_LOOP_TEXTURE_BUDGET_BYTES);
    assert_eq!(b.grid_cells, VOLUME_GRID_CELLS);
    assert_eq!(b.volume_texture_bytes, VOLUME_TEXTURE_BUDGET_BYTES);
    assert_eq!(b.offscreen_bytes, VOLUME_OFFSCREEN_BUDGET_BYTES);
    assert_eq!(b.mirror_bytes, VOLUME_MIRROR_BYTES_MAX);
    assert_eq!(b.render_cache_entries, MAX_RENDER_CACHE_ENTRIES);
    assert_eq!(b.quality_ceiling, crate::volume::quality::PLATFORM_CEILING);
    assert_eq!(b.app_texture_ceiling_bytes, APP_TEXTURE_BUDGET_BYTES);
    // And the shape a device at the guarantee is asked for is the one the
    // wasm `cargo check` row's const-assert guards.
    assert_eq!(
        b.grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        VOLUME_GRID_FLOOR_SHAPE,
    );
}

/// The limits an adapter might really report, as `(2D, 3D)` pairs.
///
/// The first four are spec floors and round numbers. The last two are
/// **measured on real machines** and are recorded here rather than spent: no
/// bracket is cut against them yet, and doing so is a separate, later decision.
///
/// | row | machine | 2D reported | 2D allocatable | 3D | `EXT_texture_norm16` |
/// |---|---|---|---|---|---|
/// | `firefox_3090` | Firefox, RTX 3090 | 32768 | **16384** | 16384 | absent |
/// | `chrome_890m`  | Chrome, Radeon 890M, DPR 2 | 16384 | 16384 | 8192 | present |
///
/// Two things they establish and one they do not. They establish that the
/// reported ceiling varies across real devices, and that one of them
/// **overstates by 2×** — so "cap against allocatable, not reported" is
/// load-bearing rather than paranoia, and the `firefox_3090` row is entered at
/// its allocatable figure for exactly that reason. They do not establish
/// anything about *browsers*: the two differ in both browser and GPU, so no row
/// is attributable to a browser alone. Neither is WebGPU present on either.
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
///
/// 3 × 5 × 6 × 4 × 2 = **720 rows**, against the three the `cfg` cascade could
/// ever compile. That is the whole value of this step stated as a number.
fn synthetic_profiles() -> Vec<DeviceProfile> {
    let mut out = Vec::new();
    for limits in BudgetLimits::SHIPPED {
        for class in CLASSES {
            for (_, two_d, three_d) in REPORTED_CEILINGS {
                for vram in [None, Some(2 << 30), Some(8 << 30), Some(24 << 30)] {
                    for memo in [
                        None,
                        Some(BudgetMemo {
                            loop_pool_bytes: limits.loop_pool_bytes.floor,
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
///
/// Factored out rather than written twice because the enumerated matrix and the
/// random sweep below have to hold each other's ground: an invariant checked on
/// only one of them is an invariant a profile nobody thought of can walk past.
fn check_invariants(profile: &DeviceProfile, from: &str) {
    let limits = &profile.limits;
    let b = resolve(profile);

    // Inside the bracket, both ends, on every field. This is the "monotone,
    // never below the floor and never above it" pair from the plan, and it is
    // what a promotion rule landing later has to keep true.
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
        b.quality_ceiling.capped_by(limits.quality_ceiling),
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
    // And the snugness proof beside it: a ceiling several times the real figure
    // passes the line above while admitting a silent doubling of any term.
    assert!(
        b.app_texture_ceiling_bytes * 4 <= total * 5,
        "{from} / {}: the {} MiB ceiling is more than 1.25x the {} MiB it bounds",
        b.name,
        b.app_texture_ceiling_bytes / (1024 * 1024),
        total / (1024 * 1024),
    );

    // The grid fits its own budget, in bytes as well as in cells.
    let grid = b.volume_bytes().expect("a bracketed grid cannot overflow");
    assert!(
        grid <= b.volume_texture_bytes,
        "{from} / {}: a {:?} grid is {grid} B against a {} B budget",
        b.name,
        b.grid_cells,
        b.volume_texture_bytes,
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

    // The loop is still a loop, and its history is still bounded by what the
    // dispatcher will texture rather than by what the pool would pay for.
    assert!(b.loop_render_budget >= crate::constants::MIN_LOOP_FRAMES_PER_PANE);
    assert!(b.loop_render_budget <= b.loop_frames_held);
    assert!(b.concurrent_renders > 0);
    assert!(b.render_cache_entries > 0);
    assert!(b.concurrent_loop_downloads > 0);
    // A full screen of loops at the minimum is payable out of the floor, or
    // the pool cliffs where it is meant to degrade.
    assert!(
        b.max_panes * crate::constants::MIN_LOOP_FRAMES_PER_PANE * b.loop_frame_bytes()
            <= b.loop_pool_floor_bytes,
        "{from} / {}: a full screen of loops does not fit the floor",
        b.name,
    );
}

/// **The whole matrix, not one row of three.**
///
/// The compile-time proof over the shipped desktop configuration is not what
/// this replaces — the floor's const-assert is still evaluated on the wasm
/// `cargo check` row, and `constants::tests` still binds every literal. What
/// this adds is the arms a `cfg` build can never reach: 720 configurations,
/// including every one the shipped desktop build can resolve to.
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
///
/// The pair the disagreement rule is asserted over. Firefox and Chromium are the
/// same binary from the same origin on the same silicon and are separated only
/// by what they report, so a `cfg` cannot express the question *at all* — not
/// coarsely, none. What governs is the **bracket**, not a branch: both resolve
/// inside the web bracket, and neither can push the app past what the other
/// would also survive.
///
/// The assertion is deliberately **relational** rather than absolute, so it is
/// worth having before the figures are spent: it says the two agree on
/// everything the bracket decides and differ only where the device does. The
/// rows carry this project's own measured readings — see [`REPORTED_CEILINGS`]
/// for what they are and what they are not evidence of.
#[test]
fn two_browser_profiles_on_one_machine_stay_inside_one_bracket() {
    let web = |two_d: u32, three_d: u32| DeviceProfile {
        adapter: AdapterCeilings {
            max_texture_dimension_2d: two_d,
            max_texture_dimension_3d: three_d,
        },
        ..shipped_profile(BudgetLimits::WASM)
    };
    // The measured pair. The Firefox row is entered at its **allocatable**
    // 16384 rather than its reported 32768: a figure a device will not hand
    // back is not a ceiling, it is a claim.
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

    // Same bracket, so same budgets: nothing here branches on browser identity,
    // and there is no term in `DeviceProfile` that could. What differs between
    // them is the *shape* the grid budget is spent into, which is the device's
    // answer rather than the browser's.
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
///
/// SplitMix64, written out rather than pulled in: this crate has no `proptest`
/// or `rand` dependency and a sweep over universally quantified invariants
/// needs neither shrinking nor a distribution — it needs a lot of rows and a
/// seed that names them again.
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
///
/// The enumerated matrix above sweeps the axes someone chose. The invariants are
/// all universally quantified, so the rows that matter are the ones outside
/// anybody's imagination: an adapter reporting a figure that is not a power of
/// two, a memo from a machine that backed off to an odd number, a browser
/// claiming a discrete GPU.
///
/// Deterministic, from a fixed seed, so a failure is reproducible from the
/// message alone rather than being a row that appears once in a thousand runs.
#[test]
fn a_random_sweep_of_profiles_satisfies_every_invariant() {
    let mut rng = Rng(0x5EED_B0D6_1234_5678);
    for row in 0..4096u32 {
        let limits = rng.pick(&BudgetLimits::SHIPPED);
        // Deliberately not held to powers of two, and deliberately allowed to
        // be under the WebGL2 guarantee: `shape_for_budget` is what has to cope,
        // and `every_axis_stays_within_the_limit_the_adapter_reported` is the
        // claim being swept here on 4096 more rows than it had.
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
                    loop_pool_bytes: rng.in_range(0, 8u64 << 30) as usize,
                }),
            },
            ..shipped_profile(limits)
        };
        check_invariants(&profile, &format!("random row {row}"));
    }
}
