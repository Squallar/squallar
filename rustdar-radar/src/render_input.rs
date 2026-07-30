//! The renderer's input, in a form that can cross a process — or a Web Worker —
//! boundary.
//!
//! [`crate::render`] takes a whole `&Scan`: a decoded volume of tens of
//! megabytes, holding every moment of every radial of every sweep. It *reads*
//! almost none of that. `find_sweep` picks one sweep and the rasterizer then
//! touches only `product.get_moment(radial)` on it — unless the product is one
//! [`RadarProduct::reads_whole_volume`] names, which reaches every tilt
//! carrying its moment, and for the hybrid classification every *other* moment
//! of those tilts too. Nothing reads the coverage pattern, the site, the
//! collection timestamps or the radial statuses.
//!
//! [`RenderInput`] is that reachable subset, flattened. For a normal product it
//! is one sweep: ~1.3 MB for a 720 × 1832 8-bit moment, ~2.6 MB for 16-bit
//! dual-pol. A whole-volume product carries every tilt its moment appears on
//! instead: NROT and SRV every velocity tilt (~10-14 MB), interpolated echo tops
//! and the hail pair every reflectivity tilt (~20 MB). The hybrid classification
//! is the outlier and much the largest — it takes every tilt carrying *any*
//! moment and, on each, the other five moments as well (`RadialData::extras`),
//! several of them 16-bit, so it runs several times the reflectivity figure
//! rather than alongside it.
//! Against a `Scan` even that is a large reduction, and for everything else it
//! is the difference between a payload a browser can post per render and one it
//! cannot.
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
    /// The user's storm motion vector, knots and degrees-from. Read by
    /// storm-relative velocity alone; `None` means "no override", which SRV
    /// answers with the Bunkers right-mover from the volume's own profile.
    storm_motion_override: Option<(f32, f32)>,
    /// The site's environmental 0 °C / −20 °C heights, km MSL
    /// ([`crate::sounding::EnvHeights`]). Read by the hail pair and the
    /// hybrid hydrometeor classification. `None` means different things to
    /// each: the hail field is undefined and its render answers nothing
    /// ([`crate::hail`]), while the HHC falls back to the operational
    /// adaptation defaults, exactly as the RPG does without environmental
    /// data.
    env_heights_km_msl: Option<(f64, f64)>,
    sweeps: Vec<SweepData>,
}

