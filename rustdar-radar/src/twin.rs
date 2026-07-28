//! Validation twins: score a locally derived polar product against the RPG's
//! own Level III rendition of the **same volume**.
//!
//! Two layers. [`compare`] is pure math — wasm-safe, no network — and is what
//! the `compare_l3` example and the product harnesses (EET, DVL, KDP, HCA,
//! DPR) share: resample a Level III radial packet onto the derived 360° ×
//! 230 km grid and produce a [`compare::Tally`]. [`live`] is the native,
//! test-only layer that finds the twin in the first place: the archived Level
//! II volume nearest a moment, and the Level III bucket object generated from
//! that very volume — never merely the newest key, which SAILS republishing
//! makes a mid-volume repeat more often than not.

/// Pure-math comparison of a derived grid against a decoded Level III radial
/// product. Everything here is deterministic and network-free.
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

    /// The PDB's volume scan start as a timestamp. The modified Julian date's
    /// **day 1 is 1970-01-01** — the same convention as the generation stamp
    /// the SRM harness reads.
    pub fn volume_scan_started(pdb: &ProductDescriptionBlock) -> Option<chrono::NaiveDateTime> {
        let days = u64::from(pdb.volume_scan_date).checked_sub(1)?;
        chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?
            .checked_add_days(chrono::Days::new(days))?
            .and_hms_opt(0, 0, 0)?
            .checked_add_signed(chrono::Duration::seconds(i64::from(pdb.volume_scan_time)))
    }

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

    /// Score `derived` (360 × 230, azimuth-major, `NaN` undefined) against a
    /// Level III message, using the message's own codec and gate spacing.
    /// `None` when the message carries no radial packet.
    pub fn tally_against_l3(
        derived: &[Vec<f32>],
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

    /// The lower-level entry point: explicit packet, gate spacing and codec.
    ///
    /// Resampling, cell for cell of the derived grid:
    ///
    /// * **azimuth** through a tenth-of-a-degree table of which packet radial
    ///   covers each 0.1° (the [`crate::srm`] resampler's mapping), read at
    ///   the cell centre `az + 0.5°`, so radials that do not start on whole
    ///   degrees land correctly;
    /// * **range** by taking, per 1-km cell, the packet gate whose centre
    ///   `(first_range_bin + j + 0.5) · gate_km` falls nearest the cell
    ///   centre — both sides' first-bin offsets honoured, sub-kilometre
    ///   products represented by their centre gate.
    ///
    /// The domain is always ≤ 230 km: packet gates beyond it are ignored.
    pub fn tally_packet(
        derived: &[Vec<f32>],
        packet: &RadialPacket,
        l3_gate_km: f64,
        codec: &ValueCodec,
        kind: ProductKind,
    ) -> Tally {
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
            let centre = (packet.first_range_bin as f64 + j as f64 + 0.5) * l3_gate_km;
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

        let mut t = Tally::default();
        for (az, row) in derived.iter().take(360).enumerate() {
            let radial = slots[az * 10 + 5].map(|ri| &packet.radials[ri]);
            for (r, &v) in row.iter().take(RANGE_BINS).enumerate() {
                let l3_level: Option<u16> = radial
                    .and_then(|run| gate_for_bin[r].and_then(|j| run.gate_values.get(j).copied()));
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

/// Finding a twin against the live buckets: native, test-only.
///
/// Everything here pairs by **volume identity**, never by key freshness — see
/// [`live::KEY_LOOKBACK`] for why the newest key is usually the wrong object.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub mod live {
    use crate::archive;
    use crate::level3::{Level3Product, ProductStamp};
    use crate::sources::DataSources;
    use chrono::NaiveDateTime;
    use nexrad_model::data::Scan;

    /// Sites are tried in order until enough tilts line up **with a nonzero
    /// vector**. Two things make a site unusable, and both are common:
    /// tgftp's `sn.last` and the bucket's newest key are frequently one volume
    /// apart, and a quiet site reports 0.0 kt, which zeroes the very term this
    /// is validating. Long and geographically spread so a calm night over any
    /// one region does not starve the sample.
    ///
    /// Shared by every live harness (SRM's tilt validation, the product
    /// twins), so a site quarantined or retired is decided in one place.
    pub const SITES: &[&str] = &[
        "KMPX", "KFSD", "KBIS", "KOAX", "KUEX", "KABR", "KTLX", "KMRX", "KTLH", "KMOB", "KSGF",
        "KPAH", "KMLB", "KMTX", "KSFX", "KMVX", "KLZK", "KSHV", "KEAX", "KDDC", "KAMA", "KFWS",
    ];

    /// How many bucket objects to open looking for a particular volume and
    /// cut, and how far from the wanted time to look.
    ///
    /// The bucket's *newest* key will not do, above all at 0.5°: SAILS
    /// republishes the lowest cut two to four times a volume, so the newest
    /// `N0G` is usually a mid-volume repeat while the wanted volume is some
    /// other cut. Taking the newest key skipped the lowest tilt at two sites
    /// in three on the SRM harness's first run — which would have left the
    /// tilt it validates almost never actually compared.
    pub const KEY_LOOKBACK: usize = 10;
    pub const KEY_WINDOW_MINUTES: i64 = 20;

    /// The bucket keys for `site`/`code` within [`KEY_WINDOW_MINUTES`] of
    /// `want`, nearest first: the matching object is written within seconds
    /// of its sibling, so the first candidate is almost always the answer.
    /// Lists the previous UTC day too, for windows spanning midnight.
    pub async fn candidate_keys(
        sources: &DataSources,
        site: &str,
        code: &str,
        want: NaiveDateTime,
    ) -> Vec<String> {
        let site3 = crate::level3::site_code(site).to_uppercase();
        let mut keys = Vec::new();
        for day in [want.date(), want.date() - chrono::Duration::days(1)] {
            if let Ok(k) = crate::level3::list_day(sources, &site3, code, &day).await {
                keys.extend(k);
            }
        }
        let mut candidates: Vec<(i64, String)> = keys
            .into_iter()
            .filter_map(|k| {
                let t = crate::level3::key_time(&k)?;
                let delta = (t - want).num_seconds().abs();
                (delta <= KEY_WINDOW_MINUTES * 60).then_some((delta, k))
            })
            .collect();
        candidates.sort();
        candidates
            .into_iter()
            .take(KEY_LOOKBACK)
            .map(|(_, k)| k)
            .collect()
    }

    /// The archived Level II volume nearest `when`, as the **raw file**: the
    /// undecoded archive plus the volume start its identifier names — the
    /// timestamp Level III products of the same volume carry in their PDB.
    /// Looks at the previous UTC day too, and skips the `_MDM` sidecars the
    /// listing interleaves.
    ///
    /// The file form exists for the harnesses that need radial-header
    /// parameters a decoded `Scan` does not carry (the KDP twin reads the
    /// initial system differential phase through
    /// [`crate::kdp::KdpParams::from_archive`]).
    pub async fn l2_archive_near(
        site: &str,
        when: NaiveDateTime,
    ) -> Option<(nexrad_data::volume::File, NaiveDateTime)> {
        crate::tls::init();
        let sources = DataSources::production();
        let mut nearest: Option<(i64, NaiveDateTime, archive::Identifier)> = None;
        for day in [when.date(), when.date() - chrono::Duration::days(1)] {
            let Ok(ids) = archive::list_files(&sources, site, &day).await else {
                continue;
            };
            for id in ids {
                if id.name().ends_with("_MDM") {
                    continue;
                }
                let Some(start) = id.date_time() else {
                    continue;
                };
                let start = start.naive_utc();
                let delta = (start - when).num_seconds().abs();
                if nearest.as_ref().is_none_or(|(d, ..)| delta < *d) {
                    nearest = Some((delta, start, id));
                }
            }
        }
        let (_, start, id) = nearest?;
        let file = archive::download_file(&sources, id).await.ok()?;
        Some((file, start))
    }

    /// [`l2_archive_near`], decoded: the scan and its volume start.
    pub async fn l2_volume_near(site: &str, when: NaiveDateTime) -> Option<(Scan, NaiveDateTime)> {
        let (file, start) = l2_archive_near(site, when).await?;
        Some((file.scan().ok()?, start))
    }

    /// The Level III object generated **from** a given Level II volume: list
    /// the day prefix (and the previous day near midnight), walk the
    /// candidates nearest the volume start first, and accept the first whose
    /// PDB names that volume — start times equal within ±60 s — and, when
    /// `elevation_number` is given, that cut.
    ///
    /// Never the newest key: see [`KEY_LOOKBACK`].
    pub async fn l3_twin(
        sources: &DataSources,
        site: &str,
        awips_code: &str,
        l2_volume_start: NaiveDateTime,
        elevation_number: Option<u8>,
    ) -> Option<Level3Product> {
        for key in candidate_keys(sources, site, awips_code, l2_volume_start).await {
            let url = sources.level3_object_url(&key);
            let Ok(bytes) = archive::get_bytes(archive::shared_client(), url).await else {
                continue;
            };
            let Ok(message) = nexrad_level3::decode::decode_product(&bytes) else {
                continue;
            };
            let Some(started) = super::compare::volume_scan_started(&message.pdb) else {
                continue;
            };
            if (started - l2_volume_start).num_seconds().abs() > 60 {
                continue;
            }
            if let Some(cut) = elevation_number
                && message.pdb.elevation_number != u16::from(cut)
            {
                continue;
            }
            return Some(Level3Product {
                message,
                stamp: ProductStamp::from_key(key),
                bytes: std::sync::Arc::new(bytes),
            });
        }
        None
    }

    /// The pairing invariant, live: a recent KOAX volume and its EET twin
    /// must name the same volume start.
    ///
    /// ```text
    /// cargo test -p rustdar-radar --release -- --ignored --nocapture twin
    /// ```
    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_the_eet_twin_names_the_l2_volumes_own_start() {
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();

        let (scan, l2_start) = l2_volume_near("KOAX", now)
            .await
            .expect("KOAX has an archived volume within the last two days");
        println!(
            "L2 volume: start {l2_start}, {} sweeps, VCP {:?}",
            scan.sweeps().len(),
            scan.coverage_pattern_number(),
        );

        let twin = l3_twin(&sources, "KOAX", "EET", l2_start, None)
            .await
            .expect("an EET object generated from that volume");
        let pdb_start = super::compare::volume_scan_started(&twin.message.pdb)
            .expect("the EET PDB carries a volume stamp");
        println!(
            "EET twin: {} (product {}, elevation_number {}), PDB volume start {pdb_start}",
            twin.stamp.key, twin.message.pdb.product_code, twin.message.pdb.elevation_number,
        );

        let skew = (pdb_start - l2_start).num_seconds();
        println!("pairing skew: {skew} s");
        assert!(
            skew.abs() <= 60,
            "the twin's PDB volume start {pdb_start} is not the L2 volume start {l2_start}",
        );
        assert_eq!(twin.message.pdb.product_code, 135, "EET decodes as 135");
    }

    /// The EET decode fix, live: a real product-135 object selects the
    /// mask/scale/offset LUT, and every defined gate decodes to a height in
    /// the ICD's 0–69 kft band — never the raw 130–199 topped levels the
    /// scaled fallback used to report.
    ///
    /// ```text
    /// cargo test -p rustdar-radar --release -- --ignored --nocapture live_eet_codec
    /// ```
    #[ignore = "hits the live S3 bucket"]
    #[tokio::test]
    async fn live_eet_codec_decodes_heights_within_the_icd_band() {
        crate::tls::init();
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();

        // Any real EET object will do — checking the decode's range needs no
        // volume pairing — so take the first candidate that decodes.
        let mut found = None;
        'sites: for &site in SITES {
            for key in candidate_keys(&sources, site, "EET", now).await {
                let url = sources.level3_object_url(&key);
                let Ok(bytes) = archive::get_bytes(archive::shared_client(), url).await else {
                    continue;
                };
                if let Ok(message) = nexrad_level3::decode::decode_product(&bytes) {
                    found = Some((site, key, message));
                    break 'sites;
                }
            }
        }
        let (site, key, message) = found.expect("some site has a recent EET object");
        assert_eq!(message.pdb.product_code, 135);
        println!(
            "{site} {key}: thresholds {:?}",
            &message.pdb.thresholds[..4]
        );

        let codec = super::compare::ValueCodec::for_message(&message)
            .expect("the EET object carries a radial packet");
        assert!(
            matches!(codec, super::compare::ValueCodec::Lut(_)),
            "product 135 must select the EET LUT, not scale/offset",
        );

        let packet = crate::srm::radial_packet(&message).expect("present per the codec");
        let (mut defined, mut topped) = (0usize, 0usize);
        for &gate in packet.radials.iter().flat_map(|r| r.gate_values.iter()) {
            let v = codec.decode(gate);
            if v.is_nan() {
                continue;
            }
            assert!(
                (0.0..=69.0).contains(&v),
                "{site} {key}: gate level {gate} decoded to {v} kft",
            );
            defined += 1;
            topped += usize::from(gate & 0x80 != 0);
        }
        println!("{site} {key}: {defined} defined bins decoded into 0–69 kft ({topped} topped)");
    }
}

#[cfg(test)]
mod tests {
    use super::compare::*;
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
        let t = tally_packet(&derived, &p, 1.0, &codec(), ProductKind::Numeric);
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
        let t = tally_packet(&derived, &p, 1.0, &codec(), ProductKind::Numeric);
        assert_eq!(t.exact, 0);
        assert_eq!(t.within_one, t.compared);
        assert_eq!(t.within_two, t.compared);

        for v in derived.iter_mut().flatten() {
            *v = (103.0 - OFFSET) / SCALE;
        }
        let t = tally_packet(&derived, &p, 1.0, &codec(), ProductKind::Numeric);
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
        let t = tally_packet(&derived, &p, 1.0, &codec(), ProductKind::Numeric);
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
        let t = tally_packet(&derived, &p, 1.0, &codec(), ProductKind::Numeric);
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
        let t = tally_packet(&derived, &p, 1.0, &codec(), ProductKind::Numeric);
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
        let t = tally_packet(&derived, &p, 0.25, &codec(), ProductKind::Numeric);
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
        let t = tally_packet(&derived, &p, 1.0, &codec(), ProductKind::Class);
        assert_eq!(t.confusion.get(&(60, 60)), Some(&(180 * 230)));
        assert_eq!(t.confusion.get(&(80, 60)), Some(&(180 * 230)));
        assert_eq!(t.confusion.values().sum::<usize>(), t.compared);
        assert_eq!(t.exact, 180 * 230);
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

    /// The volume stamp conversion: day 1 is 1970-01-01, so MJD 20661 at
    /// 7108 s is 2026-07-26 01:58:28 — checked against a calendar, not
    /// against the function.
    #[test]
    fn the_volume_stamp_reads_day_one_as_the_epoch() {
        let mut pdb = pdb_with_volume(20661, 7108);
        let t = volume_scan_started(&pdb).expect("a valid stamp");
        assert_eq!(t.to_string(), "2026-07-26 01:58:28");
        assert_eq!(
            volume_scan_started(&pdb_with_volume(1, 0)).map(|t| t.to_string()),
            Some("1970-01-01 00:00:00".to_string()),
            "day 1 is the epoch itself",
        );
        // Day 0 cannot precede the epoch: it is None, not 1969-12-31.
        pdb.volume_scan_date = 0;
        assert!(volume_scan_started(&pdb).is_none());
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
