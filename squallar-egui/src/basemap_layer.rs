//! The base map tiles as a registered layer: identity, toggle and the
//! per-source-layer visibility choices.
//!
//! The pixels move through the same machinery as ever — `tiles::MapTileState`
//! owns the source, `tile_source` fetches and decodes, and the `BasemapTiles`
//! arm of `ui_map_pane`'s layer walk draws — this handler is what makes the
//! ground a Layers-panel citizen instead of a hardcoded special case. It is
//! the CityLabels/Terrain shape (toggle only, `RenderMode::Tile`, `Surface::
//! Ground`, `TimeAxis::Live`) plus one thing of its own: the set of OMT
//! source-layers the user has switched off, which
//! [`crate::basemap_style::committed_filtered`] bakes out of the style the
//! source is built with.
//!
//! **The disabled set is layer-global, not per-pane, by construction**: every
//! pane draws from the one shared tile source, so a per-pane choice could not
//! be honoured without one source per pane. The frame loop reads the set back
//! off the layer's declared control surface ([`disabled_from_controls`]) —
//! `as_any` on `SourceHandler` is refused by ruling, and the control tree is
//! the sanctioned door to a handler's own fields.

use std::collections::BTreeSet;
use std::sync::Arc;

use squallar_source::controls::{ControlEffect, ControlItem, ControlUpdate, ControlValue};
use squallar_source::handler::{
    FetchPayload, OverlayItem, PaneMut, PaneRef, PaneToggle, RenderMode, SourceHandler, Surface,
};
use squallar_source::id::{LayerId, known};
use squallar_source::time::TimeAxis;

/// The key the disabled set is filed under in the handler's saved state.
const DISABLED_KEY: &str = "disabled_source_layers";

/// One per-source-layer visibility toggle the BasemapTiles inspector offers.
///
/// `control_id` is the [`squallar_source::controls::ControlItem::Toggle`] id
/// (a `'static` requirement of the control vocabulary), spelled as the
/// source-layer name behind [`SOURCE_LAYER_CONTROL_PREFIX`] so a reader of the
/// control surface can recover the source-layer without a second table.
pub struct SourceLayerToggle {
    pub control_id: &'static str,
    pub source_layer: &'static str,
    pub label: &'static str,
    pub default_on: bool,
}

/// The prefix every source-layer toggle's control id carries, so the frame
/// loop can read the disabled set back off the layer's **declared control
/// surface** — the one sanctioned door to a handler's own fields (`as_any` on
/// `SourceHandler` is refused by ruling; see `gui/layer_glue.rs`).
pub const SOURCE_LAYER_CONTROL_PREFIX: &str = "sl:";

/// The source-layer whose toggle is deliberately NOT offered: `place`.
///
/// Every `place` style layer is a symbol layer, and a symbol layer's entire
/// output is deferred text drawn by the **CityLabels** layer's arm of the pane
/// walk — the ground phase paints zero pixels for it. A `place` toggle in the
/// BasemapTiles inspector would therefore be a switch whose whole visible
/// effect lands on a different layer's pixels, duplicating the CityLabels
/// toggle that already governs those names. The roster pin in
/// `tests/committed_styles_parse.rs` carries this exclusion by name.
pub const UNTOGGLED_SOURCE_LAYERS: [&str; 1] = ["place"];

/// Every source-layer the committed styles reference, minus
/// [`UNTOGGLED_SOURCE_LAYERS`], each with its inspector label and shipped
/// default.
///
/// **This table cannot drift from the styles**: `tests/committed_styles_parse.rs`
/// pins both directions against the committed JSON — every toggle names a
/// source-layer some style layer references, and every referenced source-layer
/// is either here or in [`UNTOGGLED_SOURCE_LAYERS`].
///
/// `poi` and `building` ship OFF by the plan's own table; everything else
/// ships ON.
pub const SOURCE_LAYER_TOGGLES: [SourceLayerToggle; 15] = [
    SourceLayerToggle {
        control_id: "sl:aerodrome_label",
        source_layer: "aerodrome_label",
        label: "Airport labels",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:aeroway",
        source_layer: "aeroway",
        label: "Runways",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:boundary",
        source_layer: "boundary",
        label: "Boundaries",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:building",
        source_layer: "building",
        label: "Buildings",
        default_on: false,
    },
    SourceLayerToggle {
        control_id: "sl:housenumber",
        source_layer: "housenumber",
        label: "House numbers",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:landcover",
        source_layer: "landcover",
        label: "Land cover",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:landuse",
        source_layer: "landuse",
        label: "Land use",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:mountain_peak",
        source_layer: "mountain_peak",
        label: "Mountain peaks",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:park",
        source_layer: "park",
        label: "Parks",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:poi",
        source_layer: "poi",
        label: "Points of interest",
        default_on: false,
    },
    SourceLayerToggle {
        control_id: "sl:transportation",
        source_layer: "transportation",
        label: "Roads",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:transportation_name",
        source_layer: "transportation_name",
        label: "Road names",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:water",
        source_layer: "water",
        label: "Water",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:water_name",
        source_layer: "water_name",
        label: "Water names",
        default_on: true,
    },
    SourceLayerToggle {
        control_id: "sl:waterway",
        source_layer: "waterway",
        label: "Waterways",
        default_on: true,
    },
];

