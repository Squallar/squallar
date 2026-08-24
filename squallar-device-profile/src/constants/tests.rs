use super::*;
use crate::budget::{self, Budgets, DeviceProfile};
use squallar_radar::types::IMAGE_SIZE;
use squallar_radar::voxel::VoxelShape;
use squallar_radar::xsect::{NATIVE_SECTION_WIDTH, WASM_SECTION_WIDTH};

/// Every device class this workspace builds for, exactly once.
fn profiles() -> [DeviceProfile; 3] {
    crate::budget::BudgetLimits::SHIPPED.map(|limits| DeviceProfile {
        limits,
        platform: if limits.name == "wasm32" {
            crate::budget::Platform::Web
        } else {
            crate::budget::Platform::Native
        },
        ..DeviceProfile::for_target()
    })
}

/// What [`profiles`] resolve to.
fn arms() -> [Budgets; 3] {
    profiles().map(|profile| budget::resolve(&profile))
}

/// The section raster is `SECTION_WIDTH` by half of it.
#[test]
fn the_section_raster_is_its_width_by_half_of_it() {
    let compiled = if cfg!(target_arch = "wasm32") {
        WASM_SECTION_WIDTH
    } else {
        NATIVE_SECTION_WIDTH
    };
    assert_eq!(
        (
            squallar_radar::xsect::SECTION_WIDTH,
            squallar_radar::xsect::SECTION_HEIGHT
        ),
        (compiled, compiled / 2),
        "the section raster is no longer SECTION_WIDTH by half of it, so the \
         per-arm reconstruction in `Budgets::section_frame_bytes` no longer \
         describes it",
    );
}

/// **One loop, on the worst device this target admits, gets the whole of
/// [`LOOP_SPAN_BUDGET_SECS`] — at the fastest radar there is.**
#[test]
fn one_loop_at_the_floor_gets_the_whole_span_budget() {
    for arm in arms() {
        let total = arm.textured_frames() * arm.loop_frame_bytes();
        assert_eq!(
            total,
            arm.loop_pool_floor_bytes,
            "{}: {} textured frames x {}^2 x 4B = {} MiB against a {} MiB floor \
             — a single loop on this target no longer gets exactly the span it \
             is budgeted",
            arm.name,
            arm.textured_frames(),
            arm.loop_image_side_px,
            total / (1024 * 1024),
            arm.loop_pool_floor_bytes / (1024 * 1024),
        );
        assert!(arm.textured_frames() * arm.section_frame_bytes() <= arm.loop_pool_floor_bytes);
    }
}

/// **The floor seats a full screen of loops without blanking one.**
#[test]
fn the_floor_seats_every_pane_without_blanking_one() {
    for arm in arms() {
        let needed = arm.max_panes * MIN_LOOP_FRAMES_PER_PANE * arm.loop_frame_bytes();
        assert!(
            needed <= arm.loop_pool_floor_bytes,
            "{}: {} panes x {MIN_LOOP_FRAMES_PER_PANE} frames x {} MiB = {} MiB, \
             over the {} MiB floor — a full screen of loops would be cut below \
             the minimum and one of them would blank",
            arm.name,
            arm.max_panes,
            arm.loop_frame_bytes() / (1024 * 1024),
            needed / (1024 * 1024),
            arm.loop_pool_floor_bytes / (1024 * 1024),
        );
    }
}

/// The bounds are a pair, and the floor is the one that wins.
#[test]
fn every_pool_ceiling_is_at_least_its_own_floor() {
    for arm in arms() {
        assert!(
            arm.loop_pool_floor_bytes <= arm.loop_pool_ceiling_bytes,
            "{}: a {} MiB floor above a {} MiB ceiling is a `clamp` that panics",
            arm.name,
            arm.loop_pool_floor_bytes / (1024 * 1024),
            arm.loop_pool_ceiling_bytes / (1024 * 1024),
        );
    }
}

/// A section loop can never be the binding case.
#[test]
fn a_section_loop_frame_is_half_a_plan_view_one() {
    for arm in arms() {
        assert_eq!(
            arm.section_frame_bytes() * 2,
            arm.loop_frame_bytes(),
            "{}: a section loop frame is no longer half a plan-view one, so the \
             section rows of the LOOP_TEXTURE_BUDGET_BYTES table are wrong",
            arm.name,
        );
    }
}

