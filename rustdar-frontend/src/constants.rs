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

/// Ceiling on the compressed tile bytes each basemap/label tile source
/// retains beside its textures: `TILE_CACHE_ENTRIES` PNGs at a generous
/// 30 KiB each — ~7.5 MiB per source, four sources at most (light and dark,
/// base and labels), riding the same LRU slot as each tile's texture. A
/// budget *statement* rather than an enforcement point — the bound is the
/// cache's own entry count; this names what that bound costs so the next
/// memory audit does not have to rediscover it.
///
/// The retention was introduced for the 3D floor's CPU map composite, which
/// no longer exists: the floor is now the 2D pane's own render, copied (see
/// [`VOLUME_MIRROR_BYTES_MAX`]), and nothing re-decodes a tile. The bytes and
/// this figure are kept rather than removed in the same change that removed
/// their consumer, because `TileSource::raster_bytes_at` is a public seam and
/// dropping it is a separate decision from replacing the floor.
pub const TILE_BYTES_BUDGET_PER_SOURCE_BYTES: usize =
    rustdar_egui::tile_source::TILE_CACHE_ENTRIES.get() * 30 * 1024;

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
/// The grid holds palette indices, so the table is the 256 RGBA entries
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
/// One pane shows one volume, so the figure is one grid texture plus its LUT.
/// The grid is [`crate::volume::VOLUME_TEXTURE_FORMAT`] — `Rg8Unorm`,
/// **two** bytes a cell: `R = coverage × index`, `G = coverage` — and it
/// carries `volume::raymarch::GRID_MIP_LEVELS` levels, the raw field and the
/// hand-built box mean below it:
///
/// | target  | grid        | mip 0     | mip 1     | + LUT      | budget |
/// |---------|-------------|----------:|----------:|-----------:|-------:|
/// | desktop | 256x256x128 |    16 MiB |     2 MiB | 18.001 MiB | 24 MiB |
/// | mobile  | 192x192x96  |  6.75 MiB | 0.844 MiB |  7.595 MiB | 10 MiB |
/// | wasm32  | 128x128x64  |     2 MiB |  0.25 MiB |  2.251 MiB |  3 MiB |
///
/// Every arm keeps ~1.33x headroom, which is deliberate: enough for the
/// alignment and driver overhead a real 3D texture allocation carries, not
/// enough to hide a doubled axis.
///
/// # What the coverage channel cost, arm by arm
///
/// The second channel doubled mip 0 and mip 1 alike (8 → 16 MiB desktop,
/// 1 → 2 MiB wasm32), and the mip level — which the previous budget did not
/// count at all, letting it ride in the headroom — is now named. Against the
/// old ceilings that is desktop 9 → 18 MiB against 12, mobile 3.80 → 7.59
/// against 5, wasm32 1.13 → 2.25 against 1.5: **no arm's old budget absorbs
/// it**, so all three ceilings move, in the same 1.33x proportion.
///
/// The wasm32 arm is the one worth arguing rather than asserting, because it
/// is the tight target. +1.5 MiB, and it is **not** linear memory: a WebGL2
/// 3D texture lives in the GPU's own allocation, and what crosses linear
/// memory is the one-byte-per-cell index plane the worker built (unchanged at
/// 1 MiB — coverage is exactly `index != 0`, so it is synthesised at upload
/// and never travels) plus the transient staging copy of the 2 MiB
/// premultiplied plane. For scale, the same target budgets 48 MiB for loop
/// textures, so this is a 3% move against the largest thing on the page and
/// no grid-spec change is needed: every axis stays at or under the
/// [`WEBGL2_MAX_TEXTURE_DIMENSION_3D`] guarantee and no shape shrinks.
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
pub const WASM_VOLUME_TEXTURE_BUDGET_BYTES: usize = 3 * 1024 * 1024;
/// The mobile arm. See [`VOLUME_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_VOLUME_TEXTURE_BUDGET_BYTES: usize = 10 * 1024 * 1024;
/// The desktop arm. See [`VOLUME_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES: usize = 24 * 1024 * 1024;

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

/// What the 3D view's map floor costs: **one** frame-sized colour target, for
/// the whole application, worst case.
///
/// The floor is the 2D pane's own render, copied into an offscreen "pane
/// mirror" that the raymarch samples. That makes its size a property of the
/// *window*, not of any pane or any volume — and it makes it one texture rather
/// than one per pane, per floor or per scope, because the mirror covers the
/// whole frame and two 3D panes sourced from two different maps each find their
/// ground in it by sampling a different region.
///
/// Not a cascade and not a runtime bound. There is nothing to select per target
/// (a frame is a frame) and nothing to enforce (the size is the window's), so
/// this is a **budget statement**: what the design costs at its ceiling, named
/// where the next memory audit will look.
///
/// The ceiling is `egui_renderer::MIRROR_MAX_SIDE` squared, four bytes a texel:
///
/// | frame            | mirror      | bytes    |
/// |------------------|-------------|---------:|
/// | 1920 x 1080      | 1920 x 1080 |  7.9 MiB |
/// | 2560 x 1440      | 1280 x 720  |  3.5 MiB |
/// | 3840 x 2160      | 1920 x 1080 |  7.9 MiB |
/// | 2048 x 2048 (‡)  | 2048 x 2048 | 16.0 MiB |
///
/// ‡ the worst case this constant names, and it is a shape rather than a
/// monitor: the halving that fits the frame under `MIRROR_MAX_SIDE` divides
/// both axes, so the largest mirror is the largest square that still fits.
/// A real 4K display costs *half* what a 1440p one at native scale does,
/// because 3840 exceeds the cap and 2560 does not.
///
/// It replaces a per-scope cost rather than adding to a static one: the design
/// this supersedes composited a 512² RGBA floor for every live `(site, region)`
/// scope — 1 MiB each, unbounded in principle by anything but the number of
/// live scopes — plus the compressed tile bytes it re-decoded to build them.
/// The mirror is larger in the worst case and singular, and it is only
/// allocated at all once some pane actually asks for a floor.
///
/// Stated **independently of current headroom**, deliberately: the voxel
/// texture's own format is changing under a separate work item, so a figure
/// expressed as "what is left over" would be wrong by the time it is read.
pub const VOLUME_MIRROR_BYTES_MAX: usize =
    (crate::egui_renderer::MIRROR_MAX_SIDE as usize).pow(2) * 4;

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
    // Not every render path is square any more — `xsect`'s section raster is
    // `IMAGE_SIZE` × `IMAGE_SIZE / 2`. What every path does share is the side
    // itself: the plan-view projection assumes it is a power of two, and that is
    // also what makes the section's halved height exact and a power of two in
    // its own right rather than a truncating divide.
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
             it - and the failure would be a validation error inside a callback, \
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
mod tests;
