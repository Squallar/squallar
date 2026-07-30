// Reached only for `wgpu::Limits::downlevel_webgl2_defaults()`, so that the
// WebGL2 3D-texture floor below is the value the device request is held to
// rather than a 256 written out by hand. Deliberately the `egui_wgpu` re-export
// and not the direct `wgpu` dependency: `tests/wgpu_guard.rs` asserts that
// `app.rs` is the only file naming `::wgpu`, because a second copy configured by
// this crate is a copy nothing renders through.
use egui_wgpu::wgpu;

/// Default width for the application window in pixels
pub const RENDER_WIDTH: u32 = 1920;

/// Default height for the application window in pixels
pub const RENDER_HEIGHT: u32 = 1080;

/// Maximum number of concurrent background radar renders (loop + static).
/// Handhelds have much less RAM, so we cap aggressively to avoid OOM.
///
/// The web arm is not a memory cap but a *worker* cap: the browser has one
/// rasterization worker, so anything past the first only queues behind it. It
/// used to take the desktop 6 while `offload` ran jobs inline, which meant six
/// renders could run back to back inside a single frame — six times the stall
/// this cap exists to bound. Raise it in step with the worker pool, not alone.
///
/// The three arms are named outside the cascade for the reason
/// [`WASM_VOLUME_GRID_CELLS`] gives: a `cfg`-selected literal can only be
/// checked by the target that compiles it, and this workspace runs `cargo test`
/// on exactly one of the three.
pub const WASM_MAX_CONCURRENT_RENDERS: usize = 1;
/// The mobile arm. See [`WASM_MAX_CONCURRENT_RENDERS`].
pub const MOBILE_MAX_CONCURRENT_RENDERS: usize = 3;
/// The desktop arm. See [`WASM_MAX_CONCURRENT_RENDERS`].
pub const DESKTOP_MAX_CONCURRENT_RENDERS: usize = 6;

/// See [`WASM_MAX_CONCURRENT_RENDERS`].
#[cfg(target_arch = "wasm32")]
pub const MAX_CONCURRENT_RENDERS: usize = WASM_MAX_CONCURRENT_RENDERS;
/// See [`WASM_MAX_CONCURRENT_RENDERS`].
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_CONCURRENT_RENDERS: usize = MOBILE_MAX_CONCURRENT_RENDERS;
/// See [`WASM_MAX_CONCURRENT_RENDERS`].
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_CONCURRENT_RENDERS: usize = DESKTOP_MAX_CONCURRENT_RENDERS;

/// Maximum number of loop frames to consider for rendering per dispatch cycle,
/// on wasm32. See [`MAX_LOOP_RENDER_BUDGET`]; named outside the cascade for the
/// reason [`WASM_VOLUME_GRID_CELLS`] gives.
pub const WASM_MAX_LOOP_RENDER_BUDGET: usize = 8;
/// The mobile arm. See [`MAX_LOOP_RENDER_BUDGET`].
pub const MOBILE_MAX_LOOP_RENDER_BUDGET: usize = 12;
/// The desktop arm. See [`MAX_LOOP_RENDER_BUDGET`].
pub const DESKTOP_MAX_LOOP_RENDER_BUDGET: usize = 30;

/// Maximum number of loop frames to consider for rendering per dispatch cycle.
///
/// Also the steady-state cap on *textured* frames per pane:
/// `LoopPlaybackState::evict_textures_outside_render_set` is called with this every
/// dispatch and drops the texture of every frame outside the render set. That makes
/// this — not `MAX_LOOP_FRAMES` — the binding term in the per-pane texture budget.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_RENDER_BUDGET: usize = WASM_MAX_LOOP_RENDER_BUDGET;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_RENDER_BUDGET: usize = MOBILE_MAX_LOOP_RENDER_BUDGET;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_RENDER_BUDGET: usize = DESKTOP_MAX_LOOP_RENDER_BUDGET;

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
///
/// The three arms are named outside the cascade, like every other cascade in
/// this file. See [`WASM_VOLUME_GRID_CELLS`] for why.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_FRAMES: usize = WASM_MAX_LOOP_FRAMES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_FRAMES: usize = MOBILE_MAX_LOOP_FRAMES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_FRAMES: usize = DESKTOP_MAX_LOOP_FRAMES;

