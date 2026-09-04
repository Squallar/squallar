//! **What a velocity derivation holds while it runs, in tilts.**
//!
//! `derive::prepare` walks every velocity tilt of the volume for SRV and NROT.
//! A tilt decodes to an `f64` grid — a super-res cut is 720 × 1192 × 8 B, 6.55
//! MiB, plus a status byte per gate — and the walk used to `collect` all of
//! them before the first was used, so a volume of N velocity tilts held N grids
//! for the length of the derivation on top of the tilt being dealiased. The
//! wind fit the walk seeds from is two passes over every tilt, and it already
//! has a streaming spelling (`velocity::volume_wind_profile`) that decodes a
//! tilt, offers it, and drops it. The walk now streams the same way: what a
//! derivation holds is the tilt in hand, not the volume.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `squallar-overlays/tests/hitmap_id_lookup.rs` sets out: the instrument
//! counts real `GlobalAlloc` calls and knows nothing about tilts, so it
//! compiles and runs against the **unmodified** tree and can disagree with the
//! fix. What it tracks is **live bytes** — allocations minus frees since the
//! window opened — and the high-water mark of that, which is the residency
//! figure and not a traffic figure.
//!
//! The figure is **process-global, not per-thread**, on purpose: the NROT
//! stencil runs its rows on the rayon pool, and a thread-local count would
//! miss what the pool's threads allocate. That is safe here because this
//! binary holds nothing but these tests and they take one window at a time.
//!
//! The pin is a **difference on one volume**, not an absolute and not growth
//! against the tilt count. Two things besides the retained tilts grow with N
//! and neither is a defect: the derived scan (N sweeps by definition, read
//! off the window as its residual) and the wind fit's sample store — a
//! trimmed least squares that revisits every kept point against its first
//! fit, so it holds points, thinned to a per-layer cap that bounds it at
//! 15.7 MB on any volume but leaves it growing on a fixture this small. The
//! held walk and the streamed walk pay both identically on the same volume,
//! so their difference is the retained tilts and nothing else.
//!
//! The second pin is inside one tilt: what the dealiaser itself holds beside
//! the grid it is handed. That one is an absolute, in grids, because the
//! transients are what it is about.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};
use squallar_radar::derive::{Prepared, prepare};
use squallar_radar::nyquist::{DeclaredNyquist, Volume};
use squallar_radar::srv::MotionInputs;
use squallar_radar::types::RadarProduct;

/// Whether a measured window is open. Global, not thread-local — see the
/// module docs for why.
static COUNTING: AtomicBool = AtomicBool::new(false);
/// Bytes allocated minus bytes freed since the window opened. Signed: a block
/// allocated before the window and freed inside it subtracts, and that is the
/// honest reading — it is bytes the body gave back.
static LIVE: AtomicIsize = AtomicIsize::new(0);
/// The high-water mark of [`LIVE`] over the window.
static PEAK: AtomicIsize = AtomicIsize::new(0);

fn note_alloc(size: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        let live = LIVE.fetch_add(size as isize, Ordering::Relaxed) + size as isize;
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
}

fn note_free(size: usize) {
    if COUNTING.load(Ordering::Relaxed) {
        LIVE.fetch_sub(size as isize, Ordering::Relaxed);
    }
}

struct ResidencyAllocator;

unsafe impl GlobalAlloc for ResidencyAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note_free(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    /// Freed then allocated, not the other way round, so a grow is never read
    /// as holding both sizes at once.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_free(layout.size());
        note_alloc(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: ResidencyAllocator = ResidencyAllocator;

/// The counters are process-global, so a test running beside an open window
/// is read by it — and not only its window: a fixture built or a warm-up run
/// OUTSIDE its own window still allocates and frees inside the other's, and
/// the frees of blocks the window never saw allocated pull the level down.
/// The first reading taken with only the windows serialised was 0.3 grids of
/// growth for a walk this file's own control read at 19.8. So the lock is
/// held for a test's WHOLE body, taken as its first line.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The guard every test in this binary takes before it allocates anything.
/// Bound to a name and never inside an `assert!`: a lock in an assertion's
/// *message* is taken while the condition still holds it, and that hangs
/// instead of reddening.
fn serialised() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What one window read.
#[derive(Clone, Copy, Debug)]
struct Window {
    /// The high-water mark of live bytes over the window.
    peak: usize,
    /// Live bytes when the window closed — what the body RETURNED, since
    /// everything else it allocated is freed by then. For a derivation that is
    /// the derived scan, which is N sweeps by definition and grows with N.
    residual: usize,
}

impl Window {
    /// The peak above what the body handed back: its working set.
    fn working_set(self) -> usize {
        self.peak.saturating_sub(self.residual)
    }
}

/// Runs `body` with the counter on. The caller holds [`serialised`] and the
/// value is handed back alive so the residual reading is of it, not of its
/// absence.
fn peak_during<T>(body: impl FnOnce() -> T) -> (T, Window) {
    LIVE.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let value = body();
    COUNTING.store(false, Ordering::Relaxed);
    let peak = usize::try_from(PEAK.load(Ordering::Relaxed)).unwrap_or(0);
    let residual = usize::try_from(LIVE.load(Ordering::Relaxed)).unwrap_or(0);
    (value, Window { peak, residual })
}

// ── The fixture: a VAD volume of N velocity tilts ───────────────────────────

const N_RADIALS: usize = 360;
const N_GATES: usize = 240;
/// One tilt's `f64` grid — the unit every figure below is quoted in.
const GRID_BYTES: usize = N_RADIALS * N_GATES * 8;

fn vcp() -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        212,
        0,
        0.5,
        PulseWidth::Short,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    )
}

