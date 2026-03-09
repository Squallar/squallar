//! Level III product message decoder.
//!
//! Decodes a raw Level III product byte stream (possibly with WMO header and
//! zlib compression) into a [`Level3Message`].

mod header;
mod radial;
mod symbology;

use crate::model::Level3Message;
use crate::result::{Error, Result};

/// Decode a raw Level III product byte stream into a [`Level3Message`].
///
/// The input may optionally begin with a WMO/AWIPS header (which is stripped
/// automatically) and the product data may be zlib-compressed (which is
/// decompressed automatically).
pub fn decode_product(data: &[u8]) -> Result<Level3Message> {
    let data = strip_wmo_header(data);
    let data = try_decompress(data)?;

    let (msg_header, offset) = header::decode_message_header(&data)?;
    let (pdb, offset) = header::decode_pdb(&data, offset)?;

    let symbology = if pdb.symbology_offset > 0 {
        // Symbology offset is in halfwords (2 bytes) from the start of the
        // product message (i.e. from the message header).
        let sym_byte_offset = pdb.symbology_offset as usize * 2;
        Some(symbology::decode_symbology_block(&data, sym_byte_offset)?)
    } else {
        // Offset is within the message-header-relative frame, but we also check
        // beyond our current parse offset for products that always have symbology.
        if offset < data.len() {
            symbology::decode_symbology_block(&data, offset).ok()
        } else {
            None
        }
    };

    Ok(Level3Message {
        header: msg_header,
        pdb,
        symbology,
    })
}

/// Strip an optional WMO/AWIPS envelope header from the beginning of the data.
///
/// WMO headers look like: `NNN \r\r\n` followed by an AWIPS product ID line
/// and another `\r\r\n`. We detect this by looking for the `\r\r\n` sequence.
fn strip_wmo_header(data: &[u8]) -> &[u8] {
    // WMO-distributed Level III files start with a text preamble before the
    // binary product message.  The binary message begins right after the last
    // `\r\r\n` in the header envelope.  Scan the first ~100 bytes for the
    // pattern; if found, return data after it.  Otherwise, return as-is.
    let search_limit = data.len().min(100);
    let mut last_crcrlf = None;
    for i in 0..search_limit.saturating_sub(2) {
        if data[i] == b'\r' && data[i + 1] == b'\r' && data[i + 2] == b'\n' {
            last_crcrlf = Some(i + 3);
        }
    }
    if let Some(pos) = last_crcrlf {
        &data[pos..]
    } else {
        data
    }
}

/// Try to zlib-decompress the data.  If decompression fails (e.g. the data
/// is not compressed), return the original bytes unchanged.
fn try_decompress(data: &[u8]) -> Result<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    // Check for zlib magic bytes (0x78 0x01 / 0x78 0x9C / 0x78 0xDA)
    if data.len() >= 2 && data[0] == 0x78 {
        let mut decoder = ZlibDecoder::new(data);
        let mut decompressed = Vec::new();
        match decoder.read_to_end(&mut decompressed) {
            Ok(_) => return Ok(decompressed),
            Err(_) => {
                // Not actually zlib — fall through and use raw data.
                log::debug!("Data starts with 0x78 but is not valid zlib, using raw");
            }
        }
    }

    Ok(data.to_vec())
}