/// The wasm32 arm of [`MAX_LOOP_FRAMES`].
pub const WASM_MAX_LOOP_FRAMES: usize = 12;
/// The mobile arm. See [`MAX_LOOP_FRAMES`].
pub const MOBILE_MAX_LOOP_FRAMES: usize = 20;
/// The desktop arm. See [`MAX_LOOP_FRAMES`].
pub const DESKTOP_MAX_LOOP_FRAMES: usize = 60;

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
///
/// The table above is the *claim*; the three arms are named outside the cascade
/// so `loop_frames_fit_the_target_texture_budget` can check every row of it from
/// one host build rather than only the row that build compiled.
#[cfg(target_arch = "wasm32")]
pub const LOOP_TEXTURE_BUDGET_BYTES: usize = WASM_LOOP_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_TEXTURE_BUDGET_BYTES: usize = MOBILE_LOOP_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_LOOP_TEXTURE_BUDGET_BYTES;

/// The wasm32 arm of [`LOOP_TEXTURE_BUDGET_BYTES`].
pub const WASM_LOOP_TEXTURE_BUDGET_BYTES: usize = 48 * 1024 * 1024;
/// The mobile arm. See [`LOOP_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_LOOP_TEXTURE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// The desktop arm. See [`LOOP_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_LOOP_TEXTURE_BUDGET_BYTES: usize = 512 * 1024 * 1024;

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

/// The per-device-class voxel grid dimensions, named **outside** the `cfg`
/// cascade so that all three are reachable from any target's tests.
///
/// A `cfg`-selected constant can only be checked by the target that compiles
/// it, and this workspace runs `cargo test` on exactly one of the three. Spelt
/// as literals inside the cascade, two of the three could be edited freely:
/// the review that landed WP-C proved it by changing the wasm arm to
/// `[160, 160, 80]` and watching the whole suite pass 1507/0 with the wasm
/// `--all-targets` check exiting 0. That is the one-sided shape of the
/// `needs_whole_volume` / `RenderInput::extract` divergence, and it is exactly
/// what `the_grid_dimensions_match_the_shapes_rustdar_radar_names` exists to
/// prevent — so the binding has to reach all three arms, and it can only do
/// that if all three have names.
///
/// These are the frontend's copy of `rustdar_radar::voxel`'s `WASM_SHAPE`,
/// `MOBILE_SHAPE` and `DESKTOP_SHAPE`. The duplication is forced rather than
/// careless: only *this* crate's `build.rs` emits `mobile`, so only this crate
/// can pick the middle arm, while the grid is built in `rustdar-radar`, which
/// therefore has to name all three and let a caller choose.
pub const WASM_VOLUME_GRID_CELLS: [u32; 3] = [128, 128, 64];
/// The mobile arm. See [`WASM_VOLUME_GRID_CELLS`].
pub const MOBILE_VOLUME_GRID_CELLS: [u32; 3] = [192, 192, 96];
/// The desktop arm. See [`WASM_VOLUME_GRID_CELLS`].
pub const DESKTOP_VOLUME_GRID_CELLS: [u32; 3] = [256, 256, 128];

/// Cells along x, y and z in the Cartesian voxel grid a 3D volume renders from.
///
/// Every axis is at or under 256 because that is what GLES 3.0 — and so WebGL2 —
/// *guarantees*, which is the floor a phone browser may legitimately report. See
/// [`WEBGL2_MAX_TEXTURE_DIMENSION_3D`]. One code path satisfying that floor was
/// chosen over a larger desktop variant: 256 cells over a 40 km half-width is
/// 0.31 km per cell, already finer than the 1 km cube the design was compared
/// against.
///
/// The cascade shape is the one [`MAX_LOOP_FRAMES`] documents, for the reason it
/// documents. `mobile` is emitted by *this crate's* `build.rs`, so a copy of this
/// constant placed in `rustdar-egui` or `rustdar-radar` would silently take the
/// desktop arm on a phone.
///
/// The three arms select between [`WASM_VOLUME_GRID_CELLS`],
/// [`MOBILE_VOLUME_GRID_CELLS`] and [`DESKTOP_VOLUME_GRID_CELLS`] rather than
/// repeating their literals, so the selection is the only thing here that a
/// host build cannot check.
#[cfg(target_arch = "wasm32")]
pub const VOLUME_GRID_CELLS: [u32; 3] = WASM_VOLUME_GRID_CELLS;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_GRID_CELLS: [u32; 3] = MOBILE_VOLUME_GRID_CELLS;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_GRID_CELLS: [u32; 3] = DESKTOP_VOLUME_GRID_CELLS;

