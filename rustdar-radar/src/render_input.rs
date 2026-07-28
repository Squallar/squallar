//! The renderer's input, in a form that can cross a process — or a Web Worker —
//! boundary.
//!
//! [`crate::render`] takes a whole `&Scan`: a decoded volume of tens of
//! megabytes, holding every moment of every radial of every sweep. It *reads*
//! almost none of that. `find_sweep` picks one sweep and the rasterizer then
//! touches only `product.get_moment(radial)` on it; two products reach wider —
//! `NormalizedRotation` fits its wind profile from every velocity tilt, and
//! `EchoTopsInterpolated` integrates every reflectivity tilt — and nothing
//! reads the coverage pattern, the site, the collection timestamps, the radial
//! statuses, or any moment other than the product's own.
//!
//! [`RenderInput`] is that reachable subset, flattened. For a normal product it
//! is one sweep: ~1.3 MB for a 720 × 1832 8-bit moment, ~2.6 MB for 16-bit
//! dual-pol. NROT without an external wind profile carries every velocity tilt
//! (~10-14 MB) and interpolated echo tops every reflectivity tilt (~20 MB).
//! Against a `Scan` that is a large reduction, and it is the difference between
//! a payload a browser can post per render and one it cannot.
//!
//! # Why it reconstructs a `Scan` instead of replacing it
//!
//! [`RenderInput::to_scan`] rebuilds a `nexrad_model::data::Scan` holding
//! exactly the extracted sweeps, and [`crate::render::render_from`] runs the
//! ordinary renderer over it. The alternative — reshaping four rasterizers,
//! `build_velocity_grid`, `build_wind_profile` and `VolumeCube` to consume a
//! second input type — would give the project two descriptions of the same
//! data and two chances for them to disagree about a pixel.
//!
//! This way there is one renderer. The web path and the desktop path differ
//! only in *where* the `Scan` came from, and
//! `render_from_an_extracted_payload_matches_the_scan_path` pins that they
//! agree byte for byte.
//!
//! Reconstruction is exact, not approximate:
//! [`nexrad_model::data::MomentData::from_fixed_point`] takes the same
//! fixed-point fields the decoder produced, and the gate bytes are carried raw,
//! so the reconstructed moment decodes to the identical values. The fields
//! `Radial::new` demands but the renderer never reads (collection timestamp,
//! azimuth number, radial status, elevation number) are filled with the
//! placeholders in [`to_scan`](RenderInput::to_scan); if a renderer ever starts
//! reading one, the byte-identity test is what fails.

use crate::types::{MomentSlot, RadarProduct};
use nexrad_model::data::{
    DataMoment, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};

/// Everything [`crate::render::render_from`] needs to produce a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderInput {
    product: RadarProduct,
    /// The elevation the *request* asked for, not the angle any sweep carries.
    /// `find_sweep` re-runs against it on the reconstructed scan and must reach
    /// the same sweep, which is why [`RenderInput::extract`] keeps sweeps in
    /// their original order.
    elevation: f32,
    radar_lat: f64,
    radar_lon: f64,
    wind_levels: Option<Vec<(f64, f64, f64)>>,
    sweeps: Vec<SweepData>,
}

/// One sweep's worth of the product's moment.
#[derive(Debug, Clone, PartialEq)]
struct SweepData {
    /// Carried per radial rather than per sweep by the model, but constant
    /// across a sweep in practice and only read off the first radial.
    elevation_angle: f32,
    radials: Vec<RadialData>,
}

#[derive(Debug, Clone, PartialEq)]
struct RadialData {
    azimuth: f32,
    azimuth_spacing: f32,
    /// `None` for a radial that carries no data for this product. Real sweeps
    /// have them, and both `sweep_to_grid` and the rasterizer skip them, so the
    /// distinction has to survive the round trip.
    moment: Option<MomentPayload>,
}

