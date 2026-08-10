use super::*;
use rustdar_radar::types::{IMAGE_SIZE, NATIVE_IMAGE_SIZE, WASM_IMAGE_SIZE};

/// One device class's share of every cascade in this file.
///
/// The four budget invariants below used to read the `cfg`-selected
/// constants directly, which meant each of them checked one arm out of
/// three and left the other two free — the same one-sided shape 3292e8d
/// fixed for the voxel grid, and it was still here for the budgets. The
/// arms all have names now, so a table can be built and every invariant
/// run against every row.
struct Arm {
    name: &'static str,
    /// `rustdar_radar::types::IMAGE_SIZE` for this class. It is a *two*-arm
    /// cascade — mobile is native — so this is where the two cascade shapes
    /// in this workspace are reconciled.
    image_size: usize,
    concurrent_renders: usize,
    loop_frames: usize,
    render_budget: usize,
    loop_budget: usize,
    grid: [u32; 3],
    volume_budget: usize,
}

impl Arm {
    /// Bytes one loop frame's texture occupies: RGBA at `image_size²`.
    /// Loop frames carry no value grid — `poll_loop_render_results` stores an
    /// empty one — so this is the whole cost, unlike a static pane render.
    fn loop_frame_bytes(&self) -> usize {
        self.image_size * self.image_size * 4
    }

    /// Frames that hold a texture at once. `evict_textures_outside_render_set`
    /// runs every dispatch with `MAX_LOOP_RENDER_BUDGET`, so a loop of
    /// `MAX_LOOP_FRAMES` keeps only the render set textured.
    fn textured_frames(&self) -> usize {
        self.render_budget.min(self.loop_frames)
    }

    /// Bytes one pane's 3D volume texture occupies: every mip level of the
    /// grid at `crate::volume::VOLUME_TEXTURE_FORMAT`'s two bytes a cell, plus
    /// the RGBA table those cells index.
    ///
    /// Read from `volume::raymarch::grid_bytes_with_mips` rather than
    /// recomputed, so the budget is checked against the arithmetic the upload
    /// path actually allocates by — including the coarse level, which the
    /// earlier hand-written product silently left out of the budget entirely.
    ///
    /// Two bytes per cell is not an assumption to be tidied away: the format is
    /// `Rg8Unorm` because the march reconstructs `R̄ / Ḡ` from a
    /// coverage-premultiplied index and a coverage channel, and because
    /// `Rg8Unorm` is *filterable* under `Features::empty()` where `R32Float` is
    /// not.
    fn volume_bytes(&self) -> usize {
        crate::volume::raymarch::grid_bytes_with_mips(self.grid)
            .expect("a shipped grid shape cannot overflow")
            + VOLUME_LUT_BYTES
    }
}

/// Every device class this workspace builds for, exactly once.
fn arms() -> [Arm; 3] {
    [
        Arm {
            name: "wasm32",
            image_size: WASM_IMAGE_SIZE,
            concurrent_renders: WASM_MAX_CONCURRENT_RENDERS,
            loop_frames: WASM_MAX_LOOP_FRAMES,
            render_budget: WASM_MAX_LOOP_RENDER_BUDGET,
            loop_budget: WASM_LOOP_TEXTURE_BUDGET_BYTES,
            grid: WASM_VOLUME_GRID_CELLS,
            volume_budget: WASM_VOLUME_TEXTURE_BUDGET_BYTES,
        },
        Arm {
            name: "mobile",
            image_size: NATIVE_IMAGE_SIZE,
            concurrent_renders: MOBILE_MAX_CONCURRENT_RENDERS,
            loop_frames: MOBILE_MAX_LOOP_FRAMES,
            render_budget: MOBILE_MAX_LOOP_RENDER_BUDGET,
            loop_budget: MOBILE_LOOP_TEXTURE_BUDGET_BYTES,
            grid: MOBILE_VOLUME_GRID_CELLS,
            volume_budget: MOBILE_VOLUME_TEXTURE_BUDGET_BYTES,
        },
        Arm {
            name: "desktop",
            image_size: NATIVE_IMAGE_SIZE,
            concurrent_renders: DESKTOP_MAX_CONCURRENT_RENDERS,
            loop_frames: DESKTOP_MAX_LOOP_FRAMES,
            render_budget: DESKTOP_MAX_LOOP_RENDER_BUDGET,
            loop_budget: DESKTOP_LOOP_TEXTURE_BUDGET_BYTES,
            grid: DESKTOP_VOLUME_GRID_CELLS,
            volume_budget: DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES,
        },
    ]
}

