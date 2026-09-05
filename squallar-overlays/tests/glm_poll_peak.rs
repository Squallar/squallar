//! **How many bytes the lightning cache is holding at the WORST moment of a
//! poll**, which is a different question from what it holds at rest.
//!
//! `GlmStore::retained_bytes` is a *level*: what the store holds between polls,
//! and it was already correct. A poll may not hold a `std::sync::Mutex` across
//! an `await`, so `poll_glm_into_store` clones the cache out at the top of the
//! future and writes it back at the end — list, download and parse in between.
//! While the rows were owned inline by each `CachedGranule`, that clone was a
//! second copy of every row, resident for the whole poll rather than
//! momentarily, and the peak was **twice** the level: 6,681,600 B at the
//! shipped default posture and 24,000,000 B at `MAX_RETAINED_FLASHES`.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `glm_granule_blocks.rs` sets out: the instrument counts real `GlobalAlloc`
//! calls, knows nothing about GLM, and so is able to disagree with the fix. It
//! tracks **live** bytes and the maximum they reach inside one window — an
//! alloc/free *total* cannot see a doubling that is freed again before the
//! window closes, which is exactly what this one is.
//!
//! **The denominator is the seeded cache**, stated in every assertion: `GRANULES
//! × FLASHES` rows of `FLASH_BYTES` each, already resident before the window
//! opens. What the window sees is therefore the poll's own excursion above the
//! level, and nothing of the level itself.
//!
//! **The poll is real and it FAILS at the listing**, deliberately. A loopback
//! bucket answering `500` to every request makes both satellites' listings fail,
//! so `fetch_glm_flashes` returns before `flashes_in_window` — and that Vec is
//! a second full-size allocation that both trees make, on top of the copy this
//! file is about. Excluding it is what leaves one variable in the window.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use chrono::{NaiveDateTime, TimeDelta};
use squallar_overlays::glm::fetch::{FLASH_BYTES, GlmCache, GlmStore, poll_glm_into_store};
use squallar_overlays::glm::{GlmDataLevel, GlmFlash, GlmSatellite};
use squallar_source::origins::DataSources;
use squallar_source::time::Residency;

/// Granules, and rows in each. Sixteen granules is about what
/// `MAX_RETAINED_FLASHES` holds at the one measured density, so the *shape* of
/// the fixture is the shipped one; the row count is smaller so a debug build
/// runs in well under a second. Every assertion is written against
/// `resident_bytes()`, so neither figure is baked into a bar.
const GRANULES: usize = 16;
const FLASHES: usize = 10_000;

fn resident_bytes() -> usize {
    GRANULES * FLASHES * FLASH_BYTES
}

