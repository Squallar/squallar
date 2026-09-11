//! **That the cached whole-volume section payload is actually given back**,
//! watched by the allocator rather than by a field being `None`.
//!
//! `RenderDispatcher::section_input` is one `Arc<RenderInput>` — a whole
//! volume's moments, the largest single object the dispatcher holds — and until
//! the change this suite pins it was written and never taken back. It survived
//! the memory-pressure step (`App::on_pressure` drains the render cache and the
//! extraction cache and left this one field alone) and it survived a new volume
//! landing for its own site. A pane that showed one cross-section held a volume
//! for the life of the process.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `squallar-overlays/tests/gmgsi_staging_release.rs` gives: a field cleared to
//! `None` reads identical to a block handed back, and the difference is the
//! whole point. The figure here is `squallar_alloc::live_bytes()` — bytes
//! granted less bytes returned — across a window that contains the release and
//! the drop of what it handed over, and nothing else.
//!
//! **One `#[test]`, for the reason `squallar-overlays`' sibling suites give**:
//! the counter is process-global and libtest runs a binary's tests on several
//! threads, so a second test here would be allocating inside this one's window.
//! Split, the control below failed under a full `cargo test --workspace` and
//! passed when run alone — which is a race, and a race is the defect.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use nexrad_model::data::{
    ChannelConfiguration, DataMoment, ElevationCut, GateBuffer, MomentData, PulseWidth, Radial,
    RadialStatus, Scan, Sweep, VolumeCoveragePattern, WaveformType,
};
use squallar_app::render_dispatch::{RenderDispatcher, SectionDispatch};
use squallar_egui::pane::{SectionLine, SectionTarget, VolumeStamp};
use squallar_geo::GeoPoint;
use squallar_radar::types::RadarProduct;

#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

const SITE: &str = "KTLX";
const RADAR_LAT: f64 = 35.3333;
const RADAR_LON: f64 = -97.2778;

/// A surveillance-shaped sweep: 720 radials of 1832 gates, which is the radial
/// count and the real gate count of a WSR-88D 250 m surveillance cut. Large
/// enough that the payload cannot be confused with the harness's own churn,
/// small enough that the cut this suite dispatches finishes in well under a
/// second.
const RADIALS: u16 = 720;
const GATES: usize = 1832;

/// **The floor the release has to clear, derived from what the payload is** —
/// one gate code per gate, over every radial — rather than a magic number a
/// shrinking payload could quietly pass under. A `RenderInput` carries the
/// moment as the fixed-point codes it arrived as, so this is the moment plane
/// alone; the per-radial azimuth and elevation tables and the sweep framing are
/// slack on top of it, and are what the measured figure exceeds this by.
const PAYLOAD_FLOOR: u64 = RADIALS as u64 * GATES as u64;

fn at() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
        .expect("a real date")
        .and_hms_opt(18, 30, 0)
        .expect("a real time")
}

fn one_cut() -> ElevationCut {
    ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        0.0,
        false,
        false,
        false,
        false,
        0,
        0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        true,
    )
}

