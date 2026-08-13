// rayon on every target that has threads, the sequential stand-ins on wasm32.
// The whole target split lives in `par`, so the four rasterization loops below
// need no `cfg` of their own.
use crate::par::*;

use crate::l3_values::{build_eet_lut, build_vil_lut, decode_legacy_thresholds, l3_physical_value};
use crate::palette::get_color_for_value;
use crate::types;
use nexrad_model::data::{DataMoment, Radial, Scan};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};

// ── Shared rendering infrastructure ──────────────────────────────────────────

/// Pre-computed Web Mercator projection constants, derived from
/// [`types::ImageBounds`] so the pixel grid aligns with the bounds the UI gets.
struct MercatorProjection {
    radar_lat_rad: f64,
    cos_radar_lat: f64,
    center_px: f64,
    merc_y_top: f64,
    merc_y_scale: f64,
    /// The image's scale, pixels per kilometre east-west at the site.
    ///
    /// A field rather than a constant because both of the quantities behind it
    /// are per render now. Where the caller's ceiling is above the base size it
    /// stays close to 4.45 px/km on purpose — 2048 over the 230 km floor, 4096
    /// over a 460.11 km surveillance cut (4.45) or a TDWR's 417 km long-range
    /// reflectivity (4.91). Where the ceiling *is* the base size it is the
    /// extent alone that moves it, so the same sweeps read 2.23 and 2.46, and a
    /// Doppler cut 3.41; [`types::raster_side_px`] is where that is argued.
    px_per_km: f64,
    /// The half-width this raster covers, km — [`types::plan_view_extent_km`]
    /// of the sweep's own reach.
    ///
    /// Carried on the projection rather than passed alongside it because the
    /// gate loops need exactly two things from the geometry, where a gate
    /// lands and whether it is on the image at all, and splitting those across
    /// two arguments is how a loop comes to clip against one extent while
    /// painting at another.
    extent_km: f64,
    /// The image's side, pixels — [`types::raster_side_px`]'s answer for this
    /// extent and this caller's ceiling.
    ///
    /// Here beside the scale for the same reason the extent is: the gate loop's
    /// bounds check and its row stride are both this number, and a projection
    /// scaled for one side while indexed at another writes the picture on a
    /// diagonal.
    side_px: usize,
}

impl MercatorProjection {
    fn from_bounds(
        radar_lat: f64,
        bounds: &types::ImageBounds,
        extent_km: f64,
        side_px: usize,
    ) -> Self {
        let radar_lat_rad = radar_lat.to_radians();
        Self {
            radar_lat_rad,
            cos_radar_lat: radar_lat_rad.cos(),
            center_px: side_px as f64 / 2.0,
            merc_y_top: bounds.mercator_y_max,
            merc_y_scale: side_px as f64 / (bounds.mercator_y_max - bounds.mercator_y_min),
            px_per_km: side_px as f64 / (2.0 * extent_km),
            extent_km,
            side_px,
        }
    }

    fn render_gate(
        &self,
        bufs: &RenderBuffers,
        ctx: &RadialContext,
        range_km: f64,
        gate_interval: f64,
        value: f32,
        from: GateId,
    ) {
        let range_start = range_km - gate_interval / 2.0;
        let range_end = range_km + gate_interval / 2.0;

        let num_range_samples = ((range_end - range_start) * self.px_per_km).ceil() as i32 + 2;
        let num_az_samples = ((ctx.az_half_spacing * 2.0 * range_km * PI / 180.0) * self.px_per_km)
            .ceil() as i32
            + 2;
        let inv_num_range = 1.0 / num_range_samples.max(1) as f64;
        let inv_num_az = 1.0 / num_az_samples.max(1) as f64;

        let cell = RenderBuffers::cell(write_key(from), value);

        for r_step in 0..num_range_samples {
            let r = range_start + (range_end - range_start) * (r_step as f64 * inv_num_range);
            let dy_center = r * ctx.cos_az_center;
            let dest_lat_rad = self.radar_lat_rad + dy_center / types::EARTH_RADIUS_KM;
            let cos_correction = self.cos_radar_lat / dest_lat_rad.cos();

            for az_step in 0..num_az_samples {
                let t = az_step as f64 * inv_num_az;
                let sin_az = ctx.sin_az_start + ctx.sin_az_delta * t;
                let cos_az = ctx.cos_az_start + ctx.cos_az_delta * t;

                let dx_km = r * sin_az;
                let dy_km = r * cos_az;
                let px_i = (self.center_px + dx_km * cos_correction * self.px_per_km) as i32;
                let dest_lat_rad = self.radar_lat_rad + dy_km / types::EARTH_RADIUS_KM;
                let dest_merc_y = types::lat_rad_to_mercator_y(dest_lat_rad);
                let py_i = ((self.merc_y_top - dest_merc_y) * self.merc_y_scale) as i32;

                if px_i >= 0
                    && px_i < self.side_px as i32
                    && py_i >= 0
                    && py_i < self.side_px as i32
                {
                    let pixel_idx = py_i as usize * self.side_px + px_i as usize;
                    bufs.claim(pixel_idx, cell);
                }
            }
        }
    }
}

/// Pre-computed azimuth sin/cos values for a single radial strip.
struct RadialContext {
    cos_az_center: f64,
    sin_az_start: f64,
    cos_az_start: f64,
    sin_az_delta: f64,
    cos_az_delta: f64,
    az_half_spacing: f64,
}

impl RadialContext {
    /// A half-width below zero is not a narrow wedge, it is an inside-out one:
    /// `render_gate` derives its azimuth sample count from this number, so a
    /// negative value makes `0..count` empty and the radial silently paints
    /// nothing — a whole sweep can disappear without a line in the log. The
    /// Level II callers below now compute a width that cannot go negative, but
    /// the Level III path hands over a packet's declared `angle_delta`
    /// unexamined, and nothing between the wire and here checks it. Clamped
    /// rather than asserted in release because a zero-width radial draws a
    /// spoke, which is a visible, self-explaining failure; the `debug_assert`
    /// catches a NaN in the tests, where a NaN half-width would otherwise turn
    /// into a zero here and look like a mere spoke.
    fn new(azimuth_deg: f64, az_half_spacing_deg: f64) -> Self {
        debug_assert!(
            az_half_spacing_deg.is_finite(),
            "radial at {azimuth_deg}° was handed a non-finite half-width \
             ({az_half_spacing_deg})"
        );
        let az_half_spacing_deg = az_half_spacing_deg.max(0.0);
        let az_start_rad = (azimuth_deg - az_half_spacing_deg) * PI / 180.0;
        let az_end_rad = (azimuth_deg + az_half_spacing_deg) * PI / 180.0;
        let cos_az_center = (azimuth_deg * PI / 180.0).cos();
        let (sin_az_start, cos_az_start) = az_start_rad.sin_cos();
        let (sin_az_end, cos_az_end) = az_end_rad.sin_cos();
        Self {
            cos_az_center,
            sin_az_start,
            cos_az_start,
            sin_az_delta: sin_az_end - sin_az_start,
            cos_az_delta: cos_az_end - cos_az_start,
            az_half_spacing: az_half_spacing_deg,
        }
    }
}

/// One atomic cell per output pixel: `(write_key << 32) | value_bits`.
///
/// `render_gate` runs under a `par_iter` over radials, and two radials
/// routinely claim the same pixel — but *not* because their footprints overlap
/// in continuous space. They tile: `t` runs over `[0, 1)`, so a gate samples a
/// strict subset of `[range_start, range_end)`, and the `+2` on the sample
/// counts raises sample *density*, never extent. They collide because those
/// footprints are quantized onto a pixel grid nothing aligns them to — inside
/// ~26 km a 0.5° radial's arc is narrower than one pixel, and at any range the
/// truncating cast drops neighbouring wedges into the same cell. The L2 path
/// adds a second source: [`l2_wedge_width_deg`] paints each radial at the width
/// it *declares*, and a real antenna does not land its radials exactly that far
/// apart, so wherever the sweep ran a few hundredths of a degree tight the
/// wedges overlap for real. A fixture whose wedges tile exactly still contends
/// over 271 pixels; see `overlapping_radials_contend_for_pixels`.
///
/// Neither claimant is more correct — the rasterizer never computes subpixel
/// coverage — so the tie is arbitrary, and the only question is whether it gets
/// resolved *stably*.
///
/// It used to be resolved by the race. Two relaxed stores per sample, one to an
/// image buffer and one to a value buffer, last writer wins. That cost two
/// things:
///
///   * The render was not reproducible. Over 12 runs of a 720 × 1200 L3 sweep
///     at a 2048 side on 32 threads: 12 distinct hashes, ~16 k of 3.3 M
///     painted pixels differing per pair, 53 k in the union. Invisible, in
///     fairness — 91% of those differed by ≤ 0.5 dBZ (one data level), none by
///     more than 5 dBZ, and no pixel flipped between opaque and transparent.
///   * The image and value stores were a *pair*, and nothing kept them
///     together. One radial could win the colour while another won the value,
///     leaving a pixel no radial ever wrote: measured, rare, real — 3 such
///     pixels over 12 runs of that sweep.
///
/// Now there is one cell, so there is no pair to tear, and it is claimed with
/// `fetch_max` rather than a store. `fetch_max` is a set operation: the result
/// is the greatest claim, whatever order the claims arrive in. With
/// [`write_key`] ranking claims radial-major, gate-minor, the greatest claim is
/// the one a single-threaded radial-major render would have written last — so
/// the parallel result *is* the sequential result, not merely a stable one.
/// Checked against the pre-change rasterizer compiled in alongside this one:
/// 0 differing bytes and 0 differing values over all 4,194,304 pixels, on both
/// a smooth field and an adversarial one. Note the suite cannot re-check that
/// on its own — `parallel_matches_single_thread` compares this code against
/// its own single-threaded self, which `fetch_max` makes true by construction.
///
/// ## What determinism costs
///
/// It is not free. `AtomicU64::fetch_max` has no x86-64 instruction behind it;
/// it lowers to a `lock cmpxchgq` retry loop, which needs the line exclusively
/// and cannot coalesce in the store buffer, so the cost climbs with thread
/// count. Three variants compiled into one binary and interleaved in one
/// process, 30 samples each, same 720 × 1200 sweep. **Medians** — the minimum
/// is actively misleading here, because `fetch_max` widens the distribution
/// instead of shifting it, and min-of-N reports the run that got lucky:
///
/// | 2048 px square              |  1 thr | 8 thr | 16 thr | 32 thr |
/// |-----------------------------|-------:|------:|-------:|-------:|
/// | 2 × `AtomicU32`, store      |  395.8 |  73.2 |   51.1 |   42.9 |
/// | 1 × `AtomicU32`, store      |  394.6 |  67.0 |   45.2 |   37.9 |
/// | 1 × `AtomicU64`, `fetch_max`|  413.4 |  72.3 |   52.9 |   52.1 |
///
/// At 32 threads that is +21% against the old layout, and the spread tells the
/// story better than the median: 41.7 / 42.9 / 44.0 (min/median/max) before,
/// 37.0 / 52.1 / 64.7 now. At a 1024 side single-threaded — the web arm's
/// operating point when this was measured, and its loop frames' still — it is a
/// wash: 201.7 / 195.5 / 200.3.
///
/// The middle row is why the cell was collapsed at all, and it is separable
/// from the keying: a single `AtomicU32` holding just the value bits ends the
/// tearing outright, with nothing left to tear against, and is the fastest of
/// the three everywhere. Determinism is what the third row buys and the third
/// row's price.
///
/// Colour is derived in `into_output` rather than stored per gate. That is
/// *more* palette work, not less — ~663 k gates reached the extent break
/// against 3.3 M painted pixels at 2048², measured when that break was a fixed
/// 230 km — but it is parallel and off the fill loop,
/// and it removes a whole store per sample from the loop that is actually hot.
/// Leaving that pass serial costs more than everything else here put together.
///
/// ## Earlier measurement, still standing
///
/// The atomics are *not* load-bearing on wasm32, so cfg-splitting that arm to a
/// plain buffer looks like a free win. It was measured per component, not
/// assumed, against a real KTLX 0.5° reflectivity sweep (720 radials × 1832
/// gates) at a 1024 side, release, rasterizer isolated from WebGL/winit.
/// It predates the collapse to one cell, so the store counts are the old
/// paired ones and it measured relaxed *stores*, not the RMW the fill loop now
/// runs. Nothing here re-measures that in a browser; what carries over is only
/// that atomics-vs-plain was ~1% of the frame when it was measured.
///
/// | what                                    | Firefox | Chromium |
/// |-----------------------------------------|--------:|---------:|
/// | whole render                            |  233 ms |   261 ms |
/// | 28 M relaxed `store` vs plain `Vec<u32>`| 39 / 37 |  47 / 48 |
/// | `into_output` shape, atomic vs plain    | 0.8/0.4 |  0.7/0.3 |
/// | `RenderBuffers::new`, atomic vs plain   | 0.2/0.3 |  0.3/0.2 |
///
/// ~2.5 ms of a 233 ms frame — about 1%, the same 1% in both browsers. Built and
/// measured end to end too, with the wasm arm on `Vec<Cell<u32>>`: Firefox
/// 233 → 230 ms, Chromium 261 → 262 ms, byte-identical image. A 1% return does
/// not pay for two divergent buffer types under one hot loop.
///
/// Those same numbers dispose of the theory that Firefox's `radar-render`
/// penalty came from these atomics: Firefox rasterizes this sweep *faster* than
/// Chromium, and relaxed atomic stores cost it 5% over plain ones.
///
/// Most of the frame is the per-sample `(π/4 + lat/2).tan().ln()` in
/// `types::lat_rad_to_mercator_y`: 28 M of those cost 660 ms in Firefox and
/// 597 ms in Chromium against 29 ms and 37 ms for the same loop without them.
/// Reducing it means changing the arithmetic every output pixel depends on, so
/// it cannot be done bit-identically. Firefox's reported 5.7× `radar-render`
/// penalty was a measurement artifact — re-measured on a pinned sweep it is a
/// 159 ms *minimum* against Chromium's 174 ms, a matched-pair median ratio of
/// 0.88; see `rustdar-web`'s crate docs for the medians and the method.
struct RenderBuffers {
    /// Borrowed from [`POOLED_CELLS`] for the length of one render and handed
    /// back by [`Self::into_output`], because on native this single allocation
    /// is one glibc can never recycle. Exactly `side_px²` cells for the
    /// [`types::raster_side_px`] this render was given, fixed for the whole of
    /// it: [`Self::checkout`] resizes a carried buffer to that length on the way
    /// in and nothing resizes it again.
    cells: Vec<AtomicU64>,
    /// Only `into_output` needs it, but it has to be the product the gates were
    /// coloured against, so it is captured at construction rather than passed
    /// back in.
    product: types::RadarProduct,
}

