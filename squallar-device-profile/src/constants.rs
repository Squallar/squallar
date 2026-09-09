use std::time::Duration;

use squallar_radar::render::codes::half_level;

/// Default width for the application window in pixels
pub const RENDER_WIDTH: u32 = 1920;

/// Default height for the application window in pixels
pub const RENDER_HEIGHT: u32 = 1080;

/// **The frame this application is trying to hit**, and the denominator any
/// frame-thread budget in this workspace is a share of.
///
/// 4 ms: p99 interact-frame service, the 250 Hz frame the campaign holds every
/// arm to. It was prose in five doc comments and a constant in none, so every
/// budget that wanted to be a share of a frame had to name its own frame — and
/// four of them named **16.7 ms**, a 60 Hz frame this application has not aimed
/// at since the campaign opened (`squallar_gpu`'s `UPLOAD_BAND_BYTES`,
/// [`MAX_LOOP_SECTION_CUTS_PER_FRAME`], `budget`'s desktop offscreen bracket,
/// and one test-local `FRAME`). A budget sized against the wrong frame reads as
/// headroom rather than as an overrun, and nothing fires.
///
/// # It is DECLARED, not measured, and that is a limitation and not a shorthand
///
/// Nothing in this process has ever asked the display what it runs at. There is
/// no `MonitorHandle::refresh_rate_millihertz`, no `current_monitor` and no
/// `video_modes` call anywhere in the workspace; winit is a dependency of two
/// crates and neither touches the monitor API. The one place a refresh rate is
/// read is `.github/browser-rig/run_measure_native.sh`'s `plat_refresh`, out of
/// process, and it reaches the runner's own vblank-liveness check and never the
/// binary. So a budget may follow **this**; a budget that wants to follow the
/// panel in front of the user needs a monitor read that does not exist yet.
pub const TARGET_FRAME_SERVICE: Duration = Duration::from_millis(4);

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
///
/// So it is that need rounded up to its texture doubling, and the render it
/// admits is the sweep's own 7362 px: the ceiling binds nothing a real sweep
/// asks for. No pane size enters it — a pane-sized raster is under one texel
/// per gate at the ring, and the map can always out-zoom the raster
/// (`the_desktop_raster_ceiling_is_the_widest_sweeps_own_need_and_no_panes`).
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

/// **What one radar picture costs, on each memory that pays for it.**
///
/// A price the budget system *spends*, never one it computes. Every site that
/// wanted a frame's cost used to spell its own arithmetic over a side — four
/// of them spelled `side * side * 4` — and a multiplier written at a use site
/// keeps passing its assertions after the thing it describes has changed
/// shape. The three fields below name the three memories a picture is really
/// held in, so a site reads the axis it is summing onto and cannot silently
/// sum one axis's bytes into another's total.
///
/// The axes are **not** interchangeable and are never added across a boundary:
/// [`Self::gpu`] is a term of `crate::fit::PaneTerms::gpu_bytes`, the two host
/// figures of `host_bytes`. A `Pools::Unified` adapter is the one place their
/// sum is taken, and it is taken by `crate::fit::over`, once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCost {
    /// **Texture bytes on the GPU**, for as long as the picture is held: the
    /// one `Rgba8` texture the renderer's raster uploads as, four bytes a
    /// texel, no mip chain (egui uploads none).
    pub gpu: usize,
    /// **Host bytes held for as long as the picture is held**: the raster as
    /// the display layer keeps it, plus the numbers a readout reads.
    pub host_held: usize,
    /// **Host bytes live only while the render that makes the picture runs**,
    /// and live *at the same instant* as [`Self::host_held`] — not before it
    /// and not after it, so the two add rather than alternate.
    pub host_scratch: usize,
}

impl FrameCost {
    /// The host peak: what the heap is holding at the instant the render that
    /// makes this picture is inside `into_output`, with the scratch buffer and
    /// the finished buffers all alive. The figure a commitment is made
    /// against, rather than the mean over the render.
    pub const fn host_peak(&self) -> usize {
        self.host_held + self.host_scratch
    }
}

/// **Bytes a plan-view radar raster of `side` costs, buffer by buffer.**
///
/// The one statement of the composition. Every budget site takes its price
/// from here rather than multiplying a side by a literal of its own, so a
/// change to what a render allocates lands in one place instead of surviving
/// in four.
///
/// **Its home is beside the buffers, in `squallar_radar::render`.** It is here
/// only because that file is being rewritten to a polar representation — where
/// a frame costs `radials × gates × width × sweeps` and a mip factor, which no
/// expression over a `side` can state at all. This function is the seam that
/// rewrite swaps: give it the polar shape's own inputs, and every site below
/// keeps reading a [`FrameCost`] and needs no edit.
///
/// **The other half of that seam now exists**: [`polar_frame_cost`], over a
/// [`PolarFrameShape`]. **Nothing selects it, and nothing may** until the
/// renderer actually produces polar frames — one surveillance tilt prices
/// ~369× apart under the two, and a budget holding the polar figure while
/// this function's raster is what gets allocated would admit a scene ~369×
/// its own price. See [`polar_frame_cost`] for the constraint in full.
///
/// The terms, each against the allocation that makes it in
/// `squallar_radar::render`:
///
/// | buffer | site | bytes/px | axis |
/// |---|---|---:|---|
/// | `Vec<AtomicU64>` cells | `RenderBuffers::checkout` | 8 | host, scratch |
/// | `Vec<u8>` RGBA | `checkout_image` | 4 | host, held |
/// | the uploaded texture | the display layer | 4 | GPU |
///
/// Both host buffers are alive together inside `RenderBuffers::into_output`:
/// the image is checked out, coloured from the cells, and only then are the
/// cells handed back — so the cells' pages are still charged to the process
/// while the image exists, which is what makes the scratch term add to the
/// held one rather than replace it.
///
/// **There was a third, and it was never read.** Until 2026-09-08 a `Vec<f32>`
/// value grid stood between the cells and the texture at four more bytes a
/// pixel, held. Nothing downstream read it — a hover reads the polar field —
/// so it and this term went together; see `RenderBuffers::into_output`.
pub const fn plan_view_frame_cost(side: usize) -> FrameCost {
    let pixels = side * side;
    FrameCost {
        gpu: pixels * PLAN_VIEW_TEXEL_BYTES,
        host_held: pixels * PLAN_VIEW_TEXEL_BYTES,
        host_scratch: pixels * PLAN_VIEW_CELL_BYTES,
    }
}

/// One RGBA texel, in the raster the renderer writes (`checkout_image`) and in
/// the `Rgba8` texture it uploads as.
pub const PLAN_VIEW_TEXEL_BYTES: usize = 4;

/// One entry of the per-pixel value grid a cross-section readout reads
/// (`squallar_radar::xsect::CrossSection`'s `values`, a `Vec<f32>`). Host
/// only: the numbers are never uploaded.
///
/// **The section's, and the section's alone.** The plan view carried a grid of
/// its own under this name until 2026-09-08, when it was found to be written
/// once and never read; only [`section_frame_cost`] charges this now.
pub const SECTION_VALUE_BYTES: usize = 4;

/// One cell of the claim buffer a render paints into (`RenderBuffers::cells`,
/// a `Vec<AtomicU64>`: a gate key in the high 32 bits, the value in the low).
/// Host only, and live only while a render runs.
pub const PLAN_VIEW_CELL_BYTES: usize = 8;

/// Bytes one raster of `side` costs on the host once it is finished: its RGBA,
/// four bytes a pixel — the held half of [`plan_view_frame_cost`], which
/// states the composition.
///
/// Equal in value to [`converted_raster_bytes`] and deliberately a separate
/// function: this is the renderer's own `Vec<u8>`, that one is the
/// `egui::Color32` buffer built from it, and they are two allocations that
/// happen to cost the same.
pub const fn raster_bytes(side: usize) -> usize {
    plan_view_frame_cost(side).host_held
}

/// **Bytes one finished plan-view raster costs the host once it has been
/// CONVERTED for the renderer**: its `egui::Color32` pixels, four a pixel.
///
/// **A second buffer, not a second reading of
/// [`plan_view_frame_cost`]'s `host_held`.** That term prices what the render
/// itself allocates — the `Vec<u8>` RGBA out of `checkout_image` — which goes
/// back to `squallar_radar::render`'s process-wide slot when the render is done
/// (`recycle_image`), where `render pools` counts it.
/// `RenderDispatcher`'s `plan_view_image` then builds an
/// `egui::ColorImage` **from** those bytes, which is a fresh allocation with
/// a life of its own: it is what the render cache holds, what a pane's cached
/// render holds, what egui is handed and what the upload queue bands — one
/// buffer, four owners, all sharing one `Arc`.
///
/// Four bytes a pixel because `egui::Color32` is four bytes, which is also
/// [`PLAN_VIEW_TEXEL_BYTES`]: the same number as the GPU texture's, and a
/// *different memory*. Spelled through this function rather than as
/// `plan_view_frame_cost(side).gpu` so that no host total is ever summed out
/// of a field whose name says GPU.
pub const fn converted_raster_bytes(side: usize) -> usize {
    side * side * PLAN_VIEW_TEXEL_BYTES
}

