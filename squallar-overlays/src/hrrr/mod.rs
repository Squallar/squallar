//! HRRR model data fetch and types.
//!
//! Fields are byte-ranged out of the `noaa-hrrr-bdp-pds` S3 bucket; see
//! [`fetch`]. Every parameter is published at every forecast hour the run
//! carries, so the hour a fetch asks for is the caller's choice; a parameter
//! only declares how low it may go. The windowed maxima — updraft helicity,
//! `MAXREF` and the −10 °C reflectivity maximum — cannot go to f00; see
//! [`ModelParameter::min_forecast_hour`].

pub mod fetch;
pub mod fields;
pub mod lambert;

use squallar_geo::GeoBounds;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ModelParameter {
    /// Surface-Based CIN (J/kg, ≤ 0).
    SurfaceBasedCin,
    /// Mixed-Layer CIN (180-0 mb AGL, J/kg, ≤ 0).
    MixedLayerCin,
    /// Surface-Based CAPE (J/kg, ≥ 0).
    SurfaceBasedCape,
    /// Mixed-Layer CAPE (180-0 mb AGL, J/kg, ≥ 0).
    MixedLayerCape,
    /// Most-Unstable CAPE (255-0 mb AGL, J/kg, ≥ 0).
    MostUnstableCape,
    /// Best Lifted Index (500-1000 mb, K → °C).
    LiftedIndex,

    /// 0-1 km Storm-Relative Helicity (m²/s²).
    Srh1km,
    /// 0-3 km Storm-Relative Helicity (m²/s²).
    Srh3km,
    /// Max Updraft Helicity 2-5 km AGL (m²/s²).
    MaxUH2to5km,
    /// Max Updraft Helicity 0-2 km AGL (m²/s²).
    MaxUH0to2km,

    /// 0-6 km Bulk Wind Shear magnitude (GRIB U+V m/s → display kt).
    BulkShear6km,
    /// Surface Wind Gust (GRIB m/s → display kt).
    SurfaceWindGust,

    /// Precipitable Water (GRIB kg/m² → display in).
    PrecipitableWater,

    /// 2 m Temperature (GRIB K → display °F).
    Temperature2m,
    /// 2 m Dewpoint (GRIB K → display °F).
    Dewpoint2m,

    /// Surface Visibility (GRIB m → display mi).
    Visibility,

    // ── Storm-scale fields ────────────────────────────────────────────
    //
    // The forecast analogues of what squallar already draws from radar.
    // Appended after `Visibility` so no existing parameter's `sort_order`
    // moves: that number is the catalogue's row order rather than an identity,
    // but a shift would reshuffle every user's field list for no reason.
    /// Composite Reflectivity, whole column (dBZ).
    CompositeReflectivity,
    /// Reflectivity 1 km above ground (dBZ).
    Reflectivity1km,
    /// Reflectivity 4 km above ground (dBZ).
    Reflectivity4km,
    /// Reflectivity on the −10 °C isotherm, hourly maximum (dBZ).
    ReflectivityM10C,
    /// 1 km Reflectivity, hourly maximum (dBZ).
    MaxReflectivity,
    /// Echo Top height (GRIB m → display kft).
    EchoTop,
    /// Vertically Integrated Liquid (kg/m²).
    VerticallyIntegratedLiquid,
}

