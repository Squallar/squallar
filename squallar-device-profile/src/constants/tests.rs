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
/// The drop budget prices the thread its drain runs on. Desktop's drain is a
/// dead letter (discards ride the `rd-free` lane) so its 2 ms is free to be
/// generous; wasm's drain runs on the page thread, whose whole service bar is
/// 4 ms — a wasm arm as generous as desktop's is half that bar spent on
/// teardown. Strictly less, so the tightening cannot silently revert.
#[test]
fn the_wasm_drop_budget_is_tighter_than_desktops() {
    assert!(
        WASM_DEFERRED_DROP_BUDGET_PER_FRAME < DESKTOP_DEFERRED_DROP_BUDGET_PER_FRAME,
        "the wasm drop budget ({:?}) is not below desktop's ({:?}), so the \
         page thread pays a native-sized teardown allowance out of its 4 ms bar",
        WASM_DEFERRED_DROP_BUDGET_PER_FRAME,
        DESKTOP_DEFERRED_DROP_BUDGET_PER_FRAME,
    );
    assert!(
        MOBILE_DEFERRED_DROP_BUDGET_PER_FRAME < DESKTOP_DEFERRED_DROP_BUDGET_PER_FRAME,
        "the mobile drop budget ({:?}) is not below desktop's ({:?})",
        MOBILE_DEFERRED_DROP_BUDGET_PER_FRAME,
        DESKTOP_DEFERRED_DROP_BUDGET_PER_FRAME,
    );
}

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
        // The page-thread teardown allowance; wasm's arm is pinned below
        // desktop's by `the_wasm_drop_budget_is_tighter_than_desktops`.
        "DEFERRED_DROP_BUDGET_PER_FRAME",
        // The building geometry row: one number on every arm, pinned until a
        // second machine is measured.
        "PRISM_GEOMETRY_BYTES",
        // The loop cache's three: how many decoded bytes may be committed at
        // once, how many compressed bytes may be held, and how far ahead of a
        // playhead a decoded volume is kept. Sized per arm against the
        // reproduced wasm freeze -- see the block above them.
        "LOOP_DECODED_CEILING_BYTES",
        "LOOP_ARCHIVE_CEILING_BYTES",
        "LOOP_DECODED_LOOKAHEAD_FRAMES",
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
        DESKTOP_PLATFORM_CEILING, GroundPass, MOBILE_PLATFORM_CEILING, WASM_PLATFORM_CEILING,
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
        let fitted = ceiling.fit(VOLUME_OFFSCREEN_REFERENCE_PANE_PX, budget, GroundPass::Off);
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

