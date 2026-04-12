//! HRRR model data fetch and types.
//!
//! Fetches HRRR f00 (analysis) fields from NOAA NOMADS server-side filter.
//! Supports CIN and CAPE parameters; extensible to SRH, shear, etc.
//! via `ModelParameter` enum variants.

pub mod fetch;

use crate::types::GeoBounds;

/// A selectable model parameter to fetch and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ModelParameter {
    /// Surface-Based Convective Inhibition (J/kg, ≤ 0).
    SurfaceBasedCin,
    /// Mixed-Layer Convective Inhibition (180-0 mb above ground, J/kg, ≤ 0).
    MixedLayerCin,
    /// Surface-Based Convective Available Potential Energy (J/kg, ≥ 0).
    SurfaceBasedCape,
    /// Mixed-Layer CAPE (180-0 mb above ground, J/kg, ≥ 0).
    MixedLayerCape,
    /// Most-Unstable CAPE (255-0 mb above ground, J/kg, ≥ 0).
    MostUnstableCape,
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
        ]
    }

    /// NOMADS `var_*` query parameter name (e.g. `"var_CIN"`).
    pub fn nomads_var(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => "var_CIN",
            ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => "var_CAPE",
        }
    }

    /// NOMADS `lev_*` query parameter name.
    pub fn nomads_level(&self) -> &'static str {
        match self {
            ModelParameter::SurfaceBasedCin | ModelParameter::SurfaceBasedCape => "lev_surface",
            ModelParameter::MixedLayerCin | ModelParameter::MixedLayerCape => {
                "lev_180-0_mb_above_ground"
            }
            ModelParameter::MostUnstableCape => "lev_255-0_mb_above_ground",
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
        }
    }

    /// Unit label for display.
    pub fn unit_label(&self) -> &'static str {
        "J/kg"
    }

    /// Format a grid value for hover tooltip display.
    pub fn format_value(&self, value: f32) -> String {
        if value.is_nan() {
            return String::new();
        }
        format!("{}: {:.0} {}", self.short_name(), value, self.unit_label())
    }

    /// Map a data value to an RGBA color for rendering.
    pub fn color_for_value(&self, value: f32) -> [u8; 4] {
        match self {
            ModelParameter::SurfaceBasedCin | ModelParameter::MixedLayerCin => cin_color(value),
            ModelParameter::SurfaceBasedCape
            | ModelParameter::MixedLayerCape
            | ModelParameter::MostUnstableCape => cape_color(value),
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
        }
    }

    /// Color thresholds for the legend scale.
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

/// Color scale for CAPE (Convective Available Potential Energy).
///
/// CAPE values are ≥ 0 J/kg. Higher = more instability.
/// - 0 to 250: transparent (negligible)
/// - 250 to 500: light blue → green (weak)
/// - 500 to 1000: green → yellow (moderate)
/// - 1000 to 2000: yellow → orange (strong)
/// - 2000 to 3000: orange → red (very strong)
/// - 3000 to 5000+: red → purple (extreme)
fn cape_color(value: f32) -> [u8; 4] {
    const ALPHA: u8 = 160;

    if value < 250.0 {
        [0, 0, 0, 0]
    } else if value < 500.0 {
        let t = (value - 250.0) / 250.0;
        lerp_color([200, 230, 255, ALPHA], [100, 200, 100, ALPHA], t)
    } else if value < 1000.0 {
        let t = (value - 500.0) / 500.0;
        lerp_color([100, 200, 100, ALPHA], [255, 255, 0, ALPHA], t)
    } else if value < 2000.0 {
        let t = (value - 1000.0) / 1000.0;
        lerp_color([255, 255, 0, ALPHA], [255, 165, 0, ALPHA], t)
    } else if value < 3000.0 {
        let t = (value - 2000.0) / 1000.0;
        lerp_color([255, 165, 0, ALPHA], [220, 50, 50, ALPHA], t)
    } else {
        let t = ((value - 3000.0) / 2000.0).min(1.0);
        lerp_color([220, 50, 50, ALPHA], [180, 0, 200, ALPHA], t)
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
