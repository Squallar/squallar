//! **A retained `Vec` is sized by what it holds, not by what it was offered.**
//!
//! `parse_alerts` walks a `FeatureCollection` and drops features it cannot
//! draw. The result is not scratch: it is moved into
//! [`ActiveAlerts`](squallar_overlays::nws::fetch::ActiveAlerts) and parked in
//! the layer's state, so every slot the walk reserved and did not fill is
//! resident for as long as the round is.
//!
//! It is also **visible in the census**: `ItemFootprint for Vec` prices
//! `capacity`, not `len` — "the allocator is holding the capacity", as
//! `squallar-source/src/footprint.rs` puts it — so slack here is reported as
//! this layer's memory, which is the correct reading of a real cost.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Delegates to the shipped counter so `squallar_alloc::live_bytes()` reads a
/// real heap here, and counts the **grant calls** on top, which that counter
/// does not expose.
struct CountingCalls;

static GRANTS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingCalls {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        GRANTS.fetch_add(1, Relaxed);
        unsafe { squallar_alloc::Counting.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { squallar_alloc::Counting.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        GRANTS.fetch_add(1, Relaxed);
        unsafe { squallar_alloc::Counting.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingCalls = CountingCalls;

/// A feed of `drawable` features that parse and `dropped` that do not.
///
/// The dropped ones carry an `event` and no geometry and no `affectedZones`,
/// which is the "nothing to render" arm — the real shape of a watch whose
/// zones the feed omitted, not a malformed record.
fn feed(drawable: usize, dropped: usize) -> serde_json::Value {
    let mut features = Vec::new();
    for i in 0..drawable {
        features.push(serde_json::json!({
            "properties": {
                "id": format!("urn:oid:2.49.0.1.840.0.drawable.{i}"),
                "event": "Severe Thunderstorm Warning",
                "effective": "2026-09-10T14:00:00Z",
                "expires": "2026-09-10T15:00:00Z",
            },
            "geometry": {
                "type": "Polygon",
                "coordinates": [[[-97.5, 35.0], [-97.5, 36.0], [-96.5, 36.0], [-97.5, 35.0]]],
            },
        }));
    }
    for i in 0..dropped {
        features.push(serde_json::json!({
            "properties": {
                "id": format!("urn:oid:2.49.0.1.840.0.zoneless.{i}"),
                "event": "Flood Watch",
                "effective": "2026-09-10T14:00:00Z",
                "expires": "2026-09-10T15:00:00Z",
            },
            "geometry": serde_json::Value::Null,
        }));
    }
    serde_json::json!({ "features": features })
}

/// **The reservation tracks the alerts kept, not the features offered.**
///
/// Red on `95980a8de`: `Vec::with_capacity(features.len())` reserves a slot
/// for every feature in the response, and the two `continue`s above the
/// `push` mean the slots for undrawable features are never filled. They are
/// not returned either — the `Vec` is moved out at its reserved capacity.
#[test]
fn the_alert_vec_is_sized_by_the_alerts_it_keeps() {
    let json = feed(300, 700);
    let alerts = squallar_overlays::nws::alert::parse_alerts(&json);

    assert_eq!(alerts.len(), 300, "the drawable features are the ones kept");

    let row = size_of::<squallar_overlays::nws::alert::NwsAlert>();
    let held = alerts.capacity() * row;
    let content = alerts.len() * row;

    assert!(
        alerts.capacity() <= alerts.len(),
        "the alert Vec is sized by the 1000 features offered, not the {} kept: \
         capacity {} rows ({held} B) for {} rows of content ({content} B), \
         {} slots ({} B) reserved and never filled, retained in ActiveAlerts",
        alerts.len(),
        alerts.capacity(),
        alerts.len(),
        alerts.capacity() - alerts.len(),
        held - content,
    );
}

/// The same property where **nothing** is dropped: the reservation must not
/// have been traded for a `Vec` that doubles past the answer. 1000 kept out
/// of 1000 offered is one grant of exactly 1000 rows, not 1024.
#[test]
fn a_feed_that_drops_nothing_still_reserves_exactly_what_it_keeps() {
    let json = feed(1000, 0);
    let alerts = squallar_overlays::nws::alert::parse_alerts(&json);

    assert_eq!(alerts.len(), 1000);
    assert_eq!(
        alerts.capacity(),
        1000,
        "a feed with nothing to drop must still land on an exact buffer; \
         capacity {} for 1000 rows means the walk grows by doubling now",
        alerts.capacity(),
    );
}

/// **The saving, and the price, in one place.** Not an assertion — a reading,
/// printed with its denominators so the figure in the commit message is
/// reproducible.
#[test]
fn reservation_figures() {
    const REPEATS: u32 = 20;

    let row = size_of::<squallar_overlays::nws::alert::NwsAlert>();
    println!("size_of::<NwsAlert>() = {row} B");

    for (drawable, dropped) in [(300usize, 700usize), (900, 100), (1000, 0)] {
        let json = feed(drawable, dropped);
        let offered = drawable + dropped;

        // Warm, then the timed run: the first parse of a shape pulls serde's
        // scratch in and would price the fixture, not the walk.
        drop(squallar_overlays::nws::alert::parse_alerts(&json));

        let before_bytes = squallar_alloc::live_bytes().expect("the counter is installed");
        let before_grants = GRANTS.load(Relaxed);
        let start = std::time::Instant::now();
        let mut alerts = squallar_overlays::nws::alert::parse_alerts(&json);
        for _ in 1..REPEATS {
            alerts = squallar_overlays::nws::alert::parse_alerts(&json);
        }
        let elapsed = start.elapsed() / REPEATS;
        let after_grants = GRANTS.load(Relaxed);
        let after_bytes = squallar_alloc::live_bytes().expect("the counter is installed");

        println!(
            "offered {offered} features, kept {} alerts: \
             capacity {} rows, buffer {} B, content {} B, slack {} B; \
             live_bytes {} -> {} (delta {} B, whole round incl. strings/rings); \
             grants over {REPEATS} calls {}; {:?} per call",
            alerts.len(),
            alerts.capacity(),
            alerts.capacity() * row,
            alerts.len() * row,
            (alerts.capacity() - alerts.len()) * row,
            before_bytes,
            after_bytes,
            after_bytes as i64 - before_bytes as i64,
            after_grants - before_grants,
            elapsed,
        );
        drop(alerts);
    }
}
