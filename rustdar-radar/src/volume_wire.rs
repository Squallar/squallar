//! What a decoded volume looks like on a message port.
//!
//! [`crate::scan::DecodedScan`] is the output of the one expensive thing this
//! application does — 0.9–5.3 billion instructions, 99% of them bzip2 (see
//! [`crate::scan`]) — and until this module existed it was the output of a job
//! that could only run where its consumer already was. On the web that is the
//! browser's one thread, and the decode blocked it for a full second.
//!
//! A job's input has to be nameable and so does its answer. The input is easy:
//! the compressed archive bytes, which are already a `Vec<u8>`. The answer is
//! this.
//!
//! # What travels
//!
//! **Everything the decode produced.** `DecodedScan::from_bytes(&v.to_bytes())
//! == v` on real archive volumes, not on a subset of their fields, and the
//! instrument that measures the payload asserts exactly that before it prints
//! a size.
//!
//! * **Every radial's moments**, byte for byte, in the fixed-point form the
//!   decoder produced — the same [`crate::render_input::MomentPayload`] the
//!   render payload has always carried, through the same
//!   [`crate::render_input::encode_moment`]. One moment codec in the crate, so
//!   a volume and a render cannot come to describe a gate differently. That
//!   includes the **clutter-filter power** moment, the seventh slot on a
//!   `Radial`, which real WSR-88D volumes populate on every radial.
//! * **Every radial's own header**: collection timestamp, azimuth number,
//!   azimuth and its spacing, radial status, elevation number and angle. This
//!   is stricter than what already crosses the port for a render:
//!   [`crate::render_input::RenderInput::to_scan`] stamps one sweep clock
//!   across a sweep's radials, zeroes the azimuth number and writes
//!   `RadialStatus::Unknown(0)`, because a renderer reads none of the three.
//!   A decoded volume is not a render input — `record_tilt_freshness` takes
//!   the *max* collection timestamp over a sweep's radials — so here they are
//!   carried rather than reconstructed.
//! * **The site**, whole, and **the declared Nyquist table**, both halves — see
//!   [`crate::nyquist::DeclaredNyquist::to_bytes`] for why the contradiction
//!   set cannot be recomputed at the far end.
//! * **The coverage pattern**, whole: every field of it and of every
//!   [`ElevationCut`] in it.
//!
//! # The pattern used to be neutralised here, and that was the wrong trade
//!
//! An earlier version of this module carried the pattern number and the cut
//! angles only, on the argument — tested, and true — that those are all this
//! workspace ever reads off a `VolumeCoveragePattern`. It rebuilt the rest
//! through [`crate::render_input`]'s `rebuild_pattern`, which leaves every
//! other field at a neutral default. That function is still there, still doing
//! that, for the render payload that really does need nothing else.
//!
//! What that argument never had was a cost behind it. A real volume states a
//! great deal: KFTG's cut 1 is SZ2 phase coding, a contiguous surveillance
//! waveform, 21.148°/s of azimuth rate, PRF number 1, 15 pulses and 2.0 dB
//! thresholds, with three of the four super-resolution flags set. All of that
//! arrived at the page as `Unknown`, `false` and `0.0` — and **a few kilobytes
//! against a 61 MiB payload** is what it costs to arrive as itself. The
//! neutralisation was saving nothing and losing a volume's own description of
//! how it was flown.
//!
//! The same reasoning settled the clutter-filter power moment, which the first
//! version also dropped: nothing reads it *today*, and a page holding a volume
//! that quietly lost a moment the decoder produced is the shape of defect this
//! project has had to chase before. It costs what a seventh moment costs, and
//! the measurement below says how much.
//!
//! # Why the whole volume comes back rather than staying where it was decoded
//!
//! Because the main thread genuinely holds one. Five structures on the
//! frontend's `App` keep an `Arc<Scan>`, and the frame thread reads it for the
//! product-and-elevation walk, for the per-frame volume stamp and ladder
//! fingerprint, for `find_closest_elevation` on every loop frame, for the
//! merge of an in-flight chunk overlay onto a complete base, and for the loop's
//! hover readout, which decodes a gate straight out of the resident volume.
//! Leaving the volume in the worker would mean moving all of those across the
//! port too — a different and much larger change than the one that stops the
//! browser freezing for a second.
//!
//! So the volume crosses, and what it costs to cross is measured rather than
//! assumed: see [`crate::scan::DecodedScan::to_bytes`].

