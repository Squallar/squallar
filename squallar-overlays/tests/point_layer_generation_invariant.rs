//! **A point layer's drawn set and its `data_generation` move together, and
//! nothing else moves the generation.**
//!
//! `SourceHandler::data_generation` states the contract; this is the gate on
//! it. The point pass keys its kept build on that number and holds, under it,
//! the culled and projected screen positions it hit-tests clicks against, so
//! the two failures are a click landing on a station that is gone (a
//! replacement with no bump) and a cull and a projection of an identical list
//! thrown away and paid for again (a bump with no replacement). Only the first
//! is visible in a picture, which is why the second needs a test at all.
//!
//! **Everything here is driven through the registry's own doors** —
//! `apply_fetch_result` is where a live METAR round lands, `set_enabled`,
//! `apply_control`, `set_fetching` and `record_fetch_failure` are what the app
//! calls between rounds. Nothing writes a handler's state directly: a fixture
//! that can only reach the defect by a route production never takes proves the
//! harness, not the invariant.
//!
//! METAR is the subject because it is the tree's one layer with a non-default
//! `per_frame_points` — the one drawn set the kept pass hit-tests against.

use squallar_overlays::metar::fetch::MetarRound;
use squallar_overlays::metar::types::MetarOb;
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::controls::{ControlUpdate, ControlValue};
use squallar_source::handler::{OverlayFetchResult, PaneMut, PaneRef};
use squallar_source::id::known;

fn station(id: &str, lat: f64, lon: f64) -> MetarOb {
    MetarOb {
        station_id: id.into(),
        name: format!("{id} field"),
        lat,
        lon,
        elev_m: None,
        temp_c: Some(21.0),
        dewp_c: None,
        wind_dir: None,
        wind_speed_kt: None,
        wind_gust_kt: None,
        visibility: None,
        altimeter_hpa: None,
        mslp_hpa: None,
        flight_category: None,
        raw_ob: format!("{id} 041953Z AUTO"),
        clouds: Vec::new(),
        wx_string: None,
        obs_time: String::new(),
    }
}

/// A round as the fetch builds one, delivered through the door the app's
/// drain uses.
fn deliver(registry: &mut OverlayRegistry, stations: &[(&str, f64, f64)]) {
    let round = MetarRound {
        observations: stations
            .iter()
            .map(|&(id, lat, lon)| station(id, lat, lon))
            .collect(),
        failed_networks: Vec::new(),
        networks_asked: 1,
    };
    registry.apply_fetch_result(
        OverlayFetchResult {
            kind: known::METAR,
            data: OverlayRegistry::metar_payload(round),
        },
        &PaneRef::across(&[]),
    );
}

/// What the point pass would draw and hit-test, in a form two frames can be
/// compared by. The selection `Arc` is left out: it is a handle onto the same
/// observation, and its address moving is not the drawn set moving.
fn drawn(registry: &OverlayRegistry) -> Vec<(u64, u64, u32)> {
    registry
        .per_frame_points(&known::METAR)
        .iter()
        .map(|p| (p.lat.to_bits(), p.lon.to_bits(), p.id))
        .collect()
}

/// **The drawn set moved, so the generation moved.** Three transitions a live
/// layer makes: the first round of a session, a poll that returns a different
/// network, and the release that empties the layer when no pane draws it.
///
/// Each leg asserts the drawn set actually changed before asking about the
/// generation, so a fixture that stopped exercising the transition fails here
/// rather than passing vacuously.
#[test]
fn a_round_that_changes_the_drawn_set_moves_the_generation() {
    let mut registry = OverlayRegistry::default();
    let id = known::METAR;

    let empty = drawn(&registry);
    let fresh = registry.data_generation(&id);
    assert!(empty.is_empty(), "nothing has arrived yet");

    deliver(
        &mut registry,
        &[("K001", 35.0, -97.0), ("K002", 36.0, -98.0)],
    );
    let first = drawn(&registry);
    assert_ne!(first, empty, "the first round put stations on the map");
    assert_ne!(
        registry.data_generation(&id),
        fresh,
        "the drawn set went from nothing to two stations under an unmoved \
         generation: the point pass would serve a build that culled and \
         projected an empty list",
    );

    let after_first = registry.data_generation(&id);
    deliver(
        &mut registry,
        &[
            ("K003", 41.0, -87.0),
            ("K004", 42.0, -88.0),
            ("K005", 43.0, -89.0),
        ],
    );
    let second = drawn(&registry);
    assert_ne!(second, first, "the poll returned a different network");
    assert_ne!(
        registry.data_generation(&id),
        after_first,
        "a poll replaced every station under an unmoved generation: a click \
         would be hit-tested against the previous round's screen positions",
    );

    let after_second = registry.data_generation(&id);
    let handler = registry
        .get_handler_mut(&id)
        .expect("the METAR layer is registered");
    assert!(handler.release_data(), "there was a round to release");
    let released = drawn(&registry);
    assert!(released.is_empty(), "the release emptied the drawn set");
    assert_ne!(
        registry.data_generation(&id),
        after_second,
        "the layer's stations are gone and the generation says they are not",
    );
}

/// **A door that changes no drawn set moves no generation.** The ones the app
/// calls between rounds — the toggle, the layers-panel controls, the fetching
/// flag, a failed round — leave this layer drawing exactly what it drew, so
/// they leave it the cull and the projection the point pass already built.
///
/// This is the half no picture can be wrong about, so its cost is invisible
/// and only a test states it.
///
/// **This layer, not every layer.** METAR places every observation it holds
/// whenever a pane draws it, so none of these doors touches its drawn set. A
/// layer whose picture *is* a function of a pane choice — the satellite
/// channel, the gridded product, an alerts category filter — earns a bump on
/// that choice and would rightly fail a test shaped like this one. The doors
/// are enumerated rather than derived, so one added later needs a row here.
#[test]
fn the_doors_that_change_no_drawn_set_leave_the_generation_alone() {
    let mut registry = OverlayRegistry::default();
    let id = known::METAR;
    deliver(
        &mut registry,
        &[("K001", 35.0, -97.0), ("K002", 36.0, -98.0)],
    );

    let settled = drawn(&registry);
    let generation = registry.data_generation(&id);
    assert!(!settled.is_empty(), "the fixture has a drawn set to keep");

    registry.set_enabled(&id, false, &mut PaneMut::bare(0));
    registry.set_enabled(&id, true, &mut PaneMut::bare(0));
    registry.apply_control(
        &id,
        &ControlUpdate {
            id: "enabled",
            value: ControlValue::Bool(true),
        },
        &mut PaneMut::bare(0),
    );
    registry.set_fetching(&id, true, &PaneRef::bare(0));
    registry.record_fetch_failure(
        &id,
        &squallar_source::fetch_policy::FetchError::transient("the network did not answer"),
        &PaneRef::bare(0),
    );
    registry.set_fetching(&id, false, &PaneRef::bare(0));

    assert_eq!(
        drawn(&registry),
        settled,
        "none of these doors replaces data, so the drawn set is the fixture's",
    );
    assert_eq!(
        registry.data_generation(&id),
        generation,
        "one of these doors bumped the generation for a drawn set that did \
         not move, which discards the point pass's kept cull and projection \
         and rebuilds them identically",
    );
}