/// A moment block in the fixed-point form the decoder produced it in, so
/// `MomentData::from_fixed_point` can rebuild it exactly.
#[derive(Debug, Clone, PartialEq)]
struct MomentPayload {
    gate_count: u16,
    /// Metres. `MomentDataBlock` stores this as a `u16` of metres and exposes
    /// it as `km = raw * 0.001`; the model offers no raw accessor, so the
    /// kilometre value is scaled back and rounded. Exact for every `u16`.
    first_gate_range_m: u16,
    gate_interval_m: u16,
    word_size: u8,
    scale: f32,
    offset: f32,
    /// Raw gate codes, exactly as `DataMoment::raw_values` returns them: one
    /// byte per gate at 8-bit, a big-endian pair at 16-bit.
    gates: Vec<u8>,
}

impl RenderInput {
    /// The reachable subset of `scan` for this request, or `None` when the
    /// request cannot be rendered at all.
    ///
    /// `None` is returned exactly where [`crate::render`] would have returned
    /// it: a product with no Level II moment behind it, or no sweep in the
    /// requested tilt family carrying one.
    pub fn extract(
        scan: &Scan,
        elevation: f32,
        product: RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
        wind_levels: Option<&[(f64, f64, f64)]>,
    ) -> Option<Self> {
        let slot = product.moment_slot()?;
        // `None` for a Level III product: no Level II moment stands behind it,
        // so there is nothing to extract and nothing the renderer would draw.
        //
        // Two products then need every tilt carrying that moment; anything else
        // needs one sweep.
        let whole_volume = match product {
            // Tilt-independent: `compute_echo_tops` integrates the whole
            // reflectivity volume. `VolumeCube::build` dedups same-elevation
            // cuts in encounter order, which is why the original order is kept.
            RadarProduct::EchoTopsInterpolated => true,
            // The selected sweep rasterizes, but `build_wind_profile` fits the
            // volume profile from every velocity tilt — and only when the
            // caller's levels do not already yield a profile. Asking
            // `WindProfile::from_levels` here is the same question
            // `render_nrot_to_image` asks, so the two agree on whether the
            // extra tilts are needed.
            RadarProduct::NormalizedRotation => wind_levels
                .and_then(crate::nrot::WindProfile::from_levels)
                .is_none(),
            _ => false,
        };

        let sweeps = if whole_volume {
            collect_sweeps(scan, slot)
        } else {
            // One sweep: whichever `find_sweep` would have chosen. Selecting
            // here, against the whole volume, is the point — the reconstructed
            // scan has only this sweep to offer, so `find_sweep` reaches it
            // again whatever its preference rules do.
            vec![sweep_data(
                crate::render::find_sweep(scan, product, elevation)?,
                slot,
            )]
        };
        // `collect_sweeps` can come back empty on a volume that carries the
        // product nowhere. The renderer answers `None` for that, so this must
        // too rather than shipping a payload that renders nothing.
        if sweeps.is_empty() {
            return None;
        }

        Some(Self {
            product,
            elevation,
            radar_lat,
            radar_lon,
            wind_levels: wind_levels.map(<[(f64, f64, f64)]>::to_vec),
            sweeps,
        })
    }

    pub fn product(&self) -> RadarProduct {
        self.product
    }

    pub fn elevation(&self) -> f32 {
        self.elevation
    }

    pub fn radar_lat(&self) -> f64 {
        self.radar_lat
    }

    pub fn radar_lon(&self) -> f64 {
        self.radar_lon
    }

    pub fn wind_levels(&self) -> Option<&[(f64, f64, f64)]> {
        self.wind_levels.as_deref()
    }

    /// A `Scan` holding exactly the extracted sweeps.
    ///
    /// The coverage pattern is a placeholder: nothing on any render path reads
    /// it, or the site, or a radial's timestamp, azimuth number, status or
    /// elevation number. The moments are rebuilt from their fixed-point fields
    /// and raw gate bytes, so they decode to the identical values.
    pub fn to_scan(&self) -> Scan {
        // Always `Some`: both constructors refuse a product with no Level II
        // field. Degrading to "no moments" rather than panicking keeps a
        // hand-crafted payload off a message port from taking the tab down; it
        // renders nothing, which is what such a request means anyway.
        let slot = self.product.moment_slot();
        let sweeps = self
            .sweeps
            .iter()
            .enumerate()
            .map(|(si, sweep)| {
                let radials = sweep
                    .radials
                    .iter()
                    .map(|radial| {
                        let moment = radial.moment.as_ref().map(MomentPayload::to_moment_data);
                        // Put back on the field it was read from — the same
                        // `MomentSlot` `get_moment` resolves this product to,
                        // so the reconstructed radial answers `get_moment` with
                        // the moment that was extracted.
                        let (reflectivity, velocity, spectrum_width, zdr, phi, rho) =
                            place_moment(slot, moment);
                        Radial::new(
                            0,
                            0,
                            radial.azimuth,
                            radial.azimuth_spacing,
                            RadialStatus::Unknown(0),
                            si as u8,
                            sweep.elevation_angle,
                            reflectivity,
                            velocity,
                            spectrum_width,
                            zdr,
                            phi,
                            rho,
                            None,
                        )
                    })
                    .collect();
                Sweep::new(si as u8, radials)
            })
            .collect();

        Scan::new(placeholder_coverage_pattern(), sweeps)
    }
}

