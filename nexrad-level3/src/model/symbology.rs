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
    ///
    /// Carried rather than discarded because on a multi-contour product it is
    /// the only thing that tells one contour from the next. The Melting Layer
    /// product (166) draws four and numbers them 1–4 with these.
    ContourColour(u16),
    /// Linked Contour Vector (packet code 0x0E03): one polyline, as points.
    LinkedContour(LinkedContourPacket),
}

/// A Linked Contour Vector packet (code 0x0E03): an initial point followed by
/// a chain of points, each joined to the one before.
///
/// ICD 2620001 Figure 3-10: an initial-point indicator halfword (0x8000), the
/// starting `(I, J)`, a byte count, then `(I, J)` pairs. The coordinates are
/// signed halfwords in **screen units of 1/4 km** for a packet inside the
/// product symbology block, so [`points_km`](Self::points_km) is the form
/// every consumer wants and the raw units are kept only because a consumer
/// that wants to re-encode would need them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedContourPacket {
    /// `(I, J)` in raw screen units, initial point first.
    pub points: Vec<(i16, i16)>,
}

/// A product symbology block's screen coordinates are quarter-kilometres
/// (ICD 2620001 Table II: "1/4 km resolution" for the symbology block's
/// linked-vector family).
pub const SCREEN_UNIT_KM: f64 = 0.25;

impl LinkedContourPacket {
    /// The points as `(east km, north km)` from the radar.
    ///
    /// # Which way `J` points, and how that was settled
    ///
    /// `+I` is east and **`+J` is north**, so this is a scale and no sign
    /// flip. That is worth stating because the obvious reading of the ICD's
    /// "screen coordinates" is the other one — display rows increase
    /// downward — and getting it backwards is invisible: the ring radii, the
    /// recovered layer depth and every self-consistency check come out
    /// **identical** either way, because the two conventions differ only by a
    /// reflection about the east–west axis. Only the per-azimuth assignment
    /// moves.
    ///
    /// Settled by measurement, on the ten-volume twin roster (ten sites, four
    /// VCPs, six regimes, two holdouts). Classifying each volume against its
    /// own melting layer recovered both ways and scoring against the RPG's
    /// own `N0H`, `+J` north wins at **ten sites of ten**, never loses, and
    /// the margin tracks how much azimuthal structure the layer has: +4.82
    /// points at KMSX, +1.45 at KFTG, +1.30 at KDMX — the three volumes whose
    /// layer varies most around the circle — against +0.00 at KBUF and KARX,
    /// whose layer is flat at ground and where the two conventions are the
    /// same thing. A convention chosen by fitting would not have that shape.
    ///
    /// An independent physical check was attempted and **did not decide it**:
    /// correlating the recovered per-azimuth top against the height of each
    /// azimuth's minimum-ρhv gate — the bright band, from the volume's own
    /// Level II data and owing nothing to the RPG — favoured north at KFTG
    /// (r +0.38 vs −0.31) and KDMX (+0.06 vs −0.61) and south at KMSX by a
    /// margin inside its own noise (−0.30 vs −0.20, RMS 0.264 vs 0.260 km).
    /// In deep convection that proxy tracks hail cores as readily as melting
    /// snow, and the other seven volumes had too little bright band or too
    /// flat a layer to score at all. Recorded because a null result that
    /// *fails to contradict* is worth more than an unrun check.
    pub fn points_km(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.points
            .iter()
            .map(|&(i, j)| (f64::from(i) * SCREEN_UNIT_KM, f64::from(j) * SCREEN_UNIT_KM))
    }
}
