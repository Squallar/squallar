use super::*;

/// A desktop-shaped device: side limit well clear of anything, budget the
/// desktop arm.
const DESKTOP: MirrorLimits = MirrorLimits {
    max_side: 8192,
    max_bytes: 64 * 1024 * 1024,
};

/// The WebGL2 floor: the only side cap the wasm arm may assume, and the budget
/// that matches it exactly.
const WEB: MirrorLimits = MirrorLimits {
    max_side: MIRROR_MAX_SIDE,
    max_bytes: 16 * 1024 * 1024,
};

/// The one invariant the whole adaptive design rests on, checked upwards.
#[test]
fn the_quotient_that_egui_divides_by_survives_every_rung() {
    let frame = [1280.0f32, 720.0f32];
    let points = 1.5f32;
    for wanted in [1.0, 2.0] {
        let plan = mirror_plan(frame, points, wanted, DESKTOP);
        let quotient = plan.size_in_pixels[0] as f32 / plan.pixels_per_point;
        assert!(
            (quotient - frame[0]).abs() < 1e-3,
            "rung {wanted} moved screen_size_in_points from {} to \
             {quotient}: the mirror would draw scaled vertices, not a denser \
             raster",
            frame[0],
        );
        let quotient_y = plan.size_in_pixels[1] as f32 / plan.pixels_per_point;
        assert!((quotient_y - frame[1]).abs() < 1e-3);
        // Unmoved by the rung — the quantity the floor's lanes normalise to.
        assert_eq!(plan.size_in_points, frame);
    }
}

/// A rung is a power of two between 1 and the cap, and never below 1.
#[test]
fn the_wanted_rung_is_a_power_of_two_inside_the_cap() {
    assert_eq!(wanted_scale_for(0.1), 1.0);
    assert_eq!(wanted_scale_for(1.0), 1.0);
    assert_eq!(wanted_scale_for(1.01), 2.0);
    assert_eq!(wanted_scale_for(2.0), 2.0);
    assert_eq!(wanted_scale_for(3.9), MIRROR_SCALE_MAX);
    assert_eq!(wanted_scale_for(1000.0), MIRROR_SCALE_MAX);
    assert_eq!(wanted_scale_for(f32::NAN), 1.0);
    assert_eq!(wanted_scale_for(f32::INFINITY), 1.0);
}

/// The cap is where the tile cache argument bites, not only the byte budget.
#[test]
fn the_rung_above_the_cap_could_never_fit_the_tile_cache() {
    let entries = rustdar_egui::tile_source::TILE_CACHE_ENTRIES.get();
    let cap_bias = MIRROR_SCALE_MAX.log2() as u8;
    // The most favourable case bias 2 could ever get.
    let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0));
    let layers = 2;

    assert!(
        rustdar_egui::tiles::tiles_resident_for(pane, cap_bias + 1, layers) > entries,
        "bias {} fits the {entries}-entry LRU for a 900-point pane, so the cap \
         at rung {MIRROR_SCALE_MAX} is not the one the tile cache argues for",
        cap_bias + 1,
    );
    assert!(
        rustdar_egui::tiles::tiles_resident_for(pane, cap_bias, layers) <= entries,
        "bias {cap_bias} does not fit even a 900-point pane, so the cap admits \
         a rung that could never be taken",
    );
    // Bias 0 must be far inside it, or the LRU was already too small.
    assert!(rustdar_egui::tiles::tiles_resident_for(pane, 0, layers) * 4 <= entries);
}

/// A frame that cannot afford the rung says so, and the tile bias follows what
/// was applied rather than what was asked for.
#[test]
fn a_refused_rung_is_visible_and_does_not_fetch_tiles_it_cannot_show() {
    let plan = mirror_plan([1280.0, 720.0], 2.0, 2.0, WEB);
    assert!(
        plan.is_degraded(),
        "the WebGL2 floor must refuse rung 2 at 1440p"
    );
    assert_eq!(plan.tile_zoom_bias(), 0);
    assert_eq!(plan.applied_scale, 0.5);

    let plan = mirror_plan([1920.0, 1080.0], 1.0, 2.0, DESKTOP);
    assert!(!plan.is_degraded());
    assert_eq!(plan.tile_zoom_bias(), 1);
}

/// The fit terminates even on a device that reports something absurd.
#[test]
fn the_fit_cannot_loop_forever() {
    let plan = mirror_plan(
        [4.0, 4.0],
        1.0,
        2.0,
        MirrorLimits {
            max_side: 1,
            max_bytes: 1,
        },
    );
    // `max_side` is floored at `MIRROR_MAX_SIDE` only through `for_device`; a
    // hand-built limit this small is the case the break exists for.
    assert!(plan.size_in_pixels[0] >= 1 && plan.size_in_pixels[1] >= 1);
}

