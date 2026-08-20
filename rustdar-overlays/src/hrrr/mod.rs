//! HRRR model data fetch and types.
//!
//! Fields are byte-ranged out of the `noaa-hrrr-bdp-pds` S3 bucket; see
//! [`fetch`]. Most parameters are instantaneous and come from f00 (the
//! analysis). The updraft-helicity parameters are accumulations and cannot —
//! see [`ModelParameter::forecast_hour`].

pub mod fetch;
pub mod fields;
pub mod lambert;

use rustdar_geo::GeoBounds;

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
        ]
    }

    /// Which forecast hour of the run to fetch this parameter from. f00, the
    /// analysis, for every instantaneous field.
    /// **`MXUPHL` at f00 is identically 0.0 everywhere**: it is a maximum over
    /// the forecast period, and at f00 that period has zero length. f01 is the
    /// first hour with a real window (`0-1 hour max fcst`), which is why
    /// [`HrrrGridData::forecast_hour`] reaches the UI: a 0-1 h maximum must not
    /// be presented as the analysis.
    pub fn forecast_hour(&self) -> u8 {
        match self {
            ModelParameter::MaxUH2to5km | ModelParameter::MaxUH0to2km => 1,
            _ => 0,
        }
    }

    pub fn is_windowed(&self) -> bool {
        matches!(
            self,
            ModelParameter::MaxUH2to5km | ModelParameter::MaxUH0to2km
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
        }
    }

    /// Convert a raw GRIB2 value to display units: identity for CIN/CAPE/SRH/UH,
    /// m/s → kt, K → °F, kg/m² → in, m → mi, and K → °C for LFTX (a temperature
    /// differential, so the number is already °C).
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
            | ModelParameter::MaxUH0to2km => value,
            // LFTX is a temperature *difference* so K ≡ °C.
            ModelParameter::LiftedIndex => value,
            ModelParameter::BulkShear6km | ModelParameter::SurfaceWindGust => value * 1.94384,
            ModelParameter::PrecipitableWater => value / 25.4,
            ModelParameter::Temperature2m | ModelParameter::Dewpoint2m => {
                value * 9.0 / 5.0 - 459.67
            }
            ModelParameter::Visibility => value / 1609.344,
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
            | ModelParameter::Dewpoint2m => {
                format!("{:.0} {}", display, self.unit_label())
            }
            ModelParameter::LiftedIndex => {
                format!("{:.1} {}", display, self.unit_label())
            }
            ModelParameter::PrecipitableWater | ModelParameter::Visibility => {
                format!("{:.2} {}", display, self.unit_label())
            }
        }
    }

    /// Map a raw GRIB2 value to an RGBA color for rendering.
    ///
    /// The NaN guard is load-bearing: every ramp below is a descending `if`
    /// chain ending in an unguarded `else` and NaN fails every comparison, so a
    /// missing point would paint the *most extreme* colour on the scale.
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
    Explicit { lats: Vec<f64>, lons: Vec<f64> },
}

impl GridCoords {
    pub fn len(&self) -> usize {
        match self {
            GridCoords::Lambert(g) => g.len(),
            GridCoords::Explicit { lats, lons } => lats.len().min(lons.len()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn at(&self, index: usize) -> Option<(f64, f64)> {
        match self {
            GridCoords::Lambert(g) => g.latlon_at(index),
            GridCoords::Explicit { lats, lons } => Some((*lats.get(index)?, *lons.get(index)?)),
        }
    }

    /// Fractional `(i_min, i_max, j_min, j_max)` bounding every grid point inside
    /// `bounds`, or `None` when there is no cheaper answer than all. Only the
    /// Lambert case answers, and `ni`/`nj` are the *caller's* grid shape, so a
    /// shape that does not match is refused rather than answered wrongly.
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
            _ => None,
        }
    }

    /// Upper bound on how many degrees one grid cell spans near `lat` — see
    /// [`lambert::LambertGrid::cell_span_degrees`].
    pub fn cell_span_degrees(&self, lat: f64) -> Option<f64> {
        match self {
            GridCoords::Lambert(g) => Some(g.cell_span_degrees(lat)),
            GridCoords::Explicit { .. } => None,
        }
    }

    /// Whether adjacent grid points can jump most of a turn in longitude — see
    /// [`lambert::LambertGrid::wraps_longitude`]. Always `false` for `Explicit`.
    pub fn wraps_longitude(&self) -> bool {
        match self {
            GridCoords::Lambert(g) => g.wraps_longitude(),
            GridCoords::Explicit { .. } => false,
        }
    }

    /// Index of the grid point nearest `(lat, lon)`, or `None` when the grid does
    /// not cover it. O(1) for a Lambert grid — the flat scan it replaces ran over
    /// all 1.9 M points on every hover frame.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<usize> {
        match self {
            GridCoords::Lambert(g) => g.nearest(lat, lon),
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
    /// Forecast hour this grid came from. 0 is the analysis; see
    /// [`ModelParameter::forecast_hour`] for why UH is not 0.
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

/// One pass for the render-coverage summary on [`HrrrGridData`]. Non-finite
/// points are missing data, not readings — excluded from both figures.
pub fn summarize_values(values: &[f32], param: ModelParameter) -> (usize, Option<(f32, f32)>) {
    let mut visible = 0usize;
    let mut range: Option<(f32, f32)> = None;
    for &v in values {
        if !v.is_finite() {
            continue;
        }
        if param.color_for_value(v)[3] != 0 {
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
        let (visible_points, value_range) = summarize_values(&values, param);
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
            forecast_hour: param.forecast_hour(),
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
        let (visible, range) = summarize_values(&values, ModelParameter::MaxUH2to5km);
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
}
