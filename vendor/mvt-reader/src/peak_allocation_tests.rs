//! The pin this vendored copy exists for: **one feature's decode peaks at the
//! size of that feature, not at `rings x size`.**
//!
//! It is written here rather than inherited because upstream ships no test
//! target at all — see `VENDORED.md` and the workspace `Cargo.toml`'s member
//! note. `parse_geometry` reserved `geometry_data.len()` — the whole feature's
//! command-integer count — for **every** ring of the feature, and every ring
//! of a polygon is held at once in `linestrings` until the geometry is
//! assembled. So a feature with `R` rings and `N` command integers held
//! `R x N` coordinate slots at its peak while `N` was the whole of what it
//! could ever fill. On wasm32 that is an infallible `Vec` allocation against a
//! 1 GiB module ceiling: `handle_alloc_error` aborts, nothing unwinds, and the
//! shipped web build stops drawing while the page still looks healthy.
//!
//! **The measurement is a real peak**, taken by a counting global allocator
//! rather than derived from the source. It is thread-local so a parallel test
//! in the same binary cannot be read as this one's allocation, and the
//! assertion carries a non-triviality floor: a measurement that saw nothing
//! fails rather than passing vacuously.
//!
//! **Non-vacuity, measured 2026-08-31**: with the two `Vec::new()` lines in
//! `parse_geometry` put back to `Vec::with_capacity(geometry_data.len())`,
//! this test reports a peak of **168,539,425 B** against the 1,676,800 B
//! ceiling below — it fails by 100x. It is not a test that cannot fail. Under
//! the patch the same decode peaks at **1,268,001 B**, 133x smaller, measured
//! the same way; the assertion message prints both the measured peak and the
//! two derived bounds, so a future reader does not have to trust this line.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use crate::vector_tile::{Tile, tile};

