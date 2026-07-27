//! Level III product message decoder.

mod header;
mod radial;
mod symbology;

use crate::model::{Level3Message, ProductDescriptionBlock};
use crate::result::{Error, Result};

/// Decode a raw Level III product byte stream into a [`Level3Message`].
///
/// Accepts an optional leading WMO/AWIPS header, and three encodings seen in
/// the wild: whole-file zlib (Unidata/NOAAPORT), plain headers with a
/// BZ2 symbology block (digital products), and fully uncompressed (legacy).
pub fn decode_product(data: &[u8]) -> Result<Level3Message> {
    let data = strip_wmo_header(data);

    let data = strip_trailing_bytes(data);

    let data = try_decompress_whole(data);

    // Again: a zlib stream can carry its own WMO header inside it.
    let data_ref: &[u8] = &data;
    let data_ref = strip_wmo_header(data_ref);

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

    let data_vec = decompress_after_pdb(data_ref, offset, &pdb)?;
    let data_ref: &[u8] = &data_vec;

    let symbology = if pdb.symbology_offset > 0 {
        // Halfwords, counted from the start of the message header. The
        // doubling can overflow a 32-bit `usize` (wasm32), so it is checked.
        let sym_byte_offset = (pdb.symbology_offset as usize)
            .checked_mul(2)
            .ok_or_else(|| {
                Error::InvalidProductDescription(format!(
                    "symbology offset {} halfwords overflows the address space",
                    pdb.symbology_offset
                ))
            })?;
        Some(symbology::decode_symbology_block(
            data_ref,
            sym_byte_offset,
        )?)
    } else {
        // Fallback: try parsing whatever follows the PDB.
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

/// A WMO/AWIPS envelope is `NNN \r\r\n`, an AWIPS product ID line, another
/// `\r\r\n`. Detected as the last `\r\r\n` within the first ~100 bytes.
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

/// Some NOAAPORT products end with `\r\r\n` or `\xff\xff\n` plus one more
/// byte — hence the check at `len-4`, not at the very end (matches MetPy).
fn strip_trailing_bytes(data: &[u8]) -> &[u8] {
    if data.len() >= 4 {
        let check = &data[data.len() - 4..data.len() - 1];
        if check == b"\r\r\n" || check == b"\xff\xff\n" {
            return &data[..data.len() - 4];
        }
    }
    data
}

/// Returns the input unchanged when it is not zlib.
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

/// Digital products (codes 94, 99, 134, 153–168, 170–177, …) put a compression
/// flag in product-specific halfword 51 = `product_specific_47_53[4]`. Nonzero
/// means everything after the PDB is one BZ2 stream holding the symbology.
fn decompress_after_pdb(
    data: &[u8],
    pdb_end: usize,
    pdb: &ProductDescriptionBlock,
) -> Result<Vec<u8>> {
    // Same field as MetPy's depVals[7].
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal message header + PDB, all zeroes except the symbology
    /// offset, which is `u32::MAX` halfwords. Doubling it overflows a
    /// 32-bit `usize` (wasm32); on 64-bit it points far past the buffer.
    /// Either way: an error, never a panic.
    #[test]
    fn a_symbology_offset_that_overflows_when_doubled_is_an_error() {
        // 18-byte message header + 102-byte PDB.
        let mut d = vec![0u8; 120];
        // Symbology offset lives at PDB byte 90, absolute byte 108.
        d[108..112].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_product(&d).is_err());
    }
}
