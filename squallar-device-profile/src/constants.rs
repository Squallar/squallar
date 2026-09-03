use std::time::Duration;

/// Default width for the application window in pixels
pub const RENDER_WIDTH: u32 = 1920;

/// Default height for the application window in pixels
pub const RENDER_HEIGHT: u32 = 1080;

/// The side, in pixels, a **static** plan-view render is allowed to grow to
/// when its sweep reaches past [`squallar_radar::types::BASE_EXTENT_KM`].
pub const WASM_LONG_RANGE_IMAGE_SIZE: usize =
    squallar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D;
pub const MOBILE_LONG_RANGE_IMAGE_SIZE: usize = 4096;
pub const DESKTOP_LONG_RANGE_IMAGE_SIZE: usize = 4096;

#[cfg(target_arch = "wasm32")]
pub const LONG_RANGE_IMAGE_SIZE: usize = WASM_LONG_RANGE_IMAGE_SIZE;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LONG_RANGE_IMAGE_SIZE: usize = MOBILE_LONG_RANGE_IMAGE_SIZE;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LONG_RANGE_IMAGE_SIZE: usize = DESKTOP_LONG_RANGE_IMAGE_SIZE;

/// The largest side a static plan-view raster may reach on the desktop class,
/// whatever the adapter reports. Measured (Ryzen 9 7950X, release, KDMX 0.53°
/// cut): 4096 -> 8192 is 20.3 ms -> 118.1 ms and 464 -> 1070 MiB resident, the
/// one step in the ladder that is not linear in pixels; and a 1832-gate
/// surveillance cut needs only 7362 px at two texels per gate.
pub const DESKTOP_RASTER_SIDE_CEILING: usize = 8192;

pub const MOBILE_RASTER_SIDE_CEILING: usize = MOBILE_LONG_RANGE_IMAGE_SIZE;

/// What a browser gets before its adapter has answered: the WebGL2 2D
/// guarantee, which is also the side this build already draws.
pub const WASM_RASTER_SIDE_CEILING: usize = WASM_LONG_RANGE_IMAGE_SIZE;

/// **What a browser on a real driver earns**, once its adapter has reported
/// desktop-class ceilings — [`crate::budget::Promotion::Ceiling`].
///
/// Measured 2026-08-22 by `.github/browser-rig/run_gpu_arm.sh --also-software`,
/// one invocation, one build, four legs, each naming its adapter:
///
/// | browser | adapter | `MAX_TEXTURE_SIZE` | `MAX_3D_TEXTURE_SIZE` |
/// |---|---|---:|---:|
/// | Firefox 153 | llvmpipe (Mesa), Xvfb | 16384 | **2048** |
/// | Firefox 153 | NVIDIA GeForce GTX 980, or similar | 32768 | **16384** |
/// | Chromium 151 | SwiftShader via ANGLE | 8192 | **2048** |
/// | Chromium 151 | RTX 3090 via ANGLE | 32768 | **16384** |
///
/// **The 3D cap is what separates them, not the 2D one.** llvmpipe reports
/// 16384 in 2D — it clears `DESKTOP_CLASS_REPORT`'s 2D bar outright — and is
/// held at the floor only because both software rasterisers, two different
/// implementations, agree on 2048 in 3D. Agreement across two rasterisers is
/// what reads as a platform limit rather than an artifact, and it is why
/// `DeviceProfile::reported_promotion` conjoins the two axes.
///
/// The rung is the **mobile** tier's ceiling and not the desktop tier's, on
/// the same argument the grid-cell promotion is made on: the worst a misread
/// can do is hand a handheld browser a budget handheld hardware already runs.
/// The desktop tier's 8192 is not offered — the only figure priced for that
/// step is native and multi-threaded (see [`DESKTOP_RASTER_SIDE_CEILING`]),
/// and the web build rasterises on one thread.
///
/// **This is a ceiling, not a size.** `squallar_radar::types::raster_side_px`
/// returns `IMAGE_SIZE.min(ceiling)` for every sweep reaching
/// [`squallar_radar::types::BASE_EXTENT_KM`] or less, so an ordinary tilt draws
/// 2048 here exactly as it did before. Only a sweep past 230 km whose own
/// gates carry the detail reaches past it.
pub const WASM_RASTER_SIDE_CEILING_PROMOTED: usize = MOBILE_RASTER_SIDE_CEILING;

/// Bytes one raster of `side` costs on the host: its RGBA and its `f32` value
/// grid, four bytes each per pixel.
pub const fn raster_bytes(side: usize) -> usize {
    side * side * 8
}

/// The side a **loop frame** is rendered at — the whole side, not a ceiling on
/// a long-range one: a loop of a 458 km surveillance cut draws every frame at
/// this size, at whatever km/pixel that buys.
pub const WASM_LOOP_IMAGE_SIZE: usize = 1024;
pub const MOBILE_LOOP_IMAGE_SIZE: usize = squallar_radar::types::NATIVE_IMAGE_SIZE;
pub const DESKTOP_LOOP_IMAGE_SIZE: usize = squallar_radar::types::NATIVE_IMAGE_SIZE;

#[cfg(target_arch = "wasm32")]
pub const LOOP_IMAGE_SIZE: usize = WASM_LOOP_IMAGE_SIZE;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_IMAGE_SIZE: usize = MOBILE_LOOP_IMAGE_SIZE;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_IMAGE_SIZE: usize = DESKTOP_LOOP_IMAGE_SIZE;

/// The side a raster of `rgba_len` bytes must have been rendered at, or `None`
/// if no render this build can produce has that length.
pub fn raster_side_from_rgba_len(rgba_len: usize) -> Option<usize> {
    if !rgba_len.is_multiple_of(4) {
        return None;
    }
    let pixels = rgba_len / 4;
    let side = pixels.isqrt();
    // The bracket's **ceiling**, not the rung this build resolved: a promoted
    // browser really does produce rasters at the top of the bracket, and
    // reading the floor here would refuse to convert every one of them —
    // silently, since the caller only sees `None`.
    let ceiling = crate::budget::BudgetLimits::for_target()
        .raster_side_ceiling_px
        .ceiling;
    (side * side == pixels && side >= LOOP_IMAGE_SIZE && side <= ceiling).then_some(side)
}

