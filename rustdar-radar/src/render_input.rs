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
    /// no tilt: a section cuts across all of them. There is no storm motion
    /// override and no environment because the only products that read either
    /// are ones [`crate::sampler::samplable`] refuses outright — the two
    /// velocity derivations, the hail pair and the classification — so carrying
    /// them would make a section payload's bytes depend on caches no section
    /// can consult.
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
        // A `Scope::Volume` request widens it further, and by `||` rather than
        // by replacing it: the six arms of `reads_whole_volume` are unchanged
        // and still decide for every tilt-scoped request.
        let whole_volume = scope == Scope::Volume || product.reads_whole_volume();

        // Only the HHC reads moments beyond its slot; everything else ships
        // the slot moment alone.
        let all_moments = product == RadarProduct::HydrometeorClassification;
        let cuts = CutTable::of(scan);
        let sweeps = if whole_volume {
            collect_sweeps(scan, &cuts, slot, all_moments)
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
        let slot = self.product.moment_slot();
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
        Self {
            angles: scan.coverage_pattern().elevation_cuts(),
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

/// Every sweep whose first radial carries `slot`'s moment, in scan order.
/// With `all_moments` (the HHC), a sweep carrying *any* moment qualifies —
/// the split-cut Doppler halves carry no differential phase but donate the
/// velocity the classification grafts in.
fn collect_sweeps(
    scan: &Scan,
    cuts: &CutTable<'_>,
    slot: MomentSlot,
    all_moments: bool,
) -> Vec<SweepData> {
    scan.sweeps()
        .iter()
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
const FORMAT_VERSION: u16 = 7;

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
        // The same refusal `extract` makes. A payload naming a Level III
        // product has no moment any field could hold, so it could only ever
        // render nothing; refusing it here keeps that from looking like a
        // renderer that found no sweep.
        product.moment_slot()?;
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
        let mut sweeps = Vec::with_capacity(r.bounded(sweep_count, 11)?);
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
                // flag + 1 cut-angle flag (+ 8 for the angle) + 4 radial count.
                11 + if s.cut_angle_deg.is_some() { 8 } else { 0 }
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
mod tests {
    use super::*;
    use crate::render::{render_from, render_radar_to_image_full};

    const LAT: f64 = 35.3333;
    const LON: f64 = -97.2778;
    /// The standard Level II reflectivity encoding: `dBZ = (raw - 66) / 2`.
    const REFL_SCALE: f32 = 2.0;
    const REFL_OFFSET: f32 = 66.0;
    /// Velocity at 0.5 m/s resolution: `m/s = (raw - 129) / 2`.
    const VEL_SCALE: f32 = 2.0;
    const VEL_OFFSET: f32 = 129.0;
    const RADIALS: usize = 360;

    fn moment(scale: f32, offset: f32, byte: u8, gates: usize) -> MomentData {
        MomentData::from_fixed_point(gates as u16, 0, 250, 8, scale, offset, vec![byte; gates])
    }

    /// One sweep at `elevation`, `RADIALS` radials spaced evenly from 0°.
    ///
    /// `refl` and `vel` are per-radial byte generators; `None` leaves that
    /// moment absent, which is how a surveillance cut is told from a Doppler
    /// one.
    fn sweep(
        elevation: f32,
        refl: Option<&dyn Fn(usize) -> u8>,
        vel: Option<&dyn Fn(usize) -> u8>,
    ) -> Sweep {
        let radials = (0..RADIALS)
            .map(|i| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / RADIALS as f32),
                    360.0 / RADIALS as f32,
                    RadialStatus::IntermediateRadialData,
                    1,
                    elevation,
                    refl.map(|f| moment(REFL_SCALE, REFL_OFFSET, f(i), 600)),
                    vel.map(|f| moment(VEL_SCALE, VEL_OFFSET, f(i), 400)),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(1, radials)
    }

    /// Strong, uniform reflectivity — well past the echo-tops threshold so the
    /// interpolated path paints.
    fn strong_refl(_: usize) -> u8 {
        200
    }

    fn weaker_refl(_: usize) -> u8 {
        150
    }

    /// Eight cycles of ±35 m/s: enough azimuthal shear to survive the NROT fit,
    /// the range normalization and the display threshold.
    fn shear(i: usize) -> u8 {
        let theta = i as f64 / RADIALS as f64 * std::f64::consts::TAU;
        (129.0 + 35.0 * (8.0 * theta).sin() * 2.0)
            .round()
            .clamp(2.0, 254.0) as u8
    }

    /// A volume shaped like a real SAILS one: a 0.5° surveillance cut carrying
    /// only reflectivity, a 0.5° Doppler cut carrying both, and a merged 1.5°
    /// tilt carrying both.
    ///
    /// The two 0.5° cuts are what make `find_sweep`'s surveillance preference
    /// observable, and the cuts carrying *both* moments are what would catch a
    /// payload that guessed at which moment to carry.
    fn volume() -> Scan {
        Scan::new(
            placeholder_coverage_pattern(0),
            vec![
                sweep(0.5, Some(&strong_refl), None),
                sweep(0.5, Some(&weaker_refl), Some(&shear)),
                sweep(1.5, Some(&weaker_refl), Some(&shear)),
            ],
        )
    }

    /// One tilt at `elevation` carrying every moment a radial can hold.
    ///
    /// [`volume`] is shaped like a real SAILS volume and so carries only
    /// reflectivity and velocity, which is all the products behind those two
    /// moments need. `extract` refuses a product whose moment no sweep carries,
    /// so a claim made about *every* product needs a volume where every field
    /// is present — the gate values do not matter, only that they are there.
    fn every_moment_tilt(elevation: f32, number: u8) -> Sweep {
        let radials = (0..RADIALS)
            .map(|i| {
                let other = || Some(moment(1.0, 0.0, shear(i), 400));
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / RADIALS as f32),
                    360.0 / RADIALS as f32,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(moment(REFL_SCALE, REFL_OFFSET, strong_refl(i), 600)),
                    Some(moment(VEL_SCALE, VEL_OFFSET, shear(i), 400)),
                    other(),
                    other(),
                    other(),
                    other(),
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    }

    /// Byte-for-byte on the image, element-for-element on the value grid.
    /// `f32::NAN != f32::NAN`, and the grid is NaN wherever no gate claimed the
    /// pixel — which is most of it — so a naive compare would pass on two
    /// entirely blank renders.
    fn assert_same_frame(
        left: &(Vec<u8>, f64, Vec<f32>),
        right: &(Vec<u8>, f64, Vec<f32>),
        what: &str,
    ) {
        assert_eq!(left.0, right.0, "{what}: RGBA differs");
        assert_eq!(left.1, right.1, "{what}: max range differs");
        assert_eq!(
            left.2.len(),
            right.2.len(),
            "{what}: value grid length differs"
        );
        for (i, (a, b)) in left.2.iter().zip(&right.2).enumerate() {
            assert!(
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
                "{what}: value {i} differs: {a} vs {b}"
            );
        }
    }

    fn painted(frame: &(Vec<u8>, f64, Vec<f32>)) -> usize {
        frame.0.chunks_exact(4).filter(|px| px[3] != 0).count()
    }

    /// The storm motion override a storm-relative render carries, for the
    /// products whose parity is asserted below. `None` for everything else:
    /// only SRV reads it, and without one SRV would need the fixture volume
    /// to support a Bunkers fit, which its two shallow tilts cannot.
    fn override_for(product: RadarProduct) -> Option<(f32, f32)> {
        (product == RadarProduct::StormRelativeVelocity).then_some((30.0, 240.0))
    }

    /// The environmental heights a hail render carries — only the hail pair
    /// reads them, and without a pair those products render nothing at all.
    /// 2 / 4 km MSL sits the fixture's strong low tilt across the ramp.
    fn env_for(product: RadarProduct) -> Option<(f64, f64)> {
        reads_env_heights(product).then_some((2.0, 4.0))
    }

    /// The acceptance criterion for moving rasterization into a worker: the
    /// payload path and the whole-volume path produce the same frame, for every
    /// product shape — one sweep, velocity-derived, and whole-volume.
    #[test]
    fn render_from_an_extracted_payload_matches_the_scan_path() {
        let scan = volume();
        for product in [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::NormalizedRotation,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ] {
            let over = override_for(product);
            let env = env_for(product);
            let direct =
                crate::render::render_radar_to_image_full(&scan, 0.5, product, LAT, LON, over, env)
                    .unwrap();
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, over, env).unwrap();
            let viaformat = render_from(&input).unwrap();

            assert!(
                painted(&direct) > 1_000,
                "{product:?} painted only {} pixels — the comparison would be vacuous",
                painted(&direct)
            );
            assert_same_frame(&direct, &viaformat, &format!("{product:?}"));
        }
    }

    /// A sweep that opens off its own tilt while the antenna settles: the first
    /// thirty radials ramp from `first` to `flown`, the rest sit on `flown`, so
    /// the median is `flown` and the first radial is not.
    ///
    /// [`volume`] gives every sweep one constant elevation, which makes the two
    /// readings the same number — so it cannot see the hazard the next test is
    /// about, and neither could any fixture in this module before it.
    fn settling_sweep(number: u8, first: f32, flown: f32) -> Sweep {
        const SETTLING: usize = 30;
        let radials = (0..RADIALS)
            .map(|i| {
                let elevation = if i < SETTLING {
                    first + (flown - first) * (i as f32 / SETTLING as f32)
                } else {
                    flown
                };
                Radial::new(
                    0,
                    i as u16,
                    i as f32 * (360.0 / RADIALS as f32),
                    360.0 / RADIALS as f32,
                    RadialStatus::IntermediateRadialData,
                    number,
                    elevation,
                    Some(moment(REFL_SCALE, REFL_OFFSET, strong_refl(i), 600)),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    }

    /// The payload has to survive the port for a sweep that opened off its
    /// tilt, and this is the tightest constraint on what `SweepData` may carry.
    ///
    /// `to_scan` stamps one elevation onto every reconstructed radial, so
    /// whatever `sweep_data` stored *is* the reconstructed sweep's median.
    /// `find_sweep` matches on the median within a tenth of a degree, so
    /// storing the first radial's angle — 0.3° from the tilt the request names —
    /// would leave the worker unable to find the one sweep its own payload
    /// contains, and the web path would render nothing at all. Constant-elevation
    /// fixtures cannot fail this; this one can.
    #[test]
    fn a_sweep_that_opened_off_its_tilt_still_renders_after_the_port() {
        let scan = Scan::new(
            placeholder_coverage_pattern(0),
            vec![settling_sweep(1, 0.68, 0.44)],
        );
        let product = RadarProduct::Reflectivity;

        let direct =
            crate::render::render_radar_to_image_full(&scan, 0.4, product, LAT, LON, None, None)
                .expect("the scan path draws the cut this volume flew");
        let input = RenderInput::extract(&scan, 0.4, product, LAT, LON, None, None)
            .expect("the payload extracts that same cut");
        assert!(
            (input.sweeps[0].elevation_angle - 0.44).abs() < 1e-4,
            "the payload must carry the tilt the sweep flew, not the one it opened on — got {}",
            input.sweeps[0].elevation_angle,
        );
        let reconstructed = input.to_scan();
        assert!(
            crate::render::find_sweep(&reconstructed, product, 0.4).is_some(),
            "the worker must find the one sweep its payload carries",
        );
        let via = render_from(&input).expect("the payload renders");
        assert!(
            painted(&direct) > 1_000,
            "the comparison would be vacuous — only {} pixels painted",
            painted(&direct),
        );
        assert_same_frame(&direct, &via, "a sweep that opened off its tilt");
    }

    /// The same, across the wire format the worker actually receives.
    #[test]
    fn a_payload_renders_the_same_frame_after_a_round_trip() {
        let scan = volume();
        for product in [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::NormalizedRotation,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ] {
            let input = RenderInput::extract(
                &scan,
                0.5,
                product,
                LAT,
                LON,
                override_for(product),
                env_for(product),
            )
            .unwrap();
            let decoded = RenderInput::from_bytes(&input.to_bytes())
                .unwrap_or_else(|| panic!("{product:?} payload did not decode"));
            assert_eq!(input, decoded, "{product:?} payload changed in transit");
            assert_eq!(
                decoded.storm_motion_override(),
                override_for(product),
                "{product:?}: the override must survive the wire",
            );
            assert_eq!(
                decoded.env_heights_km_msl(),
                env_for(product),
                "{product:?}: the environment must survive the wire",
            );
            assert_same_frame(
                &render_from(&input).unwrap(),
                &render_from(&decoded).unwrap(),
                &format!("{product:?} round trip"),
            );
        }
    }

    /// Storm-relative velocity is a Level II product now: it extracts, it
    /// carries every velocity tilt (the profile is both its dealias seed and
    /// its Bunkers input), and the override moves the field.
    #[test]
    fn srv_extracts_the_velocity_volume_and_honours_the_override() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::StormRelativeVelocity,
            LAT,
            LON,
            Some((30.0, 240.0)),
            None,
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 2, "both velocity tilts travel");
        assert_eq!(input.storm_motion_override(), Some((30.0, 240.0)));

        // A different vector must change pixels: the override reaches the
        // arithmetic, not just the payload.
        let other = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::StormRelativeVelocity,
            LAT,
            LON,
            Some((30.0, 60.0)),
            None,
        )
        .unwrap();
        let a = render_from(&input).unwrap();
        let b = render_from(&other).unwrap();
        assert!(painted(&a) > 1_000);
        assert_ne!(a.0, b.0, "the vector was carried but never applied");
    }

    /// `to_bytes` reserves exactly what it writes. Wrong by a little is only a
    /// realloc; wrong by a lot means the layout and the estimate have drifted.
    ///
    /// Both branches of the per-sweep cut angle have to be measured: the
    /// `volume()` fixture has no cut table so every sweep writes the one-byte
    /// absent form, and [`cut_table_volume`] has one so every sweep writes the
    /// nine-byte present form. An estimate that had forgotten the angle
    /// entirely would still match the first.
    #[test]
    fn the_encoded_length_estimate_is_exact() {
        let scan = volume();
        for product in [RadarProduct::Reflectivity, RadarProduct::NormalizedRotation] {
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None).unwrap();
            assert!(
                input.sweeps.iter().all(|s| s.cut_angle_deg.is_none()),
                "precondition: this fixture is supposed to have no cut table",
            );
            assert_eq!(input.encoded_len(), input.to_bytes().len(), "{product:?}");
        }

        let scan = cut_table_volume();
        for input in [
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap(),
            RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON).unwrap(),
        ] {
            assert!(
                input.sweeps.iter().all(|s| s.cut_angle_deg.is_some()),
                "precondition: this fixture is supposed to have a cut table",
            );
            assert_eq!(input.encoded_len(), input.to_bytes().len());
        }
    }

    /// One elevation cut, angle only — the reconstruction's own
    /// [`elevation_cut`] under a name the fixtures read.
    fn cut(angle_deg: f64) -> ElevationCut {
        elevation_cut(angle_deg)
    }

    /// [`volume`], but flown under a VCP that declares its cuts — three
    /// entries, and sweeps that name them 1, 2 and 3.
    ///
    /// The declared angles are deliberately **not** the medians: 0.48 against a
    /// 0.5 median, 0.51 against a 0.5, 1.47 against a 1.5. That is what real
    /// data looks like (measured medians sit up to 0.044° off the declared cut)
    /// and it is what tells a reconstruction that carried the cut table apart
    /// from one that re-derived it from the sweeps.
    fn cut_table_volume() -> Scan {
        let mut sweeps = vec![
            sweep(0.5, Some(&strong_refl), None),
            sweep(0.5, Some(&weaker_refl), Some(&shear)),
            sweep(1.5, Some(&weaker_refl), Some(&shear)),
        ];
        for (i, s) in sweeps.iter_mut().enumerate() {
            *s = Sweep::new(i as u8 + 1, s.radials().to_vec());
        }
        Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                vec![cut(0.48), cut(0.51), cut(1.47)],
            ),
            sweeps,
        )
    }

    /// The reconstruction carries the ladder key, and carries it *raw*.
    ///
    /// Two fields, and both used to be wrong in ways nothing reported: the cut
    /// table was empty, and `elevation_number` was the sweep's index in the
    /// payload, so the first sweep claimed to be cut 0 — a number that cannot
    /// index a 1-based table at all. `crate::sampler::VolumeSampler` reads both,
    /// and the ladder it builds from them is not checkable against anything
    /// once the sampler is gone.
    #[test]
    fn the_reconstruction_carries_the_cut_table_and_the_real_elevation_numbers() {
        let scan = cut_table_volume();
        let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity");
        let rebuilt = RenderInput::from_bytes(&input.to_bytes())
            .expect("the payload round-trips")
            .to_scan();

        assert_eq!(
            rebuilt
                .sweeps()
                .iter()
                .map(Sweep::elevation_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "the reconstructed sweeps do not name the cuts the originals named",
        );
        assert_eq!(
            rebuilt
                .coverage_pattern()
                .elevation_cuts()
                .iter()
                .map(ElevationCut::elevation_angle_degrees)
                .collect::<Vec<_>>(),
            vec![0.48, 0.51, 1.47],
            "the reconstructed cut table is not the original's",
        );
        assert_eq!(
            rebuilt.coverage_pattern().pattern_number().number(),
            212,
            "a rebuilt cut table under a VCP number nobody flew is worse than \
             no table at all",
        );
        // And the angles are the *declared* ones, not the sweeps' medians —
        // which is the difference between carrying the table and re-deriving it.
        assert!(
            rebuilt
                .coverage_pattern()
                .elevation_cuts()
                .iter()
                .zip(rebuilt.sweeps())
                .all(|(cut, sweep)| {
                    let median =
                        crate::volumetric::sweep_elevation_deg(sweep.radials()).unwrap_or_default();
                    (cut.elevation_angle_degrees() - median).abs() > 1e-6
                }),
            "every reconstructed cut angle equals its sweep's median, so this \
             test cannot tell a carried table from a re-derived one",
        );
    }

    /// **A volume that stopped part way up still knows how far up its pattern
    /// goes.**
    ///
    /// The reconstruction used to size the cut table to the largest elevation
    /// number the payload carried, filling unnamed slots with a copy of the
    /// nearest carried angle. That keys every carried sweep correctly, which is
    /// all the ladder needs — and it silently makes the table's ceiling the
    /// *volume's* ceiling. Every cross-section in the app is cut from a
    /// reconstructed scan, so "did this volume reach the top of its pattern?"
    /// answered yes for all of them, and a live section three rungs into VCP
    /// 212 captioned itself as complete for the whole six minutes it was not.
    ///
    /// Nothing about that failure is visible in the ladder: the rungs are
    /// right, the heights are right, the raster is right. Only the sentence
    /// underneath it is wrong, and it is wrong in the reassuring direction.
    #[test]
    fn a_part_flown_volume_still_carries_the_ceiling_its_pattern_declares() {
        // The first cut only, out of a three-cut table: a volume caught early.
        let whole = cut_table_volume();
        let part_flown = Scan::new(
            whole.coverage_pattern().clone(),
            vec![whole.sweeps()[0].clone()],
        );

        let input = RenderInput::extract_volume(&part_flown, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity");
        let rebuilt = RenderInput::from_bytes(&input.to_bytes())
            .expect("the payload round-trips")
            .to_scan();

        let angles: Vec<f64> = rebuilt
            .coverage_pattern()
            .elevation_cuts()
            .iter()
            .map(ElevationCut::elevation_angle_degrees)
            .collect();
        assert_eq!(
            angles,
            vec![0.48, 0.51, 1.47],
            "the reconstructed table stops where the volume stopped, so nothing \
             downstream can tell a truncated volume from a complete one",
        );
        assert_eq!(
            rebuilt.sweeps().len(),
            1,
            "precondition: only one cut was flown, so the table is longer than \
             anything that could have been derived from the sweeps",
        );

        // Which is the fact the sampler hands a section, and the one a caption
        // reads to decide whether the blank above the top rung is the cone of
        // silence or air nobody has looked at yet.
        let sampler = crate::sampler::VolumeSampler::new(&rebuilt, RadarProduct::Reflectivity)
            .expect("one cut is a ladder");
        assert_eq!(sampler.top_tilt_deg(), 0.48);
        assert_eq!(sampler.top_declared_cut_deg(), 1.47);
        assert!(
            sampler.top_tilt_deg() < sampler.top_declared_cut_deg(),
            "a one-rung volume out of a three-cut pattern reported a complete \
             ladder",
        );

        // And a complete volume through the same path still reports complete,
        // so the fix is not simply "always warn".
        let complete = RenderInput::from_bytes(
            &RenderInput::extract_volume(&whole, RadarProduct::Reflectivity, LAT, LON)
                .expect("the fixture carries reflectivity")
                .to_bytes(),
        )
        .expect("the payload round-trips")
        .to_scan();
        let sampler = crate::sampler::VolumeSampler::new(&complete, RadarProduct::Reflectivity)
            .expect("three cuts are a ladder");
        assert_eq!(sampler.top_tilt_deg(), sampler.top_declared_cut_deg());
    }

    /// The carried-velocity bit survives the port, and materialises as a
    /// **gateless** marker rather than as invented data.
    ///
    /// The `volume()` fixture is a split cut: a surveillance 0.5° carrying only
    /// reflectivity, a Doppler 0.5° carrying both, and a merged 1.5° carrying
    /// both. A reflectivity payload ships none of the velocity, so the bit is
    /// the only thing that can tell the sampler which sweep is which half.
    #[test]
    fn the_doppler_half_is_still_recognisable_after_the_port() {
        let scan = volume();
        let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity");
        assert_eq!(
            input
                .sweeps
                .iter()
                .map(|s| s.carried_velocity)
                .collect::<Vec<_>>(),
            vec![false, true, true],
            "the bit does not match the fixture's split cut",
        );
        // precondition: none of the velocity itself travelled, so the bit is
        // doing the work rather than the data.
        assert!(
            input
                .sweeps
                .iter()
                .flat_map(|s| &s.radials)
                .all(|r| r.extras.is_empty()),
            "a reflectivity payload started carrying other moments, so this \
             test no longer measures what the bit is for",
        );

        let rebuilt = RenderInput::from_bytes(&input.to_bytes())
            .expect("round trips")
            .to_scan();
        let velocities: Vec<bool> = rebuilt
            .sweeps()
            .iter()
            .map(|s| s.radials()[0].velocity().is_some())
            .collect();
        assert_eq!(
            velocities,
            vec![false, true, true],
            "the reconstructed sweeps do not report the halves they were",
        );
        // And the marker is empty: a wind fit or a dealiaser reading it finds
        // nothing, rather than finding a number nobody measured.
        for sweep in rebuilt.sweeps().iter().skip(1) {
            let velocity = sweep.radials()[0].velocity().expect("marked");
            assert_eq!(velocity.raw_values().len(), 0, "the marker invented gates");
            assert_eq!(velocity.values().len(), 0);
        }
    }

    /// A cut below the horizon arrives from the decoder as ~359.7°, and the
    /// sampler is what turns that into −0.3°.
    ///
    /// So the payload must carry it **uncorrected**: correcting it here would
    /// mean the correction ran once on the main thread and not at all in the
    /// worker, and the two would key that cut differently — 359.7° sorts to the
    /// top of the ladder, −0.3° to the bottom.
    #[test]
    fn a_below_horizon_cut_travels_uncorrected() {
        let scan = Scan::new(
            VolumeCoveragePattern::new(
                212,
                0,
                0.5,
                PulseWidth::Short,
                false,
                0,
                false,
                0,
                false,
                false,
                0,
                false,
                false,
                vec![cut(359.7)],
            ),
            vec![Sweep::new(
                1,
                sweep(-0.3, Some(&strong_refl), None).radials().to_vec(),
            )],
        );
        let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity");
        assert_eq!(input.sweeps[0].cut_angle_deg, Some(359.7));
        assert_eq!(
            input.to_scan().coverage_pattern().elevation_cuts()[0].elevation_angle_degrees(),
            359.7,
        );
    }

    /// A payload from a volume whose cut table could not answer rebuilds an
    /// **empty** table rather than inventing one.
    ///
    /// That is what a volume joined mid-flight looks like — `crate::chunks`
    /// stands in a pattern with no cuts until the start chunk lands — and the
    /// sampler refuses it. Faithful includes faithfully unusable; the
    /// alternative is a ladder in the worker the main thread would not have
    /// built.
    #[test]
    fn a_payload_with_no_cut_angles_rebuilds_an_empty_table() {
        let scan = volume();
        let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity");
        assert!(input.sweeps.iter().all(|s| s.cut_angle_deg.is_none()));
        assert!(
            input
                .to_scan()
                .coverage_pattern()
                .elevation_cuts()
                .is_empty(),
        );
    }

    /// `extract_volume` carries every tilt carrying the moment, whatever
    /// [`RadarProduct::reads_whole_volume`] says about the product.
    ///
    /// Reflectivity is a one-sweep product — `a_plain_product_carries_one_sweep`
    /// pins that for `extract` — so if the two constructors ever came to share
    /// the tilt-scoped branch this would carry one sweep and a section would be
    /// drawn from a single beam.
    #[test]
    fn extract_volume_carries_every_tilt_whatever_the_product_says() {
        let scan = volume();
        assert!(
            !RadarProduct::Reflectivity.reads_whole_volume(),
            "precondition: reflectivity became a whole-volume product, so this \
             says nothing about the scope argument",
        );
        let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity");
        assert_eq!(input.sweeps.len(), scan.sweeps().len());
        // And the widening is by `||`: a product that already read the whole
        // volume still does.
        let nrot = RenderInput::extract_volume(&scan, RadarProduct::NormalizedRotation, LAT, LON)
            .expect("the fixture carries velocity");
        assert_eq!(nrot.sweeps.len(), 2, "both velocity tilts still travel");
    }

    /// A whole-volume payload handed to a *frame* consumer draws nothing — the
    /// state every render path already handles — rather than silently drawing
    /// whichever tilt happened to be nearest.
    #[test]
    fn a_whole_volume_payload_renders_no_frame() {
        let scan = cut_table_volume();
        let input = RenderInput::extract_volume(&scan, RadarProduct::Reflectivity, LAT, LON)
            .expect("the fixture carries reflectivity");
        assert_eq!(input.elevation(), NO_ELEVATION_DEG);
        assert!(
            render_from(&input).is_none(),
            "a section payload drew a plan-view frame",
        );
        // precondition: the payload is not empty, so what refuses above is the
        // elevation and not a missing sweep.
        assert_eq!(input.sweeps.len(), 3);
    }

    /// Why the sentinel is `-1000.0` and not either of the two obvious
    /// alternatives.
    ///
    /// The bar is not "no sweep in some fixture matches it". `find_sweep`
    /// accepts any sweep whose median is within
    /// [`crate::render::ELEVATION_WINDOW`], so a sentinel is only safe if it
    /// sits that far outside **every angle an antenna can point at** — the
    /// payload can be built from any volume, and a sentinel that is merely
    /// unusual is one a volume eventually walks onto.
    ///
    /// * `0.0` fails that outright: it is a legal elevation and a below-horizon
    ///   cut is a real thing (the wrap correction in
    ///   [`crate::sampler::VolumeSampler`] exists for exactly those). This test
    ///   builds a sweep 0.05° above the horizon and shows `0.0` claims it.
    /// * `NaN` fails differently: `RenderInput` derives `PartialEq`, so a
    ///   whole-volume payload carrying one would be unequal to itself and every
    ///   round-trip assertion in this module would fail on it — the failure
    ///   `CrossSection` and `VoxelGrid` hand-write their `PartialEq` to avoid.
    #[test]
    fn the_sentinel_elevation_is_one_no_sweep_can_carry() {
        let near_horizon = Scan::new(
            placeholder_coverage_pattern(0),
            vec![Sweep::new(
                1,
                sweep(0.05, Some(&strong_refl), None).radials().to_vec(),
            )],
        );
        assert!(
            crate::render::find_sweep(&near_horizon, RadarProduct::Reflectivity, 0.0).is_some(),
            "0.0 is disqualified as a sentinel because a cut just above the \
             horizon claims it — if this stops being true, say so here rather \
             than quietly reverting the constant",
        );
        assert!(
            crate::render::find_sweep(&near_horizon, RadarProduct::Reflectivity, NO_ELEVATION_DEG)
                .is_none(),
        );
        // The general bar, rather than one fixture's worth of it: outside the
        // window of every angle an antenna can point at.
        assert!(
            f64::from(NO_ELEVATION_DEG).abs() > 90.0 + crate::render::ELEVATION_WINDOW,
            "{NO_ELEVATION_DEG} is inside the window of an angle a real \
             antenna can reach",
        );
        assert!(
            NO_ELEVATION_DEG.is_finite(),
            "a NaN sentinel breaks the derived PartialEq",
        );
        // Finite and exactly representable, so it survives the f32 wire field.
        let input =
            RenderInput::extract_volume(&cut_table_volume(), RadarProduct::Reflectivity, LAT, LON)
                .unwrap();
        assert_eq!(
            RenderInput::from_bytes(&input.to_bytes()).unwrap(),
            input,
            "a whole-volume payload is not equal to itself after the wire",
        );
    }

    /// The version is a *number on the wire*, not merely a check that exists.
    ///
    /// Every other test here round-trips a payload through this build's own
    /// codec, so all of them pass whatever the constant says — a version that
    /// silently failed to bump when the layout changed would be invisible to
    /// the entire module, and the two ends of a worker port are exactly where
    /// that costs something. The literal below is the whole assertion: changing
    /// the layout without changing it fails here.
    ///
    /// The magic is written as a literal for the same reason. Asserting it
    /// against `MAGIC` is self-consistency — the encoder writes that constant,
    /// so any unused four bytes stayed green — and the relabel loop in
    /// `a_malformed_payload_is_refused_rather_than_misread` only pins `RDRI`
    /// against its two port-mates, which has nothing to say about a third
    /// value. The far end of the port has no constant that moves with this
    /// one. Mirrors `xsect`'s and `voxel`'s tests of the same name.
    #[test]
    fn the_format_version_is_the_one_this_layout_ships() {
        assert_eq!(FORMAT_VERSION, 7);
        let bytes = RenderInput::extract(
            &volume(),
            0.5,
            RadarProduct::Reflectivity,
            LAT,
            LON,
            None,
            None,
        )
        .unwrap()
        .to_bytes();
        assert_eq!(&bytes[..4], b"RDRI", "the magic moved");
        assert_eq!(
            u16::from_le_bytes([bytes[4], bytes[5]]),
            7,
            "the version is not where a decoder from another build looks for it",
        );
    }

    /// A merged tilt carries reflectivity *and* velocity. Reading "whichever
    /// moment this radial has" off it would hand a reflectivity render the
    /// velocity gates — a frame that renders, looks like weather, and is wrong.
    #[test]
    fn a_tilt_carrying_both_moments_still_yields_the_requested_one() {
        let scan = Scan::new(
            placeholder_coverage_pattern(0),
            vec![sweep(0.5, Some(&strong_refl), Some(&shear))],
        );
        let input =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap();
        let moment = input.sweeps[0].radials[0].moment.as_ref().unwrap();
        assert_eq!(moment.scale, REFL_SCALE);
        assert_eq!(moment.offset, REFL_OFFSET);
        assert_eq!(
            moment.gates[0],
            strong_refl(0),
            "carried the velocity gates under the reflectivity request"
        );
    }

    /// What travels is what [`RadarProduct::reads_whole_volume`] says travels,
    /// for every product there is.
    ///
    /// That predicate is also what the live chunk feed narrows a site's download
    /// by, and the two used to be separate hand-maintained matches: the feed's
    /// copy omitted storm-relative velocity, so a live SRV pane fit its dealias
    /// seed and its default Bunkers vector from a volume the feed had
    /// deliberately skipped cuts of — no error, no NaN, and archived volumes are
    /// whole, so nothing under test saw it.
    ///
    /// This asserts the half that lives here: that `extract` *reads* the
    /// predicate for every product rather than deciding again, so a second copy
    /// cannot grow back inside it. Whether the predicate's own answer is right
    /// is a claim about the algorithms, and each whole-volume product's
    /// individual test below is what pins that — every one of them fails if its
    /// product is downgraded to a single sweep.
    #[test]
    fn every_product_carries_the_volume_exactly_when_it_says_it_reads_one() {
        let scan = Scan::new(
            placeholder_coverage_pattern(0),
            vec![
                every_moment_tilt(0.5, 1),
                every_moment_tilt(1.5, 2),
                every_moment_tilt(2.5, 3),
            ],
        );
        let tilts = scan.sweeps().len();
        assert!(
            tilts > 1,
            "precondition: with one tilt in the volume, carrying the volume and \
             carrying one sweep are the same payload and this says nothing"
        );

        for &product in RadarProduct::all() {
            let Some(input) = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None)
            else {
                assert!(
                    product.is_level3(),
                    "{product:?} extracted nothing from a volume carrying every \
                     moment on every tilt"
                );
                continue;
            };
            let expected = if product.reads_whole_volume() {
                tilts
            } else {
                1
            };
            assert_eq!(
                input.sweeps.len(),
                expected,
                "{product:?}: reads_whole_volume() is {}, so {expected} of the \
                 volume's {tilts} tilts should have travelled",
                product.reads_whole_volume(),
            );
        }
    }

    /// The sizing decision the whole design rests on: a normal product ships
    /// one sweep, not the volume.
    #[test]
    fn a_plain_product_carries_one_sweep() {
        let scan = volume();
        let input =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap();
        assert_eq!(input.sweeps.len(), 1);
        assert_eq!(input.sweeps[0].radials.len(), RADIALS);
    }

    /// NROT fits its wind profile from every velocity tilt — that fit is the
    /// only wind source since the NVW fetch left — so every velocity tilt has
    /// to travel with the payload.
    #[test]
    fn nrot_carries_every_velocity_tilt() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::NormalizedRotation,
            LAT,
            LON,
            None,
            None,
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 2, "both velocity tilts travel");
    }

    /// Interpolated echo tops integrate the volume; every reflectivity tilt has
    /// to be there, in scan order, because `VolumeCube::build` dedups
    /// same-elevation cuts by encounter.
    #[test]
    fn interpolated_echo_tops_carries_every_reflectivity_tilt() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::EchoTopsInterpolated,
            LAT,
            LON,
            None,
            None,
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 3);
        assert_eq!(
            input
                .sweeps
                .iter()
                .map(|s| s.elevation_angle)
                .collect::<Vec<_>>(),
            vec![0.5, 0.5, 1.5],
            "scan order decides which same-elevation cut wins",
        );
    }

    /// A product with no Level II moment behind it renders nothing today, and
    /// must not produce a payload that pretends otherwise.
    #[test]
    fn a_product_with_no_level_two_moment_extracts_nothing() {
        let scan = volume();
        assert!(
            RenderInput::extract(&scan, 0.5, RadarProduct::EchoTops, LAT, LON, None, None)
                .is_none()
        );
        assert!(
            render_radar_to_image_full(&scan, 0.5, RadarProduct::EchoTops, LAT, LON, None, None)
                .is_none(),
            "the payload and the renderer must refuse the same requests"
        );
    }

    /// The hail pair without an environment: the payload still extracts —
    /// the sweeps and the request are valid — but both render paths answer
    /// nothing, the explicit "undefined field" seam (`crate::hail`), never a
    /// zero-filled grid. The same payload with an environment paints.
    #[test]
    fn hail_without_an_environment_renders_nothing_on_both_paths() {
        let scan = volume();
        for product in [
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ] {
            let input = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, None).unwrap();
            assert_eq!(input.env_heights_km_msl(), None);
            assert!(
                render_from(&input).is_none(),
                "{product:?} rendered without an environment"
            );
            assert!(
                crate::render::render_radar_to_image_full(
                    &scan, 0.5, product, LAT, LON, None, None,
                )
                .is_none(),
                "{product:?}: the payload and the renderer must refuse alike"
            );

            let with = RenderInput::extract(&scan, 0.5, product, LAT, LON, None, Some((2.0, 4.0)))
                .unwrap();
            let frame = render_from(&with).unwrap();
            assert!(
                painted(&frame) > 1_000,
                "{product:?} with an environment must paint"
            );
        }
    }

    /// The hybrid classification's payload carries every sweep with every
    /// moment (the extras), plus the environmental heights — and the whole
    /// bundle survives the byte round trip. The fixture volume carries only
    /// reflectivity and velocity, which is not enough to classify, so the
    /// pin here is structural: the extras and heights are the parts version
    /// 5 added.
    #[test]
    fn hhc_payloads_carry_extras_and_env_heights() {
        let scan = volume();
        let input = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::HydrometeorClassification,
            LAT,
            LON,
            None,
            Some((5.0, 8.6)),
        )
        .unwrap();
        assert_eq!(input.sweeps.len(), 3, "every sweep travels");
        assert_eq!(input.env_heights_km_msl(), Some((5.0, 8.6)));
        // The slot moment is reflectivity; velocity rides in the extras.
        let with_velocity = input.sweeps[1]
            .radials
            .iter()
            .filter(|r| r.extras.iter().any(|(code, _)| *code == 1))
            .count();
        assert!(with_velocity > 0, "the Doppler moment travels as an extra");
        let back = RenderInput::from_bytes(&input.to_bytes()).expect("round trips");
        assert_eq!(back, input);
        // And the reconstruction puts the extras back on their fields.
        let rebuilt = back.to_scan();
        let radial = &rebuilt.sweeps()[1].radials()[0];
        assert!(radial.reflectivity().is_some(), "slot moment placed");
        assert!(radial.velocity().is_some(), "extra placed on its field");
        // A non-HHC product never carries either, whatever the caller
        // passed — other products' payload bytes must not depend on an
        // unrelated cache.
        let refl = RenderInput::extract(
            &scan,
            0.5,
            RadarProduct::Reflectivity,
            LAT,
            LON,
            None,
            Some((5.0, 8.6)),
        )
        .unwrap();
        assert_eq!(refl.env_heights_km_msl(), None);
        assert!(refl.sweeps[0].radials.iter().all(|r| r.extras.is_empty()));
    }

    /// The bytes arrive off a message port. Every malformed shape has to be a
    /// clean `None` — the two ends of that port can be different builds.
    #[test]
    fn a_malformed_payload_is_refused_rather_than_misread() {
        let scan = volume();
        let good =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap()
                .to_bytes();

        assert!(RenderInput::from_bytes(&[]).is_none(), "empty");
        assert!(RenderInput::from_bytes(b"nope").is_none(), "wrong magic");

        // A **whole** payload relabelled, including with the two magics that
        // share this port. Mutation testing is why: the four-byte buffer above
        // cannot pin the magic test, because it runs out on the *version* read
        // whether or not the comparison exists, and the truncation loop below
        // never cuts inside the magic. Deleting `if r.take(4)? != MAGIC` left
        // the entire workspace green — so an `RDVX` grid or an `RDXS` section
        // arriving on the shared worker port would have been read as a render
        // input rather than refused. `render_input` was the last of the three
        // legs of that handshake without this loop; `voxel` and `xsect` both
        // caught the same mutation in themselves with it.
        assert!(
            RenderInput::from_bytes(&good).is_some(),
            "precondition: the payload being relabelled has to decode as it \
             stands, or each refusal below could be for some other reason",
        );
        for wrong in [*b"nope", *b"RDVX", *b"RDXS"] {
            let mut relabelled = good.clone();
            relabelled[..4].copy_from_slice(&wrong);
            assert!(
                RenderInput::from_bytes(&relabelled).is_none(),
                "a whole payload labelled {} decoded as a render input",
                String::from_utf8_lossy(&wrong),
            );
        }

        let mut wrong_version = good.clone();
        wrong_version[4] = 0xFF;
        wrong_version[5] = 0xFF;
        assert!(RenderInput::from_bytes(&wrong_version).is_none(), "version");

        let mut wrong_product = good.clone();
        wrong_product[6] = 0xFE;
        wrong_product[7] = 0xFF;
        assert!(RenderInput::from_bytes(&wrong_product).is_none(), "product");

        for cut in [1, 8, 32, good.len() / 2, good.len() - 1] {
            assert!(
                RenderInput::from_bytes(&good[..cut]).is_none(),
                "truncated to {cut} bytes"
            );
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(
            RenderInput::from_bytes(&trailing).is_none(),
            "trailing bytes mean the layouts disagree"
        );
    }

    /// A corrupt length must not be believed far enough to reserve on it.
    #[test]
    fn an_absurd_length_does_not_reach_an_allocation() {
        let scan = volume();
        let mut bytes =
            RenderInput::extract(&scan, 0.5, RadarProduct::Reflectivity, LAT, LON, None, None)
                .unwrap()
                .to_bytes();
        // The sweep count sits directly after the header and the
        // absent-override and absent-environment flag bytes.
        let at = 4 + 2 + 2 + 4 + 8 + 8 + 1 + 1;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(RenderInput::from_bytes(&bytes).is_none());
    }

    /// Round-tripping through kilometres and back is exact for every value the
    /// field can hold, which is what makes the reconstructed moment identical.
    #[test]
    fn gate_ranges_survive_the_kilometre_round_trip() {
        for raw in [0u16, 1, 250, 999, 2125, 32768, u16::MAX] {
            assert_eq!(km_to_metres(raw as f64 * 0.001), raw);
        }
    }

    /// The wire codes are a fixed table, not the enum's declaration order:
    /// reordering the variants must not silently change what a payload means.
    #[test]
    fn every_product_has_a_stable_distinct_wire_code() {
        let products = [
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialPhase,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpecificDifferentialPhase,
            RadarProduct::EchoTops,
            RadarProduct::EchoTopsInterpolated,
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::HydrometeorClassification,
            RadarProduct::PrecipitationRate,
            RadarProduct::NormalizedRotation,
            RadarProduct::VilDensity,
            RadarProduct::ProbabilityOfSevereHail,
            RadarProduct::MaxExpectedHailSize,
        ];
        let mut seen = std::collections::HashSet::new();
        for product in products {
            let code = product.wire_code();
            assert!(seen.insert(code), "{product:?} reuses wire code {code}");
            assert_eq!(RadarProduct::from_wire_code(code), Some(product));
        }
        assert_eq!(RadarProduct::from_wire_code(0), None);
        assert_eq!(RadarProduct::from_wire_code(u16::MAX), None);
    }
}