use crate::nyquist::DeclaredNyquist;
use crate::render_input::{MomentPayload, decode_moment, encode_moment};
use crate::scan::DecodedScan;
use crate::wire::Reader;
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};

/// The first two bytes of every payload: this is a decoded volume, in this
/// build's encoding of one.
///
/// A magic rather than nothing, for the reason
/// [`crate::render_input::RenderInput`]'s tagging exists: the browser's port
/// carries several payload kinds and a page and a worker can be different
/// builds. A section's bytes read as a volume would otherwise decode into some
/// number of plausible sweeps rather than into a refusal.
const MAGIC: u16 = 0x5256;

/// Bumped when the layout below changes. Read before anything else, so a
/// payload from another generation is refused rather than misread.
const VERSION: u8 = 2;

/// `RadialStatus::Unknown`'s tag. The named variants take 0–5, which is the
/// order upstream declares them in; 255 says "a byte followed".
///
/// Two bytes for every radial either way, so the tag costs nothing and a status
/// this build does not have survives the round trip as the `Unknown` it already
/// was rather than collapsing to a named variant it is not.
const STATUS_UNKNOWN: u8 = 255;

fn status_code(status: RadialStatus) -> (u8, u8) {
    match status {
        RadialStatus::ElevationStart => (0, 0),
        RadialStatus::IntermediateRadialData => (1, 0),
        RadialStatus::ElevationEnd => (2, 0),
        RadialStatus::ScanStart => (3, 0),
        RadialStatus::ScanEnd => (4, 0),
        RadialStatus::ElevationStartVCPFinal => (5, 0),
        RadialStatus::Unknown(byte) => (STATUS_UNKNOWN, byte),
    }
}

/// The inverse of [`status_code`], exhaustive over the same arms so a variant
/// added upstream fails this build rather than encoding as a silent `Unknown`.
fn status_from_code(tag: u8, byte: u8) -> Option<RadialStatus> {
    Some(match tag {
        0 => RadialStatus::ElevationStart,
        1 => RadialStatus::IntermediateRadialData,
        2 => RadialStatus::ElevationEnd,
        3 => RadialStatus::ScanStart,
        4 => RadialStatus::ScanEnd,
        5 => RadialStatus::ElevationStartVCPFinal,
        STATUS_UNKNOWN => RadialStatus::Unknown(byte),
        _ => return None,
    })
}

/// The seven moments a radial carries, in `Radial::new`'s own argument order.
///
/// The order is load-bearing and stated once: the presence mask below is bit
/// *i* of this list, and transposing two of them would swap reflectivity for
/// velocity with no type error anywhere to catch it.
///
/// The seventh is the **clutter-filter power** moment, which is a different
/// type over the same block — hence the separate read below rather than a
/// seventh entry in the array.
fn moments_of(radial: &Radial) -> ([Option<MomentPayload>; 6], Option<MomentPayload>) {
    (
        [
            radial.reflectivity(),
            radial.velocity(),
            radial.spectrum_width(),
            radial.differential_reflectivity(),
            radial.differential_phase(),
            radial.correlation_coefficient(),
        ]
        .map(|moment| moment.map(MomentPayload::from_moment_data)),
        radial
            .clutter_filter_power()
            .map(MomentPayload::from_moment_data),
    )
}