/// **The shape of one polar radar frame** — what
/// [`polar_frame_cost`] prices, in the representation's own vocabulary.
///
/// [`plan_view_frame_cost`]'s single `side` cannot state this shape: a polar
/// frame is `radials × gates` cells of `width_bytes`, repeated over `sweeps`,
/// plus a mip chain whose texel count is not `4/3` of the base for a
/// non-square grid. Every field is a *count*, and the arithmetic over them is
/// [`polar_frame_cost`]'s alone, so no call site multiplies a dimension by a
/// literal of its own — the reason `plan_view_frame_cost` exists.
///
/// # Nothing selects this price yet, and that is a safety property
///
/// **A price and a producer must not be able to disagree about which
/// representation a frame is.** This shape prices ~1.76 MB for a surveillance
/// tilt where `plan_view_frame_cost` prices 827 MiB at the side a real arm
/// actually renders one at (7362 px, bound by the sweep's own 1832 gates). If
/// anything selected the polar price while the renderer still produced a
/// 7362 px raster, the admission door would admit a scene that then allocated
/// ~470× what it was priced at — the mechanism of a hard freeze, with the
/// door signing it off. See [`polar_frame_cost`].
///
/// # Provenance of the dimensions
///
/// The dims are read **from the sweep in hand and never from the VCP**
/// (`docs/radar-polar-design.md` §2.1; radials disagree about reach, gate
/// count and spacing — `squallar_radar::render`'s `compute_max_range`,
/// `compute_gate_interval_km`, `compute_gate_span`). The figures below are
/// **observations of what the families produce, not bounds on them**; the
/// bounds are [`MAX_POLAR_RADIALS`] and [`MAX_POLAR_GATES`], and a payload
/// past either is refused rather than priced.
///
/// | family | radials × gates | width | source |
/// |---|---|---|---|
/// | REF / RHO (surveillance) | 720 × 1832 | R8 | design §2.1, observed |
/// | VEL / SW (Doppler) | 720 × 1192 | R8 | design §2.1, observed |
/// | HHC | 360 × 920 | R8 | design §2.1, observed |
/// | Level III radial | run count × `num_range_bins` | R8 | per object |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolarFrameShape {
    /// Radials in the sweep — rows of the code plane. Bounded by
    /// [`MAX_POLAR_RADIALS`].
    pub radials: usize,
    /// Gates along a radial — columns of the code plane, after the reach and
    /// [`MAX_POLAR_GATES`] clamps the design's §2.2 `gates_used` applies.
    pub gates: usize,
    /// Bytes one gate's code occupies: [`POLAR_CODE_R8_BYTES`] today,
    /// [`POLAR_CODE_R16_BYTES`] for the deferred 16-bit widening.
    pub width_bytes: usize,
    /// Sweeps this frame draws. One for a plan view of a single tilt; the
    /// per-sweep terms multiply, the per-sweep-key terms (the LUT) do not.
    pub sweeps: usize,
    /// Levels in the mip chain, **counting level 0**. `1` means `Reduce::None`
    /// — HHC and PHI — and makes [`FrameCost::host_scratch`] exactly zero.
    /// [`full_mip_levels`] answers the full chain for a shape.
    pub mip_levels: usize,
    /// Whether the level-0 codes stay on the host after upload, so a readout
    /// can decode a gate without the pinned `Arc<Scan>`.
    ///
    /// **A per-pane policy, not a build constant.** Design §6.1 spells this
    /// `if CODES_RETAINED`, as if one global const decided it; §6.2 of the
    /// same document requires it **on for a still pane** (a hover reads it —
    /// `squallar_app::render_dispatch`'s two `values_wanted: true` sites) and
    /// **off for a loop frame** (`values_wanted: false`,
    /// `squallar_radar::loop_downloads`'s `frame_render_job`). One const
    /// cannot say both, so it is a field and the caller states which pane it
    /// is pricing.
    pub codes_retained: bool,
}

/// **Bytes one polar radar frame costs, buffer by buffer** —
/// [`plan_view_frame_cost`]'s successor for the representation described in
/// `docs/radar-polar-design.md`, and the seam that document's §6.1 names.
///
/// # NOTHING MAY SELECT THIS PRICE UNTIL THE RENDERER PRODUCES POLAR FRAMES
///
/// This function is **dark**: it is called by its own tests and by nothing on
/// any path that can reach `crate::fit::NeedTerms`. That is a hard safety
/// constraint, not a staging convenience.
///
/// A surveillance tilt prices at **1,758,630 B** here and at **650,388,528 B
/// (620.3 MiB)** under `plan_view_frame_cost(7362)` — the side a desktop arm
/// really renders that sweep at, bound by the data's own 1832 gates and not by
/// [`DESKTOP_RASTER_SIDE_CEILING`]. That is a factor of **369**, and it was
/// 493 against the 867,184,704 B that figure was before the value grid left
/// the render on 2026-09-08. A budget that took the polar figure while the
/// renderer still produced the raster would admit a scene costing ~369× its
/// price, and `crate::admit`'s door would be the thing that signed it off. The
/// ordering is a safety constraint for that reason alone: the two prices
/// differ by a factor of 369, so the door cannot tell a scene it can afford
/// from one it cannot until the producer and the price agree about which
/// representation a frame is.
///
/// So when the switch comes it must be keyed on **what the renderer actually
/// produced for that frame** — not on a build flag, not on a feature gate, not
/// on anything a person can flip for a demo. A price and a producer must not
/// be able to disagree about which representation a frame is.
///
/// # The terms, and why the host pair adds rather than alternates
///
/// [`FrameCost`] keeps its meaning exactly:
///
/// | term | polar buffer | axis |
/// |---|---|---|
/// | [`FrameCost::gpu`] | the `R8Uint` code texture **and its whole mip chain** | GPU |
/// | [`FrameCost::host_held`] | the level-0 codes, when `codes_retained` | host, held |
/// | [`FrameCost::host_scratch`] | the mip tail the CPU builds before upload | host, scratch |
///
/// The scratch term **adds** to the held one for the plan view's own reason:
/// the chain is reduced *from* the level-0 codes, so the codes are still
/// allocated at the instant the deepest level exists. That is the same
/// simultaneity `RenderBuffers::into_output` has, stated over different
/// buffers.
///
/// Under `codes_retained` the peak is therefore exactly the chain:
/// `host_peak() = sweeps × base + sweeps × (chain − base) = sweeps × chain`.
///
/// # `codes_retained: false` does not describe any frame this tree builds
///
/// This paragraph read "with `codes_retained` false — the shipped loop
/// default — `host_held` is 0 and the peak is the scratch alone … they are
/// simply freed after the upload rather than kept". **Nothing frees them, and
/// nothing can.**
///
/// A loop frame's payload is built by `squallar_app::render_dispatch`'s
/// `fan_sweep` as ONE `Vec` of `CodePlane::resident_bytes()` — level 0
/// followed by every mip level — handed to the pane inside an
/// `Arc<FanSweep>`, and it is held for as long as the pane holds the frame.
/// There is no free-after-upload step: `squallar_gpu::radar_fan` uploads only
/// the levels `selected_level` says a draw will read and comes back to the
/// host buffer when the zoom selects another, and its residency is a
/// `Weak<FanSweep>` — so the payload IS the thing that keeps the GPU texture
/// alive.
///
/// So the loop's real shape is `codes_retained` **true at the whole chain**,
/// not at `base`, and it is retained per resident loop frame rather than once.
/// Measured on a HEAVY6 leg (six panes, all playing, 720 × 1832 surveillance
/// cuts): 22 to 34 payloads of 1,758,630 B live at the process peak — 36.9 to
/// 57.0 MiB — which `squallar_egui::heap_census`' `loop frames` family reports
/// as 40,737,002 B on the same leg and prices correctly.
///
/// The `false` arm is kept because it is what the design asks for and what a
/// frame *could* cost; it is not what one costs today. Whoever wires this
/// price up (see the darkness constraint below) must either select `true` for
/// a loop frame or land the release the `false` arm describes first.
///
/// # Not in this figure, because they are per **sweep-key**
///
/// The LUT ([`POLAR_LUT_BYTES`]) and the drawn-edge table
/// ([`polar_drawn_edge_bytes`]) are shared across every frame of a loop that
/// names one `(FieldId, scale, offset)`, so charging them per frame would
/// over-count a loop by its frame count. Design §6.1.
pub const fn polar_frame_cost(shape: PolarFrameShape) -> FrameCost {
    let base = shape.radials * shape.gates * shape.width_bytes;
    let chain = chain_texels(shape.radials, shape.gates, shape.mip_levels) * shape.width_bytes;
    FrameCost {
        gpu: shape.sweeps * chain,
        host_held: if shape.codes_retained {
            shape.sweeps * base
        } else {
            0
        },
        // `chain >= base` for every `mip_levels >= 1`, and `chain == base` at
        // one level, so this never wraps: level 0 is the first term of the sum.
        host_scratch: shape.sweeps * (chain - base),
    }
}

