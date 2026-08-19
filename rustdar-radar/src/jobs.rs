//! The six radar codec rows — the radar half of the job boundary, beside the
//! pipeline the rows run (WO-M7.1).
//!
//! Each row is a [`JobSpec`] whose `In` is its own typed input struct —
//! [`RadarPlanJob`] and its five siblings, the shapes of the frontend's
//! `JobRequest` variants minus the envelope — monomorphized into a
//! [`JobCodec`] in [`JOB_CODECS`]. The codec bodies moved here **verbatim**
//! (post-WO-M6.3 text) from `rustdar_frontend::offload`, which keeps
//! byte-identical duplicates until WO-M7.2 flips the frontend onto this
//! table and deletes them — until that flip nothing routes through this
//! module, and the frontend's framing digests over the duplicate bodies are
//! the byte gate the flip must pass.
//!
//! **The tag byte stays with the caller**, as the overlay rows' code byte
//! does: a row's payload is its `JobRequest` arm's bytes minus the leading
//! `TAG_*` byte. Codes are frontend-owned until WO-M7b's dense flip, and
//! the row order in [`JOB_CODECS`] — radar, level3, level3/vild, section,
//! voxels, decode — is load-bearing for exactly that flip, which assigns
//! wire codes by composed-registry index.
//!
//! **The side ceiling is envelope, not input.** The frontend's `execute`
//! reads `side_ceiling_px` once off the request so its rasterizing arms
//! cannot come to disagree about how large a picture a job was allowed to
//! make; here the same property is structural — no input struct carries the
//! field, `run` reads it only from the [`JobGeometry`], and the three
//! raster rows' decodes fill the envelope from the sparse-era wire position
//! their legacy layouts put it at (`level3`/`level3/vild` at offset 1,
//! `radar` after `values_wanted`). **The interleaves are row-owned until
//! WO-M7b**: bytes must not move before the dense flip re-bases the framing
//! digests, so the rows write today's layouts exactly.
//!
//! **The frame rows carry no reply codec yet.** `radar`, `level3` and
//! `level3/vild` ride [`JobCodec::of`] — their replies still cross the
//! browser port as the legacy named fields, and WO-M7c is where the frame
//! reply joins the OUT codec. The section, voxel and decode rows' outputs
//! already have wire forms of their own ([`CrossSection`], [`VoxelGrid`]
//! and [`DecodedScan`] `to_bytes`/`from_bytes`), which their [`JobOutCodec`]
//! halves delegate to unchanged.
//!
//! **The refusal contract**, shared by every `decode` here: `None` for a
//! flag, product code or enum byte outside this build's values, a payload
//! whose two product statements disagree ([`agree_on_product`]), a voxel
//! shape the budget cannot afford, or a buffer shorter than its layout
//! claims. The variable-length tails ride last — `RenderInput::from_bytes`
//! refuses trailing bytes, and the archive rows' tails are the buffer's own
//! remainder — so no length prefix can lie about them.
//!
//! **No clock.** Nothing here may read one — a worker that did would render
//! a picture the direct call would not; WO-M7.2 lands the grep ratchet that
//! pins this for every jobs module.

use std::sync::Arc;

use rustdar_source::job::{EncodeCtx, JobCodec, JobCost, JobGeometry, JobOutCodec, JobSpec};
use rustdar_source::wire::Reader;

use crate::frame::RenderedFrame;
use crate::render_input::RenderInput;
use crate::scan::DecodedScan;
use crate::types::RadarProduct;
use crate::voxel::{HalfExtentKm, VoxelGrid, VoxelRequest, VoxelShape};
use crate::xsect::{CrossSection, SectionRequest};

/// The six radar rows, in dispatch order: **radar, level3, level3/vild,
/// section, voxels, decode**. The order is load-bearing — WO-M7b's dense
/// code flip assigns wire codes by index into the composed registry — and
/// the labels are the shipped kind strings the frontend's `kind()` prints,
/// byte for byte (`radar`'s per-product `"radar/nrot"`/`"radar/srv"`
/// refinements are a `kind()` nicety over the same row, not row labels).
///
/// The three frame rows ride [`JobCodec::of`] — no reply codec until
/// WO-M7c — and the three whose outputs already have wire forms ride
/// [`JobCodec::with_out`].
pub static JOB_CODECS: &[JobCodec] = &[
    JobCodec::of::<RadarPlanJob>(),
    JobCodec::of::<Level3Job>(),
    JobCodec::of::<Level3PairJob>(),
    JobCodec::with_out::<SectionJob>(),
    JobCodec::with_out::<VoxelJob>(),
    JobCodec::with_out::<DecodeJob>(),
];

