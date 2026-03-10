//! Level III message header and Product Description Block types.

/// The 18-byte Level III Message Header (ICD 2620001 Figure 3-3).
#[derive(Debug, Clone, Copy)]
pub struct MessageHeader {
    /// Product message code (identifies the product type). See ICD Table V.
    pub message_code: i16,
    /// Date of message (modified Julian date, days since 1/1/1970).
    pub date_of_message: u16,
    /// Time of message (seconds since midnight UTC).
    pub time_of_message: u32,
    /// Length of the entire message in bytes (including header).
    pub message_length: u32,
    /// Numeric identifier of the source (radar site).
    pub source_id: u16,
    /// Numeric identifier of the destination.
    pub destination_id: u16,
    /// Number of blocks in the message (including header block).
    pub number_of_blocks: u16,
}

/// The 102-byte Product Description Block (ICD 2620001 Figure 3-6).
///
/// Contains radar site location, product parameters, and threshold/scaling
/// information needed to convert raw gate values to physical units.
#[derive(Debug, Clone)]
pub struct ProductDescriptionBlock {
    /// Block divider (always -1).
    pub block_divider: i16,
    /// Radar site latitude in degrees (scaled from millionths).
    pub latitude: f64,
    /// Radar site longitude in degrees (scaled from millionths).
    pub longitude: f64,
    /// Radar site height in feet above MSL.
    pub height: i16,
    /// Product code (same as message header message_code for single-product messages).
    pub product_code: i16,
    /// Operational mode: 0=Maintenance, 1=Clear Air, 2=Precipitation/Severe Weather.
    pub operational_mode: u16,
    /// Volume Coverage Pattern number (e.g. 12, 21, 212, 215).
    pub vcp: u16,
    /// Sequence number.
    pub sequence_number: i16,
    /// Volume scan number.
    pub volume_scan_number: u16,
    /// Volume scan date (modified Julian date).
    pub volume_scan_date: u16,
    /// Volume scan start time (seconds since midnight UTC).
    pub volume_scan_time: u32,
    /// Product generation date (modified Julian date).
    pub generation_date: u16,
    /// Product generation time (seconds since midnight UTC).
    pub generation_time: u32,
    /// Product-specific halfwords 27 and 28 (depend on product type).
    pub product_specific_1: i16,
    /// Product-specific halfword.
    pub product_specific_2: i16,
    /// Elevation number (1-based).
    pub elevation_number: u16,
    /// Product-specific halfword 30 — often data level threshold or offset.
    pub product_specific_3: i16,
    /// 16 data level threshold values (raw halfwords).
    /// For digital products, first two may encode offset/scale.
    pub thresholds: [u16; 16],
    /// Product-specific halfwords 47–53 (7 halfwords, product-dependent).
    pub product_specific_47_53: [i16; 7],
    /// Version number.
    pub version: u8,
    /// Spot blank flag.
    pub spot_blank: u8,
    /// Offset to symbology block (in halfwords from start of product message).
    pub symbology_offset: u32,
    /// Offset to graphic alphanumeric block.
    pub graphic_offset: u32,
    /// Offset to tabular alphanumeric block.
    pub tabular_offset: u32,
}

impl ProductDescriptionBlock {
    /// Compute the linear scale factor for digital product gate values.
    ///
    /// For digital products (codes 94+), the scale is stored in threshold
    /// entries as a big-endian IEEE 754 float spanning halfwords 0–1.
    /// Returns the scale used in: `physical = (gate - offset) / scale`.
    ///
    /// Some products (e.g. 134 DVL, 135 EET) don't use IEEE-float thresholds;
    /// their threshold bytes decode to subnormal/negative garbage.  In that
    /// case the gate values already ARE the physical values, so we return 1.0.
    pub fn data_scale(&self) -> f32 {
        let hw0 = self.thresholds[0];
        let hw1 = self.thresholds[1];
        let bits = ((hw0 as u32) << 16) | (hw1 as u32);
        let val = f32::from_bits(bits);
        // Valid IEEE-float scale must be a positive normal number.
        // Subnormal / inf / nan / zero / negative → identity scale.
        if val.is_normal() && val > 0.0 { val } else { 1.0 }
    }