/// `for_device` never trusts a device below the guarantee, and it spends the
/// byte budget it was handed rather than one of its own.
#[test]
fn the_device_side_cap_is_floored_at_the_guarantee_and_raised_above_it() {
    let budget = rustdar_device_profile::constants::VOLUME_MIRROR_BYTES_MAX;
    assert_eq!(
        MirrorLimits::for_device(512, budget).max_side,
        MIRROR_MAX_SIDE
    );
    assert_eq!(MirrorLimits::for_device(16384, budget).max_side, 16384);
    // The other half of the pair passes straight through, on every arm.
    for arm in rustdar_device_profile::budget::BudgetLimits::SHIPPED {
        let budgets = rustdar_device_profile::budget::resolve(
            &rustdar_device_profile::budget::DeviceProfile {
                limits: arm,
                ..rustdar_device_profile::budget::DeviceProfile::for_target()
            },
        );
        assert_eq!(
            MirrorLimits::for_device(8192, budgets.mirror_bytes).max_bytes,
            budgets.mirror_bytes,
            "{}",
            budgets.name,
        );
    }
}

/// A camera parked exactly on a rung boundary does not oscillate.
#[test]
fn a_camera_sitting_on_a_boundary_never_thrashes() {
    let mut rungs = MirrorRungs::default();
    // Climb to rung 2 and let it commit.
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES {
        rungs.observe(Some(1.9), [1920.0, 1080.0], 1.0, DESKTOP);
    }
    let settled = rungs.observe(Some(1.9), [1920.0, 1080.0], 1.0, DESKTOP);
    assert_eq!(settled.applied_scale, 2.0, "the rung never committed");

    // Now drift back and forth across the bare threshold. Anything inside the
    // dead band must hold rung 2.
    for magnification in [1.0, 0.95, 1.05, 0.85, 1.2, 0.81] {
        for _ in 0..MIRROR_RUNG_DWELL_FRAMES * 2 {
            let plan = rungs.observe(Some(magnification), [1920.0, 1080.0], 1.0, DESKTOP);
            assert_eq!(
                plan.applied_scale, 2.0,
                "magnification {magnification} is inside the {MIRROR_RUNG_HYSTERESIS}x \
                 dead band and must not give up rung 2",
            );
        }
    }

    // Past the band, it gives the rung back — but only after the dwell.
    let plan = rungs.observe(Some(0.5), [1920.0, 1080.0], 1.0, DESKTOP);
    assert_eq!(
        plan.applied_scale, 2.0,
        "a single frame must not move a rung"
    );
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES {
        rungs.observe(Some(0.5), [1920.0, 1080.0], 1.0, DESKTOP);
    }
    assert_eq!(
        rungs
            .observe(Some(0.5), [1920.0, 1080.0], 1.0, DESKTOP)
            .applied_scale,
        1.0,
    );
}

/// A demand that keeps changing its mind never commits anything.
#[test]
fn an_unsettled_camera_commits_no_rung() {
    let mut rungs = MirrorRungs::default();
    for frame in 0..MIRROR_RUNG_DWELL_FRAMES * 8 {
        let magnification = if frame % 2 == 0 { 4.0 } else { 0.2 };
        let plan = rungs.observe(Some(magnification), [1920.0, 1080.0], 1.0, DESKTOP);
        assert_eq!(plan.applied_scale, 1.0, "a flapping demand moved the rung");
    }
}

/// Hiding a floor holds the rung rather than resetting it.
#[test]
fn a_frame_with_no_floor_holds_the_rung() {
    let mut rungs = MirrorRungs::default();
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES + 1 {
        rungs.observe(Some(2.0), [1920.0, 1080.0], 1.0, DESKTOP);
    }
    assert_eq!(rungs.tile_zoom_bias(), 1);
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES * 4 {
        let plan = rungs.observe(None, [1920.0, 1080.0], 1.0, DESKTOP);
        assert_eq!(plan.applied_scale, 2.0);
    }
    assert_eq!(rungs.tile_zoom_bias(), 1);
}

/// The bias a frame's tiles are drawn with is the one the mirror was sized to.
#[test]
fn the_tile_bias_is_zero_until_a_mirror_has_actually_been_planned() {
    let rungs = MirrorRungs::default();
    assert_eq!(rungs.tile_zoom_bias(), 0);
}

