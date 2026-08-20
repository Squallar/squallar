use std::sync::Arc;

use super::colors::alert_color;
use crate::types::{HatchPattern, OverlayFeature};
use rustdar_geo::GeoPolygon;

/// Not an NWS field: derived from the `event` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AlertCategory {
    Warning,
    Watch,
    Advisory,
    Other,
}

impl AlertCategory {
    /// **Every category, and the only enumeration of them.** The default
    /// enabled set, the per-category toggles, the toggle handler and the
    /// status line are all built by walking this, so a variant cannot exist in
    /// the enum and be missing from the set that decides whether it paints.
    ///
    /// Order is the order the UI offers them, most severe first.
    pub const ALL: [AlertCategory; 4] = [
        AlertCategory::Warning,
        AlertCategory::Watch,
        AlertCategory::Advisory,
        AlertCategory::Other,
    ];

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

    /// The toggle's stable id. Persisted in saved control state, so these
    /// strings are a compatibility surface and must not be respelled.
    pub fn control_id(self) -> &'static str {
        match self {
            AlertCategory::Warning => "warnings",
            AlertCategory::Watch => "watches",
            AlertCategory::Advisory => "advisories",
            AlertCategory::Other => "other",
        }
    }

    /// Inverse of [`control_id`](AlertCategory::control_id); `None` for a
    /// control that is not a category toggle at all.
    pub fn from_control_id(id: &str) -> Option<Self> {
        AlertCategory::ALL
            .into_iter()
            .find(|category| category.control_id() == id)
    }

    /// The toggle's label. Not `display_name` plus an "s": the plurals are
    /// irregular and "Other" does not take one.
    pub fn plural_label(self) -> &'static str {
        match self {
            AlertCategory::Warning => "Warnings",
            AlertCategory::Watch => "Watches",
            AlertCategory::Advisory => "Advisories",
            AlertCategory::Other => "Other",
        }
    }

    /// The status line's abbreviation, e.g. the `W/Wa/Adv/Oth` in
    /// `"3 shown - W/Wa/Adv/Oth"`.
    pub fn short_name(self) -> &'static str {
        match self {
            AlertCategory::Warning => "W",
            AlertCategory::Watch => "Wa",
            AlertCategory::Advisory => "Adv",
            AlertCategory::Other => "Oth",
        }
    }
}

impl std::fmt::Display for AlertCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Fieldless enum whose `FromStr` is infallible: unrecognised strings become
/// `Unknown`, so an added NWS value never drops an alert.
macro_rules! str_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant,)+ Unknown }

        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(match s {
                    $(stringify!($variant) => Self::$variant,)+
                    _ => Self::Unknown,
                })
            }
        }
    };
}

str_enum!(AlertSeverity {
    Extreme,
    Severe,
    Moderate,
    Minor
});
str_enum!(AlertUrgency {
    Immediate,
    Expected,
    Future,
    Past
});
str_enum!(AlertCertainty {
    Observed,
    Likely,
    Possible,
    Unlikely
});

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
    pub affected_zones: Vec<String>,
    /// Empty until `zones::resolve_zone_geometries` runs, for zone-based alerts.
    ///
    /// Behind an `Arc` so a paint snapshot
    /// ([`AlertPaint`](crate::render::rasterize::AlertPaint)) is a refcount
    /// bump, not a copy of the geometry — the national feed is thousands of
    /// rings on an active day, and the snapshot is taken per raster dispatch
    /// on the frame thread. Comparison stays value-based: `Arc` derefs
    /// through `==`.
    pub features: Arc<Vec<OverlayFeature>>,
}

/// A GeoJSON `FeatureCollection` from `api.weather.gov/alerts/active`. Many
/// alerts (watches especially) carry `"geometry": null` and only
/// `affectedZones` URLs; those are kept with no features for `zones` to fill in.
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

        let polygons = parse_geometry(feature.get("geometry")).unwrap_or_default();
        let has_geometry = !polygons.is_empty();

        let affected_zones = parse_affected_zones(props);

        // Neither geometry nor zone references: nothing to render.
        if !has_geometry && affected_zones.is_empty() {
            continue;
        }

        let (fill_rgba, stroke_rgba) = alert_color(&event);

        let category = AlertCategory::from_event(&event);

        let features = if has_geometry {
            Arc::new(vec![OverlayFeature::new(
                polygons,
                fill_rgba,
                stroke_rgba,
                event.clone(),
                opt_str_field(props, "headline").unwrap_or_default(),
                HatchPattern::None,
            )])
        } else {
            Arc::new(Vec::new()) // Filled in by zone geometry resolution.
        };

        alerts.push(NwsAlert {
            id: str_field(props, "id"),
            event,
            category,
            severity: props
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .parse()
                .unwrap(),
            urgency: props
                .get("urgency")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .parse()
                .unwrap(),
            certainty: props
                .get("certainty")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .parse()
                .unwrap(),
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

/// GeoJSON permits a `GeometryCollection` to contain another one. Nothing the
/// NWS publishes nests at all — every one of the 227 collections in the zone
/// corpus is a flat list of `Polygon` and `MultiPolygon` — so this exists only
/// so that a hand-rolled or hostile document cannot recurse without bound.
const MAX_GEOMETRY_NESTING: u32 = 8;

/// `None` for null or unsupported geometry.
pub(crate) fn parse_geometry(geom: Option<&serde_json::Value>) -> Option<Vec<GeoPolygon>> {
    parse_geometry_at(geom, MAX_GEOMETRY_NESTING)
}

/// The three geometry types the NWS actually serves, `depth` guarding the one
/// that contains the other two.
fn parse_geometry_at(geom: Option<&serde_json::Value>, depth: u32) -> Option<Vec<GeoPolygon>> {
    let geom = geom?.as_object()?;
    let geom_type = geom.get("type")?.as_str()?;

    if geom_type == "GeometryCollection" {
        if depth == 0 {
            log::debug!("NWS geometry nests deeper than {MAX_GEOMETRY_NESTING}; giving up");
            return None;
        }
        let members = geom.get("geometries")?.as_array()?;
        // Flattened, not kept as a tree: a collection of a Polygon and a
        // MultiPolygon means the same thing to this renderer as the one
        // MultiPolygon holding all of their parts.
        let polys: Vec<GeoPolygon> = members
            .iter()
            .filter_map(|member| parse_geometry_at(Some(member), depth - 1))
            .flatten()
            .collect();
        return if polys.is_empty() { None } else { Some(polys) };
    }

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
                .filter_map(crate::types::parse_polygon_coords)
                .collect();
            if polys.is_empty() { None } else { Some(polys) }
        }
        _ => {
            log::debug!("Unsupported NWS geometry type: {}", geom_type);
            None
        }
    }
}

fn str_field(obj: &serde_json::Value, key: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_str_field(obj: &serde_json::Value, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

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
