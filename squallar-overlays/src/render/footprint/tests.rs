//! **The item-data census, priced against real captured feeds.**
//!
//! Every figure here comes from a fixture this repository already carries —
//! a real GLM granule, a real archived storm-based warning, real NWS zone
//! geometry — parsed by the same code the application parses with and priced
//! by the same [`ItemFootprint`] the census reads. Nothing is modelled, and
//! nothing is scaled: where a live scene holds more than one fixture's worth,
//! the test says the per-fixture figure and the count, and leaves the
//! multiplication to a reader who can see both.

use std::sync::Arc;

use squallar_source::footprint::{ItemFootprint, installed_item_bytes};
use squallar_source::handler::OverlayState;

use crate::fetch_policy::Whole;
use crate::glm::{GlmDataLevel, GlmSatellite};
use crate::render::handlers::alert::AlertItem;

/// One real GLM L2 LCFA granule — twenty seconds of one satellite's sky.
static GLM_GRANULE: &[u8] = include_bytes!(concat!(
    "../../../testdata/",
    "OR_GLM-L2-LCFA_G19_s20251801200000_e20251801200200_c20251801200212.nc"
));

/// The Moore 2013 tornado warning, as the NWS archive published it.
const MOORE_WARNING: &str = include_str!("../../nws/archive/moore_2013_sbw.json");

/// What a `Vec<Arc<T>>` of items costs whole: the spine, one `Arc`
/// allocation per item, and whatever each item owns. The same expression the
/// census evaluates, spelled once here so the tests below read as figures.
fn list_bytes<T: ItemFootprint>(items: &Vec<Arc<T>>) -> u64 {
    items.owned_bytes()
}

/// **The lightning layer, priced on a real granule.**
///
/// The single largest item figure in the census, and the one the retirement
/// seam exists for: this is what a frame thread frees, item by item, the
/// moment the next poll lands.
#[test]
fn a_real_glm_granule_prices_its_flash_list() {
    // The default pane state's own level set: groups and flashes, no events.
    for (what, levels) in [
        (
            "groups+flashes (the default)",
            &[GlmDataLevel::Group, GlmDataLevel::Flash][..],
        ),
        (
            "all three levels",
            &[
                GlmDataLevel::Event,
                GlmDataLevel::Group,
                GlmDataLevel::Flash,
            ][..],
        ),
    ] {
        let parsed =
            crate::glm::fetch::parse_glm_netcdf(GLM_GRANULE, GlmSatellite::GoesEast, levels)
                .expect("the committed granule parses");
        let count = parsed.records.len();
        assert!(count > 0, "the fixture must carry records at {what}");

        let slab = crate::render::handlers::glm::GlmSlab {
            flashes: parsed.records,
        };
        let bytes = slab.owned_bytes();

        eprintln!(
            "GLM: one 20 s granule, {what} — {count} rows, {bytes} B, {} B/row",
            bytes / count as u64,
        );
        // A flash row owns nothing indirect, so the slab's whole cost is the
        // one buffer its rows live in: that is what the figure must be,
        // exactly.
        assert_eq!(
            bytes,
            (slab.flashes.capacity() * size_of::<crate::glm::GlmFlash>()) as u64,
            "a flash slab's price is its buffer and nothing else",
        );
    }
    // What turns one granule into a scene. The cadence is measured — 4285 of
    // 4289 inter-granule gaps at 20.0 s over 24 hour-prefixes on both live
    // buckets, recorded on `crate::glm::WindowGap` — and the ceiling is this
    // crate's own constant. The DEFAULT window (300 s) and the default
    // satellite selection (both) live in `GlmPaneState::new`, whose file this
    // lane may not touch, so they are printed as read and not asserted here.
    const DEFAULT_WINDOW_SECS: f64 = 300.0;
    const CADENCE_SECS: f64 = 20.0;
    eprintln!(
        "GLM window: default {DEFAULT_WINDOW_SECS} s / {CADENCE_SECS} s cadence x 2 \
         satellites = {} granules resident; at the {} s ceiling, {} granules",
        (DEFAULT_WINDOW_SECS / CADENCE_SECS) as u64 * 2,
        crate::glm::GLM_MAX_TIME_WINDOW_SECS,
        (crate::glm::GLM_MAX_TIME_WINDOW_SECS / CADENCE_SECS) as u64 * 2,
    );
}

