//! The renderer's input, in a form that can cross a process — or a Web Worker —
//! boundary.
//!
//! [`crate::render`] takes a whole `&Scan`: a decoded volume of tens of
//! megabytes, holding every moment of every radial of every sweep. It *reads*
//! almost none of that. `find_sweep` picks one sweep and the rasterizer then
//! touches only `product.get_moment(radial)` on it — unless the product is one
//! [`RadarProduct::reads_whole_volume`] names, which reaches every tilt
//! carrying its moment, and for the hybrid classification every *other* moment
//! of those tilts too. Nothing reads the coverage pattern, the site, the
//! collection timestamps or the radial statuses.
//!
//! [`RenderInput`] is that reachable subset, flattened. For a normal product it
//! is one sweep: ~1.3 MB for a 720 × 1832 8-bit moment, ~2.6 MB for 16-bit
//! dual-pol. A whole-volume product carries every tilt its moment appears on
//! instead: NROT and SRV every velocity tilt (~10-14 MB), interpolated echo tops
//! and the hail pair every reflectivity tilt (~20 MB). The hybrid classification
//! is the outlier and much the largest — it takes every tilt carrying *any*
//! moment and, on each, the other five moments as well (`RadialData::extras`),
//! several of them 16-bit, so it runs several times the reflectivity figure
//! rather than alongside it.
//! Against a `Scan` even that is a large reduction, and for everything else it
//! is the difference between a payload a browser can post per render and one it
//! cannot.
//!
//! # Why it reconstructs a `Scan` instead of replacing it
//!
//! [`RenderInput::to_scan`] rebuilds a `nexrad_model::data::Scan` holding
//! exactly the extracted sweeps, and [`crate::render::render_from`] runs the
//! ordinary renderer over it. The alternative — reshaping four rasterizers,
//! `build_velocity_grid`, `build_wind_profile` and `VolumeCube` to consume a
//! second input type — would give the project two descriptions of the same
//! data and two chances for them to disagree about a pixel.
//!
//! This way there is one renderer. The web path and the desktop path differ
//! only in *where* the `Scan` came from, and
//! `render_from_an_extracted_payload_matches_the_scan_path` pins that they
//! agree byte for byte.
//!
//! Reconstruction is exact, not approximate:
//! [`nexrad_model::data::MomentData::from_fixed_point`] takes the same
//! fixed-point fields the decoder produced, and the gate bytes are carried raw,
//! so the reconstructed moment decodes to the identical values. The fields
//! `Radial::new` demands but the renderer never reads (collection timestamp,
//! azimuth number, radial status, elevation number) are filled with the
//! placeholders in [`to_scan`](RenderInput::to_scan); if a renderer ever starts
//! reading one, the byte-identity test is what fails.

use crate::types::{MomentSlot, RadarProduct};
use nexrad_model::data::{
    ChannelConfiguration, DataMoment, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus,
    Scan, Sweep, VolumeCoveragePattern, WaveformType,
};

/// Everything [`crate::render::render_from`] needs to produce a frame, and
/// everything [`crate::sampler::VolumeSampler`] needs to build the same tilt
/// ladder the main thread built.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderInput {
    product: RadarProduct,
    /// The elevation the *request* asked for, not the angle any sweep carries.
    /// `find_sweep` re-runs against it on the reconstructed scan and must reach
    /// the same sweep, which is why [`RenderInput::extract`] keeps sweeps in
    /// their original order.
    ///
    /// [`RenderInput::extract_volume`] has no tilt to ask for and stores
    /// [`NO_ELEVATION_DEG`] instead — see that constant for why it is neither
    /// `0.0` nor `NaN`.
    elevation: f32,
    radar_lat: f64,
    radar_lon: f64,
    /// The user's storm motion vector, knots and degrees-from. Read by
    /// storm-relative velocity alone; `None` means "no override", which SRV
    /// answers with the Bunkers right-mover from the volume's own profile.
    storm_motion_override: Option<(f32, f32)>,
    /// The site's environmental 0 °C / −20 °C heights, km MSL
    /// ([`crate::sounding::EnvHeights`]). Read by the hail pair and the
    /// hybrid hydrometeor classification. `None` means different things to
    /// each: the hail field is undefined and its render answers nothing
    /// ([`crate::hail`]), while the HHC falls back to the operational
    /// adaptation defaults, exactly as the RPG does without environmental
    /// data.
    env_heights_km_msl: Option<(f64, f64)>,
    /// The volume coverage pattern number the scan was flown under.
    ///
    /// Nothing on a render path reads it. It travels because the *cut angles*
    /// now do (see [`SweepData::cut_angle_deg`]), and a reconstructed pattern
    /// that carried a real cut table while calling itself VCP 0 would be a
    /// worse artifact than the wholly synthetic pattern this used to build:
    /// [`crate::sampler::SamplerError::EmptyCoveragePattern`] names the VCP in
    /// its message, and `crate::types::ScanInfo::from_scan` — the one reader of
    /// the pattern anywhere in this workspace — puts it in the chrome.
    vcp: u16,
    /// Every cut angle the coverage pattern **declares**, in table order and
    /// exactly as the decoder hands them over — a below-horizon cut arrives as
    /// ~359.7° here, because wrap-correcting on the way in would make this a
    /// different table from the one the main thread keys against.
    ///
    /// # The reconstruction used to top out wherever the volume did
    ///
    /// [`RenderInput::to_scan`] rebuilds the cut table, and before this it
    /// rebuilt it from the *carried sweeps' own* angles, sized to the largest
    /// elevation number in the payload. That keys every carried sweep
    /// correctly, which was all the ladder needed — but it silently loses the
    /// one fact that distinguishes a volume which flew its whole pattern from
    /// one that stopped early. A KLNX section cut three rungs in came back with
    /// a table whose highest angle was 1.3°, so "the ladder reached the top of
    /// its pattern" was true of every volume ever cut in a worker, and a live
    /// section captioned itself complete for the whole six minutes it was not.
    /// Every section goes through this type, so that was every section.
    ///
    /// Carrying the real table also makes the reconstruction *more* faithful
    /// where it was already only nearly so: slots no carried sweep names now
    /// hold their own declared angle instead of a copy of the nearest carried
    /// one.
    declared_cut_angles_deg: Vec<f64>,
    sweeps: Vec<SweepData>,
}