/// Maximum number of concurrent background radar renders (loop + static).
/// Handhelds have much less RAM, so we cap aggressively to avoid OOM. The web
/// arm is a *worker* cap, not a memory one: the browser has one rasterization
/// worker, so anything past the first only queues behind it. Raise it in step
/// with the worker pool, not alone.
///
/// **Still 1 after WS3b, and that is the correct reading of the rule above.**
/// WS3b put threads *inside* the one rasterization worker
/// ([`WASM_MAX_RAYON_THREADS`]); it did not make a second one. A render past
/// the first would still queue behind the first, and admitting two would only
/// have them contend for the same rayon pool. The pool this cap is tied to is a
/// pool of *workers*, and it is still a pool of one.
pub const WASM_MAX_CONCURRENT_RENDERS: usize = 1;
/// Ceiling on rayon threads inside the browser's rasterization worker.
///
/// A *memory* cap, unlike [`WASM_MAX_CONCURRENT_RENDERS`] above: every rayon
/// thread is a nested Web Worker with a stack inside the single shared linear
/// memory the raster worker owns, and that memory has **no swap under it and a
/// declared ceiling of exactly 1.000 GiB**. `navigator.hardwareConcurrency` is
/// clamped to this by `squallar_web::rayon_pool::threads`.
///
/// **1 GiB is measured, not inferred**, from the memory section of the shipped
/// module — `squallar-web/pkg/squallar_web_bg.wasm`, built 2026-08-31, read
/// 2026-08-31:
///
/// ```text
/// IMPORTED MEMORY ./squallar_web_bg.js.memory
///   flags=0x03 shared=true  initial=65 pages (4.1 MiB)  maximum=16384 pages (1.000 GiB)
/// ```
///
/// This doc previously said "a hard 4 GiB". That is the architectural limit of
/// a 32-bit address space and it is **not this build's ceiling**: the module
/// declares its own maximum, so the browser refuses to grow past 16384 pages on
/// every engine and every device. It is a constant, not a per-device refusal
/// point, and nothing has to be run to learn it. The figure is also not an
/// engine property but a *build* one — a `shared` memory is required by the
/// wasm threads specification to declare a maximum, so some number had to be
/// chosen here, and this is the number that was chosen.
///
/// Everything competing for that 1 GiB is competing with these stacks: the
/// overlay picture in flight ([`WASM_MAX_CONCURRENT_RENDERS`] of them, so one),
/// every decoded granule, and every tile cache.
///
/// Native has no equivalent because rayon sizes itself there: it defaults to
/// the core count and its stacks come out of an OS address space that can
/// overcommit.
pub const WASM_MAX_RAYON_THREADS: usize = 8;

/// The ceiling of one wasm linear memory, in bytes: the `--max-memory` the
/// module is linked with (`.github/scripts/wasm-threads.sh`), which is why its
/// memory section declares the `maximum=16384 pages` quoted above.
///
/// **A build constant, not a device reading.** A `shared` memory has to state
/// a maximum at link time because it cannot be relocated on growth, so the
/// module declares its own wall and no browser and no device moves it. The
/// page and the rasterization worker are two module instances, each with its
/// own memory under this same ceiling; the two figures are never added.
///
/// Held equal to the link flag by `the_linear_memory_ceiling_is_the_link_flag`
/// in `squallar-web/tests/linear_memory_ceiling.rs`, which reads the script.
/// What a reading against it means is [`crate::linear_memory`].
pub const WASM_LINEAR_MEMORY_MAX_BYTES: u64 = 1 << 30;

pub const MOBILE_MAX_CONCURRENT_RENDERS: usize = 3;
pub const DESKTOP_MAX_CONCURRENT_RENDERS: usize = 6;

#[cfg(target_arch = "wasm32")]
pub const MAX_CONCURRENT_RENDERS: usize = WASM_MAX_CONCURRENT_RENDERS;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_CONCURRENT_RENDERS: usize = MOBILE_MAX_CONCURRENT_RENDERS;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_CONCURRENT_RENDERS: usize = DESKTOP_MAX_CONCURRENT_RENDERS;

/// The wall clock one loop covers, on wasm32.
pub const WASM_LOOP_SPAN_BUDGET_SECS: usize = 45 * 60;
pub const MOBILE_LOOP_SPAN_BUDGET_SECS: usize = 60 * 60;
pub const DESKTOP_LOOP_SPAN_BUDGET_SECS: usize = 2 * 60 * 60;

/// **How much weather a loop keeps ready to draw**, in seconds of wall clock.
/// [`MAX_LOOP_RENDER_BUDGET`] is what it costs in frames at the worst radar;
/// `crate::budget::Budgets::frames_for_span` is the per-site conversion.
/// Measured median inter-volume gap: TDWR VCP 80/90 360 s, WSR-88D precip
/// 212/215 259 s, WSR-88D clear air 35 517 s.
#[cfg(target_arch = "wasm32")]
pub const LOOP_SPAN_BUDGET_SECS: usize = WASM_LOOP_SPAN_BUDGET_SECS;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_SPAN_BUDGET_SECS: usize = MOBILE_LOOP_SPAN_BUDGET_SECS;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_SPAN_BUDGET_SECS: usize = DESKTOP_LOOP_SPAN_BUDGET_SECS;

/// Maximum number of loop frames to consider for rendering per dispatch cycle,
/// on wasm32.
pub const WASM_MAX_LOOP_RENDER_BUDGET: usize = 14;
pub const MOBILE_MAX_LOOP_RENDER_BUDGET: usize = 18;
pub const DESKTOP_MAX_LOOP_RENDER_BUDGET: usize = 36;

/// Maximum number of loop frames to consider for rendering per dispatch cycle.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_RENDER_BUDGET: usize = WASM_MAX_LOOP_RENDER_BUDGET;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_RENDER_BUDGET: usize = MOBILE_MAX_LOOP_RENDER_BUDGET;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_RENDER_BUDGET: usize = DESKTOP_MAX_LOOP_RENDER_BUDGET;

