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

/// **How many layers this build registers — a hand-kept number, and it must
/// stay one.**
///
/// This is the *second spelling* of the registry's size, and the whole point
/// of it is that it does not come from [`all`]. WO-M9 built the catalog leg's
/// anti-shrink floor as two spellings maintained on opposite sides of one
/// question, so they cannot shrink together by accident; the first spelling is
/// the derived inventory the parity walk builds out of the live registry, and
/// this is the one a person has to change on purpose.
///
/// **Never derive this from `all()`, `sources()`, `LAYER_ID_LEDGER::len()` or
/// anything else that moves when a registration moves.** A floor computed from
/// the thing it is meant to floor compares the registry against itself and
/// cannot fail — the shape that cost this campaign an entire class of pins.
///
/// The `cfg!` term is the fake source, which is registered only under
/// `rustdar-overlays/fake-source`.
#[cfg(test)]
pub(crate) const REGISTERED_LAYER_COUNT: usize = 12 + cfg!(feature = "fake-source") as usize;

/// The default draw order, bottom to top — every registered layer's id sorted
/// by `SourceHandler::draw_order_weight`.
pub fn default_draw_order() -> Vec<LayerId> {
    let mut handlers = all();
    handlers.sort_by_key(|h| h.draw_order_weight());
    handlers.iter().map(|h| h.id()).collect()
}

#[cfg(test)]
mod retry_ledger_tests {
    use rustdar_source::fetch_policy::FetchError;
    use rustdar_source::id::{LayerId, known};

    use super::all;

    /// The composed twin of `rustdar-overlays`' own
    /// `every_auto_polling_handler_backs_off_after_a_failure`, which iterates
    /// that crate's six and **cannot see the radar layer** — post-M9 it lives
    /// in `rustdar-radar` and is chained in here. The overlays literal stays
    /// six and is about that crate's own registry; this one is about the app's.
    ///
    /// Radar is named as well as counted: a count alone would go on reading 7
    /// if one layer stopped polling and another started.
    #[test]
    fn every_auto_polling_layer_backs_off_after_a_failure() {
        let mut checked: Vec<LayerId> = Vec::new();
        for handler in all().iter_mut() {
            let Some(interval) = handler.auto_poll_interval() else {
                continue;
            };
            checked.push(handler.id());
            let id = handler.id();
            let id = id.as_str();

            assert!(
                handler.retry().is_some(),
                "{id} auto-polls every {interval}s but keeps no retry ledger, \
                 so a failed fetch leaves it due on every frame",
            );
            assert_eq!(
                handler.auto_fetch_delay(),
                Some(std::time::Duration::ZERO),
                "{id} has never been fetched, so it is due now",
            );

            handler
                .retry_mut()
                .expect("just asserted present")
                .record_failure(&FetchError::transient("network down"));

            let delay = handler
                .auto_fetch_delay()
                .expect("a transient failure is still owed an eventual retry");
            assert!(
                !delay.is_zero(),
                "{id} is due again immediately after a failed fetch — this is \
                 the per-frame retry storm",
            );
            assert!(
                delay <= std::time::Duration::from_secs(interval),
                "{id} backs off past its own {interval}s poll interval, so a \
                 failure recovers slower than an ordinary refresh: {delay:?}",
            );
        }

        assert!(
            checked.contains(&known::RADAR),
            "the radar layer polls the archive and must be on the same ladder \
             as every other poller; it is missing from {checked:?}",
        );
        assert_eq!(
            checked.len(),
            7,
            "the app's auto-polling layers are overlays' six plus radar; a new \
             one is not exempt and a removed one should be removed from this \
             count deliberately: {checked:?}",
        );
    }
}

