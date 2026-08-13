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

/// The side, in pixels, a **static** plan-view render is allowed to grow to
/// when its sweep reaches past [`rustdar_radar::types::BASE_EXTENT_KM`].
///
/// Only *past* it. A sweep reaching less than that keeps the base side and
/// spends it on the ground the sweep actually covers, so a TDWR Doppler pane
/// is 2048 pixels over 88.8 km rather than over 230.
///
/// # Why the size class lives here and not in the rasterizer
///
/// `rustdar_radar` cannot pick it. The `mobile` cfg that names the device class
/// is emitted by *this* crate's `build.rs`, and cargo scopes a build script's
/// cfgs to its own crate — `rustdar_radar::voxel`'s module doc records what that
/// trap looks like when it is walked into: a `#[cfg(mobile)]` over there is an
/// `unexpected_cfgs` warning attached to dead code that silently takes the
/// desktop answer on a handheld. `MOBILE_SHAPE` is the precedent for the shape
/// taken instead — a named constant this crate selects and hands over.
///
/// # What each class is, and why
///
/// | class   | base (`IMAGE_SIZE`) | long range | adaptive |
/// |---------|--------------------:|-----------:|----------|
/// | wasm32  |                2048 |       2048 | no       |
/// | mobile  |                2048 |       4096 | yes      |
/// | desktop |                2048 |       4096 | yes      |
///
/// 4096 is what keeps the scale still: 4096 over the 460.11 km a surveillance
/// cut actually covers is 4.4512 px/km against the floor's 4.4522, so a
/// long-range sweep is the same picture over more of the world rather than a
/// coarser one. A base-size raster stretched over the same ground is 2.2256
/// px/km — half a pixel per 250 m gate, where the floor gives it 1.11.
///
/// **The web arm is not adaptive, and cannot be.** 2048 is the largest 2D
/// texture WebGL2 guarantees ([`rustdar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D`]),
/// wgpu's WebGPU backend is not used here, and a browser that refused a 4096
/// texture would leave a blank pane rather than a coarse one. So the web ceiling
/// *is* the base size, and [`rustdar_radar::types::raster_side_px`] can never
/// answer anything else there.
///
/// **That makes the browser's raster size inert, not its pictures.** The extent
/// is the data's on every target, so a browser draws a 1192-gate Doppler cut
/// over the same ±300.11 km a desktop does, on a quarter of the pixels: 3.4121
/// px/km against 6.8241, and 23.4% under the floor's own scale. The trade is
/// deliberate and `raster_side_px`'s doc is where it is argued and measured;
/// what matters here is that the web class is the one that always pays it.
///
/// Native's ceiling is checked against the device rather than assumed: see
/// `AppState::long_range_raster_ok`. Vulkan guarantees 4096 and iOS Metal 8192,
/// but the GLES 3.0 floor is 2048, so an Android handheld is the one class where
/// the gate can fail — and it degrades to the base size, joining the browser in
/// that same trade, which is a correct picture rather than a failed texture.
///
/// # What it costs, measured
///
/// A 7950X (32 threads), release, medians of 11 rasterizations of a real KDMX
/// 0.5° cut on the existing render pool — nothing here is on the frame thread:
///
/// | sweep                     | side | render  | RGBA   |
/// |---------------------------|-----:|--------:|-------:|
/// | reflectivity, 460 km      | 2048 | 27.7 ms | 16 MiB |
/// | reflectivity, 460 km      | 4096 | 82.4 ms | 64 MiB |
/// | velocity, 300 km          | 2048 | 26.6 ms | 16 MiB |
/// | velocity, 300 km          | 4096 | 81.9 ms | 64 MiB |
///
/// Three times the wall clock for four times the pixels — the gate loop's
/// per-sample Mercator is the cost and it parallelises, so the fourth quarter
/// comes nearly free. Mobile has three render slots against desktop's six and
/// no comparable pool, and is **not measured here**: no handheld was available,
/// and a figure scaled off this machine would be a guess wearing a number.
/// The frame thread is untouched either way; the conversion that used to land
/// on it moved with this change (`channels::RenderedImage::image`).
///
/// The three arms are named outside the cascade for the reason
/// [`WASM_VOLUME_GRID_CELLS`] gives.
pub const WASM_LONG_RANGE_IMAGE_SIZE: usize = rustdar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D;
/// The mobile arm. See [`LONG_RANGE_IMAGE_SIZE`].
pub const MOBILE_LONG_RANGE_IMAGE_SIZE: usize = 4096;
/// The desktop arm. See [`LONG_RANGE_IMAGE_SIZE`].
pub const DESKTOP_LONG_RANGE_IMAGE_SIZE: usize = 4096;

/// See [`WASM_LONG_RANGE_IMAGE_SIZE`].
#[cfg(target_arch = "wasm32")]
pub const LONG_RANGE_IMAGE_SIZE: usize = WASM_LONG_RANGE_IMAGE_SIZE;
/// See [`WASM_LONG_RANGE_IMAGE_SIZE`].
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LONG_RANGE_IMAGE_SIZE: usize = MOBILE_LONG_RANGE_IMAGE_SIZE;
/// See [`WASM_LONG_RANGE_IMAGE_SIZE`].
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LONG_RANGE_IMAGE_SIZE: usize = DESKTOP_LONG_RANGE_IMAGE_SIZE;

/// The side a **loop frame** is rendered at — the whole side, not a ceiling on
/// a long-range one: a loop of a 458 km surveillance cut draws every frame at
/// this size, at whatever km/pixel that buys.
///
/// Natively it is the base size, so nothing about a native loop moves: a loop
/// already renders leaner than a still frame (no value grid, so no hover), and
/// this is the same idiom applied to the one dimension that had not been
/// asked to give.
///
/// **The web arm is a third of the way down, at 1024, and that is the constant
/// this whole cascade exists for.** A browser's per-pane loop budget is 48 MiB
/// ([`WASM_LOOP_TEXTURE_BUDGET_BYTES`]) and it textures eight frames at once;
/// 2048² frames are 16 MiB apiece, so following the static size would need a
/// 128 MiB loop budget — and [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`] is an alias
/// of that one, so the 3D term rises with it: 6 × 128 + 128 + 30 puts
/// [`APP_TEXTURE_BUDGET_BYTES`] at ~926 MiB against a 384 MiB ceiling.
/// `the_whole_application_fits_its_gpu_ceiling` is the line that says so and
/// `the_app_ceiling_is_not_slack_enough_to_hide_a_doubling` is why the ceiling
/// cannot simply be raised to admit it. So web loops stay exactly the size and
/// exactly the cost they are today, and only *static* web renders take the
/// quality bump.
///
/// The trade, stated: entering a loop on a long-range pane drops it to this
/// size for the duration, and back to the full one when the loop stops.
pub const WASM_LOOP_IMAGE_SIZE: usize = 1024;
/// The mobile arm. See [`LOOP_IMAGE_SIZE`].
pub const MOBILE_LOOP_IMAGE_SIZE: usize = rustdar_radar::types::NATIVE_IMAGE_SIZE;
/// The desktop arm. See [`LOOP_IMAGE_SIZE`].
pub const DESKTOP_LOOP_IMAGE_SIZE: usize = rustdar_radar::types::NATIVE_IMAGE_SIZE;

