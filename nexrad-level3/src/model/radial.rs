//! Digital radial data array types for Level III products.

/// A decoded Digital Radial Data Array (packet code 16 / 0xAF1F).
///
/// This is the primary data format for elevation-based Level III products
/// such as Storm-Relative Velocity (N0S) and Specific Differential Phase (N0K).
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
    /// Whether this packet was decoded from a legacy (0xAF1F) RLE format.
    /// Legacy packets have 4-bit gate values (0–15) that must be mapped through
    /// the PDB's threshold table, rather than using linear scale/offset.
    pub is_legacy: bool,
    /// Data value scale factor extracted from XDR per-radial attributes (packet 28).
    /// When present, physical_value = (gate_value - xdr_data_offset) / xdr_data_scale.
    pub xdr_data_scale: Option<f32>,
    /// Data value offset extracted from XDR per-radial attributes (packet 28).
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
    /// Raw gate values. For 1-byte products values are 0–255; for 2-byte
    /// products (e.g. EET, DPR via packet code 28) values are 0–65535.
    /// Interpretation depends on product type and the PDB's
    /// threshold/scale/offset values.
    pub gate_values: Vec<u16>,
}

impl RadialPacket {
    /// Range in km of each gate bin.
    ///
    /// Digital radial products typically encode bins at 0.25 km or 1.0 km spacing.
    /// The scale_factor from the packet header encodes pixels-per-bin at the
    /// product's native resolution.
    ///
    /// For standard 230 km range products at 0.25 km resolution:
    /// `gate_interval_km ≈ 1.0 / scale_factor` when scale_factor represents
    /// pixels per range-bin at the product's configured resolution.
    ///
    /// If scale_factor is 0 or very small, falls back to 1.0 km default.
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
