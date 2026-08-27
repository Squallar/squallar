//! The committed styles are what this suite judges, not the converter's report
//! about them.
//!
//! Every test here opens `www/styles/*.json` off disk. If someone hand-edits a
//! style tomorrow -- which is the whole point of them being owned source -- the
//! edit is gated by exactly these assertions and not by a memory of what the
//! converter once emitted.
//!
//! The second half of the file is the non-vacuity half. This project has a
//! named recurring defect for checks that cannot fail, so every check in the
//! first half is handed a deliberately broken document here and required to
//! reject it. A check appearing only in the first half is a check nobody has
//! shown to work.

use basemap_style::{
    ABSENT_FROM_OMT, DELIBERATE_DROPS, Expectation, OMT_SOURCE_LAYERS, PHASE_KEY, check,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use walkers::{Context, Filter, Layer, Style};

/// How many layers each committed style carries.
///
/// 93 upstream minus the one `housenumber` layer named in `DELIBERATE_DROPS`.
const EXPECTED_LAYERS: usize = 92;

/// The upstream layer count both CARTO inputs had.
const UPSTREAM_LAYERS: usize = 93;

/// Ground and label layers, which must together account for every layer.
const EXPECTED_GROUND: usize = 66;
const EXPECTED_LABEL: usize = 26;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("tools/basemap-style sits two levels under the repo root")
        .to_path_buf()
}

fn style_path(theme: &str) -> PathBuf {
    repo_root()
        .join("www")
        .join("styles")
        .join(format!("{theme}.json"))
}