#[cfg(test)]
mod controls_parity_tests {
    use rustdar_overlays::render::controls::ControlItem;
    use rustdar_overlays::render::overlay_state::{OverlayRegistry, PaneMut, PaneRef};
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
            ControlItem::TextField { id, label, .. } => format!("textfield:{id}:{label}"),
            ControlItem::Separator => "separator".into(),
        }
    }

    /// **The four surfaces WO-E8b moved into the radar layer's own body are
    /// still in it.**
    ///
    /// The parity walk is a *parity*: it asserts that everything a handler
    /// declares is reachable, so a control deleted from the tree it walks
    /// leaves it with nothing to look for and nothing to fail on. The rows
    /// this land moved left `SETTINGS_ROWS` at the same time, so without an
    /// inventory floor beside the walk, dropping one would fall out of both
    /// walks in silence — the failure the option-expression rule names.
    ///
    /// This is that floor: the labels are asserted as well as the ids,
    /// because a surface a user cannot recognise is not expressed either.
    #[test]
    fn the_radar_layer_still_offers_every_surface_that_moved_into_it() {
        use rustdar_radar::source as radar;

        let registry = OverlayRegistry::with_handlers(all());
        let shapes: Vec<String> = registry
            .controls(&rustdar_source::id::known::RADAR, &PaneRef::bare(0))
            .iter()
            .map(shape)
            .collect();
        for expected in [
            format!(
                "toggle:{}:{}",
                radar::AUTO_POLL_CONTROL,
                radar::AUTO_POLL_LABEL
            ),
            format!(
                "toggle:{}:{}",
                radar::LIVE_CHUNKS_CONTROL,
                radar::LIVE_CHUNKS_LABEL
            ),
            format!(
                "toggle:{}:{}",
                radar::CHUNK_NOTIFICATIONS_CONTROL,
                radar::CHUNK_NOTIFICATIONS_LABEL
            ),
            format!("buttons:{}", radar::REFRESH_CONTROL),
            format!(
                "textfield:{}:{}",
                radar::NOTIFIER_ENDPOINT_CONTROL,
                radar::NOTIFIER_ENDPOINT_LABEL
            ),
            format!("info:{}", radar::NOTIFIER_ENDPOINT_NOTE),
        ] {
            assert!(
                shapes.contains(&expected),
                "the radar layer no longer offers {expected:?}. It left \
                 SETTINGS_ROWS at WO-E8b, so this is the only walk that can \
                 still see it; offered: {shapes:?}",
            );
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
            12 + cfg!(feature = "fake-source") as usize,
            "the registry carries all twelve handlers - thirteen with the \
             fake source registered - and the walk below must cover every one"
        );
        let ctx = PaneRef::bare(0);
        for kind in kinds {
            registry.set_enabled(&kind, true, &mut PaneMut::bare(0));
            let shown: Vec<String> = registry.controls(&kind, &ctx).iter().map(shape).collect();
            registry.set_enabled(&kind, false, &mut PaneMut::bare(0));
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
    #[cfg(not(feature = "fake-source"))]
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

    /// The same twelve plus the fake source's own key. **A second definition
    /// rather than a `#[cfg]`'d element**, because `#[cfg]` on an array element
    /// is not stable.
    #[cfg(feature = "fake-source")]
    const STATE_KEYS: [&str; 13] = [
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
        "FakeSource",
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
    ///
    /// **This is also half of the anti-shrink floor WO-E9d rebuilt.** The
    /// count it asserts is [`super::REGISTERED_LAYER_COUNT`], a hand-kept
    /// literal, so a composition that quietly lost a registration fails here
    /// rather than handing every derived walk a shorter list to be satisfied
    /// by. The other half is `every_handlers_id_sits_in_the_ledger` below.
    #[test]
    fn no_two_handlers_share_an_id() {
        let handlers = all();
        assert_eq!(
            handlers.len(),
            super::REGISTERED_LAYER_COUNT,
            "the walk below must cover every registered layer",
        );
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
    ///
    /// **The other half of the rebuilt anti-shrink floor**, and asserted in
    /// ONE direction on purpose: every registered id is in the ledger, never
    /// the reverse. The ledger is append-only and may legitimately name a
    /// spelling this build does not register — a retired layer, or the fake
    /// source with its feature off — so the reverse would be a gate that fails
    /// on correct behaviour.
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
        #[allow(unused_mut)]
        let mut expected: Vec<&str> = vec![
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
        ];
        // Topmost where it is registered, which is deliberate: the fake exists
        // to be seen, and a proof layer under everything else proves less.
        #[cfg(feature = "fake-source")]
        expected.push("FakeSource");
        assert_eq!(
            ids, expected,
            "the weight order drifted from the historical default draw order",
        );
    }

    /// **Every layer's time axis, pinned by name over the composed twelve.**
    ///
    /// Written as the whole map rather than as "the non-Live ones", so a new
    /// layer cannot join without this list saying what it does with the clock,
    /// and a layer that quietly changes arm is a named diff. Alerts and
    /// lightning are the two `EventLifetime` layers (items with validity
    /// windows); radar and the model are the two `FrameSeries` layers — radar
    /// on the ~5-minute volume cadence and never ahead of the clock, the model
    /// on hourly runs that are — and the other eight draw the latest thing
    /// they fetched.
    ///
    /// **Radar's row moved deliberately at WO-E7b** (was `Live`), which is
    /// what gives a radar pane a time-primary layer for its clock to walk;
    /// radar sits above the model in the draw order (weight 30 against 10),
    /// so it is the time-primary layer of any pane that draws it.
    #[test]
    fn every_layer_declares_what_it_does_with_the_clock() {
        use rustdar_source::time::TimeAxis;

        let hourly_forecast = TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: true,
        };
        let volume_cadence = TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(300),
            extends_future: false,
        };
        #[allow(unused_mut)]
        let mut expected: Vec<(&str, TimeAxis)> = vec![
            ("ModelData", hourly_forecast),
            ("SpcOutlook", TimeAxis::Live),
            ("Radar", volume_cadence),
            ("SpcDiscussions", TimeAxis::Live),
            ("NwsAlerts", TimeAxis::EventLifetime),
            ("StormReports", TimeAxis::Live),
            ("Lightning", TimeAxis::EventLifetime),
            ("Metar", TimeAxis::Live),
            ("CityLabels", TimeAxis::Live),
            ("RadarSites", TimeAxis::Live),
            ("UserLocation", TimeAxis::Live),
            ("ColorScale", TimeAxis::Live),
        ];
        // The fake's step is deliberately neither of the two shipped cadences
        // — that is the whole reason it declares one.
        #[cfg(feature = "fake-source")]
        expected.push((
            "FakeSource",
            TimeAxis::FrameSeries {
                typical_step: std::time::Duration::from_secs(420),
                extends_future: false,
            },
        ));

        let mut actual: Vec<(String, TimeAxis)> = all()
            .iter()
            .map(|h| (h.id().as_str().to_owned(), h.time_axis()))
            .collect();
        actual.sort_by_key(|(id, _)| {
            expected
                .iter()
                .position(|(want, _)| want == id)
                .unwrap_or(usize::MAX)
        });
        assert_eq!(
            actual,
            expected
                .iter()
                .map(|(id, axis)| ((*id).to_owned(), *axis))
                .collect::<Vec<_>>(),
            "a layer's relationship to the clock changed, or a new layer \
             joined without declaring one",
        );
    }

    /// **Which layer takes a pane's clock, read off the two declarations that
    /// decide it** — `time_axis` and `draw_order_weight` — rather than by
    /// knowing which layer is radar.
    ///
    /// The time-primary layer is the topmost enabled `FrameSeries` layer in
    /// the draw order. Two layers declare `FrameSeries`, and radar's weight
    /// (30) puts it above the model's (10), so radar is time-primary on any
    /// pane that draws it and the model takes over only where radar is off.
    /// This is the fact WO-E7b's clock rests on; it is a **coincidence of two
    /// independent declarations**, so it is pinned rather than assumed.
    #[test]
    fn radar_takes_the_clock_wherever_it_is_drawn() {
        use rustdar_source::time::TimeAxis;

        let mut framed: Vec<(String, u32)> = all()
            .iter()
            .filter(|h| matches!(h.time_axis(), TimeAxis::FrameSeries { .. }))
            .map(|h| (h.id().as_str().to_owned(), h.draw_order_weight()))
            .collect();
        assert_eq!(
            framed.len(),
            2 + cfg!(feature = "fake-source") as usize,
            "exactly two layers come in stamped frames; a third joining changes \
             which one a pane's clock follows and must be ruled on, not \
             absorbed. The fake source IS such a third, and this is where that \
             ruling is written down: it declares a deliberately alien cadence \
             and sits topmost, so a build that registers it hands it the clock \
             on purpose — which is the property WO-E10.3 exercises.",
        );
        framed.sort_by_key(|(_, weight)| *weight);
        // The topmost frame-series layer is what a pane's clock walks.
        let expected_topmost = if cfg!(feature = "fake-source") {
            "FakeSource"
        } else {
            "Radar"
        };
        assert_eq!(
            framed.last().map(|(id, _)| id.as_str()),
            Some(expected_topmost),
            "the topmost frame-series layer in the draw order is what a pane's \
             clock walks",
        );
        // Radar above the model holds in BOTH arms: it is the shipped fact,
        // and the fake must not be able to disturb their relative order.
        let shipped: Vec<&str> = framed
            .iter()
            .map(|(id, _)| id.as_str())
            .filter(|id| *id != "FakeSource")
            .collect();
        assert_eq!(
            shipped,
            ["ModelData", "Radar"],
            "of the layers this app ships, radar is the topmost frame-series \
             one and the model is below it",
        );
        assert!(
            framed[0].1 < framed[1].1,
            "precondition: the two weights actually differ ({} vs {}), so \
             \"topmost\" is a fact and not a tie broken by registration order",
            framed[0].1,
            framed[1].1,
        );
    }

    /// The as-of quantum, for the two layers that read the depicted instant.
    /// Lightning's is a second because its fade ramp is sub-minute; everything
    /// else takes the trait's minute, which is the resolution NWS lifetimes
    /// are published at. Consumed at WO-E7c, pinned here so it cannot drift
    /// unnoticed in the meantime.
    #[test]
    fn the_as_of_quantum_is_a_second_only_where_the_picture_moves_that_fast() {
        for handler in all() {
            let want = if handler.id().as_str() == "Lightning" {
                std::time::Duration::from_secs(1)
            } else {
                std::time::Duration::from_secs(60)
            };
            assert_eq!(
                handler.as_of_quantum(),
                want,
                "{}'s as-of cache quantum",
                handler.id().as_str(),
            );
        }
    }
}

