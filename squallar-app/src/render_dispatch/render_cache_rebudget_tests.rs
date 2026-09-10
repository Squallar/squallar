//! **The byte budget can be re-applied after construction**, and shrinking it
//! evicts through the same primitive `insert` evicts through.
//!
//! Every budget here is built from what the cache itself prices a fixture
//! entry at against its capacity — `entry_budget_bytes`, the pixels, since the
//! capacity is a count of rasters — never a bracket constant. The census
//! figure (`entry_bytes`, pixels and hover) is what the returned entries are
//! checked against, because that is what the resident ledger fell by. Every
//! assertion is a property of the cache: within budget afterwards,
//! least-recently-used first, the returned entries are both ledgers' delta, a
//! raise evicts nothing, and an unpayable budget keeps what `insert` keeps.
//!
//! The fixtures are `render_cache_tests`' own, repeated because that module's
//! helpers are private to it and this file must not edit it.

use super::*;

/// The fixture side. Large enough that a raster is real money and small
/// enough that six of them are a test's worth of heap.
const SIDE: usize = 512;

/// A plan-view key at `elevation_tenths` tenths of a degree, built through the
/// one construction site so these tests key exactly as production does.
fn key(site: &str, elevation_tenths: i32) -> RenderKey {
    render_cache_key(
        site,
        &squallar_radar::fields::known::REFLECTIVITY,
        RenderView::PlanView,
        elevation_tenths as f32 / 10.0,
    )
}

/// The gates behind a raster of `side` — a full ring of 720 radials at the
/// gate count that side implies.
fn hover_field(side: usize) -> squallar_radar::render::polar::PolarField {
    use squallar_radar::render::polar::{PolarField, PolarGeometry, Wedge};
    const RADIALS: usize = 720;
    let gates = side / 4;
    let wedges = (0..RADIALS)
        .map(|i| Wedge {
            azimuth_deg: i as f32 * 0.5,
            half_width_deg: 0.25,
        })
        .collect();
    PolarField::from_parts(
        PolarGeometry::from_parts(wedges, 0.125, 0.25, None, gates),
        vec![0.0; RADIALS * gates],
    )
}