/// One sweep's worth of the product's moment, plus the two fields that let the
/// sweep be keyed back onto its VCP cut.
#[derive(Debug, Clone, PartialEq)]
struct SweepData {
    /// The sweep's **median** elevation
    /// ([`crate::volumetric::sweep_elevation_deg`]) — not its first radial's,
    /// and not a value that may be read off any single radial.
    ///
    /// The model carries elevation per radial, and it is *not* constant across a
    /// sweep: the antenna is still settling when one opens, and the opening
    /// radial can sit a third of a degree from the tilt the sweep actually flew.
    ///
    /// Two things then depend on this being the median, and both fail silently
    /// if it reverts to the first radial:
    ///
    /// * [`RenderInput::to_scan`] stamps this one value onto *every*
    ///   reconstructed radial, so it **is** the reconstructed sweep's median.
    ///   [`crate::render::find_sweep`] matches on the median within
    ///   [`crate::render::ELEVATION_WINDOW`], so a first-radial value here puts
    ///   the payload further from the request than the window allows: the worker
    ///   fails to find the one sweep its own payload carries and the whole wasm
    ///   render path draws nothing.
    /// * Every whole-volume product — echo tops, VIL, the hail pair, the hybrid
    ///   classification, NROT, SRV — builds its tilt ladder by asking
    ///   `sweep_elevation_deg` for each sweep's elevation. On the desktop path
    ///   that reads the real radials; on the web path it reads this field
    ///   copied across them. Anything but the median makes those two paths
    ///   compute *different ladders from the same volume*.
    ///
    /// `render_input::tests::a_sweep_that_opened_off_its_tilt_still_renders_after_the_port`
    /// is the guard; fixtures giving a sweep one constant elevation cannot see
    /// any of this, because for them the median and the first radial are the
    /// same number.
    elevation_angle: f32,
    /// The sweep's own `elevation_number` — the RDA's statement of which cut of
    /// the VCP this sweep is, 1-based.
    ///
    /// **This used to be the sweep's index in the payload**, written as
    /// `si as u8` off an `.enumerate()` in [`to_scan`](RenderInput::to_scan),
    /// which made the first sweep report `0` — a number that cannot index a
    /// 1-based table at all. Nothing noticed, because nothing read it.
    /// [`crate::sampler::VolumeSampler`] does: it is half of the ladder key,
    /// and the wrong half of it is not a degraded ladder but a different one.
    elevation_number: u8,
    /// The angle of the VCP cut `elevation_number` names, **exactly as the cut
    /// table stores it** — not wrap-corrected, not rounded, not the sweep's
    /// median.
    ///
    /// Raw on purpose. The sampler applies its own `key > 180.0 → key - 360.0`
    /// correction for cuts below the horizon, which arrive from the decoder as
    /// ~359.7°; carrying the corrected value would mean the correction had run
    /// once on the main thread and would not run again in the worker, and
    /// carrying the *rounded* value would fuse two cuts that the campaign's
    /// measurement says must stay apart to 0.09°. Raw in, raw out, one
    /// correction on each side of the port, applied to the same number.
    ///
    /// `None` when the scan's own cut table could not answer — an empty table
    /// (a volume joined mid-flight, before its start chunk landed) or an
    /// `elevation_number` that does not index it. The reconstruction then
    /// rebuilds an **empty** cut table, so the sampler refuses the scan exactly
    /// as it refuses the original. Faithful includes faithfully unusable: the
    /// alternative is a ladder in the worker that the main thread would not
    /// have built.
    cut_angle_deg: Option<f64>,
    /// Whether the *original* sweep's radials carried a velocity moment.
    ///
    /// One bit, and it decides which antenna pass a section is cut from.
    ///
    /// [`crate::sampler::VolumeSampler`] resolves a split cut by preferring the
    /// half that carries **no** velocity: reflectivity belongs to the
    /// surveillance half, which reaches 460 km against the Doppler half's 300,
    /// and the two halves are otherwise indistinguishable — on a measured KMPX
    /// VCP 212 volume all three members of the 0.4834° cut report the same cut
    /// angle *and* the same median. The rule discriminates on
    /// `radial.velocity().is_none()`.
    ///
    /// A reflectivity payload carries the reflectivity moment and nothing else,
    /// so before this bit every reconstructed sweep looked like a surveillance
    /// half and the chooser fell through to "newest member" — which on a real
    /// volume is a SAILS *Doppler* repeat. The reconstructed ladder then took
    /// a 1192-gate rung where the main thread took an 1832-gate one, and
    /// nothing failed: the section simply stopped at ~300 km and took the low
    /// tilt's geometry from the wrong pass.
    ///
    /// **The bit, not the decision.** Applying the surveillance preference at
    /// extraction time would put a second copy of the sampler's own rule in
    /// this module, and this campaign has already paid twice for exactly that
    /// duplication. What travels is the input the rule reads; the rule stays
    /// where it is.
    carried_velocity: bool,
    /// The Nyquist velocity this sweep's cut **declared**, m/s, or `None` when
    /// the volume this payload was extracted from declared none for it.
    ///
    /// Message 31's Radial Data Block states where the sweep's Doppler
    /// velocity folds; `nexrad_model::data::Radial` drops it, so it arrives
    /// here through [`crate::nyquist::DeclaredNyquist`] rather than off the
    /// radials, and [`RenderInput::with_declared_nyquist`] is what fills it in.
    ///
    /// # Why it has to cross the wire
    ///
    /// [`crate::sampler::VolumeSampler`] refuses to interpolate velocity
    /// readings that straddle the fold, and it needs the limit per rung. It
    /// has a fallback — `estimate_fold_limit`, off the largest speed the sweep
    /// observed — so a payload without this field still guards. **That is
    /// exactly the hazard.** The main thread would hold the declared number
    /// and the worker's reconstructed scan would hold only the estimate, the
    /// two are usually within a few m/s of each other, and nothing errors,
    /// warns or renders visibly differently: the two threads would simply
    /// classify a band of borderline pairs differently, for as long as the
    /// difference went unnoticed. Either the number crosses or the divergence
    /// is silent, and this is the field that makes it cross.
    ///
    /// `None` is honest and common: a volume decoded entirely from Message 1
    /// has no such field to declare, and a payload extracted without a table
    /// carries none. The worker then estimates, which is what the main thread
    /// does for the same volume.
    declared_nyquist_ms: Option<f64>,
    radials: Vec<RadialData>,
}

#[derive(Debug, Clone, PartialEq)]
struct RadialData {
    azimuth: f32,
    azimuth_spacing: f32,
    /// `None` for a radial that carries no data for this product. Real sweeps
    /// have them, and both `sweep_to_grid` and the rasterizer skip them, so the
    /// distinction has to survive the round trip.
    moment: Option<MomentPayload>,
    /// The radial's *other* moments, tagged by their index into `ALL_SLOTS` —
    /// carried only for the hybrid hydrometeor classification, whose
    /// derivation reads every dual-pol field plus velocity, and empty for
    /// every other product.
    extras: Vec<(u8, MomentPayload)>,
}

/// A moment block in the fixed-point form the decoder produced it in, so
/// `MomentData::from_fixed_point` can rebuild it exactly.
#[derive(Debug, Clone, PartialEq)]
struct MomentPayload {
    gate_count: u16,
    /// Metres. `MomentDataBlock` stores this as a `u16` of metres and exposes
    /// it as `km = raw * 0.001`; the model offers no raw accessor, so the
    /// kilometre value is scaled back and rounded. Exact for every `u16`.
    first_gate_range_m: u16,
    gate_interval_m: u16,
    word_size: u8,
    scale: f32,
    offset: f32,
    /// Raw gate codes, exactly as `DataMoment::raw_values` returns them: one
    /// byte per gate at 8-bit, a big-endian pair at 16-bit.
    gates: Vec<u8>,
}

