//! **The one composition: every layer the app has, from the crates that own
//! them.**

use rustdar_source::handler::SourceHandler;
use rustdar_source::id::LayerId;

/// Every layer the app registers, in registration order: this crate's own two
/// providers chained, overlays first.
pub fn all() -> Vec<Box<dyn SourceHandler>> {
    rustdar_overlays::render::handlers::sources()
        .into_iter()
        .chain(rustdar_radar::source::sources())
        .collect()
}

/// The default draw order, bottom to top — every registered layer's id sorted
/// by `SourceHandler::draw_order_weight`.
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
