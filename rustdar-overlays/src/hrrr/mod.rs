//! HRRR model data fetch and types.
//!
//! Fetches HRRR f00 (analysis) fields from NOAA NOMADS server-side filter.
//! Supports CIN, CAPE, SRH, bulk shear, wind gusts, lifted index, PWAT,
//! temperature, dewpoint, updraft helicity, and visibility via the
//! `ModelParameter` enum.

pub mod fetch;

use crate::types::GeoBounds;

/// A selectable model parameter to fetch and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ModelParameter {
    // --- Instability / Thermodynamics ---
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

    // --- Helicity / Rotation ---
    /// 0-1 km Storm-Relative Helicity (m²/s²).
    Srh1km,
    /// 0-3 km Storm-Relative Helicity (m²/s²).
    Srh3km,
    /// Max Updraft Helicity 2-5 km AGL (m²/s²).
    MaxUH2to5km,
    /// Max Updraft Helicity 0-2 km AGL (m²/s²).
    MaxUH0to2km,

    // --- Wind ---
    /// 0-6 km Bulk Wind Shear magnitude (GRIB U+V m/s → display kt).
    /// Composite parameter requiring two NOMADS fetches.
    BulkShear6km,
    /// Surface Wind Gust (GRIB m/s → display kt).
    SurfaceWindGust,

    // --- Moisture ---
    /// Precipitable Water (GRIB kg/m² → display in).
    PrecipitableWater,

    // --- Surface ---
    /// 2 m Temperature (GRIB K → display °F).
    Temperature2m,
    /// 2 m Dewpoint (GRIB K → display °F).
    Dewpoint2m,

    // --- Visibility ---
    /// Surface Visibility (GRIB m → display mi).
    Visibility,
}

impl ModelParameter {
    /// All available parameters.
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

    /// Whether this parameter requires multiple NOMADS fetches that are
    /// merged into a single grid (e.g. U+V shear components → magnitude).
    pub fn is_composite(&self) -> bool {
        matches!(self, ModelParameter::BulkShear6km)
    }

    /// For composite parameters, returns the (var, lev) pairs to fetch.
    /// Each pair results in one NOMADS request; values are merged in
    /// `fetch::fetch_composite_hrrr_data()`.
    pub fn composite_parts(&self) -> Option<Vec<(&'static str, &'static str)>> {
        match self {
            ModelParameter::BulkShear6km => Some(vec![
                ("var_VUCSH", "lev_0-6000_m_above_ground"),
                ("var_VVCSH", "lev_0-6000_m_above_ground"),
            ]),
            _ => None,
        }
    }