/// Maximum number of concurrent loop scan downloads per pane.
#[cfg(mobile)]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS;
#[cfg(not(mobile))]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS;

pub const MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 4;
pub const NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 8;

/// Maximum total number of loop frames kept per pane.
/// Limits combined memory from textures and scan data.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_FRAMES: usize = WASM_MAX_LOOP_FRAMES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_FRAMES: usize = MOBILE_MAX_LOOP_FRAMES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_FRAMES: usize = DESKTOP_MAX_LOOP_FRAMES;

pub const WASM_MAX_LOOP_FRAMES: usize = 14;
pub const MOBILE_MAX_LOOP_FRAMES: usize = 20;
pub const DESKTOP_MAX_LOOP_FRAMES: usize = 60;

/// How many cross-section loop frames may be *dispatched* in one frame.
/// `RenderInput::extract_volume_parts` runs on the frame thread (~1.0 ms on a
/// VCP-212 reflectivity volume), so the cap is against the 16.7 ms frame budget
/// rather than against a device class's memory — hence not a cfg cascade.
pub const MAX_LOOP_SECTION_CUTS_PER_FRAME: usize = 1;

/// How many non-radar loop frames may be *dispatched* in one pass.
///
/// Not a memory bound — that is the pane's byte share of the loop pool, which
/// `overlay_frames_held` divides and `dispatch_overlay_loop_renders` re-derives
/// every pass. This is a **burst** bound, and it is against the job funnel
/// rather than against the frame thread: an overlay raster is offloaded, but it
/// shares that funnel with the live radar and overlay rasters an interacting
/// user is waiting on. A desktop pool affords tens of frames, and switching a
/// forecast loop on would otherwise queue every one of them at once — a
/// measured CONUS HRRR rasterize is 133 ms median, so thirty of them is several
/// seconds of pool ahead of the next thing the user asks for.
///
/// Four, not one: unlike a section cut this costs the frame thread nothing, and
/// a loop that fills four frames a pass is showable in a fraction of a second
/// while still leaving the funnel room. Interaction stays realtime; the data
/// arrives when it arrives.
pub const MAX_OVERLAY_LOOP_RENDERS_PER_PASS: usize = 4;

/// The blocking-upload band: on a device with **no staging ring** — all of
/// web, and any native adapter without `MAPPABLE_PRIMARY_BUFFERS` — this is
/// both the largest texture delta that crosses whole on the frame's own queue
/// and the size of one banded `write_texture` chunk, one chunk per frame.
/// The ring path is untouched: its 8 MiB × 2-slot shape is separately
/// measured (squallar-gpu's `texture_upload` module note).
///
/// # 4 MiB is SWEPT, not chosen. Do not adjust it by feel.
///
/// Smaller bands cut the worst blocking chunk (8 MiB is ~3.8 ms through the
/// measured 2.1 GB/s BAR window; 4 MiB ~1.9 ms) but stretch a picture's
/// upload across more frames, and the pan pipeline pays for depth in **dry
/// frames** — frames where nothing the pane holds covers the viewport.
/// Re-swept 2026-08-30 on the in-module 60 Hz dispatch loop
/// (`squallar_egui::overlay_cache`'s `PanRig`, the same rig behind
/// `PAN_REBUILD_THRESHOLD`'s table): dry-frame fraction at the shipped
/// threshold 0.5, averaged over 56 continuous pan speeds from 0.25 to 3.0
/// viewports/second, 600 counted frames per speed, raster one frame, upload
/// depth = ceil(picture / cap) frames for the ~8 MiB whole-picture raster a
/// web pane ships (spike B measured 8.51 MB Firefox / 7.57 MB Chromium):
///
/// | cap        | frames/picture | dry % | first dry speed (vps) |
/// |------------|----------------|-------|-----------------------|
/// | 8 MiB      | 1              |  0.0  | none                  |
/// | **4 MiB**  | **2**          | **0.0** | **none**            |
/// | (2.67 MiB) | 3              |  9.5  | 2.15                  |
/// | 2 MiB      | 4              | 26.4  | 1.70                  |
/// | 1 MiB      | 8              | 66.9  | 0.90                  |
///
/// The depth-3 and depth-4 rows reproduce the published pan-threshold table
/// exactly, which is what says the re-run is the same instrument. 4 MiB is
/// the smallest cap that stays dry-free at every swept speed: it halves the
/// worst blocking chunk for free, and the next halving costs 26.4% of pan
/// frames their picture. The ≈1 MiB the original design card guessed is
/// refuted by the 8-frame row.
pub const BLOCKING_BAND_BYTES: usize = 4 << 20;

/// How long a frame keeps *starting* frees of what
/// squallar-worker's `offload::discard` handed it. It paces; it does not bound
/// the frame — `drain_deferred_drops` checks the clock *after* each free, so a
/// frame's real spend is this budget plus one whole payload.
///
/// A cascade because the thread it prices differs by target. On native the
/// discards ride the pool's `rd-free` lane and this budget is a dead letter —
/// only the no-worker fallback ever queues — so desktop keeps the 2 ms it has
/// always had. On wasm **every** discard queues on the page thread, the one
/// the campaign holds to a 4 ms service bar, so its arm (and mobile's, whose
/// frame is the scarcest) pays out in 500 µs slices instead. The overshoot
/// half of the story is the payloads: `Scan::into_sweeps` splits a decoded
/// volume at its sweep seam before it is filed, so the "plus one whole
/// payload" term is one sweep, not one 47–69 MiB volume.
#[cfg(target_arch = "wasm32")]
pub const DEFERRED_DROP_BUDGET_PER_FRAME: Duration = WASM_DEFERRED_DROP_BUDGET_PER_FRAME;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const DEFERRED_DROP_BUDGET_PER_FRAME: Duration = MOBILE_DEFERRED_DROP_BUDGET_PER_FRAME;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const DEFERRED_DROP_BUDGET_PER_FRAME: Duration = DESKTOP_DEFERRED_DROP_BUDGET_PER_FRAME;

pub const WASM_DEFERRED_DROP_BUDGET_PER_FRAME: Duration = Duration::from_micros(500);
pub const MOBILE_DEFERRED_DROP_BUDGET_PER_FRAME: Duration = Duration::from_micros(500);
pub const DESKTOP_DEFERRED_DROP_BUDGET_PER_FRAME: Duration = Duration::from_millis(2);

