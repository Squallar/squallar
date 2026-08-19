use super::*;
use crate::quality::{DESKTOP_PLATFORM_CEILING, MOBILE_PLATFORM_CEILING, WASM_PLATFORM_CEILING};

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
/// **measured on real machines**, and they are now *spent*: their componentwise
/// minimum is `DESKTOP_CLASS_REPORT`, the line a browser has to clear to be
/// promoted off the web floor.
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
    // And the snugness proof beside it: a ceiling several times the real figure
    // passes the line above while admitting a silent doubling of any term.
    assert!(
        b.app_texture_ceiling_bytes * 4 <= total * 5,
        "{from} / {}: the {} MiB ceiling is more than 1.25x the {} MiB it bounds",
        b.name,
        b.app_texture_ceiling_bytes / (1024 * 1024),
        total / (1024 * 1024),
    );

    // The grid fits its own budget **in bytes** is no longer asserted here:
    // the byte figure is the raymarch's arithmetic (`resident_grid_bytes`),
    // which sits above this crate, so that proof moved to rustdar-volumetric's
    // `raymarch::tests::budget_agreement` at WO-RD
    // (`the_volume_grid_fits_the_target_texture_budget`). Nothing is lost by
    // the move: `resolve` spends one promotion on both brackets and `demote`'s
    // grid rung resets both together, so every reachable
    // `(grid_cells, volume_texture_bytes)` pair — synthetic and random sweeps
    // included — is one of the three shipped pairs that test executes.

    // **One live 3D grid beside a looping one** is the other grid-byte
    // invariant that moved with it: both halves are swept — at every
    // promotion a bracket can reach, not only the shipped floors — by
    // `every_reachable_grid_fits_its_budgets_in_bytes` beside the raymarch.

    // The raster ceiling is a ceiling and never a *regression*: whatever a
    // promotion or a back-off did to it, a plan view may still reach the size
    // this build drew before any device was asked.
    assert!(
        b.raster_side_ceiling_px >= b.long_range_image_side_px,
        "{from} / {}: a {} px raster ceiling is below the {} px this build \
         already draws",
        b.name,
        b.raster_side_ceiling_px,
        b.long_range_image_side_px,
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
    // And a loop of *any* cadence is still a loop. Both ends of the clamp, on
    // every bracket the sweep reaches: a radar so slow one volume outlasts the
    // whole budget still gets two frames, and one so fast the median is a
    // second cannot buy frames the pool has not been sized for.
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
                    loop_pool_bytes: Some(rng.in_range(0, 8u64 << 30) as usize),
                    // Every rung of the ladder, including counts far past what
                    // `volume::degrade` lets a session reach: `demote` has to be
                    // total, and a memo written by a later build is data this
                    // one has to survive rather than trust.
                    steps_back: rng.in_range(0, 12) as u32,
                }),
            },
            ..shipped_profile(limits)
        };
        check_invariants(&profile, &format!("random row {row}"));
    }
}

/// The measured inter-volume gaps, in seconds, and what each radar is.
///
/// Campaign of 2026-08-11: six TDWR and four WSR-88D sites, a full 24 h, with
/// each object's VCP decoded from message 5 rather than inferred from its
/// interval — which mattered, because VCP 80 and VCP 90 share a cadence and no
/// interval could have told them apart.
const MEASURED_CADENCES: [(&str, u32); 4] = [
    ("TDWR VCP 80", 360),
    ("TDWR VCP 90", 360),
    ("WSR-88D precip (VCP 212/215)", 259),
    ("WSR-88D clear air (VCP 35)", 517),
];