    /// NOMADS `var_*` query parameter name (e.g. `"var_CIN"`).
    /// Panics for composite parameters — use `composite_parts()` instead.
    pub fn nomads_var(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => "var_CIN",
            ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => "var_CAPE",
            ModelParameter::LiftedIndex => "var_LFTX",
            ModelParameter::Srh1km | ModelParameter::Srh3km => "var_HLCY",
            ModelParameter::MaxUH2to5km | ModelParameter::MaxUH0to2km => "var_MXUPHL",
            ModelParameter::SurfaceWindGust => "var_GUST",
            ModelParameter::PrecipitableWater => "var_PWAT",
            ModelParameter::Temperature2m => "var_TMP",
            ModelParameter::Dewpoint2m => "var_DPT",
            ModelParameter::Visibility => "var_VIS",
            ModelParameter::BulkShear6km => {
                panic!("BulkShear6km is composite — use composite_parts()")
            }
        }
    }

    /// NOMADS `lev_*` query parameter name.
    /// Panics for composite parameters.
    pub fn nomads_level(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin
            | ModelParameter::SurfaceBasedCape
            | ModelParameter::SurfaceWindGust
            | ModelParameter::Visibility => "lev_surface",
            ModelParameter::MixedLayerCin | ModelParameter::MixedLayerCape => {
                "lev_180-0_mb_above_ground"
            }
            ModelParameter::MostUnstableCape => "lev_255-0_mb_above_ground",
            ModelParameter::LiftedIndex => "lev_500-1000_mb",
            ModelParameter::Srh1km => "lev_1000-0_m_above_ground",
            ModelParameter::Srh3km => "lev_3000-0_m_above_ground",
            ModelParameter::MaxUH2to5km => "lev_2000-5000_m_above_ground",
            ModelParameter::MaxUH0to2km => "lev_0-2000_m_above_ground",
            ModelParameter::PrecipitableWater => {
                "lev_entire_atmosphere_(considered_as_a_single_layer)"
            }
            ModelParameter::Temperature2m | ModelParameter::Dewpoint2m => {
                "lev_2_m_above_ground"
            }
            ModelParameter::BulkShear6km => {
                panic!("BulkShear6km is composite — use composite_parts()")
            }
        }
    }

    /// Human-readable display name.
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

    /// Short label for the parameter.
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

    /// Unit label for display (after conversion).
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

    /// Convert a raw GRIB2 value to display units.
    ///
    /// CIN/CAPE/SRH/UH are identity (already in display units).
    /// Wind: m/s → knots. Temperature: K → °F. PWAT: kg/m² → inches.
    /// Visibility: m → statute miles.
    /// Lifted Index: K → °C (GRIB2 LFTX is a temperature differential stored in K;
    /// the numeric value is already equivalent to °C since it's a delta).
    pub fn convert_for_display(&self, value: f32) -> f32 {
        match self {
            // Identity — already in display units.
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
            // m/s → knots.
            ModelParameter::BulkShear6km | ModelParameter::SurfaceWindGust => value * 1.94384,
            // kg/m² (≈ mm) → inches.
            ModelParameter::PrecipitableWater => value / 25.4,
            // K → °F.
            ModelParameter::Temperature2m | ModelParameter::Dewpoint2m => {
                value * 9.0 / 5.0 - 459.67
            }
            // m → statute miles.
            ModelParameter::Visibility => value / 1609.344,
        }
    }

    /// Format a grid value (in raw GRIB2 units) for hover tooltip display.
    pub fn format_value(&self, value: f32) -> String {
        if value.is_nan() {
            return String::new();
        }
        let display = self.convert_for_display(value);
        match self {
            // 0 decimal places for most.
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
                format!("{}: {:.0} {}", self.short_name(), display, self.unit_label())
            }
            // 1 decimal for LI.
            ModelParameter::LiftedIndex => {
                format!("{}: {:.1} {}", self.short_name(), display, self.unit_label())
            }
            // 2 decimal places for PWAT / visibility.
            ModelParameter::PrecipitableWater | ModelParameter::Visibility => {
                format!("{}: {:.2} {}", self.short_name(), display, self.unit_label())
            }
        }
    }

    /// Map a raw GRIB2 value to an RGBA color for rendering.
    ///
    /// Converts to display units first so all color thresholds are in
    /// human-readable units.
    pub fn color_for_value(&self, value: f32) -> [u8; 4] {
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
            ModelParameter::PrecipitableWater => {
                pwat_color(self.convert_for_display(value))
            }
            ModelParameter::Temperature2m => {
                temperature_color(self.convert_for_display(value))
            }
            ModelParameter::Dewpoint2m => {
                dewpoint_color(self.convert_for_display(value))
            }
            ModelParameter::Visibility => {
                visibility_color(self.convert_for_display(value))
            }
        }
    }

    /// Serialization key for config persistence.
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

    /// Color thresholds for the legend scale (in display units).
    /// Returns `(value, [r, g, b])` pairs in ascending order.
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
                // More negative = more unstable.
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
            _ => ModelParameter::SurfaceBasedCin,
        })
    }
}

/// Color scale for CIN (Convective Inhibition).
///
/// CIN values are ≤ 0 J/kg. More negative = stronger cap (more inhibition).
/// - 0 to −25: transparent (negligible)
/// - −25 to −50: light green (weak cap)
/// - −50 to −100: yellow (moderate cap)
/// - −100 to −200: orange (strong cap)
/// - −200 to −500+: red/dark red (extreme cap)
fn cin_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;

    // CIN is ≤ 0; we work with the magnitude for thresholding.
    let mag = -value;

    if mag < 25.0 {
        // Negligible CIN — transparent.
        [0, 0, 0, 0]
    } else if mag < 50.0 {
        // Weak cap: light green.
        let t = (mag - 25.0) / 25.0;
        lerp_color([144, 238, 144, ALPHA], [255, 255, 100, ALPHA], t)
    } else if mag < 100.0 {
        // Moderate cap: yellow → orange.
        let t = (mag - 50.0) / 50.0;
        lerp_color([255, 255, 100, ALPHA], [255, 165, 0, ALPHA], t)
    } else if mag < 200.0 {
        // Strong cap: orange → red.
        let t = (mag - 100.0) / 100.0;
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], t)
    } else {
        // Extreme cap: red → dark purple.
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
        lerp_color([200, 230, 255, ALPHA], [100, 200, 100, ALPHA], (value - 250.0) / 250.0)
    } else if value < 1000.0 {
        lerp_color([100, 200, 100, ALPHA], [255, 255, 0, ALPHA], (value - 500.0) / 500.0)
    } else if value < 2000.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (value - 1000.0) / 1000.0)
    } else if value < 3000.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (value - 2000.0) / 1000.0)
    } else {
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], ((value - 3000.0) / 2000.0).min(1.0))
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
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (-value - 2.0) / 2.0)
    } else if value > -6.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (-value - 4.0) / 2.0)
    } else {
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], ((-value - 6.0) / 4.0).min(1.0))
    }
}

/// SRH color scale (m²/s², ≥ 0). Higher = more rotation potential.
fn srh_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value < 50.0 {
        [0, 0, 0, 0]
    } else if value < 100.0 {
        lerp_color([144, 238, 144, ALPHA], [255, 255, 0, ALPHA], (value - 50.0) / 50.0)
    } else if value < 200.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (value - 100.0) / 100.0)
    } else if value < 300.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (value - 200.0) / 100.0)
    } else {
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], ((value - 300.0) / 200.0).min(1.0))
    }
}

