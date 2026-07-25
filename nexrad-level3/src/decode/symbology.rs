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
                // Legacy Radial Data Array: RLE, halfword sizes, 4-bit gates.
                0xAF1F => {
                    let (radial_packet, new_offset) =
                        super::radial::decode_legacy_radial_packet(data, o)?;
                    packets.push(DataPacket::DigitalRadial(radial_packet));
                    o = new_offset;
                }
                // Digital Radial Data Array: raw 8-bit gates, byte sizes.
                16 => {
                    let (radial_packet, new_offset) =
                        super::radial::decode_digital_radial_packet(data, o)?;
                    packets.push(DataPacket::DigitalRadial(radial_packet));
                    o = new_offset;
                }
                // Generic Data Component: self-describing XDR.
                28 => {
                    match super::radial::decode_generic_radial_packet(data, o) {
                        Ok((radial_packet, new_offset)) => {
                            packets.push(DataPacket::DigitalRadial(radial_packet));
                            o = new_offset;
                        }
                        Err(e) => {
                            log::warn!(
                                "Failed to decode Generic Data Component at offset {}: {}",
                                o, e
                            );
                            o = layer_end;
                            break;
                        }
                    }
                }
                _ => {
                    // No length field to skip by, so abandon the layer.
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

        // A short-reading packet must not leave the next layer misaligned.
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