fn style_text(theme: &str) -> String {
    let path = style_path(theme);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn style_json(theme: &str) -> Value {
    serde_json::from_str(&style_text(theme)).expect("a committed style is valid JSON")
}

fn themes() -> [&'static str; 2] {
    ["dark", "light"]
}

fn expectation() -> Expectation {
    Expectation {
        input_layers: UPSTREAM_LAYERS,
        deliberate_drops: DELIBERATE_DROPS
            .iter()
            .map(|(sl, why)| ((*sl).to_owned(), (*why).to_owned()))
            .collect(),
    }
}

/// The `source-layer` of a walkers layer, for the variants that have one.
fn source_layer_of(layer: &Layer) -> Option<&str> {
    match layer {
        Layer::Fill { source_layer, .. }
        | Layer::Line { source_layer, .. }
        | Layer::Symbol { source_layer, .. }
        | Layer::Circle { source_layer, .. } => Some(source_layer),
        Layer::Background { .. } | Layer::Raster | Layer::FillExtrusion => None,
    }
}

// ── What the committed styles must be ───────────────────────────────────────

/// The parse is the strongest single signal available, because
/// `walkers::style::Style` is a `Vec` of an internally-tagged enum: one layer
/// with an unknown `type`, or a `fill` missing its `paint`, takes the whole
/// document down. "It parsed" therefore means all 92 layers are shaped the way
/// the renderer requires.
///
/// That the failure mode is real rather than assumed is pinned by
/// [`one_unparseable_layer_fails_the_entire_style_parse`].
#[test]
fn both_committed_styles_parse_as_walkers_styles() {
    for theme in themes() {
        let style = Style::from_json(&style_text(theme)).unwrap_or_else(|e| panic!("{theme}: {e}"));
        assert_eq!(
            style.layers.len(),
            EXPECTED_LAYERS,
            "{theme}: layer count changed"
        );
    }
}

/// The layer count is preserved across the transform, minus drops named by
/// source layer in `DELIBERATE_DROPS` and recorded in each style's own
/// `metadata`.
#[test]
fn the_layer_count_is_preserved_minus_the_named_drops() {
    for theme in themes() {
        let style = style_json(theme);
        let upstream = style["metadata"]["squallar:upstream-layers"]
            .as_u64()
            .expect("the style records its upstream layer count");
        let dropped = style["metadata"]["squallar:dropped-layers"]
            .as_array()
            .expect("the style records what it dropped");
        let layers = style["layers"].as_array().expect("layers is an array");

        assert_eq!(upstream as usize, UPSTREAM_LAYERS, "{theme}");
        assert_eq!(dropped.len(), 1, "{theme}: exactly one deliberate drop");
        assert_eq!(
            dropped[0]["source-layer"], "housenumber",
            "{theme}: the drop is named"
        );
        assert!(
            dropped[0]["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "{theme}: the drop carries its reason"
        );
        assert_eq!(
            layers.len(),
            upstream as usize - dropped.len(),
            "{theme}: layers lost that nothing accounts for"
        );
    }
}

/// Every `source-layer` in the output is one of the sixteen OpenMapTiles names.
///
/// Exhaustive by construction: the loop visits every layer walkers parsed, and
/// asserts on every one that carries a source layer. A missed rename is a layer
/// that draws nothing with no error anywhere, so a spot check would be worth
/// very little. Pinned non-vacuous by
/// [`an_unrenamed_mapbox_streets_source_layer_is_rejected`].
#[test]
fn every_source_layer_is_one_of_the_sixteen_openmaptiles_names() {
    for theme in themes() {
        let style = Style::from_json(&style_text(theme)).expect("parses");
        let mut checked = 0;
        for layer in &style.layers {
            let Some(source_layer) = source_layer_of(layer) else {
                continue;
            };
            checked += 1;
            assert!(
                OMT_SOURCE_LAYERS.contains(&source_layer),
                "{theme}: `{source_layer}` is not an OpenMapTiles source layer"
            );
        }
        assert_eq!(
            checked,
            EXPECTED_LAYERS - 1,
            "{theme}: every layer but `background` carries a source layer"
        );
    }
}

/// No output layer references a source layer OpenMapTiles does not carry.
///
/// Separate from the sixteen-name check on purpose: these four are the names
/// that look like renames and are not, so a failure here should read as "this
/// cannot be satisfied from the schema", not as "you forgot a mapping".
#[test]
fn no_source_layer_is_one_openmaptiles_lacks_entirely() {
    for theme in themes() {
        let style = Style::from_json(&style_text(theme)).expect("parses");
        for layer in &style.layers {
            if let Some(source_layer) = source_layer_of(layer) {
                assert!(
                    !ABSENT_FROM_OMT.contains(&source_layer),
                    "{theme}: `{source_layer}` has no OpenMapTiles counterpart"
                );
            }
        }
    }
}

/// Every layer is tagged `ground` or `label`, and the two partition the style.
#[test]
fn every_layer_carries_a_machine_readable_phase_tag() {
    for theme in themes() {
        let style = style_json(theme);
        let mut ground = 0;
        let mut label = 0;
        for layer in style["layers"].as_array().expect("layers") {
            let id = layer["id"].as_str().unwrap_or("<no id>");
            let phase = layer["metadata"][PHASE_KEY]
                .as_str()
                .unwrap_or_else(|| panic!("{theme}: `{id}` has no {PHASE_KEY}"));
            match phase {
                "ground" => ground += 1,
                "label" => label += 1,
                other => panic!("{theme}: `{id}` has phase `{other}`"),
            }
            // The tag is the renderer's contract, so it has to agree with the
            // only thing that can place text.
            let is_symbol = layer["type"] == "symbol";
            assert_eq!(
                phase == "label",
                is_symbol,
                "{theme}: `{id}` phase disagrees with its type"
            );
        }
        assert_eq!(ground, EXPECTED_GROUND, "{theme}: ground layers");
        assert_eq!(label, EXPECTED_LABEL, "{theme}: label layers");
        assert_eq!(ground + label, EXPECTED_LAYERS, "{theme}: phases partition");
    }
}

/// The whole-document checker passes both committed styles.
#[test]
fn the_checker_finds_nothing_wrong_with_either_committed_style() {
    for theme in themes() {
        let findings = check(&style_json(theme), &expectation());
        assert!(findings.is_empty(), "{theme}: {findings:?}");
    }
}

/// Every property key a committed filter tests exists in the tiles.
///
/// Measured from the 42-tile corpus at `~/.cache/omt-corpus/`
/// (`u5corpus-keys.tsv`) and pinned here rather than read at test time, so the
/// suite is self-contained. A filter on a key the data does not carry is the
/// same silent nothing as a missed rename.
#[test]
fn every_filter_key_exists_in_the_openmaptiles_data() {
    let corpus: BTreeMap<&str, &[&str]> = BTreeMap::from([
        ("aeroway", &["class", "ref"][..]),
        ("boundary", &["admin_level", "disputed", "maritime"][..]),
        ("landcover", &["class", "subclass"][..]),
        ("landuse", &["class"][..]),
        ("park", &["class", "name", "rank"][..]),
        ("place", &["capital", "class", "iso_a2", "name", "rank"][..]),
        ("poi", &["class", "name", "rank", "subclass"][..]),
        (
            "transportation",
            &[
                "access",
                "bicycle",
                "brunnel",
                "class",
                "expressway",
                "foot",
                "layer",
                "network",
                "oneway",
                "ramp",
                "service",
                "subclass",
                "surface",
            ][..],
        ),
        (
            "transportation_name",
            &["class", "name", "network", "ref", "ref_length", "subclass"][..],
        ),
        ("water", &["class", "id", "intermittent"][..]),
        ("water_name", &["class", "intermittent", "name"][..]),
        ("waterway", &["class", "name"][..]),
        ("building", &[][..]),
    ]);

    for theme in themes() {
        let style = style_json(theme);
        for layer in style["layers"].as_array().expect("layers") {
            let Some(source_layer) = layer["source-layer"].as_str() else {
                continue;
            };
            let Some(filter) = layer.get("filter") else {
                continue;
            };
            let id = layer["id"].as_str().unwrap_or("<no id>");
            let known = corpus
                .get(source_layer)
                .unwrap_or_else(|| panic!("{theme}: no corpus row for `{source_layer}`"));
            for key in filter_keys(filter) {
                assert!(
                    known.contains(&key.as_str()),
                    "{theme}: `{id}` filters `{source_layer}` on `{key}`, which the tiles do not \
                     carry"
                );
            }
        }
    }
}

/// Property keys a filter expression tests, ignoring `zoom` and `$type`.
fn filter_keys(filter: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    collect_filter_keys(filter, &mut keys);
    keys.sort();
    keys.dedup();
    keys
}

fn collect_filter_keys(filter: &Value, into: &mut Vec<String>) {
    let Some(items) = filter.as_array() else {
        return;
    };
    let Some(operator) = items.first().and_then(Value::as_str) else {
        return;
    };
    match operator {
        "all" | "any" | "!" => {
            for nested in items.iter().skip(1) {
                collect_filter_keys(nested, into);
            }
        }
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "in" | "has" | "!has" => {
            // `$type` is a pseudo-key the evaluator answers from the geometry,
            // not a property the tiles carry.
            if let Some(key) = items.get(1).and_then(Value::as_str)
                && !key.starts_with('$')
            {
                into.push(key.to_owned());
            }
        }
        _ => {}
    }
}

/// The folded zoom range actually gates, evaluated through the real evaluator.
///
/// This is the end-to-end proof for the most contested transform in the tool.
/// `walkers::style::Layer` has no `minzoom` field and `walkers::mvt::render`
/// consults nothing but `filter`, so without the fold every layer draws at
/// every zoom. Here a building layer's own committed filter is evaluated at a
/// zoom below its range and at one inside it, and has to answer differently.
#[test]
fn a_folded_zoom_range_gates_the_layer_through_the_real_evaluator() {
    let style = style_json("dark");
    // `aeroway-runway` is the fixture because everything about it beyond the
    // zoom clause is one property equality, so a `false` can be attributed.
    let runway = style["layers"]
        .as_array()
        .expect("layers")
        .iter()
        .find(|l| l["id"] == "aeroway-runway")
        .expect("an `aeroway-runway` layer")
        .clone();

    let minzoom = runway["minzoom"].as_u64().expect("a minzoom") as u8;
    assert!(minzoom > 1, "the fixture needs a zoom below its range");

    let properties = || {
        let mut map = std::collections::HashMap::new();
        map.insert("class".to_owned(), json!("runway"));
        map
    };
    let at = |zoom: u8| Context::new("LineString".to_owned(), properties(), zoom);

    let filter: Filter = serde_json::from_value(runway["filter"].clone())
        .expect("the filter deserialises as a walkers filter");

    assert!(
        !filter.matches(&at(minzoom - 1)),
        "the layer draws below its minzoom -- the zoom fold is not gating"
    );
    assert!(
        filter.matches(&at(minzoom)),
        "the layer does not draw at its minzoom -- the zoom fold over-gates"
    );

    // Non-vacuity: the same filter with its zoom clauses stripped matches at
    // the very zoom that was just rejected. So the rejection above came from
    // the fold and not from the property equality or the geometry type.
    let stripped: Vec<Value> = runway["filter"]
        .as_array()
        .expect("an `all` filter")
        .iter()
        .filter(|clause| clause.get(1) != Some(&json!(["zoom"])))
        .cloned()
        .collect();
    let stripped: Filter = serde_json::from_value(Value::Array(stripped)).expect("still a filter");
    assert!(
        stripped.matches(&at(minzoom - 1)),
        "the control does not match, so the gating test proves nothing"
    );
}

/// Every layer with a zoom range carries that range in its filter.
///
/// The structural companion to
/// [`a_folded_zoom_range_gates_the_layer_through_the_real_evaluator`], which
/// proves one layer gates. This one proves none of the other 83 were missed.
#[test]
fn every_layer_with_a_zoom_range_folded_it_into_its_filter() {
    for theme in themes() {
        let style = style_json(theme);
        let mut folded = 0;
        for layer in style["layers"].as_array().expect("layers") {
            let id = layer["id"].as_str().unwrap_or("<no id>");
            let clauses = || -> Vec<Value> {
                layer
                    .get("filter")
                    .and_then(Value::as_array)
                    .filter(|f| f.first() == Some(&json!("all")))
                    .map(|f| f[1..].to_vec())
                    .unwrap_or_default()
            };
            if let Some(min) = layer.get("minzoom") {
                assert!(
                    clauses().contains(&json!([">=", ["zoom"], min])),
                    "{theme}: `{id}` has minzoom {min} that no filter clause enforces"
                );
                folded += 1;
            }
            if let Some(max) = layer.get("maxzoom") {
                assert!(
                    clauses().contains(&json!(["<", ["zoom"], max])),
                    "{theme}: `{id}` has maxzoom {max} that no filter clause enforces"
                );
            }
        }
        assert_eq!(folded, 84, "{theme}: layers carrying a minzoom");
    }
}

/// No legacy construct the expression evaluator cannot answer survives.
///
/// Each of these reaches `walkers::expression::Context::evaluate`'s fallback
/// arm and turns into a silent nothing: a `stops` object makes a paint property
/// take its fallback, a `!in` filter makes the layer draw nothing, and a
/// `{name}` token draws the literal text `{name}` on the map.
#[test]
fn no_legacy_stops_tokens_or_not_in_filters_survive() {
    for theme in themes() {
        let mut found = Vec::new();
        walk_for_legacy(&style_json(theme), &mut found);
        assert!(found.is_empty(), "{theme}: {found:?}");
    }
}

/// Legacy constructs anywhere in a document.
///
/// Structural rather than a substring scan, and it has to be: `text-transform`
/// takes the perfectly legal *value* `"none"`, which a scan for the string
/// `"none"` reports as a legacy `none` filter. Only the first element of an
/// array is an operator.
fn walk_for_legacy(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if object.contains_key("stops") {
                found.push("a legacy `stops` function".to_owned());
            }
            for nested in object.values() {
                walk_for_legacy(nested, found);
            }
        }
        Value::Array(items) => {
            match items.first().and_then(Value::as_str) {
                Some("!in") => found.push("a legacy `!in` filter".to_owned()),
                Some("none") => found.push("a legacy `none` filter".to_owned()),
                _ => {}
            }
            for item in items {
                walk_for_legacy(item, found);
            }
        }
        Value::String(text) => {
            let bare_token = text.starts_with('{')
                && text.ends_with('}')
                && text.len() > 2
                && !text[1..text.len() - 1].contains(['{', '}']);
            if bare_token {
                found.push(format!("an unexpanded `{text}` token"));
            }
        }
        _ => {}
    }
}