/// Whether a product reads the environmental 0 °C / −20 °C heights.
///
/// The hail pair has no field at all without them ([`crate::hail`]); the
/// hybrid hydrometeor classification uses them for its melting layer and
/// hail-size heights, falling back to the operational adaptation defaults
/// when they are absent. Every other product must never carry them, so its
/// payload bytes cannot depend on an unrelated cache.
fn reads_env_heights(product: RadarProduct) -> bool {
    matches!(
        product,
        RadarProduct::ProbabilityOfSevereHail
            | RadarProduct::MaxExpectedHailSize
            | RadarProduct::HydrometeorClassification
    )
}

impl RenderInput {
    /// The reachable subset of `scan` for this request, or `None` when the
    /// request cannot be rendered at all.
    ///
    /// `None` is returned exactly where [`crate::render`] would have returned
    /// it: a product with no Level II moment behind it, or no sweep in the
    /// requested tilt family carrying one.
    #[allow(clippy::too_many_arguments)]
    pub fn extract(
        scan: &Scan,
        elevation: f32,
        product: RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
        storm_motion_override: Option<(f32, f32)>,
        env_heights_km_msl: Option<(f64, f64)>,
    ) -> Option<Self> {
        Self::extract_with(
            scan,
            Scope::Tilt(elevation),
            product,
            radar_lat,
            radar_lon,
            storm_motion_override,
            env_heights_km_msl,
        )
    }

    /// The reachable subset of `scan` for a request that reads the **whole
    /// volume** — a cross-section or a voxel grid — or `None` when the volume
    /// carries the product's moment nowhere.
    ///
    /// The arguments [`extract`](Self::extract) takes and this one does not are
    /// the ones that mean nothing here. There is no elevation because there is
    /// no tilt: a section cuts across all of them. There is no environment
    /// because the only products that read one — the hail pair and the
    /// classification — are ones [`crate::derive::volume_slot`] refuses, so
    /// carrying it would make a section payload's bytes depend on caches no
    /// section can consult. The storm motion override *is* carried, since the
    /// vertical views derive SRV ([`crate::derive`]); this entry passes
    /// `None`, and [`extract_volume_parts`](Self::extract_volume_parts) is the
    /// door that takes a real one.
    ///
    /// The stored elevation is [`NO_ELEVATION_DEG`], which is what makes this
    /// safe to hand to a frame consumer by mistake: `render_from` runs
    /// `find_sweep` against it, matches nothing, and answers `None` — "nothing
    /// to draw", a state every path already handles — rather than silently
    /// drawing the base tilt.
    pub fn extract_volume(
        scan: &Scan,
        product: RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
    ) -> Option<Self> {
        Self::extract_with(
            scan,
            Scope::Volume,
            product,
            radar_lat,
            radar_lon,
            None,
            None,
        )
    }

