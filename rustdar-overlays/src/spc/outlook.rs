use chrono::NaiveDateTime;
use crate::types::{GeoPolygon, HatchPattern, OverlayFeature, CIG_FILL_ALPHA, REGULAR_FILL_ALPHA};

/// Which outlook day to request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OutlookDay {
    Day1,
    Day2,
    Day3,
    Day4,
    Day5,
    Day6,
    Day7,
    Day8,
}

impl OutlookDay {
    /// Returns all outlook day variants.
    pub fn all() -> &'static [OutlookDay] {
        &[
            OutlookDay::Day1,
            OutlookDay::Day2,
            OutlookDay::Day3,
            OutlookDay::Day4,
            OutlookDay::Day5,
            OutlookDay::Day6,
            OutlookDay::Day7,
            OutlookDay::Day8,
        ]
    }

    /// Short label ("1", "2", … "8") for UI display.
    pub fn label(self) -> &'static str {
        match self {
            OutlookDay::Day1 => "1",
            OutlookDay::Day2 => "2",
            OutlookDay::Day3 => "3",
            OutlookDay::Day4 => "4",
            OutlookDay::Day5 => "5",
            OutlookDay::Day6 => "6",
            OutlookDay::Day7 => "7",
            OutlookDay::Day8 => "8",
        }
    }

    /// Returns the available outlook products for this day.
    pub fn products(self) -> &'static [OutlookProduct] {
        match self {
            OutlookDay::Day1 | OutlookDay::Day2 => &[
                OutlookProduct::Categorical,
                OutlookProduct::Tornado,
                OutlookProduct::Wind,
                OutlookProduct::Hail,
            ],
            OutlookDay::Day3 => &[
                OutlookProduct::Categorical,
                OutlookProduct::Probabilistic,
            ],
            _ => &[OutlookProduct::Probabilistic],
        }
    }

    /// Whether this is an extended-range day (4-8).
    pub fn is_extended(self) -> bool {
        matches!(
            self,
            OutlookDay::Day4
                | OutlookDay::Day5
                | OutlookDay::Day6
                | OutlookDay::Day7
                | OutlookDay::Day8
        )
    }
}

impl std::fmt::Display for OutlookDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutlookDay::Day1 => write!(f, "Day 1"),
            OutlookDay::Day2 => write!(f, "Day 2"),
            OutlookDay::Day3 => write!(f, "Day 3"),
            OutlookDay::Day4 => write!(f, "Day 4"),
            OutlookDay::Day5 => write!(f, "Day 5"),
            OutlookDay::Day6 => write!(f, "Day 6"),
            OutlookDay::Day7 => write!(f, "Day 7"),
            OutlookDay::Day8 => write!(f, "Day 8"),
        }
    }
}

/// Which outlook product to request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OutlookProduct {
    Categorical,
    Tornado,
    Wind,
    Hail,
    /// Day 2/3 combined probabilistic product.
    Probabilistic,
}

impl std::fmt::Display for OutlookProduct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutlookProduct::Categorical => write!(f, "Categorical"),
            OutlookProduct::Tornado => write!(f, "Tornado"),
            OutlookProduct::Wind => write!(f, "Wind"),
            OutlookProduct::Hail => write!(f, "Hail"),
            OutlookProduct::Probabilistic => write!(f, "Probabilistic"),
        }
    }
}

/// A parsed SPC convective outlook.
#[derive(Debug, Clone)]
pub struct SpcOutlook {
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub valid: Option<NaiveDateTime>,
    pub expire: Option<NaiveDateTime>,
    pub features: Vec<OverlayFeature>,
}

/// Build the SPC GeoJSON URL for the given day and product.
///
/// The origin comes from
/// [`DataSources::spc_base`](rustdar_radar::sources::DataSources::spc_base)
/// rather than a literal, so SPC is covered by the same origin table as every
/// other feed — including the check that no production URL points at an origin
/// the browser cannot reach.
pub fn outlook_url(
    sources: &rustdar_radar::sources::DataSources,
    day: OutlookDay,
    product: OutlookProduct,
) -> String {
    let base = &sources.spc_base;
    // Days 4-8 use a separate extended-range endpoint
    if day.is_extended() {
        let n = match day {
            OutlookDay::Day4 => 4,
            OutlookDay::Day5 => 5,
            OutlookDay::Day6 => 6,
            OutlookDay::Day7 => 7,
            OutlookDay::Day8 => 8,
            _ => unreachable!(),
        };
        return format!("{base}/products/exper/day4-8/day{n}prob.lyr.geojson");
    }

    let day_str = match day {
        OutlookDay::Day1 => "day1otlk",
        OutlookDay::Day2 => "day2otlk",
        OutlookDay::Day3 => "day3otlk",
        _ => unreachable!(),
    };

    let product_str = match (day, product) {
        (_, OutlookProduct::Categorical) => "_cat",
        (OutlookDay::Day1 | OutlookDay::Day2, OutlookProduct::Tornado) => "_torn",
        (OutlookDay::Day1 | OutlookDay::Day2, OutlookProduct::Wind) => "_wind",
        (OutlookDay::Day1 | OutlookDay::Day2, OutlookProduct::Hail) => "_hail",
        // Day 3 uses combined probabilistic endpoint for all hazard types
        (_, OutlookProduct::Tornado)
        | (_, OutlookProduct::Wind)
        | (_, OutlookProduct::Hail)
        | (_, OutlookProduct::Probabilistic) => "_prob",
    };

    format!("{base}/products/outlook/{day_str}{product_str}.lyr.geojson")
}

