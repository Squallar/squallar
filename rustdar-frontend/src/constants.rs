/// Default width for the application window in pixels
pub const RENDER_WIDTH: u32 = 1920;

/// Default height for the application window in pixels
pub const RENDER_HEIGHT: u32 = 1080;

/// Maximum number of concurrent background radar render threads (loop + static).
/// Handhelds have much less RAM, so we cap aggressively to avoid OOM.
#[cfg(mobile)]
pub const MAX_CONCURRENT_RENDERS: usize = 3;
#[cfg(not(mobile))]
pub const MAX_CONCURRENT_RENDERS: usize = 6;

/// Maximum number of loop frames to consider for rendering per dispatch cycle.
///
/// Also the steady-state cap on *textured* frames per pane:
/// `LoopPlaybackState::evict_textures_outside_render_set` is called with this every
/// dispatch and drops the texture of every frame outside the render set. That makes
/// this — not `MAX_LOOP_FRAMES` — the binding term in the per-pane texture budget.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_RENDER_BUDGET: usize = 8;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_RENDER_BUDGET: usize = 12;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_RENDER_BUDGET: usize = 30;

/// Maximum number of concurrent loop scan downloads per pane.
#[cfg(mobile)]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 4;
#[cfg(not(mobile))]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 8;

/// Maximum total number of loop frames kept per pane.
/// Limits combined memory from textures and scan data.
///
/// This caps how many frames a loop *holds*, not how many are textured at once —
/// `MAX_LOOP_RENDER_BUDGET` does that, and is the smaller of the two on every
/// target. See `LOOP_TEXTURE_BUDGET_BYTES` for the resulting memory ceiling.
///
/// # The shape of the `cfg` cascade
///
/// The `not(target_arch = "wasm32")` guard on the desktop and mobile arms is
/// load-bearing, and no build on a machine without a wasm target can show it.
/// wasm32 is the only target where `target_arch = "wasm32"` and `not(mobile)`
/// are true at once: drop that guard and the cascade stays equivalent everywhere
/// it is compiled today, while wasm32 gets two definitions of the same constant
/// and fails with `error[E0428]`. `cfg` arms have no ordering and no
/// fallthrough, so exclusivity is the only thing keeping them apart. Every
/// constant below follows the same three-arm shape for that reason.
///
/// `mobile` is emitted by this crate's `build.rs` for Android and iOS. It
/// replaced `target_os = "android"` because the distinction being made is how
/// much memory the device has, not which OS it runs — and iOS needs the same
/// answer. Every target that exists today selects exactly the arm it did before.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_FRAMES: usize = 12;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_FRAMES: usize = 20;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_FRAMES: usize = 60;

/// Ceiling on what one pane's loop textures may occupy, in bytes.
///
/// Not a runtime check — nothing measures against it. It is the budget the
/// per-target constants were chosen to fit, written down so that raising any of
/// them has to be a deliberate decision about memory rather than an unnoticed
/// side effect. `loop_frames_fit_the_target_texture_budget` enforces it.
///
/// The textured-frame count is `min(MAX_LOOP_FRAMES, MAX_LOOP_RENDER_BUDGET)`, not
/// `MAX_LOOP_FRAMES`: `evict_textures_outside_render_set` runs every dispatch and
/// strips the texture off every frame outside the render set, so the frames a loop
/// *holds* and the frames that are *textured* are different numbers. Budgeting on
/// `MAX_LOOP_FRAMES` alone overstates desktop by 2x.
///
/// | target  | held | textured | frame size | total   | budget  |
/// |---------|-----:|---------:|-----------:|--------:|--------:|
/// | desktop |   60 |       30 |     16 MiB | 480 MiB | 512 MiB |
/// | mobile  |   20 |       12 |     16 MiB | 192 MiB | 256 MiB |
/// | wasm32  |   12 |        8 |      4 MiB |  32 MiB |  48 MiB |
///
/// wasm32's is the tight one: the whole linear memory is capped at 4 GiB, and the
/// loop is only one of several things competing for it.
#[cfg(target_arch = "wasm32")]
pub const LOOP_TEXTURE_BUDGET_BYTES: usize = 48 * 1024 * 1024;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_TEXTURE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_TEXTURE_BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// Maximum number of entries kept in `RenderDispatcher::render_cache`.
///
/// The cache exists so panes showing the same site/product/elevation share one
/// render; it is not a history. Each entry holds an RGBA image and a matching
/// `f32` value grid — `IMAGE_SIZE² × 8` bytes, 32 MiB at 2048² — and until this
/// bound existed the only thing that ever removed one was `reset_panes*`, so a
/// user cycling products accumulated them without limit.
///
/// Sized to comfortably exceed the pane count (`MAX_PANES_DESKTOP` is 6,
/// `MAX_PANES_MOBILE` is 4) so the panes on screen can never evict each other,
/// with a little headroom for switching back and forth.
#[cfg(mobile)]
pub const MAX_RENDER_CACHE_ENTRIES: usize = 6;
#[cfg(not(mobile))]
pub const MAX_RENDER_CACHE_ENTRIES: usize = 8;

