//! Level III data-level codecs.
//!
//! The decode direction is what [`crate::render`] has always done to draw a
//! Level III product: PDB thresholds (legacy 4-bit and Digital VIL's NEXRAD
//! float16 hybrid) or a linear scale/offset turn gate levels into physical
//! values. The encode direction is new: the validation harnesses
//! ([`crate::twin`]) need to push a *derived* physical value back through the
//! product's own encoding to compare in data levels, so every decoding here
//! has its inverse alongside.

use crate::types::RadarProduct;
use nexrad_level3::model::ProductDescriptionBlock;

/// Build a 256-entry look-up table for Digital VIL (product 134), `None` for
/// anything else.
///
/// VIL is a hybrid linear + logarithmic mapping encoded in NEXRAD 16-bit floats
/// (not IEEE-754). Thresholds 0..5 carry lin_scale, lin_offset, log_start,
/// log_scale, log_offset; gates 2..log_start are linear, log_start..254
/// exponential.
pub(crate) fn build_vil_lut(pdb: &ProductDescriptionBlock) -> Option<Vec<f32>> {
    if pdb.product_code != 134 {
        return None;
    }
    let lin_scale = nexrad_float16(pdb.thresholds[0]);
    let lin_offset = nexrad_float16(pdb.thresholds[1]);
    let log_start = pdb.thresholds[2] as usize;
    let log_scale = nexrad_float16(pdb.thresholds[3]);
    let log_offset = nexrad_float16(pdb.thresholds[4]);

    let mut lut = vec![f32::NAN; 256];
    // Gate 0 = below threshold, gate 1 = range folded → NaN
    for (i, slot) in lut.iter_mut().enumerate().take(log_start.min(255)).skip(2) {
        *slot = (i as f32 - lin_offset) / lin_scale;
    }
    for (i, slot) in lut
        .iter_mut()
        .enumerate()
        .take(255)
        .skip(log_start.min(255))
    {
        *slot = ((i as f32 - log_offset) / log_scale).exp();
    }
    // Gate 255 is reserved
    Some(lut)
}