/// See [`WASM_LOOP_IMAGE_SIZE`].
#[cfg(target_arch = "wasm32")]
pub const LOOP_IMAGE_SIZE: usize = WASM_LOOP_IMAGE_SIZE;
/// See [`WASM_LOOP_IMAGE_SIZE`].
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_IMAGE_SIZE: usize = MOBILE_LOOP_IMAGE_SIZE;
/// See [`WASM_LOOP_IMAGE_SIZE`].
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_IMAGE_SIZE: usize = DESKTOP_LOOP_IMAGE_SIZE;

/// The side a raster of `rgba_len` bytes must have been rendered at, or `None`
/// if no render this build can produce has that length.
///
/// Every consumer of a finished raster derives the side this way rather than
/// naming a constant, because the side is no longer one number: a static render
/// is [`rustdar_radar::types::IMAGE_SIZE`] or [`LONG_RANGE_IMAGE_SIZE`]
/// depending on the sweep, and a loop frame is [`LOOP_IMAGE_SIZE`]. Deriving it
/// keeps `offload`'s rule that a job's output carries no dimensions — the bytes
/// are the statement — while the closed set is what keeps that from becoming
/// "believe whatever arrived": a buffer of any other length is refused, and a
/// refusal is a logged blank pane rather than the `ColorImage`
/// assertion that would abort a browser tab.
pub fn raster_side_from_rgba_len(rgba_len: usize) -> Option<usize> {
    [
        LOOP_IMAGE_SIZE,
        rustdar_radar::types::IMAGE_SIZE,
        LONG_RANGE_IMAGE_SIZE,
    ]
    .into_iter()
    .find(|side| side * side * 4 == rgba_len)
}

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
/// target. See [`LOOP_POOL_FLOOR_BYTES`] for the resulting memory ceiling.
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

/// The **whole application's** loop allowance on a device that can tell us
/// nothing about itself, in bytes.
///
/// # This used to be a per-pane figure, and that was the bug
///
/// It was `LOOP_TEXTURE_BUDGET_BYTES`, it carried these same three numbers, and
/// it was what *one pane's* loop textures could occupy. Nothing multiplied it
/// by the pane count. `MAX_PANES_DESKTOP` is 6 and `MAX_PANES_MOBILE` is 4 —
/// and the Compact width class allows all four — so the reachable totals were
/// **3.0 GiB on desktop and 1.0 GiB on a phone**, four panes and a loop toggle.
/// The two halves of that multiplication live in different crates, which is why
/// no test put them side by side until [`APP_TEXTURE_BUDGET_BYTES`] did.
///
/// It is one pool now, divided among the loops that want one, by
/// [`crate::loop_pool`]. What the number is chosen to be is the interesting
/// part:
///
/// **The floor is exactly what one pane used to get all to itself.** Not a
/// coincidence and not nostalgia — it is the property that makes this change
/// safe to ship. A session with one loop open, on the worst device this target
/// admits, gets byte for byte and frame for frame what it gets today, because
/// one loop's share of a floor-sized pool *is* the old per-pane budget. What
/// changes is that six of them no longer cost six times it.
///
/// A plan-view frame is a [`LOOP_IMAGE_SIZE`]² RGBA raster — not the size a
/// static pane render takes, because a loop's frames are held by the dozen and
/// a still frame is held once. On the web that difference is the whole reason
/// this budget still fits; natively the two are the same 2048.
///
/// | target  | textured | frame size | one loop | floor   |
/// |---------|---------:|-----------:|---------:|--------:|
/// | desktop |       30 |     16 MiB |  480 MiB | 512 MiB |
/// | mobile  |       12 |     16 MiB |  192 MiB | 256 MiB |
/// | wasm32  |        8 |      4 MiB |   32 MiB |  48 MiB |
///
/// The textured-frame count is `min(MAX_LOOP_FRAMES, MAX_LOOP_RENDER_BUDGET)`,
/// not `MAX_LOOP_FRAMES`: `evict_textures_outside_render_set` runs every
/// dispatch and strips the texture off every frame outside the render set, so
/// the frames a loop *holds* and the frames that are *textured* are different
/// numbers. Budgeting on `MAX_LOOP_FRAMES` alone overstates desktop by 2x.
///
/// A cross-section frame is `SECTION_WIDTH × SECTION_HEIGHT`, which
/// `rustdar_radar::xsect` pins per target at 1024 × 512 on the web and
/// 2048 × 1024 native — **exactly half** a plan-view loop frame on every
/// target, so a section loop can never be the binding case. It no longer needs
/// a table of its own to say so: the pool is *bytes*, and an equal share simply
/// buys a section loop twice the history.
///
/// Section frames carry no value or status plane — those are ~10 MB apiece and
/// serve only the hover readout, which goes quiet under a loop for the same
/// reason a plan-view loop's does. See `rustdar_egui::pane::SectionImageData`.
///
/// **A 3D volume loop takes a share of this pool like any other loop, but one
/// share per *volume*, not per pane** — its frames are resident grids in a
/// single application-wide `VolumeStore`, so two 3D panes orbiting one volume
/// from two angles are one loop and cost one share. See
/// [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`] and
/// `crate::loop_pool::LoopDemand::volume_sets`.
///
/// # The floor also has to seat a full screen without blanking anything
///
/// [`MIN_LOOP_FRAMES_PER_PANE`] is what stops a busy layout cliff-ing to
/// nothing, and it is only reachable if the floor can pay for it on every pane
/// the width class admits. wasm32's row is exact — six loops at two frames of
/// 4 MiB is 48 MiB, to the byte — which is the honest statement of how tight
/// the browser is, and `the_floor_seats_every_pane_without_blanking_one` is
/// where a change to any of those four numbers has to come past.
///
/// # wasm32 is the arm a constant cannot serve at all, and that is why this is a floor
///
/// `mobile` is a compile-time cfg this crate's `build.rs` emits for native
/// Android and iOS. **A browser on a phone is not `mobile`** — it is
/// `target_arch = "wasm32"`, the same arm as a browser on a workstation, and
/// there is one wasm binary served to both. So the shipped PWA, which is the
/// tightest real target this application has, and a browser on a 24 GB desktop
/// are indistinguishable at compile time. No `cfg` can separate them; only a
/// runtime value can.
///
/// That is the strongest argument for the whole floor/ceiling design, and it is
/// why the browser sits at its **floor** today: WebGL2 reports
/// `DeviceType::Other`, so `DeviceClass::Unknown`, so 48 MiB — which is the
/// right number for a phone browser and a conservative one for a workstation.
/// Being conservative on the target we cannot measure is the correct way round;
/// the follow-up is to *raise* the workstation browser, never to lower the
/// phone.
///
/// 48 MiB is a defensible share of what a phone browser actually has. On
/// Android our textures live in Chrome's **own GPU process**, beside a
/// compositor budgeted 96 MiB on a low-end or sub-2 GB device (256 MiB
/// otherwise) and a transfer cache of 1 MiB low-end / 128 MiB normal. On iOS
/// the whole page — GPU included — is under a 2–3 GB jetsam ceiling, with real
/// kills observed around 2.0 GB, and Safari's practical WebGL heap is somewhere
/// in a 300–500 MB band.
///
/// # What exhaustion costs in a browser, which is why the floor is where it is
///
/// Not a failed allocation. In Chrome a genuine `GL_OUT_OF_MEMORY` in the ANGLE
/// passthrough decoder **restarts the entire GPU process**, taking every
/// WebGL, WebGPU and canvas context in *every tab* with it — the decoder's
/// `force_restart` is unconditionally true — and two loss clusters get the
/// origin blocked from 3D APIs for two minutes. `gl.getError()` will usually
/// not have warned first, and nothing in WebGL, WebGPU
/// (`gpuweb#5505`, Milestone 4+) or any browser API lets a page learn it is
/// approaching the limit.
///
/// So there is no measuring and no graceful degradation to be had: the wasm
/// arm's safety is staying well under, plus learning from the one event that
/// does arrive. That event is a lost surface, which
/// `crate::volume::degrade::MAX_SURFACE_LOSSES_WITH_VOLUME` already counts two
/// of before retiring the 3D view — and which `App::back_off_loop_pool` now
/// also halves the pool on, so a machine that has lost a context once starts
/// its next session smaller instead of walking into the same wall.
///
/// wasm32 is the tight arm for a second reason as well: the whole linear memory
/// is capped at 4 GiB, and the loop is only one of several things competing for
/// it.
///
/// The three arms are named outside the cascade so the invariants can check
/// every row from one host build rather than only the row that build compiled.
#[cfg(target_arch = "wasm32")]
pub const LOOP_POOL_FLOOR_BYTES: usize = WASM_LOOP_POOL_FLOOR_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_POOL_FLOOR_BYTES: usize = MOBILE_LOOP_POOL_FLOOR_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_POOL_FLOOR_BYTES: usize = DESKTOP_LOOP_POOL_FLOOR_BYTES;

