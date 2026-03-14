use crate::types::{GeoPolygon, HatchPattern, OverlayFeature};
use super::colors::alert_color;

/// Broad classification of an NWS alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertCategory {
    Warning,
    Watch,
    Advisory,
    Other,
}

impl AlertCategory {
    /// Derive category from the event name string.
    pub fn from_event(event: &str) -> Self {
        let lower = event.to_lowercase();
        if lower.contains("warning") {
            AlertCategory::Warning
        } else if lower.contains("watch") {
            AlertCategory::Watch
        } else if lower.contains("advisory")
            || lower.contains("statement")
            || lower.contains("outlook")
        {
            AlertCategory::Advisory
        } else {
            AlertCategory::Other
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            AlertCategory::Warning => "Warning",
            AlertCategory::Watch => "Watch",
            AlertCategory::Advisory => "Advisory",
            AlertCategory::Other => "Other",
        }
    }
}

impl std::fmt::Display for AlertCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Severity level from the NWS API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertSeverity {
    Extreme,
    Severe,
    Moderate,
    Minor,
    Unknown,
}

impl AlertSeverity {
    pub fn from_str(s: &str) -> Self {
        match s {
            "Extreme" => Self::Extreme,
            "Severe" => Self::Severe,
            "Moderate" => Self::Moderate,
            "Minor" => Self::Minor,
            _ => Self::Unknown,
        }
    }
}

/// Urgency level from the NWS API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertUrgency {
    Immediate,
    Expected,
    Future,
    Past,
    Unknown,
}

impl AlertUrgency {
    pub fn from_str(s: &str) -> Self {
        match s {
            "Immediate" => Self::Immediate,
            "Expected" => Self::Expected,
            "Future" => Self::Future,
            "Past" => Self::Past,
            _ => Self::Unknown,
        }
    }
}

/// Certainty level from the NWS API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertCertainty {
    Observed,
    Likely,
    Possible,
    Unlikely,
    Unknown,
}

impl AlertCertainty {
    pub fn from_str(s: &str) -> Self {
        match s {
            "Observed" => Self::Observed,
            "Likely" => Self::Likely,
            "Possible" => Self::Possible,
            "Unlikely" => Self::Unlikely,
            _ => Self::Unknown,
        }
    }
}

/// A single NWS weather alert with parsed metadata and renderable geometry.
#[derive(Debug, Clone)]
pub struct NwsAlert {
    pub id: String,
    pub event: String,
    pub category: AlertCategory,
    pub severity: AlertSeverity,
    pub urgency: AlertUrgency,
    pub certainty: AlertCertainty,
    pub headline: Option<String>,
    pub description: String,
    pub instruction: Option<String>,
    pub area_desc: String,
    pub sender_name: String,
    pub effective: String,
    pub expires: String,
    pub onset: Option<String>,
    pub ends: Option<String>,
    /// Zone URLs for zone-based alerts (used to resolve county geometries).
    pub affected_zones: Vec<String>,
    /// Renderable polygon features (reuses the shared OverlayFeature type).
    /// May be empty initially for zone-based alerts until zone geometries are resolved.
    pub features: Vec<OverlayFeature>,
}

/// Parse a NWS alerts GeoJSON response into a list of `NwsAlert`.
///
/// Expects a GeoJSON `FeatureCollection` from `api.weather.gov/alerts/active`.
/// Alerts with null geometry are included with empty features; their zone
/// geometries can be resolved later via `zones::resolve_zone_geometries`.
pub fn parse_alerts(json: &serde_json::Value) -> Vec<NwsAlert> {
    let Some(features) = json.get("features").and_then(|v| v.as_array()) else {
        log::warn!("NWS alerts response missing 'features' array");
        return Vec::new();
    };

    let mut alerts = Vec::with_capacity(features.len());

    for feature in features {
        let props = match feature.get("properties") {
            Some(p) => p,
            None => continue,
        };

        let event = str_field(props, "event");
        if event.is_empty() {
            continue;
        }

        // Parse geometry — may be null for zone-based alerts (watches, etc.)
        let polygons = parse_geometry(feature.get("geometry")).unwrap_or_default();
        let has_geometry = !polygons.is_empty();

        // Parse affected zones for alerts without inline geometry
        let affected_zones = parse_affected_zones(props);

        // Skip if no geometry AND no zone references — nothing to render
        if !has_geometry && affected_zones.is_empty() {
            continue;
        }

        let (fill_rgba, stroke_rgba) = alert_color(&event);

        let category = AlertCategory::from_event(&event);

        let features = if has_geometry {
            vec![OverlayFeature::new(
                polygons,
                fill_rgba,
                stroke_rgba,
                event.clone(),
                opt_str_field(props, "headline").unwrap_or_default(),
                HatchPattern::None,
            )]
        } else {
            // Will be populated later by zone geometry resolution
            Vec::new()
        };

        alerts.push(NwsAlert {
            id: str_field(props, "id"),
            event,
            category,
            severity: AlertSeverity::from_str(
                props.get("severity").and_then(|v| v.as_str()).unwrap_or("Unknown"),
            ),
            urgency: AlertUrgency::from_str(
                props.get("urgency").and_then(|v| v.as_str()).unwrap_or("Unknown"),
            ),
            certainty: AlertCertainty::from_str(
                props.get("certainty").and_then(|v| v.as_str()).unwrap_or("Unknown"),
            ),
            headline: opt_str_field(props, "headline"),
            description: str_field(props, "description"),
            instruction: opt_str_field(props, "instruction"),
            area_desc: str_field(props, "areaDesc"),
            sender_name: str_field(props, "senderName"),
            effective: str_field(props, "effective"),
            expires: str_field(props, "expires"),
            onset: opt_str_field(props, "onset"),
            ends: opt_str_field(props, "ends"),
            affected_zones,
            features,
        });
    }

    let with_geom = alerts.iter().filter(|a| !a.features.is_empty()).count();
    let zone_only = alerts.iter().filter(|a| a.features.is_empty()).count();
    log::info!(
        "Parsed {} NWS alerts ({} with geometry, {} zone-only) out of {} features",
        alerts.len(),
        with_geom,
        zone_only,
        features.len()
    );
    alerts
}

/// Parse a GeoJSON geometry object into our internal polygon representation.
/// Returns `None` if geometry is null or unsupported.
pub(crate) fn parse_geometry(geom: Option<&serde_json::Value>) -> Option<Vec<GeoPolygon>> {
    let geom = geom?.as_object()?;
    let geom_type = geom.get("type")?.as_str()?;
    let coords = geom.get("coordinates")?;

    match geom_type {
        "Polygon" => {
            let poly = crate::types::parse_polygon_coords(coords)?;
            Some(vec![poly])
        }
        "MultiPolygon" => {
            let multi = coords.as_array()?;
            let polys: Vec<GeoPolygon> = multi
                .iter()
                .filter_map(|p| crate::types::parse_polygon_coords(p))
                .collect();
            if polys.is_empty() { None } else { Some(polys) }
        }
        _ => {
            log::debug!("Unsupported NWS geometry type: {}", geom_type);
            None
        }
    }
}

/// Extract a required string field, defaulting to empty string.
fn str_field(obj: &serde_json::Value, key: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Extract an optional string field.
fn opt_str_field(obj: &serde_json::Value, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Parse `affectedZones` URLs from an alert's properties.
fn parse_affected_zones(props: &serde_json::Value) -> Vec<String> {
    props
        .get("affectedZones")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