/// The one cell buffer this process keeps between plan-view renders.
///
/// # What it is worth
///
/// `IMAGE_SIZE² × size_of::<AtomicU64>()` is **33,554,432 bytes** on native,
/// and that is not a size like any other. glibc raises its `mmap` threshold
/// adaptively as blocks are freed, but never past `DEFAULT_MMAP_THRESHOLD_MAX`,
/// which is 32 MiB — so a single request of 33,554,393 bytes or more is
/// `mmap`ed and `munmap`ed *every* time, however often it is made, and pays a
/// minor fault on first touch of every one of its fresh zero pages. Measured on
/// glibc 2.44: a request of 33,554,392 bytes recycles at **0** minor faults,
/// one of 33,554,432 takes **8,193**, and neither number moves with repetition.
///
/// Every plan-view rasterization funnels through [`render_with_projection`], so
/// building this buffer per call was that cost once per render, per product,
/// per pane, per loop frame. `strace` over six renders of one volume: six
/// `mmap`s and six `munmap`s of 33,558,528 bytes, against **one** `mmap` and no
/// `munmap` for the same six with the buffer carried.
///
/// # Measured
///
/// Minor faults charged to the render call itself, read from `/proc/self/stat`
/// either side of it. Interleaved A/B — the two binaries alternate, so neither
/// always follows the other — 7 launches each, 7 renders per product per
/// launch, over three volumes: KTLX 2019-07-15, KDMX 2022-03-05, KLWX
/// 2018-03-02. Release profile, so `opt-level = 3` and `lto = true`. Minimum of
/// each arm over 42 calls, with the maximum beside it; a process's first render
/// is excluded from the carried arm, because that is the one that buys the
/// pages.
///
/// | per render         |           fresh |        carried |
/// |--------------------|----------------:|---------------:|
/// | KTLX reflectivity  | 17,957 (38,309) | 4,472 ( 6,836) |
/// | KTLX velocity      | 18,284 (34,432) | 6,127 ( 6,513) |
/// | KDMX reflectivity  | 17,339 (34,414) | 7,255 ( 8,058) |
/// | KDMX velocity      | 16,015 (34,642) | 6,864 ( 7,951) |
/// | KLWX reflectivity  | 19,699 (34,463) | 6,665 ( 7,070) |
/// | KLWX velocity      | 17,589 (33,857) | 6,017 ( 6,579) |
/// | *control*: voxels  |      0 ( 1,417) |     0 ( 1,409) |
///
/// The control is a [`crate::voxel`] build of the same volume, which never
/// touches these cells and whose largest allocation is 8 MiB — under the cap,
/// so it recycles either way. It is the finding stated as an experiment: the
/// win is not "allocation is slow", it is *this one block* never being
/// reusable, and a build that allocates freely beside it does not move at all.
///
/// Fault counts are the evidence rather than wall-clock because this box runs
/// dozens of agents at once — 30-odd runnable at the time of measurement — and
/// a fault count is a property of the code, where every timing on such a box is
/// a property of the neighbours. Note also how much *steadier* the carried arm
/// is: spreads of 15,000 to 20,000 become spreads of a few hundred.
///
/// The drop is larger than this buffer's own 8,193 pages, and the surplus is
/// the second half of the same effect: while a 32 MiB block is `munmap`ed on
/// every render, the 16 MiB image and 16 MiB value grid allocated beside it
/// never get a settled heap to be recycled from either. Those two were left
/// costing 7,103–8,208 faults a render once this buffer stopped churning, which
/// is a different mechanism — 16 MiB is under the cliff, so they are an arena
/// problem and not an `mmap` one — and they are carried now too. See
/// [`POOLED_IMAGE`].
///
/// # Why one buffer, and not one per thread
///
/// A `thread_local` is the obvious shape and is the wrong one here. On native,
/// `rustdar-frontend`'s `offload` spawns a **fresh `std::thread` per job**, so a
/// thread-local would be allocated, faulted in and freed with the thread every
/// single render — the reuse rate would be exactly zero. Even against a
/// long-lived pool it would pin one 32 MiB buffer per worker thread that had
/// ever rasterized, which on a 32-thread box is a gigabyte of buffers to save
/// 32 MiB of allocation.
///
/// # Why it is not threaded through from the caller
///
/// `rustdar_frontend::volume_bridge::VolumeResources::widening` — the same
/// cliff, fixed the same way — is the caller's buffer, and that is the right
/// shape there because its caller is the frame thread, which spans every
/// upload. This renderer has no
/// such caller. Its callers are a thread that lives for one render, a browser
/// worker reached over a message port that cannot be handed a pointer, and
/// `offload::execute`, whose documented contract is that it is *pure* and the
/// one implementation shared by all three. Threading a `&mut Vec<AtomicU64>`
/// through would change ten public entry points, two crate boundaries and the
/// worker protocol to reach a buffer that only one of the three arms could
/// supply.
///
/// # Residency
///
/// **One buffer of the largest raster this process has rendered, held from its
/// first plan-view render onwards.** That is 32 MiB at the base side — the
/// `IMAGE_SIZE²` figure above, desktop, mobile and wasm32 alike — and 128 MiB
/// once a long-range cut has been rendered against a 4096 ceiling, which on a
/// desktop showing a surveillance sweep is the ordinary case and not a corner.
/// It is a *high-water* mark rather than a constant because
/// [`RenderBuffers::checkout`] resizes the one buffer instead of reallocating
/// it, so the figure does not come back down when a base-size raster follows a
/// long-range one; that is the price of the alternation costing nothing. Never
/// given back: a session that has rendered once is exactly the one about to
/// render again, and the whole point is that the pages are bought once. On
/// wasm32 the main thread and the rasterization worker are separate instances
/// with separate linear memories, so a build where both rasterize holds the
/// figure twice — at the base side both times, since the web's ceiling is 2048.
///
/// **What grows is idle residency, not peak.** This buffer was live for the
/// whole of every render before this change too; all that is different is that
/// it stays live between them. A process's high-water mark is unchanged, and
/// the honest cost is that a session sitting on a rendered pane now holds the
/// buffer it used to have handed back.
///
/// wasm32 has nothing to win here, and the cliff measured above is not the
/// reason: glibc's `mmap` threshold has no counterpart in a linear memory that
/// never shrinks, where dlmalloc recycles a freed block of this size as readily
/// as any other and there are no fresh zero pages to fault. It carries the
/// buffer regardless, because a `cfg`-gated behavioural split is a second
/// renderer that no row of this workspace's gate runs the tests of.
///
/// One buffer, not a free list, and that is a deliberate ceiling. Renders can
/// overlap — `MAX_CONCURRENT_RENDERS` permits six on desktop — and a second
/// render that finds the slot empty allocates its own and frees it just as it
/// does without a pool, plus one uncontended `Mutex` acquire around an
/// `Option::take`. The lock is never held across that allocation
/// ([`RenderBuffers::pool`] says exactly what it does cover), so the losers of
/// a burst still allocate in parallel rather than queueing. What is bought is
/// the sequential case, which is every case the win was measured in: a pane
/// refreshing, a product switching, a loop advancing a frame at a time. A free
/// list would instead make a six-wide burst's peak permanent — 192 MiB of base
/// rasters, four times that if they were long-range — to speed up renders that
/// were already holding those six buffers live at once.
static POOLED_CELLS: std::sync::Mutex<Option<Vec<AtomicU64>>> = std::sync::Mutex::new(None);

