//! The renderer's input, in a form that can cross a process — or a Web Worker —
//! boundary.

use crate::types::{MomentSlot, RadarProduct};
use crate::wire::Reader;
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
    /// their original order. [`RenderInput::extract_volume`] has no tilt to ask
    /// for and stores [`NO_ELEVATION_DEG`].
    elevation: f32,
    radar_lat: f64,
    radar_lon: f64,
    /// The user's storm motion vector, knots and degrees-from. Read by
    /// storm-relative velocity alone; `None` means "no override".
    storm_motion_override: Option<(f32, f32)>,
    /// The site's environmental 0 °C / −20 °C heights, km MSL
    /// ([`crate::sounding::EnvHeights`]). Read by the hail pair and the hybrid
    /// hydrometeor classification. `None` means different things to each: the
    /// hail field is undefined and renders nothing, while the HHC falls back to
    /// the operational adaptation defaults, exactly as the RPG does.
    env_heights_km_msl: Option<(f64, f64)>,
    /// The RPG's own **Melting Layer product** (Level III 166, `N0M`) for this
    /// very volume, as the object's own bytes.
    ///
    /// Read by the hybrid hydrometeor classification alone, and it is the
    /// difference between a classification and a guess: the same classifier
    /// scored 82.8–95.9 % exact against the RPG's `N0H` with this layer and
    /// 16.0–19.8 % without it in winter regimes — figures measured off-tree and
    /// not reproducible from here, and [`crate::hhc`] quotes a different pair
    /// (93.8–100 % against 16.0–27.3 %) for what reads as the same comparison.
    ///
    /// Bytes rather than a decoded layer: a `Level3Message` has no wire form,
    /// and shipping the object lets the worker run the same decoder. Six
    /// kilobytes against the megabytes of moments beside it.
    melting_layer_product: Option<std::sync::Arc<Vec<u8>>>,
    /// The RPG's own applied **storm motion vector** for this very volume,
    /// knots and degrees-from, read out of an `N0S` Product Description Block.
    rpg_storm_motion: Option<(f32, f32)>,
    /// Which derived rung storm-relative velocity falls to when neither an
    /// override nor an `N0S` vector reached this payload.
    srv_fallback: crate::srv::SrvFallback,
    /// The volume coverage pattern number the scan was flown under.
    vcp: u16,
    /// Every cut angle the coverage pattern **declares**, in table order and
    /// exactly as the decoder hands them over — a below-horizon cut arrives as
    /// ~359.7° here, because wrap-correcting on the way in would make this a
    /// different table from the one the main thread keys against.
    ///
    /// The whole declared table, not the part this volume got to: a table
    /// rebuilt from the carried sweeps alone tops out wherever the volume did,
    /// so "the ladder reached the top of its pattern" was true of every volume
    /// ever cut in a worker and a live section captioned itself complete for
    /// the whole six minutes it was not.
    declared_cut_angles_deg: Vec<f64>,
    sweeps: Vec<SweepData>,
}

