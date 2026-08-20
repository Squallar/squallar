//! A resampled Cartesian volume: the native tilt ladder flattened onto an
//! axis-aligned grid of palette indices, for a GPU raymarcher to upload as one
//! 3D texture.

use crate::palette::{get_color_for_value, get_legend_scale};
use crate::par::*;
use crate::sampler::{Column, VolumeSampler};
use crate::types::{MomentSlot, RadarProduct};
use std::sync::LazyLock;

/// The output shape itself lives in the substrate, so a consumer can read a
/// volume without naming the crate that resampled it. Re-exported here — as
/// `palette` re-exports `LegendScale` — so no consumer spelling had to move
/// and no consumer needed a new dependency.
///
/// `VoxelShape` is this crate's own name for the substrate's `VolumeDims`:
/// [`VoxelRequest`] asks in it, the device profile answers in it, and the two
/// are ONE type, not two.
pub use rustdar_source::volume::{
    IsoShape, LUT_LEN, LutFilter, MAX_AXIS, NO_DATA_INDEX, SEE_THROUGH_ALPHA_CEILING,
    TransferTable, VolumeDims as VoxelShape, VolumeGrid, VolumeParts, axis_centre, ramp_index,
    ramp_value,
};

/// Narrowest half-extent a request may ask for on either axis, km.
pub const MIN_HALF_WIDTH_KM: f64 = 10.0;

/// The half-width a box is given when nothing can be said about how far its
/// volume reaches, km — the WSR-88D's nominal unambiguous range.
pub const BASE_HALF_WIDTH_KM: f64 = 230.0;

/// Furthest a box's corner may stand from its centre, km —
/// [`crate::types::MAX_EXTENT_KM`] × √2, the corner of the square that
/// circumscribes the widest ring the plan view will draw.
pub const MAX_HALF_DIAGONAL_KM: f64 = crate::types::MAX_EXTENT_KM * std::f64::consts::SQRT_2;

/// Widest half-width a square request may ask for, km — the corner bound
/// solved for `east_km == north_km`.
pub const MAX_HALF_WIDTH_KM: f64 = MAX_HALF_DIAGONAL_KM / std::f64::consts::SQRT_2;

/// Half a box's east–west and north–south extent, km.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HalfExtentKm {
    pub east_km: f64,
    pub north_km: f64,
}

impl HalfExtentKm {
    /// The same half-extent on both axes.
    pub const fn square(km: f64) -> Self {
        Self {
            east_km: km,
            north_km: km,
        }
    }

    /// How far the box's corner stands from its centre, km.
    pub fn corner_km(self) -> f64 {
        self.east_km.hypot(self.north_km)
    }

    /// Whether both axes are finite.
    pub fn is_finite(self) -> bool {
        self.east_km.is_finite() && self.north_km.is_finite()
    }

    /// This extent floored at [`MIN_HALF_WIDTH_KM`] per axis, then brought
    /// inside [`MAX_HALF_DIAGONAL_KM`] without changing its aspect ratio: both
    /// axes scale by one factor, so the corner lands exactly on the bound.
    pub fn clamped(self) -> Self {
        let floored = Self {
            east_km: self.east_km.max(MIN_HALF_WIDTH_KM),
            north_km: self.north_km.max(MIN_HALF_WIDTH_KM),
        };
        let corner = floored.corner_km();
        if corner <= MAX_HALF_DIAGONAL_KM {
            return floored;
        }
        let scale = MAX_HALF_DIAGONAL_KM / corner;
        Self {
            east_km: floored.east_km * scale,
            north_km: floored.north_km * scale,
        }
    }
}

/// Half-width of the box to resample, km — the square that circumscribes the sweep's
/// own range circle, so the half-width *is* the reach.
pub fn box_half_width_km(data_reach_km: f64) -> f64 {
    // `is_nan` spelled out: every ordering against a `NaN` is false, so
    // `<= 0.0` alone would let one through to a `clamp` that propagates it.
    if data_reach_km.is_nan() || data_reach_km <= 0.0 {
        return BASE_HALF_WIDTH_KM;
    }
    // The reach itself, not `reach / √2`: the box circumscribes the ring.
    data_reach_km.clamp(MIN_HALF_WIDTH_KM, MAX_HALF_WIDTH_KM)
}

/// How far `product`'s data reaches over the ground, km, across every sweep of `scan`
/// that carries it — 0.0 if none does.
pub fn volume_reach_km(scan: &nexrad_model::data::Scan, product: RadarProduct) -> f64 {
    use nexrad_model::data::DataMoment;

    let mut reach = 0.0f64;
    for sweep in scan.sweeps() {
        let radials = sweep.radials();
        let ground = match crate::volumetric::sweep_elevation_deg(radials) {
            Some(e) if e.is_finite() => e.to_radians().cos().clamp(0.0, 1.0),
            _ => 1.0,
        };
        for radial in radials {
            let Some(moment) = product.get_moment(radial) else {
                continue;
            };
            let slant = moment.first_gate_range_km()
                + f64::from(moment.gate_count()) * moment.gate_interval_km();
            reach = reach.max(slant * ground);
        }
    }
    reach
}