/// One sweep's worth of the product's moment.
#[derive(Debug, Clone, PartialEq)]
struct SweepData {
    /// The sweep's **median** elevation
    /// ([`crate::volumetric::sweep_elevation_deg`]) — not its first radial's,
    /// and not a value that may be read off any single radial.
    ///
    /// The model carries elevation per radial, and it is *not* constant across a
    /// sweep: the antenna is still settling when one opens, and the opening
    /// radial can sit a third of a degree from the tilt the sweep actually flew.
    ///
    /// Two things then depend on this being the median, and both fail silently
    /// if it reverts to the first radial:
    ///
    /// * [`RenderInput::to_scan`] stamps this one value onto *every*
    ///   reconstructed radial, so it **is** the reconstructed sweep's median.
    ///   [`crate::render::find_sweep`] matches on the median within
    ///   [`crate::render::ELEVATION_WINDOW`], so a first-radial value here puts
    ///   the payload further from the request than the window allows: the worker
    ///   fails to find the one sweep its own payload carries and the whole wasm
    ///   render path draws nothing.
    /// * Every whole-volume product — echo tops, VIL, the hail pair, the hybrid
    ///   classification, NROT, SRV — builds its tilt ladder by asking
    ///   `sweep_elevation_deg` for each sweep's elevation. On the desktop path
    ///   that reads the real radials; on the web path it reads this field
    ///   copied across them. Anything but the median makes those two paths
    ///   compute *different ladders from the same volume*.
    ///
    /// `render_input::tests::a_sweep_that_opened_off_its_tilt_still_renders_after_the_port`
    /// is the guard; fixtures giving a sweep one constant elevation cannot see
    /// any of this, because for them the median and the first radial are the
    /// same number.
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
    /// The radial's *other* moments, tagged by their index into `ALL_SLOTS` —
    /// carried only for the hybrid hydrometeor classification, whose
    /// derivation reads every dual-pol field plus velocity, and empty for
    /// every other product.
    extras: Vec<(u8, MomentPayload)>,
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

/// Whether a product reads the environmental 0 °C / −20 °C heights.
///
/// The hail pair has no field at all without them ([`crate::hail`]); the
/// hybrid hydrometeor classification uses them for its melting layer and
/// hail-size heights, falling back to the operational adaptation defaults
/// when they are absent. Every other product must never carry them, so its
/// payload bytes cannot depend on an unrelated cache.
fn reads_env_heights(product: RadarProduct) -> bool {
    matches!(
        product,
        RadarProduct::ProbabilityOfSevereHail
            | RadarProduct::MaxExpectedHailSize
            | RadarProduct::HydrometeorClassification
    )
}

impl RenderInput {
    /// The reachable subset of `scan` for this request, or `None` when the
    /// request cannot be rendered at all.
    ///
    /// `None` is returned exactly where [`crate::render`] would have returned
    /// it: a product with no Level II moment behind it, or no sweep in the
    /// requested tilt family carrying one.
    #[allow(clippy::too_many_arguments)]
    pub fn extract(
        scan: &Scan,
        elevation: f32,
        product: RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
        storm_motion_override: Option<(f32, f32)>,
        env_heights_km_msl: Option<(f64, f64)>,
    ) -> Option<Self> {
        let slot = product.moment_slot()?;
        // `None` for a Level III product: no Level II moment stands behind it,
        // so there is nothing to extract and nothing the renderer would draw.
        //
        // Some products then need every tilt carrying that moment; anything
        // else needs one sweep. Which is which is
        // [`RadarProduct::reads_whole_volume`], *read* rather than restated:
        // the live chunk feed narrows its download by the same predicate, and
        // a second copy of it here is how an SRV pane came to be handed a
        // volume the feed had skipped cuts of.
        let whole_volume = product.reads_whole_volume();

        // Only the HHC reads moments beyond its slot; everything else ships
        // the slot moment alone.
        let all_moments = product == RadarProduct::HydrometeorClassification;
        let sweeps = if whole_volume {
            collect_sweeps(scan, slot, all_moments)
        } else {
            // One sweep: whichever `find_sweep` would have chosen. Selecting
            // here, against the whole volume, is the point — the reconstructed
            // scan has only this sweep to offer, so `find_sweep` reaches it
            // again whatever its preference rules do.
            vec![sweep_data(
                crate::render::find_sweep(scan, product, elevation)?,
                slot,
                false,
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
            storm_motion_override,
            env_heights_km_msl: if reads_env_heights(product) {
                env_heights_km_msl
            } else {
                // Nothing else reads them; carrying them anyway would make
                // byte-identity of other products' payloads depend on an
                // unrelated cache.
                None
            },
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

    /// The user's storm motion vector, knots and degrees-from, or `None`
    /// for "no override" — Bunkers applies.
    pub fn storm_motion_override(&self) -> Option<(f32, f32)> {
        self.storm_motion_override
    }

    /// The site's environmental 0 °C / −20 °C heights, km MSL, or `None` —
    /// the hail products then render nothing, and the HHC applies its
    /// adaptation defaults.
    pub fn env_heights_km_msl(&self) -> Option<(f64, f64)> {
        self.env_heights_km_msl
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
                        let mut slots = place_moment(slot, moment);
                        // The extras go back on the fields their tags name —
                        // the HHC's full-radial reconstruction.
                        for (code, payload) in &radial.extras {
                            if let Some(extra_slot) = ALL_SLOTS.get(*code as usize) {
                                place_into(&mut slots, *extra_slot, payload.to_moment_data());
                            }
                        }
                        let (reflectivity, velocity, spectrum_width, zdr, phi, rho) = slots;
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
/// With `all_moments` (the HHC), a sweep carrying *any* moment qualifies —
/// the split-cut Doppler halves carry no differential phase but donate the
/// velocity the classification grafts in.
fn collect_sweeps(scan: &Scan, slot: MomentSlot, all_moments: bool) -> Vec<SweepData> {
    scan.sweeps()
        .iter()
        .filter_map(|sweep| {
            let radials = sweep.radials();
            let first = radials.first()?;
            let wanted = if all_moments {
                ALL_SLOTS.iter().any(|s| s.read(first).is_some())
            } else {
                slot.read(first).is_some()
            };
            wanted.then(|| sweep_data(radials, slot, all_moments))
        })
        .collect()
}

/// Every moment field a radial has, in `Radial::new` order — the extras'
/// tag bytes are indices into this table.
const ALL_SLOTS: [MomentSlot; 6] = [
    MomentSlot::Reflectivity,
    MomentSlot::Velocity,
    MomentSlot::SpectrumWidth,
    MomentSlot::DifferentialReflectivity,
    MomentSlot::DifferentialPhase,
    MomentSlot::CorrelationCoefficient,
];

/// Flatten one sweep, carrying `slot`'s moment and nothing else.
///
/// `slot` comes from the caller rather than being probed off the radial: a
/// merged upper tilt carries reflectivity *and* velocity, so "the first moment
/// this radial has" would hand a reflectivity render the velocity gates.
fn sweep_data(radials: &[Radial], slot: MomentSlot, all_moments: bool) -> SweepData {
    SweepData {
        // The sweep's **median**, and it has to be: `to_scan` stamps this one
        // value onto every reconstructed radial, so it is the median of the
        // reconstructed sweep as well, and `find_sweep` — which matches on the
        // median — reaches the same sweep on both sides of the port. Carrying
        // the first radial's angle here instead would have left the payload
        // describing a tilt the sweep never flew, and, since the first radial
        // can sit a third of a degree off, `find_sweep` would have failed to
        // find the one sweep the payload contains and the worker path would
        // have rendered nothing at all.
        elevation_angle: crate::volumetric::sweep_elevation_deg(radials)
            .map(|e| e as f32)
            .unwrap_or(0.0),
        radials: radials
            .iter()
            .map(|radial| RadialData {
                azimuth: radial.azimuth_angle_degrees(),
                azimuth_spacing: radial.azimuth_spacing_degrees(),
                moment: slot.read(radial).map(MomentPayload::from_moment_data),
                extras: if all_moments {
                    ALL_SLOTS
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| **s != slot)
                        .filter_map(|(code, s)| {
                            s.read(radial)
                                .map(|m| (code as u8, MomentPayload::from_moment_data(m)))
                        })
                        .collect()
                } else {
                    Vec::new()
                },
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
    let Some(moment) = moment else { return slots };
    place_into(&mut slots, slot, moment);
    slots
}

/// Set one field of the six-slot tuple.
fn place_into(slots: &mut MomentSlots, slot: MomentSlot, moment: MomentData) {
    match slot {
        MomentSlot::Reflectivity => slots.0 = Some(moment),
        MomentSlot::Velocity => slots.1 = Some(moment),
        MomentSlot::SpectrumWidth => slots.2 = Some(moment),
        MomentSlot::DifferentialReflectivity => slots.3 = Some(moment),
        MomentSlot::DifferentialPhase => slots.4 = Some(moment),
        MomentSlot::CorrelationCoefficient => slots.5 = Some(moment),
    }
}

/// Nothing on a render path reads the coverage pattern, but `Scan::new`
/// requires one. Pattern number 0 is not a real VCP, which is the point: a
/// consumer that starts reading this sees an obviously synthetic value rather
/// than a plausible wrong one.
///
/// `pub(crate)` so [`crate::render`]'s own tests can build a `Scan` without a
/// second synthetic pattern that could drift from this one.
pub(crate) fn placeholder_coverage_pattern() -> VolumeCoveragePattern {
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
///
/// Version 2 added the storm motion override between the wind levels and the
/// sweep count, when storm-relative velocity became a Level II product.
/// Version 3 removed the wind levels: the dealias-seeding profile is fit from
/// the payload's own velocity tilts, and the NVW fetch that used to supply
/// external levels is gone.
/// Version 4 added the environmental heights between the override and the
/// sweep count, for the hail products.
/// Version 5 added the per-radial extra moments, when the hybrid hydrometeor
/// classification became a Level II product: it composites every dual-pol
/// moment of every tilt, so its payload carries them alongside the sweep's
/// own moment, and it reads the same environmental heights the hail pair
/// does.
const FORMAT_VERSION: u16 = 5;

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

        match self.storm_motion_override {
            None => out.push(0),
            Some((speed_kt, direction_deg)) => {
                out.push(1);
                out.extend_from_slice(&speed_kt.to_le_bytes());
                out.extend_from_slice(&direction_deg.to_le_bytes());
            }
        }

        match self.env_heights_km_msl {
            None => out.push(0),
            Some((h0c, hm20c)) => {
                out.push(1);
                out.extend_from_slice(&h0c.to_le_bytes());
                out.extend_from_slice(&hm20c.to_le_bytes());
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
                        encode_moment(&mut out, moment);
                    }
                }
                out.push(radial.extras.len() as u8);
                for (code, payload) in &radial.extras {
                    out.push(*code);
                    encode_moment(&mut out, payload);
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

        let storm_motion_override = match r.u8()? {
            0 => None,
            1 => Some((r.f32()?, r.f32()?)),
            _ => return None,
        };

        let env_heights_km_msl = match r.u8()? {
            0 => None,
            1 => Some((r.f64()?, r.f64()?)),
            _ => return None,
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
                    1 => Some(decode_moment(&mut r)?),
                    _ => return None,
                };
                let extra_count = r.u8()?;
                let mut extras = Vec::with_capacity(r.bounded(extra_count as u32, 16)?);
                for _ in 0..extra_count {
                    let code = r.u8()?;
                    // A tag outside the slot table means the two ends
                    // disagree about the layout; refuse the frame.
                    if code as usize >= ALL_SLOTS.len() {
                        return None;
                    }
                    extras.push((code, decode_moment(&mut r)?));
                }
                radials.push(RadialData {
                    azimuth,
                    azimuth_spacing,
                    moment,
                    extras,
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
            storm_motion_override,
            env_heights_km_msl,
            sweeps,
        })
    }

    fn encoded_len(&self) -> usize {
        let header = 4 + 2 + 2 + 4 + 8 + 8;
        let motion = 1 + if self.storm_motion_override.is_some() {
            8
        } else {
            0
        };
        let env = 1 + if self.env_heights_km_msl.is_some() {
            16
        } else {
            0
        };
        let sweeps: usize = self
            .sweeps
            .iter()
            .map(|s| {
                8 + s
                    .radials
                    .iter()
                    .map(|r| {
                        10 + r.moment.as_ref().map_or(0, |m| 19 + m.gates.len())
                            + r.extras
                                .iter()
                                .map(|(_, m)| 20 + m.gates.len())
                                .sum::<usize>()
                    })
                    .sum::<usize>()
            })
            .sum();
        header + motion + env + 4 + sweeps
    }
}

/// One moment payload's wire form, shared by the slot moment and the extras.
fn encode_moment(out: &mut Vec<u8>, moment: &MomentPayload) {
    out.extend_from_slice(&moment.gate_count.to_le_bytes());
    out.extend_from_slice(&moment.first_gate_range_m.to_le_bytes());
    out.extend_from_slice(&moment.gate_interval_m.to_le_bytes());
    out.push(moment.word_size);
    out.extend_from_slice(&moment.scale.to_le_bytes());
    out.extend_from_slice(&moment.offset.to_le_bytes());
    out.extend_from_slice(&(moment.gates.len() as u32).to_le_bytes());
    out.extend_from_slice(&moment.gates);
}

fn decode_moment(r: &mut Reader) -> Option<MomentPayload> {
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
    use crate::render::{render_from, render_radar_to_image_full};

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

    /// One tilt at `elevation` carrying every moment a radial can hold.
    ///
    /// [`volume`] is shaped like a real SAILS volume and so carries only
    /// reflectivity and velocity, which is all the products behind those two
    /// moments need. `extract` refuses a product whose moment no sweep carries,
    /// so a claim made about *every* product needs a volume where every field
    /// is present — the gate values do not matter, only that they are there.
    fn every_moment_tilt(elevation: f32, number: u8) -> Sweep {
        let radials = (0..RADIALS)
            .map(|i| {
                let other = || Some(moment(1.0, 0.0, shear(i), 400));
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / RADIALS as f32),
                    360.0 / RADIALS as f32,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(moment(REFL_SCALE, REFL_OFFSET, strong_refl(i), 600)),
                    Some(moment(VEL_SCALE, VEL_OFFSET, shear(i), 400)),
                    other(),
                    other(),
                    other(),
                    other(),
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
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

    /// The storm motion override a storm-relative render carries, for the
    /// products whose parity is asserted below. `None` for everything else:
    /// only SRV reads it, and without one SRV would need the fixture volume
    /// to support a Bunkers fit, which its two shallow tilts cannot.
    fn override_for(product: RadarProduct) -> Option<(f32, f32)> {
        (product == RadarProduct::StormRelativeVelocity).then_some((30.0, 240.0))
    }

    /// The environmental heights a hail render carries — only the hail pair
    /// reads them, and without a pair those products render nothing at all.
    /// 2 / 4 km MSL sits the fixture's strong low tilt across the ramp.
    fn env_for(product: RadarProduct) -> Option<(f64, f64)> {
        reads_env_heights(product).then_some((2.0, 4.0))
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
            RadarProduct::StormRelativeVelocity,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ] {
            let over = override_for(product);
            let env = env_for(product);
            let direct =
                crate::render::render_radar_to_image_full(&scan, 0.5, product, LAT, LON, over, env)
                    .unwrap();
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, over, env).unwrap();
            let viaformat = render_from(&input).unwrap();

            assert!(
                painted(&direct) > 1_000,
                "{product:?} painted only {} pixels — the comparison would be vacuous",
                painted(&direct)
            );
            assert_same_frame(&direct, &viaformat, &format!("{product:?}"));
        }
    }

    /// A sweep that opens off its own tilt while the antenna settles: the first
    /// thirty radials ramp from `first` to `flown`, the rest sit on `flown`, so
    /// the median is `flown` and the first radial is not.
    ///
    /// [`volume`] gives every sweep one constant elevation, which makes the two
    /// readings the same number — so it cannot see the hazard the next test is
    /// about, and neither could any fixture in this module before it.
    fn settling_sweep(number: u8, first: f32, flown: f32) -> Sweep {
        const SETTLING: usize = 30;
        let radials = (0..RADIALS)
            .map(|i| {
                let elevation = if i < SETTLING {
                    first + (flown - first) * (i as f32 / SETTLING as f32)
                } else {
                    flown
                };
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / RADIALS as f32),
                    360.0 / RADIALS as f32,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(moment(REFL_SCALE, REFL_OFFSET, strong_refl(i), 600)),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    }

    /// The payload has to survive the port for a sweep that opened off its
    /// tilt, and this is the tightest constraint on what `SweepData` may carry.
    ///
    /// `to_scan` stamps one elevation onto every reconstructed radial, so
    /// whatever `sweep_data` stored *is* the reconstructed sweep's median.
    /// `find_sweep` matches on the median within a tenth of a degree, so
    /// storing the first radial's angle — 0.3° from the tilt the request names —
    /// would leave the worker unable to find the one sweep its own payload
    /// contains, and the web path would render nothing at all. Constant-elevation
    /// fixtures cannot fail this; this one can.
    #[test]
    fn a_sweep_that_opened_off_its_tilt_still_renders_after_the_port() {
        let scan = Scan::new(
            placeholder_coverage_pattern(),
            vec![settling_sweep(1, 0.68, 0.44)],
        );
        let product = RadarProduct::Reflectivity;

        let direct =
            crate::render::render_radar_to_image_full(&scan, 0.4, product, LAT, LON, None, None)
                .expect("the scan path draws the cut this volume flew");
        let input = RenderInput::extract(&scan, 0.4, product, LAT, LON, None, None)
            .expect("the payload extracts that same cut");
        assert!(
            (input.sweeps[0].elevation_angle - 0.44).abs() < 1e-4,
            "the payload must carry the tilt the sweep flew, not the one it opened on — got {}",
            input.sweeps[0].elevation_angle,
        );
        let reconstructed = input.to_scan();
        assert!(
            crate::render::find_sweep(&reconstructed, product, 0.4).is_some(),
            "the worker must find the one sweep its payload carries",
        );
        let via = render_from(&input).expect("the payload renders");
        assert!(
            painted(&direct) > 1_000,
            "the comparison would be vacuous — only {} pixels painted",
            painted(&direct),
        );
        assert_same_frame(&direct, &via, "a sweep that opened off its tilt");
    }

    /// The same, across the wire format the worker actually receives.
    #[test]
    fn a_payload_renders_the_same_frame_after_a_round_trip() {
        let scan = volume();
        for product in [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::NormalizedRotation,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ] {
            let input = RenderInput::extract(
                &scan,
                0.5,
                product,
                LAT,
                LON,
                override_for(product),
                env_for(product),
            )
            .unwrap();
            let decoded = RenderInput::from_bytes(&input.to_bytes())
                .unwrap_or_else(|| panic!("{product:?} payload did not decode"));
            assert_eq!(input, decoded, "{product:?} payload changed in transit");
            assert_eq!(
                decoded.storm_motion_override(),
                override_for(product),
                "{product:?}: the override must survive the wire",
            );
            assert_eq!(
                decoded.env_heights_km_msl(),
                env_for(product),
                "{product:?}: the environment must survive the wire",
            );
            assert_same_frame(
                &render_from(&input).unwrap(),
                &render_from(&decoded).unwrap(),
                &format!("{product:?} round trip"),
            );
        }
    }

    /// Storm-relative velocity is a Level II product now: it extracts, it
    /// carries every velocity tilt (the profile is both its dealias seed and
    /// its Bunkers input), and the override moves the field.
    #[test]
    fn srv_extracts_the_velocity_volume_and_honours_the_override() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::StormRelativeVelocity,
            LAT,
            LON,
            Some((30.0, 240.0)),
            None,
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 2, "both velocity tilts travel");
        assert_eq!(input.storm_motion_override(), Some((30.0, 240.0)));

        // A different vector must change pixels: the override reaches the
        // arithmetic, not just the payload.
        let other = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::StormRelativeVelocity,
            LAT,
            LON,
            Some((30.0, 60.0)),
            None,
        )
        .unwrap();
        let a = render_from(&input).unwrap();
        let b = render_from(&other).unwrap();
        assert!(painted(&a) > 1_000);
        assert_ne!(a.0, b.0, "the vector was carried but never applied");
    }

    /// `to_bytes` reserves exactly what it writes. Wrong by a little is only a
    /// realloc; wrong by a lot means the layout and the estimate have drifted.
    #[test]
    fn the_encoded_length_estimate_is_exact() {
        let scan = volume();
        for product in [RadarProduct::Reflectivity, RadarProduct::NormalizedRotation] {
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None).unwrap();
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
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap();
        let moment = input.sweeps[0].radials[0].moment.as_ref().unwrap();
        assert_eq!(moment.scale, REFL_SCALE);
        assert_eq!(moment.offset, REFL_OFFSET);
        assert_eq!(
            moment.gates[0],
            strong_refl(0),
            "carried the velocity gates under the reflectivity request"
        );
    }

    /// What travels is what [`RadarProduct::reads_whole_volume`] says travels,
    /// for every product there is.
    ///
    /// That predicate is also what the live chunk feed narrows a site's download
    /// by, and the two used to be separate hand-maintained matches: the feed's
    /// copy omitted storm-relative velocity, so a live SRV pane fit its dealias
    /// seed and its default Bunkers vector from a volume the feed had
    /// deliberately skipped cuts of — no error, no NaN, and archived volumes are
    /// whole, so nothing under test saw it.
    ///
    /// This asserts the half that lives here: that `extract` *reads* the
    /// predicate for every product rather than deciding again, so a second copy
    /// cannot grow back inside it. Whether the predicate's own answer is right
    /// is a claim about the algorithms, and each whole-volume product's
    /// individual test below is what pins that — every one of them fails if its
    /// product is downgraded to a single sweep.
    #[test]
    fn every_product_carries_the_volume_exactly_when_it_says_it_reads_one() {
        let scan = Scan::new(
            placeholder_coverage_pattern(),
            vec![
                every_moment_tilt(0.5, 1),
                every_moment_tilt(1.5, 2),
                every_moment_tilt(2.5, 3),
            ],
        );
        let tilts = scan.sweeps().len();
        assert!(
            tilts > 1,
            "precondition: with one tilt in the volume, carrying the volume and \
             carrying one sweep are the same payload and this says nothing"
        );

        for &product in RadarProduct::all() {
            let Some(input) = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None)
            else {
                assert!(
                    product.is_level3(),
                    "{product:?} extracted nothing from a volume carrying every \
                     moment on every tilt"
                );
                continue;
            };
            let expected = if product.reads_whole_volume() {
                tilts
            } else {
                1
            };
            assert_eq!(
                input.sweeps.len(),
                expected,
                "{product:?}: reads_whole_volume() is {}, so {expected} of the \
                 volume's {tilts} tilts should have travelled",
                product.reads_whole_volume(),
            );
        }
    }

    /// The sizing decision the whole design rests on: a normal product ships
    /// one sweep, not the volume.
    #[test]
    fn a_plain_product_carries_one_sweep() {
        let scan = volume();
        let input =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap();
        assert_eq!(input.sweeps.len(), 1);
        assert_eq!(input.sweeps[0].radials.len(), RADIALS);
    }

    /// NROT fits its wind profile from every velocity tilt — that fit is the
    /// only wind source since the NVW fetch left — so every velocity tilt has
    /// to travel with the payload.
    #[test]
    fn nrot_carries_every_velocity_tilt() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::NormalizedRotation,
            LAT,
            LON,
            None,
            None,
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 2, "both velocity tilts travel");
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
            RenderInput::extract(&scan, 0.5, RadarProduct::EchoTops, LAT, LON, None, None)
                .is_none()
        );
        assert!(
            render_radar_to_image_full(&scan, 0.5, RadarProduct::EchoTops, LAT, LON, None, None)
                .is_none(),
            "the payload and the renderer must refuse the same requests"
        );
    }

    /// The hail pair without an environment: the payload still extracts —
    /// the sweeps and the request are valid — but both render paths answer
    /// nothing, the explicit "undefined field" seam (`crate::hail`), never a
    /// zero-filled grid. The same payload with an environment paints.
    #[test]
    fn hail_without_an_environment_renders_nothing_on_both_paths() {
        let scan = volume();
        for product in [
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ] {
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None).unwrap();
            assert_eq!(input.env_heights_km_msl(), None);
            assert!(
                render_from(&input).is_none(),
                "{product:?} rendered without an environment"
            );
            assert!(
                crate::render::render_radar_to_image_full(
                    &scan, 0.5, product, LAT, LON, None, None,
                )
                .is_none(),
                "{product:?}: the payload and the renderer must refuse alike"
            );

            let with = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, Some((2.0, 4.0)))
                .unwrap();
            let frame = render_from(&with).unwrap();
            assert!(
                painted(&frame) > 1_000,
                "{product:?} with an environment must paint"
            );
        }
    }

