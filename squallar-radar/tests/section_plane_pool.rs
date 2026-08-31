//! A cut never inherits a pixel from the cut before it.

use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};
use squallar_radar::types::RadarProduct;
use squallar_radar::xsect::{CrossSection, SectionRequest, render_section};

const SITE: (f64, f64) = (35.3333, -97.2778);
const REFL_SCALE: f32 = 2.0;
const REFL_OFFSET: f32 = 66.0;
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;
const RADIALS: usize = 360;

/// An endpoint, named in **degrees** from the site rather than in kilometres.
fn at(north_deg: f64, east_deg: f64) -> (f64, f64) {
    (SITE.0 + north_deg, SITE.1 + east_deg)
}

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

/// The three planes as one comparable value.
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
        .as_chunks::<4>()
        .0
        .iter()
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
        render_section(
            &scan,
            req,
            SITE.0,
            SITE.1,
            squallar_radar::srv::MotionInputs::default(),
        )
        .expect("the cut renders")
    };

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

    // A section that arrived over a wire rather than out of the renderer feeds the pool
    // by the same door — `from_parts` is the constructor a worker's reply goes through,
    // and its planes are the next cut's.
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
