//! **The C1 acceptance suite: what it costs to add a source.**
//!
//! `rustdar-overlays`' `fake-source` feature registers a layer no crate above
//! it has an arm for. This module is the UI half of the acceptance test —
//! catalogue, draw, clock and config — and
//! `rustdar-app/tests/fake_source_acceptance.rs` is the wire half plus the
//! footprint pin that measures the whole cost.
//!
//! **These files are campaign infrastructure and land once.** They are two of
//! the three files outside `rustdar-overlays` that the footprint pin allows to
//! name the fake, and adding a *second* source must not add a line to either.
//!
//! # The two arms, and why they are not symmetric
//!
//! `cargo test -p rustdar-egui` is the **OFF** arm;
//! `cargo test -p rustdar-egui --features fake-source` is the **ON** arm.
//! Criteria 1–3 and 4(a) are ON-arm properties: they are about a build that
//! registers the layer. Criterion 4(b) is an **OFF**-arm property — a build
//! that does *not* register it, reading a file written by one that did — so it
//! is `#[cfg(not(feature = "fake-source"))]` and its home is the default local
//! workspace gate. Nobody should "fix" that asymmetry: a downgrade test that
//! runs in the build it is downgrading from is not testing anything.
//!
//! `--all-features` (CI clippy and llvm-cov) turns the feature **on**, so CI
//! runs the ON arm automatically and never runs 4(b).

use crate::Gui;
use rustdar_source::id::LayerId;

/// The fake layer's id, as it is spelled on the wire, in the ledger and in a
/// saved config.
///
/// **The string, never `format!("{:?}", LayerId)`** — a `LayerId`'s `Debug`
/// prints `LayerId("FakeSource")`, and a persisted key built that way would be
/// a different key.
const FAKE_ID: &str = "FakeSource";

fn fake_id() -> LayerId {
    LayerId::new(FAKE_ID)
}

/// The config a build that registered the fake wrote, verbatim. Read by both
/// arms: the OFF arm loads it, the ON arm proves it is still the shape a
/// registering build produces.
const DOWNGRADE_FIXTURE: &str = include_str!("ui_config/fixtures/fake_source_downgrade.json");

/// The non-default tint the fixture was captured with. Named once so the two
/// arms cannot drift apart on it.
const DOWNGRADE_FIXTURE_TINT: &str = "cool";

/// One pane-0 layer slot out of a saved config, by id.
fn slot_named(config: &serde_json::Value, id: &str) -> Option<serde_json::Value> {
    config["panes"][0]["layer_slots"]
        .as_array()?
        .iter()
        .find(|entry| entry["id"] == id)
        .cloned()
}

// ─── Criterion 1: catalog + parity ──────────────────────────────────────────

/// **The fake's tiles are in the catalogue, and no test-side inventory was
/// edited to put them there.**
///
/// The parity walk itself (`parity_walk::every_option_is_reachable_on_*`,
/// green 3/3 in both arms) is the reachability half: WO-E9d re-pointed its
/// layer and field inventories at the live registry, so automatic pickup *is*
/// the assertion and a layer that registers is a layer it walks. This test is
/// the narrow, nameable statement of the same fact for the fake, plus the part
/// the walk cannot state about itself — that the walk's own source, and the
/// catalogue's, contain no arm for this layer.
#[cfg(feature = "fake-source")]
#[test]
fn the_fakes_tiles_appear_in_the_catalogue_with_no_inventory_arm_naming_it() {
    use crate::input_harness::InputHarness;
    use crate::ui::CatalogGroup;

    // The label and group come off the registry, not out of this file: a test
    // that hard-codes them would still pass if the handler's declarations and
    // the catalogue's rendering drifted together.
    let (display_name, field_group, field_name) = {
        let gui = Gui::new();
        let handler = gui
            .overlays
            .handler_by_id(&fake_id())
            .expect("this build registers the fake source");
        let display = handler.display_name().to_owned();
        let spec = handler
            .products()
            .first()
            .expect("the fake publishes one field");
        (display, spec.group, spec.name.to_owned())
    };

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    h.open_catalog();
    h.warm_up();
    let tiles = h.catalog().tiles;
    assert!(
        !tiles.is_empty(),
        "the catalogue drew no tiles at all, so finding the fake's would prove \
         nothing about the fake",
    );

    assert!(
        tiles
            .iter()
            .any(|tile| tile.group == CatalogGroup::Layers && tile.label == display_name),
        "this build registers the fake source and the catalogue drew no layer \
         tile for {display_name:?} - the catalogue has an inventory the \
         registry is not the source of",
    );
    assert!(
        tiles.iter().any(|tile| {
            tile.group == CatalogGroup::Fields(field_group) && tile.label == field_name
        }),
        "the fake registers the field {field_name:?} under the group \
         {field_group:?} and the catalogue drew no such tile - a source that \
         publishes a field still needs an arm somewhere, which is what C1 says \
         it must not",
    );

    // The half the walk cannot state about itself. If either of these files
    // had to learn the fake's name, "no test-side inventory edits" would be
    // false however green the walk is.
    for (name, src) in [
        ("parity_walk.rs", include_str!("parity_walk.rs")),
        ("ui_catalog.rs", include_str!("ui_catalog.rs")),
    ] {
        assert!(
            !src.contains(FAKE_ID) && !src.contains("fake"),
            "{name} names the fake source. The catalogue and the parity walk \
             must reach a new layer through the registry alone; an arm here is \
             exactly the per-source cost C1 exists to measure",
        );
    }
}

