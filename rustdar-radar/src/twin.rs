//! Validation twins: score a locally derived polar product against the RPG's
//! own Level III rendition of the **same volume**.
//!
//! Two layers. [`compare`] is pure math — wasm-safe, no network — shipped
//! code called at runtime by the render and VILD paths, and shared by the
//! product harnesses (EET, DVL, KDP, HCA, DPR). `live` was the native,
//! test-only layer that finds the twin in the first place: the archived
//! Level II volume nearest a moment, and the Level III bucket object
//! generated from that very volume — never merely the newest key, which
//! SAILS republishing makes a mid-volume repeat more often than not.
//!
//! **The live rigs live on branch `campaign-harness`**, not here: `l3_twin`,
//! the site roster and its two Level II fetchers, the per-product
//! `live_validation` harnesses, their `validation_policy` modules and offline
//! policy pins, the `compare_l3` example, and `live_elevation_audit`. Check
//! that branch out and run a rig with e.g.
//!
//! ```text
//! cargo test -p rustdar-radar --release --lib -- --ignored --nocapture live_
//! ```
//!
//! Because those rigs are not here, **every agreement percentage quoted in a
//! doc comment in this crate is a historical reading, not something a check-out
//! of this tree can reproduce.** The modules that quote one say so where they
//! quote it. What re-running means is concrete and the same everywhere: check
//! out `campaign-harness`, which carries `l3_twin`, the roster, the fetchers
//! and each product's `validation_policy`, and run the command above. Nothing
//! in this tree can stand in for that — the offline suites pin formulas and
//! transcriptions, never agreement with a twin.