/// The legacy scan rejects each construct it claims to catch.
///
/// Non-vacuity for [`no_legacy_stops_tokens_or_not_in_filters_survive`], and
/// the regression pin for the substring scan it replaced: a legal
/// `"text-transform": "none"` must NOT be reported.
#[test]
fn the_legacy_scan_catches_each_construct_and_no_legal_value() {
    for (planted, expected) in [
        (
            json!({ "paint": { "fill-color": { "stops": [[1, "#000"]] } } }),
            "stops",
        ),
        (json!({ "filter": ["!in", "class", "a"] }), "!in"),
        (json!({ "filter": ["none", ["==", "class", "a"]] }), "none"),
        (
            json!({ "layout": { "text-field": "{name}" } }),
            "unexpanded",
        ),
    ] {
        let mut found = Vec::new();
        walk_for_legacy(&planted, &mut found);
        assert!(
            found.iter().any(|f| f.contains(expected)),
            "the scan missed a planted `{expected}`: {found:?}"
        );
    }

    let legal = json!({ "layout": { "text-transform": "none", "visibility": "visible" } });
    let mut found = Vec::new();
    walk_for_legacy(&legal, &mut found);
    assert!(
        found.is_empty(),
        "the scan reported a legal `text-transform` value: {found:?}"
    );
}

