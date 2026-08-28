//! The committed basemap styles still deserialise, and still name real layers.
//!
//! `www/styles/{dark,light}.json` are shipped to users and edited by hand. They
//! were produced once by `tools/basemap-style`, which is its own workspace root
//! and so is reached by no root gate -- and the workflow that used to run its 29
//! tests was deleted in `d1517cb2` as a recurring gate on a one-time job. That
//! deletion was right, and it left these two files checked by nothing at all.
//!
//! **The gate belongs here, not there.** A converter's CI could only ever prove
//! the converter still works; what users would notice is a style that stops
//! parsing, and the crate that loads styles is this one.
//!
//! The failure being guarded is quiet by construction. `Style` is one
//! `Vec<Layer>` deserialised as an internally-tagged enum, so **a single
//! malformed layer fails the entire parse** -- the defect that forced vendoring
//! in the first place, where `Circle` was missing its `rename_all` and CARTO's
//! styles would not load at all. A hand edit that mistypes one `source-layer`
//! takes the whole basemap with it, and nothing before this test would have
//! said so.

use std::collections::BTreeSet;
use std::path::PathBuf;

use walkers::style::{Layer, Style};

/// The two committed styles, by path.
///
/// Resolved from `CARGO_MANIFEST_DIR` rather than the working directory, so the
/// test behaves the same under `cargo test` from anywhere in the workspace.
fn style_paths() -> Vec<(&'static str, PathBuf)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("squallar-egui has a parent directory")
        .join("www/styles");
    vec![
        ("dark", root.join("dark.json")),
        ("light", root.join("light.json")),
    ]
}

/// Every source layer the OpenMapTiles schema defines. Measured from real tiles
/// and confirmed against a planetiler build of our own: the schema is exactly
/// these 16, and an archive contains a subset (Monaco has 15 -- no aerodrome).
///
/// A style naming anything outside this set is drawing nothing, silently: the
/// renderer looks the layer up by name, finds no such source layer, and skips
/// it. That is invisible on screen unless you already know what should be there.
const OMT_SOURCE_LAYERS: &[&str] = &[
    "aerodrome_label",
    "aeroway",
    "boundary",
    "building",
    "housenumber",
    "landcover",
    "landuse",
    "mountain_peak",
    "park",
    "place",
    "poi",
    "transportation",
    "transportation_name",
    "water",
    "water_name",
    "waterway",
];

/// The source layer a layer draws from, or `None` for the ones that have none.
fn source_layer(layer: &Layer) -> Option<&str> {
    match layer {
        Layer::Fill { source_layer, .. }
        | Layer::Line { source_layer, .. }
        | Layer::Symbol { source_layer, .. }
        | Layer::Circle { source_layer, .. } => Some(source_layer),
        Layer::Background { .. } | Layer::Raster | Layer::FillExtrusion => None,
    }
}

fn load(name: &str, path: &PathBuf) -> Style {
    let json = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "{name} style is missing at {}: {e}\n\
             These files are committed source, not build output.",
            path.display()
        )
    });
    Style::from_json(&json).unwrap_or_else(|e| {
        panic!(
            "{name} style at {} no longer deserialises: {e}\n\
             `Style` is one Vec<Layer> as an internally-tagged enum, so ONE bad \
             layer fails the whole file and the basemap draws nothing.",
            path.display()
        )
    })
}

#[test]
fn both_committed_styles_deserialise() {
    for (name, path) in style_paths() {
        let style = load(name, &path);

        // NON-VACUITY. A bare "it parsed" would pass on `{"layers":[]}`, which
        // deserialises perfectly and renders an empty map. The floor is not a
        // pinned count -- that would redden on every legitimate style edit --
        // but a threshold far below the current 92 and far above zero.
        assert!(
            style.layers.len() > 50,
            "{name} parsed but yielded only {} layers. A style that parses and \
             draws nothing is the failure this test exists for, not a pass.",
            style.layers.len()
        );
    }
}

#[test]
fn every_styled_source_layer_exists_in_the_schema() {
    let known: BTreeSet<&str> = OMT_SOURCE_LAYERS.iter().copied().collect();

    for (name, path) in style_paths() {
        let style = load(name, &path);

        let used: BTreeSet<&str> = style.layers.iter().filter_map(source_layer).collect();

        let unknown: Vec<&str> = used.difference(&known).copied().collect();
        assert!(
            unknown.is_empty(),
            "{name} references source layers that OpenMapTiles does not define: {unknown:?}\n\
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
            "{name} draws from only {} source layers ({used:?}). The check above \
             passes vacuously on an empty set, so this is what keeps it honest.",
            used.len()
        );
    }
}