/// Updraft helicity 2–5 km color scale (m²/s²).
fn uh_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value < 25.0 {
        [0, 0, 0, 0]
    } else if value < 75.0 {
        lerp_color([144, 238, 144, ALPHA], [255, 255, 0, ALPHA], (value - 25.0) / 50.0)
    } else if value < 150.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (value - 75.0) / 75.0)
    } else if value < 300.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (value - 150.0) / 150.0)
    } else {
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], ((value - 300.0) / 200.0).min(1.0))
    }
}

/// Updraft helicity 0–2 km color scale (m²/s², lower thresholds).
fn uh_low_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if value < 10.0 {
        [0, 0, 0, 0]
    } else if value < 30.0 {
        lerp_color([144, 238, 144, ALPHA], [255, 255, 0, ALPHA], (value - 10.0) / 20.0)
    } else if value < 75.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (value - 30.0) / 45.0)
    } else if value < 150.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (value - 75.0) / 75.0)
    } else {
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], ((value - 150.0) / 150.0).min(1.0))
    }
}

/// Wind speed color scale (kt, display units). Used by gust + shear.
fn wind_color(kt: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if kt < 10.0 {
        [0, 0, 0, 0]
    } else if kt < 20.0 {
        lerp_color([144, 238, 144, ALPHA], [255, 255, 0, ALPHA], (kt - 10.0) / 10.0)
    } else if kt < 30.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (kt - 20.0) / 10.0)
    } else if kt < 45.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (kt - 30.0) / 15.0)
    } else {
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], ((kt - 45.0) / 20.0).min(1.0))
    }
}

/// Precipitable water color scale (inches, display units).
fn pwat_color(inches: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if inches < 0.75 {
        [0, 0, 0, 0]
    } else if inches < 1.0 {
        lerp_color([200, 230, 255, ALPHA], [100, 200, 100, ALPHA], (inches - 0.75) / 0.25)
    } else if inches < 1.5 {
        lerp_color([100, 200, 100, ALPHA], [255, 255, 0, ALPHA], (inches - 1.0) / 0.5)
    } else if inches < 2.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (inches - 1.5) / 0.5)
    } else {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], ((inches - 2.0) / 0.5).min(1.0))
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
        lerp_color([100, 150, 255, ALPHA], [100, 200, 100, ALPHA], (f - 32.0) / 18.0)
    } else if f < 70.0 {
        lerp_color([100, 200, 100, ALPHA], [255, 255, 0, ALPHA], (f - 50.0) / 20.0)
    } else if f < 90.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (f - 70.0) / 20.0)
    } else if f < 110.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (f - 90.0) / 20.0)
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
        lerp_color([180, 150, 100, ALPHA], [144, 238, 144, ALPHA], (f - 30.0) / 15.0)
    } else if f < 55.0 {
        lerp_color([144, 238, 144, ALPHA], [100, 200, 100, ALPHA], (f - 45.0) / 10.0)
    } else if f < 65.0 {
        lerp_color([100, 200, 100, ALPHA], [255, 255, 0, ALPHA], (f - 55.0) / 10.0)
    } else if f < 70.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (f - 65.0) / 5.0)
    } else {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], ((f - 70.0) / 5.0).min(1.0))
    }
}

/// Visibility color scale (miles, display units). Lower = worse.
fn visibility_color(mi: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;
    if mi > 10.0 {
        [0, 0, 0, 0]
    } else if mi > 5.0 {
        lerp_color([144, 238, 144, ALPHA], [255, 255, 0, ALPHA], (10.0 - mi) / 5.0)
    } else if mi > 3.0 {
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], (5.0 - mi) / 2.0)
    } else if mi > 1.0 {
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], (3.0 - mi) / 2.0)
    } else {
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], ((1.0 - mi) / 0.5).min(1.0))
    }
}

/// Linear interpolation between two RGBA colors.
fn lerp_color(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t) as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t) as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t) as u8,
        (a[3] as f32 + (b[3] as f32 - a[3] as f32) * t) as u8,
    ]
}

/// Parsed HRRR grid data for a single model parameter.
#[derive(Debug, Clone)]
pub struct HrrrGridData {
    /// Which parameter this grid represents.
    pub parameter: ModelParameter,
    /// Grid point values in row-major order (nj rows × ni columns).
    /// NaN for missing/undefined points.
    pub values: Vec<f32>,
    /// Latitude of each grid point (same length as `values`).
    pub lats: Vec<f64>,
    /// Longitude of each grid point (same length as `values`).
    pub lons: Vec<f64>,
    /// Number of columns in the grid.
    pub ni: usize,
    /// Number of rows in the grid.
    pub nj: usize,
    /// Geographic bounds enclosing all grid points.
    pub bounds: GeoBounds,
    /// Model reference time (UTC).
    pub ref_time: chrono::NaiveDateTime,
}

/// Type-erased fetch result wrapper for the overlay handler.
pub struct HrrrFetchResult(pub Result<HrrrGridData, String>);
