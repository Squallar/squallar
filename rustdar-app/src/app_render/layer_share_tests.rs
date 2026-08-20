//! **WO-E7d: the pane's frame budget divides across the layers it animates.**
//!
//! The cap is a texture-memory allowance for the whole pane, so a pane
//! animating radar *and* a model field at once must not spend it twice.

use super::layer_share;
use rustdar_device_profile::constants::{
    DESKTOP_MAX_LOOP_FRAMES, MOBILE_MAX_LOOP_FRAMES, WASM_MAX_LOOP_FRAMES,
};

/// Every platform's real cap, so the division is exercised against the numbers
/// that ship rather than against a round number chosen here.
const CAPS: [usize; 3] = [
    WASM_MAX_LOOP_FRAMES,
    MOBILE_MAX_LOOP_FRAMES,
    DESKTOP_MAX_LOOP_FRAMES,
];

/// **The no-op proof, and it is the load-bearing one**: every pane in this
/// build animates exactly one layer, so this land must not move a single
/// frame count. The one-layer answer is the budget *untouched* — not
/// `budget / 1` with the floor applied, which would silently raise a view
/// whose own allowance is legitimately below two.
#[test]
fn a_pane_animating_one_layer_keeps_the_whole_frame_budget() {
    for budget in [0usize, 1, 2, 3, 7]
        .into_iter()
        .chain(CAPS)
        .chain([100, 1000])
    {
        for animating in [0usize, 1] {
            assert_eq!(
                layer_share(budget, animating),
                budget,
                "a pane animating {animating} layer(s) must be handed the \
                 budget exactly as it stands, and {budget} became something \
                 else",
            );
        }
    }
    assert_eq!(
        layer_share(1, 1),
        1,
        "a one-frame allowance stays one frame - the floor belongs to the \
         division and there was no division",
    );
}

/// The division itself, at every shipped cap, stated as arithmetic that does
/// not go back through `layer_share` to decide what it should be.
#[test]
fn the_frame_budget_divides_across_the_layers_a_pane_animates() {
    // Two animating layers on each platform, spelled out.
    assert_eq!(
        layer_share(WASM_MAX_LOOP_FRAMES, 2),
        7,
        "14 frames, 2 layers"
    );
    assert_eq!(
        layer_share(MOBILE_MAX_LOOP_FRAMES, 2),
        10,
        "20 frames, 2 layers"
    );
    assert_eq!(
        layer_share(DESKTOP_MAX_LOOP_FRAMES, 2),
        30,
        "60 frames, 2 layers"
    );
    assert_eq!(
        layer_share(WASM_MAX_LOOP_FRAMES, 3),
        4,
        "14 / 3 rounds down"
    );

    // The shares never oversubscribe the cap, which is the point of dividing.
    for cap in CAPS {
        for animating in 2..=8 {
            let share = layer_share(cap, animating);
            assert!(
                share * animating <= cap || share == 2,
                "{animating} layers at {share} frames each overruns a cap of \
                 {cap}, and it is not the floor that put them there",
            );
        }
    }
}

/// **The floor, and the wasm arm is where it bites.** A 14-frame cap divided
/// eight ways is one frame each, and one frame is a still picture: a layer
/// that cannot hold two cannot animate at all. The floor is where the budget
/// stops being divisible, and it is allowed to oversubscribe — the
/// alternative is a layer that is on and shows nothing moving.
#[test]
fn no_animating_layer_is_cut_below_two_frames() {
    for cap in CAPS {
        for animating in 2..=64 {
            assert!(
                layer_share(cap, animating) >= 2,
                "a cap of {cap} across {animating} layers left one of them \
                 below two frames, which is a layer that cannot animate",
            );
        }
    }
    // Named on the wasm arm, whose 14 is the tightest cap that ships.
    assert_eq!(
        layer_share(WASM_MAX_LOOP_FRAMES, 7),
        2,
        "14 / 7 is exactly the floor",
    );
    assert_eq!(
        layer_share(WASM_MAX_LOOP_FRAMES, 8),
        2,
        "and 14 / 8 would be 1, so the floor is what answers",
    );
    assert_eq!(WASM_MAX_LOOP_FRAMES, 14, "the wasm cap this pins against");
    // Non-triviality floor: the division really does produce answers other
    // than the floor, or "never below two" would be true of a constant.
    assert!(
        (2..=8).any(|n| layer_share(WASM_MAX_LOOP_FRAMES, n) > 2),
        "some division on the wasm arm lands above the floor",
    );
}
