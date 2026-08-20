use crate::types::{HatchPattern, OverlayFeature, REGULAR_FILL_ALPHA, SIGNIFICANT_FILL_ALPHA};
use chrono::NaiveDateTime;
use rustdar_geo::GeoPolygon;

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
                OutlookProduct::ConditionalIntensity,
            ],
            _ => &[OutlookProduct::Probabilistic],
        }
    }

    /// Days 4-8: a separate endpoint with one product. See [`outlook_url`].
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OutlookProduct {
    Categorical,
    Tornado,
    Wind,
    Hail,
    /// Combined probabilistic product. Day 3 and days 4-8 only; days 1-2 carry
    /// the four hazard-specific products instead.
    Probabilistic,
    /// Day 3's significant-severe area — the Conditional Intensity Groups,
    /// `day3otlk_cigprob`. **Day 3 only**, and never user-selectable on its own:
    /// see [`OutlookProduct::implied_by`].
    ///
    /// Days 1-2 need no equivalent. There the CIG features are carried inline
    /// in the hazard products, confirmed over 1023 archived `.lyr.geojson`
    /// products and in SPC's own reference bundle. Day 3 has always served its
    /// significant area separately — a hole that predates SCN 26-11, not a
    /// regression the notice caused.
    ///
    /// The obvious sibling, `day3otlk_sigprob`, still answers `200` with a real
    /// `SIGN` MultiPolygon — and has not been re-issued since **2026-03-03
    /// 08:30Z**, the day after SCN 26-11 took effect, while `_cat`, `_prob` and
    /// `_cigprob` all re-issue with the outlook. Fetching it would paint a
    /// months-old hazard area, with its own stale `VALID`/`EXPIRE` to make it
    /// read as current. It is a dead file; this is its live successor. Because
    /// only one of the two is still written, `SIGN` and `CIG*` cannot both
    /// arrive for the same live Day 3.
    ConditionalIntensity,
}

impl OutlookProduct {
    /// The product whose toggle also governs this one, if this product is not
    /// independently selectable.
    pub fn implied_by(self) -> Option<OutlookProduct> {
        match self {
            OutlookProduct::ConditionalIntensity => Some(OutlookProduct::Probabilistic),
            _ => None,
        }
    }

    /// Whether the user picks this product directly. The inverse of
    /// [`implied_by`](Self::implied_by) being set.
    pub fn is_selectable(self) -> bool {
        self.implied_by().is_none()
    }
}

impl std::fmt::Display for OutlookProduct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutlookProduct::Categorical => write!(f, "Categorical"),
            OutlookProduct::Tornado => write!(f, "Tornado"),
            OutlookProduct::Wind => write!(f, "Wind"),
            OutlookProduct::Hail => write!(f, "Hail"),
            OutlookProduct::Probabilistic => write!(f, "Probabilistic"),
            OutlookProduct::ConditionalIntensity => write!(f, "Conditional Intensity"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpcOutlook {
    pub day: OutlookDay,
    pub product: OutlookProduct,
    pub valid: Option<NaiveDateTime>,
    pub expire: Option<NaiveDateTime>,
    pub features: Vec<OverlayFeature>,
}

/// Origin must come from
/// [`DataSources::spc_base`](rustdar_source::origins::DataSources::spc_base),
/// never a literal, or SPC escapes the origin table's browser-reachability check.
///
/// SPC publishes each outlook twice. The `.lyr` ("layered") form gives every
/// risk category as a whole nested region — MRGL ⊃ SLGT ⊃ ENH ⊃ MDT — so each
/// higher category is drawn again underneath every lower one, and under a
/// semi-transparent fill that accumulates: the deepest category is painted as
/// many times as there are categories above it. The `.nolyr` form gives the
/// same areas as disjoint bands, each a donut with the next category up cut out
/// as an interior ring, painting every pixel once.
pub fn outlook_url(
    sources: &rustdar_source::origins::DataSources,
    day: OutlookDay,
    product: OutlookProduct,
) -> String {
    let base = &sources.spc_base;
    // Days 4-8 live under a separate extended-range path.
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
        // Day 3's significant-severe area, which `_prob` does not carry. Only
        // `OutlookDay::Day3` lists this product, so no other day reaches here.
        (_, OutlookProduct::ConditionalIntensity) => "_cigprob",
        // Day 3 serves every hazard from the one combined `_prob` endpoint.
        (_, OutlookProduct::Tornado)
        | (_, OutlookProduct::Wind)
        | (_, OutlookProduct::Hail)
        | (_, OutlookProduct::Probabilistic) => "_prob",
    };

    format!("{base}/products/outlook/{day_str}{product_str}.lyr.geojson")
}