/// Nothing points at a CARTO service.
///
/// The style documents are BSD-licensed source and converting them is fine;
/// CARTO's tiles, glyph PBFs and sprite sheets are a restricted service. A URL
/// left in the committed file is an invitation for someone to wire a fetch to
/// it later.
#[test]
fn no_committed_style_references_a_carto_service_url() {
    for theme in themes() {
        let style = style_json(theme);
        // The provenance note in `metadata` names the GitHub repository on
        // purpose; the layers and sources must not name a CDN.
        let mut without_metadata = style.clone();
        without_metadata
            .as_object_mut()
            .expect("object")
            .remove("metadata");
        let text = without_metadata.to_string();
        for forbidden in ["cartocdn", "basemaps.cartocdn.com", "carto.com"] {
            assert!(
                !text.contains(forbidden),
                "{theme}: `{forbidden}` survives outside metadata"
            );
        }
        assert!(
            style.get("glyphs").is_none(),
            "{theme}: a glyphs URL is hosted by nobody and read by nothing"
        );
        assert!(
            style.get("sprite").is_none(),
            "{theme}: a sprite URL is hosted by nobody and read by nothing"
        );
    }
}

// ── Non-vacuity: every check above, shown to fail ───────────────────────────
//
// Each test mutates a real committed style into a specific defect and requires
// rejection. The mutation is always a single targeted edit, so a rejection can
// only be attributed to the defect introduced.

