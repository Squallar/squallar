//! Data model types for NEXRAD Level III products.

mod header;
mod radial;
mod raster;
mod symbology;

pub use header::*;
pub use radial::*;
pub use raster::*;
pub use symbology::*;

/// A fully decoded Level III product message.
#[derive(Debug, Clone)]
pub struct Level3Message {
    /// The 18-byte message header.
    pub header: MessageHeader,
    /// The 102-byte Product Description Block.
    pub pdb: ProductDescriptionBlock,
    /// Decoded symbology data (display layers).
    pub symbology: Option<SymbologyBlock>,
}

/// **What a decoded product costs on the host heap**, in the shape it is
/// actually held in rather than in the shape the ICD describes.
///
/// It walks the decoded structure, so it needs no geometry table and makes no
/// assumption about radial or gate counts — which matters, because **no
/// fixture in this tree carries a real product's shape**: the only decoded
/// sizes any test here pins are a one-radial, two-bin packet. An estimate
/// from ICD 2620001 (N0K 720x1200, DPR 360x920, DVL and EET 360x460, about
/// 3.0 MiB for one site's four products) is what motivated the figure; it is
/// not what the figure is.
///
/// **`capacity`, not `len`.** The allocator granted the capacity and
/// `squallar_alloc::live_bytes` counts what it granted, so a decode that
/// over-reserved is priced at what it took.
impl Level3Message {
    /// **What this message OWNS off the heap**, and not the struct itself:
    /// the header and the product description block are fixed-size and
    /// inline, so an owner that already counted `size_of::<Level3Message>()`
    /// as part of its own struct would double-count them. Everything that can
    /// be large hangs off the symbology block.
    pub fn resident_bytes(&self) -> usize {
        self.symbology
            .as_ref()
            .map_or(0, SymbologyBlock::resident_bytes)
    }
}

impl SymbologyBlock {
    /// What this block owns off the heap: its layer vector and every layer's
    /// own allocations. Capacity, not length.
    pub fn resident_bytes(&self) -> usize {
        self.layers.capacity() * core::mem::size_of::<DataLayer>()
            + self
                .layers
                .iter()
                .map(DataLayer::resident_bytes)
                .sum::<usize>()
    }
}

impl DataLayer {
    /// What this layer owns off the heap: its packet vector and every packet's
    /// own allocations. Capacity, not length.
    pub fn resident_bytes(&self) -> usize {
        self.packets.capacity() * core::mem::size_of::<DataPacket>()
            + self
                .packets
                .iter()
                .map(DataPacket::resident_bytes)
                .sum::<usize>()
    }
}

impl DataPacket {
    /// **The two zero arms are structural, not "nothing measured yet".**
    /// [`DataPacket::ContourColour`] is a `u16` inline in the enum, and
    /// [`RasterPacket`] is a `_private: ()` stub with no field to hold
    /// anything — both are zero because the type cannot own a byte, which is
    /// the only kind of zero that stays true without a test watching it.
    /// **Phase 2 fills the raster stub in**, and the arm below has to move
    /// when it does; `a_raster_packet_holds_nothing_yet` fails if it grows a
    /// field and this is left alone.
    pub fn resident_bytes(&self) -> usize {
        match self {
            Self::DigitalRadial(packet) => packet.resident_bytes(),
            Self::LinkedContour(packet) => packet.resident_bytes(),
            Self::Raster(_) | Self::ContourColour(_) => 0,
        }
    }
}

impl RadialPacket {
    /// The gates dominate: one `Vec<u16>` per radial.
    ///
    /// **Two bytes a gate whatever the product**, including the 8-bit digital
    /// products (`N0K`, `EET`, `DVL`, `DPR`) whose values are 0-255 — the
    /// decoder widens them on the way in. That doubling is real and priced
    /// here rather than hidden; halving it is a separate change with its own
    /// evidence to gather.
    pub fn resident_bytes(&self) -> usize {
        self.radials.capacity() * core::mem::size_of::<RadialRun>()
            + self
                .radials
                .iter()
                .map(|radial| radial.gate_values.capacity() * core::mem::size_of::<u16>())
                .sum::<usize>()
    }
}

impl LinkedContourPacket {
    /// The point chain: four bytes a point, at capacity.
    pub fn resident_bytes(&self) -> usize {
        self.points.capacity() * core::mem::size_of::<(i16, i16)>()
    }
}

#[cfg(test)]
mod resident_bytes_tests {
    use super::*;

    /// A decoded product's cost rises with the gates it decoded, and by the
    /// two bytes a gate it actually holds.
    #[test]
    fn the_gates_dominate_and_are_priced_at_two_bytes_each() {
        let radials = |count: usize, gates: usize| RadialPacket {
            first_range_bin: 0,
            num_range_bins: gates as u16,
            i_center: 0,
            j_center: 0,
            scale_factor: 1.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: (0..count)
                .map(|i| RadialRun {
                    start_angle: i as f32,
                    angle_delta: 1.0,
                    gate_values: vec![0u16; gates],
                })
                .collect(),
        };
        let small = radials(360, 460);
        let large = radials(360, 920);
        assert_eq!(
            large.resident_bytes() - small.resident_bytes(),
            360 * 460 * 2,
            "doubling the gates did not cost two bytes a gate"
        );
        // And the radial runs themselves are in it, not only their gates.
        assert!(
            small.resident_bytes() > 360 * 460 * 2,
            "the per-radial struct is unpriced"
        );
    }

    /// **The case where a null must be believable.** The raster arm answers
    /// zero because the type holds nothing; if it ever grows a field, zero
    /// stops being structural and this says so.
    #[test]
    fn a_raster_packet_holds_nothing_yet() {
        assert_eq!(
            core::mem::size_of::<RasterPacket>(),
            0,
            "RasterPacket grew a field; `DataPacket::resident_bytes` still \
             answers 0 for it and now under-counts every raster product"
        );
    }

    /// An empty symbology block costs nothing off the heap — a real zero, and
    /// the arm a message carrying no layers takes.
    ///
    /// Stated on the block rather than on a whole [`Level3Message`] because
    /// neither the header nor the product description block is
    /// constructible without a decode, and a test that needed one would be
    /// pinning the decoder rather than this arithmetic.
    #[test]
    fn an_empty_symbology_block_holds_nothing() {
        let empty = SymbologyBlock {
            block_id: 1,
            block_length: 0,
            num_layers: 0,
            layers: Vec::new(),
        };
        assert_eq!(empty.resident_bytes(), 0);
    }
}