/// The **whole application's** loop allowance on a device that can tell us
/// nothing about itself, in bytes. One pool, divided among the loops that want
/// one, by squallar-app's `loop_pool`. The floor is exactly what one loop's span
/// budget costs: desktop 2 h / 36 frames / 16 MiB = 576 MiB, mobile 1 h / 18 /
/// 16 MiB = 288 MiB, wasm32 45 min / 14 / 4 MiB = 56 MiB. A browser on a phone
/// is `target_arch = "wasm32"`, not `mobile`, so no `cfg` separates it from a
/// workstation browser — which is why this is a floor and not the answer.
#[cfg(target_arch = "wasm32")]
pub const LOOP_POOL_FLOOR_BYTES: usize = WASM_LOOP_POOL_FLOOR_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_POOL_FLOOR_BYTES: usize = MOBILE_LOOP_POOL_FLOOR_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_POOL_FLOOR_BYTES: usize = DESKTOP_LOOP_POOL_FLOOR_BYTES;

pub const WASM_LOOP_POOL_FLOOR_BYTES: usize = 56 * 1024 * 1024;
pub const MOBILE_LOOP_POOL_FLOOR_BYTES: usize = 288 * 1024 * 1024;
pub const DESKTOP_LOOP_POOL_FLOOR_BYTES: usize = 576 * 1024 * 1024;

/// The most this target will ever spend on loop textures, however much memory
/// the device claims to have.
#[cfg(target_arch = "wasm32")]
pub const LOOP_POOL_CEILING_BYTES: usize = WASM_LOOP_POOL_CEILING_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_POOL_CEILING_BYTES: usize = MOBILE_LOOP_POOL_CEILING_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_POOL_CEILING_BYTES: usize = DESKTOP_LOOP_POOL_CEILING_BYTES;

pub const WASM_LOOP_POOL_CEILING_BYTES: usize = 192 * 1024 * 1024;
pub const MOBILE_LOOP_POOL_CEILING_BYTES: usize = 640 * 1024 * 1024;
pub const DESKTOP_LOOP_POOL_CEILING_BYTES: usize = 3072 * 1024 * 1024;

/// The fewest frames a loop may be reduced to, however many panes are open.
pub const MIN_LOOP_FRAMES_PER_PANE: usize = 2;

/// How long a loop waiting on its scan listing keeps its site exempt from
/// squallar-app's `App::evict_unneeded_loop_scans`.
pub const LOOP_LISTING_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// How much larger a share has to get before every loop on screen is re-planned
/// to use it.
pub const LOOP_POOL_HYSTERESIS: f64 = 1.25;

/// How many consecutive frames the panes must ask for a different division
/// before they get one.
pub const LOOP_POOL_DWELL_FRAMES: u32 = 15;

/// Ceiling on the resident voxel grids a 3D loop may hold — **for the whole
/// application**, not per pane: the grids live in one `VolumeStore` keyed by
/// `VolumeTarget`, so two panes orbiting one volume cost one set. A 3D loop's
/// frame list must *equal* its resident set — re-entering a window costs ~89 ms
/// of resample against a 200 ms interval at [`DEFAULT_LOOP_SPEED_FPS`].
/// At the floor the share buys 11 / 17 / 14 frames (wasm / mobile / desktop),
/// leaving room for one live grid beside the loop. `loop_pool`'s
/// `the_loop_budget_is_what_the_constants_derive` pins the derived figures.
pub const VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = LOOP_POOL_FLOOR_BYTES;
pub const WASM_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = WASM_LOOP_POOL_FLOOR_BYTES;
pub const MOBILE_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = MOBILE_LOOP_POOL_FLOOR_BYTES;
pub const DESKTOP_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_LOOP_POOL_FLOOR_BYTES;

/// How many voxel grids a 3D loop may *dispatch* in one frame. The resample
/// (~89 ms) is off the frame thread; `raymarch::advance_volume` is not, and
/// runs once per frame per grid becoming resident. That call is bounded — one
/// [`BLOCKING_BAND_BYTES`] band — so what this constant now holds down is the
/// number of *fills* competing for the frame thread, not the size of one.
pub const MAX_LOOP_VOLUME_BUILDS_PER_FRAME: usize = 1;

/// Ceiling on the GPU texture memory the **whole application** budgets, in
/// bytes — every pane, every loop and every volume at once.
#[cfg(target_arch = "wasm32")]
pub const APP_TEXTURE_BUDGET_BYTES: usize = WASM_APP_TEXTURE_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const APP_TEXTURE_BUDGET_BYTES: usize = MOBILE_APP_TEXTURE_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const APP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_APP_TEXTURE_BUDGET_BYTES;

pub const WASM_APP_TEXTURE_BUDGET_BYTES: usize = 288 * 1024 * 1024;
pub const MOBILE_APP_TEXTURE_BUDGET_BYTES: usize = 1024 * 1024 * 1024;
pub const DESKTOP_APP_TEXTURE_BUDGET_BYTES: usize = 3840 * 1024 * 1024;

/// What the desktop arm becomes for a machine that earned
/// [`crate::budget::Promotion::Ceiling`].
pub const DESKTOP_APP_TEXTURE_CEILING_BYTES: usize = 4032 * 1024 * 1024;

/// Bytes the building geometry on one pane -- prism positions, normals and
/// indices together -- may occupy, per class.
///
/// **One number on all three arms, and pinned, because it was measured on one
/// machine.** `squallar_buildings::budget::DEFAULT_PRISM_VRAM_BYTES` is the
/// worker side's own default for the same row and carries the measurement
/// behind it: the ground pass timed on an RTX 3090 (Vulkan, one machine,
/// 2026-08-30), where the whole shipped rung costs under 0.035 ms of a frame.
/// Nothing has timed that pass on a WebGL2 browser or on a phone, and a figure
/// scaled from a discrete desktop card is not a measurement, so the three arms
/// carry the same 16 MiB until a second machine is measured. The two constants
/// are held equal by `squallar-worker`'s agreement test.
///
/// **Buffers, not textures**: this row is a vertex and an index buffer, so it
/// is not a term of `Budgets::app_texture_bytes` and the whole-application
/// texture ceiling neither counts nor bounds it
/// (`the_prism_buffers_are_named_even_though_the_ceiling_omits_them`).
pub const WASM_PRISM_GEOMETRY_BYTES: usize = 16 * 1024 * 1024;
pub const MOBILE_PRISM_GEOMETRY_BYTES: usize = WASM_PRISM_GEOMETRY_BYTES;
pub const DESKTOP_PRISM_GEOMETRY_BYTES: usize = WASM_PRISM_GEOMETRY_BYTES;