/// Every sweep whose first radial carries `slot`'s moment, in scan order.
fn collect_sweeps(scan: &Scan, slot: MomentSlot) -> Vec<SweepData> {
    scan.sweeps()
        .iter()
        .filter_map(|sweep| {
            let radials = sweep.radials();
            slot.read(radials.first()?)
                .is_some()
                .then(|| sweep_data(radials, slot))
        })
        .collect()
}

/// Flatten one sweep, carrying `slot`'s moment and nothing else.
///
/// `slot` comes from the caller rather than being probed off the radial: a
/// merged upper tilt carries reflectivity *and* velocity, so "the first moment
/// this radial has" would hand a reflectivity render the velocity gates.
fn sweep_data(radials: &[Radial], slot: MomentSlot) -> SweepData {
    SweepData {
        elevation_angle: radials
            .first()
            .map(Radial::elevation_angle_degrees)
            .unwrap_or(0.0),
        radials: radials
            .iter()
            .map(|radial| RadialData {
                azimuth: radial.azimuth_angle_degrees(),
                azimuth_spacing: radial.azimuth_spacing_degrees(),
                moment: slot.read(radial).map(MomentPayload::from_moment_data),
            })
            .collect(),
    }
}

/// The six `Option<MomentData>` arguments `Radial::new` takes, in its order.
type MomentSlots = (
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
);

/// Put `moment` back on the field it was read from.
///
/// The inverse of [`MomentSlot::read`], and the reason `MomentSlot` exists:
/// `get_moment` can only fetch, and rebuilding a radial needs the field named.
///
/// `slot` is `None` only for a product with no Level II field, which neither
/// constructor produces; the moment is then dropped rather than guessed at.
fn place_moment(slot: Option<MomentSlot>, moment: Option<MomentData>) -> MomentSlots {
    let mut slots: MomentSlots = (None, None, None, None, None, None);
    let Some(slot) = slot else { return slots };
    match slot {
        MomentSlot::Reflectivity => slots.0 = moment,
        MomentSlot::Velocity => slots.1 = moment,
        MomentSlot::SpectrumWidth => slots.2 = moment,
        MomentSlot::DifferentialReflectivity => slots.3 = moment,
        MomentSlot::DifferentialPhase => slots.4 = moment,
        MomentSlot::CorrelationCoefficient => slots.5 = moment,
    }
    slots
}

/// Nothing on a render path reads the coverage pattern, but `Scan::new`
/// requires one. Pattern number 0 is not a real VCP, which is the point: a
/// consumer that starts reading this sees an obviously synthetic value rather
/// than a plausible wrong one.
fn placeholder_coverage_pattern() -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        0,
        0,
        0.0,
        PulseWidth::Unknown,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    )
}

impl MomentPayload {
    fn from_moment_data(moment: &MomentData) -> Self {
        Self {
            gate_count: moment.gate_count(),
            first_gate_range_m: km_to_metres(moment.first_gate_range_km()),
            gate_interval_m: km_to_metres(moment.gate_interval_km()),
            word_size: moment.data_word_size(),
            scale: moment.scale(),
            offset: moment.offset(),
            gates: moment.raw_values().to_vec(),
        }
    }

    fn to_moment_data(&self) -> MomentData {
        MomentData::from_fixed_point(
            self.gate_count,
            self.first_gate_range_m,
            self.gate_interval_m,
            self.word_size,
            self.scale,
            self.offset,
            self.gates.clone(),
        )
    }
}