/// Bottom of the box a 3D view resamples by default, kilometres MSL.
pub const DEFAULT_BASE_KM_MSL: f64 = 0.0;

/// Top of the box a 3D view resamples by default, kilometres MSL.
pub const DEFAULT_TOP_KM_MSL: f64 = 18.0;

/// What one grid's index plane may occupy, bytes.
pub const VOXEL_TEXTURE_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// 128 × 128 × 64 — one MiB of indices, for wasm's single worker and 4 GiB
/// linear memory.
pub const WASM_SHAPE: VoxelShape = VoxelShape {
    nx: 128,
    ny: 128,
    nz: 64,
};

/// 192 × 192 × 96 — 3.375 MiB.
pub const MOBILE_SHAPE: VoxelShape = VoxelShape {
    nx: 192,
    ny: 192,
    nz: 96,
};

/// 256 × 256 × 128 — 8 MiB, every axis at the GLES 3.0 guarantee.
pub const DESKTOP_SHAPE: VoxelShape = VoxelShape {
    nx: 256,
    ny: 256,
    nz: 128,
};

/// The vertical this would rather have, and takes wherever it can be had
/// without costing the horizontal.
pub const NZ_PREFERRED: usize = 32;

/// The shallowest the vertical axis may be made in order to buy horizontal
/// resolution.
pub const NZ_MIN: usize = 16;

/// What the horizontal axes are held to a multiple of, in cells.
pub const HORIZONTAL_AXIS_MULTIPLE: usize = 64;

/// What the vertical axis is held to a multiple of, in cells.
pub const VERTICAL_AXIS_MULTIPLE: usize = 16;

/// The grid to build for the tier `shipped` names, on a device whose 3D
/// textures may be `max_axis` on a side.
pub const fn shape_for_budget(shipped: VoxelShape, max_axis: usize) -> VoxelShape {
    let cap = if max_axis < MAX_AXIS {
        max_axis
    } else {
        MAX_AXIS
    };
    let budget = shipped.cells();
    let smoother = spend_budget(budget, NZ_PREFERRED, cap);
    if smoother.nx > shipped.nx {
        return smoother;
    }
    spend_budget(budget, NZ_MIN, cap)
}

/// One arm of [`shape_for_budget`]: the widest aligned square `cell_budget`
/// buys at `nz_floor` layers, with the leftover put back into the vertical.
const fn spend_budget(cell_budget: usize, nz_floor: usize, cap: usize) -> VoxelShape {
    // The widest square the budget affords at that vertical, and no wider than
    // the device will hold.
    let mut nx = (cell_budget / nz_floor).isqrt();
    if nx > cap {
        nx = cap;
    }
    // Down to the copy alignment, unless there is not a whole step to be had.
    if nx >= HORIZONTAL_AXIS_MULTIPLE {
        nx -= nx % HORIZONTAL_AXIS_MULTIPLE;
    }
    if nx < 1 {
        nx = 1;
    }
    let mut nz = cell_budget / (nx * nx);
    if nz > cap {
        nz = cap;
    }
    // And down to the layout alignment: a vertical the texture's own block
    // depth would round up is memory spent without a cell to show for it.
    if nz >= VERTICAL_AXIS_MULTIPLE {
        nz -= nz % VERTICAL_AXIS_MULTIPLE;
    }
    if nz < 1 {
        nz = 1;
    }
    VoxelShape { nx, ny: nx, nz }
}

/// The tier a device class belongs to, as a function of the class rather than
/// of the `cfg`.
const fn default_shape_for(is_wasm: bool) -> VoxelShape {
    if is_wasm { WASM_SHAPE } else { DESKTOP_SHAPE }
}

/// The shape this target builds by default on a device whose 3D textures may
/// be `max_axis` on a side.
#[cfg(target_arch = "wasm32")]
pub fn default_shape(max_axis: usize) -> VoxelShape {
    shape_for_budget(default_shape_for(true), max_axis)
}

/// The shape this target builds by default. See the wasm arm.
#[cfg(not(target_arch = "wasm32"))]
pub fn default_shape(max_axis: usize) -> VoxelShape {
    shape_for_budget(default_shape_for(false), max_axis)
}

/// What to resample, over what box.
#[derive(Debug, Clone, PartialEq)]
pub struct VoxelRequest {
    /// Latitude and longitude of the box's horizontal centre.
    pub centre: (f64, f64),
    /// Half the box's east–west and north–south extent, km, or `None` to take
    /// the square half-width [`box_half_width_km`] derives from the volume's
    /// own reach.
    pub half_extent_km: Option<HalfExtentKm>,
    /// Bottom of the box, km MSL.
    pub base_km_msl: f64,
    /// Top of the box, km MSL. Must be strictly above `base_km_msl`.
    pub top_km_msl: f64,
    /// Which moment.
    pub product: RadarProduct,
    /// Cells per axis.
    pub shape: VoxelShape,
    /// Whether to also keep the values in their own units.
    pub values_wanted: bool,
}