/// **Texels in a `radials × gates` mip chain of `mip_levels` levels**,
/// counting level 0.
///
/// ```text
/// chain_texels(R, G, L) = Σ_{l=0}^{L-1} ceil(R / 2^l) · ceil(G / 2^l)
/// ```
///
/// # The closed form is a bracket, not an equality, and that is the point
///
/// A **square** chain sums to the familiar `4/3`. A `radials × gates` one does
/// not, and no expression free of the ceilings states it: the residual depends
/// on the binary expansions of `R` and `G`. Writing `a_l = ceil(R/2^l) =
/// (R + α_l)/2^l` with `α_l = (−R) mod 2^l ∈ [0, 2^l)`, and likewise `β_l` for
/// `G`, each term is `[R·G + R·β_l + G·α_l + α_l·β_l] / 4^l`. Summing over
/// `l < L` and using `Σ 4^-l = (4/3)(1 − 4^-L)`, `Σ α_l/4^l < Σ 2^-l < 2` and
/// `Σ α_l·β_l/4^l < L`:
///
/// ```text
/// (4/3)·R·G·(1 − 4^-L)  ≤  chain_texels(R, G, L)  <  (4/3)·R·G + 2(R+G) + L
/// ```
///
/// Both sides are pinned by `the_mip_chain_closed_form_brackets_the_long_sum`.
/// Design §6.1 states the upper bound as `+ 2L`; that is true but loose, and
/// `+ L` is what the derivation gives.
///
/// The **exact** identity that does exist is the square power-of-two one: for
/// `R = G = 2^k` over the full chain, `chain = (4^(k+1) − 1)/3`, which is
/// `(4/3)·R·G − 1/3` and shows where the clean factor comes from.
///
/// At the two real surveillance shapes the factor is measured off the sum, not
/// assumed: **1.333265** at 720 × 1832 and **1.333249** at 720 × 1192.
/// (Design §6.1 prints `1.33338` and `1.33344`; both are its ceil-halved
/// chain's factors, and the chain a texture takes is a little smaller.)
///
/// # Which halving, and how it was settled
///
/// Design §2.1 said *"each ceil-halved"*, and this function priced a
/// ceil-halved chain until 2026-09-08 with a note saying the question could
/// not be arbitrated from the code and that the GPU-store lane had to settle
/// it against wgpu. It has been settled, and against that line: a code plane
/// is uploaded as a **texture's own mip chain**, and WebGPU fixes that
/// arithmetic — a level's extent is `max(1, extent >> level)` and a texture
/// may declare `floor(log2(max(w, h))) + 1` levels. A ceil-halved chain is
/// therefore not a chain a texture will take: at 720 × 1832 wgpu answered
/// *"Texture descriptor mip level count 12 is invalid, maximum allowed is
/// 11"*, and clamping the count alone still overran seven of the eleven
/// levels, the first by *"Copy of X 0..115 would end up overrunning the bounds
/// of the Destination texture of X size 114"*.
///
/// So the halving is `squallar_radar::render::codes::half_level`, read from
/// the producer rather than restated here for the reason
/// [`MAX_POLAR_RADIALS`] is: a price computed over a chain shape the encoder
/// does not build is a price for a frame that cannot exist.
pub const fn chain_texels(radials: usize, gates: usize, mip_levels: usize) -> usize {
    // An empty sweep costs nothing, matching `PolarGeometry::is_empty`: a
    // render that painted no gates has no plane to reduce.
    if radials == 0 || gates == 0 {
        return 0;
    }
    let mut sum = 0;
    let (mut r, mut g) = (radials, gates);
    let mut level = 0;
    while level < mip_levels {
        sum += r * g;
        // `half_level` clamps at 1, so a chain asked for more levels than the
        // shape has keeps adding 1×1 rather than collapsing to zero and
        // under-pricing.
        r = half_level(r);
        g = half_level(g);
        level += 1;
    }
    sum
}

/// **Levels in the full chain over `radials × gates`**, counting level 0 — the
/// chain that runs until both extents are 1.
///
/// `floor(log2(max(R, G))) + 1`, computed by halving rather than by a log so
/// the answer and [`chain_texels`]'s own halving cannot disagree. 11 at
/// 720 × 1832 and at 720 × 1192.
///
/// **Design §2.4's `L = floor(log2 max(R,G))` is the line that survives**, and
/// the twelve-term sum it prints in the same section is the slip. This crate
/// read it the other way round until 2026-09-08 — see [`chain_texels`] for
/// what wgpu said about the twelve-term chain.
pub const fn full_mip_levels(radials: usize, gates: usize) -> usize {
    let (mut r, mut g) = (radials, gates);
    let mut levels = 1;
    while r > 1 || g > 1 {
        r = half_level(r);
        g = half_level(g);
        levels += 1;
    }
    levels
}

/// One gate's code in the 8-bit plane every family migrated in design §7's
/// phases A–C carries: REF, VEL, SW, RHO, ZDR-8, HHC and the five Level III
/// radial products.
pub const POLAR_CODE_R8_BYTES: usize = 1;

/// One gate's code in the **deferred** 16-bit widening — PHI and 16-bit ZDR,
/// design §7 phase F. Named here because [`PolarFrameShape::width_bytes`] is
/// the only place the widening lands, and a price for it should not have to be
/// invented at the call site when it does.
pub const POLAR_CODE_R16_BYTES: usize = 2;

/// Entries in the colour lookup an 8-bit code plane is decoded through: every
/// code an R8 plane can hold, so the table is **exact** rather than sampled.
pub const POLAR_LUT_ENTRIES: usize = 256;

/// **Bytes one LUT costs**, on the host and on the GPU alike: 256 RGBA8
/// entries.
///
/// **Per sweep-key — `(FieldId, scale, offset)` — and NOT per frame.** Every
/// frame of a loop rendering one moment shares one table, so a per-frame
/// charge would over-count a 60-frame loop sixtyfold. It is deliberately
/// absent from [`polar_frame_cost`]; design §6.1.
pub const POLAR_LUT_BYTES: usize = POLAR_LUT_ENTRIES * PLAN_VIEW_TEXEL_BYTES;

/// **Bytes the drawn-edge table costs for `radials` radials**, host and GPU:
/// each radial's trimmed `(lo, hi)` azimuth as two `f32`.
///
/// Per sweep-key like [`POLAR_LUT_BYTES`], and absent from
/// [`polar_frame_cost`] for the same reason. 5,760 B at 720 radials.
///
/// **The successor to `PolarGeometry::resident_bytes`**, which is the same
/// arithmetic over the same count today — `wedges.len() × size_of::<Wedge>()`,
/// and a `Wedge` is two `f32`. `squallar_radar::hover`'s `HoverSource` calls
/// that *"5.8 KiB for a full ring"*: 720 × 8 = 5,760 B is **5.625 KiB**, or
/// 5.76 kB decimal, so that prose has swapped a decimal prefix for a binary
/// one. Correcting it belongs to the lane that owns `hover.rs`.
pub const fn polar_drawn_edge_bytes(radials: usize) -> usize {
    radials * POLAR_DRAWN_EDGE_BYTES
}

/// One radial's drawn `(lo, hi)` azimuth pair, two `f32`.
pub const POLAR_DRAWN_EDGE_BYTES: usize = 2 * size_of::<f32>();

/// **The most radials a polar frame may declare**, past which the payload is
/// refused rather than priced or truncated.
///
/// A **bound**, and derived rather than observed: twice the 720 the RDA can
/// declare, because *"Level II declares 0.5° or 1.0° and nothing else, the RDA
/// has no third resolution"* (`squallar_radar::render`). Design §2.2. It plays
/// [`squallar_radar::types::MAX_EXTENT_KM`]'s role for the other axis — a
/// ceiling on arithmetic, so a mis-framed radial claiming four thousand
/// radials cannot size an allocation.
///
/// **Read from the producer, not restated here.** `CodePlane::build` is what
/// refuses a payload past this bound; a second copy of the number in the
/// pricing crate could drift from the one the encoder enforces, and a price
/// that admitted a shape the producer refuses would be pricing a frame that
/// cannot exist.
pub const MAX_POLAR_RADIALS: usize = squallar_radar::render::codes::MAX_POLAR_RADIALS;

/// **The most gates a polar frame may declare**, past which the payload is
/// refused.
///
/// A **bound**, and the WebGL2 per-axis guarantee verbatim: the code plane is
/// a texture, and `squallar_gpu`'s device setup pins
/// `downlevel_webgl2_defaults().using_resolution(adapter)` on the web, which
/// lifts resolution and nothing else. Design §2.2.
pub const MAX_POLAR_GATES: usize = squallar_radar::render::codes::MAX_POLAR_GATES;

