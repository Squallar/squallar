//! The converter's own refusals, and nothing else.
//!
//! Every test that read `www/styles/*.json` off disk moved to
//! `squallar-egui/tests/committed_styles_parse.rs` on 2026-08-28. Those are a
//! gate on files this workspace now hand-edits, so they belong where
//! `cargo test --workspace` selects them; this crate is a one-time converter
//! standing outside the root workspace, and a gate parked here runs under
//! nothing.
//!
//! What stays is the historical record: the four tests that call [`convert`]
//! and never open a committed style. The conversion ran once, on 2026-08-27,
//! and will not run again -- these pin how it behaved for whoever points it at
//! another CARTO revision.
//!
//! `basemap_style::check` is still exercised, and somewhere that runs: the
//! moved suite compiles this crate's `src/lib.rs` in directly with `#[path]`,
//! so there is exactly one `check`. Twelve tests call it there -- one passing
//! it both committed styles, and eleven handing it a single planted defect and
//! requiring rejection.

use basemap_style::ABSENT_FROM_OMT;
use serde_json::json;

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
