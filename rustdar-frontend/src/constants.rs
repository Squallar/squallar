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

/// How many cross-section loop frames may be *dispatched* in one frame.
///
/// # Why the loop path needs a cap the plan-view path does not
///
/// Cutting a section frame needs a whole-volume payload, and building one
/// (`RenderInput::extract_volume_parts`) runs on the frame thread: the job wire
/// carries a `RenderInput`, not a `Scan`, and on wasm the volume is only
/// reachable from the main thread at all. That is not new — the live section
/// pane has always paid it, once per volume — but a loop wants it once per
/// *frame*, and without a cap a desktop dispatch pass would run
/// [`MAX_CONCURRENT_RENDERS`] of them back to back on the frame that starts the
/// loop.
///
/// One, measured: on a real VCP-212 reflectivity volume the extraction is
/// ~1.0 ms and the rasterization it feeds is ~6.1 ms. At one per frame the
/// frame thread pays roughly what a single live re-cut already costs it, the
/// expensive half is on the worker, and a full desktop render set of 30 frames
/// is dispatched over 30 frames — half a second at 60 fps, during which the
/// pane shows every frame that has landed rather than blocking on the batch.
///
/// It is deliberately not a per-target cascade. The number is chosen against
/// the *frame budget*, which is 16.7 ms everywhere, rather than against a device
/// class's memory; and wasm's `MAX_CONCURRENT_RENDERS` of 1 already imposes the
/// same limit there by another route.
pub const MAX_LOOP_SECTION_CUTS_PER_FRAME: usize = 1;

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
/// A loop frame's size depends on which kind of pane is animating it, and both
/// kinds are budgeted here because both are *per pane*: a section pane's loop
/// costs what its own row says, and a screen holding one of each costs the sum
/// of two rows, exactly as two map panes have always cost two of the first.
///
/// A plan-view frame is an `IMAGE_SIZE²` RGBA raster:
///
/// | target  | held | textured | frame size | total   | budget  |
/// |---------|-----:|---------:|-----------:|--------:|--------:|
/// | desktop |   60 |       30 |     16 MiB | 480 MiB | 512 MiB |
/// | mobile  |   20 |       12 |     16 MiB | 192 MiB | 256 MiB |
/// | wasm32  |   12 |        8 |      4 MiB |  32 MiB |  48 MiB |
///
/// A cross-section frame is `SECTION_WIDTH × SECTION_HEIGHT`, and
/// `rustdar_radar::xsect` defines those as `IMAGE_SIZE` by `IMAGE_SIZE / 2` — so
/// it is **exactly half** a plan-view frame on every target, by construction
/// rather than by coincidence, and a section loop can never be the binding case:
///
/// | target  | held | textured | frame size | total   | budget  |
/// |---------|-----:|---------:|-----------:|--------:|--------:|
/// | desktop |   60 |       30 |      8 MiB | 240 MiB | 512 MiB |
/// | mobile  |   20 |       12 |      8 MiB |  96 MiB | 256 MiB |
/// | wasm32  |   12 |        8 |      2 MiB |  16 MiB |  48 MiB |
///
/// Section frames carry no value or status plane — those are ~10 MB apiece and
/// serve only the hover readout, which goes quiet under a loop for the same
/// reason a plan-view loop's does. See `rustdar_egui::pane::SectionImageData`.
///
/// **A 3D volume loop is budgeted against this line too, but not per pane** —
/// see [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`], which is the same figure spent
/// once for the whole application rather than once per pane, because the grids
/// live in a single application-wide `VolumeStore` instead of in the pane. The
/// row it buys, at the per-grid figures [`VOLUME_TEXTURE_BUDGET_BYTES`]
/// tabulates, is [`MAX_LOOP_VOLUME_FRAMES`]'.
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