/// The one RGBA texture this process keeps between plan-view renders, and the
/// one value grid, in two slots that fill and empty independently.
///
/// # What they are worth
///
/// [`RenderBuffers::into_output`] built both of these afresh on every render.
/// At the base side that is `2048² × 4` twice — **16,777,216 bytes each**,
/// 4,096 pages each — and both are written in full before the render returns.
/// Neither is near [`POOLED_CELLS`]' cliff: 16 MiB is half of glibc's
/// `DEFAULT_MMAP_THRESHOLD_MAX`, so these blocks are servable from an arena and
/// the reason they were not being served from one is different. The parallel
/// LDM decode that lands first fans 50–130 records across the whole rayon pool
/// and leaves the process with more arenas than any one of them keeps a warm
/// 16 MiB chunk in; the render then runs on a thread `offload` spawns for it
/// alone, takes whichever arena it is given, and faults all 8,192 pages back in.
///
/// Measured on this box — 32 cores, `--release` (`lto = true`, `opt-level = 3`),
/// loadavg 4.7–17 — on a fresh thread per render in a process that had already
/// run the parallel decode, over four archived volumes at four sites (KFTG, KTLX,
/// KDMX, KLWX) and two products, nine renders each, interleaved arms with a
/// fresh process for every one and the arm order alternating by round. The
/// ranges are across those eight site-product pairs, worst to best; the first
/// two renders of a process are excluded from every arm, for the reason the last
/// paragraph of this section gives.
///
/// | per render                   |          before |     after |
/// |------------------------------|----------------:|----------:|
/// | minor faults, process-wide   |     7,103–8,208 |  **0–15** |
/// | minor faults, render thread  |     3,340–7,333 |   **0–0** |
/// | allocations ≥ 1 MiB          |               2 |     **0** |
/// | allocations, all sizes       |           32–38 | **30–36** |
/// | bytes allocated              |          33.6 M |**33–47 k**|
/// | best ms                      |     35.47–38.96 |**31.48–34.91**|
/// | median ms                    |     36.71–39.68 |**32.47–35.61**|
/// | *control*, best / median ms  |   40.34 / 41.88 | 40.34 / 41.81 |
///
/// The control is a fixed, allocation-free compute kernel over a slab already
/// faulted in, run on a fresh thread beside every render. It measures the box
/// rather than the change and it does not move — 40.16–40.34 ms best and
/// 41.81–41.88 ms median in every arm — which is what says the render's numbers
/// are the render's, on a box where 30-odd agents are runnable at any moment.
///
/// Both fault columns are the evidence rather than the wall clock, and they are
/// not the same measurement twice. `value_data` is built serially on the
/// render's own thread and faults there; `image` is written by `par_chunks_mut`
/// on rayon's workers and faults on *them*, which no per-thread counter on the
/// render's thread can see. That is why the per-thread figure is roughly half
/// the process-wide one before, and why both are quoted. 8,192 of those pages
/// are these two buffers, on a raster whose size the render states up front.
///
/// The decode does not move: 50.74 → 49.16 ms best on KFTG and 16.76 → 16.90 on
/// KTLX, noise in both directions and no systematic change, which is what a pool
/// that is not in the decode should look like.
///
/// # What a miss costs, which is nothing
///
/// A render that finds a slot empty must be no worse off than one in a build
/// with no pool, or the web and every first render pay for a win they do not
/// get. It is not automatic: fitting an empty `Vec` to the raster reaches the
/// right length by `reserve` plus a `memset`, where `vec![0u8; n]` is
/// `alloc_zeroed` and glibc answers a request this size with `mmap` pages that
/// are *already* zero. The first cut of this cost 70.0 → 75.8 ms on KTLX, a
/// straight loss. [`checkout_image`]'s miss arm is therefore the original
/// expression and not a fitted empty, and the third arm of the experiment above
/// — the pooled build with nothing ever handed back — lands on the before arm to
/// within noise: 35.30–38.73 ms best against 35.47–38.96, the same 7,104–8,205
/// faults and the same two allocations ≥ 1 MiB.
///
/// # Why the first two renders of a process are excluded
///
/// The first buys the pages, as it does for [`POOLED_CELLS`]. The second is less
/// obvious and is the reset's doing: the colouring pass writes only the pixels a
/// sweep paints, so the *first* render never touches the mmap'd zero pages under
/// its own unpainted sky, and the second render's `resize` — which memsets the
/// whole buffer — is where those pages are first touched. It costs 1,096–2,664
/// faults once per process, in proportion to how much of the raster the sweep
/// leaves empty, and from the third render on the figure is the 0–15 above.
///
/// # Why not `Drop`, which is how the cross-section's planes come back
///
/// `crate::xsect`'s `POOLED_PLANES` is returned by `CrossSection`'s [`Drop`] —
/// the obvious way to carry a buffer that leaves a function *inside* a value,
/// and the shape that module can take because its three planes are private
/// fields behind accessors, so a section really is their last owner. It does
/// **not** transfer to this renderer, for two independent reasons.
///
/// The first is that it does not compile. [`SweepRender`]'s buffers are `pub`,
/// and they are moved out — by
/// `rustdar_frontend::offload`'s `From<SweepRender> for RenderedFrame`, by two
/// integration tests in that crate and by two dozen destructurings in this
/// module's own tests. A type with a `Drop` impl cannot have a field moved out
/// of it (E0509), so `Drop` here is a compile error at every one of those
/// sites.
///
/// The second is that converting them all to accessors would not help, because
/// the buffers do not *stop* at [`SweepRender`] the way a section's planes stop
/// at `CrossSection`. They keep going: into `RenderedFrame`, then the texture
/// into an egui `ColorImage` and the grid into an `Arc` a pane holds for as
/// long as it shows that render. A `Drop` on `SweepRender` would fire on an
/// emptied husk and hand the pool nothing.
///
/// So the buffers are given back where they actually die, by the crate that
/// owns those moments, through [`recycle_image`] and [`recycle_values`]. That
/// is the same trade `POOLED_CELLS` refuses for *checkout* — threading a buffer
/// in would change ten entry points and the worker protocol — and it is a
/// different trade, because handing a dead buffer back is one call at one site
/// and needs no route for it to arrive by.
///
/// # Why two slots and not one pair
///
/// Because the two buffers die in different places and at different times, and
/// on the path that matters most one of them does not die at all.
///
/// A **loop frame** gives both back, and its two buffers die at two different
/// moments. The texture is finished with as soon as it has been reinterpreted
/// for the channel, which is `app_fetch`'s `deliver`. The grid is dead the
/// moment it is produced, because a loop frame stores no values — and *where*
/// that is honoured depends on which kind of loop frame it is. A Level II frame
/// is dispatched `JobRequest::Radar { values_wanted: false }` and
/// `offload::execute` empties it there, in the instance that rasterized it. A
/// Level III frame is dispatched `JobRequest::Level3`, which has **no such
/// field** — there is nothing to strip on a raster whose values the loop never
/// asked for differently — so its grid rides back to `deliver` intact and is
/// handed over there, beside the texture. One call at that site covers both:
/// what a Level II frame carries to it is the capacity-0 husk `execute`'s
/// `mem::take` left behind, which [`recycle_values`] declines. So the
/// highest-frequency render in the application — sixty of them per loop
/// download, of either kind — allocates neither buffer, and gets the whole of
/// the table above.
///
/// A **static pane** gives back only the texture. Its grid leaves in an `Arc`
/// that the pane, the render cache and every hover that samples the field all
/// hold, so the render's thread is not its last owner and there is no moment on
/// that path at which it belongs to nobody. That slot therefore misses, which
/// costs exactly what the "miss" section above says it costs: nothing.
///
/// A single slot holding both would be empty whenever either was still out,
/// which on the static path is always — so it would give the static pane
/// nothing, where two slots give it half.
///
/// Half is worth having and is also the *smaller* half, which is worth writing
/// down because it is the opposite of what the sizes suggest. The two buffers
/// are the same 16 MiB and remove the same 4,096 pages each, but they are not
/// paid for on the same thread: the grid is built serially on the render's own
/// thread, where a fault stalls the render, and the texture is written by 32
/// rayon workers, where the same 4,096 faults are spread across the pool and
/// barely reach the clock. Measured separately over KFTG and KTLX at loadavg
/// 12–17, best ms and process-wide faults:
///
/// |                       | KFTG best | KTLX best |          faults |
/// |-----------------------|----------:|----------:|----------------:|
/// | neither slot fed      |     43.92 |     41.92 |     7,106–8,213 |
/// | texture only          |     43.34 |     40.62 |     3,011–4,107 |
/// | grid only             |     39.79 |     36.54 |     3,008–4,096 |
/// | both                  | **39.39** | **35.62** |        **0–16** |
///
/// (Those absolutes are not comparable with the table further up — that run was
/// at a lower load, and its control sits at 40.34 ms against this one's 43.50.
/// The rows here are comparable with *each other*, which is what they are for.)
///
/// So the texture's slot buys half the faults and about a millisecond, and the
/// grid's buys the other half and four or five more. Giving the static pane its
/// grid back would need the `Arc` replaced by a handle that recycles when the
/// last holder drops it — across `rustdar_frontend::channels`,
/// `rustdar_egui::pane` and `rustdar_egui::overlay_cache` — and the numbers
/// above are here so that whoever weighs that has them without re-measuring.
///
/// # Residency
///
/// **One spare texture and one spare grid, each of the largest raster that has
/// been given back to *that slot*** — which is not the same raster for the two,
/// and the difference is most of the figure. Both are `side² × 4` bytes, so at
/// the base side they are 16 MiB each and the pair is **32 MiB**. Past that they
/// part company:
///
/// * The **texture** slot is fed by both consumers, and one of them —
///   `render_dispatch`'s `deliver`, the static pane — is the one render kind
///   that may take the long-range raster. So its high-water is the 4096 ceiling:
///   **64 MiB** natively, 16 MiB on the web, where the ceiling is 2048.
/// * The **grid** slot is fed only by loop frames, for the reason the section
///   above gives — a static pane's grid leaves in an `Arc` and never belongs to
///   nobody. Every loop frame is dispatched `full_res: false`, so this slot
///   never sees anything larger than `LOOP_IMAGE_SIZE² × 4`: **16 MiB** on
///   desktop and mobile, 4 MiB on the web, where a loop frame is 1024². A
///   long-range static render does *take* this slot's buffer and grow it to 64
///   MiB — but that grown grid leaves in the `Arc` and never comes back, and the
///   slot it left is empty.
///
/// So the real ceiling is **80 MiB** natively and 20 MiB on the web, not the
/// 128 MiB that doubling the long-range texture would suggest. High-water rather
/// than constant, for [`RenderBuffers::checkout`]'s reason and with its price:
/// the slot is fitted to the render asking for it rather than reallocated, so an
/// alternation between two sides re-faults nothing once the larger has been
/// seen.
///
/// This is a spare rather than an addition to the peak — a render that is alive
/// is holding its buffers, and the slots are empty while it is — so the ceiling
/// is "the renders alive at once, plus one of each".
///
/// wasm32 carries both regardless and has less to win, exactly as
/// [`POOLED_CELLS`] does: dlmalloc in a linear memory that never shrinks
/// recycles a 16 MiB block as readily as any other and there are no fresh zero
/// pages to fault. Which of the slots can even be fed there depends on where
/// the render ran, and the two answers differ. The grid's is returned by
/// `offload::execute` itself, which is the rasterizing instance whichever it
/// is, so that slot fills in a worker as readily as on the main thread. The
/// texture's is returned by the consumers — `render_dispatch`'s `deliver` and
/// `app_fetch`'s — which are always the main instance, so with a worker
/// attached the worker's texture slot never fills and every render there
/// allocates as it always did. Without one, the inline fallback rasterizes in
/// the main instance and both slots fill. None of that is a `cfg`: a
/// `cfg`-gated behavioural split would be a second renderer that no row of this
/// workspace's gate runs the tests of, and what the unfed case costs is
/// measured above at nothing.
static POOLED_IMAGE: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// See [`POOLED_IMAGE`], which documents both slots.
static POOLED_VALUES: std::sync::Mutex<Option<Vec<f32>>> = std::sync::Mutex::new(None);

/// The texture slot, with a poisoned lock read as a live one — see
/// [`RenderBuffers::pool`], whose account of what the lock covers holds here
/// word for word.
fn image_pool() -> std::sync::MutexGuard<'static, Option<Vec<u8>>> {
    POOLED_IMAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The value-grid slot. See [`image_pool`].
fn values_pool() -> std::sync::MutexGuard<'static, Option<Vec<f32>>> {
    POOLED_VALUES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A zeroed texture of exactly `len` bytes — the pool's if it has one.
///
/// # Why it is zeroed rather than trusted to be overwritten
///
/// Because it genuinely is not overwritten. [`RenderBuffers::into_output`]'s
/// colouring pass has **no `else` arm**: a pixel whose value is `NaN` and whose
/// bits are not [`RANGE_FOLDED_BITS`] is left exactly as the buffer delivered
/// it, and every render leaves most of the raster that way — the corners
/// outside the disc, the gaps between radials, the whole of a sweep that
/// paints nothing. `vec![0u8; n]` is what made those pixels transparent, and a
/// pooled buffer that skipped this would show the previous render's echoes
/// through the new one's empty sky. That is not the vacuous case the section's
/// planes were in, where the raster loop covered every pixel and the re-seed
/// was insurance; here it is the whole correctness of the change, and
/// `tests/render_output_pool.rs` fails without it.
///
/// `clear` then `resize` rather than `resize` then `fill`: it is total in both
/// directions in one step, so a buffer coming back from a *larger* raster is
/// truncated and one going out to a larger raster has its grown tail zeroed,
/// with no length left over from the render before to disagree with the one
/// this render is about to claim.
fn checkout_image(len: usize) -> Vec<u8> {
    // Bound to a `let`, and deliberately **not** written as the scrutinee of
    // the `match` below. A guard produced inside a scrutinee temporary lives to
    // the end of the whole `match`, so `match image_pool().take() { .. }` would
    // hold the pool lock across the fallback allocation and the zero-fill under
    // it — 16 MiB and its page faults, under a process-wide mutex, exactly
    // where concurrent renders that all miss the slot would queue on each other
    // instead of allocating in parallel as they do with no pool at all. Bound
    // to a `let`, the guard is dropped at this semicolon.
    //
    // **This one is caught by a lint and its two siblings are not**, which is
    // the difference worth knowing. Fusing these two statements makes
    // `clippy::significant_drop_in_scrutinee` fire on this line and suggest
    // exactly the `let` that is written here — because this is a `match`, and a
    // `match` scrutinee is the shape that lint reads. [`checkout_values`] just
    // below and `crate::xsect`'s `checkout` are the same hazard written as a
    // method chain (`pool().take().unwrap_or_else(..)`), and neither that lint
    // nor `significant_drop_tightening` says anything about either. Verified by
    // fusing all three and re-running with both lints enabled: one warning, on
    // this line, and silence for the other two.
    //
    // Available rather than automatic: both lints are nursery and this
    // workspace's gate is `clippy --all-targets --all-features -- -D warnings`,
    // which does not turn them on. So an editor who fuses this and asks
    // `-W clippy::significant_drop_in_scrutinee` will be told, and one who does
    // not asks nothing of the two chain-shaped siblings either way — for those,
    // this comment and `xsect::checkout`'s are the whole of the guarantee.
    let taken = image_pool().take();
    match taken {
        Some(mut image) => {
            image.clear();
            image.resize(len, 0u8);
            image
        }
        // `vec!` and not `Vec::new()` fitted by the arm above, which would reach
        // the same length by `reserve` plus a `memset`. This is `alloc_zeroed`,
        // and on a request this size glibc answers it with fresh `mmap` pages
        // that are *already* zero — so the fill is free, and a render that
        // misses the slot pays exactly what every render paid before this pool
        // existed and not a byte more. Measured: fitting an empty `Vec` here
        // instead cost 70.0 → 75.8 ms on KTLX, which is the whole of what a
        // miss would otherwise have been charged for the pool's existence.
        None => vec![0u8; len],
    }
}

/// An empty value grid with the pool's capacity if it has one.
///
/// Returned empty rather than sized, because its one caller fills it by
/// `extend` from an iterator over the cells — which writes every element it
/// produces, so unlike the texture there is no seeded state for a stale tail to
/// hide in. The `clear` is still what makes that true: it is what stops a
/// longer grid from a previous render surviving past the end of this one's.
fn checkout_values() -> Vec<f32> {
    // See [`checkout_image`] for why this is a `let` and not a receiver — and
    // for why no lint would tell you if it stopped being one here, this being
    // the chain-shaped half of that asymmetry.
    let taken = values_pool().take();
    let mut values = taken.unwrap_or_default();
    values.clear();
    values
}

/// Offer a finished RGBA texture back for the next plan-view render to draw
/// into.
///
/// Call it where the texture stops being needed — after it has been copied into
/// whatever the display layer holds — and not before. What arrives is *dead*:
/// this takes ownership, and the next render will overwrite every byte.
///
/// Declining is normal and costs the caller nothing. The buffer is dropped
/// where it stands if the slot is already full, which is what makes this one
/// slot rather than a free list, and dropped if it has no capacity to lend,
/// because a `Vec::new()` in the slot would occupy it while forcing the next
/// render to allocate anyway.
///
/// Both of those refusals — and that a buffer this *does* keep is the one the
/// next render draws into, rather than a `drop` with extra steps — are pinned by
/// `tests/render_output_slot.rs`, which is its own process because the claim is
/// about the slot itself and not about any render's output.
///
/// `POOLED_IMAGE` — private, and above — is where the slot and its measurements
/// are documented. Named in text rather than linked because a `pub` item's docs
/// cannot link to a private one without a rustdoc warning.
pub fn recycle_image(image: Vec<u8>) {
    if image.capacity() == 0 {
        return;
    }
    let mut pool = image_pool();
    if pool.is_none() {
        *pool = Some(image);
    }
}

/// Offer a finished value grid back. See [`recycle_image`], which this mirrors
/// exactly.
pub fn recycle_values(values: Vec<f32>) {
    if values.capacity() == 0 {
        return;
    }
    let mut pool = values_pool();
    if pool.is_none() {
        *pool = Some(values);
    }
}

impl RenderBuffers {
    /// `side_px` is the only statement of the buffer's shape, and
    /// [`RenderBuffers::into_output`] reads the lengths back off `cells` rather
    /// than being told them again — so nothing downstream can be handed a
    /// picture whose dimensions disagree with its bytes.
    fn new(product: types::RadarProduct, side_px: usize) -> Self {
        Self {
            cells: Self::checkout(side_px * side_px),
            product,
        }
    }