/// The whole application's GPU texture memory, against a ceiling.
#[test]
fn the_whole_application_fits_its_gpu_ceiling() {
    for arm in arms() {
        let total = arm.app_texture_bytes();
        assert!(
            total <= arm.app_texture_ceiling_bytes,
            "{}: a {} MiB loop pool + a {} MiB volume-store floor + {} panes x \
             {} MiB of raymarch offscreen = {} MiB, over the {} MiB \
             whole-application ceiling",
            arm.name,
            arm.loop_pool_ceiling_bytes / (1024 * 1024),
            arm.volume_loop_bytes() / (1024 * 1024),
            arm.max_panes,
            arm.offscreen_bytes / (1024 * 1024),
            total / (1024 * 1024),
            arm.app_texture_ceiling_bytes / (1024 * 1024),
        );
    }
}

/// The whole-application ceiling is snug: a ceiling several times the real
/// figure passes the check above while admitting a silent doubling.
#[test]
fn the_app_ceiling_is_not_slack_enough_to_hide_a_doubling() {
    for arm in arms() {
        let total = arm.app_texture_bytes();
        assert!(
            arm.app_texture_ceiling_bytes * 4 <= total * 5,
            "{}: the {} MiB ceiling is more than 1.25x the {} MiB it bounds, so \
             a term inside it could double unnoticed",
            arm.name,
            arm.app_texture_ceiling_bytes / (1024 * 1024),
            total / (1024 * 1024),
        );
    }
}

/// **What a loop of a given wall clock costs in frames, measured.**
const MEASURED_PEAK_LOOP_FRAMES: [(usize, usize); 7] = [
    (30 * 60, 10),
    (45 * 60, 14),
    (60 * 60, 18),
    (75 * 60, 23),
    (90 * 60, 27),
    (120 * 60, 36),
    (150 * 60, 44),
];

/// Frames [`MEASURED_PEAK_LOOP_FRAMES`] says a window of `secs` costs.
fn peak_frames(secs: usize) -> usize {
    MEASURED_PEAK_LOOP_FRAMES
        .into_iter()
        .find_map(|(window, frames)| (window == secs).then_some(frames))
        .unwrap_or_else(|| {
            panic!(
                "{secs} s is not a window the campaign measured — a span budget \
                 has to be a row of MEASURED_PEAK_LOOP_FRAMES, because the frame \
                 count it costs is a fact about radars rather than arithmetic on \
                 a median"
            )
        })
}

/// **The span budget is priced at the fastest radar, and the render budget is
/// that price.**
#[test]
fn the_render_budget_is_the_span_priced_at_the_fastest_radar() {
    for arm in arms() {
        assert_eq!(
            arm.loop_render_budget,
            peak_frames(arm.loop_span_secs),
            "{}: a {} min loop costs {} frames at the fastest measured site, not {}",
            arm.name,
            arm.loop_span_secs / 60,
            peak_frames(arm.loop_span_secs),
            arm.loop_render_budget,
        );
    }
}

/// **Each arm's span is the longest window its GPU ceiling can pay for.**
#[test]
fn the_span_budget_is_the_longest_the_ceiling_can_pay_for() {
    for arm in arms() {
        let fixed = arm.loop_pool_ceiling_bytes + arm.max_panes * arm.offscreen_bytes;
        let headroom = arm
            .app_texture_ceiling_bytes
            .checked_sub(fixed)
            .unwrap_or_else(|| panic!("{}: the ceiling no longer covers the pool", arm.name));
        let affordable = headroom / arm.loop_frame_bytes();
        assert!(
            arm.loop_render_budget <= affordable,
            "{}: a {} min loop wants {} frames and the app ceiling leaves room for {}",
            arm.name,
            arm.loop_span_secs / 60,
            arm.loop_render_budget,
            affordable,
        );
        let longer = MEASURED_PEAK_LOOP_FRAMES
            .into_iter()
            .find(|(window, _)| *window > arm.loop_span_secs)
            .expect("the campaign measured a window longer than every shipped span");
        assert!(
            longer.1 > affordable,
            "{}: the next measured window up ({} min) needs {} frames and the \
             ceiling affords {} — the span budget is short of what this arm can \
             pay for",
            arm.name,
            longer.0 / 60,
            longer.1,
            affordable,
        );
    }
}