/// Invariants of the polar pricing above, checked at compile time. Each one
/// would make the arithmetic silently wrong rather than loudly broken.
const _: () = const {
    // The LUT is exact only while it has one entry per code an R8 plane can
    // address. Widening the code without widening the table would silently
    // sample the palette instead of reproducing it.
    assert!(POLAR_LUT_ENTRIES == 1 << (8 * POLAR_CODE_R8_BYTES));
    // The LUT entry is RGBA8, which is the raster texel's own width. This
    // const is built out of `PLAN_VIEW_TEXEL_BYTES`, so if that ever stops
    // meaning four bytes of colour the table silently re-sizes.
    assert!(PLAN_VIEW_TEXEL_BYTES == 4);
    assert!(POLAR_LUT_BYTES == 1024);
    // Two `f32`, not a packed pair and not a `f64` one: the drawn-edge table
    // is uploaded as-is.
    assert!(POLAR_DRAWN_EDGE_BYTES == 8);
    assert!(polar_drawn_edge_bytes(720) == 5_760);
    // Codes 0 and 1 are sentinels (below-threshold and range-folded) and the
    // reduce operators exclude them, so a width that could not hold both plus
    // one real code would make every mip level a sentinel.
    assert!(POLAR_LUT_ENTRIES > 2);
    assert!(POLAR_CODE_R16_BYTES > POLAR_CODE_R8_BYTES);

    // Level 0 is the first term, so one level is the base plane exactly and
    // `polar_frame_cost`'s `chain - base` cannot wrap.
    assert!(chain_texels(720, 1832, 1) == 720 * 1832);
    assert!(chain_texels(1, 1, 1) == 1);
    // The two real surveillance shapes, off the sum rather than off a factor.
    assert!(chain_texels(720, 1832, full_mip_levels(720, 1832)) == 1_758_630);
    assert!(chain_texels(720, 1192, full_mip_levels(720, 1192)) == 1_144_248);
    assert!(full_mip_levels(720, 1832) == 11);
    // A chain asked for more levels than the shape has keeps adding 1x1 rather
    // than collapsing: an over-long request must never price at less than the
    // full chain.
    assert!(chain_texels(720, 1832, 40) == 1_758_630 + (40 - 11));

    // The caps bound arithmetic, and the observed shapes sit inside them --
    // an observed maximum is not a bound, and these are the bounds.
    assert!(MAX_POLAR_RADIALS >= 720);
    assert!(MAX_POLAR_GATES >= 1832);
    assert!(MAX_POLAR_GATES == 2048);
    // The widest frame the caps admit must not overflow a 32-bit `usize`,
    // because these constants compile on wasm32 too. R16, 2 sweeps, full chain.
    assert!(MAX_POLAR_RADIALS * MAX_POLAR_GATES * POLAR_CODE_R16_BYTES < (u32::MAX as usize) / 8);
};

/// **Bytes a cross-section raster of `width × height` costs, buffer by
/// buffer** — [`plan_view_frame_cost`]'s counterpart for the other 2D view,
/// and stated here for the same reason.
///
/// Its buffers are the three fields `squallar_radar::xsect::CrossSection`
/// holds, all `width × height` and all kept for the life of the section:
///
/// | buffer | bytes/px | axis |
/// |---|---:|---|
/// | `image`, RGBA8 | 4 | host, held (and the GPU texture) |
/// | `values`, `f32` in the product's own unit | 4 | host, held |
/// | `status`, one `SampleStatus::wire_code` | 1 | host, held |
///
/// **No scratch term, and that is a difference from the plan view, not an
/// omission**: the section sampler writes its three buffers directly and
/// allocates no claim buffer, so there is nothing here answering to
/// [`PLAN_VIEW_CELL_BYTES`].
pub const fn section_frame_cost(width: usize, height: usize) -> FrameCost {
    let pixels = width * height;
    FrameCost {
        gpu: pixels * PLAN_VIEW_TEXEL_BYTES,
        host_held: pixels * (PLAN_VIEW_TEXEL_BYTES + SECTION_VALUE_BYTES + SECTION_STATUS_BYTES),
        host_scratch: 0,
    }
}

/// One pixel's sample-status code in a cross-section
/// (`squallar_radar::xsect::CrossSection`'s `status`, a `Vec<u8>`). Host only.
pub const SECTION_STATUS_BYTES: usize = 1;

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

/// **The largest wasm linear memory this build can ever be given**, in bytes:
/// the `--max-memory` the module is linked with
/// (`.github/scripts/wasm-threads.sh`), which is why its memory section
/// declares the `maximum=16384 pages` quoted above.
///
/// **A validation bound, not a device reading and no longer the wall itself.**
/// A `shared` memory has to state a maximum at link time because it cannot be
/// relocated on growth, so the module declares one — but that declaration is
/// what a *supplied* memory is matched against, and the match permits
/// shrinking: a memory whose maximum is at or below this instantiates, one
/// above it raises `LinkError: imported Memory with incompatible maximum
/// size` (54 cells plus negative controls on Firefox and Chromium,
/// 2026-09-03). So `squallar-web/heap.js` chooses per device *underneath* this
/// figure before the module is instantiated, a desktop gets exactly this and a
/// handheld gets less, and the choice reaches the application as a value on
/// `budget::DeviceProfile::linear_memory_max_bytes` because nothing can read a
/// memory's maximum back — `WebAssembly.Memory.prototype.type()` exists in
/// neither engine.
///
/// The page and the rasterization worker are two module instances with two
/// memories, **and since the choice is made per instance the two ceilings need
/// not be equal** (on a handheld they are not). Neither the readings nor the
/// walls are ever added.
///
/// Held equal to the link flag, and held above every per-device figure, by
/// `squallar-web/tests/linear_memory_ceiling.rs`, which reads both the script
/// and `heap.js`. What a reading against a ceiling means is
/// [`crate::linear_memory`].
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

/// **The three loop-cache budgets, per arm, sized against a reproduced death.**
///
/// # Provenance: a user's browser froze, and the rig reproduced it byte for byte
///
/// Web build, MRMS and GMGSI on, a loop playing. An infallible allocation at
/// the 1 GiB wasm page ceiling — `alloc failed: 2384 B requested, 1024 of 1024
/// MiB linear in page` — aborting inside the `requestAnimationFrame` callback
/// with winit's runner `RefCell` mutably borrowed, so the frame loop stops ten
/// to thirteen seconds after boot while rAF stays healthy and the canvas holds
/// its last frame. Identical on the previous night's tip: not a regression.
///
/// The census at death, read from the rig's own json — the durable copy at
/// `/home/reddragon/.cache/rd-perf-webfreeze-repro/wf-rig-out-main-lw/`,
/// `chromium.long.json`, the last `heap census` line before the trap, with
/// `frame_lines.budget_state.linear_page_mib` reading 1024 in the same file:
///
/// ```text
/// resident total   952,242,007 B   908.1 MiB   of a 1024 MiB page
/// loop scans       625,178,880 B   596.2 MiB
/// overlay grids    219,773,456 B   209.6 MiB
/// still scans       62,017,120 B    59.1 MiB
/// everything else                    43.2 MiB
/// ```
///
/// That leg's `loop_or_refusal.resident` reads 18 and the firefox leg's reads
/// 14 (`WASM_MAX_LOOP_FRAMES`), so the per-frame figure is 33-43 MiB depending
/// on which count is the denominator; the sizing below uses the TOTAL and
/// does not depend on the split.
///
/// The firefox leg died at 828.6 MiB with 447.2 MiB of loop scans and **122.2
/// MiB** of everything else, so the non-loop residue is 312-381 MiB across the
/// two browsers. **The sizing below uses 381, the worse of the two.**
///
/// The loop's decoded volumes were 66 % of the page. What the loop needs is
/// one sweep's moments per textured frame (1,359,376 B, a constant of the
/// instrument — see `hover::SweepGates`), the compressed archive of each frame
/// so an evicted volume is a decode away rather than a download, and a bounded
/// number of volumes decoded at any one moment. Three budgets, one per term.
///
/// # Why each is BYTES and not a frame count
///
/// Over 39 real archive volumes the compressed form spans 1,023,254 to
/// 16,895,202 B — a 16.5x swing — and the decoded form spans 33.7 to 82.7 MiB,
/// while the frame count a loop holds does not move. A frame-count ceiling
/// sized for the median is 2.9x over budget on the largest archive: fourteen
/// frames at the maximum is 225.5 MiB of archives alone. Bound the quantity
/// that varies.
///
/// # Why each is an ADMISSION ceiling and not only an eviction one
///
/// A retarget blanks every frame's texture at once. A policy that keeps a
/// volume decoded while its frame has no texture would then admit all fourteen
/// volumes together — the death scene, rebuilt by the fix. So the decoded
/// ceiling is enforced where a decode is DISPATCHED (the download pump and the
/// download arrival), not only where a volume is evicted after its texture
/// lands. Over the ceiling, a frame waits compressed; it does not allocate and
/// die.
///
/// # The wasm arithmetic, against the worse leg
///
/// ```text
/// page                                   1024 MiB
/// non-loop residue, firefox leg         - 381
/// decoded ceiling (2 x 74.6 MiB max V)  - 160
/// archive ceiling                       - 128
/// 14 sweeps x 1.30 MiB                  -  18
///                                       ------
/// short of the wall by                    337 MiB
/// ```
///
/// 337 MiB is the margin for a second overlay family the size of the grids
/// (210 MiB) with room left over. It is a stated margin against a measured
/// scene, and the test beside these constants asserts it from the same
/// figures, so a change to any one term moves the assertion.
///
/// An archive ceiling of 128 MiB holds fourteen archives at both corpus
/// medians (40 and 78 MiB) and trims fourteen at the corpus maximum (226 MiB)
/// to the nearest seven, which costs those seven a re-download if wanted —
/// what every frame costs today.
///
/// # The arrival rate does not defeat this, and the reason is admission, not
/// # eviction
///
/// The reproduction's console, at two-second ticks: loop scans 0, 0, 0, 99.4,
/// 397.5, 596.2 MiB — decoded volumes arriving at ~100 MiB/s (149 over one
/// tick) onto a page already past 600. Two bounds hold that, and the stronger
/// is the first:
///
/// 1. A download landing with the decoded set full is filed COMPRESSED at
///    arrival (`App::take_loop_frame_archive` asks `decoded_room_for` before it
///    decodes). Decoded bytes therefore never exceed the ceiling committed, at
///    any arrival rate, whether or not a sweep ever ran.
/// 2. Archives then arrive at ~13 MiB/s at the corpus-median size and ~38 at
///    the maximum (100 MiB/s ÷ 42.6 MiB per volume × C). The archive ceiling is
///    enforced by `App::evict_unshown_scans`, called from `handle_redraw` —
///    the per-frame path — so the overshoot between two sweeps at 60 Hz is
///    under 0.7 MiB at the worst rate, against a ceiling that takes 3.4 s to
///    fill. A policy that evicted on the next LISTING would not hold; one on
///    the frame does.
///
/// # What is still PROVISIONAL, and what replaces it
///
/// The decoded ceiling's real driver is decode latency against playback
/// cadence, and that latency is UNMEASURED: the harness exists
/// (`squallar-radar/tests/mem_loop_decode_cost.rs`) and has never been run.
/// The archive ceiling's right shape is a per-site observed archive peak that
/// only rises, the archive twin of `LoopDownloadManager::site_scan_peak`. Both
/// figures below are the honest bootstrap for that measurement, not its result.
#[cfg(target_arch = "wasm32")]
pub const LOOP_DECODED_CEILING_BYTES: usize = WASM_LOOP_DECODED_CEILING_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_DECODED_CEILING_BYTES: usize = MOBILE_LOOP_DECODED_CEILING_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_DECODED_CEILING_BYTES: usize = DESKTOP_LOOP_DECODED_CEILING_BYTES;

