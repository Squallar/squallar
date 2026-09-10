//! **The parked observation list is sized by the pointers it holds.**
//!
//! `MetarHandler` turns a round's `Vec<MetarOb>` into the `Vec<Arc<MetarItem>>`
//! it parks. Written as a `collect`, that conversion hits the standard
//! library's **in-place** specialization: destination and source share an
//! alignment and the destination element is no larger, so the `Arc` pointers
//! are written over the observations and **the source buffer is kept as the
//! destination's**. A `MetarOb` is 272 B and an `Arc` is 8, so the list is
//! parked holding a buffer 34x the pointers in it.
//!
//! This is not slack that goes unnoticed. `ItemFootprint for Vec` prices
//! `capacity` — "the allocator is holding the capacity" — so the whole buffer
//! is reported in the `overlay items` census family
//! ([`installed_item_bytes`](squallar_overlays::render::overlay_state::installed_item_bytes)),
//! which is the correct reading of a cost that is really being paid.

use squallar_overlays::metar::fetch::MetarRound;
use squallar_overlays::metar::types::MetarOb;
use squallar_overlays::render::overlay_state::{OverlayRegistry, installed_item_bytes};
use squallar_source::handler::{OverlayFetchResult, PaneRef};
use squallar_source::id::known;

fn station(i: usize) -> MetarOb {
    MetarOb {
        station_id: format!("K{i:03}"),
        name: format!("station {i}"),
        lat: 35.0 + (i as f64) * 0.001,
        lon: -97.0 - (i as f64) * 0.001,
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
        raw_ob: format!("K{i:03} 041953Z AUTO"),
        clouds: Vec::new(),
        wx_string: None,
        obs_time: String::new(),
    }
}

/// **What the layer parks does not depend on how the round reserved.**
///
/// Two deliveries of the *same thousand observations*, differing only in the
/// capacity of the `Vec` carrying them. A conversion that builds its own
/// exactly sized list parks the same bytes both times; an in-place `collect`
/// inherits the round's buffer, so the over-reserved round parks 34 slots per
/// pointer against the exact round's, and the census -- which prices
/// `capacity` -- reports the difference as this layer's memory.
///
/// Threshold-free on purpose: it asserts two readings agree rather than
/// naming a byte count, so it cannot rot as `MetarItem` gains or loses a
/// field.
///
/// Red on `95980a8de`: 716,000 B against 1,532,000 B.
///
/// One test, start to finish in one thread, because `installed_item_bytes` is
/// a process-wide level a second test installing data would land inside.
#[test]
fn the_parked_figure_does_not_depend_on_the_rounds_reservation() {
    const STATIONS: usize = 1000;

    let parked = |reserve: usize| -> u64 {
        let mut registry = OverlayRegistry::default();
        let before = installed_item_bytes();
        let mut observations = Vec::with_capacity(reserve);
        observations.extend((0..STATIONS).map(station));
        assert_eq!(
            observations.capacity(),
            reserve,
            "the round reserved {reserve}"
        );
        registry.apply_fetch_result(
            OverlayFetchResult {
                kind: known::METAR,
                data: OverlayRegistry::metar_payload(MetarRound {
                    observations,
                    failed_networks: Vec::new(),
                    networks: vec!["OK"],
                }),
            },
            &PaneRef::across(&[]),
        );
        let installed = installed_item_bytes() - before;
        // Held here so the reading is taken while the data is still parked.
        drop(registry);
        installed
    };

    let exact = parked(STATIONS);
    let over_reserved = parked(STATIONS * 4);

    println!(
        "overlay items family, {STATIONS} stations: \
         exact round {exact} B, 4x-reserved round {over_reserved} B \
         (size_of::<MetarOb>() = {} B, size_of::<Arc<_>>() = {} B)",
        size_of::<MetarOb>(),
        size_of::<std::sync::Arc<()>>(),
    );

    assert_eq!(
        exact, over_reserved,
        "the same {STATIONS} observations parked {exact} B when the round's \
         Vec was exact and {over_reserved} B when it reserved 4x -- the \
         round's buffer is being carried into the parked list by an in-place \
         collect, and the census prices every slot of it",
    );
}