/// Ceiling on the resident voxel grids a 3D loop may hold — **for the whole
/// application**, not per pane.
///
/// # A 3D loop's frames are grids, not images
///
/// A plan-view or cross-section loop frame is a *rendered picture*, so it can
/// be cached, evicted and re-rendered as the playhead walks. A 3D pane's
/// picture is raymarched live from the eye, so caching it per frame would make
/// every frame wrong the moment the camera moved. What a 3D loop caches
/// instead is the **input**: each frame is a live [`VOLUME_GRID_CELLS`] 3D
/// texture and the march swaps which one it samples. Measured on an RTX 3090
/// and on lavapipe over seven consecutive VCP-212 volumes, marching a
/// *different* resident grid each frame costs **+0.01 ms (+2%)** on the
/// discrete GPU and **+0.31–0.78 ms (+3–4%)** on the software rasteriser
/// against marching one — a `set_bind_group` and a 192-byte uniform write, not
/// an upload. Orbiting a resident loop is therefore free, and there is no
/// re-render on a camera change at all.
///
/// # Why the frame list must *equal* the resident set
///
/// The two loop kinds above hold more frames than they texture
/// ([`MAX_LOOP_FRAMES`] against [`MAX_LOOP_RENDER_BUDGET`]) and re-render as
/// the playhead walks back into a window it had left. That treadmill does not
/// close here: re-entering a resident 3D window costs ~140 ms (89 ms resample,
/// 51 ms upload) against the 200 ms interval at [`DEFAULT_LOOP_SPEED_FPS`] and
/// 33 ms at [`MAX_LOOP_SPEED_FPS`]. So [`MAX_LOOP_VOLUME_FRAMES`] is both
/// numbers at once, and `the_3d_loop_holds_exactly_what_it_marches` pins it.
///
/// # Why once for the application rather than once per pane
///
/// The grids live in one `VolumeStore` keyed by `VolumeTarget`, shared by every
/// 3D pane — two panes orbiting one volume from two angles already share one
/// build and one upload. So two 3D loops on the same site, product and region
/// cost one set, and the bound that matters is the store's total. That is also
/// what keeps this feature out of the multiplication
/// [`APP_TEXTURE_BUDGET_BYTES`] names: a 3D loop is the one loop kind whose
/// budget is **not** multiplied by `MAX_PANES_DESKTOP`.
///
/// Unlike [`LOOP_TEXTURE_BUDGET_BYTES`], this one **is enforced at runtime**:
/// `VolumeStore::enforce_budget` evicts oldest-first until the resident grids
/// fit, every frame, and `the_store_eviction_actually_bounds` drives it past
/// the line. The frame counts below are chosen so it never has to fire in
/// steady state; it exists for the transition, where a pane can hold its live
/// grid and a loop set at once.
///
/// | target  | frames | 3D texture | resident | budget  |
/// |---------|-------:|-----------:|---------:|--------:|
/// | wasm32  |      8 |  4.501 MiB | 36.0 MiB |  48 MiB |
/// | mobile  |     12 | 15.189 MiB | 182.3MiB | 256 MiB |
/// | desktop |     14 | 36.001 MiB |  504 MiB | 512 MiB |
///
/// Deliberately the same figure as [`LOOP_TEXTURE_BUDGET_BYTES`] rather than a
/// number of its own: a loop is a loop, and a screen showing a 3D loop instead
/// of a map loop should cost about the same. Written as an alias so that
/// raising one raises the other, which is the honest coupling — the day these
/// need to diverge, that is a decision to make here rather than a drift to
/// discover.
pub const VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = LOOP_TEXTURE_BUDGET_BYTES;
/// The wasm32 arm of [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const WASM_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = WASM_LOOP_TEXTURE_BUDGET_BYTES;
/// The mobile arm. See [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = MOBILE_LOOP_TEXTURE_BUDGET_BYTES;
/// The desktop arm. See [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_LOOP_TEXTURE_BUDGET_BYTES;