impl ModelParameter {
    pub const fn all() -> &'static [ModelParameter] {
        &[
            ModelParameter::SurfaceBasedCin,
            ModelParameter::MixedLayerCin,
            ModelParameter::SurfaceBasedCape,
            ModelParameter::MixedLayerCape,
            ModelParameter::MostUnstableCape,
            ModelParameter::LiftedIndex,
            ModelParameter::Srh1km,
            ModelParameter::Srh3km,
            ModelParameter::MaxUH2to5km,
            ModelParameter::MaxUH0to2km,
            ModelParameter::BulkShear6km,
            ModelParameter::SurfaceWindGust,
            ModelParameter::PrecipitableWater,
            ModelParameter::Temperature2m,
            ModelParameter::Dewpoint2m,
            ModelParameter::Visibility,
            ModelParameter::CompositeReflectivity,
            ModelParameter::Reflectivity1km,
            ModelParameter::Reflectivity4km,
            ModelParameter::ReflectivityM10C,
            ModelParameter::MaxReflectivity,
            ModelParameter::EchoTop,
            ModelParameter::VerticallyIntegratedLiquid,
        ]
    }

    /// The **lowest** forecast hour of a run this parameter may be fetched
    /// from. f00, the analysis, for every instantaneous field.
    ///
    /// A floor rather than a value: every parameter is published at every
    /// forecast hour the run carries (f00-f18, or f00-f48 on 00/06/12/18Z), so
    /// the hour is the caller's to choose and this only says how low it may go.
    /// Callers clamp the requested hour *up* to it.
    ///
    /// **A windowed maximum at f00 is identically 0.0 everywhere**: it is a
    /// maximum over the forecast period, and at f00 that period has zero
    /// length. f01 is the first hour with a real window (`0-1 hour max fcst`),
    /// which is why [`HrrrGridData::forecast_hour`] reaches the UI: a 0-1 h
    /// maximum must not be presented as the analysis. [`Self::is_windowed`] is
    /// that reason stated on its own, and the two must agree —
    /// `a_windowed_parameter_never_requests_f00` is what holds them together.
    pub fn min_forecast_hour(&self) -> u8 {
        match self {
            ModelParameter::MaxUH2to5km
            | ModelParameter::MaxUH0to2km
            | ModelParameter::ReflectivityM10C
            | ModelParameter::MaxReflectivity => 1,
            _ => 0,
        }
    }

    /// Whether this parameter's record is a maximum over the hour ending at the
    /// forecast time rather than an instantaneous reading — the third component
    /// of the key [`fetch::byte_range`] selects on, via
    /// [`fetch::record_forecast`].
    ///
    /// **`REFC`, `REFD` and `MAXREF` are not interchangeable here.** `REFD` at
    /// `263 K level` is published *twice* in every index, once instantaneous
    /// and once as the hourly maximum, and only this flag tells them apart;
    /// `MAXREF` is published *only* as a maximum; and the 1 km / 4 km `REFD`
    /// records and `REFC` are instantaneous only, so marking one of them
    /// windowed selects no record at all rather than the wrong one.
    pub fn is_windowed(&self) -> bool {
        matches!(
            self,
            ModelParameter::MaxUH2to5km
                | ModelParameter::MaxUH0to2km
                | ModelParameter::ReflectivityM10C
                | ModelParameter::MaxReflectivity
        )
    }

    /// Whether this parameter's colour bar is read as **bands** rather than as
    /// a continuous ramp.
    ///
    /// Reflectivity is: the number a reader takes off a dBZ bar is the band's
    /// floor, and a colour interpolated between 45 and 50 dBZ names no band.
    /// Everything else here is a continuous quantity whose bar is a wash.
    ///
    /// [`Self::color_for_value`] and `fields::spec(..).scale.is_gradient` must
    /// state the same thing — the raster paints through the former and the
    /// legend draws the latter, so a disagreement is a legend explaining a
    /// picture that is not on screen.
    pub fn is_banded(&self) -> bool {
        matches!(
            self,
            ModelParameter::CompositeReflectivity
                | ModelParameter::Reflectivity1km
                | ModelParameter::Reflectivity4km
                | ModelParameter::ReflectivityM10C
                | ModelParameter::MaxReflectivity
        )
    }

    pub fn is_composite(&self) -> bool {
        matches!(self, ModelParameter::BulkShear6km)
    }

    /// For composite parameters, the (var, level) pairs to fetch; values are
    /// merged in `fetch::fetch_composite_hrrr_data()`.
    pub fn composite_parts(&self) -> Option<Vec<(&'static str, &'static str)>> {
        match self {
            ModelParameter::BulkShear6km => Some(vec![
                ("VUCSH", "0-6000 m above ground"),
                ("VVCSH", "0-6000 m above ground"),
            ]),
            _ => None,
        }
    }

    /// The GRIB2 variable abbreviation, exactly as the `.idx` spells it — matched
    /// literally, not normalised. Panics for composite parameters.
    pub fn grib_var(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => "CIN",
            ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => "CAPE",
            ModelParameter::LiftedIndex => "LFTX",
            ModelParameter::Srh1km | ModelParameter::Srh3km => "HLCY",
            ModelParameter::MaxUH2to5km | ModelParameter::MaxUH0to2km => "MXUPHL",
            ModelParameter::SurfaceWindGust => "GUST",
            ModelParameter::PrecipitableWater => "PWAT",
            ModelParameter::Temperature2m => "TMP",
            ModelParameter::Dewpoint2m => "DPT",
            ModelParameter::Visibility => "VIS",
            ModelParameter::CompositeReflectivity => "REFC",
            ModelParameter::Reflectivity1km
            | ModelParameter::Reflectivity4km
            | ModelParameter::ReflectivityM10C => "REFD",
            ModelParameter::MaxReflectivity => "MAXREF",
            ModelParameter::EchoTop => "RETOP",
            ModelParameter::VerticallyIntegratedLiquid => "VIL",
            ModelParameter::BulkShear6km => {
                panic!("BulkShear6km is composite - use composite_parts()")
            }
        }
    }

    pub fn grib_level(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin
            | ModelParameter::SurfaceBasedCape
            | ModelParameter::SurfaceWindGust
            | ModelParameter::Visibility => "surface",
            ModelParameter::MixedLayerCin | ModelParameter::MixedLayerCape => {
                "180-0 mb above ground"
            }
            ModelParameter::MostUnstableCape => "255-0 mb above ground",
            ModelParameter::LiftedIndex => "500-1000 mb",
            ModelParameter::Srh1km => "1000-0 m above ground",
            ModelParameter::Srh3km => "3000-0 m above ground",
            // MXUPHL layers are top-bound-first in the `.idx`. Do not
            // "normalise" them to match the ascending layers elsewhere: HRRR is
            // not self-consistent about bound order and the index is matched
            // literally, so an ascending spelling selects no record at all.
            ModelParameter::MaxUH2to5km => "5000-2000 m above ground",
            ModelParameter::MaxUH0to2km => "2000-0 m above ground",
            ModelParameter::PrecipitableWater => "entire atmosphere (considered as a single layer)",
            ModelParameter::Temperature2m | ModelParameter::Dewpoint2m => "2 m above ground",
            // `REFC` and `VIL` spell it `entire atmosphere` bare, where `PWAT`
            // above spells it `entire atmosphere (considered as a single
            // layer)`. Both are in the same index and the match is literal, so
            // neither may be "normalised" into the other.
            ModelParameter::CompositeReflectivity | ModelParameter::VerticallyIntegratedLiquid => {
                "entire atmosphere"
            }
            ModelParameter::Reflectivity1km | ModelParameter::MaxReflectivity => {
                "1000 m above ground"
            }
            ModelParameter::Reflectivity4km => "4000 m above ground",
            // The −10 °C isotherm, named by its absolute temperature. This is
            // the one level a squallar parameter selects that repeats in the
            // index, and [`Self::is_windowed`] is what picks between the two.
            ModelParameter::ReflectivityM10C => "263 K level",
            ModelParameter::EchoTop => "cloud top",
            ModelParameter::BulkShear6km => {
                panic!("BulkShear6km is composite - use composite_parts()")
            }
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin => "Surface-Based CIN",
            ModelParameter::MixedLayerCin => "Mixed-Layer CIN",
            ModelParameter::SurfaceBasedCape => "Surface-Based CAPE",
            ModelParameter::MixedLayerCape => "Mixed-Layer CAPE",
            ModelParameter::MostUnstableCape => "Most-Unstable CAPE",
            ModelParameter::LiftedIndex => "Lifted Index",
            ModelParameter::Srh1km => "0-1 km SRH",
            ModelParameter::Srh3km => "0-3 km SRH",
            ModelParameter::MaxUH2to5km => "Max UH 2-5 km",
            ModelParameter::MaxUH0to2km => "Max UH 0-2 km",
            ModelParameter::BulkShear6km => "0-6 km Bulk Shear",
            ModelParameter::SurfaceWindGust => "Surface Wind Gust",
            ModelParameter::PrecipitableWater => "Precipitable Water",
            ModelParameter::Temperature2m => "2m Temperature",
            ModelParameter::Dewpoint2m => "2m Dewpoint",
            ModelParameter::Visibility => "Visibility",
            ModelParameter::CompositeReflectivity => "Composite Reflectivity",
            ModelParameter::Reflectivity1km => "1 km Reflectivity",
            ModelParameter::Reflectivity4km => "4 km Reflectivity",
            ModelParameter::ReflectivityM10C => "-10 \u{b0}C Reflectivity",
            ModelParameter::MaxReflectivity => "Max Reflectivity",
            ModelParameter::EchoTop => "Echo Top",
            ModelParameter::VerticallyIntegratedLiquid => "Vertically Integrated Liquid",
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin => "SBCIN",
            ModelParameter::MixedLayerCin => "MLCIN",
            ModelParameter::SurfaceBasedCape => "SBCAPE",
            ModelParameter::MixedLayerCape => "MLCAPE",
            ModelParameter::MostUnstableCape => "MUCAPE",
            ModelParameter::LiftedIndex => "LI",
            ModelParameter::Srh1km => "SRH1",
            ModelParameter::Srh3km => "SRH3",
            ModelParameter::MaxUH2to5km => "UH2-5",
            ModelParameter::MaxUH0to2km => "UH0-2",
            ModelParameter::BulkShear6km => "SHR6",
            ModelParameter::SurfaceWindGust => "GUST",
            ModelParameter::PrecipitableWater => "PWAT",
            ModelParameter::Temperature2m => "T2m",
            ModelParameter::Dewpoint2m => "Td2m",
            ModelParameter::Visibility => "VIS",
            ModelParameter::CompositeReflectivity => "REFC",
            ModelParameter::Reflectivity1km => "REF1",
            ModelParameter::Reflectivity4km => "REF4",
            ModelParameter::ReflectivityM10C => "REF-10C",
            ModelParameter::MaxReflectivity => "MAXREF",
            ModelParameter::EchoTop => "ETOP",
            ModelParameter::VerticallyIntegratedLiquid => "VIL",
        }
    }

    pub fn unit_label(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin
            | ModelParameter::MixedLayerCin
            | ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => "J/kg",
            ModelParameter::LiftedIndex => "°C",
            ModelParameter::Srh1km
            | ModelParameter::Srh3km
            | ModelParameter::MaxUH2to5km
            | ModelParameter::MaxUH0to2km => "m²/s²",
            ModelParameter::BulkShear6km | ModelParameter::SurfaceWindGust => "kt",
            ModelParameter::PrecipitableWater => "in",
            ModelParameter::Temperature2m | ModelParameter::Dewpoint2m => "°F",
            ModelParameter::Visibility => "mi",
            ModelParameter::CompositeReflectivity
            | ModelParameter::Reflectivity1km
            | ModelParameter::Reflectivity4km
            | ModelParameter::ReflectivityM10C
            | ModelParameter::MaxReflectivity => "dBZ",
            ModelParameter::EchoTop => "kft",
            ModelParameter::VerticallyIntegratedLiquid => "kg/m\u{b2}",
        }
    }

    /// Convert a raw GRIB2 value to display units: identity for
    /// CIN/CAPE/SRH/UH, for the five reflectivity fields (already dBZ) and for
    /// VIL (already kg/m²); m/s → kt, K → °F, kg/m² → in, m → mi, m → kft for
    /// the echo top, and K → °C for LFTX (a temperature differential, so the
    /// number is already °C).
    pub fn convert_for_display(&self, value: f32) -> f32 {
        match self {
            ModelParameter::SurfaceBasedCin
            | ModelParameter::MixedLayerCin
            | ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape
            | ModelParameter::Srh1km
            | ModelParameter::Srh3km
            | ModelParameter::MaxUH2to5km
            | ModelParameter::MaxUH0to2km
            // dBZ and kg/m² are the units the grid already carries.
            | ModelParameter::CompositeReflectivity
            | ModelParameter::Reflectivity1km
            | ModelParameter::Reflectivity4km
            | ModelParameter::ReflectivityM10C
            | ModelParameter::MaxReflectivity
            | ModelParameter::VerticallyIntegratedLiquid => value,
            // LFTX is a temperature *difference* so K ≡ °C.
            ModelParameter::LiftedIndex => value,
            ModelParameter::BulkShear6km | ModelParameter::SurfaceWindGust => value * 1.94384,
            ModelParameter::PrecipitableWater => value / 25.4,
            ModelParameter::Temperature2m | ModelParameter::Dewpoint2m => {
                value * 9.0 / 5.0 - 459.67
            }
            ModelParameter::Visibility => value / 1609.344,
            // `RETOP` is metres; a storm top is read in kilofeet.
            ModelParameter::EchoTop => value / 304.8,
        }
    }

    /// Format a grid value (raw GRIB2 units) for hover tooltip display. Empty
    /// for a non-finite value: a missing grid point has no reading to report.
    pub fn format_value(&self, value: f32) -> String {
        if !value.is_finite() {
            return String::new();
        }
        format!("{}: {}", self.short_name(), self.format_magnitude(value))
    }

    /// A bare magnitude with units, e.g. `"0 m²/s²"`, where the parameter is
    /// already named by surrounding text.
    pub fn format_magnitude(&self, value: f32) -> String {
        let display = self.convert_for_display(value);
        match self {
            ModelParameter::SurfaceBasedCin
            | ModelParameter::MixedLayerCin
            | ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape
            | ModelParameter::Srh1km
            | ModelParameter::Srh3km
            | ModelParameter::MaxUH2to5km
            | ModelParameter::MaxUH0to2km
            | ModelParameter::BulkShear6km
            | ModelParameter::SurfaceWindGust
            | ModelParameter::Temperature2m
            | ModelParameter::Dewpoint2m
            | ModelParameter::CompositeReflectivity
            | ModelParameter::Reflectivity1km
            | ModelParameter::Reflectivity4km
            | ModelParameter::ReflectivityM10C
            | ModelParameter::MaxReflectivity => {
                format!("{:.0} {}", display, self.unit_label())
            }
            ModelParameter::LiftedIndex
            | ModelParameter::EchoTop
            | ModelParameter::VerticallyIntegratedLiquid => {
                format!("{:.1} {}", display, self.unit_label())
            }
            ModelParameter::PrecipitableWater | ModelParameter::Visibility => {
                format!("{:.2} {}", display, self.unit_label())
            }
        }
    }

    /// Map a raw GRIB2 value to an RGBA color for rendering.
    ///
    /// The NaN guard is load-bearing: the eleven hand-written ramps below are
    /// descending `if` chains ending in an unguarded `else` and NaN fails every
    /// comparison, so a missing point would paint the *most extreme* colour on
    /// the scale. The three storm-scale ramps walk a stop table instead and
    /// answer transparent for a NaN on their own — a property of the walk, not
    /// a reason to drop the guard the other eleven still need.
    pub fn color_for_value(&self, value: f32) -> [u8; 4] {
        if !value.is_finite() {
            return [0, 0, 0, 0];
        }
        match self {
            // CIN is special: works with raw negative values.
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => cin_color(value),
            ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => cape_color(value),
            ModelParameter::LiftedIndex => li_color(value),
            ModelParameter::Srh1km | ModelParameter::Srh3km => srh_color(value),
            ModelParameter::MaxUH2to5km => uh_color(value),
            ModelParameter::MaxUH0to2km => uh_low_color(value),
            ModelParameter::BulkShear6km | ModelParameter::SurfaceWindGust => {
                wind_color(self.convert_for_display(value))
            }
            ModelParameter::PrecipitableWater => pwat_color(self.convert_for_display(value)),
            ModelParameter::Temperature2m => temperature_color(self.convert_for_display(value)),
            ModelParameter::Dewpoint2m => dewpoint_color(self.convert_for_display(value)),
            ModelParameter::Visibility => visibility_color(self.convert_for_display(value)),
            ModelParameter::CompositeReflectivity
            | ModelParameter::Reflectivity1km
            | ModelParameter::Reflectivity4km
            | ModelParameter::ReflectivityM10C
            | ModelParameter::MaxReflectivity => reflectivity_color(value),
            ModelParameter::EchoTop => echo_top_color(self.convert_for_display(value)),
            ModelParameter::VerticallyIntegratedLiquid => vil_color(value),
        }
    }

    /// Whether `value` (raw GRIB2 units) lands anywhere visible on this
    /// parameter's ramp, answered **without evaluating it**.
    ///
    /// `color_for_value(v)[3] != 0` holds for exactly the same values, pinned by
    /// `a_visibility_test_agrees_with_every_ramp`. It exists because
    /// [`summarize_values`] runs once per grid point on the fetch path — 1.9 M
    /// for HRRR — and a colour dispatch per point is the wrong shape for a
    /// question with a one-comparison answer.
    ///
    /// The three postures below are the ramps' own, not a simplification:
    /// CIN/LI/VIS are transparent *above* their last stop, temperature is
    /// transparent nowhere (both its ends are opaque clamps), and the rest are
    /// transparent *below* their first stop.
    pub fn paints(&self, value: f32) -> bool {
        if !value.is_finite() {
            return false;
        }
        match self {
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => value <= -25.0,
            ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => value >= 250.0,
            ModelParameter::LiftedIndex => value <= 0.0,
            ModelParameter::Srh1km | ModelParameter::Srh3km => value >= 50.0,
            ModelParameter::MaxUH2to5km => value >= 25.0,
            ModelParameter::MaxUH0to2km => value >= 10.0,
            ModelParameter::BulkShear6km | ModelParameter::SurfaceWindGust => {
                self.convert_for_display(value) >= 10.0
            }
            ModelParameter::PrecipitableWater => self.convert_for_display(value) >= 0.75,
            ModelParameter::Temperature2m => true,
            ModelParameter::Dewpoint2m => self.convert_for_display(value) >= 30.0,
            ModelParameter::Visibility => self.convert_for_display(value) <= 10.0,
            ModelParameter::CompositeReflectivity
            | ModelParameter::Reflectivity1km
            | ModelParameter::Reflectivity4km
            | ModelParameter::ReflectivityM10C
            | ModelParameter::MaxReflectivity => value >= REFLECTIVITY_BANDS[0].0,
            ModelParameter::EchoTop => self.convert_for_display(value) >= ECHO_TOP_STOPS[0].0,
            ModelParameter::VerticallyIntegratedLiquid => value >= VIL_STOPS[0].0,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin => "sbcin",
            ModelParameter::MixedLayerCin => "mlcin",
            ModelParameter::SurfaceBasedCape => "sbcape",
            ModelParameter::MixedLayerCape => "mlcape",
            ModelParameter::MostUnstableCape => "mucape",
            ModelParameter::LiftedIndex => "li",
            ModelParameter::Srh1km => "srh1",
            ModelParameter::Srh3km => "srh3",
            ModelParameter::MaxUH2to5km => "uh25",
            ModelParameter::MaxUH0to2km => "uh02",
            ModelParameter::BulkShear6km => "shr6",
            ModelParameter::SurfaceWindGust => "gust",
            ModelParameter::PrecipitableWater => "pwat",
            ModelParameter::Temperature2m => "t2m",
            ModelParameter::Dewpoint2m => "td2m",
            ModelParameter::Visibility => "vis",
            ModelParameter::CompositeReflectivity => "refc",
            ModelParameter::Reflectivity1km => "refd1",
            ModelParameter::Reflectivity4km => "refd4",
            ModelParameter::ReflectivityM10C => "refdm10",
            ModelParameter::MaxReflectivity => "maxref",
            ModelParameter::EchoTop => "retop",
            ModelParameter::VerticallyIntegratedLiquid => "vil",
        }
    }

    pub fn legend_thresholds(&self) -> Vec<(f32, [u8; 3])> {
        match self {
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => {
                vec![
                    (-500.0, [128, 0, 128]),
                    (-200.0, [220, 50, 50]),
                    (-100.0, [255, 165, 0]),
                    (-50.0, [255, 255, 100]),
                    (-25.0, [144, 238, 144]),
                ]
            }
            ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => {
                vec![
                    (250.0, [200, 230, 255]),
                    (500.0, [100, 200, 100]),
                    (1000.0, [255, 255, 0]),
                    (2000.0, [255, 165, 0]),
                    (3000.0, [220, 50, 50]),
                    (5000.0, [180, 0, 200]),
                ]
            }
            ModelParameter::LiftedIndex => {
                vec![
                    (-10.0, [180, 0, 200]),
                    (-6.0, [220, 50, 50]),
                    (-4.0, [255, 165, 0]),
                    (-2.0, [255, 255, 0]),
                    (0.0, [144, 238, 144]),
                ]
            }
            ModelParameter::Srh1km | ModelParameter::Srh3km => {
                vec![
                    (50.0, [144, 238, 144]),
                    (100.0, [255, 255, 0]),
                    (200.0, [255, 165, 0]),
                    (300.0, [220, 50, 50]),
                    (500.0, [180, 0, 200]),
                ]
            }
            ModelParameter::MaxUH2to5km => {
                vec![
                    (25.0, [144, 238, 144]),
                    (75.0, [255, 255, 0]),
                    (150.0, [255, 165, 0]),
                    (300.0, [220, 50, 50]),
                    (500.0, [180, 0, 200]),
                ]
            }
            ModelParameter::MaxUH0to2km => {
                vec![
                    (10.0, [144, 238, 144]),
                    (30.0, [255, 255, 0]),
                    (75.0, [255, 165, 0]),
                    (150.0, [220, 50, 50]),
                    (300.0, [180, 0, 200]),
                ]
            }
            // Wind thresholds in kt (display units).
            ModelParameter::BulkShear6km | ModelParameter::SurfaceWindGust => {
                vec![
                    (10.0, [144, 238, 144]),
                    (20.0, [255, 255, 0]),
                    (30.0, [255, 165, 0]),
                    (45.0, [220, 50, 50]),
                    (65.0, [180, 0, 200]),
                ]
            }
            // PWAT thresholds in inches.
            ModelParameter::PrecipitableWater => {
                vec![
                    (0.75, [200, 230, 255]),
                    (1.0, [100, 200, 100]),
                    (1.5, [255, 255, 0]),
                    (2.0, [255, 165, 0]),
                    (2.5, [220, 50, 50]),
                ]
            }
            // Temperature thresholds in °F.
            ModelParameter::Temperature2m => {
                vec![
                    (0.0, [180, 0, 200]),
                    (32.0, [100, 150, 255]),
                    (50.0, [100, 200, 100]),
                    (70.0, [255, 255, 0]),
                    (90.0, [255, 165, 0]),
                    (110.0, [220, 50, 50]),
                ]
            }
            // Dewpoint thresholds in °F.
            ModelParameter::Dewpoint2m => {
                vec![
                    (30.0, [180, 150, 100]),
                    (45.0, [144, 238, 144]),
                    (55.0, [100, 200, 100]),
                    (65.0, [255, 255, 0]),
                    (70.0, [255, 165, 0]),
                    (75.0, [220, 50, 50]),
                ]
            }
            // Visibility thresholds in miles.
            ModelParameter::Visibility => {
                vec![
                    (0.5, [180, 0, 200]),
                    (1.0, [220, 50, 50]),
                    (3.0, [255, 165, 0]),
                    (5.0, [255, 255, 0]),
                    (10.0, [144, 238, 144]),
                ]
            }
            // The three storm-scale bars hand back the same tables their ramps
            // walk rather than restating them: the legend and the raster are
            // the same picture, so a stop moved in one moves in both.
            ModelParameter::CompositeReflectivity
            | ModelParameter::Reflectivity1km
            | ModelParameter::Reflectivity4km
            | ModelParameter::ReflectivityM10C
            | ModelParameter::MaxReflectivity => REFLECTIVITY_BANDS.to_vec(),
            // Echo-top stops in kft (display units).
            ModelParameter::EchoTop => ECHO_TOP_STOPS.to_vec(),
            ModelParameter::VerticallyIntegratedLiquid => VIL_STOPS.to_vec(),
        }
    }
}