/// The off-screen floor strip costs at most one rung, on every target.
#[test]
fn the_strip_costs_at_most_one_rung_on_any_target() {
    let mobile = MirrorLimits {
        max_side: 8192,
        max_bytes: 16 * 1024 * 1024,
    };
    // Frames in points, at the density each target really presents them at.
    let frames = [
        ("desktop 4K", [1920.0f32, 1080.0f32], 2.0f32, DESKTOP),
        ("desktop 1440p", [2560.0, 1440.0], 1.0, DESKTOP),
        ("desktop 1080p", [1920.0, 1080.0], 1.0, DESKTOP),
        ("wasm 1080p", [1920.0, 1080.0], 1.0, WEB),
        ("wasm retina 1080p", [960.0, 540.0], 2.0, WEB),
        ("wasm laptop", [1440.0, 900.0], 1.0, WEB),
        ("mobile phone portrait", [360.0, 780.0], 3.0, mobile),
        ("mobile small phone", [360.0, 800.0], 2.0, mobile),
        ("mobile tablet", [800.0, 1280.0], 2.0, mobile),
    ];
    for (name, frame, points, limits) in frames {
        for wanted in [1.0, 2.0] {
            let bare = mirror_plan(frame, points, wanted, limits);
            // The worst strip there is: a 3D pane filling the frame, so the
            // mirror is the frame twice over.
            let strip = mirror_plan([frame[0], frame[1] * 2.0], points, wanted, limits);
            assert!(
                strip.applied_scale >= bare.applied_scale * 0.5,
                "{name} at rung {wanted}: the strip took the mirror from scale \
                 {} to {}, which is more than the one halving twice the area \
                 can ever cost",
                bare.applied_scale,
                strip.applied_scale,
            );
            assert!(
                strip.applied_scale <= bare.applied_scale,
                "{name} at rung {wanted}: a taller mirror came out *sharper*",
            );
            // And whatever it settled on, it really fits.
            let bytes = strip.size_in_pixels[0] as usize * strip.size_in_pixels[1] as usize * 4;
            assert!(
                bytes <= limits.max_bytes
                    && strip.size_in_pixels[0].max(strip.size_in_pixels[1]) <= limits.max_side,
                "{name} at rung {wanted}: {:?} is {bytes} bytes against a \
                 {}-byte budget and a {} side cap",
                strip.size_in_pixels,
                limits.max_bytes,
                limits.max_side,
            );
        }
    }
}

/// The rows of that table that are a *change*, spelled out so they cannot drift.
#[test]
fn the_strips_verdict_is_the_one_the_table_states() {
    // Desktop 1080p keeps rung 2, which is what makes the loss below a loss at
    // one resolution rather than a loss of the rung altogether.
    let plan = mirror_plan([1920.0, 2160.0], 1.0, 2.0, DESKTOP);
    assert_eq!(
        (plan.size_in_pixels, plan.applied_scale),
        ([3840, 4320], 2.0),
        "desktop 1080p with a full-height strip lost rung 2",
    );

    // Desktop 1440p does not. This is the deliberate loss.
    let plan = mirror_plan([2560.0, 2880.0], 1.0, 2.0, DESKTOP);
    assert_eq!(
        plan.applied_scale, 1.0,
        "desktop 1440p at rung 2 now fits a 64 MiB mirror; either the budget \
         moved or the strip did, and the table is wrong either way",
    );
    assert!(plan.is_degraded() && plan.tile_zoom_bias() == 0);

    // The WebGL2 side cap, which binds before that arm's budget ever does.
    let plan = mirror_plan([1920.0, 2160.0], 1.0, 1.0, WEB);
    assert_eq!(
        (plan.size_in_pixels, plan.applied_scale),
        ([960, 1080], 0.5),
        "a 1080p wasm frame plus its strip is 2160 texels tall against a 2048 \
         side cap, so it must halve exactly once",
    );
    // ...and a smaller browser window does not pay it at all.
    let plan = mirror_plan([1440.0, 1760.0], 1.0, 1.0, WEB);
    assert_eq!(
        plan.applied_scale, 1.0,
        "a 1440x900 wasm frame lost density"
    );
}

/// The constants-agreement proof that bridges to this module's plan.
mod budget_agreement {
    use rustdar_device_profile::constants::{
        DESKTOP_VOLUME_MIRROR_BYTES_MAX, MOBILE_VOLUME_MIRROR_BYTES_MAX,
        WASM_VOLUME_MIRROR_BYTES_MAX,
    };

