//! Validation twins: score a locally derived polar product against the RPG's
//! own Level III rendition of the same volume.
//!
//! [`compare`] is pure math — wasm-safe, no network — shipped code called at
//! runtime by the render and VILD paths. The live rigs that find the twin in
//! the first place (`l3_twin`, the site roster, the per-product
//! `live_validation` harnesses and their `validation_policy` modules) are not
//! in this tree, so every agreement percentage quoted in a doc comment in this
//! crate is a historical reading rather than something a check-out can
//! reproduce.

/// Pure-math comparison of a derived grid against a decoded Level III radial product.
pub mod compare {
    use crate::l3_values;
    use crate::volumetric::RANGE_BINS;
    use nexrad_level3::model::{Level3Message, ProductDescriptionBlock, RadialPacket};
    use std::collections::BTreeMap;

    /// How a Level III product's gate levels become physical values, and back.
    #[derive(Debug, Clone)]
    pub enum ValueCodec {
        /// Indexed table: the legacy 4-bit products and Digital VIL's hybrid.
        Lut(Vec<f32>),
        /// `physical = (gate − offset) / scale` — the digital family.
        Scaled { scale: f32, offset: f32 },
    }

    impl ValueCodec {
        /// The codec a message's own PDB and packet declare, as
        /// [`crate::render`] selects it to draw the product.
        pub fn for_message(msg: &Level3Message) -> Option<Self> {
            let packet = crate::srm::radial_packet(msg)?;
            if let Some(lut) =
                l3_values::build_vil_lut(&msg.pdb).or_else(|| l3_values::build_eet_lut(&msg.pdb))
            {
                return Some(Self::Lut(lut));
            }
            if packet.is_legacy {
                return Some(Self::Lut(
                    l3_values::decode_legacy_thresholds(&msg.pdb).to_vec(),
                ));
            }
            let scale = packet
                .xdr_data_scale
                .unwrap_or_else(|| msg.pdb.data_scale());
            let offset = packet
                .xdr_data_offset
                .unwrap_or_else(|| msg.pdb.data_offset());
            Some(Self::Scaled { scale, offset })
        }

        /// Gate level → physical value.
        pub fn decode(&self, gate: u16) -> f32 {
            if gate <= 1 {
                return f32::NAN;
            }
            match self {
                Self::Lut(table) => table.get(gate as usize).copied().unwrap_or(f32::NAN),
                Self::Scaled { scale, offset } => (gate as f32 - offset) / scale,
            }
        }

        /// Physical value → gate level; `NaN` encodes as 0.
        pub fn encode(&self, value: f32) -> u8 {
            match self {
                Self::Lut(table) => l3_values::quantize_via_lut(value, table),
                Self::Scaled { scale, offset } => {
                    l3_values::quantize_scaled(value, *scale, *offset)
                }
            }
        }

        /// The level a physical value lands on, at full width — what the tally
        /// compares.
        fn encode_level(&self, value: f32) -> i64 {
            match self {
                Self::Lut(table) => i64::from(l3_values::quantize_via_lut(value, table)),
                Self::Scaled { scale, offset } => (f64::from(value) * f64::from(*scale)
                    + f64::from(*offset))
                .round()
                .clamp(2.0, 65535.0) as i64,
            }
        }
    }

    /// The packet's gate spacing with the PDB's product-code override — the
    /// 0.25 km velocity products whose packet scale-factor halfword lies.
    pub fn gate_km(pdb: &ProductDescriptionBlock, packet: &RadialPacket) -> f64 {
        pdb.range_gate_km()
            .unwrap_or_else(|| packet.gate_interval_km())
    }

    /// The PDB's volume scan start as a timestamp — re-exported from
    /// [`crate::level3`], where the pairing that reads it lives.
    pub use crate::level3::volume_scan_started;

