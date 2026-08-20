//! **The one composition: every layer the app has, from the crates that own
//! them.**
//!
//! Since WO-M9 a layer is a `rustdar_source::handler::SourceHandler` living in
//! the crate that owns its data — the eleven overlay layers in
//! `rustdar_overlays::render::handlers::sources`, the radar layer in
//! `rustdar_radar::source::sources`. Neither of those crates can see the other
//! (the WO-M3 charter cuts the overlays -> radar edge and
//! `rustdar-source`'s `tests/charter.rs` keeps it cut), so the whole set exists
//! only where both are in scope. [`all`] is that place, and it is the only
//! provider list outside the source crates.
//!
//! # Why here rather than in `rustdar-app`
//!
//! WO-M9 ordered this into the app crate, and it does not fit there: this crate
//! is *below* `rustdar-app` and has two production readers of the whole set
//! that the app cannot reach down into.
//!
//! * [`default_draw_order`] backs `#[serde(default = "KindList::
//!   default_draw_order")]` in `ui::config` — a config file with no
//!   `draw_order` key reads its stacking order from there, and a serde default
//!   takes no arguments and has no registry in reach. An eleven-row answer
//!   would quietly drop radar out of every fresh pane's layer stack and out of
//!   the catalogue built from it.
//! * `Gui::new` builds the registry, and it has 155 call sites. Threading the
//!   list through them is a sweep, not a move — and `rustdar-web`, which boots
//!   the browser build, could not supply one anyway.
//!
//! This crate already declares both source crates as dependencies, so hosting
//! the composition here adds no edge. `rustdar-app` reads it as
//! `rustdar_egui::sources::all()`.

use rustdar_source::handler::SourceHandler;
use rustdar_source::id::LayerId;

/// Every layer the app registers, in registration order: this crate's own two
/// providers chained, overlays first.
///
/// The order here is registration order and **not** draw order — those are two
/// different orders and always have been (the vec puts radar last while the
/// draw order puts it third). [`default_draw_order`] is the draw one, derived
/// from the weights each handler declares.
pub fn all() -> Vec<Box<dyn SourceHandler>> {
    rustdar_overlays::render::handlers::sources()
        .into_iter()
        .chain(rustdar_radar::source::sources())
        .collect()
}

/// The default draw order, bottom to top — every registered layer's id sorted
/// by `SourceHandler::draw_order_weight`.
///
/// The free-function twin of `OverlayRegistry::default_draw_order`, for the
/// callers (fresh-pane construction, the config-absent serde default) that have
/// no live registry in reach. Never a literal list: the weights are the one
/// spelling of the order, and the literal-list pin in
/// `registry_identity_tests` below is what holds them to the order users have
/// always seen.
pub fn default_draw_order() -> Vec<LayerId> {
    let mut handlers = all();
    handlers.sort_by_key(|h| h.draw_order_weight());
    handlers.iter().map(|h| h.id()).collect()
}

#[cfg(test)]
mod controls_parity_tests {
    use rustdar_overlays::render::controls::{ControlItem, PaneControlContext};
    use rustdar_overlays::render::overlay_state::OverlayRegistry;
    use rustdar_source::id::LayerId;

    use super::all;

    /// A control's identity, stripped of its live values. The *set of
    /// options offered* is what must not depend on state; a toggle's
    /// checked-ness, a dropdown's selection and a slider's value
    /// legitimately do.
    fn shape(item: &ControlItem) -> String {
        match item {
            ControlItem::Toggle { id, label, .. } => format!("toggle:{id}:{label}"),
            ControlItem::Dropdown { id, label, .. } => format!("dropdown:{id}:{label}"),
            ControlItem::Slider { id, label, .. } => format!("slider:{id}:{label}"),
            ControlItem::ButtonRow { buttons } => {
                let ids: Vec<&str> = buttons.iter().map(|b| b.id).collect();
                format!("buttons:{}", ids.join(","))
            }
            ControlItem::InfoText { text } => format!("info:{text}"),
            ControlItem::Heading { text } => format!("heading:{text}"),
            ControlItem::Section { label, items, .. } => {
                let children: Vec<String> = items.iter().map(shape).collect();
                format!("section:{label}[{}]", children.join(";"))
            }
            ControlItem::Separator => "separator".into(),
        }
    }

    /// Every handler offers the identical control tree hidden and shown —
    /// the every-option rule: the stack row's eye hides *pixels*, never
    /// options. A handler whose disabled tree shrank stranded its
    /// sub-options exactly when a user goes looking for why a layer is off
    /// or what it will show once on (the M9.1 user report), so each of the
    /// twelve is pinned by name.
    #[test]
    fn every_handlers_control_tree_is_identical_hidden_and_shown() {
        let mut registry = OverlayRegistry::with_handlers(all());
        let kinds: Vec<LayerId> = registry.handlers().map(|h| h.id()).collect();
        assert_eq!(
            kinds.len(),
            12,
            "the registry carries all twelve handlers - the walk below \
             must cover every one"
        );
        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        for kind in kinds {
            registry.set_enabled(&kind, true);
            let shown: Vec<String> = registry.controls(&kind, &ctx).iter().map(shape).collect();
            registry.set_enabled(&kind, false);
            let hidden: Vec<String> = registry.controls(&kind, &ctx).iter().map(shape).collect();
            assert_eq!(
                shown, hidden,
                "{kind:?} offers a different option set hidden than shown - \
                 the eye must change pixels, never the options"
            );
        }
    }
}

#[cfg(test)]
mod state_key_tests {
    use super::all;