/// The bottom and top **data** levels of a moment: the values index 1 and
/// index 255 stand for.
fn data_levels(slot: MomentSlot) -> (f32, f32) {
    match slot {
        // Legend 0…95; encoding (2, 66) decodes codes 2…255 to −32.0…94.5 dBZ.
        MomentSlot::Reflectivity => (-32.0, 95.0),
        // Legend ±36.01 m/s; encoding (2, 129) decodes to −63.5…+63.0 m/s.
        MomentSlot::Velocity => (-63.5, 63.5),
        // Legend 0…10.2889; the same (2, 129) encoding, non-negative half, to 63.0 m/s.
        MomentSlot::SpectrumWidth => (0.0, 63.5),
        // Legend −2.0…5.5 (its NEG_INFINITY floor is not a value); encoding (16, 128)
        // decodes to −7.875…+7.9375 dB.
        MomentSlot::DifferentialReflectivity => (-7.875, 8.0),
        // A circular moment over its whole turn.
        MomentSlot::DifferentialPhase => (0.0, 360.0),
        // Legend 0.45…0.98; encoding (300, −60.5) decodes to 0.208…1.052.
        MomentSlot::CorrelationCoefficient => (0.2, 1.06),
    }
}

/// [`data_levels`], with the derived products' own ranges layered over the
/// slot's.
fn data_levels_for(product: RadarProduct, slot: MomentSlot) -> (f32, f32) {
    match product {
        // Unitless, and one number with the field's own NROT_LIMIT clamp, at 0.0395
        // resolution.
        RadarProduct::NormalizedRotation => (-5.0, 5.0),
        // The estimator's own display clamp.
        RadarProduct::SpecificDifferentialPhase => {
            (crate::kdp::KDP_MIN_DISPLAY, crate::kdp::KDP_MAX_DISPLAY)
        }
        _ => data_levels(slot),
    }
}

/// The full ramp: [`data_levels`] with index 0 placed one step below index 1.
fn value_range_for(slot: MomentSlot) -> (f32, f32) {
    let (lo, hi) = data_levels(slot);
    let step = (f64::from(hi) - f64::from(lo)) / 254.0;
    ((f64::from(lo) - step) as f32, hi)
}

/// [`value_range_for`] keyed by product first — the derived products carry
/// their own ranges (see [`data_levels_for`]).
fn value_range_for_product(product: RadarProduct, slot: MomentSlot) -> (f32, f32) {
    match product {
        RadarProduct::NormalizedRotation | RadarProduct::SpecificDifferentialPhase => {
            let (lo, hi) = data_levels_for(product, slot);
            let step = (f64::from(hi) - f64::from(lo)) / 254.0;
            ((f64::from(lo) - step) as f32, hi)
        }
        _ => value_range_for(slot),
    }
}

/// Where a moment's default 3D transparency starts and ends, in the moment's
/// own units.
mod volume_alpha_profile {
    /// Velocity: the palette is diverging, so the uninteresting band is the middle.
    pub const VELOCITY_CLEAR_MS: f32 = 4.0;
    pub const VELOCITY_OPAQUE_MS: f32 = 20.0;

    /// Spectrum width is sequential: clear below 2 m/s, opaque by 8 m/s.
    pub const SW_CLEAR_MS: f32 = 2.0;
    pub const SW_OPAQUE_MS: f32 = 8.0;

    /// ZDR's quiet band is the interval the ORPG-derived HCA leaves for
    /// ordinary rain — which does not contain zero.
    pub const ZDR_RAIN_LO_DB: f32 = crate::hca::MIN_ZDR_BD as f32;
    pub const ZDR_RAIN_HI_DB: f32 = crate::hca::MAX_ZDR_GR as f32;
    pub const ZDR_TUMBLING_DB: f32 = crate::hca::MIN_ZDR_WS as f32;
    pub const ZDR_TUMBLING_ALPHA: f32 = PHI_ALPHA;
    pub const ZDR_NEGATIVE_DB: f32 = -3.0;
    pub const ZDR_COLUMN_DB: f32 = 3.0;

    /// The diverging centre the isosurface reads for ZDR — a display choice
    /// rather than a derivation, hence a bare literal.
    pub const ZDR_CENTRE_DB: f32 = 0.25;

    /// ρHV inverts the usual shape: uniform precipitation reads 0.97–1.0 and
    /// is the background. Clear above 0.97, opaque below 0.90.
    pub const CC_OPAQUE: f32 = 0.90;
    pub const CC_CLEAR: f32 = 0.97;