/// Parse an SPC GeoJSON response into an `SpcOutlook`.
///
/// The GeoJSON has this structure:
/// ```json
/// {
///   "type": "FeatureCollection",
///   "features": [
///     {
///       "type": "Feature",
///       "geometry": { "type": "MultiPolygon", "coordinates": [[[[lon, lat], ...]]] },
///       "properties": {
///         "LABEL": "SLGT",
///         "LABEL2": "Slight Risk",
///         "fill": "#FFE066",
///         "stroke": "#DDAA00",
///         "VALID": "202603062000",
///         "EXPIRE": "202603071200",
///         ...
///       }
///     }
///   ]
/// }
/// ```
pub fn parse_geojson(
    json: &serde_json::Value,
    day: OutlookDay,
    product: OutlookProduct,
) -> Result<SpcOutlook, String> {
    let features_array = json
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'features' array in GeoJSON".to_string())?;

    let mut features = Vec::new();
    let mut valid: Option<NaiveDateTime> = None;
    let mut expire: Option<NaiveDateTime> = None;

    for feature_val in features_array {
        let ParsedOutlookFeature { feature, valid: feat_valid, expire: feat_expire } = match parse_outlook_feature(feature_val)? {
            Some(result) => result,
            None => continue,
        };
        if valid.is_none() {
            valid = feat_valid;
        }
        if expire.is_none() {
            expire = feat_expire;
        }
        features.push(feature);
    }

    Ok(SpcOutlook {
        day,
        product,
        valid,
        expire,
        features,
    })
}

/// A parsed outlook feature with its optional validity window.
struct ParsedOutlookFeature {
    feature: OverlayFeature,
    valid: Option<NaiveDateTime>,
    expire: Option<NaiveDateTime>,
}

/// Parse a single GeoJSON feature into an `OverlayFeature` with optional timestamps.
///
/// Returns `None` for features that should be skipped (empty geometry, unsupported types).
fn parse_outlook_feature(
    feature_val: &serde_json::Value,
) -> Result<Option<ParsedOutlookFeature>, String> {
    let properties = feature_val
        .get("properties")
        .ok_or_else(|| "Feature missing 'properties'".to_string())?;

    let label = properties
        .get("LABEL")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let label2 = properties
        .get("LABEL2")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let fill_hex = properties
        .get("fill")
        .and_then(|v| v.as_str())
        .unwrap_or("#888888");

    let stroke_hex = properties
        .get("stroke")
        .and_then(|v| v.as_str())
        .unwrap_or("#000000");

    // Detect CIG hatching pattern from label
    let hatch = match label.as_str() {
        "CIG1" => HatchPattern::Cig1,
        "CIG2" => HatchPattern::Cig2,
        "CIG3" => HatchPattern::Cig3,
        _ => HatchPattern::None,
    };

    // CIG areas: use transparent fill + hatching; regular areas: semi-transparent fill
    let fill_alpha = if hatch != HatchPattern::None { CIG_FILL_ALPHA } else { REGULAR_FILL_ALPHA };
    let fill_rgba = super::colors::parse_hex_color(fill_hex, fill_alpha);
    let stroke_rgba = super::colors::parse_hex_color(stroke_hex, 255);

    let valid = properties
        .get("VALID")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M").ok());
    let expire = properties
        .get("EXPIRE")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M").ok());

    // Parse geometry
    let geometry = feature_val
        .get("geometry")
        .ok_or_else(|| "Feature missing 'geometry'".to_string())?;

    let geo_type = geometry
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut polygons = match geo_type {
        "MultiPolygon" => parse_multi_polygon(geometry)?,
        "Polygon" => vec![parse_polygon(geometry)?],
        "GeometryCollection" => {
            let geometries = geometry
                .get("geometries")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if geometries == 0 {
                return Ok(None);
            }
            log::warn!("Non-empty GeometryCollection not supported, skipping");
            return Ok(None);
        }
        other => {
            log::warn!("Skipping unsupported geometry type: {}", other);
            return Ok(None);
        }
    };

    crate::render::geo::simplify_polygons(&mut polygons, crate::types::SIMPLIFY_EPSILON);
    if polygons.is_empty() {
        return Ok(None);
    }

    let feature = OverlayFeature::new(polygons, fill_rgba, stroke_rgba, label, label2, hatch);
    Ok(Some(ParsedOutlookFeature { feature, valid, expire }))
}

/// Parse a GeoJSON MultiPolygon geometry into our polygon type.
/// GeoJSON coordinates are [longitude, latitude] — we convert to (lat, lon).
fn parse_multi_polygon(geometry: &serde_json::Value) -> Result<Vec<GeoPolygon>, String> {
    let coords = geometry
        .get("coordinates")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "MultiPolygon missing 'coordinates'".to_string())?;

    let mut polygons = Vec::new();
    for polygon_coords in coords {
        let poly = crate::types::parse_polygon_coords(polygon_coords)
            .ok_or_else(|| "Invalid polygon coordinates".to_string())?;
        polygons.push(poly);
    }
    Ok(polygons)
}

/// Parse a GeoJSON Polygon geometry.
fn parse_polygon(geometry: &serde_json::Value) -> Result<GeoPolygon, String> {
    let coords = geometry
        .get("coordinates")
        .ok_or_else(|| "Polygon missing 'coordinates'".to_string())?;
    crate::types::parse_polygon_coords(coords)
        .ok_or_else(|| "Invalid polygon coordinates".to_string())
}
