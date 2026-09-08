//! The six radar codec rows — the radar half of the job boundary, beside the
//! pipeline the rows run (WO-M7.1).

use std::sync::Arc;

use squallar_source::job::{EncodeCtx, JobCodec, JobCost, JobGeometry, JobOutCodec, JobSpec};
use squallar_source::wire::Reader;

use crate::frame::RenderedFrame;
use crate::render_input::RenderInput;
use crate::scan::DecodedScan;
use crate::types::RadarProduct;
use crate::voxel::{HalfExtentKm, VolumeGrid, VoxelRequest, VoxelShape};
use crate::xsect::{CrossSection, SectionRequest};

/// The six radar rows, in dispatch order: **radar, level3, level3/vild,
pub static JOB_CODECS: &[JobCodec] = &[
    JobCodec::of::<RadarPlanJob>(),
    JobCodec::of::<Level3Job>(),
    JobCodec::of::<Level3PairJob>(),
    JobCodec::of::<SectionJob>(),
    JobCodec::of::<VoxelJob>(),
    JobCodec::of::<DecodeJob>(),
];

/// The frame rows' shared reply half: the wire form lives on the type
macro_rules! frame_reply_codec {
    ($spec:ty) => {
        impl JobOutCodec for $spec {
            fn encode_out(v: RenderedFrame, head: &mut Vec<u8>, tails: &mut Vec<Vec<u8>>) {
                head.reserve_exact(RenderedFrame::WIRE_HEAD_MAX_BYTES);
                v.write_head(head);
                tails.push(v.polar.to_tail());
                // The two surface tails, in the order `from_parts` reads them.
                // Exactly one is non-empty, and which one the head's surface
                // discriminant states.
                let (codes, image) = v.into_surface_tails();
                tails.push(codes);
                tails.push(image);
            }

            fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RenderedFrame> {
                RenderedFrame::from_parts(head, tails)
            }
        }
    };
}

frame_reply_codec!(RadarPlanJob);
frame_reply_codec!(Level3Job);
frame_reply_codec!(Level3PairJob);

/// A finished frame whose numbers are wanted, holding them the narrow way.
///
/// Every rasterizing row but the Level II one keeps its numbers unconditionally
/// — only [`RadarPlanJob`] has a caller that says it will not read them — so
/// this is where the other rows say the same thing the Level II row's
/// `values_wanted` arm says. See [`crate::render::polar::PolarField::compact_values`]
/// for why it cannot cost fidelity.
fn holding_its_numbers_narrow(mut frame: RenderedFrame) -> RenderedFrame {
    frame.polar.compact_values();
    frame
}

/// **Which surface a plan-view render is asked for**, as the byte the request
/// carries.
///
/// A request states this rather than the renderer deciding, because the answer
/// is a property of the *caller*: a polar surface has no fallback — the raster
/// it would fall back to is exactly the allocation the representation exists
/// not to make — so a machine with no `RadarFanPainter` installed asking for
/// one would be a pane with no radar on it. The caller is the only half that
/// knows.
///
/// The pattern is `render::polar::PolarWireForm`'s, deliberately and to the
/// letter: a byte with an explicit code each way, `from_wire_code` answering
/// `None` for anything this build does not write, and the byte written in the
/// same statement as the field it describes. An unknown form is declined and
/// never assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanSurface {
    /// The raster this build has always produced.
    Raster,
    /// A code plane where the sweep can carry one, and the raster where it
    /// cannot — see [`crate::render::render_sweep_plane`]. Asking for a fan is
    /// never a promise of one; every reason a sweep is refused is a fidelity
    /// reason and the answer to all of them is today's raster.
    Fan,
}

impl PlanSurface {
    /// This surface as the byte the request carries.
    pub fn wire_code(self) -> u8 {
        match self {
            Self::Raster => 0,
            Self::Fan => 1,
        }
    }

