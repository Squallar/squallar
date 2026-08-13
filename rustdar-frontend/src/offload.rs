//! Where a long-running, CPU-bound job runs.
//!
//! Four places in this crate hand a closure somewhere it will not stall the
//! frame that created it: the static radar render, the loop-frame render, the
//! overlay rasterization and the radar-sites rasterization. All four have the
//! same shape — a `FnOnce` that ends by sending its result on an
//! `mpsc::Sender` and calling `notify_redraw` — and all four had the same
//! `std::thread::Builder` call written out inline.
//!
//! They are funnelled through here so the wasm arm exists once.
//!
//! # Two shapes, one funnel
//!
//! A closure cannot be posted to a Web Worker, so the funnel takes work in two
//! forms and makes one decision about both:
//!
//! * [`offload`] takes an opaque `FnOnce`. It runs on a thread natively and
//!   inline on the web, which is the best available answer for a job whose
//!   inputs cannot be described — see [`offload`]'s own note on which those are.
//! * [`offload_job`] takes a [`JobRequest`], which *is* a description. Given a
//!   worker it posts; without one it runs [`execute`] in exactly the place
//!   [`offload`] would have run the closure.
//!
//! The second is not a second code path. Both arms of [`offload_job`] call the
//! same [`execute`] and the same `deliver`, so the fallback is derived from the
//! worker path rather than written beside it, and there is no pair to drift.

use rustdar_radar::render_input::RenderInput;
use rustdar_radar::voxel::{VoxelGrid, VoxelRequest, VoxelShape};
use rustdar_radar::xsect::{CrossSection, SectionRequest};
use std::cell::RefCell;
use std::collections::HashMap;

/// Run `job` away from the frame that requested it.
///
/// Native spawns a named OS thread and returns immediately.
///
/// wasm32-unknown-unknown has no threads: `std::thread::Builder::spawn` there
/// returns `Err(Unsupported)` at *runtime* rather than failing to compile, so a
/// bare spawn site does not break the web build — it compiles clean and then
/// panics the first time the user asks for a radar frame. That is the failure
/// this function exists to remove. The web arm runs `job` inline.
///
/// Running inline blocks the frame. For rasterization that is a visible stall,
/// and [`offload_job`] is the answer for the paths that can describe their
/// input. The two that cannot stay here:
///
/// * `overlay-render` captures a `RasterizeFn` — a `Box<dyn FnOnce(..) -> ..>`
///   holding overlay handler state — and answers with a `HitMap` whose
///   `id_map` is a `HashMap<u32, Arc<dyn OverlayItem>>`. Neither a trait-object
///   closure nor a trait-object map crosses a message port. Making it portable
///   means returning a `u32` id image and rebuilding the map on this side, a
///   refactor of `rustdar-overlays` against a rasterizer that draws vector
///   shapes rather than the 28 M projections the radar one does.
/// * `sites-render` is portable — a `Vec<RadarSiteInfo>` in, a `Vec<u8>` out —
///   and simply is not expensive enough yet to be worth a second job kind.
///
/// Inline execution preserves the contract the callers actually depend on. Each
/// `job` delivers through a channel that is drained on a later frame, so a send
/// that happens before the caller returns is indistinguishable from one that
/// happens after it — the receiver cannot tell, and neither can the render
/// budget, whose `RenderGuard` simply drops sooner.
///
/// The `Send` bound is kept on both arms deliberately. It costs the web arm
/// nothing (every existing caller already satisfies it, since they were written
/// for threads) and dropping it would silently license a `!Send` job that then
/// fails to compile on desktop.
pub fn offload(name: &'static str, job: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(job)
            .unwrap_or_else(|e| panic!("failed to spawn {name} thread: {e}"));
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Timed because this is the one arm where the cost lands on the frame.
        // The number is what decides whether a worker is needed and how many, so
        // it is logged rather than estimated.
        let started = web_time::Instant::now();
        job();
        log::info!(
            "{name} took {} ms on the main thread",
            started.elapsed().as_millis()
        );
    }
}

/// A CPU-bound job described as data, so it can be executed somewhere that does
/// not share this thread's memory.
///
/// Every variant is an *input* to a render, never its output: what travels is
/// the smallest thing the renderer can be re-run from, because re-running it is
/// how the worker and this thread stay byte-identical without a second
/// implementation to keep in step.
#[derive(Debug, Clone, PartialEq)]
pub enum JobRequest {
    /// Rasterize a Level II frame.
    Radar {
        /// Boxed because a `RenderInput` owns its gate bytes and is the largest
        /// thing in the enum by three orders of magnitude.
        input: Box<RenderInput>,
        /// Whether the caller wants the per-pixel value grid back.
        ///
        /// Static pane renders do — it is what a hover reads. Loop frames drop
        /// it on arrival, and it is the same size as the texture, so returning
        /// it would copy `LOOP_IMAGE_SIZE² × 4` bytes across a worker boundary
        /// per frame purely to discard them.
        ///
        /// The texture is unaffected either way; only the grid is cleared.
        values_wanted: bool,
        /// Whether this render may take the long-range raster size. See
        /// [`JobRequest::side_ceiling_px`].
        full_res: bool,
    },
    /// Rasterize a Level III radial product.
    ///
    /// The product's *bytes*, not its decoded form: a `Level3Message` holds
    /// run-length radial packets with no serde derives anywhere in the graph,
    /// and re-decoding is both cheap against the render and a use of the one
    /// decoder rather than a second description of the format. The decode moves
    /// off the main thread with the render as a result.
    Level3 {
        bytes: std::sync::Arc<Vec<u8>>,
        product: rustdar_radar::types::RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
        /// See [`JobRequest::side_ceiling_px`].
        full_res: bool,
    },
    /// Rasterize a Level III product **derived from two objects of the same
    /// volume**: VIL density, Digital VIL over Enhanced Echo Tops
    /// (`rustdar_radar::vild`).
    ///
    /// A second variant rather than a `Vec<Arc<Vec<u8>>>` on the one above: the
    /// two objects are not interchangeable — the first is the numerator and the
    /// second the denominator — and a positional pair says so where a list
    /// would leave it to a comment. The bytes travel for the same reason
    /// [`JobRequest::Level3`]'s do.
    Level3Pair {
        dvl: std::sync::Arc<Vec<u8>>,
        eet: std::sync::Arc<Vec<u8>>,
        radar_lat: f64,
        radar_lon: f64,
        /// See [`JobRequest::side_ceiling_px`].
        full_res: bool,
    },
    /// Draw a vertical cross-section through a volume.
    ///
    /// The geometry rides here rather than on the [`RenderInput`]: a section's
    /// endpoints are not a render parameter *of reflectivity*, and a
    /// `RenderInput` carrying them would make every plan-view payload's bytes
    /// depend on where somebody last drew a line.
    ///
    /// The `input` is a [`RenderInput::extract_volume`] payload — every tilt
    /// carrying the moment, and the cut table that keys them.
    Section {
        input: Box<RenderInput>,
        request: SectionRequest,
    },
    /// Resample a volume into a Cartesian grid for a raymarch.
    Voxels {
        input: Box<RenderInput>,
        request: VoxelRequest,
    },
}