    /// ΦDP gets a flat translucency instead of a value band: the moment is
    /// cumulative along the ray and offset by a per-site system phase, so no
    /// fixed value band is "background".
    pub const PHI_ALPHA: f32 = 0.35;

    /// Storm-relative velocity keeps velocity's shape and numbers.
    pub const SRV_CLEAR_MS: f32 = VELOCITY_CLEAR_MS;
    pub const SRV_OPAQUE_MS: f32 = VELOCITY_OPAQUE_MS;

    /// Normalized rotation: clear under [`crate::nrot::SIGNIFICANT`], opaque at
    /// |1.0| and beyond — the mesocyclone convention GR pins its meso class to.
    pub const NROT_CLEAR: f32 = crate::nrot::SIGNIFICANT as f32;
    pub const NROT_OPAQUE: f32 = 1.0;
    pub const NROT_WEAK_ALPHA: f32 = 0.25;

    /// KDP is sequential: clear under 0.25 °/km (below the estimator's own
    /// significance), opaque by 1.5 °/km.
    pub const KDP_CLEAR_DEG_KM: f32 = 0.25;
    pub const KDP_OPAQUE_DEG_KM: f32 = 1.5;
}

/// `x` mapped smoothly from 0 at `edge0` to 1 at `edge1`, clamped.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The isosurface shape per product.
pub fn iso_shape(product: RadarProduct) -> IsoShape {
    use volume_alpha_profile as p;
    match product {
        RadarProduct::Reflectivity
        | RadarProduct::SpectrumWidth
        | RadarProduct::DifferentialPhase
        | RadarProduct::SpecificDifferentialPhase => IsoShape::Sequential,
        RadarProduct::Velocity
        | RadarProduct::StormRelativeVelocity
        | RadarProduct::NormalizedRotation => IsoShape::DeviationFrom { centre: 0.0 },
        RadarProduct::DifferentialReflectivity => IsoShape::DeviationFrom {
            centre: p::ZDR_CENTRE_DB,
        },
        RadarProduct::CorrelationCoefficient => IsoShape::AtOrBelow,
        // Not renderable in 3D at all (`crate::derive::volume_slot`).
        RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::ProbabilityOfSevereHail
        | RadarProduct::MaxExpectedHailSize
        | RadarProduct::HydrometeorClassification
        | RadarProduct::PrecipitationRate => IsoShape::Sequential,
    }
}

/// The default isosurface threshold per product, in the units
/// [`iso_shape`] gives the slider: a value for the sequential products, a
/// deviation for the diverging ones, a bound for ρHV.
pub fn default_iso_threshold(product: RadarProduct) -> f32 {
    use volume_alpha_profile as p;
    match product {
        RadarProduct::Reflectivity => 18.0,
        RadarProduct::Velocity => p::VELOCITY_OPAQUE_MS,
        RadarProduct::StormRelativeVelocity => p::SRV_OPAQUE_MS,
        RadarProduct::SpectrumWidth => p::SW_OPAQUE_MS,
        RadarProduct::DifferentialReflectivity => p::ZDR_COLUMN_DB - p::ZDR_CENTRE_DB,
        RadarProduct::DifferentialPhase => 180.0,
        RadarProduct::CorrelationCoefficient => p::CC_OPAQUE,
        RadarProduct::SpecificDifferentialPhase => p::KDP_OPAQUE_DEG_KM,
        RadarProduct::NormalizedRotation => p::NROT_OPAQUE,
        RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::ProbabilityOfSevereHail
        | RadarProduct::MaxExpectedHailSize
        | RadarProduct::HydrometeorClassification
        | RadarProduct::PrecipitationRate => 0.0,
    }
}

