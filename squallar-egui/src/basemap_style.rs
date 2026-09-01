//! The two committed basemap styles, compiled in.
//!
//! `www/styles/{dark,light}.json` are the source of truth and are also served
//! to the web build from `www/`; the native build has no `www/` next to it, so
//! it carries them as `include_str!`. One pair of files, two ways in, and
//! `tests/committed_styles_parse.rs` gates the files themselves.
//!
//! Parsing is done **once per theme per process**, not per tile and not per
//! source construction. `Style::from_json` walks 95 internally-tagged layers
//! and every expression inside them; a tile fetch that repeated it would put
//! that on the IO path for every one of the tens of tiles a pane asks for, and
//! [`committed`] used to repeat it on the frame thread every time a layer
//! toggle rebuilt a source — **measured 0.44–0.70 ms per call, release,
//! 2026-08-31**, against a 4 ms frame budget. The `OnceLock` pair in
//! [`committed`] is what makes "once per theme" true rather than aspirational.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use walkers::Style;
use walkers::style::Layer;

/// The dark basemap style, verbatim.
const DARK_JSON: &str = include_str!("../../www/styles/dark.json");

/// The light basemap style, verbatim.
const LIGHT_JSON: &str = include_str!("../../www/styles/light.json");

/// The committed style for a theme.
///
/// # Panics
///
/// If the compiled-in JSON does not deserialise. That is unreachable in a
/// build that ran the test suite — `committed_styles_parse.rs` parses these two
/// files and asserts they yield more than 50 layers — and there is no useful
/// recovery from it: a basemap with no style draws a blank rectangle, which is
/// the silent-partial-success shape this workspace refuses.
pub fn committed(is_dark: bool) -> Arc<Style> {
    static DARK: OnceLock<Arc<Style>> = OnceLock::new();
    static LIGHT: OnceLock<Arc<Style>> = OnceLock::new();

    let (slot, name, json) = if is_dark {
        (&DARK, "dark", DARK_JSON)
    } else {
        (&LIGHT, "light", LIGHT_JSON)
    };

    Arc::clone(slot.get_or_init(|| {
        Arc::new(Style::from_json(json).unwrap_or_else(|error| {
            panic!(
                "the compiled-in {name} basemap style does not deserialise: {error}\n\
                 `Style` is one Vec<Layer> as an internally-tagged enum, so one bad \
                 layer fails the whole file. `tests/committed_styles_parse.rs` gates \
                 this and would have gone red first."
            )
        }))
    }))
}

/// The source-layer a style layer draws from, or `None` for the variants that
/// have no source-layer (`background`, `raster`, `fill-extrusion`) — those are
/// kept by every filter.
pub fn source_layer_of(layer: &Layer) -> Option<&str> {
    match layer {
        Layer::Fill { source_layer, .. }
        | Layer::Line { source_layer, .. }
        | Layer::Symbol { source_layer, .. }
        | Layer::Circle { source_layer, .. } => Some(source_layer),
        Layer::Background { .. } | Layer::Raster | Layer::FillExtrusion => None,
    }
}