/// An entry that costs what a real raster of `SIDE` costs — the texture and
/// the gates behind it — with `range` as its identity so a test can tell
/// which render came back.
fn output(range: f64) -> CachedRenderOutput {
    CachedRenderOutput {
        surface: crate::channels::StillSurface::Raster(Arc::new(egui::ColorImage::new(
            [SIDE, SIDE],
            vec![egui::Color32::BLACK; SIDE * SIDE],
        ))),
        max_range_km: range,
        hover: Arc::new(squallar_radar::hover::HoverSource::resident(hover_field(
            SIDE,
        ))),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

/// What one fixture entry costs against the capacity — its pixels, as the
/// cache itself prices it — read off the buffer, never written down.
fn raster() -> usize {
    RenderCache::entry_budget_bytes(&output(0.0))
}

/// Which of the fixture keys `k` is: the `i` it was built with.
fn index_of(k: &RenderKey) -> usize {
    (0..8i32)
        .find(|&i| key("KTLX", i) == *k)
        .expect("a key this file built") as usize
}

/// Four entries under no byte bound, with the first one read after the others
/// arrived, so the recency order is `[1, 2, 3, 0]`: least-recently-used first
/// is distinguishable from oldest-inserted first.
fn four_with_zero_touched() -> RenderCache {
    let mut cache = RenderCache::new(usize::MAX, usize::MAX);
    for i in 0..4 {
        cache.insert(key("KTLX", i), output(i as f64), &[]);
    }
    assert!(cache.get(&key("KTLX", 0)).is_some());
    assert_eq!(
        cache.recency_order(),
        vec![
            key("KTLX", 1),
            key("KTLX", 2),
            key("KTLX", 3),
            key("KTLX", 0)
        ],
        "fixture: the read did not move 0 to the most-recently-used end",
    );
    cache
}

/// The shed path: within budget afterwards, least-recently-used first, and
/// what comes back is exactly what left — by identity and by bytes.
#[test]
fn shrinking_the_budget_evicts_least_recently_used_first_and_hands_them_back() {
    let mut cache = four_with_zero_touched();
    let order_before = cache.recency_order();
    let resident_before = cache.resident_bytes();
    let budgeted_before = cache.budgeted_bytes;
    let budget = 2 * raster();

    let evicted = cache.set_byte_capacity(budget, &[]);

    assert!(
        !evicted.is_empty(),
        "four rasters against two rasters' budget evicted nothing"
    );
    assert!(
        cache.budgeted_bytes <= budget,
        "{} B budgeted against a {budget} B budget after the shrink",
        cache.budgeted_bytes,
    );
    assert_eq!(
        evicted.len(),
        2,
        "two rasters' budget did not leave two rasters resident"
    );

    // Least-recently-used first: the evicted identities are the front of the
    // recency order, in that order.
    let evicted_ids: Vec<f64> = evicted.iter().map(|e| e.max_range_km).collect();
    let expected_ids: Vec<f64> = order_before[..evicted.len()]
        .iter()
        .map(|k| index_of(k) as f64)
        .collect();
    assert_eq!(
        evicted_ids, expected_ids,
        "not the least-recently-used entries, or not in that order"
    );

    // Exactly the evicted: every survivor is the rest of the recency order and
    // still answers with its own render; nothing evicted is still findable.
    let survivors = &order_before[evicted.len()..];
    assert_eq!(cache.entry_count(), survivors.len());
    for k in survivors {
        assert_eq!(
            cache.get(k).map(|c| c.max_range_km),
            Some(index_of(k) as f64),
            "a survivor was lost or swapped for another render",
        );
    }
    for k in &order_before[..evicted.len()] {
        assert!(cache.get(k).is_none(), "an evicted entry is still resident");
    }

    // The bytes handed back are the bytes both ledgers gave up: the census
    // figure the discard path is priced at, and the budgeted one that decided.
    let returned: usize = evicted.iter().map(RenderCache::entry_bytes).sum();
    assert_eq!(
        returned,
        resident_before - cache.resident_bytes(),
        "the returned entries are not what the resident figure fell by"
    );
    let returned_budgeted: usize = evicted.iter().map(RenderCache::entry_budget_bytes).sum();
    assert_eq!(
        returned_budgeted,
        budgeted_before - cache.budgeted_bytes,
        "the returned entries are not what the budgeted figure fell by"
    );
}

/// A raise evicts nothing and returns nothing — and does take effect, which is
/// the difference between a raise and a no-op.
#[test]
fn raising_the_budget_evicts_nothing() {
    let mut cache = RenderCache::new(usize::MAX, 4 * raster());
    for i in 0..4 {
        cache.insert(key("KTLX", i), output(i as f64), &[]);
    }
    assert_eq!(cache.entry_count(), 4, "fixture: four rasters did not fit");
    let order_before = cache.recency_order();
    let resident_before = cache.resident_bytes();

    let evicted = cache.set_byte_capacity(8 * raster(), &[]);
    assert!(
        evicted.is_empty(),
        "raising the budget evicted {} entries",
        evicted.len()
    );
    assert_eq!(cache.entry_count(), 4);
    assert_eq!(
        cache.recency_order(),
        order_before,
        "a raise reordered the queue"
    );
    assert_eq!(
        cache.resident_bytes(),
        resident_before,
        "a raise moved the ledger"
    );

    let evicted = cache.set_byte_capacity(usize::MAX, &[]);
    assert!(evicted.is_empty(), "raising to unbounded evicted something");

    // The raise was written, not just harmless: a fifth entry now fits where
    // the old budget would have evicted for it.
    let evicted = cache.set_byte_capacity(8 * raster(), &[]);
    assert!(evicted.is_empty());
    cache.insert(key("KTLX", 4), output(4.0), &[]);
    assert_eq!(
        cache.entry_count(),
        5,
        "the raised budget was not the one the next insert was held to"
    );
}

/// A budget no single entry can pay: `insert` keeps a floor rather than
/// evicting everything, and shrinking keeps the same floor — read off `insert`
/// on a fresh cache, not written down here.
#[test]
fn shrinking_below_one_entry_keeps_what_insert_keeps() {
    // What `insert` keeps when the budget cannot pay for one entry.
    let mut by_insert = RenderCache::new(usize::MAX, 1);
    for i in 0..4 {
        by_insert.insert(key("KTLX", i), output(i as f64), &[]);
    }
    let floor = by_insert.entry_count();
    assert!(
        floor < 4,
        "precondition: a 1 B budget held all four entries, so nothing below binds"
    );
    assert!(
        by_insert.resident_bytes() > 1,
        "precondition: the floor is not over the budget, so this is not the floor case"
    );

    let mut cache = four_with_zero_touched();
    let order_before = cache.recency_order();
    let evicted = cache.set_byte_capacity(1, &[]);

    assert_eq!(
        cache.entry_count(),
        floor,
        "shrinking kept a different floor from the one `insert` keeps"
    );
    assert_eq!(evicted.len(), 4 - floor);
    // The same floor in kind, too: the most-recently-used survive on both paths.
    assert_eq!(
        cache.recency_order(),
        order_before[4 - floor..].to_vec(),
        "the shrink's survivors are not the most-recently-used entries"
    );
    assert_eq!(
        by_insert.recency_order(),
        (4 - floor..4)
            .map(|i| key("KTLX", i as i32))
            .collect::<Vec<_>>(),
        "`insert`'s survivors are not the most-recently-used entries"
    );
}

/// The two paths are one accounting: for any budget, shrinking a full cache to
/// it leaves what a cache built at that budget would have kept — the same
/// keys in the same order at the same resident figure. This is what makes a
/// separate eviction loop in `set_byte_capacity` a regression rather than a
/// style choice: the day the figure `insert` compares changes, so does this.
#[test]
fn shrinking_agrees_with_what_insert_would_have_kept() {
    const ENTRIES: i32 = 6;
    let raster = raster();
    for budget in [
        1,
        raster / 2,
        raster,
        raster + 1,
        2 * raster + 1,
        3 * raster,
        5 * raster,
        ENTRIES as usize * raster,
    ] {
        let mut shrunk = RenderCache::new(usize::MAX, usize::MAX);
        let mut built = RenderCache::new(usize::MAX, budget);
        for i in 0..ENTRIES {
            shrunk.insert(key("KTLX", i), output(i as f64), &[]);
            built.insert(key("KTLX", i), output(i as f64), &[]);
        }
        let evicted = shrunk.set_byte_capacity(budget, &[]);

        assert_eq!(
            shrunk.recency_order(),
            built.recency_order(),
            "at a {budget} B budget the shrink kept a different set from an insert",
        );
        assert_eq!(
            shrunk.resident_bytes(),
            built.resident_bytes(),
            "at a {budget} B budget the two census ledgers disagree",
        );
        assert_eq!(
            shrunk.budgeted_bytes, built.budgeted_bytes,
            "at a {budget} B budget the two budgeted ledgers disagree",
        );
        assert_eq!(
            evicted.len(),
            ENTRIES as usize - built.entry_count(),
            "at a {budget} B budget the shrink handed back a different count from what it dropped",
        );
    }
}

/// The dispatcher's door does the same thing to the cache it owns.
#[test]
fn the_dispatchers_door_re_applies_the_budget_to_its_own_cache() {
    let mut d = RenderDispatcher::new();
    d.render_cache = RenderCache::new(usize::MAX, usize::MAX);
    for i in 0..4 {
        d.cache_render(
            &format!("SITE{i}"),
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.5,
            output(i as f64),
        );
    }
    assert_eq!(d.render_cache.entry_count(), 4);

    let evicted = d.set_render_cache_budget_bytes(2 * raster());

    assert_eq!(evicted.len(), 2);
    assert_eq!(d.render_cache.entry_count(), 2);
    assert!(
        d.get_cached_render(
            "SITE0",
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.5
        )
        .is_none(),
        "the least-recently-used survived the dispatcher's shrink"
    );
    assert_eq!(
        d.get_cached_render(
            "SITE3",
            RadarProduct::Reflectivity,
            RenderView::PlanView,
            0.5
        )
        .map(|c| c.max_range_km),
        Some(3.0),
        "the most-recently-used did not survive the dispatcher's shrink"
    );
}