/// The default 3D alpha multiplier for `product` at `value`.
fn volume_alpha_scale(product: RadarProduct, value: f32) -> f32 {
    use volume_alpha_profile as p;
    match product {
        RadarProduct::Reflectivity => 1.0,
        RadarProduct::Velocity => {
            smoothstep(p::VELOCITY_CLEAR_MS, p::VELOCITY_OPAQUE_MS, value.abs())
        }
        RadarProduct::SpectrumWidth => smoothstep(p::SW_CLEAR_MS, p::SW_OPAQUE_MS, value),
        // Two-sided and asymmetric, not a deviation from one centre: the quiet
        // band is `[ZDR_RAIN_LO_DB, ZDR_RAIN_HI_DB]`.
        RadarProduct::DifferentialReflectivity => {
            if value >= p::ZDR_RAIN_LO_DB {
                smoothstep(p::ZDR_RAIN_HI_DB, p::ZDR_COLUMN_DB, value)
            } else {
                // The plateau: held at the tumbling value until the deep
                // negative tail earns full strength.
                let toward_tumbling =
                    1.0 - smoothstep(p::ZDR_TUMBLING_DB, p::ZDR_RAIN_LO_DB, value);
                let deep = 1.0 - smoothstep(p::ZDR_NEGATIVE_DB, p::ZDR_TUMBLING_DB, value);
                (1.0 - p::ZDR_TUMBLING_ALPHA)
                    .mul_add(deep, p::ZDR_TUMBLING_ALPHA * toward_tumbling)
                    .min(1.0)
            }
        }
        RadarProduct::CorrelationCoefficient => 1.0 - smoothstep(p::CC_OPAQUE, p::CC_CLEAR, value),
        RadarProduct::DifferentialPhase => p::PHI_ALPHA,
        // The derived products, admitted by `crate::derive`.
        RadarProduct::StormRelativeVelocity => {
            smoothstep(p::SRV_CLEAR_MS, p::SRV_OPAQUE_MS, value.abs())
        }
        // Stepped, not faded, at the significance floor: NROT's palette is
        // class-structured, so the volume goes visible where the plan view does.
        RadarProduct::NormalizedRotation => {
            let magnitude = value.abs();
            if magnitude < p::NROT_CLEAR {
                0.0
            } else {
                (1.0 - p::NROT_WEAK_ALPHA)
                    .mul_add(
                        smoothstep(p::NROT_CLEAR, p::NROT_OPAQUE, magnitude),
                        p::NROT_WEAK_ALPHA,
                    )
                    .min(1.0)
            }
        }
        RadarProduct::SpecificDifferentialPhase => {
            smoothstep(p::KDP_CLEAR_DEG_KM, p::KDP_OPAQUE_DEG_KM, value)
        }
        // Unreachable today: `crate::derive::volume_slot` refuses these before a table
        // is built.
        RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::ProbabilityOfSevereHail
        | RadarProduct::MaxExpectedHailSize
        | RadarProduct::HydrometeorClassification
        | RadarProduct::PrecipitationRate => 1.0,
    }
}

/// The 256-entry RGBA table for a product over a ramp, entry 0 forced fully
/// transparent.
fn colormap_lut(product: RadarProduct, range: (f32, f32)) -> Vec<u8> {
    let mut lut = Vec::with_capacity(LUT_LEN);
    // Entry 0 is the no-data entry, forced transparent: most palettes hand
    // back an opaque colour at the ramp's bottom, and an opaque no-data index
    // paints the whole outside of the volume.
    lut.extend_from_slice(&[0, 0, 0, 0]);
    for index in 1..=255u8 {
        let value = ramp_value(range, index);
        let (r, g, b, a) = get_color_for_value(product, value);
        let a = (f32::from(a) * volume_alpha_scale(product, value)).round() as u8;
        lut.extend_from_slice(&[r, g, b, a]);
    }
    lut
}

/// [`colormap_lut`]'s answer for every voxel-capable product, built once.
pub(crate) fn volume_lut_static(product: RadarProduct) -> Option<&'static [u8]> {
    static ALL: LazyLock<Vec<Option<Vec<u8>>>> = LazyLock::new(|| {
        RadarProduct::all()
            .iter()
            .map(|&p| {
                crate::derive::volume_slot(p)
                    .map(|slot| colormap_lut(p, value_range_for_product(p, slot)))
            })
            .collect()
    });
    ALL[product as usize].as_deref()
}

/// One y row's share of a grid under construction: the row's `nx` cells in
/// each of the `nz` horizontal planes.
struct VoxelRow<'grid> {
    iy: usize,
    indices: Vec<&'grid mut [u8]>,
    /// Empty unless [`VoxelRequest::values_wanted`] asked for the value plane.
    values: Vec<&'grid mut [f32]>,
}

/// A box of `half` about `centre`, as `(x_range_km, y_range_km)` —
/// kilometres east and north of the radar, which is the frame
/// [`VolumeGrid::x_range_km`] and [`VolumeGrid::y_range_km`] report in.
///
/// The extent is taken as already decided and already clamped through
/// [`HalfExtentKm::clamped`]. This is the one definition, because the
/// renderer needs the same two ranges for a box whose grid has not been built
/// yet and `DrawnBox::for_target` compares the two answers with `==`; a second
/// spelling (per-axis `clamp`) differs by an ULP at some inputs and would put
/// the pane permanently on the crop path.
///
/// Polar from the site and back, so this is the same tangent plane the
/// resampler's per-cell mapping uses and a centre at the site lands exactly on
/// `(0, 0)`.
///
/// The lattice is deliberately not anchored to the site. Measured on a KTLX
/// volume, anchoring both axes moved a 4:1 zoom handover from 33.975 to 33.470
/// mean |dRGB|/255 — 1.5% — while moving the settled box by half a fine cell
/// (98 m) is worth 28.090 on its own.
pub fn horizontal_ranges_km(
    centre: (f64, f64),
    half: HalfExtentKm,
    site_lat: f64,
    site_lon: f64,
) -> ((f64, f64), (f64, f64)) {
    let (bearing_deg, range_km) =
        rustdar_geo::site_bearing_range_km(site_lat, site_lon, centre.0, centre.1);
    let bearing = bearing_deg.to_radians();
    let (cx, cy) = (range_km * bearing.sin(), range_km * bearing.cos());
    (
        (cx - half.east_km, cx + half.east_km),
        (cy - half.north_km, cy + half.north_km),
    )
}