#[cfg(target_arch = "wasm32")]
pub const PRISM_GEOMETRY_BYTES: usize = WASM_PRISM_GEOMETRY_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const PRISM_GEOMETRY_BYTES: usize = MOBILE_PRISM_GEOMETRY_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const PRISM_GEOMETRY_BYTES: usize = DESKTOP_PRISM_GEOMETRY_BYTES;
/// One mebibyte, for the tile allowances below, which are argued in MiB.
pub const fn mib(n: usize) -> usize {
    n * 1024 * 1024
}

/// The map tile caches' host-heap allowances, in bytes, as `[floor, step,
/// ceiling]` per bracket: styled entries, parsed geometry, terrain rasters.
///
/// **What they price.** Three resident populations, two slots
/// (`squallar_egui::tiles::MapTileState` holds one basemap source and one
/// terrain source; a theme flip restyles the basemap in place, so there is
/// never a second copy of either). A **styled** entry is a vector tile's
/// shapes plus its flattened `TileMeshes`; the measured city-core tail is
/// `squallar_egui::tile_source::MEASURED_STYLED_ENTRY_BYTES`, **1.46 MB**
/// (the plan's ~1.03 MB had the fills and not the strokes; the band test
/// re-derived it at 1,462,708), and a typical dense-city entry is ~30 KB. A
/// **parsed** entry is the style-independent decode a restyle re-tessellates
/// from, ~2.09 MB at the same tail (`MEASURED_PARSED_TILE_BYTES`). A
/// **terrain** entry is one 256x256 RGBA texture, 256 KiB, no tail.
///
/// **Need and economy.** The tile cache is a byte-bounded LRU with a floor in
/// entries: the working set the last pass measured (tiles on the glass plus
/// the ancestor net) is never evicted whatever the budget says, and the
/// budget bounds what is kept *beyond* it. So these figures are the economy's
/// bound and the working set's target, never a cap on the glass.
///
/// **The arithmetic, on the canvases the floors must hold.** The user's own
/// 2878x1651 browser window measured 86 tiles on the glass at a whole zoom and
/// ~106 at the half step that lands on the archive's top level (rig legs,
/// 2026-09-02; the 104/187 the grid arithmetic predicts overstate the map
/// pane, which is 0.75-0.83 of the canvas). Worst case that is 106 x 1.46 MB
/// = 155 MB of styled entries. **The 48 MiB wasm floor cannot hold that worst
/// case** — it holds 34 such entries — and is not asked to: the working-set
/// floor keeps the 106 resident as overrun, and the tile-sharpness rung
/// (whole-zoom snapping, a later landing) is what brings a scene like that
/// back under budget. What the floor does hold is 1,600 typical tiles,
/// fifteen canvases of them, and every raster the terrain slot can be asked
/// for over that window (106 x 256 KiB = 26.5 MiB against 25 MiB: the terrain
/// floor is that working set to the mebibyte, and its own floor in entries
/// carries the difference).
///
/// **Per bracket.** wasm32 is a browser tab whose linear memory is one
/// gigabyte (`WASM_LINEAR_MEMORY_MAX_BYTES`) shared with every other heap in
/// the page: 48/48/25 MiB at the floor is 121 MiB, an eighth of it. The step
/// — a desktop-class adapter report — buys 64/64/32, 160 MiB, because a tab
/// on a real driver has the RAM and the window that wants a longer history.
/// **The wasm ceiling is the wasm step**, as it is for every other field of
/// the bracket
/// (`the_web_step_is_todays_ceiling_until_a_desktop_browser_tier_is_measured`):
/// the desktop-browser tier that would fill the ceiling — 96/96/48, 240 MiB,
/// the figures the plan wrote for it — lands with its measurement (U1) and
/// not before, so a browser whose shape nobody classified resolves exactly
/// what a desktop-shaped one does. Mobile is **pinned** at the wasm floor:
/// aarch64 is unmeasured and every mobile bracket is pinned until it is
/// (`the_mobile_bracket_promotes_nothing_until_somebody_measures_aarch64`).
/// Desktop starts at 160/192/64 — 114 tail entries, the user's own window at
/// the tail (106) with eight to spare, and a parsed cache that restyles the
/// common 1920x1200 canvas (96 x 2.09 MB = 201 MB, held to 192) wholly from
/// cache; a 2560x1440 window between zooms (144 entries, 211 MB at the tail)
/// is the floor's overrun and the step's fit (256 MiB, 183 entries) — and
/// rises to 512/384/128 on a discrete adapter with a desktop shape, where a
/// 3840x2160 window between zooms (299 tiles, 437 MB at the tail) fits
/// without the floor's help.
///
/// **The host ceilings** (`*_TILE_HOST_CEILING_BYTES`) bound the three at each
/// rung the way `APP_TEXTURE_BUDGET_BYTES` bounds the GPU sum, and
/// `check_budgets` holds each within 1.25x of the sum it bounds. Terrain
/// rasters are egui textures and so GPU memory, yet they are priced here and
/// **omitted from `app_texture_bytes` by name**: the wasm GPU sum sits at 278
/// of its 288 MiB, and folding 25 MiB of hillshade into it would fail the
/// snugness proof for a population the tile cache already bounds. See
/// `the_terrain_rasters_are_omitted_from_the_gpu_sum_by_name`.
pub const WASM_TILE_STYLED_BYTES: [usize; 3] = [mib(48), mib(64), mib(64)];
pub const WASM_TILE_PARSED_BYTES: [usize; 3] = [mib(48), mib(64), mib(64)];
pub const WASM_TILE_TERRAIN_BYTES: [usize; 3] = [mib(25), mib(32), mib(32)];
/// 121 / 160 / 160 MiB of allowances at the three rungs, each held under
/// this within 1.25x. See [`WASM_TILE_STYLED_BYTES`].
pub const WASM_TILE_HOST_CEILING_BYTES: [usize; 3] = [mib(128), mib(192), mib(192)];