/// The mutation helper: the dark style with one layer's field replaced.
fn dark_with_layer_field(layer_id: &str, key: &str, value: Value) -> Value {
    let mut style = style_json("dark");
    let layers = style["layers"].as_array_mut().expect("layers");
    let layer = layers
        .iter_mut()
        .find(|l| l["id"] == layer_id)
        .unwrap_or_else(|| panic!("no layer `{layer_id}`"));
    layer
        .as_object_mut()
        .expect("layer object")
        .insert(key.to_owned(), value);
    style
}

/// A Mapbox Streets name left unrenamed is rejected.
///
/// The defect the sixteen-name check exists for: `road` is what CARTO's
/// Mapbox-era styles called `transportation`, and a tile has no layer by that
/// name, so the roads would simply not be there.
#[test]
fn an_unrenamed_mapbox_streets_source_layer_is_rejected() {
    let broken = dark_with_layer_field("building", "source-layer", json!("road"));
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("road")),
        "an unrenamed `road` source layer was accepted: {findings:?}"
    );
}

/// A source layer OpenMapTiles lacks entirely is rejected, and is reported as
/// such rather than as an unknown name.
#[test]
fn a_source_layer_absent_from_openmaptiles_is_rejected() {
    for absent in ABSENT_FROM_OMT {
        let broken = dark_with_layer_field("building", "source-layer", json!(absent));
        let findings = check(&broken, &expectation());
        assert!(
            findings
                .iter()
                .any(|f| f.0.contains(absent) && f.0.contains("does not carry")),
            "`{absent}` was accepted: {findings:?}"
        );
    }
}

