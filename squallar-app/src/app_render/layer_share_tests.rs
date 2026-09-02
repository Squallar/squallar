//! **WB-7: a pane's loop allowance divides across the layers it animates in
//! BYTES, not in frame count.**
//!
//! WO-E7d divided the count, equally. That is right only while every animating
//! layer's frame costs the same, and they never did: a radar plan-view frame is
//! `LOOP_IMAGE_SIZE`² × 4 and a model or satellite frame is the pane's own
//! raster. An equal count split hands the expensive layer the same number of
//! frames as the cheap one and oversubscribes the share to pay for it.
//!
//! **Every arm is spelled from its own constants**, because this runs on one
//! of the three and the other two are the ones the arithmetic bites hardest
//! on. The end-to-end wiring — that these numbers are what a real pane's frame
//! lists come out at, through the real listing arrivals — is
//! `loop_overlay_render_tests::a_pane_animating_two_layers_divides_its_bytes_and_not_its_frame_count`.

use super::layer_share;
use crate::loop_pool::{LoopAllocation, LoopDemand, LoopFrameModel, LoopPool, LoopPoolLimits};
use squallar_device_profile::constants::{
    DESKTOP_LOOP_IMAGE_SIZE, DESKTOP_LOOP_POOL_CEILING_BYTES, DESKTOP_LOOP_POOL_FLOOR_BYTES,
    DESKTOP_MAX_LOOP_FRAMES, DESKTOP_MAX_LOOP_RENDER_BUDGET, MIN_LOOP_FRAMES_PER_PANE,
    MOBILE_LOOP_IMAGE_SIZE, MOBILE_LOOP_POOL_CEILING_BYTES, MOBILE_LOOP_POOL_FLOOR_BYTES,
    MOBILE_MAX_LOOP_FRAMES, MOBILE_MAX_LOOP_RENDER_BUDGET, WASM_LOOP_IMAGE_SIZE,
    WASM_LOOP_POOL_CEILING_BYTES, WASM_LOOP_POOL_FLOOR_BYTES, WASM_MAX_LOOP_FRAMES,
    WASM_MAX_LOOP_RENDER_BUDGET,
};

/// One shipped arm, from its own constants rather than from round numbers
/// chosen here.
struct Arm {
    name: &'static str,
    /// `LOOP_IMAGE_SIZE`² × 4 — what one radar plan-view loop frame costs.
    plan_view: usize,
    /// `MAX_LOOP_FRAMES` — the radar frame **list** length, which is the count
    /// cap and not a byte figure.
    frames_held: usize,
    pool_floor: usize,
    pool_ceiling: usize,
    render_budget: usize,
}

fn arms() -> [Arm; 3] {
    [
        Arm {
            name: "wasm32",
            plan_view: WASM_LOOP_IMAGE_SIZE * WASM_LOOP_IMAGE_SIZE * 4,
            frames_held: WASM_MAX_LOOP_FRAMES,
            pool_floor: WASM_LOOP_POOL_FLOOR_BYTES,
            pool_ceiling: WASM_LOOP_POOL_CEILING_BYTES,
            render_budget: WASM_MAX_LOOP_RENDER_BUDGET,
        },
        Arm {
            name: "mobile",
            plan_view: MOBILE_LOOP_IMAGE_SIZE * MOBILE_LOOP_IMAGE_SIZE * 4,
            frames_held: MOBILE_MAX_LOOP_FRAMES,
            pool_floor: MOBILE_LOOP_POOL_FLOOR_BYTES,
            pool_ceiling: MOBILE_LOOP_POOL_CEILING_BYTES,
            render_budget: MOBILE_MAX_LOOP_RENDER_BUDGET,
        },
        Arm {
            name: "desktop",
            plan_view: DESKTOP_LOOP_IMAGE_SIZE * DESKTOP_LOOP_IMAGE_SIZE * 4,
            frames_held: DESKTOP_MAX_LOOP_FRAMES,
            pool_floor: DESKTOP_LOOP_POOL_FLOOR_BYTES,
            pool_ceiling: DESKTOP_LOOP_POOL_CEILING_BYTES,
            render_budget: DESKTOP_MAX_LOOP_RENDER_BUDGET,
        },
    ]
}