    /// The reachable subset of a volume handed over as **parts** — a pattern
    /// and an ordered sweep list — for a whole-volume request.
    ///
    /// This is [`extract_volume`](Self::extract_volume) for a volume that is
    /// not one `Scan`: the current merged volume ([`crate::current`]) composes
    /// borrowed sweeps from two volumes under one pattern, and this entry
    /// copies the product's moment out of exactly that composition. The
    /// `Scan`-taking constructors delegate here, so there is one extraction,
    /// not a pair that can disagree.
    ///
    /// Sweep order is the caller's and it is load-bearing: the reconstructed
    /// scan preserves it, and every newest-wins rule downstream — `find_sweep`
    /// and the sampler's rung choice — reads "later" as "newer".
    pub fn extract_volume_parts(
        pattern: &VolumeCoveragePattern,
        sweeps: &[&Sweep],
        product: RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
        storm_motion_override: Option<(f32, f32)>,
    ) -> Option<Self> {
        // The slot the vertical views sample through: the native moment, or a
        // derived product's *source* moment — SRV and NROT ride the velocity
        // planes and KDP the ΦDP planes to the worker, which derives there
        // (`crate::derive`).
        let slot = product
            .moment_slot()
            .or_else(|| crate::derive::derived_slot(product))?;
        // The HHC reads moments beyond its slot; so does the KDP derivation,
        // whose estimator gates on Z and ρHV. Everything else ships the slot
        // moment alone.
        let all_moments = matches!(
            product,
            RadarProduct::HydrometeorClassification | RadarProduct::SpecificDifferentialPhase
        );
        let cuts = CutTable::of_pattern(pattern);
        let sweeps = collect_sweeps(sweeps.iter().copied(), &cuts, slot, all_moments);
        // Empty on a volume that carries the product nowhere. The renderer
        // answers `None` for that, so this must too rather than shipping a
        // payload that renders nothing.
        if sweeps.is_empty() {
            return None;
        }
        Some(Self {
            product,
            elevation: NO_ELEVATION_DEG,
            radar_lat,
            radar_lon,
            // Carried for exactly the one product whose derivation reads it,
            // so no other product's payload bytes depend on the storm-motion
            // cache — the byte-identity rule `env_heights_km_msl` follows
            // below, applied here too.
            storm_motion_override: (product == RadarProduct::StormRelativeVelocity)
                .then_some(storm_motion_override)
                .flatten(),
            env_heights_km_msl: None,
            vcp: pattern.pattern_number().number(),
            declared_cut_angles_deg: pattern
                .elevation_cuts()
                .iter()
                .map(ElevationCut::elevation_angle_degrees)
                .collect(),
            sweeps,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn extract_with(
        scan: &Scan,
        scope: Scope,
        product: RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
        storm_motion_override: Option<(f32, f32)>,
        env_heights_km_msl: Option<(f64, f64)>,
    ) -> Option<Self> {
        // The volume scope is the parts extraction run over this scan's own
        // parts — one implementation, whether the volume is one `Scan` or a
        // merged composition.
        if scope == Scope::Volume {
            let sweeps: Vec<&Sweep> = scan.sweeps().iter().collect();
            return Self::extract_volume_parts(
                scan.coverage_pattern(),
                &sweeps,
                product,
                radar_lat,
                radar_lon,
                storm_motion_override,
            );
        }
        let elevation = scope.elevation();
        let slot = product.moment_slot()?;
        // `None` for a Level III product: no Level II moment stands behind it,
        // so there is nothing to extract and nothing the renderer would draw.
        //
        // Some products then need every tilt carrying that moment; anything
        // else needs one sweep. Which is which is
        // [`RadarProduct::reads_whole_volume`], *read* rather than restated:
        // the live chunk feed narrows its download by the same predicate, and
        // a second copy of it here is how an SRV pane came to be handed a
        // volume the feed had skipped cuts of.
        //
        // (A `Scope::Volume` request used to widen it here by `||`; that scope
        // now returns above, through the parts extraction.)
        let whole_volume = product.reads_whole_volume();

        // Only the HHC reads moments beyond its slot; everything else ships
        // the slot moment alone.
        let all_moments = product == RadarProduct::HydrometeorClassification;
        let cuts = CutTable::of(scan);
        let sweeps = if whole_volume {
            collect_sweeps(scan.sweeps().iter(), &cuts, slot, all_moments)
        } else {
            // One sweep: whichever `find_sweep` would have chosen. Selecting
            // here, against the whole volume, is the point — the reconstructed
            // scan has only this sweep to offer, so `find_sweep` reaches it
            // again whatever its preference rules do.
            let sweep = crate::render::find_sweep_owner(scan, product, elevation)?;
            vec![sweep_data(sweep, &cuts, slot, false)]
        };
        // `collect_sweeps` can come back empty on a volume that carries the
        // product nowhere. The renderer answers `None` for that, so this must
        // too rather than shipping a payload that renders nothing.
        if sweeps.is_empty() {
            return None;
        }

        Some(Self {
            product,
            elevation,
            radar_lat,
            radar_lon,
            storm_motion_override,
            env_heights_km_msl: if reads_env_heights(product) {
                env_heights_km_msl
            } else {
                // Nothing else reads them; carrying them anyway would make
                // byte-identity of other products' payloads depend on an
                // unrelated cache.
                None
            },
            vcp: scan.coverage_pattern().pattern_number().number(),
            declared_cut_angles_deg: scan
                .coverage_pattern()
                .elevation_cuts()
                .iter()
                .map(ElevationCut::elevation_angle_degrees)
                .collect(),
            sweeps,
        })
    }

    pub fn product(&self) -> RadarProduct {
        self.product
    }

    pub fn elevation(&self) -> f32 {
        self.elevation
    }

    pub fn radar_lat(&self) -> f64 {
        self.radar_lat
    }

    pub fn radar_lon(&self) -> f64 {
        self.radar_lon
    }

    /// The user's storm motion vector, knots and degrees-from, or `None`
    /// for "no override" — Bunkers applies.
    pub fn storm_motion_override(&self) -> Option<(f32, f32)> {
        self.storm_motion_override
    }

    /// The site's environmental 0 °C / −20 °C heights, km MSL, or `None` —
    /// the hail products then render nothing, and the HHC applies its
    /// adaptation defaults.
    pub fn env_heights_km_msl(&self) -> Option<(f64, f64)> {
        self.env_heights_km_msl
    }

    /// Stamp each carried sweep with the Nyquist velocity its cut declared,
    /// looked up in `declared` by the sweep's own elevation number.
    ///
    /// # Why this is a second step rather than an extraction argument
    ///
    /// [`sweep_data`] builds a `SweepData` out of a `Sweep` and the cut table,
    /// and everything else in the payload is derivable from those. The
    /// declared Nyquist velocity is not: `nexrad_model::data::Radial` dropped
    /// it at the decoder, so it can only come from a table the *caller* is
    /// holding — [`crate::scan::DecodedScan`]'s, the chunk feed's, or the
    /// merged current volume's. Threading it into
    /// [`extract`](Self::extract), [`extract_volume`](Self::extract_volume)
    /// and [`extract_volume_parts`](Self::extract_volume_parts) would put an
    /// argument in three signatures — two of them already at clippy's
    /// argument ceiling — that almost every caller would spell as "nothing",
    /// and it would make the *absence* of a declaration the thing every test
    /// fixture has to write out.
    ///
    /// So the payload extracts without it and is stamped by the callers who
    /// hold one. Forgetting the second step degrades rather than breaks: the
    /// sweep carries `None`, the worker estimates the fold limit, and that is
    /// what every path did before this field existed. What must not happen is
    /// the *asymmetric* case — one side declaring, the other estimating —
    /// which is why the same table that feeds the sampler on this thread is
    /// the one stamped here.
    ///
    /// Sweeps `declared` does not name keep whatever they had, so stamping
    /// with an empty table is a no-op rather than an erasure.
    #[must_use]
    pub fn with_declared_nyquist(mut self, declared: &crate::nyquist::DeclaredNyquist) -> Self {
        for sweep in &mut self.sweeps {
            if let Some(ms) = declared.get(sweep.elevation_number) {
                sweep.declared_nyquist_ms = Some(ms);
            }
        }
        self
    }

    /// The declared Nyquist table this payload carries, rebuilt from its
    /// sweeps — the reverse of [`with_declared_nyquist`](Self::with_declared_nyquist).
    ///
    /// [`to_scan`](Self::to_scan) cannot carry it (the model type is what
    /// dropped it in the first place), so a worker holding a payload gets the
    /// scan from one call and the table from this one, and pairs them in a
    /// [`crate::nyquist::Volume`] for the sampler. Empty when no sweep carried
    /// a declaration, which the sampler reads as "estimate every rung".
    pub fn declared_nyquist(&self) -> crate::nyquist::DeclaredNyquist {
        self.sweeps
            .iter()
            .filter_map(|s| s.declared_nyquist_ms.map(|ms| (s.elevation_number, ms)))
            .collect()
    }

    /// A `Scan` holding exactly the extracted sweeps.
    ///
    /// Nothing on any render path reads the site, or a radial's timestamp,
    /// azimuth number or status. The moments are rebuilt from their fixed-point
    /// fields and raw gate bytes, so they decode to the identical values.
    ///
    /// # The coverage pattern is rebuilt, and it used to be a placeholder
    ///
    /// [`crate::sampler::VolumeSampler`] keys its tilt ladder on
    /// `coverage_pattern().elevation_cuts()[sweep.elevation_number() - 1]`, a
    /// rule settled by measurement over 203 volumes because **no angular
    /// threshold can substitute for it**. Both halves of that expression used
    /// to be broken here, in ways that do not announce themselves:
    ///
    /// * the cut table was empty, so nothing could be indexed at all; and
    /// * `elevation_number` was the sweep's *index in the payload*, so the
    ///   first sweep reported `0`, which cannot index a 1-based table.
    ///
    /// So the table is rebuilt from the angles the payload now carries, sized
    /// to the largest elevation number in it, and each carried sweep's slot
    /// holds the angle its own cut had. Slots no carried sweep names are filled
    /// with a **copy of the nearest carried angle** rather than a sentinel:
    /// they are unreachable from this scan's sweeps by construction, and a
    /// `NaN` or a wild value sitting in a table someone later decides to scan
    /// linearly is a landmine for no gain. Every other field of every cut is
    /// left at a neutral default — the ladder reads the angle and nothing else,
    /// and a fabricated SAILS flag would be a lie a consumer could act on.
    ///
    /// If any carried sweep has no cut angle (see
    /// [`SweepData::cut_angle_deg`]), the table is rebuilt **empty**, which is
    /// what the original looked like and what the sampler refuses. The
    /// reconstruction is faithful, including when the thing it is faithful to
    /// cannot be sampled.
    pub fn to_scan(&self) -> Scan {
        // Always `Some`: both constructors refuse a product with no Level II
        // field. Degrading to "no moments" rather than panicking keeps a
        // hand-crafted payload off a message port from taking the tab down; it
        // renders nothing, which is what such a request means anyway.
        //
        // The same slot resolution `extract_volume_parts` writes through: the
        // native slot, or a derived product's *source* slot — KDP's primary
        // moment is ΦDP, and reading only `moment_slot` here dropped it on
        // the floor while the extras (which exclude the slot) survived, so a
        // KDP payload reconstructed with reflectivity and ρHV and no phase.
        // `the_kdp_payload_round_trips_its_phase` pins the pair.
        let slot = self
            .product
            .moment_slot()
            .or_else(|| crate::derive::derived_slot(self.product));
        let sweeps = self
            .sweeps
            .iter()
            .map(|sweep| {
                let radials = sweep
                    .radials
                    .iter()
                    .map(|radial| {
                        let moment = radial.moment.as_ref().map(MomentPayload::to_moment_data);
                        // Put back on the field it was read from — the same
                        // `MomentSlot` `get_moment` resolves this product to,
                        // so the reconstructed radial answers `get_moment` with
                        // the moment that was extracted.
                        let mut slots = place_moment(slot, moment);
                        // The extras go back on the fields their tags name —
                        // the HHC's full-radial reconstruction.
                        for (code, payload) in &radial.extras {
                            if let Some(extra_slot) = ALL_SLOTS.get(*code as usize) {
                                place_into(&mut slots, *extra_slot, payload.to_moment_data());
                            }
                        }
                        // The Doppler-half marker. Only when the sweep really
                        // carried velocity and none of it travelled — for the
                        // hybrid classification, whose payload carries every
                        // moment, the real thing is already in the slot and
                        // this does nothing.
                        if sweep.carried_velocity && slots.1.is_none() {
                            slots.1 = Some(doppler_marker());
                        }
                        let (reflectivity, velocity, spectrum_width, zdr, phi, rho) = slots;
                        Radial::new(
                            0,
                            0,
                            radial.azimuth,
                            radial.azimuth_spacing,
                            RadialStatus::Unknown(0),
                            sweep.elevation_number,
                            sweep.elevation_angle,
                            reflectivity,
                            velocity,
                            spectrum_width,
                            zdr,
                            phi,
                            rho,
                            None,
                        )
                    })
                    .collect();
                Sweep::new(sweep.elevation_number, radials)
            })
            .collect();

        Scan::new(self.coverage_pattern(), sweeps)
    }

    /// The coverage pattern [`to_scan`](Self::to_scan) rebuilds. See its doc
    /// for why the table is sized this way and why the unclaimed slots are
    /// filled the way they are.
    fn coverage_pattern(&self) -> VolumeCoveragePattern {
        // One missing angle is enough: a table with a hole in it would key some
        // sweeps and mis-key the rest, which is worse than keying none.
        let angles: Option<Vec<(usize, f64)>> = self
            .sweeps
            .iter()
            .map(|s| {
                let index = usize::from(s.elevation_number).checked_sub(1)?;
                Some((index, s.cut_angle_deg?))
            })
            .collect();
        let Some(angles) = angles else {
            return placeholder_coverage_pattern(self.vcp);
        };
        let Some(len) = angles.iter().map(|(i, _)| i + 1).max() else {
            // No sweeps at all. `extract_with` refuses that, so this is only
            // reachable from a hand-built payload; an empty table is the honest
            // answer and the sampler refuses it.
            return placeholder_coverage_pattern(self.vcp);
        };
        // The declared table, when the payload carries one that can key every
        // sweep in it. This is the whole table the radar was flying, not the
        // part of it this volume got to, which is the difference between a
        // section that knows it stopped early and one that cannot tell. The
        // reconstruction below stands in only for a payload built by hand or by
        // an older sender, and it is kept rather than removed because it is
        // what makes the fallback a *worse table* rather than no table.
        if self.declared_cut_angles_deg.len() >= len {
            return rebuild_pattern(self.vcp, &self.declared_cut_angles_deg);
        }
        let mut table = vec![None; len];
        for (index, angle) in &angles {
            table[*index] = Some(*angle);
        }
        // Unclaimed slots take the nearest claimed angle. Unreachable from this
        // scan's sweeps either way; this keeps the table free of values a later
        // linear scan would have to special-case.
        let filler = angles[0].1;
        let mut last = filler;
        let angles: Vec<f64> = table
            .iter()
            .map(|slot| {
                last = slot.unwrap_or(last);
                last
            })
            .collect();
        rebuild_pattern(self.vcp, &angles)
    }
}

/// A coverage pattern carrying `angles` and nothing else.
///
/// Every other field is left at a neutral default, which is the same decision
/// [`elevation_cut`] makes per cut and for the same reason: the ladder reads
/// the angle, and a fabricated SAILS flag or PRF number would be a lie a
/// consumer could act on. Shared by the declared table and the reconstructed
/// one so the two cannot come to differ in anything but their angles.
fn rebuild_pattern(vcp: u16, angles: &[f64]) -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        vcp,
        0,
        0.5,
        PulseWidth::Unknown,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        angles.iter().copied().map(elevation_cut).collect(),
    )
}

/// How much of the volume a request reads, and — for a tilt request — which
/// tilt.
///
/// One private enum rather than an `Option<f32>` argument: "no elevation" and
/// "elevation `None`" would be the same value with two meanings, and the
/// second is not a state [`RenderInput::extract`] has.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Scope {
    /// One tilt, chosen by `find_sweep` against this angle — unless the
    /// product's own [`RadarProduct::reads_whole_volume`] widens it anyway.
    Tilt(f32),
    /// Every tilt carrying the moment, whatever the product says.
    Volume,
}