    /// The inverse of [`wire_code`](Self::wire_code), or `None` for a surface
    /// this build does not write.
    pub fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Raster),
            1 => Some(Self::Fan),
            _ => None,
        }
    }
}

/// Rasterize a Level II frame.
#[derive(Debug, PartialEq)]
pub struct RadarPlanJob {
    /// Boxed because a `RenderInput` owns its gate bytes and is the largest
    /// thing in the request by three orders of magnitude.
    pub input: Box<RenderInput>,
    /// Whether the caller wants the numbers behind the gates, or only the
    /// geometry of where they are.
    pub values_wanted: bool,
    /// Which surface the caller can draw. See [`PlanSurface`].
    pub surface: PlanSurface,
}

squallar_source::impl_job_input!(RadarPlanJob);

impl JobSpec for RadarPlanJob {
    type In = RadarPlanJob;
    type Out = RenderedFrame;
    const LABEL: &'static str = "radar";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &RadarPlanJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.push(u8::from(input.values_wanted));
        // Behind `values_wanted` and in front of the nested input, so every
        // byte that was in this framing before keeps the offset it had and the
        // input still ends the payload.
        out.push(input.surface.wire_code());
        out.extend_from_slice(&input.input.to_bytes());
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(RadarPlanJob, JobGeometry)> {
        let values_wanted = flag(r.u8()?)?;
        // Declined, never assumed: a byte this build does not write is a
        // request from a peer it cannot serve, and defaulting it to the raster
        // would answer a question that was not asked.
        let surface = PlanSurface::from_wire_code(r.u8()?)?;
        let input = RenderInput::from_bytes(r.rest())?;
        Some((
            RadarPlanJob {
                input: Box::new(input),
                values_wanted,
                surface,
            },
            geo,
        ))
    }

    /// **The polar surface is tried first and falls back**, which is the whole
    /// of the arm: `render_sweep_plane` answers `None` for every sweep an
    /// eight-bit plane cannot carry without dropping a distinction, and the
    /// answer to all of them is the raster this row has always produced.
    ///
    /// It is asked only where the caller wants the picture and not the numbers.
    /// A plane's numbers **are** its codes, and the polar field it travels with
    /// carries geometry alone; a caller that asked for the values would
    /// otherwise be handed a frame that cannot answer a hover, silently. The
    /// two flags are read in one place so that trade is stated rather than
    /// discovered.
    fn run(input: &RadarPlanJob, geo: &JobGeometry) -> Option<RenderedFrame> {
        if input.surface == PlanSurface::Fan && !input.values_wanted {
            let scan = input.input.to_scan();
            let plane = crate::render::render_sweep_plane(
                &scan,
                input.input.elevation(),
                input.input.product(),
                &input.input.declared_nyquist(),
            );
            if let Some(render) = plane {
                // The polar field is already numberless — the renderer never
                // wrote values into it — so neither `strip_values` nor
                // `compact_values` has anything to do here.
                return Some(RenderedFrame::from(render));
            }
        }
        crate::render::render_from_sized(&input.input, geo.side_ceiling_px as usize).map(|render| {
            let mut frame = RenderedFrame::from(render);
            if input.values_wanted {
                frame.polar.compact_values();
            } else {
                frame.polar.strip_values();
            }
            frame
        })
    }
}

