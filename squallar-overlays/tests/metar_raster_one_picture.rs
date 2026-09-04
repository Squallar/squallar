//! **A finished station-model raster costs one picture, not two.**
//!
//! `rasterize_metar_stations` ends by handing `Pixmap`'s buffer to its caller.
//! `tiny_skia::Pixmap::take(self) -> Vec<u8>` already yields that buffer
//! **owned**, so the move is free; a `.to_vec()` on the result — which is what
//! this function used to write — allocates a second whole picture and memcpys
//! the first into it for nothing. At the default oversample rung (150 % per
//! side, `OVERLAY_OVERSAMPLE_PERCENTS[0]`) a 1280x815 pane rasterizes at
//! 1920x1222, so the discarded copy was 9,384,960 B — 8.95 MiB per picture,
//! per dispatch.
//!
//! The other six `pixmap.take()` sites in `rasterize.rs` never had it. This is
//! the one that did, so this is the one with a gate.
//!
//! Its own binary with a counting `#[global_allocator]`, the arrangement
//! `hitmap_id_lookup.rs` and `gridded_projection_band.rs` use: the instrument
//! counts real `GlobalAlloc` calls and their requested sizes and knows nothing
//! about rasters, so it compiles and runs against the **unmodified** tree and
//! can disagree with the fix.
//!
//! **Picture-sized allocations are counted, not all allocations.** A station
//! model draws 41 shapes and every path builder behind them allocates; those
//! are tens of bytes and they move with the drawing code, so a total-bytes
//! figure would be a pin on `station_model` rather than on this copy. What is
//! asserted is that nothing the size of the texture is allocated twice.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use squallar_geo::GeoBounds;
use squallar_overlays::metar::types::{MetarOb, WindDir};
use squallar_overlays::render::rasterize::{MetarInput, rasterize_metar_stations};

/// The texture this test rasterizes. Small enough to be cheap, far enough
/// above every incidental allocation in the call that "at least this many
/// bytes" selects picture-sized blocks and nothing else.
const WIDTH: u32 = 512;
const HEIGHT: u32 = 512;
const PICTURE_BYTES: usize = (WIDTH * HEIGHT * 4) as usize;

static PICTURE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static PICTURE_BYTES_SEEN: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Whether **this thread** is inside a measured window. Thread-local and
    /// not a global `AtomicBool` for the reason `hitmap_id_lookup.rs` records:
    /// libtest runs a binary's tests concurrently, and a global flag counts
    /// whatever the other tests allocate at the same moment.
    /// `const`-initialised so that reading it inside the allocator cannot
    /// itself allocate.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the calling thread is measuring. `try_with` rather than `with`:
/// allocations arrive during TLS teardown too, and a destroyed key is not an
/// error here, it is "not measuring".
fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

fn record(size: usize) {
    if size >= PICTURE_BYTES && counting() {
        PICTURE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        PICTURE_BYTES_SEEN.fetch_add(size, Ordering::Relaxed);
    }
}

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    /// `Pixmap::new` zeroes its buffer, so the raster's own allocation arrives
    /// here and not through `alloc`. Counting only `alloc` would read zero
    /// picture allocations for a function that certainly makes one.
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow is a fresh block of `new_size`; a shrink is not a new block.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            record(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The counters are process-global, so two threads measuring at once would
/// share them even though the flag that gates them is not. One window at a
/// time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Runs `body` with the counter on, returning `(value, picture_allocs, bytes)`.
fn picture_allocations_during<T>(body: impl FnOnce() -> T) -> (T, usize, usize) {
    // Bound to a name and taken before the window opens. Never inside an
    // `assert!`: a lock in an assertion's *message* is taken while the
    // condition still holds it, and that hangs instead of reddening.
    let _serialised: MutexGuard<'_, ()> = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    PICTURE_ALLOCS.store(0, Ordering::Relaxed);
    PICTURE_BYTES_SEEN.store(0, Ordering::Relaxed);
    COUNTING.set(true);
    let value = body();
    COUNTING.set(false);
    (
        value,
        PICTURE_ALLOCS.load(Ordering::Relaxed),
        PICTURE_BYTES_SEEN.load(Ordering::Relaxed),
    )
}

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: -100.0,
        max_lon: -90.0,
    }
}