/// One velocity sweep carrying an exact VAD signature of `(u, v)`, the shape
/// `velocity/tests.rs` fits its digest on — so the wind fit succeeds and the
/// walk's second pass (dealias under the seed, refit) runs as it does on a
/// real volume.
fn vad_sweep(elevation_number: u8, elevation_deg: f32, az0: f64, (u, v): (f64, f64)) -> Sweep {
    let cos_el = f64::from(elevation_deg).to_radians().cos();
    let spacing = 360.0 / N_RADIALS as f32;
    let radials = (0..N_RADIALS)
        .map(|i| {
            let az = (az0 + i as f64 * f64::from(spacing)).rem_euclid(360.0);
            let (s, c) = az.to_radians().sin_cos();
            let vr = (u * s + v * c) * cos_el;
            let byte = ((vr * 2.0 + 129.0).round() as i64).clamp(2, 255) as u8;
            Radial::new(
                0,
                i as u16,
                az as f32,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                None,
                Some(MomentData::from_fixed_point(
                    N_GATES as u16,
                    2125,
                    250,
                    8,
                    2.0,
                    129.0,
                    vec![byte; N_GATES],
                )),
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Sweep::new(elevation_number, radials)
}

/// `n` velocity tilts of one wind, each at its own elevation and opening
/// azimuth. Unclocked (every radial's timestamp is 0), so `prepare` never
/// memoises it and every call derives.
fn vad_volume(n: u8) -> Scan {
    Scan::new(
        vcp(),
        (1..=n)
            .map(|k| {
                let el = 0.5 + 0.9 * f64::from(k - 1);
                let az0 = (97.3 * f64::from(k)).rem_euclid(360.0);
                vad_sweep(k, el as f32, az0, (12.0, -5.0))
            })
            .collect(),
    )
}

/// NROT for the whole volume through the production entry point.
fn derive_nrot(scan: &Scan) -> Prepared<'_> {
    static NOTHING_DECLARED: DeclaredNyquist = DeclaredNyquist::empty();
    let prepared = prepare(
        Volume::new(scan, &NOTHING_DECLARED),
        RadarProduct::NormalizedRotation,
        MotionInputs::default(),
        0.0,
        0.0,
    )
    .expect("a volume of velocity tilts derives NROT");
    assert!(
        matches!(prepared, Prepared::Derived(_)),
        "NROT is derived, never native"
    );
    prepared
}

/// What the walk holds at its peak for a volume of `n` tilts, after one
/// unmeasured run to bring the rayon pool up — its threads' bookkeeping is
/// allocated on first use and belongs to no tilt count.
fn nrot_window(n: u8) -> Window {
    let scan = vad_volume(n);
    assert_eq!(
        derive_nrot(&scan).scan().sweeps().len(),
        usize::from(n),
        "every tilt derives"
    );
    let (derived, window) = peak_during(|| derive_nrot(&scan));
    assert_eq!(derived.scan().sweeps().len(), usize::from(n));
    eprintln!(
        "prepare NROT, {n} tilts: peak {} B = {:.2} grids, derived scan {} B = {:.2} grids, \
         working set {:.2} grids",
        window.peak,
        window.peak as f64 / GRID_BYTES as f64,
        window.residual,
        window.residual as f64 / GRID_BYTES as f64,
        window.working_set() as f64 / GRID_BYTES as f64,
    );
    drop(derived);
    window
}

/// The walk as it used to be spelled — every tilt decoded and held, the fit
/// lent the held run, the stencil walked over it — for the instrument to read
/// beside the streamed one. This is the regression the pin exists to catch,
/// spelled out so the pin is shown to distinguish the two.
fn held_walk_window(n: u8) -> Window {
    use squallar_radar::nrot::compute_nrot_grid_with_profile;
    use squallar_radar::velocity::{VelocityTilt, tilts, wind_profile_of};
    let scan = vad_volume(n);
    let walk = || {
        let held: Vec<VelocityTilt<'_>> = tilts(&scan).collect();
        let profile = wind_profile_of(held.iter());
        held.iter()
            .map(|tilt| {
                compute_nrot_grid_with_profile(
                    &tilt.grid.sweep(None),
                    tilt.elevation_deg,
                    profile.as_ref(),
                )
                .len()
            })
            .sum::<usize>()
    };
    walk();
    let (_, window) = peak_during(walk);
    eprintln!(
        "held walk, {n} tilts: peak {} B = {:.2} grids, working set {:.2} grids",
        window.peak,
        window.peak as f64 / GRID_BYTES as f64,
        window.working_set() as f64 / GRID_BYTES as f64,
    );
    window
}

/// Three tilts against twelve: the control varies the count; the pin uses
/// the larger one.
const FEW: u8 = 3;
const MANY: u8 = 12;

/// Streaming sheds every tilt but the one in hand: on one twelve-tilt volume
/// the held walk's working set stands above the streamed walk's by the
/// eleven grids it kept and the streamed walk did not.
///
/// Peaks, not working sets: the held walk here builds no derived scan, so
/// subtracting the streamed walk's would count the product as a saving. The
/// streamed peak carries the sweeps derived so far and the held one none,
/// which leans the comparison against the pin. A grid and an eighth (values
/// plus status) per shed tilt is what the held walk pays; the pin asks for a
/// grid, and the eighth is the slack. Measured 2026-09-04 on this fixture:
/// held 31.26 grids, streamed 18.56, a difference of 12.7 for eleven shed
/// tilts.
#[test]
fn streaming_the_walk_sheds_every_tilt_but_the_one_in_hand() {
    let _lock = serialised();
    let held = held_walk_window(MANY);
    let streamed = nrot_window(MANY);
    let shed = held.peak.saturating_sub(streamed.peak);
    let tilts_shed = usize::from(MANY - 1);
    assert!(
        shed >= tilts_shed * GRID_BYTES,
        "the streamed walk's peak is {shed} B ({:.2} grids of {GRID_BYTES} B) \
         under the held walk's on the same {MANY}-tilt volume, and {tilts_shed} \
         shed tilts are {tilts_shed} grids: the walk is holding the volume's \
         tilts, not the tilt in hand — held {held:?}, streamed {streamed:?}",
        shed as f64 / GRID_BYTES as f64,
    );
}

/// The control: the same instrument reads the held walk as growing by a grid
/// and more per tilt. Without this, a pin that stayed green could be an
/// instrument that reads zero — and a difference of two readings that are
/// both zero is zero, which the pin above would refuse, but only this shows
/// which of the two readings is the one that moved.
#[test]
fn the_instrument_reads_the_held_walk_as_a_grid_per_tilt() {
    let _lock = serialised();
    let few = held_walk_window(FEW);
    let many = held_walk_window(MANY);
    let growth = many.working_set().saturating_sub(few.working_set());
    let extra = usize::from(MANY - FEW);
    assert!(
        growth >= extra * GRID_BYTES,
        "the held walk grew by {growth} B for {extra} more tilts, under a grid \
         ({GRID_BYTES} B) each: the instrument cannot see a retained tilt, so \
         the streaming pin beside this proves nothing — few {few:?}, many {many:?}",
    );
}

/// **Inside one tilt.** The dealiaser used to take two grid-sized copies of
/// the field it was handed — the field as reported, then the same field with
/// the refused gates punched to NaN — and hold both for the whole pass beside
/// the grid itself, which nothing wrote to until the end. The punched field
/// is now the grid in place and the refused gates' reported values are kept
/// aside sparsely, so the pass holds two grids fewer.
///
/// Measured 2026-09-04 on this fixture, the NROT pipeline on one tilt peaked
/// at 5.68 grids with the copies and 4.17 without — less than the two grids
/// removed, because the peak moved: without them the dealiaser is no longer
/// the tilt's peak phase, the stencil is. The pin sits at five, 0.8 above the
/// latter and 0.7 under the former. The refused-gate store is zero here (a
/// clean VAD field refuses nothing) and a grid in the worst case of every
/// gate refused — still one grid under the old shape's two.
#[test]
fn one_tilts_dealias_holds_no_copy_of_the_reported_field() {
    use squallar_radar::nrot::compute_nrot_grid_with_profile;
    use squallar_radar::velocity::tilts;
    let _lock = serialised();
    let scan = vad_volume(1);
    let tilt = tilts(&scan)
        .next()
        .expect("the fixture's one velocity tilt");
    let run =
        || compute_nrot_grid_with_profile(&tilt.grid.sweep(None), tilt.elevation_deg, None).len();
    run();
    let (_, window) = peak_during(run);
    eprintln!(
        "one tilt NROT: peak {} B = {:.2} grids",
        window.peak,
        window.peak as f64 / GRID_BYTES as f64,
    );
    const CEILING_GRIDS: usize = 5;
    assert!(
        window.peak <= CEILING_GRIDS * GRID_BYTES,
        "one tilt's NROT pipeline peaked at {} B ({:.2} grids of {GRID_BYTES} B), \
         over the {CEILING_GRIDS}-grid ceiling: the dealiaser is copying the field \
         it was handed again",
        window.peak,
        window.peak as f64 / GRID_BYTES as f64,
    );
}
