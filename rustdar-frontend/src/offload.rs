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
        /// it would copy `IMAGE_SIZE² × 4` bytes across a worker boundary per
        /// frame purely to discard them.
        ///
        /// The texture is unaffected either way; only the grid is cleared.
        values_wanted: bool,
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
/// height: `loop_frame_image`'s constant-shaped length check and
/// `ColorImage::from_rgba_unmultiplied([IMAGE_SIZE, IMAGE_SIZE], …)` are guards
/// that exist because a `ColorImage` panic on a render worker means no response
/// ever arrives and the pane stays blank forever. Payload-supplied dimensions
/// would delete them. The existing `IMAGE_SIZE` assumptions survive here
/// because the new outputs never reach them — see [`JobOutput::frame`].
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

/// What a rasterizing job produces: the RGBA texture, the range it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
///
/// Named fields rather than the renderer's `(Vec<u8>, f64, Vec<f32>)`: the two
/// buffers are the same shape to a message port, and transposing them would
/// swap a texture for a value grid somewhere with no type error to catch it.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: Vec<u8>,
    pub max_range_km: f64,
    pub values: Vec<f32>,
}

/// `None` where the renderer found nothing to draw — a scan with no matching
/// sweep. Callers treat it as the failure the renderer already meant by it.
pub type JobResult = Option<JobOutput>;