/// Rasterize a Level II frame.
///
/// The shape of the frontend's `JobRequest::Radar` arm minus its
/// `side_ceiling_px`: the ceiling is envelope, not input — `run` reads it
/// from the [`JobGeometry`] and the row's sparse-era wire interleave
/// carries it (see the module doc).
#[derive(Debug, PartialEq)]
pub struct RadarPlanJob {
    /// Boxed because a `RenderInput` owns its gate bytes and is the largest
    /// thing in the request by three orders of magnitude.
    pub input: Box<RenderInput>,
    /// Whether the caller wants the numbers behind the gates, or only the
    /// geometry of where they are.
    ///
    /// Static pane renders want both — the numbers are what a hover reads.
    /// A **loop frame** wants only the geometry: 5.03 MiB of values for the
    /// widest sweep, across a loop of up to 36 frames, is not affordable and
    /// does not have to be paid, because the volume the frame was rendered
    /// from is resident for as long as the loop lives and the wedges are
    /// what turn a point back into a gate of it. See
    /// [`crate::hover::SweepGates`].
    ///
    /// It used to mean the `side²` `f32` raster grid, which no longer
    /// leaves `rustdar-radar` on any path — see [`RenderedFrame::polar`].
    /// The geometry is kept on both settings; only the values are dropped,
    /// and the texture is unaffected either way.
    pub values_wanted: bool,
}

rustdar_source::impl_job_input!(RadarPlanJob);

impl JobSpec for RadarPlanJob {
    type In = RadarPlanJob;
    type Out = RenderedFrame;
    const LABEL: &'static str = "radar";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &RadarPlanJob, ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.push(u8::from(input.values_wanted));
        // The ceiling comes off the envelope — the input carries no copy —
        // and rides after `values_wanted`, at the offset the legacy layout
        // put it (row-owned until WO-M7b).
        out.extend_from_slice(&ctx.geometry.side_ceiling_px.to_le_bytes());
        out.extend_from_slice(&input.input.to_bytes());
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(RadarPlanJob, JobGeometry)> {
        let values_wanted = flag(r.u8()?)?;
        let side_ceiling_px = r.u32()?;
        let input = RenderInput::from_bytes(r.rest())?;
        Some((
            RadarPlanJob {
                input: Box::new(input),
                values_wanted,
            },
            JobGeometry {
                side_ceiling_px,
                ..geo
            },
        ))
    }

    fn run(input: &RadarPlanJob, geo: &JobGeometry) -> Option<RenderedFrame> {
        crate::render::render_from_sized(&input.input, geo.side_ceiling_px as usize).map(|render| {
            let mut frame = RenderedFrame::from(render);
            if !input.values_wanted {
                // A loop frame keeps its geometry and drops its numbers. 5.03
                // MiB apiece across a loop of up to 36 frames is not affordable
                // and does not have to be paid: the volume the frame was
                // rendered from is resident for as long as the loop lives, and
                // 5.8 KiB of wedges is what turns a point back into a gate of
                // it. See `crate::hover::SweepGates`.
                //
                // A *Level II* loop frame is the one render that reaches this
                // arm. The Level III loop has no `values_wanted` to reach it by
                // and strips at the consumer instead; `app_fetch`'s `deliver`
                // is the site, and stripping an already-stripped field there is
                // what makes the one call safe for both.
                frame.polar.strip_values();
            }
            frame
        })
    }
}

/// Rasterize a Level III radial product.
///
/// The product's *bytes*, not its decoded form: a `Level3Message` holds
/// run-length radial packets with no serde derives anywhere in the graph,
/// and re-decoding is both cheap against the render and a use of the one
/// decoder rather than a second description of the format. The decode moves
/// off the main thread with the render as a result.
#[derive(Debug, PartialEq)]
pub struct Level3Job {
    pub bytes: Arc<Vec<u8>>,
    pub product: RadarProduct,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

rustdar_source::impl_job_input!(Level3Job);

impl JobSpec for Level3Job {
    type In = Level3Job;
    type Out = RenderedFrame;
    const LABEL: &'static str = "level3";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &Level3Job, ctx: &EncodeCtx, out: &mut Vec<u8>) {
        // The ceiling first, off the envelope, at offset 1 of the legacy
        // layout (row-owned until WO-M7b).
        out.extend_from_slice(&ctx.geometry.side_ceiling_px.to_le_bytes());
        out.extend_from_slice(&input.product.wire_code().to_le_bytes());
        out.extend_from_slice(&input.radar_lat.to_le_bytes());
        out.extend_from_slice(&input.radar_lon.to_le_bytes());
        out.extend_from_slice(&input.bytes);
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(Level3Job, JobGeometry)> {
        let side_ceiling_px = r.u32()?;
        Some((
            Level3Job {
                product: RadarProduct::from_wire_code(r.u16()?)?,
                radar_lat: r.f64()?,
                radar_lon: r.f64()?,
                bytes: Arc::new(r.rest().to_vec()),
            },
            JobGeometry {
                side_ceiling_px,
                ..geo
            },
        ))
    }