/// The wasm32 arm of [`LOOP_POOL_FLOOR_BYTES`].
pub const WASM_LOOP_POOL_FLOOR_BYTES: usize = 48 * 1024 * 1024;
/// The mobile arm. See [`LOOP_POOL_FLOOR_BYTES`].
pub const MOBILE_LOOP_POOL_FLOOR_BYTES: usize = 256 * 1024 * 1024;
/// The desktop arm. See [`LOOP_POOL_FLOOR_BYTES`].
pub const DESKTOP_LOOP_POOL_FLOOR_BYTES: usize = 512 * 1024 * 1024;

/// The most this target will ever spend on loop textures, however much memory
/// the device claims to have.
///
/// The other half of the pair [`LOOP_POOL_FLOOR_BYTES`] opens.
/// `crate::loop_pool::LoopPool::for_device` picks a value between the two from
/// `AdapterInfo::device_type`, and this is what stops a misread — or a device
/// that lies — from claiming the whole GPU.
///
/// | target  | old reachable | ceiling  | what the ceiling is measured against       |
/// |---------|--------------:|---------:|--------------------------------------------|
/// | desktop |      3072 MiB | 3072 MiB | a discrete GPU's own VRAM                   |
/// | mobile  |      1024 MiB |  640 MiB | a 4 GB iPhone's ~2001 MiB jetsam hard limit |
/// | wasm32  |       288 MiB |  192 MiB | iOS Safari's ~300–500 MB WebGL heap band    |
///
/// **Desktop's ceiling is exactly what the per-pane figure could already
/// reach**, and that is deliberate: a machine that genuinely has the memory
/// behaves exactly as it does today, so nobody with a discrete GPU loses a
/// frame of history. It is reachable only by `DeviceClass::Discrete`. An
/// integrated desktop adapter gets one doubling from the floor — 1024 MiB —
/// which against the ~50 % of system RAM Windows lets an iGPU share is 25 % of
/// an 8 GB laptop's shared pool.
///
/// # The two arms that came *down*, and the evidence for each
///
/// **mobile, 1024 → 640 MiB.** The claim this design was questioned over —
/// "1.0 GiB is more GPU memory than a mid-range phone has" — is **wrong about
/// Android and right about iOS**, and this constant covers both, because
/// `mobile` is `android | ios`.
///
/// On Android it was overblown. AnTuTu's Q2 2026 global base is 8 GB 40.0 %,
/// 12 GB 35.8 %, 6 GB 9.5 %, 4 GB 7.6 % — so ~82 % of devices have 8 GB or
/// more, where a gigabyte of textures is 6–13 % of RAM. Even the June 2026
/// Android 17 Memory Limiter, which is the first hard per-app cap Android has
/// had, allows a *visible* process "at least 1/2 and at most 2/3 of total
/// physical RAM" (AOSP `docs/core/perf/memory-limiter`), and the worked example
/// in that document is `visibleMem=1948MB` on a 4 GB device.
///
/// iOS is where it bites. A 4 GB iPhone's jetsam `ActiveHard` limit is ~2098 MB
/// for the **whole process**, Metal textures on unified memory count against
/// it, and there is no eviction or retry — the process is killed. 1024 MiB of
/// loop textures is half of that before the binary, egui, a decoded volume and
/// the network stack. 640 MiB is under a third, which is the share this
/// application is willing to be of a phone it did not choose.
///
/// It is also mostly a bound on a value nothing reaches: a phone GPU is
/// `IntegratedGpu`, so `for_device` gives it **512 MiB**, one doubling from the
/// floor. The gap between 512 and 640 is the room a future signal that
/// *measures* would have — `os_proc_available_memory()` on iOS returns exactly
/// this budget, and is the one platform API in this whole area that answers the
/// question directly.
///
/// **wasm32, 288 → 192 MiB.** iOS Safari's practical WebGL heap sits somewhere
/// around 300–500 MB (secondary sources; treat the band, not the endpoints),
/// and WebKit begins evicting at 50 % of its limit. A browser reports
/// `DeviceType::Other`, so `DeviceClass::Unknown`, so the *reachable* browser
/// pool is the 48 MiB floor and this ceiling is unreachable today — but it is
/// what a future signal would be held to, and holding it well under the
/// conservative edge of that band is the only safe place for it. There is
/// nothing to measure against in a browser: no WebGL extension reports memory,
/// WebGPU has none either (`gpuweb#5505`, opened January 2026, Milestone 4+),
/// and Chrome's answer to exhaustion is to **lose the context** rather than
/// return an error — "Chrome won't currently deliver that error — it will
/// instead lose the WebGL context", Kenneth Russell, `webgl-dev-list`.
///
/// # Why none of these is a *queried* number
///
/// Because on the two arms that matter there is nothing to query, and that is
/// documented rather than assumed:
///
/// * **wgpu 29.0.4 reports no capacity on any backend.**
///   `Device::generate_allocator_report` is this process's own suballocator and
///   is `None` outside Vulkan and DX12; `AdapterInfo` has no memory field;
///   `wgpu#2447` is still open.
/// * **Android has no API for it, by Google's own statement**: "As of Android 17
///   (SDK 37), apps don't have an API to query the memory limits at run time".
///   `ActivityManager.getMemoryClass()` bounds the **Java heap only** — it is a
///   read of `dalvik.vm.heapgrowthlimit`, plateaus at 256/512 MB even on 16 GB
///   flagships, and says nothing about a Rust process's GPU allocations. It is
///   not worth a JNI call.
/// * **`VK_EXT_memory_budget` covers 8.48 % of Android** against 88.7 % of
///   Windows and 93.1 % of Linux (`vulkan.gpuinfo.org`, fetched 2026-08-11), and
///   wgpu 29.0.4 consumes it privately anyway.
/// * **GLES and EGL expose nothing at all.**
///
/// Named outside the cascade for the reason [`WASM_VOLUME_GRID_CELLS`] gives.
#[cfg(target_arch = "wasm32")]
pub const LOOP_POOL_CEILING_BYTES: usize = WASM_LOOP_POOL_CEILING_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_POOL_CEILING_BYTES: usize = MOBILE_LOOP_POOL_CEILING_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_POOL_CEILING_BYTES: usize = DESKTOP_LOOP_POOL_CEILING_BYTES;