    /// Take the pooled buffer resized to `n` cells, or build one if this is the
    /// first render or a second render is already holding it. See
    /// [`POOLED_CELLS`].
    ///
    /// The pool's invariant is that every cell it holds is [`Self::EMPTY`].
    /// [`Self::into_output`] is the only path that puts a buffer back and it
    /// establishes that, so this hands out something indistinguishable from a
    /// fresh allocation. `tests/render_cell_pool.rs` is what pins it.
    ///
    /// # Why the length is made to match rather than asserted
    ///
    /// The pool holds one *block*, not one shape. A raster's side is
    /// [`types::raster_side_px`]'s answer and it genuinely varies — 2048 at the
    /// floor, the caller's ceiling past it (4096 on a device that can take it),
    /// 1024 for a browser loop frame — so a buffer coming back out of the slot
    /// is only sometimes the length the render asking for it needs, and a
    /// `debug_assert` on that length would be a claim that is simply false.
    /// Nor can the mismatch be answered by declining the buffer: the long-range
    /// raster is the *most* expensive one to allocate and reflectivity's
    /// surveillance cut reaches past the floor routinely, so the shape that
    /// would go unpooled is the one with the most to win.
    ///
    /// So this grows and shrinks the one buffer instead — the same grow-only
    /// arm as the caller's buffer in
    /// `rustdar_frontend::volume_raymarch::coverage_premultiplied_into`, which
    /// has always varied its grid shape this way. `resize_with` truncating is a
    /// length store that keeps the capacity, and `resize_with` extending fills
    /// from that capacity when it is already there, so a workload alternating
    /// between two sides re-faults nothing once the larger of them has been
    /// rendered once: what alternates is the length, not the allocation. It
    /// also means the length cannot disagree with what the render asked for,
    /// because it is *made* equal here rather than checked — a `debug_assert`
    /// would leave a release build to index off the end of a short buffer.
    ///
    /// The cost is that residency follows the high-water mark rather than a
    /// constant; [`POOLED_CELLS`] states the figure.
    fn checkout(n: usize) -> Vec<AtomicU64> {
        // Bound to a `let`, and deliberately **not** written as the `match`
        // scrutinee. A guard produced in a scrutinee lives to the end of the
        // match, so `match Self::pool().take()` would hold the pool lock across
        // the fallback allocation below — 32 MiB and its page faults, under a
        // process-wide mutex, exactly where concurrent renders that all miss
        // the slot would then queue on each other instead of allocating in
        // parallel as they do without a pool at all. `clippy::
        // significant_drop_in_scrutinee` is the lint for that shape and it is
        // nursery, so this gate cannot catch it; the statement below is the
        // only thing keeping the lock off the allocation.
        let pooled = Self::pool().take();
        match pooled {
            Some(mut cells) => {
                cells.resize_with(n, || AtomicU64::new(Self::EMPTY));
                cells
            }
            None => (0..n).map(|_| AtomicU64::new(Self::EMPTY)).collect(),
        }
    }

    /// Offer a drained buffer back to the pool, keeping it only if the slot is
    /// free. See [`POOLED_CELLS`] for why the slot is one and not many.
    fn recycle(cells: Vec<AtomicU64>) {
        let mut pool = Self::pool();
        if pool.is_none() {
            *pool = Some(cells);
        }
    }

    /// The pool, with a poisoned lock read as a live one.
    ///
    /// **What the lock covers is one `Option::take` in [`Self::checkout`] and
    /// one `is_none` plus a move-assign in [`Self::recycle`], and nothing
    /// else.** In particular the 32 MiB fallback allocation, the resize that
    /// fits a carried buffer to the raster asking for it, the whole
    /// rasterization and the drain are all outside it, and so is the drop of a
    /// buffer `recycle` declines to keep — the
    /// guard goes out of scope before the argument does. That is what makes it
    /// a lock renders never contend on for any measurable time; it is also why
    /// nothing under it can panic, which makes poisoning unreachable.
    ///
    /// Recovering rather than unwrapping keeps a panic that cannot happen out
    /// of the renderer anyway. The claim above is a property of two call sites
    /// rather than of a type, so it has to be re-read whenever either changes;
    /// [`Self::checkout`] says which line is load-bearing and why the lint that
    /// would otherwise catch a regression cannot run in this gate.
    fn pool() -> std::sync::MutexGuard<'static, Option<Vec<AtomicU64>>> {
        POOLED_CELLS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// No gate has claimed this pixel. Distinct from every real cell because
    /// [`write_key`] never yields 0.
    const EMPTY: u64 = 0;

    /// Pack a gate's claim. The key takes the high bits so `fetch_max` orders
    /// by it and not by the value riding along in the low ones.
    #[inline]
    fn cell(key: u32, value: f32) -> u64 {
        ((key as u64) << 32) | value.to_bits() as u64
    }

    /// Give `cell` the pixel if it outranks whatever holds it.
    #[inline]
    fn claim(&self, pixel_idx: usize, cell: u64) {
        self.cells[pixel_idx].fetch_max(cell, Ordering::Relaxed);
    }

    /// Pixels per colouring task. Big enough that rayon's per-task overhead
    /// vanishes against the palette lookups.
    const COLOR_CHUNK: usize = 16 * 1024;

    /// Split the cells into the RGBA texture and the value grid, give the
    /// drained buffer back to [`POOLED_CELLS`], and hand back the extent they
    /// were painted at so that whatever places the picture places it on the
    /// same ground the gates were projected onto.
    ///
    /// The texture and the grid are themselves taken from [`POOLED_IMAGE`] and
    /// [`POOLED_VALUES`] rather than allocated here, which is why the two `vec!`
    /// this used to open with are now [`checkout_image`] and
    /// [`checkout_values`]. Both are handed out indistinguishable from a fresh
    /// allocation and both come back somewhere else entirely — the crate that
    /// consumes them — for the reasons [`POOLED_IMAGE`] sets out.
    ///
    /// Colour is derived here rather than stored per sample: it is a pure
    /// function of the value at every call site, so keeping it in the cell
    /// would only give it a second chance to disagree. Deriving it is also
    /// less work — one lookup per pixel instead of one per gate — but only
    /// once the pass is parallel. Serial, it dominates the whole render.
    ///
    /// # The reset is this pass, not another one
    ///
    /// A carried buffer has to go back to the pool [`Self::EMPTY`] everywhere,
    /// or the next render inherits whatever pixels this one painted that it
    /// does not. That reset costs nothing here because this pass already reads
    /// every cell: it takes each one's value and leaves `EMPTY` behind it, in
    /// the same iteration, on cache lines it has just pulled in. There is no
    /// second sweep of 32 MiB to bulk-zero — not a loop, not a `memset`, not a
    /// `calloc` on the way in — which is strictly better than any of them, and
    /// it also means the buffer cannot be handed out un-reset by a path that
    /// forgot, because there is only the one path.
    ///
    /// That is a claim about the **cells**, and the texture below cannot make
    /// it: nothing reads a texel before writing it, because the colouring pass
    /// writes only the pixels a gate claimed. Its reset is therefore a real
    /// `memset` in [`checkout_image`], and paying for one is still cheaper than
    /// faulting the pages it writes over.
    ///
    /// `get_mut` rather than a `swap`: rasterization is over by the time this
    /// runs and taking `self` by value proves it, so the drain is a plain load
    /// and store per cell rather than 4 M locked read-modify-writes.
    fn into_output(self, extent_km: f64) -> SweepRender {
        let Self { mut cells, product } = self;
        let mut value_data = checkout_values();
        value_data.extend(cells.iter_mut().map(|a| {
            match std::mem::replace(a.get_mut(), Self::EMPTY) {
                Self::EMPTY => f32::NAN,
                cell => f32::from_bits(cell as u32),
            }
        }));
        Self::recycle(cells);
        let mut image = checkout_image(value_data.len() * 4);
        image
            .par_chunks_mut(4 * Self::COLOR_CHUNK)
            .zip(value_data.par_chunks_mut(Self::COLOR_CHUNK))
            .for_each(|(px, vals)| {
                for (px, v) in px.chunks_exact_mut(4).zip(vals) {
                    if !v.is_nan() {
                        let c = get_color_for_value(product, *v);
                        px.copy_from_slice(&[c.0, c.1, c.2, c.3]);
                    } else if v.to_bits() == RANGE_FOLDED_BITS {
                        // The one colour on this raster that no product scale
                        // produces, and the one the value grid cannot carry —
                        // so it is painted here and erased from the numbers in
                        // the same pass. See [`RANGE_FOLDED_BITS`].
                        let c = crate::palette::RANGE_FOLDED;
                        px.copy_from_slice(&[c.0, c.1, c.2, c.3]);
                        *v = f32::NAN;
                    }
                }
            });
        SweepRender {
            image,
            max_range_km: extent_km,
            values: value_data,
            // Set by the sweep paths that know one; see the field.
            nyquist_ms: None,
        }
    }
}

/// The bit pattern a range-folded gate claims its pixel with.
///
/// The RDA reports three things about a gate and Level II encodes all three:
/// a value, "below threshold", and **range folded** — the gate's echo came back
/// from beyond the waveform's unambiguous range, so the radar has a return and
/// cannot say how far away it was. `nexrad_model` keeps the three apart
/// (`MomentValue::RangeFolded`), the cross-section painter has always drawn the
/// distinction ([`crate::palette::RANGE_FOLDED`], `xsect::sample_color`), and
/// the plan view was the one renderer that dropped the last two on the floor
/// together. On TDWR that is a large share of a Doppler sweep — its
/// unambiguous range is around 90 km against a WSR-88D's ~150 — and painting
/// it as nothing says the radar saw nothing there, which is the opposite of
/// what it saw.
///
/// # Why a sentinel and not a second plane
///
/// The cell is one `AtomicU64` — key in the high half, `f32` bits in the low —
/// and that packing is what makes the parallel raster deterministic
/// ([`write_key`]). A status plane beside it would be a second cell to claim
/// and a second chance for the two to tear apart, for one bit. So the status
/// rides in the value: a **NaN payload**, unreachable from real data because
/// every gate loop in this module already drops NaN before it claims a pixel,
/// and distinct from the canonical NaN [`RenderBuffers::EMPTY`] decodes to.
/// Ordering is untouched — the key is the whole of the high 32 bits and no two
/// gates share one, so the value bits never break a tie.
///
/// **What it costs, stated:** the exported value grid canonicalizes this back
/// to a plain NaN, because that grid crosses into JavaScript, where a NaN
/// payload is not carried. So the plan view's hover reads "no data" over a
/// purple pixel. The pixel is the honest half and the readout is the
/// incomplete one; a cross-section through the same gate reports
/// `SampleStatus::RangeFolded` properly, and that stays the readout that can.
const RANGE_FOLDED_BITS: u32 = 0x7FC0_0F1D;

/// The value a range-folded gate carries through the fill loop.
const RANGE_FOLDED_SENTINEL: f32 = f32::from_bits(RANGE_FOLDED_BITS);

/// One finished plan-view raster.
///
/// A named struct because the third thing this used to be a tuple of was the
/// extent, the fourth is the fold limit, and a reader at a call site three
/// crates away has no way to tell a `f64` extent from a `f64` Nyquist velocity.
/// Every render path in this module answers with one of these, including the
/// Level III and volume products, so a consumer has one shape to handle rather
/// than one per pipeline.
pub struct SweepRender {
    /// RGBA, `side²` pixels; the side is derivable from the length and
    /// deliberately not restated (see `crate::types::raster_side_px`).
    pub image: Vec<u8>,
    /// The half-width the raster was **projected** at, km — where its corners
    /// go on the ground, not how far the data reached. See
    /// [`render_with_projection`].
    pub max_range_km: f64,
    /// Per-pixel values, `f32::NAN` where nothing was painted **and** where a
    /// range-folded gate was (see [`RANGE_FOLDED_BITS`]).
    pub values: Vec<f32>,
    /// Where the rendered sweep's cut declared its velocity folds, m/s.
    ///
    /// A property of the **sweep**, not of the product drawn from it, so every
    /// per-tilt Level II render reports it and the volume products (echo tops,
    /// the hail pair, the hybrid classification) and every Level III product
    /// report `None` — there is no one cut behind those to have declared
    /// anything. `None` also where the volume itself declared nothing, which is
    /// every Message 1 volume and every payload that reached the renderer
    /// without a table.
    pub nyquist_ms: Option<f64>,
}

impl SweepRender {
    /// Stamp the fold limit of the sweep this raster was drawn from.
    fn declaring(mut self, nyquist_ms: Option<f64>) -> Self {
        self.nyquist_ms = nyquist_ms;
        self
    }
}

/// Which gate a claim came from. Named fields rather than two `usize`
/// arguments: three call sites build one of these, and transposing them would
/// reorder the tie-break silently on whichever path got it wrong.
#[derive(Clone, Copy)]
struct GateId {
    radial: usize,
    gate: usize,
}

/// Rank a gate's write the way a single-threaded, radial-major render would:
/// radial index first, gate index within it second. `fetch_max` over these is
/// order-independent, so the parallel result is the sequential one.
///
/// Never 0, so [`RenderBuffers::EMPTY`] stays unambiguous. Saturates: past
/// 65535 radials or 65534 gates some writes rank equally, which stays
/// deterministic (`fetch_max` is a set operation) but stops matching the
/// sequential order. No NEXRAD product comes close — 720 radials and 1832
/// gates is the widest sweep.
#[inline]
fn write_key(from: GateId) -> u32 {
    let r = from.radial.min(0xFFFF) as u32;
    let g = from.gate.min(0xFFFE) as u32;
    (r << 16) | (g + 1)
}

// ── Sweep / azimuth helpers ──────────────────────────────────────────────────

/// How near a sweep's elevation has to sit to a requested one to count as it.
///
/// Read by [`find_sweep`], which explains why it is this narrow and why it can
/// only be this narrow now that sweeps are keyed on their median rather than
/// their first radial.
pub const ELEVATION_WINDOW: f64 = 0.1;