/// Frames a 3D volume loop holds — which is also how many voxel grids it keeps
/// resident, because for this loop kind those are the same number. See
/// [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
///
/// # Desktop takes fewer frames at the full grid, not more at a coarser one
///
/// 14 frames of the full 256×256×128 grid is ~70 minutes of history where 30
/// frames would be ~150. That is a real loss and it is stated rather than
/// hidden. The alternative — a loop-specific coarser grid — was rejected for
/// three reasons, in the order they bite:
///
/// * A coarser grid halves the **vertical** axis (141 → 188 m/cell at
///   192×192×64), and that is where 3D structure lives. A BWER or an overhang
///   is a few hundred metres; a loop exists to watch exactly those evolve.
/// * The region picker exists to spend a fixed cell count over less ground,
///   and it *prints the km/cell it bought* (`VolumeRegion::resolution_km`). A
///   loop-specific grid would silently undo the user's resolution choice at
///   the moment they zoomed in to look at structure, and would make that
///   caption a lie unless it changed under a loop too.
/// * There is no performance argument either way: 0.60 ms against 0.42 ms per
///   march on the measured hardware, both trivial against a 16.7 ms frame.
///
/// # Each arm is the tighter of two bounds
///
/// What [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`] admits, and
/// [`MAX_LOOP_RENDER_BUDGET`]. The budget binds desktop (14 grids where a
/// plan-view loop textures 30 frames); the render budget binds wasm32 and
/// mobile, where the grids are small enough that the budget would admit 10 and
/// 16 — a 3D loop is not licensed to hold *more* history than the plan-view
/// loop beside it on the same device merely because its frames are cheaper
/// there. `the_3d_loop_holds_exactly_what_it_marches` computes both and pins
/// the minimum.
///
/// Named outside the cascade for the reason [`WASM_VOLUME_GRID_CELLS`] gives.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_VOLUME_FRAMES: usize = WASM_MAX_LOOP_VOLUME_FRAMES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_VOLUME_FRAMES: usize = MOBILE_MAX_LOOP_VOLUME_FRAMES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_VOLUME_FRAMES: usize = DESKTOP_MAX_LOOP_VOLUME_FRAMES;

/// The wasm32 arm of [`MAX_LOOP_VOLUME_FRAMES`].
pub const WASM_MAX_LOOP_VOLUME_FRAMES: usize = 8;
/// The mobile arm. See [`MAX_LOOP_VOLUME_FRAMES`].
pub const MOBILE_MAX_LOOP_VOLUME_FRAMES: usize = 12;
/// The desktop arm. See [`MAX_LOOP_VOLUME_FRAMES`].
pub const DESKTOP_MAX_LOOP_VOLUME_FRAMES: usize = 14;

/// How many voxel grids a 3D loop may *dispatch* in one frame.
///
/// The exact counterpart of [`MAX_LOOP_SECTION_CUTS_PER_FRAME`], for the same
/// reason and at the same value: building a loop frame's grid needs a
/// whole-volume payload, and `RenderInput::extract_volume_parts` runs on the
/// frame thread because the job wire carries a `RenderInput`, not a `Scan`, and
/// on wasm the volume is only reachable from the main thread. The resample
/// (~89 ms) and the upload (~51 ms) are both off it.
///
/// One per frame means a full desktop set of 14 is dispatched over 14 frames —
/// under a quarter of a second at 60 fps — and every grid that lands is shown
/// as it lands rather than the pane blocking on the batch.
pub const MAX_LOOP_VOLUME_BUILDS_PER_FRAME: usize = 1;

