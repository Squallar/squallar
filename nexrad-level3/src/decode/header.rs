//! Decoders for the 18-byte Message Header and 102-byte Product Description Block.

use crate::model::{MessageHeader, ProductDescriptionBlock};
use crate::result::{Error, Result};

/// Extract a `[u8; N]` slice from `data` at the given offset.
fn read_bytes<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    data.get(offset..offset + N)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::UnexpectedEof {
            offset,
            expected: N,
            available: data.len().saturating_sub(offset),
        })
}

/// Read a big-endian `i16` from `data` at the given offset.
pub(crate) fn read_i16(data: &[u8], offset: usize) -> Result<i16> {
    Ok(i16::from_be_bytes(read_bytes(data, offset)?))
}

/// Read a big-endian `u16` from `data` at the given offset.
pub(crate) fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(read_bytes(data, offset)?))
}

/// Read a big-endian `u32` from `data` at the given offset.
pub(crate) fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(read_bytes(data, offset)?))
}

/// Read a big-endian `i32` from `data` at the given offset.
pub(crate) fn read_i32(data: &[u8], offset: usize) -> Result<i32> {
    Ok(i32::from_be_bytes(read_bytes(data, offset)?))
}

/// Decode the 18-byte Message Header.
///
/// Returns the decoded header and the byte offset immediately after it.
///
/// ICD 2620001 Figure 3-3 (Message Header):
/// ```text
/// Halfword  Field
/// 1         Message Code (i16)
/// 2         Date of Message (u16, modified Julian)
/// 3-4       Time of Message (u32, seconds since midnight)
/// 5-6       Length of Message (u32, bytes)
/// 7         Source ID (u16)
/// 8         Destination ID (u16)
/// 9         Number of Blocks (u16)
/// ```
pub(crate) fn decode_message_header(data: &[u8]) -> Result<(MessageHeader, usize)> {
    let mut o = 0;
    let message_code = read_i16(data, o)?;
    o += 2;
    let date_of_message = read_u16(data, o)?;
    o += 2;
    let time_of_message = read_u32(data, o)?;
    o += 4;
    let message_length = read_u32(data, o)?;
    o += 4;
    let source_id = read_u16(data, o)?;
    o += 2;
    let destination_id = read_u16(data, o)?;
    o += 2;
    let number_of_blocks = read_u16(data, o)?;
    o += 2;

    Ok((
        MessageHeader {
            message_code,
            date_of_message,
            time_of_message,
            message_length,
            source_id,
            destination_id,
            number_of_blocks,
        },
        o,
    ))
}

/// Decode the 102-byte Product Description Block (PDB).
///
/// The PDB starts immediately after the message header and contains radar
/// site location, product parameters, data thresholds, and symbology offsets.
///
/// ICD 2620001 Figure 3-6 layout (halfword numbers):
/// ```text
/// HW 1      Block divider (-1)
/// HW 2-3    Latitude (i32, 1/1000 degree)
/// HW 4-5    Longitude (i32, 1/1000 degree)
/// HW 6      Height (i16, feet MSL)
/// HW 7      Product code (i16)
/// HW 8      Operational mode (u16)
/// HW 9      VCP (u16)
/// HW 10     Sequence number (i16)
/// HW 11     Volume scan number (u16)
/// HW 12     Volume scan date (u16)
/// HW 13-14  Volume scan time (u32)
/// HW 15     Product generation date (u16)
/// HW 16-17  Product generation time (u32)
/// HW 18-19  Product specific (p1, p2)
/// HW 20     Elevation number (u16)
/// HW 21     Product specific (p3)
/// HW 22-37  Thresholds[0..16] (u16 × 16)
/// HW 38-44  Product specific HW 47-53 (i16 × 7)
/// HW 45     Version (u8 high) + Spot blank (u8 low)
/// HW 46-47  Symbology offset (u32, halfwords)
/// HW 48-49  Graphic offset (u32, halfwords)
/// HW 50-51  Tabular offset (u32, halfwords)
/// ```
pub(crate) fn decode_pdb(data: &[u8], start: usize) -> Result<(ProductDescriptionBlock, usize)> {
    let mut o = start;

    let block_divider = read_i16(data, o)?;
    o += 2;
    let lat_raw = read_i32(data, o)?;
    o += 4;
    let lon_raw = read_i32(data, o)?;
    o += 4;
    let height = read_i16(data, o)?;
    o += 2;
    let product_code = read_i16(data, o)?;
    o += 2;
    let operational_mode = read_u16(data, o)?;
    o += 2;
    let vcp = read_u16(data, o)?;
    o += 2;
    let sequence_number = read_i16(data, o)?;
    o += 2;
    let volume_scan_number = read_u16(data, o)?;
    o += 2;
    let volume_scan_date = read_u16(data, o)?;
    o += 2;
    let volume_scan_time = read_u32(data, o)?;
    o += 4;
    let generation_date = read_u16(data, o)?;
    o += 2;
    let generation_time = read_u32(data, o)?;
    o += 4;
    let product_specific_1 = read_i16(data, o)?;
    o += 2;
    let product_specific_2 = read_i16(data, o)?;
    o += 2;
    let elevation_number = read_u16(data, o)?;
    o += 2;
    let product_specific_3 = read_i16(data, o)?;
    o += 2;

    let mut thresholds = [0u16; 16];
    for threshold in &mut thresholds {
        *threshold = read_u16(data, o)?;
        o += 2;
    }

    let mut product_specific_47_53 = [0i16; 7];
    for ps in &mut product_specific_47_53 {
        *ps = read_i16(data, o)?;
        o += 2;
    }

    let version_spot = read_u16(data, o)?;
    o += 2;
    let version = (version_spot >> 8) as u8;
    let spot_blank = (version_spot & 0xFF) as u8;

    let symbology_offset = read_u32(data, o)?;
    o += 4;
    let graphic_offset = read_u32(data, o)?;
    o += 4;
    let tabular_offset = read_u32(data, o)?;
    o += 4;

    Ok((
        ProductDescriptionBlock {
            block_divider,
            latitude: lat_raw as f64 / 1000.0,
            longitude: lon_raw as f64 / 1000.0,
            height,
            product_code,
            operational_mode,
            vcp,
            sequence_number,
            volume_scan_number,
            volume_scan_date,
            volume_scan_time,
            generation_date,
            generation_time,
            product_specific_1,
            product_specific_2,
            elevation_number,
            product_specific_3,
            thresholds,
            product_specific_47_53,
            version,
            spot_blank,
            symbology_offset,
            graphic_offset,
            tabular_offset,
        },
        o,
    ))
}
