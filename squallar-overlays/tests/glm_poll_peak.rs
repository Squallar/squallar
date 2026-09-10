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
//! shipped default posture and 24,000,000 B at `MAX_RETAINED_FLASHES`, both
//! measured against the 48-byte row of the time.
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
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
        energy: 1e-14,
        area: f32::NAN,
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
            0,
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
        0,
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

/// Field-for-field and bit-for-bit — `GlmFlash` derives no `PartialEq` and a
/// comparison that skipped a column could not see a poll that rewrote it. The
/// energy and area sentinels are compared as raw bits rather than through
/// `energy_j`/`area_km2`, which is the stronger claim: it refuses a NaN payload
/// that changed as well as a value that did.
fn same_flash(a: &GlmFlash, b: &GlmFlash) -> bool {
    a.lat.to_bits() == b.lat.to_bits()
        && a.lon.to_bits() == b.lon.to_bits()
        && a.energy.to_bits() == b.energy.to_bits()
        && a.area.to_bits() == b.area.to_bits()
        && a.time == b.time
        && a.satellite == b.satellite
        && a.level == b.level
}

// ── A poll that SUCCEEDS: what a cold round allocates on its way to a frame ─
//
// The three tests above deliberately fail at the listing, so nothing
// downstream of it runs and the figure has one variable in it. What follows is
// the other half — a round that lists, downloads, parses, caches and builds a
// delivery, against an **empty** store, so the level is zero and the peak *is*
// the round's own allocation.
//
// **Three tests, because a cold round's costs do not peak together and a
// single window cannot separate them.** The download phase's file buffers are
// all freed before `flashes_in_window` runs, so the round's global maximum is
// the delivery and the download term is invisible inside it; and the
// transport's own buffers vary between runs by more than a granule, so a term
// worth one granule is invisible inside *that*. Each test therefore carries the
// fixture and the window that put its own term on top:
//
// - `a_cold_poll_holds_one_row_buffer_per_delivery` — row-dense granules, whole
//   round: the delivery, which is the round's global peak.
// - `a_batch_holds_no_more_bodies_than_the_concurrency_cap` — granules the size
//   of the product's on the wire carrying few rows, a batch wider than the cap:
//   the bodies in flight.
// - `a_granule_under_the_parser_exists_once` — one granule, no network, the
//   window around the parse alone: the parse door's copy.
//
// Every figure is printed beside the denominator it was taken in. **They are
// never added**: three different fixtures, three different windows.

use squallar_overlays::glm::fetch::GRANULE_FETCH_CONCURRENCY;

/// Granules one satellite publishes into a 300 s window at one per 20 s. The
/// shipped default posture polls both, so a cold round is 30 objects.
const COLD_GRANULES_PER_SAT: usize = 15;

/// Rows in each of the two shipped levels of one **row-dense** granule.
///
/// Chosen by the granule's file size, not by its row count: ten `f32` datasets
/// of this length write a 281,461 B file, within 0.4% of the measured product
/// granule `squallar-overlays/testdata/OR_GLM-L2-LCFA_G19_…nc` (280,380 B).
/// The product packs shorts with a scale factor where this fixture writes
/// `f32`, so it carries fewer rows than the product does for the same bytes —
/// the figures below are in this fixture's rows, and the file size is read off
/// the fixture rather than assumed.
const COLD_ROWS_PER_LEVEL: usize = 7_000;

/// The shipped default: groups and flashes, events off.
const COLD_LEVELS: [GlmDataLevel; 2] = [GlmDataLevel::Group, GlmDataLevel::Flash];

fn cold_rows_total() -> usize {
    COLD_ROWS_PER_LEVEL * COLD_LEVELS.len() * COLD_GRANULES_PER_SAT * 2
}

/// A granule with `rows` rows in each shipped level, all stamped inside the
/// 20 s its key names, plus `filler` bytes of variables the parser never reads.
///
/// The filler is what lets a granule be the product's size on the wire without
/// being the product's size in rows — the real L2 LCFA file carries event-level
/// and identifier columns that the shipped groups+flashes posture never opens,
/// and `Granule::from_vec` holds the whole file either way.
///
/// `lat_base` separates the two satellites' rows, so a granule served from the
/// wrong bucket would be visible rather than silently equivalent.
fn a_granule(start: NaiveDateTime, lat_base: f32, rows: usize, filler: usize) -> Vec<u8> {
    let lats: Vec<f32> = (0..rows)
        .map(|i| lat_base + (i % 400) as f32 * 0.01)
        .collect();
    let lons: Vec<f32> = (0..rows).map(|i| -99.0 + (i % 300) as f32 * 0.01).collect();
    let energies: Vec<f32> = vec![1.0e-14; rows];
    let areas: Vec<f32> = vec![128.0; rows];
    // Under a second, so every row lands in the 20 s its key covers.
    let offsets: Vec<f32> = (0..rows).map(|i| (i % 10) as f32 * 0.05).collect();

    let mut file = hdf5_pure::FileBuilder::new();
    file.set_attr(
        "time_coverage_start",
        hdf5_pure::AttrValue::String(format!("{}Z", start.format("%Y-%m-%dT%H:%M:%S.0"))),
    );
    {
        let mut put = |name: &str, values: &[f32], units: Option<&str>| {
            let var = file.create_dataset(name);
            var.with_f32_data(values);
            if let Some(u) = units {
                var.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
            }
        };
        put("flash_lat", &lats, None);
        put("flash_lon", &lons, None);
        put("flash_energy", &energies, Some("J"));
        put("flash_area", &areas, Some("km2"));
        put("flash_time_offset_of_first_event", &offsets, None);

        put("group_lat", &lats, None);
        put("group_lon", &lons, None);
        put("group_energy", &energies, Some("J"));
        put("group_area", &areas, Some("km2"));
        put("group_time_offset", &offsets, None);

        if filler > 0 {
            // Names the shipped posture never opens, one `f32` per four bytes.
            let padding: Vec<f32> = vec![1.0; filler.div_ceil(4 * 4)];
            for name in [
                "event_id",
                "event_parent_group_id",
                "group_parent_flash_id",
                "flash_frame_time_offset_of_last_event",
            ] {
                put(name, &padding, None);
            }
        }
    }
    file.finish().expect("write a granule")
}