impl std::str::FromStr for ModelParameter {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            // Explicit even though the fallback yields the same value: relying
            // on the coincidence hid every unrecognised key behind a silent SBCIN.
            "sbcin" => ModelParameter::SurfaceBasedCin,
            "mlcin" => ModelParameter::MixedLayerCin,
            "sbcape" => ModelParameter::SurfaceBasedCape,
            "mlcape" => ModelParameter::MixedLayerCape,
            "mucape" => ModelParameter::MostUnstableCape,
            "li" => ModelParameter::LiftedIndex,
            "srh1" => ModelParameter::Srh1km,
            "srh3" => ModelParameter::Srh3km,
            "uh25" => ModelParameter::MaxUH2to5km,
            "uh02" => ModelParameter::MaxUH0to2km,
            "shr6" => ModelParameter::BulkShear6km,
            "gust" => ModelParameter::SurfaceWindGust,
            "pwat" => ModelParameter::PrecipitableWater,
            "t2m" => ModelParameter::Temperature2m,
            "td2m" => ModelParameter::Dewpoint2m,
            "vis" => ModelParameter::Visibility,
            "refc" => ModelParameter::CompositeReflectivity,
            "refd1" => ModelParameter::Reflectivity1km,
            "refd4" => ModelParameter::Reflectivity4km,
            "refdm10" => ModelParameter::ReflectivityM10C,
            "maxref" => ModelParameter::MaxReflectivity,
            "retop" => ModelParameter::EchoTop,
            "vil" => ModelParameter::VerticallyIntegratedLiquid,
            // Deliberately infallible — this parses persisted UI config — but audible.
            other => {
                log::warn!(
                    "Unknown HRRR model parameter {other:?} in saved config; \
                     falling back to {}",
                    ModelParameter::SurfaceBasedCin.display_name(),
                );
                ModelParameter::SurfaceBasedCin
            }
        })
    }
}

/// The dBZ ladder in **5 dBZ bands**, from 5 dBZ up:
/// [`squallar_source::product::REFLECTIVITY_OVERLAY_STOPS`], which MRMS's mosaic
/// draws too.
///
/// **It ends at 75 dBZ where a radar tilt's ladder runs to 95, on purpose.**
/// The three layers share
/// [`squallar_source::product::REFLECTIVITY_SHARED_STOPS`] through 70; above
/// that radar draws a hail band and this bar does not, because a forecast
/// composite is a model diagnostic and does not produce values up there. See
/// [`squallar_source::product::REFLECTIVITY_DIVERGENCE_DBZ`] for the one dBZ
/// that carries two colours.
///
/// **This doc used to say the table was "deliberately a second copy" of
/// `crate::mrms::fields::REFLECTIVITY`'s stops, held equal only by a test.**
/// The copies that mattered were three, not two: `squallar-radar` kept a third,
/// no test compared it to either of these, and it had drifted about one 5 dBZ
/// band through the green-to-red region. The stops are now one `const` in the
/// substrate and every layer slices it. What survives of the old reasoning is
/// the *shape* this file needs them in: the HRRR parameters resolve their own
/// ramp instead of the generic one over a `LegendScale`
/// (`render::gridded::register_model_fields` records why), so this table has to
/// exist as an array `color_for_value` can walk — which is a slice of the
/// substrate's array, not a transcription of it.
///
/// **The first stop is the transparency floor.** Clear air in a forecast grid
/// is a small positive or negative dBZ rather than a missing value, so a bar
/// starting at 0 would paint the whole CONUS domain. That is why the slice
/// starts at the overlay floor and not at the table's own 0 dBZ stop, which
/// exists for radar's 3D transfer table.
/// The slicing and capping this file used to do itself now happens once in the
/// substrate, so this is a plain alias rather than a `const fn`: what the
/// parameters' ramps need is a fixed-size array `color_for_value` can walk, and
/// `REFLECTIVITY_OVERLAY_STOPS` already is one. A stop added or removed on
/// either side of the divergence changes its length and stops this building.
const REFLECTIVITY_BANDS: [(f32, [u8; 3]); 15] =
    squallar_source::product::REFLECTIVITY_OVERLAY_STOPS;

/// Echo-top stops in **kft**, the unit [`ModelParameter::convert_for_display`]
/// hands the ramp — `RETOP` itself is metres.
///
/// A continuous ramp and not bands: a storm top is a height, read off the bar
/// by where it sits between two labels, and 34 kft is a different answer from
/// 30 kft rather than a different category. 5 kft is the floor because a
/// "cloud top" below it is fair-weather cumulus over the whole domain.
const ECHO_TOP_STOPS: [(f32, [u8; 3]); 7] = [
    (5.0, [0x20, 0x50, 0x90]),
    (10.0, [0x20, 0xa0, 0xd0]),
    (20.0, [0x00, 0xc0, 0x60]),
    (30.0, [0xe0, 0xe0, 0x00]),
    (40.0, [0xff, 0x90, 0x00]),
    (50.0, [0xe0, 0x20, 0x20]),
    (60.0, [0xff, 0x00, 0xff]),
];