/// The available elevation angle (rounded to 0.1°) closest to
/// `target_elevation` that carries this product. The loop renderer uses it to
/// snap the selected elevation to what each historical scan actually holds.
///
/// On the sweep's median, for the reason [`find_sweep`] gives: a tilt named off
/// the first radial is not the tilt the sweep flew, and the loop would snap a
/// steady selection onto a different cut from one frame to the next as the
/// antenna's settling wandered.
pub fn find_closest_elevation(
    scan: &Scan,
    product: types::RadarProduct,
    target_elevation: f32,
) -> Option<f32> {
    scan.sweeps()
        .iter()
        .filter_map(|sweep| {
            let radials = sweep.radials();
            let r = radials.first()?;
            let elevation = crate::volumetric::sweep_elevation_deg(radials)?;
            let rounded = (elevation * 10.0).round() as f32 / 10.0;
            product.get_moment(r).is_some().then_some(rounded)
        })
        .min_by(|a, b| ((*a - target_elevation).abs()).total_cmp(&((*b - target_elevation).abs())))
}

/// Find the newest sweep in `elevation_angle`'s tilt *family* that carries
/// the requested product's moment data.
///
/// Searched newest-first: SAILS volumes carry several cuts of the low tilts,
/// minutes apart, and the last one in the scan is the most recent. The
/// reference display shows the newest cut too — cursor samples of its NROT
/// correlate at 0.95 with the matching cut and near zero with the stale ones.
///
/// Sweeps are compared on [`crate::volumetric::sweep_elevation_deg`] — the
/// **median** of the sweep's radials — and the window is a tight 0.1°.
///
/// Both halves of that are one decision. This used to match the *first
/// radial's* angle within 0.3°, and the wide window was a workaround for the
/// first radial rather than a property of the radar: the antenna is still
/// settling when a sweep opens, so across 951 archived sweeps the opening radial
/// landed within 0.05° of its own cut's commanded angle only **36%** of the
/// time, and missed it by as much as 0.23°. The median landed within 0.05° on
/// **99.9%**, and never missed by more than 0.06°.
///
/// (0.23° is the first radial's error from nominal. The *span* of elevations
/// within one sweep is a different and wider quantity — it reaches 0.43° — and
/// the two are easy to confuse: the opening radial sits somewhere inside that
/// span rather than at its extreme.)
///
/// A window wide enough to absorb that error is also wide enough to admit the
/// *neighbouring* cut, and since the search runs newest-first it then answered
/// with whichever cut came last rather than whichever was nearer. Measured over
/// the live archive, that drew the wrong tilt for roughly **three quarters** of
/// all picker entries — one KDDC VCP 215 volume offered 0.5, 0.6, 0.7 and 0.8
/// and drew the *same* 0.48° sweep for all four, leaving its 0.88° cut
/// unreachable.
///
/// Removing the drift removes the need for the workaround: on the median, 0.1°
/// is still twice the 0.05° worst case of the picker's own rounding, and it is
/// narrow enough to keep adjacent cuts apart. Keeping the wide window on top of
/// the median would have left most of the harm in place, so neither change is
/// useful without the other.
///
/// Within the family, non-Doppler products prefer the newest sweep *without*
/// a velocity moment: a split cut's Doppler half repeats a short-range copy
/// of the surveillance moments, and the reference display draws reflectivity
/// from the surveillance half (measured on a KLOT SAILS volume: the 0.63°
/// surveillance cut's painted mask matches the reference at IoU 0.73 /
/// area ratio 0.98, against 0.69 / 0.89 for the newer 0.53° Doppler cut).
/// Upper tilts are single merged cuts carrying everything, so the preference
/// falls back to any sweep with the product's moment.
/// `pub(crate)` for [`crate::render_input`], which has to make this exact
/// choice against the whole volume so the one sweep it carries is the one
/// `find_sweep` reaches again on the reconstructed scan.
///
/// The live elevation audit that measured these rules over archived volumes
/// (`live_elevation_audit`, with its mirror test of this function) lives on
/// branch `campaign-harness`; changing `find_sweep` invalidates that audit's
/// figures until it is re-run there.
pub(crate) fn find_sweep(
    scan: &Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<&[Radial]> {
    find_sweep_owner(scan, product, elevation_angle).map(nexrad_model::data::Sweep::radials)
}

/// [`find_sweep`], answering the `Sweep` rather than its radials.
///
/// The policy lives here and [`find_sweep`] is one line over it, so there is
/// no second selection rule that could come to disagree with the first.
///
/// It exists because [`crate::render_input::RenderInput`] needs one thing off
/// the chosen sweep that a `&[Radial]` cannot give it *authoritatively*: the
/// **sweep's** `elevation_number`, which is what
/// [`crate::sampler::VolumeSampler`] keys its tilt ladder on. A radial carries
/// an elevation number too, and in every producer in this workspace the two
/// agree — the archive decoder splits radials into sweeps *by* that field, and
/// the chunk assembler does the same — but "they agree in the producers we
/// have" is a claim about data, and `Sweep::new` takes the number separately,
/// so reading the radial's would be a second source of truth for the one field
/// the ladder cannot get wrong. This returns the first.
///
/// # Which radials the moment questions are asked of
///
/// **Every radial, not the first one.** Whether a sweep carries a product, and
/// whether it is a split cut's Doppler half, are properties of the *sweep*, and
/// asking them of `radials.first()` let one radial answer for all 720 of them:
/// a leading radial whose moment was missing — a truncated record, a mis-framed
/// message, an antenna still settling — hid the entire sweep from this search,
/// and the pane then rendered a different cut or nothing at all. On the extent
/// path that is not a cosmetic failure, because the chosen sweep's reach is
/// what [`types::plan_view_extent_km`] frames the raster and the range ring at:
/// a hidden 88.8 km Doppler cut is answered by a 417 km surveillance one, and
/// the ring moves 328 km.
///
/// It is the same first-radial assumption the wind-profile fits carried, and it
/// costs nothing to drop: `any` short-circuits on the first radial that does
/// carry the moment, which on every well-formed sweep is the first radial.
/// `a_sweep_whose_leading_radial_is_blank_is_still_found_and_still_framed_by_its_own_reach`
/// is the guard.
pub(crate) fn find_sweep_owner(
    scan: &Scan,
    product: types::RadarProduct,
    elevation_angle: f32,
) -> Option<&nexrad_model::data::Sweep> {
    let newest = |surveillance_only: bool| {
        scan.sweeps().iter().rev().find(|sweep| {
            let radials = sweep.radials();
            crate::volumetric::sweep_elevation_deg(radials)
                .map(|elevation| {
                    (elevation - f64::from(elevation_angle)).abs() < ELEVATION_WINDOW
                        && radials.iter().any(|r| product.get_moment(r).is_some())
                        && !(surveillance_only
                            && radials.iter().any(|r| r.velocity().is_some()))
                })
                .unwrap_or(false)
        })
    };
    match product {
        types::RadarProduct::Velocity
        | types::RadarProduct::SpectrumWidth
        | types::RadarProduct::NormalizedRotation
        | types::RadarProduct::StormRelativeVelocity => newest(false),
        _ => newest(true).or_else(|| newest(false)),
    }
}

/// The widest wedge any radial is ever painted at, degrees.
///
/// A ceiling on top of the per-sweep one, and not redundant with it, because
/// the per-sweep ceiling is derived from the sweep and a sweep with two radials
/// in it has no meaningful spacing to derive from: two azimuths give two
/// circular gaps, and [`crate::azimuth::median_azimuth_step_deg`] answers with
/// the larger of them, so a pair 10° apart reports a step of 350°. As a sampler
/// footprint that is generous; as a wedge width it is a 350°-wide chord lens
/// laid across the display, apex at the site. A radial that declares nothing
/// would fall back onto exactly that number.
///
/// 2.0° caps every such pathology while touching nothing real: Level II
/// declares 0.5° or 1.0° and nothing else, the RDA has no third resolution, so
/// no sweep this display has ever drawn comes within a factor of two of the
/// cap.
const MAX_WEDGE_DEG: f64 = 2.0;

/// How wide to paint one Level II radial, degrees, given what it declares and
/// what the sweep around it measures.
///
/// A radial declares its own azimuth resolution on the wire and has since
/// Message 31 — 0.5° for a super-res cut, 1.0° otherwise — and that declaration
/// is the honest answer to "how much sky does this sample stand for". The
/// sweep's median step is the cross-check, not the answer: it is what the
/// radials around this one are actually spaced by, and
/// [`crate::azimuth::MAX_ADJACENT_GAP_STEPS`] is already the rule for how far
/// past that spacing a consumer may reach before it is inventing coverage.
///
/// So the declaration wins where it exists and is believable, the median stands
/// in where it does not (the legacy Message 1 path is the case that matters,
/// and it declares a flat 1.0°, which is what those volumes are — so this
/// fallback is reached only by a genuinely empty declaration), and both are
/// held under [`MAX_WEDGE_DEG`].
///
/// What this buys is the property the sampler already had and the plan view did
/// not: a sweep with radials missing leaves the gap *empty*. The width no
/// longer has anything to do with where the next radial is, so a survivor
/// cannot fan across to it.
fn l2_wedge_width_deg(declared_deg: f64, median_step_deg: f64) -> f64 {
    let base = if declared_deg > 0.0 {
        declared_deg
    } else {
        median_step_deg
    };
    base.min(crate::azimuth::MAX_ADJACENT_GAP_STEPS * median_step_deg)
        .min(MAX_WEDGE_DEG)
}

/// How wide to paint one row of a derived polar grid, degrees.
///
/// NROT, SRV and KDP are computed onto grids of their own, and a grid row
/// carries an azimuth but no declared resolution — the declaration belongs to
/// the radial the row was computed *from*, and deliberately is not threaded
/// through, since a derived value spans whatever its input span was rather than
/// one radial's. So these rows are painted at the sweep's measured median step,
/// capped by [`MAX_WEDGE_DEG`] for the two-radial reason given there.
///
/// The alternative these replace was `360 / rows`, which is the same
/// fan-to-the-neighbour assumption in a different disguise: it is only the
/// spacing if the grid closes the circle. A 36° NROT sector of 0.5° radials
/// came out at 5° a row and smeared two and a half degrees past its own edge.
fn derived_grid_wedge_deg(azimuths_deg: &[f64]) -> f64 {
    crate::azimuth::median_azimuth_step_deg(azimuths_deg.iter().copied())
        .unwrap_or(1.0)
        .min(MAX_WEDGE_DEG)
}

/// How far apart two radials' reaches may be before the sweep is reported as
/// disagreeing with itself, km.
///
/// Within one cut the RDA declares one gate count per moment, and real volumes
/// hold to it exactly: walked over 102 sweeps and all six moments of the
/// opening volume at KTLX, KDMX, KMPX, KAMX, KFTG and TJUA — plains, upper
/// midwest, coastal, mountain, tropical — every sweep's radials agreed on their
/// reach to the last bit, spread **0.000 km** everywhere. So this fires on
/// something wrong rather than on ordinary variation, and 1.0 km is chosen for
/// what it catches rather than for what it tolerates: it is under half of the
/// widest gate this display draws (TDWR surveillance, 0.3 km), so a difference
/// of four gates anywhere trips it.
///
/// A `warn` and not a clamp: a sweep whose radials disagree is a sweep this
/// code cannot resolve on its own, and guessing which of them is right is how
/// the first-radial rule got this wrong in the first place.
const RADIAL_REACH_DISAGREEMENT_KM: f64 = 1.0;

/// How far the sweep's data actually reaches, km: the **greatest** reach among
/// the radials carrying this product's moment, or 0 if none do.
///
/// The greatest and not the first, which is what this used to read. A radial
/// whose gate count came back short — a truncated record, a mis-framed
/// message — is a radial, not a sweep, and taking the first one that happened
/// to carry the moment let one of them speak for all 720. The greatest and not
/// the median either: this number becomes the extent the raster is projected at
/// and the range the gate loops stop at, so anything below the true maximum is
/// real data cut off at the edge of the image.
///
/// That does mean one *over*-long radial coarsens the whole sweep's km/pixel
/// until the next render. The trade is deliberate — a render that is slightly
/// too zoomed-out still shows everything, a render that is too zoomed-in does
/// not — and the spread warning below is what makes the case visible when it
/// happens rather than leaving it to be noticed on the glass.
fn compute_max_range(radials: &[Radial], product: types::RadarProduct) -> f64 {
    let mut reach = f64::NEG_INFINITY;
    let mut shortest = f64::INFINITY;
    for radial in radials {
        let Some(moment) = product.get_moment(radial) else {
            continue;
        };
        let km = moment.first_gate_range_km()
            + f64::from(moment.gate_count()) * moment.gate_interval_km();
        reach = reach.max(km);
        shortest = shortest.min(km);
    }
    if reach == f64::NEG_INFINITY {
        return 0.0;
    }
    if reach - shortest > RADIAL_REACH_DISAGREEMENT_KM {
        log::warn!(
            "{product:?}: this sweep's radials do not agree on how far they reach — \
             {shortest:.1}km to {reach:.1}km, a spread of {:.1}km; rendering to the \
             longest",
            reach - shortest
        );
    }
    reach
}

/// The factor between the slant range a sweep's gates are measured at and the
/// ground range they sit over: `cos e` of the sweep's **median** elevation.
///
/// The median and not the first radial's, and not the tilt label either,
/// because that is the angle [`crate::sampler`] keys its rungs on
/// ([`crate::volumetric::sweep_elevation_deg`]) — a section and a plan view
/// that disagree about which angle a sweep flew disagree about where its
/// echoes are, and the antenna is still settling when a sweep starts (a live
/// KMRX 0.5° cut opened at 0.283°).
///
/// Hoisted once per sweep rather than evaluated per gate: it is one number for
/// every gate of every radial, and the gate loops below run a third of a
/// million times per sweep.
///
/// Two answers are not the median's. `None` is an empty sweep, which paints
/// nothing whichever factor it is handed. A non-finite median is a corrupt
/// angle, and 1.0 draws that sweep where the RDA said it was measured, which
/// is a better failure than `cos NaN` collapsing all of it onto the site.
fn sweep_ground_factor(radials: &[Radial]) -> f64 {
    match crate::volumetric::sweep_elevation_deg(radials) {
        Some(e) if e.is_finite() => e.to_radians().cos().clamp(0.0, 1.0),
        _ => 1.0,
    }
}

/// Project a field onto the image, at the extent its own data asks for.
///
/// Every render path in this module comes through here, and each already knew
/// how far its data reached — the number used to be carried through purely so
/// it could be reported. It now decides the geometry: [`types::plan_view_extent_km`]
/// turns the reach into the raster's half-width, the bounds and the projection
/// are both built from that one number, and it is what comes back as
/// `max_range_km` for the placement sites downstream.
///
/// So the returned figure is the **extent of the picture**, not the reach of
/// the data. Below the floor those differ — a 40 km Level III product is drawn
/// on a 230 km frame — and the picture's extent is the one a consumer can do
/// anything with: it is what says where the corners of the texture go and
/// which pixel a hover lands in. Where the data actually stopped is a property
/// of the sweep, answered by [`compute_max_range`], and a cross-section is
/// where this display reports it (`SectionAxes::coverage_ground_range_km`).
///
/// `reach_km` is measured in **the coordinate its caller paints in**, which
/// for the four per-tilt paths is a ground range: they have already folded
/// [`sweep_ground_factor`] into both the reach and every gate, so the frame
/// and the picture inside it are sized by the same ruler. A raster whose
/// gates were shortened but whose extent was not would draw a 60° TDWR tilt
/// at half radius on a frame twice as wide as it needed.
///
/// `side_ceiling_px` is the largest side the caller will accept; the extent and
/// that ceiling together give the raster's own side through
/// [`types::raster_side_px`], which is the second half of the geometry and the
/// only half this crate cannot decide alone.
fn render_with_projection(
    radar_lat: f64,
    radar_lon: f64,
    reach_km: f64,
    product: types::RadarProduct,
    side_ceiling_px: usize,
    label: &str,
    fill: impl FnOnce(&MercatorProjection, &RenderBuffers),
) -> SweepRender {
    let extent_km = types::plan_view_extent_km(reach_km);
    let side_px = types::raster_side_px(extent_km, side_ceiling_px);
    let bounds = types::ImageBounds::from_radar_site(radar_lat, radar_lon, extent_km);
    let proj = MercatorProjection::from_bounds(radar_lat, &bounds, extent_km, side_px);
    let bufs = RenderBuffers::new(product, side_px);

    fill(&proj, &bufs);

    let output = bufs.into_output(extent_km);
    log::info!(
        "{} rendering complete: data reaches {:.1}km, projected at ±{:.1}km \
         onto {side_px}² px ({:.2} px/km)",
        label,
        reach_km,
        output.max_range_km,
        proj.px_per_km,
    );
    output
}

// ── Public rendering functions ───────────────────────────────────────────────

//
// Four of these come in pairs: a `_sized` entry taking the caller's side
// ceiling, and the plain name over it passing [`types::IMAGE_SIZE`]. The plain
// name is not a legacy shim — it is the honest answer for every caller that
// does not own a GPU and so has nothing to say about texture limits, which is
// every test in this workspace and every consumer outside the frontend. What
// the pairing buys is that "the base size" is spelt once, in one place, so a
// caller who wanted the floor's behaviour cannot get anything else.
//

/// Render radar data to an image projected for geographic display; see
/// [`SweepRender`] for what comes back.
///
/// The volume declares nothing, so the velocity products' dealiaser estimates
/// its fold limit off the sweep and [`SweepRender::nyquist_ms`] is `None` — the
/// answer for a caller holding only model types, which is every caller of this
/// short form. [`render_radar_to_image_full`] takes the table.
pub fn render_radar_to_image(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<SweepRender> {
    render_radar_to_image_full(
        data,
        elevation_angle,
        product,
        radar_lat,
        radar_lon,
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
    )
}

/// [`render_radar_to_image`] from a [`RenderInput`] instead of a `Scan`.
///
/// This is the entry point for a caller that does not hold the volume — the
/// browser's rasterization worker, which is handed
/// [`RenderInput::to_bytes`](crate::render_input::RenderInput::to_bytes) over a
/// message port because a decoded `Scan` is tens of megabytes and a `RenderInput`
/// is one sweep.
///
/// It reconstructs a `Scan` and runs the ordinary renderer, so there is one
/// rasterizer rather than two that could disagree about a pixel; see
/// [`crate::render_input`] for why the reconstruction is exact.
pub fn render_from(input: &crate::render_input::RenderInput) -> Option<SweepRender> {
    render_from_sized(input, types::IMAGE_SIZE)
}

/// [`render_from`] at a caller-chosen side ceiling — the entry the offload
/// job's `execute` takes, because it is the one place that knows both whether
/// this is a static render or a loop frame and what this device's textures can
/// be.
pub fn render_from_sized(
    input: &crate::render_input::RenderInput,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    // Lifted back out separately because `to_scan` rebuilds model types and the
    // model type is precisely what drops the number. The payload has carried it
    // per sweep since the sampler needed it for the vertical views — the same
    // field, read by the path that draws the plan view, so a worker and the
    // main thread unfold a velocity sweep around the same limit.
    render_radar_to_image_full_sized(
        &input.to_scan(),
        input.elevation(),
        input.product(),
        input.radar_lat(),
        input.radar_lon(),
        input.storm_motion_override(),
        input.env_heights_km_msl(),
        &input.declared_nyquist(),
        side_ceiling_px,
    )
}

/// [`render_radar_to_image`] plus the two render parameters: the storm
/// motion override, in knots and degrees-from — read by storm-relative
/// velocity alone; `None` is "no override" and SRV applies the Bunkers
/// right-mover from the volume's own wind profile ([`crate::srv`]) — and
/// the environmental 0 °C / −20 °C heights in km MSL, read by the products
/// [`types::RadarProduct::reads_env_heights`] names: the hail pair, whose
/// field is undefined without them so `None` renders nothing
/// ([`crate::hail`]), and the hybrid classification, which answers `None` by
/// falling back to the operational adaptation defaults and so draws a
/// *different* picture rather than no picture ([`render_hhc_to_image`], 30
/// lines below).
///
/// The environmental wind profile NROT's and SRV's dealiasers seed from is
/// not a parameter: it is fit from the volume's own velocity tilts
/// ([`crate::velocity::volume_wind_profile`]). The RPG's NVW product used to
/// be an alternate
/// source, until the local VAD fit was validated against the RPG's own
/// dealiased velocity and the fetch dropped.
///
/// `declared_nyquist` is what each cut said about where its velocity folds
/// ([`crate::nyquist::DeclaredNyquist`]). NROT and SRV dealias, and this is the
/// interval they fold around; the sweep's own value also comes back in
/// [`SweepRender::nyquist_ms`]. Pass an empty table for a volume that declared
/// nothing and the dealiaser estimates, which is what every path here did
/// before the declaration crossed the model boundary.
#[allow(clippy::too_many_arguments)]
pub fn render_radar_to_image_full(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    storm_motion_override: Option<(f32, f32)>,
    env_heights_km_msl: Option<(f64, f64)>,
    declared_nyquist: &crate::nyquist::DeclaredNyquist,
) -> Option<SweepRender> {
    render_radar_to_image_full_sized(
        data,
        elevation_angle,
        product,
        radar_lat,
        radar_lon,
        storm_motion_override,
        env_heights_km_msl,
        declared_nyquist,
        types::IMAGE_SIZE,
    )
}

/// [`render_radar_to_image_full`] at a caller-chosen side ceiling. See
/// [`types::raster_side_px`] for what a ceiling is and why the caller owns it.
#[allow(clippy::too_many_arguments)]
pub fn render_radar_to_image_full_sized(
    data: &Scan,
    elevation_angle: f32,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    storm_motion_override: Option<(f32, f32)>,
    env_heights_km_msl: Option<(f64, f64)>,
    declared_nyquist: &crate::nyquist::DeclaredNyquist,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    if product == types::RadarProduct::EchoTopsInterpolated {
        return render_echo_tops_interp_to_image(data, radar_lat, radar_lon, side_ceiling_px);
    }

    if matches!(
        product,
        types::RadarProduct::ProbabilityOfSevereHail | types::RadarProduct::MaxExpectedHailSize
    ) {
        return render_hail_to_image(
            data,
            product,
            radar_lat,
            radar_lon,
            env_heights_km_msl,
            side_ceiling_px,
        );
    }

    if product == types::RadarProduct::HydrometeorClassification {
        return render_hhc_to_image(
            data,
            radar_lat,
            radar_lon,
            env_heights_km_msl,
            side_ceiling_px,
        );
    }

    // The owner and not just its radials: the declared Nyquist table is keyed
    // by the RDA's `elevation_number`, and a `Sweep` is where that number is
    // authoritative. A radial carries one too and in every producer here they
    // agree, but `find_sweep_owner` exists precisely because "they agree in the
    // producers we have" is a claim about data — and reading the wrong cut's
    // number would fold this sweep around another sweep's PRF.
    let owner = find_sweep_owner(data, product, elevation_angle)?;
    let radials = owner.radials();
    let nyquist_ms = declared_nyquist.get(owner.elevation_number());

    if product == types::RadarProduct::NormalizedRotation {
        return render_nrot_to_image(
            data,
            radials,
            radar_lat,
            radar_lon,
            nyquist_ms,
            side_ceiling_px,
        );
    }

    if product == types::RadarProduct::StormRelativeVelocity {
        return render_srv_to_image(
            data,
            radials,
            radar_lat,
            radar_lon,
            storm_motion_override,
            nyquist_ms,
            side_ceiling_px,
        );
    }

    // The stand-in for an unmeasurable sweep is 1.0° because that is the
    // coarser of the two resolutions the RDA has, so a sweep too degenerate to
    // measure is painted as if it were the wider one rather than as a spoke.
    let median_step = crate::azimuth::median_azimuth_step_deg(
        radials.iter().map(|r| f64::from(r.azimuth_angle_degrees())),
    )
    .unwrap_or(1.0);
    // Slant out of `compute_max_range`, ground into the projection: how far
    // the *data* goes is a property of the sweep, how wide the *picture* is
    // has to be the ground it covers, and the four sweep paths in this module
    // are where the one becomes the other.
    let cos_e = sweep_ground_factor(radials);
    let ground_reach_km = compute_max_range(radials, product) * cos_e;

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        ground_reach_km,
        product,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            radials
                .par_iter()
                .enumerate()
                .for_each(|(radial_idx, radial)| {
                    let azimuth = radial.azimuth_angle_degrees() as f64;
                    let width = l2_wedge_width_deg(
                        f64::from(radial.azimuth_spacing_degrees()),
                        median_step,
                    );
                    let ctx = RadialContext::new(azimuth, width / 2.0);

                    if let Some(moment) = product.get_moment(radial) {
                        let first_gate_range = moment.first_gate_range_km();
                        let gate_size = moment.gate_interval_km();

                        // `iter`, not `values`: the latter is `iter().collect()`
                        // and this walk is strictly sequential, so the `Vec`
                        // would be eight bytes per gate allocated and dropped
                        // for every radial of every render.
                        for (gate_idx, moment_value) in moment.iter().enumerate() {
                            // A gate is measured out along the beam and drawn
                            // on the ground under it, so what this loop counts
                            // in and what the projection paints in are two
                            // different ranges. `cos_e` is monotone in neither
                            // direction here — it is one constant for the
                            // sweep — so the break below still short-circuits.
                            let ground_km =
                                (first_gate_range + (gate_idx as f64 * gate_size)) * cos_e;
                            // The edge of the image, and the image was sized
                            // from this sweep's own reach — so on a sweep whose
                            // radials agree this never fires, and a 1832-gate
                            // surveillance cut paints all 458 km of itself. It
                            // is reached only where a radial runs past the
                            // extent the sweep as a whole asked for: past
                            // `types::MAX_EXTENT_KM`, or past a shorter
                            // neighbour's agreed reach on a sweep
                            // `compute_max_range` has already warned about.
                            if ground_km > proj.extent_km {
                                break;
                            }

                            use nexrad_model::data::MomentValue;
                            let scaled_value = match moment_value {
                                // `v < 999.0` is false for a NaN too, so the
                                // one test drops both the out-of-scale codes
                                // and anything that decoded to nothing.
                                MomentValue::Value(v) if v < 999.0 => v,
                                MomentValue::Value(_) => continue,
                                // A range-folded gate is a *reading*, not an
                                // absence, so it claims its pixel like any
                                // other and is coloured at output time. See
                                // [`RANGE_FOLDED_BITS`].
                                MomentValue::RangeFolded => RANGE_FOLDED_SENTINEL,
                                // Below threshold stays transparent: there the
                                // radar looked and found nothing above the
                                // noise, which is what an unpainted pixel
                                // already says. Painting the two alike would
                                // lay the folded-gate colour over most of a
                                // clear-air sweep.
                                MomentValue::BelowThreshold => continue,
                            };

                            let from = GateId {
                                radial: radial_idx,
                                gate: gate_idx,
                            };
                            // The depth foreshortens with the range: a gate is
                            // a segment of the beam, and its shadow on the
                            // ground is that segment times the same `cos e`.
                            // Painting a full-depth cell at a shortened range
                            // would overlap its neighbour by `1 − cos e` of a
                            // gate and leave the sweep's outer edge long.
                            proj.render_gate(
                                bufs,
                                &ctx,
                                ground_km,
                                gate_size * cos_e,
                                scaled_value,
                                from,
                            );
                        }
                    }
                });
        },
    );
    Some(output.declaring(nyquist_ms))
}