/// The ceiling the per-target constants were chosen to fit, checked on
/// **every** arm rather than on the one this build compiled.
///
/// This is the table in [`LOOP_TEXTURE_BUDGET_BYTES`]' doc comment, executed.
/// Two of its three rows were previously prose.
#[test]
fn loop_frames_fit_the_target_texture_budget() {
    for arm in arms() {
        let total = arm.textured_frames() * arm.loop_frame_bytes();
        assert!(
            total <= arm.loop_budget,
            "{}: {} textured frames x {}^2 x 4B = {} MiB, over the {} MiB budget",
            arm.name,
            arm.textured_frames(),
            arm.image_size,
            total / (1024 * 1024),
            arm.loop_budget / (1024 * 1024),
        );
    }
}

/// The budget is meant to be snug. A ceiling several times the real figure would
/// pass the check above while permitting a silent doubling of any constant in it.
#[test]
fn the_budget_is_not_slack_enough_to_hide_a_doubling() {
    for arm in arms() {
        let total = arm.textured_frames() * arm.loop_frame_bytes();
        assert!(
            total * 2 > arm.loop_budget,
            "{}: budget {} MiB is more than twice the actual {} MiB — it would \
                 not catch a regression",
            arm.name,
            arm.loop_budget / (1024 * 1024),
            total / (1024 * 1024),
        );
    }
}

/// The eviction budget is what bounds memory, so it has to be the smaller of the
/// two. If it ever exceeded the frame cap, `render_set_indices` would clamp it
/// back to the frame count and every held frame would stay textured — silently
/// restoring the `MAX_LOOP_FRAMES × frame` figure the budget above rules out.
/// The ordering itself is asserted at compile time next to the constants — but
/// only for the compiled arm, which is why it is asserted for all three here.
#[test]
fn the_render_budget_is_what_bounds_the_textured_frames() {
    for arm in arms() {
        assert_eq!(arm.textured_frames(), arm.render_budget, "{}", arm.name);
        // A zero anywhere in the cascade is a loop that renders nothing, and
        // the compile-time block next to the constants only sees one arm.
        assert!(arm.render_budget > 0, "{}", arm.name);
        assert!(arm.concurrent_renders > 0, "{}", arm.name);
    }
}

/// Every arm is held to its own volume budget, exactly as
/// `loop_frames_fit_the_target_texture_budget` holds it to its loop budget.
#[test]
fn the_volume_grid_fits_the_target_texture_budget() {
    for arm in arms() {
        let total = arm.volume_bytes();
        assert!(
            total <= arm.volume_budget,
            "{}: a {:?} grid plus a {VOLUME_LUT_BYTES} B table is {total} B, \
                 over the {} B budget",
            arm.name,
            arm.grid,
            arm.volume_budget,
        );
    }
}

/// The sibling of `the_budget_is_not_slack_enough_to_hide_a_doubling`, and for
/// the same reason: a ceiling several times the real figure passes the check
/// above while permitting any axis to be silently doubled.
///
/// Doubling one axis is the realistic regression here, not doubling the whole
/// grid — and it is exactly what this catches, because doubling any single
/// axis doubles the total.
#[test]
fn the_volume_budget_is_not_slack_enough_to_hide_a_doubling() {
    for arm in arms() {
        let total = arm.volume_bytes();
        assert!(
            total * 2 > arm.volume_budget,
            "{}: budget {} B is more than twice the actual {total} B — it \
                 would not catch a doubled grid axis",
            arm.name,
            arm.volume_budget,
        );
    }
}