/// Ceiling on the GPU texture memory the **whole application** budgets, in
/// bytes — every pane, every loop and every volume at once.
///
/// # Why this constant did not exist before, and why it has to now
///
/// [`LOOP_TEXTURE_BUDGET_BYTES`] and [`VOLUME_TEXTURE_BUDGET_BYTES`] are both
/// *per pane*, and nothing multiplied either of them by the pane count. The two
/// halves of that multiplication even live in different crates —
/// `MAX_PANES_DESKTOP` is `rustdar_egui::pane`'s — so no test could have
/// noticed. This is that missing line.
///
/// The worst case is stated as a sum rather than a maximum, and deliberately
/// over-counts by one pane: every pane a 2D loop *and* a full 3D loop set *and*
/// every pane's raymarch offscreen. A pane is only ever one kind at a time, so
/// nothing can reach this; what matters is that raising any term has to come
/// past `the_whole_application_fits_its_gpu_ceiling`.
///
/// | target  | panes | 2D loops   | 3D grids | offscreens | total     | ceiling  |
/// |---------|------:|-----------:|---------:|-----------:|----------:|---------:|
/// | desktop |     6 |   3072 MiB |  512 MiB |    120 MiB |  3704 MiB | 3840 MiB |
/// | mobile  |     4 |   1024 MiB |  256 MiB |     20 MiB |  1300 MiB | 1408 MiB |
/// | wasm32  |     6 |    288 MiB |   48 MiB |     30 MiB |   366 MiB |  384 MiB |
///
/// # Two findings this arithmetic makes visible, neither of them this change's
///
/// **The per-pane loop budget is 83% of the desktop figure and 79% of
/// mobile's.** `MAX_PANES × LOOP_TEXTURE_BUDGET_BYTES` is 3.0 GiB on desktop
/// and 1.0 GiB on a phone — the latter is more GPU memory than a mid-range
/// phone has for everything. Bringing desktop under 2 GiB with the same pane
/// count would mean a per-pane loop budget of ~320 MiB, i.e.
/// [`MAX_LOOP_RENDER_BUDGET`] falling from 30 to 19 and the loop's history
/// with it. That is a product decision about how much history a map loop
/// holds, not a side effect of teaching the 3D pane to animate, so it is
/// written down here rather than taken.
///
/// **The 3D loop is the one loop kind that does not multiply.** Its grids are
/// in one application-wide store, so the term above is
/// [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`] flat, not per pane. Making it per-pane
/// would add 2.5 GiB to the desktop row, and this test is what would say so.
///
/// Like [`LOOP_TEXTURE_BUDGET_BYTES`], a budget *statement*: the enforcement
/// points are the per-subsystem ones. `the_app_ceiling_is_not_slack_enough_to_
/// hide_a_doubling` keeps it snug, so it cannot be quietly raised to admit
/// whatever the constants grew into.
#[cfg(target_arch = "wasm32")]
pub const APP_TEXTURE_BUDGET_BYTES: usize = WASM_APP_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const APP_TEXTURE_BUDGET_BYTES: usize = MOBILE_APP_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const APP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_APP_TEXTURE_BUDGET_BYTES;

/// The wasm32 arm of [`APP_TEXTURE_BUDGET_BYTES`].
pub const WASM_APP_TEXTURE_BUDGET_BYTES: usize = 384 * 1024 * 1024;
/// The mobile arm. See [`APP_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_APP_TEXTURE_BUDGET_BYTES: usize = 1408 * 1024 * 1024;
/// The desktop arm. See [`APP_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_APP_TEXTURE_BUDGET_BYTES: usize = 3840 * 1024 * 1024;

