//! What a raster side under the data's own floor costs, counted in gates.
//!
//! The widest honest sweep this display draws is a WSR-88D surveillance cut:
//! 1832 gates of 0.25 km on 720 radials of 0.5°. `types::data_limited_side_px`
//! asks `TEXELS_PER_SAMPLE` texels per gate for it — 7362 px over ±460 km —
//! and a caller's ceiling under that paints the same gates on fewer texels.
//! This suite renders that sweep at the floor and at the sides under it and
//! counts, per gate, whether the gate still owns at least one texel of the
//! value grid, which is the grid the picture is coloured from. A gate with no
//! texel is a gate the user cannot see at any zoom.
//!
//! Eight probe radials, one every 45°, carry a value unique to each of their
//! gates; their two neighbours carry a filler so the azimuthal contest near the
//! site is real; every other radial is below threshold and paints nothing.

use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};
use squallar_radar::render::render_radar_to_image_full_sized;
use squallar_radar::types::{self, RadarProduct};
use std::collections::HashSet;

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const ELEVATION: f32 = 0.5;
const GATES: usize = 1832;
const GATE_M: u16 = 250;
const FIRST_GATE_M: u16 = 2125;
const RADIALS: usize = 720;
const AZ_STEP_DEG: f32 = 0.5;
/// Radial indices at 0°, 45°, … 315°.
const PROBES: [usize; 8] = [0, 90, 180, 270, 360, 450, 540, 630];
/// Decoded value is `raw / SCALE`; 16-bit raws up to 14 657 stay under the
/// 999 the plan view paints below.
const SCALE: f32 = 16.0;
/// The neighbours' value: above every probe raw, under the paint limit.
const FILLER_RAW: u16 = 15_000;

/// The raw a probe radial `p` carries at gate `g` — never 0 or 1, which are
/// the reserved codes.
fn probe_raw(p: usize, g: usize) -> u16 {
    u16::try_from(2 + p * GATES + g).expect("8 × 1832 raws fit a u16")
}

fn moment(raws: impl Iterator<Item = u16>) -> MomentData {
    let bytes: Vec<u8> = raws.flat_map(u16::to_be_bytes).collect();
    MomentData::from_fixed_point(GATES as u16, FIRST_GATE_M, GATE_M, 16, SCALE, 0.0, bytes)
}