/// One observation carrying enough moments that the station model actually
/// draws: a temperature/dewpoint pair, a barbed wind and a cloud group are
/// what put shapes on the canvas.
fn station(lat: f64, lon: f64) -> MetarOb {
    MetarOb {
        station_id: "KXXX".to_string(),
        name: "Fixture".to_string(),
        lat,
        lon,
        elev_m: Some(300.0),
        temp_c: Some(21.0),
        dewp_c: Some(14.0),
        wind_dir: Some(WindDir::Degrees(230)),
        wind_speed_kt: Some(15),
        wind_gust_kt: Some(25),
        visibility: None,
        altimeter_hpa: Some(1013.2),
        mslp_hpa: Some(1012.4),
        flight_category: None,
        raw_ob: String::new(),
        clouds: Vec::new(),
        wx_string: None,
        obs_time: String::new(),
    }
}

fn input(obs: Vec<MetarOb>) -> MetarInput {
    MetarInput {
        obs,
        zoom: 8.0,
        is_dark: false,
        device_scale: 1.0,
    }
}

/// **The gate.** The raster the caller receives is the buffer the pixmap
/// rasterized into, moved — so exactly one picture-sized block is allocated
/// for the whole call.
#[test]
fn a_station_raster_allocates_one_picture_and_not_two() {
    let input = input(
        (0..24)
            .map(|i| station(31.0 + i as f64 * 0.3, -99.0 + i as f64 * 0.3))
            .collect(),
    );
    let (out, allocs, bytes) =
        picture_allocations_during(|| rasterize_metar_stations(&input, &bounds(), WIDTH, HEIGHT));

    assert_eq!(
        out.rgba.len(),
        PICTURE_BYTES,
        "control: the call did not produce a {WIDTH}x{HEIGHT} raster, so the \
         allocation figures below are not measuring what this test names",
    );
    assert_eq!(
        allocs, 1,
        "`rasterize_metar_stations` allocated {allocs} picture-sized blocks \
         ({bytes} B in total) where the pixmap's own buffer is the only one it \
         needs. Two is the `pixmap.take().to_vec()` that used to end this \
         function: `Pixmap::take(self)` returns an owned `Vec<u8>` already, so \
         the `to_vec` memcpys a whole texture into a second allocation and \
         drops the first. At the default oversample rung that copy is 8.95 MiB \
         per dispatch. Move the buffer instead.",
    );
    assert_eq!(
        bytes, PICTURE_BYTES,
        "the one picture-sized allocation was {bytes} B where the texture is \
         {PICTURE_BYTES} B",
    );
}

/// The counter is not answering "one" because it cannot see the second
/// allocation: an explicit copy of the finished raster, made inside the same
/// window, reads as the second picture. Without this, a gate that had stopped
/// counting would pass exactly as the fixed tree does.
#[test]
fn the_counter_sees_a_second_picture_when_one_is_made() {
    let input = input(vec![station(35.0, -95.0)]);
    let (copy, allocs, bytes) = picture_allocations_during(|| {
        let out = rasterize_metar_stations(&input, &bounds(), WIDTH, HEIGHT);
        out.rgba.to_vec()
    });

    assert_eq!(
        copy.len(),
        PICTURE_BYTES,
        "control: the copy is a whole raster"
    );
    assert_eq!(
        allocs, 2,
        "one raster plus one deliberate copy of it must read as two \
         picture-sized allocations; {allocs} means the instrument is not \
         counting what the gate above credits it with",
    );
    assert_eq!(
        bytes,
        PICTURE_BYTES * 2,
        "two pictures, by size as well as by count"
    );
}
