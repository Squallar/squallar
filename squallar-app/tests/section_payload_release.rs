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

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
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

/// Cache a payload in `d` and wait for the cut it dispatched to answer, so the
/// job's own clone of the payload is gone before anything is measured.
fn cache_a_payload(d: &mut RenderDispatcher) {
    let (tx, rx) = mpsc::channel();
    let scan = volume();
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
}

fn live() -> u64 {
    squallar_alloc::live_bytes().expect("this binary installed the counter")
}

/// **The pressure step hands the section payload back to the allocator.**
///
/// Floor — revert `clear_extract_cache` to `self.extract_cache.drain()...`
/// alone: `released` reads 0 instead of 1 and the fall below reads a few KiB
/// instead of megabytes.
#[test]
fn the_pressure_step_gives_the_section_payload_back_to_the_allocator() {
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
}

/// **A new volume for the site takes the site's payload with it**, on the same
/// terms it already takes every cached raster for that site.
///
/// Floor — delete the `section_input` arm from `reset_panes_for_site`: the fall
/// below reads a few KiB.
#[test]
fn a_new_volume_for_the_site_releases_that_sites_section_payload() {
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
}

/// **Non-triviality: the instrument can see a free of the class being measured.**
///
/// Without this the two figures above could both be reporting an allocator that
/// never moves.
#[test]
fn the_counter_can_see_a_payload_sized_block_go() {
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
