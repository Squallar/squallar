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
/// # It is a floor now, not the ceiling
///
/// This used to *be* the answer for a device that could hold it, and it was a
/// literal — 4096 while the box it was written on reported 32768, eight times
/// it per axis. `budget::Budgets::raster_side_for_adapter` reads the device
/// instead, and what
/// this constant does there is guarantee that reading can only ever add: a
/// device that reports at least this much still gets at least this much, so no
/// machine that draws a 4096 raster today draws a smaller one after.
///
/// # What it costs, measured
///
/// Release, medians of 7 rasterizations of a real KDMX 0.53° cut (2022-03-05
/// 23:23) through `rustdar_radar::render`, on an AMD Ryzen 9 7950X (32
/// threads) — nothing here is on the frame thread:
///
/// | sweep                | side | render   | RGBA + grid |
/// |----------------------|-----:|---------:|------------:|
/// | reflectivity, 460 km | 2048 |   8.8 ms |      32 MiB |
/// | reflectivity, 460 km | 4096 |  20.3 ms |     128 MiB |
/// | reflectivity, 460 km | 8192 | 118.1 ms |     512 MiB |
/// | velocity, 300 km     | 2048 |   9.8 ms |      32 MiB |
/// | velocity, 300 km     | 4096 |  36.5 ms |     128 MiB |
/// | velocity, 300 km     | 8192 | 120.7 ms |     512 MiB |
///
/// **These replace a table that was roughly three times higher** — 27.7 ms and
/// 82.4 ms for the first two rows — taken on the same processor and the volume
/// this one names. Re-measured cold, one render per process, the same two rows
/// are 10.8 ms and 27.5 ms, so the gap is not a warm pool: the figures had gone
/// stale. Six further sites agree with the new ones (KCRP, KFTG, KATX, KPDT,
/// KTLX, TORD); the spread across them is 6.4–13.2 ms at 2048 and 14.6–45.0 ms
/// at 4096.
///
/// The step from 4096 to 8192 is the one that is **not** linear in pixels: four
/// times the texels for five to six times the wall clock, and the 512 MiB is
/// host bytes on top of the same again in rasterization cells. That is what
/// [`DESKTOP_RASTER_SIDE_CEILING`] is argued against, and why the ceiling is a
/// figure someone measured rather than whatever the adapter reports.
///
/// Mobile has three render slots against desktop's six and no comparable pool,
/// and is **not measured here**: no handheld was available, and a figure scaled
/// off this machine would be a guess wearing a number. The frame thread is
/// untouched either way; the conversion that used to land on it moved with an
/// earlier change (`channels::RenderedImage::image`).
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

/// The largest side a static plan-view raster may reach on the desktop class,
/// whatever the adapter reports.
///
/// **A side and not a byte budget, because the bracket it populates is a side**
/// — `budget::BudgetLimits::raster_side_ceiling_px`, whose floor is
/// [`DESKTOP_LONG_RANGE_IMAGE_SIZE`]. Bytes are still where it is *argued*, and
/// [`raster_bytes`] is what states them: 8192 is 512 MiB of RGBA and value grid
/// together, against 128 MiB at the 4096 this class ships today.
///
/// # Why it stops here
///
/// Two independent reasons that agree, which is what makes it a ceiling rather
/// than a preference.
///
/// **The data stops first.** A 1832-gate surveillance cut over ±460.11 km is
/// 2.23 texels per 0.25 km gate at 8192, and
/// `rustdar_radar::types::data_limited_side_px` will not ask for more than the
/// 7362 px two-texels-per-gate actually needs. So on the widest sweep a
/// WSR-88D flies this ceiling is never reached — the data binds a doubling
/// below it, and anything above 8192 would be sampling its own interpolation.
///
/// **The cost stops with it.** Measured on an AMD Ryzen 9 7950X (32 threads),
/// release, medians of 7 rasterizations of a real KDMX 0.53° cut: the step from
/// 4096 to 8192 is 20.3 ms to 118.1 ms — five to six times the wall clock for
/// four times the texels, the one step in the ladder that is not linear in
/// pixels — and takes process residency from 464 MiB to 1070 MiB, because the
/// rasterization cells are `side² × 8` again on top of the raster itself.
/// Doubling once more is 443 ms and 2 GiB for texels no gate can fill.
///
/// See [`LONG_RANGE_IMAGE_SIZE`] for the full ladder and the six further sites
/// it was taken over.
pub const DESKTOP_RASTER_SIDE_CEILING: usize = 8192;

/// The mobile arm, pinned to what that class already renders.
///
/// Not a claim that a handheld cannot hold more — a modern one reports 8192 or
/// 16384 and very likely can. It is a refusal to raise a ceiling on an
/// unmeasured device: mobile has three render slots against desktop's six and
/// no comparable render pool, no handheld was available, and the step this
/// ceiling would authorise is the expensive one. `constants`' own rule is that
/// a figure scaled off a desktop is a guess wearing a number.
pub const MOBILE_RASTER_SIDE_CEILING: usize = MOBILE_LONG_RANGE_IMAGE_SIZE;

/// The web arm, pinned to what the browser already renders.
///
/// The premise this used to rest on is **gone**: 2048 is what WebGL2
/// *guarantees*, not what a browser reports, and Firefox on this project's own
/// machine reports 32768. `budget::Budgets::raster_side_for_adapter` runs
/// identically here and would spend a real reading if this ceiling let it.
///
/// What holds it is the other half of the argument, which the runtime reading
/// does not touch. [`APP_TEXTURE_BUDGET_BYTES`] is 288 MiB for the whole
/// application on this class, and one 4096 raster's GPU texture alone is 64 MiB
/// of it — a fifth of the budget for one pane of one view. wasm32 is also
/// single-threaded, so the rasterization does not fan out the way every figure
/// on [`LONG_RANGE_IMAGE_SIZE`] was measured with, and **no browser render was
/// measured at any side**. Raising this needs a browser on a stopwatch, not an
/// inference from a 32-thread desktop.
pub const WASM_RASTER_SIDE_CEILING: usize = WASM_LONG_RANGE_IMAGE_SIZE;