    fn run(input: &Level3Job, geo: &JobGeometry) -> Option<RenderedFrame> {
        decode_level3(&input.bytes).and_then(|message| {
            crate::render::render_level3_message_to_image_sized(
                &message,
                input.product,
                input.radar_lat,
                input.radar_lon,
                geo.side_ceiling_px as usize,
            )
            .map(RenderedFrame::from)
        })
    }
}

/// Rasterize a Level III product **derived from two objects of the same
/// volume**: VIL density, Digital VIL over Enhanced Echo Tops
/// ([`crate::vild`]).
///
/// A second row rather than a `Vec<Arc<Vec<u8>>>` on the one above: the
/// two objects are not interchangeable — the first is the numerator and the
/// second the denominator — and a positional pair says so where a list
/// would leave it to a comment. The bytes travel for the same reason
/// [`Level3Job`]'s do.
#[derive(Debug, PartialEq)]
pub struct Level3PairJob {
    pub dvl: Arc<Vec<u8>>,
    pub eet: Arc<Vec<u8>>,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

rustdar_source::impl_job_input!(Level3PairJob);

impl JobSpec for Level3PairJob {
    type In = Level3PairJob;
    type Out = RenderedFrame;
    const LABEL: &'static str = "level3/vild";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &Level3PairJob, ctx: &EncodeCtx, out: &mut Vec<u8>) {
        // The ceiling first, off the envelope, at offset 1 of the legacy
        // layout (row-owned until WO-M7b). The first object is
        // length-prefixed and the second takes the rest, so neither length
        // can lie about the other.
        out.extend_from_slice(&ctx.geometry.side_ceiling_px.to_le_bytes());
        out.extend_from_slice(&input.radar_lat.to_le_bytes());
        out.extend_from_slice(&input.radar_lon.to_le_bytes());
        out.extend_from_slice(&(input.dvl.len() as u32).to_le_bytes());
        out.extend_from_slice(&input.dvl);
        out.extend_from_slice(&input.eet);
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(Level3PairJob, JobGeometry)> {
        let side_ceiling_px = r.u32()?;
        let radar_lat = r.f64()?;
        let radar_lon = r.f64()?;
        let dvl_len = r.u32()? as usize;
        Some((
            Level3PairJob {
                radar_lat,
                radar_lon,
                dvl: Arc::new(r.take(dvl_len)?.to_vec()),
                eet: Arc::new(r.rest().to_vec()),
            },
            JobGeometry {
                side_ceiling_px,
                ..geo
            },
        ))
    }

    fn run(input: &Level3PairJob, geo: &JobGeometry) -> Option<RenderedFrame> {
        match (decode_level3(&input.dvl), decode_level3(&input.eet)) {
            (Some(dvl), Some(eet)) => crate::render::render_derived_vild_to_image_sized(
                &dvl,
                &eet,
                input.radar_lat,
                input.radar_lon,
                geo.side_ceiling_px as usize,
            )
            .map(RenderedFrame::from),
            // One of the two did not decode, which `decode_level3` has already
            // logged: nothing to draw, the same answer a missing sweep gets.
            _ => None,
        }
    }
}

/// Draw a vertical cross-section through a volume.
///
/// The geometry rides here rather than on the [`RenderInput`]: a section's
/// endpoints are not a render parameter *of reflectivity*, and a
/// `RenderInput` carrying them would make every plan-view payload's bytes
/// depend on where somebody last drew a line.
///
/// The `input` is a [`RenderInput::extract_volume`] payload — every tilt
/// carrying the moment, and the cut table that keys them.
#[derive(Debug, PartialEq)]
pub struct SectionJob {
    pub input: Box<RenderInput>,
    pub request: SectionRequest,
}

rustdar_source::impl_job_input!(SectionJob);

impl JobSpec for SectionJob {
    type In = SectionJob;
    type Out = CrossSection;
    const LABEL: &'static str = "section";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &SectionJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        encode_section_request(out, &input.request);
        // The `RenderInput` goes **last**, because `RenderInput::from_bytes`
        // refuses trailing bytes: it has to be handed exactly the remainder,
        // so nothing may follow it.
        out.extend_from_slice(&input.input.to_bytes());
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(SectionJob, JobGeometry)> {
        let request = decode_section_request(r)?;
        let input = RenderInput::from_bytes(r.rest())?;
        agree_on_product(request.product, &input)?;
        Some((
            SectionJob {
                input: Box::new(input),
                request,
            },
            geo,
        ))
    }