impl Arm {
    /// The allocation one loop gets on an idle application of this arm: the
    /// whole pool, at its floor, undivided.
    fn allocation(&self) -> LoopAllocation {
        let limits = LoopPoolLimits {
            floor: self.pool_floor,
            ceiling: self.pool_ceiling,
        };
        let model = LoopFrameModel {
            plan_view: self.plan_view,
            section: self.plan_view / 2,
            grid: self.plan_view,
            overlay: self.plan_view,
            render_budget: self.render_budget,
            list_cap: self.frames_held,
        };
        LoopPool::new(limits.floor, limits).plan(model, &LoopDemand::default())
    }
}

/// **The no-op proof, and it is the load-bearing one: a pane animating radar
/// alone gets the same count it got before WB-7, on every arm.**
///
/// Radar's whole-pane budget is a frame **list** length — downloaded scans, of
/// which only the render set is ever textured — so the texture bytes have
/// nothing to say about it while nothing else is competing for them. The
/// undivided answer is the count cap exactly: **14 / 20 / 60**, unmoved.
///
/// **The non-triviality is the second block**, and it is why "unchanged" is
/// not vacuous here: on mobile and desktop the byte division would answer
/// something else (**18** against 20, **36** against 60), so an implementation
/// that had let the bytes bind on the undivided arm would fail this. On wasm
/// the two coincide at 14 — 56 MiB of pool over a 4 MiB frame — which is a
/// property of how `LOOP_POOL_FLOOR_BYTES` was derived and is stated here so
/// that arm is not read as evidence either way.
#[test]
fn a_pane_animating_radar_alone_keeps_the_count_it_had_before_wb7() {
    let mut differed = 0;
    for arm in arms() {
        let allocation = arm.allocation();
        for animating in [0usize, 1] {
            assert_eq!(
                layer_share(
                    &allocation,
                    0,
                    Some(arm.frames_held),
                    arm.plan_view,
                    animating,
                ),
                arm.frames_held,
                "{}: a pane animating {animating} layer(s) must be handed \
                 radar's list length exactly as it stands, and {} became \
                 something else",
                arm.name,
                arm.frames_held,
            );
        }
        // The undivided answer is not the byte division wearing its clothes.
        let by_bytes = allocation.share_bytes / arm.plan_view;
        if by_bytes != arm.frames_held {
            differed += 1;
        }
    }
    assert!(
        differed >= 2,
        "non-triviality: on at least two arms the byte division must answer \
         something other than the count cap, or this test passes for an \
         implementation that let the bytes bind on the undivided arm too",
    );
}

/// **The division itself: two animating layers whose frames cost 4:1 hold
/// frames 1:4, and the two of them together fit the one share.**
///
/// The cheap layer is a quarter of a radar plan-view frame on each arm, so the
/// expected ratio is the same number on all three and no arm can pass on a
/// figure chosen for it. Per arm, at the pool floor, two animating layers:
///
/// | arm | share | radar frame | radar | cheap | total |
/// |---|---:|---:|---:|---:|---:|
/// | wasm | 56 MiB | 4 MiB | 7 | 28 | **56 MiB** |
/// | mobile | 288 MiB | 16 MiB | 9 | 36 | **288 MiB** |
/// | desktop | 576 MiB | 16 MiB | 18 | 72 | **576 MiB** |
///
/// **Floor — `divide_the_count`: put `(whole_pane / animating).max(2)` back**,
/// where `whole_pane` is the count cap for radar and `share_bytes / frame_bytes`
/// for the other. Mobile answers 10 and 36 (ratio 3.6) and desktop 30 and 72
/// (ratio 2.4), so the ratio assertion reds on both — and their totals go to
/// 304 MiB and 768 MiB, over floors of 288 and 576, so the byte assertion reds
/// on both too. **wasm passes under that mutation** and is not evidence: its
/// 14-frame cap and its 56 MiB / 4 MiB byte answer are the same number by
/// construction. Applied and observed.
#[test]
fn two_animating_layers_divide_the_bytes_and_not_the_count() {
    for arm in arms() {
        let allocation = arm.allocation();
        let cheap_bytes = arm.plan_view / 4;

        let radar = layer_share(&allocation, 0, Some(arm.frames_held), arm.plan_view, 2);
        let cheap = layer_share(&allocation, 0, None, cheap_bytes, 2);

        assert_ne!(
            radar, cheap,
            "{}: a 4 MiB frame and a 16 MiB frame came out at the same count, \
             so the share was split by count and not by bytes",
            arm.name,
        );
        assert_eq!(
            cheap,
            radar * 4,
            "{}: a layer whose frames cost a quarter as much must hold four \
             times as many — {radar} against {cheap}, from {} B of share at \
             {} B and {cheap_bytes} B a frame",
            arm.name,
            allocation.share_bytes,
            arm.plan_view,
        );
        assert!(
            radar * arm.plan_view + cheap * cheap_bytes <= arm.pool_floor,
            "{}: {radar} frames at {} B and {cheap} at {cheap_bytes} B is {} B, \
             over this arm's {} B pool floor — the point of dividing is that \
             the two together fit",
            arm.name,
            arm.plan_view,
            radar * arm.plan_view + cheap * cheap_bytes,
            arm.pool_floor,
        );
    }
}

