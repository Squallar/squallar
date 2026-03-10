//! Decoders for radial data array packets (packet code 16 / 0xAF1F / 28).

use crate::model::{RadialPacket, RadialRun};
use crate::result::{Error, Result};

use super::header::{read_i16, read_u16, read_u32};

/// Decode a Digital Radial Data Array packet (packet code 16).
///
/// Layout (ICD 2620001, packet code 16):
/// ```text
/// HW 1     Packet code (16)
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
pub(crate) fn decode_digital_radial_packet(
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
            is_legacy: false,
            radials,
        },
        o,
    ))
}

/// Decode a Legacy Radial Data Array packet (packet code 0xAF1F).
///
/// This format uses Run-Length Encoding (RLE) and records data sizes in
/// halfwords rather than bytes. Gate values are 4-bit (0–15) and must be
/// mapped through the PDB's threshold table for physical values.
///
/// Layout (ICD 2620001):
/// ```text
/// HW 1     Packet code (0xAF1F)
/// HW 2     Index of first range bin
/// HW 3     Number of range bins
/// HW 4     I center of sweep
/// HW 5     J center of sweep
/// HW 6     Scale factor (multiplied by 1000)
/// HW 7     Number of radials
/// --- per radial ---
/// HW 1     Number of RLE halfwords in this radial
/// HW 2     Start angle (0.1° units)
/// HW 3     Angle delta (0.1° units)
/// ... RLE data (num_hwords * 2 bytes) ...
/// ```
pub(crate) fn decode_legacy_radial_packet(
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

    let scale_factor = scale_factor_raw as f32 / 1000.0;

    let mut radials = Vec::with_capacity(num_radials as usize);
    for _ in 0..num_radials {
        let num_hwords = read_u16(data, o)? as usize;
        o += 2;
        let start_angle_raw = read_i16(data, o)?;
        o += 2;
        let angle_delta_raw = read_i16(data, o)?;
        o += 2;

        let start_angle = start_angle_raw as f32 / 10.0;
        let angle_delta = angle_delta_raw as f32 / 10.0;

        // Read RLE data: num_hwords halfwords = num_hwords * 2 bytes
        let rle_bytes = num_hwords * 2;
        let rle_end = o + rle_bytes;
        if rle_end > data.len() {
            return Err(Error::UnexpectedEof {
                offset: o,
                expected: rle_bytes,
                available: data.len().saturating_sub(o),
            });
        }

        // Decode RLE: each byte encodes high nibble = run count, low nibble = value (0-15)
        let mut gate_values = Vec::with_capacity(num_range_bins as usize);
        for &byte in &data[o..rle_end] {
            let run = (byte >> 4) as usize;
            let val = byte & 0x0F;
            for _ in 0..run {
                gate_values.push(val);
            }
        }
        o = rle_end;

        // Truncate to expected number of range bins
        gate_values.truncate(num_range_bins as usize);

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
            is_legacy: true,
            radials,
        },
        o,
    ))
}

/// Decode a Generic Data Component packet (packet code 28).
///
/// This is used by newer NEXRAD products. Layout (ICD Table V, packet 28):
/// ```text
/// HW 1        Packet code (28)
/// HW 2        Reserved
/// HW 3-4      Length of data block (u32, bytes after this field)
/// HW 5        Number of radials (u16)
/// HW 6        Number of range bins per radial (u16)
/// HW 7        I center of sweep (i16)
/// HW 8        J center of sweep (i16)
/// HW 9        Scale factor (i16, range bin width in 0.001 km)
/// HW 10       Number of bytes per radial gate (1 or 2)
/// --- per radial ---
/// HW 1-2      Starting angle (u32, scaled by 10, 0.1° units)
/// HW 3-4      Angle delta (u32, scaled by 10, 0.1° units)
/// ... gate data (num_bins * bytes_per_gate bytes, padded to halfword) ...
/// ```
///
/// Returns the decoded [`RadialPacket`] and the byte offset after the packet.
pub(crate) fn decode_generic_radial_packet(
    data: &[u8],
    offset: usize,
) -> Result<(RadialPacket, usize)> {
    let mut o = offset;

    let _packet_code = read_u16(data, o)?; // 28
    o += 2;
    let _reserved = read_u16(data, o)?;
    o += 2;
    let block_length = read_u32(data, o)? as usize;
    o += 4;

    let block_end = o + block_length;

    let num_radials = read_u16(data, o)?;
    o += 2;
    let num_range_bins = read_u16(data, o)?;
    o += 2;
    let i_center = read_i16(data, o)?;
    o += 2;
    let j_center = read_i16(data, o)?;
    o += 2;
    let scale_factor_raw = read_i16(data, o)?;
    o += 2;
    let bytes_per_gate = read_u16(data, o)? as usize;
    o += 2;

    // Scale factor encodes range bin width: raw value * 0.001 km
    // We store it as "pixels per range bin" for consistency with packet 16,
    // so scale_factor = 1.0 / (raw * 0.001) when raw > 0.
    let scale_factor = if scale_factor_raw > 0 {
        1000.0 / scale_factor_raw as f32
    } else {
        1.0
    };

    let mut radials = Vec::with_capacity(num_radials as usize);
    for _ in 0..num_radials {
        if o >= block_end || o + 4 > data.len() {
            break;
        }
        // Angles are stored as unsigned scaled values (0.1° units) in some
        // implementations, but the ICD-defined format uses two halfwords.
        let start_angle_raw = read_u16(data, o)?;
        o += 2;
        let angle_delta_raw = read_u16(data, o)?;
        o += 2;

        let start_angle = start_angle_raw as f32 / 10.0;
        let angle_delta = angle_delta_raw as f32 / 10.0;

        let gate_data_len = num_range_bins as usize * bytes_per_gate;
        let gate_end = o + gate_data_len;
        if gate_end > data.len() {
            return Err(Error::UnexpectedEof {
                offset: o,
                expected: gate_data_len,
                available: data.len().saturating_sub(o),
            });
        }

        // For 2-byte gates, take only the MSB to produce a Vec<u8> compatible
        // with the rest of the pipeline.
        let gate_values = if bytes_per_gate == 2 {
            data[o..gate_end]
                .chunks(2)
                .map(|pair| pair[0])
                .collect()
        } else {
            data[o..gate_end].to_vec()
        };
        o = gate_end;

        // Pad to halfword boundary
        if gate_data_len % 2 != 0 {
            o += 1;
        }

        radials.push(RadialRun {
            start_angle,
            angle_delta,
            gate_values,
        });
    }

    // Ensure we advance past the full block
    o = o.max(block_end);

    Ok((
        RadialPacket {
            first_range_bin: 0,
            num_range_bins,
            i_center,
            j_center,
            scale_factor,
            is_legacy: false,
            radials,
        },
        o,
    ))
}