/// **The same budget is the same wall clock on every radar.**
///
/// The whole point of a span budget, stated as the property rather than as the
/// formula: whatever the cadence, the frames the budget buys span at most the
/// budget and at least the budget less one volume. A frame count could not make
/// that claim — the desktop 30 frames this replaced spanned 2 h 05 m on a
/// WSR-88D in precip, 2 h 54 m on a TDWR and 4 h 18 m on the same WSR-88D in
/// clear air.
///
/// The lower bound is `span - cadence` rather than `span` because a truncating
/// divide is what makes this a *cap*: 2 h at 517 s is 14 frames covering
/// 1 h 52 m, and the fifteenth frame would put the loop over the budget.
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
                // The arm ran out of frames before it ran out of budget, which
                // is the ceiling doing its job rather than the span failing —
                // `constants::tests::the_span_budget_is_the_longest_the_ceiling
                // _can_pay_for` is where that case is argued.
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
///
/// Not a corner case: on 2026-08-11 every measured site but TDFW alternated
/// VCPs during the day, and a WSR-88D going from precip to clear air doubles
/// its volume length. `median_step_secs` is a median for exactly this reason —
/// the mean of a 259 s run and a 517 s run describes neither — so the frame
/// count follows whichever cadence held for more than half the listing, and
/// steps to the other one when the majority does.
///
/// What that buys is the property the caption depends on: the loop's wall clock
/// does not lurch when the radar changes mode, because both counts span the
/// same budget. What it costs is a step in the frame count, which is bounded
/// here.
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
        // A window that straddles the change takes whichever ran for most of
        // it, and both ends are inside the pair — there is no third answer for
        // a mixed window to land on.
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
///
/// The headline of this stage, stated as the two rows a `cfg` cascade cannot
/// tell apart at all: same binary, same origin, same WebGL2 backend, same
/// `DeviceClass::Unknown`, and the *only* thing between them is what the
/// adapter reports.
///
/// The phone row is deliberately not a measurement — this project has no phone
/// browser reading, and inventing one would be the scaled figure it forbids. It
/// is the **WebGL2 guarantee**, which is what the web budgets were derived from
/// in the first place, so what this asserts is the honest claim: a browser that
/// reports no more than the spec floor keeps every byte it had, and a browser
/// that reports what a measured desktop machine reported does not.
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

    // What the promotion actually buys, named rather than implied.
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
    // "And the pool it is divided out of moves with it, on the same rung" is
    // asserted beside the pool: `LoopPool` sits above this crate, so that half
    // lives in rustdar-app's `loop_pool::tests::budget_agreement`
    // (`a_promoted_browsers_pool_moves_on_the_same_rung`, WO-RD).

    // Neither is promoted past the bracket, and both satisfy every invariant.
    for profile in [&at_the_guarantee, &desktop_class] {
        check_invariants(profile, "browser separation");
    }
}

