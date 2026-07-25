//! Symbology block types for Level III product display data.

/// The data-carrying block of a Level III message: layers of data packets
/// (radial arrays, raster grids, text, vectors).
#[derive(Debug, Clone)]
pub struct SymbologyBlock {
    /// Block ID (always 1 for symbology).
    pub block_id: u16,
    /// Total length of the block in bytes (excluding the block-divider/ID/length header).
    pub block_length: u32,
    /// Number of data layers.
    pub num_layers: u16,
    /// The data layers.
    pub layers: Vec<DataLayer>,
}

/// A single display data layer within the symbology block.
#[derive(Debug, Clone)]
pub struct DataLayer {
    /// Length of this layer's data in bytes.
    pub layer_length: u32,
    /// The data packets contained in this layer.
    pub packets: Vec<DataPacket>,
}

/// A decoded data packet from a symbology layer.
#[derive(Debug, Clone)]
pub enum DataPacket {
    /// Digital Radial Data Array (packet code 16 / 0xAF1F).
    DigitalRadial(super::RadialPacket),
    /// Raster data (stub for Phase 2).
    Raster(super::RasterPacket),
}