/// Two volumes at the corpus maximum of 74.63 MiB, or three at the death
/// scene's 42.6. The pipeline depth of the initial fill and of a retarget.
pub const WASM_LOOP_DECODED_CEILING_BYTES: usize = 160 * 1024 * 1024;
pub const MOBILE_LOOP_DECODED_CEILING_BYTES: usize = 320 * 1024 * 1024;
/// Held to the campaign's 250 MiB process target rather than to the box: a
/// 25-stamp desktop loop was 1,222 MiB of decoded volumes before this.
pub const DESKTOP_LOOP_DECODED_CEILING_BYTES: usize = 256 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
pub const LOOP_ARCHIVE_CEILING_BYTES: usize = WASM_LOOP_ARCHIVE_CEILING_BYTES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_ARCHIVE_CEILING_BYTES: usize = MOBILE_LOOP_ARCHIVE_CEILING_BYTES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_ARCHIVE_CEILING_BYTES: usize = DESKTOP_LOOP_ARCHIVE_CEILING_BYTES;

pub const WASM_LOOP_ARCHIVE_CEILING_BYTES: usize = 128 * 1024 * 1024;
pub const MOBILE_LOOP_ARCHIVE_CEILING_BYTES: usize = 192 * 1024 * 1024;
pub const DESKTOP_LOOP_ARCHIVE_CEILING_BYTES: usize = 256 * 1024 * 1024;

/// **Which frames keep their DECODED volume beyond the ones with no texture
/// yet**: `None` keeps none — a textured frame's volume goes the moment its
/// texture lands — and `Some(n)` keeps the playhead's and the `n` ahead of it
/// in the direction of travel, so a retarget's first frames appear without a
/// decode.
///
/// `None` on wasm, because the death above is what a spare 42-75 MiB volume
/// costs on a 1 GiB page carrying 381 MiB of other families, and a retarget
/// that waits one decode is a wait and not a wall. `Some(0)` on mobile keeps
/// the playhead alone. `Some(2)` on desktop is PROVISIONAL: the right value is
/// `ceil(decode_latency / frame_interval)` and the latency is unmeasured (see
/// above). Do not inherit the campaign's 250-300 MiB/s-per-lane figure for it;
/// that came from an eight-lane arm under contention.
#[cfg(target_arch = "wasm32")]
pub const LOOP_DECODED_LOOKAHEAD_FRAMES: Option<usize> = WASM_LOOP_DECODED_LOOKAHEAD_FRAMES;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const LOOP_DECODED_LOOKAHEAD_FRAMES: Option<usize> = MOBILE_LOOP_DECODED_LOOKAHEAD_FRAMES;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const LOOP_DECODED_LOOKAHEAD_FRAMES: Option<usize> = DESKTOP_LOOP_DECODED_LOOKAHEAD_FRAMES;

pub const WASM_LOOP_DECODED_LOOKAHEAD_FRAMES: Option<usize> = None;
pub const MOBILE_LOOP_DECODED_LOOKAHEAD_FRAMES: Option<usize> = Some(0);
pub const DESKTOP_LOOP_DECODED_LOOKAHEAD_FRAMES: Option<usize> = Some(2);

/// **The census at death, as this crate's own record of the scene the wasm
/// budgets above were sized against** — bytes, from the rig json, so the test
/// below can assert the margin from the same figures the doc quotes.
pub mod web_freeze_2026_09_07 {
    /// `resident total`, chromium leg.
    pub const RESIDENT_TOTAL_CHROMIUM: u64 = 952_242_007;
    /// `loop scans`, chromium leg.
    pub const LOOP_SCANS_CHROMIUM: u64 = 625_178_880;
    /// `resident total`, firefox leg.
    pub const RESIDENT_TOTAL_FIREFOX: u64 = 868_884_686;
    /// `loop scans`, firefox leg.
    pub const LOOP_SCANS_FIREFOX: u64 = 468_884_160;
    /// `overlay grids`, identical on both legs.
    pub const OVERLAY_GRIDS: u64 = 219_773_456;
    /// Frames the wasm loop holds: `WASM_MAX_LOOP_FRAMES`. The firefox leg's
    /// `loop_or_refusal.resident` read exactly this; chromium's read 18.
    pub const FRAMES: u64 = 14;
    /// One frame's extracted sweep, `hover::SweepGates::scan_bytes()`,
    /// constant across all 39 measured volumes.
    pub const SWEEP_BYTES: u64 = 1_359_376;
    /// The page.
    pub const PAGE_BYTES: u64 = 1024 * 1024 * 1024;
    /// The margin the sizing claims. A stated figure, not headroom by
    /// accident.
    pub const CLAIMED_MARGIN_BYTES: u64 = 320 * 1024 * 1024;
}

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

/// **Host bytes reserved per radar loop frame whose decoded Level II volume
/// has not arrived yet** — what `crate::fit::NeedTerms::loop_scans_host`
/// prices a pending frame at; a frame the download cache already holds is
/// priced at the size it was measured at on arrival
/// (`PaneNeed::loop_scans_resident_bytes`), never at this.
///
/// A reservation, not a measurement. The real decode path was measured under a
/// counting global allocator over **208 real archive volumes** at a 48.88 MiB
/// median and a **74.63 MiB maximum** (`squallar_app::volume_inventory`, which
/// carries the corpus, the instrument and the correction below). 80 MiB covers
/// that maximum with 5.37 MiB to spare and over-states a median volume by 64 %
/// (80 ÷ 48.88 = 1.637), so it over-prices only what has not arrived — the
/// direction a reservation must err. The ladder answers the term with the host
/// rungs alone: the span is the user's and no host rung shortens it.
///
/// **This is a bootstrap, not a ceiling.** What stood here before — that the
/// reserve "under-states nothing measured" — was a claim about a sample worn
/// as a bound, and it stopped being true the moment the corpus was
/// re-measured: 58.3 MiB, the figure 64 MiB had been rounded up from, turned
/// out to be the **70.7th percentile** of those same 208 volumes rather than
/// their maximum, exceeded by 61 of them. The lesson is not that the number
/// was stale. It is that a maximum drawn from a non-random corpus was never a
/// bound: this one sampled two hours of the day (06Z and 18Z) and lost one of
/// four planned seasons to a silent fetch failure, so no re-measurement of it
/// can promote this constant to a ceiling. Starting the count honestly is all
/// this figure is for.
///
/// The reserve is due to become the **floor** under a per-site self-calibrating
/// one — `max(bootstrap, that site's resident maximum)` — because a site that
/// has already handed this process a volume larger than 80 MiB is evidence
/// about that site which no corpus percentile outranks. Until that lands, this
/// bootstrap is the whole reserve; afterwards it is still the whole reserve for
/// the first frame at any site, which is the case that has no measurement yet.
///
/// **The power-of-two rounding is gone, deliberately.** 64 MiB was "58.3
/// rounded up to the next power of two" — a convention with no reason under
/// it, and the rounding is what hid how little headroom there really was.
/// Carried forward onto the corrected maximum it would give 128 MiB, which
/// over-prices an eleven-frame loop by 11 × (128 − 80) MiB = **528 MiB** of
/// bytes nobody allocates, and the ladder pays that in shed rungs — picture
/// quality a user can see — for a quantity that does not exist. The rule is
/// now a clean figure just above the measured maximum.
pub const LOOP_SCAN_RESERVE_BYTES: u64 = 80 * 1024 * 1024;

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