/// Render NROT (Normalized Rotation): azimuthal shear derived from Level II
/// velocity, normalized by range to remove beam broadening and scaled to a
/// unitless field where >1.0 is significant and >2.5 extreme.
fn render_nrot_to_image(
    scan: &Scan,
    radials: &[Radial],
    radar_lat: f64,
    radar_lon: f64,
    declared_nyquist_ms: Option<f64>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let num_radials = radials.len();
    if num_radials < 3 {
        return None;
    }

    let vg = crate::velocity::grid(radials)?;

    let cos_e = sweep_ground_factor(radials);
    let ground_reach_km =
        (vg.first_gate_range_km + vg.gate_count as f64 * vg.gate_interval_km) * cos_e;
    let wedge_deg = derived_grid_wedge_deg(&vg.azimuths_deg);

    // The physics keeps the first radial's angle. It is the same number the
    // shear normalization has always divided by, and the two are not
    // interchangeable: `sweep_ground_factor`'s median is where the sweep is
    // *drawn*, this is what the sweep was *computed* at, and swapping one in
    // for the other would move every NROT value in the field to buy a
    // difference the settling spread bounds at 0.23° (a KMRX 0.5° cut opening
    // at 0.283°, the worst of 203 volumes). Placement is worth correcting at
    // 60°; a value is not worth perturbing for a fifth of a degree.
    let elevation_deg = radials
        .first()
        .map(|r| r.elevation_angle_degrees() as f64)
        .unwrap_or(0.5);
    let profile = crate::velocity::volume_wind_profile(scan);
    let nrot_grid = crate::nrot::compute_nrot_grid_with_profile(
        &vg.sweep(declared_nyquist_ms),
        elevation_deg,
        profile.as_ref(),
    );

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        ground_reach_km,
        types::RadarProduct::NormalizedRotation,
        side_ceiling_px,
        "NROT",
        |proj, bufs| {
            nrot_grid.par_iter().enumerate().for_each(|(i, nrot_row)| {
                let ctx = RadialContext::new(vg.azimuths_deg[i], wedge_deg / 2.0);

                for (j, &nrot_val) in nrot_row.iter().enumerate() {
                    if nrot_val.is_nan() {
                        continue;
                    }

                    let ground_km =
                        (vg.first_gate_range_km + j as f64 * vg.gate_interval_km) * cos_e;
                    if ground_km > proj.extent_km {
                        break;
                    }

                    // Sub-threshold shear must not claim the pixel at all, or
                    // it would outrank a real return from a lower radial.
                    // `into_output` would colour it transparent either way, so
                    // this has to happen here, not there.
                    let scaled_value = nrot_val as f32;
                    let color =
                        get_color_for_value(types::RadarProduct::NormalizedRotation, scaled_value);
                    if color.3 == 0 {
                        continue;
                    }

                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(
                        bufs,
                        &ctx,
                        ground_km,
                        vg.gate_interval_km * cos_e,
                        scaled_value,
                        from,
                    );
                }
            });
        },
    );
    Some(output.declaring(declared_nyquist_ms))
}