/// Undo `MomentDataBlock`'s `raw as f64 * 0.001`.
///
/// `0.001` is not exact in binary, so the product is not exactly the integer
/// metres that went in; rounding recovers it, and does so for every `u16` the
/// field can hold.
fn km_to_metres(km: f64) -> u16 {
    (km * 1000.0).round().clamp(0.0, u16::MAX as f64) as u16
}

// ── Codec ────────────────────────────────────────────────────────────────────

/// Identifies the payload, so a message that is not one fails on its first four
/// bytes instead of being read as a wildly-sized allocation.
const MAGIC: [u8; 4] = *b"RDRI";

/// Bumped whenever the layout below changes. The two ends of a worker boundary
/// can be different builds — see `rustdar-web`'s build-token handshake — so a
/// mismatch has to be a clean `None`, not a misparse.
const FORMAT_VERSION: u16 = 1;

impl RenderInput {
    /// Encode for transport. Little-endian throughout; gate blobs are copied
    /// verbatim, which is where nearly all the bytes are.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&self.product.wire_code().to_le_bytes());
        out.extend_from_slice(&self.elevation.to_le_bytes());
        out.extend_from_slice(&self.radar_lat.to_le_bytes());
        out.extend_from_slice(&self.radar_lon.to_le_bytes());

        match &self.wind_levels {
            // `u32::MAX` distinguishes "no profile was offered" from "an empty
            // one was". `render_nrot_to_image` treats them differently: `None`
            // falls through to the volume fit, `Some(&[])` does not.
            None => out.extend_from_slice(&u32::MAX.to_le_bytes()),
            Some(levels) => {
                out.extend_from_slice(&(levels.len() as u32).to_le_bytes());
                for (h, u, v) in levels {
                    out.extend_from_slice(&h.to_le_bytes());
                    out.extend_from_slice(&u.to_le_bytes());
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }

        out.extend_from_slice(&(self.sweeps.len() as u32).to_le_bytes());
        for sweep in &self.sweeps {
            out.extend_from_slice(&sweep.elevation_angle.to_le_bytes());
            out.extend_from_slice(&(sweep.radials.len() as u32).to_le_bytes());
            for radial in &sweep.radials {
                out.extend_from_slice(&radial.azimuth.to_le_bytes());
                out.extend_from_slice(&radial.azimuth_spacing.to_le_bytes());
                match &radial.moment {
                    None => out.push(0),
                    Some(moment) => {
                        out.push(1);
                        out.extend_from_slice(&moment.gate_count.to_le_bytes());
                        out.extend_from_slice(&moment.first_gate_range_m.to_le_bytes());
                        out.extend_from_slice(&moment.gate_interval_m.to_le_bytes());
                        out.push(moment.word_size);
                        out.extend_from_slice(&moment.scale.to_le_bytes());
                        out.extend_from_slice(&moment.offset.to_le_bytes());
                        out.extend_from_slice(&(moment.gates.len() as u32).to_le_bytes());
                        out.extend_from_slice(&moment.gates);
                    }
                }
            }
        }
        out
    }

    /// Decode a payload [`to_bytes`](Self::to_bytes) produced.
    ///
    /// `None` on anything malformed — wrong magic, unknown version, truncation,
    /// a product code this build does not have. Every length is checked against
    /// what remains before it is used, so a corrupt frame cannot ask for a
    /// large allocation.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.take(4)? != MAGIC {
            return None;
        }
        if r.u16()? != FORMAT_VERSION {
            return None;
        }
        let product = RadarProduct::from_wire_code(r.u16()?)?;
        // The same refusal `extract` makes. A payload naming a Level III
        // product has no moment any field could hold, so it could only ever
        // render nothing; refusing it here keeps that from looking like a
        // renderer that found no sweep.
        product.moment_slot()?;
        let elevation = r.f32()?;
        let radar_lat = r.f64()?;
        let radar_lon = r.f64()?;

        let wind_count = r.u32()?;
        let wind_levels = if wind_count == u32::MAX {
            None
        } else {
            let mut levels = Vec::with_capacity(r.bounded(wind_count, 24)?);
            for _ in 0..wind_count {
                levels.push((r.f64()?, r.f64()?, r.f64()?));
            }
            Some(levels)
        };

