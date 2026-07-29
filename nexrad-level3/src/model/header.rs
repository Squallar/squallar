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
#[derive(Debug, Clone)]
pub struct ProductDescriptionBlock {
    /// Block divider (always -1).
    pub block_divider: i16,
    /// Radar site latitude in degrees (scaled from thousandths).
    pub latitude: f64,
    /// Radar site longitude in degrees (scaled from thousandths).
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

/// The storm motion vector the RPG's SCIT algorithm used to compute a
/// storm-relative product, read from the Product Description Block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StormMotion {
    /// Halfword 51 ÷ 10, in knots.
    pub speed_kt: f32,
    /// Halfword 52 ÷ 10, in degrees. Meteorological convention — the direction
    /// the storm is coming *from*, which is why the radial correction adds
    /// rather than subtracts. See [`StormMotion::radial_component_kt`].
    pub direction_deg: f32,
    /// Halfword 49 is -1 when the vector is the SCIT cell average, 0 when the
    /// operator entered it by hand.
    pub is_scit_average: bool,
}

impl StormMotion {
    /// The vector's component along the outbound radial at `azimuth_deg`, in
    /// knots. `SRM = V + radial_component_kt(az)`.
    ///
    /// The sign is empirical: over a million gates from 19 sites it reproduces
    /// the RPG's own product 56 for 92.7% of gates exactly, against 3.9% for
    /// the opposite sign. It is a `+` because halfword 52 is the direction the
    /// storm comes *from*, not the one it moves toward.
    ///
    /// Cosine is even, so `direction − azimuth` and `azimuth − direction` are
    /// **provably** equal — not merely untested. A mutation run reports the
    /// swap as a surviving mutant; it is an equivalent mutant, and no test can
    /// kill it. Do not "fix" the order.
    pub fn radial_component_kt(&self, azimuth_deg: f64) -> f64 {
        let phase = (self.direction_deg as f64 - azimuth_deg).to_radians();
        self.speed_kt as f64 * phase.cos()
    }
}

impl ProductDescriptionBlock {
    /// `(minimum, increment)` for the products that encode their data levels as
    /// halfword 31 = min×10, halfword 32 = increment×10, halfword 33 = level
    /// count, rather than as an IEEE float pair.
    ///
    /// Both halfwords are **signed** — 99's minimum is -635 — so the `i16` cast
    /// is load-bearing: read as `u16` it becomes 6490.1.
    fn min_increment(&self) -> Option<(f32, f32)> {
        if !matches!(self.product_code, 93 | 99 | 154 | 155) {
            return None;
        }
        let increment = self.thresholds[1] as i16 as f32 / 10.0;
        if increment <= 0.0 {
            return None;
        }
        Some((self.thresholds[0] as i16 as f32 / 10.0, increment))
    }

    /// Number of data levels (halfword 33) for the min/increment products.
    pub fn data_levels(&self) -> Option<u16> {
        self.min_increment().map(|_| self.thresholds[2])
    }

    /// The first gate value carrying data. Levels 0 and 1 are "below threshold"
    /// and "range folded" for every product in the min/increment family.
    pub const FIRST_DATA_LEVEL: f32 = 2.0;

    /// Scale in `physical = (gate - offset) / scale`, stored by digital
    /// products (codes 94+) as a big-endian IEEE 754 float over thresholds 0–1.
    ///
    /// Products such as 134 DVL and 135 EET do not use IEEE-float thresholds —
    /// theirs decode to subnormal/negative garbage. Their gate values already
    /// ARE physical, hence the 1.0 fallback.
    pub fn data_scale(&self) -> f32 {
        if let Some((_, increment)) = self.min_increment() {
            return 1.0 / increment;
        }
        let hw0 = self.thresholds[0];
        let hw1 = self.thresholds[1];
        let bits = ((hw0 as u32) << 16) | (hw1 as u32);
        let val = f32::from_bits(bits);
        if val.is_normal() && val > 0.0 {
            val
        } else {
            1.0
        }
    }