/// Feed shape — property names and casing are SPC's:
/// ```json
/// { "features": [ {
///     "geometry": { "type": "MultiPolygon", "coordinates": [[[[lon, lat], ...]]] },
///     "properties": {
///       "LABEL": "SLGT", "LABEL2": "Slight Risk",
///       "fill": "#FFE066", "stroke": "#DDAA00",
///       "VALID": "202603062000", "EXPIRE": "202603071200" } } ] }
/// ```
/// `VALID`/`EXPIRE` are `%Y%m%d%H%M`, no zone marker.
pub fn parse_geojson(
    json: &serde_json::Value,
    day: OutlookDay,
    product: OutlookProduct,
) -> Result<SpcOutlook, String> {
    let features_array = json
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'features' array in GeoJSON".to_string())?;

    // Two tiers, concatenated at the end. `features` is drawn in list order
    // (`rasterize::rasterize_spc_outlooks`), so this is the paint order.
    let mut risk = Vec::new();
    let mut overlays = Vec::new();
    let mut valid: Option<NaiveDateTime> = None;
    let mut expire: Option<NaiveDateTime> = None;

    for feature_val in features_array {
        // One malformed feature must not blank the whole product: SPC has
        // shipped single degenerate polygons (every ring under 3 points), and
        // propagating the error here turned one of them into an empty outlook.
        let ParsedOutlookFeature {
            feature,
            layer,
            valid: feat_valid,
            expire: feat_expire,
        } = match parse_outlook_feature(feature_val) {
            Ok(Some(result)) => result,
            Ok(None) => continue,
            Err(e) => {
                log::warn!("SPC {day} {product}: skipping malformed feature: {e}");
                continue;
            }
        };
        if valid.is_none() {
            valid = feat_valid;
        }
        if expire.is_none() {
            expire = feat_expire;
        }
        if layer.is_overlay() {
            overlays.push(feature);
        } else {
            risk.push(feature);
        }
    }

    // Stable within each tier, so SPC's ascending publication order — which
    // is what puts the higher category on top — is preserved.
    risk.extend(overlays);

    Ok(SpcOutlook {
        day,
        product,
        valid,
        expire,
        features: risk,
    })
}

struct ParsedOutlookFeature {
    feature: OverlayFeature,
    layer: OutlookLayer,
    valid: Option<NaiveDateTime>,
    expire: Option<NaiveDateTime>,
}

/// SPC's reserved fill for the significant-severe area.
///
/// Every `SIGN` feature SPC published between 2020 and 2025 and every `CIG*`
/// feature it has published since carries exactly this fill on a black
/// stroke, and no risk-category or probability feature carries it — checked
/// over the 121 significant-severe features in 600 archived and live outlook
/// products. It is the second, independent signal [`OutlookLayer::of`] reads,
/// so that identifying this layer does not rest on the label vocabulary alone.
const SIGNIFICANT_SEVERE_FILL: &str = "#888888";

/// What SPC's `LABEL` says a feature is: part of the risk stack, or the
/// significant-severe area laid over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutlookLayer {
    /// A risk category (`TSTM`, `MRGL` … `HIGH`) or probability contour
    /// (`0.02` … `0.60`): the stack, painted in the feed's ascending order.
    Risk,
    /// The significant-severe area laid over that stack.
    Significant(HatchPattern),
}