// ─── Criterion 2: draws ─────────────────────────────────────────────────────

/// **Enabling the fake in a pane draws it, at its own place in the stack.**
///
/// Two legs, because the paint-order probe alone would be the weaker one: it
/// records that the pane *dispatched* the layer's arm, which a layer holding
/// no texture also does. The second leg hands the fake's own overlay cache a
/// texture — the same door `apply_render_to_pane` writes through when a render
/// lands — and counts the textured quads the pane actually painted, against
/// the count from the same scene with the layer off.
#[cfg(feature = "fake-source")]
#[test]
fn enabling_the_fake_in_a_pane_draws_it_at_its_own_place_in_the_stack() {
    use crate::input_harness::InputHarness;
    use crate::overlay_cache::OverlayTextureData;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    let pane_rect = h.pane_rects()[0];

    // Control: the layer defaults off, so it is absent before anything is
    // clicked. Without this, "present when enabled" could be satisfied by a
    // probe that lists every registered layer rather than every drawn one.
    assert!(
        !h.overlay_enabled_on(0, &fake_id()),
        "precondition: the fake's `default_enabled` is false",
    );
    assert!(
        !h.paint_order(0).iter().any(|(id, _)| *id == fake_id()),
        "the pane painted the fake before it was ever enabled",
    );
    let images_before = h.painted_images_in(pane_rect).len();

    h.set_overlay_on_pane(0, &fake_id(), true);
    h.warm_up();

    let order = h.paint_order(0);
    let pos = order
        .iter()
        .position(|(id, _)| *id == fake_id())
        .unwrap_or_else(|| {
            panic!(
                "the fake is enabled on pane 0 and the pane's paint order does \
                 not contain it: {order:?}"
            )
        });
    assert_eq!(
        pos,
        order.len() - 1,
        "the fake declares draw_order_weight 130, above every shipped layer, \
         so it paints last: {order:?}",
    );
    assert_eq!(
        order[pos].1, order[0].1,
        "the fake painted onto its own egui layer while the rest of the pane \
         painted onto the pane's - its stacking would then be egui's hash-order \
         drain and not the pane's draw order",
    );

    // Leg two: real pixels. A 1×1 texture over ground the default view shows,
    // parked in the pane's overlay cache, is what a landed render leaves
    // behind. The handle is minted from a context of its own so this suite
    // needs nothing new from the harness; only the `TextureId` reaches the
    // paint list, which is what the probe reads.
    let texture_ctx = egui::Context::default();
    let texture = texture_ctx.load_texture(
        "acceptance_fake",
        egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
        egui::TextureOptions::NEAREST,
    );
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .overlay_cache_mut(&fake_id())
        .show(OverlayTextureData {
            texture,
            placed: rustdar_geo::PlacedRaster::of(rustdar_geo::GeoBounds {
                min_lat: 30.0,
                max_lat: 40.0,
                min_lon: -102.0,
                max_lon: -92.0,
            }),
            data_generation: 0,
            render_zoom: 0,
            width: 1,
            height: 1,
            radar_meta: None,
            hit_map: None,
        });
    h.warm_up();

    let images_after = h.painted_images_in(pane_rect).len();
    assert!(
        images_after > images_before,
        "the fake's overlay cache holds a texture and the pane painted no more \
         textured quads than it did with the layer off ({images_before} then \
         {images_after}) - the layer is in the draw order and nothing draws it",
    );
}