impl Scope {
    fn elevation(self) -> f32 {
        match self {
            Self::Tilt(elevation) => elevation,
            Self::Volume => NO_ELEVATION_DEG,
        }
    }
}

/// The elevation an [`RenderInput::extract_volume`] payload carries: an angle
/// no sweep can match.
///
/// It exists so that a whole-volume payload handed to a *frame* consumer —
/// a section payload routed to a plan-view pane, say — answers `None` rather
/// than quietly drawing whatever tilt happened to be nearest.
///
/// Two obvious choices are wrong, and both were considered:
///
/// * **`0.0` is not unmatchable.** `find_sweep` matches within
///   `render::ELEVATION_WINDOW` of a sweep's *median*, and the settling drift
///   this module already measures puts a real base tilt as low as 0.283°. A
///   below-horizon cut goes lower still. `0.0` would find one.
/// * **`NaN` breaks the type.** `RenderInput` derives `PartialEq`, and
///   `NaN != NaN` would make a whole-volume payload unequal to itself — which
///   is precisely the failure `CrossSection` and `VoxelGrid` hand-write their
///   `PartialEq` to avoid, and which every round-trip assertion in this module
///   would then fail on.
///
/// `-1000.0` is finite, orders of magnitude outside the ±90° an elevation can
/// occupy at all, and survives the `f32` wire round trip exactly.
pub const NO_ELEVATION_DEG: f32 = -1000.0;

/// The scan's elevation cut angles, indexed the way a sweep's
/// `elevation_number` indexes them.
///
/// Reading the table once per extraction rather than per sweep, because
/// `elevation_cuts()` is a slice off the pattern and the pattern is behind two
/// accessors.
struct CutTable<'a> {
    angles: &'a [ElevationCut],
}

impl<'a> CutTable<'a> {
    fn of(scan: &'a Scan) -> Self {
        Self::of_pattern(scan.coverage_pattern())
    }

    fn of_pattern(pattern: &'a VolumeCoveragePattern) -> Self {
        Self {
            angles: pattern.elevation_cuts(),
        }
    }