/// Render storm-relative velocity derived locally from Level II: the sweep's
/// velocity dealiased under the Coverage profile, plus the storm-motion
/// correction — a user override when one is set, otherwise the Bunkers
/// right-mover from the volume's wind profile. Values are m/s, like every
/// Level II velocity field, so the palette and `format_value` read them
/// unchanged. See [`crate::srv`].
///
/// `None` when no vector exists at all — no override and a wind profile too
/// hollow for even the mean-wind fallback — because painting base velocity
/// under a storm-relative label is the failure the old Level III path
/// refused too (it waited for an `N0S`).
fn render_srv_to_image(
    scan: &Scan,
    radials: &[Radial],
    radar_lat: f64,
    radar_lon: f64,
    storm_motion_override: Option<(f32, f32)>,
    declared_nyquist_ms: Option<f64>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    if radials.len() < 3 {
        return None;
    }
    let elevation_deg = radials
        .first()
        .map(|r| r.elevation_angle_degrees() as f64)
        .unwrap_or(0.5);
    let profile = crate::velocity::volume_wind_profile(scan);
    let user = storm_motion_override.and_then(|(speed_kt, direction_deg)| {
        crate::srv::SrvMotion::user_override(speed_kt, direction_deg)
    });
    let motion = crate::srv::storm_motion(profile.as_ref(), user)?;
    log::info!(
        "SRV {elevation_deg:.1}°: {:.1} kt from {:.1}° ({:?})",
        motion.speed_kt,
        motion.direction_deg,
        motion.source,
    );
    let grid = crate::srv::compute_srv_grid(
        radials,
        elevation_deg,
        profile.as_ref(),
        &motion,
        declared_nyquist_ms,
    )?;

    // As in NROT: the dealiaser above keeps the first radial's angle, this is
    // where the finished field is *placed*.
    let cos_e = sweep_ground_factor(radials);
    let ground_reach_km =
        (grid.first_gate_range_km + grid.gate_count as f64 * grid.gate_interval_km) * cos_e;
    let wedge_deg = derived_grid_wedge_deg(&grid.azimuths_deg);
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        ground_reach_km,
        types::RadarProduct::StormRelativeVelocity,
        side_ceiling_px,
        "SRV",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(i, row)| {
                let ctx = RadialContext::new(grid.azimuths_deg[i], wedge_deg / 2.0);
                for (j, &value) in row.iter().enumerate() {
                    if value.is_nan() {
                        continue;
                    }
                    let ground_km =
                        (grid.first_gate_range_km + j as f64 * grid.gate_interval_km) * cos_e;
                    if ground_km > proj.extent_km {
                        break;
                    }
                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(
                        bufs,
                        &ctx,
                        ground_km,
                        grid.gate_interval_km * cos_e,
                        value as f32,
                        from,
                    );
                }
            });
        },
    );
    Some(output.declaring(declared_nyquist_ms))
}

/// Render interpolated echo tops: the whole reflectivity volume reduced to a
/// 1° × 1 km polar grid of threshold-crossing heights, painted with the echo
/// tops palette. Tilt-independent — every elevation request renders the same
/// volume product.
pub fn render_echo_tops_interp_to_image(
    scan: &Scan,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let grid = crate::volumetric::compute_echo_tops(scan);
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        types::RadarProduct::EchoTopsInterpolated,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    proj.render_gate(bufs, &ctx, r as f64 + 0.5, 1.0, *v, from);
                }
            });
        },
    );
    Some(output)
}

/// Render VIL density from the RPG's own two published products for one
/// volume — Digital VIL (134) over Enhanced Echo Tops (135), see
/// [`crate::vild`] — as a 1° × 1 km polar grid in g/m³ painted with the
/// VIL-density palette.
///
/// The Level III counterpart of [`render_level3_message_to_image`], separate
/// only because it takes **two** messages: the palette, the value grid the
/// hover reads and the legend downstream are the ordinary Level III display
/// pipeline's.
///
/// `None` where the pair cannot make a field — a mismatched volume above all,
/// which is refused rather than painted ([`crate::vild::Refusal`]). Drawing
/// nothing is the same answer the hail products give without a sounding, and
/// the reason is logged.
pub fn render_derived_vild_to_image(
    dvl: &nexrad_level3::model::Level3Message,
    eet: &nexrad_level3::model::Level3Message,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<SweepRender> {
    render_derived_vild_to_image_sized(dvl, eet, radar_lat, radar_lon, types::IMAGE_SIZE)
}

/// [`render_derived_vild_to_image`] at a caller-chosen side ceiling.
///
/// The derivation is a 360 × 1 km grid, so its extent never leaves the floor
/// and the ceiling never changes what this draws. It takes one anyway, because
/// `execute` dispatches every render-producing job through one shape and an
/// arm that quietly ignored the size would be the one place a future
/// longer-reaching Level III product got drawn at the wrong scale.
pub fn render_derived_vild_to_image_sized(
    dvl: &nexrad_level3::model::Level3Message,
    eet: &nexrad_level3::model::Level3Message,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let grid = match crate::vild::compute_vild(dvl, eet) {
        Ok(grid) => grid,
        Err(refusal) => {
            log::info!("VIL density: nothing to render — {refusal:?}");
            return None;
        }
    };
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        types::RadarProduct::VilDensity,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    proj.render_gate(bufs, &ctx, r as f64 + 0.5, 1.0, *v, from);
                }
            });
        },
    );
    Some(output)
}

/// The site height every render path anchors its MSL heights on: the
/// **feedhorn**, not the ground under the tower.
///
/// One function rather than the call repeated at each site, because the two
/// call sites here spelled the conversion two different ways and would have
/// drifted apart the first time one of them was edited. [`crate::beam`]
/// measures every height above the antenna, so the feedhorn is the datum that
/// makes those heights MSL; the ground is 30–115 ft lower and was what both
/// call sites silently used before [`crate::sites::Datum`] existed.
///
/// Pinned by `the_render_paths_site_height_is_the_feedhorn`, which is the
/// only thing standing between this and a silent revert: neither hail nor
/// HCA has a render-level test that would notice a tower's worth of shift.
///
/// # When the table cannot place the coordinates at all
///
/// Zero, which makes every height this render produces one above the antenna
/// rather than above sea level. That is the only honest answer available: it
/// is not a claim that the radar is at sea level, it is the absence of an MSL
/// datum to add. The alternative — which is what
/// [`crate::eet::radar_height_ft_near`] used to do — was to report the
/// elevation of whichever site in the table was least far away, and for a pane
/// stranded at (0, 0) that is a real site two thousand kilometres off.
///
/// Unreachable for a site the build knows or a volume that states its own
/// position. See [`crate::types::ScanInfo::site_source`], which is where a
/// caller finds out that it is in this state.
fn render_site_height_ft(lat: f64, lon: f64) -> f64 {
    crate::eet::radar_height_ft_near(lat, lon, crate::sites::Datum::Feedhorn).unwrap_or(0.0)
}

/// Render one of the derived hail products ([`crate::hail`]): POSH in %,
/// or MEHS converted from the field's mm into **inches** — the palette's,
/// legend's and hover's unit — on a 1° × 1 km polar grid. Tilt-independent:
/// every elevation request renders the same volume product.
///
/// `env_heights_km_msl` is the per-site 0 °C / −20 °C pair
/// ([`crate::sounding::EnvHeights`], km MSL). **`None` renders nothing** —
/// `compute_hail` has no field without an environment, and this seam turns
/// that into the ordinary "no data" answer rather than a zero-filled grid
/// pretending to be one. The site height that resolves the MSL heights to
/// the beam's ARL datum comes from the nearest-site table, as the VIL
/// density render path's does.
pub fn render_hail_to_image(
    scan: &Scan,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    env_heights_km_msl: Option<(f64, f64)>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let Some((h0c_km_msl, hm20c_km_msl)) = env_heights_km_msl else {
        log::info!("{product:?}: no environmental heights — nothing to render");
        return None;
    };
    let env = crate::sounding::EnvHeights {
        h0c_km_msl,
        hm20c_km_msl,
        fetched_at: chrono::Utc::now(),
    };
    let radar_height_ft = render_site_height_ft(radar_lat, radar_lon);
    // The observing antenna's beam, not the WSR-88D's: this caps the ceiling
    // layer, and a TDWR's 0.55° is not the fleet's 0.95°. See
    // `hail::layer_bounds_km`.
    let beamwidth_deg = crate::beam::half_power_beamwidth_deg_near(radar_lat, radar_lon);
    let grids = crate::hail::compute_hail(scan, Some(&env), radar_height_ft, beamwidth_deg)?;
    const MM_PER_IN: f32 = 25.4;
    let (grid, unit_scale) = match product {
        types::RadarProduct::MaxExpectedHailSize => (grids.mehs_mm, 1.0 / MM_PER_IN),
        _ => (grids.posh, 1.0),
    };
    let max_range = grid.range_bins as f64;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        product,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    proj.render_gate(bufs, &ctx, r as f64 + 0.5, 1.0, *v * unit_scale, from);
                }
            });
        },
    );
    Some(output)
}