/// The mobile arm: the wasm floor, pinned. See [`WASM_TILE_STYLED_BYTES`].
pub const MOBILE_TILE_STYLED_BYTES: usize = WASM_TILE_STYLED_BYTES[0];
pub const MOBILE_TILE_PARSED_BYTES: usize = WASM_TILE_PARSED_BYTES[0];
pub const MOBILE_TILE_TERRAIN_BYTES: usize = WASM_TILE_TERRAIN_BYTES[0];
pub const MOBILE_TILE_HOST_CEILING_BYTES: usize = WASM_TILE_HOST_CEILING_BYTES[0];

/// The desktop arm. See [`WASM_TILE_STYLED_BYTES`].
pub const DESKTOP_TILE_STYLED_BYTES: [usize; 3] = [mib(160), mib(256), mib(512)];
pub const DESKTOP_TILE_PARSED_BYTES: [usize; 3] = [mib(192), mib(256), mib(384)];
pub const DESKTOP_TILE_TERRAIN_BYTES: [usize; 3] = [mib(64), mib(80), mib(128)];
/// 416 / 592 / 1024 MiB of allowances at the three rungs. The ceiling rung is
/// exactly its sum: nothing is slack there.
pub const DESKTOP_TILE_HOST_CEILING_BYTES: [usize; 3] = [mib(448), mib(640), mib(1024)];

/// The share of a **measured or probed** GPU capacity the scene's need may
/// occupy, as `(numerator, denominator)`: three quarters. Metal's own
/// recommended working set is ~75 % of RAM on M-series, the one vendor figure
/// for how much of a memory a renderer is meant to take; the rest is the
/// driver, the compositor and the picture in flight. **Never applied to a
/// presumed capacity**: the bracket's `APP_TEXTURE_BUDGET_BYTES` constant was
/// argued with its own headroom and is what today's sum proof already spends up
/// to, so on that arm the constant is the allowance
/// ([`crate::scene::Capacity::allowance`]).
pub const NEED_FRACTION: (u64, u64) = (3, 4);

/// **How much larger than its pane a whole-picture overlay raster is planned**,
/// per side, in percent — the overlay-oversampling rung of the ladder, top
/// first. `150` is the overdraw margin the renderer asks for
/// (`squallar_egui::overlay_cache::OVERDRAW_FRACTION` = 0.25 of the viewport
/// on each side, so 1.5x per side and 2.25x the pane's pixels); `125` halves
/// the margin (1.5625x the pixels); `100` is the viewport alone (1x).
///
/// The margin is *cover under pan*: ground the pane can still draw on while
/// the replacement raster is on its way. Giving it up costs nothing while the
/// map stands still and a blank strip at the leading edge while it pans fast,
/// until the next raster lands. It never costs a wrong pixel, never a frame
/// of input latency, and never a raster the layer would not have drawn.
///
/// Every value is a dyadic rational (3/2, 5/4, 1/1), so the planner's `f32`
/// side and the need model's integer side agree to the pixel on every pane
/// width a real adapter can hold: `a_shown_picture_is_priced_at_the_planners_own_arithmetic`.
///
/// What the three steps buy on the user's own 2878x1651 canvas, per picture:
/// 42,755,568 B, 29,682,444 B, 19,006,312 B of the page heap — and thirteen
/// shown layers re-rasterise together on every move and every loop bucket,
/// so a batch is 556, 386 or 247 MB of a 1 GiB linear memory.
pub const OVERLAY_OVERSAMPLE_PERCENTS: [u16; 3] = [150, 125, 100];

/// How full of economy — what is resident beyond need — a capacity may be
/// filled, as `(numerator, denominator)`: nine tenths. The last tenth is the
/// in-flight picture, the driver and the compositor. Also the step a session's
/// presumption is lowered by when pressure says the wall was under it: nothing
/// in this tree measures GPU residency, so the wall is taken to be at most the
/// allowance in force, less the economy the eviction just took.
pub const ECONOMY_FRACTION: (u64, u64) = (9, 10);

/// What a unified-memory GPU is taken to hold when nothing has measured it:
/// the host's RAM divided by this. **A guard, not a measurement.** Vulkan's
/// heap listing lies both ways on a UMA part — this box's llvmpipe flags 93.9
/// GiB of system RAM device-local — so the two heap-listing backends are not
/// believed for an integrated adapter and the RAM figure stands in. Metal's
/// own recommended working set is ~75 % of RAM on M-series, so a half is
/// conservative, and Metal answers for every class and replaces this wherever
/// it does ([`crate::budget::DeviceProfile::gpu_capacity_bytes`]).
pub const UNIFIED_MEMORY_GPU_DIVISOR: u64 = 2;

/// Maximum number of entries kept in `RenderDispatcher::render_cache`.
#[cfg(mobile)]
pub const MAX_RENDER_CACHE_ENTRIES: usize = MOBILE_MAX_RENDER_CACHE_ENTRIES;
#[cfg(not(mobile))]
pub const MAX_RENDER_CACHE_ENTRIES: usize = NON_MOBILE_MAX_RENDER_CACHE_ENTRIES;

pub const MOBILE_MAX_RENDER_CACHE_ENTRIES: usize = 4;
pub const NON_MOBILE_MAX_RENDER_CACHE_ENTRIES: usize = 8;

/// The per-device-class voxel grid dimensions, named **outside** the `cfg`
/// cascade so that all three are reachable from any target's tests.
pub const WASM_VOLUME_GRID_CELLS: [u32; 3] = [128, 128, 64];
pub const MOBILE_VOLUME_GRID_CELLS: [u32; 3] = [192, 192, 96];
pub const DESKTOP_VOLUME_GRID_CELLS: [u32; 3] = [256, 256, 128];