thread_local! {
    /// Thread-local, not global: libtest runs this binary's tests concurrently
    /// and a global figure would be another test's live bytes as much as this
    /// one's. `const`-initialised so reading it inside the allocator cannot
    /// itself allocate.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    /// Bytes granted less bytes returned **on this thread, inside the open
    /// window**. Signed: a window that frees a block allocated before it opened
    /// goes negative, which is honest and harmless — the reading taken is the
    /// maximum, and it starts at zero.
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

/// Move the live figure and raise the high-water mark with it. Tolerates a
/// thread whose TLS is already torn down.
fn moved(by: i64) {
    let _ = LIVE.try_with(|live| {
        let now = live.get().wrapping_add(by);
        live.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

struct HighWater;

// SAFETY-adjacent note: every method delegates to `System` unchanged; the
// counting happens beside the pointer and never through it.
unsafe impl GlobalAlloc for HighWater {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && counting() {
            moved(layout.size() as i64);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() && counting() {
            moved(layout.size() as i64);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        if counting() {
            moved(-(layout.size() as i64));
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved_ptr.is_null() && counting() {
            moved(new_size as i64 - layout.size() as i64);
        }
        moved_ptr
    }
}

#[global_allocator]
static A: HighWater = HighWater;

/// The most bytes live at once inside `f`, over what was live when it opened.
///
/// The reading is taken before the caller gets control back, which is the rule
/// `glm_granule_blocks.rs` states: a figure read after the window closes is one
/// the next window on this thread may already have reset.
fn peak_during<T>(f: impl FnOnce() -> T) -> (T, i64) {
    LIVE.with(|c| c.set(0));
    PEAK.with(|c| c.set(0));
    COUNTING.with(|c| c.set(true));
    let out = f();
    COUNTING.with(|c| c.set(false));
    (out, PEAK.with(Cell::get))
}

fn as_of() -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

/// The residency a live pane's poll carries: one 300 s window ending at the
/// depicted instant, which is the shipped default `time_window_secs`.
fn a_live_window() -> Residency {
    Residency::over([(as_of() - TimeDelta::seconds(300), as_of())])
}

fn a_flash(i: usize) -> GlmFlash {
    GlmFlash {
        lat: 33.0 + (i % 400) as f64 * 0.01,
        lon: -99.0 + (i % 300) as f64 * 0.01,
        energy: Some(1e-14),
        area: None,
        // Inside the 300 s window above, so `evict_before` keeps every seeded
        // granule and the level under measurement is the whole fixture.
        time: as_of() - TimeDelta::seconds((i % 200) as i64),
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    }
}

/// A key shaped like the real ones, so `granule_start_of` parses it rather than
/// falling back — the fallback would date every granule identically.
fn granule_key(index: usize) -> String {
    let start = as_of() - TimeDelta::seconds(20 * index as i64);
    format!(
        "GLM-L2-LCFA/{}/OR_GLM-L2-LCFA_G19_s{}0_e{}0_c{}0.nc",
        start.format("%Y/%j/%H"),
        start.format("%Y%j%H%M%S"),
        (start + TimeDelta::seconds(20)).format("%Y%j%H%M%S"),
        (start + TimeDelta::seconds(21)).format("%Y%j%H%M%S"),
    )
}

/// A store already holding `GRANULES × FLASHES` rows — built outside every
/// measured window, because the level is not what is under test.
fn a_seeded_store() -> GlmStore {
    let store = GlmStore::default();
    store.with_mut(|cache: &mut GlmCache| {
        for g in 0..GRANULES {
            let flashes: Vec<GlmFlash> = (0..FLASHES).map(a_flash).collect();
            cache.insert(
                granule_key(g),
                as_of() - TimeDelta::seconds(20 * g as i64),
                flashes,
            );
        }
    });
    assert_eq!(
        store.retained_bytes(),
        resident_bytes(),
        "premise: the fixture must be resident, or every figure below is of an \
         empty cache",
    );
    store
}

/// A loopback bucket that answers `500` to everything, so both satellites'
/// listings fail and the poll returns `Err` before it builds an outcome.
///
/// No `#[ignore]`d network reach and no fixture parsing: what this file
/// measures is the cache handling around the await points, and a round that
/// cannot list still snapshots at the top and writes back at the end.
fn s3_refusing() -> DataSources {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut scratch = [0u8; 4096];
            let _ = stream.read(&mut scratch);
            let body = "<Error/>";
            let response = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/xml\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    DataSources {
        goes_east_bucket: "east".into(),
        goes_west_bucket: "west".into(),
        s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
        ..DataSources::production()
    }
}

/// A cleartext-capable client: `tls::client` sets `https_only`, which a
/// loopback URL cannot satisfy.
fn loopback_client() -> reqwest::Client {
    squallar_source::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

/// **The poll.** Its excursion above the resident level must not scale with the
/// flash count.
///
/// Red before the granule rows moved behind an `Arc`: the snapshot at the top
/// of the future copied every row, so the peak was the whole cache again.
#[test]
fn a_poll_does_not_hold_a_second_copy_of_the_granule_rows() {
    let store = a_seeded_store();
    let sources = s3_refusing();
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    // Everything but the poll itself is built outside the window: the runtime,
    // the client and the fixture are not what a poll allocates.
    let (result, peak) = peak_during(|| {
        runtime.block_on(poll_glm_into_store(
            &store,
            &client,
            &sources,
            &[GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            &[GlmDataLevel::Flash],
            as_of(),
            a_live_window(),
        ))
    });

    assert!(
        result.is_err(),
        "premise: both listings must fail, or the outcome's own row Vec is in \
         the window and the figure has two variables in it",
    );
    // Printed whether or not the bar is what fails: the figure is the point.
    println!(
        "poll peak over a resident cache of {} B ({GRANULES} granules × \
         {FLASHES} rows × {FLASH_BYTES} B): {peak} B = {:.4} B per resident row",
        resident_bytes(),
        peak as f64 / (GRANULES * FLASHES) as f64,
    );
    assert!(
        peak < (resident_bytes() / 4) as i64,
        "a poll over a {} B cache peaked at {peak} B above the level — {:.2} B \
         per resident row. The snapshot must be the granule map alone (a \
         `String` key and a table slot each, kilobytes at the shipped cap); a \
         second copy of the rows is {} B and is resident for the whole poll, \
         not momentarily",
        resident_bytes(),
        peak as f64 / (GRANULES * FLASHES) as f64,
        resident_bytes(),
    );
}

/// **The instrument's own liveness.** The same window that reads kilobytes
/// above must be able to read a whole-cache copy, or its small figure says
/// nothing about whether a copy happened.
///
/// This is the copy the poll used to make, spelled by hand through the cache's
/// own public reader.
#[test]
fn the_window_can_see_a_whole_cache_copy() {
    let store = a_seeded_store();

    let (copied, peak) = peak_during(|| {
        store.with_mut(|cache: &mut GlmCache| cache.all_flashes().cloned().collect::<Vec<_>>())
    });

    assert_eq!(
        copied.len(),
        GRANULES * FLASHES,
        "the copy must be of the whole fixture",
    );
    println!(
        "hand-written whole-cache copy of {} B: peak {peak} B",
        resident_bytes(),
    );
    assert!(
        peak >= resident_bytes() as i64,
        "copying {} rows out of the cache peaked at {peak} B, under the {} B \
         the rows themselves are — a window that cannot see this copy cannot \
         be quoted for the small figure the poll reads",
        GRANULES * FLASHES,
        resident_bytes(),
    );
}

/// **The dangerous direction.** A poll that fails partway must leave the store
/// exactly as it was — same granules, same rows in the same order, same
/// published level.
///
/// Not a property of the copy: the write-back is unconditional, so what this
/// pins is that the *failing* path reaches `replace` with a cache it did not
/// disturb. It would go red on a fix that mutated the store in place and left
/// an eviction or a half-inserted granule behind on the error path.
#[test]
fn a_poll_that_fails_at_the_listing_leaves_the_cache_exactly_as_it_was() {
    let store = a_seeded_store();
    let before_bytes = store.retained_bytes();
    let before = granule_contents(&store);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let result = runtime.block_on(poll_glm_into_store(
        &store,
        &loopback_client(),
        &s3_refusing(),
        &[GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        &[GlmDataLevel::Flash],
        as_of(),
        a_live_window(),
    ));

    assert!(result.is_err(), "premise: the poll must have failed");
    let (keys_after, rows_after) = granule_contents(&store);
    let (keys_before, rows_before) = before;
    assert_eq!(
        keys_after, keys_before,
        "a failed poll dropped or renamed granules",
    );
    assert_eq!(
        rows_after.len(),
        rows_before.len(),
        "a failed poll changed the row count: {} before, {} after",
        rows_before.len(),
        rows_after.len(),
    );
    assert!(
        rows_before
            .iter()
            .zip(rows_after.iter())
            .all(|(a, b)| same_flash(a, b)),
        "a failed poll rewrote a row",
    );
    assert_eq!(
        store.retained_bytes(),
        before_bytes,
        "the published level must be unmoved by a failed poll",
    );
}

/// The cache's contents as a value two readings can be compared by: which of
/// the seeded keys are still held, and every row it holds in a total order.
///
/// A row set is what catches a granule swapped for another's rows; the key list
/// is what catches a granule dropped whole, which a row set alone would only
/// see as a shorter total. `retained_bytes` beside them catches rows *added*
/// under a key this list does not name.
fn granule_contents(store: &GlmStore) -> (Vec<String>, Vec<GlmFlash>) {
    store.with_mut(|cache: &mut GlmCache| {
        let keys: Vec<String> = (0..GRANULES)
            .map(granule_key)
            .filter(|key| cache.contains_key(key))
            .collect();
        let mut rows: Vec<GlmFlash> = cache.all_flashes().cloned().collect();
        rows.sort_by(|a, b| {
            a.time
                .cmp(&b.time)
                .then(a.lat.total_cmp(&b.lat))
                .then(a.lon.total_cmp(&b.lon))
        });
        (keys, rows)
    })
}

/// Field-for-field, including the `Option`s — `GlmFlash` derives no `PartialEq`
/// and a comparison that skipped a column could not see a poll that rewrote it.
fn same_flash(a: &GlmFlash, b: &GlmFlash) -> bool {
    a.lat.to_bits() == b.lat.to_bits()
        && a.lon.to_bits() == b.lon.to_bits()
        && a.energy.map(f32::to_bits) == b.energy.map(f32::to_bits)
        && a.area.map(f32::to_bits) == b.area.map(f32::to_bits)
        && a.time == b.time
        && a.satellite == b.satellite
        && a.level == b.level
}