    // The `Scan` is rebuilt from the payload and dropped again here, which
    // is the same shape the `radar` row has: one renderer, run wherever the
    // job landed, rather than a worker-side reimplementation that could
    // come to disagree with the main thread's.
    // The storm motion override rides the `RenderInput` — the lane the
    // plan-view SRV render already uses — and is threaded here into the
    // derivation seam both vertical renderers share. The RPG's own vector
    // rides the same payload, one field over, and is threaded through
    // beside it: the two are rungs of one chain and the derivation is what
    // arbitrates between them, so a caller that passed only the override
    // would silently demote every vertical SRV cut to a derived rung while
    // the map beside it used the RPG's.
    //
    // So does the declared Nyquist table, and it has to be lifted back out
    // separately: `to_scan` rebuilds model types, and the model type is
    // precisely what dropped the number. Pairing the two here is what
    // keeps this thread's velocity fold guard on the same limits the
    // thread that extracted the payload used.
    fn run(input: &SectionJob, _geo: &JobGeometry) -> Option<CrossSection> {
        let (scan, declared) = (input.input.to_scan(), input.input.declared_nyquist());
        crate::xsect::render_section(
            crate::nyquist::Volume::new(&scan, &declared),
            &input.request,
            input.input.radar_lat(),
            input.input.radar_lon(),
            input.input.storm_motion(),
        )
    }
}

impl JobOutCodec for SectionJob {
    // Delegating to the type's own wire form keeps one description of the
    // layout; the `to_bytes` Vec is copied into `out` to keep that moved
    // signature exact — WO-M7c, which makes this path live, may flatten it.
    fn encode_out(v: &CrossSection, out: &mut Vec<u8>) {
        out.extend_from_slice(&v.to_bytes());
    }

    fn decode_out(bytes: &[u8]) -> Option<CrossSection> {
        CrossSection::from_bytes(bytes)
    }
}

/// The reply half of the job boundary's erasure seam: a described section
/// render answers this type through the codec rows in this module.
impl rustdar_source::job::JobOut for CrossSection {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

/// Resample a volume into a Cartesian grid for a raymarch.
#[derive(Debug, PartialEq)]
pub struct VoxelJob {
    pub input: Box<RenderInput>,
    pub request: VoxelRequest,
}

rustdar_source::impl_job_input!(VoxelJob);

impl JobSpec for VoxelJob {
    type In = VoxelJob;
    type Out = VoxelGrid;
    const LABEL: &'static str = "voxels";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &VoxelJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        encode_voxel_request(out, &input.request);
        // The `RenderInput` goes **last**, because `RenderInput::from_bytes`
        // refuses trailing bytes: it has to be handed exactly the remainder,
        // so nothing may follow it.
        out.extend_from_slice(&input.input.to_bytes());
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(VoxelJob, JobGeometry)> {
        let request = decode_voxel_request(r)?;
        let input = RenderInput::from_bytes(r.rest())?;
        agree_on_product(request.product, &input)?;
        Some((
            VoxelJob {
                input: Box::new(input),
                request,
            },
            geo,
        ))
    }

    fn run(input: &VoxelJob, _geo: &JobGeometry) -> Option<VoxelGrid> {
        let (scan, declared) = (input.input.to_scan(), input.input.declared_nyquist());
        crate::voxel::build_voxels_with_motion(
            crate::nyquist::Volume::new(&scan, &declared),
            &input.request,
            input.input.radar_lat(),
            input.input.radar_lon(),
            input.input.storm_motion(),
        )
    }
}

impl JobOutCodec for VoxelJob {
    fn encode_out(v: &VoxelGrid, out: &mut Vec<u8>) {
        out.extend_from_slice(&v.to_bytes());
    }