/// **The floor, and the wasm arm is where it bites.** A share divided eight
/// ways buys one frame each of a 4 MiB frame on a 56 MiB pool, and one frame is
/// a still picture: a layer that cannot hold two cannot animate at all. The
/// floor is where the allowance stops being divisible, and it is allowed to
/// oversubscribe — the alternative is a layer that is on and shows nothing
/// moving.
///
/// It is **not** applied to the undivided radar answer: a view whose own
/// allowance is legitimately below two must not be raised to two by a division
/// that did not happen, which the last block pins.
#[test]
fn no_animating_layer_is_cut_below_two_frames() {
    for arm in arms() {
        let allocation = arm.allocation();
        for animating in 2..=64 {
            for cap in [Some(arm.frames_held), None] {
                assert!(
                    layer_share(&allocation, 0, cap, arm.plan_view, animating) >= 2,
                    "{}: {animating} layers left one of them below two frames, \
                     which is a layer that cannot animate",
                    arm.name,
                );
            }
        }
        // A frame that costs nothing is a model built wrong; it must answer
        // the floor rather than divide by zero or become unbounded.
        assert_eq!(
            layer_share(&allocation, 0, None, 0, 2),
            MIN_LOOP_FRAMES_PER_PANE,
            "{}: a zero-byte frame did not answer the floor",
            arm.name,
        );
        // Non-triviality: the division really does produce answers other than
        // the floor, or "never below two" would be true of a constant.
        assert!(
            (2..=8).any(|n| layer_share(&allocation, 0, None, arm.plan_view, n) > 2),
            "{}: some division must land above the floor",
            arm.name,
        );
        // **The interaction, stated rather than left to chance**: at the
        // number of animating layers where the floor bites, the frames it
        // hands out cost MORE than the share — knowingly, because two frames
        // is where a loop stops being a loop. On wasm that is eight layers of
        // a 4 MiB frame against a 56 MiB pool: 8 x 2 x 4 MiB = 64 MiB.
        let crowded = 32;
        let each = layer_share(&allocation, 0, None, arm.plan_view, crowded);
        assert_eq!(
            each, MIN_LOOP_FRAMES_PER_PANE,
            "{}: {crowded} animating layers must be the floor's arm",
            arm.name,
        );
        assert!(
            crowded * each * arm.plan_view > allocation.share_bytes,
            "{}: the floor is documented as exceeding the byte bound when it \
             bites, and here it did not — {crowded} layers x {each} frames x \
             {} B against {} B of share. If this ever fits, the floor is not \
             the thing being tested.",
            arm.name,
            arm.plan_view,
            allocation.share_bytes,
        );
    }
    // The undivided arm keeps a sub-floor allowance, because there was no
    // division to floor.
    let allocation = arms()[0].allocation();
    assert_eq!(
        layer_share(&allocation, 0, Some(1), arms()[0].plan_view, 1),
        1,
        "a one-frame allowance stays one frame — the floor belongs to the \
         division and there was no division",
    );
}
