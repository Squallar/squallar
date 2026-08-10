use super::*;

/// A desktop-shaped device: side limit well clear of anything, budget the
/// desktop arm. Written out rather than taken from `MirrorLimits::for_device`
/// so these tests say what they are exercising on every target that compiles
/// them.
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
///
/// egui's vertex shader divides by `screen_size_in_points`, which is
/// `size_in_pixels / pixels_per_point`. The pre-adaptive code only ever *halved*
/// both, so "the geometry is unaffected" had only been demonstrated downwards.
/// It is a statement about a quotient and therefore direction-free, and this is
/// what says so in a form that fails if a future edit scales one and not the
/// other.
///
/// The floor's own uniform lanes are the reciprocal of the same quotient
/// (`volume_bridge::floor_lanes`), so this is also the reason adaptive
/// resolution cannot disturb registration — the property `floor_alignment`
/// measures as best translation `(0, 0)`.
#[test]
fn the_quotient_that_egui_divides_by_survives_every_rung() {
    let frame = [1920u32, 1080u32];
    let points = 1.5f32;
    let reference = frame[0] as f32 / points;
    for wanted in [1.0, 2.0] {
        let plan = mirror_plan(frame, points, wanted, DESKTOP);
        let quotient = plan.size_in_pixels[0] as f32 / plan.pixels_per_point;
        assert!(
            (quotient - reference).abs() < 1e-3,
            "rung {wanted} moved screen_size_in_points from {reference} to \
             {quotient}: the mirror would draw scaled vertices, not a denser \
             raster",
        );
        let quotient_y = plan.size_in_pixels[1] as f32 / plan.pixels_per_point;
        assert!((quotient_y - frame[1] as f32 / points).abs() < 1e-3);
    }
}

/// A rung is a power of two between 1 and the cap, and never below 1.
///
/// Below 1 is not "the camera is zoomed out and can spare texels": the mirror is
/// one texture for the whole application, so a reduction taken for one 3D pane
/// would blur the floor under every other one. Reductions exist only as the
/// fit's answer to a frame that does not fit.
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
///
/// `MIRROR_SCALE_MAX` is documented as being held down by
/// `tile_source::TILE_CACHE_ENTRIES` as well as by memory, and the two figures
/// live in different crates. This is the arithmetic that connects them, through
/// the same function the UI actually gates on.
///
/// Note what it does *not* claim: that bias 1 always fits. It does not — a large
/// enough window overruns the LRU at bias 1 too, which is why
/// `Gui::tile_zoom_bias_for_pane` measures rather than assumes. What the cap has
/// to guarantee is that the level *above* it could never fit, so that no window
/// size would have made bias 2 worth allowing.
#[test]
fn the_rung_above_the_cap_could_never_fit_the_tile_cache() {
    let entries = rustdar_egui::tile_source::TILE_CACHE_ENTRIES.get();
    let cap_bias = MIRROR_SCALE_MAX.log2() as u8;
    // A modest source pane — smaller than any real one that would carry a 3D
    // floor, so this is the most favourable case bias 2 could ever get.
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
    // And bias 0 — what every pane with no 3D floor over it draws — must be far
    // inside it, or the LRU was already too small before any of this.
    assert!(rustdar_egui::tiles::tiles_resident_for(pane, 0, layers) * 4 <= entries);
}

/// A frame that cannot afford the rung says so, and the tile bias follows what
/// was applied rather than what was asked for.
///
/// The second half is the part that matters at runtime: a mirror that was
/// refused its rung but whose source pane went on fetching a slippy level
/// deeper would pay the whole cost of the detail — four times the tiles, four
/// times the decode — and then interpolate it away.
#[test]
fn a_refused_rung_is_visible_and_does_not_fetch_tiles_it_cannot_show() {
    let plan = mirror_plan([2560, 1440], 2.0, 2.0, WEB);
    assert!(
        plan.is_degraded(),
        "the WebGL2 floor must refuse rung 2 at 1440p"
    );
    assert_eq!(plan.tile_zoom_bias(), 0);
    assert_eq!(plan.applied_scale, 0.5);

    let plan = mirror_plan([1920, 1080], 1.0, 2.0, DESKTOP);
    assert!(!plan.is_degraded());
    assert_eq!(plan.tile_zoom_bias(), 1);
}