/// Rasterize a Level III radial product.
#[derive(Debug, PartialEq)]
pub struct Level3Job {
    pub bytes: Arc<Vec<u8>>,
    pub product: RadarProduct,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

squallar_source::impl_job_input!(Level3Job);

impl JobSpec for Level3Job {
    type In = Level3Job;
    type Out = RenderedFrame;
    const LABEL: &'static str = "level3";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &Level3Job, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.extend_from_slice(&input.product.wire_code().to_le_bytes());
        out.extend_from_slice(&input.radar_lat.to_le_bytes());
        out.extend_from_slice(&input.radar_lon.to_le_bytes());
        out.extend_from_slice(&input.bytes);
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(Level3Job, JobGeometry)> {
        Some((
            Level3Job {
                product: RadarProduct::from_wire_code(r.u16()?)?,
                radar_lat: r.f64()?,
                radar_lon: r.f64()?,
                bytes: Arc::new(r.rest().to_vec()),
            },
            geo,
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
            .map(holding_its_numbers_narrow)
        })
    }
}

/// Rasterize a Level III product **derived from two objects of the same
/// volume**: VIL density, Digital VIL over Enhanced Echo Tops
/// ([`crate::vild`]).
#[derive(Debug, PartialEq)]
pub struct Level3PairJob {
    pub dvl: Arc<Vec<u8>>,
    pub eet: Arc<Vec<u8>>,
    pub radar_lat: f64,
    pub radar_lon: f64,
}

squallar_source::impl_job_input!(Level3PairJob);

impl JobSpec for Level3PairJob {
    type In = Level3PairJob;
    type Out = RenderedFrame;
    const LABEL: &'static str = "level3/vild";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &Level3PairJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.extend_from_slice(&input.radar_lat.to_le_bytes());
        out.extend_from_slice(&input.radar_lon.to_le_bytes());
        out.extend_from_slice(&(input.dvl.len() as u32).to_le_bytes());
        out.extend_from_slice(&input.dvl);
        out.extend_from_slice(&input.eet);
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(Level3PairJob, JobGeometry)> {
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
            geo,
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
            .map(RenderedFrame::from)
            .map(holding_its_numbers_narrow),
            _ => None,
        }
    }
}

/// Draw a vertical cross-section through a volume.
#[derive(Debug, PartialEq)]
pub struct SectionJob {
    pub input: Box<RenderInput>,
    pub request: SectionRequest,
}

squallar_source::impl_job_input!(SectionJob);

impl JobSpec for SectionJob {
    type In = SectionJob;
    type Out = CrossSection;
    const LABEL: &'static str = "section";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &SectionJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        encode_section_request(out, &input.request);
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
    fn encode_out(v: CrossSection, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        head.extend_from_slice(&v.to_bytes());
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<CrossSection> {
        if !tails.is_empty() {
            return None;
        }
        CrossSection::from_bytes(head)
    }
}

/// The reply half of the job boundary's erasure seam: a described section
impl squallar_source::job::JobOut for CrossSection {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
        vec![self.image_mut()]
    }
}

/// Resample a volume into a Cartesian grid for a raymarch.
#[derive(Debug, PartialEq)]
pub struct VoxelJob {
    pub input: Box<RenderInput>,
    pub request: VoxelRequest,
}

squallar_source::impl_job_input!(VoxelJob);

impl JobSpec for VoxelJob {
    type In = VoxelJob;
    type Out = VolumeGrid;
    const LABEL: &'static str = "voxels";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &VoxelJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        encode_voxel_request(out, &input.request);
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