impl OutlookLayer {
    /// Classified from the label first, and from SPC's reserved fill as a
    /// fallback — two independent signals, because the label vocabulary has
    /// already changed once and will again.
    fn of(label: &str, fill_hex: &str) -> Self {
        let hatch = match label {
            // SPC's Conditional Intensity Groups, live since 2026-03-02.
            "CIG1" => HatchPattern::Cig1,
            "CIG2" => HatchPattern::Cig2,
            "CIG3" => HatchPattern::Cig3,
            // What SPC labelled the single significant-severe area until NWS
            // Service Change Notice 26-11 replaced it with the three CIG
            // levels ("labeled 'CIG1', 'CIG2', and 'CIG3' replacing the
            // current 'SIGN' label"). Archived outlooks and third-party
            // mirrors still carry it, and it is mapped to the lowest level
            // that replaced it because that is the area it drew.
            "SIGN" => HatchPattern::Cig1,
            _ => HatchPattern::None,
        };
        if hatch != HatchPattern::None {
            return OutlookLayer::Significant(hatch);
        }
        if fill_hex.eq_ignore_ascii_case(SIGNIFICANT_SEVERE_FILL) {
            log::warn!(
                "SPC significant-severe label {label:?} is not one this build \
                 knows; drawing it as an unhatched overlay rather than over \
                 the probabilities"
            );
            return OutlookLayer::Significant(HatchPattern::None);
        }
        OutlookLayer::Risk
    }

    fn hatch(self) -> HatchPattern {
        match self {
            OutlookLayer::Risk => HatchPattern::None,
            OutlookLayer::Significant(hatch) => hatch,
        }
    }

    /// Never derived from whether [`hatch`](OutlookLayer::hatch) came back
    /// `None`: that is the coupling this type exists to break.
    fn fill_alpha(self) -> u8 {
        match self {
            OutlookLayer::Risk => REGULAR_FILL_ALPHA,
            OutlookLayer::Significant(_) => SIGNIFICANT_FILL_ALPHA,
        }
    }

    /// Overlays are drawn after the whole risk stack — see [`parse_geojson`].
    fn is_overlay(self) -> bool {
        matches!(self, OutlookLayer::Significant(_))
    }
}

/// `None` means skip: empty geometry or an unsupported geometry type.
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

    // Not `#888888`: that is [`SIGNIFICANT_SEVERE_FILL`], the sentinel
    // [`OutlookLayer::of`] reads as "this feature *is* the significant-severe
    // layer". Defaulting a *missing* fill to it made the fallback colour and
    // the classifier's sentinel the same constant, so a risk feature arriving
    // without a `fill` was lifted out of the risk stack and drawn over the
    // whole ladder. Empty is the honest default, and is already the value SPC
    // itself publishes on features it draws no geometry for.
    let fill_hex = properties
        .get("fill")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let stroke_hex = properties
        .get("stroke")
        .and_then(|v| v.as_str())
        .unwrap_or("#000000");

    // There is no dedicated field for any of this: the layer is read off the
    // LABEL text, with SPC's reserved fill as a second signal.
    let layer = OutlookLayer::of(&label, fill_hex);
    let hatch = layer.hatch();
    let fill_rgba = super::colors::parse_hex_color(fill_hex, layer.fill_alpha());
    let stroke_rgba = super::colors::parse_hex_color(stroke_hex, 255);

    let valid = properties
        .get("VALID")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M").ok());
    let expire = properties
        .get("EXPIRE")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M").ok());

    let geometry = feature_val
        .get("geometry")
        .ok_or_else(|| "Feature missing 'geometry'".to_string())?;

    let geo_type = geometry.get("type").and_then(|v| v.as_str()).unwrap_or("");

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
    Ok(Some(ParsedOutlookFeature {
        feature,
        layer,
        valid,
        expire,
    }))
}

/// GeoJSON is `[lon, lat]`; output is `(lat, lon)`.
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

