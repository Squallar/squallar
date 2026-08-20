//! Decoder for the Symbology Block and its data layers.

use crate::model::{DataLayer, DataPacket, LinkedContourPacket, SymbologyBlock};
use crate::result::{Error, Result};

use super::header::{checked_end, read_i16, read_u16, read_u32};

/// Set Colour Level (packet code 0x0802), ICD 2620001 Figure 3-11a: the code,
/// a `0x0002` value indicator, then the level. Six bytes, fixed. Returns the
/// level and the offset after the packet.
fn decode_contour_colour(data: &[u8], offset: usize) -> Result<(u16, usize)> {
    let indicator = read_u16(data, offset + 2)?;
    if indicator != 0x0002 {
        return Err(Error::InvalidSymbologyBlock(format!(
            "set-colour packet at offset {offset} has value indicator 0x{indicator:04X}, not 0x0002"
        )));
    }
    Ok((read_u16(data, offset + 4)?, offset + 6))
}

/// Linked Contour Vector (packet code 0x0E03), ICD 2620001 Figure 3-10.
///
/// ```text
/// HW 1    Packet code (0x0E03)
/// HW 2    Initial point indicator (0x8000)
/// HW 3    I of the initial point (signed)
/// HW 4    J of the initial point (signed)
/// HW 5    Length of the vectors that follow, in bytes
/// ...     (I, J) pairs, signed halfwords
/// ```
///
/// The length halfword counts only the chained points, not the initial one. A
/// byte count that is not a whole number of `(I, J)` pairs is refused rather
/// than truncated: half-reading it would misalign the rest of the layer.
fn decode_linked_contour(data: &[u8], offset: usize) -> Result<(LinkedContourPacket, usize)> {
    let indicator = read_u16(data, offset + 2)?;
    if indicator != 0x8000 {
        return Err(Error::InvalidSymbologyBlock(format!(
            "linked-contour packet at offset {offset} has initial-point indicator \
             0x{indicator:04X}, not 0x8000"
        )));
    }
    let start = (read_i16(data, offset + 4)?, read_i16(data, offset + 6)?);
    let num_bytes = read_u16(data, offset + 8)? as usize;
    if num_bytes % 4 != 0 {
        return Err(Error::InvalidSymbologyBlock(format!(
            "linked-contour packet at offset {offset} declares {num_bytes} bytes of vectors, \
             which is not a whole number of (I, J) pairs"
        )));
    }
    let body = offset + 10;
    let end = checked_end(data, body, num_bytes)?;
    let mut points = Vec::with_capacity(1 + num_bytes / 4);
    points.push(start);
    for pair in (body..end).step_by(4) {
        points.push((read_i16(data, pair)?, read_i16(data, pair + 2)?));
    }
    Ok((LinkedContourPacket { points }, end))
}

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
pub(crate) fn decode_symbology_block(data: &[u8], offset: usize) -> Result<SymbologyBlock> {
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

        let layer_end = checked_end(data, o, layer_length as usize)?;
        let mut packets = Vec::new();

        while o + 2 <= layer_end && o + 2 <= data.len() {
            let packet_code = read_u16(data, o)?;
            match packet_code {
                0xAF1F => {
                    let (radial_packet, new_offset) =
                        super::radial::decode_legacy_radial_packet(data, o)?;
                    packets.push(DataPacket::DigitalRadial(radial_packet));
                    o = new_offset;
                }
                16 => {
                    let (radial_packet, new_offset) =
                        super::radial::decode_digital_radial_packet(data, o)?;
                    packets.push(DataPacket::DigitalRadial(radial_packet));
                    o = new_offset;
                }
                28 => match super::radial::decode_generic_radial_packet(data, o) {
                    Ok((radial_packet, new_offset)) => {
                        packets.push(DataPacket::DigitalRadial(radial_packet));
                        o = new_offset;
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to decode Generic Data Component at offset {}: {}",
                            o,
                            e
                        );
                        o = layer_end;
                        break;
                    }
                },
                0x0802 => {
                    let (level, new_offset) = decode_contour_colour(data, o)?;
                    packets.push(DataPacket::ContourColour(level));
                    o = new_offset;
                }
                0x0E03 => {
                    let (contour, new_offset) = decode_linked_contour(data, o)?;
                    packets.push(DataPacket::LinkedContour(contour));
                    o = new_offset;
                }
                _ => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn block_of(packets: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&(-1i16).to_be_bytes()); // block divider
        d.extend_from_slice(&1u16.to_be_bytes()); // block id
        d.extend_from_slice(&((packets.len() + 10) as u32).to_be_bytes());
        d.extend_from_slice(&1u16.to_be_bytes()); // one layer
        d.extend_from_slice(&(-1i16).to_be_bytes()); // layer divider
        d.extend_from_slice(&(packets.len() as u32).to_be_bytes());
        d.extend_from_slice(packets);
        d
    }

    fn colour(level: u16) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0x0802u16.to_be_bytes());
        p.extend_from_slice(&0x0002u16.to_be_bytes());
        p.extend_from_slice(&level.to_be_bytes());
        p
    }

    fn contour(start: (i16, i16), chain: &[(i16, i16)]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0x0E03u16.to_be_bytes());
        p.extend_from_slice(&0x8000u16.to_be_bytes());
        p.extend_from_slice(&start.0.to_be_bytes());
        p.extend_from_slice(&start.1.to_be_bytes());
        p.extend_from_slice(&((chain.len() * 4) as u16).to_be_bytes());
        for (i, j) in chain {
            p.extend_from_slice(&i.to_be_bytes());
            p.extend_from_slice(&j.to_be_bytes());
        }
        p
    }

    /// The two packets the Melting Layer product is drawn from, decoded off
    /// bytes laid out by hand from the ICD figures.
    #[test]
    fn a_colour_and_contour_pair_decodes_to_its_level_and_its_points() {
        let mut packets = colour(3);
        packets.extend(contour((40, 0), &[(0, 40), (-40, 0), (0, -40), (40, 0)]));
        let Ok(block) = decode_symbology_block(&block_of(&packets), 0) else {
            panic!("the hand-laid block should decode")
        };

        assert_eq!(block.layers.len(), 1);
        assert_eq!(block.layers[0].packets.len(), 2, "both packets are kept");

        let DataPacket::ContourColour(level) = block.layers[0].packets[0] else {
            panic!("first packet should be the colour level");
        };
        assert_eq!(level, 3);

        let DataPacket::LinkedContour(ref c) = block.layers[0].packets[1] else {
            panic!("second packet should be the contour");
        };
        assert_eq!(
            c.points,
            vec![(40, 0), (0, 40), (-40, 0), (0, -40), (40, 0)],
            "the initial point leads the chain",
        );
        // 1/4 km screen units, +I east and +J north: a 40-unit ring is 10 km.
        let km: Vec<(f64, f64)> = c.points_km().collect();
        assert_eq!(km[0], (10.0, 0.0), "due east");
        assert_eq!(km[1], (0.0, 10.0), "due north");
        assert_eq!(km[2], (-10.0, 0.0), "due west");
        assert_eq!(km[3], (0.0, -10.0), "due south");
    }

    /// A second contour has to start where the first ended.
    #[test]
    fn contours_after_the_first_are_found_at_the_right_offset() {
        let mut packets = colour(1);
        packets.extend(contour((8, 0), &[(0, 8)]));
        packets.extend(colour(2));
        packets.extend(contour((16, 0), &[(0, 16), (-16, 0)]));
        let Ok(block) = decode_symbology_block(&block_of(&packets), 0) else {
            panic!("the hand-laid block should decode")
        };

        let contours: Vec<&crate::model::LinkedContourPacket> = block.layers[0]
            .packets
            .iter()
            .filter_map(|p| match p {
                DataPacket::LinkedContour(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(contours.len(), 2);
        assert_eq!(contours[0].points, vec![(8, 0), (0, 8)]);
        assert_eq!(contours[1].points, vec![(16, 0), (0, 16), (-16, 0)]);
        let levels: Vec<u16> = block.layers[0]
            .packets
            .iter()
            .filter_map(|p| match p {
                DataPacket::ContourColour(l) => Some(*l),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![1, 2]);
    }

    /// The two indicator halfwords are the only thing that says a packet is the
    /// packet its code claims.
    #[test]
    fn a_wrong_indicator_halfword_is_refused() {
        let mut bad_contour = contour((8, 0), &[(0, 8)]);
        bad_contour[2..4].copy_from_slice(&0x4000u16.to_be_bytes());
        assert!(decode_symbology_block(&block_of(&bad_contour), 0).is_err());

        let mut bad_colour = colour(1);
        bad_colour[2..4].copy_from_slice(&0x0003u16.to_be_bytes());
        assert!(decode_symbology_block(&block_of(&bad_colour), 0).is_err());
    }

    /// A byte count that is not a whole number of `(I, J)` pairs cannot be this
    /// packet.
    #[test]
    fn a_contour_byte_count_that_is_not_whole_pairs_is_refused() {
        let mut odd = contour((8, 0), &[(0, 8)]);
        odd[8..10].copy_from_slice(&6u16.to_be_bytes());
        assert!(decode_symbology_block(&block_of(&odd), 0).is_err());
    }

    /// A one-layer block whose declared layer length is `u32::MAX`: adding it
    /// to the running offset overflows a 32-bit `usize`. Either way an error,
    /// never a panic.
    #[test]
    fn a_layer_length_that_overflows_the_offset_is_an_error() {
        let mut d = Vec::new();
        d.extend_from_slice(&(-1i16).to_be_bytes()); // block divider
        d.extend_from_slice(&1u16.to_be_bytes()); // block id
        d.extend_from_slice(&16u32.to_be_bytes()); // block length
        d.extend_from_slice(&1u16.to_be_bytes()); // one layer
        d.extend_from_slice(&(-1i16).to_be_bytes()); // layer divider
        d.extend_from_slice(&u32::MAX.to_be_bytes()); // layer length
        d.extend_from_slice(&16u16.to_be_bytes()); // digital radial packet…
        // …with nothing after it, so the 64-bit path errors on truncation.
        assert!(decode_symbology_block(&d, 0).is_err());
    }
}