/// VIL stops in **kg/m²**, the unit the grid already carries.
///
/// Continuous for the same reason the echo top is: liquid water content is a
/// magnitude, and the reading that matters — the hail signature above roughly
/// 30 — is found by watching a wash brighten, not by counting bands.
const VIL_STOPS: [(f32, [u8; 3]); 8] = [
    (0.5, [0x00, 0x60, 0xc0]),
    (2.0, [0x00, 0xa0, 0xf0]),
    (5.0, [0x00, 0xc0, 0x60]),
    (10.0, [0xc0, 0xe0, 0x00]),
    (20.0, [0xff, 0xd0, 0x00]),
    (30.0, [0xff, 0x80, 0x00]),
    (50.0, [0xe0, 0x20, 0x20]),
    (70.0, [0xff, 0x00, 0xff]),
];

/// The band `value` falls in, flat: the colour of the highest stop at or below
/// it, transparent below the first, and the last stop's colour above it.
///
/// `stops` ascends. A NaN answers `None` from the search — every `>=` is false
/// — so this needs no separate guard, though [`ModelParameter::color_for_value`]
/// keeps one for the ramps that do.
fn banded_color(stops: &[(f32, [u8; 3])], value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    match stops.iter().rev().find(|&&(floor, _)| value >= floor) {
        Some(&(_, [r, g, b])) => [r, g, b, ALPHA],
        None => [0, 0, 0, 0],
    }
}

/// `value` interpolated between the two stops bracketing it, transparent below
/// the first and clamped to the last stop's colour above it.
///
/// `stops` ascends and has at least two entries. A NaN fails every `>=` and
/// falls out transparent.
fn gradient_color(stops: &[(f32, [u8; 3])], value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    let Some(k) = stops.iter().rposition(|&(floor, _)| value >= floor) else {
        return [0, 0, 0, 0];
    };
    let (lo_value, lo) = stops[k];
    let Some(&(hi_value, hi)) = stops.get(k + 1) else {
        return [lo[0], lo[1], lo[2], ALPHA];
    };
    let t = ((value - lo_value) / (hi_value - lo_value)).clamp(0.0, 1.0);
    lerp_color(
        [lo[0], lo[1], lo[2], ALPHA],
        [hi[0], hi[1], hi[2], ALPHA],
        t,
    )
}

/// Reflectivity in dBZ — the grid's own unit, so nothing converts between the
/// value and the bar. Banded: see [`REFLECTIVITY_BANDS`].
fn reflectivity_color(dbz: f32) -> [u8; 4] {
    banded_color(&REFLECTIVITY_BANDS, dbz)
}

/// Echo top in kft (display units).
fn echo_top_color(kft: f32) -> [u8; 4] {
    gradient_color(&ECHO_TOP_STOPS, kft)
}

/// VIL in kg/m² (the grid's own unit).
fn vil_color(kg_per_m2: f32) -> [u8; 4] {
    gradient_color(&VIL_STOPS, kg_per_m2)
}

/// Color scale for CIN (Convective Inhibition).
///
/// CIN values are ≤ 0 J/kg; more negative = stronger cap. Transparent to −25,
/// light green to −50, yellow to −100, orange to −200, red/dark purple beyond.
fn cin_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;

    let mag = -value;

    if mag < 25.0 {
        [0, 0, 0, 0]
    } else if mag < 50.0 {
        let t = (mag - 25.0) / 25.0;
        lerp_color([144, 238, 144, ALPHA], [255, 255, 100, ALPHA], t)
    } else if mag < 100.0 {
        let t = (mag - 50.0) / 50.0;
        lerp_color([255, 255, 100, ALPHA], [255, 165, 0, ALPHA], t)
    } else if mag < 200.0 {
        let t = (mag - 100.0) / 100.0;
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], t)
    } else {
        let t = ((mag - 200.0) / 300.0).min(1.0);
        lerp_color([220, 50, 50, ALPHA], [128, 0, 128, ALPHA], t)
    }
}

/// CAPE color scale (J/kg, ≥ 0). Higher = more instability.
fn cape_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value < 250.0 {
        [0, 0, 0, 0]
    } else if value < 500.0 {
        lerp_color(
            [200, 230, 255, ALPHA],
            [100, 200, 100, ALPHA],
            (value - 250.0) / 250.0,
        )
    } else if value < 1000.0 {
        lerp_color(
            [100, 200, 100, ALPHA],
            [255, 255, 0, ALPHA],
            (value - 500.0) / 500.0,
        )
    } else if value < 2000.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (value - 1000.0) / 1000.0,
        )
    } else if value < 3000.0 {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            (value - 2000.0) / 1000.0,
        )
    } else {
        lerp_color(
            [220, 50, 50, ALPHA],
            [180, 0, 200, ALPHA],
            ((value - 3000.0) / 2000.0).min(1.0),
        )
    }
}

/// Lifted Index color scale (°C, more negative = more unstable).
fn li_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value > 0.0 {
        [0, 0, 0, 0]
    } else if value > -2.0 {
        lerp_color([144, 238, 144, ALPHA], [255, 255, 0, ALPHA], -value / 2.0)
    } else if value > -4.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (-value - 2.0) / 2.0,
        )
    } else if value > -6.0 {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            (-value - 4.0) / 2.0,
        )
    } else {
        lerp_color(
            [220, 50, 50, ALPHA],
            [180, 0, 200, ALPHA],
            ((-value - 6.0) / 4.0).min(1.0),
        )
    }
}

/// SRH color scale (m²/s², ≥ 0). Higher = more rotation potential.
fn srh_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value < 50.0 {
        [0, 0, 0, 0]
    } else if value < 100.0 {
        lerp_color(
            [144, 238, 144, ALPHA],
            [255, 255, 0, ALPHA],
            (value - 50.0) / 50.0,
        )
    } else if value < 200.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (value - 100.0) / 100.0,
        )
    } else if value < 300.0 {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            (value - 200.0) / 100.0,
        )
    } else {
        lerp_color(
            [220, 50, 50, ALPHA],
            [180, 0, 200, ALPHA],
            ((value - 300.0) / 200.0).min(1.0),
        )
    }
}

/// Updraft helicity 2–5 km color scale (m²/s²).
fn uh_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value < 25.0 {
        [0, 0, 0, 0]
    } else if value < 75.0 {
        lerp_color(
            [144, 238, 144, ALPHA],
            [255, 255, 0, ALPHA],
            (value - 25.0) / 50.0,
        )
    } else if value < 150.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (value - 75.0) / 75.0,
        )
    } else if value < 300.0 {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            (value - 150.0) / 150.0,
        )
    } else {
        lerp_color(
            [220, 50, 50, ALPHA],
            [180, 0, 200, ALPHA],
            ((value - 300.0) / 200.0).min(1.0),
        )
    }
}

/// Updraft helicity 0–2 km color scale (m²/s², lower thresholds).
fn uh_low_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value < 10.0 {
        [0, 0, 0, 0]
    } else if value < 30.0 {
        lerp_color(
            [144, 238, 144, ALPHA],
            [255, 255, 0, ALPHA],
            (value - 10.0) / 20.0,
        )
    } else if value < 75.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (value - 30.0) / 45.0,
        )
    } else if value < 150.0 {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            (value - 75.0) / 75.0,
        )
    } else {
        lerp_color(
            [220, 50, 50, ALPHA],
            [180, 0, 200, ALPHA],
            ((value - 150.0) / 150.0).min(1.0),
        )
    }
}

/// Wind speed color scale (kt, display units). Used by gust + shear.
fn wind_color(kt: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if kt < 10.0 {
        [0, 0, 0, 0]
    } else if kt < 20.0 {
        lerp_color(
            [144, 238, 144, ALPHA],
            [255, 255, 0, ALPHA],
            (kt - 10.0) / 10.0,
        )
    } else if kt < 30.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (kt - 20.0) / 10.0,
        )
    } else if kt < 45.0 {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            (kt - 30.0) / 15.0,
        )
    } else {
        lerp_color(
            [220, 50, 50, ALPHA],
            [180, 0, 200, ALPHA],
            ((kt - 45.0) / 20.0).min(1.0),
        )
    }
}

/// Precipitable water color scale (inches, display units).
fn pwat_color(inches: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if inches < 0.75 {
        [0, 0, 0, 0]
    } else if inches < 1.0 {
        lerp_color(
            [200, 230, 255, ALPHA],
            [100, 200, 100, ALPHA],
            (inches - 0.75) / 0.25,
        )
    } else if inches < 1.5 {
        lerp_color(
            [100, 200, 100, ALPHA],
            [255, 255, 0, ALPHA],
            (inches - 1.0) / 0.5,
        )
    } else if inches < 2.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (inches - 1.5) / 0.5,
        )
    } else {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            ((inches - 2.0) / 0.5).min(1.0),
        )
    }
}

/// 2m temperature color scale (°F, display units).
fn temperature_color(f: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if f < 0.0 {
        [180, 0, 200, ALPHA]
    } else if f < 32.0 {
        lerp_color([180, 0, 200, ALPHA], [100, 150, 255, ALPHA], f / 32.0)
    } else if f < 50.0 {
        lerp_color(
            [100, 150, 255, ALPHA],
            [100, 200, 100, ALPHA],
            (f - 32.0) / 18.0,
        )
    } else if f < 70.0 {
        lerp_color(
            [100, 200, 100, ALPHA],
            [255, 255, 0, ALPHA],
            (f - 50.0) / 20.0,
        )
    } else if f < 90.0 {
        lerp_color(
            [255, 255, 0, ALPHA],
            [255, 165, 0, ALPHA],
            (f - 70.0) / 20.0,
        )
    } else if f < 110.0 {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            (f - 90.0) / 20.0,
        )
    } else {
        [220, 50, 50, ALPHA]
    }
}

/// 2m dewpoint color scale (°F, display units). Higher = more moisture.
fn dewpoint_color(f: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if f < 30.0 {
        [0, 0, 0, 0]
    } else if f < 45.0 {
        lerp_color(
            [180, 150, 100, ALPHA],
            [144, 238, 144, ALPHA],
            (f - 30.0) / 15.0,
        )
    } else if f < 55.0 {
        lerp_color(
            [144, 238, 144, ALPHA],
            [100, 200, 100, ALPHA],
            (f - 45.0) / 10.0,
        )
    } else if f < 65.0 {
        lerp_color(
            [100, 200, 100, ALPHA],
            [255, 255, 0, ALPHA],
            (f - 55.0) / 10.0,
        )
    } else if f < 70.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (f - 65.0) / 5.0)
    } else {
        lerp_color(
            [255, 165, 0, ALPHA],
            [220, 50, 50, ALPHA],
            ((f - 70.0) / 5.0).min(1.0),
        )
    }
}