        let sweep_count = r.u32()?;
        // A sweep costs at least its own header, so this bounds the count
        // against what is actually left rather than trusting it.
        let mut sweeps = Vec::with_capacity(r.bounded(sweep_count, 8)?);
        for _ in 0..sweep_count {
            let elevation_angle = r.f32()?;
            let radial_count = r.u32()?;
            let mut radials = Vec::with_capacity(r.bounded(radial_count, 9)?);
            for _ in 0..radial_count {
                let azimuth = r.f32()?;
                let azimuth_spacing = r.f32()?;
                let moment = match r.u8()? {
                    0 => None,
                    1 => {
                        let gate_count = r.u16()?;
                        let first_gate_range_m = r.u16()?;
                        let gate_interval_m = r.u16()?;
                        let word_size = r.u8()?;
                        let scale = r.f32()?;
                        let offset = r.f32()?;
                        let gate_len = r.u32()?;
                        let gates = r.take(gate_len as usize)?.to_vec();
                        Some(MomentPayload {
                            gate_count,
                            first_gate_range_m,
                            gate_interval_m,
                            word_size,
                            scale,
                            offset,
                            gates,
                        })
                    }
                    _ => return None,
                };
                radials.push(RadialData {
                    azimuth,
                    azimuth_spacing,
                    moment,
                });
            }
            sweeps.push(SweepData {
                elevation_angle,
                radials,
            });
        }

        // Trailing bytes mean the two ends disagree about the layout even
        // though the version matched. Better to refuse than to render half a
        // frame from it.
        r.at_end().then_some(Self {
            product,
            elevation,
            radar_lat,
            radar_lon,
            wind_levels,
            sweeps,
        })
    }

    fn encoded_len(&self) -> usize {
        let header = 4 + 2 + 2 + 4 + 8 + 8 + 4;
        let winds = self.wind_levels.as_ref().map_or(0, |l| l.len() * 24);
        let sweeps: usize = self
            .sweeps
            .iter()
            .map(|s| {
                8 + s
                    .radials
                    .iter()
                    .map(|r| 9 + r.moment.as_ref().map_or(0, |m| 19 + m.gates.len()))
                    .sum::<usize>()
            })
            .sum();
        header + winds + 4 + sweeps
    }
}