/// The source-layers the shipped defaults exclude — the toggles whose
/// `default_on` is false, as the set every fresh install and every config that
/// predates the toggles filters with.
pub fn default_disabled_source_layers() -> BTreeSet<String> {
    SOURCE_LAYER_TOGGLES
        .iter()
        .filter(|toggle| !toggle.default_on)
        .map(|toggle| toggle.source_layer.to_owned())
        .collect()
}

/// The base tiles: per-pane visibility through `PaneToggle` like CityLabels
/// and Terrain, plus the layer-global disabled source-layer set.
pub(crate) struct BasemapTilesHandler {
    pub enabled: bool,
    /// OMT source-layers the ground draw excludes — the complement of the
    /// inspector's toggles, kept as the *disabled* set so its serialized form
    /// is empty-of-surprises: a config that has never touched a toggle whose
    /// default is ON carries nothing about it.
    disabled_source_layers: BTreeSet<String>,
}

impl BasemapTilesHandler {
    pub fn new() -> Self {
        Self {
            // ON by default and ships enabled: the base map is what every
            // existing user already sees, and a fresh install must show it
            // without touching anything.
            enabled: true,
            disabled_source_layers: default_disabled_source_layers(),
        }
    }
}

/// The disabled source-layer set, read off the layer's declared control
/// surface: every source-layer toggle that is off, by the name its control id
/// carries behind [`SOURCE_LAYER_CONTROL_PREFIX`].
pub(crate) fn disabled_from_controls(controls: &[ControlItem]) -> BTreeSet<String> {
    controls
        .iter()
        .filter_map(|item| match item {
            ControlItem::Toggle {
                id, enabled: false, ..
            } => id
                .strip_prefix(SOURCE_LAYER_CONTROL_PREFIX)
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

impl SourceHandler for BasemapTilesHandler {
    fn id(&self) -> LayerId {
        known::BASEMAP_TILES
    }

    /// The archive is one compiled-in generation; the ground it draws does
    /// not move with the clock.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    /// The bottom of the stack, under Terrain's 2: the ground everything else
    /// paints over.
    fn draw_order_weight(&self) -> u32 {
        1
    }
    fn display_name(&self) -> &str {
        "Base Map"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Tile
    }
    fn default_enabled(&self) -> bool {
        true
    }
    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        PaneToggle::is_on(pane, self.enabled)
    }
    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        if !PaneToggle::set(pane, enabled) {
            self.enabled = enabled;
        }
    }

    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _fetching: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }

    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(&self, _selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {}

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let mut items = vec![
            ControlItem::Toggle {
                id: "enabled",
                label: "Base Map".to_string(),
                enabled: self.is_enabled(pane),
            },
            ControlItem::Heading {
                text: "Map detail".to_string(),
            },
        ];
        items.extend(
            SOURCE_LAYER_TOGGLES
                .iter()
                .map(|toggle| ControlItem::Toggle {
                    id: toggle.control_id,
                    label: toggle.label.to_string(),
                    enabled: !self.disabled_source_layers.contains(toggle.source_layer),
                }),
        );
        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        let ControlValue::Bool(val) = update.value else {
            return ControlEffect::None;
        };
        if update.id == "enabled" {
            if !PaneToggle::set(pane, val) {
                self.enabled = val;
            }
        } else if let Some(source_layer) = update.id.strip_prefix(SOURCE_LAYER_CONTROL_PREFIX) {
            // The frame loop notices the set changed and re-styles the live
            // tile source from its parsed-geometry cache
            // (`MapTileState::ensure_base_tiles` -> `HttpsTiles::set_style`)
            // — zero fetches, zero re-parses; nothing to signal here.
            if val {
                self.disabled_source_layers.remove(source_layer);
            } else {
                self.disabled_source_layers.insert(source_layer.to_owned());
            }
        }
        ControlEffect::None
    }

    // Per-pane state is the visibility toggle, exactly the CityLabels shape;
    // the disabled set is layer-global and travels through
    // `serialize_state`/`deserialize_state` instead.

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        PaneToggle::create(enabled)
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        PaneToggle::restore(&value, enabled)
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        PaneToggle::save(state)
    }

    fn serialize_state(&self) -> serde_json::Value {
        // Null at the shipped defaults, so a user who never touched a toggle
        // writes nothing — the byte-preservation pin on downgrade fixtures
        // (`ui_config/fixture_tests.rs`) rests on untouched state staying
        // unwritten. An EMPTY set is not the default set and is written: it is
        // the user switching everything on.
        if self.disabled_source_layers == default_disabled_source_layers() {
            return serde_json::Value::Null;
        }
        serde_json::json!({
            DISABLED_KEY: self.disabled_source_layers.iter().collect::<Vec<_>>()
        })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        // The key's absence — every config written before the toggles existed
        // — keeps the defaults; a present list is the user's whole choice,
        // including an empty one (everything switched on).
        if let Some(list) = value.get(DISABLED_KEY).and_then(|v| v.as_array()) {
            self.disabled_source_layers = list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handler declares the ground's facts: the ledger spelling, tile
    /// render mode, ground surface, the bottom weight, and ON by default —
    /// ships-enabled is what makes an old config sprout the row.
    #[test]
    fn the_handler_declares_the_grounds_facts() {
        let handler = BasemapTilesHandler::new();
        assert_eq!(handler.id(), known::BASEMAP_TILES);
        assert_eq!(handler.render_mode(), RenderMode::Tile);
        assert_eq!(handler.surface(), Surface::Ground);
        assert!(handler.default_enabled(), "the base map ships ON");
        assert!(handler.is_enabled(&PaneRef::bare(0)));
        assert_eq!(handler.time_axis(), TimeAxis::Live);
        assert!(
            handler.draw_order_weight() < 2,
            "the ground draws under Terrain's 2"
        );
    }

    /// Flipping a source-layer toggle moves exactly that name in and out of
    /// the disabled set, read back through the declared control surface — the
    /// same door the frame loop uses. Control: a toggle not touched keeps its
    /// state.
    #[test]
    fn a_source_layer_toggle_moves_exactly_its_name() {
        let mut handler = BasemapTilesHandler::new();
        let before = disabled_from_controls(&handler.controls(&PaneRef::bare(0)));
        assert_eq!(
            before,
            default_disabled_source_layers(),
            "the control surface starts at the shipped defaults"
        );
        assert!(
            !before.contains("water"),
            "precondition: water ships ON, so the flip below is a real change"
        );

        // A bare pane: the source-layer toggles are layer-global, so no
        // per-pane state is involved in the edit.
        let mut view = PaneMut::bare(0);
        handler.apply_control(
            &ControlUpdate {
                id: "sl:water",
                value: ControlValue::Bool(false),
            },
            &mut view,
        );

        let after = disabled_from_controls(&handler.controls(&PaneRef::bare(0)));
        let mut expected = before.clone();
        expected.insert("water".to_owned());
        assert_eq!(after, expected, "exactly `water` joined the disabled set");

        handler.apply_control(
            &ControlUpdate {
                id: "sl:water",
                value: ControlValue::Bool(true),
            },
            &mut view,
        );
        assert_eq!(
            disabled_from_controls(&handler.controls(&PaneRef::bare(0))),
            before,
            "switching it back on restores the set exactly"
        );
    }

    /// The disabled set survives the save/load round trip — including an
    /// empty set, which is a choice (everything on) and not an absence — and
    /// a state with no key keeps the defaults.
    #[test]
    fn the_disabled_set_round_trips_and_absence_means_defaults() {
        assert!(
            BasemapTilesHandler::new().serialize_state().is_null(),
            "untouched state writes nothing — the downgrade byte-preservation \
             pin rests on it"
        );

        let mut handler = BasemapTilesHandler::new();
        handler.disabled_source_layers = ["boundary".to_owned(), "park".to_owned()]
            .into_iter()
            .collect();
        let saved = handler.serialize_state();

        let mut restored = BasemapTilesHandler::new();
        restored.deserialize_state(saved);
        assert_eq!(
            restored.disabled_source_layers,
            handler.disabled_source_layers
        );

        // The empty set is a statement, not a default.
        handler.disabled_source_layers.clear();
        let saved = handler.serialize_state();
        let mut restored = BasemapTilesHandler::new();
        restored.deserialize_state(saved);
        assert!(
            restored.disabled_source_layers.is_empty(),
            "a user who switched everything on must not get poi/building \
             re-disabled on reopen"
        );

        // No key at all — the pre-toggle config — keeps the shipped defaults.
        let mut fresh = BasemapTilesHandler::new();
        fresh.deserialize_state(serde_json::json!({}));
        assert_eq!(
            fresh.disabled_source_layers,
            default_disabled_source_layers()
        );
    }
}