/// An invented source layer is rejected.
#[test]
fn an_invalid_source_layer_is_rejected() {
    let broken = dark_with_layer_field("building", "source-layer", json!("not_a_real_layer"));
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("not_a_real_layer")),
        "an invented source layer was accepted: {findings:?}"
    );
}

/// A silently dropped layer is rejected.
///
/// The defect the count check exists for: a transform that loses a layer
/// produces a map that is missing something, with nothing anywhere saying so.
#[test]
fn a_silently_dropped_layer_is_rejected() {
    let mut broken = style_json("dark");
    broken["layers"].as_array_mut().expect("layers").remove(10);
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("layer count")),
        "a dropped layer was accepted: {findings:?}"
    );
}

/// A layer smuggled in beyond the upstream count is rejected too.
///
/// The count check has to fail in both directions or it only catches half of
/// what it looks like it catches.
#[test]
fn a_layer_added_beyond_the_upstream_count_is_rejected() {
    let mut broken = style_json("dark");
    let extra = broken["layers"][5].clone();
    broken["layers"].as_array_mut().expect("layers").push(extra);
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("layer count")),
        "an extra layer was accepted: {findings:?}"
    );
}

/// A deliberately dropped source layer coming back is rejected.
#[test]
fn a_resurrected_housenumber_layer_is_rejected() {
    let broken = dark_with_layer_field("building", "source-layer", json!("housenumber"));
    let findings = check(&broken, &expectation());
    assert!(
        findings
            .iter()
            .any(|f| f.0.contains("deliberate-drop list")),
        "a resurrected `housenumber` layer was accepted: {findings:?}"
    );
}

/// A layer with no phase tag is rejected.
#[test]
fn a_layer_without_a_phase_tag_is_rejected() {
    let mut broken = style_json("dark");
    broken["layers"][3]
        .as_object_mut()
        .expect("layer object")
        .remove("metadata");
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains(PHASE_KEY)),
        "an untagged layer was accepted: {findings:?}"
    );
}

/// A phase tag disagreeing with the layer's type is rejected.
#[test]
fn a_mistagged_render_phase_is_rejected() {
    let mut broken = style_json("dark");
    let layers = broken["layers"].as_array_mut().expect("layers");
    let symbol = layers
        .iter_mut()
        .find(|l| l["type"] == "symbol")
        .expect("a symbol layer");
    symbol["metadata"]
        .as_object_mut()
        .expect("metadata object")
        .insert(PHASE_KEY.to_owned(), json!("ground"));
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("tagged `ground`")),
        "a mistagged symbol layer was accepted: {findings:?}"
    );
}

/// A legacy `stops` function left behind is rejected.
#[test]
fn a_surviving_legacy_stops_function_is_rejected() {
    let broken = dark_with_layer_field(
        "building",
        "paint",
        json!({ "fill-color": { "stops": [[10, "#111"], [14, "#222"]] } }),
    );
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("legacy `stops`")),
        "a legacy stops function was accepted: {findings:?}"
    );
}

/// A legacy `!in` filter left behind is rejected.
#[test]
fn a_surviving_legacy_not_in_filter_is_rejected() {
    let broken = dark_with_layer_field(
        "building",
        "filter",
        json!(["all", ["!in", "class", "a", "b"]]),
    );
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("legacy `!in`")),
        "a legacy !in filter was accepted: {findings:?}"
    );
}

/// An unexpanded `{name}` token left behind is rejected.
#[test]
fn a_surviving_text_field_token_is_rejected() {
    let broken = dark_with_layer_field("building", "layout", json!({ "text-field": "{name}" }));
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("unexpanded")),
        "an unexpanded token was accepted: {findings:?}"
    );
}