    fn decode_out(bytes: &[u8]) -> Option<VoxelGrid> {
        VoxelGrid::from_bytes(bytes)
    }
}

/// The reply half of the job boundary's erasure seam: a described voxel
/// build answers this type through the codec rows in this module.
impl rustdar_source::job::JobOut for VoxelGrid {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

/// **Decode a downloaded Level II archive volume.**
///
/// The one row here that does not rasterize anything, and the one whose
/// input is not already a decoded volume — it is what *produces* the
/// volume every other row is built from.
///
/// It is a job for the reason the renders are: on the web the work has to
/// happen somewhere that is not the one thread the browser has.
/// [`crate::scan`]'s own doc predicted this — the walk is paid "on
/// cold start, on every timeline scrub, on every 'next scan', and once per
/// frame of a loop download … and on the web it is paid on the browser's
/// main thread" — and the frame-thread audit put a number on it: **1021.9
/// ms in Firefox 153 and 911.4 ms in Chrome 151** for a 16.9 MB, 21-sweep
/// volume, against 42–66 ms on a native thread pool. Nothing else this
/// application does blocks a frame for a second.
///
/// # The bytes, not a `File`
///
/// `nexrad_data::volume::File` owns a `Vec<u8>` and nothing else, so the
/// archive bytes *are* the job's input and no wrapper has to cross. They
/// arrive here straight off the download, which is the split this row
/// exists to make: the network half belongs to whoever has the fetch stack
/// and stays on the async task, and the CPU half comes here.
///
/// `Arc` so that the dispatch site — which may hold the bytes for a retry —
/// does not have to hand over its only copy, and so a clone costs a
/// refcount rather than 16 MB.
#[derive(Debug, PartialEq)]
pub struct DecodeJob {
    pub archive: Arc<Vec<u8>>,
}

rustdar_source::impl_job_input!(DecodeJob);

impl JobSpec for DecodeJob {
    type In = DecodeJob;
    type Out = DecodedScan;
    const LABEL: &'static str = "decode";
    const COST: JobCost = JobCost::VolumeDecode;

    fn encode(input: &DecodeJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        // The archive takes the rest: an archive volume has no framing this
        // needs to know about, and a length prefix would be a second
        // statement of a length the buffer already has.
        out.extend_from_slice(&input.archive);
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(DecodeJob, JobGeometry)> {
        Some((
            DecodeJob {
                archive: Arc::new(r.rest().to_vec()),
            },
            geo,
        ))
    }

    // The one row that produces a volume rather than consuming one. It is
    // also the one row whose input is bigger than a pointer: `File::new`
    // takes the archive by value, so the bytes are cloned out of the `Arc`
    // here. That is one 16 MB memcpy against a decode of ~1000 ms in a
    // browser, and it happens wherever the job ran rather than on the
    // thread that asked for it.
    fn run(input: &DecodeJob, _geo: &JobGeometry) -> Option<DecodedScan> {
        match crate::scan::decode_bytes(input.archive.as_ref().clone()) {
            Ok(volume) => Some(volume),
            Err(e) => {
                // "Nothing to draw", which is what every other row's
                // failure already means, and what the caller's `deliver`
                // already handles: the fetch reports it and the pane keeps
                // whatever it had.
                log::error!("could not decode a Level II volume: {e}");
                None
            }
        }
    }
}

impl JobOutCodec for DecodeJob {
    fn encode_out(v: &DecodedScan, out: &mut Vec<u8>) {
        out.extend_from_slice(&v.to_bytes());
    }