    fn run(input: &VoxelJob, _geo: &JobGeometry) -> Option<VolumeGrid> {
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
    fn encode_out(v: VolumeGrid, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        // `to_bytes` refuses a field this build has no wire code for. No
        // builder can produce one, so the empty head that follows is not a
        // reachable state — but it decodes to `None` on the magic rather than
        // into a different moment, and it says so in the log.
        match crate::voxel::to_bytes(&v) {
            Some(bytes) => head.extend_from_slice(&bytes),
            None => log::error!(
                "voxel reply not encoded: no wire code for field {}",
                v.field(),
            ),
        }
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<VolumeGrid> {
        if !tails.is_empty() {
            return None;
        }
        crate::voxel::from_bytes(head)
    }
}

/// **Decode a downloaded Level II archive volume.**
#[derive(Debug, PartialEq)]
pub struct DecodeJob {
    pub archive: Arc<Vec<u8>>,
}

squallar_source::impl_job_input!(DecodeJob);

impl JobSpec for DecodeJob {
    type In = DecodeJob;
    type Out = DecodedScan;
    const LABEL: &'static str = "decode";
    const COST: JobCost = JobCost::VolumeDecode;

    fn encode(input: &DecodeJob, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
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

    /// **The archive is not copied to decode it.** This read
    /// `decode_bytes(input.archive.as_ref().clone())`, which duplicated the
    /// whole volume — 0.34–17.96 MB, median 5.56 MB — for the length of the
    /// decode, on up to `Budgets::concurrent_renders` decodes at once, purely
    /// because `decode_bytes` needs an owned buffer and `run` only ever sees a
    /// `&DecodeJob`. `decode_shared` takes the pointer the funnel already moved.
    fn run(input: &DecodeJob, _geo: &JobGeometry) -> Option<DecodedScan> {
        match crate::scan::decode_shared(Arc::clone(&input.archive)) {
            Ok(volume) => Some(volume),
            Err(e) => {
                log::error!("could not decode a Level II volume: {e}");
                None
            }
        }
    }
}

impl JobOutCodec for DecodeJob {
    fn encode_out(v: DecodedScan, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        head.extend_from_slice(&v.to_bytes());
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<DecodedScan> {
        if !tails.is_empty() {
            return None;
        }
        DecodedScan::from_bytes(head)
    }
}

/// The reply half of the job boundary's erasure seam: a described archive
impl squallar_source::job::JobOut for DecodedScan {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
        Vec::new()
    }
}

/// The product is on the wire twice — once in the request's own geometry and
/// once inside the [`RenderInput`] — and two statements of one fact can
/// disagree.
fn agree_on_product(wanted: RadarProduct, input: &RenderInput) -> Option<()> {
    (wanted == input.product()).then_some(())
}

/// A wire boolean, refusing anything that is not 0 or 1.
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
    // machine can hold, and unlike `voxel::from_bytes` there is no payload
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
    use squallar_source::job::DescribedJob;

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

    /// [`sample_scan`]'s one tilt, with a **sixteen-bit** differential phase
    /// moment beside the eight-bit reflectivity one — one volume holding both
    /// sides of the R8 admission.
    fn a_two_moment_scan() -> nexrad_model::data::Scan {
        use nexrad_model::data::{MomentData, Radial, RadialStatus, Sweep};
        let radials = (0..36)
            .map(|i| {
                let phi: Vec<u8> = (0..120u16).flat_map(|g| (g + 2).to_be_bytes()).collect();
                Radial::new(
                    0,
                    i,
                    f32::from(i) * 10.0,
                    10.0,
                    RadialStatus::IntermediateRadialData,
                    1,
                    0.5,
                    Some(MomentData::from_fixed_point(
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
                    Some(MomentData::from_fixed_point(
                        120, 0, 250, 16, 2.8361, 2.0, phi,
                    )),
                    None,
                    None,
                )
            })
            .collect();
        let mut scan = sample_scan();
        scan = nexrad_model::data::Scan::new(
            scan.coverage_pattern().clone(),
            vec![Sweep::new(1, radials)],
        );
        scan
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

    /// The envelope the frontend's dispatch hands the rows: distinctive
    /// width/height/bounds/ceiling every row must pass through untouched —
    /// since WO-M7b the caller's canonical envelope is the one carrier of
    /// all four, so a row that amended any of them would be a second
    /// statement of the envelope.
    fn geometry_with_ceiling(side_ceiling_px: u32) -> JobGeometry {
        JobGeometry {
            width: 64,
            height: 32,
            bounds: squallar_geo::GeoBounds {
                min_lat: 30.0,
                max_lat: 40.0,
                min_lon: -100.0,
                max_lon: -90.0,
            },
            side_ceiling_px,
        }
    }

    /// The one round-trip harness (WO-M7b re-pin of the WO-M7.1 pair):
    /// encode `job` through `row`, decode it back under the same envelope,
    /// and require the identity — the decoded job equals the original and
    /// the geometry passes through unchanged, every field including the
    /// deliberately non-zero ceiling. No row writes or reads envelope bytes
    /// anymore; a row that zeroed or filled any envelope field would fail
    /// the pass-through half.
    ///
    /// No cursor assertion: every radar row's payload ends in a tail that
    /// takes the rest, so completeness is proven by value equality — a stray
    /// trailing byte lands inside the tail (or is refused by
    /// `RenderInput::from_bytes`) and fails the equality either way.
    fn assert_round_trips_passing_geo_through(row: &JobCodec, job: &DescribedJob) {
        let geo = geometry_with_ceiling(4096);
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
             load-bearing: the dense wire code (WO-M7b) is the row's index \
             into the composed registry, plus one",
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
        }
        // Every row carries its reply codec by construction since WO-M7c
        // de-Optioned the pair — what is left to pin is that the three frame
        // rows' reply half really is the frame codec: a frame reply
        // round-trips through the erased row exactly as through the type.
        for row in &JOB_CODECS[..3] {
            let frame = crate::frame::RenderedFrame {
                image: crate::frame::RasterImage::Bytes(vec![9, 8, 7, 6]),
                max_range_km: 230.0,
                polar: crate::render::polar::PolarField::default(),
                nyquist_ms: Some(8.5),
                melting_layer_source: None,
                storm_motion: None,
                codes: None,
            };
            let mut head = Vec::new();
            let mut tails = Vec::new();
            (row.encode_out)(
                squallar_source::job::DescribedOut(Box::new(frame.clone())),
                &mut head,
                &mut tails,
            );
            let mut expected_head = Vec::new();
            frame.write_head(&mut expected_head);
            assert_eq!(
                head, expected_head,
                "`{}`: the head is not the frame's own",
                row.label
            );
            assert_eq!(
                tails.len(),
                3,
                "`{}`: the frame nominates three tails (which three, and in \
                 what order, is the frame-reply digest rows' pin)",
                row.label,
            );
            assert_eq!(
                (row.decode_out)(&head, tails)
                    .expect("the frame reply decodes")
                    .take::<crate::frame::RenderedFrame>(),
                Some(frame),
                "`{}`: the reply half is not the frame codec",
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
        // Both flag values and both surfaces: each is a wire byte of its
        // own, and the loop-frame case (`false` / `Fan`) is the one the
        // frontend fixture set does not frame.
        for values_wanted in [true, false] {
            for surface in [PlanSurface::Raster, PlanSurface::Fan] {
                let job = DescribedJob::new(RadarPlanJob {
                    input: Box::new(a_plan_input()),
                    values_wanted,
                    surface,
                });
                assert_round_trips_passing_geo_through(&JOB_CODECS[0], &job);
            }
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
        assert_round_trips_passing_geo_through(&JOB_CODECS[1], &job);
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
        assert_round_trips_passing_geo_through(&JOB_CODECS[2], &job);
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
                surface: PlanSurface::Raster,
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

    /// **A fan request answers a plane, and the plane is the frame's only
    /// surface.**
    ///
    /// Three conjuncts and none is optional. The plane is present — which is
    /// the whole of what this lane added, and was `None` on every frame this
    /// build produced before it. The raster is *absent*, which is the saving
    /// rather than a detail of it: a frame that carried both would hold the
    /// 650 MB cells-and-image pair the representation exists not to allocate,
    /// and `into_surface_tails` would refuse it. And the polar field carries
    /// its geometry, because a pane still has to know where the gates are.
    ///
    /// TAMPER: make `run`'s fan arm fall through to the raster and the first
    /// two go red; make `render_sweep_plane` keep the projected image and the
    /// second goes red alone.
    #[test]
    fn a_fan_request_answers_a_plane_and_no_raster() {
        let job = DescribedJob::new(RadarPlanJob {
            input: Box::new(a_plan_input()),
            values_wanted: false,
            surface: PlanSurface::Fan,
        });
        let out =
            (JOB_CODECS[0].run)(&job, &geometry_with_ceiling(4096)).expect("the fixture renders");
        let frame = out
            .take::<RenderedFrame>()
            .expect("the radar row answers a frame");
        let plane = frame
            .codes
            .as_ref()
            .expect("the fan request produced a plane");
        assert!(
            frame.image.is_empty(),
            "a frame with a plane also carried a raster; that is both surfaces at once",
        );
        assert!(
            !frame.polar.geometry().is_empty(),
            "the plane arrived with no geometry to place it by",
        );
        // The two halves describe the same sweep, which is what
        // `render_dispatch::fan_sweep` refuses a mismatch of.
        assert_eq!(
            plane.shape(),
            (
                frame.polar.geometry().radials(),
                frame.polar.geometry().gates()
            ),
        );
    }

    /// **A caller that asked for the numbers is given the form that has
    /// them.**
    ///
    /// A plane's numbers are its codes and the polar field beside one carries
    /// geometry alone, so a fan request with `values_wanted` set falls back to
    /// the raster rather than answering a frame no hover can read. Stated
    /// here rather than discovered by a pane whose readout went blank.
    ///
    /// TAMPER: drop the `!input.values_wanted` conjunct in `run` and this goes
    /// red on both assertions.
    #[test]
    fn a_fan_request_that_wants_the_numbers_gets_the_raster() {
        let job = DescribedJob::new(RadarPlanJob {
            input: Box::new(a_plan_input()),
            values_wanted: true,
            surface: PlanSurface::Fan,
        });
        let out =
            (JOB_CODECS[0].run)(&job, &geometry_with_ceiling(4096)).expect("the fixture renders");
        let frame = out
            .take::<RenderedFrame>()
            .expect("the radar row answers a frame");
        assert!(frame.codes.is_none(), "a plane cannot answer a hover");
        assert!(
            frame.polar.has_values(),
            "and the raster's numbers are gone"
        );
    }

    /// **A product an eight-bit plane cannot carry renders the way it renders
    /// today**, even under a fan request.
    ///
    /// Differential phase is refused by domain (`WireWordTooWide`: sixteen-bit
    /// wire codes, 65,534 reachable values against 254 a plane can index), and
    /// the answer is a raster with a texture in it — not a blank pane, and not
    /// a quantised plane.
    ///
    /// **The control is the first half**, and it is what stops this reading
    /// green through an arm it never reached: the same scan's reflectivity,
    /// under the same request, must come back WITH a plane. Without it a
    /// fixture that simply failed to extract would satisfy every assertion
    /// below.
    #[test]
    fn a_refused_product_under_a_fan_request_still_renders_its_raster() {
        let _ledger = crate::render::codes::hold_refusal_ledger();
        let scan = a_two_moment_scan();
        let fan_frame = |product| {
            let input = RenderInput::extract(&scan, 0.5, product, 35.0, -97.0, None, None)
                .unwrap_or_else(|| panic!("the fixture carries {product:?}"));
            let job = DescribedJob::new(RadarPlanJob {
                input: Box::new(input),
                values_wanted: false,
                surface: PlanSurface::Fan,
            });
            (JOB_CODECS[0].run)(&job, &geometry_with_ceiling(4096))
                .expect("the fixture renders")
                .take::<RenderedFrame>()
                .expect("a frame")
        };

        let admitted = fan_frame(RadarProduct::Reflectivity);
        assert!(
            admitted.codes.is_some(),
            "the control: an eight-bit reflectivity sweep of this scan must \
             reach the fan arm, or the refusal below proves nothing",
        );

        let refused = fan_frame(RadarProduct::DifferentialPhase);
        assert!(
            refused.codes.is_none(),
            "a sixteen-bit moment was given an R8 plane",
        );
        assert!(
            !refused.image.is_empty(),
            "a refused product lost its raster instead of keeping it",
        );
    }

    /// The surface byte is declined, never assumed — the pattern
    /// `PolarWireForm` set.
    #[test]
    fn an_unknown_surface_byte_is_declined() {
        assert_eq!(PlanSurface::from_wire_code(0), Some(PlanSurface::Raster));
        assert_eq!(PlanSurface::from_wire_code(1), Some(PlanSurface::Fan));
        assert_ne!(
            PlanSurface::Raster.wire_code(),
            PlanSurface::Fan.wire_code(),
            "two surfaces sharing a code is a request that cannot say what it wants",
        );
        for code in 2..=u8::MAX {
            assert_eq!(
                PlanSurface::from_wire_code(code),
                None,
                "surface byte {code} was read as a form this build writes",
            );
        }
    }

    // ── The envelope the layer itself shapes (WO-M14b-2) ────────────────

    /// The context the frontend hands over for `input`, over the default box
    /// about the site. Built with the substrate's own vocabulary and nothing
    /// else — which is the whole point: the caller of `volume_job` cannot name
    /// one type in what it gets back.
    fn a_handover(input: RenderInput) -> squallar_source::volume::VolumeJobContext {
        squallar_source::volume::VolumeJobContext {
            payload: Box::new(input),
            field: crate::fields::known::REFLECTIVITY,
            centre: squallar_geo::GeoPoint {
                lat: 35.0,
                lon: -97.0,
            },
            half_extent_km: None,
            cells: [128, 128, 32],
            max_axis: 2048,
        }
    }

    /// **What a layer hands back IS the voxel row's own input**, request and
    /// payload together.
    ///
    /// The dispatch side receives a `DescribedJob` and puts it in an envelope
    /// without looking inside; the only thing that makes that safe is that the
    /// row keyed by this input type is the row that will run it. So the pin is
    /// the downcast the funnel itself performs, and then the round trip
    /// through that row's codec — the same harness every other row is held to.
    #[test]
    fn the_job_a_radar_layer_shapes_is_the_voxel_row_it_will_be_run_by() {
        use squallar_source::volume::VolumeCapable;

        let input = a_volume_input();
        let described = crate::source::RadarSource::new()
            .volume_job(a_handover(input.clone()))
            .expect("radar shapes a job for a field it registers");
        let job = described
            .downcast_ref::<VoxelJob>()
            .expect("the voxels row owns VoxelJob, and the funnel finds the row by this type");
        assert_eq!(
            job.request,
            crate::voxel::request_for(&a_handover(a_volume_input()), (35.0, -97.0))
                .expect("the same context shapes the same request"),
            "the envelope carries the request the context names",
        );
        assert_eq!(
            *job.input, input,
            "the payload the frontend extracted is what travels, unaltered",
        );
        assert_round_trips_passing_geo_through(&JOB_CODECS[4], &described);
    }

    /// **A payload of another layer's shape is refused, never unwrapped.**
    ///
    /// The handover is `dyn Any` precisely so the frontend can carry a
    /// source's data without a name for it — which means nothing upstream can
    /// be trusted to have got the type right, and a wrong one has to be a
    /// `None` rather than a panic in the middle of a frame.
    #[test]
    fn a_payload_of_another_shape_is_refused_rather_than_unwrapped() {
        use squallar_source::volume::VolumeCapable;

        let mut ctx = a_handover(a_volume_input());
        ctx.payload = Box::new(String::from("not a volume"));
        assert!(crate::source::RadarSource::new().volume_job(ctx).is_none());
        // Control: the identical call with the right payload does shape one,
        // so the `None` above is about the payload and nothing else.
        assert!(
            crate::source::RadarSource::new()
                .volume_job(a_handover(a_volume_input()))
                .is_some(),
        );
    }

    /// **A field this build has no product for shapes no job**, and it is
    /// refused before the payload is even looked at.
    #[test]
    fn an_unregistered_field_shapes_no_job() {
        use squallar_source::volume::VolumeCapable;

        let mut ctx = a_handover(a_volume_input());
        ctx.field = squallar_source::product::FieldId::from_static("NotAFieldThisBuildHas");
        assert!(crate::source::RadarSource::new().volume_job(ctx).is_none());
    }
}