/// Visibility color scale (miles, display units). Lower = worse.
fn visibility_color(mi: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if mi > 10.0 {
        [0, 0, 0, 0]
    } else if mi > 5.0 {
        lerp_color(
            [144, 238, 144, ALPHA],
            [255, 255, 0, ALPHA],
            (10.0 - mi) / 5.0,
        )
    } else if mi > 3.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (5.0 - mi) / 2.0)
    } else if mi > 1.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (3.0 - mi) / 2.0)
    } else {
        lerp_color(
            [220, 50, 50, ALPHA],
            [180, 0, 200, ALPHA],
            ((1.0 - mi) / 0.5).min(1.0),
        )
    }
}

fn lerp_color(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
        (a[3] as f32 + (b[3] as f32 - a[3] as f32) * t) as u8,
    ]
}

/// Where a grid point's coordinates come from.
///
/// HRRR is 1,905,141 points, so a materialised `lats`/`lons` pair is 30.5 MB
/// *per cached parameter*, on targets with a 4 GiB address space (wasm32) or a
/// hard per-app cap (Android). The Lambert case rebuilds any point from the
/// projection constants instead; see [`lambert::LambertGrid`].
#[derive(Debug, Clone, PartialEq)]
pub enum GridCoords {
    Lambert(lambert::LambertGrid),
    /// A plate-carrée grid stated by its first point and its two steps.
    ///
    /// Every method below is a closed form, so the whole grid is these seven
    /// scalars — ~64 bytes whether it is HRRR's 1.9 M points or MRMS's 24.5 M,
    /// where [`Self::Explicit`] would be 30.5 MB and 392 MB respectively. It is
    /// also the arm that lets [`Self::index_bounds`] and
    /// [`Self::cell_span_degrees`] answer at all: `Explicit` returns `None` from
    /// both, which sends `crate::render::rasterize`'s projection window back to
    /// the full grid.
    Regular {
        /// The first point's latitude — the scanning origin, not the south edge.
        lat0: f64,
        /// The first point's longitude — the scanning origin, not the west edge.
        lon0: f64,
        /// **Signed** steps: the `-i` / `-j` scanning-mode bits live in the sign
        /// here rather than in a flag this arm would have to re-read.
        dlat: f64,
        dlon: f64,
        ni: usize,
        nj: usize,
        /// GRIB2 section 3 scanning mode, as the octet. Only `0b0010_0000`
        /// (j-consecutive) and `0b0001_0000` (alternating rows) are read; the
        /// two direction bits are already in the signs of the steps.
        scan_mode: u8,
    },
    Explicit {
        lats: Vec<f64>,
        lons: Vec<f64>,
    },
    /// A grid whose two coordinates are **separable**: every point's latitude
    /// depends only on its row and every point's longitude only on its column,
    /// so the whole geometry is one axis per dimension rather than one pair per
    /// point.
    ///
    /// **Point ordering is row-major with longitude fastest**: `index` is
    /// `row * lon_axis.len() + column`, so `at(0)` is the first row's first
    /// column, `at(1)` steps one column, and `at(lon_axis.len())` steps one
    /// row. [`Self::Regular`] carries a `scan_mode` octet because a GRIB
    /// section 3 does not fix its ordering; this arm fixes it here instead,
    /// because its source — a NetCDF4 granule whose data variable is declared
    /// `(time, yc, xc)` — states the ordering in the file's own dimension list
    /// and offers no octet to carry it.
    ///
    /// **The axes are read, never rebuilt.** GMGSI's latitude axis is uniform
    /// in *Mercator y*, not in latitude: its rows step `-14.52°` near the top
    /// of the grid and `-33.86°` in the middle, a 2.3x spread, while
    /// `squallar_geo::lat_rad_to_mercator_y` of the same rows steps a constant
    /// `-0.628397` to within 1e-6. A uniform latitude axis through the declared
    /// corners is 9.73° wrong by row 500. Neither is the longitude step the
    /// declared `geospatial_lon_resolution`: that says `0.0722` where the array
    /// steps `0.0720089`.
    ///
    /// The two field names differ from [`Self::Explicit`]'s `lats`/`lons` on
    /// purpose. Those are per-*point* parallel arrays, one entry per grid
    /// point; these are per-*axis*, and the tests that compare the two arms
    /// would read either name as plausible.
    ///
    /// Costs one `f64` per row plus one per column — 64 KB for GMGSI's
    /// 3000 x 5000 grid, where [`Self::Explicit`] would be 240 MB.
    Separable {
        lat_axis: Vec<f64>,
        lon_axis: Vec<f64>,
    },
}

/// Whether `axis` is strictly monotonic, and ascending if so.
///
/// `None` for an axis that reverses anywhere, which is not a malformed grid:
/// GMGSI's longitude axis holds `+179.99961` at column 0 and `-179.92838` at
/// column 1, because the grid starts a hair west of the antimeridian and the
/// file states every longitude already wrapped into `[-180, 180]`. Only the
/// bracketing search below needs the ordering, and it answers "the whole axis"
/// rather than a wrong window when it cannot have it.
fn axis_is_ascending(axis: &[f64]) -> Option<bool> {
    let (first, last) = (*axis.first()?, *axis.last()?);
    if axis.len() < 2 || !first.is_finite() || !last.is_finite() {
        return None;
    }
    let ascending = last > first;
    let ordered = axis
        .windows(2)
        .all(|w| w[1].is_finite() && if ascending { w[1] > w[0] } else { w[1] < w[0] });
    ordered.then_some(ascending)
}

/// The fractional index interval of `axis` covering `[lo, hi]`, clamped to the
/// axis, or the whole axis when the axis is not strictly monotonic.
fn axis_bracket(axis: &[f64], lo: f64, hi: f64) -> (f64, f64) {
    let whole = (0.0, axis.len().saturating_sub(1) as f64);
    let Some(ascending) = axis_is_ascending(axis) else {
        return whole;
    };
    // Fractional position of `v` on the axis, by binary search plus a linear
    // interpolation inside the bracketing cell. Returned unclamped so a value
    // off the end yields an empty rather than a full window.
    let position = |v: f64| -> f64 {
        let cmp = |probe: &f64| {
            if ascending {
                probe.partial_cmp(&v).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                v.partial_cmp(probe).unwrap_or(std::cmp::Ordering::Equal)
            }
        };
        match axis.binary_search_by(cmp) {
            Ok(i) => i as f64,
            Err(0) => 0.0,
            Err(i) if i >= axis.len() => (axis.len() - 1) as f64,
            Err(i) => {
                let (a, b) = (axis[i - 1], axis[i]);
                let t = if b == a { 0.0 } else { (v - a) / (b - a) };
                (i - 1) as f64 + t
            }
        }
    };
    let (a, b) = (position(lo), position(hi));
    (a.min(b), a.max(b))
}

/// The fractional column interval of a *longitude* axis covering `[lo, hi]`.
///
/// An axis that does not close the globe is [`axis_bracket`]'s own case: its
/// values and the box's are on one scale and the search is arithmetic.
///
/// An axis that closes the globe is not. Its values and the box's are angles,
/// and the two need not be folded the same way — GMGSI's axis holds
/// `+179.99961` at column 0 beside `-179.92838` at column 1, while `walkers`
/// hands out box longitudes running past ±180 at low zoom. So each endpoint is
/// located **angularly**, by the column nearest it the short way round.
///
/// The columns of such an axis sweep east once around, so the columns covering
/// `[lo, hi]` are one contiguous run unless the box walks over the axis's own
/// end — and that is exactly the two endpoints landing out of index order. A
/// box that does walk over it covers two disjoint runs, and the honest single
/// interval is then the whole axis: not narrower than the truth, which is what
/// the window contract asks for.
fn lon_axis_bracket(axis: &[f64], lo: f64, hi: f64) -> (f64, f64) {
    let whole = (0.0, axis.len().saturating_sub(1) as f64);
    // The wrap test of `GridCoords::wraps_longitude`'s `Separable` arm, asked
    // of the axis itself: below it the arithmetic search is the right one.
    if separable_lon_span(axis) + 2.0 * separable_lon_step(axis) < 360.0 {
        return axis_bracket(axis, lo, hi);
    }
    // At most one backward step, so the axis really does sweep east once
    // around; and a box spanning a whole turn covers every column whatever the
    // endpoints say. Any other shape is one this reasoning does not describe.
    if axis.len() < 2
        || !(lo.is_finite() && hi.is_finite())
        || hi - lo >= 360.0
        || axis.iter().any(|v| !v.is_finite())
        || axis.windows(2).filter(|w| w[1] <= w[0]).count() > 1
    {
        return whole;
    }
    let (Some(a), Some(b)) = (
        nearest_on_axis(axis, lo, longitude_distance),
        nearest_on_axis(axis, hi, longitude_distance),
    ) else {
        return whole;
    };
    if a > b {
        return whole;
    }
    // `nearest_on_axis` rounds to the closer column, so the box's own edge sits
    // up to half a cell outside it at either end.
    (a as f64 - 0.5, b as f64 + 0.5)
}

/// Index of the entry of `axis` nearest `v`, comparing with `distance`.
fn nearest_on_axis(axis: &[f64], v: f64, distance: impl Fn(f64, f64) -> f64) -> Option<usize> {
    let mut best = None;
    let mut best_d = f64::MAX;
    for (i, &a) in axis.iter().enumerate() {
        let d = distance(a, v);
        if d < best_d {
            best_d = d;
            best = Some(i);
        }
    }
    best
}

/// Shortest angular separation between two longitudes, in degrees.
fn longitude_distance(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// Degrees of longitude between the axis's extreme columns.
fn separable_lon_span(lon_axis: &[f64]) -> f64 {
    if lon_axis.len() < 2 {
        return 0.0;
    }
    lon_axis.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b))
        - lon_axis.iter().fold(f64::INFINITY, |a, &b| a.min(b))
}

/// The widest column of the axis, in degrees.
///
/// The **widest** and not the mean, because the only consumer
/// ([`GridCoords::cell_span_degrees`]) is a pad that must never under-cover a
/// cell. On GMGSI the mean taken from the span is 0.0720000 where the widest
/// real column is 0.0720215, and a pad below the true width crops the raster.
///
/// Differences of half a turn or more are skipped: an axis that wraps holds one
/// enormous difference at the seam (GMGSI's is -359.928°), which is precisely
/// the figure an unguarded maximum would return.
fn separable_lon_step(lon_axis: &[f64]) -> f64 {
    if lon_axis.len() < 2 {
        return 0.0;
    }
    let widest = lon_axis
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .filter(|d| d.is_finite() && *d < 180.0)
        .fold(0.0f64, f64::max);
    if widest > 0.0 {
        widest
    } else {
        // Every step was a seam or non-finite: fall back to the mean, which at
        // least has the right order of magnitude.
        separable_lon_span(lon_axis) / (lon_axis.len() - 1) as f64
    }
}

