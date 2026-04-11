//! Decoders for radial data array packets (packet code 16 / 0xAF1F / 28).

use crate::model::{RadialPacket, RadialRun};
use crate::result::{Error, Result};

use super::header::{read_i16, read_i32, read_u16, read_u32};

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

        // Read gate values (num_bytes of data), widening u8 → u16
        let gate_end = o + num_bytes;
        if gate_end > data.len() {
            return Err(Error::UnexpectedEof {
                offset: o,
                expected: num_bytes,
                available: data.len().saturating_sub(o),
            });
        }
        let gate_values: Vec<u16> = data[o..gate_end].iter().map(|&b| b as u16).collect();
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
            xdr_data_scale: None,
            xdr_data_offset: None,
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
        let mut gate_values: Vec<u16> = Vec::with_capacity(num_range_bins as usize);
        for &byte in &data[o..rle_end] {
            let run = (byte >> 4) as usize;
            let val = (byte & 0x0F) as u16;
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
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials,
        },
        o,
    ))
}

/// Decode a Generic Data Component packet (packet code 28).
///
/// Packet 28 uses XDR (External Data Representation) encoding with a
/// self-describing product description header, followed by one or more
/// components (radial=1, text=4).
///
/// Layout:
/// ```text
/// HW 1        Packet code (28)
/// HW 2        Reserved (0)
/// HW 3-4      Length of data block (u32, bytes after this field)
/// ... XDR-encoded product description and component data ...
/// ```
///
/// The XDR product description contains variable-length strings and metadata,
/// followed by a parameter list and component list. For radial components,
/// the data includes per-radial azimuth, elevation, angular width, bin count,
/// attributes, and an `i32` data array.
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

    // --- XDR Product Description ---
    // Skip: name, description (variable-length strings)
    o = skip_xdr_string(data, o)?;
    o = skip_xdr_string(data, o)?;

    // Skip: code(i32), type(i32), prod_time(u32)
    o = skip_xdr_bytes(data, o, 12)?;

    // Skip: radar_name (variable-length string)
    o = skip_xdr_string(data, o)?;

    // Skip: lat(f32), lon(f32), height(f32), vol_time(u32), el_time(u32),
    //        el_angle(f32), vol_num(i32), op_mode(i32), vcp_num(i32),
    //        el_num(i32), compression(i32), uncompressed_size(i32)
    //        = 12 × 4 = 48 bytes
    o = skip_xdr_bytes(data, o, 48)?;

    // Skip product-level parameters
    o = skip_xdr_param_list(data, o)?;

    // --- Components ---
    let num_components = read_i32(data, o)?;
    o += 4;
    o += 4; // skip "pointer" field (always present, value is meaningless)

    for ci in 0..num_components {
        let comp_code = read_i32(data, o)?;
        o += 4;

        if comp_code == 1 {
            // Radial component
            return decode_xdr_radial_component(data, o, block_end);
        }

        // Unknown component type — we can't skip it without knowing its size,
        // so bail out. (Text components (code=4) aren't radial data.)
        log::warn!(
            "Skipping unknown XDR component code {} ({}/{})",
            comp_code,
            ci + 1,
            num_components
        );
        break;
    }

    // No radial component found — return empty packet so caller can handle gracefully
    Ok((
        RadialPacket {
            first_range_bin: 0,
            num_range_bins: 0,
            i_center: 0,
            j_center: 0,
            scale_factor: 1.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: Vec::new(),
        },
        block_end,
    ))
}

// ---------------------------------------------------------------------------
// XDR helper functions
// ---------------------------------------------------------------------------

/// Read a big-endian f32 from XDR data.
fn read_xdr_f32(data: &[u8], offset: usize) -> Result<f32> {
    let bits = read_u32(data, offset)?;
    Ok(f32::from_bits(bits))
}

/// Skip `n` bytes, validating bounds.
fn skip_xdr_bytes(data: &[u8], offset: usize, n: usize) -> Result<usize> {
    if offset + n > data.len() {
        return Err(Error::UnexpectedEof {
            offset,
            expected: n,
            available: data.len().saturating_sub(offset),
        });
    }
    Ok(offset + n)
}

/// Skip an XDR string (4-byte length prefix + padded-to-4 content).
fn skip_xdr_string(data: &[u8], offset: usize) -> Result<usize> {
    let len = read_u32(data, offset)? as usize;
    let padded = (len + 3) / 4 * 4;
    let end = offset + 4 + padded;
    if end > data.len() {
        return Err(Error::UnexpectedEof {
            offset,
            expected: 4 + padded,
            available: data.len().saturating_sub(offset),
        });
    }
    Ok(end)
}