/// The archive's keys for one satellite, newest first — the same shape
/// [`granule_key`] builds, with the satellite's own designator so the two
/// buckets' keyspaces are disjoint exactly as the real ones are.
///
/// The newest granule starts one full span **before** the depicted instant, not
/// on it: a granule keyed at `as_of` carries rows stamped after `as_of`, and
/// `flashes_in_window` culls those from the delivery while the cache keeps
/// them. That gap is real behaviour, and it would put a second variable in
/// every figure below.
fn archive_keys(designator: &str, count: usize) -> Vec<(String, NaiveDateTime)> {
    (0..count)
        .map(|i| {
            let start = as_of() - TimeDelta::seconds(20 * (i as i64 + 1));
            let key = format!(
                "GLM-L2-LCFA/{}/OR_GLM-L2-LCFA_{designator}_s{}0_e{}0_c{}0.nc",
                start.format("%Y/%j/%H"),
                start.format("%Y%j%H%M%S"),
                (start + TimeDelta::seconds(20)).format("%Y%j%H%M%S"),
                (start + TimeDelta::seconds(21)).format("%Y%j%H%M%S"),
            );
            (key, start)
        })
        .collect()
}

/// A loopback bucket that answers **per prefix, per object and per bucket**: a
/// `list-type=2` request returns the seeded keys under the prefix it names in
/// the bucket its path addresses, and an object request returns that granule's
/// own bytes.
///
/// Modelled on `glm::fetch::tests::s3_archive`, which an integration test
/// cannot reach; the addition is that the two buckets carry **different**
/// granule sets, which is what makes a two-satellite poll download twice.
///
/// **A thread per connection, so the bodies overlap.** A serial acceptor
/// answers one request at a time however many the client has in flight, which
/// makes a poll's concurrent slots hold one arrived body between them: a
/// measurement of the download phase taken against it cannot see a per-slot
/// cost at all, and a bar written over one would be vacuous. S3 serves
/// concurrently, so this does.
///
/// Every byte it allocates is on a server thread, and the high-water mark is
/// thread-local — the reply buffers are in no figure below.
/// `object_gets` counts **object** requests this archive answered and not
/// listings — the wire's own reading of what a poll asked for, per archive
/// instance and therefore immune to the neighbouring arms in this binary that
/// the process-global `gauge` counters are not.
use std::sync::atomic::AtomicUsize;

fn s3_two_bucket_archive(
    east: Vec<(String, Vec<u8>)>,
    west: Vec<(String, Vec<u8>)>,
    gate: Option<Arc<BodiesInFlight>>,
    object_gets: Arc<AtomicUsize>,
) -> DataSources {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let east = Arc::new(east);
    let west = Arc::new(west);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let east = Arc::clone(&east);
            let west = Arc::clone(&west);
            let gate = gate.clone();
            let object_gets = Arc::clone(&object_gets);
            std::thread::spawn(move || {
                let mut scratch = [0u8; 8192];
                let read = stream.read(&mut scratch).unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]).to_string();
                let line = request.lines().next().unwrap_or("").to_string();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let granules: &[(String, Vec<u8>)] = if path.starts_with("/west/") {
                    &west
                } else {
                    &east
                };
                // Objects only. A listing is one request per satellite and is
                // not a body in flight; holding it would gate the round on a
                // window that can never fill.
                if !is_a_listing(&path) {
                    object_gets.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let Some(gate) = &gate {
                        gate.hold(GRANULE_FETCH_CONCURRENCY, A_QUIET_WIRE);
                    }
                }
                let _ = stream.write_all(&archive_reply(&path, granules));
                let _ = stream.flush();
            });
        }
    });
    DataSources {
        goes_east_bucket: "east".into(),
        goes_west_bucket: "west".into(),
        s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
        ..DataSources::production()
    }
}

/// The one route table [`s3_two_bucket_archive`] serves: a path carrying
/// `prefix=` is a listing, anything else addresses an object.
///
/// Spelled once and read twice — [`archive_reply`] routes on it and the
/// in-flight gate holds everything it answers `false` for.
fn is_a_listing(path: &str) -> bool {
    path.contains("prefix=")
}

/// A listing's keys under the prefix it names, or one object's own bytes.
fn archive_reply(path: &str, granules: &[(String, Vec<u8>)]) -> Vec<u8> {
    if let Some(rest) = path.split("prefix=").nth(1) {
        let prefix = rest.split('&').next().unwrap_or("");
        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>bucket</Name><IsTruncated>false</IsTruncated>",
        );
        for key in granules
            .iter()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with(prefix))
        {
            body.push_str(&format!(
                "<Contents><Key>{key}</Key><Size>1</Size></Contents>"
            ));
        }
        body.push_str("</ListBucketResult>");
        return http_reply("200 OK", "application/xml", body.as_bytes());
    }
    match granules.iter().find(|(k, _)| path.ends_with(k.as_str())) {
        Some((_, bytes)) => http_reply("200 OK", "application/octet-stream", bytes),
        None => http_reply("404 Not Found", "application/xml", b"<Error/>"),
    }
}