/// Scanning-mode bit: adjacent points are consecutive in `j`, not `i`.
pub const SCAN_J_CONSECUTIVE: u8 = 0b0010_0000;
/// Scanning-mode bit: alternate rows scan in the opposite direction.
pub const SCAN_ALTERNATING: u8 = 0b0001_0000;

/// The `(i, j)` a regular grid's scan puts at `index`, or `None` past its end.
/// Mirrors [`lambert::LambertGrid`]'s own `ij_at`, which is driven by grib's
/// `GridPointIndex::ij` — the order the decoded values arrive in.
fn regular_ij_at(index: usize, ni: usize, nj: usize, scan_mode: u8) -> Option<(usize, usize)> {
    let j_consecutive = scan_mode & SCAN_J_CONSECUTIVE != 0;
    let (major_len, minor_len) = if j_consecutive { (ni, nj) } else { (nj, ni) };
    if minor_len == 0 {
        return None;
    }
    let major = index / minor_len;
    if major >= major_len {
        return None;
    }
    let mut minor = index % minor_len;
    if scan_mode & SCAN_ALTERNATING != 0 && major % 2 == 1 {
        minor = minor_len - minor - 1;
    }
    Some(if j_consecutive {
        (major, minor)
    } else {
        (minor, major)
    })
}

/// Inverse of [`regular_ij_at`], for an `(i, j)` already known to be in range.
fn regular_index_of(i: usize, j: usize, ni: usize, nj: usize, scan_mode: u8) -> usize {
    let j_consecutive = scan_mode & SCAN_J_CONSECUTIVE != 0;
    let (major, minor, minor_len) = if j_consecutive {
        (i, j, nj)
    } else {
        (j, i, ni)
    };
    let minor = if scan_mode & SCAN_ALTERNATING != 0 && major % 2 == 1 {
        minor_len - minor - 1
    } else {
        minor
    };
    major * minor_len + minor
}

impl GridCoords {
    pub fn len(&self) -> usize {
        match self {
            GridCoords::Lambert(g) => g.len(),
            GridCoords::Regular { ni, nj, .. } => ni * nj,
            GridCoords::Explicit { lats, lons } => lats.len().min(lons.len()),
            GridCoords::Separable { lat_axis, lon_axis } => lat_axis.len() * lon_axis.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn at(&self, index: usize) -> Option<(f64, f64)> {
        match self {
            GridCoords::Lambert(g) => g.latlon_at(index),
            GridCoords::Regular {
                lat0,
                lon0,
                dlat,
                dlon,
                ni,
                nj,
                scan_mode,
            } => {
                let (i, j) = regular_ij_at(index, *ni, *nj, *scan_mode)?;
                Some((lat0 + j as f64 * dlat, lon0 + i as f64 * dlon))
            }
            GridCoords::Explicit { lats, lons } => Some((*lats.get(index)?, *lons.get(index)?)),
            // Row-major, longitude fastest -- see the variant's own doc.
            GridCoords::Separable { lat_axis, lon_axis } => {
                let nx = lon_axis.len();
                if nx == 0 {
                    return None;
                }
                Some((*lat_axis.get(index / nx)?, *lon_axis.get(index % nx)?))
            }
        }
    }

    /// Fractional `(i_min, i_max, j_min, j_max)` bounding every grid point inside
    /// `bounds`, or `None` when there is no cheaper answer than all. Only the
    /// Lambert and regular cases answer, and `ni`/`nj` are the *caller's* grid
    /// shape, so a shape that does not match is refused rather than answered
    /// wrongly.
    pub fn index_bounds(
        &self,
        bounds: &GeoBounds,
        ni: usize,
        nj: usize,
    ) -> Option<(f64, f64, f64, f64)> {
        match self {
            GridCoords::Lambert(g) if g.is_row_major(ni, nj) => g.index_bounds(
                bounds.min_lat,
                bounds.max_lat,
                bounds.min_lon,
                bounds.max_lon,
            ),
            // The same three conditions the Lambert arm's `is_row_major` states,
            // for the same reason: the caller steps `index ± 1` and `index ± ni`
            // for the four neighbours, which is `j * ni + i` only when the scan
            // is i-consecutive, non-alternating, and of this very shape.
            GridCoords::Regular {
                lat0,
                lon0,
                dlat,
                dlon,
                ni: gni,
                nj: gnj,
                scan_mode,
            } => {
                if *scan_mode & (SCAN_J_CONSECUTIVE | SCAN_ALTERNATING) != 0
                    || *gni != ni
                    || *gnj != nj
                    || *dlat == 0.0
                    || *dlon == 0.0
                    || bounds.min_lat > bounds.max_lat
                    || bounds.min_lon > bounds.max_lon
                {
                    return None;
                }
                let (ia, ib) = (
                    (bounds.min_lon - lon0) / dlon,
                    (bounds.max_lon - lon0) / dlon,
                );
                let (ja, jb) = (
                    (bounds.min_lat - lat0) / dlat,
                    (bounds.max_lat - lat0) / dlat,
                );
                // A grid that closes the globe has no single linear column for
                // a longitude: `(lon - lon0) / dlon` is a whole turn out for
                // any box stated on the far side of `lon0`, and lands
                // *negative*, which crops to an empty window rather than a wide
                // one. Its rows are still parallels, so only the column pair
                // goes wide and the latitude bracket answers as ever.
                let (i_lo, i_hi) = if self.wraps_longitude() {
                    (0.0, gni.saturating_sub(1) as f64)
                } else {
                    (ia.min(ib), ia.max(ib))
                };
                // A negative step reverses the ordering, hence the min/max pairs.
                let out = (i_lo, i_hi, ja.min(jb), ja.max(jb));
                (out.0.is_finite() && out.1.is_finite() && out.2.is_finite() && out.3.is_finite())
                    .then_some(out)
            }
            // The same row-major precondition the two arms above state, for the
            // same reason: the caller steps `index ± 1` and `index ± ni`, which
            // is this arm's ordering only at this arm's own shape.
            GridCoords::Separable { lat_axis, lon_axis } => {
                if lon_axis.len() != ni
                    || lat_axis.len() != nj
                    || bounds.min_lat > bounds.max_lat
                    || bounds.min_lon > bounds.max_lon
                {
                    return None;
                }
                // Per axis, and independently. The latitude axis is a plain
                // monotonic search; the longitude axis may close the globe, and
                // then neither its values nor the box's are on one scale. A
                // window that is merely *not narrower* than the truth is
                // correct; one that is narrower silently crops the raster.
                let (j_min, j_max) = axis_bracket(lat_axis, bounds.min_lat, bounds.max_lat);
                let (i_min, i_max) = lon_axis_bracket(lon_axis, bounds.min_lon, bounds.max_lon);
                Some((i_min, i_max, j_min, j_max))
            }
            _ => None,
        }
    }

    /// Upper bound on how many degrees one grid cell spans near `lat` — see
    /// [`lambert::LambertGrid::cell_span_degrees`]. A regular grid's cell is
    /// already stated in degrees, so the bound is the larger step and `lat` does
    /// not enter it.
    pub fn cell_span_degrees(&self, lat: f64) -> Option<f64> {
        match self {
            GridCoords::Lambert(g) => Some(g.cell_span_degrees(lat)),
            GridCoords::Regular { dlat, dlon, .. } => Some(dlat.abs().max(dlon.abs())),
            GridCoords::Explicit { .. } => None,
            // The latitude step is *local*: GMGSI's rows span 0.029° at the
            // equator and 0.068° at the top of the grid, so a single global
            // figure would under-cover one end or over-cover the other.
            GridCoords::Separable { lat_axis, lon_axis } => {
                let dlat = nearest_on_axis(lat_axis, lat, |a, b| (a - b).abs())
                    .map(|j| {
                        let lo = j.saturating_sub(1);
                        let hi = (j + 1).min(lat_axis.len() - 1);
                        (lat_axis[lo] - lat_axis[j])
                            .abs()
                            .max((lat_axis[hi] - lat_axis[j]).abs())
                    })
                    .unwrap_or(0.0);
                Some(dlat.max(separable_lon_step(lon_axis)))
            }
        }
    }

    /// Whether adjacent grid points can jump most of a turn in longitude — see
    /// [`lambert::LambertGrid::wraps_longitude`]. Always `false` for `Explicit`.
    pub fn wraps_longitude(&self) -> bool {
        match self {
            GridCoords::Lambert(g) => g.wraps_longitude(),
            GridCoords::Regular { dlon, ni, .. } => (*ni as f64 * dlon).abs() >= 360.0,
            GridCoords::Explicit { .. } => false,
            // True when the axis covers the whole turn to within one cell.
            // GMGSI spans 359.928° in 5000 columns of 0.0720089°, so the seam
            // between its last column and its first is one ordinary cell wide
            // and the raster must be allowed to close across it.
            GridCoords::Separable { lon_axis, .. } => {
                separable_lon_span(lon_axis) + 2.0 * separable_lon_step(lon_axis) >= 360.0
            }
        }
    }

    /// Whether a row of this grid is a **parallel** — every point of row `j` at
    /// one latitude, fixed by `j` alone and by nothing the longitude axis does.
    ///
    /// This is what decides whether [`Self::wraps_longitude`] costs the row
    /// axis as well as the column axis. On the two separable arms it does not:
    /// a longitude discontinuity cannot reach a latitude that longitude is no
    /// input to, so their rows bracket whatever the columns do.
    ///
    /// On [`Self::Lambert`] it does, and the difference is not cosmetic.
    /// A Lambert row is not a parallel, both indices are axes of the projection
    /// *plane*, and `lambert::LambertGrid::detect_longitude_wrap` reports a
    /// discontinuity found stepping along **either** of them — so a wrapping
    /// Lambert grid has no usable window on either axis and must decline both.
    /// [`Self::Explicit`] has no axis structure at all to bracket.
    pub fn rows_are_parallels(&self) -> bool {
        match self {
            GridCoords::Regular { .. } | GridCoords::Separable { .. } => true,
            GridCoords::Lambert(_) | GridCoords::Explicit { .. } => false,
        }
    }

    /// Index of the grid point nearest `(lat, lon)`, or `None` when the grid does
    /// not cover it. O(1) for a Lambert or regular grid — the flat scan it
    /// replaces ran over all 1.9 M points on every hover frame.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<usize> {
        match self {
            GridCoords::Lambert(g) => g.nearest(lat, lon),
            GridCoords::Regular {
                lat0,
                lon0,
                dlat,
                dlon,
                ni,
                nj,
                scan_mode,
            } => {
                let fi = ((lon - lon0) / dlon).round();
                let fj = ((lat - lat0) / dlat).round();
                if !(fi.is_finite() && fj.is_finite()) || fi < 0.0 || fj < 0.0 {
                    return None;
                }
                let (i, j) = (fi as usize, fj as usize);
                if i >= *ni || j >= *nj {
                    return None;
                }
                Some(regular_index_of(i, j, *ni, *nj, *scan_mode))
            }
            GridCoords::Explicit { lats, lons } => {
                let mut best = None;
                let mut best_d2 = f64::MAX;
                for (i, (&glat, &glon)) in lats.iter().zip(lons.iter()).enumerate() {
                    let (dlat, dlon) = (glat - lat, glon - lon);
                    let d2 = dlat * dlat + dlon * dlon;
                    if d2 < best_d2 {
                        best_d2 = d2;
                        best = Some(i);
                    }
                }
                best
            }
            // One scan per axis rather than one over the product: 8,000 probes
            // where `Explicit` at GMGSI's size would be 15,000,000. Longitude
            // is compared the short way round so a query just east of the
            // antimeridian finds column 0 rather than the far side of the grid.
            GridCoords::Separable { lat_axis, lon_axis } => {
                let j = nearest_on_axis(lat_axis, lat, |a, b| (a - b).abs())?;
                let i = nearest_on_axis(lon_axis, lon, longitude_distance)?;
                Some(j * lon_axis.len() + i)
            }
        }
    }
}

/// `PartialEq` is derived for the described-overlay wire tests and carries the
/// usual `NaN != NaN` caveat.
#[derive(Debug, Clone, PartialEq)]
pub struct HrrrGridData {
    pub parameter: ModelParameter,
    pub values: Vec<f32>,
    pub coords: GridCoords,
    pub ni: usize,
    pub nj: usize,
    pub bounds: GeoBounds,
    pub ref_time: chrono::NaiveDateTime,
    /// Forecast hour this grid was fetched at — the hour the caller asked for,
    /// clamped up to [`ModelParameter::min_forecast_hour`]. 0 is the analysis;
    /// see that method for why UH can never read 0 here.
    pub forecast_hour: u8,
    /// How many grid points map to a non-transparent colour; computed once at
    /// parse time. See [`HrrrGridData::blank_notice`].
    pub visible_points: usize,
    pub value_range: Option<(f32, f32)>,
}

impl HrrrGridData {
    pub fn valid_time(&self) -> chrono::NaiveDateTime {
        self.ref_time + chrono::Duration::hours(self.forecast_hour as i64)
    }