    /// The raw angle of the cut `elevation_number` names, or `None` when the
    /// table cannot answer — see [`SweepData::cut_angle_deg`].
    fn angle_for(&self, elevation_number: u8) -> Option<f64> {
        let index = usize::from(elevation_number).checked_sub(1)?;
        Some(self.angles.get(index)?.elevation_angle_degrees())
    }
}

/// A velocity moment with **no gates**: the reconstructed statement of
/// [`SweepData::carried_velocity`].
///
/// The sampler's split-cut rule reads `radial.velocity().is_none()`, so the bit
/// has to be materialised on the field the rule looks at — a `Radial` has no
/// other channel, every one of its fields being structural.
///
/// Zero gates, not fabricated ones. A consumer that reads this moment's values
/// gets an empty list, which is the honest answer to "what velocity did this
/// payload carry" — it carried none. What it must never do is invent numbers a
/// wind fit or a dealiaser could take for measurements.
///
/// Nothing else on a render path is misled by it. Every whole-volume product
/// that reads velocity — NROT and SRV — carries the *real* velocity as its slot
/// moment, and the hybrid classification carries it in the extras, so in every
/// case where a consumer reads velocity values the marker is not there. The one
/// path that reads the field's mere *presence* is
/// [`crate::render::find_sweep`]'s surveillance preference, which is the same
/// question the marker exists to answer, and which falls back to any sweep — so
/// a single-sweep payload is still found.
fn doppler_marker() -> MomentData {
    MomentData::from_fixed_point(0, 0, 0, 8, 1.0, 0.0, Vec::new())
}

/// One reconstructed cut: the angle, and neutral values everywhere else.
///
/// The neutral values are not a guess at what the RDA sent. Nothing this crate
/// has reads any other field of a cut, and inventing a plausible SAILS flag or
/// PRF number would be a fabrication a future consumer could act on, where an
/// obviously blank one is a gap it will notice.
fn elevation_cut(elevation_angle_degrees: f64) -> ElevationCut {
    ElevationCut::new(
        elevation_angle_degrees,
        ChannelConfiguration::Unknown,
        WaveformType::Unknown,
        0.0,
        false,
        false,
        false,
        false,
        0,
        0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    )
}

/// Every sweep whose first radial carries `slot`'s moment, in input order.
/// With `all_moments` (the HHC), a sweep carrying *any* moment qualifies —
/// the split-cut Doppler halves carry no differential phase but donate the
/// velocity the classification grafts in.
fn collect_sweeps<'s>(
    sweeps: impl Iterator<Item = &'s Sweep>,
    cuts: &CutTable<'_>,
    slot: MomentSlot,
    all_moments: bool,
) -> Vec<SweepData> {
    sweeps
        .filter_map(|sweep| {
            let radials = sweep.radials();
            let first = radials.first()?;
            let wanted = if all_moments {
                ALL_SLOTS.iter().any(|s| s.read(first).is_some())
            } else {
                slot.read(first).is_some()
            };
            wanted.then(|| sweep_data(sweep, cuts, slot, all_moments))
        })
        .collect()
}

/// Every moment field a radial has, in `Radial::new` order — the extras'
/// tag bytes are indices into this table.
const ALL_SLOTS: [MomentSlot; 6] = [
    MomentSlot::Reflectivity,
    MomentSlot::Velocity,
    MomentSlot::SpectrumWidth,
    MomentSlot::DifferentialReflectivity,
    MomentSlot::DifferentialPhase,
    MomentSlot::CorrelationCoefficient,
];

/// Flatten one sweep, carrying `slot`'s moment and nothing else.
///
/// `slot` comes from the caller rather than being probed off the radial: a
/// merged upper tilt carries reflectivity *and* velocity, so "the first moment
/// this radial has" would hand a reflectivity render the velocity gates.
fn sweep_data(
    sweep: &Sweep,
    cuts: &CutTable<'_>,
    slot: MomentSlot,
    all_moments: bool,
) -> SweepData {
    let radials = sweep.radials();
    SweepData {
        // The sweep's **median**, and it has to be: `to_scan` stamps this one
        // value onto every reconstructed radial, so it is the median of the
        // reconstructed sweep as well, and `find_sweep` — which matches on the
        // median — reaches the same sweep on both sides of the port. Carrying
        // the first radial's angle here instead would have left the payload
        // describing a tilt the sweep never flew, and, since the first radial
        // can sit a third of a degree off, `find_sweep` would have failed to
        // find the one sweep the payload contains and the worker path would
        // have rendered nothing at all.
        elevation_angle: crate::volumetric::sweep_elevation_deg(radials)
            .map(|e| e as f32)
            .unwrap_or(0.0),
        // The **sweep's** number, not the first radial's and not the payload
        // index. `Sweep::new` takes it separately from the radials, so the two
        // are separate claims; the sampler reads this one.
        elevation_number: sweep.elevation_number(),
        cut_angle_deg: cuts.angle_for(sweep.elevation_number()),
        // Read off the first radial, which is where every other per-sweep
        // property in this module is read from and where the sampler's own
        // chooser reads it.
        carried_velocity: radials
            .first()
            .is_some_and(|r| MomentSlot::Velocity.read(r).is_some()),
        // Filled in afterwards by `with_declared_nyquist`, never here: the
        // number is not on the radials — the model type dropped it — so this
        // function, which reads a `Sweep` and nothing else, has no honest way
        // to know it. See that method for why the two steps are separate.
        declared_nyquist_ms: None,
        radials: radials
            .iter()
            .map(|radial| RadialData {
                azimuth: radial.azimuth_angle_degrees(),
                azimuth_spacing: radial.azimuth_spacing_degrees(),
                moment: slot.read(radial).map(MomentPayload::from_moment_data),
                extras: if all_moments {
                    ALL_SLOTS
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| **s != slot)
                        .filter_map(|(code, s)| {
                            s.read(radial)
                                .map(|m| (code as u8, MomentPayload::from_moment_data(m)))
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            })
            .collect(),
    }
}

/// The six `Option<MomentData>` arguments `Radial::new` takes, in its order.
type MomentSlots = (
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
    Option<MomentData>,
);

/// Put `moment` back on the field it was read from.
///
/// The inverse of [`MomentSlot::read`], and the reason `MomentSlot` exists:
/// `get_moment` can only fetch, and rebuilding a radial needs the field named.
///
/// `slot` is `None` only for a product with no Level II field, which neither
/// constructor produces; the moment is then dropped rather than guessed at.
fn place_moment(slot: Option<MomentSlot>, moment: Option<MomentData>) -> MomentSlots {
    let mut slots: MomentSlots = (None, None, None, None, None, None);
    let Some(slot) = slot else { return slots };
    let Some(moment) = moment else { return slots };
    place_into(&mut slots, slot, moment);
    slots
}

/// Set one field of the six-slot tuple.
fn place_into(slots: &mut MomentSlots, slot: MomentSlot, moment: MomentData) {
    match slot {
        MomentSlot::Reflectivity => slots.0 = Some(moment),
        MomentSlot::Velocity => slots.1 = Some(moment),
        MomentSlot::SpectrumWidth => slots.2 = Some(moment),
        MomentSlot::DifferentialReflectivity => slots.3 = Some(moment),
        MomentSlot::DifferentialPhase => slots.4 = Some(moment),
        MomentSlot::CorrelationCoefficient => slots.5 = Some(moment),
    }
}

/// A pattern with **no cuts**, for a payload that could not carry them.
///
/// This is what [`to_scan`](RenderInput::to_scan) used to build for every
/// payload, and now builds only for one that has no cut angles to rebuild from
/// — which is the same shape `crate::chunks`' own placeholder has for a volume
/// joined mid-flight, before its start chunk landed. An empty cut table is what
/// [`crate::sampler::VolumeSampler`] refuses, and that refusal is the point:
/// the original could not have been sampled either.
///
/// `pub(crate)` so [`crate::render`]'s own tests can build a `Scan` without a
/// second synthetic pattern that could drift from this one. Pattern number 0 is
/// not a real VCP, which is why it is the default the tests pass.
pub(crate) fn placeholder_coverage_pattern(pattern_number: u16) -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        pattern_number,
        0,
        0.0,
        PulseWidth::Unknown,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    )
}