/// **How many BYTES of whole-picture overlay raster the whole application may
/// have outstanding at once** — dispatched and not yet arrived, plus arrived
/// and not yet delivered to the GPU.
///
/// # The quantity nothing bounded
///
/// `squallar_egui::overlay_cache::RendersInFlight` bounds one pane and layer,
/// and its own type note says what that leaves open: "the aggregate is `panes
/// x texture layers x budget x plan bytes` — **which the budget alone does not
/// bound**". The shown layers re-rasterize *together* on every map move, so the
/// unbounded quantity is a whole batch, and it is resident twice over on its
/// way to the card: as an `Arc<egui::ColorImage>` between the rasterizer and
/// the frame thread (`overlay replies`), and then as one band queue entry per
/// picture in `squallar_gpu`'s `TextureUploads::pending` (`upload pending`),
/// which holds each picture **whole** until its last band has crossed.
///
/// # Bytes, because a count of pictures is not a quantity of memory
///
/// This was a count of four until 2026-09-09, and a count bounds the wrong
/// thing: one outstanding picture is `1.5 x 1.5` viewports at 4 bytes a texel
/// ([`OVERLAY_OVERSAMPLE_PERCENTS`]`[0]`, taken by
/// `squallar_egui::overlay_cache::plan_overlay_texture`), so what four of them
/// cost is a property of the **canvas** and not of the door:
///
/// | canvas | one picture | four |
/// |---|---|---|
/// | 1920x1080 | 18,662,400 B | 71.2 MiB |
/// | 2878x1651 | 42,755,568 B | 163.1 MiB |
/// | 3440x1440 | 44,582,400 B | 170.1 MiB |
///
/// A ceiling sized for the median is over budget on the largest display by the
/// square of the side, which is the argument this workspace already made for
/// loop frames. **Bound the quantity.**
///
/// The count's own note argued the other way — "a byte budget would re-derive
/// the plan here, and `budgets that silently re-derive` is the defect" — and
/// that objection does not survive contact with the door's site. **Nothing is
/// re-derived.** The bytes charged are the ones the planner already produced:
/// `squallar_egui::overlay_cache::OverlayTexturePlan::bytes` for a dispatch,
/// and the arrived picture's own `width x height` for a hold.
/// The door spends the figure the plan carries; this constant is the only new
/// number.
///
/// # Twelve bands, and why not eight
///
/// The band queue drains one [`BLOCKING_BAND_BYTES`] band a frame on every
/// device without a staging ring — which is every browser — so what the pipe
/// must hold is **drain, measured in frames**, not pictures. A whole-picture
/// rasterize is 133 ms median, which is 8 frames at 60 Hz, so a pipe holding
/// fewer than 8 frames of drain runs dry before the picture behind it can
/// arrive: the drain idles, and a batch that used to reach the glass in
/// slices starts reaching it in slices *with gaps between them*. Eight bands
/// is that floor exactly and leaves nothing for the frame the refusal latch
/// costs; twelve is the floor and half again.
///
/// **Written as a literal and pinned by a test, not as arithmetic over
/// [`BLOCKING_BAND_BYTES`].** A budget spelled as a product of other constants
/// re-derives silently when one of its terms moves — the defect that produced
/// the 7.6 ms blocking allowance nobody chose in `squallar_gpu`'s
/// `whole_budget`. `tests::the_outstanding_ceiling_is_twelve_bands` is the
/// coupling made loud instead.
///
/// # What it affords, and the floor it may never cross
///
/// The door admits while the pipe is **below** this line, so the pipe can
/// overshoot by at most one picture and can never be short of one: at zero
/// outstanding every canvas affords its first picture whatever its size.
/// A ceiling that could afford *none* is the failure mode a prior door's
/// vacuity check found — the one value that passes every gate while idling the
/// handover — and `overlay_dispatch_budget_tests` gates the stronger property,
/// that **every canvas the ladder can plan affords at least two**, which is
/// what keeps a picture queued behind the one draining:
///
/// | canvas | afforded | outstanding | was |
/// |---|---|---|---|
/// | 1920x1080 | 3 | 53.4 MiB | 71.2 MiB |
/// | 2878x1651 | 2 | 81.6 MiB | 163.1 MiB |
/// | 3440x1440 | 2 | 85.0 MiB | 170.1 MiB |
///
/// # What a refusal costs, and why it cannot lose a raster
///
/// The door is the draw pass's, beside the per-cache one, and a refusal there
/// latches `overlay_work_owed` — the `request_once` retry that exists because
/// "a dispatch that was REFUSED has no arrival to wait for and nothing else
/// would ever re-ask". So the layer asks again on the next frame and every
/// frame after until it is admitted. Nothing is dropped and no affordance
/// stops working; what changes is that the layers of a batch reach the glass
/// in sequence rather than all at the end of one, and the total is unchanged
/// because the drain never moved faster than one band a frame either way.
/// Re-denominating the door in bytes changes the size of a slice, not whether
/// slicing happens.
///
/// # The ordering it does not promise, stated rather than left to be found
///
/// A pane REPLACING a picture it is already uploading is charged nothing —
/// the supersede is net zero on the pipe — so under a pan long enough to
/// outlast a picture's drain the layers already in the pipe can keep taking
/// their own slots back while a layer with nothing in it waits. Nothing here
/// rotates the walk, and the brake that ends it is
/// `OverlayTextureCache::sweep_discarded`, which answers `needs_rerender`
/// false for a pane that has demonstrated it discards uploads. **What the
/// waiting layer shows meanwhile is the picture it already had**, which is
/// what it showed before this door existed too: the batch's last picture was
/// 135 frames of draining behind the move either way. The door changes which
/// layers are early, not whether any is served.
pub const MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING: u64 = 50_331_648;

/// How many whole **plan-view radar pictures** the application may have
/// outstanding at once — dispatched to the renderer and not yet arrived, plus
/// uploaded and not yet delivered to the GPU.
///
/// # The same quantity as [`MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING`], one producer over
///
/// That door is the draw pass's and is asked per layer per pane, so it reaches
/// the overlay rasters and nothing else — its own note says radar is left out
/// "because its rasters come from `App::dispatch_pane_renders`, not from the
/// overlay door this figure feeds". This is that producer's door. The two are
/// separate figures rather than one shared allowance for the reason that note
/// gives: charging radar to the overlay door lets a playing loop close it
/// against every other layer, and a single allowance would put the two
/// producers back in one another's way.
///
/// # What is unbounded without it
///
/// A plan-view picture is `side² × 4` and `side` is the *adapter's*, not the
/// pane's — [`Budgets::raster_side_for_adapter`](crate::budget::Budgets::raster_side_for_adapter)
/// holds it into `[long_range_image_side_px, raster_side_ceiling_px]` — so the
/// desktop bracket reaches 8192 px, 268,435,456 B a picture, and a measured
/// WSR-88D surveillance cut takes 7362 px, 216,796,176 B. `squallar_gpu`'s
/// band queue holds each of them **whole** until its last band crosses, and it
/// moves [`BLOCKING_BAND_BYTES`] a frame on every device with no staging ring
/// and two 8 MiB bands a frame on one that has, breaking after any frame on
/// which it allocated a texture. So a picture occupies the queue for
/// `ceil(bytes / band) ` frames and the queue works through them strictly
/// oldest-first, one picture at a time.
///
/// Nothing counted them. `RenderDispatcher::render_slot_free` bounds the
/// renders *in flight* at [`MAX_CONCURRENT_RENDERS`] — 6 on desktop, 3 on
/// mobile, 1 on the web — and says nothing about a picture that has already
/// arrived and is sitting in the queue, so the outstanding total was
/// `concurrent_renders + (pictures the queue has not finished)`, whose second
/// term had no ceiling at all. On a resume that is the whole batch: the
/// `squallar_gpu` `texture_upload` module note prices a surveillance cut at
/// "7362 px, once per distinct raster per volume, **six panes at a time on a
/// resume**".
///
/// # Three, and why a refusal cannot cost an arrival
///
/// One picture being drained, one already queued behind it so the drain never
/// idles at the handover, and one in flight from the renderer to replace the
/// one that just left. That is the pipeline this bounds, and the third slot is
/// the margin: the door only has to keep the *queue* fed, because the queue is
/// what serialises the batch.
///
/// **A refusal cannot delay a picture that the queue would have reached
/// sooner.** The queue is strictly oldest-first and holds every picture whole,
/// so picture `k` of a batch cannot begin before picture `k-1` has finished
/// whatever the producer did; a picture dispatched past what the queue can
/// absorb does not arrive earlier, it waits in the queue instead of waiting to
/// be asked for, and pays a whole picture of host memory for the difference.
/// That is [`MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING`]'s argument, and it holds here
/// on the same condition — that the producer is faster than the drain — which
/// at boot is not an assumption but the **reading**: the batch was measured
/// with four to five whole pictures resident in the queue at once, and a
/// producer slower than the drain cannot put two there.
///
/// **It is a count and not a byte budget, and the reason it used to give for
/// that is gone.** Until 2026-09-09 this paragraph read "for
/// `MAX_OVERLAY_PICTURES_OUTSTANDING`'s reason", and that door is now
/// [`MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING`] — bytes, because a count of
/// pictures is a property of the canvas and not of the door. What is left
/// here is the narrower fact that door's re-denomination turned on: the
/// overlay door is asked where the pane's own plan is already in hand, so it
/// charges a figure the planner produced, while this one is asked in
/// `App::dispatch_pane_renders` off the *adapter's* side. Re-denominating
/// this door means putting that side on the wire; it is its own change with
/// its own measurement, not a side effect of the overlay one.
///
/// # What a refusal costs, and why nothing is dropped
///
/// The door is asked in `App::dispatch_pane_renders`, which runs on every
/// frame, and a refusal writes nothing: `PaneRenderState::last_rendered` is
/// left where it was, so `needs_render` is still true and the same pane asks
/// again on the next frame and every frame after until it is admitted. No
/// raster is discarded and no pane is retired.
///
/// **A refusal cannot strand the app on a frame nobody asks for**, which is
/// the one way a door on a `ControlFlow::Wait` loop could deadlock. The door
/// closes only when the outstanding total has reached this figure, and every
/// unit of that total is itself a wake: a render in flight wakes the loop when
/// it arrives, and a picture the queue has not delivered keeps
/// `TextureUploads::uploads_pending` true, which is what buys the next frame.
/// A closed door therefore always has an event coming that will reopen it.
///
/// # What the total counts, and what it does not charge
///
/// **The DISTINCT whole pictures in the queue**, which is
/// `TextureUploads::publish_pending_level`'s own denominator, restricted to
/// radar. `Gui::plan_view_pictures_outstanding` counted *panes holding a
/// raster* until 2026-09-08 and that was a different set in both directions;
/// both halves were measured over on the post-door tree, at 4.40 / 4.29 /
/// 6.20 pictures resident against this ceiling of 3.
///
/// * A pane's FIRST picture is SHOWN rather than held —
///   `PaneState::place_radar_raster` has nothing on the glass to protect, so
///   the raster goes up and fills top-down as its bands land. It is whole in
///   the queue the entire time and was charged nothing, so on a resume, the
///   batch this door exists for, the occupancy term was structurally zero for
///   the whole burst and this reduced to a bound on the renders in flight.
///   `OverlayTextureCache::showing_arriving` is the missing half.
/// * A picture shared by sibling panes was charged once PER PANE, where the
///   queue holds one `Arc` and prices it once — closing the door against
///   traffic that was never in the pipe.
///
/// **A pane served out of the shared `RenderCache` is not refused, and it is
/// not free either.** These are two questions and they were one. It is not
/// refused because its picture is an `Arc` the cache is already holding, so
/// the upload files a refcount and no host bytes — the `upload pending`
/// census family's own note says it "OVERLAPS the two raster families while a
/// radar raster is in flight: the `Arc` it holds is the one
/// `apply_render_to_pane` handed `ctx.load_texture`, which is the same `Arc`
/// `render cache` and `cached renders` hold" — and refusing it would cost a
/// paint to free nothing. But that upload IS a queue entry, sitting ahead of
/// pictures that do cost host bytes and extending how long those are
/// resident, so `App::dispatch_pane_renders` charges it against every pane
/// the same walk visits after it. `a_cache_hit_files_the_buffer_the_cache_is_
/// already_holding` proves the no-host-bytes half rather than asserting it.
///
/// **A sibling pane waiting on a picture already being made is charged
/// nothing**, and that one holds: it uploads nothing at the door, and when
/// the picture arrives `poll_render_results` hands every sibling the same
/// `egui::TextureHandle` out of one `PlanViewUploads`, which is one entry in
/// the queue and one charge here.
///
/// A loop frame and a cross-section cut are not charged either: they are not
/// plan views, they do not set `PaneRenderState::in_flight_plan_view`, and
/// their pictures are a twentieth of one of these.
pub const MAX_PLAN_VIEW_PICTURES_OUTSTANDING: usize = 3;

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