/// Bytes one raster of `side` costs on the host: its RGBA and its `f32` value
/// grid, four bytes each per pixel.
///
/// The GPU texture beside them is the RGBA half again, and the rasterization
/// cells are another `side² × 8` while the render runs — so this is the
/// *durable* half of a raster's cost, which is the half a budget can bound.
pub const fn raster_bytes(side: usize) -> usize {
    side * side * 8
}

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
/// this whole cascade exists for.** A browser's per-pane loop budget is 56 MiB
/// ([`WASM_LOOP_TEXTURE_BUDGET_BYTES`]) and it textures fourteen frames at
/// once; 2048² frames are 16 MiB apiece, so following the static size would
/// need a 224 MiB loop budget — and [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`] is an
/// alias of that one, so the volume-store term rises with it. Even holding the
/// pool *ceiling* where it is, 192 + 224 + 30 puts [`APP_TEXTURE_BUDGET_BYTES`]
/// at 446 MiB against a 288 MiB ceiling.
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
/// naming a constant, because the side is not one number: a loop frame is
/// [`LOOP_IMAGE_SIZE`] and a static render is anywhere from
/// [`rustdar_radar::types::IMAGE_SIZE`] up to whatever
/// `budget::Budgets::raster_side_for_adapter` found this device good for, spent
/// as far as the sweep's own gates justify. Deriving it keeps `offload`'s rule that a job's
/// output carries no dimensions — the bytes are the statement.
///
/// # Why this is no longer a closed set
///
/// It used to be three constants, tried in turn. That was exactly as strong as
/// the guard needs to be while the three were the only sides that existed, and
/// it stopped being true the moment a side could be 7362 — a real surveillance
/// cut on a device that reports 32768. Left as it was, every such render would
/// have come back unrecognised and every such pane would have gone blank.
///
/// What replaces it keeps the property that mattered, which was never
/// "membership of a list" but **"a length this build could plausibly have
/// produced, checked rather than believed"**: the length must be a whole number
/// of pixels, a perfect square, and a side between the smallest frame this
/// build renders and the largest this build's own bracket allows. A buffer
/// failing any of those is refused, and a refusal is a logged blank pane rather
/// than the `ColorImage` assertion that would abort a browser tab.
///
/// The bound is the *bracket's* ceiling and not the resolved one, deliberately:
/// this is a guard on bytes that arrived over a port, and it has to answer the
/// same way whichever adapter this process happens to have met. A guard that
/// tightened with the device would refuse a cached raster from a session on a
/// larger one.
///
/// The refusals the closed set gave are all still refusals — a cross-section's
/// `2048 × 1024` is not square, a truncated raster is not a perfect square, and
/// a 512 px buffer is under the floor.
pub fn raster_side_from_rgba_len(rgba_len: usize) -> Option<usize> {
    if !rgba_len.is_multiple_of(4) {
        return None;
    }
    let pixels = rgba_len / 4;
    let side = pixels.isqrt();
    let ceiling = crate::budget::BudgetLimits::for_target().raster_side_ceiling_px;
    (side * side == pixels && side >= LOOP_IMAGE_SIZE && side <= ceiling).then_some(side)
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
/// # What a second browser worker would cost, measured
///
/// The native arm's pool (`crate::offload`) is the same architecture on a
/// target that has threads, and it does **not** move this number: the browser
/// still has exactly one instance, so the sentence above still holds. What has
/// changed is that the price of a second one is no longer unknown.
///
/// Each worker is its own wasm instance with its own linear memory, and that
/// memory only grows. Measured by reading `WebAssembly.Memory.buffer.byteLength`
/// inside four freshly-booted instances of this build, each given one real job:
///
/// |  | Firefox 153 | Chrome 151 |
/// |---|---|---|
/// | at boot | 3.63 MiB | 3.63 MiB |
/// | after one 2048 px Level II render | 87.06 MiB | 87.06 MiB |
/// | after one 16.9 MB Level II decode | 200.81 MiB | 200.81 MiB |
///
/// Byte-identical across the two engines and across all four instances, which
/// is what says it is a property of this module rather than of a browser. **A
/// pool sized from `navigator.hardwareConcurrency` would be a catastrophe**: 32
/// on the box these were taken on, and the decode row makes that 6.3 GiB of
/// linear memory before a single texture. If this is ever raised, it is
/// resolved against that column and not against a core count.
///
/// The other half of the same finding is that a second worker is **not** the
/// way to fix a render queued behind decodes. With the eight decodes
/// [`MAX_CONCURRENT_LOOP_DOWNLOADS`] permits already posted, a render answers in
/// 8210.6 ms (Firefox) / 7783.8 ms (Chrome). Letting the page post the render
/// first — one worker, no extra memory — answers in 185.5 ms / 194.1 ms, and a
/// second worker answers in 190.4 ms / 201.4 ms. The two are the same latency
/// and one of them is free.
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

/// The wall clock one loop covers, on wasm32. See [`LOOP_SPAN_BUDGET_SECS`];
/// named outside the cascade for the reason [`WASM_VOLUME_GRID_CELLS`] gives.
pub const WASM_LOOP_SPAN_BUDGET_SECS: usize = 45 * 60;
/// The mobile arm. See [`LOOP_SPAN_BUDGET_SECS`].
pub const MOBILE_LOOP_SPAN_BUDGET_SECS: usize = 60 * 60;
/// The desktop arm. See [`LOOP_SPAN_BUDGET_SECS`].
pub const DESKTOP_LOOP_SPAN_BUDGET_SECS: usize = 2 * 60 * 60;

/// **How much weather a loop keeps ready to draw**, in seconds of wall clock.
///
/// This is the loop budget. [`MAX_LOOP_RENDER_BUDGET`] is what it costs in
/// frames at the worst radar; `crate::budget::Budgets::frames_for_span` is the
/// conversion, and it is a per-site one.
///
/// # A frame is not a unit of anything the user cares about
///
/// One frame is one volume scan, and a volume scan is not a fixed length of
/// time. Measured 2026-08-11 over a full 24 h, with the VCP decoded per file
/// from message 5 rather than inferred from the interval, across six TDWR and
/// four WSR-88D sites plus a two-day two-extra-site holdout:
///
/// | radar | VCP | median inter-volume gap | n |
/// |---|---|---:|---:|
/// | TDWR | 80 | 360.0 s | 569 |
/// | TDWR | 90 | 360.0 s | 832 |
/// | WSR-88D precip | 212 / 215 | 259.0 s | 942 |
/// | WSR-88D clear air | 35 | 517.0 s | 179 |
///
/// **A TDWR volume is six minutes and a WSR-88D precip volume is four**, so the
/// widely-repeated claim that TDWR is the fastest-cadence radar is backwards:
/// a TDWR loop covers *more* wall clock per frame, not less. Under the old
/// 30-frame desktop budget the same slider bought 2 h 05 m on a WSR-88D in
/// precip, 2 h 54 m on a TDWR and 4 h 18 m on a WSR-88D in clear air — three
/// different amounts of weather from one number, and nothing on screen said so.
///
/// Stating the budget in seconds makes the loop mean one thing everywhere, and
/// makes the frame count the *derived* quantity it always was.
///
/// # Each arm is the longest window its texture ceiling can pay for
///
/// The cost is not `span / median`: the count has to hold at the **fastest**
/// listing a site can present, not at its typical one. Swept over every window
/// of the measured day — see
/// `tests::MEASURED_PEAK_LOOP_FRAMES` for the table and the sweep that
/// produced it — the worst case is always a WSR-88D in precip, never a TDWR:
///
/// | span | TDWR | KPBZ | KLOT | KFTG | KOKX | frames | one loop | pool floor |
/// |---|---:|---:|---:|---:|---:|---:|---:|---:|
/// | 45 min |  8 | 14 | 14 | 12 | 12 | **14** |  56 MiB |  56 MiB |
/// | 1 h    | 11 | 18 | 18 | 16 | 16 | **18** | 288 MiB | 288 MiB |
/// | 1.25 h | 13 | 23 | 22 | 20 | 20 |     23 | 368 MiB |  — |
/// | 2 h    | 21 | 36 | 35 | 30 | 30 | **36** | 576 MiB | 576 MiB |
/// | 2.5 h  | 26 | 44 | 44 | 38 | 37 |     44 | 704 MiB |  — |
///
/// The windows are round ones a user can hold in their head rather than the
/// byte-maximal figure each arm could reach — wasm32 could pay for 50 minutes
/// and takes 45, which is the only place any arm leaves anything on the table,
/// and it leaves it on the arm whose answer to exhausting texture memory is to
/// restart the browser's whole GPU process.
///
/// [`APP_TEXTURE_BUDGET_BYTES`] leaves the pool floor 66 MiB on wasm32,
/// 364 MiB on mobile and 648 MiB on desktop, so the next row up fails on every
/// arm — 1 h costs a browser 72 MiB, 1.25 h costs a phone 368 MiB, 2.5 h costs
/// a desktop 704 MiB. These three are the longest windows that fit, and
/// `the_span_budget_is_the_longest_the_ceiling_can_pay_for` is where a change
/// to any of the six numbers has to come past.
#[cfg(target_arch = "wasm32")]
pub const LOOP_SPAN_BUDGET_SECS: usize = WASM_LOOP_SPAN_BUDGET_SECS;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_SPAN_BUDGET_SECS: usize = MOBILE_LOOP_SPAN_BUDGET_SECS;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_SPAN_BUDGET_SECS: usize = DESKTOP_LOOP_SPAN_BUDGET_SECS;

/// Maximum number of loop frames to consider for rendering per dispatch cycle,
/// on wasm32. See [`MAX_LOOP_RENDER_BUDGET`]; named outside the cascade for the
/// reason [`WASM_VOLUME_GRID_CELLS`] gives.
pub const WASM_MAX_LOOP_RENDER_BUDGET: usize = 14;
/// The mobile arm. See [`MAX_LOOP_RENDER_BUDGET`].
pub const MOBILE_MAX_LOOP_RENDER_BUDGET: usize = 18;
/// The desktop arm. See [`MAX_LOOP_RENDER_BUDGET`].
pub const DESKTOP_MAX_LOOP_RENDER_BUDGET: usize = 36;

/// Maximum number of loop frames to consider for rendering per dispatch cycle.
///
/// Also the steady-state cap on *textured* frames per pane:
/// `LoopPlaybackState::evict_textures_outside_render_set` is called with this every
/// dispatch and drops the texture of every frame outside the render set. That makes
/// this — not `MAX_LOOP_FRAMES` — the binding term in the per-pane texture budget.
///
/// # It is [`LOOP_SPAN_BUDGET_SECS`] priced at the fastest radar, not a number of its own
///
/// The budget is the span; this is what the span costs where it costs the most.
/// A loop on a slower radar takes fewer frames for the same wall clock and this
/// is never reached — a TDWR needs 21 of a desktop's 36 for the same two hours,
/// and a WSR-88D in clear air 14. So the arms are the *ceiling* on a per-site
/// figure now rather than the figure itself, and
/// `crate::budget::Budgets::frames_for_span` is where the site's own answer
/// comes from.
///
/// | target  | span   | this | what it used to be | what that used to guarantee |
/// |---------|-------:|-----:|-------------------:|----------------------------:|
/// | wasm32  | 45 min |   14 |                  8 |                      26 min |
/// | mobile  |    1 h |   18 |                 12 |                      39 min |
/// | desktop |    2 h |   36 |                 30 |                  1 h 41 min |
///
/// The right-hand column is the same sweep read backwards, and it is the reason
/// the arms moved at all: a frame count is a promise about *pictures*, and the
/// promise about time hiding inside it was shorter than anyone would have
/// guessed and different on every arm.
#[cfg(target_arch = "wasm32")]
pub const MAX_LOOP_RENDER_BUDGET: usize = WASM_MAX_LOOP_RENDER_BUDGET;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const MAX_LOOP_RENDER_BUDGET: usize = MOBILE_MAX_LOOP_RENDER_BUDGET;
/// See the wasm32 arm above.
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const MAX_LOOP_RENDER_BUDGET: usize = DESKTOP_MAX_LOOP_RENDER_BUDGET;

/// Maximum number of concurrent loop scan downloads per pane.
///
/// The arms are named outside the cascade for the reason
/// [`WASM_VOLUME_GRID_CELLS`] gives, and because `crate::budget::BudgetLimits`
/// has to be able to reach both from one host build. A two-arm `mobile` /
/// `not(mobile)` cascade has no `target_arch` arm, so a host build already
/// picks between the same two values a phone build would — but it picks *one*,
/// and the bracket names both.
#[cfg(mobile)]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS;
/// See the `mobile` arm above.
#[cfg(not(mobile))]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS;

/// The `mobile` arm of [`MAX_CONCURRENT_LOOP_DOWNLOADS`].
pub const MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 4;
/// Every other target's arm. See [`MAX_CONCURRENT_LOOP_DOWNLOADS`].
pub const NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 8;

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
///
/// 12 until [`LOOP_SPAN_BUDGET_SECS`] priced a browser's 45 minutes at 14
/// frames. A held frame is scan data and a timestamp rather than a texture, so
/// the two extra cost the loop pool nothing — but a hold cap *below* the render
/// budget would be a loop that textures every frame it has and still cannot
/// reach the span it was budgeted, which
/// `the_render_budget_is_what_bounds_the_textured_frames` is the guard against.
pub const WASM_MAX_LOOP_FRAMES: usize = 14;
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
/// expensive half is on the worker, and a full desktop render set of 36 frames
/// is dispatched over 36 frames — six tenths of a second at 60 fps, during which the
/// pane shows every frame that has landed rather than blocking on the batch.
///
/// It is deliberately not a per-target cascade. The number is chosen against
/// the *frame budget*, which is 16.7 ms everywhere, rather than against a device
/// class's memory; and wasm's `MAX_CONCURRENT_RENDERS` of 1 already imposes the
/// same limit there by another route.
pub const MAX_LOOP_SECTION_CUTS_PER_FRAME: usize = 1;

/// How long a frame keeps *starting* frees of what
/// [`crate::offload::discard`] handed it.
///
/// # It paces; it does not bound the frame, and the difference matters here
///
/// [`crate::offload::drain_deferred_drops`] checks the clock **after** each
/// free, because a check before one would let a zero budget — or a clock too
/// coarse to resolve a free — stop the queue entirely. The cost of that choice
/// is that a frame's real spend is this budget *plus one whole payload*, and on
/// wasm32, the only arm that queues in production, one payload can be a 47–69
/// MiB `DecodedScan`: precisely the free this whole mechanism exists to keep
/// off a frame. **A single large free is not made smaller by any number here.**
///
/// What the web arm buys is therefore *pacing* rather than a bound: sixty
/// volumes evicted at once are freed a few per frame instead of all on one, and
/// the map keeps moving between them. The bound proper is the native arm's,
/// where the free happens on the pool's lane and the frame pays nothing.
///
/// # A duration, because the thing being paced is not a countable object
///
/// This started as a count of payloads per frame, and the count could not be
/// justified. A cap of *n* entries is only a cap on frame time if every entry
/// costs about the same, and these do not: one is whatever a caller discarded,
/// from a `PolarField` to a whole `DecodedScan` — 47–69 MiB across thousands
/// of per-radial buffers. Pricing a count needs a per-free millisecond figure
/// for the browser that nobody has measured, on payloads of no one size, and
/// the first attempt got it by multiplying a *loop frame* count by a
/// *decoded volume* cost — two different objects, since a loop caches pictures
/// at ~4 MiB each (see [`LOOP_POOL_FLOOR_BYTES`]) and volumes live in the scan
/// caches, not in loop frames.
///
/// A duration needs none of that. It is priced against the frame, which is
/// 16.7 ms on every target and is the quantity actually being protected, and
/// it self-calibrates: a frame full of cheap payloads retires many and a frame
/// handed one enormous one retires it and stops.
///
/// # Two milliseconds, and what bounds it either way
///
/// An eighth of a frame, and `the_teardown_slice_paces_rather_than_stalls`
/// pins it at that rather than at the looser bound it could have been given.
/// The lower bound is not a floor on progress — the drain frees at least one
/// payload per call whatever this says, so the queue empties even at zero — it
/// is a floor on *rate*: too small a slice makes a large teardown take more
/// frames than the eviction that caused it, and the memory the app decided it
/// wanted back stays resident longer. The upper bound is the frame: this is
/// overhead against drawing, spent on work nothing is waiting for.
///
/// The **whole** per-frame cost of a draining queue is larger than this number
/// twice over, and both are deliberate: the overrun above, and the re-arm, which
/// asks for another frame while anything is queued — so a teardown is paced at
/// this budget *plus a rendered frame* apiece, at whatever rate the display
/// runs. That is the price of draining without waiting for the user, and
/// `crate::offload::drain_deferred_drops` is where it is argued.
///
/// [`MAX_LOOP_SECTION_CUTS_PER_FRAME`]'s ~1.0 ms is the nearest measured figure
/// in this file, and it is cited as a scale rather than as a precedent: that
/// millisecond is *foreground* work a pane is waiting on, and this one is not,
/// which is an argument for staying under it rather than for matching it.
///
/// Deliberately not a per-target cascade, for
/// [`MAX_LOOP_SECTION_CUTS_PER_FRAME`]'s reason: it is chosen against the frame
/// budget, which is 16.7 ms everywhere, rather than against a device class's
/// memory.
pub const DEFERRED_DROP_BUDGET_PER_FRAME: std::time::Duration = std::time::Duration::from_millis(2);

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
/// **The floor is exactly what one loop's span budget costs.** Not slack and
/// not a round number — it is the property that makes the pool safe to ship: a
/// session with one loop open, on the worst device this target admits, gets the
/// whole of [`LOOP_SPAN_BUDGET_SECS`] textured at the fastest radar there is,
/// because one loop's share of a floor-sized pool *is*
/// [`MAX_LOOP_RENDER_BUDGET`] frames. What the pool changed is that six of them
/// no longer cost six times it.
///
/// It used to be "exactly what one pane used to get all to itself", which was
/// the same equality read off the frame count instead of the span — the arms
/// were 480 / 192 / 32 MiB against 512 / 256 / 48 MiB floors, and the rounding
/// up to a power of two was the only slack in it. Pricing the span rather than
/// the frame count removed even that.
///
/// A plan-view frame is a [`LOOP_IMAGE_SIZE`]² RGBA raster — not the size a
/// static pane render takes, because a loop's frames are held by the dozen and
/// a still frame is held once. On the web that difference is the whole reason
/// this budget still fits; natively the two are the same 2048.
///
/// | target  | span   | textured | frame size | one loop | floor   |
/// |---------|-------:|---------:|-----------:|---------:|--------:|
/// | desktop |    2 h |       36 |     16 MiB |  576 MiB | 576 MiB |
/// | mobile  |    1 h |       18 |     16 MiB |  288 MiB | 288 MiB |
/// | wasm32  | 45 min |       14 |      4 MiB |   56 MiB |  56 MiB |
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
/// the width class admits. wasm32 is the tight row — six loops at two frames of
/// 4 MiB is 48 MiB against a 56 MiB floor, one frame of slack in the whole
/// browser arm — and `the_floor_seats_every_pane_without_blanking_one` is where
/// a change to any of those four numbers has to come past. It was exact to the
/// byte at the old 48 MiB floor; the eight the span budget added are the only
/// margin that rule has ever had.
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
/// `DeviceType::Other`, so `DeviceClass::Unknown`, so 56 MiB — which is the
/// right number for a phone browser and a conservative one for a workstation.
/// Being conservative on the target we cannot measure is the correct way round;
/// the follow-up is to *raise* the workstation browser, never to lower the
/// phone.
///
/// 56 MiB is a defensible share of what a phone browser actually has. On
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
/// of before retiring the 3D view — and which `App::back_off_budgets` now
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
pub const WASM_LOOP_POOL_FLOOR_BYTES: usize = 56 * 1024 * 1024;
/// The mobile arm. See [`LOOP_POOL_FLOOR_BYTES`].
pub const MOBILE_LOOP_POOL_FLOOR_BYTES: usize = 288 * 1024 * 1024;
/// The desktop arm. See [`LOOP_POOL_FLOOR_BYTES`].
pub const DESKTOP_LOOP_POOL_FLOOR_BYTES: usize = 576 * 1024 * 1024;

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
/// integrated desktop adapter gets one doubling from the floor — 1152 MiB —
/// which against the ~50 % of system RAM Windows lets an iGPU share is 28 % of
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
/// `IntegratedGpu`, so `for_device` gives it **576 MiB**, one doubling from the
/// floor. The gap between 576 and 640 is the room a future signal that
/// *measures* would have — `os_proc_available_memory()` on iOS returns exactly
/// this budget, and is the one platform API in this whole area that answers the
/// question directly. That gap narrowed when [`LOOP_SPAN_BUDGET_SECS`] took the
/// floor from 256 MiB to 288: a doubling of the floor is the *only* step
/// between the two, so raising the floor moves the reachable value twice as
/// fast as it moves the floor, and one more doubling of the span budget would
/// put the integrated arm at the ceiling rather than under it.
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

/// How long a loop waiting on its scan listing keeps its site exempt from
/// `App::evict_unneeded_loop_scans`.
///
/// # Why the exemption needs a clock at all
///
/// A loop in `LoopPhase::FetchingScanList` names no frame, so the sweep would
/// take its whole window in the gap before the listing installs one — every
/// product switch and every loop re-init re-downloading what it already had.
/// The exemption is what prevents that, and this is what prevents the exemption
/// from being permanent.
///
/// It is not a formality. **On wasm32 nothing else ever ends it**:
/// `rustdar_radar::tls::client` accepts and ignores the timeout it is handed,
/// because reqwest's wasm `ClientBuilder` has no `timeout` and a browser
/// `fetch()` has no default of its own, so a black-holed connection leaves the
/// listing future pending for the life of the tab. `settle_loop_phase` returns
/// early on an empty frame list and `accept_scan_listing` never runs, so the
/// phase never moves — while the poll and chunk-feed paths go on writing a
/// volume per seal into the very cache the exemption is protecting. Without
/// this the leak resumes at full rate inside the 4 GiB address space.
///
/// # Why sixty seconds
///
/// A scan listing is an S3 directory listing of a few kilobytes; a healthy one
/// answers in well under a second, and a bad mobile link in a few. Sixty is two
/// orders of magnitude above that, so no honest listing is cut off.
///
/// It is priced from the other end too. At the 0.4–1 GB/h accumulation this
/// sweep exists to stop, a 60 s exemption is worth at most ~17 MB — a figure
/// under a single decoded volume's own order of magnitude, rather than a
/// fraction of an address space. Native `ARCHIVE_TIMEOUT` is 300 s **per
/// request** and `list_scans_for_range` issues one listing per UTC day the
/// window touches, so this bounds the native stall well before the transport
/// does, and it is the only bound on wasm.
///
/// A listing that really does overrun this is not lost: the sweep sheds that
/// window, the listing lands, and the loop re-downloads it — the same cost as
/// toggling the loop off and on, which this feature already accepts.
pub const LOOP_LISTING_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

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
/// A 3D loop's frame count is therefore both numbers at once — held and
/// resident are one figure for this kind — and
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
/// **This one is enforced at runtime**, and so — the doc here used to deny it —
/// is the pool statement above: `VolumeStore::enforce_budget` evicts
/// oldest-first until the resident grids fit, every frame, and
/// `the_store_eviction_actually_bounds` drives it past the line. What it is
/// held to at runtime is `LoopAllocation::volume_reserve_bytes` **floored at
/// this constant** — `App::setup_egui_frame` takes the `max` of the two, so a
/// session with no 3D loop still has room for the live grids ordinary 3D panes
/// need — one share per distinct set — and
/// the frame count that share buys is chosen so the eviction never has to fire
/// for a loop and a live 3D pane together, which is the layout it would
/// otherwise fire for constantly. The `headroom` column is what buys that, and
/// `a_full_3d_loop_leaves_room_for_a_live_grid_beside_it` is why every row of
/// it is at least one grid wide.
///
/// The `3D texture` column is `volume::raymarch::resident_grid_bytes` — every
/// mip level the device lays the grid out with, plus the colour table's own
/// texture and the jitter tile created beside it — not the packed product of
/// the two levels the descriptor names. The difference is 1.6% and it is the
/// difference between twelve frames and thirteen; see
/// `volume::raymarch::grid_bytes_at`.
///
/// ## What that column costs on a backend that does not lay the pyramid out
///
/// Not every backend does, and the one CI runs on does not: Mesa's lavapipe
/// reserves the levels the descriptor names and nothing under them, so the
/// column above over-states by **1.6% to 2.3%** there — 606,208 B on desktop,
/// 359,424 B on mobile, 81,920 B on wasm, measured. It is a real over-charge and
/// it is spent on nothing.
///
/// **It buys no frame back at any shipped rung.** Run through `LoopPool::plan`
/// with the honest figure substituted, at every share count from one to six:
///
/// * **At the floor — every row of the table above — the count is identical.**
///   11, 17 and 14 either way. The desktop row is the near miss and it is worth
///   naming: 14.74 frames on the charge against **14.99** on the device's own
///   figure, which is nine thousandths of a frame short of a fifteenth and does
///   not get it. wasm 11.18 against 11.39, mobile 17.52 against 17.94.
/// * **At `floor * 2`, which is what `Integrated` takes, also identical**, all
///   three arms, all six share counts.
/// * At [`LOOP_POOL_CEILING_BYTES`], which only `Discrete` takes, the honest
///   figure buys one more frame at three or more concurrent loops — 26 → 27 on
///   desktop, 12 → 13 on wasm and mobile.
///
/// The last row is the one that would matter and **it is not a configuration
/// anything has been measured in**: a lavapipe adapter reports `DeviceType::Cpu`
/// and classifies `Software`, which takes the *floor*, and the only discrete GPU
/// this has been read on lays the whole pyramid out and is not over-charged at
/// all. A discrete adapter using the named-levels layout is what would have to
/// exist for the 1.6% to cost a user a frame; RADV is the obvious candidate and
/// no AMD hardware was available to read it on. Charging what the device
/// actually reserves rather than the worst case is the fix if one turns up, and
/// it is a change to this budget's architecture rather than to the arithmetic —
/// see `volume::raymarch::TEXTURE_ALLOCATION_SLACK_BYTES`.
///
/// Every figure below is derived and checked — `the_loop_budget_table_is_the
/// _one_the_constants_derive` reads these rows back out of this doc comment and
/// fails if any of them has drifted from `resident_grid_bytes`. It is here
/// because they had: the table went on quoting a 13-frame desktop row at
/// `36.001 MiB` for a while after the mip pyramid was charged for and the frame
/// count became 12, so the prose and the code contradicted each other in the
/// same file.
///
/// The decimals are load-bearing rather than cosmetic: that test compares the
/// cells as **strings**, at `{:.3}` for the texture column and `{:.2}` for the
/// two derived from it, so a row rounded to one place fails it.
///
/// | target  | frames | 3D texture | resident  | headroom | share   |
/// |---------|-------:|-----------:|----------:|---------:|--------:|
/// | wasm32  |     11 |  4.598 MiB |  50.57 MiB |  5.43 MiB |  56 MiB |
/// | mobile  |     17 | 15.550 MiB | 264.35 MiB | 23.65 MiB | 288 MiB |
/// | desktop |     14 | 36.598 MiB | 512.37 MiB | 63.63 MiB | 576 MiB |
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
///
/// # The `frames` column is derived, and there is no constant for it any more
///
/// There was: `MAX_LOOP_VOLUME_FRAMES`, an 8 / 12 / 12 cascade with three named
/// arms. It had **no runtime consumer at all** — `LoopPool::plan` computed the
/// real number from this budget and the grid's cost, and the constant sat
/// beside it as a pinned expectation that the test re-derived. Two arithmetics
/// for one number, one of which nothing executed, is the shape that lets a
/// budget change land in one of them; the count is the planner's answer now,
/// and `the_pool_reproduces_the_shipped_3d_frame_count` pins the planner
/// against the literals in the table above.
///
/// What that retirement must not lose is the reasoning, which is below.
///
/// ## Desktop takes fewer frames at the full grid, not more at a coarser one
///
/// 14 frames of the full 512×512×32 grid is ~56 minutes of a WSR-88D in precip
/// where the 2 h [`LOOP_SPAN_BUDGET_SECS`] buys a plan-view loop beside it.
/// That is a real loss and it is stated rather than hidden — it is also the one
/// place in this application where a loop does *not* deliver the span budget,
/// and it is bounded by bytes rather than by a decision. (The shape was
/// 256×256×128 when this was written, and is the same
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
/// ## Each arm is the tighter of two bounds
///
/// What this budget admits **beside one live grid**, and
/// [`MAX_LOOP_RENDER_BUDGET`]. The budget binds every arm now — 11, 17 and 14
/// grids against render budgets of 14, 18 and 36 — where it used to bind
/// desktop alone. That is [`LOOP_SPAN_BUDGET_SECS`]' doing and it is the right
/// way round: a grid is 4 to 9 times a raster frame, so pricing the span in
/// frames buys a raster loop the whole window and a 3D loop as much of it as
/// the grids allow. The render budget is still the other half of the minimum,
/// because a 3D loop is not licensed to hold *more* history than the plan-view
/// loop beside it on the same device merely because its frames are cheaper
/// there. `the_3d_loop_holds_exactly_what_it_marches` computes both and pins
/// the minimum.
///
/// ## Desktop 13 → 12, and it is the same defect a second time
///
/// The correction below was made against a per-grid figure that charged the
/// two mip levels the descriptor names. A two-level descriptor is laid out
/// with **every** level down to 1×1×1, measured — see
/// `volume::raymarch::grid_bytes_at` — so a desktop grid costs 36.6 MiB and
/// not 36.0, and 13 of them beside a live one was 512.4 MiB of the 512 MiB
/// budget of the day. That is the treadmill described below, arrived at from
/// 1.6% of accounting rather than from a missing subtraction, and it is why the
/// arm was 12 while the floor was 512 MiB.
///
/// **No budget moved for that one.** What changed was what a grid is known to
/// cost inside it. The arm is 14 today for the opposite kind of reason: the
/// budget itself moved, because it is [`LOOP_POOL_FLOOR_BYTES`] and that floor
/// is now [`LOOP_SPAN_BUDGET_SECS`] priced in raster frames. The subtraction
/// below is what keeps the count honest through a budget change rather than
/// only through a cost change.
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
pub const VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = LOOP_POOL_FLOOR_BYTES;
/// The wasm32 arm of [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const WASM_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = WASM_LOOP_POOL_FLOOR_BYTES;
/// The mobile arm. See [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = MOBILE_LOOP_POOL_FLOOR_BYTES;
/// The desktop arm. See [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_VOLUME_LOOP_TEXTURE_BUDGET_BYTES: usize = DESKTOP_LOOP_POOL_FLOOR_BYTES;

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
/// One per frame means a full desktop set of 14 is dispatched over 14 frames —
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
/// | target  | panes | loop pool | store floor | offscreens | total    | ceiling  | reachable |
/// |---------|------:|----------:|------------:|-----------:|---------:|---------:|----------:|
/// | desktop |     6 |  3072 MiB |     576 MiB |    120 MiB | 3768 MiB | 3840 MiB |  3768 MiB |
/// | mobile  |     4 |   640 MiB |     288 MiB |     20 MiB |  948 MiB | 1024 MiB |   884 MiB |
/// | wasm32  |     6 |   192 MiB |      56 MiB |     30 MiB |  278 MiB |  288 MiB |   142 MiB |
///
/// # This table is what bounded the span budget
///
/// The `store floor` column is [`LOOP_POOL_FLOOR_BYTES`], and that floor is
/// [`LOOP_SPAN_BUDGET_SECS`] priced at the fastest radar measured — so the
/// slack between `total` and `ceiling` *is* the room the span budget had. It
/// leaves the floor 66 MiB on wasm32, 364 MiB on mobile and 648 MiB on desktop,
/// which at 4 / 16 / 16 MiB a frame is 16 / 22 / 40 frames, which is 47 min /
/// 1 h 15 m / 2 h 12 m of the worst measured radar. The next round window up on
/// each arm — 1 h, 1.25 h, 2.5 h — needs 18 / 23 / 44 and does not fit. **No
/// ceiling here moved to make room; the windows were chosen to fit the ceilings
/// that were already argued for**, which is the only order that keeps this
/// constant the independent bound its own doc says it is.
///
/// The last column is what the *device classification* actually admits, and it
/// is the number a memory audit should care about: a phone GPU is
/// `IntegratedGpu` and gets one doubling from the floor, and a browser is
/// `Unknown` and gets the floor itself. See [`LOOP_POOL_CEILING_BYTES`].
///
/// # The `store floor` column, and the two ceilings it moved
///
/// `App::setup_egui_frame` bounds the volume store at
/// `loop_allocation().volume_reserve_bytes().max(VOLUME_LOOP_TEXTURE_BUDGET_BYTES)`.
/// That `max` is **outside** the pool, not inside it: with `v` volume sets out
/// of `shares` loops the raster kinds hold at most `pool − v·share` while the
/// store is allowed `max(v·share, floor)`, so the sum is
/// `pool + max(0, floor − v·share)` and it is widest at `v = 0` — a screen with
/// no 3D *loop* at all, where every byte of the pool goes to raster frames and
/// the store is still floored so ordinary live 3D panes have somewhere to live.
///
/// This table had no such column and the sum was short by a whole pool floor.
/// Desktop absorbed it (3192 → 3704 of 3840, unmoved). The other two did not:
/// **mobile 768 → 1024 MiB** and **wasm32 256 → 288 MiB**. Mobile's was not
/// merely a permitted overrun either — an `IntegratedGpu` phone resolves a
/// 512 MiB pool, and 512 + 256 + 20 is 788 MiB, past the old ceiling with the
/// pool nowhere near its own.
///
/// Neither raise loosens anything: both stay inside the 1.25× snugness
/// `the_app_ceiling_is_not_slack_enough_to_hide_a_doubling` holds them to
/// (1024 / 948 = 1.08, 288 / 278 = 1.04), and the reachable column — the figure
/// a memory audit cares about — is unchanged in *kind*, having simply gained
/// the same term the total did.
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
/// is the double-count the old sum carried. (It reads 3704 again above, and the
/// coincidence is worth naming rather than tripping over: the 512 MiB is back
/// as the store-floor term, which is a real second bound rather than the
/// double-count of the first.)
///
/// **The mobile ceiling came down from 1408 MiB to 768 MiB and the wasm32 one
/// from 384 to 256 MiB.** Both followed their pool ceilings, which came down
/// against measured platform limits rather than against arithmetic — a 4 GB
/// iPhone's ~2098 MB jetsam hard limit and iOS Safari's ~300–500 MB WebGL heap
/// band. [`LOOP_POOL_CEILING_BYTES`] carries the evidence and the sources.
/// Desktop's was unmoved, because a discrete GPU has the memory and the old
/// figure was never the problem. Both then went back up — 1024 and 288 — when
/// the store-floor term above was found missing from the sum, which is a
/// correction to the *arithmetic* and leaves those platform readings exactly
/// where they were: the pool ceilings they argue for did not move.
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
pub const WASM_APP_TEXTURE_BUDGET_BYTES: usize = 288 * 1024 * 1024;
/// The mobile arm. See [`APP_TEXTURE_BUDGET_BYTES`].
pub const MOBILE_APP_TEXTURE_BUDGET_BYTES: usize = 1024 * 1024 * 1024;
/// The desktop arm. See [`APP_TEXTURE_BUDGET_BYTES`].
pub const DESKTOP_APP_TEXTURE_BUDGET_BYTES: usize = 3840 * 1024 * 1024;

/// What the desktop arm becomes for a machine that earned
/// [`crate::budget::Promotion::Ceiling`].
///
/// The **second rung** of `budget::BudgetLimits::app_texture_ceiling_bytes`,
/// and it exists for one reason: [`DESKTOP_VOLUME_OFFSCREEN_CEILING_BYTES`]
/// moves a term of the sum, so the bound over the sum has to move with it or
/// the promotion is unprovable. Both rungs are argued in bytes and neither is
/// read off the device, which is what keeps
/// `the_app_ceiling_is_not_slack_enough_to_hide_a_doubling` biting rather than
/// degenerating into two sides that move together.
///
/// The arithmetic at the promoted rung: 3072 MiB of loop pool ceiling + 576 MiB
/// of volume-store floor + 6 panes x 48 MiB of offscreen = **3936 MiB**,
/// against this 4032 MiB. That is 1.02x, the same snugness the unpromoted
/// 3768-against-3840 keeps.
///
/// **Not 4096 MiB**, and the reason is not aesthetic: this is a `usize`,
/// wasm32's is 32 bits, and `budget::BudgetLimits::DESKTOP` is a `const`
/// compiled on every target including that one. 4096 MiB is exactly
/// `u32::MAX + 1`.
pub const DESKTOP_APP_TEXTURE_CEILING_BYTES: usize = 4032 * 1024 * 1024;

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
///
/// Named outside the cascade for the reason [`MAX_CONCURRENT_LOOP_DOWNLOADS`]
/// gives.
#[cfg(mobile)]
pub const MAX_RENDER_CACHE_ENTRIES: usize = MOBILE_MAX_RENDER_CACHE_ENTRIES;
/// See the `mobile` arm above.
#[cfg(not(mobile))]
pub const MAX_RENDER_CACHE_ENTRIES: usize = NON_MOBILE_MAX_RENDER_CACHE_ENTRIES;

/// The `mobile` arm of [`MAX_RENDER_CACHE_ENTRIES`].
pub const MOBILE_MAX_RENDER_CACHE_ENTRIES: usize = 4;
/// Every other target's arm. See [`MAX_RENDER_CACHE_ENTRIES`].
pub const NON_MOBILE_MAX_RENDER_CACHE_ENTRIES: usize = 8;

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
    volume_grid_shape_of(VOLUME_GRID_CELLS, max_axis)
}

