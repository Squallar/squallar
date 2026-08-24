//! Storm-relative mean velocity, derived from Level III **dealiased** velocity.
//!
//! Every tilt is computed here, 0.5° included:
//!
//! ```text
//! SRM_kt = V_kt + speed · cos(direction − azimuth)
//! ```
//!
//! Only the lowest SRM *product* is still published. NWS SCN 22-96 dropped
//! `N1S`/`N2S`/`N3S` from the NOAAPort broadcast in 2022, and every CORS-clean
//! source is NOAAPort-derived: `unidata-nexrad-level3` last wrote to those three
//! keys in 2020, while `N0S` runs 294 objects a day. THREDDS, GCS, IEM, COD and
//! NCEI were all checked. `N0S` is still fetched — see
//! [`STORM_MOTION_PRODUCT`] — but for its vector alone; it is no longer drawn.
//!
//! **Deriving 0.5° rather than rendering `N0S`** is what makes the four panes
//! one thing rather than two. `N0S` is 1 km at the RPG's 16 display levels
//! while the derived tilts are 0.25 km at 254, and its gate values already have
//! the RPG's own vector baked in, so it was the one tilt a storm motion
//! override could not reach. `N0G` is the same product 154 as `N1G` at the same
//! 0.5° cut — verified at `TLX`.
//!
//! **From Level III, never Level II.** L2 velocity is aliased and
//! `nexrad-decode` has no dealiasing; the errors would be 2×Nyquist — 50–70 kt
//! in exactly the mesocyclone cores the product exists to show. The RPG
//! dealiases before publishing `N?G`/`N?U`.
//!
//! **The vector is read, not estimated.** It is in the `N0S` Product
//! Description Block, halfwords 51 and 52; see
//! [`nexrad_level3::model::ProductDescriptionBlock::storm_motion`]. No velocity
//! product can supply it — halfword 51 is the BZ2 compression flag on every
//! digital product, and `N0G` carries a 1 there, which reads as "0.1 kt".
//!
//! **Native resolution is kept.** The RPG resamples to 1 km × 1° and 16 levels;
//! the source products are 0.25 km with 254 levels.
//! [`quantize_to_rpg_levels`] exists only so the validation test can compare
//! like with like.
//!
//! # Accuracy
//!
//! Measured off-tree against the RPG's own `N?S`, exact / within-one-level:
//! 0.5° 76.1-91.0% / 99.10-99.80%, 1.3° 77.3-90.1% / 98.94-99.87%, 2.4°
//! 82.2-98.8% / 99.90-99.99%, 3.1° 80.7-98.6% / 99.91-100.00%. Treat
//! exact-match as indicative and within-one-level as the criterion. A
//! forty-volume survey at 0.5° puts the real spread at 93.7-99.9% and
//! quarantines seven of eighteen sites.
//!
//! **Almost all of the residual is the comparison's resampler, not the
//! derivation.** `N2U`/`N3U` are already 1°, so only the range step runs on
//! them and they agree to 99.9%+; `N0G`/`N1G` are half-degree and need the
//! azimuth step too. On sites reporting a 0.0 kt vector — where the correction
//! is multiplied by zero — the disagreement keeps the same shape. That says
//! the resampler accounts for the residual, not that the correction is free.
//!
//! # Volume pairing
//!
//! All four tilts share one vector and the RPG re-fits every volume, but only
//! `N0S` carries one, and it is published when the 0.5° cut finishes while the
//! upper tilts publish when their own cuts do. Measured over 792 renders at 22
//! sites, the newest vector belongs to another volume on 11.6% of 0.5°
//! renders, 38.4% at 1.3°, 50.5% at 2.4° and 54.0% at 3.1°.
//!
//! **What it costs is bimodal, and the tail is what matters.** The median
//! mismatch costs nothing — 93.6% leave every gate inside one data level — but
//! the worst observed pair cost **82 points** of within-one-level agreement at
//! `KFSD` on a 47 kt re-fit, on gates that agree to 99.9% once the right vector
//! is applied. So production keeps the last few volumes' vectors per site and
//! applies the one belonging to the velocity product being rendered
//! (`render_dispatch::RenderDispatcher::storm_motion_for`), falling back to the
//! newest only when no vector for that volume was ever seen.

use nexrad_level3::model::{DataPacket, Level3Message, RadialPacket, RadialRun, StormMotion};

/// Knots per metre per second.
const MS_TO_KT: f64 = 1.0 / 0.514_444;

/// Product codes carrying dealiased velocity that an SRM tilt can be derived
/// from: 154 super-resolution (`N?G`, 0.5° radials) and 99 (`N?U`, 1°). Both
/// encode 0.25 km gates and 254 levels of 0.5 m/s.
pub const VELOCITY_PRODUCT_CODES: [i16; 2] = [154, 99];

/// The AWIPS ID fetched **for its storm motion vector alone**, never rendered.
pub const STORM_MOTION_PRODUCT: &str = "N0S";