/// Render the locally derived Hybrid Hydrometeor Classification
/// ([`crate::hhc::compute_hhc`]): the whole volume's per-tilt
/// classification composited down the hybrid scan, a 1° × 0.25 km polar
/// grid of class codes painted with the HHC palette. Tilt-independent —
/// every elevation request renders the same volume product.
///
/// `env_heights_km_msl` is the sounding's (0 °C, −20 °C) pair; `None`
/// falls back to the operational adaptation defaults, exactly as the RPG
/// runs without environmental data. The radar height comes from the
/// nearest-site table, as the EET render path's does; the radial-header
/// parameters a decoded `Scan` cannot carry come from
/// [`crate::kdp::KdpParams::render_fallback`] (fleet-typical `dbz0`/atmos —
/// without a `dbz0` the SNR gate reads every gate as no-echo and the
/// product would be blank) with the initial phase from the volume's own
/// estimator, the same fallback family the KDP render arm documents.
///
/// # No `cos e` here either, and for a structural reason
///
/// A hybrid-scan grid has no elevation to correct by. Every bin is answered
/// by whichever cut first produced a rate there
/// ([`crate::hhc::composite_hybrid_scan`]), and the composite keeps the class
/// and not the tilt it came from, so there is no per-bin angle for a
/// correction to use — one number for the grid would be wrong for every bin
/// the lowest cut did not fill.
///
/// It would also be a correction to nothing much. The bins that reach a
/// higher cut are exactly the ones the cut below could not fill — the cone of
/// silence and blocked sectors, all of them near — and near is where `cos e`
/// costs nothing: 4.5° over 50 km moves a bin 154 m, under one 0.25 km bin,
/// while the 0.5° cut that fills the rest of the grid moves 11 m at 230 km.
/// The same holds for the CAPPI and `crate::dpprep` fields under it, which
/// are scored bin-for-bin against the RPG's own hybrid-scan products.
pub fn render_hhc_to_image(
    scan: &Scan,
    radar_lat: f64,
    radar_lon: f64,
    env_heights_km_msl: Option<(f64, f64)>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let radar_km_msl = render_site_height_ft(radar_lat, radar_lon) * 0.0003048;
    let params = crate::kdp::KdpParams {
        isdp_est_deg: crate::kdp::estimate_volume_isdp(scan),
        ..crate::kdp::KdpParams::render_fallback()
    };
    let (h0c, hsda) = match env_heights_km_msl {
        Some((h0c, hm20c)) => (
            h0c,
            crate::hca::HsdaHeights::from_env_heights(h0c, hm20c, radar_km_msl),
        ),
        None => (
            crate::hca::DEFAULT_HEIGHT_0_KM_MSL,
            crate::hca::HsdaHeights::operational_defaults(radar_km_msl),
        ),
    };
    let default_top_arl = (h0c - radar_km_msl).max(0.0);

    let all: Vec<&[nexrad_model::data::Radial]> =
        scan.sweeps().iter().map(|s| s.radials()).collect();
    let dp: Vec<&[nexrad_model::data::Radial]> = all
        .iter()
        .copied()
        .filter(|r| {
            r.first()
                .map(|x| x.differential_phase().is_some())
                .unwrap_or(false)
        })
        .collect();
    let cappi = crate::hca::build_refl_cappi(&dp);
    let ml_sweeps: Vec<&[nexrad_model::data::Radial]> = dp
        .iter()
        .copied()
        .filter(|r| {
            r.first()
                .map(|x| (4.0..=10.0).contains(&f64::from(x.elevation_angle_degrees())))
                .unwrap_or(false)
        })
        .collect();
    let ml =
        crate::hca::detect_melting_layer(&ml_sweeps, &params, default_top_arl, &hsda, Some(&cappi));
    let tilts = crate::hhc::volume_tilts(&all);
    let grid = crate::hhc::compute_hhc(&tilts, &params, &ml, &hsda, Some(&cappi))?;

    let max_gates = grid.values.iter().map(Vec::len).max().unwrap_or(0);
    let max_range = grid.first_gate_km + max_gates as f64 * grid.gate_interval_km;
    let output = render_with_projection(
        radar_lat,
        radar_lon,
        max_range,
        types::RadarProduct::HydrometeorClassification,
        side_ceiling_px,
        "Radar",
        |proj, bufs| {
            grid.values.par_iter().enumerate().for_each(|(az, row)| {
                let ctx = RadialContext::new(az as f64 + 0.5, 0.5);
                for (r, &v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let range_km = grid.first_gate_km + r as f64 * grid.gate_interval_km;
                    let from = GateId {
                        radial: az,
                        gate: r,
                    };
                    proj.render_gate(bufs, &ctx, range_km, grid.gate_interval_km, v, from);
                }
            });
        },
    );
    Some(output)
}

/// Render the locally derived Specific Differential Phase
/// ([`crate::kdp::compute_kdp`]) for the tilt family nearest
/// `elevation_angle`: the sweep is picked with the same tilt-family rule as
/// the differential phase moment it derives from (surveillance cut
/// preferred), and the recombined 1° × 0.25 km field paints with the KDP
/// palette.
///
/// `params` carries the radial-header quantities a decoded `Scan` lacks —
/// [`crate::kdp::KdpParams::from_archive`] when the caller holds the raw
/// file, `KdpParams::default()` (the documented estimator fallback)
/// otherwise.
pub fn render_derived_kdp_to_image(
    scan: &Scan,
    elevation_angle: f32,
    radar_lat: f64,
    radar_lon: f64,
    params: &crate::kdp::KdpParams,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    let radials = find_sweep(
        scan,
        types::RadarProduct::DifferentialPhase,
        elevation_angle,
    )?;
    let derived = crate::kdp::compute_kdp(radials, params)?;
    let n_radials = derived.values.len();
    if n_radials == 0 {
        return None;
    }
    let max_gates = derived.values.iter().map(Vec::len).max().unwrap_or(0);
    // KDP is a range derivative of ΦDP, so its grid keeps the differential
    // phase sweep's own gate spacing and reaches where that sweep reached —
    // and is placed on the ground the same way that sweep's moments are.
    let cos_e = sweep_ground_factor(radials);
    let ground_reach_km =
        (derived.first_gate_km + max_gates as f64 * derived.gate_interval_km) * cos_e;
    let wedge_deg = derived_grid_wedge_deg(&derived.azimuths_deg);

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        ground_reach_km,
        types::RadarProduct::SpecificDifferentialPhase,
        side_ceiling_px,
        "KDP",
        |proj, bufs| {
            derived.values.par_iter().enumerate().for_each(|(i, row)| {
                let ctx = RadialContext::new(derived.azimuths_deg[i], wedge_deg / 2.0);
                for (j, &v) in row.iter().enumerate() {
                    if v.is_nan() {
                        continue;
                    }
                    let ground_km =
                        (derived.first_gate_km + j as f64 * derived.gate_interval_km) * cos_e;
                    if ground_km > proj.extent_km {
                        break;
                    }
                    let from = GateId { radial: i, gate: j };
                    proj.render_gate(
                        bufs,
                        &ctx,
                        ground_km,
                        derived.gate_interval_km * cos_e,
                        v,
                        from,
                    );
                }
            });
        },
    );
    Some(output)
}

/// Render a Level III radial product, as [`render_radar_to_image`] does for a
/// Level II `Scan`.
///
/// For digital products `physical = (gate_byte - offset) / scale`. A `lut`
/// overrides that and indexes on the gate value directly, covering legacy 4-bit
/// products (16 entries) and VIL (256 entries).
#[allow(clippy::too_many_arguments)]
pub fn render_level3_radial_to_image(
    radial_packet: &nexrad_level3::model::RadialPacket,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    scale: f32,
    offset: f32,
    lut: Option<&[f32]>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    render_level3_radial_with_gate_km(
        radial_packet,
        radial_packet.gate_interval_km(),
        product,
        radar_lat,
        radar_lon,
        scale,
        offset,
        lut,
        side_ceiling_px,
    )
}

/// [`render_level3_radial_to_image`] with the gate spacing chosen by the
/// caller. The message path passes the PDB's product-code override — some
/// products' packet-16 scale-factor halfword does not carry the gate size
/// (see `ProductDescriptionBlock::range_gate_km`) — so the first gate's range
/// is also re-derived from `first_range_bin` at the chosen spacing rather
/// than taken from the packet.
///
/// # No `cos e` here, unlike the four Level II paths
///
/// The sweep rasterizers above turn a gate's slant range into the ground
/// range under it. This one deliberately does not, and the reason is that a
/// Level III bin is **already** the RPG's answer about where something is,
/// not a measurement this display is placing:
///
/// * The RPG bins on the ground itself. Its own generators carry `cos(elev)`
///   as a *display* constant rather than as a range correction —
///   `dualpol8bit.c` writes `cos(elev)·1000` into the packet-16 scale-factor
///   halfword, which `nexrad_level3::model::ProductDescriptionBlock::
///   range_gate_km` documents and overrides. Applying it a second time here
///   would move every bin of every product inward by a factor the generator
///   has already accounted for.
/// * The codes this app fetches leave nothing to correct anyway. `EET` and
///   `DVL` are **volume** products with no elevation at all, `DPR` is the
///   hybrid scan, and `N0K`/`N0H` are the 0.5° cut, where `1 − cos e` is
///   3.8e-5 — 11 m at the 300 km edge of an `N0K`, a twenty-third of one
///   0.25 km bin and a twentieth of a pixel.
/// * It would break the only thing that can tell whether the derived
///   products here are right. `crate::eet`, `crate::vil` and `crate::hhc`
///   are scored bin-for-bin against the fetched twin; shifting one side of
///   that comparison by a range-dependent factor turns a per-bin agreement
///   bar into a registration test.
#[allow(clippy::too_many_arguments)]
fn render_level3_radial_with_gate_km(
    radial_packet: &nexrad_level3::model::RadialPacket,
    gate_interval: f64,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    scale: f32,
    offset: f32,
    lut: Option<&[f32]>,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    if radial_packet.radials.is_empty() {
        return None;
    }

    let first_gate_range = radial_packet.first_range_bin as f64 * gate_interval;
    let num_bins = radial_packet.num_range_bins as usize;
    let actual_max_range = first_gate_range + num_bins as f64 * gate_interval;

    let radials = &radial_packet.radials;

    let output = render_with_projection(
        radar_lat,
        radar_lon,
        actual_max_range,
        product,
        side_ceiling_px,
        "Level III",
        |proj, bufs| {
            radials
                .par_iter()
                .enumerate()
                .for_each(|(radial_idx, radial_run)| {
                    let azimuth =
                        radial_run.start_angle as f64 + radial_run.angle_delta as f64 / 2.0;
                    let ctx = RadialContext::new(azimuth, radial_run.angle_delta as f64 / 2.0);

                    let bins_to_render = radial_run.gate_values.len().min(num_bins);
                    for (gate_idx, &gate_value) in
                        radial_run.gate_values[..bins_to_render].iter().enumerate()
                    {
                        if gate_value <= 1 {
                            continue;
                        }

                        let physical_value =
                            l3_physical_value(gate_value, product, scale, offset, lut);
                        if physical_value.is_nan() || physical_value >= 999.0 {
                            continue;
                        }

                        let range_km = first_gate_range + gate_idx as f64 * gate_interval;
                        if range_km > proj.extent_km {
                            break;
                        }

                        let from = GateId {
                            radial: radial_idx,
                            gate: gate_idx,
                        };
                        proj.render_gate(bufs, &ctx, range_km, gate_interval, physical_value, from);
                    }
                });
        },
    );
    Some(output)
}

/// Render a storm-relative velocity field derived from dealiased Level III
/// velocity. See [`crate::srm`].
///
/// Separate from [`render_level3_message_to_image`] because the derived packet
/// is not what any product on the wire looks like: its gate values are knots on
/// a scale this crate chose, and its gate spacing comes from the source
/// product's code rather than from the packet.
pub fn render_derived_srm_to_image(
    derived: &crate::srm::DerivedSrm,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    render_level3_radial_to_image(
        &derived.packet,
        types::RadarProduct::StormRelativeVelocity,
        radar_lat,
        radar_lon,
        derived.scale,
        derived.offset,
        None,
        side_ceiling_px,
    )
}

/// Render a Level III message, taking the radial packet, scale/offset and LUT
/// out of its symbology and product description blocks. Keeps every
/// nexrad-level3 internal out of the callers.
pub fn render_level3_message_to_image(
    l3_msg: &nexrad_level3::model::Level3Message,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
) -> Option<SweepRender> {
    render_level3_message_to_image_sized(l3_msg, product, radar_lat, radar_lon, types::IMAGE_SIZE)
}

/// [`render_level3_message_to_image`] at a caller-chosen side ceiling.
///
/// Every Level III product this display fetches stops well inside the floor —
/// the longest is a 460 km-diameter composite — so like the VIL density pair
/// this reaches the base size whatever ceiling it is handed. The parameter is
/// here so that the job that carries it does not have to know which of its
/// render arms cares.
pub fn render_level3_message_to_image_sized(
    l3_msg: &nexrad_level3::model::Level3Message,
    product: types::RadarProduct,
    radar_lat: f64,
    radar_lon: f64,
    side_ceiling_px: usize,
) -> Option<SweepRender> {
    use nexrad_level3::model::DataPacket;

    let radial_packet = l3_msg.symbology.as_ref().and_then(|sym| {
        sym.layers.iter().find_map(|layer| {
            layer.packets.iter().find_map(|pkt| {
                if let DataPacket::DigitalRadial(rp) = pkt {
                    Some(rp)
                } else {
                    None
                }
            })
        })
    });

    let rp = match radial_packet {
        Some(rp) => {
            log::debug!(
                "L3 {:?}: radials={}, bins={}, legacy={}, scale_factor={}",
                product,
                rp.radials.len(),
                rp.num_range_bins,
                rp.is_legacy,
                rp.scale_factor
            );
            rp
        }
        None => {
            log::warn!("L3 {:?}: no radial packet found in symbology!", product);
            return None;
        }
    };

    // Prefer the XDR scale/offset from packet 28 attributes: PDB thresholds do
    // not encode IEEE floats for some products (134 DVL, 135 EET).
    let scale = rp.xdr_data_scale.unwrap_or_else(|| l3_msg.pdb.data_scale());
    let offset = rp
        .xdr_data_offset
        .unwrap_or_else(|| l3_msg.pdb.data_offset());
    let product_lut = build_vil_lut(&l3_msg.pdb).or_else(|| build_eet_lut(&l3_msg.pdb));
    let legacy_lut;
    let lut: Option<&[f32]> = if product_lut.is_some() {
        product_lut.as_deref()
    } else if rp.is_legacy {
        legacy_lut = decode_legacy_thresholds(&l3_msg.pdb);
        Some(legacy_lut.as_slice())
    } else {
        None
    };

    log::debug!(
        "L3 {:?}: rendering with scale={}, offset={}, legacy={}, lut_len={:?}, xdr_scale={:?}, xdr_offset={:?}",
        product,
        scale,
        offset,
        rp.is_legacy,
        lut.map(|l| l.len()),
        rp.xdr_data_scale,
        rp.xdr_data_offset
    );

    // The packet's own gate spacing with the PDB's product-code override —
    // 99/154/163's scale-factor halfword lies about the gate size, and the
    // twin-comparison path already prefers the PDB the same way.
    let gate_interval = crate::twin::compare::gate_km(&l3_msg.pdb, rp);
    render_level3_radial_with_gate_km(
        rp,
        gate_interval,
        product,
        radar_lat,
        radar_lon,
        scale,
        offset,
        lut,
        side_ceiling_px,
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