/// The coverage pattern, whole.
///
/// Every field `VolumeCoveragePattern` and `ElevationCut` have. This used to
/// carry the pattern number and the cut angles only, on the argument that they
/// are all this workspace reads — which was true, and which was still the
/// wrong trade: a real volume's cuts state SZ2 phase coding, a contiguous
/// surveillance waveform, a 21.148°/s azimuth rate, PRF number 1 and 2.0 dB
/// thresholds, and a page that received a volume claiming all of that was
/// `Unknown` held a different volume from the one the worker decoded. The
/// whole pattern is **a few kilobytes against a 61 MiB payload**, so the
/// argument for dropping it never had a cost behind it.
fn encode_pattern(out: &mut Vec<u8>, pattern: &VolumeCoveragePattern) {
    out.extend_from_slice(&pattern.pattern_number().number().to_le_bytes());
    out.push(pattern.version());
    out.extend_from_slice(&pattern.doppler_velocity_resolution().to_le_bytes());
    out.push(pulse_width_code(pattern.pulse_width()));
    out.push(bits(&[
        pattern.sails_enabled(),
        pattern.mrle_enabled(),
        pattern.mpda_enabled(),
        pattern.base_tilt_enabled(),
        pattern.sequence_active(),
        pattern.truncated(),
    ]));
    out.push(pattern.sails_cuts());
    out.push(pattern.mrle_cuts());
    out.push(pattern.base_tilt_count());
    let cuts = pattern.elevation_cuts();
    out.extend_from_slice(&(cuts.len() as u32).to_le_bytes());
    for cut in cuts {
        out.extend_from_slice(&cut.elevation_angle_degrees().to_le_bytes());
        out.push(channel_code(cut.channel_configuration()));
        out.push(waveform_code(cut.waveform_type()));
        out.extend_from_slice(&cut.azimuth_rate_degrees_per_second().to_le_bytes());
        out.push(bits(&[
            cut.super_resolution_half_degree_azimuth(),
            cut.super_resolution_quarter_km_reflectivity(),
            cut.super_resolution_doppler_to_300km(),
            cut.super_resolution_dual_pol_to_300km(),
            cut.is_sails_cut(),
            cut.is_mrle_cut(),
            cut.is_mpda_cut(),
            cut.is_base_tilt_cut(),
        ]));
        out.push(cut.surveillance_prf_number());
        out.extend_from_slice(&cut.surveillance_prf_pulse_count().to_le_bytes());
        for threshold in [
            cut.reflectivity_threshold_db(),
            cut.velocity_threshold_db(),
            cut.spectrum_width_threshold_db(),
            cut.differential_reflectivity_threshold_db(),
            cut.differential_phase_threshold_db(),
            cut.correlation_coefficient_threshold_db(),
        ] {
            out.extend_from_slice(&threshold.to_le_bytes());
        }
        out.push(cut.sails_sequence_number());
        out.push(cut.mrle_sequence_number());
    }
}

/// Bytes per cut, for the reader's capacity guard: angle 8, two enum tags,
/// azimuth rate 8, flags 1, PRF number 1 and count 2, six `f32` thresholds,
/// two sequence numbers.
const CUT_BYTES: usize = 8 + 1 + 1 + 8 + 1 + 1 + 2 + 24 + 1 + 1;

fn decode_pattern(r: &mut Reader) -> Option<VolumeCoveragePattern> {
    let number = r.u16()?;
    let version = r.u8()?;
    let doppler_velocity_resolution = r.f32()?;
    let pulse_width = pulse_width_from_code(r.u8()?)?;
    let flags = r.u8()?;
    let sails_cuts = r.u8()?;
    let mrle_cuts = r.u8()?;
    let base_tilt_count = r.u8()?;
    let declared = r.u32()?;
    let count = r.bounded(declared, CUT_BYTES)?;
    let mut cuts = Vec::with_capacity(count);
    for _ in 0..count {
        let elevation_angle_degrees = r.f64()?;
        let channel_configuration = channel_from_code(r.u8()?)?;
        let waveform_type = waveform_from_code(r.u8()?)?;
        let azimuth_rate = r.f64()?;
        let cut_flags = r.u8()?;
        let surveillance_prf_number = r.u8()?;
        let surveillance_prf_pulse_count = r.u16()?;
        let mut thresholds = [0f32; 6];
        for threshold in &mut thresholds {
            *threshold = r.f32()?;
        }
        let sails_sequence_number = r.u8()?;
        let mrle_sequence_number = r.u8()?;
        cuts.push(ElevationCut::new(
            elevation_angle_degrees,
            channel_configuration,
            waveform_type,
            azimuth_rate,
            cut_flags & 1 != 0,
            cut_flags & 2 != 0,
            cut_flags & 4 != 0,
            cut_flags & 8 != 0,
            surveillance_prf_number,
            surveillance_prf_pulse_count,
            thresholds[0],
            thresholds[1],
            thresholds[2],
            thresholds[3],
            thresholds[4],
            thresholds[5],
            cut_flags & 16 != 0,
            sails_sequence_number,
            cut_flags & 32 != 0,
            mrle_sequence_number,
            cut_flags & 64 != 0,
            cut_flags & 128 != 0,
        ));
    }
    Some(VolumeCoveragePattern::new(
        number,
        version,
        doppler_velocity_resolution,
        pulse_width,
        flags & 1 != 0,
        sails_cuts,
        flags & 2 != 0,
        mrle_cuts,
        flags & 4 != 0,
        flags & 8 != 0,
        base_tilt_count,
        flags & 16 != 0,
        flags & 32 != 0,
        cuts,
    ))
}