impl From<(Vec<u8>, f64, Vec<f32>)> for RenderedFrame {
    fn from((image, max_range_km, values): (Vec<u8>, f64, Vec<f32>)) -> Self {
        Self {
            image,
            max_range_km,
            values,
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
            } => {
                let mut out = Vec::new();
                out.push(TAG_RADAR);
                out.push(u8::from(*values_wanted));
                out.extend_from_slice(&input.to_bytes());
                out
            }
            Self::Level3 {
                bytes,
                product,
                radar_lat,
                radar_lon,
            } => {
                let mut out = vec![TAG_LEVEL3];
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
            } => {
                // The first object is length-prefixed and the second takes the
                // rest, so neither length can lie about the other.
                let mut out = vec![TAG_LEVEL3_PAIR];
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
                let (flag, rest) = rest.split_first()?;
                Some(Self::Radar {
                    values_wanted: match flag {
                        0 => false,
                        1 => true,
                        _ => return None,
                    },
                    input: Box::new(RenderInput::from_bytes(rest)?),
                })
            }
            TAG_LEVEL3 => {
                let mut r = Reader::new(rest);
                Some(Self::Level3 {
                    product: rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?,
                    radar_lat: r.f64()?,
                    radar_lon: r.f64()?,
                    bytes: std::sync::Arc::new(r.rest().to_vec()),
                })
            }
            TAG_LEVEL3_PAIR => {
                let mut r = Reader::new(rest);
                let radar_lat = r.f64()?;
                let radar_lon = r.f64()?;
                let dvl_len = r.u32()? as usize;
                Some(Self::Level3Pair {
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
    out.extend_from_slice(&request.half_width_km.to_le_bytes());
    out.extend_from_slice(&request.base_km_msl.to_le_bytes());
    out.extend_from_slice(&request.top_km_msl.to_le_bytes());
    // `u16` per axis rather than `u8`: `MAX_AXIS` is 256, which does not fit in
    // a byte, and a wrapped 256 would arrive as a 0-length axis.
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
        half_width_km: r.f64()?,
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
    request.shape.is_supported().then_some(request)
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
pub fn execute(request: &JobRequest) -> JobResult {
    match request {
        JobRequest::Radar {
            input,
            values_wanted,
        } => rustdar_radar::render::render_from(input).map(|(image, max_range_km, values)| {
            JobOutput::Frame(RenderedFrame {
                image,
                max_range_km,
                // Dropped rather than never produced: the grid is what the
                // rasterizer writes into, and the texture is derived from it.
                // Clearing it here costs nothing and keeps the renderer's
                // output the one thing it has always been.
                values: if *values_wanted { values } else { Vec::new() },
            })
        }),
        JobRequest::Level3 {
            bytes,
            product,
            radar_lat,
            radar_lon,
        } => decode_level3(bytes).and_then(|message| {
            rustdar_radar::render::render_level3_message_to_image(
                &message, *product, *radar_lat, *radar_lon,
            )
            .map(Into::into)
            .map(JobOutput::Frame)
        }),
        JobRequest::Level3Pair {
            dvl,
            eet,
            radar_lat,
            radar_lon,
        } => match (decode_level3(dvl), decode_level3(eet)) {
            (Some(dvl), Some(eet)) => rustdar_radar::render::render_derived_vild_to_image(
                &dvl, &eet, *radar_lat, *radar_lon,
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
        JobRequest::Section { input, request } => rustdar_radar::xsect::render_section(
            &input.to_scan(),
            request,
            input.radar_lat(),
            input.radar_lon(),
        )
        .map(|section| JobOutput::Section(Box::new(section))),
        JobRequest::Voxels { input, request } => rustdar_radar::voxel::build_voxels(
            &input.to_scan(),
            request,
            input.radar_lat(),
            input.radar_lon(),
        )
        .map(|grid| JobOutput::Voxels(Box::new(grid))),
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
/// treats `Arc::strong_count(flag) > 1` as "still running", and the second
/// reference used to be the one the offloaded closure held. It is now the one
/// `deliver` holds, kept alive by the pending map for exactly as long as the
/// job is outstanding.
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
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    /// What a `FakePort` recorded: the id it was given and the bytes it was
    /// asked to post, in order.
    type Posted = Arc<std::sync::Mutex<Vec<(u64, Vec<u8>)>>>;

    /// A port that records what it was handed instead of posting anywhere.
    struct FakePort {
        posted: Posted,
        accept: bool,
    }

    impl WorkerPort for FakePort {
        fn post(&self, id: u64, request: Vec<u8>) -> bool {
            if self.accept {
                self.posted.lock().unwrap().push((id, request));
            }
            self.accept
        }
    }

    fn attach(accept: bool) -> Posted {
        let posted: Posted = Arc::new(std::sync::Mutex::new(Vec::new()));
        set_worker(Box::new(FakePort {
            posted: Arc::clone(&posted),
            accept,
        }));
        posted
    }

    /// Every test shares one thread-local port and pending map, and `cargo
    /// test` runs them concurrently on separate threads — which is precisely
    /// why the state is thread-local. Each test still tears down so a panic
    /// mid-test cannot leak a port into the next one on the same thread.
    fn detach() {
        abandon_worker("test teardown");
    }

    /// A job that is cheap to execute and easy to recognize. It renders
    /// nothing, which is fine: the funnel's contract is about *where* and
    /// *whether* `deliver` runs, not what the renderer drew.
    fn a_job() -> JobRequest {
        JobRequest::Radar {
            input: Box::new(
                RenderInput::from_bytes(&sample_input_bytes()).expect("fixture payload decodes"),
            ),
            values_wanted: true,
        }
    }

    /// The smallest real volume: two sweeps of a handful of radials, under a
    /// VCP that **declares its cuts**.
    ///
    /// The cut table is what the tilt ladder is keyed by, so a fixture without
    /// one can only ever exercise the refusal path in
    /// `rustdar_radar::sampler::VolumeSampler` — which would make every
    /// assertion below about a section or a grid vacuously `None`.
    fn sample_scan() -> nexrad_model::data::Scan {
        use nexrad_model::data::{
            ChannelConfiguration, ElevationCut, PulseWidth, Radial, RadialStatus, Scan, Sweep,
            VolumeCoveragePattern, WaveformType,
        };
        let cut = |angle: f64| {
            ElevationCut::new(
                angle,
                ChannelConfiguration::ConstantPhase,
                WaveformType::CS,
                20.0,
                true,
                true,
                false,
                false,
                1,
                20,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                false,
                0,
                false,
                0,
                false,
                false,
            )
        };
        let sweep = |elevation_number: u8, elevation: f32| {
            let radials = (0..36)
                .map(|i| {
                    Radial::new(
                        0,
                        i,
                        f32::from(i) * 10.0,
                        10.0,
                        RadialStatus::IntermediateRadialData,
                        elevation_number,
                        elevation,
                        Some(nexrad_model::data::MomentData::from_fixed_point(
                            120,
                            0,
                            250,
                            8,
                            2.0,
                            66.0,
                            vec![200; 120],
                        )),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .collect();
            Sweep::new(elevation_number, radials)
        };
        Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                vec![cut(0.5), cut(1.5)],
            ),
            vec![sweep(1, 0.5), sweep(2, 1.5)],
        )
    }

    /// The single-tilt payload the `Radar` job carries.
    fn sample_input_bytes() -> Vec<u8> {
        let scan = sample_scan();
        RenderInput::extract(
            &scan,
            0.5,
            rustdar_radar::types::RadarProduct::Reflectivity,
            35.0,
            -97.0,
            None,
            None,
        )
        .expect("fixture extracts")
        .to_bytes()
    }

    /// A Level III job. The bytes are opaque here on purpose: the framing must
    /// carry an arbitrary tail without a length prefix that could lie about it.
    fn a_level3_job() -> JobRequest {
        JobRequest::Level3 {
            bytes: std::sync::Arc::new(vec![7, 8, 9, 0xFF, 0]),
            product: rustdar_radar::types::RadarProduct::EchoTops,
            radar_lat: 35.0,
            radar_lon: -97.0,
        }
    }

    /// The two-object VIL density job. The two payloads differ in length *and*
    /// in content, so a framing that swapped them, or one that split them at
    /// the wrong offset, cannot round-trip.
    fn a_level3_pair_job() -> JobRequest {
        JobRequest::Level3Pair {
            dvl: std::sync::Arc::new(vec![1, 2, 3]),
            eet: std::sync::Arc::new(vec![4, 5, 6, 7, 0xFF, 0]),
            radar_lat: 35.0,
            radar_lon: -97.0,
        }
    }

    /// The whole-volume payload the two vertical job kinds carry.
    ///
    /// `extract_volume` rather than `extract`, which is the difference between
    /// a section cut from the ladder and one interpolated across the tilts that
    /// did not travel.
    fn a_volume_input() -> RenderInput {
        RenderInput::extract_volume(
            &sample_scan(),
            rustdar_radar::types::RadarProduct::Reflectivity,
            35.0,
            -97.0,
        )
        .expect("the fixture carries reflectivity")
    }

    fn a_section_job() -> JobRequest {
        JobRequest::Section {
            input: Box::new(a_volume_input()),
            request: SectionRequest {
                start: (35.0, -97.5),
                end: (35.4, -96.8),
                top_km_msl: Some(18.0),
                product: rustdar_radar::types::RadarProduct::Reflectivity,
            },
        }
    }

    fn a_voxel_job() -> JobRequest {
        JobRequest::Voxels {
            input: Box::new(a_volume_input()),
            request: VoxelRequest {
                centre: (35.0, -97.0),
                half_width_km: 60.0,
                base_km_msl: 0.0,
                top_km_msl: 15.0,
                product: rustdar_radar::types::RadarProduct::Reflectivity,
                // Small and *asymmetric*, so a decoder that read the three axes
                // in the wrong order does not round-trip.
                shape: VoxelShape {
                    nx: 8,
                    ny: 6,
                    nz: 4,
                },
                values_wanted: true,
            },
        }
    }

    #[test]
    fn every_job_kind_survives_the_wire_format() {
        for job in [
            a_job(),
            a_level3_job(),
            a_level3_pair_job(),
            a_section_job(),
            a_voxel_job(),
        ] {
            assert_eq!(
                JobRequest::from_bytes(&job.to_bytes()),
                Some(job.clone()),
                "{:?} did not survive its round trip",
                job.kind()
            );
        }
    }

    /// The retired SRM job tag must be refused, not resurrected: a worker
    /// from a build that still posts it gets a failed job, never a render of
    /// something this build would compute differently.
    #[test]
    fn the_retired_srm_tag_is_refused() {
        assert_eq!(JobRequest::from_bytes(&[TAG_SRM_RETIRED, 1, 2, 3]), None);
    }

    /// Every tag is distinct, and the two new ones are **not 4**.
    ///
    /// 4 is [`TAG_LEVEL3_PAIR`], and because the new names are new consts
    /// nothing would have stopped the build. Worse, nothing would have stopped
    /// it at *runtime* either: that arm reads two `f64`s and a `u32` length and
    /// then takes the rest, which on a section's plausible bytes succeeds — so
    /// a section posted as tag 4 comes back as a VIL-density job built out of
    /// cross-section geometry, and renders. The assertion below is the whole
    /// guard, and it is cheap because the alternative is invisible.
    #[test]
    fn no_two_job_tags_collide() {
        let tags = [
            TAG_RADAR,
            TAG_LEVEL3,
            TAG_SRM_RETIRED,
            TAG_LEVEL3_PAIR,
            TAG_SECTION,
            TAG_VOXELS,
        ];
        let mut seen = std::collections::HashSet::new();
        for tag in tags {
            assert!(seen.insert(tag), "tag {tag} is used twice");
        }
        assert_ne!(TAG_SECTION, TAG_LEVEL3_PAIR);
        assert_ne!(TAG_VOXELS, TAG_LEVEL3_PAIR);
        // And the framing really is tag-first, so the byte asserted above is
        // the byte a decoder switches on.
        assert_eq!(a_section_job().to_bytes()[0], TAG_SECTION);
        assert_eq!(a_voxel_job().to_bytes()[0], TAG_VOXELS);
    }

    /// Every job tag is pinned to the literal byte it ships as.
    ///
    /// [`no_two_job_tags_collide`] above asserts distinctness, and the round
    /// trip asserts the two ends agree — but **both survive a renumbering**,
    /// because both read the constants they are checking. Swap
    /// [`TAG_LEVEL3_PAIR`]'s 4 with [`TAG_VOXELS`]'s 6 and every tag is still
    /// distinct, every job still round-trips through this build, and the whole
    /// workspace still passes.
    ///
    /// What that costs is already written down above the constants: a job
    /// landing in the `TAG_LEVEL3_PAIR` arm reads two `f64`s and a `u32`
    /// length and then takes the rest, so on another kind's plausible bytes it
    /// *succeeds* and renders a VIL-density product out of the wrong geometry.
    /// The tag is a contract between two builds — a page that renumbers is
    /// talking to workers that did not — so the numbers have to be written
    /// out, not read back.
    #[test]
    fn every_job_tag_is_the_literal_byte_it_ships_as() {
        // Deliberately spelled out. Do not regenerate this from the constants.
        let table: [(&str, u8, u8); 6] = [
            ("TAG_RADAR", TAG_RADAR, 1),
            ("TAG_LEVEL3", TAG_LEVEL3, 2),
            ("TAG_SRM_RETIRED", TAG_SRM_RETIRED, 3),
            ("TAG_LEVEL3_PAIR", TAG_LEVEL3_PAIR, 4),
            ("TAG_SECTION", TAG_SECTION, 5),
            ("TAG_VOXELS", TAG_VOXELS, 6),
        ];
        for (name, actual, expected) in table {
            assert_eq!(
                actual, expected,
                "{name} moved on the wire: it is {actual} now, not {expected}",
            );
        }

        // And the encoder really posts those bytes — the constant could be
        // right while the arm that writes it is not. Every constructible kind,
        // framed against its literal rather than against its own constant.
        let framing: [(JobRequest, u8); 5] = [
            (a_job(), 1),
            (a_level3_job(), 2),
            (a_level3_pair_job(), 4),
            (a_section_job(), 5),
            (a_voxel_job(), 6),
        ];
        for (job, tag) in framing {
            let bytes = job.to_bytes();
            assert_eq!(
                bytes[0],
                tag,
                "{:?} posts tag {}, not {tag} — a worker of another build \
                 decodes it as whatever {} names there",
                job.kind(),
                bytes[0],
                bytes[0],
            );
            assert_eq!(
                JobRequest::from_bytes(&bytes),
                Some(job.clone()),
                "{:?} did not decode back from its own framing",
                job.kind(),
            );
        }

        // The unallocated bytes on either end of the table stay unallocated.
        // A seventh kind added without a line in the table above makes 7
        // decode, and this is what says so.
        let mut bytes = a_voxel_job().to_bytes();
        for unallocated in [0u8, 7] {
            bytes[0] = unallocated;
            assert_eq!(
                JobRequest::from_bytes(&bytes),
                None,
                "tag {unallocated} decodes, so the table above has stopped \
                 being the whole wire",
            );
        }
    }

    /// The product is on the wire twice — in the request geometry and inside
    /// the payload — and a disagreement is refused rather than drawn.
    ///
    /// It has to be refused *here*, because downstream it does not fail:
    /// `VolumeSampler` builds no rung for a moment the payload does not carry,
    /// every sample reads `NoCoverage`, and the result is a full-size,
    /// correctly-shaped raster of clear air — indistinguishable from a section
    /// through genuinely empty sky.
    #[test]
    fn a_request_naming_a_different_product_from_its_payload_is_refused() {
        for (job, product_offset) in [(a_section_job(), 1), (a_voxel_job(), 2)] {
            let mut bytes = job.to_bytes();
            let code = rustdar_radar::types::RadarProduct::Velocity.wire_code();
            bytes[product_offset..product_offset + 2].copy_from_slice(&code.to_le_bytes());
            assert_eq!(
                JobRequest::from_bytes(&bytes),
                None,
                "{}: a request for a moment the payload does not carry was accepted",
                job.kind(),
            );
        }
    }

    /// The vertical jobs' own malformed shapes.
    #[test]
    fn a_malformed_vertical_job_is_refused_rather_than_misread() {
        for job in [a_section_job(), a_voxel_job()] {
            let bytes = job.to_bytes();
            for cut in 1..bytes.len() {
                // Truncation anywhere must be a clean refusal. The tail is a
                // `RenderInput`, which refuses trailing bytes, so unlike the
                // Level III jobs every cut can be asserted rather than merely
                // exercised.
                assert_eq!(
                    JobRequest::from_bytes(&bytes[..cut]),
                    None,
                    "{} truncated to {cut} bytes was accepted",
                    job.kind(),
                );
            }
            // Trailing bytes land inside `RenderInput::from_bytes`, which is
            // exactly why the payload has to be last.
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert_eq!(
                JobRequest::from_bytes(&trailing),
                None,
                "{}: trailing bytes mean the layouts disagree",
                job.kind(),
            );
            // A product code this build does not have.
            let mut bad_product = bytes.clone();
            let at = if matches!(job, JobRequest::Section { .. }) {
                1
            } else {
                2
            };
            bad_product[at] = 0xFE;
            bad_product[at + 1] = 0xFF;
            assert_eq!(JobRequest::from_bytes(&bad_product), None, "product code");
        }

        // The voxel job's `values_wanted` is a bool, not a byte.
        let mut bad_flag = a_voxel_job().to_bytes();
        bad_flag[1] = 2;
        assert_eq!(JobRequest::from_bytes(&bad_flag), None, "values_wanted");

        // And a shape with a zero axis is refused at the boundary rather than
        // deep inside `build_voxels`: a renderer dividing an extent by a zero
        // dimension gets an infinity.
        let bytes = a_voxel_job().to_bytes();
        let shape_at = bytes.len() - a_volume_input().to_bytes().len() - 6;
        for axis in 0..3 {
            let mut zeroed = bytes.clone();
            let at = shape_at + axis * 2;
            zeroed[at] = 0;
            zeroed[at + 1] = 0;
            assert_eq!(
                JobRequest::from_bytes(&zeroed),
                None,
                "a zero axis {axis} was accepted",
            );
        }
        // precondition: the offset arithmetic above really points at the shape,
        // so the assertions are about the guard rather than about corrupting
        // some other field into invalidity.
        let mut same = bytes.clone();
        same[shape_at] = 8;
        same[shape_at + 1] = 0;
        assert_eq!(
            JobRequest::from_bytes(&same),
            Some(a_voxel_job()),
            "the shape is not where this test thinks it is",
        );
    }

    /// The two vertical arms of [`execute`] actually run, end to end, on a
    /// volume with a cut table.
    ///
    /// Without this the wire could round-trip perfectly and `execute` could
    /// answer `None` for both kinds forever — which is what the
    /// `assert_eq!(execute(&…), None)` assertions elsewhere in this module
    /// would look like if they were the only evidence.
    #[test]
    fn the_vertical_jobs_produce_their_own_output_kinds() {
        let section = execute(&a_section_job()).expect("the section job draws");
        assert_eq!(
            section.view(),
            rustdar_radar::types::RenderView::CrossSection
        );
        assert!(section.section().is_some());

        let voxels = execute(&a_voxel_job()).expect("the voxel job builds");
        assert_eq!(voxels.view(), rustdar_radar::types::RenderView::Volume);
        let grid = voxels.voxels().expect("the voxel job answers a grid");
        assert_eq!(grid.shape().cells(), 8 * 6 * 4);

        // And the same jobs off the wire, which is the path a worker takes.
        assert_eq!(
            execute_bytes(&a_section_job().to_bytes()).map(|o| o.view()),
            Some(rustdar_radar::types::RenderView::CrossSection),
        );
        assert_eq!(
            execute_bytes(&a_voxel_job().to_bytes()).map(|o| o.view()),
            Some(rustdar_radar::types::RenderView::Volume),
        );
    }

    /// A frame consumer handed an output of another kind sees `None` — the
    /// "nothing to draw" every render path already handles — and **never** a
    /// wrong-shaped buffer.
    ///
    /// This is the accessor the whole widening rests on. `RenderedFrame` was
    /// deliberately not given a width and a height, so every consumer of one
    /// still assumes `IMAGE_SIZE`; the assumption survives only because a
    /// section cannot reach those consumers, and this is what says so.
    #[test]
    fn a_frame_consumer_sees_nothing_rather_than_another_kinds_buffers() {
        let section = execute(&a_section_job()).expect("the section job draws");
        assert_eq!(section.frame(), None);
        let voxels = execute(&a_voxel_job()).expect("the voxel job builds");
        assert_eq!(voxels.frame(), None);
        // And the frame arm still yields its frame, so the accessor is not
        // simply always `None`.
        assert!(
            execute(&a_job())
                .and_then(JobOutput::frame)
                .is_some_and(|f| !f.image.is_empty()),
        );
        // The two vertical accessors are equally narrow.
        assert!(execute(&a_job()).and_then(JobOutput::section).is_none());
        assert!(execute(&a_job()).and_then(JobOutput::voxels).is_none());
        assert!(
            execute(&a_section_job())
                .and_then(JobOutput::voxels)
                .is_none()
        );
    }

    /// The worker reply's non-frame half, both directions.
    ///
    /// This is the whole `OUT` field: `rustdar-web` copies bytes out of a
    /// `Uint8Array` and hands them here with the kind tag, so everything that
    /// can go wrong with that field can be exercised on a host.
    #[test]
    fn an_out_of_band_payload_round_trips_and_refuses_what_it_should() {
        use rustdar_radar::types::RenderView;

        let section = execute(&a_section_job())
            .and_then(JobOutput::section)
            .expect("the section job draws");
        let grid = execute(&a_voxel_job())
            .and_then(JobOutput::voxels)
            .expect("the voxel job builds");

        let section_bytes = section.to_bytes();
        let grid_bytes = grid.to_bytes();
        assert_eq!(
            decode_output(RenderView::CrossSection.wire_code(), &section_bytes),
            Some(JobOutput::Section(section)),
        );
        assert_eq!(
            decode_output(RenderView::Volume.wire_code(), &grid_bytes),
            Some(JobOutput::Voxels(grid)),
        );

        // A kind byte this build does not have.
        assert_eq!(decode_output(0, &section_bytes), None);
        assert_eq!(decode_output(u8::MAX, &section_bytes), None);
        // A frame does not travel this way; a reply claiming it does is from a
        // build whose protocol is not this one.
        assert_eq!(
            decode_output(RenderView::PlanView.wire_code(), &section_bytes),
            None,
        );
        // The two payload codecs each have their own magic, so the tag naming
        // the wrong decoder is a refusal rather than a reinterpretation.
        assert_eq!(
            decode_output(RenderView::Volume.wire_code(), &section_bytes),
            None,
        );
        assert_eq!(
            decode_output(RenderView::CrossSection.wire_code(), &grid_bytes),
            None,
        );
        assert_eq!(
            decode_output(RenderView::CrossSection.wire_code(), &[]),
            None
        );
    }

    /// **The invariant the render budget depends on: every `deliver` sends on
    /// its channel on every arm, including the wrong-kind arm.**
    ///
    /// A pane takes a render slot and an in-flight mark when it dispatches, and
    /// only `deliver` running unwinds them. A wrong-kind result that returned
    /// early instead of delivering would leak one slot per occurrence, and with
    /// `MAX_CONCURRENT_RENDERS` at **1 on wasm** the first leak stops every
    /// render in the tab, permanently — the pane wedges with no error.
    #[test]
    fn a_job_answered_with_the_wrong_output_kind_still_delivers() {
        for job in [a_section_job(), a_voxel_job()] {
            let kind = job.kind();
            detach();
            let (tx, rx) = mpsc::channel();
            // The consumer is shaped for a frame — the shape both production
            // `offload_job` callers have — and the job answers a section.
            offload_job("test", Job::Described(job), move |output| {
                let _ = tx.send(output.and_then(JobOutput::frame).is_some());
            });
            assert_eq!(
                rx.recv_timeout(std::time::Duration::from_secs(10)),
                Ok(false),
                "{kind}: a wrong-kind result did not reach deliver, so the \
                 render budget just leaked a slot",
            );
        }

        // The same across the worker boundary, where the reply is what carries
        // the result: `abandon_worker` must fail a posted vertical job too.
        let posted = attach(true);
        let (tx, rx) = mpsc::channel();
        offload_job("test", Job::Described(a_section_job()), move |output| {
            let _ = tx.send(output.is_some());
        });
        assert_eq!(posted.lock().unwrap().len(), 1);
        abandon_worker("test");
        assert_eq!(
            rx.try_recv(),
            Ok(false),
            "a posted section job the worker never answered was forgotten \
             rather than failed",
        );
        assert_eq!(jobs_in_worker(), 0);
    }

    #[test]
    fn a_malformed_job_is_refused_rather_than_misread() {
        assert_eq!(JobRequest::from_bytes(&[]), None, "empty");
        assert_eq!(JobRequest::from_bytes(&[0xFF, 1, 2]), None, "unknown tag");
        assert_eq!(JobRequest::from_bytes(&[TAG_RADAR]), None, "no flag");
        assert_eq!(JobRequest::from_bytes(&[TAG_RADAR, 1]), None, "no payload");
        assert_eq!(
            JobRequest::from_bytes(&[TAG_RADAR, 2]),
            None,
            "the flag is a bool, not a byte"
        );

        // A length prefix that claims more than the payload holds must be
        // refused, not read as a short object: the pair's first length is the
        // one number on the wire that could lie.
        let mut overlong = a_level3_pair_job().to_bytes();
        overlong[17] = 0xFF;
        assert_eq!(
            JobRequest::from_bytes(&overlong),
            None,
            "a DVL length past the end of the payload",
        );

        // A truncated header must not be read as a short one. The variable tail
        // is whatever is left, so only the fixed part can be checked this way.
        for job in [a_job(), a_level3_job(), a_level3_pair_job()] {
            let bytes = job.to_bytes();
            for cut in 1..bytes.len().min(20) {
                let _ = JobRequest::from_bytes(&bytes[..cut]);
            }
            assert_eq!(
                JobRequest::from_bytes(&bytes[..1]),
                None,
                "a tag with no header must be refused"
            );
        }

        let mut bad_product = a_level3_job().to_bytes();
        bad_product[1] = 0xFE;
        bad_product[2] = 0xFF;
        assert_eq!(
            JobRequest::from_bytes(&bad_product),
            None,
            "a product code this build does not have"
        );
    }

    /// A Level III payload that does not decode is a render that drew nothing,
    /// not a panic — the bytes come off a message port.
    #[test]
    fn an_undecodable_level3_payload_renders_nothing() {
        assert_eq!(execute(&a_level3_job()), None);
        assert_eq!(
            execute(&a_level3_pair_job()),
            None,
            "neither object of the pair decodes",
        );
    }

    /// With no worker installed, `offload_job` is the old behaviour: the job
    /// runs and `deliver` sees its result.
    #[test]
    fn without_a_worker_the_job_runs_here() {
        detach();
        let (tx, rx) = mpsc::channel();
        offload_job("test", Job::Described(a_job()), move |result| {
            let _ = tx.send(result.is_some());
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)),
            Ok(true),
            "the inline arm must deliver the rendered frame"
        );
        assert_eq!(jobs_in_worker(), 0);
    }

    /// With a worker, nothing runs here — the job is posted and `deliver` waits
    /// for the reply.
    #[test]
    fn with_a_worker_the_job_is_posted_and_deferred() {
        let posted = attach(true);
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        offload_job("test", Job::Described(a_job()), move |_| {
            flag.store(true, Ordering::Relaxed)
        });

        assert_eq!(posted.lock().unwrap().len(), 1, "the job should be posted");
        assert_eq!(jobs_in_worker(), 1);
        assert!(
            !ran.load(Ordering::Relaxed),
            "deliver must wait for a reply"
        );

        let id = posted.lock().unwrap()[0].0;
        deliver_worker_reply(id, None);
        assert!(ran.load(Ordering::Relaxed), "the reply must reach deliver");
        assert_eq!(jobs_in_worker(), 0, "the pending entry must be retired");
        detach();
    }

    /// The cancellation contract, across the worker boundary. `deliver` carries
    /// the pane's flag, so a render abandoned while the worker held it must
    /// deliver nothing — exactly as the inline arm's `wanted` check does.
    #[test]
    fn a_reply_to_an_abandoned_render_is_not_delivered() {
        let posted = attach(true);
        let wanted = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel();
        let flag = Arc::clone(&wanted);
        offload_job("test", Job::Described(a_job()), move |result| {
            if result.is_some() && flag.load(Ordering::Relaxed) {
                let _ = tx.send(());
            }
        });

        // Two references while the job is outstanding: the pane's list and the
        // one inside `deliver`. That is what `want_result`'s pruning reads.
        assert_eq!(Arc::strong_count(&wanted), 2);

        wanted.store(false, Ordering::Relaxed);
        let id = posted.lock().unwrap()[0].0;
        deliver_worker_reply(
            id,
            Some(JobOutput::Frame(RenderedFrame {
                image: vec![0; 4],
                max_range_km: 230.0,
                values: vec![f32::NAN],
            })),
        );

        assert!(rx.try_recv().is_err(), "an abandoned render must not send");
        assert_eq!(
            Arc::strong_count(&wanted),
            1,
            "retiring the job must drop deliver's reference, or want_result never prunes"
        );
        detach();
    }

    /// A worker that dies owes replies that will never come. Those jobs have to
    /// be failed, not forgotten: `deliver` holds the render budget's guard and
    /// the pane's in-flight mark.
    #[test]
    fn losing_the_worker_fails_every_job_it_owed() {
        attach(true);
        let (tx, rx) = mpsc::channel();
        offload_job("test", Job::Described(a_job()), move |result| {
            let _ = tx.send(result.is_some());
        });
        assert_eq!(jobs_in_worker(), 1);

        abandon_worker("test");
        assert_eq!(rx.try_recv(), Ok(false), "the owed job must be failed");
        assert_eq!(jobs_in_worker(), 0);
        assert!(!worker_attached());
    }

    /// A port that will not take the job must not strand it.
    #[test]
    fn a_refused_post_runs_the_job_here() {
        detach();
        attach(false);
        let (tx, rx) = mpsc::channel();
        offload_job("test", Job::Described(a_job()), move |result| {
            let _ = tx.send(result.is_some());
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)),
            Ok(true),
            "a refused post must fall back to running here"
        );
        assert_eq!(jobs_in_worker(), 0);
        detach();
    }

    /// A reply nobody is waiting for — the job was already failed by
    /// `abandon_worker` — must be dropped, not delivered a second time.
    #[test]
    fn a_reply_for_a_retired_job_is_ignored() {
        let posted = attach(true);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        offload_job("test", Job::Described(a_job()), move |_| {
            seen.fetch_add(1, Ordering::Relaxed);
        });
        let id = posted.lock().unwrap()[0].0;

        abandon_worker("test");
        assert_eq!(count.load(Ordering::Relaxed), 1);
        deliver_worker_reply(id, None);
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "a late reply must not deliver a second response for one render"
        );
    }
}
