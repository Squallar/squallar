//! Decoders for radial data array packets (packet code 16 / 0xAF1F / 28).

use crate::model::{RadialPacket, RadialRun};
use crate::result::{Error, Result};

use super::header::{checked_end, read_i16, read_i32, read_u16, read_u32};

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

        // Gate data is padded to a halfword boundary.
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
/// RLE, with data sizes in halfwords rather than bytes. Gate values are 4-bit
/// (0–15) and only become physical values through the PDB's threshold table.
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

        let rle_bytes = num_hwords * 2;
        let rle_end = o + rle_bytes;
        if rle_end > data.len() {
            return Err(Error::UnexpectedEof {
                offset: o,
                expected: rle_bytes,
                available: data.len().saturating_sub(o),
            });
        }

        // Per byte: high nibble = run count, low nibble = value (0–15).
        let mut gate_values: Vec<u16> = Vec::with_capacity(num_range_bins as usize);
        for &byte in &data[o..rle_end] {
            let run = (byte >> 4) as usize;
            let val = (byte & 0x0F) as u16;
            for _ in 0..run {
                gate_values.push(val);
            }
        }
        o = rle_end;

        // Runs can overrun the declared bin count.
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
/// XDR encoding: a self-describing product description, then a component list
/// (radial = 1, text = 4).
///
/// Layout:
/// ```text
/// HW 1        Packet code (28)
/// HW 2        Reserved (0)
/// HW 3-4      Length of data block (u32, bytes after this field)
/// ... XDR-encoded product description and component data ...
/// ```
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

    let block_end = checked_end(data, o, block_length)?;

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

    if let Some(ci) = (0..num_components).next() {
        let comp_code = read_i32(data, o)?;
        o += 4;

        if comp_code == 1 {
            return decode_xdr_radial_component(data, o, block_end);
        }

        // Cannot skip an unknown component: its size is not recorded anywhere.
        log::warn!(
            "Skipping unknown XDR component code {} ({}/{})",
            comp_code,
            ci + 1,
            num_components
        );
    }

    // No radial component: an empty packet, not an error.
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

fn read_xdr_f32(data: &[u8], offset: usize) -> Result<f32> {
    let bits = read_u32(data, offset)?;
    Ok(f32::from_bits(bits))
}

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
    let padded = len.div_ceil(4) * 4;
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

