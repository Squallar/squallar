//! The committed styles are what this suite judges, not the converter's report
//! about them.
//!
//! `www/styles/{dark,light}.json` are shipped to users and edited by hand. They
//! were produced once, on 2026-08-27, by a converter that stood outside this
//! workspace and so was reached by no root gate -- and the workflow that used to
//! run its tests was deleted in `d1517cb2` as a recurring gate on a one-time
//! job. That deletion was right, and it left these two files checked by nothing
//! that `cargo test --workspace` selects.
//!
//! **The gate belongs here.** A converter's CI could only ever prove the
//! converter still works; what users would notice is a style that stops parsing,
//! and the crate that loads styles is this one. Every check that reads
//! `www/styles/*.json` off disk lives here, and on 2026-08-28 the checker they
//! run came with them: the converter was deleted (its output is owned source now,
//! so re-running it would overwrite work rather than reproduce anything) and the
//! half of it that was never one-shot moved into [`style_gate`], which also
//! carries the conversion's decision record.
//!
//! The failure being guarded is quiet by construction. `Style` is one
//! `Vec<Layer>` deserialised as an internally-tagged enum, so **a single
//! malformed layer fails the entire parse** -- the defect that forced vendoring
//! in the first place, where `Circle` was missing its `rename_all` and CARTO's
//! styles would not load at all. A hand edit that mistypes one `source-layer`
//! takes the whole basemap with it, and nothing before this test would have
//! said so.
//!
//! The second half of the file is the non-vacuity half. This project has a
//! named recurring defect for checks that cannot fail, so every check in the
//! first half is handed a deliberately broken document here and required to
//! reject it. A check appearing only in the first half is a check nobody has
//! shown to work.

// The checker and the vocabulary it judges with, an ordinary sibling module of
// this test binary.
//
// It reached here through `#[path = "../../tools/basemap-style/src/lib.rs"]`
// for one day, so that there was exactly one `check` while the converter still
// existed. That include had a cost worth remembering: rustfmt follows `mod`
// declarations, `#[path]` included, so `cargo fmt -p squallar-egui` reached out
// of this package and rewrote a file in `tools/` -- a package-scoped format that
// was no longer package-confined. Deleting the converter removed the dead code
// and that reach together.
mod style_gate;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{Value, json};
use style_gate::{
    ABSENT_FROM_OMT, DELIBERATE_DROPS, Expectation, OMT_SOURCE_LAYERS, PHASE_KEY, check,
    walk_for_legacy,
};
use walkers::style::{Layer, Style};
use walkers::{Context, Filter};

/// How many layers each committed style carries.
///
/// 93 upstream, nothing dropped, plus the two layers CARTO never styled that
/// `metadata."squallar:added-layers"` declares (`aerodrome_label`,
/// `mountain_peak`). `housenumber` is not among them: CARTO's 93 already
/// contained a housenumber layer and ours fills that slot restyled, which
/// `metadata."squallar:restored-layers"` records.
const EXPECTED_LAYERS: usize = 95;

/// The upstream layer count both CARTO inputs had.
const UPSTREAM_LAYERS: usize = 93;

/// Ground and label layers, which must together account for every layer.
const EXPECTED_GROUND: usize = 66;
const EXPECTED_LABEL: usize = 29;

/// The repository root, from this crate's manifest.
///
/// Resolved from `CARGO_MANIFEST_DIR` rather than the working directory, so the
/// tests behave the same under `cargo test` from anywhere in the workspace.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-egui has a parent directory")
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
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{theme} style is missing at {}: {e}\n\
             These files are committed source, not build output.",
            path.display()
        )
    })
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

fn load(theme: &str) -> Style {
    Style::from_json(&style_text(theme)).unwrap_or_else(|e| {
        panic!(
            "{theme} style at {} no longer deserialises: {e}\n\
             `Style` is one Vec<Layer> as an internally-tagged enum, so ONE bad \
             layer fails the whole file and the basemap draws nothing.",
            style_path(theme).display()
        )
    })
}

