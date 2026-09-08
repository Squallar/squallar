//! What a raster side under the data's own floor costs, counted in gates.
//!
//! The widest honest sweep this display draws is a WSR-88D surveillance cut:
//! 1832 gates of 0.25 km on 720 radials of 0.5°. `types::data_limited_side_px`
//! asks `TEXELS_PER_SAMPLE` texels per gate for it — 7362 px over ±460 km —
//! and a caller's ceiling under that paints the same gates on fewer texels.
//! This suite renders that sweep at the floor and at the sides under it and
//! counts, per gate, whether the gate still owns at least one texel. A gate
//! with no texel is a gate the user cannot see at any zoom.
//!
//! **Ownership is read off the claim the render actually made** — `write_key`,
//! the rank `fetch_max` settled each pixel's contest by — which is why this
//! suite is in the crate rather than in `tests/`: the keys live in the cells,
//! and the cells are drained and handed back before a render returns. See
//! [`super::claims`].
//!
//! Eight probe radials, one every 45°, carry a value unique to each of their
//! gates; their two neighbours carry a filler so the azimuthal contest near the
//! site is real; every other radial is below threshold and paints nothing.

use super::{GateId, claims, render_radar_to_image_full_sized, write_key};
use crate::types::{self, RadarProduct};
use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};
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
    claims::arm();
    let out = render_radar_to_image_full_sized(
        scan,
        ELEVATION,
        RadarProduct::Reflectivity,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
        ceiling,
    )
    .expect("a filled surveillance cut renders");
    // **The oracle is the claim, not the value.** `write_key` is the rank
    // `fetch_max` settled each pixel's contest by, so it is literally "which
    // gate owns this texel" — the question this file asks.
    //
    // It replaced a value oracle on 2026-09-08, when the `f32` value grid that
    // oracle read was removed from the render for being written once per pixel
    // and never read. The value oracle worked only because `probe_raw` is
    // injective over the probe gates. **The colour the picture carries is not,
    // and could not have stood in for it**: this fixture runs 0.125 dBZ to
    // 916 dBZ against a ladder that saturates at 95, so almost every probe gate
    // paints the same white, and a colour oracle would have reported nearly
    // every gate as surviving on every rung. That would not have passed
    // quietly — the base-side assertion at the end of this file catches an
    // oracle that reports no losses, and a tamper confirms it does — but it
    // would have turned a measurement of the rasterizer into a measurement of
    // how flat the palette is up there. There is no colour-based fixture that
    // makes this question answerable: 14,656 probe gates need 14,656
    // distinguishable answers and the reflectivity ladder reaches 3,549.
    //
    // **The two oracles were measured against each other before the old one
    // went, and they agree digit for digit.** The value form was run on
    // `ec8e7855c` and this form on the change that replaced it; all five rungs
    // match on every figure the table below prints — total gates owning a
    // texel, the per-probe array, the loss count, the furthest lost gate and
    // the count past the crowding gate. That is the expected result rather than
    // a lucky one: `probe_raw` is injective over the probe gates, so "this
    // gate's value is somewhere in the raster" and "this gate's key is
    // somewhere in the raster" select the same gates. Nothing in this file's
    // expectations was re-pointed for the change.
    let keys = claims::take().expect("the render was armed");
    let side = keys.len().isqrt();
    assert_eq!(side * side, keys.len(), "the claim raster is square");
    assert_eq!(
        out.image.len(),
        keys.len() * 4,
        "the claim raster and the texture describe different pictures"
    );
    let claimed: HashSet<u32> = keys.into_iter().filter(|&k| k != 0).collect();
    let mut survived = [0usize; 8];
    let mut lost: Vec<(usize, usize)> = Vec::new();
    for (p, count) in survived.iter_mut().enumerate() {
        for g in 0..GATES {
            let key = write_key(GateId {
                radial: PROBES[p],
                gate: g,
            });
            if claimed.contains(&key) {
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