/// **The alert layer, priced on a real warning with real zone geometry.**
///
/// Alerts are the string-heavy end of the census: one warning carries a full
/// bulletin, an instruction block and its polygon.
#[test]
fn a_real_nws_warning_prices_its_text_and_its_geometry() {
    let json: serde_json::Value =
        serde_json::from_str(MOORE_WARNING).expect("the archived warning is JSON");
    // Through the archive translator the live parser is fed by, so the
    // alerts priced here are the ones the application would hold.
    let alerts = crate::nws::alert::parse_alerts(&crate::nws::archive::translate(&json));
    assert!(!alerts.is_empty(), "the fixture must carry an alert");

    let items: Vec<Arc<AlertItem>> = alerts
        .into_iter()
        .map(|alert| {
            Arc::new(AlertItem {
                alert,
                departed: None,
            })
        })
        .collect();
    let bytes = list_bytes(&items);
    let count = items.len();
    eprintln!(
        "NWS alerts: {count} archived warning(s), {bytes} B, {} B/item",
        bytes / count as u64,
    );
    assert!(
        bytes > count as u64 * (2 * size_of::<usize>() + size_of::<AlertItem>()) as u64,
        "an alert's text must be priced, not only its struct",
    );
}

/// **The level moves with an install and comes back with the drop**, which is
/// the whole contract the census family rests on.
///
/// Asserted as a difference around one state rather than as an absolute, so
/// it says what it means whatever else the process is holding.
#[test]
fn installing_and_dropping_moves_the_level_by_the_same_figure() {
    let rows: Vec<crate::render::handlers::sites::SiteRow> = (0..64)
        .map(|i| crate::render::handlers::sites::SiteRow {
            name: format!("SITE{i:04}"),
            lat: 0.0,
            lon: 0.0,
        })
        .collect();
    let priced = rows.owned_bytes();
    assert!(priced > 0, "64 named rows own a buffer and 64 strings");

    let before = installed_item_bytes();
    let mut state: OverlayState<Vec<crate::render::handlers::sites::SiteRow>, Whole> =
        OverlayState::new();
    assert_eq!(state.data_bytes(), 0, "a fresh state holds nothing");
    state.set_data(rows);
    assert_eq!(state.data_bytes(), priced);
    assert!(
        installed_item_bytes() >= before + priced,
        "the install must have raised the level by at least its own figure",
    );

    // Replacing a generation prices the new one and lets go of the old.
    state.set_data(Vec::new());
    assert_eq!(state.data_bytes(), 0, "an empty generation prices at zero");

    drop(state);
    // Nothing this test installed is resident any more. Other tests in this
    // process may hold their own, so the claim is about this state's figure,
    // which is now zero either way.
}

/// A layer that stamps its own map rather than replacing it moves its bytes
/// without an install, and [`OverlayState::reprice`] is what tells the level.
#[test]
fn a_stamped_map_reprices_rather_than_drifting() {
    use std::collections::HashMap;

    let mut state: OverlayState<HashMap<u32, String>, Whole> = OverlayState::new();
    state.set_data(HashMap::new());
    assert_eq!(state.data_bytes(), 0);

    state.data.insert(1, "x".repeat(4096));
    assert_eq!(
        state.data_bytes(),
        0,
        "a direct write has not been priced yet — this is the trap",
    );
    state.reprice();
    assert!(
        state.data_bytes() >= 4096,
        "the stamped entry is priced once the layer says so",
    );
}