fn surveillance_cut() -> Scan {
    let is_probe = |i: usize| PROBES.contains(&i);
    let radials = (0..RADIALS)
        .map(|i| {
            let raws: Vec<u16> = if let Some(p) = PROBES.iter().position(|&x| x == i) {
                (0..GATES).map(|g| probe_raw(p, g)).collect()
            } else if is_probe((i + 1) % RADIALS) || is_probe((i + RADIALS - 1) % RADIALS) {
                vec![FILLER_RAW; GATES]
            } else {
                vec![0; GATES]
            };
            Radial::new(
                0,
                i as u16,
                i as f32 * AZ_STEP_DEG,
                AZ_STEP_DEG,
                RadialStatus::IntermediateRadialData,
                1,
                ELEVATION,
                Some(moment(raws.into_iter())),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Scan::new(
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
        ),
        vec![Sweep::new(1, radials)],
    )
}

/// One rung of the ladder: the side it drew at and how many of each probe's
/// gates own a texel there.
struct Rung {
    ceiling: usize,
    side: usize,
    extent_km: f64,
    survived: [usize; 8],
    /// Every `(probe, gate)` that owns no texel.
    lost: Vec<(usize, usize)>,
}

impl Rung {
    fn texels_per_gate(&self) -> f64 {
        self.side as f64 / (2.0 * self.extent_km) * (f64::from(GATE_M) / 1000.0)
    }
    fn total(&self) -> usize {
        self.survived.iter().sum()
    }
}

fn render_rung(scan: &Scan, ceiling: usize) -> Rung {
    let out = render_radar_to_image_full_sized(
        scan,
        ELEVATION,
        RadarProduct::Reflectivity,
        LAT,
        LON,
        squallar_radar::srv::MotionInputs::default(),
        None,
        None,
        &squallar_radar::nyquist::DeclaredNyquist::empty(),
        ceiling,
    )
    .expect("a filled surveillance cut renders");
    let side = out.values.len().isqrt();
    assert_eq!(side * side, out.values.len(), "the value grid is square");
    let painted: HashSet<u32> = out
        .values
        .iter()
        .filter(|v| !v.is_nan())
        .map(|v| v.to_bits())
        .collect();
    let mut survived = [0usize; 8];
    let mut lost: Vec<(usize, usize)> = Vec::new();
    for (p, count) in survived.iter_mut().enumerate() {
        for g in 0..GATES {
            if painted.contains(&(f32::from(probe_raw(p, g)) / SCALE).to_bits()) {
                *count += 1;
            } else {
                lost.push((p, g));
            }
        }
    }
    Rung {
        ceiling,
        side,
        extent_km: out.max_range_km,
        survived,
        lost,
    }
}

/// The gate index past which a 0.5° wedge is at least two texels wide on a
/// raster of `px_per_km` — outside it a gate has a texel no neighbour's
/// samples touch, inside it the radials crowd onto shared texels whatever the
/// side, which is the one loss a Cartesian raster cannot buy its way out of.
fn crowding_gate(px_per_km: f64) -> usize {
    let radius_km = 2.0 / (f64::from(AZ_STEP_DEG).to_radians() * px_per_km);
    ((radius_km - f64::from(FIRST_GATE_M) / 1000.0) / (f64::from(GATE_M) / 1000.0)).ceil() as usize
}

/// **Along range, one texel per gate is the floor and two buys nothing more:**
/// on every rung over one texel per gate, every gate outside the crowding
/// radius owns a texel, and every loss is inside it. At the base side the
/// losses reach the ring. The rungs are printed so a ceiling proposed under
/// the data's own is priced in gates, not guessed.
#[test]
fn over_one_texel_per_gate_every_gate_outside_the_crowding_radius_owns_a_texel() {
    let scan = surveillance_cut();
    const ALL: usize = PROBES.len() * GATES;
    // A ceiling above the data's floor: what a desktop reporting 16384+ is
    // given. Rendered from the top down so the first render sizes the cell
    // pool once and the smaller rungs reuse it.
    const ABOVE_FLOOR: usize = 8192;
    let rungs: Vec<Rung> = [ABOVE_FLOOR, 5522, 4096, 3681, types::IMAGE_SIZE]
        .into_iter()
        .map(|ceiling| render_rung(&scan, ceiling))
        .collect();
    for r in &rungs {
        let px_per_km = r.side as f64 / (2.0 * r.extent_km);
        let crowding = crowding_gate(px_per_km);
        let far = r.lost.iter().filter(|(_, g)| *g >= crowding).count();
        let max_lost = r.lost.iter().map(|(_, g)| *g).max();
        println!(
            "ceiling {:>5} -> side {:>5} px over ±{:.1} km: {px_per_km:.2} px/km, {:.3} \
             texels/gate; gates owning a texel {:>5}/{ALL} ({:.2}%), per probe {:?}; lost {} \
             (furthest at gate {:?}), {far} of them past the crowding gate {crowding} \
             ({:.1} km)",
            r.ceiling,
            r.side,
            r.extent_km,
            r.texels_per_gate(),
            r.total(),
            100.0 * r.total() as f64 / ALL as f64,
            r.survived,
            r.lost.len(),
            max_lost,
            (f64::from(FIRST_GATE_M) + crowding as f64 * f64::from(GATE_M)) / 1000.0,
        );
    }

    let floor = &rungs[0];
    assert!(
        floor.side < ABOVE_FLOOR,
        "an {ABOVE_FLOOR} ceiling bound a surveillance cut at {} px: the data's own side should \
         have",
        floor.side
    );
    assert!(
        floor.texels_per_gate() >= types::TEXELS_PER_SAMPLE - 1e-6,
        "{:.3} texels per gate at the floor",
        floor.texels_per_gate()
    );

    // The data's own floor is held to it whatever `TEXELS_PER_SAMPLE` says:
    // a rule that let the floor fall under one texel a gate would lose gates
    // along the radial, and this is where that would show.
    for (i, r) in rungs.iter().enumerate() {
        let px_per_km = r.side as f64 / (2.0 * r.extent_km);
        let crowding = crowding_gate(px_per_km);
        let far: Vec<&(usize, usize)> = r.lost.iter().filter(|(_, g)| *g >= crowding).collect();
        if i == 0 || r.texels_per_gate() > 1.1 {
            assert!(
                far.is_empty(),
                "at {:.3} texels per gate ({} px) {} gates past the crowding gate {crowding} own \
                 no texel: {:?}",
                r.texels_per_gate(),
                r.side,
                far.len(),
                far.iter().take(8).collect::<Vec<_>>(),
            );
        }
    }

    let base = rungs.last().unwrap();
    assert_eq!(base.side, types::IMAGE_SIZE);
    let base_crowding = crowding_gate(base.side as f64 / (2.0 * base.extent_km));
    let past_crowding = PROBES.len() * (GATES - base_crowding);
    let base_far = base
        .lost
        .iter()
        .filter(|(_, g)| *g >= base_crowding)
        .count();
    assert!(
        base_far * 10 > past_crowding,
        "the base side of {} px lost only {base_far} of the {past_crowding} gates past the \
         crowding radius, so the long-range side is buying little",
        types::IMAGE_SIZE
    );
}
