//! The two committed basemap styles — **re-exported from
//! [`squallar_basemap::style`], which owns them now.**
//!
//! They moved when the tile pump's parse-and-tessellate moved to the worker.
//! The worker composes `squallar-basemap`'s registry row and runs `styled`
//! itself, so it needs the same two files; leaving a copy here would compile
//! `www/styles/{dark,light}.json` into the binary **twice**, 221,904 bytes of
//! duplicated JSON in a wasm bundle, and would give the page and the worker
//! two independent `OnceLock`s over identical input.
//!
//! This module stays as the name every call site in this crate already uses.
//! Nothing about the styles changed in the move — `squallar-basemap`'s
//! `style.rs` is this file's body, and `tests/committed_styles_parse.rs` still
//! gates the files themselves from here.

pub use squallar_basemap::style::{committed, committed_filtered, source_layer_of};

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

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
}