    /// Explain why this grid will render as nothing, when it will. A field that
    /// decodes perfectly but paints zero pixels is indistinguishable on screen
    /// from a fetch that never happened; this also separates a genuinely uniform
    /// field from one that never crosses the lowest colour threshold.
    pub fn blank_notice(&self) -> Option<String> {
        if self.visible_points > 0 {
            return None;
        }
        let name = self.parameter.short_name();
        Some(match self.value_range {
            None => format!("! {name}: no usable values in the grid"),
            Some((lo, hi)) if lo == hi => format!(
                "! {name} is uniformly {} across all {} points - nothing to draw",
                self.parameter.format_magnitude(lo),
                self.values.len(),
            ),
            Some((lo, hi)) => format!(
                "! {name} never reaches the lowest colour threshold (range {} to {}) - nothing to draw",
                self.parameter.format_magnitude(lo),
                self.parameter.format_magnitude(hi),
            ),
        })
    }
}

/// One pass for the render-coverage summary on a gridded field. Non-finite
/// points are missing data, not readings — excluded from both figures.
///
/// `paints` answers "does this value paint anything", and takes a **predicate
/// rather than a colour**: this runs once per grid point on the fetch path
/// (1.9 M for HRRR, 24.5 M for a CONUS composite), where evaluating a ramp only
/// to look at its alpha is the wrong shape. [`ModelParameter::paints`] is the
/// model's, and `a_visibility_test_agrees_with_every_ramp` is what keeps the two
/// answers the same.
pub fn summarize_values(
    values: &[f32],
    paints: impl Fn(f32) -> bool,
) -> (usize, Option<(f32, f32)>) {
    summarize_values_iter(values.iter().copied(), paints)
}

/// [`summarize_values`] over a stream rather than a slice.
///
/// The same pass, for a grid whose values are not `f32` in memory: a
/// [`GridValues::Scaled`](crate::render::gridded::GridValues::Scaled) store
/// reads back one value at a time and never materialises a slice of them, and
/// materialising one here purely to summarise it would allocate the whole
/// widened mosaic the narrow store exists to avoid.
pub fn summarize_values_iter(
    values: impl Iterator<Item = f32>,
    paints: impl Fn(f32) -> bool,
) -> (usize, Option<(f32, f32)>) {
    let mut visible = 0usize;
    let mut range: Option<(f32, f32)> = None;
    for v in values {
        if !v.is_finite() {
            continue;
        }
        if paints(v) {
            visible += 1;
        }
        range = Some(match range {
            Some((lo, hi)) => (lo.min(v), hi.max(v)),
            None => (v, v),
        });
    }
    (visible, range)
}

pub struct HrrrFetchResult(pub Result<HrrrGridData, crate::fetch_policy::FetchError>);

/// **[`Whole`], not assembled.** The round asks NOMADS for two candidate runs an
/// hour apart and keeps whichever answers, but either alone is the same product
/// and a complete answer to the question the layer asked.
///
/// [`Whole`]: crate::fetch_policy::Whole
impl crate::fetch_policy::FetchRound for HrrrFetchResult {
    type Shape = crate::fetch_policy::Whole;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(param: ModelParameter, values: Vec<f32>) -> HrrrGridData {
        let n = values.len();
        let (visible_points, value_range) = summarize_values(&values, |v| param.paints(v));
        HrrrGridData {
            parameter: param,
            values,
            coords: GridCoords::Explicit {
                lats: vec![35.0; n],
                lons: vec![-97.0; n],
            },
            ni: n,
            nj: 1,
            bounds: GeoBounds {
                min_lat: 35.0,
                max_lat: 35.0,
                min_lon: -97.0,
                max_lon: -97.0,
            },
            ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
                .unwrap()
                .and_hms_opt(3, 0, 0)
                .unwrap(),
            forecast_hour: param.min_forecast_hour(),
            visible_points,
            value_range,
        }
    }

    /// The f00 failure mode: `MXUPHL` at f00 decodes to exactly 0.0 at every
    /// point, and must announce itself rather than rendering an empty map.
    #[test]
    fn a_uniformly_zero_field_says_so_instead_of_rendering_nothing() {
        let g = grid(ModelParameter::MaxUH2to5km, vec![0.0; 4]);
        assert_eq!(g.visible_points, 0);
        let notice = g.blank_notice().expect("blank grid must explain itself");
        assert!(notice.contains("UH2-5"), "{notice}");
        assert!(notice.contains("uniformly"), "{notice}");
        assert!(notice.contains("0 m\u{b2}/s\u{b2}"), "{notice}");
    }

    #[test]
    fn a_below_threshold_field_reports_its_range_not_uniformity() {
        let g = grid(ModelParameter::MaxUH2to5km, vec![1.0, 5.0, 9.0, 24.0]);
        assert_eq!(g.visible_points, 0);
        let notice = g.blank_notice().expect("blank grid must explain itself");
        assert!(notice.contains("never reaches"), "{notice}");
        assert!(notice.contains("1 m\u{b2}/s\u{b2}"), "{notice}");
        assert!(notice.contains("24 m\u{b2}/s\u{b2}"), "{notice}");
        assert!(!notice.contains("uniformly"), "{notice}");
    }

    #[test]
    fn a_field_with_visible_points_produces_no_notice() {
        let g = grid(ModelParameter::MaxUH2to5km, vec![0.0, 0.0, 0.0, 120.0]);
        assert_eq!(g.visible_points, 1);
        assert_eq!(g.blank_notice(), None);
    }

    #[test]
    fn an_all_nan_field_reports_no_usable_values() {
        let g = grid(ModelParameter::MaxUH2to5km, vec![f32::NAN; 4]);
        assert_eq!(g.visible_points, 0);
        assert_eq!(g.value_range, None);
        let notice = g.blank_notice().expect("blank grid must explain itself");
        assert!(notice.contains("no usable values"), "{notice}");
    }

    #[test]
    fn summarize_values_ignores_non_finite_points() {
        let values = vec![f32::NAN, 100.0, f32::INFINITY, 200.0];
        let (visible, range) = summarize_values(&values, |v| ModelParameter::MaxUH2to5km.paints(v));
        assert_eq!(visible, 2, "NaN/inf must never count as painted points");
        assert_eq!(range, Some((100.0, 200.0)));
    }

    /// UH is a 0-1 h maximum valid an hour after the run, so the UI must be
    /// able to show a valid time that differs from the reference time.
    #[test]
    fn valid_time_advances_by_the_forecast_hour() {
        let uh = grid(ModelParameter::MaxUH2to5km, vec![50.0]);
        assert_eq!(uh.ref_time.format("%H:%Mz").to_string(), "03:00z");
        assert_eq!(uh.valid_time().format("%H:%Mz").to_string(), "04:00z");

        let cin = grid(ModelParameter::SurfaceBasedCin, vec![-100.0]);
        assert_eq!(
            cin.valid_time(),
            cin.ref_time,
            "f00 is valid at its run time"
        );
    }

    #[test]
    fn format_magnitude_omits_the_name_that_format_value_prepends() {
        let p = ModelParameter::MaxUH2to5km;
        assert_eq!(p.format_magnitude(120.0), "120 m\u{b2}/s\u{b2}");
        assert_eq!(p.format_value(120.0), "UH2-5: 120 m\u{b2}/s\u{b2}");
        assert_eq!(
            ModelParameter::PrecipitableWater.format_magnitude(25.4),
            "1.00 in",
        );
    }

    #[test]
    fn every_parameter_round_trips_through_its_config_key() {
        for param in ModelParameter::all() {
            let parsed: ModelParameter = param.as_str().parse().unwrap();
            assert_eq!(
                parsed,
                *param,
                "key {:?} did not round-trip",
                param.as_str()
            );
        }
    }

    #[test]
    fn config_keys_are_unique() {
        let mut keys: Vec<&str> = ModelParameter::all().iter().map(|p| p.as_str()).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "duplicate config key among {keys:?}");
    }

    fn explicit() -> GridCoords {
        GridCoords::Explicit {
            lats: vec![35.0, 35.0, 35.1, 35.1],
            lons: vec![-97.1, -97.0, -97.1, -97.0],
        }
    }

    #[test]
    fn explicit_coords_read_back_by_index_and_stop_at_the_end() {
        let c = explicit();
        assert_eq!(c.len(), 4);
        assert!(!c.is_empty());
        assert_eq!(c.at(0), Some((35.0, -97.1)));
        assert_eq!(c.at(3), Some((35.1, -97.0)));
        assert_eq!(c.at(4), None, "one past the end must not wrap");
    }

    /// A ragged pair must report the shorter length, not an index one side lacks.
    #[test]
    fn explicit_coords_are_bounded_by_the_shorter_array() {
        let c = GridCoords::Explicit {
            lats: vec![35.0, 36.0, 37.0],
            lons: vec![-97.0],
        };
        assert_eq!(c.len(), 1);
        assert_eq!(c.at(1), None);
    }