/// The `source-layer` of a walkers layer, for the variants that have one.
fn source_layer(layer: &Layer) -> Option<&str> {
    match layer {
        Layer::Fill { source_layer, .. }
        | Layer::Line { source_layer, .. }
        | Layer::Symbol { source_layer, .. }
        | Layer::Circle { source_layer, .. } => Some(source_layer),
        Layer::Background { .. } | Layer::Raster | Layer::FillExtrusion => None,
    }
}

// ── What the committed styles must be ───────────────────────────────────────

/// Both committed styles deserialise, into the layer count they declare.
///
/// The parse is the strongest single signal available, because
/// `walkers::style::Style` is a `Vec` of an internally-tagged enum: one layer
/// with an unknown `type`, or a `fill` missing its `paint`, takes the whole
/// document down. "It parsed" therefore means all 95 layers are shaped the way
/// the renderer requires.
///
/// That the failure mode is real rather than assumed is pinned by
/// [`one_unparseable_layer_fails_the_entire_style_parse`].
///
/// This absorbed `both_committed_styles_parse_as_walkers_styles`, which was the
/// same parse under a different name in the converter's own suite. Both
/// assertions survive: the exact count that test carried, and the floor this one
/// was written with.
#[test]
fn both_committed_styles_deserialise() {
    for theme in themes() {
        let style = load(theme);

        assert_eq!(
            style.layers.len(),
            EXPECTED_LAYERS,
            "{theme}: layer count changed"
        );

        // NON-VACUITY. A bare "it parsed" would pass on `{"layers":[]}`, which
        // deserialises perfectly and renders an empty map. The count above is
        // exact and would catch that on its own; this states the floor the
        // check exists for, so a future edit that relaxes the count cannot
        // relax it all the way to zero without deleting a line that says so.
        assert!(
            style.layers.len() > 50,
            "{theme} parsed but yielded only {} layers. A style that parses and \
             draws nothing is the failure this test exists for, not a pass.",
            style.layers.len()
        );
    }
}

/// Every layer is accounted for: `upstream - dropped + added`.
///
/// This replaced `upstream - dropped` on 2026-08-28. That form encoded an
/// assumption that stopped being true the moment a layer CARTO never styled was
/// written by hand: that our styles are a strict subset of CARTO's. The
/// *intent* was "nothing is silently lost", and it survives intact -- what the
/// new term adds is that nothing is silently gained either.
///
/// It is strictly stronger than the count it replaces, because the additions
/// are read from the document rather than from a constant here: a hand edit
/// that adds a layer without declaring it in `squallar:added-layers` makes the
/// two sides disagree, and so does a declaration with no layer behind it.
///
/// A layer filling an upstream slot with our own styling is neither dropped nor
/// added and does not move this count; `squallar:restored-layers` records it,
/// and every entry there still has to name a real layer and carry a reason.
#[test]
fn every_layer_is_accounted_for_as_upstream_minus_dropped_plus_added() {
    for theme in themes() {
        let style = style_json(theme);
        let metadata = &style["metadata"];
        let upstream = metadata["squallar:upstream-layers"]
            .as_u64()
            .expect("the style records its upstream layer count") as usize;
        let dropped = metadata["squallar:dropped-layers"]
            .as_array()
            .expect("the style records what it dropped");
        let added = metadata["squallar:added-layers"]
            .as_array()
            .expect("the style records what it added");
        let restored = metadata["squallar:restored-layers"]
            .as_array()
            .expect("the style records what it restored");
        let layers = style["layers"].as_array().expect("layers is an array");

        assert_eq!(upstream, UPSTREAM_LAYERS, "{theme}");
        assert_eq!(
            layers.len(),
            upstream - dropped.len() + added.len(),
            "{theme}: layers that nothing accounts for"
        );

        // Every declaration names a real layer and says why. Without this the
        // arithmetic above is gameable from the other side: a bare entry
        // raises the expected total, so an undeclared layer could be
        // "declared" by an entry that names nothing at all.
        let ids: Vec<&str> = layers.iter().filter_map(|l| l["id"].as_str()).collect();
        for (key, entries) in [
            ("squallar:dropped-layers", dropped),
            ("squallar:added-layers", added),
            ("squallar:restored-layers", restored),
        ] {
            for entry in entries {
                let id = entry["id"].as_str().unwrap_or_else(|| {
                    panic!("{theme}: an entry in `{key}` names no id");
                });
                // A drop names a layer that is gone; the other two name layers
                // that are present.
                if key != "squallar:dropped-layers" {
                    assert!(
                        ids.contains(&id),
                        "{theme}: `{key}` declares `{id}`, which no layer provides"
                    );
                }
                assert!(
                    entry["source-layer"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty()),
                    "{theme}: `{key}` entry `{id}` names no source layer"
                );
                assert!(
                    entry["reason"]
                        .as_str()
                        .is_some_and(|r| !r.trim().is_empty()),
                    "{theme}: `{key}` entry `{id}` carries no reason"
                );
            }
        }
    }
}