/// The composed registry's **field** contract, which only exists once radar and
/// the overlays are in one list.
///
/// These pins live here rather than in either source crate because that is where
/// the composition lives (WO-M9): a `FieldId` collision between two *different*
/// crates' fields is invisible to each crate's own suite, and it is exactly the
/// collision that would make one field's saved curves, thresholds and preset
/// panes silently resolve to the other's.
#[cfg(test)]
mod field_registry_tests {
    use rustdar_source::product::FieldId;

    use super::all;

    /// Every field the composed registry offers, with its owning layer.
    fn fields() -> Vec<(String, &'static rustdar_source::product::ProductSpec)> {
        all()
            .iter()
            .flat_map(|h| {
                let id = h.id().as_str().to_owned();
                h.products().iter().map(move |s| (id.clone(), s))
            })
            .collect()
    }

    /// No two fields — from any two layers — share an id.
    #[test]
    fn no_two_fields_share_an_id_across_the_whole_registry() {
        let fields = fields();
        // Non-triviality floor: there are fields from more than one layer, so
        // the check is actually cross-crate and not a restatement of one
        // crate's own uniqueness test.
        let owners: std::collections::HashSet<&str> =
            fields.iter().map(|(o, _)| o.as_str()).collect();
        assert!(
            owners.len() >= 2,
            "only {} layer(s) register fields, so this pin cannot see a \
             cross-layer collision: {owners:?}",
            owners.len(),
        );
        assert_eq!(
            fields.len(),
            17 + 16 + cfg!(feature = "fake-source") as usize,
            "the composed field count moved (radar's seventeen products plus \
             the model's sixteen parameters, plus the fake's one where it is \
             registered); re-cut this pin in the land that changed it",
        );

        let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for (owner, spec) in &fields {
            if let Some(prev) = seen.insert(spec.id.as_str(), owner.as_str()) {
                panic!(
                    "{:?} is registered by both {prev} and {owner}; one \
                     layer's saved curves, thresholds and preset panes would \
                     resolve to the other's field",
                    spec.id.as_str(),
                );
            }
        }
    }

