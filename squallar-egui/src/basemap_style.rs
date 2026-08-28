//! The two committed basemap styles, compiled in.
//!
//! `www/styles/{dark,light}.json` are the source of truth and are also served
//! to the web build from `www/`; the native build has no `www/` next to it, so
//! it carries them as `include_str!`. One pair of files, two ways in, and
//! `tests/committed_styles_parse.rs` gates the files themselves.
//!
//! Parsing is done **once per theme, at construction**, not per tile.
//! `Style::from_json` walks 92 internally-tagged layers and every expression
//! inside them; a tile fetch that repeated it would put that on the IO path for
//! every one of the tens of tiles a pane asks for.

use std::sync::Arc;

use walkers::Style;

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
    let (name, json) = if is_dark {
        ("dark", DARK_JSON)
    } else {
        ("light", LIGHT_JSON)
    };

    Arc::new(Style::from_json(json).unwrap_or_else(|error| {
        panic!(
            "the compiled-in {name} basemap style does not deserialise: {error}\n\
             `Style` is one Vec<Layer> as an internally-tagged enum, so one bad \
             layer fails the whole file. `tests/committed_styles_parse.rs` gates \
             this and would have gone red first."
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Both themes parse to a real style, through the accessor the app calls.
    ///
    /// Non-vacuity: `{"layers":[]}` deserialises perfectly and draws nothing,
    /// so "it parsed" is not the assertion. 92 layers per theme as of
    /// 2026-08-28; the floor is far below that and far above zero, so a
    /// legitimate style edit does not redden it.
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