    /// The hybrid classification's payload carries every sweep with every
    /// moment (the extras), plus the environmental heights — and the whole
    /// bundle survives the byte round trip. The fixture volume carries only
    /// reflectivity and velocity, which is not enough to classify, so the
    /// pin here is structural: the extras and heights are the parts version
    /// 5 added.
    #[test]
    fn hhc_payloads_carry_extras_and_env_heights() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::HydrometeorClassification,
            LAT,
            LON,
            None,
            Some((5.0, 8.6)),
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 3, "every sweep travels");
        assert_eq!(input.env_heights_km_msl(), Some((5.0, 8.6)));
        // The slot moment is reflectivity; velocity rides in the extras.
        let with_velocity = input.sweeps[1]
            .radials
            .iter()
            .filter(|r| r.extras.iter().any(|(code, _)| *code == 1))
            .count();
        assert!(with_velocity > 0, "the Doppler moment travels as an extra");
        let back = RenderInput::from_bytes(&input.to_bytes()).expect("round trips");
        assert_eq!(back, input);
        // And the reconstruction puts the extras back on their fields.
        let rebuilt = back.to_scan();
        let radial = &rebuilt.sweeps()[1].radials()[0];
        assert!(radial.reflectivity().is_some(), "slot moment placed");
        assert!(radial.velocity().is_some(), "extra placed on its field");
        // A non-HHC product never carries either, whatever the caller
        // passed — other products' payload bytes must not depend on an
        // unrelated cache.
        let refl = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::Reflectivity,
            LAT,
            LON,
            None,
            Some((5.0, 8.6)),
        )
        .unwrap();
        assert_eq!(refl.env_heights_km_msl(), None);
        assert!(refl.sweeps[0].radials.iter().all(|r| r.extras.is_empty()));
    }

    /// The bytes arrive off a message port. Every malformed shape has to be a
    /// clean `None` — the two ends of that port can be different builds.
    #[test]
    fn a_malformed_payload_is_refused_rather_than_misread() {
        let scan = volume();
        let good =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
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
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap()
                .to_bytes();
        // The sweep count sits directly after the header and the
        // absent-override and absent-environment flag bytes.
        let at = 4 + 2 + 2 + 4 + 8 + 8 + 1 + 1;
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
            RadarProduct::VilDensity,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
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