/// The 3D loop's pacing cap is a real cap.
#[test]
fn the_volume_build_cap_paces_rather_than_stalls() {
    const { assert!(MAX_LOOP_VOLUME_BUILDS_PER_FRAME >= 1) };
    for arm in arms() {
        assert!(
            MAX_LOOP_VOLUME_BUILDS_PER_FRAME <= arm.concurrent_renders,
            "{}: the per-frame build cap ({MAX_LOOP_VOLUME_BUILDS_PER_FRAME}) is \
             above the concurrent render budget ({}), so it caps nothing",
            arm.name,
            arm.concurrent_renders,
        );
    }
}

/// The teardown slice paces rather than stalls: a real slice of a frame, and a
/// small one.
#[test]
fn the_teardown_slice_paces_rather_than_stalls() {
    const FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);
    const { assert!(DEFERRED_DROP_BUDGET_PER_FRAME.as_micros() > 0) };
    assert!(
        DEFERRED_DROP_BUDGET_PER_FRAME * 8 <= FRAME,
        "the teardown slice ({:?}) is more than the eighth of a 16.7 ms frame \
         its own doc claims; it is overhead against drawing, spent on work \
         nothing is waiting for, and it already overruns by one whole payload",
        DEFERRED_DROP_BUDGET_PER_FRAME,
    );
}

/// The pacing cap is a real cap: at least one cut per pass, and fewer than the
/// concurrent render budget on every arm.
#[test]
fn the_section_cut_cap_paces_rather_than_stalls() {
    const { assert!(MAX_LOOP_SECTION_CUTS_PER_FRAME >= 1) };
    for arm in arms() {
        assert!(
            MAX_LOOP_SECTION_CUTS_PER_FRAME <= arm.concurrent_renders,
            "{}: the per-frame cut cap ({MAX_LOOP_SECTION_CUTS_PER_FRAME}) is \
             above the concurrent render budget ({}), so it caps nothing",
            arm.name,
            arm.concurrent_renders,
        );
    }
}

/// The budget is snug: a ceiling several times the real figure would pass the
/// check above while permitting a silent doubling.
#[test]
fn the_budget_is_not_slack_enough_to_hide_a_doubling() {
    for arm in arms() {
        let total = arm.textured_frames() * arm.loop_frame_bytes();
        assert!(
            total * 2 > arm.loop_pool_floor_bytes,
            "{}: floor {} MiB is more than twice the {} MiB one full loop costs \
                 — it would not catch a regression, and it would mean the floor \
                 is no longer 'what one pane used to get'",
            arm.name,
            arm.loop_pool_floor_bytes / (1024 * 1024),
            total / (1024 * 1024),
        );
    }
}

/// The eviction budget bounds memory, so it must be the smaller of the two: if
/// it exceeded the frame cap, every held frame would stay textured.
#[test]
fn the_render_budget_is_what_bounds_the_textured_frames() {
    for arm in arms() {
        assert_eq!(
            arm.textured_frames(),
            arm.loop_render_budget,
            "{}",
            arm.name
        );
        assert!(arm.loop_render_budget > 0, "{}", arm.name);
        assert!(arm.concurrent_renders > 0, "{}", arm.name);
    }
}