/// Pure-math comparison of a derived grid against a decoded Level III radial
/// product. Everything here is deterministic and network-free.
///
/// # Provenance: this is our instrument, not anyone's standard
///
/// **No authority defines the resampling below, so there is nothing external
/// to check it against.** The ICD says how a Level III packet is laid out; it
/// does not say how to put a 360 × 230 km derived grid and a packet of
/// arbitrary radial width and gate spacing onto one grid so the two can be
/// differenced. The four rules that decide that — the tenth-of-a-degree
/// azimuth table read at the cell centre, nearest-gate-centre range selection,
/// the 230 km cut, and levels 0/1 read as undefined — were chosen here. The
/// azimuth mapping is [`crate::srm`]'s resampler reused, which is the only
/// part with a stated ancestor; the rest carries no record of having been
/// arbitrated against an alternative.
///
/// That matters more than the size of the code suggests. **Every "% exact" and
/// "% within one level" anywhere in this crate is computed by this module**,
/// so a systematic bias here moves every product's score in one direction at
/// once and is invisible in all of them — including in the product-to-product
/// comparisons the campaign uses to arbitrate cell statistics. One bug of
/// exactly that shape is on the record: product 135's levels read through the
/// scaled fallback instead of its LUT painted every EET bin 2 kft high, in
/// every EET score, until it was found by hand — the codec selection that
/// stops it is pinned by `for_message_selects_the_eet_lut_for_product_135`.
///
/// The check this class of instrument admits is an independent regrid — a
/// second implementation written from the packet layout rather than from this
/// file, differenced against this one. That does not exist in this tree and
/// nothing here is a substitute for it, so read the level statistics as
/// **self-consistent and unarbitrated**: they compare products to each other
/// on equal terms; they do not establish an absolute agreement figure.
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
        /// The codec a message's own PDB and packet declare — the same
        /// selection [`crate::render`] makes to draw the product: Digital
        /// VIL's hybrid LUT for product 134, EET's mask/scale/offset LUT for
        /// product 135, the legacy threshold table for RLE packets, otherwise
        /// packet-28 XDR scale/offset falling back to the PDB's pair. `None`
        /// when the message carries no radial packet.
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

        /// Gate level → physical value. Levels 0 and 1 are below-threshold
        /// and range-folded across the whole family — the renderer skips them
        /// too — so they decode `NaN`, as do a LUT's own `NaN` levels and any
        /// level past the table.
        pub fn decode(&self, gate: u16) -> f32 {
            if gate <= 1 {
                return f32::NAN;
            }
            match self {
                Self::Lut(table) => table.get(gate as usize).copied().unwrap_or(f32::NAN),
                Self::Scaled { scale, offset } => (gate as f32 - offset) / scale,
            }
        }

        /// Physical value → gate level; `NaN` encodes as 0. Byte-sized, which
        /// every LUT product is; a 16-bit product's levels above 255 clamp,
        /// so score wide products with [`Tally`] (which compares levels at
        /// full width) rather than through this.
        pub fn encode(&self, value: f32) -> u8 {
            match self {
                Self::Lut(table) => l3_values::quantize_via_lut(value, table),
                Self::Scaled { scale, offset } => {
                    l3_values::quantize_scaled(value, *scale, *offset)
                }
            }
        }

        /// The level a physical value lands on, at full width — what the
        /// tally compares. Identical to [`encode`](Self::encode) for byte
        /// products, but a 16-bit product (DPR) keeps its upper levels.
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
    ///
    /// Kept in this namespace because the product harnesses (on branch
    /// `campaign-harness`) reach it here, and because it belongs to the same
    /// idea as the rest of `compare`: a twin is the object of *this* volume, not the newest
    /// one. There is one implementation, in production code, so the frontend's
    /// Level III loop and the harnesses cannot disagree about which volume an
    /// object names.
    pub use crate::level3::volume_scan_started;

    /// Whether level distance means anything. A numeric product's levels are
    /// ordered, so within-±1 is "one shade off"; a class product's levels are
    /// codes, where only equality and the confusion matrix carry information.
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
        /// `(derived level, L3 level) → count`, filled only for
        /// [`ProductKind::Class`]. Levels above 255 clamp.
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
    ///
    /// Returned by [`PolarGrid::new`] rather than asserted, so the caller
    /// learns *which* dimension is wrong and what it actually had. The two
    /// variants are the two directions the old `take(360)` / `take(RANGE_BINS)`
    /// walk failed silently in: a short grid shrank the denominator and scored
    /// better, a tall one was truncated and scored against the wrong azimuths.
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
    ///
    /// # Why this is a type and not an assertion
    ///
    /// The lattice is not a property of any one product — it is the coordinate
    /// system every twin score is quoted in, and the denominator of every
    /// `% exact` this crate publishes. Expressing it as a type moves the
    /// question from *"did this function remember to check?"* to *"where did
    /// this grid become a comparison grid?"*, which has exactly one answer
    /// ([`PolarGrid::new`]) and is checked once, at the boundary, instead of
    /// once per scorer.
    ///
    /// It is a **borrow**, not an owned grid: every producer in the tree
    /// already allocates `vec![vec![f32::NAN; RANGE_BINS]; 360]` and fills it,
    /// so requiring them to change representation would buy nothing. What the
    /// borrow buys is that [`tally_packet`] can no longer be *reached* with a
    /// mis-shape — there is no expressible call that gets past the constructor
    /// — and that its inner loop needs no bounds cap, so nothing in it can
    /// silently iterate fewer cells than the domain.
    #[derive(Debug, Clone, Copy)]
    pub struct PolarGrid<'a> {
        rows: &'a [Vec<f32>],
    }

    impl<'a> PolarGrid<'a> {
        /// Azimuth rows in the comparison domain: one per whole degree.
        pub const AZIMUTHS: usize = 360;

        /// Borrow `rows` as a comparison grid, or say why it is not one.
        ///
        /// Every row is checked, not just the first: a ragged grid is as
        /// unscoreable as a short one, and costs the same walk to rule out
        /// as the tally itself does to index.
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
        ///
        /// # Panics
        /// If `az >= 360`, like any slice index.
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

    /// Score `derived` against a Level III message, using the message's own
    /// codec and gate spacing. `None` when the message carries no radial
    /// packet.
    ///
    /// The 360 × 230 azimuth-major contract is carried by [`PolarGrid`], which
    /// is the only way to build the argument — it is no longer a sentence in
    /// this comment that nothing enforces.
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
    ///
    /// Levels, not values: what [`tally_packet`] compares is the level, so
    /// the resampling has to stop short of the codec — a rate product's
    /// tolerance-based harness decodes them itself.
    ///
    /// Resampling, cell for cell of the derived grid:
    ///
    /// * **azimuth** through a tenth-of-a-degree table of which packet radial
    ///   covers each 0.1° (the [`crate::srm`] resampler's mapping), read at
    ///   the cell centre `az + 0.5°`, so radials that do not start on whole
    ///   degrees land correctly;
    /// * **range** by taking, per 1-km cell, the packet gate whose centre
    ///   [`RadialPacket::gate_range_km`] reports falls nearest the cell
    ///   centre — both sides' first-bin offsets honoured, sub-kilometre
    ///   products represented by their centre gate.
    ///
    /// The domain is always ≤ 230 km: packet gates beyond it are ignored.
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
    ///
    /// Both sides are the full 360 × [`RANGE_BINS`] domain and neither walk is
    /// capped: the derived side because [`PolarGrid`] cannot be built any other
    /// shape, the reference side because [`resample_packet_levels`] builds its
    /// own grid from those two constants whatever the packet's radial count and
    /// gate spacing are (pinned by
    /// `the_reference_side_is_always_the_full_domain`). So `compared` counts
    /// over a domain fixed in advance, not over however many cells the caller
    /// happened to supply.
    pub fn tally_packet(
        derived: PolarGrid<'_>,
        packet: &RadialPacket,
        l3_gate_km: f64,
        codec: &ValueCodec,
        kind: ProductKind,
    ) -> Tally {
        let levels = resample_packet_levels(packet, l3_gate_km);
        // The reference side is the full domain for every packet, by the same
        // two constants `PolarGrid` holds the derived side to. Asserted rather
        // than assumed because the walk below reads the two together, and a
        // short *reference* would shrink `compared` exactly the way a short
        // derived grid used to — the defect this commit closes, arriving from
        // the other side.
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

    /// The tests' own way onto the comparison lattice. A fixture that is not
    /// 360 × 230 is a bug in the test, so it panics here rather than
    /// producing a score — which is the whole change these tests exercise.
    fn grid(rows: &[Vec<f32>]) -> PolarGrid<'_> {
        PolarGrid::new(rows).expect("test fixture is a 360 × 230 grid")
    }

    /// A derived grid decoding exactly what the packet encodes scores 100%
    /// exact with no presence disagreement.
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

    /// One level off everywhere: 0% exact, 100% within one.
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

    /// Levels 0 and 1 are undefined on the Level III side; a derived value
    /// there is a presence disagreement, as is a defined Level III gate under
    /// a derived NaN.
    #[test]
    fn presence_disagreements_count_cells_defined_on_exactly_one_side() {
        // Radial 0: level 0 (undefined). Radial 1: level 100 (defined).
        let p = packet(360, 230, 0, 1.0, |az, _| if az == 0 { 0 } else { 100 });
        let mut derived = empty_grid();
        // Derived defined on radials 0 and 1 only.
        for az in [0usize, 1] {
            for v in derived[az].iter_mut() {
                *v = (100.0 - OFFSET) / SCALE;
            }
        }
        let t = tally_packet(grid(&derived), &p, 1.0, &codec(), ProductKind::Numeric);
        // az 0: derived-only (230 cells). az 1: both. az 2..: L3-only.
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

    /// Half-degree radials: the cell centre at az + 0.5° must land in the
    /// *second* of the two runs covering the degree, since it starts there.
    #[test]
    fn sub_degree_radials_resolve_through_the_tenth_degree_table() {
        // 720 radials, 0.5° wide. Runs 2k and 2k+1 cover degree k; the cell
        // centre k + 0.5° belongs to run 2k+1. Encode run parity in the level.
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

    /// Both first-bin offsets shift the range mapping: packet gate j sits at
    /// (first_range_bin + j + 0.5) km, so with first_range_bin = 2 the cell
    /// at 5 km reads gate 3.
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

    /// A 0.25-km product: each 1-km cell is represented by the sub-gate
    /// nearest its centre, and gates beyond 230 km never enter the domain.
    #[test]
    fn quarter_km_gates_resample_to_the_cell_centre_and_stop_at_230() {
        // 4 gates/km out to 250 km. Level = 20 + km, on every sub-gate of
        // that km, so any in-cell choice reads the right level — what is
        // pinned is the mapping, plus the domain cut at 230.
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

    /// Class products fill the confusion matrix; the level stats still count.
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

    // ---- AUDIT DEMONSTRATIONS (campaign/oracle-round2), REFUTED ---------
    //
    // These arrived as *demonstrations*: each built a mis-shaped grid and
    // asserted the flattering number the old walk produced for it. Their
    // premises — the grids — are preserved below cell for cell, and each
    // one's original assertions are quoted verbatim in its doc comment.
    //
    // They cannot pass as written under a correct scorer, and they never
    // failed: on `main` before this commit both PASSED, because what they
    // assert *is* the old behaviour. A grid that scores 100% while missing
    // 359 of its 360 rows is the defect; a fix that keeps that assertion
    // true is not a fix. So each has been turned into its own refutation —
    // same fixture, same name, the expectation inverted from "this is what
    // it scores" to "this is refused, and here is which dimension was
    // wrong". The unchanged demonstration remains in this branch's parent
    // commit as the record of the old behaviour.

    /// AUDIT, REFUTED: a grid that is *physically short* is now refused by
    /// [`PolarGrid::new`] naming the dimension, instead of scoring a perfect
    /// 100% exact and 0% presence disagreement off a shrunken denominator.
    ///
    /// The demonstration this replaces asserted, on the same three fixtures:
    ///
    /// ```text
    /// assert_eq!(t.compared, 230, "only the row that exists was compared");
    /// assert_eq!(t.exact_pct(), 100.0, "a 1/360-height grid scores perfect");
    /// assert_eq!(t.presence_disagreement_pct(), 0.0, …);
    /// assert_eq!(t.l3_defined, 230, "the reference's other 359 rows vanished");
    /// assert_eq!(t.exact_pct(), 100.0, "a 1-bin-wide grid scores perfect");
    /// assert_eq!(t.exact_pct(), 0.0, "0/max(1) reads 0%, not 100%");
    /// ```
    ///
    /// Every one of those was true, and none of them can be reached now:
    /// there is no `Tally` to interrogate, because there is no scoring call
    /// to make. Contrast
    /// `presence_disagreements_count_cells_defined_on_exactly_one_side`: a
    /// full-height grid whose rows are `NaN` is still correctly penalised.
    /// The difference between "360 rows, 359 of them NaN" (0.28% of the
    /// union) and "1 row" is no longer invisible — it is the difference
    /// between a score and a refusal.
    #[test]
    fn audit_a_short_grid_scores_perfect() {
        // One single row, agreeing exactly. 359 rows simply absent.
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

        // The same holds for a short *row*: 1 bin of 230.
        let narrow: Vec<Vec<f32>> = (0..360)
            .map(|az| vec![((az % 200 + 20) as f32 - OFFSET) / SCALE])
            .collect();
        assert_eq!(
            PolarGrid::new(&narrow).unwrap_err(),
            GridShape::RangeBins { az: 0, found: 1 },
            "a 1-bin-wide grid is refused, and the row is named",
        );

        // A ragged grid — full height, one short row at 200 — is refused
        // too. Nothing about the old walk would have noticed this one.
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

        // And the degenerate case: an empty grid was 0/0 and read as 0%.
        // It is now refused for what it is — no rows at all.
        assert_eq!(
            PolarGrid::new(&[]).unwrap_err(),
            GridShape::Azimuths { found: 0 },
            "an empty grid is refused, not scored 0/0",
        );
    }

    /// AUDIT, REFUTED: a *tall* grid is no longer truncated to its first 360
    /// rows and scored against azimuths it does not correspond to — it is
    /// refused, naming 720.
    ///
    /// The demonstration this replaces built the same *correct* 720-row
    /// half-degree derivation and asserted:
    ///
    /// ```text
    /// assert_eq!(t.compared, 360 * 230, "only half the grid was even read");
    /// assert_eq!(t.exact, 230, "1 of 360 scored rows landed on its own azimuth");
    /// assert!(t.exact_pct() < 1.0, "a *correct* 720-row derivation scores {:.2}%", …);
    /// ```
    ///
    /// This is the case that defeats the obvious defence: `compared` came
    /// back at 82,800 — the full domain, exactly what a healthy score looks
    /// like — while every cell was mis-registered by a factor of two in
    /// azimuth, so the 0.28% would have been read as a catastrophic
    /// *algorithm* failure with nothing to distinguish it from one.
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

    /// The refusals say which dimension was wrong and what it actually was —
    /// the whole point of returning [`GridShape`] rather than asserting.
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

    /// The reference side needs no such view: [`resample_packet_levels`]
    /// builds the full domain from the two constants whatever the packet
    /// carries, so `tally_packet`'s uncapped walk cannot index past it.
    ///
    /// Not circular — the packets here are deliberately *unlike* the domain:
    /// 720 radials, and 1832 gates at a quarter of the grid's spacing.
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

    /// The codec round trip and its undefined levels, through the public
    /// surface the example uses.
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

    /// Product 135 selects the EET mask/scale/offset LUT — its thresholds
    /// are `[127, 1, 2, 128]`, not floats, and decoding them as the scaled
    /// fallback painted every bin 2 kft high and topped bins as 130–199 kft.
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

    // The volume stamp conversion is asserted where it now lives, beside the
    // pairing that reads it: `level3::the_volume_stamp_reads_day_one_as_the_epoch`
    // makes the same three claims (MJD 20661 → 2026-07-26 01:58:28, day 1 is the
    // epoch, day 0 is `None`) against the one implementation.

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