fn read_xdr_string(data: &[u8], offset: usize) -> Result<(String, usize)> {
    let len = read_u32(data, offset)? as usize;
    let padded = len.div_ceil(4) * 4;
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

/// Semicolon-separated key=value pairs, e.g.
/// `"type = ushort; Unit = kft; Scale = 10; Offset = 0"`.
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

/// `o` must point at the component body, immediately after the component code.
fn decode_xdr_radial_component(
    data: &[u8],
    mut o: usize,
    block_end: usize,
) -> Result<(RadialPacket, usize)> {
    // Description string
    o = skip_xdr_string(data, o)?;

    // gate_width and first_gate, both f32 metres
    let gate_width = read_xdr_f32(data, o)?;
    o += 4;
    let first_gate = read_xdr_f32(data, o)?;
    o += 4;

    // Radial-level parameters
    o = skip_xdr_param_list(data, o)?;

    // Real products carry 360–720 radials per sweep; 3600 allows 0.1°
    // spacing with room to spare. A negative count would sign-extend through
    // `as usize` into a capacity-overflow panic in `Vec::with_capacity`.
    const MAX_RADIALS: i32 = 3600;
    let num_radials_raw = read_i32(data, o)?;
    o += 4;
    if !(0..=MAX_RADIALS).contains(&num_radials_raw) {
        return Err(Error::InvalidSymbologyBlock(format!(
            "XDR radial count {num_radials_raw} outside 0..={MAX_RADIALS}"
        )));
    }
    let num_radials = num_radials_raw as usize;

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

        // Every radial in a product shares one encoding, so read Scale/Offset
        // from the first and skip the rest.
        if radial_idx == 0 {
            let (attrs, new_o) = read_xdr_string(data, o)?;
            o = new_o;
            let (s, off) = parse_xdr_attrs(&attrs);
            xdr_data_scale = s;
            xdr_data_offset = off;
        } else {
            o = skip_xdr_string(data, o)?;
        }

        // Data array: length prefix (i32) + N × i32 values. A negative
        // length would wrap `data_end` right past the bounds check below.
        let arr_len_raw = read_i32(data, o)?;
        o += 4;
        let arr_len = usize::try_from(arr_len_raw).map_err(|_| {
            Error::InvalidSymbologyBlock(format!("negative XDR data array length {arr_len_raw}"))
        })?;

        let data_end = arr_len
            .checked_mul(4)
            .and_then(|bytes| o.checked_add(bytes))
            .filter(|&end| end <= data.len())
            .ok_or(Error::UnexpectedEof {
                offset: o,
                expected: arr_len.saturating_mul(4),
                available: data.len().saturating_sub(o),
            })?;

        // i32 on the wire, but the values are unsigned shorts.
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

    // Chosen so gate_interval_km() = 1.0 / scale_factor = gate_width_m / 1000.
    let scale_factor = if gate_width > 0.0 {
        1000.0 / gate_width
    } else {
        1.0
    };

    // Metres to range bins. `first_gate` is the range of the **centre** of
    // the first bin (the generic radial component's own definition — the
    // RPG's `buildDPR_Packet28.c` writes `first_range = 125.0` for a
    // 250 m bin, half a bin), while `first_range_bin` is a bin *index*
    // whose centre sits at `(index + 0.5) · gate_width`. Converting one to
    // the other therefore drops the half bin; rounding the raw ratio
    // instead put every gate of a half-bin product one bin too far out.
    let first_range_bin = if gate_width > 0.0 {
        (((first_gate / gate_width) - 0.5).max(0.0)).round() as i16
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

#[cfg(test)]
mod tests {
    use super::*;

    /// XDR scalars are all 4-byte big-endian.
    fn push_i32(d: &mut Vec<u8>, v: i32) {
        d.extend_from_slice(&v.to_be_bytes());
    }

    fn push_f32(d: &mut Vec<u8>, v: f32) {
        d.extend_from_slice(&v.to_bits().to_be_bytes());
    }

    /// An empty XDR string is just its zero length prefix.
    fn push_empty_string(d: &mut Vec<u8>) {
        push_i32(d, 0);
    }

    /// The packet-28 header and XDR product description leading up to the
    /// radial component body: empty strings, zeroed scalars, one component
    /// of code 1 (radial). Bytes appended afterwards land exactly where
    /// `decode_xdr_radial_component` starts reading.
    fn xdr_radial_component_prelude() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&28u16.to_be_bytes()); // packet code
        d.extend_from_slice(&0u16.to_be_bytes()); // reserved
        push_i32(&mut d, 0); // block length, patched by `finish`
        push_empty_string(&mut d); // name
        push_empty_string(&mut d); // description
        push_i32(&mut d, 0); // code
        push_i32(&mut d, 0); // type
        push_i32(&mut d, 0); // prod_time
        push_empty_string(&mut d); // radar_name
        d.extend_from_slice(&[0u8; 48]); // lat .. uncompressed_size
        push_i32(&mut d, 0); // product parameter count
        push_i32(&mut d, 0); // parameter list pointer
        push_i32(&mut d, 1); // number of components
        push_i32(&mut d, 0); // component pointer
        push_i32(&mut d, 1); // component code: radial
        push_empty_string(&mut d); // component description
        push_f32(&mut d, 1000.0); // gate_width
        push_f32(&mut d, 0.0); // first_gate
        push_i32(&mut d, 0); // radial parameter count
        push_i32(&mut d, 0); // parameter list pointer
        d
    }

    /// Patches the block length the packet header declares to match the body.
    fn finish(mut d: Vec<u8>) -> Vec<u8> {
        let len = (d.len() - 8) as u32;
        d[4..8].copy_from_slice(&len.to_be_bytes());
        d
    }

    /// The generic radial component declares `first_gate` as the range of
    /// the **centre** of bin 0, while `first_range_bin` is a bin index whose
    /// centre sits at `(index + 0.5) · gate_width`. The RPG's DPR
    /// (`buildDPR_Packet28.c`) writes 125 m for a 250 m bin — half a bin —
    /// which has to decode as index 0, not the 1 a raw `round` gives.
    #[test]
    fn a_half_bin_first_gate_decodes_as_bin_zero() {
        for (gate_width, first_gate, want) in [
            (250.0f32, 125.0f32, 0i16), // DPR: half a bin
            (1000.0, 500.0, 0),         // 1 km bins, half a bin
            (250.0, 375.0, 1),          // one whole bin out
            (250.0, 625.0, 2),
            (1000.0, 0.0, 0), // degenerate: clamps at the first bin
        ] {
            let mut d = xdr_radial_component_prelude();
            // Patch the gate_width / first_gate the prelude wrote.
            let n = d.len();
            d[n - 16..n - 12].copy_from_slice(&gate_width.to_bits().to_be_bytes());
            d[n - 12..n - 8].copy_from_slice(&first_gate.to_bits().to_be_bytes());
            push_i32(&mut d, 1); // one radial
            push_f32(&mut d, 0.0); // azimuth
            push_f32(&mut d, 0.0); // elevation
            push_f32(&mut d, 1.0); // width
            push_i32(&mut d, 1); // n_bins
            push_empty_string(&mut d); // attrs
            push_i32(&mut d, 1); // array length
            push_i32(&mut d, 7); // one gate
            let Ok((packet, _)) = decode_generic_radial_packet(&finish(d), 0) else {
                panic!("gate_width {gate_width} first_gate {first_gate} must decode");
            };
            assert_eq!(
                packet.first_range_bin, want,
                "gate_width {gate_width} first_gate {first_gate}",
            );
            // And the bin the model reports covers the declared centre.
            let centre = (f64::from(packet.first_range_bin) + 0.5) * packet.gate_interval_km();
            assert!(
                (centre - f64::from(first_gate) / 1000.0).abs() <= packet.gate_interval_km() * 0.5,
                "bin {} centre {centre} km does not cover the declared {first_gate} m",
                packet.first_range_bin,
            );
        }
    }

    /// -1 read as `i32` and cast straight to `usize` sign-extends to ~2^64,
    /// and `Vec::with_capacity` on that panics "capacity overflow".
    #[test]
    fn a_negative_xdr_radial_count_is_an_error_not_a_capacity_panic() {
        let mut d = xdr_radial_component_prelude();
        push_i32(&mut d, -1); // number of radials
        let r = decode_generic_radial_packet(&finish(d), 0);
        assert!(matches!(r, Err(Error::InvalidSymbologyBlock(_))), "{r:?}");
    }

    /// A count that passes the sign check must still be bounded: nothing
    /// real produces more than a few hundred radials per sweep.
    #[test]
    fn an_absurd_xdr_radial_count_is_rejected_before_allocation() {
        let mut d = xdr_radial_component_prelude();
        push_i32(&mut d, i32::MAX); // number of radials
        let r = decode_generic_radial_packet(&finish(d), 0);
        assert!(matches!(r, Err(Error::InvalidSymbologyBlock(_))), "{r:?}");
    }

    /// For `arr_len = -1`, the old `arr_len * 4` wrapped in release so
    /// `data_end` landed 4 bytes *before* the slice start and
    /// `data[o..data_end]` panicked in both build modes.
    #[test]
    fn a_negative_xdr_data_array_length_is_an_error_not_a_slice_panic() {
        let mut d = xdr_radial_component_prelude();
        push_i32(&mut d, 1); // one radial
        push_f32(&mut d, 0.0); // azimuth
        push_f32(&mut d, 0.0); // elevation
        push_f32(&mut d, 1.0); // width
        push_i32(&mut d, 0); // number of bins
        push_empty_string(&mut d); // attributes
        push_i32(&mut d, -1); // data array length
        let r = decode_generic_radial_packet(&finish(d), 0);
        assert!(matches!(r, Err(Error::InvalidSymbologyBlock(_))), "{r:?}");
    }

    /// The declared block length feeds `block_end`; `u32::MAX` overflows the
    /// add on 32-bit targets and merely runs past the buffer on 64-bit —
    /// either way the decoder must error, not panic.
    #[test]
    fn a_block_length_past_the_buffer_is_an_error() {
        let mut d = xdr_radial_component_prelude();
        push_i32(&mut d, 1); // one radial, but no radial bytes follow
        let mut d = finish(d);
        d[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_generic_radial_packet(&d, 0).is_err());
    }

    /// The guardrails must not reject a well-formed packet: one radial,
    /// two gates, decoded end to end.
    #[test]
    fn a_well_formed_generic_radial_packet_still_decodes() {
        let mut d = xdr_radial_component_prelude();
        push_i32(&mut d, 1); // one radial
        push_f32(&mut d, 90.0); // azimuth
        push_f32(&mut d, 0.5); // elevation
        push_f32(&mut d, 1.0); // width
        push_i32(&mut d, 2); // number of bins
        push_empty_string(&mut d); // attributes
        push_i32(&mut d, 2); // data array length
        push_i32(&mut d, 7);
        push_i32(&mut d, 11);
        let d = finish(d);
        let (packet, end) = match decode_generic_radial_packet(&d, 0) {
            Ok(v) => v,
            Err(e) => panic!("well-formed packet failed to decode: {e}"),
        };
        assert_eq!(end, d.len());
        assert_eq!(packet.radials.len(), 1);
        assert_eq!(packet.radials[0].gate_values, vec![7, 11]);
        assert_eq!(packet.radials[0].start_angle, 90.0);
        assert_eq!(packet.num_range_bins, 2);
    }
}