fn http_reply(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len(),
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// How long the gate below waits for another request before deciding the client
/// has nothing more to send.
///
/// It bounds how long the fixture hunts for a violation, never the verdict: a
/// window that closes under the cap is discarded and re-armed rather than read.
const A_QUIET_WIRE: Duration = Duration::from_secs(1);

/// **How many object requests the archive is holding at once**, and the most it
/// ever held — the download phase's concurrency counted in bodies rather than
/// weighed in bytes.
///
/// A byte high-water mark of work in flight is a scheduling outcome wearing
/// load-immune units: it reads whatever had arrived at the instant the maximum
/// fell, so the figure moves with the load on the box while the assertion over
/// it reads like a property of the code. A count of requests the server is
/// holding does not move: the client cannot have more requests outstanding than
/// its stream lets it start, however fast or slowly the box runs them.
///
/// The gate is what makes that count a measurement rather than a sighting.
/// Every object request blocks in [`hold`](Self::hold), so **no reply can be
/// sent while the client is still free to send one more** — the fixture waits
/// for the violation instead of hoping to be looking when it happens. It lets
/// the requests it holds go when either
///
/// - one more than `cap` are held, which is the violation itself, or
/// - `quiet` passes with nothing new arriving, which is the client having
///   nothing more to send.
///
/// A window that expires under the cap is not a reading, so the gate re-arms
/// and the next slots to fill get another turn. It latches open once it has
/// seen the cap filled, and the rest of the round runs at full speed.
struct BodiesInFlight {
    state: Mutex<InFlight>,
    arrived: Condvar,
}

struct InFlight {
    /// Requests received and not yet let go.
    held: usize,
    /// The most held at once, which is the figure under test.
    peak: usize,
    /// Bumped when a generation is let go; each waiter watches its own value.
    generation: u64,
    /// Set once a reading has been taken: every request after passes through.
    latched: bool,
    last_arrival: Instant,
}

impl BodiesInFlight {
    fn new() -> Arc<Self> {
        Arc::new(BodiesInFlight {
            state: Mutex::new(InFlight {
                held: 0,
                peak: 0,
                generation: 0,
                latched: false,
                last_arrival: Instant::now(),
            }),
            arrived: Condvar::new(),
        })
    }

    /// Hold this request until the batch cannot grow any further, counting it
    /// while it waits.
    ///
    /// Every wait is bounded by `quiet` from the *last* arrival, so a waiter
    /// always leaves and the fixture cannot hang on a client that stops.
    fn hold(&self, cap: usize, quiet: Duration) {
        let mut state = self.state.lock().expect("in-flight gate");
        state.held += 1;
        state.peak = state.peak.max(state.held);
        state.last_arrival = Instant::now();
        if !state.latched {
            if state.held > cap {
                // The claim is already false and the reading is taken: let the
                // round run rather than spending a window on it.
                state.latched = true;
                state.generation += 1;
            }
            let mine = state.generation;
            self.arrived.notify_all();
            while state.generation == mine {
                match quiet.checked_sub(state.last_arrival.elapsed()) {
                    None => {
                        state.latched = state.peak >= cap;
                        state.generation += 1;
                    }
                    Some(left) => {
                        state = self
                            .arrived
                            .wait_timeout(state, left)
                            .expect("in-flight gate")
                            .0;
                    }
                }
            }
        }
        state.held -= 1;
        drop(state);
        self.arrived.notify_all();
    }

    fn peak(&self) -> usize {
        self.state.lock().expect("in-flight gate").peak
    }
}

/// Run one poll against a two-bucket archive over an **empty** store, and
/// report the round's peak above that zero level.
fn peak_of_a_cold_poll(
    east: Vec<(String, Vec<u8>)>,
    west: Vec<(String, Vec<u8>)>,
    window: Residency,
    gate: Option<Arc<BodiesInFlight>>,
) -> (
    GlmStore,
    squallar_overlays::glm::GlmFetchOutcome,
    i64,
    Arc<AtomicUsize>,
) {
    let object_gets = Arc::new(AtomicUsize::new(0));
    let sources = s3_two_bucket_archive(east, west, gate, Arc::clone(&object_gets));
    let store = GlmStore::default();
    assert_eq!(
        store.retained_bytes(),
        0,
        "premise: a COLD poll, so the level is zero and the peak below is the \
         round's own allocation and nothing else",
    );
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    // Everything but the poll is built outside the window: the archive, the
    // runtime and the client are not what a round allocates.
    let (result, peak) = peak_during(|| {
        runtime.block_on(poll_glm_into_store(
            &store,
            &client,
            &sources,
            &[GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            &COLD_LEVELS,
            as_of(),
            window,
            0,
        ))
    });
    (
        store,
        result.expect("the cold poll must succeed"),
        peak,
        object_gets,
    )
}

/// **The delivery.** A round that caches `N` rows must not hold more than one
/// further `N` rows to hand them on.
///
/// Red before `flashes_in_window` was presized: `collect` over a `Filter`
/// starts at four rows and doubles, so the delivery's *capacity* was the next
/// power of two above the row count — 524,288 rows for 420,000 of them,
/// 25,165,824 B carrying 20,160,000 B — and that capacity is not a transient.
/// It travels into `GlmFetchOutcome::flashes` and on into the render handler's
/// slab.
#[test]
fn a_cold_poll_holds_one_row_buffer_per_delivery() {
    let east: Vec<(String, Vec<u8>)> = archive_keys("G19", COLD_GRANULES_PER_SAT)
        .into_iter()
        .map(|(key, start)| (key, a_granule(start, 33.0, COLD_ROWS_PER_LEVEL, 0)))
        .collect();
    let west: Vec<(String, Vec<u8>)> = archive_keys("G18", COLD_GRANULES_PER_SAT)
        .into_iter()
        .map(|(key, start)| (key, a_granule(start, 41.0, COLD_ROWS_PER_LEVEL, 0)))
        .collect();
    let granule_bytes = east[0].1.len();

    let (store, outcome, peak, _gets) = peak_of_a_cold_poll(east, west, a_live_window(), None);

    let rows = cold_rows_total();
    assert_eq!(
        outcome.flashes.len(),
        rows,
        "premise: the round must have downloaded and parsed all {} objects — a \
         short delivery measures a different round",
        COLD_GRANULES_PER_SAT * 2,
    );
    assert_eq!(
        store.retained_bytes(),
        rows * FLASH_BYTES,
        "premise: every parsed row must be cached, or the peak below is of a \
         round that threw its work away",
    );

    let level = rows * FLASH_BYTES;
    println!(
        "cold poll (row-dense): {} granules × {} rows, file {granule_bytes} B \
         each = {rows} rows = {level} B cached; PEAK {peak} B = {:.3}× the \
         cached level",
        COLD_GRANULES_PER_SAT * 2,
        COLD_ROWS_PER_LEVEL * COLD_LEVELS.len(),
        peak as f64 / level as f64,
    );

    // One cached level plus one delivery, and a tenth of a level of slack for
    // the file buffers still in flight, the per-granule parse and the listings.
    // A delivery grown by doubling adds up to another whole level and cannot
    // fit under this.
    let bar = level * 2 + level / 10;
    assert!(
        peak < bar as i64,
        "a cold poll caching {level} B peaked at {peak} B ({:.3}× the level), \
         over the {bar} B that one cached level plus one delivery costs. The \
         delivery is a `Vec` presized from `GlmCache::retained_flashes`; grown \
         by doubling it settles on the next power of two above {rows} rows, \
         which is {} B for {level} B of rows",
        peak as f64 / level as f64,
        rows.next_power_of_two() * FLASH_BYTES,
    );
}

// ── The download phase, where the file buffers live ────────────────────────

/// Granules one satellite publishes into a 900 s window — **more than twice
/// [`GRANULE_FETCH_CONCURRENCY`]**, so the cap is what bounds the buffers in
/// flight and a batch run uncapped is visibly dearer than one run under it. A
/// live pane may ask for up to `GLM_MAX_TIME_WINDOW_SECS` (1800 s, 90
/// granules), so this is inside the shipped posture and not a span or a loop.
const WIRE_GRANULES_PER_SAT: usize = 45;

/// Rows in each shipped level of a **wire-sized** granule: few, so the file
/// buffers and not the rows are what the peak is made of.
const WIRE_ROWS_PER_LEVEL: usize = 200;

/// Bytes of unread variables padding a wire-sized granule up to the measured
/// product granule's 280,380 B.
const WIRE_FILLER_BYTES: usize = 272_000;

// The premise of `a_batch_holds_no_more_bodies_than_the_concurrency_cap`, and a
// compile-time one because both terms are constants: a batch no wider than the
// cap would leave the test measuring its own batch size, and the bar it sets
// would pass whether or not the stream was bounded at all.
const _: () = assert!(WIRE_GRANULES_PER_SAT > 2 * GRANULE_FETCH_CONCURRENCY);

fn a_wire_window() -> Residency {
    Residency::over([(
        as_of() - TimeDelta::seconds(20 * WIRE_GRANULES_PER_SAT as i64),
        as_of(),
    )])
}

/// **The parse door takes the body.** A granule under the reader must exist
/// once, not twice.
///
/// **Measured at the door, not around a poll**, and that is the finding as much
/// as the figure. `buffer_unordered` polls its sub-futures inline from one
/// task, so however many bodies are in flight, exactly **one parse runs at a
/// time**: the copy is one granule, not one per slot. A window taken around a
/// whole round cannot see 280 KB — the transport's own buffers move by more
/// than that between runs (6,970,916–8,415,961 B over five runs of
/// [`a_batch_holds_no_more_bodies_than_the_concurrency_cap`], a 1.4 MB spread),
/// and a bar written over that spread would be a bar this fix could not move.
///
/// The window here is the parse alone, and it is exact. `bytes` is allocated
/// **before** it opens, so a door that adopts the buffer neither allocates nor
/// frees the file inside it and the reading is the parse's own outputs; a door
/// that copies allocates one whole file inside it. Red before
/// `parse_glm_granule`: `Granule::open` copies, and the caller's buffer stays
/// alive beside the copy for the length of the parse.
#[test]
fn a_granule_under_the_parser_exists_once() {
    let start = as_of() - TimeDelta::seconds(20);
    let bytes = a_granule(start, 33.0, WIRE_ROWS_PER_LEVEL, WIRE_FILLER_BYTES);
    let file_bytes = bytes.len();

    let (parsed, peak) = peak_during(|| {
        squallar_overlays::glm::fetch::parse_glm_granule(
            bytes,
            GlmSatellite::GoesEast,
            &COLD_LEVELS,
        )
    });

    let rows = parsed.expect("the fixture parses").records.len();
    assert_eq!(
        rows,
        WIRE_ROWS_PER_LEVEL * COLD_LEVELS.len(),
        "premise: both shipped levels must have parsed, or the window is around \
         less work than a granule is",
    );
    println!(
        "parse door: a {file_bytes} B granule parsed to {rows} rows \
         ({} B); PEAK {peak} B = {:.3} granule bodies",
        rows * FLASH_BYTES,
        peak as f64 / file_bytes as f64,
    );
    assert!(
        peak < (file_bytes / 2) as i64,
        "parsing a {file_bytes} B granule peaked at {peak} B — over half the \
         file, which is what a second copy of it looks like. The door takes its \
         bytes by value so `Granule::from_vec` adopts the caller's buffer; \
         `Granule::open` copies the whole file and the caller's buffer lives on \
         beside it until the parse returns",
    );
}

/// **The concurrency cap is what bounds the bodies in flight**, not the size of
/// the batch.
///
/// The batch is 45 objects per satellite against a cap of
/// [`GRANULE_FETCH_CONCURRENCY`]; run uncapped it holds all 45. What is
/// asserted is **the count of bodies in flight**, read off the archive through
/// [`BodiesInFlight`], and it is the only figure here that is a property of the
/// code rather than of the box.
///
/// **The byte peak beside it is printed and never asserted, and that is the
/// finding.** It was the bar until 2026-09-09, set at one body per slot plus
/// half as much again for the transport and two cached levels — 11,913,720 B
/// against a 5,638,480 B capped cost and a 12,686,580 B uncapped one. A peak of
/// *in-flight* bytes is a scheduling outcome: it reads whatever had arrived at
/// the instant the maximum fell, so it moved with the load on the box while its
/// units read like a property of the code. Five runs on one idle box read
/// 11,972,932 / 11,973,188 / 12,104,068 / 12,104,132 / 12,104,260 B — over the
/// bar every time, with the gate below reporting the cap **exactly filled and
/// never exceeded** on the same runs. The bar had drifted into the top 6 % of
/// the gap between the two hypotheses it was there to separate, and lifting it
/// clear of the excursion would have put it over the uncapped cost, where it
/// would pass whether or not the stream was bounded at all.
///
/// The count has no such excursion, and the gate is what turns it from a
/// sighting into a measurement: the archive answers nothing while the client is
/// still free to send one more, so a batch that runs uncapped is *held* until
/// its 21st request has been counted.
#[test]
fn a_batch_holds_no_more_bodies_than_the_concurrency_cap() {
    let east: Vec<(String, Vec<u8>)> = archive_keys("G19", WIRE_GRANULES_PER_SAT)
        .into_iter()
        .map(|(key, start)| {
            (
                key,
                a_granule(start, 33.0, WIRE_ROWS_PER_LEVEL, WIRE_FILLER_BYTES),
            )
        })
        .collect();
    let west: Vec<(String, Vec<u8>)> = archive_keys("G18", WIRE_GRANULES_PER_SAT)
        .into_iter()
        .map(|(key, start)| {
            (
                key,
                a_granule(start, 41.0, WIRE_ROWS_PER_LEVEL, WIRE_FILLER_BYTES),
            )
        })
        .collect();
    let granule_bytes = east[0].1.len();

    let gate = BodiesInFlight::new();
    let (store, outcome, peak, _gets) =
        peak_of_a_cold_poll(east, west, a_wire_window(), Some(Arc::clone(&gate)));
    let bodies = gate.peak();

    let rows = WIRE_ROWS_PER_LEVEL * COLD_LEVELS.len() * WIRE_GRANULES_PER_SAT * 2;
    assert_eq!(
        outcome.flashes.len(),
        rows,
        "premise: the round must have downloaded and parsed all {} objects",
        WIRE_GRANULES_PER_SAT * 2,
    );
    assert_eq!(
        store.retained_bytes(),
        rows * FLASH_BYTES,
        "premise: every parsed row must be cached",
    );

    let level = rows * FLASH_BYTES;
    println!(
        "cold poll (wire-sized): {} granules × {} rows, file {granule_bytes} B \
         each = {rows} rows = {level} B cached; PEAK {bodies} bodies in flight \
         of a {WIRE_GRANULES_PER_SAT}-object batch against a cap of \
         {GRANULE_FETCH_CONCURRENCY} = {} B of bodies; byte peak of the whole \
         round, NOT asserted: {peak} B",
        WIRE_GRANULES_PER_SAT * 2,
        WIRE_ROWS_PER_LEVEL * COLD_LEVELS.len(),
        bodies * granule_bytes,
    );

    assert!(
        bodies >= GRANULE_FETCH_CONCURRENCY,
        "premise: the archive never held more than {bodies} of the \
         {WIRE_GRANULES_PER_SAT} objects at once, under the \
         {GRANULE_FETCH_CONCURRENCY} the cap allows, so the bar below was \
         never approached and says nothing. The gate holds every object \
         request until nothing new has arrived for {A_QUIET_WIRE:?}, so this \
         is not a slow box losing a race — it is a round that could not put \
         {GRANULE_FETCH_CONCURRENCY} requests in flight in that time",
    );
    assert!(
        bodies <= GRANULE_FETCH_CONCURRENCY,
        "a batch of {WIRE_GRANULES_PER_SAT} objects per satellite put {bodies} \
         bodies in flight at once, over the {GRANULE_FETCH_CONCURRENCY} the \
         cap allows — {} B of granule against the {} B a capped batch holds. \
         `download_and_parse_batch` must bound its stream by \
         GRANULE_FETCH_CONCURRENCY",
        bodies * granule_bytes,
        GRANULE_FETCH_CONCURRENCY * granule_bytes,
    );
}

// ── The retention ceiling, on the peak rather than on the leftovers ────────

/// Granules one satellite publishes into the span below, at one per 20 s.
///
/// **Sized against [`MAX_RETAINED_FLASHES`] and not against a window**, which
/// is the whole point of the fixture: two satellites at
/// [`COLD_ROWS_PER_LEVEL`] rows in each of [`COLD_LEVELS`] is
/// `2 × 50 × 14 000 = 1 400 000` rows, **5.6× the ceiling**. The nearest prior
/// arm, `a_cold_poll_holds_one_row_buffer_per_delivery`, polls 30 objects into
/// a 300 s live window for 420 000 rows and never reaches the ceiling at all,
/// so it could not have seen this defect however it was written — the class
/// needs a poll that downloads several times what it is allowed to keep.
///
/// The ratio is not arbitrary: 5.6× is the ratio a HEAVY6 leg's own
/// `GLM: … flashes held over …s of residency in N range(s)` lines imply, at
/// residencies of 1500–4073 s across up to 12 ranges.
const SPAN_GRANULES_PER_SAT: usize = 50;

/// A residency wider than any live pane's window, which is the second disjunct
/// of the posture test in `fetch_glm_flashes` — a loop's span, coalesced. One
/// range, so it is the *total* and not the range count that arms the ceiling,
/// and the arm therefore holds for the single-range shape too.
fn a_spanned_window() -> Residency {
    Residency::over([(as_of() - TimeDelta::seconds(2_000), as_of())])
}

/// **The ceiling bounds what a poll HOLDS, not only what it leaves behind.**
///
/// Red before the streaming install, and by a factor the fixture chooses:
/// `PollAccumulator::entries` held every granule of every satellite until the
/// last download returned, and the trim to [`MAX_RETAINED_FLASHES`] ran once,
/// after all of them. So a poll's transient peak was the *residency's whole
/// download* — 1 400 000 rows, 67 200 000 B here — while the figure the
/// constant advertises is 250 000 rows and 12 000 000 B. The ceiling described
/// the leftovers.
///
/// **The retained set is the same set either way, and that is what makes this
/// safe rather than a trade.** Eviction removes the globally-oldest granule
/// present and a poll's total only ever grows, so the survivors are the newest
/// suffix within the ceiling whether the trim runs once at the end or after
/// every install — and independently of the order `buffer_unordered` completes
/// in, which is the property that matters now that the trim runs inside the
/// stream. The assertions below pin the *set*, by key, not just its size.
///
/// **Floor — `trim_at_the_end`: move the `evict_oldest_over` call out of
/// `GranuleSink::install` and back to a single call after the satellite loop.**
/// The key and row assertions stay green, which is the point; the peak bar
/// reads ~67 MB against a ~34 MB bar and fails.
#[test]
fn a_spanned_poll_holds_no_more_rows_than_it_is_allowed_to_keep() {
    use squallar_overlays::glm::fetch::MAX_RETAINED_FLASHES;

    let east: Vec<(String, Vec<u8>)> = archive_keys("G19", SPAN_GRANULES_PER_SAT)
        .into_iter()
        .map(|(key, start)| (key, a_granule(start, 33.0, COLD_ROWS_PER_LEVEL, 0)))
        .collect();
    let west: Vec<(String, Vec<u8>)> = archive_keys("G18", SPAN_GRANULES_PER_SAT)
        .into_iter()
        .map(|(key, start)| (key, a_granule(start, 41.0, COLD_ROWS_PER_LEVEL, 0)))
        .collect();

    let rows_per_granule = COLD_ROWS_PER_LEVEL * COLD_LEVELS.len();
    let downloaded_rows = rows_per_granule * SPAN_GRANULES_PER_SAT * 2;
    assert!(
        downloaded_rows > 4 * MAX_RETAINED_FLASHES,
        "non-triviality floor: the fixture must download several times the \
         ceiling ({downloaded_rows} rows against {MAX_RETAINED_FLASHES}), or \
         the trim never runs and this arm asks nothing of it",
    );

    let before = squallar_overlays::glm::fetch::gauge::read();
    let (store, outcome, peak, object_gets) =
        peak_of_a_cold_poll(east, west, a_spanned_window(), None);
    let after = squallar_overlays::glm::fetch::gauge::read();

    // ── What survived, by key ──
    //
    // Read off the fixture's own key list, not `granule_contents`: that helper
    // enumerates the *seeded* fixture's `granule_key` shape, and a poll of the
    // archive caches `archive_keys`' shape. Asking it here answered 9 of 17
    // retained granules — a reader's own miss, which is why the row and byte
    // levels below are asserted against the same set rather than beside it.
    let served: Vec<(String, usize)> = ["G19", "G18"]
        .iter()
        .flat_map(|d| {
            archive_keys(d, SPAN_GRANULES_PER_SAT)
                .into_iter()
                .enumerate()
                .map(|(i, (key, _))| (key, i))
        })
        .collect();
    let retained_keys: Vec<&(String, usize)> = store.with_mut(|cache: &mut GlmCache| {
        served
            .iter()
            .filter(|(key, _)| cache.contains_key(key))
            .collect()
    });
    let retained = retained_keys.len();
    let expected_retained = MAX_RETAINED_FLASHES / rows_per_granule;
    assert_eq!(
        retained, expected_retained,
        "the trim must keep every granule that fits under the ceiling and no \
         more: {MAX_RETAINED_FLASHES} rows at {rows_per_granule} a granule is \
         {expected_retained}",
    );
    // Both satellites publish on the same 20 s grid, so the newest `n`
    // granules are the newest `ceil(n/2)` publication slots.
    let newest_slot = expected_retained.div_ceil(2) - 1;
    for (key, slot) in &retained_keys {
        assert!(
            *slot <= newest_slot,
            "eviction is oldest-first, so every survivor must come from the \
             newest {} publications; {key} is #{slot}",
            newest_slot + 1,
        );
    }

    // **Neither satellite is starved.** The satellites are downloaded in
    // sequence and now share one ceiling *inside* the poll rather than meeting
    // it at the end, so a floor raised by GOES-East's granules is a floor
    // GOES-West's have to clear. They publish on the same 20 s grid, so both
    // must be represented in the survivors — a fix that quietly kept only the
    // satellite that downloaded first would pass every byte assertion here.
    for designator in ["G19", "G18"] {
        assert!(
            retained_keys
                .iter()
                .any(|(key, _)| key.contains(&format!("_{designator}_"))),
            "{designator} has no granule in the retained set: the two \
             satellites share one ceiling within a poll, and the one listed \
             second must not be shut out of it",
        );
    }

    // ── What a pane draws ──
    assert_eq!(
        outcome.flashes.len(),
        retained * rows_per_granule,
        "the delivery is the retained set filtered to the residency, and the \
         residency covers every granule the fixture serves",
    );
    assert_eq!(
        store.retained_bytes(),
        retained * rows_per_granule * FLASH_BYTES,
        "the level must agree with the survivors",
    );
    assert!(
        store.retained_bytes() <= MAX_RETAINED_FLASHES * FLASH_BYTES,
        "the ceiling is a ceiling",
    );

    // ── The fires-counter, in both its halves ──
    //
    // A granule the ceiling does not admit is accounted for exactly once, in
    // one of **three** buckets, and they are three different prices: the trim
    // evicted it after it was cached, the retention floor refused it after it
    // was downloaded and parsed, or the early stop never asked for it at all.
    // The third costs nothing anywhere — no body buffer, no row vector, no
    // wire — so the split is worth reporting rather than summing away.
    let trim_granules = after.3 - before.3;
    let trim_rows = after.4 - before.4;
    let trim_sole_rows = after.5 - before.5;
    let refused_granules = after.6 - before.6;
    let refused_rows = after.7 - before.7;
    let stopped_granules = after.14 - before.14;
    let stopped_bytes = after.15 - before.15;
    let fetched_granules = after.16 - before.16;
    let fetched_bytes = after.17 - before.17;
    let planned_bytes = after.18 - before.18;
    let bound_exceeded = after.19 - before.19;
    assert_eq!(
        trim_granules + refused_granules + stopped_granules,
        SPAN_GRANULES_PER_SAT * 2 - retained,
        "every granule the ceiling did not admit must have been trimmed, \
         refused or stopped, and the counters must have seen it — a mechanism \
         that fires zero times is the defect these counters exist to catch",
    );
    // **`fetched_granules` is not asserted against this fixture's key count**,
    // and the reason is the counters' scope rather than the mechanism's: they
    // are process-global and the other arms in this binary poll concurrently.
    // Those arms run with no cap, so they add to `fetched` and to nothing else
    // — 77 fetched against this fixture's 100 planned keys was 30 of a
    // neighbour's on top of 47 of this one's. The three-bucket identity above
    // survives that because a poll with no ceiling trims, refuses and stops
    // nothing.
    assert!(
        fetched_granules + stopped_granules >= SPAN_GRANULES_PER_SAT * 2,
        "every planned key of this fixture is either fetched or stopped, so the \
         two counters together cannot be short of it: {fetched_granules} \
         fetched and {stopped_granules} stopped against {} planned",
        SPAN_GRANULES_PER_SAT * 2,
    );
    // **The early stop's soundness, measured on this fixture rather than
    // argued.** `granule_bound_of` substitutes an upper bound read off the
    // key's `_e` field for the newest flash the granule actually carries; every
    // granule that parses is checked against it. Nonzero here is the bound
    // failing to bound, which is the early stop able to refuse a granule
    // `GranuleSink::install` would have kept.
    assert_eq!(
        bound_exceeded, 0,
        "{bound_exceeded} parsed granules carried a flash later than their \
         key's declared end: the early stop's substitute quantity does not \
         bound the one the floor tests, so it can refuse a granule the floor \
         would have admitted",
    );
    assert!(
        trim_granules > 0 && stopped_granules > 0,
        "the trim and the early stop must both fire on this fixture, or one of \
         them is untested here: trimmed {trim_granules}, stopped \
         {stopped_granules}",
    );
    // **The wire's own count, which is the claim itself.** The counters above
    // say the mechanism fired; this says the GETs it refused were never issued.
    // Per-archive and so uncontaminated by the neighbouring arms, and tied to
    // the process-global `stopped_granules` by the fact that an arm with no
    // ceiling stops nothing.
    assert_eq!(
        object_gets.load(std::sync::atomic::Ordering::Relaxed),
        SPAN_GRANULES_PER_SAT * 2 - stopped_granules,
        "the archive answered a GET for a granule the early stop refused: \
         {stopped_granules} stopped of {} planned",
        SPAN_GRANULES_PER_SAT * 2,
    );
    assert!(
        stopped_bytes > 0 && stopped_bytes < planned_bytes,
        "the stopped bytes must be a real fraction of the planned download, \
         not all of it and not none: {stopped_bytes} of {planned_bytes} B",
    );
    assert_eq!(
        (trim_rows + refused_rows) / rows_per_granule,
        trim_granules + refused_granules,
        "the row counters must agree with the granule counters",
    );
    assert_eq!(
        trim_sole_rows, trim_rows,
        "this poll is the only holder of the granules it downloaded — a cold \
         store, so nothing carried in behind an `Arc` — and every trimmed row \
         must therefore be a row the allocator gets back rather than a \
         refcount",
    );
    println!(
        "counters: trimmed {trim_granules} granules / {trim_rows} rows ({} B, \
         all sole); floor refused {refused_granules} granules / {refused_rows} \
         rows before they were cached ({} B never inserted); the early stop \
         refused {stopped_granules} granules / {stopped_bytes} B of object \
         before a GET, against {fetched_granules} fetched / {fetched_bytes} B, \
         of {planned_bytes} B planned",
        trim_rows * FLASH_BYTES,
        refused_rows * FLASH_BYTES,
    );

    // ── The peak ──
    let ceiling = MAX_RETAINED_FLASHES * FLASH_BYTES;
    let unstreamed = downloaded_rows * FLASH_BYTES;
    println!(
        "spanned poll: {} granules × {rows_per_granule} rows downloaded = \
         {downloaded_rows} rows ({unstreamed} B) for a {MAX_RETAINED_FLASHES}-row \
         ceiling ({ceiling} B); {retained} granules retained; PEAK {peak} B = \
         {:.3}× the ceiling, {:.3}× the download",
        SPAN_GRANULES_PER_SAT * 2,
        peak as f64 / ceiling as f64,
        peak as f64 / unstreamed as f64,
    );

    // One ceiling of cache, one delivery presized from it, and 10 MB for the
    // `GRANULE_FETCH_CONCURRENCY` file buffers in flight (20 × ~281 KB = 5.6 MB),
    // the granule under the parser and the listings. A poll that accumulates
    // its whole download cannot fit under this and does not have to be close.
    let bar = ceiling * 2 + 10_000_000;
    assert!(
        (peak as usize) < bar,
        "a poll allowed to keep {ceiling} B peaked at {peak} B ({:.3}× the \
         ceiling), over the {bar} B that one ceiling plus one delivery plus the \
         file buffers costs. Accumulating every granule until the last download \
         returns puts the whole {unstreamed} B download in this window.",
        peak as f64 / ceiling as f64,
    );
    assert!(
        unstreamed > bar,
        "non-triviality floor: the bar must be one the old shape could not \
         clear ({unstreamed} B download against a {bar} B bar)",
    );
}

// ── The store's half of the race ────────────────────────────────────────────

/// **Two rounds in flight at once both reach the store.**
///
/// The `satellite` control is per pane — `GOES-19`, `GOES-18`, `Both` — so two
/// panes on one instant with two selections are two distinct asks, and
/// `Gui::panes_owed_a_round` makes both due in the same frame. Their key sets
/// are disjoint (the two satellites publish into two buckets) and their
/// residency is identical, so neither round's retention has anything to say
/// about the other's granules: whatever is missing at the end was **lost**, not
/// evicted.
///
/// Red before `GlmStore::round`: both rounds snapshot the store before either
/// writes back, so the one that finishes second publishes a cache built on an
/// empty snapshot and one satellite's whole download — listed, downloaded,
/// parsed and installed — is gone. Which satellite is whichever finished first,
/// so the assertion names both and does not care about the order.
///
/// This is the shape behind the handed-over reading of one pane logging 77,034
/// flashes held and then 357: the delivery is built from the poll's own copy of
/// the cache, and a poll's copy is a snapshot of whatever the last write-back
/// happened to leave.
#[test]
fn two_rounds_in_flight_at_once_both_reach_the_store() {
    const RACING_GRANULES: usize = 4;
    const ROWS: usize = 3;

    let east: Vec<(String, Vec<u8>)> = archive_keys("G19", RACING_GRANULES)
        .into_iter()
        .map(|(key, start)| (key, a_granule(start, 33.0, ROWS, 0)))
        .collect();
    let west: Vec<(String, Vec<u8>)> = archive_keys("G18", RACING_GRANULES)
        .into_iter()
        .map(|(key, start)| (key, a_granule(start, 41.0, ROWS, 0)))
        .collect();
    let sources = s3_two_bucket_archive(
        east.clone(),
        west.clone(),
        None,
        Arc::new(AtomicUsize::new(0)),
    );
    let store = GlmStore::default();
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    let (from_east, from_west) = runtime.block_on(async {
        tokio::join!(
            poll_glm_into_store(
                &store,
                &client,
                &sources,
                &[GlmSatellite::GoesEast],
                &COLD_LEVELS,
                as_of(),
                a_live_window(),
                0,
            ),
            poll_glm_into_store(
                &store,
                &client,
                &sources,
                &[GlmSatellite::GoesWest],
                &COLD_LEVELS,
                as_of(),
                a_live_window(),
                1,
            ),
        )
    });
    from_east.expect("the GOES-East round must succeed");
    from_west.expect("the GOES-West round must succeed");

    let missing: Vec<&String> = store.with_mut(|cache: &mut GlmCache| {
        east.iter()
            .chain(west.iter())
            .map(|(key, _)| key)
            .filter(|key| !cache.contains_key(key))
            .collect()
    });
    assert!(
        missing.is_empty(),
        "{} of {} granules the two rounds downloaded are not in the store. \
         Their residency is identical, so nothing here evicted them — a \
         round's write-back discarded them: {missing:?}",
        missing.len(),
        2 * RACING_GRANULES,
    );

    // **The latency this must not cost, as a fixture.** The round that took the
    // gate is not delayed by it, so the newest granule of the archive is in the
    // store when the FIRST round returns — not when the last one does. Asserted
    // through the level rather than a clock: the first round's own rows are
    // resident, which is only true if it ran to completion without waiting.
    let rows_per_granule = ROWS * COLD_LEVELS.len();
    store.with_mut(|cache: &mut GlmCache| {
        assert_eq!(
            cache.flash_count(),
            2 * RACING_GRANULES * rows_per_granule,
            "both rounds' rows are held, at {rows_per_granule} rows a granule",
        );
    });
}