/// The wasm32 arm of [`LOOP_POOL_CEILING_BYTES`].
pub const WASM_LOOP_POOL_CEILING_BYTES: usize = 192 * 1024 * 1024;
/// The mobile arm. See [`LOOP_POOL_CEILING_BYTES`].
pub const MOBILE_LOOP_POOL_CEILING_BYTES: usize = 640 * 1024 * 1024;
/// The desktop arm. See [`LOOP_POOL_CEILING_BYTES`].
pub const DESKTOP_LOOP_POOL_CEILING_BYTES: usize = 3072 * 1024 * 1024;

/// The fewest frames a loop may be reduced to, however many panes are open.
///
/// **Degrade smoothly, never cliff.** A pane arriving must make its neighbours'
/// loops shorter, not blank them — a loop that vanishes when a second pane is
/// opened reads as a bug, and a user who cannot see why has no way to get it
/// back except by guessing.
///
/// Two, because a one-frame loop is not a loop; the same threshold
/// `the_3d_loop_holds_exactly_what_it_marches` already asserts for the 3D kind.
/// It is a real floor rather than a formality: [`LOOP_POOL_FLOOR_BYTES`] is
/// chosen so that even a full screen of loops on the worst device can be paid
/// for at this count, which is what makes it reachable rather than aspirational.
pub const MIN_LOOP_FRAMES_PER_PANE: usize = 2;

/// How much larger a share has to get before every loop on screen is re-planned
/// to use it.
///
/// The dead band on the *optional* direction. Deliberately the same 1.25 as
/// `crate::egui_renderer::MIRROR_RUNG_HYSTERESIS`, and the same idea one level
/// up: there, a camera drifting across a rung boundary would re-render the
/// mirror and re-fetch a tile pyramid on alternate frames; here, a pane opening
/// and closing would re-fetch and re-render every loop on screen.
///
/// It applies to growth only, and that asymmetry is the point. Being *over* the
/// pool is not something to be sticky about, so a shrink is taken as soon as the
/// dwell allows. Being under it is only a missed opportunity, and closing the
/// sixth of six panes buys each survivor 20 % more share — a frame or two of
/// history — which is not worth re-fetching the world for. Closing the second of
/// two doubles the share and is taken. See
/// `crate::loop_pool::LoopPoolState::observe`.
pub const LOOP_POOL_HYSTERESIS: f64 = 1.25;

/// How many consecutive frames the panes must ask for a different division
/// before they get one.
///
/// 15 frames is a quarter-second at 60 Hz, and it is
/// `crate::egui_renderer::MIRROR_RUNG_DWELL_FRAMES`' figure for that constant's
/// reason: the dead band above stops an oscillation at a fixed demand, and this
/// stops a *transient* — a pane being dragged into existence, a layout settling
/// after a rotation, a pane closed and immediately reopened. Under this rule
/// none of those costs a single re-render, because none of them lasts a quarter
/// of a second.
pub const LOOP_POOL_DWELL_FRAMES: u32 = 15;

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
/// close here: re-entering a resident 3D window costs the 89 ms of resample,
/// plus a few tens of milliseconds of upload on the frame thread (it was ~94 ms
/// of CPU passes and the staging copy on top, and has come down twice since;
/// see [`MAX_LOOP_VOLUME_BUILDS_PER_FRAME`] for what moved, what did not, and
/// why the upload's total is a range rather than a figure) —
/// against the 200 ms interval at [`DEFAULT_LOOP_SPEED_FPS`] and 33 ms at
/// [`MAX_LOOP_SPEED_FPS`]. The resample alone settles it at both speeds, so the
/// conclusion is the one it always was and does not rest on the upload figure —
/// which is just as well, since that figure turns out to depend on the card.
/// [`MAX_LOOP_VOLUME_FRAMES`] is therefore both numbers at once, and
/// `the_3d_loop_holds_exactly_what_it_marches` pins it.
///
/// # Why once for the application rather than once per pane
///
/// The grids live in one `VolumeStore` keyed by `VolumeTarget`, shared by every
/// 3D pane — two panes orbiting one volume from two angles already share one
/// build and one upload. So two 3D loops on the same site, product and region
/// cost one set, and the bound that matters is the store's total.
///
/// That is why the pool is divided per **loop** rather than per pane, and why
/// `crate::loop_pool::LoopDemand::volume_sets` counts distinct volume keys: a
/// naive per-pane split would charge one resident set twice and under-serve the
/// one loop kind that cannot re-render its way out of being short.
///
/// **This one is enforced at runtime**, unlike the pool statement above:
/// `VolumeStore::enforce_budget` evicts oldest-first until the resident grids
/// fit, every frame, and `the_store_eviction_actually_bounds` drives it past
/// the line. What it is held to at runtime is
/// `LoopAllocation::volume_reserve_bytes` — one share per distinct set — and
/// the frame count that share buys is chosen so the eviction never has to fire
/// for a loop and a live 3D pane together, which is the layout it would
/// otherwise fire for constantly. The `headroom` column is what buys that, and
/// `a_full_3d_loop_leaves_room_for_a_live_grid_beside_it` is why every row of
/// it is at least one grid wide.
///
/// | target  | frames | 3D texture | resident  | headroom | share   |
/// |---------|-------:|-----------:|----------:|---------:|--------:|
/// | wasm32  |      8 |  4.501 MiB |  36.0 MiB | 12.0 MiB |  48 MiB |
/// | mobile  |     12 | 15.189 MiB | 182.3 MiB | 73.7 MiB | 256 MiB |
/// | desktop |     13 | 36.001 MiB | 468.0 MiB | 44.0 MiB | 512 MiB |
///
/// # Why this is the *floor* rather than a number of its own
///
/// A loop is a loop, and a screen showing a 3D loop instead of a map loop
/// should cost about the same — so this is one share of the pool, and the table
/// above is the share a single loop gets when the pool is at
/// [`LOOP_POOL_FLOOR_BYTES`]. That makes it the **worst case** rather than the
/// only case: on a device that reports more, the share is larger and
/// `LoopPool::plan` gives the loop more frames, up to
/// [`MAX_LOOP_RENDER_BUDGET`].
///
/// The subtraction that keeps the live grid's room is inside `plan`, not baked
/// into a constant, which is what makes the property hold at *every* pool size
/// rather than at the one figure a constant was tuned against.
pub const VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = LOOP_POOL_FLOOR_BYTES;
/// The wasm32 arm of [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const WASM_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = WASM_LOOP_POOL_FLOOR_BYTES;
/// The mobile arm. See [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = MOBILE_LOOP_POOL_FLOOR_BYTES;
/// The desktop arm. See [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_LOOP_POOL_FLOOR_BYTES;