/// Pack up to eight flags into a byte, least significant bit first.
fn bits(flags: &[bool]) -> u8 {
    flags
        .iter()
        .enumerate()
        .filter(|(_, set)| **set)
        .fold(0u8, |byte, (i, _)| byte | (1 << i))
}

/// The three model enums, each with an exhaustive pair so a variant added
/// upstream fails this build rather than encoding as a silent `Unknown`.
fn pulse_width_code(width: PulseWidth) -> u8 {
    match width {
        PulseWidth::Short => 0,
        PulseWidth::Long => 1,
        PulseWidth::Unknown => 2,
    }
}

fn pulse_width_from_code(code: u8) -> Option<PulseWidth> {
    Some(match code {
        0 => PulseWidth::Short,
        1 => PulseWidth::Long,
        2 => PulseWidth::Unknown,
        _ => return None,
    })
}

fn channel_code(channel: ChannelConfiguration) -> u8 {
    match channel {
        ChannelConfiguration::ConstantPhase => 0,
        ChannelConfiguration::RandomPhase => 1,
        ChannelConfiguration::SZ2Phase => 2,
        ChannelConfiguration::Unknown => 3,
    }
}

fn channel_from_code(code: u8) -> Option<ChannelConfiguration> {
    Some(match code {
        0 => ChannelConfiguration::ConstantPhase,
        1 => ChannelConfiguration::RandomPhase,
        2 => ChannelConfiguration::SZ2Phase,
        3 => ChannelConfiguration::Unknown,
        _ => return None,
    })
}

fn waveform_code(waveform: WaveformType) -> u8 {
    match waveform {
        WaveformType::CS => 0,
        WaveformType::CDW => 1,
        WaveformType::CDWO => 2,
        WaveformType::B => 3,
        WaveformType::SPP => 4,
        WaveformType::Unknown => 5,
    }
}

fn waveform_from_code(code: u8) -> Option<WaveformType> {
    Some(match code {
        0 => WaveformType::CS,
        1 => WaveformType::CDW,
        2 => WaveformType::CDWO,
        3 => WaveformType::B,
        4 => WaveformType::SPP,
        5 => WaveformType::Unknown,
        _ => return None,
    })
}

impl DecodedScan {
    /// Encode for a message port.
    ///
    /// # The size is the point, so here is the size
    ///
    /// A decoded volume is much larger than the archive it came from, because
    /// the archive is bzip2 and this is not. Measured, release, best of nine,
    /// with `from_bytes(&to_bytes()) == self` asserted on each before the row
    /// was printed:
    ///
    /// | volume | archive | payload | encode | parse |
    /// | --- | --- | --- | --- | --- |
    /// | KFTG 2023-06-22 03:46 | 16.11 MiB | 70.16 MiB | 13.8 ms | 10.3 ms |
    /// | KDMX 2022-03-05 23:23 | 12.11 MiB | 80.37 MiB | 16.3 ms | 16.1 ms |
    /// | KICX 2026-08-11 00:05 | 10.07 MiB | 77.19 MiB | 18.9 ms | 14.1 ms |
    /// | KMSX 2022-06-04 20:05 | 13.82 MiB | 60.28 MiB | 12.4 ms | 9.3 ms |
    /// | KCRP 2017-08-26 04:41 | 13.09 MiB | 48.32 MiB | 9.9 ms | 7.5 ms |
    ///
    /// Two things in that table are worth reading twice.
    ///
    /// **The archive size does not predict the payload size.** KICX is the
    /// smallest file and the second largest volume. What the payload tracks is
    /// gate count — radials × moments × gates — which has nothing to do with
    /// how well the storm compressed.
    ///
    /// **KCRP is the one row where the payload is barely over its moments.**
    /// It is a 2017 volume and it carries no clutter-filter power moment; the
    /// other four do, on every radial, and that seventh moment is most of the
    /// gap between the 59.96 MiB of moment bytes KFTG's radials hold in the six
    /// named slots and the 70.16 MiB written here.
    ///
    /// Sending the *archive* bytes instead and decoding at the far end is not
    /// an option on this port — decoding is the thing being moved.
    ///
    /// # One allocation
    ///
    /// [`byte_len`](Self::byte_len) is computed first and the buffer reserved
    /// once. At these sizes a growing `Vec` is not a rounding error: the
    /// doubling walk copies about as many bytes again as the payload holds, and
    /// it does it on the worker, whose linear memory only ever grows.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.byte_len());
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.push(VERSION);