/// Resample `scan` onto a Cartesian grid, or `None` if it cannot be done
/// honestly.
pub fn build_voxels<'a>(
    volume: impl Into<crate::nyquist::Volume<'a>>,
    req: &VoxelRequest,
    lat: f64,
    lon: f64,
) -> Option<VolumeGrid> {
    build_voxels_with_motion(volume, req, lat, lon, crate::srv::MotionInputs::default())
}

/// [`build_voxels`] with the user's storm motion override
/// `(speed_kt, direction_from_deg)`, read only when the product is
/// storm-relative velocity.
pub fn build_voxels_with_motion<'a>(
    volume: impl Into<crate::nyquist::Volume<'a>>,
    req: &VoxelRequest,
    lat: f64,
    lon: f64,
    motion: crate::srv::MotionInputs,
) -> Option<VolumeGrid> {
    let volume = volume.into();
    let shape = req.shape;
    if !shape.is_supported() {
        log::warn!(
            "voxel grid refused: shape {}x{}x{} has an axis outside 1..={MAX_AXIS}",
            shape.nx,
            shape.ny,
            shape.nz,
        );
        return None;
    }
    if !(req.half_extent_km.is_none_or(HalfExtentKm::is_finite)
        && req.base_km_msl.is_finite()
        && req.top_km_msl.is_finite()
        && req.centre.0.is_finite()
        && req.centre.1.is_finite()
        && lat.is_finite()
        && lon.is_finite())
    {
        log::warn!("voxel grid refused: a non-finite coordinate in the request or the site");
        return None;
    }
    if req.top_km_msl <= req.base_km_msl {
        log::warn!(
            "voxel grid refused: top {} km MSL is not above base {} km MSL",
            req.top_km_msl,
            req.base_km_msl,
        );
        return None;
    }

    // The derivation seam, shared with `xsect::render_section`: native moments
    // pass through as a borrow; SRV/NROT/KDP are computed per sweep here.
    let slot = crate::derive::volume_slot(req.product)?;
    let prepared = crate::derive::prepare(volume, req.product, motion, lat, lon)?;
    // The declared Nyquist table is keyed by elevation number, which `prepare`
    // preserves, so the field that arrives here was built against the limits
    // this sampler guards on.
    let declared = volume.declared_nyquist();
    let sampler = match &prepared {
        crate::derive::Prepared::Native(scan) => {
            VolumeSampler::new(crate::nyquist::Volume::new(scan, declared), req.product).ok()?
        }
        crate::derive::Prepared::Derived(scan) => VolumeSampler::for_derived(
            crate::nyquist::Volume::new(scan, declared),
            req.product,
            slot,
        )
        .ok()?,
    };

    // The box's extent, decided once and here.
    let half = match req.half_extent_km {
        Some(picked) => picked.clamped(),
        None => HalfExtentKm::square(box_half_width_km(volume_reach_km(
            volume.scan(),
            req.product,
        ))),
    };

    let (x_range_km, y_range_km) = horizontal_ranges_km(req.centre, half, lat, lon);
    let z_range_km_msl = (req.base_km_msl, req.top_km_msl);

    // The same spelling `render.rs` uses for `radar_km_msl`.
    let site_km_msl = crate::eet::radar_height_ft_near(lat, lon, crate::sites::Datum::Feedhorn)
        .unwrap_or(0.0)
        * 0.0003048;

    let transfer = transfer_table_for(req.product, slot)?;

    let (nx, ny, nz) = (shape.nx, shape.ny, shape.nz);
    let cells = shape.cells();
    let mut indices = vec![NO_DATA_INDEX; cells];
    let mut values = req.values_wanted.then(|| vec![f32::NAN; cells]);

    // Heights above the antenna, one per z row.
    let heights_km: Vec<f64> = (0..nz)
        .map(|iz| axis_centre(z_range_km_msl, nz, iz) - site_km_msl)
        .collect();

    let plane = ny * nx;

    // One task per y row, with that row's output cut out of the grid first.
    let mut rows: Vec<VoxelRow<'_>> = (0..ny)
        .map(|iy| VoxelRow {
            iy,
            indices: Vec::with_capacity(nz),
            values: Vec::new(),
        })
        .collect();
    for plane_cells in indices.chunks_mut(plane) {
        for (iy, row) in plane_cells.chunks_mut(nx).enumerate() {
            rows[iy].indices.push(row);
        }
    }
    if let Some(values) = values.as_mut() {
        for plane_cells in values.chunks_mut(plane) {
            for (iy, row) in plane_cells.chunks_mut(nx).enumerate() {
                rows[iy].values.push(row);
            }
        }
    }

    rows.into_par_iter().for_each(|mut row| {
        let y_km = axis_centre(y_range_km, ny, row.iy);
        let mut column = Column::new();
        for ix in 0..nx {
            let x_km = axis_centre(x_range_km, nx, ix);
            let ground_range_km = x_km.hypot(y_km);
            let azimuth_deg = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);
            sampler.column_into(azimuth_deg, ground_range_km, &mut column);

            for (iz, &height_km) in heights_km.iter().enumerate() {
                // One rule for both planes: a sample is carried only if it has
                // a finite number.
                let Some(value) = column
                    .at_height_km(height_km)
                    .value()
                    .filter(|v| v.is_finite())
                else {
                    continue;
                };
                row.indices[iz][ix] = transfer.value_to_index(value);
                if let Some(plane_cells) = row.values.get_mut(iz) {
                    plane_cells[ix] = value;
                }
            }
        }
    });

    Some(VolumeGrid::from_parts(VolumeParts {
        indices,
        values,
        dims: shape,
        anchor: (lat, lon),
        x_range_km,
        y_range_km,
        z_range_km_msl,
        field: crate::fields::spec(req.product).id.clone(),
        transfer,
        levels: sampler.tilt_count(),
        widest_level_gap_deg: sampler.widest_tilt_gap_deg(),
    }))
}

