//! Decoder for the Digital Radial Data Array (packet code 16 / 0xAF1F).

use crate::model::{RadialPacket, RadialRun};
use crate::result::{Error, Result};

use super::header::{read_i16, read_u16};

/// Decode a Digital Radial Data Array packet.
///
/// Layout (ICD 2620001, packet code 16):
/// ```text
/// HW 1     Packet code (16 or 0xAF1F)
/// HW 2     Index of first range bin
/// HW 3     Number of range bins
/// HW 4     I center of sweep
/// HW 5     J center of sweep
/// HW 6     Scale factor (multiplied by 1000)
/// HW 7     Number of radials
/// --- per radial ---
/// HW 1     Number of bytes in this radial (data only)
/// HW 2     Start angle (scaled by 10, 0.1° units)
/// HW 3     Angle delta (scaled by 10, 0.1° units)
/// ... num_bytes of gate data (padded to halfword) ...
/// ```
///
/// Returns the decoded [`RadialPacket`] and the byte offset after the packet.
pub(crate) fn decode_radial_packet(
    data: &[u8],
    offset: usize,
) -> Result<(RadialPacket, usize)> {
    let mut o = offset;

    let _packet_code = read_u16(data, o)?;
    o += 2;
    let first_range_bin = read_i16(data, o)?;
    o += 2;
    let num_range_bins = read_u16(data, o)?;
    o += 2;
    let i_center = read_i16(data, o)?;
    o += 2;
    let j_center = read_i16(data, o)?;
    o += 2;
    let scale_factor_raw = read_i16(data, o)?;
    o += 2;
    let num_radials = read_u16(data, o)?;
    o += 2;

    // Scale factor: the raw value is multiplied by 1000 in the packet.
    let scale_factor = scale_factor_raw as f32 / 1000.0;

    let mut radials = Vec::with_capacity(num_radials as usize);
    for _ in 0..num_radials {
        let num_bytes = read_u16(data, o)? as usize;
        o += 2;
        let start_angle_raw = read_i16(data, o)?;
        o += 2;
        let angle_delta_raw = read_i16(data, o)?;
        o += 2;

        let start_angle = start_angle_raw as f32 / 10.0;
        let angle_delta = angle_delta_raw as f32 / 10.0;

        // Read gate values (num_bytes of data)
        let gate_end = o + num_bytes;
        if gate_end > data.len() {
            return Err(Error::UnexpectedEof {
                offset: o,
                expected: num_bytes,
                available: data.len().saturating_sub(o),
            });
        }
        let gate_values = data[o..gate_end].to_vec();
        o = gate_end;

        // Data is padded to halfword boundary
        if num_bytes % 2 != 0 {
            o += 1;
        }

        radials.push(RadialRun {
            start_angle,
            angle_delta,
            gate_values,
        });
    }

    Ok((
        RadialPacket {
            first_range_bin,
            num_range_bins,
            i_center,
            j_center,
            scale_factor,
            radials,
        },
        o,
    ))
}
