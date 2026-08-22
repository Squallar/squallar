//! SPC Fire Weather Outlooks — days 1-8, two hazards, one risk tier.
//!
//! # The tier question, settled 2026-08-21
//!
//! All 28 production payloads were fetched (`200` on every one) and every
//! feature's `LABEL`/`LABEL2`/`stroke`/`fill`/`DN` read off. The complete
//! distinct set was:
//!
//! | `LABEL` | `LABEL2` | `stroke` | `fill` | `DN` |
//! |---|---|---|---|---|
//! | `IDRT` | Isolated Dry Thunderstorm Risk | `#8B4726` | `#C5A393` | 5 |
//! | `SDRT` | Scattered Dry Thunderstorm Risk | `#FF0000` | `#FF8080` | 8 |
//! | `ELEV` | Elevated Fire Risk | `#FF7F00` | `#FFBF80` | 5 |
//! | `CRIT` | Critical Fire Risk | `#FF0000` | `#FF8080` | 8 |
//! | `0.10` | 10% Dry Thunder Risk | `#8B4726` | `#C5A393` | 10 |
//! | `0.40` | 40% Wind/RH Risk | `#FF7F00` | `#FFBF80` | 40 |
//! | `Predictability Too Low` | *(empty)* | *(empty)* | *(empty)* | 0 |
//! | `Probability Too Low` | *(empty)* | *(empty)* | *(empty)* | 0 |
//!
//! **One tier.** Nothing here is a second layer laid over the first: there is
//! no `SIGN`, no `CIG*`, no reserved `#888888` fill, and no feature whose `DN`
//! sits outside its product's own ladder. The convective layer's
//! [`OutlookLayer`](super::outlook) split exists because SPC publishes a
//! significant-severe area *inside* the same payload as the probability
//! ladder it qualifies; fire weather publishes no such thing on any of the 28
//! endpoints. So this module has **no hatch concept and no overlay tier** —
//! every feature is a risk feature, painted in the feed's own ascending order
//! at [`REGULAR_FILL_ALPHA`](crate::types::REGULAR_FILL_ALPHA).
//!
//! **What that reading rests on, stated honestly.** The sample is one issuance
//! (2026-08-21 21:48Z), so it does not show every category the vocabulary has
//! — `EXTM` (Extremely Critical) is a real day-1/2 Wind/RH category and was
//! not out that day. That does not weaken the tier finding: a further
//! *category* is another rung of the same ladder, not a second tier, and the
//! parse below reads colours from the payload rather than from a table of
//! ours, so an unseen category draws in SPC's own colours with no edit here.
//!
//! **The empty payload is a real state, and it is not empty.** Days 3-8
//! out of season answer `200` with exactly one feature carrying
//! `LABEL: "Predictability Too Low"` (categorical) or `"Probability Too Low"`
//! (probabilistic), no style, and `geometry: {"type": "GeometryCollection",
//! "geometries": []}`. That feature is skipped by the geometry arm below and
//! the product parses to zero features — which is the honest answer, since
//! there is nothing to draw. 22 of the 28 payloads were in that state on
//! 2026-08-21.

use chrono::NaiveDateTime;
use rustdar_geo::GeoPolygon;

use crate::types::{HatchPattern, OverlayFeature, REGULAR_FILL_ALPHA};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FireDay {
    Day1,
    Day2,
    Day3,
    Day4,
    Day5,
    Day6,
    Day7,
    Day8,
}