/// Frames a 3D volume loop holds — which is also how many voxel grids it keeps
/// resident, because for this loop kind those are the same number. See
/// [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
///
/// # Desktop takes fewer frames at the full grid, not more at a coarser one
///
/// 12 frames of the full 512×512×32 grid is ~60 minutes of history where 30
/// frames would be ~150. That is a real loss and it is stated rather than
/// hidden. (The shape was 256×256×128 when this was written, and is the same
/// 8,388,608 cells either way — `shape_for_budget` respends the budget rather
/// than enlarging it — so the triple moved without moving the count. What did
/// move the count is below.)
/// The alternative — a loop-specific coarser grid — was rejected for three
/// reasons, in the order they bite:
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
/// What [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`] admits **beside one live grid**,
/// and [`MAX_LOOP_RENDER_BUDGET`]. The budget binds desktop (12 grids where a
/// plan-view loop textures 30 frames); the render budget binds wasm32 and
/// mobile, where the grids are small enough that the budget would admit 9 and
/// 15 — a 3D loop is not licensed to hold *more* history than the plan-view
/// loop beside it on the same device merely because its frames are cheaper
/// there. `the_3d_loop_holds_exactly_what_it_marches` computes both and pins
/// the minimum.
///
/// # Desktop 13 → 12, and it is the same defect a second time
///
/// The correction below was made against a per-grid figure that charged the
/// two mip levels the descriptor names. A two-level descriptor is laid out
/// with **every** level down to 1×1×1, measured — see
/// `volume::raymarch::grid_bytes_at` — so a desktop grid costs 38.4 MiB and
/// not 36.0, and 13 of them beside a live one is 512.4 MiB of a 512 MiB
/// budget. That is the treadmill described below, arrived at from 1.6% of
/// accounting rather than from a missing subtraction, and it is why this arm
/// is 12.
///
/// **No budget moved.** [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`] is untouched;
/// what changed is what a grid is known to cost inside it, and
/// `the_3d_loop_holds_exactly_what_it_marches` derives this count from the two
/// rather than restating it.
///
/// The subtracted grid is the correction to the count this loop kind shipped
/// with. Desktop was 14 — the whole budget, to the last 1.5% — and the store
/// is application-wide, so a second 3D pane showing a live volume put it over
/// by 28 MiB. `enforce_budget` evicts *oldest first*, and the loop's frames
/// are older than the live grid that arrived after it, so what went was the
/// loop's own frame 0. The dispatcher re-plans it the next pass, rebuilds it
/// at ~89 ms, and the store evicts frame 1 to make room: a permanent rebuild
/// treadmill whose only symptoms are a warm machine and a loop one frame
/// short. Two 3D panes, one looping and one live, is an ordinary layout.
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
pub const DESKTOP_MAX_LOOP_VOLUME_FRAMES: usize = 12;