/// Bytes in the colour lookup table that travels with a voxel grid.
///
/// The grid holds one-byte palette indices, so the table is the 256 RGBA entries
/// they index — 1 KiB, on every target. It carries **alpha**, which is what makes
/// the per-product transparency floors the raymarcher's transfer function for
/// free, so it cannot be dropped to three bytes per entry.
pub const VOLUME_LUT_BYTES: usize = 256 * 4;

/// The largest 3D texture WebGL2 is *guaranteed* to accept, per axis.
///
/// Taken from wgpu's own WebGL2 downlevel limits rather than written as 256, so
/// it cannot drift from the value the device request is actually held to
/// (`app_state::device_limits`). Note the web arm of that function calls
/// `using_resolution`, which *lifts* `max_texture_dimension_3d` to whatever the
/// adapter reports (wgpu-types 29.0.4 `limits.rs:603-610`) — so this is a floor
/// the grid must fit, not a ceiling it is held to. The grid above fits the
/// unlifted floor on every target, which is the point: no runtime step-down is
/// needed for a device that reports exactly the guarantee.
pub const WEBGL2_MAX_TEXTURE_DIMENSION_3D: u32 =
    wgpu::Limits::downlevel_webgl2_defaults().max_texture_dimension_3d;

/// Ceiling on what one pane's 3D volume textures may occupy, in bytes.
///
/// Not a runtime check — nothing measures against it, exactly like
/// [`LOOP_TEXTURE_BUDGET_BYTES`]. It is the budget [`VOLUME_GRID_CELLS`] was
/// chosen to fit, written down so that growing an axis has to be a deliberate
/// decision about memory. `the_volume_grid_fits_the_target_texture_budget`
/// enforces it and `the_volume_budget_is_not_slack_enough_to_hide_a_doubling`
/// keeps it snug.
///
/// One pane shows one volume, so the figure is one `R8Unorm` grid plus its LUT:
///
/// | target  | grid          | grid bytes | + LUT     | budget    |
/// |---------|---------------|-----------:|----------:|----------:|
/// | desktop | 256x256x128   |      8 MiB |  8.001 MiB|    12 MiB |
/// | mobile  | 192x192x96    |  3.375 MiB |  3.376 MiB|     5 MiB |
/// | wasm32  | 128x128x64    |      1 MiB |  1.001 MiB|   1.5 MiB |
///
/// Every arm keeps ~1.5x headroom, which is deliberate: enough for the alignment
/// and driver overhead a real 3D texture allocation carries, not enough to hide
/// a doubled axis.
///
/// **This budgets the volume texture only.** The pane-sized `Rgba8Unorm`
/// offscreen target the raymarch renders into is a separate cost, and it has
/// its own line: [`VOLUME_OFFSCREEN_BUDGET_BYTES`]. Folding the two together
/// would make this ceiling untestable against [`VOLUME_GRID_CELLS`], which is
/// the only thing it can be checked against, and would leave a doubled grid
/// axis hiding inside the offscreen's slack.
///
/// Named outside the cascade, like [`VOLUME_GRID_CELLS`] itself: budgeting the
/// grid arm-by-arm is only possible if both sides of every row of the table
/// above have names a host build can reach.
#[cfg(target_arch = "wasm32")]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = WASM_VOLUME_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = MOBILE_VOLUME_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES;

/// The wasm32 arm of [`VOLUME_TEXTURE_BUDGET_BYTES`].
pub const WASM_VOLUME_TEXTURE_BUDGET_BYTES: usize = 1536 * 1024;
/// The mobile arm. See [`VOLUME_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_VOLUME_TEXTURE_BUDGET_BYTES: usize = 5 * 1024 * 1024;
/// The desktop arm. See [`VOLUME_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES: usize = 12 * 1024 * 1024;