    /// The pane mirror's ceiling is the cap squared, four bytes a texel.
    #[test]
    fn the_pane_mirrors_ceiling_is_the_cap_it_is_actually_halved_to() {
        let side = crate::egui_renderer::MIRROR_MAX_SIDE as usize;
        assert_eq!(
            WASM_VOLUME_MIRROR_BYTES_MAX,
            side * side * 4,
            "the wasm32 budget is not the guaranteed cap squared at four bytes a texel",
        );
        assert_eq!(
            WASM_VOLUME_MIRROR_BYTES_MAX,
            16 * 1024 * 1024,
            "the wasm32 mirror's worst case moved. WebGL2 guarantees only a 2048 \
             side, so this arm is pinned by the device as well as by the budget.",
        );
        assert_eq!(
            MOBILE_VOLUME_MIRROR_BYTES_MAX, WASM_VOLUME_MIRROR_BYTES_MAX,
            "mobile is held to the same 16 MiB the pre-adaptive design cost, so \
             landing the rung moved no phone's floor-on memory",
        );
        assert_eq!(
            DESKTOP_VOLUME_MIRROR_BYTES_MAX,
            64 * 1024 * 1024,
            "the desktop mirror's worst case moved. It is one allocation for the \
             whole application, so a change here is a change to the application's \
             floor-on memory, not to a per-pane cost.",
        );

        // The tight row: 1440p at the top rung, with no floor strip under it.
        let bytes = |w: usize, h: usize| w * h * 4;
        assert!(
            bytes(5120, 2880) <= DESKTOP_VOLUME_MIRROR_BYTES_MAX,
            "1440p at rung 2 no longer fits the desktop budget",
        );
        assert!(
            bytes(3840 * 4, 2160 * 4) > DESKTOP_VOLUME_MIRROR_BYTES_MAX,
            "the desktop budget is slack enough to hide a rung-4 4K mirror",
        );

        // `screen_size_in_points` is `size_in_pixels / pixels_per_point`, so
        // both must move together or the frame's vertices scale instead of its
        // sampling rate.
        let desktop = crate::egui_renderer::MirrorLimits {
            max_side: 8192,
            max_bytes: DESKTOP_VOLUME_MIRROR_BYTES_MAX,
        };
        // Points, not pixels: `mirror_plan` sizes a region of egui's own space.
        let plan = crate::egui_renderer::mirror_plan([1280.0, 720.0], 1.5, 2.0, desktop);
        assert_eq!(
            (plan.size_in_pixels, plan.pixels_per_point),
            ([3840, 2160], 3.0),
            "a desktop 1080p frame asked for rung 2 must get it, both halves moved",
        );
        assert!(!plan.is_degraded() && plan.tile_zoom_bias() == 1);
        let plan = crate::egui_renderer::mirror_plan([1920.0, 1080.0], 2.0, 2.0, desktop);
        assert_eq!(
            (
                plan.size_in_pixels,
                plan.pixels_per_point,
                plan.applied_scale
            ),
            ([3840, 2160], 2.0, 1.0),
            "a 4K frame cannot afford rung 2 and falls back to its own size — an \
             improvement on the old cap, which halved it to 1920x1080",
        );
        assert!(
            plan.is_degraded() && plan.tile_zoom_bias() == 0,
            "a degraded plan must not go on fetching a slippy level it cannot show",
        );

        // The wasm32 arm, where the device's own guarantee binds before the budget.
        let web = crate::egui_renderer::MirrorLimits {
            max_side: crate::egui_renderer::MIRROR_MAX_SIDE,
            max_bytes: WASM_VOLUME_MIRROR_BYTES_MAX,
        };
        let plan = crate::egui_renderer::mirror_plan([1280.0, 720.0], 2.0, 2.0, web);
        assert_eq!(
            (plan.size_in_pixels, plan.applied_scale),
            ([1280, 720], 0.5),
            "the WebGL2 floor still halves a 1440p frame twice, rung or no rung",
        );
        assert!(plan.is_degraded() && plan.tile_zoom_bias() == 0);

        // The pre-adaptive helper, unchanged for every frame with no 3D pane on it.
        let (size, scale) = crate::egui_renderer::mirror_size_for([1920.0, 1080.0], 2.0);
        assert_eq!((size, scale), ([1920, 1080], 1.0), "a 4K frame halves once");
        let (size, scale) = crate::egui_renderer::mirror_size_for([1280.0, 720.0], 1.5);
        assert_eq!(
            (size, scale),
            ([1920, 1080], 1.5),
            "a frame already under the cap is mirrored at its own size",
        );
        let (size, _) = crate::egui_renderer::mirror_size_for([8192.0, 8192.0], 1.0);
        assert!(
            size[0].max(size[1]) <= crate::egui_renderer::MIRROR_MAX_SIDE
                && size[0] * size[1] * 4 <= WASM_VOLUME_MIRROR_BYTES_MAX as u32,
            "a frame far over the cap must halve until it fits, got {size:?}",
        );
    }
}