/// How many voxel grids a 3D loop may *dispatch* in one frame.
///
/// The exact counterpart of [`MAX_LOOP_SECTION_CUTS_PER_FRAME`], for the same
/// reason and at the same value: building a loop frame's grid needs a
/// whole-volume payload, and `RenderInput::extract_volume_parts` runs on the
/// frame thread because the job wire carries a `RenderInput`, not a `Scan`, and
/// on wasm the volume is only reachable from the main thread.
///
/// The resample (~89 ms) is off the frame thread — it is the offload job's
/// whole body. **The upload is not**, and saying it was is what let a CPU pass
/// over 8 MiB of index bytes sit in `egui_wgpu::CallbackTrait::prepare`
/// unexamined. `volume::raymarch::upload_volume_at` runs there, on the frame
/// thread, once per grid that becomes resident — which under this constant is
/// once per frame while a loop set fills.
///
/// What it costs there, desktop shape. The texel widening was **58.1 ms** as
/// per-cell arithmetic, **10.0 ms** once it became a table lookup, and is
/// **1.86 ms** since the widened plane's 32 MiB stopped being allocated inside
/// every call: that request lands just over glibc's
/// `DEFAULT_MMAP_THRESHOLD_MAX`, which is the one size the allocator's adaptive
/// threshold can never grow to cover, so it was `mmap`ed, faulted in a page at
/// a time and `munmap`ed on every upload — **8193 minor faults a call, and
/// 10.1 of the pass's 11.95 ms**, for a buffer whose life ended when
/// `write_texture` returned. `volume::raymarch::coverage_premultiplied_into`
/// has the syscall evidence, and `volume::bridge::VolumeResources::widening` is
/// the buffer that replaced it. The coarse level was **35.9 ms** on top and is
/// **5.9 ms** when it is built at all — which at reflectivity's whole-volume
/// box is never, and at the other five products' is every time (see
/// `volume::raymarch::CoarseLevel`). Its own 4 MiB allocation is
/// well under that cap, does recycle — 0 faults a call, measured — and was
/// therefore left alone.
///
/// One more thing used to be in here and is not a per-call cost at all: the
/// jitter tile. `volume::blue_noise::blue_noise_tile` was a `OnceLock` filled
/// from the *first* of these calls, so the **first** upload in a process
/// carried a void-and-cluster run — 9.90 ms natively, 31.4 ms in a browser — on
/// top of everything above, landing in the frame that first shows a volume. It
/// is an `include_bytes!` now and the whole of that is gone; that module has
/// the measurement and the determinism evidence.
///
/// # Why this constant no longer quotes one figure for the whole call
///
/// It used to, and the figure was **12.7 ms**. That was not wrong, but it was
/// not a property of the code either, and the paragraph has to say so, because
/// the same call also measures ~33 ms with nothing changed.
///
/// The whole call is **bimodal on the card's host-visible BAR occupancy**.
/// `queue.write_texture` copies the plane into write-combined memory across
/// PCIe at ~2 GB/s, and that copy costs ~15.5 ms while the RTX 3090's 246 MiB
/// host-visible window still has headroom and about a millisecond once the
/// window is saturated and wgpu stages through system RAM instead — a host
/// memcpy either way, at **0** minor faults, measured separately from anything
/// here. Whichever mode a given call lands in is decided by what else is
/// resident on the card at that instant, so a single end-to-end number
/// describes the machine's state at one moment and not this code.
///
/// What can be quoted is the pass that changed, and the fault count that
/// attributes it. On a Ryzen 9 7950X (32 threads) otherwise idle, an RTX 3090
/// through Vulkan, `cargo test --release` — so `opt-level = 3` and
/// `lto = true` — 31 interleaved pairs with the arms alternating, at the
/// default box where the coarse pass does not run: the widening **11.95 ms →
/// 1.86 ms** and **8193 → 0** minor faults, with the whole call around it
/// moving 26.26 ms → 17.30 ms in that same sweep and in that sweep's BAR mode.
/// The control is the same call at a `[128, 128, 64]` shape, whose 4 MiB plane
/// is under the cap and recycles: 2.19 ms against 2.15 ms, 0 faults in both
/// arms — which is what makes this a finding about one unrecyclable block
/// rather than about allocation in general.
///
/// # What it cost
///
/// **Host** residency, and it is now permanent rather than transient: one
/// widened plane is held for the renderer's life instead of allocated and given
/// back per upload, sized to the largest shape this process has uploaded:
/// `cells.product() * GRID_BYTES_PER_CELL`, which is **32.00 MiB** on
/// [`DESKTOP_VOLUME_GRID_CELLS`], **13.50 MiB** on
/// [`MOBILE_VOLUME_GRID_CELLS`] and **4.00 MiB** on
/// [`WASM_VOLUME_GRID_CELLS`]. That is the level-0 plane only — not
/// `grid_bytes_at(cells, CoarseLevel::Built)`, which folds in a mip this buffer
/// never holds. It is host memory, so it is outside
/// [`APP_TEXTURE_BUDGET_BYTES`] and every other budget here, all of which count
/// device textures; and a session that never opens a 3D pane never allocates it
/// at all.
///
/// What is left of the widening is one pass of 32 MiB of stores. The honest fix
/// for *that* is to widen the plane inside the offload job that already builds
/// the grid; the reason not to is wasm, where it would push 32 MiB over the
/// worker message port instead of 8 MiB.
///
/// # What the staging ring changed, and what it cost on top
///
/// The other end of it — fusing the widening into a pooled mapped staging
/// buffer, so the pass and the BAR copy become one write — has since been done,
/// and it is `volume::raymarch::staging`. On a device with
/// `staging::STAGING_RING_FEATURE` the plane is widened **straight into a
/// host-memory buffer the copy engine reads**, which removes both the second
/// pass and the blocking BAR copy: the two together were **17.56 ms best /
/// 18.38 ms median** in `prepare` on the desktop shape and are **2.04 ms best /
/// 2.26 ms median**. The bytes still cross a PCIe 4.0 x4 link at the same speed
/// they always did; what left the frame thread is the waiting.
///
/// Its residency mostly replaces the figure above rather than adding to it, and
/// "mostly" is the whole of what has to be said carefully. The ring is
/// [`staging::STAGING_RING_DEPTH`] planes — **64.00 MiB** on
/// [`DESKTOP_VOLUME_GRID_CELLS`], 27.00 MiB on [`MOBILE_VOLUME_GRID_CELLS`] —
/// and while every upload finds a free slot the widening `Vec` above is never
/// touched and stays at zero length. That is the steady state and it is
/// **+32.00 MiB of host memory** over what an open 3D session already held.
///
/// It is **not** the number to plan against. The ring is allowed to decline an
/// upload — never waiting is the property the whole design is built on — and the
/// first frame on which every slot is still feeding a copy takes the
/// `write_texture` fallback, which allocates the widening buffer and, that `Vec`
/// only ever growing, keeps it for the session. The two then coexist:
/// **96.00 MiB on desktop, +64.00 MiB**, and 40.50 MiB on the mobile rung. Both
/// figures and the condition between them are stated on
/// `volume::raymarch::staging::VolumeStaging` and pinned by a test there.
///
/// Either way it is still outside every budget here for the same reason, and
/// still nothing at all for a session that never opens a 3D pane.
/// [`WASM_VOLUME_GRID_CELLS`] is unchanged at 4.00 MiB: WebGL2 has neither the
/// feature nor a BAR window to have been slow across.
///
/// The bimodality two sections up is not repealed by any of this — it is
/// **scoped**. It was always a property of `write_texture`, and `write_texture`
/// is now the fallback arm, so the ~33 ms / ~18.6 ms swing is what a device
/// without the feature still sees and what a device with one sees on a frame its
/// ring could not serve.
///
/// One per frame means a full desktop set of 13 is dispatched over 13 frames —
/// under a quarter of a second at 60 fps — and every grid that lands is shown
/// as it lands rather than the pane blocking on the batch.
///
/// [`staging::STAGING_RING_DEPTH`]: crate::volume::raymarch::staging::STAGING_RING_DEPTH
/// [`staging::STAGING_RING_FEATURE`]: crate::volume::raymarch::staging::STAGING_RING_FEATURE
pub const MAX_LOOP_VOLUME_BUILDS_PER_FRAME: usize = 1;

/// Ceiling on the GPU texture memory the **whole application** budgets, in
/// bytes — every pane, every loop and every volume at once.
///
/// # Why this constant did not exist before, and why it has to now
///
/// The loop budget and [`VOLUME_TEXTURE_BUDGET_BYTES`] were both *per pane*,
/// and nothing multiplied either of them by the pane count. The two halves of
/// that multiplication even live in different crates — `MAX_PANES_DESKTOP` is
/// `rustdar_egui::pane`'s — so no test could have noticed. This is that missing
/// line, and writing it down is what made the loop budget a pool.
///
/// The worst case is stated as a sum rather than a maximum, and deliberately
/// over-counts: the whole loop pool at its ceiling *and* every pane's raymarch
/// offscreen at once. A pane is only ever one kind at a time and the pool is
/// divided rather than repeated, so nothing can reach this; what matters is
/// that raising any term has to come past
/// `the_whole_application_fits_its_gpu_ceiling`.
///
/// | target  | panes | loop pool | offscreens | total    | ceiling  | reachable |
/// |---------|------:|----------:|-----------:|---------:|---------:|----------:|
/// | desktop |     6 |  3072 MiB |    120 MiB | 3192 MiB | 3840 MiB |  3192 MiB |
/// | mobile  |     4 |   640 MiB |     20 MiB |  660 MiB |  768 MiB |   532 MiB |
/// | wasm32  |     6 |   192 MiB |     30 MiB |  222 MiB |  256 MiB |    78 MiB |
///
/// The last column is what the *device classification* actually admits, and it
/// is the number a memory audit should care about: a phone GPU is
/// `IntegratedGpu` and gets one doubling from the floor, and a browser is
/// `Unknown` and gets the floor itself. See [`LOOP_POOL_CEILING_BYTES`].
///
/// # What changed in this table when the loop budget became a pool
///
/// **The loop term stopped being multiplied and the 3D term disappeared into
/// it.** It used to read `panes × per-pane loop budget` *plus* a flat 3D loop
/// term, because the 3D loop's grids were the one loop kind whose budget was
/// application-wide. Now every loop kind is: the pool is divided among the
/// loops that want one, a 3D loop takes one share per *volume* rather than per
/// pane, and there is a single loop term.
///
/// The desktop total therefore fell from 3704 MiB to 3192 MiB with the ceiling
/// unmoved — the loop pool's ceiling is deliberately the same 3072 MiB the
/// per-pane figure could already reach (see [`LOOP_POOL_CEILING_BYTES`]), so
/// nothing on a machine that has the memory changed, and the 512 MiB that went
/// is the double-count the old sum carried.
///
/// **The mobile ceiling came down from 1408 MiB to 768 MiB and the wasm32 one
/// from 384 to 256 MiB.** Both follow their pool ceilings, which came down
/// against measured platform limits rather than against arithmetic — a 4 GB
/// iPhone's ~2098 MB jetsam hard limit and iOS Safari's ~300–500 MB WebGL heap
/// band. [`LOOP_POOL_CEILING_BYTES`] carries the evidence and the sources.
/// Desktop's is unmoved, because a discrete GPU has the memory and the old
/// figure was never the problem.
///
/// **What is not in this table**, and is worth saying because it is the figure
/// a memory audit will go looking for: the pool is what the *loops* may hold,
/// not what the device has. On a phone the two are not the same question at
/// all — Android's GPU allocations come out of the same DRAM as everything
/// else, `ActivityManager.getMemoryClass()` bounds only the Java heap, and
/// nothing in Vulkan, GLES or wgpu reports what is actually available. That is
/// why the pool is a floor, a ceiling and a runtime classification rather than
/// a single number: see [`LOOP_POOL_FLOOR_BYTES`] and `crate::loop_pool`.
///
/// A budget *statement*: the enforcement points are the per-subsystem ones, and
/// for the loop pool it is `LoopPool::plan`.
/// `the_app_ceiling_is_not_slack_enough_to_hide_a_doubling` keeps it snug, so it
/// cannot be quietly raised to admit whatever the constants grew into.
#[cfg(target_arch = "wasm32")]
pub const APP_TEXTURE_BUDGET_BYTES: usize = WASM_APP_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const APP_TEXTURE_BUDGET_BYTES: usize = MOBILE_APP_TEXTURE_BUDGET_BYTES;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const APP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_APP_TEXTURE_BUDGET_BYTES;

