//! Decoder for the Symbology Block and its data layers.

use crate::model::{DataLayer, DataPacket, SymbologyBlock};
use crate::result::Result;

use super::header::{read_i16, read_u16, read_u32};

/// Decode the Product Symbology Block starting at `offset` in `data`.
///
/// Layout (ICD 2620001 Figure 3-6 / Packet Layer structure):
/// ```text
/// HW 1    Block divider (-1)
/// HW 2    Block ID (1)
/// HW 3-4  Block length (u32, bytes following this field)
/// HW 5    Number of layers (u16)
/// --- per layer ---
/// HW 1    Layer divider (-1)
/// HW 2-3  Layer length (u32, bytes of data packets)
/// ... data packets ...
/// ```
pub(crate) fn decode_symbology_block(
    data: &[u8],
    offset: usize,
) -> Result<SymbologyBlock> {
    let mut o = offset;

    let _block_divider = read_i16(data, o)?; // should be -1
    o += 2;
    let block_id = read_u16(data, o)?;
    o += 2;
    let block_length = read_u32(data, o)?;
    o += 4;
    let num_layers = read_u16(data, o)?;
    o += 2;

    let mut layers = Vec::with_capacity(num_layers as usize);
    for _ in 0..num_layers {
        let _layer_divider = read_i16(data, o)?; // should be -1
        o += 2;
        let layer_length = read_u32(data, o)?;
        o += 4;

        let layer_end = o + layer_length as usize;
        let mut packets = Vec::new();

        while o + 2 <= layer_end && o + 2 <= data.len() {
            let packet_code = read_u16(data, o)?;
            match packet_code {
                // Digital Radial Data Array (packet code 16 = 0xAF1F)
                // Note: the ICD defines this as packet code 16, but the actual
                // on-wire code is 0xAF1F (big-endian representation in the
                // packet header). Some docs refer to it as "AF1F" or 16.
                0xAF1F | 16 => {
                    let (radial_packet, new_offset) =
                        super::radial::decode_radial_packet(data, o)?;
                    packets.push(DataPacket::DigitalRadial(radial_packet));
                    o = new_offset;
                }
                _ => {
                    // Skip unknown packet types by advancing to the end of this layer
                    log::warn!(
                        "Skipping unknown data packet code 0x{:04X} at offset {}",
                        packet_code,
                        o
                    );
                    o = layer_end;
                    break;
                }
            }
        }

        // Ensure we don't fall behind the expected layer end
        o = o.max(layer_end);

        layers.push(DataLayer {
            layer_length,
            packets,
        });
    }

    Ok(SymbologyBlock {
        block_id,
        block_length,
        num_layers,
        layers,
    })
}