/// The prism buffers the app ceiling does **not** count: buffers rather than
/// textures, one number on every arm, and the worst case named.
#[test]
fn the_prism_buffers_are_named_even_though_the_ceiling_omits_them() {
    let expected = [("wasm32", 96), ("mobile", 64), ("desktop", 96)];
    for (arm, (name, worst_mib)) in arms().into_iter().zip(expected) {
        assert_eq!(arm.name, name);
        assert_eq!(
            arm.prism_vram_bytes,
            16 * 1024 * 1024,
            "{name}: the building geometry row is the one machine's 16 MiB \
             until a second machine is measured",
        );
        assert_eq!(
            arm.max_panes * arm.prism_vram_bytes / (1024 * 1024),
            worst_mib,
            "{name} worst-case prism buffers",
        );
        assert_eq!(
            arm.app_texture_bytes(),
            arm.loop_pool_ceiling_bytes
                + arm.volume_loop_bytes()
                + arm.max_panes * arm.offscreen_bytes,
            "{name}: the texture ceiling's sum gained a term; prism buffers \
             are not textures and are priced by `need`, not by this sum",
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

/// The desktop ceiling is the widest honest sweep's own need — a WSR-88D
/// surveillance cut at `TEXELS_PER_SAMPLE` — rounded up to its texture
/// doubling. **No pane size enters it, and none could:** a raster the side of
/// the default window, or of the largest canvas the browser rig measures at,
/// is under one texel per gate over ±460 km, so a pane-proportionate bound
/// would drop gates the moment the user zoomed past the raster's own scale.
/// The pane can always out-zoom the raster (walkers accepts zoom 26, where a
/// pane shows some 500 000 px/km), so the only floor that keeps every gate on
/// the glass at every reachable zoom is the data's.
#[test]
fn the_desktop_raster_ceiling_is_the_widest_sweeps_own_need_and_no_panes() {
    use squallar_radar::types::{
        SideBound, TEXELS_PER_SAMPLE, data_limited_side_px, plan_view_extent_km, raster_side,
        raster_side_px,
    };
    // 2.125 + 1832 × 0.25 km, the longest reach in this display.
    const SURVEILLANCE_REACH_KM: f64 = 460.125;
    const SUPER_RES_GATE_KM: f64 = 0.25;
    let extent_km = plan_view_extent_km(SURVEILLANCE_REACH_KM);
    let need = data_limited_side_px(extent_km, SUPER_RES_GATE_KM);
    let texels_per_gate = |side: usize| side as f64 / (2.0 * extent_km) * SUPER_RES_GATE_KM;

    // The ceiling clears the need, so at the ceiling the widest sweep draws
    // every gate at the texels the data asks for …
    let side = raster_side_px(extent_km, DESKTOP_RASTER_SIDE_CEILING, SUPER_RES_GATE_KM);
    assert!(
        texels_per_gate(side) >= TEXELS_PER_SAMPLE - 1e-9,
        "under a {DESKTOP_RASTER_SIDE_CEILING} px ceiling a surveillance cut draws {side} px, \
         {:.3} texels per 250 m gate against the {TEXELS_PER_SAMPLE} its data asks for",
        texels_per_gate(side),
    );
    // … and it binds nothing: the render is the data's own size, so the bytes
    // a desktop render costs are the sweep's, not the ceiling's.
    assert_eq!(
        side, need,
        "the desktop ceiling is what sized a surveillance cut, not its gates"
    );
    // And a readout can say which bound won without re-deriving either figure:
    // at the shipped ceiling the word is the data's, and at the rung below it
    // the machine's.
    assert_eq!(
        raster_side(extent_km, DESKTOP_RASTER_SIDE_CEILING, SUPER_RES_GATE_KM).bound,
        SideBound::Data,
        "a surveillance cut under the {DESKTOP_RASTER_SIDE_CEILING} px ceiling is bound by \
         its own gates, and the render has to be able to say so",
    );
    assert_eq!(
        raster_side(extent_km, DESKTOP_LONG_RANGE_IMAGE_SIZE, SUPER_RES_GATE_KM).bound,
        SideBound::Capacity,
        "a {DESKTOP_LONG_RANGE_IMAGE_SIZE} px device holds the same cut under its own need, \
         and the render has to name the device rather than the data",
    );
    // Within one doubling of the need: the data's number rounded to its
    // texture doubling, and not the 16384 the adapter rule would admit.
    assert_eq!(
        DESKTOP_RASTER_SIDE_CEILING,
        need.next_power_of_two(),
        "the desktop ceiling is no longer the surveillance cut's {need} px need rounded to \
         its doubling",
    );

    // Why no pane is the denominator: the default window's longer side and the
    // largest canvas the browser rig measures at (`run_tier2.sh`'s `huge` leg,
    // 2878 × 1651) both give a 250 m gate less than one texel over ±460 km.
    for (pane_side, what) in [
        (
            RENDER_WIDTH.max(RENDER_HEIGHT) as usize,
            "the default window",
        ),
        (2878, "the huge leg's canvas"),
    ] {
        assert!(
            texels_per_gate(pane_side) < 1.0,
            "{what} is {pane_side} px across, which is {:.3} texels per gate at the ring: a \
             pane-sized raster would not drop gates, so the pane could be the bound",
            texels_per_gate(pane_side),
        );
    }
}

/// **What a plan-view render costs on each arm, buffer by buffer** — the
/// denominator table, so a figure quoted anywhere can be checked against the
/// class it came from and no two arms can be added.
///
/// Every row is the class's own `raster_side_ceiling_px`, which is what
/// `Budgets::static_frame_cost` prices a still pane at. The two columns are
/// two memories and are never summed across the boundary: the GPU one is the
/// `Rgba8` texture, the host one is the raster, the value grid and the claim
/// buffer that painted them, all three alive together inside
/// `RenderBuffers::into_output`.
///
/// | arm | ceiling | GPU | host peak |
/// |---|---:|---:|---:|
/// | wasm32 | 2048 | 16.00 MiB | 64.00 MiB |
/// | mobile | 4096 | 64.00 MiB | 256.00 MiB |
/// | desktop | 8192 | 256.00 MiB | 1024.00 MiB |
///
/// The **promoted** web rung is the mobile row: a browser on a real driver
/// earns 4096 (`WASM_RASTER_SIDE_CEILING_PROMOTED`), so its render peak is
/// 256.00 MiB and not 64.00 MiB, and the two web figures are never merged.
///
/// A **loop** frame is a different denominator again and is never the static
/// one: `LOOP_IMAGE_SIZE` is 1024 on the web and `NATIVE_IMAGE_SIZE` = 2048 on
/// both native arms, so a native loop frame is 16.00 MiB of texture whatever
/// the class ceiling says.
#[test]
fn what_a_plan_view_render_costs_on_each_arm_buffer_by_buffer() {
    const MIB: usize = 1024 * 1024;
    let expected = [
        ("wasm32", 2048, 16 * MIB, 64 * MIB),
        ("mobile", 4096, 64 * MIB, 256 * MIB),
        ("desktop", 8192, 256 * MIB, 1024 * MIB),
    ];
    for (arm, (name, side, gpu, host_peak)) in arms().into_iter().zip(expected) {
        assert_eq!(arm.name, name);
        assert_eq!(arm.raster_side_ceiling_px, side, "{name} ceiling");
        let cost = arm.static_frame_cost();
        assert_eq!(
            cost,
            plan_view_frame_cost(side),
            "{name}: the one statement"
        );
        assert_eq!(cost.gpu, gpu, "{name} GPU texture");
        assert_eq!(cost.host_peak(), host_peak, "{name} host peak");
        // The composition, term by term, against the buffers themselves. Not
        // one multiplier restated: each term is its own allocation's width,
        // and the held pair is what survives the render.
        let px = side * side;
        assert_eq!(cost.gpu, px * PLAN_VIEW_TEXEL_BYTES);
        assert_eq!(
            cost.host_held,
            px * (PLAN_VIEW_TEXEL_BYTES + PLAN_VIEW_VALUE_BYTES),
        );
        assert_eq!(cost.host_scratch, px * PLAN_VIEW_CELL_BYTES);
        assert_eq!(cost.host_held, raster_bytes(side), "the held half");
        // The scratch is not a rounding on the held bytes: the claim buffer
        // is eight bytes a pixel against the finished pair's eight, so the
        // peak is twice what is kept and four times the texture.
        assert_eq!(cost.host_peak(), 4 * cost.gpu);
        assert_eq!(cost.host_peak(), 2 * cost.host_held);
    }

    // The promoted web rung is the mobile row, and the loop frame is neither.
    assert_eq!(
        plan_view_frame_cost(WASM_RASTER_SIDE_CEILING_PROMOTED).host_peak(),
        256 * MIB,
    );
    assert_eq!(plan_view_frame_cost(WASM_LOOP_IMAGE_SIZE).gpu, 4 * MIB);
    assert_eq!(plan_view_frame_cost(DESKTOP_LOOP_IMAGE_SIZE).gpu, 16 * MIB);
    assert_eq!(DESKTOP_LOOP_IMAGE_SIZE, MOBILE_LOOP_IMAGE_SIZE);
}

/// **A cross-section frame is priced from its own buffers, not the plan
/// view's**, and the difference is real in both directions: it carries a
/// per-pixel status byte the plan view has no counterpart for, and it
/// allocates no claim buffer, so its host peak is its held bytes exactly.
#[test]
fn a_cross_section_frame_is_priced_from_its_own_three_buffers() {
    for arm in arms() {
        let cost = arm.section_frame_cost();
        let px = arm.section_width_px * (arm.section_width_px / 2);
        assert_eq!(cost.gpu, px * PLAN_VIEW_TEXEL_BYTES, "{}", arm.name);
        assert_eq!(
            cost.host_held,
            px * (PLAN_VIEW_TEXEL_BYTES + PLAN_VIEW_VALUE_BYTES + SECTION_STATUS_BYTES),
            "{}: image, values and status",
            arm.name,
        );
        assert_eq!(
            cost.host_scratch, 0,
            "{}: the section sampler allocates no claim buffer",
            arm.name,
        );
        assert_eq!(cost.host_peak(), cost.host_held, "{}", arm.name);
        assert_eq!(cost.gpu, arm.section_frame_bytes(), "{}", arm.name);
        // Nine bytes a pixel, not eight: a status code the plan view does
        // not keep. Restating it as a ratio would lose exactly that.
        assert_eq!(cost.host_held * 4, cost.gpu * 9, "{}", arm.name);
    }
}

// ---------------------------------------------------------------------------
// The polar frame price. Nothing in the tree calls `polar_frame_cost` outside
// this block, and the first test below is what says so.
// ---------------------------------------------------------------------------

/// The three shapes design §2.1 names for the families phase A migrates, and
/// the one HHC lands on. Observations of what those families produce — **not**
/// bounds; the bounds are `MAX_POLAR_RADIALS` and `MAX_POLAR_GATES`.
const SURVEILLANCE: (usize, usize) = (720, 1832);
const DOPPLER: (usize, usize) = (720, 1192);
const HHC: (usize, usize) = (360, 920);

fn shape(dims: (usize, usize), levels: usize, retained: bool) -> PolarFrameShape {
    PolarFrameShape {
        radials: dims.0,
        gates: dims.1,
        width_bytes: POLAR_CODE_R8_BYTES,
        sweeps: 1,
        mip_levels: levels,
        codes_retained: retained,
    }
}

/// **The mip chain the long way, in a second implementation** — a level list
/// built by repeated ceil-halving, each level's texels multiplied out and
/// summed. Shares no line with `chain_texels`: it materialises the extents
/// rather than accumulating, so a defect in the accumulator cannot hide here.
fn chain_the_long_way(radials: usize, gates: usize, levels: usize) -> usize {
    if radials == 0 || gates == 0 {
        return 0;
    }
    let mut extents = Vec::new();
    let (mut r, mut g) = (radials, gates);
    for _ in 0..levels {
        extents.push((r, g));
        r = if r > 1 {
            (r as f64 / 2.0).ceil() as usize
        } else {
            1
        };
        g = if g > 1 {
            (g as f64 / 2.0).ceil() as usize
        } else {
            1
        };
    }
    extents.into_iter().map(|(r, g)| r * g).sum()
}

/// **The mip chain as WebGPU would size it** — `max(1, floor(size / 2^level))`
/// — for the comparison that shows the ceil form cannot under-price.
fn chain_floor_halved(radials: usize, gates: usize, levels: usize) -> usize {
    (0..levels)
        .map(|l| (radials >> l).max(1) * (gates >> l).max(1))
        .sum()
}

/// **`chain_texels` against a second implementation of the same sum**, over
/// every shape the design names and a sweep of hostile ones, at every level
/// count from one to past the full chain.
///
/// Not against a recorded number: a pinned total proves the sum has not moved,
/// not that it was ever right. The long way builds the level list explicitly
/// and multiplies each level out.
#[test]
fn the_mip_chain_equals_the_sum_computed_the_long_way() {
    let mut checked = 0usize;
    for (r, g) in [
        SURVEILLANCE,
        DOPPLER,
        HHC,
        (360, 230),
        (1, 1),
        (1, 2048),
        (2048, 1),
        (3, 5),
        (721, 1833),
        (MAX_POLAR_RADIALS, MAX_POLAR_GATES),
        (0, 1832),
        (720, 0),
    ] {
        for levels in 1..=(full_mip_levels(r.max(1), g.max(1)) + 3) {
            assert_eq!(
                chain_texels(r, g, levels),
                chain_the_long_way(r, g, levels),
                "chain_texels({r}, {g}, {levels})",
            );
            checked += 1;
        }
    }
    // The instrument has to have been pointed at something: a loop that ran
    // zero times passes every assertion inside it.
    assert!(checked > 100, "only {checked} shape/level pairs compared");
}

/// **The closed form, both sides of it.**
///
/// A square chain sums to the clean `4/3`; a `radials × gates` one does not,
/// and the ceilings admit no exact expression. What is exact is the bracket
///
/// ```text
/// (4/3)·R·G·(1 − 4^-L)  ≤  chain  <  (4/3)·R·G + 2(R+G) + L
/// ```
///
/// and the square power-of-two identity `(4^(k+1) − 1)/3` the 4/3 comes from.
#[test]
fn the_mip_chain_closed_form_brackets_the_long_sum() {
    for (r, g) in [SURVEILLANCE, DOPPLER, HHC, (360, 230), (3, 5), (1024, 1024)] {
        let levels = full_mip_levels(r, g);
        let exact = chain_texels(r, g, levels) as f64;
        let rg = (r * g) as f64;
        let lower = 4.0 / 3.0 * rg * (1.0 - 4f64.powi(-(levels as i32)));
        let upper = 4.0 / 3.0 * rg + 2.0 * (r + g) as f64 + levels as f64;
        assert!(
            lower <= exact && exact < upper,
            "{r}x{g} at {levels} levels: {lower} <= {exact} < {upper} does not hold",
        );
        // The design's own upper bound is the same statement with `2L` in
        // place of `L` — true, and looser. Both are asserted so a later
        // tightening cannot silently break the document's version.
        assert!(exact < 4.0 / 3.0 * rg + 2.0 * (r + g) as f64 + 2.0 * levels as f64);
    }

    // The exact identity a SQUARE power-of-two chain has, which is where the
    // clean 4/3 comes from and why a non-square one has no equivalent.
    for k in 1..=11u32 {
        let side = 1usize << k;
        assert_eq!(
            chain_texels(side, side, full_mip_levels(side, side)),
            ((4usize.pow(k + 1)) - 1) / 3,
            "a {side}x{side} full chain is (4^(k+1) - 1)/3",
        );
    }

    // And the factor at the two real shapes, off the sum rather than assumed.
    // Six figures, because the design quotes six and one of them is a slip.
    let factor =
        |(r, g): (usize, usize)| chain_texels(r, g, full_mip_levels(r, g)) as f64 / (r * g) as f64;
    assert!((factor(SURVEILLANCE) - 1.333_418).abs() < 5e-7);
    assert!((factor(DOPPLER) - 1.333_439).abs() < 5e-7);
}

/// **The ceil-halved chain never prices below a floor-halved one.**
///
/// Design §2.1 halves by `ceil`; WebGPU sizes a mip level `max(1, floor(size /
/// 2^level))`. Nothing in this tree builds a chain yet, so the code cannot
/// arbitrate — but the price must be conservative whichever the renderer picks,
/// and `ceil(x) >= max(1, floor(x))` for every `x > 0` makes it termwise so.
///
/// The control is the second assertion: at the real shapes the two forms
/// **differ**, so this is not a comparison of a function with itself.
#[test]
fn ceil_halving_never_underprices_a_floor_halved_chain() {
    let mut differed = 0usize;
    for (r, g) in [
        SURVEILLANCE,
        DOPPLER,
        HHC,
        (360, 230),
        (3, 5),
        (721, 1833),
        (MAX_POLAR_RADIALS, MAX_POLAR_GATES),
    ] {
        for levels in 1..=full_mip_levels(r, g) {
            let ceiled = chain_texels(r, g, levels);
            let floored = chain_floor_halved(r, g, levels);
            assert!(
                ceiled >= floored,
                "{r}x{g} at {levels}: ceil chain {ceiled} < floor chain {floored}",
            );
            if ceiled != floored {
                differed += 1;
            }
        }
    }
    assert!(
        differed > 0,
        "the two halvings agreed everywhere, so this test compared a function \
         with itself and would pass on a `chain_floor_halved` that called \
         `chain_texels`",
    );
    // The gap at the surveillance shape, so its size is on the record: 201
    // texels on 1,758,832 — immaterial to a budget, material to the upload.
    let levels = full_mip_levels(SURVEILLANCE.0, SURVEILLANCE.1);
    assert_eq!(
        chain_texels(SURVEILLANCE.0, SURVEILLANCE.1, levels)
            - chain_floor_halved(SURVEILLANCE.0, SURVEILLANCE.1, levels),
        201,
    );
}

/// **`FrameCost` keeps its meaning under the polar terms**, term by term
/// against the buffers, and the host pair adds rather than alternates for the
/// plan view's own reason: the chain is reduced *from* the level-0 codes, so
/// the codes are still allocated when the deepest level exists.
#[test]
fn a_polar_frame_is_priced_from_its_own_buffers() {
    let levels = full_mip_levels(SURVEILLANCE.0, SURVEILLANCE.1);
    let base = SURVEILLANCE.0 * SURVEILLANCE.1 * POLAR_CODE_R8_BYTES;
    let chain = chain_texels(SURVEILLANCE.0, SURVEILLANCE.1, levels) * POLAR_CODE_R8_BYTES;

    // A still pane: codes retained, because a hover reads them.
    let still = polar_frame_cost(shape(SURVEILLANCE, levels, true));
    assert_eq!(still.gpu, chain, "the code texture and its whole chain");
    assert_eq!(still.host_held, base, "the level-0 codes a readout reads");
    assert_eq!(still.host_scratch, chain - base, "the mip tail");
    assert_eq!(
        still.host_peak(),
        chain,
        "held + scratch is the whole chain exactly: the codes are still \
         allocated at the instant the deepest level exists",
    );
    assert_eq!(still.host_peak(), still.host_held + still.host_scratch);

    // A loop frame under the SHIPPED default: codes dropped after upload.
    // `host_held` falls to zero; `host_scratch` does NOT, because the codes
    // must exist to be reduced.
    let looped = polar_frame_cost(shape(SURVEILLANCE, levels, false));
    assert_eq!(looped.gpu, still.gpu, "the GPU term is unaffected");
    assert_eq!(looped.host_held, 0);
    assert_eq!(looped.host_scratch, chain - base);
    assert_eq!(looped.host_peak(), chain - base);
    assert!(
        looped.host_peak() < still.host_peak(),
        "retention has to be visible in the price or the two arms are one",
    );

    // `Reduce::None` — HHC and PHI, one level — makes the scratch exactly
    // zero, and the GPU term exactly the base plane.
    let flat = polar_frame_cost(shape(HHC, 1, true));
    assert_eq!(flat.host_scratch, 0, "one level is the base plane exactly");
    assert_eq!(flat.gpu, HHC.0 * HHC.1 * POLAR_CODE_R8_BYTES);
    assert_eq!(flat.host_peak(), flat.host_held);

    // Sweeps multiply every per-frame term and nothing else.
    let three = PolarFrameShape {
        sweeps: 3,
        ..shape(SURVEILLANCE, levels, true)
    };
    let cost = polar_frame_cost(three);
    assert_eq!(cost.gpu, 3 * still.gpu);
    assert_eq!(cost.host_held, 3 * still.host_held);
    assert_eq!(cost.host_scratch, 3 * still.host_scratch);

    // The R16 widening is a width, not a second function.
    let wide = PolarFrameShape {
        width_bytes: POLAR_CODE_R16_BYTES,
        ..shape(SURVEILLANCE, levels, true)
    };
    assert_eq!(polar_frame_cost(wide).host_peak(), 2 * still.host_peak());
}

/// **The byte comparison this seam exists for**, at the side a desktop arm
/// really renders a surveillance cut at — not at a nominal 2048.
///
/// `data_limited_side_px` puts a 1832-gate cut at ±460.125 km at **7362 px**,
/// bound by the sweep's own gates and not by `DESKTOP_RASTER_SIDE_CEILING`
/// (`the_desktop_raster_ceiling_is_the_widest_sweeps_own_need_and_no_panes`
/// pins that reading). So the raster this replaces is 827.0 MiB of host peak
/// for a picture that lands in a pane under 1920 px across.
///
/// | | raster @ 7362 | polar, one tilt | ratio |
/// |---|---:|---:|---:|
/// | GPU | 216,796,176 | 1,758,832 | 123× |
/// | host held (still) | 433,592,352 | 1,319,040 | 329× |
/// | host scratch | 433,592,352 | 439,792 | 986× |
/// | **host peak** | **867,184,704** | **1,758,832** | **493×** |
///
/// The design's §6.1 table quotes the ratio against side 2048 (0.105× GPU),
/// which is the right comparison for a loop frame and the wrong one for the
/// still this campaign's 827 MiB `render pools` figure came from.
#[test]
fn what_a_polar_frame_costs_against_the_raster_it_replaces() {
    use squallar_radar::types::{data_limited_side_px, plan_view_extent_km};

    // The side, re-derived rather than pinned, so a change to the sizing rule
    // moves this comparison instead of leaving it quoting a stale number.
    const SURVEILLANCE_REACH_KM: f64 = 460.125;
    const SUPER_RES_GATE_KM: f64 = 0.25;
    let side = data_limited_side_px(
        plan_view_extent_km(SURVEILLANCE_REACH_KM),
        SUPER_RES_GATE_KM,
    );
    assert_eq!(
        side, 7362,
        "the observed data-bound side of a surveillance cut"
    );

    let raster = plan_view_frame_cost(side);
    assert_eq!(raster.gpu, 216_796_176);
    assert_eq!(raster.host_held, 433_592_352);
    assert_eq!(raster.host_scratch, 433_592_352);
    assert_eq!(raster.host_peak(), 867_184_704);
    assert_eq!(raster.host_peak() / (1024 * 1024), 827);

    let levels = full_mip_levels(SURVEILLANCE.0, SURVEILLANCE.1);
    let polar = polar_frame_cost(shape(SURVEILLANCE, levels, true));
    assert_eq!(polar.gpu, 1_758_832);
    assert_eq!(polar.host_held, 1_319_040);
    assert_eq!(polar.host_scratch, 439_792);
    assert_eq!(polar.host_peak(), 1_758_832);

    // The ratio, integer-floored so it cannot be read as more precise than it
    // is. 493x is the number the admission door must never be wrong about.
    assert_eq!(raster.host_peak() / polar.host_peak(), 493);
    assert_eq!(raster.gpu / polar.gpu, 123);

    // And against the design's own denominator, so both readings are on the
    // record and neither can be quoted as the other. §6.1's table is side 2048.
    let at_2048 = plan_view_frame_cost(2048);
    assert_eq!(at_2048.gpu, 16_777_216);
    assert_eq!(at_2048.host_peak(), 67_108_864);
    assert_eq!(at_2048.host_peak() / polar.host_peak(), 38);

    // The Doppler figure, which shares no denominator with either of the above.
    let doppler = polar_frame_cost(shape(DOPPLER, full_mip_levels(DOPPLER.0, DOPPLER.1), true));
    assert_eq!(doppler.host_peak(), 1_144_411);
}

/// **The per-sweep-key terms are not in the per-frame price**, and charging
/// them there would over-count a loop by its frame count.
#[test]
fn the_lut_and_the_edge_table_are_per_sweep_key_not_per_frame() {
    assert_eq!(POLAR_LUT_BYTES, 1_024);
    assert_eq!(polar_drawn_edge_bytes(720), 5_760);
    // The prose in `squallar_radar::hover` calls that "5.8 KiB". It is 5.625
    // KiB — 5.76 kB decimal — and the two prefixes are not the same number.
    assert_ne!(polar_drawn_edge_bytes(720), (5.8 * 1024.0) as usize);

    // Neither is in a frame's price: the whole host peak is the chain, with
    // no room left over for a 1,024 B table or a 5,760 B one. So a 60-frame
    // loop is 60 x the frame cost and ONE LUT, not 60 LUTs.
    let levels = full_mip_levels(SURVEILLANCE.0, SURVEILLANCE.1);
    let cost = polar_frame_cost(shape(SURVEILLANCE, levels, true));
    let base = SURVEILLANCE.0 * SURVEILLANCE.1 * POLAR_CODE_R8_BYTES;
    let chain = chain_texels(SURVEILLANCE.0, SURVEILLANCE.1, levels) * POLAR_CODE_R8_BYTES;
    assert_eq!(cost.host_held + cost.host_scratch, chain);
    assert_eq!(cost.host_held, base);
    assert_eq!(cost.gpu, chain);

    // The control: a price that HAD folded them in would read differently, and
    // by an amount this assertion can see. Without this line the three above
    // pass on any function whose terms happen to sum to the chain.
    let with_tables = chain + POLAR_LUT_BYTES + polar_drawn_edge_bytes(SURVEILLANCE.0);
    assert_ne!(cost.gpu, with_tables);
    assert_eq!(with_tables - cost.gpu, 6_784);
}

/// **The caps are bounds, and the observed shapes are not.**
///
/// `MAX_POLAR_RADIALS` is twice what the RDA can declare and
/// `MAX_POLAR_GATES` is the WebGL2 per-axis guarantee verbatim. A payload past
/// either is refused by the producing lane rather than priced — but this seam
/// must still price the widest admissible frame without overflowing, including
/// on wasm32 where `usize` is 32 bits.
#[test]
fn the_caps_bound_the_arithmetic_and_the_observed_shapes_sit_inside_them() {
    // NOT `assert_eq!(MAX_POLAR_GATES, WEBGL2_MAX_TEXTURE_DIMENSION_2D)` —
    // the const is DEFINED as that expression, so such an assertion cannot
    // fail and would be evidence of nothing. The values are pinned in the
    // const-assert block; what is testable here is the property that makes
    // these caps BOUNDS rather than observed maxima.
    assert_eq!(MAX_POLAR_GATES, 2_048);
    assert_eq!(MAX_POLAR_RADIALS, 1_440);

    // Every shape the design observes is STRICTLY inside both caps. An
    // observed maximum is not a bound: a cap equal to the widest thing seen
    // says only that nothing wider has been seen yet.
    let observed = [SURVEILLANCE, DOPPLER, HHC, (360, 230)];
    let widest_radials = observed.iter().map(|s| s.0).max().expect("non-empty");
    let widest_gates = observed.iter().map(|s| s.1).max().expect("non-empty");
    assert!(
        widest_radials < MAX_POLAR_RADIALS,
        "the radial cap {MAX_POLAR_RADIALS} is the widest observed sweep          ({widest_radials}), which makes it an observation and not a bound",
    );
    assert!(
        widest_gates < MAX_POLAR_GATES,
        "the gate cap {MAX_POLAR_GATES} is the widest observed sweep          ({widest_gates}), which makes it an observation and not a bound",
    );
    // But the gate cap is a LIVE bound, not decoration: a real surveillance
    // cut sits in its upper half, so a sweep half again as long is refused.
    assert!(widest_gates > MAX_POLAR_GATES / 2);
    // The radial cap has the opposite shape and the doc says why: it is twice
    // what the RDA can declare, so it is slack by construction.
    assert_eq!(MAX_POLAR_RADIALS, 2 * widest_radials);

    // The widest admissible frame, at the widest code, over two sweeps and the
    // full chain: still inside a 32-bit `usize`.
    let widest = PolarFrameShape {
        radials: MAX_POLAR_RADIALS,
        gates: MAX_POLAR_GATES,
        width_bytes: POLAR_CODE_R16_BYTES,
        sweeps: 2,
        mip_levels: full_mip_levels(MAX_POLAR_RADIALS, MAX_POLAR_GATES),
        codes_retained: true,
    };
    let cost = polar_frame_cost(widest);
    assert!(cost.host_peak() < u32::MAX as usize, "{}", cost.host_peak());
    // And it is still an order of magnitude under one raster at 7362.
    assert!(cost.host_peak() * 10 < plan_view_frame_cost(7362).host_peak());
}

/// **Nothing in this workspace calls the polar price outside its own tests.**
///
/// The safety constraint this seam lands under: `polar_frame_cost` prices
/// ~493x below what the renderer actually allocates today, so a call site on
/// any path reaching `crate::fit::NeedTerms` would have the admission door
/// admit a scene costing ~493x its price. The switch must be keyed on what the
/// renderer produced for that frame — never a build flag, never a feature gate.
///
/// A source scrape, because that is the only instrument that can see a call
/// site this crate does not compile. It reads the whole workspace tree, and
/// the second half proves the scrape is sensitive: the same walk finds the
/// call sites of `plan_view_frame_cost`, which are many.
#[test]
fn nothing_selects_the_polar_price_yet() {
    use std::fs;
    use std::path::Path;

    fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !matches!(name.as_ref(), "target" | ".git" | "node_modules" | "pkg") {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(text) = fs::read_to_string(&path)
            {
                out.push((path, text));
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits one level under the workspace root")
        .to_path_buf();
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(
        files.len() > 200,
        "the walk found {} .rs files, which is not this workspace",
        files.len(),
    );

    // The one file allowed to name it: this crate's constants module and its
    // own tests, which are `constants.rs` and `constants/tests.rs`.
    let permitted = |p: &Path| {
        p.ends_with("squallar-device-profile/src/constants.rs")
            || p.ends_with("squallar-device-profile/src/constants/tests.rs")
    };
    let offenders: Vec<_> = files
        .iter()
        .filter(|(p, text)| !permitted(p) && text.contains("polar_frame_cost"))
        .map(|(p, _)| p.display().to_string())
        .collect();
    assert!(
        offenders.is_empty(),
        "polar_frame_cost is selected outside its own module: {offenders:?}. It \
         prices ~493x below what the renderer allocates today; a call site on \
         any path that reaches NeedTerms makes the admission door admit a \
         scene at ~1/493 of its real cost. The switch is keyed on what the \
         renderer PRODUCED for the frame, never on a flag.",
    );

    // The scrape is sensitive: the same walk over the same corpus finds the
    // predecessor's call sites, which exist in several crates. A null from an
    // instrument never shown to fire is not a null.
    let raster_sites: Vec<_> = files
        .iter()
        .filter(|(p, text)| !permitted(p) && text.contains("plan_view_frame_cost"))
        .map(|(p, _)| p.display().to_string())
        .collect();
    assert!(
        raster_sites.len() >= 2,
        "the scrape found only {} call sites of plan_view_frame_cost outside \
         this module, so it is not reading the workspace and its null above \
         means nothing: {raster_sites:?}",
        raster_sites.len(),
    );
}

/// **The wasm loop budgets leave the reproduced freeze short of the page by the
/// margin their doc claims** — asserted from the recorded figures the doc
/// quotes, so a change to any one term moves this and not just the prose.
///
/// A runtime test and not a `const` assertion: it needs `max` and formatted
/// failure messages, and neither is const on this toolchain.
///
/// **Nothing here reads the admission door.** On that reproduction the door's
/// `asked` counter moves only on refusals, so a zero is what a door that is
/// never called and a door that admits every time both print; a grant is not
/// evidence that anything fits. The claim is resident bytes against the page.
#[test]
fn the_wasm_loop_budgets_clear_the_reproduced_freeze() {
    use super::web_freeze_2026_09_07 as wf;
    use super::{WASM_LOOP_ARCHIVE_CEILING_BYTES, WASM_LOOP_DECODED_CEILING_BYTES};

    let residue_firefox = wf::RESIDENT_TOTAL_FIREFOX - wf::LOOP_SCANS_FIREFOX;
    let residue_chromium = wf::RESIDENT_TOTAL_CHROMIUM - wf::LOOP_SCANS_CHROMIUM;
    assert!(
        residue_firefox > residue_chromium,
        "the doc names firefox as the worse leg for non-loop residue; it is not \
         ({residue_firefox} vs {residue_chromium}) — re-derive the margin"
    );
    let residue = residue_firefox;

    let loop_after = WASM_LOOP_DECODED_CEILING_BYTES as u64
        + WASM_LOOP_ARCHIVE_CEILING_BYTES as u64
        + wf::FRAMES * wf::SWEEP_BYTES;
    let short_of_wall = wf::PAGE_BYTES
        .checked_sub(residue + loop_after)
        .expect("the wasm loop budgets alone push the reproduced scene past the page");
    assert!(
        short_of_wall >= wf::CLAIMED_MARGIN_BYTES,
        "the budgets leave the death scene {} MiB short of the wall, less than \
         the {} MiB claimed",
        short_of_wall >> 20,
        wf::CLAIMED_MARGIN_BYTES >> 20
    );
    // And the claim is stated, not headroom by accident: within one grid
    // family of the truth, or the doc is under-stating what it has.
    assert!(
        short_of_wall - wf::CLAIMED_MARGIN_BYTES < wf::OVERLAY_GRIDS,
        "the doc claims {} MiB and the arithmetic gives {} MiB; re-state it",
        wf::CLAIMED_MARGIN_BYTES >> 20,
        short_of_wall >> 20
    );
    // The decoded ceiling admits at least two volumes at the corpus maximum,
    // or the initial fill cannot pipeline at all.
    assert!(WASM_LOOP_DECODED_CEILING_BYTES as u64 >= 2 * 78_255_227);
    // The archive ceiling holds a full loop at the tree's 208-corpus median
    // archive (5.56 MiB), so the common case never re-downloads.
    assert!(WASM_LOOP_ARCHIVE_CEILING_BYTES as u64 >= wf::FRAMES * 5_830_000);
}