impl MomentPayload {
    fn from_moment_data(moment: &MomentData) -> Self {
        Self {
            gate_count: moment.gate_count(),
            first_gate_range_m: km_to_metres(moment.first_gate_range_km()),
            gate_interval_m: km_to_metres(moment.gate_interval_km()),
            word_size: moment.data_word_size(),
            scale: moment.scale(),
            offset: moment.offset(),
            gates: moment.raw_values().to_vec(),
        }
    }

    fn to_moment_data(&self) -> MomentData {
        MomentData::from_fixed_point(
            self.gate_count,
            self.first_gate_range_m,
            self.gate_interval_m,
            self.word_size,
            self.scale,
            self.offset,
            self.gates.clone(),
        )
    }
}

/// Undo `MomentDataBlock`'s `raw as f64 * 0.001`.
///
/// `0.001` is not exact in binary, so the product is not exactly the integer
/// metres that went in; rounding recovers it, and does so for every `u16` the
/// field can hold.
fn km_to_metres(km: f64) -> u16 {
    (km * 1000.0).round().clamp(0.0, u16::MAX as f64) as u16
}

// ── Codec ────────────────────────────────────────────────────────────────────

/// Identifies the payload, so a message that is not one fails on its first four
/// bytes instead of being read as a wildly-sized allocation.
const MAGIC: [u8; 4] = *b"RDRI";

/// Bumped whenever the layout below changes. The two ends of a worker boundary
/// can be different builds — see `rustdar-web`'s build-token handshake — so a
/// mismatch has to be a clean `None`, not a misparse.
///
/// Version 2 added the storm motion override between the wind levels and the
/// sweep count, when storm-relative velocity became a Level II product.
/// Version 3 removed the wind levels: the dealias-seeding profile is fit from
/// the payload's own velocity tilts, and the NVW fetch that used to supply
/// external levels is gone.
/// Version 4 added the environmental heights between the override and the
/// sweep count, for the hail products.
/// Version 5 added the per-radial extra moments, when the hybrid hydrometeor
/// classification became a Level II product: it composites every dual-pol
/// moment of every tilt, so its payload carries them alongside the sweep's
/// own moment, and it reads the same environmental heights the hail pair
/// does.
/// Version 6 added the coverage pattern number, and per sweep the
/// `elevation_number`, the VCP cut angle and the carried-velocity bit, when the
/// volume sampler became reachable from a worker. Those four are the whole
/// input to the tilt ladder: the first three let [`RenderInput::to_scan`]
/// rebuild a cut table, and the fourth is what resolves a split cut. Without
/// any of them a reconstructed scan builds a *different ladder* from the one
/// the main thread built — silently, since none of the failures errors and none
/// produces a `NaN`.
/// Version 7 added the coverage pattern's **whole declared cut-angle table**,
/// after the pattern number. Version 6's table was rebuilt from the carried
/// sweeps alone, which keys every carried sweep correctly and tops out wherever
/// the volume did — so a reconstructed scan could not tell a pattern it had
/// flown to the top from one it had stopped a third of the way up, and every
/// cross-section in the app is cut from a reconstructed scan.
/// Version 8 added the per-sweep **declared Nyquist velocity**, after the cut
/// angle. The sampler's velocity fold guard used to estimate that limit off the
/// data on both sides of the port, because the archive's own statement of it
/// (Message 31's Radial Data Block) is dropped by `nexrad_model::data::Radial`.
/// Now that the main thread reads it, a payload that did not carry it would
/// leave the worker estimating while the main thread declared — two ladders
/// guarding a band of borderline velocity pairs differently, with no error, no
/// warning and no visible difference to point at.
const FORMAT_VERSION: u16 = 8;

impl RenderInput {
    /// Encode for transport. Little-endian throughout; gate blobs are copied
    /// verbatim, which is where nearly all the bytes are.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&self.product.wire_code().to_le_bytes());
        out.extend_from_slice(&self.elevation.to_le_bytes());
        out.extend_from_slice(&self.radar_lat.to_le_bytes());
        out.extend_from_slice(&self.radar_lon.to_le_bytes());

        match self.storm_motion_override {
            None => out.push(0),
            Some((speed_kt, direction_deg)) => {
                out.push(1);
                out.extend_from_slice(&speed_kt.to_le_bytes());
                out.extend_from_slice(&direction_deg.to_le_bytes());
            }
        }

        match self.env_heights_km_msl {
            None => out.push(0),
            Some((h0c, hm20c)) => {
                out.push(1);
                out.extend_from_slice(&h0c.to_le_bytes());
                out.extend_from_slice(&hm20c.to_le_bytes());
            }
        }