/// The transfer table for `product`, built ONCE here and stored on the grid.
///
/// **One definition, two callers**: the builder bakes it, and the wire decoder
/// rebuilds it from the same two statics the payload is checked against. A
/// second spelling would let a decoded grid disagree with the grid it decoded
/// from about how its own indices become colour.
fn transfer_table_for(product: RadarProduct, slot: MomentSlot) -> Option<TransferTable> {
    Some(transfer_table_over(
        volume_lut_static(product)?.to_vec(),
        product,
        value_range_for_product(product, slot),
    ))
}

/// [`transfer_table_for`] over a table and ramp the caller already holds — the
/// decoder's arm, which keeps the bytes that arrived rather than the statics
/// it just compared them against.
fn transfer_table_over(
    lut: Vec<u8>,
    product: RadarProduct,
    value_range: (f32, f32),
) -> TransferTable {
    TransferTable::new(
        lut,
        if get_legend_scale(product).is_gradient {
            LutFilter::Linear
        } else {
            LutFilter::Nearest
        },
        // Circular: the two ends of the ramp are the same physical value, so a
        // linear filter across the seam returns the opposite phase rather than
        // a blend. True only for differential phase.
        product == RadarProduct::DifferentialPhase,
        value_range,
        iso_shape(product),
        default_iso_threshold(product),
    )
}

// ── Codec ────────────────────────────────────────────────────────────────────
//
// The payload type owns its codec; the job framing that carries it lives in
// `rustdar-worker`'s `offload`. The frame is self-delimiting and
// self-describing — its own magic, version and lengths.

/// Identifies a voxel payload, so a message that is not one fails on its first
/// four bytes instead of being read as a wildly-sized allocation.
const MAGIC: [u8; 4] = *b"RDVX";

/// Bumped whenever the layout below changes.
const FORMAT_VERSION: u16 = 1;