/// A bounds-checked cursor. Every accessor returns `None` rather than panicking,
/// because the bytes come off a message port and are not trusted.
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

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
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
    /// many items of `min_size` bytes each. Keeps a corrupt length from
    /// reserving gigabytes before the read fails.
    fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{render_from, render_radar_to_image_with_winds};

    const LAT: f64 = 35.3333;
    const LON: f64 = -97.2778;
    /// The standard Level II reflectivity encoding: `dBZ = (raw - 66) / 2`.
    const REFL_SCALE: f32 = 2.0;
    const REFL_OFFSET: f32 = 66.0;
    /// Velocity at 0.5 m/s resolution: `m/s = (raw - 129) / 2`.
    const VEL_SCALE: f32 = 2.0;
    const VEL_OFFSET: f32 = 129.0;
    const RADIALS: usize = 360;

    fn moment(scale: f32, offset: f32, byte: u8, gates: usize) -> MomentData {
        MomentData::from_fixed_point(gates as u16, 0, 250, 8, scale, offset, vec![byte; gates])
    }

    /// One sweep at `elevation`, `RADIALS` radials spaced evenly from 0°.
    ///
    /// `refl` and `vel` are per-radial byte generators; `None` leaves that
    /// moment absent, which is how a surveillance cut is told from a Doppler
    /// one.
    fn sweep(
        elevation: f32,
        refl: Option<&dyn Fn(usize) -> u8>,
        vel: Option<&dyn Fn(usize) -> u8>,
    ) -> Sweep {
        let radials = (0..RADIALS)
            .map(|i| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / RADIALS as f32),
                    360.0 / RADIALS as f32,
                    RadialStatus::IntermediateRadialData,
                    1,
                    elevation,
                    refl.map(|f| moment(REFL_SCALE, REFL_OFFSET, f(i), 600)),
                    vel.map(|f| moment(VEL_SCALE, VEL_OFFSET, f(i), 400)),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(1, radials)
    }

    /// Strong, uniform reflectivity — well past the echo-tops threshold so the
    /// interpolated path paints.
    fn strong_refl(_: usize) -> u8 {
        200
    }

    fn weaker_refl(_: usize) -> u8 {
        150
    }

    /// Eight cycles of ±35 m/s: enough azimuthal shear to survive the NROT fit,
    /// the range normalization and the display threshold.
    fn shear(i: usize) -> u8 {
        let theta = i as f64 / RADIALS as f64 * std::f64::consts::TAU;
        (129.0 + 35.0 * (8.0 * theta).sin() * 2.0)
            .round()
            .clamp(2.0, 254.0) as u8
    }

    /// A volume shaped like a real SAILS one: a 0.5° surveillance cut carrying
    /// only reflectivity, a 0.5° Doppler cut carrying both, and a merged 1.5°
    /// tilt carrying both.
    ///
    /// The two 0.5° cuts are what make `find_sweep`'s surveillance preference
    /// observable, and the cuts carrying *both* moments are what would catch a
    /// payload that guessed at which moment to carry.
    fn volume() -> Scan {
        Scan::new(
            placeholder_coverage_pattern(),
            vec![
                sweep(0.5, Some(&strong_refl), None),
                sweep(0.5, Some(&weaker_refl), Some(&shear)),
                sweep(1.5, Some(&weaker_refl), Some(&shear)),
            ],
        )
    }

    /// Byte-for-byte on the image, element-for-element on the value grid.
    /// `f32::NAN != f32::NAN`, and the grid is NaN wherever no gate claimed the
    /// pixel — which is most of it — so a naive compare would pass on two
    /// entirely blank renders.
    fn assert_same_frame(
        left: &(Vec<u8>, f64, Vec<f32>),
        right: &(Vec<u8>, f64, Vec<f32>),
        what: &str,
    ) {
        assert_eq!(left.0, right.0, "{what}: RGBA differs");
        assert_eq!(left.1, right.1, "{what}: max range differs");
        assert_eq!(
            left.2.len(),
            right.2.len(),
            "{what}: value grid length differs"
        );
        for (i, (a, b)) in left.2.iter().zip(&right.2).enumerate() {
            assert!(
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
                "{what}: value {i} differs: {a} vs {b}"
            );
        }
    }

    fn painted(frame: &(Vec<u8>, f64, Vec<f32>)) -> usize {
        frame.0.chunks_exact(4).filter(|px| px[3] != 0).count()
    }

    /// The acceptance criterion for moving rasterization into a worker: the
    /// payload path and the whole-volume path produce the same frame, for every
    /// product shape — one sweep, velocity-derived, and whole-volume.
    #[test]
    fn render_from_an_extracted_payload_matches_the_scan_path() {
        let scan = volume();
        for product in [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::NormalizedRotation,
            RadarProduct::EchoTopsInterpolated,
        ] {
            let direct =
                render_radar_to_image_with_winds(&scan, 0.5, product, LAT, LON, None).unwrap();
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None).unwrap();
            let viaformat = render_from(&input).unwrap();

            assert!(
                painted(&direct) > 1_000,
                "{product:?} painted only {} pixels — the comparison would be vacuous",
                painted(&direct)
            );
            assert_same_frame(&direct, &viaformat, &format!("{product:?}"));
        }
    }

    /// The same, across the wire format the worker actually receives.
    #[test]
    fn a_payload_renders_the_same_frame_after_a_round_trip() {
        let scan = volume();
        for product in [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::NormalizedRotation,
            RadarProduct::EchoTopsInterpolated,
        ] {
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None).unwrap();
            let decoded = RenderInput::from_bytes(&input.to_bytes())
                .unwrap_or_else(|| panic!("{product:?} payload did not decode"));
            assert_eq!(input, decoded, "{product:?} payload changed in transit");
            assert_same_frame(
                &render_from(&input).unwrap(),
                &render_from(&decoded).unwrap(),
                &format!("{product:?} round trip"),
            );
        }
    }

    /// `to_bytes` reserves exactly what it writes. Wrong by a little is only a
    /// realloc; wrong by a lot means the layout and the estimate have drifted.
    #[test]
    fn the_encoded_length_estimate_is_exact() {
        let scan = volume();
        for product in [RadarProduct::Reflectivity, RadarProduct::NormalizedRotation] {
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None).unwrap();
            assert_eq!(input.encoded_len(), input.to_bytes().len(), "{product:?}");
        }
    }

    /// A merged tilt carries reflectivity *and* velocity. Reading "whichever
    /// moment this radial has" off it would hand a reflectivity render the
    /// velocity gates — a frame that renders, looks like weather, and is wrong.
    #[test]
    fn a_tilt_carrying_both_moments_still_yields_the_requested_one() {
        let scan = Scan::new(
            placeholder_coverage_pattern(),
            vec![sweep(0.5, Some(&strong_refl), Some(&shear))],
        );
        let input =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None).unwrap();
        let moment = input.sweeps[0].radials[0].moment.as_ref().unwrap();
        assert_eq!(moment.scale, REFL_SCALE);
        assert_eq!(moment.offset, REFL_OFFSET);
        assert_eq!(
            moment.gates[0],
            strong_refl(0),
            "carried the velocity gates under the reflectivity request"
        );
    }

    /// The sizing decision the whole design rests on: a normal product ships
    /// one sweep, not the volume.
    #[test]
    fn a_plain_product_carries_one_sweep() {
        let scan = volume();
        let input =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None).unwrap();
        assert_eq!(input.sweeps.len(), 1);
        assert_eq!(input.sweeps[0].radials.len(), RADIALS);
    }

    /// NROT fits its wind profile from every velocity tilt, so those have to
    /// travel with it — but only when the caller has not supplied a profile
    /// that already works. Both branches must render identically to the
    /// whole-volume path, which is what makes the smaller payload safe.
    #[test]
    fn nrot_carries_every_velocity_tilt_only_when_it_needs_them() {
        let scan = volume();
        let without =
            RenderInput::extract(&scan, 0.5, RadarProduct::NormalizedRotation, LAT, LON, None)
                .unwrap();
        assert_eq!(
            without.sweeps.len(),
            2,
            "both velocity tilts should travel when no profile was supplied"
        );

        // A profile `WindProfile::from_levels` accepts: the extra tilts are
        // then dead weight, because `render_nrot_to_image` never fits from the
        // volume.
        let levels: Vec<(f64, f64, f64)> = (0..12).map(|i| (i as f64 * 0.3, 10.0, -5.0)).collect();
        assert!(
            crate::nrot::WindProfile::from_levels(&levels).is_some(),
            "fixture profile must be one the renderer would accept"
        );
        let with = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::NormalizedRotation,
            LAT,
            LON,
            Some(&levels),
        )
        .unwrap();
        assert_eq!(
            with.sweeps.len(),
            1,
            "a usable profile makes the tilts dead weight"
        );

        assert_same_frame(
            &render_radar_to_image_with_winds(
                &scan,
                0.5,
                RadarProduct::NormalizedRotation,
                LAT,
                LON,
                Some(&levels),
            )
            .unwrap(),
            &render_from(&with).unwrap(),
            "NROT with a supplied profile",
        );
    }

    /// Interpolated echo tops integrate the volume; every reflectivity tilt has
    /// to be there, in scan order, because `VolumeCube::build` dedups
    /// same-elevation cuts by encounter.
    #[test]
    fn interpolated_echo_tops_carries_every_reflectivity_tilt() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::EchoTopsInterpolated,
            LAT,
            LON,
            None,
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 3);
        assert_eq!(
            input
                .sweeps
                .iter()
                .map(|s| s.elevation_angle)
                .collect::<Vec<_>>(),
            vec![0.5, 0.5, 1.5],
            "scan order decides which same-elevation cut wins",
        );
    }

    /// A product with no Level II moment behind it renders nothing today, and
    /// must not produce a payload that pretends otherwise.
    #[test]
    fn a_product_with_no_level_two_moment_extracts_nothing() {
        let scan = volume();
        assert!(
            RenderInput::extract(
                &scan,
                0.5,
                RadarProduct::HydrometeorClassification,
                LAT,
                LON,
                None
            )
            .is_none()
        );
        assert!(
            render_radar_to_image_with_winds(
                &scan,
                0.5,
                RadarProduct::HydrometeorClassification,
                LAT,
                LON,
                None
            )
            .is_none(),
            "the payload and the renderer must refuse the same requests"
        );
    }

    /// "No profile offered" and "an empty profile offered" are different
    /// questions to `render_nrot_to_image`: the first falls through to the
    /// volume fit, the second does not.
    #[test]
    fn an_absent_wind_profile_is_not_an_empty_one() {
        let scan = volume();
        let absent =
            RenderInput::extract(&scan, 0.5, RadarProduct::Velocity, LAT, LON, None).unwrap();
        let empty =
            RenderInput::extract(&scan, 0.5, RadarProduct::Velocity, LAT, LON, Some(&[])).unwrap();
        assert_eq!(absent.wind_levels(), None);
        assert_eq!(empty.wind_levels(), Some(&[][..]));
        assert_eq!(
            RenderInput::from_bytes(&absent.to_bytes())
                .unwrap()
                .wind_levels(),
            None
        );
        assert_eq!(
            RenderInput::from_bytes(&empty.to_bytes())
                .unwrap()
                .wind_levels(),
            Some(&[][..])
        );
    }

    /// The bytes arrive off a message port. Every malformed shape has to be a
    /// clean `None` — the two ends of that port can be different builds.
    #[test]
    fn a_malformed_payload_is_refused_rather_than_misread() {
        let scan = volume();
        let good = RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None)
            .unwrap()
            .to_bytes();

        assert!(RenderInput::from_bytes(&[]).is_none(), "empty");
        assert!(RenderInput::from_bytes(b"nope").is_none(), "wrong magic");

        let mut wrong_version = good.clone();
        wrong_version[4] = 0xFF;
        wrong_version[5] = 0xFF;
        assert!(RenderInput::from_bytes(&wrong_version).is_none(), "version");

        let mut wrong_product = good.clone();
        wrong_product[6] = 0xFE;
        wrong_product[7] = 0xFF;
        assert!(RenderInput::from_bytes(&wrong_product).is_none(), "product");

        for cut in [1, 8, 32, good.len() / 2, good.len() - 1] {
            assert!(
                RenderInput::from_bytes(&good[..cut]).is_none(),
                "truncated to {cut} bytes"
            );
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(
            RenderInput::from_bytes(&trailing).is_none(),
            "trailing bytes mean the layouts disagree"
        );
    }

    /// A corrupt length must not be believed far enough to reserve on it.
    #[test]
    fn an_absurd_length_does_not_reach_an_allocation() {
        let scan = volume();
        let mut bytes =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None)
                .unwrap()
                .to_bytes();
        // The sweep count sits directly after the header and the `u32::MAX`
        // wind marker.
        let at = 4 + 2 + 2 + 4 + 8 + 8 + 4;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(RenderInput::from_bytes(&bytes).is_none());
    }

    /// Round-tripping through kilometres and back is exact for every value the
    /// field can hold, which is what makes the reconstructed moment identical.
    #[test]
    fn gate_ranges_survive_the_kilometre_round_trip() {
        for raw in [0u16, 1, 250, 999, 2125, 32768, u16::MAX] {
            assert_eq!(km_to_metres(raw as f64 * 0.001), raw);
        }
    }

    /// The wire codes are a fixed table, not the enum's declaration order:
    /// reordering the variants must not silently change what a payload means.
    #[test]
    fn every_product_has_a_stable_distinct_wire_code() {
        let products = [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialPhase,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpecificDifferentialPhase,
            RadarProduct::EchoTops,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::HydrometeorClassification,
            RadarProduct::PrecipitationRate,
            RadarProduct::NormalizedRotation,
        ];
        let mut seen = std::collections::HashSet::new();
        for product in products {
            let code = product.wire_code();
            assert!(seen.insert(code), "{product:?} reuses wire code {code}");
            assert_eq!(RadarProduct::from_wire_code(code), Some(product));
        }
        assert_eq!(RadarProduct::from_wire_code(0), None);
        assert_eq!(RadarProduct::from_wire_code(u16::MAX), None);
    }
}