    /// Compute the linear offset for digital product gate values.
    ///
    /// Same IEEE-float encoding caveat as [`data_scale`](Self::data_scale).
    pub fn data_offset(&self) -> f32 {
        let hw2 = self.thresholds[2];
        let hw3 = self.thresholds[3];
        let bits = ((hw2 as u32) << 16) | (hw3 as u32);
        let val = f32::from_bits(bits);
        // Accept normal floats and exact zero (valid offset).
        // Subnormal / inf / nan → identity offset.
        if val.is_normal() || val == 0.0 { val } else { 0.0 }
    }

    /// The elevation angle in degrees, extracted from product-specific fields.
    /// For elevation-based products, halfword 30 (product_specific_3) contains
    /// the elevation angle scaled by 10.
    pub fn elevation_angle(&self) -> f32 {
        self.product_specific_3 as f32 / 10.0
    }

    /// Build a 256-entry look-up table for Digital VIL (product 134).
    ///
    /// VIL uses a hybrid linear + logarithmic mapping encoded with
    /// NEXRAD-specific 16-bit floats (not IEEE-754).  The first five
    /// thresholds carry: lin_scale, lin_offset, log_start, log_scale,
    /// log_offset.  Gate values 2..log_start use a linear formula;
    /// gate values log_start..254 use an exponential formula.
    ///
    /// Returns `None` when the product code is not 134.
    pub fn build_vil_lut(&self) -> Option<Vec<f32>> {
        if self.product_code != 134 {
            return None;
        }
        let lin_scale = nexrad_float16(self.thresholds[0]);
        let lin_offset = nexrad_float16(self.thresholds[1]);
        let log_start = self.thresholds[2] as usize;
        let log_scale = nexrad_float16(self.thresholds[3]);
        let log_offset = nexrad_float16(self.thresholds[4]);

        let mut lut = vec![f32::NAN; 256];
        // Gate 0 = below threshold, gate 1 = range folded → NaN
        for i in 2..log_start.min(255) {
            lut[i] = (i as f32 - lin_offset) / lin_scale;
        }
        for i in log_start.min(255)..255 {
            lut[i] = ((i as f32 - log_offset) / log_scale).exp();
        }
        // Gate 255 is reserved
        Some(lut)
    }

    /// Decode the 16 legacy data level thresholds into physical values.
    ///
    /// For legacy products (e.g., code 56 SRM), each threshold `u16` encodes
    /// a physical value with flag bits in the high byte and the numeric value
    /// in the low byte. Returns a 16-element array where `NaN` means the
    /// level is not displayable (blank, threshold, no data, or range-folded).
    pub fn decode_legacy_thresholds(&self) -> [f32; 16] {
        let mut lut = [f32::NAN; 16];
        for (i, &t) in self.thresholds.iter().enumerate() {
            let codes = (t >> 8) as u8;
            let mut val = (t & 0xFF) as f32;

            if codes & 0x80 != 0 {
                // Special category: Blank, TH (below threshold),
                // ND (no data), RF (range folded) → not displayable
                continue;
            } else if codes & 0x40 != 0 {
                val *= 0.01;
            } else if codes & 0x20 != 0 {
                val *= 0.05;
            } else if codes & 0x10 != 0 {
                val *= 0.1;
            }

            if codes & 0x01 != 0 {
                val = -val;
            }

            lut[i] = val;
        }
        lut
    }
}

/// Decode a NEXRAD-specific 16-bit floating-point value.
///
/// Format: sign (bit 15), exponent (bits 14–10), fraction (bits 9–0).
/// value = (-1)^sign × 2^(exp − 16) × (1 + frac/1024)  when exp ≠ 0
/// value = (-1)^sign × frac / 512                        when exp = 0
fn nexrad_float16(raw: u16) -> f32 {
    let frac = (raw & 0x03FF) as f32;
    let exp = ((raw >> 10) & 0x1F) as i32;
    let sign = raw >> 15;
    let value = if exp != 0 {
        2f32.powi(exp - 16) * (1.0 + frac / 1024.0)
    } else {
        frac / 512.0
    };
    if sign != 0 { -value } else { value }
}