/// One reflectivity sweep at surveillance width.
fn volume() -> Arc<Scan> {
    let radial = |azimuth_number: u16| {
        Radial::new(
            1_760_000_000_000 + i64::from(azimuth_number),
            azimuth_number,
            f32::from(azimuth_number) * (360.0 / f32::from(RADIALS)),
            360.0 / f32::from(RADIALS),
            RadialStatus::ElevationStart,
            1,
            0.5,
            Some(MomentData::from_fixed_point(
                1,
                0,
                250,
                8,
                2.0,
                66.0,
                vec![32; GATES],
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    };
    Arc::new(Scan::new(
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
            vec![one_cut()],
        ),
        vec![Sweep::new(1, (0..RADIALS).map(radial).collect())],
    ))
}

fn target() -> SectionTarget {
    SectionTarget {
        volume: VolumeStamp {
            site: SITE.to_owned(),
            collected: at(),
        },
        product: squallar_radar::fields::spec(RadarProduct::Reflectivity)
            .id
            .clone(),
        line: SectionLine::new(
            GeoPoint {
                lat: 35.0,
                lon: -97.8,
            },
            GeoPoint {
                lat: 35.6,
                lon: -96.9,
            },
        )
        .expect("a valid line"),
        ladder: 1,
    }
}

/// Cache a payload in `d` and wait until the dispatcher's cached field is the
/// payload's **only** owner, so every byte the release has to give back is held
/// by the thing under test and by nothing else when the measurement runs.
///
/// **What makes that true is the owner count, and only the owner count.** The
/// dispatched job is not given a copy of the payload, it is given a co-owner:
/// `nexrad_model::data::GateBuffer` is an `Arc<Vec<u8>>` (see
/// `squallar_radar::payload_share`, which prices what that stopped copying), so
/// the job's `RenderInput` clone and the dispatcher's cached payload own the
/// same gate bytes and only the framing is duplicated. Waiting for the cut to
/// *answer* does not establish that the job has let go of them — the reply is
/// sent from inside the job's own delivery, and the frame that owns the request
/// drops it only once that delivery has returned. A release measured in between
/// hands back the framing alone: 46,524 B against this suite's 1,319,040 B
/// floor, measured on 36 of 38 failures.
///
/// So the wait is on a state. Hold every gate buffer the fixture built, wait
/// for each to fall to this function's handle plus the dispatcher's payload,
/// and drop the handles here, before anything is measured.
///
/// **Every buffer, and not one of them.** The payload tears down front to back,
/// so one buffer going quiet says nothing about the rest: two of those 38
/// failures had the first radial's buffer already released with ~295 KB of the
/// payload still co-owned, and a wait on a single buffer passes both of those
/// through into a green.
///
/// **Kept even once the job lane drops its request before delivering**, which
/// makes waiting on the reply sufficient *by construction*. By construction
/// means by an implementation detail of the worker pool, and a later reorder —
/// or a sink whose delivery holds the request — puts this suite back to
/// measuring a transient with nothing anywhere saying so. A wait on a state
/// survives that; a wait on the reply does not. It is not redundant, it is the
/// reason the suite cannot quietly stop measuring what it claims to.
fn cache_a_payload(d: &mut RenderDispatcher) {
    let (tx, rx) = mpsc::channel();
    let scan = volume();
    // Counted against `RADIALS` because `any()` over an empty set is false for
    // free: a wait that cannot wait is the exact failure this function is being
    // repaired for, and it would read as a pass.
    let probes: Vec<GateBuffer> = scan
        .sweeps()
        .iter()
        .flat_map(Sweep::radials)
        .filter_map(Radial::reflectivity)
        .map(|moment| moment.gate_buffer().clone())
        .collect();
    assert_eq!(
        probes.len(),
        RADIALS as usize,
        "premise: every fixture radial carries the reflectivity this waits on",
    );
    let dispatched = d.spawn_section_render(
        0,
        &target(),
        move || {
            squallar_radar::render_input::RenderInput::extract_volume(
                &scan,
                RadarProduct::Reflectivity,
                RADAR_LAT,
                RADAR_LON,
            )
        },
        tx,
        None,
    );
    assert_eq!(
        dispatched,
        SectionDispatch::Dispatched,
        "premise: the fixture volume carries a reflectivity cut to extract",
    );
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the dispatched cut answers");

    // The fixture volume went with the extraction closure `spawn_section_render`
    // consumed, so what is left owning each buffer is this function's handle,
    // the dispatcher's cached payload, and the job's clone until the worker's
    // frame has dropped its request: two apiece once it has. The deadline is a
    // hang guard and never the gate — what is asserted is the count.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while probes.iter().any(|gates| gates.owners() > 2) {
        assert!(
            std::time::Instant::now() < deadline,
            "the dispatched job never gave the payload back: {} of {} gate \
             buffers are still owned by more than this handle and the \
             dispatcher's cached payload",
            probes.iter().filter(|gates| gates.owners() > 2).count(),
            probes.len(),
        );
        std::thread::yield_now();
    }
}

fn live() -> u64 {
    squallar_alloc::live_bytes().expect("this binary installed the counter")
}

/// **Every release path this change adds, and the control that says the
/// instrument can see one.**
///
/// Floors, in order:
/// * revert `clear_extract_cache` to `self.extract_cache.drain()...` alone —
///   `released` reads 0 instead of 1 and the fall reads a few KiB;
/// * delete the `section_input` arm from `reset_panes_for_site` — that fall
///   reads a few KiB too (measured: 24 B).
#[test]
fn the_section_payload_is_released_and_the_counter_can_see_it() {
    // ── The pressure step hands the payload back to the allocator ─────────
    let mut d = RenderDispatcher::new();
    cache_a_payload(&mut d);

    let held = live();
    let released = d.clear_extract_cache();
    assert_eq!(
        released.len(),
        1,
        "the pressure lever must hand the cached section payload back owned, \
         the way it hands the extraction cache's entries back, so the caller \
         can route it to the deferred-drop path instead of freeing a volume on \
         the frame thread",
    );
    drop(released);
    let after = live();

    assert!(
        held.saturating_sub(after) >= PAYLOAD_FLOOR,
        "the pressure step freed {} B where a volume payload is at least {PAYLOAD_FLOOR} B \
         ({held} then {after}). A field cleared to `None` with the bytes still \
         referenced elsewhere would print exactly this.",
        held.saturating_sub(after),
    );

    // Control: pulling the lever again finds nothing and costs nothing.
    let quiet = live();
    assert!(
        d.clear_extract_cache().is_empty(),
        "a second pull has nothing to find",
    );
    assert!(
        live() <= quiet + (1 << 20),
        "and it must not take anything to report that",
    );

    // ── A new volume for the site takes that site's payload with it ───────
    let mut d = RenderDispatcher::new();
    cache_a_payload(&mut d);
    let gui = squallar_egui::Gui::new();

    // A different site's volume must leave it alone.
    let held = live();
    d.reset_panes_for_site("KOUN", &gui);
    let untouched = live();
    assert!(
        held.saturating_sub(untouched) < PAYLOAD_FLOOR,
        "another site's volume landing released this site's payload ({held} \
         then {untouched}); the payload would be re-extracted on the next frame \
         for nothing",
    );

    d.reset_panes_for_site(SITE, &gui);
    let after = live();
    assert!(
        untouched.saturating_sub(after) >= PAYLOAD_FLOOR,
        "this site's own volume landing left the payload resident: freed {} B \
         where a volume payload is at least {PAYLOAD_FLOOR} B ({untouched} then \
         {after})",
        untouched.saturating_sub(after),
    );

    // ── Non-triviality: the counter can see a block of this class go ──────
    let before = live();
    let block: Vec<u8> = vec![0; PAYLOAD_FLOOR as usize];
    let held = live();
    assert!(
        held >= before + PAYLOAD_FLOOR,
        "the counter did not see a {PAYLOAD_FLOOR} B grant ({before} then {held})",
    );
    drop(block);
    let after = live();
    assert!(
        held.saturating_sub(after) >= PAYLOAD_FLOOR,
        "the counter did not see the free ({held} then {after})",
    );
}