/// How long one frame may spend applying arrivals before the rest wait for the
/// next one.
///
/// The arrival drains of [`PumpPhase::Ingest`] are `while let Ok(..) =
/// try_recv()` loops with no bound, so a frame that happens to follow a burst
/// applies the whole burst. Measured on the Mac (M2, Metal, 1920x1000 native,
/// scene A, one pane) against the `frame worst:` latch: the worst frame of a
/// 300-frame latched sample spent **36,017 µs of its 39,567 µs in
/// `pre_ingest`**, on the frame after fourteen overlay payloads and four
/// Level III products landed together; 25 of those 300 spent over 500 µs
/// there. A second leg, instrumented per drain, caught nine spikes over two
/// runs at 1.5–3.3 ms, spread across three different rows — `poll_chunk_results`,
/// `poll_overlay_fetch_results` and `publish_base_volumes` — so no one drain
/// owns it and the bound belongs to the phase.
///
/// Checked **between** arrivals, so a frame's real spend is this budget plus
/// one whole arrival, on [`DEFERRED_DROP_BUDGET_PER_FRAME`]'s terms. What is
/// left stays queued and the window is asked for another frame, which is the
/// product rule directly: interaction is realtime, data may lag.
///
/// A cascade for that constant's reason — wasm and mobile hold the scarcer
/// thread — and it selects a value, never behaviour.
#[cfg(target_arch = "wasm32")]
pub const INGEST_BUDGET_PER_FRAME: Duration = WASM_INGEST_BUDGET_PER_FRAME;
#[cfg(all(not(target_arch = "wasm32"), mobile))]
pub const INGEST_BUDGET_PER_FRAME: Duration = MOBILE_INGEST_BUDGET_PER_FRAME;
#[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
pub const INGEST_BUDGET_PER_FRAME: Duration = DESKTOP_INGEST_BUDGET_PER_FRAME;

pub const WASM_INGEST_BUDGET_PER_FRAME: Duration = Duration::from_micros(500);
pub const MOBILE_INGEST_BUDGET_PER_FRAME: Duration = Duration::from_micros(500);
pub const DESKTOP_INGEST_BUDGET_PER_FRAME: Duration = Duration::from_millis(1);

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
/// from, **~0.67 MB** at the same tail (`MEASURED_PARSED_TILE_BYTES`, re-derived
/// at 670,110 on 2026-09-07 — it was ~2.09 MB until the parse stopped expanding
/// the wire's own interned property tables into a `HashMap` per feature, and
/// stopped holding two feature slots per feature). A **terrain** entry is one
/// 256x256 RGBA texture, 256 KiB, no tail.
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
/// **Per bracket.** wasm32 is a browser tab whose linear memory is at most one
/// gigabyte (`WASM_LINEAR_MEMORY_MAX_BYTES`, the bound the module is linked
/// with — a handheld is given half of it) shared with every other heap in the
/// page: 48/48/25 MiB at the floor is 121 MiB, an eighth of the full bound and
/// a quarter of a handheld's. The step
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
///
/// **The wasm parsed arm does not move when the parse gets cheaper, because
/// its surplus is owed and not available.** 48 MiB held ~24.1 parses at the
/// old tail and holds ~75.1 at this one; 64 MiB held ~32.1 and holds ~100.1.
/// Against the half-step set this block argues the styled floor from — 106
/// here, and **174** where the tree measures it today
/// (`squallar_egui::tiles::measured::HALF_STEP_TILES`; the paragraphs above
/// still carry the earlier ~106, which `docs/cross-platform-resource-limits.md`
/// records as a count cap's ceiling rather than a working set) — the arm is
/// short either way, before the parse got cheaper and after. So the whole
/// 3.1x is spent on overrun the working-set floor was already carrying:
/// nothing here is banked, and mobile stays pinned to it.
///
/// Desktop starts at 160/62/64 — 114 styled tail entries, the user's own
/// window at the styled tail (106) with eight to spare, and a parsed cache
/// that restyles the common 1920x1200 canvas (96 tiles) wholly from cache; a
/// 2560x1440 window between zooms (144 styled entries, 211 MB at the styled
/// tail) is the styled floor's overrun and the styled step's fit (256 MiB,
/// 183 entries) — and rises to 512/192/128 on a discrete adapter with a
/// desktop shape, where a 3840x2160 window between zooms (299 tiles, 437 MB
/// at the styled tail) fits without the floor's help.
///
/// **Every parsed rung is now a canvas tile count times the parsed tail. At
/// the step and the ceiling that is a DECISION taken here, and it is not the
/// preservation of anything.** Read the paragraph above for what it actually
/// derived: **96 is the only parsed count in it.** 144, 183 and 299 are styled
/// counts — 144 x 1.46 MB = 211, 256 MiB / 1.46 MB = 183, 299 x 1.46 MB = 437.
/// The parsed step and ceiling never had a canvas count at all. 256 and 384
/// MiB are simply what somebody wrote, and 128.3 and 192.5 parses are what
/// those bytes happened to buy at a cost that has since changed by 3.1x.
///
/// So **neither** reading preserves a decision, and the question is not which
/// figure to keep but which quantity is worth deciding. Both were computed:
///
/// | rung | canvas | keep what the bytes BOUGHT | cover the CANVAS |
/// |---|---|---:|---:|
/// | floor | 1920x1200 | 96.2 -> 61.50 -> **62 MiB** | 96 -> 61.35 -> **62 MiB** |
/// | step | 2560x1440 | 128.3 -> 82.00 -> **83 MiB** | 144 -> 92.03 -> **93 MiB** |
/// | ceiling | 3840x2160 | 192.5 -> 123.00 -> **124 MiB** | 299 -> 191.08 -> **192 MiB** |
///
/// (parses -> exact MiB -> rounded up to the whole MiB, as the floor already was)
///
/// **Where the 78 MiB between them falls is the argument.** 87 % of it is the
/// ceiling rung and 0 % is the floor: as landed the two readings give the
/// floor the *same* 62 MiB, and unrounded the canvas reading is 0.15 MiB
/// *smaller* there. The ceiling rung is the discrete-adapter desktop with a
/// 3840x2160 canvas — the machine least constrained by resident footprint —
/// and the floor is where a footprint target actually binds. The choice
/// therefore never trades bytes against coverage where the bytes matter; it
/// spends 68 MiB on a large-GPU desktop to cover that same desktop's canvas.
///
/// **The fact that argues against it, because it is real and was weighed.**
/// The same change cut a parsed miss from **1.7756 ms to 1.0521 ms** on this
/// fixture — a 40.7 % cut — so tolerating under-coverage is materially
/// cheaper than it was when 256 and 384 MiB were written. That genuinely
/// weakens the case for buying coverage and does not overturn it: 1.0521 ms
/// is still **26 % of the 4 ms frame bar**, a miss is still the cost the
/// cache exists to avoid, and the rung paying for it is the one with the
/// memory to spare. Coverage of a named canvas is also a quantity a later
/// reader can check against a window, where "128.3 parses" is only an
/// artefact of arithmetic nobody performed on purpose.
///
/// Stated in both directions, because one alone misreads it: **parsed
/// coverage RISES at two rungs while parsed bytes fall at all three.**
/// 96/128/192 parses become 96/144/299; 192/256/384 MiB become 62/93/192.
///
/// The arithmetic, rounded up to the whole MiB the way the floor already was
/// (96 x 2,092,002 = 191.53 -> mib(192)): 96 x 670,110 = 61.35 -> 62 MiB,
/// 144 x 670,110 = 92.03 -> 93 MiB, 299 x 670,110 = 191.08 -> 192 MiB.
/// `the_tile_allowances_are_the_written_figures_on_every_bracket` holds each
/// rung against `count x tail` in **both** directions rather than against a
/// byte literal, so the next tail measurement moves these for free — and
/// cannot silently multiply a count the way fixed bytes under a 3.1x cheaper
/// parse would have (300, 400 and 600 parses, chosen by nobody).
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
pub const DESKTOP_TILE_PARSED_BYTES: [usize; 3] = [mib(62), mib(93), mib(192)];
pub const DESKTOP_TILE_TERRAIN_BYTES: [usize; 3] = [mib(64), mib(80), mib(128)];
/// 286 / 429 / 832 MiB of allowances at the three rungs, each rounded up to
/// the next 64 MiB — which is what every host ceiling in this block already
/// was, checked in `the_host_ceilings_are_their_sums_rounded_up_to_64_mib`.
/// The ceiling rung is still exactly its sum, 832 being a multiple of 64:
/// nothing is slack there. These fell with the parsed allowances; leaving
/// them where they were would have failed `check_budgets`'s 1.25x snugness
/// at the floor (448 over 286) and at the step (640 over 429).
pub const DESKTOP_TILE_HOST_CEILING_BYTES: [usize; 3] = [mib(320), mib(448), mib(832)];

