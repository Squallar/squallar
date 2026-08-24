use std::sync::Arc;

use super::colors::alert_color;
use crate::types::{HatchPattern, OverlayFeature};
use squallar_geo::GeoPolygon;

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
    /// **Parsed at parse time, never at raster time**
    /// ([`parse_valid_window`]): the instant this alert starts being true,
    /// `None` when neither `onset` nor `effective` parses.
    ///
    /// `None` means **always valid on that side** — an alert is never dropped
    /// for want of a readable time. The four strings above stay display truth;
    /// these two exist only so the as-of filter is two `Option` comparisons
    /// and not a per-raster parse of hundreds of RFC 3339 strings.
    pub valid_from: Option<chrono::NaiveDateTime>,
    /// The instant it stops being true, exclusive; `None` when neither `ends`
    /// nor `expires` parses. See [`Self::valid_from`].
    pub valid_until: Option<chrono::NaiveDateTime>,
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

/// One CAP timestamp, tolerantly. NWS emits local offsets (`-05:00`) as often
/// as `Z`, so this is [`chrono::DateTime::parse_from_rfc3339`] — which handles
/// both and normalises to UTC — never a hand-rolled format string.
///
/// Absent or unparseable is `None`, which the filter reads as "no bound on
/// that side". A bad timestamp must never cost an alert its pixels.
fn parse_cap_time(s: &str) -> Option<chrono::NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.naive_utc())
        .ok()
}