/// **A software rasteriser is never promoted, whatever it reports.**
///
/// The one class where the reported ceilings say nothing worth having: llvmpipe
/// will advertise 16384 and then take seconds a frame. `quality::select`
/// already puts it at the bottom of its own ladder, and this is the same
/// judgement applied to the budgets.
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
///
/// Named in the one unit that matters to the user: what a maximised 4K pane is
/// allowed to cost. Before this, 20 MiB paid for the 2560 x 1440 reference pane
/// at `Native` and nothing larger, so a 3840 x 2160 pane on a card with 24576
/// MiB of measured VRAM was rendered at half resolution and upscaled by the
/// blit.
#[test]
fn a_discrete_desktop_gpu_can_afford_a_4k_pane_at_native_resolution() {
    use crate::quality::{ResolutionRung, VolumeQuality};

    const FOUR_K: [u32; 2] = [3840, 2160];
    let unpromoted = resolve(&shipped_profile(BudgetLimits::DESKTOP));
    let discrete = resolve(&DeviceProfile {
        class: DeviceClass::Discrete,
        ..shipped_profile(BudgetLimits::DESKTOP)
    });
    assert_eq!(discrete.promotion, Promotion::Ceiling);

    let before = VolumeQuality::BEST.fit(FOUR_K, unpromoted.offscreen_bytes);
    let after = VolumeQuality::BEST.fit(FOUR_K, discrete.offscreen_bytes);
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
    // And the sum still holds with six of them budgeted at once.
    check_invariants(
        &DeviceProfile {
            class: DeviceClass::Discrete,
            ..shipped_profile(BudgetLimits::DESKTOP)
        },
        "discrete desktop",
    );

    // **An integrated desktop GPU is left exactly where it was, and it stays
    // there even when it reports desktop-class ceilings.** That is the
    // measurement's answer rather than an omission: the same cost model puts it
    // at 12-23 ms for a *1440 x 900* pane, so a 4K one is not a frame. It is
    // also the row this project actually measured — a Radeon 890M reporting
    // 16384/8192, which clears `DESKTOP_CLASS_REPORT` outright. A rule that let
    // the report raise a class the driver had already named would promote that
    // machine past what it can hold a frame at, on the strength of a number
    // about capacity answering a question about fill rate.
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
///
/// aarch64 is three of five targets and is entirely unmeasured from here. This
/// test is the standing note in executable form: if somebody unpins a mobile
/// rung, they have to come and delete this and say what they measured.
#[test]
fn the_mobile_bracket_promotes_nothing_until_somebody_measures_aarch64() {
    let pinned = |b: Bracket| b.floor == b.step && b.step == b.ceiling;
    let limits = BudgetLimits::MOBILE;
    assert!(pinned(limits.offscreen_bytes));
    assert!(pinned(limits.volume_texture_bytes));
    assert!(pinned(limits.app_texture_ceiling_bytes));
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
                // The rung is still *resolved* — a pinned bracket is a bracket
                // with nothing to spend it on, not a signal that was ignored —
                // so the claim is that every field came out the same, whatever
                // rung this class earned.
                promotion: profile.promotion(),
                ..resolve(&shipped_profile(BudgetLimits::MOBILE))
            },
            "a {class:?} adapter moved a mobile budget, on a target nobody has \
             run this on",
        );
    }
}

/// **The ladder is ordered, and lighting goes first.**
///
/// The order is the plan's, and it is the order the measurements argue for: the
/// cloud rung is 0.766 ms dense against 0.263 for the flat march, which is the
/// cheapest large saving in the application and the one a user is least likely
/// to be able to name. Resolution is next, and the picture itself gets coarser
/// only after both.
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

    // 1: lighting, and nothing else.
    let one = stepped(1);
    assert_eq!(one.quality_ceiling.shading, GradientShading::Off);
    assert_eq!(one.quality_ceiling.resolution, ResolutionRung::Native);
    assert_eq!(one.offscreen_bytes, top.offscreen_bytes);
    assert_eq!(one.grid_cells, top.grid_cells);

    // 2: the offscreen, both the rung and the budget that enforces it.
    let two = stepped(2);
    assert_eq!(two.quality_ceiling.resolution, ResolutionRung::Half);
    assert_eq!(
        two.offscreen_bytes,
        BudgetLimits::DESKTOP.offscreen_bytes.floor
    );
    assert_eq!(two.grid_cells, top.grid_cells);
    assert_eq!(two.raster_side_ceiling_px, top.raster_side_ceiling_px);

    // The picture's own resolution is late, and the raster side is last.
    let deep = stepped(9);
    assert_eq!(deep.grid_cells, BudgetLimits::DESKTOP.grid_cells.floor);
    assert_eq!(
        deep.raster_side_ceiling_px,
        BudgetLimits::DESKTOP.long_range_image_side_px.floor,
    );
}

/// **A machine that keeps failing lands on the configuration this build already
/// shipped it, and stops.**
///
/// The floor is a decision, not a limit that happens to be reached: below it
/// the answer is not a smaller budget, it is `volume::degrade` retiring the 3D
/// view, which latches after two surface losses and is a different mechanism.
/// So the ladder has to be *total* — any number of steps, on any bracket, still
/// resolves inside the bracket.
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
                // Everything this ladder owns is at its stop, and what is left
                // is the same configuration a device that said nothing got.
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