/// The wasm32 arm of [`APP_TEXTURE_BUDGET_BYTES`].
pub const WASM_APP_TEXTURE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// The mobile arm. See [`APP_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_APP_TEXTURE_BUDGET_BYTES: usize = 768 * 1024 * 1024;
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
/// `f32` value grid, so an entry is `side² × 8` bytes — and `side` is no longer
/// one number:
///
/// | render                        | side | entry   |
/// |-------------------------------|-----:|--------:|
/// | anything at or inside 230 km     | 2048 |  32 MiB |
/// | a long-range sweep, gate passed  | 4096 | 128 MiB |
///
/// Until this bound existed the only thing that ever removed an entry was
/// `reset_panes*`, so a user cycling products accumulated them without limit.
///
/// # The cap is a count, and the worst case is stated rather than bounded
///
/// A byte cap would be the obvious answer to a 4× entry and is the wrong one:
/// it would break the guarantee this number is chosen for, which is that the
/// panes on screen can never evict *each other*. Four long-range panes under a
/// byte cap sized for the common case would thrash, re-rendering a 4096 raster
/// per pane per frame. So the ceiling is stated instead: 8 × 128 MiB = 1 GiB of
/// host memory on desktop, 4 × 128 MiB = 512 MiB on mobile, both only reachable
/// with every cached entry a long-range sweep — which needs a whole cache of
/// surveillance cuts or TDWR long-range reflectivity and no Doppler, derived or
/// Level III product among them.
///
/// **Mobile is 4, exactly `MAX_PANES_MOBILE`**, down from 6. That is the
/// smallest number that still keeps the never-evict-each-other guarantee, and
/// what it gives back is the 256 MiB of headroom that 6 entries would have
/// spent on switching back and forth — on the class with the least host memory
/// and, at 4096, the largest entries. Desktop keeps 8 against 6 panes.
#[cfg(mobile)]
pub const MAX_RENDER_CACHE_ENTRIES: usize = 4;
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

/// The voxel grid budget this target is held to: the **cell count** every
/// allocation here is sized against, and the horizontal axis the grid may not
/// regress below.
///
/// # It is a budget, not the shape that gets built
///
/// It was both until the grid was rebalanced. The count is what costs memory —
/// 512 × 512 × 32 and 256 × 256 × 128 are the same 8,388,608 cells, the same
/// 32 MiB of `Rg16Float` and the same coarse level — so how it is spent over
/// the three axes is free, and spending it on the largest square the device
/// will hold is a strictly better picture at the same price.
/// `rustdar_radar::voxel::shape_for_budget` is what spends it, from the
/// adapter's own `max_texture_dimension_3d`; [`volume_grid_shape`] is this
/// crate's entry to it and [`VOLUME_GRID_FLOOR_SHAPE`] is what a device
/// reporting the bare guarantee gets, which is this triple, unchanged.
///
/// Every axis here is at or under 256 because that is what GLES 3.0 — and so
/// WebGL2 — *guarantees*, which is the floor a phone browser may legitimately
/// report. See [`WEBGL2_MAX_TEXTURE_DIMENSION_3D`]. That property is now
/// asserted where it belongs, on the floor shape rather than on the budget.
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

/// A cell triple as the `VoxelShape` a [`rustdar_radar::voxel::VoxelRequest`]
/// carries.
///
/// The axis order is x, y, z — [`VOLUME_GRID_CELLS`]'s own — and
/// `the_requested_shape_is_the_one_this_targets_budget_was_computed_for`
/// asserts that mapping on a triple whose three entries differ, because an
/// index typo here would be invisible on all three real triples: every one of
/// them has `nx == ny`.
const fn shape_of(cells: [u32; 3]) -> rustdar_radar::voxel::VoxelShape {
    rustdar_radar::voxel::VoxelShape {
        nx: cells[0] as usize,
        ny: cells[1] as usize,
        nz: cells[2] as usize,
    }
}

/// The grid shape this target should actually **request** on a device whose 3D
/// textures may be `max_axis` on a side.
///
/// [`rustdar_radar::voxel::default_shape`] cannot answer this and says so: it
/// takes one `is_wasm` bool, because `mobile` is emitted by *this* crate's
/// `build.rs` and `rustdar-radar` cannot see it. So the frontend — the one
/// crate that can — has to make the selection, and until it did, an Android
/// build budgeted every allocation against [`MOBILE_VOLUME_GRID_CELLS`]
/// (3.375 MiB of indices) while `voxel_request_for` asked `build_voxels` for
/// [`DESKTOP_SHAPE`](rustdar_radar::voxel::DESKTOP_SHAPE)'s 8 MiB — 2.4× the
/// budget, on the class with the least memory to absorb it.
///
/// `max_axis` is the **second** capability the selection needs and it is a
/// runtime one, so it arrives as an argument: `app_state`'s
/// `state.device.limits().max_texture_dimension_3d`, which the web arm of
/// `device_limits` has already lifted to whatever the browser's adapter really
/// reports. A caller with no device yet passes
/// [`WEBGL2_MAX_TEXTURE_DIMENSION_3D`] and gets [`VOLUME_GRID_FLOOR_SHAPE`] —
/// the shape that shipped — which is the conservative answer rather than a
/// guess.
///
/// Derived from [`VOLUME_GRID_CELLS`] rather than from a fourth copy of the
/// literals, so `the_grid_dimensions_match_the_shapes_rustdar_radar_names`
/// keeps this tied to the shapes `rustdar-radar` names and a drift fails by
/// name rather than by a mismatched allocation at runtime.
pub const fn volume_grid_shape(max_axis: u32) -> rustdar_radar::voxel::VoxelShape {
    rustdar_radar::voxel::shape_for_budget(shape_of(VOLUME_GRID_CELLS), max_axis as usize)
}