/// Ceiling on the compressed tile bytes each basemap/label tile source
/// retains beside its textures: `TILE_CACHE_ENTRIES` PNGs at a generous
/// 30 KiB each — ~7.5 MiB per source, four sources at most (light and dark,
/// base and labels), riding the same LRU slot as each tile's texture. A
/// budget *statement* rather than an enforcement point — the bound is the
/// cache's own entry count; this names what that bound costs so the next
/// memory audit does not have to rediscover it.
///
/// # FOLLOW-UP: this budget currently has no consumer
///
/// The retention was introduced for the 3D floor's CPU map composite, which no
/// longer exists: the floor is now the 2D pane's own render, copied (see
/// [`VOLUME_MIRROR_BYTES_MAX`]), and nothing re-decodes a tile. So the ~30 MiB
/// this names is live and read by nobody.
///
/// It is *stated* here rather than removed alongside its consumer because
/// dropping it is a separate decision from replacing the floor, and because
/// nothing warns: `rustdar_egui::tile_source::TileSource::raster_bytes_at` and
/// `rustdar_egui::ui::Gui::map_tiles_mut` are both unreferenced now and both
/// `pub`, so no dead-code lint fires on either. The work, when it is taken:
///
///  1. delete `TileSource::raster_bytes_at` and `Gui::map_tiles_mut`;
///  2. drop `CachedTile::bytes` (`rustdar-egui/src/tile_source.rs`), which is
///     what actually retains the compressed PNGs;
///  3. delete this constant and its test.
///
/// Until then, treat this figure as a *debt* rather than a cost: it is the size
/// of the thing step 2 gives back.
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
/// The grid is [`crate::volume::VOLUME_TEXTURE_FORMAT`] — `Rg16Float`,
/// **four** bytes a cell: `R = coverage × index`, `G = coverage`, a half float
/// each — and it carries `volume::raymarch::GRID_MIP_LEVELS` levels, the raw
/// field and the hand-built box mean below it:
///
/// | target  | grid        | mip 0     | mip 1     | + LUT      | budget |
/// |---------|-------------|----------:|----------:|-----------:|-------:|
/// | desktop | 256x256x128 |    32 MiB |     4 MiB | 36.001 MiB | 48 MiB |
/// | mobile  | 192x192x96  |  13.5 MiB | 1.688 MiB | 15.189 MiB | 20 MiB |
/// | wasm32  | 128x128x64  |     4 MiB |   0.5 MiB |  4.501 MiB |  6 MiB |
///
/// Every arm keeps ~1.33x headroom, which is deliberate: enough for the
/// alignment and driver overhead a real 3D texture allocation carries, not
/// enough to hide a doubled axis.
///
/// # What the half-float channels cost, arm by arm
///
/// Widening each channel from a byte to a half float doubled mip 0 and mip 1
/// alike (16 → 32 MiB desktop, 2 → 4 MiB wasm32), so every arm's ceiling
/// doubles with it: desktop 24 → 48 MiB, mobile 10 → 20, wasm32 3 → 6, the
/// same ~1.33x headroom kept throughout.
///
/// The width is not slack. `Rg8Unorm` filters `R̄` and `Ḡ` with an
/// **absolute** error of up to one 1/255 quantum on real samplers, and the
/// march's reconstruction divides by `Ḡ` — so at an echo edge, where `Ḡ` is a
/// few 255ths, the error arrives at the palette index multiplied by 255 and
/// the shell around every echo paints bands the data never held. A float
/// channel's error is relative instead, which is the whole reason for the
/// second byte; [`crate::volume::VOLUME_TEXTURE_FORMAT`] carries the
/// measurement and the derivation.
///
/// The wasm32 arm is the one worth arguing rather than asserting, because it
/// is the tight target. +2.25 MiB, and it is **not** linear memory: a WebGL2
/// 3D texture lives in the GPU's own allocation, and what crosses linear
/// memory is the one-byte-per-cell index plane the worker built (unchanged at
/// 1 MiB — coverage is exactly `index != 0`, so it is synthesised at upload
/// and never travels) plus the transient staging copy of the 4 MiB
/// premultiplied plane. For scale, the same target budgets 48 MiB for loop
/// textures, so this is a 5% move against the largest thing on the page and
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
pub const WASM_VOLUME_TEXTURE_BUDGET_BYTES: usize = 6 * 1024 * 1024;
/// The mobile arm. See [`VOLUME_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_VOLUME_TEXTURE_BUDGET_BYTES: usize = 20 * 1024 * 1024;
/// The desktop arm. See [`VOLUME_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES: usize = 48 * 1024 * 1024;

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
/// # Why this is now a cascade, and a real bound
///
/// It was one figure — the guaranteed texture cap squared — because the mirror
/// was always the frame's own size and the only question was how far a large
/// frame had to be halved. It is three figures now because the mirror is drawn
/// at a **rung** keyed to the 3D camera's distance (`egui_renderer::mirror`): a
/// low, close camera magnifies the floor it samples, and the answer to that is
/// more texels, which is memory, which differs per target. This constant is
/// what `MirrorLimits::for_device` holds the rung to, so it is *enforced*
/// rather than merely stated — unlike [`LOOP_TEXTURE_BUDGET_BYTES`] and
/// [`VOLUME_TEXTURE_BUDGET_BYTES`], and like [`VOLUME_OFFSCREEN_BUDGET_BYTES`].
///
/// # The arithmetic, per target, four bytes a texel
///
/// `mirror_plan` scales the frame by the rung and then halves both axes until
/// the result fits **both** this budget and the device's own
/// `max_texture_dimension_2d`. So each row below is what a frame of that shape
/// actually gets, not what it asks for:
///
/// | target  | frame       | rung | mirror      | bytes    | budget |
/// |---------|-------------|-----:|-------------|---------:|-------:|
/// | desktop | 1920 x 1080 |   2x | 3840 x 2160 | 31.6 MiB | 64 MiB |
/// | desktop | 2560 x 1440 |   2x | 5120 x 2880 | 56.2 MiB | 64 MiB |
/// | desktop | 3840 x 2160 |   1x | 3840 x 2160 | 31.6 MiB | 64 MiB |
/// | mobile  | 2400 x 1080 |   1x | 2400 x 1080 |  9.9 MiB | 16 MiB |
/// | wasm32  | 2560 x 1440 | 0.5x | 1280 x  720 |  3.5 MiB | 16 MiB |
/// | wasm32  | 2048 x 2048 |   1x | 2048 x 2048 | 16.0 MiB | 16 MiB |
///
/// Three things that table says out loud, because each is a decision:
///
/// * **Desktop gains at 4K.** The old single cap halved a 3840-wide frame to
///   1920 because 3840 exceeded 2048 — so the largest displays got the
///   *softest* floors. `MirrorLimits::for_device` now raises the side cap to
///   the adapter's own limit (8192 or more on any desktop), and 31.6 MiB is
///   inside the budget, so a 4K frame is mirrored at 4K.
/// * **Desktop supersamples below 4K, and that is what 64 MiB buys.** 56.2 MiB
///   at 1440p is the tight row; 64 MiB clears it with the ~1.14x margin a real
///   allocation's alignment wants and not enough to hide another doubling.
///   Rung 4 would be 225 MiB at 1440p, refused here and separately capped by
///   `MIRROR_SCALE_MAX` for a reason that is about the tile cache rather than
///   about memory.
/// * **Mobile and wasm32 get no rung at all, deliberately.** 16 MiB is exactly
///   the old ceiling, so neither arm's floor-on memory moves by a byte. On
///   wasm32 the side cap binds first anyway — `downlevel_webgl2_defaults`
///   guarantees only 2048, and 2048² is 16 MiB — so the budget and the device
///   agree there. On mobile the budget is what refuses the rung: a phone frame
///   at 2x is 39.6 MiB, beside a 5 MiB volume texture and a 5 MiB offscreen.
///   Both degrade through `MirrorPlan::is_degraded`, and the tile zoom bias is
///   taken from the rung that was *applied*, so a target that cannot show the
///   detail does not fetch it either.
///
/// It replaces a per-scope cost rather than adding to a static one: the design
/// this supersedes composited a 512² RGBA floor for every live `(site, region)`
/// scope — 1 MiB each, unbounded in principle by anything but the number of
/// live scopes — plus the compressed tile bytes it re-decoded to build them.
/// The mirror is larger in the worst case and singular, and it is held only
/// while some pane is actually asking for a floor: the frame path allocates it
/// on the first frame with a non-empty guest list and calls
/// `VolumeResources::release_mirror` on every frame without one, so closing the
/// last 3D pane returns the whole figure rather than holding it for the
/// session. A machine that never opens one never pays it at all.
///
/// Stated **independently of current headroom**, deliberately: the voxel
/// texture's own format is changing under a separate work item, so a figure
/// expressed as "what is left over" would be wrong by the time it is read.
///
/// Named outside the cascade, the shape [`WASM_VOLUME_GRID_CELLS`] documents
/// and for the reason it gives: this workspace runs `cargo test` on one arm, so
/// the other two are only reachable from a test if they have names.
#[cfg(target_arch = "wasm32")]
pub const VOLUME_MIRROR_BYTES_MAX: usize = WASM_VOLUME_MIRROR_BYTES_MAX;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_MIRROR_BYTES_MAX: usize = MOBILE_VOLUME_MIRROR_BYTES_MAX;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_MIRROR_BYTES_MAX: usize = DESKTOP_VOLUME_MIRROR_BYTES_MAX;

/// The wasm32 arm of [`VOLUME_MIRROR_BYTES_MAX`]: the guaranteed side cap
/// squared, four bytes a texel — the figure the whole constant used to be.
pub const WASM_VOLUME_MIRROR_BYTES_MAX: usize =
    (crate::egui_renderer::MIRROR_MAX_SIDE as usize).pow(2) * 4;
/// The mobile arm. See [`VOLUME_MIRROR_BYTES_MAX`].
pub const MOBILE_VOLUME_MIRROR_BYTES_MAX: usize = WASM_VOLUME_MIRROR_BYTES_MAX;
/// The desktop arm. See [`VOLUME_MIRROR_BYTES_MAX`].
pub const DESKTOP_VOLUME_MIRROR_BYTES_MAX: usize = 64 * 1024 * 1024;

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