    fn decode_out(bytes: &[u8]) -> Option<DecodedScan> {
        DecodedScan::from_bytes(bytes)
    }
}

/// The reply half of the job boundary's erasure seam: a described archive
/// decode answers this type through the codec rows in this module.
impl rustdar_source::job::JobOut for DecodedScan {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
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
/// request's field from it at decode — was rejected because it makes the
/// request not round-trip: a caller who built an inconsistent pair would
/// get a *different* request back rather than a refusal, which moves the
/// disagreement from the wire into the type.
fn agree_on_product(wanted: RadarProduct, input: &RenderInput) -> Option<()> {
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
    let product = RadarProduct::from_wire_code(r.u16()?)?;
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
    let product = RadarProduct::from_wire_code(r.u16()?)?;
    let request = VoxelRequest {
        centre: (r.f64()?, r.f64()?),
        half_extent_km: match r.u8()? {
            0 => None,
            1 => Some(HalfExtentKm {
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
    let affordable = request.shape.cells() <= crate::voxel::VOXEL_TEXTURE_BUDGET_BYTES;
    (request.shape.is_supported() && affordable).then_some(request)
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

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_source::job::DescribedJob;

    /// The smallest real volume: two sweeps of a handful of radials, under a
    /// VCP that **declares its cuts** — a value-mirror of the frontend's
    /// `offload::tests` fixture, so the rows here round-trip exactly the
    /// payloads whose framed bytes the frontend's digests pin.
    ///
    /// The cut table is what the tilt ladder is keyed by, so a fixture without
    /// one can only ever exercise the refusal path in
    /// [`crate::sampler::VolumeSampler`] — which would make every assertion
    /// below about a section or a grid vacuously `None`.
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

    /// The single-tilt payload the `radar` row carries.
    fn a_plan_input() -> RenderInput {
        RenderInput::extract(
            &sample_scan(),
            0.5,
            RadarProduct::Reflectivity,
            35.0,
            -97.0,
            None,
            None,
        )
        .expect("fixture extracts")
    }

    /// The whole-volume payload the two vertical rows carry.
    ///
    /// `extract_volume` rather than `extract`, which is the difference between
    /// a section cut from the ladder and one interpolated across the tilts
    /// that did not travel.
    fn a_volume_input() -> RenderInput {
        RenderInput::extract_volume(&sample_scan(), RadarProduct::Reflectivity, 35.0, -97.0)
            .expect("the fixture carries reflectivity")
    }

    /// The envelope the frontend's dispatch hands the rows at WO-M7.2:
    /// distinctive width/height/bounds the radar rows must pass through
    /// untouched, and the ceiling under test in `side_ceiling_px`.
    fn geometry_with_ceiling(side_ceiling_px: u32) -> JobGeometry {
        JobGeometry {
            width: 64,
            height: 32,
            bounds: rustdar_source::geo::GeoBounds {
                min_lat: 30.0,
                max_lat: 40.0,
                min_lon: -100.0,
                max_lon: -90.0,
            },
            side_ceiling_px,
        }
    }

    /// Encode `job` through `row` under an envelope carrying `ceiling`,
    /// decode it back under an envelope that does NOT carry it (the WO-M7.2
    /// caller's shape: the shared header fills width/height/bounds and the
    /// radar row fills the ceiling from its own sparse-era wire position),
    /// and require the identity: the decoded job equals the original and the
    /// returned envelope is the encode-side one — the ceiling filled,
    /// everything else passed through.
    ///
    /// No cursor assertion: every radar row's payload ends in a tail that
    /// takes the rest, so completeness is proven by value equality — a stray
    /// trailing byte lands inside the tail (or is refused by
    /// `RenderInput::from_bytes`) and fails the equality either way.
    fn assert_round_trips_filling_the_ceiling(row: &JobCodec, job: &DescribedJob, ceiling: u32) {
        let mut bytes = Vec::new();
        (row.encode)(
            job,
            &EncodeCtx {
                geometry: geometry_with_ceiling(ceiling),
            },
            &mut bytes,
        );
        let mut r = Reader::new(&bytes);
        let (decoded, geo_out) = (row.decode)(&mut r, geometry_with_ceiling(0))
            .expect("a row must decode its own encode");
        assert_eq!(
            &decoded, job,
            "decode ∘ encode must be the identity for `{}`",
            row.label,
        );
        assert_eq!(
            geo_out,
            geometry_with_ceiling(ceiling),
            "`{}` fills the envelope's ceiling from the wire and passes the \
             rest through",
            row.label,
        );
    }

    /// The harness for the rows with no ceiling on the wire (`section`,
    /// `voxels`, `decode`): the geometry passes through unchanged — their
    /// effective ceiling is 0, exactly what the frontend's
    /// `side_ceiling_px()` has always answered for them.
    fn assert_round_trips_passing_geo_through(row: &JobCodec, job: &DescribedJob) {
        let geo = geometry_with_ceiling(0);
        let mut bytes = Vec::new();
        (row.encode)(job, &EncodeCtx { geometry: geo }, &mut bytes);
        let mut r = Reader::new(&bytes);
        let (decoded, geo_out) =
            (row.decode)(&mut r, geo).expect("a row must decode its own encode");
        assert_eq!(
            &decoded, job,
            "decode ∘ encode must be the identity for `{}`",
            row.label,
        );
        assert_eq!(
            geo_out, geo,
            "`{}` amends nothing: the geometry passes through unchanged",
            row.label,
        );
    }

    #[test]
    fn the_registry_is_the_six_rows_in_dispatch_order() {
        assert_eq!(
            JOB_CODECS.iter().map(|row| row.label).collect::<Vec<_>>(),
            [
                "radar",
                "level3",
                "level3/vild",
                "section",
                "voxels",
                "decode"
            ],
            "the labels are the shipped kind strings and the order is \
             load-bearing: WO-M7b's dense code flip assigns codes by index \
             into the composed registry",
        );
        for row in JOB_CODECS {
            assert_eq!(
                row.cost,
                if row.label == "decode" {
                    JobCost::VolumeDecode
                } else {
                    JobCost::Raster
                },
                "the archive decode is the one non-raster job (`{}`)",
                row.label,
            );
            let frame_row = matches!(row.label, "radar" | "level3" | "level3/vild");
            assert_eq!(
                row.encode_out.is_none() && row.decode_out.is_none(),
                frame_row,
                "the three frame rows carry no reply codec until WO-M7c, and \
                 the three rows whose outputs have wire forms carry both \
                 (`{}`)",
                row.label,
            );
        }
        let distinct: std::collections::HashSet<std::any::TypeId> =
            JOB_CODECS.iter().map(|row| (row.input_type)()).collect();
        assert_eq!(
            distinct.len(),
            JOB_CODECS.len(),
            "rows are selected by input type; two rows sharing one would \
             make routing ambiguous",
        );
    }

    #[test]
    fn the_radar_row_round_trips() {
        // Both flag values: the wire byte differs, and the loop-frame case
        // (`false`) is the one the frontend fixture set does not frame.
        for values_wanted in [true, false] {
            let job = DescribedJob::new(RadarPlanJob {
                input: Box::new(a_plan_input()),
                values_wanted,
            });
            assert_round_trips_filling_the_ceiling(&JOB_CODECS[0], &job, 4096);
        }
    }

    /// The bytes are opaque here on purpose: the framing must carry an
    /// arbitrary tail without a length prefix that could lie about it.
    #[test]
    fn the_level3_row_round_trips() {
        let job = DescribedJob::new(Level3Job {
            bytes: Arc::new(vec![7, 8, 9, 0xFF, 0]),
            product: RadarProduct::EchoTops,
            radar_lat: 35.0,
            radar_lon: -97.0,
        });
        assert_round_trips_filling_the_ceiling(&JOB_CODECS[1], &job, 4096);
    }

    /// The two payloads differ in length *and* in content, so a framing that
    /// swapped them, or one that split them at the wrong offset, cannot
    /// round-trip.
    #[test]
    fn the_level3_pair_row_round_trips() {
        let job = DescribedJob::new(Level3PairJob {
            dvl: Arc::new(vec![1, 2, 3]),
            eet: Arc::new(vec![4, 5, 6, 7, 0xFF, 0]),
            radar_lat: 35.0,
            radar_lon: -97.0,
        });
        assert_round_trips_filling_the_ceiling(&JOB_CODECS[2], &job, 4096);
    }

    #[test]
    fn the_section_row_round_trips() {
        let job = DescribedJob::new(SectionJob {
            input: Box::new(a_volume_input()),
            request: SectionRequest {
                start: (35.0, -97.5),
                end: (35.4, -96.8),
                top_km_msl: Some(18.0),
                product: RadarProduct::Reflectivity,
            },
        });
        assert_round_trips_passing_geo_through(&JOB_CODECS[3], &job);
    }

    #[test]
    fn the_voxel_row_round_trips() {
        let job = DescribedJob::new(VoxelJob {
            input: Box::new(a_volume_input()),
            request: VoxelRequest {
                centre: (35.0, -97.0),
                half_extent_km: Some(HalfExtentKm::square(60.0)),
                base_km_msl: 0.0,
                top_km_msl: 15.0,
                product: RadarProduct::Reflectivity,
                // Small and *asymmetric*, so a decoder that read the three
                // axes in the wrong order does not round-trip.
                shape: VoxelShape {
                    nx: 8,
                    ny: 6,
                    nz: 4,
                },
                values_wanted: true,
            },
        });
        assert_round_trips_passing_geo_through(&JOB_CODECS[4], &job);
    }

    /// The voxel job a pane with **no picked region** posts: the width is
    /// left for `build_voxels` to take from the volume's own reach. The
    /// half-width is the only tagged optional in this encoding and `None` is
    /// the case every ordinary 3D pane sends; a decoder that read the tag
    /// byte as the first byte of an `f64` would round-trip the `Some` arm
    /// and hand the worker a nonsense box for this one.
    #[test]
    fn the_sourceless_voxel_row_round_trips() {
        let job = DescribedJob::new(VoxelJob {
            input: Box::new(a_volume_input()),
            request: VoxelRequest {
                centre: (35.0, -97.0),
                half_extent_km: None,
                base_km_msl: 0.0,
                top_km_msl: 15.0,
                product: RadarProduct::Reflectivity,
                shape: VoxelShape {
                    nx: 8,
                    ny: 6,
                    nz: 4,
                },
                values_wanted: true,
            },
        });
        assert_round_trips_passing_geo_through(&JOB_CODECS[4], &job);
    }

    /// A `decode` job whose archive is bytes no decoder will accept. The
    /// point is the *framing*, which has to survive whatever the payload
    /// turns out to be.
    #[test]
    fn the_decode_row_round_trips() {
        let job = DescribedJob::new(DecodeJob {
            archive: Arc::new(b"AR2V0006.001not-a-real-volume".to_vec()),
        });
        assert_round_trips_passing_geo_through(&JOB_CODECS[5], &job);
    }

    /// The affordability guard's one crate-local exercise until WO-M7.2's
    /// flip puts the frontend's refusal tests over this body: a supported
    /// per-axis shape whose cell count no budget affords must refuse at the
    /// boundary, not allocate. The green round-trips above are the control —
    /// an affordable shape decodes.
    #[test]
    fn the_voxel_decode_refuses_an_unaffordable_shape() {
        let job = DescribedJob::new(VoxelJob {
            input: Box::new(a_volume_input()),
            request: VoxelRequest {
                centre: (35.0, -97.0),
                half_extent_km: None,
                base_km_msl: 0.0,
                top_km_msl: 15.0,
                product: RadarProduct::Reflectivity,
                // 1625 an axis passes `is_supported`; 1625³ cells is 4.29
                // billion — over any budget this workspace has.
                shape: VoxelShape {
                    nx: 1625,
                    ny: 1625,
                    nz: 1625,
                },
                values_wanted: true,
            },
        });
        let mut bytes = Vec::new();
        (JOB_CODECS[4].encode)(
            &job,
            &EncodeCtx {
                geometry: geometry_with_ceiling(0),
            },
            &mut bytes,
        );
        let mut r = Reader::new(&bytes);
        assert!(
            (JOB_CODECS[4].decode)(&mut r, geometry_with_ceiling(0)).is_none(),
            "an unaffordable voxel shape must be refused at the boundary",
        );
    }

    /// [`agree_on_product`]'s one crate-local exercise until WO-M7.2: a
    /// request naming one product over a payload carrying another must
    /// refuse rather than draw clear air. The green section round-trip
    /// above is the control — an agreeing pair decodes.
    #[test]
    fn a_section_decode_refuses_a_product_disagreement() {
        let job = DescribedJob::new(SectionJob {
            // The payload carries reflectivity; the request claims velocity.
            input: Box::new(a_volume_input()),
            request: SectionRequest {
                start: (35.0, -97.5),
                end: (35.4, -96.8),
                top_km_msl: Some(18.0),
                product: RadarProduct::Velocity,
            },
        });
        let mut bytes = Vec::new();
        (JOB_CODECS[3].encode)(
            &job,
            &EncodeCtx {
                geometry: geometry_with_ceiling(0),
            },
            &mut bytes,
        );
        let mut r = Reader::new(&bytes);
        assert!(
            (JOB_CODECS[3].decode)(&mut r, geometry_with_ceiling(0)).is_none(),
            "a product disagreement must be refused at the boundary",
        );
    }

    /// The `radar` row's run body owns the loop-frame value strip now: the
    /// numbers stay when the caller wants them and die when it does not,
    /// with the texture unaffected either way. Until WO-M7.2 flips the
    /// frontend onto this table, this is the strip's only guard over THIS
    /// copy of the body (the frontend's `loop_raster_ceiling_tests` still
    /// guard its duplicate).
    #[test]
    fn the_radar_row_runs_the_renderer_and_strips_values_on_request() {
        for (values_wanted, wants) in [(true, true), (false, false)] {
            let job = DescribedJob::new(RadarPlanJob {
                input: Box::new(a_plan_input()),
                values_wanted,
            });
            let out = (JOB_CODECS[0].run)(&job, &geometry_with_ceiling(4096))
                .expect("the fixture renders");
            let frame = out
                .take::<RenderedFrame>()
                .expect("the radar row answers a frame");
            assert!(
                !frame.image.is_empty(),
                "the texture is unaffected by the flag",
            );
            assert_eq!(
                frame.polar.has_values(),
                wants,
                "values_wanted: {values_wanted} must leave has_values() = {wants}",
            );
        }
    }
}