/// The literals behind the tables in the two budget doc comments.
///
/// The invariants above are relations, and a relation holds just as well
/// after both of its sides move together — which is the one change they
/// cannot see. `the_grid_dimensions_match_the_shapes_rustdar_radar_names`
/// pins the grid triples for the same reason; this is the rest of the row.
#[test]
fn the_documented_per_class_figures_are_what_the_arms_actually_say() {
    let expected = [
        // name, image, concurrent, held, textured, loop budget MiB, volume budget B
        ("wasm32", 1024, 1, 12, 8, 48, 3 * 1024 * 1024),
        ("mobile", 2048, 3, 20, 12, 256, 10 * 1024 * 1024),
        ("desktop", 2048, 6, 60, 30, 512, 24 * 1024 * 1024),
    ];
    for (arm, (name, image, concurrent, held, textured, loop_mib, volume)) in
        arms().into_iter().zip(expected)
    {
        assert_eq!(arm.name, name);
        assert_eq!(arm.image_size, image, "{name} image size");
        assert_eq!(arm.concurrent_renders, concurrent, "{name} renders");
        assert_eq!(arm.loop_frames, held, "{name} held frames");
        assert_eq!(arm.render_budget, textured, "{name} render budget");
        assert_eq!(
            arm.loop_budget,
            loop_mib * 1024 * 1024,
            "{name} loop budget"
        );
        assert_eq!(arm.volume_budget, volume, "{name} volume budget");
    }
}

/// This target's cascades all selected the *same* arm as each other.
///
/// `cfg`-gated, because the selection is the one thing here no other target
/// can check on behalf of this one — and it is a real hazard rather than a
/// formality: the arms are six near-identical `#[cfg(all(…))]` lines per
/// constant, and a mismatched one gives a build a mobile frame budget with
/// a desktop texture ceiling, which passes every invariant above.
#[test]
fn every_cascade_in_this_file_selected_the_same_arm() {
    #[cfg(target_arch = "wasm32")]
    let arm = &arms()[0];
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    let arm = &arms()[1];
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    let arm = &arms()[2];

    assert_eq!(IMAGE_SIZE, arm.image_size, "{}", arm.name);
    assert_eq!(
        MAX_CONCURRENT_RENDERS, arm.concurrent_renders,
        "{}",
        arm.name
    );
    assert_eq!(MAX_LOOP_FRAMES, arm.loop_frames, "{}", arm.name);
    assert_eq!(MAX_LOOP_RENDER_BUDGET, arm.render_budget, "{}", arm.name);
    assert_eq!(LOOP_TEXTURE_BUDGET_BYTES, arm.loop_budget, "{}", arm.name);
    assert_eq!(VOLUME_GRID_CELLS, arm.grid, "{}", arm.name);
    assert_eq!(
        VOLUME_TEXTURE_BUDGET_BYTES, arm.volume_budget,
        "{}",
        arm.name
    );
}

/// The `(cfg attribute, right-hand side)` of every `#[cfg]`-gated
/// definition of `name`, in source order.
fn cascade_arms(code: &str, name: &str) -> Vec<(String, String)> {
    let definition = format!("pub const {name}: ");
    let lines: Vec<&str> = code.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with(&definition))
        .map(|(i, line)| {
            let (_, rhs) = line
                .split_once(" = ")
                .unwrap_or_else(|| panic!("{name} has no right-hand side: {line}"));
            let cfg = lines[..i]
                .iter()
                .rev()
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with("//"))
                .unwrap_or_else(|| panic!("nothing at all precedes {name}"));
            (
                cfg.to_string(),
                rhs.trim().trim_end_matches(';').to_string(),
            )
        })
        .collect()
}