impl FireDay {
    pub fn all() -> &'static [FireDay] {
        &[
            FireDay::Day1,
            FireDay::Day2,
            FireDay::Day3,
            FireDay::Day4,
            FireDay::Day5,
            FireDay::Day6,
            FireDay::Day7,
            FireDay::Day8,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            FireDay::Day1 => "1",
            FireDay::Day2 => "2",
            FireDay::Day3 => "3",
            FireDay::Day4 => "4",
            FireDay::Day5 => "5",
            FireDay::Day6 => "6",
            FireDay::Day7 => "7",
            FireDay::Day8 => "8",
        }
    }

    /// The digit in the path. Kept beside [`label`](Self::label) rather than
    /// parsed back out of it, so the URL cannot depend on a display string.
    fn number(self) -> u8 {
        match self {
            FireDay::Day1 => 1,
            FireDay::Day2 => 2,
            FireDay::Day3 => 3,
            FireDay::Day4 => 4,
            FireDay::Day5 => 5,
            FireDay::Day6 => 6,
            FireDay::Day7 => 7,
            FireDay::Day8 => 8,
        }
    }

    /// Days 3-8: the experimental path, and a categorical/probabilistic split.
    /// See [`firewx_url`].
    ///
    /// **The boundary is 3, not 4** — unlike the convective layer, whose
    /// extended range starts at day 4. Fire weather's day 3 already lives
    /// under `/products/exper/fire_wx/` and already carries the `cat`/`prob`
    /// suffix; days 1-2 have neither.
    pub fn is_extended(self) -> bool {
        !matches!(self, FireDay::Day1 | FireDay::Day2)
    }

    /// The `(hazard, product)` pairs **this day** publishes: two for days 1-2,
    /// four for days 3-8. Every one of the 28 was fetched and answered `200`.
    pub fn products(self) -> &'static [(FireHazard, FireProduct)] {
        const NEAR: [(FireHazard, FireProduct); 2] = [
            (FireHazard::DryThunderstorm, FireProduct::Categorical),
            (FireHazard::WindRh, FireProduct::Categorical),
        ];
        const EXTENDED: [(FireHazard, FireProduct); 4] = [
            (FireHazard::DryThunderstorm, FireProduct::Categorical),
            (FireHazard::DryThunderstorm, FireProduct::Probabilistic),
            (FireHazard::WindRh, FireProduct::Categorical),
            (FireHazard::WindRh, FireProduct::Probabilistic),
        ];
        if self.is_extended() { &EXTENDED } else { &NEAR }
    }
}

/// Free function form of [`FireDay::products`], the spelling task 1.1 names.
pub fn products_for(day: FireDay) -> &'static [(FireHazard, FireProduct)] {
    day.products()
}

impl std::fmt::Display for FireDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Day {}", self.label())
    }
}

/// The two things SPC forecasts fire-weather risk for. Independently
/// selectable, and each publishes its own file at every day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FireHazard {
    /// `dryt` — dry thunderstorms, the lightning-without-rain ignition source.
    DryThunderstorm,
    /// `windrh` — the wind and relative-humidity combination that carries fire.
    WindRh,
}

impl FireHazard {
    pub fn all() -> &'static [FireHazard] {
        &[FireHazard::DryThunderstorm, FireHazard::WindRh]
    }

    /// The path fragment, which is also the wire spelling.
    fn slug(self) -> &'static str {
        match self {
            FireHazard::DryThunderstorm => "dryt",
            FireHazard::WindRh => "windrh",
        }
    }
}

impl std::fmt::Display for FireHazard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FireHazard::DryThunderstorm => write!(f, "Dry Thunderstorm"),
            FireHazard::WindRh => write!(f, "Wind/RH"),
        }
    }
}

/// Which of a day-3-to-8 hazard's two forms this is.
///
/// Days 1-2 publish exactly one file per hazard, whose features are named
/// categories (`IDRT`, `ELEV`, ...). It is filed as
/// [`Categorical`](Self::Categorical) and the URL builder drops the suffix —
/// the alternative, a third variant meaning "neither", would put a value in
/// the type that no day-3-to-8 path can spell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FireProduct {
    Categorical,
    Probabilistic,
}

impl FireProduct {
    fn slug(self) -> &'static str {
        match self {
            FireProduct::Categorical => "cat",
            FireProduct::Probabilistic => "prob",
        }
    }
}