/// What a job produces.
///
/// Widened from a bare [`RenderedFrame`] when a section and a voxel grid became
/// things a worker could be asked for. **[`RenderedFrame`] itself is
/// deliberately untouched**, and in particular did not gain a width and a
/// height even once a plan view stopped having one size: its consumers derive
/// the side from the buffer's own length and check it against the closed set of
/// sizes this build renders (`constants::raster_side_from_rgba_len`), which is
/// the same guard a named constant was — a `ColorImage` panic on a render
/// worker means no response ever arrives and the pane stays blank forever —
/// without the payload being trusted to describe itself. See
/// [`JobOutput::frame`].
#[derive(Debug, PartialEq)]
pub enum JobOutput {
    Frame(RenderedFrame),
    /// Boxed: a `CrossSection` owns three `SECTION_WIDTH × SECTION_HEIGHT`
    /// planes, which is megabytes against the enum's other variants.
    Section(Box<CrossSection>),
    /// Boxed for the same reason, more so: a desktop grid is 8 MiB of indices.
    Voxels(Box<VoxelGrid>),
}

impl JobOutput {
    /// The frame, or `None` for an output of another kind.
    ///
    /// This is what makes widening the result type safe for every existing
    /// consumer: a `Section` handed to a frame consumer becomes `None`, which
    /// is "nothing to draw" — a state every path already handles, with
    /// `deliver` still running and the render budget still unwound.
    pub fn frame(self) -> Option<RenderedFrame> {
        match self {
            Self::Frame(frame) => Some(frame),
            Self::Section(_) | Self::Voxels(_) => None,
        }
    }

    /// The section, or `None` for an output of another kind.
    pub fn section(self) -> Option<Box<CrossSection>> {
        match self {
            Self::Section(section) => Some(section),
            Self::Frame(_) | Self::Voxels(_) => None,
        }
    }

    /// The voxel grid, or `None` for an output of another kind.
    pub fn voxels(self) -> Option<Box<VoxelGrid>> {
        match self {
            Self::Voxels(grid) => Some(grid),
            Self::Frame(_) | Self::Section(_) => None,
        }
    }

    /// Which view this output is of. For a cache key and for the sibling
    /// broadcast, both of which must never hand a consumer a wrong-shaped
    /// buffer.
    pub fn view(&self) -> rustdar_radar::types::RenderView {
        use rustdar_radar::types::RenderView;
        match self {
            Self::Frame(_) => RenderView::PlanView,
            Self::Section(_) => RenderView::CrossSection,
            Self::Voxels(_) => RenderView::Volume,
        }
    }
}

/// What a rasterizing job produces: the RGBA texture, the half-width it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
///
/// Named fields, as the renderer's own [`rustdar_radar::render::SweepRender`]
/// has: the two buffers are the same shape to a message port, and transposing
/// them would swap a texture for a value grid somewhere with no type error to
/// catch it. A separate type and not that one because this is what crosses the
/// port.
///
/// The extent and the fold limit are metadata and stay metadata — they say
/// where the pixels *are* and what speed they wrap at, never how many of them
/// there are. How many there are is the buffer's own length, read back against
/// a closed set at each consumer (`constants::raster_side_from_rgba_len`);
/// nothing on this port describes its own shape, which is what keeps a
/// malformed payload from being believed. Adding a second `f64` beside the
/// extent does not weaken that: neither number can be read as a dimension,
/// and the guard that protects a pane from a blank texture reads the length
/// and only the length.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: Vec<u8>,
    pub max_range_km: f64,
    pub values: Vec<f32>,
    /// Where the rendered sweep's cut declared its velocity folds, m/s, or
    /// `None` for a raster with no one cut behind it — every Level III
    /// product and every volume product — and for a volume that declared
    /// nothing, which is every Message 1 volume.
    ///
    /// See [`rustdar_radar::render::SweepRender::nyquist_ms`], which is where
    /// it comes from and which explains what it is a property of.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer this raster was classified against came from,
    /// or `None` for every raster that classified nothing — which is every
    /// product but the hybrid classification.
    ///
    /// See [`rustdar_radar::hca::MeltingLayerSource`]. It rides beside
    /// `nyquist_ms` and for the same reason: it is a fact about *this* picture
    /// that the far end cannot recompute, and here it is the difference
    /// between a classification measured for this volume and one standing on a
    /// fleet constant that has been measured 3 km wrong.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
}