/// The literals behind the tables in the two budget doc comments.
#[test]
fn the_documented_per_class_figures_are_what_the_arms_actually_say() {
    let expected = [
        // name, base, long range, loop, section width, concurrent, held,
        // textured, pool floor MiB, pool ceiling MiB, volume budget B
        (
            "wasm32",
            2048,
            2048,
            1024,
            1024,
            1,
            14,
            14,
            56,
            192,
            6 * 1024 * 1024,
        ),
        (
            "mobile",
            2048,
            4096,
            2048,
            2048,
            3,
            20,
            18,
            288,
            640,
            20 * 1024 * 1024,
        ),
        (
            "desktop",
            2048,
            4096,
            2048,
            2048,
            6,
            60,
            36,
            576,
            3072,
            48 * 1024 * 1024,
        ),
    ];
    for (
        arm,
        (
            name,
            image,
            long_range,
            loop_image,
            section_width,
            concurrent,
            held,
            textured,
            floor_mib,
            ceiling_mib,
            volume,
        ),
    ) in arms().into_iter().zip(expected)
    {
        assert_eq!(arm.name, name);
        assert_eq!(arm.image_side_px, image, "{name} image size");
        assert_eq!(
            arm.long_range_image_side_px, long_range,
            "{name} long-range image size"
        );
        assert_eq!(arm.loop_image_side_px, loop_image, "{name} loop image size");
        assert_eq!(arm.section_width_px, section_width, "{name} section width");
        // The three sides a plan-view raster can have on this class, ordered.
        assert!(
            arm.loop_image_side_px <= arm.image_side_px,
            "{name}: a loop frame is larger than a still one"
        );
        assert!(
            arm.image_side_px <= arm.long_range_image_side_px,
            "{name}: the long-range ceiling is under the base size"
        );
        assert_eq!(arm.concurrent_renders, concurrent, "{name} renders");
        assert_eq!(arm.loop_frames_held, held, "{name} held frames");
        assert_eq!(arm.loop_render_budget, textured, "{name} render budget");
        assert_eq!(
            arm.loop_pool_floor_bytes,
            floor_mib * 1024 * 1024,
            "{name} pool floor"
        );
        assert_eq!(
            arm.loop_pool_ceiling_bytes,
            ceiling_mib * 1024 * 1024,
            "{name} pool ceiling"
        );
        assert_eq!(arm.volume_texture_bytes, volume, "{name} volume budget");
    }
}