/// The AWIPS IDs the four SRM tilts are **derived** from, lowest first: `N0G`
/// and `N1G` super-resolution (product 154, 0.5° radials), `N2U`/`N3U` at 1°
/// (product 99). All four are 0.25 km gates over 254 levels.
///
/// **These are request keys, not elevations.** `N1G` is *not* 1.5°: in VCP 212
/// it is 1.3°, and the angle always comes from the fetched product's own
/// Product Description Block.
pub const SRM_TILT_PRODUCTS: [&str; 4] = ["N0G", "N1G", "N2U", "N3U"];

/// Everything squallar fetches for storm-relative velocity: the vector source
/// followed by the four tilts it is applied to.
pub const SRM_FETCH_PRODUCTS: [&str; 5] = ["N0S", "N0G", "N1G", "N2U", "N3U"];

/// Physical value per gate step in the derived packet, in knots. Finer than the
/// 0.5 m/s (0.97 kt) the source products carry, so the requantisation adds no
/// error of its own.
const DERIVED_SCALE: f32 = 2.0;

/// Gate value standing for 0 kt, so the representable range is
/// `(2 - offset)/scale` upward: **-499 kt to +32,267 kt**.
const DERIVED_OFFSET: f32 = 1000.0;

/// Largest storm motion the settings dialog admits, in knots. Lives here
/// because [`DERIVED_OFFSET`] is sized from it; the widget reads it. Well past
/// anything meteorological — the fastest observed storm motions are ~70 kt.
pub const MAX_OVERRIDE_SPEED_KT: f32 = 200.0;

/// Gate values 0 and 1 are "below threshold" and "range folded" in every
/// product involved, and the renderer skips both.
const NO_DATA: u16 = 0;
const FIRST_DATA_GATE: u16 = 2;

/// A storm-relative velocity field computed from a dealiased velocity product.
#[derive(Debug, Clone)]
pub struct DerivedSrm {
    /// Gate values are storm-relative knots through
    /// [`scale`](Self::scale)/[`offset`](Self::offset), in the same geometry as
    /// the source product.
    pub packet: RadialPacket,
    /// `knots = (gate - offset) / scale`.
    pub scale: f32,
    /// See [`scale`](Self::scale).
    pub offset: f32,
    /// From the source product's PDB, never from its AWIPS mnemonic.
    pub elevation_angle: f32,
    /// From the source product's PDB. Identifies the cut within the volume;
    /// split cuts and SAILS/MRLE repeats share an angle but not a number.
    pub elevation_number: u16,
    /// The vector applied.
    pub motion: StormMotion,
    /// Which volume the vector belongs to, relative to this velocity product.
    pub motion_provenance: MotionProvenance,
}

/// Where the vector a derived field used stands relative to the velocity
/// product it was applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionProvenance {
    /// The RPG's vector, fitted for this very volume scan.
    SameVolume,
    /// The RPG's vector, but fitted for an earlier volume. Usually costs
    /// nothing within one data level — adjacent fits typically agree to about
    /// 1.4 kt — but the distribution is bimodal and the tail has been measured
    /// at 82 points, so it is not a figure to average.
    PreviousVolume,
    /// A vector the user typed in. It belongs to no volume, so the velocity
    /// product's volume says nothing about it either way.
    UserOverride,
}

impl DerivedSrm {
    /// Whether the vector was fitted for this very volume — the accuracy
    /// signal, and `false` for an override, which has no volume to agree with.
    pub fn motion_volume_matches(&self) -> bool {
        self.motion_provenance == MotionProvenance::SameVolume
    }
}

/// Where a storm motion vector came from, and which volume it describes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StormMotionSample {
    pub motion: StormMotion,
    /// [`ProductDescriptionBlock::volume_key`] of the `N0S` it was read from,
    /// or `None` for a vector the user typed in.
    ///
    /// [`ProductDescriptionBlock::volume_key`]: nexrad_level3::model::ProductDescriptionBlock::volume_key
    pub volume: Option<(u16, u32)>,
}

impl StormMotionSample {
    /// The vector an `N0S` product carries, or `None` for anything else.
    pub fn from_message(msg: &Level3Message) -> Option<Self> {
        Some(Self {
            motion: msg.pdb.storm_motion()?,
            volume: Some(msg.pdb.volume_key()),
        })
    }

    /// A vector the user typed in, which never claims the RPG's provenance and
    /// never claims to be *stale* either.
    pub fn user_override(speed_kt: f32, direction_deg: f32) -> Option<Self> {
        if !speed_kt.is_finite() || !direction_deg.is_finite() {
            return None;
        }
        Some(Self {
            motion: StormMotion {
                speed_kt,
                direction_deg,
                is_scit_average: false,
            },
            volume: None,
        })
    }
}

/// Whether a validation run's nonzero-vector sample is worth drawing a
/// conclusion from.
pub fn sample_is_conclusive(sites_asserted: usize, nonzero_gates: usize) -> bool {
    sites_asserted > 0 && nonzero_gates > MIN_NONZERO_GATES
}

/// Floor for [`sample_is_conclusive`]. Roughly one tilt's worth of echo.
pub const MIN_NONZERO_GATES: usize = 10_000;