/// A [`MeltingLayerSource`](rustdar_radar::hca::MeltingLayerSource) as a
/// number, for the one boundary that can only carry numbers.
///
/// The enum lives in `rustdar-radar`, which has no wire form for it and needs
/// none: nothing in that crate crosses a message port. The browser's
/// page↔worker port does, and it carries JS values — so the mapping is written
/// here, beside [`RenderedFrame`], which is the type that actually crosses.
///
/// A newtype rather than two free functions so the pair cannot drift apart:
/// [`from_wire_code`](Self::from_wire_code) is exhaustive over the same match
/// arms [`wire_code`](Self::wire_code) writes, so adding a variant upstream
/// fails this build rather than silently encoding as "unknown".
///
/// `None` from `from_wire_code` is a byte this build does not have — a page and
/// a worker on opposite sides of a deploy, which the protocol token already
/// refuses — and reads as "no source stated", the same as an absent field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeltingLayerWire(pub rustdar_radar::hca::MeltingLayerSource);

impl MeltingLayerWire {
    pub fn wire_code(self) -> u8 {
        use rustdar_radar::hca::MeltingLayerSource as S;
        match self.0 {
            S::Rpg => 0,
            S::RadarDetected => 1,
            S::Sounding => 2,
            S::FleetDefault => 3,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code).
    pub fn from_wire_code(code: u8) -> Option<Self> {
        use rustdar_radar::hca::MeltingLayerSource as S;
        let source = match code {
            0 => S::Rpg,
            1 => S::RadarDetected,
            2 => S::Sounding,
            3 => S::FleetDefault,
            _ => return None,
        };
        Some(Self(source))
    }
}

/// `None` where the renderer found nothing to draw — a scan with no matching
/// sweep. Callers treat it as the failure the renderer already meant by it.
pub type JobResult = Option<JobOutput>;

impl From<rustdar_radar::render::SweepRender> for RenderedFrame {
    /// The renderer's own answer, whole. One conversion for all three
    /// rasterizing arms, so a Level III frame and a Level II one cannot come to
    /// describe themselves differently.
    fn from(render: rustdar_radar::render::SweepRender) -> Self {
        Self {
            image: render.image,
            max_range_km: render.max_range_km,
            values: render.values,
            nyquist_ms: render.nyquist_ms,
            melting_layer_source: render.melting_layer_source,
        }
    }
}

/// A rasterizing job, described where it can be and opaque where it cannot.
///
/// Both arms reach [`offload_job`], which is the point: there is one place that
/// decides where work runs, and adding a job kind does not add a dispatch site.
pub enum Job {
    /// Portable. Goes to the worker when one is attached, and runs through
    /// [`execute`] when none is. Every rasterizing dispatch is one of these.
    Described(JobRequest),
    /// Not describable, so it runs where [`offload`] runs things — a thread
    /// natively, this frame in the browser.
    ///
    /// Nothing in production is one today; it is what [`Job::renders_nothing`]
    /// is built from, and the shape a future job kind takes before it has a
    /// wire form. Reaching for it for a *rasterizing* job would put that job
    /// back on the browser's main thread, which is the thing this module
    /// exists to stop.
    Opaque(Box<dyn FnOnce() -> JobResult + Send>),
}

impl Job {
    /// A job whose answer is "nothing to draw".
    ///
    /// Used where a request cannot even be described because there is no data
    /// behind it. It is deliberately still a *job*: the caller has already
    /// taken a slot in the render budget and marked its pane in flight, and
    /// those are unwound by `deliver` running, not by returning early.
    pub fn renders_nothing() -> Self {
        Self::Opaque(Box::new(|| None))
    }
}

impl JobRequest {
    /// Encode for a worker. The framing is one tag byte and then the variant's
    /// own bytes, so a new variant cannot be mistaken for an old one.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Radar {
                input,
                values_wanted,
                full_res,
            } => {
                let mut out = Vec::new();
                out.push(TAG_RADAR);
                out.push(u8::from(*values_wanted));
                out.push(u8::from(*full_res));
                out.extend_from_slice(&input.to_bytes());
                out
            }
            Self::Level3 {
                bytes,
                product,
                radar_lat,
                radar_lon,
                full_res,
            } => {
                let mut out = vec![TAG_LEVEL3];
                out.push(u8::from(*full_res));
                out.extend_from_slice(&product.wire_code().to_le_bytes());
                out.extend_from_slice(&radar_lat.to_le_bytes());
                out.extend_from_slice(&radar_lon.to_le_bytes());
                out.extend_from_slice(bytes);
                out
            }
            Self::Level3Pair {
                dvl,
                eet,
                radar_lat,
                radar_lon,
                full_res,
            } => {
                // The first object is length-prefixed and the second takes the
                // rest, so neither length can lie about the other.
                let mut out = vec![TAG_LEVEL3_PAIR];
                out.push(u8::from(*full_res));
                out.extend_from_slice(&radar_lat.to_le_bytes());
                out.extend_from_slice(&radar_lon.to_le_bytes());
                out.extend_from_slice(&(dvl.len() as u32).to_le_bytes());
                out.extend_from_slice(dvl);
                out.extend_from_slice(eet);
                out
            }
            // Both of the two below put the `RenderInput` **last**, because
            // `RenderInput::from_bytes` refuses trailing bytes: it has to be
            // handed exactly the remainder, so nothing may follow it.
            Self::Section { input, request } => {
                let mut out = vec![TAG_SECTION];
                encode_section_request(&mut out, request);
                out.extend_from_slice(&input.to_bytes());
                out
            }
            Self::Voxels { input, request } => {
                let mut out = vec![TAG_VOXELS];
                encode_voxel_request(&mut out, request);
                out.extend_from_slice(&input.to_bytes());
                out
            }
        }
    }

    /// `None` on an unknown tag or a payload this build cannot read — the two
    /// ends of a message port can be different builds, so that has to be a
    /// clean refusal rather than a misparse.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (tag, rest) = bytes.split_first()?;
        match *tag {
            TAG_RADAR => {
                let (values_wanted, rest) = rest.split_first()?;
                let (full_res, rest) = rest.split_first()?;
                Some(Self::Radar {
                    values_wanted: flag(*values_wanted)?,
                    full_res: flag(*full_res)?,
                    input: Box::new(RenderInput::from_bytes(rest)?),
                })
            }
            TAG_LEVEL3 => {
                let mut r = Reader::new(rest);
                Some(Self::Level3 {
                    full_res: flag(r.u8()?)?,
                    product: rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?,
                    radar_lat: r.f64()?,
                    radar_lon: r.f64()?,
                    bytes: std::sync::Arc::new(r.rest().to_vec()),
                })
            }
            TAG_LEVEL3_PAIR => {
                let mut r = Reader::new(rest);
                let full_res = flag(r.u8()?)?;
                let radar_lat = r.f64()?;
                let radar_lon = r.f64()?;
                let dvl_len = r.u32()? as usize;
                Some(Self::Level3Pair {
                    full_res,
                    radar_lat,
                    radar_lon,
                    dvl: std::sync::Arc::new(r.take(dvl_len)?.to_vec()),
                    eet: std::sync::Arc::new(r.rest().to_vec()),
                })
            }
            TAG_SECTION => {
                let mut r = Reader::new(rest);
                let request = decode_section_request(&mut r)?;
                let input = RenderInput::from_bytes(r.rest())?;
                agree_on_product(request.product, &input)?;
                Some(Self::Section {
                    input: Box::new(input),
                    request,
                })
            }
            TAG_VOXELS => {
                let mut r = Reader::new(rest);
                let request = decode_voxel_request(&mut r)?;
                let input = RenderInput::from_bytes(r.rest())?;
                agree_on_product(request.product, &input)?;
                Some(Self::Voxels {
                    input: Box::new(input),
                    request,
                })
            }
            _ => None,
        }
    }

    /// The largest raster side this job's render may produce.
    ///
    /// One byte on the wire — `full_res` — and two questions answered by it,
    /// which is deliberate rather than a conflation. The renderer needs a
    /// single number; what it is *for* is "how big a texture is this result
    /// allowed to become", and there are exactly two reasons to say "the base
    /// one":
    ///
    ///   * **This is a loop frame.** A loop holds frames by the dozen, so it
    ///     renders leaner by policy — it already drops the value grid for the
    ///     same reason. [`crate::constants::LOOP_IMAGE_SIZE`] is the base size
    ///     natively and 1024 on the web, where the per-pane loop budget is what
    ///     it is.
    ///   * **This device cannot take the long-range texture.** The gate is
    ///     `AppState::long_range_raster_ok`, a `max_texture_dimension_2d`
    ///     comparison made once at device creation; a sub-4096 GLES handheld
    ///     degrades to a correct base-size picture instead of a texture
    ///     creation that fails and leaves the pane blank.
    ///
    /// Both answers are "render this at the base size", so they are one flag.
    /// That is not the same as "the picture the display always made": the
    /// extent is the sweep's either way, so a base-size ceiling over a sweep
    /// reaching past 230 km buys the ground and pays in scale —
    /// `rustdar_radar::types::raster_side_px` costs that out. The gate is
    /// applied at the dispatch site rather than here because this type travels
    /// to a worker that has no device to ask.
    ///
    /// The [`JobRequest::Section`] and [`JobRequest::Voxels`] arms have no such
    /// byte: a section's raster is a constant of the view (`xsect`'s
    /// `SECTION_WIDTH`) and a voxel grid's shape is already on the wire.
    fn side_ceiling_px(&self) -> usize {
        let full_res = match self {
            Self::Radar { full_res, .. }
            | Self::Level3 { full_res, .. }
            | Self::Level3Pair { full_res, .. } => *full_res,
            Self::Section { .. } | Self::Voxels { .. } => return 0,
        };
        if full_res {
            crate::constants::LONG_RANGE_IMAGE_SIZE
        } else {
            crate::constants::LOOP_IMAGE_SIZE
        }
    }

    /// For the timing log, so a slow job says which kind it was.
    fn kind(&self) -> &'static str {
        match self {
            Self::Radar { input, .. } => match input.product() {
                rustdar_radar::types::RadarProduct::NormalizedRotation => "radar/nrot",
                rustdar_radar::types::RadarProduct::StormRelativeVelocity => "radar/srv",
                _ => "radar",
            },
            Self::Level3 { .. } => "level3",
            Self::Level3Pair { .. } => "level3/vild",
            Self::Section { .. } => "section",
            Self::Voxels { .. } => "voxels",
        }
    }
}