/// Decode the 16 legacy data level thresholds into physical values.
///
/// For legacy products (e.g. code 56 SRM) each threshold `u16` carries flag
/// bits in the high byte and the value in the low byte. `NaN` marks a level
/// that is not displayable.
pub(crate) fn decode_legacy_thresholds(pdb: &ProductDescriptionBlock) -> [f32; 16] {
    let mut lut = [f32::NAN; 16];
    for (i, &t) in pdb.thresholds.iter().enumerate() {
        let codes = (t >> 8) as u8;
        let mut val = (t & 0xFF) as f32;

        if codes & 0x80 != 0 {
            // Blank, TH (below threshold), ND (no data) or RF (range folded).
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

/// Decode a NEXRAD-specific 16-bit float: sign (bit 15), exponent (14–10),
/// fraction (9–0).
/// `value = (-1)^sign × 2^(exp − 16) × (1 + frac/1024)` when exp ≠ 0,
/// `value = (-1)^sign × frac / 512` when exp = 0.
pub(crate) fn nexrad_float16(raw: u16) -> f32 {
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

/// Level III gate byte to physical value, via LUT or scale/offset. SRV is
/// converted knots → m/s.
pub(crate) fn l3_physical_value(
    gate_value: u16,
    product: RadarProduct,
    scale: f32,
    offset: f32,
    lut: Option<&[f32]>,
) -> f32 {
    let v = if let Some(table) = lut {
        let idx = gate_value as usize;
        if idx < table.len() {
            table[idx]
        } else {
            f32::NAN
        }
    } else {
        (gate_value as f32 - offset) / scale
    };
    if matches!(product, RadarProduct::StormRelativeVelocity) {
        v * 0.514444
    } else {
        v
    }
}

/// The inverse of a LUT decoding: the level whose physical value sits nearest
/// `value_phys`, ties to the lower level. `NaN` — undefined, and the LUT's own
/// `NaN` levels (below threshold, range folded, reserved) — encodes as 0.
///
/// A strictly monotone LUT makes decode → encode the identity on every defined
/// level, which is what the round-trip tests below pin.
pub(crate) fn quantize_via_lut(value_phys: f32, lut: &[f32]) -> u8 {
    if value_phys.is_nan() {
        return 0;
    }
    let mut best: Option<(usize, f64)> = None;
    for (i, &level) in lut.iter().enumerate().take(256) {
        if level.is_nan() {
            continue;
        }
        // f64: the VIL log branch tops out near 10⁶, where an f32 subtraction
        // rounds hard enough to tie levels that are 30 apart.
        let d = (level as f64 - value_phys as f64).abs();
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map_or(0, |(i, _)| i as u8)
}

/// The inverse of the linear `physical = (gate − offset) / scale` decoding the
/// digital products use (packet-28 XDR attributes, or the PDB's IEEE-float /
/// min-increment pairs): the nearest level, clamped into the data range.
/// Levels 0 and 1 are below-threshold and range-folded across the digital
/// family, so a defined value never encodes below 2; `NaN` encodes as 0.
pub(crate) fn quantize_scaled(value: f32, scale: f32, offset: f32) -> u8 {
    if value.is_nan() {
        return 0;
    }
    (value * scale + offset).round().clamp(2.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDB carrying only what these codecs read: the product code and the
    /// threshold halfwords.
    fn pdb(product_code: i16, thresholds: [u16; 16]) -> ProductDescriptionBlock {
        ProductDescriptionBlock {
            block_divider: -1,
            latitude: 35.333,
            longitude: -97.278,
            height: 1200,
            product_code,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 1,
            volume_scan_date: 20661,
            volume_scan_time: 7108,
            generation_date: 20661,
            generation_time: 7200,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number: 1,
            product_specific_3: 5,
            thresholds,
            product_specific_47_53: [0; 7],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        }
    }

    /// Synthetic DVL thresholds in NEXRAD float16: lin_scale 2.0 (0x4400),
    /// lin_offset −2.0 (0xC400), log_start 100, log_scale 16.0 (0x5000),
    /// log_offset 36.0 (0x5480). Linear levels run (i + 2)/2 = 2.0 … 50.5 and
    /// the log branch takes over at e⁴ ≈ 54.6, so the whole table is strictly
    /// increasing — the property the round trip depends on.
    fn dvl_thresholds() -> [u16; 16] {
        let mut t = [0u16; 16];
        t[0] = 0x4400;
        t[1] = 0xC400;
        t[2] = 100;
        t[3] = 0x5000;
        t[4] = 0x5480;
        t
    }

    #[test]
    fn nexrad_float16_decodes_sign_exponent_and_fraction() {
        assert_eq!(nexrad_float16(0x4400), 2.0, "2^(17-16) × 1.0");
        assert_eq!(nexrad_float16(0xC400), -2.0, "the sign bit negates");
        assert_eq!(nexrad_float16(0x5000), 16.0, "2^(20-16)");
        assert_eq!(nexrad_float16(0x5480), 36.0, "2^5 × (1 + 128/1024)");
        // The denormal path: exp 0 reads frac/512.
        assert_eq!(nexrad_float16(256), 0.5);
        assert_eq!(nexrad_float16(0x8000 | 256), -0.5);
        assert_eq!(nexrad_float16(0), 0.0);
    }

    #[test]
    fn the_vil_lut_is_hybrid_linear_then_log_and_guards_its_ends() {
        let lut = build_vil_lut(&pdb(134, dvl_thresholds())).expect("134 is DVL");
        assert_eq!(lut.len(), 256);
        assert!(lut[0].is_nan(), "level 0 is below threshold");
        assert!(lut[1].is_nan(), "level 1 is range folded");
        assert!(lut[255].is_nan(), "level 255 is reserved");
        assert_eq!(lut[2], 2.0, "(2 + 2)/2");
        assert_eq!(lut[99], 50.5, "the last linear level");
        assert!((lut[100] - 4f32.exp()).abs() < 1e-3, "the first log level");
        // Strictly increasing across the junction and everywhere else.
        for i in 3..255 {
            assert!(lut[i] > lut[i - 1], "lut[{i}] regressed");
        }
        // Any other product code has no VIL table.
        assert!(build_vil_lut(&pdb(135, dvl_thresholds())).is_none());
    }

    /// The inverse the DVL harness will lean on: every displayable level
    /// survives decode → encode exactly.
    #[test]
    fn every_vil_level_round_trips_through_the_lut() {
        let lut = build_vil_lut(&pdb(134, dvl_thresholds())).expect("134 is DVL");
        for level in 2..=254u16 {
            let physical = l3_physical_value(
                level,
                RadarProduct::VerticallyIntegratedLiquid,
                1.0,
                0.0,
                Some(&lut),
            );
            assert!(!physical.is_nan(), "level {level} decoded to NaN");
            assert_eq!(
                quantize_via_lut(physical, &lut),
                level as u8,
                "level {level} ({physical} kg/m²) did not round-trip",
            );
        }
    }

    #[test]
    fn legacy_thresholds_decode_their_flag_bits() {
        let mut t = [0x8000u16; 16]; // flagged: not displayable
        t[1] = 0x0005; // plain 5
        t[2] = 0x4005; // ×0.01 → 0.05
        t[3] = 0x2005; // ×0.05 → 0.25
        t[4] = 0x1005; // ×0.1 → 0.5
        t[5] = 0x0105; // negated → −5
        t[6] = 0x1105; // ×0.1, negated → −0.5
        let lut = decode_legacy_thresholds(&pdb(56, t));
        assert!(lut[0].is_nan());
        assert_eq!(lut[1], 5.0);
        assert!((lut[2] - 0.05).abs() < 1e-6);
        assert!((lut[3] - 0.25).abs() < 1e-6);
        assert!((lut[4] - 0.5).abs() < 1e-6);
        assert_eq!(lut[5], -5.0);
        assert!((lut[6] + 0.5).abs() < 1e-6);
        assert!(lut[7..].iter().all(|v| v.is_nan()));
    }

    /// The 16-level legacy table round-trips too — the SRM product 56 harness
    /// compares in these levels.
    #[test]
    fn every_defined_legacy_level_round_trips_through_the_lut() {
        // A realistic velocity ladder: RF/ND flagged, the rest distinct.
        let t: [u16; 16] = [
            0x8000, 0x0140, 0x0132, 0x0124, 0x011A, 0x010A, 0x0101, 0x0000, 0x0001, 0x000A, 0x001A,
            0x0024, 0x0032, 0x0040, 0x004A, 0x8001,
        ];
        let lut = decode_legacy_thresholds(&pdb(56, t));
        for (i, &v) in lut.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            assert_eq!(
                quantize_via_lut(v, &lut) as usize,
                i,
                "legacy level {i} ({v}) did not round-trip",
            );
        }
    }

    #[test]
    fn quantize_via_lut_maps_nan_to_zero_and_picks_the_nearest_level() {
        let lut = [f32::NAN, f32::NAN, 10.0, 20.0, 30.0];
        assert_eq!(quantize_via_lut(f32::NAN, &lut), 0);
        assert_eq!(quantize_via_lut(10.0, &lut), 2);
        assert_eq!(quantize_via_lut(14.9, &lut), 2);
        assert_eq!(
            quantize_via_lut(15.0, &lut),
            2,
            "a tie goes to the lower level"
        );
        assert_eq!(quantize_via_lut(15.1, &lut), 3);
        assert_eq!(quantize_via_lut(1e9, &lut), 4, "clamped to the top level");
        assert_eq!(
            quantize_via_lut(-1e9, &lut),
            2,
            "clamped to the bottom level"
        );
        assert_eq!(quantize_via_lut(5.0, &[f32::NAN; 4]), 0, "an all-NaN table");
    }

    /// Scaled encodings across the conventions the products actually use:
    /// N0K's KDP (scale 20, offset 43), the classic reflectivity pair
    /// (2, 66), and the velocity pair (2, 129).
    #[test]
    fn every_scaled_level_round_trips_for_the_shipping_conventions() {
        for &(scale, offset) in &[(20.0f32, 43.0f32), (2.0, 66.0), (2.0, 129.0), (10.0, 0.0)] {
            for level in 2..=254u16 {
                let physical =
                    l3_physical_value(level, RadarProduct::EchoTops, scale, offset, None);
                assert_eq!(
                    quantize_scaled(physical, scale, offset),
                    level as u8,
                    "level {level} did not round-trip at scale {scale} offset {offset}",
                );
            }
        }
    }

    #[test]
    fn quantize_scaled_maps_nan_to_zero_and_clamps_into_the_data_range() {
        assert_eq!(quantize_scaled(f32::NAN, 20.0, 43.0), 0);
        // (0.5·20 + 43) = 53.
        assert_eq!(quantize_scaled(0.5, 20.0, 43.0), 53);
        // Way below the range: never lands on the reserved 0/1 levels.
        assert_eq!(quantize_scaled(-1e6, 20.0, 43.0), 2);
        assert_eq!(quantize_scaled(1e6, 20.0, 43.0), 255);
    }

    /// The one product-dependent decode: SRV converts knots to m/s. Everything
    /// else passes through.
    #[test]
    fn l3_physical_value_converts_only_srv_and_guards_the_lut_bounds() {
        assert_eq!(
            l3_physical_value(100, RadarProduct::EchoTops, 2.0, 66.0, None),
            17.0,
        );
        let srv = l3_physical_value(100, RadarProduct::StormRelativeVelocity, 2.0, 66.0, None);
        assert!((srv - 17.0 * 0.514444).abs() < 1e-5);
        // An index past the table is undefined, not a panic.
        let lut = [1.0f32, 2.0];
        assert!(l3_physical_value(2, RadarProduct::EchoTops, 1.0, 0.0, Some(&lut)).is_nan());
        assert_eq!(
            l3_physical_value(1, RadarProduct::EchoTops, 1.0, 0.0, Some(&lut)),
            2.0,
        );
    }
}