fn parse_polygon(geometry: &serde_json::Value) -> Result<GeoPolygon, String> {
    let coords = geometry
        .get("coordinates")
        .ok_or_else(|| "Polygon missing 'coordinates'".to_string())?;
    crate::types::parse_polygon_coords(coords)
        .ok_or_else(|| "Invalid polygon coordinates".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(label: &str, rings: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "properties": {
                "LABEL": label,
                "LABEL2": "Slight Risk",
                "fill": "#FFE066",
                "stroke": "#DDAA00",
                "VALID": "202603062000",
                "EXPIRE": "202603071200"
            },
            "geometry": { "type": "MultiPolygon", "coordinates": rings }
        })
    }

    #[test]
    fn a_degenerate_feature_is_skipped_and_the_good_ones_survive() {
        let degenerate = feature(
            "MRGL",
            serde_json::json!([[[[-100.0, 30.0], [-99.0, 30.0]]]]),
        );
        let good = feature(
            "SLGT",
            serde_json::json!([[[
                [-100.0, 30.0],
                [-95.0, 30.0],
                [-95.0, 35.0],
                [-100.0, 35.0],
                [-100.0, 30.0]
            ]]]),
        );
        let json = serde_json::json!({ "features": [degenerate, good] });

        let outlook = parse_geojson(&json, OutlookDay::Day1, OutlookProduct::Categorical)
            .expect("one bad feature must not abort the whole outlook");

        assert_eq!(
            outlook.features.len(),
            1,
            "the good feature survives, the bad one is dropped"
        );
        assert_eq!(outlook.features[0].label, "SLGT");
        assert!(outlook.valid.is_some() && outlook.expire.is_some());
    }

    // ── SPC's significant-severe area ─────────────────────────────────────
    //
    // Both fixtures are SPC's own bytes, saved unmodified from the outlook
    // archive, so nothing below is checked against a table of ours:
    //
    //   spc.noaa.gov/products/outlook/archive/2026/day1otlk_20260425_1300_hail.lyr.geojson
    //   spc.noaa.gov/products/outlook/archive/2024/day1otlk_20240506_1300_torn.lyr.geojson

    /// SPC's hail outlook for 2026-04-25 13Z, verbatim.
    const CIG_PRODUCT: &str =
        include_str!("../../testdata/day1otlk_20260425_1300_hail.lyr.geojson");

    /// SPC's tornado outlook for 2024-05-06 13Z, verbatim — the retired
    /// vocabulary.
    const SIGN_PRODUCT: &str =
        include_str!("../../testdata/day1otlk_20240506_1300_torn.lyr.geojson");

    fn parse(raw: &str, product: OutlookProduct) -> SpcOutlook {
        let json: serde_json::Value =
            serde_json::from_str(raw).expect("the fixture is SPC's own JSON");
        parse_geojson(&json, OutlookDay::Day1, product).expect("a real product must parse")
    }

    fn labels(outlook: &SpcOutlook) -> Vec<&str> {
        outlook.features.iter().map(|f| f.label.as_str()).collect()
    }

    #[test]
    fn the_conditional_intensity_groups_hatch_over_the_probability_ladder() {
        let outlook = parse(CIG_PRODUCT, OutlookProduct::Hail);
        assert_eq!(
            labels(&outlook),
            ["0.05", "0.15", "0.30", "0.45", "CIG1", "CIG2"],
            "fixture: SPC's own hail ladder with two intensity groups over it",
        );

        for feature in &outlook.features {
            let significant = feature.label.starts_with("CIG");
            if significant {
                assert_ne!(
                    feature.hatch,
                    HatchPattern::None,
                    "{} is a significant-severe area and must be hatched",
                    feature.label,
                );
                assert_eq!(
                    feature.fill_rgba[3], SIGNIFICANT_FILL_ALPHA,
                    "{} must not bury the contours it qualifies",
                    feature.label,
                );
            } else {
                assert_eq!(feature.hatch, HatchPattern::None);
                assert_eq!(feature.fill_rgba[3], REGULAR_FILL_ALPHA);
            }
        }
        assert_eq!(outlook.features[4].hatch, HatchPattern::Cig1);
        assert_eq!(outlook.features[5].hatch, HatchPattern::Cig2);
    }

    #[test]
    fn the_retired_sign_label_is_still_the_significant_severe_area() {
        let outlook = parse(SIGN_PRODUCT, OutlookProduct::Tornado);
        assert_eq!(
            labels(&outlook),
            ["0.02", "0.05", "0.10", "0.15", "0.30", "SIGN"],
            "fixture: SPC's own tornado ladder with the significant area over it",
        );

        let sign = outlook.features.last().expect("the fixture has features");
        assert_eq!(sign.label, "SIGN");
        assert_eq!(
            sign.hatch,
            HatchPattern::Cig1,
            "SIGN is the area the lowest intensity group replaced",
        );
        assert_eq!(
            sign.fill_rgba[3], SIGNIFICANT_FILL_ALPHA,
            "SIGN painted at the regular alpha is an opaque grey blob over the \
             30% tornado contour",
        );
    }

    #[test]
    fn an_unrecognised_significant_severe_label_is_not_painted_over_the_forecast() {
        let mut json: serde_json::Value =
            serde_json::from_str(CIG_PRODUCT).expect("the fixture is SPC's own JSON");
        let features = json["features"].as_array_mut().expect("a feature array");
        let mut renamed = features.pop().expect("the significant area is last");
        assert_eq!(renamed["properties"]["LABEL"], "CIG2", "fixture premise");
        renamed["properties"]["LABEL"] = serde_json::json!("CIG4");
        features.insert(0, renamed);

        let outlook = parse_geojson(&json, OutlookDay::Day1, OutlookProduct::Hail)
            .expect("an unknown label must not fail the product");
        let unknown = outlook
            .features
            .iter()
            .find(|f| f.label == "CIG4")
            .expect("the feature is kept, not dropped");

        assert_eq!(
            unknown.fill_rgba[3], SIGNIFICANT_FILL_ALPHA,
            "an unrecognised significant-severe label was promoted to the \
             regular fill and buried the probabilities under it",
        );
        assert_eq!(
            unknown.hatch,
            HatchPattern::None,
            "we cannot invent a hatch level for a label we do not know",
        );
        assert_eq!(
            labels(&outlook),
            ["0.05", "0.15", "0.30", "0.45", "CIG4", "CIG1"],
            "the unknown area is drawn after the whole probability ladder \
             because it is an overlay, having been moved to the *front* of \
             SPC's array to prove position is not what decides — and the \
             overlay tier keeps the feed's own order within itself",
        );
    }

    #[test]
    fn a_probability_contour_with_no_fill_property_is_not_promoted_to_an_overlay() {
        let mut json: serde_json::Value =
            serde_json::from_str(CIG_PRODUCT).expect("the fixture is SPC's own JSON");
        let features = json["features"].as_array_mut().expect("a feature array");
        let stripped = features
            .iter_mut()
            .find(|f| f["properties"]["LABEL"] == "0.30")
            .expect("fixture premise: the hail ladder carries a 30% band");
        stripped["properties"]
            .as_object_mut()
            .expect("properties is an object")
            .remove("fill")
            .expect("fixture premise: it had a fill to remove");

        let outlook = parse_geojson(&json, OutlookDay::Day1, OutlookProduct::Hail)
            .expect("a missing fill must not fail the product");
        let band = outlook
            .features
            .iter()
            .find(|f| f.label == "0.30")
            .expect("the band is kept, not dropped");

        assert_eq!(
            band.fill_rgba[3], REGULAR_FILL_ALPHA,
            "a probability band with no fill is still a probability band",
        );
        assert_eq!(band.hatch, HatchPattern::None);
        assert_eq!(
            labels(&outlook),
            ["0.05", "0.15", "0.30", "0.45", "CIG1", "CIG2"],
            "it keeps its place in the ladder rather than being drawn last \
             over the contours above it",
        );
    }

    #[test]
    fn the_probability_contours_keep_the_regular_fill_and_the_feeds_order() {
        let outlook = parse(CIG_PRODUCT, OutlookProduct::Hail);
        let ladder: Vec<&str> = labels(&outlook)
            .into_iter()
            .take_while(|l| !l.starts_with("CIG"))
            .collect();
        assert_eq!(
            ladder,
            ["0.05", "0.15", "0.30", "0.45"],
            "SPC publishes ascending, and drawing in that order is what puts \
             the higher probability on top",
        );
        for feature in outlook.features.iter().take(ladder.len()) {
            assert_eq!(feature.fill_rgba[3], REGULAR_FILL_ALPHA);
        }
    }

    #[test]
    fn a_missing_features_array_is_still_a_hard_error() {
        let json = serde_json::json!({ "type": "FeatureCollection" });
        assert!(
            parse_geojson(&json, OutlookDay::Day1, OutlookProduct::Categorical).is_err(),
            "an envelope without features is a broken product, not an empty one"
        );
    }
}