/// The first digital radial packet in a message's symbology.
pub fn radial_packet(msg: &Level3Message) -> Option<&RadialPacket> {
    msg.symbology.as_ref()?.layers.iter().find_map(|layer| {
        layer.packets.iter().find_map(|pkt| match pkt {
            DataPacket::DigitalRadial(rp) => Some(rp),
            _ => None,
        })
    })
}

/// Whether `msg` is a dealiased velocity product an SRM tilt can be built from.
pub fn is_velocity_source(msg: &Level3Message) -> bool {
    VELOCITY_PRODUCT_CODES.contains(&msg.pdb.product_code)
}

/// Compute storm-relative velocity from a dealiased velocity product.
///
/// Returns `None` for anything that is not one of [`VELOCITY_PRODUCT_CODES`],
/// or that carries no radial data. An `N0S` is refused: it is already
/// storm-relative, so the correction would be applied twice.
pub fn derive(velocity: &Level3Message, sample: &StormMotionSample) -> Option<DerivedSrm> {
    if !is_velocity_source(velocity) {
        return None;
    }
    let source = radial_packet(velocity)?;
    if source.radials.is_empty() {
        return None;
    }

    let pdb = &velocity.pdb;
    let scale = pdb.data_scale();
    let offset = pdb.data_offset();
    let motion = sample.motion;

    let radials = source
        .radials
        .iter()
        .map(|run| {
            // The packet records the leading edge of the radial; the correction
            // belongs at its centre, which is also where the renderer places it.
            let azimuth = run.start_angle as f64 + run.angle_delta as f64 / 2.0;
            let component = motion.radial_component_kt(azimuth);
            let gate_values = run
                .gate_values
                .iter()
                .map(|&gate| {
                    if gate < FIRST_DATA_GATE {
                        return NO_DATA;
                    }
                    let v_kt = (gate as f32 - offset) as f64 / scale as f64 * MS_TO_KT;
                    let derived = (v_kt + component) * DERIVED_SCALE as f64 + DERIVED_OFFSET as f64;
                    derived
                        .round()
                        .clamp(FIRST_DATA_GATE as f64, u16::MAX as f64) as u16
                })
                .collect();
            RadialRun {
                start_angle: run.start_angle,
                angle_delta: run.angle_delta,
                gate_values,
            }
        })
        .collect();

    // The packet's own scale factor halfword reads 999 for the 1 km product 56
    // and the 0.25 km velocity products alike, so it is replaced rather than
    // carried over — see `ProductDescriptionBlock::range_gate_km`.
    let scale_factor = match pdb.range_gate_km() {
        Some(km) if km > 0.0 => (1.0 / km) as f32,
        _ => source.scale_factor,
    };

    // `first_range_bin` is an index *denominated in gates*
    // (`RadialPacket::gate_range_km`), so re-spacing the packet above changes
    // what the very same number means. Re-index onto the new spacing so the
    // first gate stays where the source put it. Every live product declares 0
    // here, so this is inert on the wire today.
    let old_gate_km = source.gate_interval_km();
    let new_gate_km = pdb.range_gate_km().unwrap_or(old_gate_km);
    let first_range_bin = if new_gate_km > 0.0 {
        ((source.first_range_bin as f64 * old_gate_km) / new_gate_km).round() as i16
    } else {
        source.first_range_bin
    };

    Some(DerivedSrm {
        packet: RadialPacket {
            first_range_bin,
            num_range_bins: source.num_range_bins,
            i_center: source.i_center,
            j_center: source.j_center,
            scale_factor,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials,
        },
        scale: DERIVED_SCALE,
        offset: DERIVED_OFFSET,
        elevation_angle: pdb.elevation_angle(),
        elevation_number: pdb.elevation_number,
        motion,
        motion_provenance: match sample.volume {
            None => MotionProvenance::UserOverride,
            Some(volume) if volume == pdb.volume_key() => MotionProvenance::SameVolume,
            Some(_) => MotionProvenance::PreviousVolume,
        },
    })
}

/// The 14 displayable levels of the RPG's legacy velocity products, in knots.
/// Level `i` covers `[RPG_LEVEL_EDGES[i-2], RPG_LEVEL_EDGES[i-1])`; levels 0
/// and 15 are "no data" and "range folded".
///
/// Transcribed from the data level thresholds of a real `N0S` — halfwords
/// 31–46 decode to `-64, -50, -36, -26, -20, -10, -1, 0, 10, 20, 26, 36, 50,
/// 64` — with the `-1`/`0` pair read as the single boundary at zero.
pub const RPG_LEVEL_EDGES: [f32; 13] = [
    -64.0, -50.0, -36.0, -26.0, -20.0, -10.0, 0.0, 10.0, 20.0, 26.0, 36.0, 50.0, 64.0,
];

/// Quantise storm-relative knots to the RPG's 16-level scale.
pub fn quantize_to_rpg_levels(knots: f32) -> u8 {
    for (level, edge) in (1u8..).zip(RPG_LEVEL_EDGES) {
        if knots < edge {
            return level;
        }
    }
    14
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