/// A handheld target must have been given the `mobile` cfg.
///
/// This is the control on `build.rs` actually running. If it is deleted, or the
/// manifest stops pointing at it, or its condition is wrong, `mobile` is simply
/// never set — and every cascade above then silently selects the *desktop* arm.
/// On a phone that means `MAX_CONCURRENT_RENDERS` 6 instead of 3 and a 512 MiB
/// texture budget instead of 256 MiB, which is an OOM, not a warning.
///
/// `rustc-check-cfg` alone does not cover this: a missing build script turns
/// each `mobile` arm into an `unexpected_cfgs` warning, and nothing in CI turns
/// warnings into failures (`clippy.yaml` ends with a bare `cargo clippy`). This
/// does not depend on CI — the build simply stops.
#[cfg(all(any(target_os = "android", target_os = "ios"), not(mobile)))]
compile_error!(
    "the `mobile` cfg is not set on a handheld target: rustdar-frontend's \
     build.rs did not run, or its target list is wrong. Without it this crate \
     would compile desktop memory budgets into a mobile build."
);

/// Sanity of the `cfg` cascades above, checked at compile time so the arm a future
/// wasm build selects is validated the moment that target exists — a `#[test]` only
/// ever exercises the arm the test runner itself was built for.
const _: () = const {
    assert!(MAX_LOOP_FRAMES > 0);
    assert!(MAX_LOOP_RENDER_BUDGET > 0);
    assert!(LOOP_TEXTURE_BUDGET_BYTES > 0);
    assert!(MAX_RENDER_CACHE_ENTRIES > 0);
    assert!(MAX_CONCURRENT_RENDERS > 0);
    assert!(MAX_CONCURRENT_LOOP_DOWNLOADS > 0);
    // Eviction is what bounds the textured-frame count, so it must bind first.
    assert!(MAX_LOOP_RENDER_BUDGET <= MAX_LOOP_FRAMES);
    // Every render path indexes a square image; the projection assumes a power of two.
    assert!(rustdar_radar::types::IMAGE_SIZE.is_power_of_two());
};

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_radar::types::IMAGE_SIZE;

    /// Bytes one loop frame's texture occupies: RGBA at `IMAGE_SIZE²`.
    /// Loop frames carry no value grid — `poll_loop_render_results` stores an empty
    /// one — so this is the whole cost, unlike a static pane render.
    const fn loop_frame_bytes() -> usize {
        IMAGE_SIZE * IMAGE_SIZE * 4
    }

    /// Frames that hold a texture at once. `evict_textures_outside_render_set` runs
    /// every dispatch with `MAX_LOOP_RENDER_BUDGET`, so a loop of `MAX_LOOP_FRAMES`
    /// keeps only the render set textured.
    const fn textured_frames() -> usize {
        if MAX_LOOP_RENDER_BUDGET < MAX_LOOP_FRAMES { MAX_LOOP_RENDER_BUDGET } else { MAX_LOOP_FRAMES }
    }

    /// The ceiling the per-target constants were chosen to fit. Whichever arm this
    /// build compiled, its own constants are what get checked — so the wasm arm is
    /// held to the wasm budget when built for wasm, and desktop to desktop's here.
    #[test]
    fn loop_frames_fit_the_target_texture_budget() {
        let total = textured_frames() * loop_frame_bytes();
        assert!(
            total <= LOOP_TEXTURE_BUDGET_BYTES,
            "{} textured frames x {IMAGE_SIZE}^2 x 4B = {} MiB, over the {} MiB budget",
            textured_frames(),
            total / (1024 * 1024),
            LOOP_TEXTURE_BUDGET_BYTES / (1024 * 1024),
        );
    }

    /// The budget is meant to be snug. A ceiling several times the real figure would
    /// pass the check above while permitting a silent doubling of any constant in it.
    #[test]
    fn the_budget_is_not_slack_enough_to_hide_a_doubling() {
        let total = textured_frames() * loop_frame_bytes();
        assert!(
            total * 2 > LOOP_TEXTURE_BUDGET_BYTES,
            "budget {} MiB is more than twice the actual {} MiB — it would not catch a regression",
            LOOP_TEXTURE_BUDGET_BYTES / (1024 * 1024),
            total / (1024 * 1024),
        );
    }

    /// The eviction budget is what bounds memory, so it has to be the smaller of the
    /// two. If it ever exceeded the frame cap, `render_set_indices` would clamp it
    /// back to the frame count and every held frame would stay textured — silently
    /// restoring the `MAX_LOOP_FRAMES × frame` figure the budget above rules out.
    /// The ordering itself is asserted at compile time next to the constants; this
    /// pins the consequence the budget arithmetic depends on.
    #[test]
    fn the_render_budget_is_what_bounds_the_textured_frames() {
        assert_eq!(textured_frames(), MAX_LOOP_RENDER_BUDGET);
    }

}