        out.extend_from_slice(&self.vcp.to_le_bytes());
        out.extend_from_slice(&(self.declared_cut_angles_deg.len() as u32).to_le_bytes());
        for angle in &self.declared_cut_angles_deg {
            out.extend_from_slice(&angle.to_le_bytes());
        }
        out.extend_from_slice(&(self.sweeps.len() as u32).to_le_bytes());
        for sweep in &self.sweeps {
            out.extend_from_slice(&sweep.elevation_angle.to_le_bytes());
            out.push(sweep.elevation_number);
            out.push(u8::from(sweep.carried_velocity));
            match sweep.cut_angle_deg {
                None => out.push(0),
                Some(angle) => {
                    out.push(1);
                    out.extend_from_slice(&angle.to_le_bytes());
                }
            }
            match sweep.declared_nyquist_ms {
                None => out.push(0),
                Some(ms) => {
                    out.push(1);
                    out.extend_from_slice(&ms.to_le_bytes());
                }
            }
            out.extend_from_slice(&(sweep.radials.len() as u32).to_le_bytes());
            for radial in &sweep.radials {
                out.extend_from_slice(&radial.azimuth.to_le_bytes());
                out.extend_from_slice(&radial.azimuth_spacing.to_le_bytes());
                match &radial.moment {
                    None => out.push(0),
                    Some(moment) => {
                        out.push(1);
                        encode_moment(&mut out, moment);
                    }
                }
                out.push(radial.extras.len() as u8);
                for (code, payload) in &radial.extras {
                    out.push(*code);
                    encode_moment(&mut out, payload);
                }
            }
        }
        out
    }

    /// Decode a payload [`to_bytes`](Self::to_bytes) produced.
    ///
    /// `None` on anything malformed — wrong magic, unknown version, truncation,
    /// a product code this build does not have. Every length is checked against
    /// what remains before it is used, so a corrupt frame cannot ask for a
    /// large allocation.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.take(4)? != MAGIC {
            return None;
        }
        if r.u16()? != FORMAT_VERSION {
            return None;
        }
        let product = RadarProduct::from_wire_code(r.u16()?)?;
        // The same refusal the extractors make: a native slot or a derived
        // product's source slot. A payload naming a product with neither has
        // no moment any field could hold, so it could only ever render
        // nothing; refusing it here keeps that from looking like a renderer
        // that found no sweep. (KDP passes through the derived arm — its
        // primary payload is ΦDP.)
        product
            .moment_slot()
            .or_else(|| crate::derive::derived_slot(product))?;
        let elevation = r.f32()?;
        let radar_lat = r.f64()?;
        let radar_lon = r.f64()?;

        let storm_motion_override = match r.u8()? {
            0 => None,
            1 => Some((r.f32()?, r.f32()?)),
            _ => return None,
        };

        let env_heights_km_msl = match r.u8()? {
            0 => None,
            1 => Some((r.f64()?, r.f64()?)),
            _ => return None,
        };

        let vcp = r.u16()?;
        // Eight bytes per angle, so the claimed count is measured against what
        // remains before it becomes a capacity.
        let declared_count = r.u32()?;
        let mut declared_cut_angles_deg = Vec::with_capacity(r.bounded(declared_count, 8)?);
        for _ in 0..declared_count {
            declared_cut_angles_deg.push(r.f64()?);
        }
        let sweep_count = r.u32()?;
        // A sweep costs at least its own header, so this bounds the count
        // against what is actually left rather than trusting it.
        let mut sweeps = Vec::with_capacity(r.bounded(sweep_count, 12)?);
        for _ in 0..sweep_count {
            let elevation_angle = r.f32()?;
            let elevation_number = r.u8()?;
            let carried_velocity = match r.u8()? {
                0 => false,
                1 => true,
                _ => return None,
            };
            let cut_angle_deg = match r.u8()? {
                0 => None,
                1 => Some(r.f64()?),
                _ => return None,
            };
            let declared_nyquist_ms = match r.u8()? {
                0 => None,
                1 => Some(r.f64()?),
                _ => return None,
            };
            let radial_count = r.u32()?;
            let mut radials = Vec::with_capacity(r.bounded(radial_count, 9)?);
            for _ in 0..radial_count {
                let azimuth = r.f32()?;
                let azimuth_spacing = r.f32()?;
                let moment = match r.u8()? {
                    0 => None,
                    1 => Some(decode_moment(&mut r)?),
                    _ => return None,
                };
                let extra_count = r.u8()?;
                let mut extras = Vec::with_capacity(r.bounded(extra_count as u32, 16)?);
                for _ in 0..extra_count {
                    let code = r.u8()?;
                    // A tag outside the slot table means the two ends
                    // disagree about the layout; refuse the frame.
                    if code as usize >= ALL_SLOTS.len() {
                        return None;
                    }
                    extras.push((code, decode_moment(&mut r)?));
                }
                radials.push(RadialData {
                    azimuth,
                    azimuth_spacing,
                    moment,
                    extras,
                });
            }
            sweeps.push(SweepData {
                elevation_angle,
                elevation_number,
                cut_angle_deg,
                carried_velocity,
                declared_nyquist_ms,
                radials,
            });
        }

        // Trailing bytes mean the two ends disagree about the layout even
        // though the version matched. Better to refuse than to render half a
        // frame from it.
        r.at_end().then_some(Self {
            product,
            elevation,
            radar_lat,
            radar_lon,
            storm_motion_override,
            env_heights_km_msl,
            vcp,
            declared_cut_angles_deg,
            sweeps,
        })
    }

    fn encoded_len(&self) -> usize {
        let header = 4 + 2 + 2 + 4 + 8 + 8;
        let motion = 1 + if self.storm_motion_override.is_some() {
            8
        } else {
            0
        };
        let env = 1 + if self.env_heights_km_msl.is_some() {
            16
        } else {
            0
        };
        let sweeps: usize = self
            .sweeps
            .iter()
            .map(|s| {
                // 4 elevation angle + 1 elevation number + 1 carried-velocity
                // flag + 1 cut-angle flag (+ 8 for the angle) + 1
                // declared-Nyquist flag (+ 8 for the value) + 4 radial count.
                12 + if s.cut_angle_deg.is_some() { 8 } else { 0 }
                    + if s.declared_nyquist_ms.is_some() {
                        8
                    } else {
                        0
                    }
                    + s.radials
                        .iter()
                        .map(|r| {
                            10 + r.moment.as_ref().map_or(0, |m| 19 + m.gates.len())
                                + r.extras
                                    .iter()
                                    .map(|(_, m)| 20 + m.gates.len())
                                    .sum::<usize>()
                        })
                        .sum::<usize>()
            })
            .sum();
        // `+ 2` for the coverage pattern number, `+ 4` and its `f64`s for the
        // declared cut table, `+ 4` for the sweep count.
        let declared = 4 + self.declared_cut_angles_deg.len() * 8;
        header + motion + env + 2 + declared + 4 + sweeps
    }
}

/// One moment payload's wire form, shared by the slot moment and the extras.
fn encode_moment(out: &mut Vec<u8>, moment: &MomentPayload) {
    out.extend_from_slice(&moment.gate_count.to_le_bytes());
    out.extend_from_slice(&moment.first_gate_range_m.to_le_bytes());
    out.extend_from_slice(&moment.gate_interval_m.to_le_bytes());
    out.push(moment.word_size);
    out.extend_from_slice(&moment.scale.to_le_bytes());
    out.extend_from_slice(&moment.offset.to_le_bytes());
    out.extend_from_slice(&(moment.gates.len() as u32).to_le_bytes());
    out.extend_from_slice(&moment.gates);
}

fn decode_moment(r: &mut Reader) -> Option<MomentPayload> {
    let gate_count = r.u16()?;
    let first_gate_range_m = r.u16()?;
    let gate_interval_m = r.u16()?;
    let word_size = r.u8()?;
    let scale = r.f32()?;
    let offset = r.f32()?;
    let gate_len = r.u32()?;
    let gates = r.take(gate_len as usize)?.to_vec();
    Some(MomentPayload {
        gate_count,
        first_gate_range_m,
        gate_interval_m,
        word_size,
        scale,
        offset,
        gates,
    })
}

/// A bounds-checked cursor. Every accessor returns `None` rather than panicking,
/// because the bytes come off a message port and are not trusted.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// `count` as a capacity, refused if the buffer cannot possibly hold that
    /// many items of `min_size` bytes each. Keeps a corrupt length from
    /// reserving gigabytes before the read fails.
    fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

#[cfg(test)]
mod tests;