    /// Every registered field states all eleven facts usefully — the doctrine
    /// that no field carries a `Default`, checked rather than claimed.
    #[test]
    fn every_registered_field_states_its_facts() {
        for (owner, s) in fields() {
            assert!(!s.id.as_str().is_empty(), "{owner} registered an empty id");
            assert!(!s.name.is_empty(), "{:?} has no display name", s.id);
            assert!(!s.code.is_empty(), "{:?} has no code", s.id);
            assert!(!s.group.is_empty(), "{:?} is filed under no group", s.id);
            assert!(
                !s.scale.thresholds.is_empty(),
                "{:?} has a colour bar with no stops",
                s.id,
            );
            let (lo, hi) = s.value_domain;
            assert!(
                lo.is_finite() && hi.is_finite() && lo < hi,
                "{:?}'s value domain is {lo}..={hi}, which nothing can travel",
                s.id,
            );
            assert!(
                !s.domain_label_ends.0.is_empty(),
                "{:?} states no threshold prefix",
                s.id,
            );
            // `vertical` implies the field is `tilted`: a field with vertical
            // extent is sampled or derived tilt by tilt.
            if s.vertical {
                assert!(
                    s.tilted,
                    "{:?} claims vertical extent without a per-tilt field",
                    s.id,
                );
            }
        }
    }