        match self.scan.site() {
            None => out.push(0),
            Some(site) => {
                out.push(1);
                out.extend_from_slice(site.identifier());
                out.extend_from_slice(&site.latitude().to_le_bytes());
                out.extend_from_slice(&site.longitude().to_le_bytes());
                out.extend_from_slice(&site.height_meters().to_le_bytes());
                out.extend_from_slice(&site.tower_height_meters().to_le_bytes());
            }
        }

        encode_pattern(&mut out, self.scan.coverage_pattern());

        out.extend_from_slice(&self.declared_nyquist.to_bytes());

        out.extend_from_slice(&(self.scan.sweeps().len() as u32).to_le_bytes());
        for sweep in self.scan.sweeps() {
            out.push(sweep.elevation_number());
            out.extend_from_slice(&(sweep.radials().len() as u32).to_le_bytes());
            for radial in sweep.radials() {
                out.extend_from_slice(&radial.collection_timestamp().to_le_bytes());
                out.extend_from_slice(&radial.azimuth_number().to_le_bytes());
                out.extend_from_slice(&radial.azimuth_angle_degrees().to_le_bytes());
                out.extend_from_slice(&radial.azimuth_spacing_degrees().to_le_bytes());
                let (tag, byte) = status_code(radial.radial_status());
                out.push(tag);
                out.push(byte);
                out.push(radial.elevation_number());
                out.extend_from_slice(&radial.elevation_angle_degrees().to_le_bytes());
                let (moments, cfp) = moments_of(radial);
                let mut mask = moments
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.is_some())
                    .fold(0u8, |mask, (i, _)| mask | (1 << i));
                if cfp.is_some() {
                    mask |= 1 << 6;
                }
                out.push(mask);
                for moment in moments.iter().flatten() {
                    encode_moment(&mut out, moment);
                }
                if let Some(cfp) = &cfp {
                    encode_moment(&mut out, cfp);
                }
            }
        }
        out
    }

    /// Exactly how many bytes [`to_bytes`](Self::to_bytes) will write.
    ///
    /// Exact rather than an estimate, because it is used as the one
    /// reservation: an under-estimate silently reintroduces the doubling walk
    /// it exists to avoid, and `to_bytes_reserves_exactly_what_it_writes` is
    /// what keeps the two in step.
    fn byte_len(&self) -> usize {
        // magic, version, site tag.
        let mut total = 2 + 1 + 1;
        if self.scan.site().is_some() {
            total += 4 + 4 + 4 + 2 + 2;
        }
        // The pattern: number, version, resolution, pulse width, flags, three
        // counts, the cut count, and each cut.
        total += 2
            + 1
            + 4
            + 1
            + 1
            + 3
            + 4
            + self.scan.coverage_pattern().elevation_cuts().len() * CUT_BYTES;
        total +=
            4 + self.declared_nyquist.len() * 9 + 4 + self.declared_nyquist.contradicted().count();
        total += 4;
        for sweep in self.scan.sweeps() {
            total += 1 + 4;
            for radial in sweep.radials() {
                // timestamp, azimuth number, azimuth, spacing, status pair,
                // elevation number, elevation angle, moment mask.
                total += 8 + 2 + 4 + 4 + 2 + 1 + 4 + 1;
                let (moments, cfp) = moments_of(radial);
                for moment in moments.iter().flatten().chain(cfp.iter()) {
                    // gate count, first gate, interval, word size, scale,
                    // offset, gate length, gates.
                    total += 2 + 2 + 2 + 1 + 4 + 4 + 4 + moment.gates.len();
                }
            }
        }
        total
    }

    /// The inverse of [`to_bytes`](Self::to_bytes).
    ///
    /// `None` for a payload this build cannot read — a foreign magic, a version
    /// it does not have, a length that does not fit what follows it, or
    /// trailing bytes. The two ends of a message port can be different builds,
    /// and this is 60 MiB of untrusted input arriving in a browser tab, so
    /// every read is bounds-checked and every refusal is clean.
    ///
    /// **Trailing bytes are refused**, as `RenderInput::from_bytes` refuses
    /// them: a payload that decodes into a valid volume *and* has bytes left
    /// over is not this encoding, and accepting it would make the codec
    /// non-injective in the direction that hides a truncation.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.u16()? != MAGIC || r.u8()? != VERSION {
            return None;
        }

        let site = match r.u8()? {
            0 => None,
            1 => {
                let mut identifier = [0u8; 4];
                identifier.copy_from_slice(r.take(4)?);
                Some(nexrad_model::meta::Site::new(
                    identifier,
                    r.f32()?,
                    r.f32()?,
                    i16::from_le_bytes(r.take(2)?.try_into().ok()?),
                    r.u16()?,
                ))
            }
            _ => return None,
        };

        let pattern = decode_pattern(&mut r)?;

        let declared_nyquist = DeclaredNyquist::read(&mut r)?;

        // 5 is the smallest a sweep can encode to — the elevation number and an
        // empty radial count — so a count past that is a corrupt length rather
        // than a volume, refused before anything is reserved for it.
        let sweeps_declared = r.u32()?;
        let sweep_count = r.bounded(sweeps_declared, 5)?;
        let mut sweeps = Vec::with_capacity(sweep_count);
        for _ in 0..sweep_count {
            let elevation_number = r.u8()?;
            // 26 is a radial with no moments at all: timestamp 8, azimuth
            // number 2, azimuth 4, spacing 4, status 2, elevation number 1,
            // elevation angle 4, and the 1-byte moment mask.
            let radials_declared = r.u32()?;
            let radial_count = r.bounded(radials_declared, 26)?;
            let mut radials = Vec::with_capacity(radial_count);
            for _ in 0..radial_count {
                let collection_timestamp = r.i64()?;
                let azimuth_number = r.u16()?;
                let azimuth_angle_degrees = r.f32()?;
                let azimuth_spacing_degrees = r.f32()?;
                let radial_status = status_from_code(r.u8()?, r.u8()?)?;
                let radial_elevation_number = r.u8()?;
                let elevation_angle_degrees = r.f32()?;
                let mask = r.u8()?;
                // A bit this build does not have means an eighth moment from a
                // newer sender, whose bytes this reader would then take for the
                // next radial's header. Refused rather than skipped.
                if mask >= 1 << 7 {
                    return None;
                }
                let mut moments: [Option<MomentPayload>; 6] = Default::default();
                for (i, slot) in moments.iter_mut().enumerate() {
                    if mask & (1 << i) != 0 {
                        *slot = Some(decode_moment(&mut r)?);
                    }
                }
                // Bit 6 is the clutter-filter power moment, encoded last
                // because it is read last.
                let cfp = match mask & (1 << 6) != 0 {
                    true => Some(decode_moment(&mut r)?),
                    false => None,
                };
                let [reflectivity, velocity, spectrum_width, zdr, phi, rho] =
                    moments.map(|m| m.as_ref().map(MomentPayload::to_moment_data));
                radials.push(Radial::new(
                    collection_timestamp,
                    azimuth_number,
                    azimuth_angle_degrees,
                    azimuth_spacing_degrees,
                    radial_status,
                    radial_elevation_number,
                    elevation_angle_degrees,
                    reflectivity,
                    velocity,
                    spectrum_width,
                    zdr,
                    phi,
                    rho,
                    cfp.as_ref().map(MomentPayload::to_cfp_moment_data),
                ));
            }
            sweeps.push(Sweep::new(elevation_number, radials));
        }

        if !r.at_end() {
            return None;
        }

        let scan = match site {
            Some(site) => Scan::with_site(site, pattern, sweeps),
            None => Scan::new(pattern, sweeps),
        };
        Some(Self {
            scan,
            declared_nyquist,
        })
    }
}

#[cfg(test)]
mod tests;