/// The voxel grid budget this target is held to: the **cell count** every
/// allocation here is sized against, and the horizontal axis the grid may not
/// regress below.
#[cfg(target_arch = "wasm32")]
pub const VOLUME_GRID_CELLS: [u32; 3] = WASM_VOLUME_GRID_CELLS;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_GRID_CELLS: [u32; 3] = MOBILE_VOLUME_GRID_CELLS;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_GRID_CELLS: [u32; 3] = DESKTOP_VOLUME_GRID_CELLS;

/// The grid shape this target should actually **request** on a device whose 3D
/// textures may be `max_axis` on a side.
pub const fn volume_grid_shape(max_axis: u32) -> squallar_radar::voxel::VoxelShape {
    volume_grid_shape_of(VOLUME_GRID_CELLS, max_axis)
}

/// [`volume_grid_shape`], for a cell budget that is not this target's own.
pub const fn volume_grid_shape_of(
    cells: [u32; 3],
    max_axis: u32,
) -> squallar_radar::voxel::VoxelShape {
    squallar_radar::voxel::shape_for_budget(
        squallar_radar::voxel::VoxelShape::of_cells(cells),
        max_axis as usize,
    )
}

/// The grid this target builds on a device reporting exactly the guarantee —
/// and so the only shape here that is still a compile-time constant.
pub const VOLUME_GRID_FLOOR_SHAPE: squallar_radar::voxel::VoxelShape =
    volume_grid_shape(WEBGL2_MAX_TEXTURE_DIMENSION_3D);

/// Bytes in the colour lookup table that travels with a voxel grid.
pub const VOLUME_LUT_BYTES: usize = 256 * 4;

/// The largest 3D texture WebGL2 is *guaranteed* to accept, per axis.
pub const WEBGL2_MAX_TEXTURE_DIMENSION_3D: u32 = 256;

/// The largest `navigator.deviceMemory` declaration that reads as a handheld:
/// Chromium's bucket edge, the top of the 2 GiB bucket. A declaration is a
/// hint the page makes about itself, so it can only **lower** a presumption —
/// a desktop-class browser declaring at most this much is held at
/// [`crate::budget::Promotion::Step`] — and never raise one: a browser that
/// declares nothing, or declares more, is promoted by its adapter report and
/// its form factor alone. Safari never declares, and Chromium's buckets stop
/// at 8 GiB, which is why the figure is a floor to fall through rather than a
/// scale to climb.
pub const DECLARED_RAM_HANDHELD_BYTES: u64 = 2 << 30;

/// Ceiling on what one pane's 3D volume textures may occupy, in bytes.
#[cfg(target_arch = "wasm32")]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = WASM_VOLUME_TEXTURE_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = MOBILE_VOLUME_TEXTURE_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_TEXTURE_BUDGET_BYTES: usize = DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES;

pub const WASM_VOLUME_TEXTURE_BUDGET_BYTES: usize = 6 * 1024 * 1024;
pub const MOBILE_VOLUME_TEXTURE_BUDGET_BYTES: usize = 20 * 1024 * 1024;
pub const DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES: usize = 48 * 1024 * 1024;

/// The largest pane, in physical pixels, the offscreen budget is sized for.
pub const VOLUME_OFFSCREEN_REFERENCE_PANE_PX: [u32; 2] = [2560, 1440];

/// Ceiling on the pane-sized `Rgba8Unorm` target one volume renders into.
pub const WASM_VOLUME_OFFSCREEN_BUDGET_BYTES: usize = 5 * 1024 * 1024;
pub const MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES: usize = 5 * 1024 * 1024;
pub const DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES: usize = 20 * 1024 * 1024;

/// What the desktop arm becomes on an adapter that named itself discrete or
/// reported desktop-class texture ceilings — `crate::budget::Promotion::Ceiling`.
pub const DESKTOP_VOLUME_OFFSCREEN_CEILING_BYTES: usize = 48 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
pub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize = WASM_VOLUME_OFFSCREEN_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize = MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_OFFSCREEN_BUDGET_BYTES: usize = DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES;

/// The largest side the pane mirror is allowed when nothing better is known.
pub const MIRROR_MAX_SIDE: u32 = 2048;

/// What the 3D view's map floor costs: **one** frame-sized colour target, for
/// the whole application, worst case.
#[cfg(target_arch = "wasm32")]
pub const VOLUME_MIRROR_BYTES_MAX: usize = WASM_VOLUME_MIRROR_BYTES_MAX;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const VOLUME_MIRROR_BYTES_MAX: usize = MOBILE_VOLUME_MIRROR_BYTES_MAX;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const VOLUME_MIRROR_BYTES_MAX: usize = DESKTOP_VOLUME_MIRROR_BYTES_MAX;

/// The guaranteed side cap squared, four bytes a texel.
pub const WASM_VOLUME_MIRROR_BYTES_MAX: usize = (MIRROR_MAX_SIDE as usize).pow(2) * 4;
pub const MOBILE_VOLUME_MIRROR_BYTES_MAX: usize = WASM_VOLUME_MIRROR_BYTES_MAX;
pub const DESKTOP_VOLUME_MIRROR_BYTES_MAX: usize = 64 * 1024 * 1024;

/// The playback rates the loop timer is willing to divide by.
pub const MIN_LOOP_SPEED_FPS: f32 = 1.0;

pub const MAX_LOOP_SPEED_FPS: f32 = 30.0;

/// What a speed that is not a number at all falls back to.
pub const DEFAULT_LOOP_SPEED_FPS: f32 = 5.0;

/// A handheld target must have been given the `mobile` cfg.
#[cfg(all(any(target_os = "android", target_os = "ios"), not(mobile)))]
compile_error!(
    "the `mobile` cfg is not set on a handheld target: squallar-device-profile's \
     build.rs did not run, or its target list is wrong. Without it this crate \
     would compile desktop memory budgets into a mobile build."
);