/// The fit terminates even on a device that reports something absurd.
#[test]
fn the_fit_cannot_loop_forever() {
    let plan = mirror_plan(
        [4, 4],
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

/// `for_device` never trusts a device below the guarantee.
#[test]
fn the_device_side_cap_is_floored_at_the_guarantee_and_raised_above_it() {
    assert_eq!(MirrorLimits::for_device(512).max_side, MIRROR_MAX_SIDE);
    assert_eq!(MirrorLimits::for_device(16384).max_side, 16384);
    assert_eq!(
        MirrorLimits::for_device(8192).max_bytes,
        crate::constants::VOLUME_MIRROR_BYTES_MAX,
    );
}

/// A camera parked exactly on a rung boundary does not oscillate.
///
/// The failure this prevents is not cosmetic: every rung change re-allocates the
/// mirror *and* moves the source pane's tile zoom, so a camera drifting across
/// 2.0 would re-fetch a tile pyramid on alternate frames.
#[test]
fn a_camera_sitting_on_a_boundary_never_thrashes() {
    let mut rungs = MirrorRungs::default();
    // Climb to rung 2 and let it commit.
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES {
        rungs.observe(Some(1.9), [1920, 1080], 1.0, DESKTOP);
    }
    let settled = rungs.observe(Some(1.9), [1920, 1080], 1.0, DESKTOP);
    assert_eq!(settled.applied_scale, 2.0, "the rung never committed");

    // Now drift back and forth across the bare threshold. Anything inside the
    // dead band must hold rung 2.
    for magnification in [1.0, 0.95, 1.05, 0.85, 1.2, 0.81] {
        for _ in 0..MIRROR_RUNG_DWELL_FRAMES * 2 {
            let plan = rungs.observe(Some(magnification), [1920, 1080], 1.0, DESKTOP);
            assert_eq!(
                plan.applied_scale, 2.0,
                "magnification {magnification} is inside the {MIRROR_RUNG_HYSTERESIS}x \
                 dead band and must not give up rung 2",
            );
        }
    }

    // Past the band, it gives the rung back — but only after the dwell.
    let plan = rungs.observe(Some(0.5), [1920, 1080], 1.0, DESKTOP);
    assert_eq!(
        plan.applied_scale, 2.0,
        "a single frame must not move a rung"
    );
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES {
        rungs.observe(Some(0.5), [1920, 1080], 1.0, DESKTOP);
    }
    assert_eq!(
        rungs
            .observe(Some(0.5), [1920, 1080], 1.0, DESKTOP)
            .applied_scale,
        1.0,
    );
}

/// A demand that keeps changing its mind never commits anything.
///
/// This is the sweep case rather than the boundary case: a drag that alternates
/// between wanting two different rungs restarts the dwell each time, so the tile
/// fetcher is never asked for a new zoom mid-gesture.
#[test]
fn an_unsettled_camera_commits_no_rung() {
    let mut rungs = MirrorRungs::default();
    for frame in 0..MIRROR_RUNG_DWELL_FRAMES * 8 {
        let magnification = if frame % 2 == 0 { 4.0 } else { 0.2 };
        let plan = rungs.observe(Some(magnification), [1920, 1080], 1.0, DESKTOP);
        assert_eq!(plan.applied_scale, 1.0, "a flapping demand moved the rung");
    }
}

/// Hiding a floor holds the rung rather than resetting it.
///
/// A pane whose floor is toggled off for a moment — or a frame where the volume
/// is still building — reports no demand. Treating that as "wants rung 1" would
/// drop the tile zoom and throw away the pyramid, and the user would watch the
/// ground go soft and then sharpen again for having toggled something unrelated.
#[test]
fn a_frame_with_no_floor_holds_the_rung() {
    let mut rungs = MirrorRungs::default();
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES + 1 {
        rungs.observe(Some(2.0), [1920, 1080], 1.0, DESKTOP);
    }
    assert_eq!(rungs.tile_zoom_bias(), 1);
    for _ in 0..MIRROR_RUNG_DWELL_FRAMES * 4 {
        let plan = rungs.observe(None, [1920, 1080], 1.0, DESKTOP);
        assert_eq!(plan.applied_scale, 2.0);
    }
    assert_eq!(rungs.tile_zoom_bias(), 1);
}

/// The bias a frame's tiles are drawn with is the one the mirror was sized to.
///
/// Necessarily last frame's: tiles are drawn while the egui pass is open and the
/// mirror is planned after it closes. What must not happen is a bias reported
/// before any plan exists.
#[test]
fn the_tile_bias_is_zero_until_a_mirror_has_actually_been_planned() {
    let rungs = MirrorRungs::default();
    assert_eq!(rungs.tile_zoom_bias(), 0);
}