    /// Whether level distance means anything.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProductKind {
        Numeric,
        Class,
    }

    /// Agreement between a derived grid and its Level III twin, in the twin's
    /// own data levels, over the 360° × 230 km comparison domain.
    #[derive(Debug, Default, Clone)]
    pub struct Tally {
        /// Cells where the derived grid is defined.
        pub derived_defined: usize,
        /// Cells where the resampled Level III product is defined.
        pub l3_defined: usize,
        /// Cells defined on both sides — what the level statistics run over.
        pub compared: usize,
        pub exact: usize,
        pub within_one: usize,
        pub within_two: usize,
        /// Cells defined on exactly one side.
        pub presence_disagreements: usize,
        /// `(derived level, L3 level) → count`, filled only for [`ProductKind::Class`].
        pub confusion: BTreeMap<(u8, u8), usize>,
    }

    impl Tally {
        pub fn exact_pct(&self) -> f64 {
            100.0 * self.exact as f64 / self.compared.max(1) as f64
        }

        pub fn within_one_pct(&self) -> f64 {
            100.0 * self.within_one as f64 / self.compared.max(1) as f64
        }

        pub fn within_two_pct(&self) -> f64 {
            100.0 * self.within_two as f64 / self.compared.max(1) as f64
        }

        /// Share of the cells defined on at least one side that are defined
        /// on only one.
        pub fn presence_disagreement_pct(&self) -> f64 {
            let union = self.compared + self.presence_disagreements;
            100.0 * self.presence_disagreements as f64 / union.max(1) as f64
        }
    }

    /// Why a derived grid was refused: the comparison lattice is exactly 360
    /// azimuth rows of [`RANGE_BINS`] range cells, and this names the first
    /// dimension that was not.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum GridShape {
        /// The grid does not have 360 azimuth rows.
        Azimuths { found: usize },
        /// Row `az` does not have [`RANGE_BINS`] range cells.
        RangeBins { az: usize, found: usize },
    }

    impl std::fmt::Display for GridShape {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match *self {
                Self::Azimuths { found } => write!(
                    f,
                    "derived grid has {found} azimuth rows, not {}",
                    PolarGrid::AZIMUTHS,
                ),
                Self::RangeBins { az, found } => write!(
                    f,
                    "derived grid row {az} has {found} range cells, not {RANGE_BINS}",
                ),
            }
        }
    }

    impl std::error::Error for GridShape {}

    /// A derived field **on the comparison lattice**: exactly 360 azimuth-major
    /// rows of exactly [`RANGE_BINS`] cells, `NaN` undefined.
    #[derive(Debug, Clone, Copy)]
    pub struct PolarGrid<'a> {
        rows: &'a [Vec<f32>],
    }

    impl<'a> PolarGrid<'a> {
        /// Azimuth rows in the comparison domain: one per whole degree.
        pub const AZIMUTHS: usize = 360;

        /// Borrow `rows` as a comparison grid, or say why it is not one.
        pub fn new(rows: &'a [Vec<f32>]) -> Result<Self, GridShape> {
            if rows.len() != Self::AZIMUTHS {
                return Err(GridShape::Azimuths { found: rows.len() });
            }
            if let Some((az, row)) = rows
                .iter()
                .enumerate()
                .find(|(_, row)| row.len() != RANGE_BINS)
            {
                return Err(GridShape::RangeBins {
                    az,
                    found: row.len(),
                });
            }
            Ok(Self { rows })
        }

        /// Row `az`, [`RANGE_BINS`] cells wide — guaranteed by construction.
        pub fn row(&self, az: usize) -> &'a [f32] {
            &self.rows[az]
        }
    }

    impl<'a> TryFrom<&'a [Vec<f32>]> for PolarGrid<'a> {
        type Error = GridShape;

        fn try_from(rows: &'a [Vec<f32>]) -> Result<Self, Self::Error> {
            Self::new(rows)
        }
    }

    /// Score `derived` against a Level III message, using the message's own codec and
    /// gate spacing.
    pub fn tally_against_l3(
        derived: PolarGrid<'_>,
        msg: &Level3Message,
        kind: ProductKind,
    ) -> Option<Tally> {
        let packet = crate::srm::radial_packet(msg)?;
        let codec = ValueCodec::for_message(msg)?;
        Some(tally_packet(
            derived,
            packet,
            gate_km(&msg.pdb, packet),
            &codec,
            kind,
        ))
    }

    /// The packet's own gate levels on the 360° × 230 km comparison grid,
    /// `None` where the packet has no gate for a cell.
    pub fn resample_packet_levels(packet: &RadialPacket, l3_gate_km: f64) -> Vec<Vec<Option<u16>>> {
        // Which packet radial covers each tenth of a degree; later radials
        // overwrite earlier ones, as in the SRM resampler.
        let mut slots: Vec<Option<usize>> = vec![None; 3600];
        for (i, run) in packet.radials.iter().enumerate() {
            let start = (run.start_angle as f64 * 10.0).round() as i32;
            let width = (run.angle_delta as f64 * 10.0).round().max(1.0) as i32;
            for k in 0..width {
                slots[(start + k).rem_euclid(3600) as usize] = Some(i);
            }
        }

        // Which packet gate represents each 1-km cell: the one whose centre
        // sits nearest the cell centre, first gate winning ties.
        let n_gates = packet
            .radials
            .iter()
            .map(|r| r.gate_values.len())
            .max()
            .unwrap_or(0)
            .max(packet.num_range_bins as usize);
        let mut gate_for_bin: Vec<Option<usize>> = vec![None; RANGE_BINS];
        let mut best = vec![f64::INFINITY; RANGE_BINS];
        for j in 0..n_gates {
            let centre = packet.gate_range_km(j, l3_gate_km);
            let bin = centre.floor() as i64;
            if !(0..RANGE_BINS as i64).contains(&bin) {
                continue;
            }
            let d = (centre - (bin as f64 + 0.5)).abs();
            if d < best[bin as usize] {
                best[bin as usize] = d;
                gate_for_bin[bin as usize] = Some(j);
            }
        }

        (0..360)
            .map(|az| {
                let radial = slots[az * 10 + 5].map(|ri| &packet.radials[ri]);
                (0..RANGE_BINS)
                    .map(|r| {
                        radial.and_then(|run| {
                            gate_for_bin[r].and_then(|j| run.gate_values.get(j).copied())
                        })
                    })
                    .collect()
            })
            .collect()
    }

    /// The lower-level entry point: explicit packet, gate spacing and codec,
    /// scoring the derived grid against [`resample_packet_levels`].
    pub fn tally_packet(
        derived: PolarGrid<'_>,
        packet: &RadialPacket,
        l3_gate_km: f64,
        codec: &ValueCodec,
        kind: ProductKind,
    ) -> Tally {
        let levels = resample_packet_levels(packet, l3_gate_km);
        // The reference side is the full domain for every packet, by the same two
        // constants `PolarGrid` holds the derived side to.
        assert_eq!(
            levels.len(),
            PolarGrid::AZIMUTHS,
            "the resampled reference is not the full comparison domain",
        );

        let mut t = Tally::default();
        for (az, l3_row) in levels.iter().enumerate() {
            let row = derived.row(az);
            for (r, &l3_level) in l3_row.iter().enumerate() {
                // Both rows are `RANGE_BINS` wide; a mismatch panics here
                // rather than silently scoring the shorter of the two.
                let v = row[r];
                let l3_defined = l3_level.is_some_and(|g| codec.decode(g).is_finite());
                match (v.is_finite(), l3_defined) {
                    (true, true) => {
                        t.derived_defined += 1;
                        t.l3_defined += 1;
                        t.compared += 1;
                        let dl = codec.encode_level(v);
                        // `l3_defined` proved the level exists.
                        let ll = i64::from(l3_level.unwrap_or(0));
                        let diff = (dl - ll).abs();
                        t.exact += usize::from(diff == 0);
                        t.within_one += usize::from(diff <= 1);
                        t.within_two += usize::from(diff <= 2);
                        if kind == ProductKind::Class {
                            let key = (dl.clamp(0, 255) as u8, ll.clamp(0, 255) as u8);
                            *t.confusion.entry(key).or_insert(0) += 1;
                        }
                    }
                    (true, false) => {
                        t.derived_defined += 1;
                        t.presence_disagreements += 1;
                    }
                    (false, true) => {
                        t.l3_defined += 1;
                        t.presence_disagreements += 1;
                    }
                    (false, false) => {}
                }
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::compare::*;
    use crate::volumetric::RANGE_BINS;
    use nexrad_level3::model::{RadialPacket, RadialRun};

    const SCALE: f32 = 2.0;
    const OFFSET: f32 = 66.0;

    fn codec() -> ValueCodec {
        ValueCodec::Scaled {
            scale: SCALE,
            offset: OFFSET,
        }
    }

    /// A packet of `n` even radials whose gate levels come from `level_at`.
    fn packet(
        n_radials: usize,
        n_gates: usize,
        first_range_bin: i16,
        scale_factor: f32,
        level_at: impl Fn(usize, usize) -> u16,
    ) -> RadialPacket {
        let width = 360.0 / n_radials as f32;
        RadialPacket {
            first_range_bin,
            num_range_bins: n_gates as u16,
            i_center: 0,
            j_center: 0,
            scale_factor,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: (0..n_radials)
                .map(|i| RadialRun {
                    start_angle: i as f32 * width,
                    angle_delta: width,
                    gate_values: (0..n_gates).map(|j| level_at(i, j)).collect(),
                })
                .collect(),
        }
    }

    fn empty_grid() -> Vec<Vec<f32>> {
        vec![vec![f32::NAN; 230]; 360]
    }

    /// The tests' own way onto the comparison lattice.
    fn grid(rows: &[Vec<f32>]) -> PolarGrid<'_> {
        PolarGrid::new(rows).expect("test fixture is a 360 × 230 grid")
    }

    #[test]
    fn a_perfect_derivation_scores_exact_everywhere() {
        let p = packet(360, 230, 0, 1.0, |az, r| ((az + r) % 200 + 20) as u16);
        let mut derived = empty_grid();
        for (az, row) in derived.iter_mut().enumerate() {
            for (r, v) in row.iter_mut().enumerate() {
                *v = (((az + r) % 200 + 20) as f32 - OFFSET) / SCALE;
            }
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Numeric);
        assert_eq!(t.compared, 360 * 230);
        assert_eq!(t.exact, t.compared);
        assert_eq!(t.within_one, t.compared);
        assert_eq!(t.within_two, t.compared);
        assert_eq!(t.presence_disagreements, 0);
        assert_eq!(t.exact_pct(), 100.0);
        assert!(t.confusion.is_empty(), "numeric products fill no matrix");
    }

    #[test]
    fn a_one_level_shift_is_within_one_but_never_exact() {
        let p = packet(360, 230, 0, 1.0, |_, _| 100);
        let mut derived = empty_grid();
        for row in derived.iter_mut() {
            for v in row.iter_mut() {
                *v = (101.0 - OFFSET) / SCALE;
            }
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Numeric);
        assert_eq!(t.exact, 0);
        assert_eq!(t.within_one, t.compared);
        assert_eq!(t.within_two, t.compared);

        for v in derived.iter_mut().flatten() {
            *v = (103.0 - OFFSET) / SCALE;
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Numeric);
        assert_eq!(t.within_one, 0);
        assert_eq!(t.within_two, 0, "three levels is outside every band");
    }

    #[test]
    fn presence_disagreements_count_cells_defined_on_exactly_one_side() {
        let p = packet(360, 230, 0, 1.0, |az, _| if az == 0 { 0 } else { 100 });
        let mut derived = empty_grid();
        for az in [0usize, 1] {
            for v in derived[az].iter_mut() {
                *v = (100.0 - OFFSET) / SCALE;
            }
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Numeric);
        assert_eq!(t.compared, 230);
        assert_eq!(t.exact, 230);
        assert_eq!(t.presence_disagreements, 230 + 358 * 230);
        assert_eq!(t.derived_defined, 2 * 230);
        assert_eq!(t.l3_defined, 359 * 230);
        let union = (t.compared + t.presence_disagreements) as f64;
        assert!(
            (t.presence_disagreement_pct() - 100.0 * (t.presence_disagreements as f64) / union)
                .abs()
                < 1e-12,
        );
    }

    #[test]
    fn sub_degree_radials_resolve_through_the_tenth_degree_table() {
        let p = packet(720, 230, 0, 1.0, |i, _| if i % 2 == 0 { 100 } else { 110 });
        let mut derived = empty_grid();
        for row in derived.iter_mut() {
            for v in row.iter_mut() {
                *v = (110.0 - OFFSET) / SCALE; // the odd run's value
            }
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Numeric);
        assert_eq!(
            t.exact, t.compared,
            "a cell centre read the radial left of it",
        );
    }

    #[test]
    fn the_range_mapping_honours_the_packets_first_bin_offset() {
        let p = packet(360, 10, 2, 1.0, |_, j| 100 + j as u16);
        let mut derived = empty_grid();
        for row in derived.iter_mut() {
            for (r, v) in row.iter_mut().enumerate() {
                // Cells 0 and 1 sit before the packet's first gate; cells
                // 2..12 map to gates 0..10.
                if (2..12).contains(&r) {
                    *v = ((100 + r - 2) as f32 - OFFSET) / SCALE;
                }
            }
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Numeric);
        assert_eq!(t.compared, 360 * 10);
        assert_eq!(t.exact, t.compared, "the offset shifted the gate mapping");
        assert_eq!(t.presence_disagreements, 0);
    }

    #[test]
    fn quarter_km_gates_resample_to_the_cell_centre_and_stop_at_230() {
        let p = packet(360, 1000, 0, 4.0, |_, j| 20 + (j / 4) as u16);
        let mut derived = empty_grid();
        for row in derived.iter_mut() {
            for (r, v) in row.iter_mut().enumerate() {
                *v = ((20 + r) as f32 - OFFSET) / SCALE;
            }
        }
        let t = tally_packet(grid(&derived), &p, 0.25, &codec(), ProductKind::Numeric);
        assert_eq!(t.compared, 360 * 230, "the domain is 230 km, not 250");
        assert_eq!(t.exact, t.compared);
    }

    #[test]
    fn class_products_get_a_confusion_matrix() {
        // L3 says class 60 everywhere; the derivation says 60 on even
        // azimuths and 80 on odd ones.
        let p = packet(360, 230, 0, 1.0, |_, _| 60);
        let mut derived = empty_grid();
        for (az, row) in derived.iter_mut().enumerate() {
            let class = if az % 2 == 0 { 60.0 } else { 80.0 };
            for v in row.iter_mut() {
                *v = (class - OFFSET) / SCALE;
            }
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Class);
        assert_eq!(t.confusion.get(&(60, 60)), Some(&(180 * 230)));
        assert_eq!(t.confusion.get(&(80, 60)), Some(&(180 * 230)));
        assert_eq!(t.confusion.values().sum::<usize>(), t.compared);
        assert_eq!(t.exact, 180 * 230);
    }

    #[test]
    fn audit_a_short_grid_scores_perfect() {
        let derived: Vec<Vec<f32>> = vec![
            (0..230)
                .map(|r| ((r % 200 + 20) as f32 - OFFSET) / SCALE)
                .collect(),
        ];
        assert_eq!(derived.len(), 1, "premise: the grid is 1/360 of a grid");
        assert_eq!(
            PolarGrid::new(&derived).unwrap_err(),
            GridShape::Azimuths { found: 1 },
            "a 1/360-height grid is refused, not scored perfect",
        );

        let narrow: Vec<Vec<f32>> = (0..360)
            .map(|az| vec![((az % 200 + 20) as f32 - OFFSET) / SCALE])
            .collect();
        assert_eq!(
            PolarGrid::new(&narrow).unwrap_err(),
            GridShape::RangeBins { az: 0, found: 1 },
            "a 1-bin-wide grid is refused, and the row is named",
        );

        let mut ragged = vec![vec![f32::NAN; 230]; 360];
        ragged[200].truncate(229);
        assert_eq!(
            PolarGrid::new(&ragged).unwrap_err(),
            GridShape::RangeBins {
                az: 200,
                found: 229
            },
            "one short row of 360 is refused, and it is named",
        );

        assert_eq!(
            PolarGrid::new(&[]).unwrap_err(),
            GridShape::Azimuths { found: 0 },
            "an empty grid is refused, not scored 0/0",
        );
    }

    #[test]
    fn audit_a_720_row_grid_is_scored_against_the_wrong_azimuths() {
        // A 720-row half-degree grid that is *correct*: row i is azimuth
        // i * 0.5, and it carries the level of the degree it sits in.
        let derived: Vec<Vec<f32>> = (0..720)
            .map(|i| vec![((i / 2 + 20) as f32 - OFFSET) / SCALE; 230])
            .collect();

        assert_eq!(
            PolarGrid::new(&derived).unwrap_err(),
            GridShape::Azimuths { found: 720 },
            "super-resolution height is refused, not silently halved",
        );
    }

    #[test]
    fn a_refusal_names_the_dimension_and_the_count() {
        assert_eq!(
            GridShape::Azimuths { found: 720 }.to_string(),
            "derived grid has 720 azimuth rows, not 360",
        );
        assert_eq!(
            GridShape::RangeBins { az: 200, found: 1 }.to_string(),
            "derived grid row 200 has 1 range cells, not 230",
        );
    }

    #[test]
    fn the_reference_side_is_always_the_full_domain() {
        for (radials, gates, gate_km) in [(720, 1832, 0.25), (360, 230, 1.0), (1, 4, 4.0)] {
            let p = packet(radials, gates, 0, 1.0, |_, _| 20);
            let levels = resample_packet_levels(&p, gate_km);
            assert_eq!(levels.len(), 360, "{radials} radials");
            assert!(
                levels.iter().all(|row| row.len() == RANGE_BINS),
                "{gates} gates at {gate_km} km",
            );
        }
    }

    #[test]
    fn the_scaled_codec_decodes_and_encodes_symmetrically() {
        let c = codec();
        assert!(c.decode(0).is_nan(), "level 0 is below threshold");
        assert!(c.decode(1).is_nan(), "level 1 is range folded");
        assert_eq!(c.decode(100), 17.0);
        assert_eq!(c.encode(17.0), 100);
        assert_eq!(c.encode(f32::NAN), 0);

        let lut = ValueCodec::Lut(vec![f32::NAN, f32::NAN, 5.0, 10.0]);
        assert!(lut.decode(1).is_nan());
        assert_eq!(lut.decode(3), 10.0);
        assert!(lut.decode(4).is_nan(), "past the table is undefined");
        assert_eq!(lut.encode(9.0), 3);
    }

    #[test]
    fn for_message_selects_the_eet_lut_for_product_135() {
        use nexrad_level3::model::{
            DataLayer, DataPacket, Level3Message, MessageHeader, SymbologyBlock,
        };
        let mut pdb = pdb_with_volume(20661, 7108);
        pdb.thresholds[..4].copy_from_slice(&[0x7F, 1, 2, 0x80]);
        let msg = Level3Message {
            header: MessageHeader {
                message_code: 135,
                date_of_message: 20661,
                time_of_message: 7200,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb,
            symbology: Some(SymbologyBlock {
                block_id: 1,
                block_length: 0,
                num_layers: 1,
                layers: vec![DataLayer {
                    layer_length: 0,
                    packets: vec![DataPacket::DigitalRadial(packet(
                        360,
                        4,
                        0,
                        0.001, // what a live EET packet carries
                        |_, j| [0, 2, 71, 130][j],
                    ))],
                }],
            }),
        };
        let codec = ValueCodec::for_message(&msg).expect("a radial packet is present");
        assert!(matches!(codec, ValueCodec::Lut(_)), "135 decodes via LUT");
        assert!(codec.decode(0).is_nan());
        assert_eq!(codec.decode(2), 0.0);
        assert_eq!(codec.decode(71), 69.0);
        assert_eq!(codec.decode(130), 0.0, "bit 7 flags topped, not height");
        assert_eq!(codec.decode(199), 69.0);
        assert!(codec.decode(100).is_nan(), "outside the encodable band");
    }

    /// A PDB carrying only the fields the twin pairing reads.
    fn pdb_with_volume(date: u16, time: u32) -> nexrad_level3::model::ProductDescriptionBlock {
        nexrad_level3::model::ProductDescriptionBlock {
            block_divider: -1,
            latitude: 41.320,
            longitude: -96.367,
            height: 1148,
            product_code: 135,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 1,
            volume_scan_date: date,
            volume_scan_time: time,
            generation_date: date,
            generation_time: time + 90,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number: 0,
            product_specific_3: 0,
            thresholds: [0; 16],
            product_specific_47_53: [0; 7],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        }
    }
}