    #[test]
    fn an_empty_grid_reports_itself_empty() {
        let c = GridCoords::Explicit {
            lats: Vec::new(),
            lons: Vec::new(),
        };
        assert!(c.is_empty());
        assert_eq!(c.at(0), None);
        assert_eq!(c.nearest(35.0, -97.0), None);
    }

    #[test]
    fn explicit_nearest_picks_the_closest_point_not_the_first() {
        let c = explicit();
        assert_eq!(c.nearest(35.099, -97.001), Some(3));
        assert_eq!(c.nearest(35.001, -97.099), Some(0));
        assert_eq!(c.nearest(35.001, -97.001), Some(1));
        assert_eq!(c.nearest(35.099, -97.099), Some(2));
    }

    #[test]
    fn format_value_is_empty_for_non_finite_readings() {
        for p in ModelParameter::all() {
            assert_eq!(p.format_value(f32::NAN), "", "{}", p.display_name());
            assert_eq!(p.format_value(f32::INFINITY), "", "{}", p.display_name());
        }
    }

    // ── GridCoords::Regular ────────────────────────────────────────────

    /// MRMS's own shape at a hundredth of the scale: a north-down scan, so
    /// `dlat` is negative and `dlon` positive.
    fn regular(ni: usize, nj: usize, scan_mode: u8) -> GridCoords {
        GridCoords::Regular {
            lat0: 54.995,
            lon0: -129.995,
            dlat: -0.01,
            dlon: 0.01,
            ni,
            nj,
            scan_mode,
        }
    }

    /// An `Explicit` grid built by walking the same parameters — the form the
    /// regular arm exists to avoid materialising.
    fn materialise(coords: &GridCoords) -> GridCoords {
        let (mut lats, mut lons) = (Vec::new(), Vec::new());
        for k in 0..coords.len() {
            let (lat, lon) = coords.at(k).expect("in range");
            lats.push(lat);
            lons.push(lon);
        }
        GridCoords::Explicit { lats, lons }
    }

    /// **The parity, with its non-vacuity floor.** Every point of a regular grid
    /// is the point its materialised form holds, at exact `f64` equality — the
    /// analytic arm is the same grid, not an approximation of it.
    #[test]
    fn a_regular_grid_indexes_the_same_points_as_its_materialised_form() {
        for scan_mode in [0b0100_0000u8, 0b0110_0000, 0b0101_0000, 0b0111_0000] {
            let coords = regular(151, 97, scan_mode);
            assert!(
                coords.len() > 10_000,
                "a grid of {} points is small enough that a zero-size grid \
                 would pass this test trivially",
                coords.len(),
            );
            // The materialised form is built *through* `at`, so this pins the
            // storage question rather than the formula. The formula's own
            // floors are the corner readings below and the `nearest` inverse.
            let explicit = materialise(&coords);
            assert_eq!(explicit.len(), coords.len());
            for k in 0..coords.len() {
                assert_eq!(
                    coords.at(k),
                    explicit.at(k),
                    "point {k}, mode {scan_mode:b}"
                );
            }
            assert_eq!(coords.at(coords.len()), None, "past the end is no point");
        }

        // The formula itself, read off the corners of the default i-consecutive
        // scan: point 0 is the origin, point 1 steps in longitude, point `ni`
        // steps in latitude.
        let coords = regular(151, 97, 0b0100_0000);
        assert_eq!(coords.at(0), Some((54.995, -129.995)));
        let (lat, lon) = coords.at(1).expect("in range");
        assert!((lon - (-129.985)).abs() < 1e-12, "{lon}");
        assert!((lat - 54.995).abs() < 1e-12, "{lat}");
        let (lat, lon) = coords.at(151).expect("in range");
        assert!((lat - 54.985).abs() < 1e-12, "{lat}");
        assert!((lon - (-129.995)).abs() < 1e-12, "{lon}");
    }

    /// `nearest` is `at` run backwards, for every point of the grid and for a
    /// probe nudged off each lattice point.
    #[test]
    fn a_regular_grid_nearest_is_the_inverse_of_at() {
        for scan_mode in [0b0100_0000u8, 0b0110_0000, 0b0101_0000] {
            let coords = regular(151, 97, scan_mode);
            assert!(coords.len() > 10_000);
            for k in 0..coords.len() {
                let (lat, lon) = coords.at(k).expect("in range");
                assert_eq!(coords.nearest(lat, lon), Some(k), "point {k}");
                // A third of a cell off in both directions still rounds home.
                assert_eq!(
                    coords.nearest(lat - 0.01 / 3.0, lon + 0.01 / 3.0),
                    Some(k),
                    "point {k} nudged",
                );
            }
        }
        let coords = regular(151, 97, 0b0100_0000);
        assert_eq!(coords.nearest(54.995 + 0.02, -129.995), None, "north of it");
        assert_eq!(coords.nearest(54.995, -129.995 - 0.02), None, "west of it");
        assert_eq!(coords.nearest(0.0, 0.0), None, "nowhere near it");
        assert_eq!(coords.nearest(f64::NAN, -129.0), None);
    }

    /// `index_bounds` answers for the shape it was built with and refuses every
    /// other, exactly as the Lambert arm's `is_row_major` guard does. A grid
    /// answering for a shape it does not have would place the window's four
    /// edges against a different lattice.
    #[test]
    fn a_regular_grid_refuses_a_shape_it_was_not_built_with() {
        let coords = regular(151, 97, 0b0100_0000);
        let bounds = GeoBounds {
            min_lat: 54.0,
            max_lat: 54.5,
            min_lon: -129.5,
            max_lon: -129.0,
        };
        let answer = coords
            .index_bounds(&bounds, 151, 97)
            .expect("its own shape is answered");
        // 0.5 degrees at a 0.01 step is fifty cells each way.
        assert!((answer.1 - answer.0 - 50.0).abs() < 1e-6, "{answer:?}");
        assert!((answer.3 - answer.2 - 50.0).abs() < 1e-6, "{answer:?}");

        for (ni, nj) in [(150, 97), (151, 96), (97, 151), (0, 0)] {
            assert_eq!(
                coords.index_bounds(&bounds, ni, nj),
                None,
                "answered for {ni}x{nj}, which is not the shape it holds",
            );
        }
        // A scan whose flat index is not `j * ni + i` is refused for the same
        // reason, even at the right shape.
        for scan_mode in [0b0110_0000u8, 0b0101_0000, 0b0111_0000] {
            assert_eq!(
                regular(151, 97, scan_mode).index_bounds(&bounds, 151, 97),
                None,
                "mode {scan_mode:b} is not row-major",
            );
        }
        // An inverted box is refused rather than answered upside down.
        assert_eq!(
            coords.index_bounds(
                &GeoBounds {
                    min_lat: 54.5,
                    max_lat: 54.0,
                    min_lon: -129.0,
                    max_lon: -129.5,
                },
                151,
                97,
            ),
            None,
        );
    }

    /// The two window inputs `Explicit` cannot answer, which is the whole reason
    /// this arm exists.
    #[test]
    fn a_regular_grid_answers_the_two_questions_explicit_cannot() {
        let coords = regular(151, 97, 0b0100_0000);
        assert_eq!(coords.cell_span_degrees(45.0), Some(0.01));
        assert_eq!(
            coords.cell_span_degrees(89.0),
            coords.cell_span_degrees(0.0),
            "a degree grid's cell is degrees; latitude does not enter it",
        );
        assert!(!coords.wraps_longitude());
        assert_eq!(materialise(&coords).cell_span_degrees(45.0), None);

        // A grid that goes all the way round does wrap, and one a cell short
        // does not — the boundary, from both sides.
        let round = GridCoords::Regular {
            lat0: 0.0,
            lon0: -180.0,
            dlat: 0.1,
            dlon: 0.1,
            ni: 3600,
            nj: 10,
            scan_mode: 0b0100_0000,
        };
        assert!(round.wraps_longitude());
        assert!(
            !GridCoords::Regular {
                lat0: 0.0,
                lon0: -180.0,
                dlat: 0.1,
                dlon: 0.1,
                ni: 3599,
                nj: 10,
                scan_mode: 0b0100_0000,
            }
            .wraps_longitude(),
        );
    }

    // ── The visibility short-circuit ───────────────────────────────────

    /// `paints` is what `summarize_values` asks instead of building a colour,
    /// so the two must answer the same everywhere — including at every stop of
    /// every ramp, where an off-by-one comparison would live.
    #[test]
    fn a_visibility_test_agrees_with_every_ramp() {
        for &p in ModelParameter::all() {
            let mut probes: Vec<f32> = vec![
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                0.0,
                -0.0,
                f32::MIN,
                f32::MAX,
            ];
            // Every stop, exactly, and either side of it — in the ramp's own
            // units carried back to the raw ones the grid states, by bisecting
            // `convert_for_display` rather than restating its arithmetic.
            for (stop, _) in p.legend_thresholds() {
                for scale in [1.0f32, 0.999_999, 1.000_001] {
                    let display = stop * scale;
                    probes.push(display);
                    let (mut lo, mut hi) = (-1.0e6f32, 1.0e6f32);
                    for _ in 0..200 {
                        let mid = 0.5 * (lo + hi);
                        if p.convert_for_display(mid) < display {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    probes.push(lo);
                    probes.push(hi);
                }
            }
            let mut v = -1500.0f32;
            while v <= 20_000.0 {
                probes.push(v);
                v += if v.abs() < 1200.0 { 0.5 } else { 11.0 };
            }

            let mut seen = (false, false);
            for probe in probes {
                let painted = p.color_for_value(probe)[3] != 0;
                assert_eq!(
                    p.paints(probe),
                    painted,
                    "{p:?} at {probe}: the short-circuit and the ramp disagree",
                );
                if painted {
                    seen.0 = true;
                } else {
                    seen.1 = true;
                }
            }
            assert_eq!(
                seen,
                (true, true),
                "{p:?}: the sweep produced only one answer, so the agreement \
                 above is vacuous",
            );
        }
    }

    /// `summarize_values` asks its predicate once per **finite** point and
    /// never builds a colour: it runs over 1.9 M points on the fetch path.
    #[test]
    fn summarize_is_linear_in_points() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let values = vec![1.0, f32::NAN, 2.0, f32::INFINITY, 3.0, f32::NEG_INFINITY];
        let (visible, range) = summarize_values(&values, |v| {
            calls.set(calls.get() + 1);
            v > 1.5
        });
        assert_eq!(
            calls.get(),
            3,
            "the predicate ran {} times over three finite points",
            calls.get(),
        );
        assert_eq!(visible, 2);
        assert_eq!(range, Some((1.0, 3.0)));

        // The floor: the count above would also read three if the predicate's
        // answer were ignored, so pin that a different answer moves the tally.
        let (all, _) = summarize_values(&values, |_| true);
        assert_eq!(all, 3);
        let (none, _) = summarize_values(&values, |_| false);
        assert_eq!(none, 0);
    }
}