/// The grid this target builds on a device reporting exactly the guarantee —
/// and so the only shape here that is still a compile-time constant.
///
/// # Why this exists rather than a const assert over the derived shape
///
/// The const assert below used to run over [`VOLUME_GRID_CELLS`] itself and
/// state that the grid fits WebGL2's guaranteed 3D texture size. That was the
/// enforcement — the wasm `cargo check` row of the gauntlet evaluates it, and a
/// `#[test]` never can, because a test only exercises the arm its own runner
/// was built for. Deriving the shape from the adapter would have deleted that
/// enforcement outright, since there is no adapter at compile time.
///
/// So the guarantee is asserted where it is still constant: the shape a
/// 256-reporting device gets. That is precisely what the guarantee was ever
/// about — *can the least capable conforming browser hold what we ask for* —
/// and it is checkable at compile time on every target. Everything above the
/// floor is guarded at runtime instead, by
/// `every_axis_stays_within_the_limit_the_adapter_reported`, which sweeps the
/// limits a real adapter might report and holds each result to that device's
/// own figure. The compile-time guarantee survives where it can and the runtime
/// path gained its own guard, rather than the project trading one for none.
pub const VOLUME_GRID_FLOOR_SHAPE: rustdar_radar::voxel::VoxelShape =
    volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D);

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
/// the grid must fit, not a ceiling it is held to.
///
/// It is also the answer [`volume_grid_shape`] is given when there is no device
/// to ask, which is what makes [`VOLUME_GRID_FLOOR_SHAPE`] a constant: a device
/// reporting exactly the guarantee needs no step-down, because the shape it is
/// handed is derived against that very figure.
pub const WEBGL2_MAX_TEXTURE_DIMENSION_3D: u32 =
    wgpu::Limits::downlevel_webgl2_defaults().max_texture_dimension_3d;

/// Ceiling on what one pane's 3D volume textures may occupy, in bytes.
///
/// Not a runtime check — nothing measures against it, exactly like
/// [`LOOP_POOL_FLOOR_BYTES`]. It is the budget [`VOLUME_GRID_CELLS`] was
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
/// The table is what the device **reserves**, not what the levels pack into:
/// naming a second mip level buys the whole pyramid down to 1x1x1, measured,
/// and the tail is charged. See `volume::raymarch::grid_bytes_at`.
///
/// | target  | cell budget | mip 0     | + pyramid | + LUT, jitter | budget |
/// |---------|-------------|----------:|----------:|--------------:|-------:|
/// | desktop | 256x256x128 |    32 MiB | 36.577 MiB|    36.597 MiB | 48 MiB |
/// | mobile  | 192x192x96  |  13.5 MiB | 15.530 MiB|    15.550 MiB | 20 MiB |
/// | wasm32  | 128x128x64  |     4 MiB |  4.578 MiB|     4.598 MiB |  6 MiB |
///
/// Every arm keeps ~1.3x headroom, which is deliberate: room for a driver
/// laying textures out more coarsely than the one those figures were measured
/// on, not enough to hide a doubled axis.
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
/// Unlike [`LOOP_POOL_FLOOR_BYTES`] and [`VOLUME_TEXTURE_BUDGET_BYTES`],
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
/// rather than merely stated — unlike [`LOOP_POOL_FLOOR_BYTES`] and
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
/// **The mirror is no longer the frame.** A 3D pane draws its own map into a
/// strip below the frame and the mirror has to reach it, so what is measured
/// against this budget is up to twice the frame — bounded there, and at most one
/// extra halving, both of which `MIRROR_SCALE_MAX`'s table works through per
/// target. The budget itself is unmoved, deliberately: on the arms where the
/// doubling bites, a softer floor is the answer rather than a bigger single
/// allocation on the devices least able to spare one.
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
    assert!(LOOP_POOL_FLOOR_BYTES > 0);
    // A crossed pair would make `LoopPoolLimits::hold`'s `clamp` panic at
    // startup on one target only, which is exactly the arm a host test cannot
    // reach.
    assert!(LOOP_POOL_FLOOR_BYTES <= LOOP_POOL_CEILING_BYTES);
    assert!(MIN_LOOP_FRAMES_PER_PANE >= 2);
    assert!(LOOP_POOL_DWELL_FRAMES > 0);
    assert!(LOOP_POOL_HYSTERESIS > 1.0);
    // A share divided into frames has to buy at least the minimum for a full
    // screen of loops, or the pool cliffs where it is meant to degrade.
    assert!(LOOP_POOL_FLOOR_BYTES / MIN_LOOP_FRAMES_PER_PANE > 0);
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
    // `SECTION_WIDTH` × half of it. What every path does share is the side
    // itself: the plan-view projection assumes it is a power of two, and that is
    // also what makes the section's halved height exact and a power of two in
    // its own right rather than a truncating divide. All three plan-view sides
    // are checked, because `raster_side_from_rgba_len` will hand any of them to
    // the same projection arithmetic.
    assert!(rustdar_radar::types::IMAGE_SIZE.is_power_of_two());
    assert!(LONG_RANGE_IMAGE_SIZE.is_power_of_two());
    assert!(LOOP_IMAGE_SIZE.is_power_of_two());
    // A ceiling under the base size is a deliberate choice (the web's loop
    // frames); one *over* the largest texture the class can hold is not a
    // choice at all, and on the web it would be every render failing to
    // upload. `the_web_image_fits_the_texture_size_webgl2_guarantees` states
    // the browser half against wgpu's own limits.
    assert!(LONG_RANGE_IMAGE_SIZE >= rustdar_radar::types::IMAGE_SIZE);
    assert!(LOOP_IMAGE_SIZE <= rustdar_radar::types::IMAGE_SIZE);

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
        axis += 1;
    }
    // The guarantee, on the shape it is still a compile-time claim about. See
    // `VOLUME_GRID_FLOOR_SHAPE`: the grid a device *actually* gets is derived
    // from that device's own limit and is guarded at runtime, but the shape a
    // browser reporting exactly the guarantee is handed is constant, and it is
    // the one this assert was ever really about.
    let floor = [
        VOLUME_GRID_FLOOR_SHAPE.nx,
        VOLUME_GRID_FLOOR_SHAPE.ny,
        VOLUME_GRID_FLOOR_SHAPE.nz,
    ];
    let mut axis = 0;
    while axis < floor.len() {
        assert!(floor[axis] > 0);
        assert!(
            floor[axis] <= WEBGL2_MAX_TEXTURE_DIMENSION_3D as usize,
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