    /// Offset from thresholds 2–3. Same IEEE-float caveat as
    /// [`data_scale`](Self::data_scale).
    pub fn data_offset(&self) -> f32 {
        if let Some((minimum, increment)) = self.min_increment() {
            // physical = min + (gate - 2)·inc must equal (gate - offset)/scale
            // with scale = 1/inc.
            return Self::FIRST_DATA_LEVEL - minimum / increment;
        }
        let hw2 = self.thresholds[2];
        let hw3 = self.thresholds[3];
        let bits = ((hw2 as u32) << 16) | (hw3 as u32);
        let val = f32::from_bits(bits);
        // Exact zero is a legitimate offset; subnormal/inf/nan are not.
        if val.is_normal() || val == 0.0 {
            val
        } else {
            0.0
        }
    }

    /// Elevation-based products put the angle in halfword 30
    /// (`product_specific_3`), scaled by 10.
    pub fn elevation_angle(&self) -> f32 {
        self.product_specific_3 as f32 / 10.0
    }

    /// Range spacing per gate, in km, for the products whose packet-16 scale
    /// factor halfword cannot supply it.
    ///
    /// That halfword reads 999 for the 1 km product 56 and for the 0.25 km
    /// products 99 and 154 alike, so `RadialPacket::gate_interval_km` returns
    /// ~1 km for all three and is 4× wrong for the velocity products. `None`
    /// means the packet's own value is usable.
    ///
    /// Product 163 (Digital Specific Differential Phase) lies the same way:
    /// its generator (`dualpol8bit.c` in the ORPG source) writes the scan
    /// projection constant `cos(elev)·1000` into that halfword, and a live
    /// `TLX_N0K` packet of 360 × 1200 gates decodes to ~1.0 km per gate —
    /// 1200 km of range for a 300 km product. The ICD's 0.25 km wins.
    /// Product 165 (Digital Hydrometeor Classification) shares the same
    /// generator and the same lie — live `N0H` packets at every roster site
    /// decode 360 × 1200 gates to ~1.0 km per gate. (The remaining siblings
    /// 159/161 are not consumed anywhere in this workspace; they can join
    /// the table when a live packet of each has been checked the same way.)
    /// Product 177 (Hybrid Hydrometeor Classification) writes a literal
    /// `1000.` into that halfword (`hhc8bit.c`'s
    /// `RPGC_digital_radial_data_hdr` call in the CODE B21 source), so its
    /// 920 × 0.25 km packet also decodes to ~1.0 km per gate; confirmed on
    /// live `HHC` objects by the HHC twin harness.
    pub fn range_gate_km(&self) -> Option<f64> {
        match self.product_code {
            99 | 154 | 163 | 165 | 177 => Some(0.25),
            _ => None,
        }
    }

    /// The storm motion vector, present only on the storm-relative velocity
    /// products (55 region, 56 map).
    ///
    /// Gated on the product code because halfword 51 is the **BZ2 compression
    /// flag** on every digital product — read as a vector, `N1G` reports
    /// "0.1 kt from 1.3°", which is plausible enough to ship. See
    /// `decode::decompress_after_pdb`, which reads the same halfword.
    pub fn storm_motion(&self) -> Option<StormMotion> {
        if !matches!(self.product_code, 55 | 56) {
            return None;
        }
        Some(StormMotion {
            speed_kt: self.product_specific_47_53[4] as f32 / 10.0,
            direction_deg: self.product_specific_47_53[5] as f32 / 10.0,
            is_scit_average: self.product_specific_47_53[2] == -1,
        })
    }