/// [`volume_grid_shape`], for a cell budget that is not this target's own.
///
/// The same selection with the budget as an argument, so `crate::budget` can
/// ask it for any resolved [`Budgets::grid_cells`](crate::budget::Budgets)
/// rather than only for the arm this build compiled — which is the whole reason
/// the arms have names.
pub const fn volume_grid_shape_of(
    cells: [u32; 3],
    max_axis: u32,
) -> rustdar_radar::voxel::VoxelShape {
    rustdar_radar::voxel::shape_for_budget(shape_of(cells), max_axis as usize)
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
/// Not a runtime check — the only things that read it are the `const _: () =
/// assert!` beside `crate::volume::raymarch::GRID_MIP_LEVELS` and the tests
/// below. It is the budget [`VOLUME_GRID_CELLS`] was chosen to fit, written
/// down so that growing an axis has to be a deliberate decision about memory.
/// `the_volume_grid_fits_the_target_texture_budget` enforces it and
/// `the_volume_budget_is_not_slack_enough_to_hide_a_doubling` keeps it snug.
///
/// **Not "like [`LOOP_POOL_FLOOR_BYTES`]", which this used to say.** That one
/// *is* measured against, twice: `LoopPoolLimits::for_target` makes it the
/// floor a discovered pool is clamped to, and `App::setup_egui_frame` floors
/// the volume store's eviction bound at it through
/// [`VOLUME_LOOP_TEXTURE_BUDGET_BYTES`].
/// The self-claim here was right and the comparison was not, which is the
/// worse of the two ways to be half stale — it invites the next reader to
/// treat an enforced bound as prose.
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
/// premultiplied plane. For scale, the same target budgets 56 MiB for loop
/// textures, so this is a 4% move against the largest thing on the page and
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
/// Unlike [`VOLUME_TEXTURE_BUDGET_BYTES`] — and *like* [`LOOP_POOL_FLOOR_BYTES`],
/// which is measured against at `LoopPoolLimits::for_target` and again where
/// the volume store's eviction bound is floored —
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

/// What the desktop arm becomes on an adapter that named itself discrete or
/// reported desktop-class texture ceilings — `crate::budget::Promotion::Ceiling`.
///
/// **The compromise a 24 GiB card was still eating.** The table above stops at
/// the 2560 x 1440 reference pane; the paragraph under it states the
/// consequence and then accepts it — *"a maximised pane on a 4K display is 31.6
/// MiB at `Native`, so it steps to `Half` and is upscaled by the blit's
/// `Linear` sampler"*. On an RTX 3090 with 24576 MiB of measured VRAM, a 4K
/// pane being upscaled from half resolution because of a 20 MiB budget is the
/// user's complaint in one line.
///
/// # The number
///
/// 3840 x 2160 x 4 B = **31.64 MiB** for the pane, plus the headroom the
/// shipped figure keeps: 14.06 -> 20 MiB is 1.42x, and 31.64 x 1.42 = 44.9,
/// rounded up to a whole 48 MiB. That is 1.52x — enough for the alignment a
/// real allocation carries, not enough to hide a doubling.
///
/// # The fill rate, measured rather than assumed
///
/// `volume::quality`'s own table is 0.766 ms for the cloud rung at 1440 x 900
/// over a dense real volume on this exact class of device, and the cost model
/// behind it is fetch-bound and linear in covered pixels. 4K is 6.4x that area,
/// so about **4.9 ms** of a 16.7 ms frame for one pane — which is what a
/// maximised pane is, and which is the same ~4 ms the paragraph above already
/// worked out. The interaction rule is not at risk: the raymarch is offscreen
/// precisely so a frame can drop quality without the map dropping anything.
///
/// # Which machines do *not* get it
///
/// The middle rung stays at the unpromoted figure, so an integrated GPU keeps
/// 20 MiB. Same model, same table: integrated extrapolates to 12-23 ms at
/// *1440 x 900*, so it is the class the measurement argues against promoting
/// rather than the class nobody got round to.
pub const DESKTOP_VOLUME_OFFSCREEN_CEILING_BYTES: usize = 48 * 1024 * 1024;

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
/// rather than merely stated — unlike [`VOLUME_TEXTURE_BUDGET_BYTES`], and like
/// [`VOLUME_OFFSCREEN_BUDGET_BYTES`] and [`LOOP_POOL_FLOOR_BYTES`].
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
/// On a phone that means `MAX_CONCURRENT_RENDERS` 6 instead of 3 and a 576 MiB
/// texture budget instead of 288 MiB, which is an OOM, not a warning.
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
    // `frames_for_span` divides by the cadence and adds one, so a zero span
    // would resolve every loop to the minimum on every radar — a loop budget
    // that is not a budget. The upper bound is what stops a cascade edit from
    // making the span the binding term instead of the memory: the render budget
    // is what the span costs at the fastest radar, so a span this arm's frames
    // cannot pay for is a promise the loop silently breaks.
    assert!(LOOP_SPAN_BUDGET_SECS > 0);
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
    // The plan-view side is **not** required to be a power of two, and the
    // three sizes here being powers of two is a fact about them rather than a
    // rule they obey. It was claimed to be a rule, twice — here and in
    // `rustdar_radar::types::tests` — and the claim was checked and is false:
    // `MercatorProjection` derives every field by `side_px as f64` division,
    // the gate loop indexes `py * side_px + px`, and `queue.write_texture`
    // repacks rows itself, so `COPY_BYTES_PER_ROW_ALIGNMENT` never applies. A
    // 7362 px raster — a real surveillance cut on a device that reports 32768 —
    // goes through all of it unchanged, and 7362 is not even a multiple of four.
    // The section's halved height was the other half of the claim, and it does
    // not depend on this at all: `SECTION_HEIGHT` comes from `SECTION_WIDTH`,
    // its own constant, which no plan-view side reaches.
    //
    // What every path does still share is a floor and a ceiling, which is what
    // `raster_side_from_rgba_len` checks a finished buffer against.
    assert!(rustdar_radar::types::IMAGE_SIZE > 0);
    assert!(LONG_RANGE_IMAGE_SIZE > 0);
    assert!(LOOP_IMAGE_SIZE > 0);
    // The ceiling has to be at least what this build already renders, or
    // `Budgets::raster_side_for_adapter`'s bracket would be inverted — and
    // `raster_side_from_rgba_len` would refuse the rasters this class produces.
    assert!(WASM_RASTER_SIDE_CEILING >= WASM_LONG_RANGE_IMAGE_SIZE);
    assert!(MOBILE_RASTER_SIDE_CEILING >= MOBILE_LONG_RANGE_IMAGE_SIZE);
    assert!(DESKTOP_RASTER_SIDE_CEILING >= DESKTOP_LONG_RANGE_IMAGE_SIZE);
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