/// Encode for transport, or `None` when this build has no wire code for the
/// grid's field.
///
/// The FieldId <-> wire-code map is **private to this crate**: the payload
/// names its moment as a `RadarProduct::wire_code`, and a grid carrying a
/// field this build does not register has no code to write. Nothing in the
/// tree can produce one — `build_voxels` takes the id from `fields::spec` —
/// so this is a refusal rather than a panic: the impossible case stays
/// checked for one branch, and the alternative is a payload that decodes
/// into a different moment.
pub fn to_bytes(grid: &VolumeGrid) -> Option<Vec<u8>> {
    let product = crate::fields::product_for(grid.field())?;
    let mut out = Vec::with_capacity(encoded_len(grid));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&product.wire_code().to_le_bytes());

    let dims = grid.dims();
    out.extend_from_slice(&(dims.nx as u32).to_le_bytes());
    out.extend_from_slice(&(dims.ny as u32).to_le_bytes());
    out.extend_from_slice(&(dims.nz as u32).to_le_bytes());

    for (lo, hi) in [grid.x_range_km(), grid.y_range_km(), grid.z_range_km_msl()] {
        out.extend_from_slice(&lo.to_le_bytes());
        out.extend_from_slice(&hi.to_le_bytes());
    }
    let anchor = grid.anchor();
    out.extend_from_slice(&anchor.0.to_le_bytes());
    out.extend_from_slice(&anchor.1.to_le_bytes());
    let value_range = grid.value_range();
    out.extend_from_slice(&value_range.0.to_le_bytes());
    out.extend_from_slice(&value_range.1.to_le_bytes());

    // A `u32` for a `usize` field: the ladder has one rung per elevation
    // flown, and the model numbers its cuts in a `u8`.
    out.extend_from_slice(&(grid.levels() as u32).to_le_bytes());
    out.extend_from_slice(&grid.widest_level_gap_deg().to_le_bytes());

    out.extend_from_slice(&(grid.lut().len() as u32).to_le_bytes());
    out.extend_from_slice(grid.lut());
    out.extend_from_slice(&(grid.indices().len() as u32).to_le_bytes());
    out.extend_from_slice(grid.indices());
    match grid.values() {
        None => out.extend_from_slice(&0u32.to_le_bytes()),
        Some(values) => {
            out.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    Some(out)
}

/// Decode a payload [`to_bytes`] produced.
pub fn from_bytes(bytes: &[u8]) -> Option<VolumeGrid> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    if r.u16()? != FORMAT_VERSION {
        return None;
    }
    let product = RadarProduct::from_wire_code(r.u16()?)?;
    // The same refusal `build_voxels` makes: a product with neither a
    // native moment nor a derivation has no ramp its indices decode against.
    let slot = crate::derive::volume_slot(product)?;

    let dims = VoxelShape {
        nx: r.u32()? as usize,
        ny: r.u32()? as usize,
        nz: r.u32()? as usize,
    };
    // Before `cells()`, which multiplies three untrusted numbers: with
    // every axis at or under `MAX_AXIS` the product cannot overflow a
    // 32-bit `usize`, and a zero axis would give a plane length of zero
    // that every later check then agreed with.
    if !dims.is_supported() {
        return None;
    }
    let cells = dims.cells();

    let x_range_km = (r.f64()?, r.f64()?);
    let y_range_km = (r.f64()?, r.f64()?);
    let z_range_km_msl = (r.f64()?, r.f64()?);
    let anchor = (r.f64()?, r.f64()?);
    let value_range = (r.f32()?, r.f32()?);
    let levels = r.u32()? as usize;
    let widest_level_gap_deg = r.f64()?;

    // Every number that describes where the box is.
    if ![
        x_range_km.0,
        x_range_km.1,
        y_range_km.0,
        y_range_km.1,
        z_range_km_msl.0,
        z_range_km_msl.1,
        anchor.0,
        anchor.1,
        widest_level_gap_deg,
    ]
    .iter()
    .all(|v| v.is_finite())
        || !value_range.0.is_finite()
        || !value_range.1.is_finite()
    {
        return None;
    }

    // `value_range` and the table are both functions of the product, so a payload
    // states each twice and the copies can disagree without failing anything.
    if value_range != value_range_for_product(product, slot) {
        return None;
    }

    // One byte per element, so `take` is the bound: nothing is reserved on
    // the claimed length.
    let lut_len = r.u32()?;
    let lut = r.take(lut_len as usize)?.to_vec();
    if lut.len() != LUT_LEN || Some(lut.as_slice()) != volume_lut_static(product) {
        return None;
    }
    let index_len = r.u32()?;
    let indices = r.take(index_len as usize)?.to_vec();
    if indices.len() != cells {
        return None;
    }

    // Four bytes per element, so the claimed count is measured against what
    // remains before it becomes a capacity: a believed `u32::MAX` would
    // otherwise reserve 16 GiB and then fail the read.
    let value_len = r.u32()?;
    let value_len = r.bounded(value_len, 4)?;
    let values = match value_len {
        // `is_supported` put at least one cell in the grid, so zero can
        // only mean "no plane".
        0 => None,
        n if n == cells => {
            let mut values = Vec::with_capacity(n);
            for _ in 0..n {
                values.push(r.f32()?);
            }
            Some(values)
        }
        // Any other length is a plane that does not describe this grid.
        _ => return None,
    };

    // Trailing bytes mean the two ends disagree about the layout even
    // though the version matched.
    if !r.at_end() {
        return None;
    }
    Some(VolumeGrid::from_parts(VolumeParts {
        indices,
        values,
        dims,
        anchor,
        x_range_km,
        y_range_km,
        z_range_km_msl,
        field: crate::fields::spec(product).id.clone(),
        // The table the bytes carried, not a second one: `lut` and
        // `value_range` were just proven equal to the statics above.
        transfer: transfer_table_over(lut, product, value_range),
        levels,
        widest_level_gap_deg,
    }))
}

/// What [`to_bytes`] will write, exactly.
fn encoded_len(grid: &VolumeGrid) -> usize {
    // Magic, version, product, three axes, three ranges, the anchor, the
    // value range, the level count and the widest gap.
    let header = 4 + 2 + 2 + 3 * 4 + 3 * 16 + 16 + 8 + 4 + 8;
    header
        + (4 + grid.lut().len())
        + (4 + grid.indices().len())
        + (4 + grid.values().map_or(0, |v| v.len() * 4))
}

/// A bounds-checked cursor.
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

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// `count` as a capacity, refused if the buffer cannot possibly hold that
    /// many items of `min_size` bytes each.
    fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

#[cfg(test)]
mod tests;