/// The largest pane, in physical pixels, the offscreen budget is sized for.
///
/// Not a cascade, deliberately. A phone in landscape is about 2.6 Mpx and a
/// browser canvas on a 1440p display is 3.7 Mpx, so one figure bounds every
/// target; what differs per target is the *rung* applied to it
/// (`volume::quality::PLATFORM_CEILING`), and that is where the per-target
/// judgement belongs. Splitting it into two constants that both vary would let
/// them drift against each other with nothing to notice.
///
/// A pane larger than this is not refused — `VolumeQuality::fit` steps down the
/// resolution ladder and, at the bottom, shrinks proportionally. This figure is
/// what the budget below is *checked* against, not a limit the code enforces.
pub const VOLUME_OFFSCREEN_REFERENCE_PANE_PX: [u32; 2] = [2560, 1440];

/// Ceiling on the pane-sized `Rgba8Unorm` target one volume renders into.
///
/// Unlike [`LOOP_TEXTURE_BUDGET_BYTES`] and [`VOLUME_TEXTURE_BUDGET_BYTES`],
/// **this one is enforced at runtime**: `VolumeQuality::fit` walks down the
/// resolution ladder until the offscreen fits it. That makes it a real bound on
/// fill rate as well as on memory, which is the point — the offscreen exists so
/// that resolution is tunable independently of pane size, and a budget is the
/// only thing that makes the tuning happen without a human in the loop.
///
/// At [`VOLUME_OFFSCREEN_REFERENCE_PANE_PX`], with each target's own quality
/// ceiling applied:
///
/// | target  | rung   | offscreen   | bytes     | budget |
/// |---------|--------|-------------|----------:|-------:|
/// | desktop | Native | 2560 x 1440 | 14.06 MiB | 20 MiB |
/// | mobile  | Half   | 1280 x 720  |  3.52 MiB |  5 MiB |
/// | wasm32  | Half   | 1280 x 720  |  3.52 MiB |  5 MiB |
///
/// Every arm keeps about 1.4x headroom, the same shape the two budgets above
/// keep and for the same reason: enough for the alignment a real allocation
/// carries, not enough to hide a doubling.
///
/// Consequence worth stating rather than discovering: a maximised pane on a 4K
/// display is 31.6 MiB at `Native`, so it steps to `Half` and is upscaled by
/// the blit's `Linear` sampler. On the measured hardware that is also the right
/// call for fill rate — 4K native extrapolates to about 4 ms of a 16.7 ms frame
/// for one pane.
/// The three budgets, named **outside** the cascade so all three are reachable
/// from any target's tests — the shape [`WASM_VOLUME_GRID_CELLS`] uses, for the
/// reason it gives. Two of three arms would otherwise be editable freely, since
/// this workspace runs `cargo test` on exactly one of them.
pub const WASM_VOLUME_OFFSCREEN_BUDGET_BYTES: usize = 5 * 1024 * 1024;
/// The mobile arm. See [`WASM_VOLUME_OFFSCREEN_BUDGET_BYTES`].
pub const MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES: usize = 5 * 1024 * 1024;
/// The desktop arm. See [`WASM_VOLUME_OFFSCREEN_BUDGET_BYTES`].
pub const DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES: usize = 20 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
pub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize = WASM_VOLUME_OFFSCREEN_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize = MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize = DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES;

/// The playback rates the loop timer is willing to divide by.
///
/// `loop_speed_fps` is a config value before it is a slider value. The settings
/// slider clamps to 1..=30 while it is being dragged, but that clamp only
/// applies to an edit: `load_ui_config` assigns whatever the stored blob holds,
/// and the save-side guard rejects only non-finite. So an older or hand-edited
/// config can hand the frame loop a zero, a negative or a NaN — and
/// `Duration::from_secs_f32` panics on every one of them, on every frame, in a
/// state the user cannot get out of because the panic is in the frame loop.
///
/// The bounds mirror that slider (`rustdar_egui`'s settings pane). Widening
/// either without widening the slider only admits values the UI cannot produce.
pub const MIN_LOOP_SPEED_FPS: f32 = 1.0;