/// The share of a **measured, probed or derived** GPU capacity the scene's
/// need may occupy, as `(numerator, denominator)`: three quarters. Metal's own
/// recommended working set is ~75 % of RAM on M-series, the one vendor figure
/// for how much of a memory a renderer is meant to take; the rest is the
/// driver, the compositor and the picture in flight. A derived figure is a
/// share of that same real memory, so it takes the same fraction — spending it
/// whole is what put an APU's two allowances over its one pool.
///
/// **Not applied to a presumed capacity**: the bracket's
/// `APP_TEXTURE_BUDGET_BYTES` constant was argued with its own headroom and is
/// what today's sum proof already spends up to, so on that arm the constant is
/// the allowance ([`crate::scene::Capacity::allowance`]). The written decision
/// to apply it there too — a presumption is a wall as well — is deferred
/// pending a measurement of what it costs the web arm's oversample rung, which
/// that function's own doc records.
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
/// What the three steps buy on the user's own 2878x1651 window, whose pane is
/// 2878x1611 under a forty-point top bar, per picture: 41,719,488 B,
/// 28,963,044 B, 18,545,832 B of the page heap — and thirteen shown layers
/// re-rasterise together on every move and every loop bucket, so a batch is
/// 542, 377 or 241 MB of a 1 GiB linear memory. The 1.5x figure is the one
/// both Tier-2 `huge` legs reported allocating and the one Firefox's
/// allocation failures named twelve times.
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
///
/// **It divides the pool into two named shares, not just the GPU's.** The
/// remainder is the host's ([`crate::scene::Capacity::unified`]), so the
/// figure this names is one side of a partition and the two sides sum to the
/// memory that carries both. Moving it moves where the line falls and can
/// never let the two sides overlap — which is the property the number itself
/// could not carry, and why the incident this comes from was not fixed by
/// changing it.
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

/// One cell of a resident [`squallar_radar::voxel::VolumeGrid`]'s **index
/// plane**, on the host: one palette index a cell, `VolumeGrid::indices`'s
/// `Vec<u8>`.
///
/// The host counterpart of `squallar_volumetric::raymarch::GRID_BYTES_PER_CELL`,
/// which is four: the plane is widened into the texture format on its way to
/// the device (`coverage_premultiplied_into`), so the same grid is one byte a
/// cell here and four there. Two memories, two prices.
pub const HOST_GRID_BYTES_PER_CELL: usize = 1;

/// **Bytes one resident voxel grid costs the HOST**, for a cell budget of
/// `cells` — the counterpart of
/// `squallar_volumetric::raymarch::resident_grid_bytes`, which prices the same
/// grid's three *textures*.
///
/// The figure is `VolumeGrid::memory_bytes`'s, term for term, for the grids
/// this application actually builds: the index plane at
/// [`HOST_GRID_BYTES_PER_CELL`] and the transfer table at
/// [`VOLUME_LUT_BYTES`]. **The value plane is not in it, and that is a
/// property of the request rather than an omission**: every 3D volume this
/// tree builds asks `values_wanted: false` (`squallar_radar::voxel`'s
/// `volume_request_for` — "the values in their own units are a second buffer
/// nothing up there reads"), so `VolumeGrid::values` is `None` and its
/// `Vec<f32>` is never allocated. A build that asked for one would hold four
/// more bytes a cell than this says.
///
/// **Priced from the cell BUDGET, not from the resolved shape, and the
/// direction is stated.** The shape a device builds is
/// [`volume_grid_shape_of`], whose `spend_budget` rounds each axis *down* to
/// its alignment and so never yields more cells than the budget it was
/// handed. So this over-prices a grid whose alignment lost cells and can
/// never under-price one — the direction a budget term has to err in, and the
/// reason no `max_axis` has to reach this function.
pub const fn volume_grid_host_bytes(cells: [u32; 3]) -> u64 {
    let cells = (cells[0] as u64) * (cells[1] as u64) * (cells[2] as u64);
    cells * HOST_GRID_BYTES_PER_CELL as u64 + VOLUME_LUT_BYTES as u64
}

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
        // `ceiling <= 1.25 x sum`, spelled without the product: these
        // constants compile for wasm32 too, where `usize` is 32 bits and a
        // `sum x 4` on a gibibyte-scale desktop rung overflows it. The rung
        // that motivated the spelling was 1 GiB; it is 832 MiB today and the
        // spelling stays, because the hazard is the scale and not one value.
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
    // The host index plane costs a whole byte a cell — a zero here would price
    // a resident grid's host half at its transfer table alone.
    assert!(HOST_GRID_BYTES_PER_CELL > 0);
    // One spelling of the table that travels with a grid: `VOLUME_LUT_BYTES`
    // is this crate's name for it, `LUT_LEN` the substrate's. A grid priced
    // against one and built against the other is a silent gap.
    assert!(VOLUME_LUT_BYTES == squallar_radar::voxel::LUT_LEN);
    assert!(MAX_CONCURRENT_RENDERS > 0);
    assert!(MAX_CONCURRENT_LOOP_DOWNLOADS > 0);
    // The loop timer divides by this; a reversed pair is a `clamp` that panics.
    assert!(MIN_LOOP_SPEED_FPS > 0.0);
    assert!(MIN_LOOP_SPEED_FPS <= DEFAULT_LOOP_SPEED_FPS);
    assert!(DEFAULT_LOOP_SPEED_FPS <= MAX_LOOP_SPEED_FPS);
    assert!(MAX_LOOP_RENDER_BUDGET <= MAX_LOOP_FRAMES);
    // The scan reserve prices one decoded Level II volume per radar loop frame
    // on the host. Both arms say what they pin, so that neither can be retyped
    // to match a constant that moved under it.
    //
    // The first pins the WIDTH. The value is **bytes**, spelled here as the
    // decimal that `80 * 1024 * 1024` expands to, so a reserve re-entered as
    // `80` — or as MiB anywhere on the path — under-prices a pending frame by
    // 2^20 and reads as a scene that fits.
    //
    // The second pins WHAT IT PRICES: the 74.63 MiB maximum measured over 208
    // real archive volumes (`squallar_app::volume_inventory`), 78,255,227 B,
    // which the reserve covers with 5.37 MiB to spare. This arm exists to go
    // red when a re-measurement moves that maximum — moving this literal to
    // match a new maximum *without* moving the reserve is precisely the defect
    // it is here to catch, and it is how the superseded 58.3 MiB figure
    // (61,131,981 B) sat compiled in under a 64 MiB reserve that had quietly
    // fallen 10.63 MiB below the real maximum.
    assert!(LOOP_SCAN_RESERVE_BYTES == 83_886_080);
    assert!(LOOP_SCAN_RESERVE_BYTES >= 78_255_227);
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
