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

}
