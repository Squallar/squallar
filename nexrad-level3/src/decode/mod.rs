//! Level III product message decoder.
//!
//! Decodes a raw Level III product byte stream (possibly with WMO header and
//! zlib/bz2 compression) into a [`Level3Message`].

mod header;
mod radial;
mod symbology;

use crate::model::{Level3Message, ProductDescriptionBlock};
use crate::result::Result;

/// Decode a raw Level III product byte stream into a [`Level3Message`].
///
/// The input may optionally begin with a WMO/AWIPS header (which is stripped
/// automatically). The data may be:
/// - Entirely zlib-compressed (common for Unidata/NOAAPORT distribution)
/// - Uncompressed headers with BZ2-compressed symbology data (digital products)
/// - Fully uncompressed (legacy products)
pub fn decode_product(data: &[u8]) -> Result<Level3Message> {
    // Step 1: Strip WMO header
    let data = strip_wmo_header(data);

    // Step 2: Strip trailing \r\r\n or \xff\xff\n end-of-transmission bytes
    let data = strip_trailing_bytes(data);

    // Step 3: Try whole-file zlib decompression. Unidata/NOAAPORT files are
    //         often entirely zlib-compressed after the WMO envelope.
    let data = try_decompress_whole(data);

    // Step 4: Strip WMO header again — the decompressed payload may contain
    //         its own WMO header that was inside the zlib stream.
    let data_ref: &[u8] = &data;
    let data_ref = strip_wmo_header(data_ref);

    // Step 5: Parse the 18-byte Message Header + 102-byte PDB (120 bytes total)
    let (msg_header, offset) = header::decode_message_header(data_ref)?;
    let (pdb, offset) = header::decode_pdb(data_ref, offset)?;

    log::debug!(
        "Level III message: code={}, blocks={}, msg_len={}, buf_len={}, product_code={}, sym_off={}",
        msg_header.message_code,
        msg_header.number_of_blocks,
        msg_header.message_length,
        data_ref.len(),
        pdb.product_code,
        pdb.symbology_offset
    );

    // Step 6: Handle per-product BZ2 compression after the PDB.
    //         Many digital products (codes 94, 99, 134, 153–168, etc.) store a
    //         compression flag in product-specific halfword 51 (ps47_53[4]).
    //         When set, the remaining data after the PDB is BZ2-compressed.
    let data_vec = decompress_after_pdb(data_ref, offset, &pdb)?;
    let data_ref: &[u8] = &data_vec;

    // Step 7: Parse the symbology block
    let symbology = if pdb.symbology_offset > 0 {
        // Symbology offset is in halfwords (2 bytes) from the start of the
        // product message (i.e. from the message header).
        let sym_byte_offset = pdb.symbology_offset as usize * 2;
        Some(symbology::decode_symbology_block(data_ref, sym_byte_offset)?)
    } else {
        // Fallback: try parsing whatever follows the PDB
        if offset < data_ref.len() {
            symbology::decode_symbology_block(data_ref, offset).ok()
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
/// and another `\r\r\n`. We detect this by looking for the `\r\r\n` sequence
/// in the first ~100 bytes and returning data after the last occurrence.
fn strip_wmo_header(data: &[u8]) -> &[u8] {
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

/// Strip trailing end-of-transmission bytes from the data.
///
/// Some NOAAPORT-distributed products end with `\r\r\n` + trailing byte
/// or `\xff\xff\n` + trailing byte. MetPy checks the last 4 bytes for this
/// pattern and truncates them.
fn strip_trailing_bytes(data: &[u8]) -> &[u8] {
    if data.len() >= 4 {
        let check = &data[data.len() - 4..data.len() - 1];
        if check == b"\r\r\n" || check == b"\xff\xff\n" {
            return &data[..data.len() - 4];
        }
    }
    data
}

/// Try to zlib-decompress the entire byte stream.
///
/// Matches MetPy's `zlib_decompress_all_frames` approach: attempt decompression
/// and if it fails (data isn't zlib), return the original bytes unchanged.
fn try_decompress_whole(data: &[u8]) -> Vec<u8> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    let mut decoder = ZlibDecoder::new(data);
    let mut decompressed = Vec::new();
    match decoder.read_to_end(&mut decompressed) {
        Ok(_) if !decompressed.is_empty() => {
            log::debug!(
                "Whole-file zlib decompressed {} -> {} bytes",
                data.len(),
                decompressed.len()
            );
            decompressed
        }
        _ => data.to_vec(),
    }
}

/// Decompress BZ2-compressed data after the PDB for products that use it.
///
/// Many digital Level III products (codes 94, 99, 134, 153–168, 170–177, etc.)
/// store a compression flag in product-specific halfword 51, which maps to
/// `product_specific_47_53[4]` in our PDB struct. When nonzero, the data
/// following the PDB is a single BZ2 stream containing the symbology block.
fn decompress_after_pdb(
    data: &[u8],
    pdb_end: usize,
    pdb: &ProductDescriptionBlock,
) -> Result<Vec<u8>> {
    // Compression flag: product_specific_47_53[4] = depVals[7] in MetPy's ordering
    let compression_flag = pdb.product_specific_47_53[4];

    if compression_flag == 0 || pdb_end >= data.len() {
        return Ok(data.to_vec());
    }

    log::debug!(
        "Product code={} has compression flag={}, attempting BZ2 decompression from offset {}",
        pdb.product_code,
        compression_flag,
        pdb_end
    );

    let compressed_tail = &data[pdb_end..];

    // Try BZ2 decompression
    use bzip2::read::BzDecoder;
    use std::io::Read;

    let mut decoder = BzDecoder::new(compressed_tail);
    let mut decompressed = Vec::new();
    match decoder.read_to_end(&mut decompressed) {
        Ok(_) if !decompressed.is_empty() => {
            log::debug!(
                "BZ2 decompressed {} -> {} bytes after PDB",
                compressed_tail.len(),
                decompressed.len()
            );
            let mut buf = data[..pdb_end].to_vec();
            buf.extend_from_slice(&decompressed);
            Ok(buf)
        }
        Ok(_) => {
            log::debug!("BZ2 decompression produced empty output, using raw data");
            Ok(data.to_vec())
        }
        Err(e) => {
            log::debug!("BZ2 decompression failed ({}), using raw data", e);
            Ok(data.to_vec())
        }
    }
}
