//! A cut never inherits a pixel from the cut before it.
//!
//! `xsect`'s three planes are carried from one section to the next — see
//! `POOLED_PLANES` for why — and the whole safety of that rests on what
//! `SectionPlanes::fit` hands out: exactly the raster's length, seeded exactly
//! as the three `vec!`s it replaced were. A section handed a used set must not
//! be able to tell it from a fresh allocation.
//!
//! Break it and the failure is not a crash. It is a cut that finds no data
//! showing the *previous* cut's storm — and it would look entirely plausible,
//! because a section is a picture of interpolated air with no sharp features to
//! say which volume it came from. That is the shape of bug that survives a test
//! asserting only that the output has the right size, and it is the shape this
//! campaign has already shipped twice.
//!
//! # Why this is an integration test
//!
//! The claim is about a **process-wide** value. `POOLED_PLANES` holds one set of
//! planes, and which cut receives it depends on which cuts are running: inside
//! the library's own test binary, other tests render sections on other threads
//! and any of them can take the slot in between, so a dense cut's buffer is not
//! reliably the one the blank cut after it receives — the test would pass
//! without ever exercising the case it is named for. An integration test file is
//! its own process, and this is the only test in it, so the cuts below are the
//! only cuts there are and the set handed to each one is the set the previous
//! one gave back.
//!
//! Adding a second `#[test]` to this file would silently undo that: libtest
//! would run the two in parallel and put the interleaving back.
//!
//! # What this file does *not* check
//!
//! That the planes are reused at all. Every assertion below passes just as well
//! against a renderer that allocates afresh every time — correctly, because the
//! property being pinned is "a cut's bytes do not depend on what was cut before
//! it", and a fresh allocation satisfies it trivially. So this file cannot fail
//! if the pool is ever removed or quietly bypassed; it only fails if the pool is
//! kept and the re-seed is not.
//!
//! It also cannot fail *today*, for a second reason, and that one is worth
//! saying out loud: `render_with_sampler`'s raster loop writes every pixel of
//! all three planes, so nothing a pooled set arrives holding is observable
//! through this door at all. The invariant that makes that true is pinned where
//! it is observable — `xsect::tests::a_fitted_plane_set_is_what_three_fresh_vecs_would_be`
//! poisons a set and checks what `fit` gives back. This file is the end-to-end
//! statement of the property, and it is what starts failing the moment that loop
//! stops covering the whole raster.
//!
//! Reuse itself is a *performance* claim and is measured out of tree — minor
//! faults per cut and allocation counts per cut, both quoted in
//! `POOLED_PLANES`'s documentation — because an instrument that could assert it
//! from in here is a harness, and harnesses do not ship on main.

use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};
use rustdar_radar::types::RadarProduct;
use rustdar_radar::xsect::{CrossSection, SectionRequest, render_section};

const SITE: (f64, f64) = (35.3333, -97.2778);
const REFL_SCALE: f32 = 2.0;
const REFL_OFFSET: f32 = 66.0;
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;
const RADIALS: usize = 360;

/// An endpoint, named in **degrees** from the site rather than in kilometres.
///
/// Degrees deliberately: this file needs three geometries that are unlike each
/// other, not three geometries of a stated length, and converting would mean
/// naming a kilometres-per-degree figure here — a second opinion about the
/// planet, which `tests/geodesy_one_definition.rs` exists to refuse and did
/// refuse the first version of this line. `render_section` does the real
/// great-circle work on `types::EARTH_RADIUS_KM` and reports the length it
/// found; nothing here needs to predict it.
fn at(north_deg: f64, east_deg: f64) -> (f64, f64) {
    (SITE.0 + north_deg, SITE.1 + east_deg)
}

/// A field that varies with both range and azimuth, so a cut that read one gate
/// and smeared it still paints something and it is the wrong something.
fn dbz(azimuth_deg: f64, slant_km: f64) -> f64 {
    35.0 + (slant_km / 17.0).sin() * 22.0 + (azimuth_deg / 41.0).cos() * 12.0
}