/// **The validity window of one alert, from all four of its time strings.**
///
/// `onset` before `effective` and `ends` before `expires`, because the pair
/// that describes the **event** outranks the pair that describes the
/// **message**: `effective`/`expires` are when the bulletin was issued and
/// when it goes stale, `onset`/`ends` are when the weather starts and stops.
/// A watch issued at 14:00 for 18:00–22:00 is not depicted at 15:00.
///
/// Either side may be `None` — unbounded there. Called from
/// [`parse_alerts`] and nowhere on a rasterize path.
pub fn parse_valid_window(
    effective: &str,
    expires: &str,
    onset: Option<&str>,
    ends: Option<&str>,
) -> (Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>) {
    let from = onset
        .and_then(parse_cap_time)
        .or_else(|| parse_cap_time(effective));
    let until = ends
        .and_then(parse_cap_time)
        .or_else(|| parse_cap_time(expires));
    (from, until)
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
    let (mut unparsed_from, mut unparsed_until) = (0usize, 0usize);

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

        // Parsed HERE, once per alert per fetch — never inside the as-of
        // filter, which runs once per alert per rasterize.
        let effective = str_field(props, "effective");
        let expires = str_field(props, "expires");
        let onset = opt_str_field(props, "onset");
        let ends = opt_str_field(props, "ends");
        let (valid_from, valid_until) =
            parse_valid_window(&effective, &expires, onset.as_deref(), ends.as_deref());
        if valid_from.is_none() {
            unparsed_from += 1;
        }
        if valid_until.is_none() {
            unparsed_until += 1;
        }

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
            effective,
            expires,
            onset,
            ends,
            valid_from,
            valid_until,
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
    if unparsed_from > 0 || unparsed_until > 0 {
        // Debug, not a warning: an unreadable bound is treated as no bound, so
        // nothing is lost and there is nothing the reader could act on.
        log::debug!(
            "{unparsed_from} alert(s) with no readable start and {unparsed_until} \
             with no readable end - unbounded on that side",
        );
    }
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

#[cfg(test)]
mod valid_window_tests {
    use super::*;

    fn t(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    /// One alert as the API serves one, with the four time strings under test
    /// and enough geometry that the parser admits it.
    fn feed(
        effective: &str,
        expires: &str,
        onset: Option<&str>,
        ends: Option<&str>,
    ) -> Vec<NwsAlert> {
        let mut props = serde_json::json!({
            "event": "Tornado Warning",
            "severity": "Severe",
            "urgency": "Immediate",
            "certainty": "Observed",
            "effective": effective,
            "expires": expires,
        });
        if let Some(onset) = onset {
            props["onset"] = serde_json::json!(onset);
        }
        if let Some(ends) = ends {
            props["ends"] = serde_json::json!(ends);
        }
        parse_alerts(&serde_json::json!({
            "features": [{
                "properties": props,
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[[-97.5, 35.0], [-97.5, 36.0], [-96.5, 36.0], [-97.5, 35.0]]],
                },
            }]
        }))
    }

    /// **The event's own pair outranks the message's pair**, and it is the
    /// parser — not a fixture — that says so. A watch issued at 14:00 for an
    /// 18:00-22:00 event is not depicted at 15:00, so `onset` beats
    /// `effective` and `ends` beats `expires`. Four distinct instants, so a
    /// wrong preference cannot land on the right answer by coincidence.
    #[test]
    fn the_events_own_times_outrank_the_messages_times() {
        let alerts = feed(
            "2026-08-20T14:00:00Z",
            "2026-08-20T23:00:00Z",
            Some("2026-08-20T18:00:00Z"),
            Some("2026-08-20T22:00:00Z"),
        );
        let alert = alerts.first().expect("the parser admits a polygon alert");
        assert_eq!(
            alert.valid_from,
            Some(t(2026, 8, 20, 18, 0)),
            "valid_from took `effective` (14:00) where `onset` (18:00) is present",
        );
        assert_eq!(
            alert.valid_until,
            Some(t(2026, 8, 20, 22, 0)),
            "valid_until took `expires` (23:00) where `ends` (22:00) is present",
        );
    }

    /// `ends` is real and is the field a three-field reading of this struct
    /// misses. Without `onset`/`ends` the message pair is all there is.
    #[test]
    fn the_message_times_are_the_fallback_when_the_event_names_none() {
        let alerts = feed("2026-08-20T14:00:00Z", "2026-08-20T23:00:00Z", None, None);
        let alert = alerts.first().expect("the parser admits a polygon alert");
        assert_eq!(alert.valid_from, Some(t(2026, 8, 20, 14, 0)));
        assert_eq!(alert.valid_until, Some(t(2026, 8, 20, 23, 0)));
    }

    /// **A local offset is not an error.** The NWS publishes `-05:00` at least
    /// as often as `Z`, and `parse_from_rfc3339` normalises it — a hand-rolled
    /// format string would drop these alerts off the map.
    #[test]
    fn a_local_offset_parses_and_normalises_to_utc() {
        let alerts = feed(
            "2026-08-20T09:00:00-05:00",
            "2026-08-20T18:00:00-05:00",
            None,
            None,
        );
        let alert = alerts.first().expect("the parser admits a polygon alert");
        assert_eq!(
            alert.valid_from,
            Some(t(2026, 8, 20, 14, 0)),
            "09:00 at -05:00 is 14:00 UTC",
        );
        assert_eq!(alert.valid_until, Some(t(2026, 8, 20, 23, 0)));
    }

    /// **An unreadable time costs an alert nothing.** All four garbage means
    /// unbounded on both sides, which the filter reads as always valid — never
    /// a dropped alert, and never a dropped *field*: the strings survive for
    /// the popup to format.
    #[test]
    fn four_unreadable_times_leave_the_alert_unbounded_and_its_strings_intact() {
        let alerts = feed("soon", "later", Some("whenever"), Some("eventually"));
        let alert = alerts.first().expect("the parser admits a polygon alert");
        assert_eq!(alert.valid_from, None, "unparseable is unbounded, not zero");
        assert_eq!(alert.valid_until, None);
        assert_eq!(alert.effective, "soon", "the display string is untouched");
        assert_eq!(alert.expires, "later");
        assert_eq!(alert.onset.as_deref(), Some("whenever"));
        assert_eq!(alert.ends.as_deref(), Some("eventually"));
    }

    /// A missing pair is the same as an unreadable one, and the alert still
    /// parses rather than being skipped.
    #[test]
    fn absent_times_are_unbounded_too() {
        let alerts = feed("", "", None, None);
        let alert = alerts.first().expect("the parser admits a polygon alert");
        assert_eq!(alert.valid_from, None);
        assert_eq!(alert.valid_until, None);
    }
}