/// One sweep's worth of the product's moment, plus the two fields that let the
/// sweep be keyed back onto its VCP cut.
#[derive(Debug, Clone, PartialEq)]
struct SweepData {
    /// The sweep's **median** elevation
    /// ([`crate::volumetric::sweep_elevation_deg`]) — not its first radial's.
    ///
    /// The model carries elevation per radial and it is *not* constant across a
    /// sweep: the antenna is still settling when one opens, and the opening
    /// radial can sit a third of a degree from the tilt the sweep flew. Two
    /// things depend on this being the median, and both fail silently if it
    /// reverts to the first radial:
    ///
    /// * [`RenderInput::to_scan`] stamps this one value onto *every*
    ///   reconstructed radial, and [`crate::render::find_sweep`] matches on the
    ///   median within [`crate::render::ELEVATION_WINDOW`], so a first-radial
    ///   value puts the payload further from the request than the window
    ///   allows and the whole wasm render path draws nothing.
    /// * Every whole-volume product builds its tilt ladder from
    ///   `sweep_elevation_deg`; on the desktop path that reads the real
    ///   radials, on the web path this field copied across them. Anything but
    ///   the median makes the two paths compute *different ladders*.
    elevation_angle: f32,
    /// The sweep's own `elevation_number` — the RDA's statement of which cut of
    /// the VCP this sweep is, 1-based. Half of the sampler's ladder key, and
    /// the wrong half of it is not a degraded ladder but a different one.
    elevation_number: u8,
    /// The angle of the VCP cut `elevation_number` names, **exactly as the cut
    /// table stores it** — not wrap-corrected, not rounded, not the sweep's
    /// median.
    cut_angle_deg: Option<f64>,
    /// Whether the *original* sweep's radials carried a velocity moment.
    ///
    /// [`crate::sampler::VolumeSampler`] resolves a split cut by preferring the
    /// half that carries **no** velocity: reflectivity belongs to the
    /// surveillance half, which reaches 460 km against the Doppler half's 300,
    /// and the two are otherwise indistinguishable — on a measured KMPX VCP 212
    /// volume all three members of the 0.4834° cut report the same cut angle
    /// *and* the same median. The rule discriminates on
    /// `radial.velocity().is_none()`, so a reflectivity payload without this
    /// bit looks like a surveillance half and the chooser falls through to
    /// "newest member" — on a real volume a SAILS *Doppler* repeat.
    ///
    /// **The bit, not the decision.** Applying the preference at extraction
    /// time would put a second copy of the sampler's rule in this module.
    carried_velocity: bool,
    /// The Nyquist velocity this sweep's cut **declared**, m/s, or `None` when
    /// the volume this payload was extracted from declared none for it.
    declared_nyquist_ms: Option<f64>,
    /// When the RDA flew this sweep — the **earliest** collection timestamp on
    /// any of its radials, milliseconds since the Unix epoch, `0` when none of
    /// them carried one.
    collected_ms: i64,
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
    /// carried only for the hybrid hydrometeor classification, and empty for
    /// every other product.
    extras: Vec<(u8, MomentPayload)>,
}

/// A moment block in the fixed-point form the decoder produced it in, so
/// `MomentData::from_fixed_point` can rebuild it exactly.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MomentPayload {
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
    pub(crate) gates: Vec<u8>,
}

impl RenderInput {
    /// The reachable subset of `scan` for this request, or `None` when the
    /// request cannot be rendered at all — a product with no Level II moment
    /// behind it, or no sweep in the requested tilt family carrying one.
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
        // planes and KDP the ΦDP planes to the worker (`crate::derive`).
        let slot = product
            .moment_slot()
            .or_else(|| crate::derive::derived_slot(product))?;
        // The HHC reads moments beyond its slot; so does the KDP derivation,
        // whose estimator gates on Z and ρHV.
        let all_moments = matches!(
            product,
            RadarProduct::HydrometeorClassification | RadarProduct::SpecificDifferentialPhase
        );
        let cuts = CutTable::of_pattern(pattern);
        let sweeps = collect_sweeps(sweeps.iter().copied(), &cuts, slot, all_moments);
        // Empty on a volume that carries the product nowhere. The renderer
        // answers `None` for that, so this must too.
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
            // cache.
            storm_motion_override: (product == RadarProduct::StormRelativeVelocity)
                .then_some(storm_motion_override)
                .flatten(),
            env_heights_km_msl: None,
            melting_layer_product: None,
            rpg_storm_motion: None,
            srv_fallback: crate::srv::SrvFallback::default(),
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
        // `None` for a Level III product: no Level II moment stands behind it.
        // Which products need every tilt is [`RadarProduct::reads_whole_volume`],
        // *read* rather than restated: the live chunk feed narrows its download
        // by the same predicate, and a second copy here is how an SRV pane came
        // to be handed a volume the feed had skipped cuts of.
        let whole_volume = product.reads_whole_volume();

        let all_moments = product == RadarProduct::HydrometeorClassification;
        let cuts = CutTable::of(scan);
        let sweeps = if whole_volume {
            collect_sweeps(scan.sweeps().iter(), &cuts, slot, all_moments)
        } else {
            // One sweep: whichever `find_sweep` would have chosen. Selecting
            // here, against the whole volume, is the point — the reconstructed
            // scan has only this sweep to offer.
            let sweep = crate::render::find_sweep_owner(scan, product, elevation)?;
            vec![sweep_data(sweep, &cuts, slot, false)]
        };
        if sweeps.is_empty() {
            return None;
        }