    /// Every name saved handler state has ever been filed under, as a
    /// **literal** list — the self-verifying-inventory discipline: the live
    /// set is checked against it below, so neither side can rot alone.
    const STATE_KEYS: [&str; 12] = [
        "ModelData",
        "SpcOutlook",
        "SpcDiscussions",
        "NwsAlerts",
        "StormReports",
        "Lightning",
        "Metar",
        "Radar",
        "CityLabels",
        "RadarSites",
        "UserLocation",
        "ColorScale",
    ];

    /// **The tripwire on the bytes saved handler state is filed under.**
    ///
    /// `serialize_handler_states` and `deserialize_handler_states` key the
    /// saved state by `h.id().as_str()` since M8b-b1 (the two
    /// `format!("{:?}")` sites this test was written against are gone — m1).
    /// Those keys are already sitting in every user's config file, so a
    /// handler whose id drifts from the name below silently stops matching
    /// the file and that user's saved state for the layer is orphaned
    /// without a single error. This test is what fails instead: the live
    /// twelve and the literal twelve are compared as sets in **both**
    /// directions, so a rename fails on the live side and a retirement
    /// fails on the pinned side.
    ///
    /// **What this test cannot see, and what does:** two handlers *swapping*
    /// ids leaves this set equal. The cross-check that used to catch it read
    /// each handler's `kind()` — a second, independent spelling of its
    /// identity, deleted at M8b-b3 with the enum bridge. The pin that
    /// catches a swap now is
    /// `registry_identity_tests::draw_order_weights_encode_the_default_draw_order`:
    /// weights are pinned unique and each id is pinned to its weight's
    /// position in the literal draw order, so swapping any two ids moves
    /// both in the weight-sorted list.
    ///
    /// New persistence code must key by [`LayerId::as_str`], never by a new
    /// `{:?}` site (`LayerId`'s derived `Debug` prints `LayerId("…")` —
    /// visibly wrong on purpose).
    ///
    /// [`LayerId::as_str`]: rustdar_source::id::LayerId::as_str
    #[test]
    fn handler_state_keys_are_the_twelve_names_saved_configs_file_state_under() {
        let handlers = all();
        assert_eq!(
            handlers.len(),
            STATE_KEYS.len(),
            "a handler was registered or retired without updating the literal \
             key list; saved state for it has no pinned spelling",
        );
        let mut live: Vec<String> = handlers
            .iter()
            .map(|h| h.id().as_str().to_string())
            .collect();
        live.sort_unstable();
        let mut pinned: Vec<String> = STATE_KEYS.iter().map(|k| (*k).to_string()).collect();
        pinned.sort_unstable();
        assert_eq!(
            live, pinned,
            "the registered ids are no longer exactly the twelve names saved \
             configs file handler state under — a rename or a retirement \
             orphans every user's saved state for that layer",
        );
    }
}

#[cfg(test)]
mod registry_identity_tests {
    use rustdar_source::id::LAYER_ID_LEDGER;

    use super::all;

    /// b1 pin: no two handlers answer the same id. The open string has no
    /// compiler to refuse a duplicate the way the enum's match arms did, so
    /// the registry pins uniqueness instead — the replacement rigor the M8c
    /// enum deletion depends on.
    #[test]
    fn no_two_handlers_share_an_id() {
        let handlers = all();
        assert_eq!(handlers.len(), 12, "the walk below must cover all twelve");
        let mut seen = std::collections::HashSet::new();
        for h in &handlers {
            assert!(
                seen.insert(h.id()),
                "two handlers both register {:?} — the second shadows the \
                 first at every registry lookup",
                h.id(),
            );
        }
    }

    /// b1 pin: every handler's id sits in the append-only ledger — a handler
    /// cannot register a spelling `LAYER_ID_LEDGER` does not carry.
    #[test]
    fn every_handlers_id_sits_in_the_ledger() {
        for h in &all() {
            assert!(
                LAYER_ID_LEDGER.contains(&h.id().as_str()),
                "{}'s id is missing from LAYER_ID_LEDGER — ledger rows are \
                 append-only and this one was never appended",
                h.display_name(),
            );
        }
    }

    /// **The draw-weight order pin.** Sorting the registered handlers by
    /// `draw_order_weight` yields EXACTLY the historical default draw order,
    /// bottom to top, spelled out as literals.
    ///
    /// Two orders exist here and only this one is the draw order:
    /// the composition's order differs (radar is chained on last,
    /// while the draw order puts SpcOutlook BELOW Radar). This literal list
    /// is the pin that keeps a weight edit from silently reordering what
    /// occludes what on every user's map.
    ///
    /// Since WO-M8c it is also the anti-swap pin: weights are unique and each
    /// id is pinned to its weight's position, so two handlers exchanging ids
    /// move both in this list. Nothing else can see that swap — the second,
    /// independent spelling of a handler's identity died with the layer enum.
    #[test]
    fn draw_order_weights_encode_the_default_draw_order() {
        let mut handlers = all();
        let mut weights: Vec<u32> = handlers.iter().map(|h| h.draw_order_weight()).collect();
        weights.sort_unstable();
        weights.dedup();
        assert_eq!(
            weights.len(),
            handlers.len(),
            "two handlers share a draw-order weight — their relative order \
             would be an accident of registration order",
        );
        handlers.sort_by_key(|h| h.draw_order_weight());
        let ids: Vec<String> = handlers
            .iter()
            .map(|h| h.id().as_str().to_string())
            .collect();
        assert_eq!(
            ids,
            [
                "ModelData",
                "SpcOutlook",
                "Radar",
                "SpcDiscussions",
                "NwsAlerts",
                "StormReports",
                "Lightning",
                "Metar",
                "CityLabels",
                "RadarSites",
                "UserLocation",
                "ColorScale",
            ],
            "the weight order drifted from the historical default draw order",
        );
    }
}