/// The committed style for a theme with every style layer whose source-layer
/// is in `disabled` removed — what the tile source is built with, so a
/// disabled source-layer costs no decode and no paint at all rather than being
/// skipped per frame.
///
/// Re-parses the JSON when the filter is non-empty; that runs once per
/// **restyle** (a detail flip or a theme flip), never per tile or per frame.
/// Unlike [`committed`], this cannot be memoised behind a `OnceLock` — the
/// filter is a parameter and the result has to be an owned, mutated `Style` —
/// so the ~95-layer walk is the accepted cost on the frame thread here, at the
/// 0.44–0.70 ms [`committed`] records. It is only reached when the user has
/// actually turned a map-detail row off; the common path is the empty set,
/// which returns the memoised parse.
pub fn committed_filtered(is_dark: bool, disabled: &BTreeSet<String>) -> Arc<Style> {
    if disabled.is_empty() {
        return committed(is_dark);
    }
    let (name, json) = if is_dark {
        ("dark", DARK_JSON)
    } else {
        ("light", LIGHT_JSON)
    };
    let mut style = Style::from_json(json).unwrap_or_else(|error| {
        panic!(
            "the compiled-in {name} basemap style does not deserialise: {error}\n\
             `tests/committed_styles_parse.rs` gates this and would have gone red first."
        )
    });
    style
        .layers
        .retain(|layer| source_layer_of(layer).is_none_or(|sl| !disabled.contains(sl)));
    Arc::new(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second `committed` call re-uses the first parse instead of repeating it.
    ///
    /// **The frame-thread claim, as a property rather than a stopwatch.** Every
    /// layer toggle that rebuilds a tile source calls this from inside
    /// `Gui::ui`, and before the `OnceLock` pair each of those calls walked 95
    /// internally-tagged layers again — 0.44–0.70 ms of the 4 ms frame budget,
    /// measured release on 2026-08-31, for a result that cannot differ because
    /// the input is a `const &str`. `Arc::ptr_eq` is the whole assertion: same
    /// allocation means the same parse, and nothing here has to time anything.
    #[test]
    fn a_theme_is_parsed_once_and_then_shared() {
        for is_dark in [true, false] {
            let first = committed(is_dark);
            let second = committed(is_dark);
            assert!(
                Arc::ptr_eq(&first, &second),
                "committed(is_dark = {is_dark}) parsed the compiled-in style a \
                 second time; it is a `const &str`, so the second parse can only \
                 produce what the first did, on the frame thread, for nothing"
            );
        }

        assert!(
            !Arc::ptr_eq(&committed(true), &committed(false)),
            "the two themes share one cached style, so one of them is drawing \
             in the other's colours"
        );
    }

    /// The compiled-in bytes are the committed files, not a stale copy.
    ///
    /// `include_str!` is resolved at compile time from a path, so a divergence
    /// would need the file to change without a rebuild — but the two styles are
    /// also read from disk by `tests/committed_styles_parse.rs`, and a test
    /// that compared the parse results of two different sources of the same
    /// file is the only thing that holds the two entry points together.
    #[test]
    fn the_compiled_in_styles_are_the_committed_files() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("squallar-egui has a parent directory")
            .join("www/styles");

        for (name, compiled) in [("dark", DARK_JSON), ("light", LIGHT_JSON)] {
            let on_disk = std::fs::read_to_string(root.join(format!("{name}.json")))
                .expect("the committed style is readable");
            assert_eq!(
                compiled, on_disk,
                "the compiled-in {name} style is not the committed file"
            );
        }
    }

    /// Disabling a source-layer removes exactly its style layers and nothing
    /// else, in both themes; an empty filter is the unfiltered style; and the
    /// control — a source-layer NOT in the set — keeps its count untouched.
    #[test]
    fn the_filter_removes_exactly_the_disabled_source_layers_style_layers() {
        let count_of = |style: &Style, sl: &str| {
            style
                .layers
                .iter()
                .filter(|layer| source_layer_of(layer) == Some(sl))
                .count()
        };

        for is_dark in [true, false] {
            let full = committed(is_dark);
            let water_layers = count_of(&full, "water");
            let road_layers = count_of(&full, "transportation");
            assert!(
                water_layers > 0 && road_layers > 0,
                "non-vacuity: the committed styles reference both subjects"
            );

            let disabled: BTreeSet<String> = ["water".to_owned()].into();
            let filtered = committed_filtered(is_dark, &disabled);
            assert_eq!(
                filtered.layers.len(),
                full.layers.len() - water_layers,
                "exactly the water layers left, nothing else"
            );
            assert_eq!(count_of(&filtered, "water"), 0);
            assert_eq!(
                count_of(&filtered, "transportation"),
                road_layers,
                "an untouched source-layer keeps every style layer"
            );

            // Enabling restores: the empty set is the unfiltered style.
            let restored = committed_filtered(is_dark, &BTreeSet::new());
            assert_eq!(restored.layers.len(), full.layers.len());
        }
    }

    /// Every toggle in the shipped table names a source-layer the committed
    /// styles actually reference — a toggle that filters nothing is a control
    /// that visibly does nothing. (The other direction — every referenced
    /// source-layer has a toggle — lives in `tests/committed_styles_parse.rs`
    /// beside the styles' own gate.)
    #[test]
    fn every_shipped_toggle_filters_at_least_one_style_layer() {
        for is_dark in [true, false] {
            let full = committed(is_dark);
            for toggle in crate::basemap_layer::SOURCE_LAYER_TOGGLES {
                let disabled: BTreeSet<String> = [toggle.source_layer.to_owned()].into();
                let filtered = committed_filtered(is_dark, &disabled);
                assert!(
                    filtered.layers.len() < full.layers.len(),
                    "the {} toggle removed no style layer from the {} theme",
                    toggle.source_layer,
                    if is_dark { "dark" } else { "light" },
                );
            }
        }
    }

    /// Both themes parse to a real style, through the accessor the app calls.
    ///
    /// Non-vacuity: `{"layers":[]}` deserialises perfectly and draws nothing,
    /// so "it parsed" is not the assertion. 95 layers per theme as of
    /// 2026-08-28, counted off the committed files; the floor is far below that
    /// and far above zero, so a legitimate style edit does not redden it.
    /// `tests/committed_styles_parse.rs` pins the exact count.
    #[test]
    fn both_themes_yield_a_style_with_layers() {
        for is_dark in [true, false] {
            let style = committed(is_dark);
            assert!(
                style.layers.len() > 50,
                "the {} style yielded only {} layers",
                if is_dark { "dark" } else { "light" },
                style.layers.len()
            );
        }
    }
}