        Some(Self {
            product,
            elevation,
            radar_lat,
            radar_lon,
            storm_motion_override,
            env_heights_km_msl: if product.reads_env_heights() {
                env_heights_km_msl
            } else {
                // Nothing else reads them; carrying them anyway would make
                // byte-identity of other products' payloads depend on an
                // unrelated cache.
                None
            },
            melting_layer_product: None,
            rpg_storm_motion: None,
            srv_fallback: crate::srv::SrvFallback::default(),
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

    /// The user's storm motion vector, knots and degrees-from, or `None` for
    /// "no override" — the chain's next rung applies.
    pub fn storm_motion_override(&self) -> Option<(f32, f32)> {
        self.storm_motion_override
    }

    /// The site's environmental 0 °C / −20 °C heights, km MSL, or `None` — the
    /// hail products then render nothing, and the HHC applies its defaults.
    pub fn env_heights_km_msl(&self) -> Option<(f64, f64)> {
        self.env_heights_km_msl
    }

    /// Stamp the RPG's own melting layer object for this volume onto a payload
    /// the extraction has already built.
    #[must_use]
    pub fn with_melting_layer_product(mut self, object: Option<std::sync::Arc<Vec<u8>>>) -> Self {
        self.melting_layer_product =
            object.filter(|_| self.product == RadarProduct::HydrometeorClassification);
        self
    }

    /// The RPG's own melting layer object for this volume, or `None` — the
    /// classification then falls to [`crate::hca::resolve_melting_layer`]'s
    /// next source.
    pub fn melting_layer_product(&self) -> Option<&std::sync::Arc<Vec<u8>>> {
        self.melting_layer_product.as_ref()
    }

    /// Stamp the RPG's own storm motion vector for this volume onto a payload
    /// the extraction has already built.
    #[must_use]
    pub fn with_rpg_storm_motion(mut self, motion: Option<(f32, f32)>) -> Self {
        self.rpg_storm_motion =
            motion.filter(|_| self.product == RadarProduct::StormRelativeVelocity);
        self
    }

    /// The RPG's own storm motion vector for this volume, or `None` — SRV then
    /// falls to [`crate::srv::storm_motion`]'s next rung.
    pub fn rpg_storm_motion(&self) -> Option<(f32, f32)> {
        self.rpg_storm_motion
    }

    /// Stamp which derived rung SRV should fall to when no override and no
    /// `N0S` vector reached this payload. Dropped for every product but SRV.
    #[must_use]
    pub fn with_srv_fallback(mut self, fallback: crate::srv::SrvFallback) -> Self {
        if self.product == RadarProduct::StormRelativeVelocity {
            self.srv_fallback = fallback;
        }
        self
    }

    /// Which derived rung SRV falls to here — the 0–6 km mean wind unless a
    /// reader asked for the Bunkers right-mover.
    pub fn srv_fallback(&self) -> crate::srv::SrvFallback {
        self.srv_fallback
    }

    /// The three storm-motion facts as the one bundle every derivation seam
    /// takes.
    pub fn storm_motion(&self) -> crate::srv::MotionInputs {
        crate::srv::MotionInputs {
            user_override: self.storm_motion_override,
            rpg: self.rpg_storm_motion,
            fallback: self.srv_fallback,
        }
    }

    /// Stamp each carried sweep with the Nyquist velocity its cut declared,
    /// looked up in `declared` by the sweep's own elevation number.
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
    /// The coverage pattern is rebuilt from the angles the payload carries,
    /// sized to the largest elevation number in it, because
    /// [`crate::sampler::VolumeSampler`] keys its tilt ladder on
    /// `coverage_pattern().elevation_cuts()[sweep.elevation_number() - 1]`.
    /// Slots no carried sweep names hold a **copy of the nearest carried
    /// angle** rather than a sentinel: they are unreachable by construction,
    /// and a `NaN` in a table someone later scans linearly is a landmine for no
    /// gain. Every other field of every cut is left at a neutral default — a
    /// fabricated SAILS flag would be a lie a consumer could act on.
    ///
    /// If any carried sweep has no cut angle (see
    /// [`SweepData::cut_angle_deg`]), the table is rebuilt **empty**, which is
    /// what the original looked like and what the sampler refuses.
    pub fn to_scan(&self) -> Scan {
        // Always `Some`: both constructors refuse a product with no Level II
        // field. Degrading to "no moments" rather than panicking keeps a
        // hand-crafted payload off a message port from taking the tab down.
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
                        // `MomentSlot` `get_moment` resolves this product to.
                        let mut slots = place_moment(slot, moment);
                        // The extras go back on the fields their tags name —
                        // the HHC's full-radial reconstruction.
                        for (code, payload) in &radial.extras {
                            if let Some(extra_slot) = ALL_SLOTS.get(*code as usize) {
                                place_into(&mut slots, *extra_slot, payload.to_moment_data());
                            }
                        }
                        // The Doppler-half marker. Only when the sweep really
                        // carried velocity and none of it travelled.
                        if sweep.carried_velocity && slots.1.is_none() {
                            slots.1 = Some(doppler_marker());
                        }
                        let (reflectivity, velocity, spectrum_width, zdr, phi, rho) = slots;
                        Radial::new(
                            // The sweep's own clock, stamped across its
                            // radials exactly as `elevation_angle` is.
                            sweep.collected_ms,
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
    /// for why the table is sized this way.
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
            // reachable from a hand-built payload; an empty table is honest.
            return placeholder_coverage_pattern(self.vcp);
        };
        // The declared table, when the payload carries one that can key every
        // sweep in it — the whole table the radar was flying, not the part this
        // volume got to. The reconstruction below stands in only for a payload
        // built by hand or by an older sender.
        if self.declared_cut_angles_deg.len() >= len {
            return rebuild_pattern(self.vcp, &self.declared_cut_angles_deg);
        }
        let mut table = vec![None; len];
        for (index, angle) in &angles {
            table[*index] = Some(*angle);
        }
        // Unclaimed slots take the nearest claimed angle: unreachable from this
        // scan's sweeps either way, and free of values a later linear scan
        // would have to special-case.
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
pub(crate) fn rebuild_pattern(vcp: u16, angles: &[f64]) -> VolumeCoveragePattern {
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
/// no sweep can match, so a whole-volume payload handed to a *frame* consumer
/// answers `None` rather than quietly drawing whatever tilt was nearest.
///
/// Two obvious choices are wrong. **`0.0` is not unmatchable**: `find_sweep`
/// matches within `render::ELEVATION_WINDOW` of a sweep's *median*, and
/// settling drift puts a real base tilt as low as 0.283°. **`NaN` breaks the
/// type**: `RenderInput` derives `PartialEq`, and `NaN != NaN` would make a
/// whole-volume payload unequal to itself.
///
/// `-1000.0` is finite, orders of magnitude outside the ±90° an elevation can
/// occupy, and survives the `f32` wire round trip exactly.
pub const NO_ELEVATION_DEG: f32 = -1000.0;

/// The scan's elevation cut angles, indexed the way a sweep's
/// `elevation_number` indexes them.
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
/// has to be materialised on the field the rule looks at.
///
/// Zero gates, not fabricated ones: a consumer that reads this moment's values
/// gets an empty list, and must never be handed numbers a wind fit or a
/// dealiaser could take for measurements. Every whole-volume product that reads
/// velocity carries the *real* velocity as its slot moment or in the extras.
fn doppler_marker() -> MomentData {
    MomentData::from_fixed_point(0, 0, 0, 8, 1.0, 0.0, Vec::new())
}

/// One reconstructed cut: the angle, and neutral values everywhere else.
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

/// Every sweep carrying `slot`'s moment, in input order. With `all_moments`
/// (the HHC), a sweep carrying *any* moment qualifies — the split-cut Doppler
/// halves carry no differential phase but donate the velocity the
/// classification grafts in.
fn collect_sweeps<'s>(
    sweeps: impl Iterator<Item = &'s Sweep>,
    cuts: &CutTable<'_>,
    slot: MomentSlot,
    all_moments: bool,
) -> Vec<SweepData> {
    sweeps
        .filter_map(|sweep| {
            let radials = sweep.radials();
            let wanted = radials.iter().any(|radial| {
                if all_moments {
                    ALL_SLOTS.iter().any(|s| s.read(radial).is_some())
                } else {
                    slot.read(radial).is_some()
                }
            });
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

/// When a sweep was flown: the earliest positive collection timestamp on its
/// radials, milliseconds since the Unix epoch, or `0` when it carries none.
pub(crate) fn sweep_collected_ms(radials: &[Radial]) -> i64 {
    radials
        .iter()
        .map(Radial::collection_timestamp)
        .filter(|&ms| ms > 0)
        .min()
        .unwrap_or(0)
}

/// Flatten one sweep, carrying `slot`'s moment and nothing else.
fn sweep_data(
    sweep: &Sweep,
    cuts: &CutTable<'_>,
    slot: MomentSlot,
    all_moments: bool,
) -> SweepData {
    let radials = sweep.radials();
    SweepData {
        // The sweep's **median**, and it has to be: `to_scan` stamps this one
        // value onto every reconstructed radial, and `find_sweep` matches on
        // the median, so the first radial's angle here would describe a tilt
        // the sweep never flew.
        elevation_angle: crate::volumetric::sweep_elevation_deg(radials)
            .map(|e| e as f32)
            .unwrap_or(0.0),
        // The **sweep's** number, not the first radial's and not the payload
        // index. `Sweep::new` takes it separately from the radials.
        elevation_number: sweep.elevation_number(),
        cut_angle_deg: cuts.angle_for(sweep.elevation_number()),
        // Read off **every** radial: this is a claim about the sweep — "this is
        // a split cut's Doppler half" — and one radial cannot make it for 720.
        carried_velocity: radials
            .iter()
            .any(|r| MomentSlot::Velocity.read(r).is_some()),
        // Filled in afterwards by `with_declared_nyquist`, never here: the
        // number is not on the radials, so this function has no honest way to
        // know it.
        declared_nyquist_ms: None,
        // A whole pass over the radials rather than `first()`, because a
        // sweep's radials are in *collection* order. Zero is the decoder's own
        // "no timestamp" value and is filtered rather than minimised over.
        collected_ms: sweep_collected_ms(radials),
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

/// Put `moment` back on the field it was read from — the inverse of
/// [`MomentSlot::read`]. `slot` is `None` only for a product with no Level II
/// field, and the moment is then dropped rather than guessed at.
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
    /// Generic over [`DataMoment`] rather than taking a `MomentData`, so the
    /// **clutter-filter power** moment — a `CFPMomentData`, a distinct type
    /// over the same block — encodes through this one implementation too.
    pub(crate) fn from_moment_data(moment: &impl DataMoment) -> Self {
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

    /// The clutter-filter power moment these bytes describe, for the one
    /// payload that carries one. Same block, a different newtype over it.
    pub(crate) fn to_cfp_moment_data(&self) -> nexrad_model::data::CFPMomentData {
        nexrad_model::data::CFPMomentData::from_fixed_point(
            self.gate_count,
            self.first_gate_range_m,
            self.gate_interval_m,
            self.word_size,
            self.scale,
            self.offset,
            self.gates.clone(),
        )
    }

    pub(crate) fn to_moment_data(&self) -> MomentData {
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

/// Undo `MomentDataBlock`'s `raw as f64 * 0.001`. `0.001` is not exact in
/// binary, so the product is not exactly the integer metres that went in;
/// rounding recovers it for every `u16` the field can hold.
fn km_to_metres(km: f64) -> u16 {
    (km * 1000.0).round().clamp(0.0, u16::MAX as f64) as u16
}

// ── Codec ────────────────────────────────────────────────────────────────────

/// Identifies the payload, so a message that is not one fails on its first four
/// bytes instead of being read as a wildly-sized allocation.
const MAGIC: [u8; 4] = *b"RDRI";

/// Bumped whenever the layout below changes. The two ends of a worker boundary
/// can be different builds — see `squallar-web`'s build-token handshake — so a
/// mismatch has to be a clean `None`, not a misparse.
///
/// 2: storm motion override. 3: removed the wind levels (the dealias-seeding
/// profile is fit from the payload's own velocity tilts). 4: environmental
/// heights, for the hail products. 5: per-radial extra moments, for the hybrid
/// hydrometeor classification. 6: coverage pattern number, and per sweep the
/// `elevation_number`, VCP cut angle and carried-velocity bit — the whole
/// input to the tilt ladder. 7: the pattern's whole declared cut-angle table,
/// so a reconstructed scan can tell a pattern flown to the top from one
/// stopped a third of the way up. 8: per-sweep declared Nyquist velocity, so
/// the worker does not estimate while the main thread declares. 9: per-sweep
/// collection timestamp, for the ladder's per-rung ages. 10: the RPG's melting
/// layer object (`N0M`) as a length-prefixed blob. 11: the RPG's storm motion
/// vector (`N0S` halfwords 51 and 52). 12: the derived-rung preference
/// (`crate::srv::SrvFallback`) as one byte.
const FORMAT_VERSION: u16 = 12;

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

        match &self.melting_layer_product {
            None => out.extend_from_slice(&0u32.to_le_bytes()),
            Some(object) => {
                out.extend_from_slice(&(object.len() as u32).to_le_bytes());
                out.extend_from_slice(object);
            }
        }

        match self.rpg_storm_motion {
            None => out.push(0),
            Some((speed_kt, direction_deg)) => {
                out.push(1);
                out.extend_from_slice(&speed_kt.to_le_bytes());
                out.extend_from_slice(&direction_deg.to_le_bytes());
            }
        }

        out.push(match self.srv_fallback {
            crate::srv::SrvFallback::MeanWind => 0,
            crate::srv::SrvFallback::BunkersRightMover => 1,
        });

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
            // Unconditional rather than flagged: `0` is already this field's
            // "no clock" value on the struct.
            out.extend_from_slice(&sweep.collected_ms.to_le_bytes());
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
        // product's source slot. (KDP passes through the derived arm — its
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

        // A zero length is the absent case; the object is never empty.
        let melting_layer_len = r.u32()?;
        let melting_layer_product = if melting_layer_len == 0 {
            None
        } else {
            Some(std::sync::Arc::new(
                r.take(melting_layer_len as usize)?.to_vec(),
            ))
        };

        let rpg_storm_motion = match r.u8()? {
            0 => None,
            1 => Some((r.f32()?, r.f32()?)),
            _ => return None,
        };

        // Refused rather than defaulted: an unknown discriminant means the two
        // ends disagree about the layout despite the version matching, and
        // every byte after this point is misaligned anyway.
        let srv_fallback = match r.u8()? {
            0 => crate::srv::SrvFallback::MeanWind,
            1 => crate::srv::SrvFallback::BunkersRightMover,
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
        // against what is actually left. Twenty since the collection timestamp
        // joined it — see `encoded_len`.
        let mut sweeps = Vec::with_capacity(r.bounded(sweep_count, 20)?);
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
            let collected_ms = r.i64()?;
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
                collected_ms,
                radials,
            });
        }

        // Trailing bytes mean the two ends disagree about the layout even
        // though the version matched.
        r.at_end().then_some(Self {
            product,
            elevation,
            radar_lat,
            radar_lon,
            storm_motion_override,
            env_heights_km_msl,
            melting_layer_product,
            rpg_storm_motion,
            srv_fallback,
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
        let melting_layer = 4 + self.melting_layer_product.as_ref().map_or(0, |o| o.len());
        let rpg_motion = 1 + if self.rpg_storm_motion.is_some() {
            8
        } else {
            0
        };
        // One byte, always present: the preference has no absent case.
        let fallback = 1;
        let sweeps: usize = self
            .sweeps
            .iter()
            .map(|s| {
                // 4 elevation angle + 1 elevation number + 1 carried-velocity
                // flag + 1 cut-angle flag (+ 8 for the angle) + 1
                // declared-Nyquist flag (+ 8 for the value) + 8 collection
                // timestamp + 4 radial count.
                20 + if s.cut_angle_deg.is_some() { 8 } else { 0 }
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
        header + motion + env + melting_layer + rpg_motion + fallback + 2 + declared + 4 + sweeps
    }
}

/// One moment payload's wire form, shared by the slot moment and the extras.
pub(crate) fn encode_moment(out: &mut Vec<u8>, moment: &MomentPayload) {
    out.extend_from_slice(&moment.gate_count.to_le_bytes());
    out.extend_from_slice(&moment.first_gate_range_m.to_le_bytes());
    out.extend_from_slice(&moment.gate_interval_m.to_le_bytes());
    out.push(moment.word_size);
    out.extend_from_slice(&moment.scale.to_le_bytes());
    out.extend_from_slice(&moment.offset.to_le_bytes());
    out.extend_from_slice(&(moment.gates.len() as u32).to_le_bytes());
    out.extend_from_slice(&moment.gates);
}

pub(crate) fn decode_moment(r: &mut Reader) -> Option<MomentPayload> {
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

#[cfg(test)]
mod tests;
