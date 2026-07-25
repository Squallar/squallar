//! Digital radial data array types for Level III products.

/// A decoded Digital Radial Data Array (packet code 16 / 0xAF1F) — the format
/// used by elevation-based products such as N0S and N0K.
#[derive(Debug, Clone)]
pub struct RadialPacket {
    /// Index of the first range bin (distance from radar).
    pub first_range_bin: i16,
    /// Number of range bins (gates) per radial.
    pub num_range_bins: u16,
    /// I-coordinate of the center of sweep (usually 0).
    pub i_center: i16,
    /// J-coordinate of the center of sweep (usually 0).
    pub j_center: i16,
    /// Scale factor (number of pixels per range bin).
    pub scale_factor: f32,
    /// Legacy (0xAF1F) RLE format: 4-bit gate values (0–15) that go through the
    /// PDB's threshold table, not a linear scale/offset.
    pub is_legacy: bool,
    /// From the packet-28 XDR per-radial attributes. When present,
    /// `physical = (gate - xdr_data_offset) / xdr_data_scale`.
    pub xdr_data_scale: Option<f32>,
    /// See [`xdr_data_scale`](Self::xdr_data_scale).
    pub xdr_data_offset: Option<f32>,
    /// The individual radials.
    pub radials: Vec<RadialRun>,
}

/// A single radial within a Digital Radial Data Array.
#[derive(Debug, Clone)]
pub struct RadialRun {
    /// Starting azimuth angle in degrees (0 = north, clockwise).
    pub start_angle: f32,
    /// Azimuth angular width in degrees.
    pub angle_delta: f32,
    /// Raw gate values: 0–255 for 1-byte products, 0–65535 for 2-byte ones
    /// (EET, DPR via packet 28). Meaningless without the PDB's
    /// threshold/scale/offset.
    pub gate_values: Vec<u16>,
}

impl RadialPacket {
    /// Range in km per gate bin: `1.0 / scale_factor`, since the packet header
    /// carries pixels-per-bin at the product's native resolution (typically
    /// 0.25 km or 1.0 km). Falls back to 1.0 km when the factor is ~0.
    pub fn gate_interval_km(&self) -> f64 {
        if self.scale_factor > 0.01 {
            1.0 / self.scale_factor as f64
        } else {
            1.0
        }
    }

    /// Range in km of the first gate's center.
    pub fn first_gate_range_km(&self) -> f64 {
        self.first_range_bin as f64 * self.gate_interval_km()
    }
}