/// Sanity of the `cfg` cascades above, checked at compile time: a `#[test]`
/// only exercises the arm the test runner itself was built for.
const _: () = const {
    assert!(MAX_LOOP_FRAMES > 0);
    assert!(MAX_LOOP_RENDER_BUDGET > 0);
    assert!(LOOP_SPAN_BUDGET_SECS > 0);
    assert!(LOOP_POOL_FLOOR_BYTES > 0);
    // A crossed pair makes `LoopPoolLimits::hold`'s `clamp` panic at startup.
    assert!(LOOP_POOL_FLOOR_BYTES <= LOOP_POOL_CEILING_BYTES);
    assert!(MIN_LOOP_FRAMES_PER_PANE >= 2);
    assert!(LOOP_POOL_DWELL_FRAMES > 0);
    assert!(LOOP_POOL_HYSTERESIS > 1.0);
    assert!(LOOP_POOL_FLOOR_BYTES / MIN_LOOP_FRAMES_PER_PANE > 0);
    // Each tile host ceiling holds the three allowances at its rung and is
    // within 1.25x of their sum; a pinned mobile arm is one rung.
    let mut rung = 0;
    while rung < 3 {
        let wasm = WASM_TILE_STYLED_BYTES[rung]
            + WASM_TILE_PARSED_BYTES[rung]
            + WASM_TILE_TERRAIN_BYTES[rung];
        assert!(wasm <= WASM_TILE_HOST_CEILING_BYTES[rung]);
        // `ceiling <= 1.25 x sum`, spelled without the product: the desktop
        // ceiling rung is 1 GiB and `x 4` overflows a 32-bit `usize` on wasm32,
        // where these constants compile too.
        assert!(WASM_TILE_HOST_CEILING_BYTES[rung] <= wasm + wasm / 4);
        let desktop = DESKTOP_TILE_STYLED_BYTES[rung]
            + DESKTOP_TILE_PARSED_BYTES[rung]
            + DESKTOP_TILE_TERRAIN_BYTES[rung];
        assert!(desktop <= DESKTOP_TILE_HOST_CEILING_BYTES[rung]);
        assert!(DESKTOP_TILE_HOST_CEILING_BYTES[rung] <= desktop + desktop / 4);
        rung += 1;
    }
    let mobile = MOBILE_TILE_STYLED_BYTES + MOBILE_TILE_PARSED_BYTES + MOBILE_TILE_TERRAIN_BYTES;
    assert!(mobile <= MOBILE_TILE_HOST_CEILING_BYTES);
    assert!(MOBILE_TILE_HOST_CEILING_BYTES <= mobile + mobile / 4);
    // A fraction at or over one is no headroom; a zero one is no allowance.
    assert!(NEED_FRACTION.0 > 0 && NEED_FRACTION.0 < NEED_FRACTION.1);
    assert!(ECONOMY_FRACTION.0 > 0 && ECONOMY_FRACTION.0 < ECONOMY_FRACTION.1);
    // Need sits under economy: what the scene costs may not be allowed to
    // occupy more than what is resident is allowed to.
    assert!(NEED_FRACTION.0 * ECONOMY_FRACTION.1 <= ECONOMY_FRACTION.0 * NEED_FRACTION.1);
    // A divisor under one would make the guard a raise.
    assert!(UNIFIED_MEMORY_GPU_DIVISOR >= 1);
    assert!(MAX_RENDER_CACHE_ENTRIES > 0);
    assert!(MAX_CONCURRENT_RENDERS > 0);
    assert!(MAX_CONCURRENT_LOOP_DOWNLOADS > 0);
    // The loop timer divides by this; a reversed pair is a `clamp` that panics.
    assert!(MIN_LOOP_SPEED_FPS > 0.0);
    assert!(MIN_LOOP_SPEED_FPS <= DEFAULT_LOOP_SPEED_FPS);
    assert!(DEFAULT_LOOP_SPEED_FPS <= MAX_LOOP_SPEED_FPS);
    assert!(MAX_LOOP_RENDER_BUDGET <= MAX_LOOP_FRAMES);
    // The plan-view side is **not** required to be a power of two; what every
    // path shares is a floor and a ceiling, which is what
    // `raster_side_from_rgba_len` checks a finished buffer against.
    assert!(squallar_radar::types::IMAGE_SIZE > 0);
    assert!(LONG_RANGE_IMAGE_SIZE > 0);
    assert!(LOOP_IMAGE_SIZE > 0);
    // The ceiling must be at least what this build already renders, or
    // `Budgets::raster_side_for_adapter`'s bracket is inverted.
    assert!(WASM_RASTER_SIDE_CEILING >= WASM_LONG_RANGE_IMAGE_SIZE);
    assert!(MOBILE_RASTER_SIDE_CEILING >= MOBILE_LONG_RANGE_IMAGE_SIZE);
    assert!(DESKTOP_RASTER_SIDE_CEILING >= DESKTOP_LONG_RANGE_IMAGE_SIZE);
    // A promoted rung below the rung it promotes from is a bracket the
    // resolver would silently hold at the floor, so the promotion would read
    // as present and resolve as absent.
    assert!(WASM_RASTER_SIDE_CEILING_PROMOTED >= WASM_RASTER_SIDE_CEILING);
    // A ceiling over the largest texture the class can hold is every render
    // failing to upload.
    assert!(LONG_RANGE_IMAGE_SIZE >= squallar_radar::types::IMAGE_SIZE);
    assert!(LOOP_IMAGE_SIZE <= squallar_radar::types::IMAGE_SIZE);

    assert!(VOLUME_TEXTURE_BUDGET_BYTES > 0);
    // A zero axis is a texture wgpu refuses outright.
    let mut axis = 0;
    while axis < VOLUME_GRID_CELLS.len() {
        assert!(VOLUME_GRID_CELLS[axis] > 0);
        axis += 1;
    }
    // The shape a browser reporting exactly the WebGL2 guarantee is handed.
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

    // `VolumeQuality::fit` guarantees at least 1 x 1, so the budget must pay
    // for one pixel.
    assert!(VOLUME_OFFSCREEN_BUDGET_BYTES >= 4);
    assert!(VOLUME_OFFSCREEN_REFERENCE_PANE_PX[0] > 0);
    assert!(VOLUME_OFFSCREEN_REFERENCE_PANE_PX[1] > 0);
};

#[cfg(test)]
mod tests;
