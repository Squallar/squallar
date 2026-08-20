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
    /// Set Colour Level (packet code 0x0802): the contour level that the
    /// contour packets after it are drawn at.
    /// Carried rather than discarded: on a multi-contour product it is the only
    /// thing that tells one contour from the next.
    ContourColour(u16),
    /// Linked Contour Vector (packet code 0x0E03): one polyline, as points.
    LinkedContour(LinkedContourPacket),
}

/// A Linked Contour Vector packet (code 0x0E03): an initial point followed by
/// a chain of points, each joined to the one before.
///
/// ICD 2620001 Figure 3-10: an initial-point indicator halfword (0x8000), the
/// starting `(I, J)`, a byte count, then `(I, J)` pairs, as signed halfwords in
/// **screen units of 1/4 km**. [`points_km`](Self::points_km) is the useful form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedContourPacket {
    /// `(I, J)` in raw screen units, initial point first.
    pub points: Vec<(i16, i16)>,
}

/// A product symbology block's screen coordinates are quarter-kilometres
/// (ICD 2620001 Table II).
pub const SCREEN_UNIT_KM: f64 = 0.25;

impl LinkedContourPacket {
    /// The points as `(east km, north km)` from the radar.
    ///
    /// `+I` is east and **`+J` is north**, so this is a scale and no sign flip.
    /// Getting it backwards is invisible — the two conventions differ only by a
    /// reflection about the east–west axis, so only the per-azimuth assignment
    /// moves. Settled by measurement on a ten-volume twin roster: scored against
    /// the RPG's own `N0H`, `+J` north wins at ten sites of ten, with the margin
    /// tracking how much azimuthal structure the layer has.
    pub fn points_km(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.points
            .iter()
            .map(|&(i, j)| (f64::from(i) * SCREEN_UNIT_KM, f64::from(j) * SCREEN_UNIT_KM))
    }
}