    /// Identifies the volume scan this product came from. Two products share a
    /// vector only when these agree — the RPG re-fits the SCIT average every
    /// volume, and adjacent volumes differed by up to 4.7 kt in the sample used
    /// to validate the derivation.
    pub fn volume_key(&self) -> (u16, u32) {
        (self.volume_scan_date, self.volume_scan_time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate denies `unwrap` and `expect` everywhere, tests included.
    fn motion_of(p: &ProductDescriptionBlock) -> StormMotion {
        match p.storm_motion() {
            Some(m) => m,
            None => panic!("product {} reports no storm motion", p.product_code),
        }
    }

    /// A PDB carrying the halfwords that matter here and zeroes elsewhere.
    /// `thresholds` is halfwords 31–46, `ps47_53` halfwords 47–53.
    fn pdb(product_code: i16, thresholds: [u16; 16], ps47_53: [i16; 7]) -> ProductDescriptionBlock {
        ProductDescriptionBlock {
            block_divider: -1,
            latitude: 44.849,
            longitude: -93.565,
            height: 1000,
            product_code,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 39,
            volume_scan_date: 20661,
            volume_scan_time: 7108,
            generation_date: 20661,
            generation_time: 7108,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number: 9,
            product_specific_3: 13,
            thresholds,
            product_specific_47_53: ps47_53,
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        }
    }

    /// Halfwords 31–46 of a real `MPX_N1G` (product 154) and `TLX_N2U`
    /// (product 99): min -63.5, increment 0.5, 254 levels, rest zero.
    const VELOCITY_THRESHOLDS: [u16; 16] = [
        -635i16 as u16,
        5,
        254,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];

    /// Halfwords 47–53 of a real `MPX_N0S`: max neg/pos velocity, the SCIT
    /// flag, a spare, then 25.7 kt from 296.1°.
    const SRM_PS47_53: [i16; 7] = [-109, 76, -1, 7663, 257, 2961, 0];

    /// Halfwords 47–53 of a real `MPX_N1G`. Halfword 51 is the BZ2 compression
    /// flag here, not a speed, and halfword 52 is the uncompressed size's high
    /// halfword.
    const VELOCITY_PS47_53: [i16; 7] = [-93, 74, 0, 8097, 1, 13, 16382];

    /// Every gate value of a real velocity product, through the scale/offset
    /// pair, must land on the physical value the min/increment halfwords
    /// describe. Gate 129 is 0 m/s in both `MPX_N0G` and `TLX_N2U`; the
    /// endpoints are the ones an off-by-one in `FIRST_DATA_LEVEL` moves.
    #[test]
    fn a_velocity_products_scale_and_offset_come_from_its_min_and_increment() {
        for code in [93, 99, 154, 155] {
            let p = pdb(code, VELOCITY_THRESHOLDS, VELOCITY_PS47_53);
            let (scale, offset) = (p.data_scale(), p.data_offset());
            let physical = |gate: u16| (gate as f32 - offset) / scale;
            assert_eq!(scale, 2.0, "product {code} increment is 0.5 m/s");
            assert_eq!(offset, 129.0, "product {code} minimum is -63.5 m/s");
            assert_eq!(physical(2), -63.5, "product {code} gate 2 is the minimum");
            assert_eq!(physical(129), 0.0, "product {code} gate 129 is 0 m/s");
            assert_eq!(
                physical(255),
                63.0,
                "product {code} gate 255 is the maximum"
            );
            assert_eq!(p.data_levels(), Some(254));
        }
    }

    /// The minimum is a **signed** halfword. Read as `u16`, -635 becomes 64901
    /// and the whole field is scaled by 6490.1 rather than offset by -63.5 —
    /// a velocity product that decodes without error and is nonsense.
    #[test]
    fn a_negative_minimum_is_read_as_signed() {
        let p = pdb(99, VELOCITY_THRESHOLDS, VELOCITY_PS47_53);
        assert_eq!(p.thresholds[0], 64901, "the raw halfword really is 0xFD85");
        // 2 - (-63.5 / 0.5) = 129, not 2 - (6490.1 / 0.5) = -12978.2.
        assert_eq!(p.data_offset(), 129.0);
    }

    /// Products outside the min/increment family must keep the IEEE-float path.
    /// 134 DVL's thresholds are NEXRAD 16-bit floats, which decode to garbage
    /// as IEEE and must fall back to 1.0/0.0 rather than to a min/increment
    /// reading of the same halfwords.
    #[test]
    fn products_outside_the_family_keep_the_ieee_float_path() {
        // Product 94 DR: scale 2.0, offset 66.0 as two IEEE-float pairs.
        let ieee = |v: f32| {
            let b = v.to_bits();
            ((b >> 16) as u16, (b & 0xFFFF) as u16)
        };
        let (s_hi, s_lo) = ieee(2.0);
        let (o_hi, o_lo) = ieee(66.0);
        let mut t = [0u16; 16];
        t[0] = s_hi;
        t[1] = s_lo;
        t[2] = o_hi;
        t[3] = o_lo;
        let p = pdb(94, t, [0; 7]);
        assert_eq!(p.data_scale(), 2.0);
        assert_eq!(p.data_offset(), 66.0);
        assert_eq!(p.data_levels(), None);

        // The counterweight: a *velocity* product's halfwords read through the
        // IEEE path give neither 2.0 nor 129.0, so widening the product list to
        // 94 — or narrowing it away from 99 — changes an answer here.
        let misread = pdb(94, VELOCITY_THRESHOLDS, VELOCITY_PS47_53);
        assert_eq!(
            misread.data_scale(),
            1.0,
            "0xFD850005 is not a positive normal float"
        );
        assert_ne!(misread.data_offset(), 129.0);
    }

    /// A zero or negative increment would divide by zero or invert the field;
    /// the IEEE fallback is the safe answer.
    #[test]
    fn a_nonpositive_increment_falls_back_rather_than_dividing_by_zero() {
        for increment in [0i16, -5] {
            let mut t = VELOCITY_THRESHOLDS;
            t[1] = increment as u16;
            let p = pdb(99, t, VELOCITY_PS47_53);
            assert!(p.data_scale().is_finite(), "increment {increment}");
            assert_eq!(p.data_levels(), None, "increment {increment}");
        }
    }

    /// Storm motion is halfwords 51 and 52, each ÷10. Transcribed from a real
    /// `MPX_N0S`, whose tabular header reads 25.7 kt from 296.1°.
    #[test]
    fn storm_motion_is_read_from_halfwords_51_and_52() {
        let p = pdb(56, [0; 16], SRM_PS47_53);
        let m = motion_of(&p);
        assert_eq!(m.speed_kt, 25.7);
        assert_eq!(m.direction_deg, 296.1);
        assert!(m.is_scit_average, "halfword 49 is -1");
        // Halfwords 47/48 are max negative/positive velocity in knots, and 50
        // is a spare — reading any of them as the vector gives a different
        // number, so the indices are pinned by value, not by shape.
        assert_ne!(m.speed_kt, -10.9);
        assert_ne!(m.speed_kt, 7.6);
        assert_ne!(m.speed_kt, 766.3);
        assert_ne!(m.direction_deg, 766.3);
        assert_ne!(m.direction_deg, 25.7);
    }

    /// Halfword 49 distinguishes the SCIT cell average from an operator entry.
    #[test]
    fn a_hand_entered_vector_is_not_a_scit_average() {
        let mut ps = SRM_PS47_53;
        ps[2] = 0;
        let m = motion_of(&pdb(56, [0; 16], ps));
        assert!(!m.is_scit_average);
        // Still the same vector — only its provenance changed.
        assert_eq!((m.speed_kt, m.direction_deg), (25.7, 296.1));
    }

    /// Halfword 51 is the BZ2 compression flag on every digital product, so
    /// reading a vector off one yields 0.1 kt from a flag of 1. Only the
    /// storm-relative products have a vector at all.
    #[test]
    fn only_the_storm_relative_products_report_a_vector() {
        for code in [55, 56] {
            assert!(
                pdb(code, [0; 16], SRM_PS47_53).storm_motion().is_some(),
                "{code}"
            );
        }
        for code in [94, 99, 134, 135, 153, 154, 163, 176, 177] {
            let p = pdb(code, VELOCITY_THRESHOLDS, VELOCITY_PS47_53);
            assert_eq!(
                p.storm_motion(),
                None,
                "product {code} halfword 51 is a compression flag, not 0.1 kt",
            );
        }
    }

    /// `SRM = V + component`, with the component peaking where the azimuth
    /// points at the direction the storm comes from.
    #[test]
    fn the_radial_component_peaks_along_the_motion_direction() {
        let m = StormMotion {
            speed_kt: 30.0,
            direction_deg: 90.0,
            is_scit_average: true,
        };
        assert!(
            (m.radial_component_kt(90.0) - 30.0).abs() < 1e-9,
            "toward 090"
        );
        assert!(
            (m.radial_component_kt(270.0) + 30.0).abs() < 1e-9,
            "away at 270"
        );
        assert!(m.radial_component_kt(0.0).abs() < 1e-9, "orthogonal at 000");
        assert!(
            m.radial_component_kt(180.0).abs() < 1e-9,
            "orthogonal at 180"
        );
        // 60° off the motion direction is half the speed, so a sign flip or a
        // swapped subtraction cannot pass by symmetry alone.
        assert!((m.radial_component_kt(30.0) - 15.0).abs() < 1e-9);
        assert!((m.radial_component_kt(150.0) - 15.0).abs() < 1e-9);
        assert!((m.radial_component_kt(210.0) + 15.0).abs() < 1e-9);
    }

    /// Wrapping past 360° must not change the answer.
    #[test]
    fn the_radial_component_is_periodic_in_azimuth() {
        let m = StormMotion {
            speed_kt: 25.7,
            direction_deg: 296.1,
            is_scit_average: true,
        };
        for az in [0.0, 17.0, 183.5, 359.0] {
            let a = m.radial_component_kt(az);
            let b = m.radial_component_kt(az + 360.0);
            assert!((a - b).abs() < 1e-9, "azimuth {az}");
        }
    }

    /// The packet-16 scale factor halfword reads 999 for the 1 km product 56
    /// and the 0.25 km products 99, 154 and 163 alike, so only the product
    /// code can answer this. A wrong answer here draws the field 4× too far
    /// out — a live `TLX_N0K` (360 × 1200 gates) decoded to ~1.0 km per gate,
    /// 1200 km of range for a 300 km product.
    #[test]
    fn only_the_quarter_kilometre_products_override_the_gate_spacing() {
        for code in [99, 154] {
            assert_eq!(
                pdb(code, VELOCITY_THRESHOLDS, VELOCITY_PS47_53).range_gate_km(),
                Some(0.25)
            );
        }
        assert_eq!(pdb(163, [0; 16], [0; 7]).range_gate_km(), Some(0.25));
        assert_eq!(pdb(165, [0; 16], [0; 7]).range_gate_km(), Some(0.25));
        assert_eq!(pdb(177, [0; 16], [0; 7]).range_gate_km(), Some(0.25));
        for code in [56, 94, 134, 135, 176] {
            assert_eq!(
                pdb(code, [0; 16], [0; 7]).range_gate_km(),
                None,
                "product {code} must keep the packet's own spacing",
            );
        }
    }

    /// Two products belong to the same volume only when both halves agree —
    /// the date alone rolls over once a day and the time alone repeats.
    #[test]
    fn the_volume_key_is_the_date_and_the_time_together() {
        let a = pdb(56, [0; 16], SRM_PS47_53);
        let mut b = a.clone();
        assert_eq!(a.volume_key(), b.volume_key());
        b.volume_scan_time += 1;
        assert_ne!(a.volume_key(), b.volume_key());
        let mut c = a.clone();
        c.volume_scan_date += 1;
        assert_ne!(a.volume_key(), c.volume_key());
    }
}