fn sweep(elevation_number: u8, elevation_deg: f32, gates: usize) -> Sweep {
    let spacing = 360.0 / RADIALS as f64;
    let radials = (0..RADIALS)
        .map(|i| {
            let az = i as f64 * spacing;
            let bytes: Vec<u8> = (0..gates)
                .map(|gate| {
                    let slant =
                        f64::from(FIRST_GATE_M) / 1000.0 + gate as f64 * f64::from(GATE_M) / 1000.0;
                    let raw = dbz(az, slant) * f64::from(REFL_SCALE) + f64::from(REFL_OFFSET);
                    (raw.round() as i64).clamp(2, 255) as u8
                })
                .collect();
            Radial::new(
                0,
                i as u16,
                az as f32,
                spacing as f32,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                Some(MomentData::from_fixed_point(
                    bytes.len() as u16,
                    FIRST_GATE_M,
                    GATE_M,
                    8,
                    REFL_SCALE,
                    REFL_OFFSET,
                    bytes,
                )),
                None,
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

fn cut(angle_deg: f64) -> ElevationCut {
    ElevationCut::new(
        angle_deg,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        20,
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
        false,
    )
}

/// Three cuts, upper ones range-truncated as real ones are.
fn scan() -> Scan {
    let ladder = [(1u8, 0.53f32, 900usize), (2, 4.02, 600), (3, 9.94, 400)];
    let vcp = VolumeCoveragePattern::new(
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
        ladder.iter().map(|&(_, a, _)| cut(f64::from(a))).collect(),
    );
    let sweeps = ladder
        .iter()
        .map(|&(n, deg, gates)| sweep(n, deg, gates))
        .collect();
    Scan::new(vcp, sweeps)
}

/// The three planes as one comparable value. `values` goes in as bit patterns
/// because `f32::NAN != f32::NAN` and almost every pixel of a section is one —
/// under `==` the comparison would be vacuously false rather than vacuously
/// true, which is worse.
struct Planes {
    image: Vec<u8>,
    status: Vec<u8>,
    values: Vec<u32>,
}

fn planes(section: &CrossSection) -> Planes {
    Planes {
        image: section.image().to_vec(),
        status: section.status().to_vec(),
        values: section.values().iter().map(|v| v.to_bits()).collect(),
    }
}

fn first_diff<T: PartialEq>(got: &[T], want: &[T]) -> Option<usize> {
    if got.len() != want.len() {
        return Some(got.len().min(want.len()));
    }
    got.iter().zip(want).position(|(a, b)| a != b)
}

/// `got == want`, reported as the plane and index that first disagree.
///
/// Not `assert_eq!` on the planes themselves: they are 8 MiB each and a failed
/// `assert_eq!` prints both sides in full. The position is also the more useful
/// fact — which plane drifted, and whether it drifted everywhere or only in a
/// tail, are different defects.
#[track_caller]
fn same(got: &Planes, want: &Planes, what: &str) {
    assert_eq!(first_diff(&got.image, &want.image), None, "{what}: image");
    assert_eq!(
        first_diff(&got.status, &want.status),
        None,
        "{what}: status"
    );
    assert_eq!(
        first_diff(&got.values, &want.values),
        None,
        "{what}: values"
    );
}

/// How many pixels this cut painted anything at all — the anti-vacuity measure.
fn painted(section: &CrossSection) -> usize {
    section
        .image()
        .chunks_exact(4)
        .filter(|px| px[3] != 0)
        .count()
}

/// A cut through the volume, along a line the caller names.
fn request(start: (f64, f64), end: (f64, f64), top_km_msl: Option<f64>) -> SectionRequest {
    SectionRequest {
        start,
        end,
        top_km_msl,
        product: RadarProduct::Reflectivity,
    }
}

#[test]
fn a_cut_never_inherits_a_pixel_from_the_one_before_it() {
    let scan = scan();
    let render = |req: &SectionRequest| {
        render_section(&scan, req, SITE.0, SITE.1, None, None).expect("the cut renders")
    };

    // Three deliberately unlike geometries. `blank` sits six degrees off the
    // site — several times any volume's reach — so every column of it is past
    // the data and it paints nothing at all; `dense` crosses the site and is
    // full of echo; `short` is a fraction of a degree under a 6 km axis top, a
    // completely different mapping of both axes. The assertions below check
    // that the first two really are the extremes they are claimed to be, so
    // these three lines are not load-bearing on their own.
    let blank = request(at(6.0, 6.0), at(6.6, 6.6), None);
    let dense = request(at(-0.9, -0.9), at(0.9, 0.9), None);
    let short = request(at(-0.14, 0.0), at(0.14, 0.0), Some(6.0));

    // The pool is empty here — this is the only cut in this process that is
    // certain to be drawn into a fresh allocation, so it is the reference every
    // later blank cut is held against.
    let blank_first = planes(&render(&blank));

    let dense_first = planes(&render(&dense));
    let dense_painted = {
        let s = render(&dense);
        painted(&s)
    };

    // The two predecessors are as unlike as this volume can make them. Without
    // this the assertions below could hold because every cut here paints the
    // same picture, which would make the whole file vacuous in a way no
    // assertion about equality can reveal.
    assert!(
        dense_painted > 100_000,
        "the dense cut painted only {dense_painted} pixels, so it is not a \
         predecessor that would show up in a plane it left behind",
    );
    assert_eq!(
        painted(&render(&blank)),
        0,
        "the blank cut painted something, so it is not the empty predecessor \
         this file needs",
    );
    assert!(
        first_diff(&dense_first.image, &blank_first.image).is_some(),
        "the dense and blank cuts drew the same image",
    );

    // The claim, three times over: the same request after a maximally different
    // predecessor is the same bytes.
    same(
        &planes(&render(&blank)),
        &blank_first,
        "a blank cut taken after a dense one is not the blank cut taken first",
    );
    let short_after_blank = planes(&render(&short));
    same(
        &planes(&render(&dense)),
        &dense_first,
        "a dense cut taken after a short one is not the dense cut taken first",
    );
    same(
        &planes(&render(&short)),
        &short_after_blank,
        "a short, low-axis cut taken after a long, full-height one is not the \
         same cut taken after a blank",
    );

    // Two sections alive at once: the second finds the slot empty and allocates,
    // which is the arm a single-slot pool has and a free list does not. Both
    // must still be their own bytes, and the one dropped second must be the one
    // that goes back — so the cut after them is checked too.
    let held_dense = render(&dense);
    let held_blank = render(&blank);
    same(
        &planes(&held_dense),
        &dense_first,
        "the held dense cut drifted",
    );
    same(
        &planes(&held_blank),
        &blank_first,
        "the held blank cut drifted",
    );
    drop(held_blank);
    drop(held_dense);
    same(
        &planes(&render(&blank)),
        &blank_first,
        "a cut after two overlapping ones is not itself",
    );

    // A section that arrived over a wire rather than out of the renderer feeds
    // the pool by the same door — `from_parts` is the constructor a worker's
    // reply goes through, and its planes are the next cut's. The round trip
    // has to be exact for that to be safe, `NaN` payloads included.
    let wire = CrossSection::from_bytes(&render(&dense).to_bytes())
        .expect("a section round-trips through its own codec");
    same(
        &planes(&wire),
        &dense_first,
        "a section decoded from its own bytes is not the section encoded",
    );
    drop(wire);
    same(
        &planes(&render(&blank)),
        &blank_first,
        "a cut drawn into a wire-built section's planes is not itself",
    );
}