impl std::fmt::Display for FireProduct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FireProduct::Categorical => write!(f, "Categorical"),
            FireProduct::Probabilistic => write!(f, "Probabilistic"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpcFireOutlook {
    pub day: FireDay,
    pub hazard: FireHazard,
    pub product: FireProduct,
    pub valid: Option<NaiveDateTime>,
    pub expire: Option<NaiveDateTime>,
    pub features: Vec<OverlayFeature>,
}

/// Origin must come from
/// [`DataSources::spc_base`](rustdar_source::origins::DataSources::spc_base),
/// never a literal, or SPC escapes the origin table's browser-reachability
/// check.
///
/// As with the convective outlooks, the `.lyr` ("layered") form gives every
/// risk category as a whole nested region, so a higher category is drawn again
/// underneath every lower one. That is SPC's own publication order and what
/// the paint order below preserves.
pub fn firewx_url(
    sources: &rustdar_source::origins::DataSources,
    day: FireDay,
    hazard: FireHazard,
    product: FireProduct,
) -> String {
    let base = &sources.spc_base;
    let n = day.number();
    let hazard = hazard.slug();
    if day.is_extended() {
        let product = product.slug();
        format!("{base}/products/exper/fire_wx/day{n}fw_{hazard}{product}.lyr.geojson")
    } else {
        format!("{base}/products/fire_wx/day{n}fw_{hazard}.lyr.geojson")
    }
}

/// The SPC page that shows `day`'s fire weather outlook.
pub fn firewx_page_url(day: FireDay) -> String {
    if day.is_extended() {
        "https://www.spc.noaa.gov/products/exper/fire_wx/".to_owned()
    } else {
        format!(
            "https://www.spc.noaa.gov/products/fire_wx/fwdy{}.html",
            day.label()
        )
    }
}

/// Feed shape — property names and casing are SPC's, and **identical to the
/// convective outlooks'**, inline `stroke`/`fill` included:
/// ```json
/// { "features": [ {
///     "geometry": { "type": "MultiPolygon", "coordinates": [[[[lon, lat], ...]]] },
///     "properties": {
///       "LABEL": "ELEV", "LABEL2": "Elevated Fire Risk",
///       "fill": "#FFBF80", "stroke": "#FF7F00",
///       "VALID": "202608221200", "EXPIRE": "202608231200" } } ] }
/// ```
/// `VALID`/`EXPIRE` are `%Y%m%d%H%M`, no zone marker.
pub fn parse_geojson(
    json: &serde_json::Value,
    day: FireDay,
    hazard: FireHazard,
    product: FireProduct,
) -> Result<SpcFireOutlook, String> {
    let features_array = json
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'features' array in GeoJSON".to_string())?;

    // One tier — see the module docs. `features` is drawn in list order
    // (`rasterize::rasterize_spc_outlooks`), so SPC's own ascending
    // publication order is the paint order and nothing is re-sorted.
    let mut features = Vec::new();
    let mut valid: Option<NaiveDateTime> = None;
    let mut expire: Option<NaiveDateTime> = None;

    for feature_val in features_array {
        // One malformed feature must not blank the whole product: SPC has
        // shipped single degenerate polygons in the convective feed, and
        // propagating the error there turned one of them into an empty
        // outlook. Same feed generator, same posture.
        let ParsedFireFeature {
            feature,
            valid: feat_valid,
            expire: feat_expire,
        } = match parse_fire_feature(feature_val) {
            Ok(parsed) => parsed,
            Err(e) => {
                log::warn!("SPC fire {day} {hazard} {product}: skipping malformed feature: {e}");
                continue;
            }
        };
        // The window before the geometry, deliberately: a feature that draws
        // nothing still dates the product, and out of season it is the ONLY
        // feature there is.
        if valid.is_none() {
            valid = feat_valid;
        }
        if expire.is_none() {
            expire = feat_expire;
        }
        if let Some(feature) = feature {
            features.push(feature);
        }
    }

    Ok(SpcFireOutlook {
        day,
        hazard,
        product,
        valid,
        expire,
        features,
    })
}

struct ParsedFireFeature {
    /// `None` means the feature draws nothing: empty geometry or an
    /// unsupported geometry type. **The window beside it is still good** —
    /// see the note on [`parse_fire_feature`].
    feature: Option<OverlayFeature>,
    valid: Option<NaiveDateTime>,
    expire: Option<NaiveDateTime>,
}

/// **The window is read even from a feature that draws nothing.** An
/// out-of-season day-3-to-8 product is exactly one styleless feature over an
/// empty `GeometryCollection`, and it is the only place that product's
/// `VALID`/`EXPIRE` appear — so the properties are parsed before the geometry
/// and returned whichever way the geometry goes. Pinned by
/// `an_out_of_season_product_parses_to_no_features_and_keeps_its_window`.
fn parse_fire_feature(feature_val: &serde_json::Value) -> Result<ParsedFireFeature, String> {
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

    // Empty is the honest default for a missing style: SPC itself publishes
    // empty strings on the features it draws no geometry for, and
    // `parse_hex_color` answers grey for anything shorter than six hex digits.
    // There is no reserved sentinel fill in this feed to collide with — the
    // convective layer's `#888888` hazard does not exist here, because there
    // is no second tier for it to mean.
    let fill_hex = properties
        .get("fill")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let stroke_hex = properties
        .get("stroke")
        .and_then(|v| v.as_str())
        .unwrap_or("#000000");

    let fill_rgba = super::colors::parse_hex_color(fill_hex, REGULAR_FILL_ALPHA);
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
        // The out-of-season form. SPC writes an empty collection rather than
        // an empty `features` array, so this arm is routine, not exceptional.
        "GeometryCollection" => {
            let geometries = geometry
                .get("geometries")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if geometries > 0 {
                log::warn!("Non-empty GeometryCollection not supported, skipping");
            }
            return Ok(ParsedFireFeature {
                feature: None,
                valid,
                expire,
            });
        }
        other => {
            log::warn!("Skipping unsupported geometry type: {other}");
            return Ok(ParsedFireFeature {
                feature: None,
                valid,
                expire,
            });
        }
    };

    crate::render::geo::simplify_polygons(&mut polygons, crate::types::SIMPLIFY_EPSILON);
    let feature = (!polygons.is_empty()).then(|| {
        OverlayFeature::new(
            polygons,
            fill_rgba,
            stroke_rgba,
            label,
            label2,
            HatchPattern::None,
        )
    });
    Ok(ParsedFireFeature {
        feature,
        valid,
        expire,
    })
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

    #[test]
    fn every_fire_day_names_its_own_products() {
        for &day in FireDay::all() {
            let expected = if day.is_extended() { 4 } else { 2 };
            assert_eq!(
                products_for(day).len(),
                expected,
                "{day} publishes {expected} (hazard, product) pairs",
            );
        }
        assert_eq!(
            products_for(FireDay::Day1),
            &[
                (FireHazard::DryThunderstorm, FireProduct::Categorical),
                (FireHazard::WindRh, FireProduct::Categorical),
            ],
            "days 1-2 publish one file per hazard and no probabilistic form",
        );
        // The boundary is 3, not the convective layer's 4.
        assert!(!FireDay::Day2.is_extended());
        assert!(FireDay::Day3.is_extended());
    }

    /// Fails if any fire URL bypasses `spc_base`.
    #[test]
    fn every_fire_url_comes_from_the_declared_origin() {
        let sources = rustdar_source::origins::DataSources {
            spc_base: std::borrow::Cow::Borrowed("http://127.0.0.1:8080"),
            ..rustdar_source::origins::DataSources::production()
        };
        for &day in FireDay::all() {
            for &(hazard, product) in products_for(day) {
                let url = firewx_url(&sources, day, hazard, product);
                assert!(
                    url.starts_with("http://127.0.0.1:8080/"),
                    "{url} does not come from spc_base",
                );
                assert!(
                    !url.contains("spc.noaa.gov"),
                    "{url} still hardcodes the origin",
                );
            }
        }
    }

    /// All 28 literal paths, transcribed from the endpoints probed live on
    /// 2026-08-21 — every one answered `200`. Hardcoded, so threading
    /// `spc_base` cleanly while mangling a path still fails.
    #[test]
    fn the_production_fire_paths_are_the_ones_that_were_probed() {
        let s = rustdar_source::origins::DataSources::production();
        let built: Vec<String> = FireDay::all()
            .iter()
            .flat_map(|&day| {
                let s = &s;
                products_for(day)
                    .iter()
                    .map(move |&(hazard, product)| firewx_url(s, day, hazard, product))
            })
            .collect();
        let expected = [
            "https://www.spc.noaa.gov/products/fire_wx/day1fw_dryt.lyr.geojson",
            "https://www.spc.noaa.gov/products/fire_wx/day1fw_windrh.lyr.geojson",
            "https://www.spc.noaa.gov/products/fire_wx/day2fw_dryt.lyr.geojson",
            "https://www.spc.noaa.gov/products/fire_wx/day2fw_windrh.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day3fw_drytcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day3fw_drytprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day3fw_windrhcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day3fw_windrhprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day4fw_drytcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day4fw_drytprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day4fw_windrhcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day4fw_windrhprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day5fw_drytcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day5fw_drytprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day5fw_windrhcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day5fw_windrhprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day6fw_drytcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day6fw_drytprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day6fw_windrhcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day6fw_windrhprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day7fw_drytcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day7fw_drytprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day7fw_windrhcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day7fw_windrhprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day8fw_drytcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day8fw_drytprob.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day8fw_windrhcat.lyr.geojson",
            "https://www.spc.noaa.gov/products/exper/fire_wx/day8fw_windrhprob.lyr.geojson",
        ];
        assert_eq!(built.len(), 28, "the 28 endpoints that were probed");
        assert_eq!(built, expected);
    }

    fn feature(label: &str, fill: &str, rings: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "properties": {
                "LABEL": label,
                "LABEL2": "Elevated Fire Risk",
                "fill": fill,
                "stroke": "#FF7F00",
                "VALID": "202608221200",
                "EXPIRE": "202608231200"
            },
            "geometry": { "type": "MultiPolygon", "coordinates": rings }
        })
    }

    fn square() -> serde_json::Value {
        serde_json::json!([[[
            [-100.0, 30.0],
            [-95.0, 30.0],
            [-95.0, 35.0],
            [-100.0, 35.0],
            [-100.0, 30.0]
        ]]])
    }

    fn parse(json: &serde_json::Value) -> SpcFireOutlook {
        parse_geojson(
            json,
            FireDay::Day1,
            FireHazard::WindRh,
            FireProduct::Categorical,
        )
        .expect("a well-formed product parses")
    }

    #[test]
    fn a_degenerate_feature_is_skipped_and_the_good_ones_survive() {
        let degenerate = feature(
            "ELEV",
            "#FFBF80",
            serde_json::json!([[[[-100.0, 30.0], [-99.0, 30.0]]]]),
        );
        let good = feature("CRIT", "#FF8080", square());
        let outlook = parse(&serde_json::json!({ "features": [degenerate, good] }));

        assert_eq!(
            outlook.features.len(),
            1,
            "the good feature survives, the bad one is dropped",
        );
        assert_eq!(outlook.features[0].label, "CRIT");
        assert!(outlook.valid.is_some() && outlook.expire.is_some());
    }

    /// The whole tier finding, as an assertion: SPC's own colours arrive
    /// unmodified, every feature is at the regular alpha, nothing is hatched,
    /// and the feed's order is the paint order.
    #[test]
    fn every_fire_feature_is_one_tier_in_spcs_own_colours() {
        let json = serde_json::json!({
            "features": [
                feature("ELEV", "#FFBF80", square()),
                feature("CRIT", "#FF8080", square()),
            ]
        });
        let outlook = parse(&json);
        let labels: Vec<&str> = outlook.features.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            ["ELEV", "CRIT"],
            "SPC publishes ascending, and drawing in that order is what puts \
             the higher category on top",
        );
        for f in &outlook.features {
            assert_eq!(
                f.hatch,
                HatchPattern::None,
                "{} was hatched; fire weather has no overlay tier to hatch",
                f.label,
            );
            assert_eq!(
                f.fill_rgba[3], REGULAR_FILL_ALPHA,
                "{} was painted at an overlay alpha",
                f.label,
            );
        }
        assert_eq!(outlook.features[0].fill_rgba, [0xFF, 0xBF, 0x80, 100]);
        assert_eq!(outlook.features[0].stroke_rgba, [0xFF, 0x7F, 0x00, 255]);
    }

    /// The convective layer's reserved significant-severe fill has **no
    /// meaning here**. A feature carrying it is an ordinary risk feature, not
    /// an overlay — this is the assertion that would fail if someone ported
    /// `OutlookLayer::of` across.
    #[test]
    fn the_convective_layers_reserved_fill_does_not_promote_a_fire_feature() {
        let json = serde_json::json!({
            "features": [feature("ELEV", "#888888", square())]
        });
        let outlook = parse(&json);
        let f = &outlook.features[0];
        assert_eq!(f.hatch, HatchPattern::None);
        assert_eq!(f.fill_rgba, [0x88, 0x88, 0x88, REGULAR_FILL_ALPHA]);
    }

    /// SPC's own out-of-season shape, verbatim: one styleless feature over an
    /// empty `GeometryCollection`. It must parse to a product with no features
    /// and the window still read off.
    #[test]
    fn an_out_of_season_product_parses_to_no_features_and_keeps_its_window() {
        let json = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "properties": {
                    "DN": 0,
                    "VALID": "202608251200",
                    "EXPIRE": "202608261200",
                    "LABEL": "Probability Too Low",
                    "LABEL2": "",
                    "stroke": "",
                    "fill": ""
                },
                "geometry": { "type": "GeometryCollection", "geometries": [] }
            }]
        });
        let outlook = parse_geojson(
            &json,
            FireDay::Day5,
            FireHazard::DryThunderstorm,
            FireProduct::Probabilistic,
        )
        .expect("an out-of-season product is a product, not a fault");
        assert!(
            outlook.features.is_empty(),
            "the placeholder draws nothing and must not become a feature",
        );
        assert_eq!(
            outlook.valid,
            chrono::NaiveDate::from_ymd_opt(2026, 8, 25).and_then(|d| d.and_hms_opt(12, 0, 0)),
            "the window is the only fact the placeholder carries, and it is \
             read off even though the feature itself is skipped",
        );
    }

    #[test]
    fn a_missing_features_array_is_still_a_hard_error() {
        let json = serde_json::json!({ "type": "FeatureCollection" });
        assert!(
            parse_geojson(
                &json,
                FireDay::Day1,
                FireHazard::WindRh,
                FireProduct::Categorical,
            )
            .is_err(),
            "an envelope without features is a broken product, not an empty one",
        );
    }
}