/// Read an XDR string, returning the UTF-8 content and the offset after it.
fn read_xdr_string(data: &[u8], offset: usize) -> Result<(String, usize)> {
    let len = read_u32(data, offset)? as usize;
    let padded = (len + 3) / 4 * 4;
    let end = offset + 4 + padded;
    if end > data.len() {
        return Err(Error::UnexpectedEof {
            offset,
            expected: 4 + padded,
            available: data.len().saturating_sub(offset),
        });
    }
    let s = std::str::from_utf8(&data[offset + 4..offset + 4 + len])
        .unwrap_or("")
        .to_owned();
    Ok((s, end))
}

/// Parse Scale and Offset from an XDR attributes string.
///
/// The string format is semicolon-separated key=value pairs, e.g.:
/// `"type = ushort; Unit = kft; Scale = 10; Offset = 0"`
fn parse_xdr_attrs(attrs: &str) -> (Option<f32>, Option<f32>) {
    let mut scale = None;
    let mut offset = None;
    for part in attrs.split(';') {
        let part = part.trim();
        if let Some((key, val)) = part.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            match key {
                "Scale" => scale = val.parse::<f32>().ok(),
                "Offset" => offset = val.parse::<f32>().ok(),
                _ => {}
            }
        }
    }
    (scale, offset)
}

/// Skip an XDR parameter list: count(i32) + pointer(i32) + N × (string, string, [pointer]).
fn skip_xdr_param_list(data: &[u8], mut offset: usize) -> Result<usize> {
    let num = read_i32(data, offset)?;
    offset += 4;
    offset += 4; // skip pointer

    for i in 0..num {
        offset = skip_xdr_string(data, offset)?; // parameter id
        offset = skip_xdr_string(data, offset)?; // parameter attributes
        if i < num - 1 {
            offset += 4; // inter-item pointer
        }
    }
    Ok(offset)
}

/// Decode an XDR radial component into a [`RadialPacket`].
///
/// Assumes `offset` points to the start of the radial component body
/// (immediately after the component code).
fn decode_xdr_radial_component(
    data: &[u8],
    mut o: usize,
    block_end: usize,
) -> Result<(RadialPacket, usize)> {
    // Description string
    o = skip_xdr_string(data, o)?;

    // gate_width (float, meters) and first_gate (float, meters)
    let gate_width = read_xdr_f32(data, o)?;
    o += 4;
    let first_gate = read_xdr_f32(data, o)?;
    o += 4;

    // Skip radial-level parameters
    o = skip_xdr_param_list(data, o)?;

    // Number of radials
    let num_radials = read_i32(data, o)? as usize;
    o += 4;

    let mut radials = Vec::with_capacity(num_radials);
    let mut max_bins: u16 = 0;
    let mut xdr_data_scale: Option<f32> = None;
    let mut xdr_data_offset: Option<f32> = None;

    for radial_idx in 0..num_radials {
        let azimuth = read_xdr_f32(data, o)?;
        o += 4;
        let _elevation = read_xdr_f32(data, o)?;
        o += 4;
        let width = read_xdr_f32(data, o)?;
        o += 4;
        let num_bins = read_i32(data, o)? as usize;
        o += 4;

        // Read attributes string on first radial to extract Scale/Offset.
        // All radials in a product share the same encoding.
        if radial_idx == 0 {
            let (attrs, new_o) = read_xdr_string(data, o)?;
            o = new_o;
            let (s, off) = parse_xdr_attrs(&attrs);
            xdr_data_scale = s;
            xdr_data_offset = off;
        } else {
            o = skip_xdr_string(data, o)?;
        }

        // Data array: length prefix (i32) + N × i32 values
        let arr_len = read_i32(data, o)? as usize;
        o += 4;

        let data_end = o + arr_len * 4;
        if data_end > data.len() {
            return Err(Error::UnexpectedEof {
                offset: o,
                expected: arr_len * 4,
                available: data.len().saturating_sub(o),
            });
        }

        // Values are i32 in XDR but typically represent unsigned shorts.
        // Truncate to u16.
        let gate_values: Vec<u16> = data[o..data_end]
            .chunks_exact(4)
            .map(|c| {
                let val = i32::from_be_bytes([c[0], c[1], c[2], c[3]]);
                val as u16
            })
            .collect();
        o = data_end;

        max_bins = max_bins.max(num_bins as u16);

        radials.push(RadialRun {
            start_angle: azimuth,
            angle_delta: width,
            gate_values,
        });
    }

    // gate_width is in meters; scale_factor = 1000 / gate_width_m
    // so that gate_interval_km() = 1.0 / scale_factor = gate_width_m / 1000.0
    let scale_factor = if gate_width > 0.0 {
        1000.0 / gate_width
    } else {
        1.0
    };

    // Convert first_gate from meters to range bins
    let first_range_bin = if gate_width > 0.0 {
        (first_gate / gate_width).round() as i16
    } else {
        0
    };

    Ok((
        RadialPacket {
            first_range_bin,
            num_range_bins: max_bins,
            i_center: 0,
            j_center: 0,
            scale_factor,
            is_legacy: false,
            xdr_data_scale,
            xdr_data_offset,
            radials,
        },
        block_end.max(o),
    ))
}