    /// The registry's `by_id` read answers for every registered field and
    /// refuses one it does not have.
    #[test]
    fn a_field_id_resolves_to_exactly_its_own_registration() {
        let fields = fields();
        for (_, s) in &fields {
            let hits = fields.iter().filter(|(_, o)| o.id == s.id).count();
            assert_eq!(hits, 1, "{:?} resolved {hits} ways", s.id);
        }
        let unknown = FieldId::new("NoBuildRegistersThisField");
        assert!(
            !fields.iter().any(|(_, s)| s.id == unknown),
            "an unregistered id must resolve to nothing",
        );
    }

    /// Every group a field declares is one of the two this build registers, and
    /// both are actually populated — a group label nobody uses would make the
    /// catalogue's generic loop draw an empty heading.
    #[test]
    fn the_declared_groups_are_populated() {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (_, s) in fields() {
            *counts.entry(s.group).or_default() += 1;
        }
        assert_eq!(
            counts.get("Radar products").copied(),
            Some(17),
            "the radar group's size moved",
        );
        assert_eq!(
            counts.get("HRRR parameters").copied(),
            Some(16),
            "the model group's size moved",
        );
        #[cfg(feature = "fake-source")]
        assert_eq!(
            counts.get("Fake").copied(),
            Some(1),
            "the fake source's group is registered and holds its one field",
        );
        assert_eq!(
            counts.len(),
            2 + cfg!(feature = "fake-source") as usize,
            "an unexpected group appeared: {counts:?}",
        );
    }
}