/// See [`MIN_LOOP_SPEED_FPS`].
pub const MAX_LOOP_SPEED_FPS: f32 = 30.0;

/// What a speed that is not a number at all falls back to.
///
/// The UI's default, and the same substitute `save_ui_config` writes when it
/// finds a non-finite value on the way out.
pub const DEFAULT_LOOP_SPEED_FPS: f32 = 5.0;

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
    // The loop timer divides by this, so zero is a division by zero and a
    // reversed pair is a `clamp` that panics.
    assert!(MIN_LOOP_SPEED_FPS > 0.0);
    assert!(MIN_LOOP_SPEED_FPS <= DEFAULT_LOOP_SPEED_FPS);
    assert!(DEFAULT_LOOP_SPEED_FPS <= MAX_LOOP_SPEED_FPS);
    // Eviction is what bounds the textured-frame count, so it must bind first.
    assert!(MAX_LOOP_RENDER_BUDGET <= MAX_LOOP_FRAMES);
    // Every render path indexes a square image; the projection assumes a power of two.
    assert!(rustdar_radar::types::IMAGE_SIZE.is_power_of_two());

    assert!(VOLUME_TEXTURE_BUDGET_BYTES > 0);
    // A zero axis is a texture wgpu refuses outright, and every axis has to fit
    // the WebGL2 guarantee — checked here rather than in a `#[test]` because a
    // test only ever exercises the arm its own runner was built for, and the arm
    // that matters most is the one only a wasm32 build selects. `cargo check
    // --target wasm32-unknown-unknown` evaluates this, which is why the wasm row
    // of the gauntlet is what actually enforces it.
    let mut axis = 0;
    while axis < VOLUME_GRID_CELLS.len() {
        assert!(VOLUME_GRID_CELLS[axis] > 0);
        assert!(
            VOLUME_GRID_CELLS[axis] <= WEBGL2_MAX_TEXTURE_DIMENSION_3D,
            "a voxel grid axis exceeds the 3D texture size WebGL2 guarantees, so \
             a phone browser reporting exactly the guarantee could not allocate \
             it — and the failure would be a validation error inside a callback, \
             where there is no Result to check"
        );
        axis += 1;
    }

    // The offscreen budget has to pay for at least one pixel, because
    // `VolumeQuality::fit` guarantees a size of at least 1 x 1 and that is the
    // one case where it can return something the budget cannot cover. Checked
    // here rather than in a `#[test]` for the reason above: the arm that would
    // go unexercised is the one only a wasm32 build selects.
    assert!(VOLUME_OFFSCREEN_BUDGET_BYTES >= 4);
    assert!(VOLUME_OFFSCREEN_REFERENCE_PANE_PX[0] > 0);
    assert!(VOLUME_OFFSCREEN_REFERENCE_PANE_PX[1] > 0);
};

#[cfg(test)]
mod tests {
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

        /// Bytes one pane's 3D volume occupies: an `R8Unorm` cell per grid cell,
        /// plus the RGBA table those cells index.
        ///
        /// One byte per cell is not an assumption to be tidied away: `R8Unorm` was
        /// chosen because it is *filterable* under `Features::empty()`, which
        /// `R32Float` is not, and because index-to-dBZ being affine makes hardware
        /// filtering exactly linear dBZ interpolation.
        fn volume_bytes(&self) -> usize {
            self.grid.iter().map(|&n| n as usize).product::<usize>() + VOLUME_LUT_BYTES
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
            ("wasm32", 1024, 1, 12, 8, 48, 1536 * 1024),
            ("mobile", 2048, 3, 20, 12, 256, 5 * 1024 * 1024),
            ("desktop", 2048, 6, 60, 30, 512, 12 * 1024 * 1024),
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
        let source = include_str!("constants.rs");
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
        let source = include_str!("constants.rs");
        for (cfg, class) in [
            (r#"target_arch = "wasm32""#, "WASM"),
            (r#"all(not(target_arch = "wasm32"), mobile)"#, "MOBILE"),
            (
                r#"all(not(target_arch = "wasm32"), not(mobile))"#,
                "DESKTOP",
            ),
        ] {
            let definition =
                format!("#[cfg({cfg})]\npub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize =");
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
        let source = include_str!("constants.rs");
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
}