// ─── Criterion 3: alien cadence ─────────────────────────────────────────────

/// **Two cadences, one clock.**
///
/// One pane holds radar and the fake. The fake's stamps come from the fake's
/// own `list_frames` **through the registry** — this test never names the
/// fake's frame grid, it asks for it, which is the whole point. Radar's are
/// laid out on the step radar itself declares.
///
/// Then one clock is walked across both, and each layer's playhead must land
/// on the latest stamp of **its own** list at or before the clock. The
/// non-vacuity floor is explicit: there has to be an instant where the two
/// layers show different stamps, or a build that had quietly put both layers
/// on one cadence would pass.
#[cfg(feature = "fake-source")]
#[test]
fn one_clock_walks_two_cadences_and_each_layer_lands_on_its_own_stamps() {
    use crate::pane::{LoopFrame, LoopPhase, TimeMode};
    use rustdar_source::handler::PaneRef;
    use rustdar_source::id::known;
    use rustdar_source::time::TimeAxis;

    let mut gui = Gui::new();
    let fake = fake_id();

    // The two declared steps, read off the registry rather than written here.
    let step = |gui: &Gui, id: &LayerId| -> i64 {
        match gui
            .overlays
            .handler_by_id(id)
            .unwrap_or_else(|| panic!("{id:?} is not registered in this build"))
            .time_axis()
        {
            TimeAxis::FrameSeries { typical_step, .. } => typical_step.as_secs() as i64,
            other => panic!("{id:?} declares {other:?}, not a frame series"),
        }
    };
    let radar_step = step(&gui, &known::RADAR);
    let fake_step = step(&gui, &fake);
    assert_ne!(
        radar_step, fake_step,
        "both layers declare the same cadence, so this pane is not holding two \
         of them and every assertion below is about one grid",
    );

    // A window wide enough that the two grids disagree many times over.
    let base = chrono::DateTime::from_timestamp(1_700_000_000, 0)
        .expect("a literal inside chrono's range")
        .naive_utc();
    let window = (base, base + chrono::Duration::seconds(3 * 3600));

    // The fake's stamps come from the fake, through the trait.
    let fake_stamps: Vec<chrono::NaiveDateTime> = {
        // A `reqwest::Client` cannot be built before the process has a rustls
        // provider, and a test that builds one is otherwise green only when
        // some earlier test in the same binary happened to install it.
        rustdar_source::tls::init();
        let config = rustdar_source::handler::FetchConfig {
            client: Default::default(),
            zone_cache_dir: None,
            sources: rustdar_source::origins::DataSources::default(),
            viewport: None,
        };
        gui.overlays
            .list_frames(&fake, &config, &PaneRef::bare(0), window)
            .frames
            .iter()
            .map(|f| f.valid)
            .collect()
    };
    assert!(
        fake_stamps.len() > 4,
        "the fake listed {} frames over three hours; there is nothing to step \
         through",
        fake_stamps.len(),
    );
    for pair in fake_stamps.windows(2) {
        assert_eq!(
            (pair[1] - pair[0]).num_seconds(),
            fake_step,
            "the fake's listing is not on the step it declares",
        );
    }
    assert!(
        fake_stamps
            .iter()
            .all(|s| s.and_utc().timestamp() % fake_step == 0),
        "amendment M9 requires real epoch-aligned stamps, not stubs: \
         {fake_stamps:?}",
    );

    // Radar's, on the step radar declares. Radar's own listing is an archive
    // index this crate has no network to ask, so the grid is synthesised — but
    // its SPACING is radar's own declaration, not a number chosen here.
    let radar_stamps: Vec<chrono::NaiveDateTime> = (0..)
        .map(|i| base + chrono::Duration::seconds(i * radar_step))
        .take_while(|t| *t <= window.1)
        .collect();

    let install = |gui: &mut Gui, id: &LayerId, stamps: &[chrono::NaiveDateTime]| {
        gui.set_overlay_on_pane_for_test(0, id, true);
        let state = gui.pane_mut(0).expect("pane 0").time_state_mut(id);
        state.phase = LoopPhase::Ready;
        state.frames = stamps
            .iter()
            .map(|t| LoopFrame {
                timestamp: *t,
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
    };
    install(&mut gui, &known::RADAR, &radar_stamps);
    install(&mut gui, &fake, &fake_stamps);

    // The fake is the topmost animating frame-series layer, so this pane's
    // clock is the fake's — a build that registers it hands the clock over on
    // purpose (`sources::radar_takes_the_clock_wherever_it_is_drawn` is where
    // that ruling is written down).
    assert_eq!(
        gui.pane(0).expect("pane 0").clock_layer(),
        Some(&fake),
        "the fake sits topmost and is animating, so it is this pane's clock \
         layer",
    );

    // One clock, walked across the window in a step that is a multiple of
    // neither grid, so the two playheads are repeatedly asked to disagree.
    //
    // `expected` is `TimeAxis::FrameSeries`'s own rule stated independently:
    // the latest stamp at or before the clock, and NOTHING when the clock sits
    // before every stamp this layer holds — which it does at the start of the
    // window, because the fake's grid is epoch-aligned and the window's start
    // is not on it. Radar's grid starts at `base`, so radar always qualifies;
    // that asymmetry is the point, and it is the mixed-span case in miniature.
    let expected = |stamps: &[chrono::NaiveDateTime], t: chrono::NaiveDateTime| {
        stamps.iter().rev().find(|s| **s <= t).copied()
    };
    let mut disagreements = 0;
    let mut fake_blank = 0;
    let mut fake_distinct = std::collections::BTreeSet::new();
    let mut radar_distinct = std::collections::BTreeSet::new();
    for tick in 0..60 {
        let clock = base + chrono::Duration::seconds(tick * 137);
        gui.pane_mut(0)
            .expect("pane 0")
            .set_time_mode(TimeMode::AsOf(clock));

        let pane = gui.pane(0).expect("pane 0");
        let fake_shown = pane.time_state(&fake).playhead_stamp();
        let radar_shown = pane.time_state(&known::RADAR).playhead_stamp();
        if fake_shown.is_none() {
            fake_blank += 1;
        }

        assert_eq!(
            fake_shown,
            expected(&fake_stamps, clock),
            "at {clock} the fake is not on the latest of ITS OWN stamps at or \
             before the pane's clock",
        );
        assert_eq!(
            radar_shown,
            expected(&radar_stamps, clock),
            "at {clock} radar is not on the latest of ITS OWN stamps at or \
             before the pane's clock - one clock has been resolved against the \
             other layer's grid",
        );
        if fake_shown != radar_shown {
            disagreements += 1;
        }
        // Only real stamps count toward "a playhead moved" — otherwise a layer
        // that answered `None` throughout would inflate its own floor.
        fake_distinct.extend(fake_shown);
        radar_distinct.extend(radar_shown);
    }

    // The floors. Without these the whole loop is satisfied by two layers
    // parked on one frame, or by two grids that happen to coincide.
    assert!(
        disagreements > 0,
        "the two layers showed the same stamp at every one of 60 clock \
         positions, so this pane is holding one cadence twice and the test \
         cannot tell an alien cadence from radar's",
    );
    assert!(
        fake_distinct.len() > 1 && radar_distinct.len() > 1,
        "a playhead never moved across the whole scrub (fake {} distinct, \
         radar {} distinct) - the clock is not reaching the layers",
        fake_distinct.len(),
        radar_distinct.len(),
    );
    // The mixed-span floor, both halves. The fake really did answer "nothing
    // qualifies" at least once — so the walk above exercised the empty answer
    // rather than merely tolerating it — and it did NOT answer that always,
    // which would be a silently blank layer passing as a fixed contract.
    assert!(
        fake_blank > 0,
        "the fake qualified at all 60 clock positions, so this walk never \
         reached the case the contract is about",
    );
    assert!(
        fake_blank < 60,
        "the fake answered `None` at every one of 60 clock positions - a \
         blanket blank passes the contract and draws an empty map",
    );
}

// ─── Criterion 4(a): the round trip, feature ON ─────────────────────────────

/// Write one control update into pane `idx`'s own slot state, the way
/// `ui_catalog`'s field pick does — through `apply_control` on a `PaneMut`
/// built from the pane's slot, never a field write. `Gui::apply_layer_control`
/// is deliberately not used: it passes `PaneMut::bare`, which writes the
/// handler's defaults and not the slot the config file is serialized from.
#[cfg(feature = "fake-source")]
fn apply_control_on_pane(gui: &mut Gui, idx: usize, id: &LayerId, update: &ControlUpdate) {
    let (panes, overlays) = gui.panes_and_overlays_mut();
    let pane = &mut panes[idx];
    pane.hydrate_layer_states(overlays, idx);
    let mut pane_ctx = rustdar_source::handler::PaneMut {
        pane_idx: idx,
        state: pane
            .slot_mut(id)
            .and_then(|slot| slot.state.as_deref_mut())
            .map(|s| s as &mut dyn std::any::Any),
        peers: &[],
    };
    overlays.apply_control(id, update, &mut pane_ctx);
    // The write lands in the slot's live STATE; `slot.config` — which is what
    // the save serializes — is refreshed by asking the handler for it, the same
    // door every frame goes through.
    pane.adopt_handler_state(overlays);
}

#[cfg(feature = "fake-source")]
use rustdar_source::controls::{ControlItem, ControlUpdate, ControlValue};

/// The `(id, options, selected)` of the one dropdown the fake offers on pane 0.
#[cfg(feature = "fake-source")]
fn fake_dropdown(gui: &Gui, idx: usize) -> (&'static str, Vec<(String, String)>, String) {
    let fake = fake_id();
    let view = gui.pane(idx).expect("pane").view(idx);
    let items = gui.overlays.controls(&fake, &view.layer(&fake));
    items
        .into_iter()
        .find_map(|item| match item {
            ControlItem::Dropdown {
                id,
                options,
                selected,
                ..
            } => Some((id, options, selected)),
            _ => None,
        })
        .expect("the fake offers exactly one dropdown")
}

/// **A registered fake's config survives save → load → save, 1:1.**
///
/// The layer's dropdown is moved off its default first, because a round trip
/// carrying only defaults is satisfied by a build that writes nothing and
/// reads nothing.
#[cfg(feature = "fake-source")]
#[test]
fn the_fakes_own_config_survives_a_reopen_unchanged() {
    let mut gui = Gui::new();
    let fake = fake_id();
    gui.set_overlay_on_pane_for_test(0, &fake, true);

    let (control_id, options, default_value) = fake_dropdown(&gui, 0);
    let other = options
        .iter()
        .map(|(value, _)| value)
        .find(|value| **value != default_value)
        .cloned()
        .expect("the dropdown offers more than one option");
    apply_control_on_pane(
        &mut gui,
        0,
        &fake,
        &ControlUpdate {
            id: control_id,
            value: ControlValue::String(other.clone()),
        },
    );
    assert_eq!(
        fake_dropdown(&gui, 0).2,
        other,
        "precondition: the control edit landed on the pane's own state, so the \
         round trip has something to lose",
    );

    let store = rustdar_kv::MemoryKvStore::default();
    gui.save_ui_config(&store);
    let mut reopened = Gui::new();
    assert!(
        reopened.load_ui_config(&store),
        "the saved config must load"
    );

    assert!(
        reopened.pane(0).expect("pane 0").is_overlay_enabled(&fake),
        "the fake was on when the file was written and is off after the reopen",
    );
    assert_eq!(
        fake_dropdown(&reopened, 0).2,
        other,
        "the fake's dropdown selection did not survive the reopen",
    );

    let first: serde_json::Value =
        serde_json::from_str(&gui.ui_config_json().expect("serializable")).expect("valid JSON");
    let second: serde_json::Value =
        serde_json::from_str(&reopened.ui_config_json().expect("serializable"))
            .expect("valid JSON");
    assert_eq!(
        first, second,
        "save -> load -> save is not a fixpoint for a build that registers the \
         fake: the reopen is not 1:1",
    );
}

/// **The downgrade fixture is what this build actually writes.**
///
/// Criterion 4(b) lives in the other arm and cannot see this one, so the
/// fixture it reads would rot in silence. This closes the loop: the ON build
/// asserts the checked-in fixture's fake slot equals the slot it writes itself.
#[cfg(feature = "fake-source")]
#[test]
fn the_downgrade_fixture_carries_the_slot_a_registering_build_writes() {
    let mut gui = Gui::new();
    let fake = fake_id();
    gui.set_overlay_on_pane_for_test(0, &fake, true);
    let (control_id, _, _) = fake_dropdown(&gui, 0);
    apply_control_on_pane(
        &mut gui,
        0,
        &fake,
        &ControlUpdate {
            id: control_id,
            value: ControlValue::String(DOWNGRADE_FIXTURE_TINT.to_string()),
        },
    );

    let written: serde_json::Value =
        serde_json::from_str(&gui.ui_config_json().expect("serializable")).expect("valid JSON");
    let mine = slot_named(&written, FAKE_ID).expect("this build writes a slot for the fake");
    assert_ne!(
        mine["config"],
        serde_json::Value::Null,
        "the slot this build writes carries no config, so the fixture \
         comparison below would be about an empty object",
    );

    let fixture: serde_json::Value =
        serde_json::from_str(DOWNGRADE_FIXTURE).expect("the fixture is valid JSON");
    let theirs = slot_named(&fixture, FAKE_ID).expect("the fixture carries a slot for the fake");

    assert_eq!(
        mine, theirs,
        "the downgrade fixture's fake slot is not the slot this build writes, \
         so the OFF arm's byte-preservation test is preserving a shape nothing \
         produces",
    );
}

// ─── Criterion 4(b): the downgrade, feature OFF ─────────────────────────────

/// **A build with no fake reads a file that has one, and gives it back
/// untouched.**
///
/// This is the criterion the whole feature exists to make checkable: what a
/// source costs a build that does not have it. It rides the unknown-id
/// preservation path — no handler, no arm, kept in place in the stack and
/// written back as it was found.
///
/// **OFF arm only, deliberately.** Under `--features fake-source` (which
/// `--all-features` implies, so CI's clippy and llvm-cov runs) the id resolves
/// to a real handler and there is no downgrade to test. Its home is the
/// default `cargo test --workspace`.
#[cfg(not(feature = "fake-source"))]
#[test]
fn a_config_naming_the_fake_source_loads_and_is_written_back_byte_preserved() {
    use rustdar_kv::KvStore;

    let store = rustdar_kv::MemoryKvStore::default();
    store
        .store(crate::UI_CONFIG_KEY, DOWNGRADE_FIXTURE)
        .expect("the memory store accepts a write");

    let original: serde_json::Value =
        serde_json::from_str(DOWNGRADE_FIXTURE).expect("the fixture is valid JSON");
    let saved_slot = slot_named(&original, FAKE_ID)
        .expect("precondition: the fixture carries a slot for the fake");
    assert!(
        saved_slot["config"]
            .as_object()
            .is_some_and(|config| !config.is_empty()),
        "precondition: the fixture's fake slot carries a non-empty config, or \
         \"preserved\" below is a statement about an empty object",
    );
    assert_eq!(
        saved_slot["config"]["tint"], DOWNGRADE_FIXTURE_TINT,
        "precondition: the fixture was captured with the layer's NON-default \
         tint, so what is preserved below is content this build could not have \
         regenerated from a default",
    );

    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the fixture must load");

    // No handler serves it — this is the predicate the draw loop skips on, and
    // the reason the slot is unknown rather than merely disabled.
    let fake = fake_id();
    assert!(
        gui.overlays.handler_by_id(&fake).is_none(),
        "this build registers the fake, so it is not the downgrade arm",
    );

    // Direction 1: retained IN the live stack, at the position the file gave
    // it, not appended to the end.
    let live_order: Vec<String> = gui
        .pane(0)
        .expect("pane 0")
        .draw_order_vec()
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect();
    let file_order: Vec<String> = original["panes"][0]["layer_slots"]
        .as_array()
        .expect("layer_slots is a list")
        .iter()
        .map(|e| e["id"].as_str().expect("every slot names an id").to_owned())
        .collect();
    assert_eq!(
        live_order, file_order,
        "the loaded stack is not the file's stack - an unknown id must keep \
         its saved position, and nothing else may move around it",
    );

    // Direction 2: written back, in place, with its config intact.
    let saved = gui.ui_config_json().expect("serializable");
    let round_tripped: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    assert_eq!(
        slot_named(&round_tripped, FAKE_ID).as_ref(),
        Some(&saved_slot),
        "the fake's slot did not come back preserved: a build without the \
         layer rewrote a layer it cannot serve",
    );
    assert_eq!(
        round_tripped, original,
        "the whole file moved under a build that has no arm for one of its \
         layers - the reopen is not 1:1 for the user who has both builds",
    );

    // And it survives a session, not only a load: a layer toggle overwrites
    // every registered layer's slot state, which is where an unknown one is
    // most easily dropped.
    gui.set_overlay_on_pane_for_test(0, &rustdar_source::id::known::CITY_LABELS, true);
    let after: serde_json::Value =
        serde_json::from_str(&gui.ui_config_json().expect("serializable")).expect("valid JSON");
    assert_eq!(
        slot_named(&after, FAKE_ID).as_ref(),
        Some(&saved_slot),
        "a layer toggle dropped or rewrote the unknown fake slot",
    );
}