/// The name of every `const` whose wasm32 arm this file declares, sorted
/// and deduplicated.
///
/// Keyed on the wasm32 arm because that is the one no build on this machine
/// compiles. Two-arm `mobile` / `not(mobile)` cascades — the download and
/// render-cache caps — have no `target_arch` arm at all, so a host build
/// picks between the same two values a phone build would and they are not
/// device-class cascades in this sense.
///
/// Three near-misses this deliberately does *not* have, each of which was a
/// way to add a cascade the census could not see:
///
/// - **a doc comment between the attribute and the item.** Legal Rust,
///   `fmt`-clean, and a look at line `i + 1` alone walks straight past it.
///   So the look-ahead skips `///`, `//` and blank lines, exactly as
///   [`cascade_arms`] already does looking *back*.
/// - **`const` without `pub`, or an indented one.** Neither changes that the
///   value is `cfg`-selected.
/// - **a wasm arm spelled some other way**, e.g. `all(target_arch =
///   "wasm32")`. Matched on content rather than byte-for-byte: any `cfg`
///   naming the wasm arch, other than the `not(...)` guard the sibling arms
///   carry. The per-name check below then insists on the canonical spelling,
///   so an odd one fails there rather than vanishing here.
fn wasm_gated_constants(code: &str) -> Vec<&str> {
    let lines: Vec<&str> = code.lines().collect();
    let is_wasm_arm = |line: &str| {
        let line = line.trim();
        line.starts_with("#[cfg(")
            && line.contains(r#"target_arch = "wasm32""#)
            && !line.contains(r#"not(target_arch = "wasm32")"#)
    };
    let mut names: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_wasm_arm(line))
        .filter_map(|(i, _)| {
            lines[i + 1..]
                .iter()
                .map(|l| l.trim_start())
                .find(|l| !l.is_empty() && !l.starts_with("//"))
        })
        .map(|item| item.strip_prefix("pub ").unwrap_or(item))
        .filter_map(|item| item.strip_prefix("const "))
        .filter_map(|rest| rest.split_once(':'))
        .map(|(name, _)| name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Every `cfg` arm selects the constant named for *its own* device class.
///
/// `every_cascade_in_this_file_selected_the_same_arm` covers this for the
/// arm the running target compiles and can cover no other. That is not a
/// theoretical gap: pointing the wasm32 arm of `MAX_LOOP_FRAMES` at
/// `DESKTOP_MAX_LOOP_FRAMES` leaves every test in this workspace passing
/// and the wasm `cargo check` exiting 0, because nothing on a host ever
/// evaluates that line. It is the one mutation that survived the probe run
/// that landed these tests, which is why this exists.
///
/// So read the cascades as source instead. Three arms per constant in one
/// fixed shape: the `cfg` picks the device class, and the right-hand side
/// has to name the constant for that class. Reading the source is the weak
/// form of the check — it cannot see a wrongly *valued* constant, which is
/// what every test above is for — but it is the only form available without
/// a wasm test runner.
#[test]
fn every_cfg_arm_selects_the_constant_named_for_its_device_class() {
    let source = include_str!("../constants.rs");
    // The shipped half only: the expected strings below appear verbatim in
    // this test's own source.
    let (code, _) = source
        .split_once("#[cfg(test)]")
        .expect("constants.rs no longer has a test module");

    let expected = [
        (r#"#[cfg(target_arch = "wasm32")]"#, "WASM"),
        (
            r#"#[cfg(all(not(target_arch = "wasm32"), mobile))]"#,
            "MOBILE",
        ),
        (
            r#"#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]"#,
            "DESKTOP",
        ),
    ];

    let covered = [
        "MAX_CONCURRENT_RENDERS",
        "MAX_LOOP_RENDER_BUDGET",
        "MAX_LOOP_FRAMES",
        "LOOP_TEXTURE_BUDGET_BYTES",
        "VOLUME_GRID_CELLS",
        "VOLUME_TEXTURE_BUDGET_BYTES",
        // Lifted by WP-I after this test first listed it as exempt. It is
        // covered here as well as by
        // `each_offscreen_budget_arm_selects_its_own_classs_constant`; the
        // overlap is deliberate, because that test checks one cascade and
        // this one checks that no cascade is missing.
        "VOLUME_OFFSCREEN_BUDGET_BYTES",
    ];

    // Cascades that still spell their arms as literals, and so cannot be
    // checked here. Written down rather than left implicit: a test named
    // "every cfg arm" that silently covered six of seven would be the same
    // shape of vacuity it exists to catch. Empty today, and the mechanism
    // stays because the next cascade to land will need it before it is
    // lifted — as `VOLUME_OFFSCREEN_BUDGET_BYTES` did for one commit.
    let exempt: [&str; 0] = [];

    // Every three-arm cascade in the file is one or the other, so adding a
    // new one is a failure here rather than a silent gap.
    let found = wasm_gated_constants(code);
    let mut accounted: Vec<&str> = covered.iter().chain(exempt.iter()).copied().collect();
    accounted.sort_unstable();
    assert_eq!(
        found, accounted,
        "the set of `cfg`-selected constants in this file has changed. A \
             new one has to be lifted into named arms and listed in `covered`, \
             or listed in `exempt` with the reason it cannot be."
    );

    // An exemption has to still *be* one. The rot that matters runs the
    // other way from the obvious one: a cascade gets lifted and nobody
    // moves it out of `exempt`, so it looks accounted for while its arms go
    // unchecked — which is exactly what happened to
    // `VOLUME_OFFSCREEN_BUDGET_BYTES` between this test landing and WP-I
    // lifting it, and the census did not notice. A lifted arm's right-hand
    // side is a bare `SCREAMING_CASE` name; a literal never is.
    for name in exempt {
        for (cfg, rhs) in cascade_arms(code, name) {
            assert!(
                !rhs.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "the {cfg} arm of {name} selects `{rhs}`, which is a named \
                     constant, so {name} has been lifted. Move it from `exempt` \
                     to `covered` — while it sits here its arms are checked by \
                     nothing."
            );
        }
    }

    for name in covered {
        let arms = cascade_arms(code, name);
        assert_eq!(
            arms.len(),
            expected.len(),
            "{name} has {} `cfg` arms, not {}: {arms:?}. The three-arm shape \
                 is what keeps them mutually exclusive — see MAX_LOOP_FRAMES' \
                 doc comment.",
            arms.len(),
            expected.len(),
        );
        for ((cfg, rhs), (want_cfg, class)) in arms.iter().zip(expected) {
            assert_eq!(cfg, want_cfg, "{name}");
            assert_eq!(
                rhs,
                &format!("{class}_{name}"),
                "the {cfg} arm of {name} selects `{rhs}`, which is not the \
                     {class} value. No host build can evaluate this line."
            );
        }
    }
}

/// The web image fits what a browser is *guaranteed* to accept.
///
/// `rustdar_radar` states the 2048 floor as a literal because it has no wgpu
/// dependency and must not grow one — it hands finished RGBA buffers to the
/// crate that owns the GPU. This is that crate, so this is where the floor
/// gets checked against wgpu's own downlevel limits rather than against a
/// number someone typed. Without it, `WEBGL2_MAX_TEXTURE_DIMENSION_2D` could
/// be raised to accommodate an over-large image instead of the image being
/// the thing that gives.
#[test]
fn the_web_image_fits_the_texture_size_webgl2_guarantees() {
    let guaranteed = wgpu::Limits::downlevel_webgl2_defaults().max_texture_dimension_2d;
    assert_eq!(
        rustdar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D as u32,
        guaranteed,
        "rustdar_radar's copy of the WebGL2 2D floor has drifted from wgpu's"
    );
    assert!(
        WASM_IMAGE_SIZE as u32 <= guaranteed,
        "the web radar image is {WASM_IMAGE_SIZE} px, over the {guaranteed} px \
             2D texture WebGL2 guarantees — every browser render would fail"
    );
    // And with the whole other half of the guarantee still free, which is
    // the stated reason the web arm halves rather than matching native: the
    // overlay textures are allocated alongside the radar frame.
    assert!(WASM_IMAGE_SIZE as u32 * 2 <= guaranteed);
}

/// The reference pane fits this target's offscreen budget **at its own
/// quality ceiling**, i.e. without being degraded to get there.
///
/// The sibling of `the_volume_grid_fits_the_target_texture_budget`, with
/// one extra assertion it does not need: the grid either fits or it does
/// not, whereas the offscreen would silently step down a rung. A budget
/// that forced the reference pane to degrade would pass a plain "fits"
/// check while quietly halving the resolution of every volume on a display
/// this target is meant to render at full size.
#[test]
fn the_reference_pane_fits_the_target_offscreen_budget_undegraded() {
    let fitted = crate::volume::quality::reference_offscreen();
    assert!(
        fitted.bytes() <= VOLUME_OFFSCREEN_BUDGET_BYTES,
        "a {:?} offscreen is {} B, over the {VOLUME_OFFSCREEN_BUDGET_BYTES} \
             B budget",
        fitted.size,
        fitted.bytes(),
    );
    assert_eq!(
        fitted.quality,
        crate::volume::quality::PLATFORM_CEILING,
        "the {VOLUME_OFFSCREEN_REFERENCE_PANE_PX:?} reference pane cannot be \
             rendered at this target's own quality ceiling within a \
             {VOLUME_OFFSCREEN_BUDGET_BYTES} B budget, so the ceiling describes \
             a quality the budget never lets anything select"
    );
}

/// And the offscreen budget is snug, exactly as the other two are.
///
/// The realistic regression is the reference pane growing or the ceiling
/// moving up a rung — both of which double the figure, and both of which a
/// budget several times the real number would absorb without a word.
#[test]
fn the_offscreen_budget_is_not_slack_enough_to_hide_a_doubling() {
    let total = crate::volume::quality::reference_offscreen().bytes();
    assert!(
        total * 2 > VOLUME_OFFSCREEN_BUDGET_BYTES,
        "budget {VOLUME_OFFSCREEN_BUDGET_BYTES} B is more than twice the \
             actual {total} B — it would not catch a doubled reference pane"
    );
}

/// Both offscreen budget checks, on **all three** arms rather than the one
/// this build compiled.
///
/// The two tests above are one-sided in exactly the way
/// `the_grid_dimensions_match_the_shapes_rustdar_radar_names` was before
/// `3292e8d`: they read `VOLUME_OFFSCREEN_BUDGET_BYTES` and
/// `PLATFORM_CEILING`, both `cfg`-selected, so two of three arms went
/// unchecked. A budget that could not pay for its own reference pane on
/// wasm would be a browser whose every volume is quietly rendered a rung
/// coarser than intended, and no CI row would say so.
///
/// The pairing is the point: each arm is checked against **its own**
/// ceiling, because the ceiling is what decides how many pixels the
/// reference pane costs there.
#[test]
fn every_offscreen_budget_arm_pays_for_its_own_reference_pane() {
    use crate::volume::quality::{
        DESKTOP_PLATFORM_CEILING, MOBILE_PLATFORM_CEILING, WASM_PLATFORM_CEILING,
    };

    for (target, budget, ceiling) in [
        (
            "wasm",
            WASM_VOLUME_OFFSCREEN_BUDGET_BYTES,
            WASM_PLATFORM_CEILING,
        ),
        (
            "mobile",
            MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES,
            MOBILE_PLATFORM_CEILING,
        ),
        (
            "desktop",
            DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
            DESKTOP_PLATFORM_CEILING,
        ),
    ] {
        let fitted = ceiling.fit(VOLUME_OFFSCREEN_REFERENCE_PANE_PX, budget);
        assert_eq!(
            fitted.quality, ceiling,
            "the {target} budget of {budget} B cannot render the \
                 {VOLUME_OFFSCREEN_REFERENCE_PANE_PX:?} reference pane at its \
                 own {ceiling:?} ceiling — it degrades to {:?}, so the ceiling \
                 names a quality that target never reaches",
            fitted.quality
        );
        assert!(
            fitted.bytes() <= budget,
            "the {target} offscreen is {} B against a {budget} B budget",
            fitted.bytes()
        );
        assert!(
            fitted.bytes() * 2 > budget,
            "the {target} budget of {budget} B is more than twice its \
                 actual {} B — it would not catch a doubled reference pane",
            fitted.bytes()
        );
    }
}

/// Each offscreen budget arm selects **its own** class's constant.
///
/// Naming the arms outside the cascade pins their values and nothing else:
/// pointing the wasm32 arm at `DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES` was
/// measured to leave the whole workspace green with the wasm
/// `--all-targets` check at 0, because on a host the other two arms are
/// dead text. Reading the source is the only instrument that sees it.
///
/// Shares its reasoning, and its shape, with
/// `volume::quality::each_ceiling_arm_selects_its_own_classs_constant`.
#[test]
fn each_offscreen_budget_arm_selects_its_own_classs_constant() {
    let source = include_str!("../constants.rs");
    for (cfg, class) in [
        (r#"target_arch = "wasm32""#, "WASM"),
        (r#"all(not(target_arch = "wasm32"), mobile)"#, "MOBILE"),
        (
            r#"all(not(target_arch = "wasm32"), not(mobile))"#,
            "DESKTOP",
        ),
    ] {
        let definition = format!("#[cfg({cfg})]\npub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize =");
        let occurrences = source.matches(&definition).count();
        assert_eq!(
            occurrences, 1,
            "expected exactly one VOLUME_OFFSCREEN_BUDGET_BYTES definition \
                 under `#[cfg({cfg})]`, found {occurrences}"
        );
        let at = source.find(&definition).expect("just counted one");
        let (selected, _) = source[at + definition.len()..]
            .split_once(';')
            .expect("a const definition with no semicolon");
        let expected = format!("{class}_VOLUME_OFFSCREEN_BUDGET_BYTES");
        assert_eq!(
            selected.trim(),
            expected,
            "the `#[cfg({cfg})]` arm does not select `{expected}`. An arm \
                 pointing at another class's budget compiles and passes \
                 everything CI runs."
        );
    }
}

/// The compiled cascade selects one of the three named budgets.
///
/// Weaker than the scrape above and kept anyway: it is the one assertion
/// that survives the source being reformatted out from under the scrape.
#[test]
fn the_compiled_offscreen_budget_is_one_of_the_named_arms() {
    assert!(
        [
            WASM_VOLUME_OFFSCREEN_BUDGET_BYTES,
            MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES,
            DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
        ]
        .contains(&VOLUME_OFFSCREEN_BUDGET_BYTES),
        "VOLUME_OFFSCREEN_BUDGET_BYTES is {VOLUME_OFFSCREEN_BUDGET_BYTES}, \
             which is none of the three named arms"
    );
}

/// The WebGL2 3D-texture floor is wgpu's figure, not a hand-written 256.
///
/// Comparing the *value* against wgpu proves nothing on its own: a
/// `= 256;` literal satisfies that assertion exactly, because 256 is what
/// wgpu says today. What makes the constant honest is where it comes from, and
/// only the source says that. The realistic regression is someone replacing
/// the derivation with the literal in order to drop the `wgpu` import from
/// this file — at which point the doc comment above becomes false and the
/// bound stops tracking the limits the device request is held to.
#[test]
fn the_webgl2_3d_limit_is_derived_from_wgpu_rather_than_written_out() {
    let source = include_str!("../constants.rs");
    let definition = source
        .split_once("pub const WEBGL2_MAX_TEXTURE_DIMENSION_3D: u32 =")
        .and_then(|(_, rest)| rest.split_once(';'))
        .map(|(value, _)| value)
        .expect("WEBGL2_MAX_TEXTURE_DIMENSION_3D is no longer defined here");
    assert!(
        definition.contains("downlevel_webgl2_defaults()")
            && definition.contains("max_texture_dimension_3d"),
        "WEBGL2_MAX_TEXTURE_DIMENSION_3D is defined as `{}`, which does not \
             read wgpu's own WebGL2 downlevel limits. A literal cannot drift \
             *with* wgpu, so it stops describing what the device request is held \
             to the moment wgpu revises the figure.",
        definition.trim()
    );

    // And 256 is still what that derivation yields. Separate assertion so a
    // wgpu bump that raised the floor is a visible failure to be reviewed,
    // rather than a grid bound that silently loosened.
    assert_eq!(WEBGL2_MAX_TEXTURE_DIMENSION_3D, 256);
}

/// [`VOLUME_GRID_CELLS`] and `rustdar_radar::voxel`'s named shapes are two
/// hand-maintained copies of the same three triples, in two crates.
///
/// The split is forced, not accidental: only *this* crate has a `build.rs`
/// emitting `mobile`, so only this crate can pick the middle arm — while
/// the grid is *built* in `rustdar-radar`, which therefore has to name all
/// three as plain constants and let a caller choose. `voxel::default_shape`
/// says as much and deliberately cannot return the mobile one.
///
/// Two copies that agree today is exactly the shape of the
/// `needs_whole_volume` / `RenderInput::extract` divergence this campaign
/// already paid for once, where the copies were "obviously" the same until
/// one of them was not. They agree; this is what keeps them agreeing, and
/// it checks **all three** arms rather than only the one this target
/// compiles, because the arm a host build skips is the one nothing else
/// would catch.
#[test]
fn the_grid_dimensions_match_the_shapes_rustdar_radar_names() {
    use rustdar_radar::voxel::{DESKTOP_SHAPE, LUT_LEN, MOBILE_SHAPE, VoxelShape, WASM_SHAPE};

    let triple = |s: VoxelShape| [s.nx as u32, s.ny as u32, s.nz as u32];

    // **All three arms, unconditionally.** The first version of this test
    // bound only the arm the running target compiled, which left two of
    // the three free to drift — a reviewer changed the wasm triple to
    // `[160, 160, 80]` and the entire workspace suite passed 1507/0 with
    // the wasm `--all-targets` check exiting 0. Both sides are now named
    // constants, so both sides are reachable from any host.
    assert_eq!(WASM_VOLUME_GRID_CELLS, triple(WASM_SHAPE));
    assert_eq!(MOBILE_VOLUME_GRID_CELLS, triple(MOBILE_SHAPE));
    assert_eq!(DESKTOP_VOLUME_GRID_CELLS, triple(DESKTOP_SHAPE));

    // Pinned literals as well as the binding, so that editing *both* sides
    // in step — the one change the comparison above cannot see — still has
    // to be deliberate.
    assert_eq!(WASM_VOLUME_GRID_CELLS, [128, 128, 64]);
    assert_eq!(MOBILE_VOLUME_GRID_CELLS, [192, 192, 96]);
    assert_eq!(DESKTOP_VOLUME_GRID_CELLS, [256, 256, 128]);

    // And that this target's cascade selected the matching one. This half
    // *is* cfg-gated, because the cascade is the one thing here that no
    // other target can check on its behalf.
    #[cfg(target_arch = "wasm32")]
    assert_eq!(VOLUME_GRID_CELLS, WASM_VOLUME_GRID_CELLS);
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    assert_eq!(VOLUME_GRID_CELLS, MOBILE_VOLUME_GRID_CELLS);
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    assert_eq!(VOLUME_GRID_CELLS, DESKTOP_VOLUME_GRID_CELLS);

    // Every axis must clear the WebGL2 floor on **every** arm, not just
    // this one — that bound is the reason the triples are what they are,
    // and it was previously checked on one arm out of three.
    for cells in [
        WASM_VOLUME_GRID_CELLS,
        MOBILE_VOLUME_GRID_CELLS,
        DESKTOP_VOLUME_GRID_CELLS,
    ] {
        for axis in cells {
            assert!(
                (1..=WEBGL2_MAX_TEXTURE_DIMENSION_3D).contains(&axis),
                "{cells:?}"
            );
        }
    }

    // The table travels *inside* the grid, so its size is one number in
    // two places too.
    assert_eq!(VOLUME_LUT_BYTES, LUT_LEN);
}

/// The pane mirror's ceiling is the cap squared, four bytes a texel — and the
/// cap is the one the renderer actually applies.
///
/// Two numbers in two crates that have to agree: `MIRROR_MAX_SIDE` is what
/// `mirror_size_for` halves the frame down to, and `VOLUME_MIRROR_BYTES_MAX` is
/// what the budget prose claims that costs. Spelling the product here means the
/// documented figure cannot drift from the enforced cap — which is the failure
/// mode a budget written as a literal always has.
///
/// The lower bound is the real content of the assertion: 16 MiB is a large
/// single allocation, so a future cap raise has to come past this line rather
/// than land as a silently bigger texture.
#[test]
fn the_pane_mirrors_ceiling_is_the_cap_it_is_actually_halved_to() {
    let side = crate::egui_renderer::MIRROR_MAX_SIDE as usize;
    assert_eq!(
        VOLUME_MIRROR_BYTES_MAX,
        side * side * 4,
        "the budget figure is not the cap squared at four bytes a texel",
    );
    assert_eq!(
        VOLUME_MIRROR_BYTES_MAX,
        16 * 1024 * 1024,
        "the mirror's worst case moved. It is one allocation for the whole \
         application, so a change here is a change to the application's \
         floor-on memory, not to a per-pane cost.",
    );

    // The halving is the only reduction that leaves egui's geometry alone —
    // `screen_size_in_points` is `size_in_pixels / pixels_per_point`, so both
    // must move together. A cap applied to one and not the other would scale
    // the frame's vertices instead of its sampling rate.
    let (size, scale) = crate::egui_renderer::mirror_size_for([3840, 2160], 2.0);
    assert_eq!((size, scale), ([1920, 1080], 1.0), "a 4K frame halves once");
    let (size, scale) = crate::egui_renderer::mirror_size_for([1920, 1080], 1.5);
    assert_eq!(
        (size, scale),
        ([1920, 1080], 1.5),
        "a frame already under the cap is mirrored at its own size",
    );
    let (size, _) = crate::egui_renderer::mirror_size_for([8192, 8192], 1.0);
    assert!(
        size[0].max(size[1]) <= crate::egui_renderer::MIRROR_MAX_SIDE
            && size[0] * size[1] * 4 <= VOLUME_MIRROR_BYTES_MAX as u32,
        "a frame far over the cap must halve until it fits, got {size:?}",
    );
}