/// One malformed layer fails the entire style parse.
///
/// This is what makes [`both_committed_styles_parse_as_walkers_styles`] worth
/// running: `Style` is a `Vec` of an internally-tagged enum, so the parse is
/// all-or-nothing across all 92 layers rather than a per-layer best effort.
#[test]
fn one_unparseable_layer_fails_the_entire_style_parse() {
    let mut broken = style_json("dark");
    broken["layers"][40]
        .as_object_mut()
        .expect("layer object")
        .insert("type".to_owned(), json!("heatmap"));
    assert!(
        Style::from_json(&broken.to_string()).is_err(),
        "a layer with an unsupported `type` did not fail the parse"
    );

    // And the same document minus that one edit does parse, so the rejection
    // is attributable to the edit rather than to anything else about it.
    assert!(
        Style::from_json(&style_json("dark").to_string()).is_ok(),
        "the unmutated control did not parse"
    );
}

/// A filter on a key the tiles do not carry is caught.
///
/// The non-vacuity control for
/// [`every_filter_key_exists_in_the_openmaptiles_data`]: that test walks the
/// committed styles, so it can only be trusted if the same walk rejects a
/// planted bad key.
#[test]
fn a_filter_on_a_key_the_tiles_lack_is_caught() {
    let broken = json!(["all", ["==", "no_such_key", "x"]]);
    let keys = filter_keys(&broken);
    assert_eq!(keys, vec!["no_such_key".to_owned()]);
    let known: &[&str] = &["class", "render_height"];
    assert!(
        !known.contains(&keys[0].as_str()),
        "the key check would have accepted an unknown key"
    );
}

// ── The converter's own refusals ────────────────────────────────────────────

/// The rename table works, even though it fired zero times on the real input.
///
/// Everything CARTO ships today already targets OpenMapTiles, so the six
/// renames are dead code against those two files. Dead code that has never been
/// executed is not known to work, and the whole reason the table exists is the
/// next person who points this converter at an older revision.
#[test]
fn the_converter_renames_every_mapbox_streets_source_layer() {
    for (from, to) in basemap_style::SOURCE_LAYER_RENAMES {
        let input = json!({
            "layers": [
                { "id": "bg", "type": "background", "paint": {} },
                { "id": "x", "type": "line", "source": "carto", "source-layer": from,
                  "paint": { "line-color": "#fff" } }
            ]
        });
        let (converted, report) =
            basemap_style::convert(&input, "t", &serde_json::Map::new()).expect("converts");
        assert_eq!(
            converted["layers"][1]["source-layer"], to,
            "`{from}` was not renamed to `{to}`"
        );
        assert_eq!(report.renamed.len(), 1, "`{from}` rename went unreported");
    }
}

/// The converter refuses a source layer OpenMapTiles cannot satisfy, rather
/// than dropping it quietly.
#[test]
fn the_converter_refuses_a_source_layer_openmaptiles_cannot_satisfy() {
    for absent in ABSENT_FROM_OMT {
        let input = json!({
            "layers": [
                { "id": "x", "type": "line", "source-layer": absent,
                  "paint": { "line-color": "#fff" } }
            ]
        });
        let result = basemap_style::convert(&input, "t", &serde_json::Map::new());
        assert!(result.is_err(), "`{absent}` was silently converted");
    }
}

/// The converter refuses an unknown source layer rather than emitting a layer
/// that draws nothing.
#[test]
fn the_converter_refuses_an_unknown_source_layer() {
    let input = json!({
        "layers": [
            { "id": "x", "type": "line", "source-layer": "invented",
              "paint": { "line-color": "#fff" } }
        ]
    });
    assert!(basemap_style::convert(&input, "t", &serde_json::Map::new()).is_err());
}

/// A single-stop function collapses to a scalar instead of becoming an
/// `interpolate` the evaluator cannot answer.
///
/// `walkers::expression` needs two stops to form a window; given exactly one it
/// returns an error and the property takes its fallback -- `0.5` for a float.
/// Collapsing is not a tidy-up, it is the difference between the written value
/// and `0.5`.
#[test]
fn a_single_stop_function_collapses_to_a_scalar() {
    let input = json!({
        "layers": [
            { "id": "x", "type": "line", "source-layer": "transportation",
              "paint": { "line-opacity": { "stops": [[10, 0.25]] } } }
        ]
    });
    let (converted, report) =
        basemap_style::convert(&input, "t", &serde_json::Map::new()).expect("converts");
    assert_eq!(converted["layers"][0]["paint"]["line-opacity"], json!(0.25));
    assert_eq!(report.stop_sets_collapsed_to_scalars, 1);
}