/// Every `source-layer` in the output is one of the sixteen OpenMapTiles names.
///
/// Exhaustive by construction: the loop visits every layer walkers parsed, and
/// asserts on every one that carries a source layer. A missed rename is a layer
/// that draws nothing with no error anywhere, so a spot check would be worth
/// very little. Pinned non-vacuous by
/// [`an_unrenamed_mapbox_streets_source_layer_is_rejected`].
///
/// This absorbed `every_source_layer_is_one_of_the_sixteen_openmaptiles_names`
/// from the converter's own suite, which asserted the same membership by a
/// per-layer loop. Both non-vacuity floors survive: that test's exact count of
/// layers carrying a source layer, and this one's floor on how many distinct
/// source layers are drawn.
#[test]
fn every_styled_source_layer_exists_in_the_schema() {
    let known: BTreeSet<&str> = OMT_SOURCE_LAYERS.iter().copied().collect();

    for theme in themes() {
        let style = load(theme);

        let used: BTreeSet<&str> = style.layers.iter().filter_map(source_layer).collect();

        let unknown: Vec<&str> = used.difference(&known).copied().collect();
        assert!(
            unknown.is_empty(),
            "{theme} references source layers that OpenMapTiles does not define: {unknown:?}\n\
             These draw NOTHING and fail silently -- the renderer looks the name \
             up, finds no such layer in the tile, and moves on. Six of these are \
             Mapbox Streets names that needed renaming (road -> transportation, \
             admin -> boundary, poi_label -> poi, place_label -> place, \
             road_label -> transportation_name, airport_label -> aerodrome_label)."
        );

        // The same non-vacuity argument as above, one level down: a style whose
        // layers all happened to be `Background` would pass the check above
        // trivially, because an empty set is a subset of anything.
        assert!(
            used.len() >= 10,
            "{theme} draws from only {} source layers ({used:?}). The check above \
             passes vacuously on an empty set, so this is what keeps it honest.",
            used.len()
        );

        let checked = style.layers.iter().filter_map(source_layer).count();
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
        let style = load(theme);
        for layer in &style.layers {
            if let Some(source_layer) = source_layer(layer) {
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
        // The three layers the styles gained on 2026-08-28. Same 42-tile
        // corpus, same `u5corpus-keys.tsv`, name-translation variants
        // (`name:xx`, `name_de`, `name_en`, `name_int`) excluded exactly as
        // the rows above exclude them.
        ("housenumber", &["housenumber"][..]),
        (
            "mountain_peak",
            &["class", "customary_ft", "ele", "ele_ft", "name", "rank"][..],
        ),
        (
            "aerodrome_label",
            &["class", "ele", "ele_ft", "iata", "icao", "name"][..],
        ),
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
/// proves one layer gates. This one proves none of the other 86 were missed.
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
        assert_eq!(folded, 87, "{theme}: layers carrying a minzoom");
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

/// The legacy scan rejects each construct it claims to catch.
///
/// Non-vacuity for [`no_legacy_stops_tokens_or_not_in_filters_survive`], and
/// the regression pin for the substring scan it replaced: a legal
/// `"text-transform": "none"` must NOT be reported.
///
/// It exercises [`style_gate::walk_for_legacy`], which is also the scan `check`
/// runs per layer. This file carried a textually equivalent copy while the
/// checker lived in another crate; the copy went when the crate did, so this
/// non-vacuity floor now sits under both callers instead of one.
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

/// A declaration naming no layer is rejected.
///
/// This is the half the `+ added` term introduced: the declaration cannot be a
/// bare number that buys a layer nobody has to justify. Without it the count is
/// gameable from the wrong side -- an entry raises the expected total, so an
/// undeclared layer could be "declared" by an entry naming nothing at all.
///
/// It took the slot of the test that pinned `housenumber` against the
/// deliberate-drop list, which lost its subject when `housenumber` came off
/// `DELIBERATE_DROPS` on 2026-08-28. That mechanism is still checked: rule 3 of
/// `check` still rejects any source layer on the drop list, and
/// [`a_silently_dropped_layer_is_rejected`] still pins the count direction.
#[test]
fn a_declared_addition_naming_no_layer_is_rejected() {
    let mut broken = style_json("dark");
    broken["metadata"]["squallar:added-layers"]
        .as_array_mut()
        .expect("added-layers")
        .push(json!({
            "id": "a_layer_that_does_not_exist",
            "source-layer": "poi",
            "reason": "a declaration with nothing behind it",
        }));
    let findings = check(&broken, &expectation());
    assert!(
        findings
            .iter()
            .any(|f| f.0.contains("no layer by that id exists")),
        "a declaration naming no layer was accepted: {findings:?}"
    );
}

/// An addition that does not declare itself is rejected.
///
/// The direction that matters most for hand-edited source: a layer appended to
/// `layers` with no matching `squallar:added-layers` entry makes the count
/// disagree, so the style cannot quietly grow. Together with
/// [`a_silently_dropped_layer_is_rejected`] this is the count check failing in
/// both directions, which it has to do or it catches half of what it looks
/// like it catches.
#[test]
fn an_undeclared_addition_is_rejected() {
    let mut broken = style_json("dark");
    let extra = broken["layers"][5].clone();
    broken["layers"].as_array_mut().expect("layers").push(extra);
    let findings = check(&broken, &expectation());
    assert!(
        findings.iter().any(|f| f.0.contains("layer count")),
        "an undeclared addition was accepted: {findings:?}"
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
/// This is what makes [`both_committed_styles_deserialise`] worth running:
/// `Style` is a `Vec` of an internally-tagged enum, so the parse is
/// all-or-nothing across all 95 layers rather than a per-layer best effort.
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

/// **A fill never draws over the line that shares its feature.**
///
/// `waterway` sat at index 6 and `water` at 9, so the water POLYGONS painted
/// over the river CENTRELINE. In dark the fill is `#2C353C` against a
/// `#3F5A6D` line on a `#0e0e0e` ground, so a reach wide enough to be a polygon
/// lost its bright centreline and read as a hole in the river. It got worse
/// with zoom because that is when the polygons arrive: measured over one
/// Oklahoma view, water polygons go 51 at z10 to 3,286 at z12, 26 to 45 km².
///
/// **Nothing here pinned draw order before this, which is why it shipped.**
/// Both committed styles parsed, every layer was accounted for, every filter key
/// existed -- and the map still drew a broken river, because order is not a
/// property any of those checks look at.
#[test]
fn water_polygons_draw_beneath_the_waterway_centreline() {
    for theme in ["dark", "light"] {
        let style = style_json(theme);
        let ids: Vec<&str> = style["layers"]
            .as_array()
            .expect("layers is an array")
            .iter()
            .map(|l| l["id"].as_str().expect("every layer has an id"))
            .collect();

        let at = |id: &str| {
            ids.iter()
                .position(|l| *l == id)
                .unwrap_or_else(|| panic!("{theme}: no layer {id}"))
        };

        for fill in ["water", "water_shadow"] {
            assert!(
                at(fill) < at("waterway"),
                "{theme}: {fill} is at {} and waterway at {}, so the fill paints \
                 over the centreline",
                at(fill),
                at("waterway")
            );
        }

        // NON-VACUITY: waterway must still be under its own label and under the
        // things that legitimately cover it, so this is an ordering constraint
        // rather than "push waterway to the end".
        assert!(
            at("waterway") < at("waterway_label"),
            "{theme}: the river label must draw over the river"
        );
    }
}

/// **The BasemapTiles inspector's toggle roster is the committed styles' own
/// source-layer census, in both directions.**
///
/// One toggle per source-layer some style layer actually references, so a
/// toggle cannot be a control that filters nothing; and every referenced
/// source-layer is either toggled or in the named exclusion list, so a style
/// edit that starts referencing a new source-layer cannot ship without the
/// inspector saying what the user may do about it. The exclusion —
/// [`squallar_egui::basemap_layer::UNTOGGLED_SOURCE_LAYERS`], `place` — is
/// itself pinned to be *referenced*: excluding a name the styles no longer
/// use would be a stale carve-out.
#[test]
fn the_source_layer_toggle_roster_matches_what_the_styles_reference() {
    use squallar_egui::basemap_layer::{
        SOURCE_LAYER_CONTROL_PREFIX, SOURCE_LAYER_TOGGLES, UNTOGGLED_SOURCE_LAYERS,
    };

    for theme in themes() {
        let style = style_json(theme);
        let referenced: BTreeSet<&str> = style["layers"]
            .as_array()
            .expect("layers is an array")
            .iter()
            .filter_map(|l| l["source-layer"].as_str())
            .collect();
        assert!(
            referenced.len() >= 10,
            "{theme}: non-triviality floor — the census found only {} \
             source-layers",
            referenced.len()
        );

        let toggled: BTreeSet<&str> = SOURCE_LAYER_TOGGLES
            .iter()
            .map(|t| t.source_layer)
            .collect();
        assert_eq!(
            toggled.len(),
            SOURCE_LAYER_TOGGLES.len(),
            "the toggle table names a source-layer twice"
        );

        // Direction one: every toggle filters something the styles draw.
        for toggle in &SOURCE_LAYER_TOGGLES {
            assert!(
                referenced.contains(toggle.source_layer),
                "{theme}: the {} toggle names a source-layer no style layer \
                 references — a control that visibly does nothing",
                toggle.source_layer
            );
            assert_eq!(
                toggle.control_id,
                format!("{SOURCE_LAYER_CONTROL_PREFIX}{}", toggle.source_layer),
                "the control id must be recoverable to the source-layer name"
            );
        }

        // Direction two: every referenced source-layer is toggled or
        // deliberately excluded, never silently absent.
        for source_layer in &referenced {
            assert!(
                toggled.contains(source_layer) || UNTOGGLED_SOURCE_LAYERS.contains(source_layer),
                "{theme}: the styles reference `{source_layer}` and the \
                 inspector neither toggles it nor names its exclusion"
            );
        }

        // The exclusion list is real and disjoint from the toggles.
        for excluded in UNTOGGLED_SOURCE_LAYERS {
            assert!(
                referenced.contains(excluded),
                "{theme}: `{excluded}` is excluded but no style layer \
                 references it — a stale carve-out"
            );
            assert!(
                !toggled.contains(excluded),
                "`{excluded}` is both toggled and excluded"
            );
        }
    }
}
