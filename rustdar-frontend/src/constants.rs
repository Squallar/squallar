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
#[cfg(target_arch = "wasm32")]
pub const MAX_CONCURRENT_RENDERS: usize = 1;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_CONCURRENT_RENDERS: usize = 3;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
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
#[cfg(target_arch = "wasm32")]
pub const VOLUME_GRID_CELLS: [u32; 3] = [128, 128, 64];
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_GRID_CELLS: [u32; 3] = [192, 192, 96];
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_GRID_CELLS: [u32; 3] = [256, 256, 128];

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
/// offscreen target the raymarch renders into is a separate cost — roughly 3 MiB
/// at 900 x 900 — and needs its own line when the code that allocates it lands.
/// Folding it in here would make this ceiling untestable against
/// [`VOLUME_GRID_CELLS`], which is the only thing it can currently be checked
/// against.
#[cfg(target_arch = "wasm32")]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = 1536 * 1024;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = 5 * 1024 * 1024;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = 12 * 1024 * 1024;

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
        if MAX_LOOP_RENDER_BUDGET < MAX_LOOP_FRAMES {
            MAX_LOOP_RENDER_BUDGET
        } else {
            MAX_LOOP_FRAMES
        }
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

    /// Bytes one pane's 3D volume occupies: an `R8Unorm` cell per grid cell, plus
    /// the RGBA table those cells index.
    ///
    /// One byte per cell is not an assumption to be tidied away: `R8Unorm` was
    /// chosen because it is *filterable* under `Features::empty()`, which
    /// `R32Float` is not, and because index-to-dBZ being affine makes hardware
    /// filtering exactly linear dBZ interpolation.
    fn volume_bytes() -> usize {
        let cells = VOLUME_GRID_CELLS
            .iter()
            .map(|&n| n as usize)
            .product::<usize>();
        cells + VOLUME_LUT_BYTES
    }

    /// Whichever arm this build compiled is held to its own budget, exactly as
    /// `loop_frames_fit_the_target_texture_budget` is.
    #[test]
    fn the_volume_grid_fits_the_target_texture_budget() {
        let total = volume_bytes();
        assert!(
            total <= VOLUME_TEXTURE_BUDGET_BYTES,
            "a {:?} grid plus a {VOLUME_LUT_BYTES} B table is {total} B, over the \
             {} B budget",
            VOLUME_GRID_CELLS,
            VOLUME_TEXTURE_BUDGET_BYTES,
        );
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
        let total = volume_bytes();
        assert!(
            total * 2 > VOLUME_TEXTURE_BUDGET_BYTES,
            "budget {} B is more than twice the actual {total} B — it would not \
             catch a doubled grid axis",
            VOLUME_TEXTURE_BUDGET_BYTES,
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
}