/// The product is on the wire twice — once in the request's own geometry and
/// once inside the [`RenderInput`] — and two statements of one fact can
/// disagree.
///
/// They must not be allowed to. A section of a moment the payload does not
/// carry does not fail: `VolumeSampler` builds no rung for it, every sample
/// comes back `NoCoverage`, and the raster is a full-size, correctly-shaped
/// picture of clear air. That is indistinguishable from a genuinely empty
/// section, so it is refused here rather than drawn.
///
/// The alternative — carrying the product only in the payload and filling the
/// request's field from it at decode — was rejected because it makes
/// [`JobRequest`] not round-trip: a caller who built an inconsistent pair would
/// get a *different* request back rather than a refusal, which moves the
/// disagreement from the wire into the type.
fn agree_on_product(wanted: rustdar_radar::types::RadarProduct, input: &RenderInput) -> Option<()> {
    (wanted == input.product()).then_some(())
}

/// A wire boolean, refusing anything that is not 0 or 1.
///
/// The two ends of a message port can be different builds, and a byte outside
/// the pair is a payload this one cannot read rather than a `true` to guess at
/// — the same refusal `values_wanted` has always made, now spelt once for the
/// several flags that make it.
fn flag(byte: u8) -> Option<bool> {
    match byte {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn encode_section_request(out: &mut Vec<u8>, request: &SectionRequest) {
    out.extend_from_slice(&request.product.wire_code().to_le_bytes());
    out.extend_from_slice(&request.start.0.to_le_bytes());
    out.extend_from_slice(&request.start.1.to_le_bytes());
    out.extend_from_slice(&request.end.0.to_le_bytes());
    out.extend_from_slice(&request.end.1.to_le_bytes());
    match request.top_km_msl {
        None => out.push(0),
        Some(top) => {
            out.push(1);
            out.extend_from_slice(&top.to_le_bytes());
        }
    }
}

fn decode_section_request(r: &mut Reader) -> Option<SectionRequest> {
    let product = rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?;
    Some(SectionRequest {
        start: (r.f64()?, r.f64()?),
        end: (r.f64()?, r.f64()?),
        top_km_msl: match r.u8()? {
            0 => None,
            1 => Some(r.f64()?),
            _ => return None,
        },
        product,
    })
}

fn encode_voxel_request(out: &mut Vec<u8>, request: &VoxelRequest) {
    out.push(u8::from(request.values_wanted));
    out.extend_from_slice(&request.product.wire_code().to_le_bytes());
    out.extend_from_slice(&request.centre.0.to_le_bytes());
    out.extend_from_slice(&request.centre.1.to_le_bytes());
    // Tagged rather than sent as a sentinel width, the same shape the storm
    // motion override above is sent in: `None` means "as wide as the volume
    // reaches", which is a decision `build_voxels` makes on the worker side
    // with the volume in hand, and no f64 can stand for it without also being
    // a width somebody could legitimately ask for.
    // East then north, and both always written when the tag says `Some`: the
    // two axes are independent, so a wire that carried one and squared it on
    // the far side would silently resample ground the pane is not framing.
    match request.half_extent_km {
        None => out.push(0),
        Some(half) => {
            out.push(1);
            out.extend_from_slice(&half.east_km.to_le_bytes());
            out.extend_from_slice(&half.north_km.to_le_bytes());
        }
    }
    out.extend_from_slice(&request.base_km_msl.to_le_bytes());
    out.extend_from_slice(&request.top_km_msl.to_le_bytes());
    // `u16` per axis rather than `u8`: `MAX_AXIS` is 1625, which does not fit
    // in a byte, and a wrapped axis would arrive as a shorter one rather than
    // as an error. It fits a `u16` with room to spare, and
    // `the_arithmetic_bound_is_the_largest_cubable_axis` is what keeps this
    // encoding and that bound agreeing if the bound moves again.
    for n in [request.shape.nx, request.shape.ny, request.shape.nz] {
        out.extend_from_slice(&(n as u16).to_le_bytes());
    }
}

fn decode_voxel_request(r: &mut Reader) -> Option<VoxelRequest> {
    let values_wanted = match r.u8()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let product = rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?;
    let request = VoxelRequest {
        centre: (r.f64()?, r.f64()?),
        half_extent_km: match r.u8()? {
            0 => None,
            1 => Some(rustdar_radar::voxel::HalfExtentKm {
                east_km: r.f64()?,
                north_km: r.f64()?,
            }),
            _ => return None,
        },
        base_km_msl: r.f64()?,
        top_km_msl: r.f64()?,
        product,
        shape: VoxelShape {
            nx: r.u16()? as usize,
            ny: r.u16()? as usize,
            nz: r.u16()? as usize,
        },
        values_wanted,
    };
    // `build_voxels` refuses an unsupported shape too, and logs it — but that
    // refusal happens after the whole payload has been decoded and the sampler
    // built. Refusing here keeps the same rule at the boundary where the bytes
    // are untrusted, and it is the shape check that `is_supported` owns rather
    // than a second copy of the bounds.
    //
    // The **cell count** is checked beside it, and that half is new since
    // `MAX_AXIS` stopped being the 256 a GLES 3.0 device guarantees.
    // `is_supported` now admits 1625 an axis, which is 4.29 *billion* cells —
    // the bound is on what `VoxelShape::cells` can represent, not on what a
    // machine can hold, and unlike `VoxelGrid::from_bytes` there is no payload
    // in hand here whose length would have to match. A request is thirty-odd
    // bytes and `build_voxels` allocates the grid it names, so without this a
    // malformed job would be a multi-gigabyte allocation rather than a refusal.
    // `VOXEL_TEXTURE_BUDGET_BYTES` is one byte per cell of the largest index
    // plane this workspace produces, which is exactly the ceiling wanted: every
    // shape any tier can ask for is at or under it.
    let affordable = request.shape.cells() <= rustdar_radar::voxel::VOXEL_TEXTURE_BUDGET_BYTES;
    (request.shape.is_supported() && affordable).then_some(request)
}

const TAG_RADAR: u8 = 1;
const TAG_LEVEL3: u8 = 2;
/// Tag 3 was the Level III SRM derivation job, retired when storm-relative
/// velocity became a Level II product; the number stays reserved so a stale
/// worker's job cannot be misread as a future kind.
#[allow(dead_code)]
const TAG_SRM_RETIRED: u8 = 3;
/// The two-object Level III derivation: VIL density. Its product is not on the
/// wire — the tag names it, because there is exactly one such product and a
/// wire code would let a mismatched pair claim to be another one.
const TAG_LEVEL3_PAIR: u8 = 4;
/// A vertical cross-section. **5, not 4** — the next free number, not the next
/// one that looks free. Posted as tag 4 a section lands in the
/// [`TAG_LEVEL3_PAIR`] arm, which reads two `f64`s and a `u32` length and takes
/// the rest: on a section's plausible bytes that *succeeds*, and renders a
/// VIL-density product out of cross-section geometry.
const TAG_SECTION: u8 = 5;
/// A Cartesian voxel grid.
const TAG_VOXELS: u8 = 6;

/// A bounds-checked cursor over a job's fixed-width header.
///
/// Every accessor answers `None` rather than panicking: these bytes arrive on a
/// message port and are not trusted. The variable-length tail is whatever
/// [`rest`](Reader::rest) is left holding, so no length prefix can lie about it.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// Do the work.
///
/// Pure, and the *only* implementation: the worker calls it, the native thread
/// calls it, and the inline fallback calls it. That is what makes a frame
/// rendered in a worker byte-identical to one rendered on this thread — the
/// two are not two renderers that agree, they are one renderer.
///
/// "Pure" is a claim about what it *returns*, and it survives four pieces of
/// process-wide state underneath, all of them buffer pools and all admissible
/// for the same one reason:
///
/// * the plan-view rasterizer carries its cell buffer between calls
///   (`rustdar_radar::render`'s `POOLED_CELLS`);
/// * the section rasterizer carries its three planes between cuts
///   (`rustdar_radar::xsect`'s `POOLED_PLANES`), which the `Section` arm below
///   reaches through `render_section`; and
/// * the plan-view rasterizer carries the RGBA texture and the value grid it
///   answers with (`rustdar_radar::render`'s `POOLED_IMAGE` and
///   `POOLED_VALUES`).
///
/// No buffer can be handed out in any state but the one a fresh allocation would
/// be in — drained for the cells, re-seeded and resized to the raster for the
/// planes and the texture, empty for the grid — so no call can observe
/// another's. The byte-identity above was re-measured across five sites and
/// twelve products with the first in place, across nine volumes at eight sites
/// and six cut geometries with the second, and across seven sites, six products
/// and three image sides with the last two. Anything added below that *is*
/// observable between calls breaks the worker equivalence this paragraph
/// promises, because a worker does not share this process's memory.
///
/// The last pair is also the one place this function *writes* to that state: the
/// `values_wanted: false` arm below hands the grid back rather than dropping it.
/// That is still a claim about a buffer and not about a result — what the next
/// call receives is a re-seeded buffer, which is what it would have allocated,
/// so a call that finds the slot full and one that finds it empty return the
/// same bytes. Worker equivalence is untouched by it and was re-measured with
/// the write in place; what a worker's write reaches is that instance's own
/// slot, in its own linear memory, which is a fact about where the win lands
/// and not about what comes back.
pub fn execute(request: &JobRequest) -> JobResult {
    // Read once, off the request, so the three rasterizing arms cannot come to
    // disagree about how large a picture this job was allowed to make.
    let side_ceiling_px = request.side_ceiling_px();
    match request {
        JobRequest::Radar {
            input,
            values_wanted,
            ..
        } => rustdar_radar::render::render_from_sized(input, side_ceiling_px).map(|render| {
            let mut frame = RenderedFrame::from(render);
            if !*values_wanted {
                // Dropped rather than never produced: the grid is what the
                // rasterizer writes into, and the texture is derived from it.
                // Clearing it here costs nothing and keeps the renderer's
                // output the one thing it has always been.
                //
                // Handed back rather than freed, because this is the moment it
                // dies and the renderer has a slot waiting for it —
                // `rustdar_radar::render::POOLED_VALUES`. A *Level II* loop
                // frame is the one render that reaches this arm, and it is also
                // one of the two that repeat sixty times over, so this is where
                // its grid's 4,096 pages stop being re-faulted per frame. The
                // Level III loop has no `values_wanted` to reach it by and gives
                // its grid back at the consumer instead; `app_fetch`'s `deliver`
                // is the site, and the husk this `take` leaves is what makes the
                // one call there safe for both.
                rustdar_radar::render::recycle_values(std::mem::take(&mut frame.values));
            }
            JobOutput::Frame(frame)
        }),
        JobRequest::Level3 {
            bytes,
            product,
            radar_lat,
            radar_lon,
            ..
        } => decode_level3(bytes).and_then(|message| {
            rustdar_radar::render::render_level3_message_to_image_sized(
                &message,
                *product,
                *radar_lat,
                *radar_lon,
                side_ceiling_px,
            )
            .map(Into::into)
            .map(JobOutput::Frame)
        }),
        JobRequest::Level3Pair {
            dvl,
            eet,
            radar_lat,
            radar_lon,
            ..
        } => match (decode_level3(dvl), decode_level3(eet)) {
            (Some(dvl), Some(eet)) => rustdar_radar::render::render_derived_vild_to_image_sized(
                &dvl,
                &eet,
                *radar_lat,
                *radar_lon,
                side_ceiling_px,
            )
            .map(Into::into)
            .map(JobOutput::Frame),
            // One of the two did not decode, which `decode_level3` has already
            // logged: nothing to draw, the same answer a missing sweep gets.
            _ => None,
        },
        // The `Scan` is rebuilt from the payload and dropped again here, which
        // is the same shape the `Radar` arm has: one renderer, run wherever the
        // job landed, rather than a worker-side reimplementation that could
        // come to disagree with the main thread's.
        // The storm motion override rides the `RenderInput` — the lane the
        // plan-view SRV render already uses — and is threaded here into the
        // derivation seam both vertical renderers share.
        //
        // So does the declared Nyquist table, and it has to be lifted back out
        // separately: `to_scan` rebuilds model types, and the model type is
        // precisely what dropped the number. Pairing the two here is what
        // keeps this thread's velocity fold guard on the same limits the
        // thread that extracted the payload used.
        JobRequest::Section { input, request } => {
            let (scan, declared) = (input.to_scan(), input.declared_nyquist());
            rustdar_radar::xsect::render_section(
                rustdar_radar::nyquist::Volume::new(&scan, &declared),
                request,
                input.radar_lat(),
                input.radar_lon(),
                input.storm_motion_override(),
            )
            .map(|section| JobOutput::Section(Box::new(section)))
        }
        JobRequest::Voxels { input, request } => {
            let (scan, declared) = (input.to_scan(), input.declared_nyquist());
            rustdar_radar::voxel::build_voxels_with_motion(
                rustdar_radar::nyquist::Volume::new(&scan, &declared),
                request,
                input.radar_lat(),
                input.radar_lon(),
                input.storm_motion_override(),
            )
            .map(|grid| JobOutput::Voxels(Box::new(grid)))
        }
    }
}

/// The product these bytes decode to, or `None` — which the caller reports as a
/// render that drew nothing, the same answer a scan with no matching sweep gets.
fn decode_level3(bytes: &[u8]) -> Option<nexrad_level3::model::Level3Message> {
    match nexrad_level3::decode::decode_product(bytes) {
        Ok(message) => Some(message),
        Err(e) => {
            log::error!("could not decode a Level III product for rendering: {e}");
            None
        }
    }
}

/// [`execute`] straight off the wire, for a worker that holds bytes rather than
/// a `JobRequest`. `None` for a payload it cannot read, which the caller
/// reports back as a failed job rather than dropping silently.
pub fn execute_bytes(bytes: &[u8]) -> JobResult {
    execute(&JobRequest::from_bytes(bytes)?)
}

/// The reverse of the non-frame half of a worker reply: a
/// [`RenderView::wire_code`](rustdar_radar::types::RenderView::wire_code) byte
/// and the payload type's own bytes, back into a [`JobOutput`].
///
/// Here rather than in `rustdar-web` for the reason [`execute_bytes`] is here:
/// the browser crate is the adapter, this crate owns what a job means, and a
/// decode that lived over there would be reachable only from a browser. It also
/// keeps `rustdar-web` from needing a `rustdar-radar` dependency of its own.
///
/// `None` for a kind byte this build does not have, for a payload the type's
/// own codec refuses, and for a `PlanView` tag — a frame does not travel this
/// way, and a reply that says it does comes from a build whose protocol is not
/// this one. All three are "nothing to draw", which is what a failed render has
/// always meant, and all three still deliver.
pub fn decode_output(kind: u8, bytes: &[u8]) -> Option<JobOutput> {
    use rustdar_radar::types::RenderView;
    match RenderView::from_wire_code(kind)? {
        RenderView::CrossSection => {
            CrossSection::from_bytes(bytes).map(|section| JobOutput::Section(Box::new(section)))
        }
        RenderView::Volume => {
            VoxelGrid::from_bytes(bytes).map(|grid| JobOutput::Voxels(Box::new(grid)))
        }
        RenderView::PlanView => {
            log::error!("a worker sent an out-of-band payload tagged as a plan view");
            None
        }
    }
}

// ── The worker port ──────────────────────────────────────────────────────────

/// A place to send [`JobRequest`]s that is not this thread.
///
/// Implemented by `rustdar-web` over a dedicated `Worker`. It is a trait, and
/// installed rather than constructed here, because the dependency runs the
/// other way: `rustdar-web` depends on this crate, and nothing in this crate
/// may reach back for `web-sys`.
pub trait WorkerPort {
    /// Send `request` to be executed. `id` comes back with the reply so the
    /// funnel can pair them.
    ///
    /// `false` if it could not be posted at all, which makes the caller run the
    /// job here instead of waiting for a reply that is not coming.
    fn post(&self, id: u64, request: Vec<u8>) -> bool;
}

/// The state a posted job needs when its reply lands.
struct Pending {
    kind: &'static str,
    started: web_time::Instant,
    /// Holds the `RenderGuard`, the pane's `Arc<AtomicBool>` and the response
    /// channel. Consuming it is what decrements the render budget and clears
    /// the pane's in-flight mark, so it must run on *every* path out of the
    /// pending map — reply, worker loss, or shutdown.
    deliver: Box<dyn FnOnce(JobResult) + Send>,
}

thread_local! {
    /// Single-threaded by construction: only the browser build installs a port,
    /// and the browser's main thread is the only place these are registered or
    /// retired.
    static WORKER: RefCell<Option<Box<dyn WorkerPort>>> = const { RefCell::new(None) };
    static PENDING: RefCell<HashMap<u64, Pending>> = RefCell::new(HashMap::new());
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// Route [`offload_job`] through `port` from now on.
///
/// Called once, from `rustdar-web`'s entry point, after the worker has proved
/// itself with a build-token handshake. Until then — and forever, on a browser
/// where the worker could not start — [`offload_job`] runs jobs inline, which
/// is the behaviour the web build had before any of this existed.
pub fn set_worker(port: Box<dyn WorkerPort>) {
    WORKER.with(|w| *w.borrow_mut() = Some(port));
}

/// Give up on the worker: it died, or answered the handshake with a build that
/// is not this one.
///
/// Every job it still owes is failed rather than forgotten. Dropping them would
/// leak the render budget and leave panes marked in-flight forever; failing
/// them clears both, and the next frame re-dispatches — inline now, because the
/// port is gone.
pub fn abandon_worker(reason: &str) {
    let had_port = WORKER.with(|w| w.borrow_mut().take().is_some());
    let orphaned: Vec<Pending> = PENDING.with(|p| p.borrow_mut().drain().map(|(_, v)| v).collect());
    if had_port || !orphaned.is_empty() {
        log::warn!(
            "rasterization worker abandoned ({reason}); failing {} in-flight job(s)",
            orphaned.len()
        );
    }
    for pending in orphaned {
        (pending.deliver)(None);
    }
}

/// Whether jobs are currently going to a worker. For diagnostics and tests.
pub fn worker_attached() -> bool {
    WORKER.with(|w| w.borrow().is_some())
}

/// A [`WorkerPort`] installed for the length of a test, retired when this drops.
///
/// [`WORKER`] is a thread-local and the test harness reuses its threads, so a
/// port left installed is a port the *next* test on that thread inherits — and
/// that test posts its renders into a recorder nobody reads instead of running
/// them, then fails for reasons that have nothing to do with it.
///
/// **Retiring the port on the test's last line does not cover the case that
/// matters.** A failed assertion unwinds straight past that line, so the first
/// failure in a module quietly contaminates every test that runs after it on
/// the same thread and buries the real fault under wreckage it caused. `Drop`
/// runs on the unwind too, which is the whole reason this is a guard and not a
/// function called at the end.
#[cfg(test)]
pub struct InstalledTestWorker;

#[cfg(test)]
impl Drop for InstalledTestWorker {
    fn drop(&mut self) {
        abandon_worker("test teardown");
    }
}

/// Route [`offload_job`] through `port` until the returned guard drops.
///
/// The test-only counterpart of [`set_worker`] — see [`InstalledTestWorker`]
/// for why the retirement is a guard rather than a call at the end of the test.
#[cfg(test)]
pub fn install_test_worker(port: Box<dyn WorkerPort>) -> InstalledTestWorker {
    set_worker(port);
    InstalledTestWorker
}

/// Run `request` away from the frame that requested it, and hand the result to
/// `deliver`.
///
/// `deliver` runs where the result can be used: on the spawned thread natively,
/// and on the main thread in the browser. It is the whole tail of the old
/// closure — the `RenderGuard`, the cancellation check, the channel send and
/// the redraw — so the cancellation semantics are not reimplemented here, they
/// are carried inside it.
///
/// That is also what keeps `PaneRenderState::want_result`'s pruning honest. It
/// treats `Arc::strong_count(flag) > 1` as "still stoppable", and the second
/// reference used to be the one the offloaded closure held. It is now the one
/// `deliver` holds, kept alive by the pending map for exactly as long as the
/// job is outstanding, and released inside `deliver` at the cancellation check
/// — which is the last moment at which clearing the flag would change anything.
pub fn offload_job(name: &'static str, job: Job, deliver: impl FnOnce(JobResult) + Send + 'static) {
    let request = match job {
        Job::Described(request) => request,
        // Nothing to post. This is the same `offload` the opaque callers use
        // directly, reached through the funnel rather than around it.
        Job::Opaque(run) => return offload(name, move || deliver(run())),
    };
    let kind = request.kind();
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id.wrapping_add(1));
        id
    });

    // Try the worker on every target. Nothing installs one outside the browser,
    // so this is a single load of a `None` on desktop — and it means the
    // browser path is reachable from a host test with a fake port rather than
    // only from a browser.
    let posted = WORKER.with(|w| {
        w.borrow()
            .as_ref()
            .map(|port| port.post(id, request.to_bytes()))
    });
    match posted {
        Some(true) => {
            PENDING.with(|p| {
                p.borrow_mut().insert(
                    id,
                    Pending {
                        kind,
                        started: web_time::Instant::now(),
                        deliver: Box::new(deliver),
                    },
                );
            });
            return;
        }
        // The port exists but would not take the job. Falling through runs it
        // here, which is slow but correct; a port that keeps refusing is a
        // worker that has died, and `abandon_worker` is what retires it.
        Some(false) => log::warn!("{name}: worker refused the job; running it here"),
        None => {}
    }

    offload(name, move || deliver(execute(&request)));
}

/// Hand a worker's answer to the job that asked for it.
///
/// Called by `rustdar-web` from the worker's `onmessage`, on the main thread.
/// An `id` with no pending entry is ignored: it is a reply to a job that
/// [`abandon_worker`] already failed, and delivering it twice would send two
/// responses for one render.
pub fn deliver_worker_reply(id: u64, result: JobResult) {
    let Some(pending) = PENDING.with(|p| p.borrow_mut().remove(&id)) else {
        log::debug!("worker reply {id} has no pending job; already abandoned");
        return;
    };
    // The counterpart of `offload`'s wasm log line: the same measurement, for
    // the arm where the time is *not* spent on this thread.
    log::info!(
        "{} took {} ms in the worker",
        pending.kind,
        pending.started.elapsed().as_millis()
    );
    (pending.deliver)(result);
}

/// How many jobs a worker owes an answer for. For diagnostics and tests.
pub fn jobs_in_worker() -> usize {
    PENDING.with(|p| p.borrow().len())
}

#[cfg(test)]
mod tests;