thread_local! {
    /// Bytes this thread currently holds through [`Counting`], and the largest
    /// that has ever been. `const` initialised and carrying no destructor on
    /// purpose: a `thread_local!` that allocates on first touch would recurse
    /// through the allocator that is touching it.
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    /// Whether this thread is inside a measured window. Off by default, so the
    /// harness's own threads cost two `Cell` reads and nothing else.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

struct Counting;

impl Counting {
    fn record_alloc(size: usize) {
        if !ARMED.with(Cell::get) {
            return;
        }
        let live = LIVE.with(Cell::get) + size;
        LIVE.with(|c| c.set(live));
        if live > PEAK.with(Cell::get) {
            PEAK.with(|c| c.set(live));
        }
    }

    fn record_dealloc(size: usize) {
        if !ARMED.with(Cell::get) {
            return;
        }
        LIVE.with(|c| c.set(LIVE.with(Cell::get).saturating_sub(size)));
    }
}

// SAFETY: every method forwards to `System`, which is a correct allocator, and
// the bookkeeping around the forward touches only `Cell`s that are `const`
// initialised and never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            Self::record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::record_dealloc(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            Self::record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(ptr, layout, new_size) };
        if !out.is_null() {
            Self::record_dealloc(layout.size());
            Self::record_alloc(new_size);
        }
        out
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Run `body` with this thread's peak measured, and answer that peak in bytes.
fn peak_bytes<R>(body: impl FnOnce() -> R) -> (R, usize) {
    LIVE.with(|c| c.set(0));
    PEAK.with(|c| c.set(0));
    ARMED.with(|c| c.set(true));
    let out = body();
    ARMED.with(|c| c.set(false));
    (out, PEAK.with(Cell::get))
}

/// Rings per feature and points per ring in the synthetic tile below.
///
/// Not arbitrary: a low-zoom `landcover` or `water` layer in a planet basemap
/// is exactly this shape — a handful of features, each a multipolygon of
/// hundreds of small rings — and it is the shape the shipped web build aborted
/// on. Small enough that the fixed decode is a fraction of a millisecond.
const RINGS: usize = 400;
const POINTS_PER_RING: usize = 64;

/// One polygon feature's MVT geometry command array: `RINGS` closed rings of
/// `POINTS_PER_RING` points each, in the encoding the spec defines —
/// `MoveTo(1)`, `LineTo(n - 1)`, `ClosePath`, repeated.
fn many_ringed_geometry() -> Vec<u32> {
    // Zigzag: MVT parameter integers are `(value << 1) ^ (value >> 31)`.
    fn param(v: i32) -> u32 {
        ((v << 1) ^ (v >> 31)) as u32
    }
    fn command(id: u32, count: u32) -> u32 {
        (count << 3) | id
    }

    let mut out = Vec::new();
    for _ in 0..RINGS {
        // MoveTo one point, then LineTo the rest, then close. Every step is a
        // +1/+1 delta, so the ring is a staircase: its shape does not matter,
        // only that it is a ring and that there are many of them.
        out.push(command(1, 1));
        out.push(param(1));
        out.push(param(1));
        out.push(command(2, (POINTS_PER_RING - 1) as u32));
        for _ in 0..(POINTS_PER_RING - 1) {
            out.push(param(1));
            out.push(param(1));
        }
        out.push(command(7, 1));
    }
    out
}

/// A one-layer, one-feature tile carrying [`many_ringed_geometry`].
fn many_ringed_tile() -> Vec<u8> {
    use prost::Message;

    let tile = Tile {
        layers: vec![tile::Layer {
            version: 2,
            name: "landcover".to_owned(),
            features: vec![tile::Feature {
                id: Some(1),
                tags: Vec::new(),
                r#type: Some(tile::GeomType::Polygon as i32),
                geometry: many_ringed_geometry(),
            }],
            keys: Vec::new(),
            values: Vec::new(),
            extent: Some(4096),
        }],
    };
    tile.encode_to_vec()
}

#[test]
fn one_feature_peaks_at_its_own_size_and_not_at_rings_times_it() {
    let bytes = many_ringed_tile();
    let commands = many_ringed_geometry().len();
    let coord = size_of::<geo_types::Coord<f32>>();

    // The two bounds this test sits between, both derived here rather than
    // written down: the quadratic peak the pre-patch expression reserved, and
    // the linear one a decode of this feature can actually need.
    let quadratic = RINGS * commands * coord;
    let linear = commands * coord;

    // Four times the linear bound, and the slack is named rather than round:
    // the first ring still keeps upstream's `with_capacity(commands)`
    // reservation (1x), the rings themselves hold the decoded points and grow
    // by doubling (at most 2x their content, which is under 1x of `commands`),
    // `shoelace_formula` clones each ring twice transiently, and the decoded
    // protobuf tile is live throughout.
    let ceiling = 4 * linear;

    let (features, peak) = peak_bytes(|| {
        let reader = crate::Reader::new(bytes).expect("the synthetic tile decodes");
        reader.get_features(0).expect("its one layer decodes")
    });

    assert_eq!(features.len(), 1, "the fixture is one feature");

    // The non-triviality floor. A measurement that saw nothing — an allocator
    // that was never armed, a decode that was optimised away — would satisfy
    // any upper bound at all, so the lower one is what makes the upper one
    // mean something. One ring's worth of coordinates is the least any real
    // decode of this fixture can touch.
    assert!(
        peak >= POINTS_PER_RING * coord,
        "the counter measured {peak} B, which is less than a single ring: \
         the measurement did not happen"
    );

    assert!(
        peak <= ceiling,
        "decoding one {RINGS}-ring feature peaked at {peak} B. The linear \
         bound is {linear} B ({commands} command integers x {coord} B) and \
         this test allows {ceiling} B. The pre-patch expression reserved the \
         whole feature's command count for every ring, which is {quadratic} B \
         — if the figure above is near that, `parse_geometry`'s per-ring \
         `Vec::new()` has been put back to `Vec::with_capacity`."
    );
}