/// This target's cascades all selected the *same* arm as each other.
#[test]
fn every_cascade_in_this_file_selected_the_same_arm() {
    #[cfg(target_arch = "wasm32")]
    let arm = &arms()[0];
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    let arm = &arms()[1];
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    let arm = &arms()[2];

    assert_eq!(IMAGE_SIZE, arm.image_side_px, "{}", arm.name);
    assert_eq!(
        MAX_CONCURRENT_RENDERS, arm.concurrent_renders,
        "{}",
        arm.name
    );
    assert_eq!(MAX_LOOP_FRAMES, arm.loop_frames_held, "{}", arm.name);
    assert_eq!(
        MAX_LOOP_RENDER_BUDGET, arm.loop_render_budget,
        "{}",
        arm.name
    );
    assert_eq!(
        LOOP_POOL_FLOOR_BYTES, arm.loop_pool_floor_bytes,
        "{}",
        arm.name
    );
    assert_eq!(
        LOOP_POOL_CEILING_BYTES, arm.loop_pool_ceiling_bytes,
        "{}",
        arm.name
    );
    assert_eq!(VOLUME_GRID_CELLS, arm.grid_cells, "{}", arm.name);
    assert_eq!(
        VOLUME_TEXTURE_BUDGET_BYTES, arm.volume_texture_bytes,
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
        // The two raster-size cascades; `IMAGE_SIZE` is `squallar_radar`'s.
        "LONG_RANGE_IMAGE_SIZE",
        "LOOP_IMAGE_SIZE",
        "MAX_CONCURRENT_RENDERS",
        // The loop budget and what it costs.
        "LOOP_SPAN_BUDGET_SECS",
        "MAX_LOOP_RENDER_BUDGET",
        "MAX_LOOP_FRAMES",
        // The pool's two bounds.
        "LOOP_POOL_FLOOR_BYTES",
        "LOOP_POOL_CEILING_BYTES",
        "VOLUME_GRID_CELLS",
        "VOLUME_TEXTURE_BUDGET_BYTES",
        // Also covered by
        // `each_offscreen_budget_arm_selects_its_own_classs_constant`; that
        // test checks one cascade, this one that no cascade is missing.
        "VOLUME_OFFSCREEN_BUDGET_BYTES",
        // The 3D loop's cascade.
        "APP_TEXTURE_BUDGET_BYTES",
        // Three arms: how much supersampling a 3D floor is worth per target.
        "VOLUME_MIRROR_BYTES_MAX",
    ];

    // Cascades that still spell their arms as literals, and so cannot be
    // checked here. Empty today; the mechanism stays for the next one to land.
    let exempt: [&str; 0] = [];

    let found = wasm_gated_constants(code);
    let mut accounted: Vec<&str> = covered.iter().chain(exempt.iter()).copied().collect();
    accounted.sort_unstable();
    assert_eq!(
        found, accounted,
        "the set of `cfg`-selected constants in this file has changed. A \
             new one has to be lifted into named arms and listed in `covered`, \
             or listed in `exempt` with the reason it cannot be."
    );

    // An exemption has to still *be* one: a cascade lifted but left in
    // `exempt` looks accounted for while its arms go unchecked. A lifted arm's
    // right-hand side is a bare `SCREAMING_CASE` name; a literal never is.
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

/// The reference pane fits this target's offscreen budget **at its own
/// quality ceiling**, i.e. without being degraded to get there.
#[test]
fn the_reference_pane_fits_the_target_offscreen_budget_undegraded() {
    let fitted = crate::quality::reference_offscreen();
    assert!(
        fitted.bytes() <= VOLUME_OFFSCREEN_BUDGET_BYTES,
        "a {:?} offscreen is {} B, over the {VOLUME_OFFSCREEN_BUDGET_BYTES} \
             B budget",
        fitted.size,
        fitted.bytes(),
    );
    assert_eq!(
        fitted.quality,
        crate::quality::PLATFORM_CEILING,
        "the {VOLUME_OFFSCREEN_REFERENCE_PANE_PX:?} reference pane cannot be \
             rendered at this target's own quality ceiling within a \
             {VOLUME_OFFSCREEN_BUDGET_BYTES} B budget, so the ceiling describes \
             a quality the budget never lets anything select"
    );
}

/// And the offscreen budget is snug, exactly as the other two are.
#[test]
fn the_offscreen_budget_is_not_slack_enough_to_hide_a_doubling() {
    let total = crate::quality::reference_offscreen().bytes();
    assert!(
        total * 2 > VOLUME_OFFSCREEN_BUDGET_BYTES,
        "budget {VOLUME_OFFSCREEN_BUDGET_BYTES} B is more than twice the \
             actual {total} B — it would not catch a doubled reference pane"
    );
}

/// Both offscreen budget checks, on **all three** arms rather than the one
/// this build compiled.
#[test]
fn every_offscreen_budget_arm_pays_for_its_own_reference_pane() {
    use crate::quality::{
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

/// [`VOLUME_GRID_CELLS`] and `squallar_radar::voxel`'s named shapes are two
/// copies of the same three triples, in two crates.
#[test]
fn the_grid_dimensions_match_the_shapes_squallar_radar_names() {
    use squallar_radar::voxel::{DESKTOP_SHAPE, LUT_LEN, MOBILE_SHAPE, VoxelShape, WASM_SHAPE};

    let triple = |s: VoxelShape| [s.nx as u32, s.ny as u32, s.nz as u32];

    // All three arms, unconditionally: both sides are named constants, so
    // both are reachable from any host.
    assert_eq!(WASM_VOLUME_GRID_CELLS, triple(WASM_SHAPE));
    assert_eq!(MOBILE_VOLUME_GRID_CELLS, triple(MOBILE_SHAPE));
    assert_eq!(DESKTOP_VOLUME_GRID_CELLS, triple(DESKTOP_SHAPE));

    // Pinned literals as well as the binding, so editing both sides in step
    // still has to be deliberate.
    assert_eq!(WASM_VOLUME_GRID_CELLS, [128, 128, 64]);
    assert_eq!(MOBILE_VOLUME_GRID_CELLS, [192, 192, 96]);
    assert_eq!(DESKTOP_VOLUME_GRID_CELLS, [256, 256, 128]);

    // This target's cascade selected the matching one. cfg-gated, because no
    // other target can check the cascade on its behalf.
    #[cfg(target_arch = "wasm32")]
    assert_eq!(VOLUME_GRID_CELLS, WASM_VOLUME_GRID_CELLS);
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    assert_eq!(VOLUME_GRID_CELLS, MOBILE_VOLUME_GRID_CELLS);
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    assert_eq!(VOLUME_GRID_CELLS, DESKTOP_VOLUME_GRID_CELLS);

    // Every axis must clear the WebGL2 floor on every arm.
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

    assert_eq!(VOLUME_LUT_BYTES, LUT_LEN);
}

/// The shape the frontend **asks** `build_voxels` for is the one this target's
/// budgets were computed from.
#[test]
fn the_requested_shape_is_the_one_this_targets_budget_was_computed_for() {
    use squallar_radar::voxel::{DESKTOP_SHAPE, MOBILE_SHAPE, WASM_SHAPE};

    // The axis order asserted rather than trusted: every real triple has
    // `nx == ny`, so a transposition would be invisible on all three.
    assert_eq!(
        VoxelShape::of_cells([1, 2, 3]),
        VoxelShape {
            nx: 1,
            ny: 2,
            nz: 3
        },
        "VOLUME_GRID_CELLS is x, y, z",
    );

    assert_eq!(VoxelShape::of_cells(WASM_VOLUME_GRID_CELLS), WASM_SHAPE);
    assert_eq!(VoxelShape::of_cells(MOBILE_VOLUME_GRID_CELLS), MOBILE_SHAPE);
    assert_eq!(
        VoxelShape::of_cells(DESKTOP_VOLUME_GRID_CELLS),
        DESKTOP_SHAPE
    );

    // The shape a device at the guarantee is asked for is this target's own
    // budget triple, unchanged — the no-regression claim.
    #[cfg(target_arch = "wasm32")]
    assert_eq!(
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        squallar_radar::voxel::shape_for_budget(WASM_SHAPE, 256),
    );
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    assert_eq!(
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        squallar_radar::voxel::shape_for_budget(MOBILE_SHAPE, 256),
    );
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    assert_eq!(
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        DESKTOP_SHAPE
    );

    assert_eq!(
        VOLUME_GRID_FLOOR_SHAPE,
        volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D),
        "the floor shape the const assert guards has to be the one a device \
         at the guarantee is actually asked for",
    );
}

/// The limits a real adapter might report, which every sweep below runs.
const REPORTED_LIMITS: [u32; 5] = [256, 512, 704, 1024, 2048];

/// The three budget triples, whatever this target's cascade selected.
const ALL_ARMS: [(&str, [u32; 3]); 3] = [
    ("wasm", WASM_VOLUME_GRID_CELLS),
    ("mobile", MOBILE_VOLUME_GRID_CELLS),
    ("desktop", DESKTOP_VOLUME_GRID_CELLS),
];

/// A device is never asked for an axis it did not say it could hold.
#[test]
fn every_axis_stays_within_the_limit_the_adapter_reported() {
    for (name, budget) in ALL_ARMS {
        for limit in REPORTED_LIMITS {
            let shape = squallar_radar::voxel::shape_for_budget(
                VoxelShape::of_cells(budget),
                limit as usize,
            );
            for (axis, n) in [("nx", shape.nx), ("ny", shape.ny), ("nz", shape.nz)] {
                assert!(
                    n as u32 <= limit,
                    "{name} on a {limit}-reporting device: {axis} is {n}, \
                     which that device cannot allocate — and the failure \
                     would be a validation error inside a callback, where \
                     there is no Result to check",
                );
            }
        }
    }
}

/// The device guarantee.
#[test]
fn a_shape_derived_for_a_device_at_the_guarantee_stays_within_it() {
    for (name, budget) in ALL_ARMS {
        let shape = squallar_radar::voxel::shape_for_budget(
            VoxelShape::of_cells(budget),
            WEBGL2_MAX_TEXTURE_DIMENSION_3D as usize,
        );
        for (axis, n) in [("nx", shape.nx), ("ny", shape.ny), ("nz", shape.nz)] {
            assert!(
                n >= 1 && n as u32 <= WEBGL2_MAX_TEXTURE_DIMENSION_3D,
                "{name}: {axis} is {n}, outside the 3D texture size WebGL2 \
                 guarantees, so a phone browser reporting exactly the \
                 guarantee could not allocate it",
            );
        }
    }
}

/// The static pane textures the app ceiling does **not** count.
#[test]
fn the_static_render_textures_are_named_even_though_the_ceiling_omits_them() {
    let expected = [
        ("wasm32", 16 * 1024 * 1024, 96),
        ("mobile", 64 * 1024 * 1024, 256),
        ("desktop", 256 * 1024 * 1024, 1536),
    ];
    for (arm, (name, frame, worst_mib)) in arms().into_iter().zip(expected) {
        assert_eq!(arm.name, name);
        assert_eq!(arm.static_frame_bytes(), frame, "{name} static frame");
        assert_eq!(
            arm.max_panes * arm.static_frame_bytes() / (1024 * 1024),
            worst_mib,
            "{name} worst-case static textures",
        );
    }
}

/// The closed set a finished raster's length is read against, and the
/// lengths that are not in it.
#[test]
fn a_rasters_side_is_read_back_from_its_length_against_a_closed_set() {
    for side in [
        LOOP_IMAGE_SIZE,
        squallar_radar::types::IMAGE_SIZE,
        LONG_RANGE_IMAGE_SIZE,
    ] {
        assert_eq!(
            raster_side_from_rgba_len(side * side * 4),
            Some(side),
            "{side} px is a size this build renders",
        );
    }
    for (len, why) in [
        (0, "an empty buffer"),
        (1, "a single byte"),
        (3, "a length that is not even a whole pixel"),
        (512 * 512 * 4, "a square raster of a size nothing renders"),
        (
            LONG_RANGE_IMAGE_SIZE * LONG_RANGE_IMAGE_SIZE * 4 - 4,
            "one pixel short of the long-range raster",
        ),
        (
            LONG_RANGE_IMAGE_SIZE * LONG_RANGE_IMAGE_SIZE * 4 + 4,
            "one pixel over it",
        ),
        (
            squallar_radar::xsect::SECTION_WIDTH * squallar_radar::xsect::SECTION_HEIGHT * 4,
            "a cross-section raster, which is not square and not a plan view",
        ),
    ] {
        assert_eq!(raster_side_from_rgba_len(len), None, "{why}");
    }
}

/// The raster ceiling is the device's own answer, bounded by a measurement.
#[test]
fn the_raster_ceiling_follows_the_device_and_never_falls_below_what_shipped() {
    for arm in arms() {
        let floor = arm.long_range_image_side_px;
        for reports in [32768u32, 16384, 8192, 4096, 2048] {
            let got = arm.raster_side_for_adapter(reports);
            let reported = reports as usize;
            let why = format!("{}, a device reporting {reports}", arm.name);

            assert!(got <= reported, "{why}: {got} px over {reported} px");
            assert!(
                got <= (reported / 2).max(floor.min(reported)),
                "{why}: {got} px is more than half of what was reported",
            );
            assert!(
                got <= arm.raster_side_ceiling_px,
                "{why}: {got} px over the {} px ceiling",
                arm.raster_side_ceiling_px,
            );
            assert!(
                got >= floor.min(reported),
                "{why}: {got} px is under the {} px this build already draws",
                floor.min(reported),
            );
        }

        assert!(arm.raster_side_for_adapter(0) <= 1);
        assert!(arm.raster_side_for_adapter(1) <= 1);

        // The two classes with a pinned ceiling must be unmoved by any adapter.
        if arm.raster_side_ceiling_px == arm.long_range_image_side_px {
            assert_eq!(
                arm.raster_side_for_adapter(32768),
                arm.long_range_image_side_px,
                "{} has a pinned ceiling and must not move off it",
                arm.name,
            );
        }
    }

    // On the class whose ceiling was raised, two devices that differ must not
    // be given the same answer.
    let desktop = budget::resolve(&DeviceProfile {
        limits: crate::budget::BudgetLimits::DESKTOP,
        ..DeviceProfile::for_target()
    });
    assert!(
        desktop.raster_side_for_adapter(2048) < desktop.raster_side_for_adapter(32768),
        "a device reporting the GLES floor and one reporting 32768 were \
     offered the same ceiling",
    );
    assert_eq!(desktop.raster_side_for_adapter(32768), 8192);
    assert_eq!(desktop.raster_side_for_adapter(4096), 4096);
}
