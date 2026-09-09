//! The skeleton's two claims: it answers the gateless readers identically,
//! and it costs a small fraction of what a volume costs.

use super::*;
use nexrad_model::data::{
    ChannelConfiguration, DataMoment, ElevationCut, MomentData, PulseWidth, RadialStatus,
    VolumeCoveragePattern, WaveformType,
};

/// A VCP-212 cut table: the split-cut pairs at the bottom, then singles.
const TABLE: [f64; 16] = [
    0.5, 0.5, 0.9, 0.9, 1.3, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.5, 15.6,
];

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

/// **A volume shaped like a real one.** The shapes are the corpus's, named
/// where they are chosen so a reader can check them rather than trust them:
/// VCP 212 is 16 cuts; a WSR-88D sweep is ~720 radials at 0.5 degree spacing;
/// a surveillance reflectivity moment reaches 460 km at 250 m gates (1832)
/// and a Doppler moment 300 km (1192). Dual-pol moments are 16-bit.
///
/// It is a fixture and not a decode, so the RATIO it produces is a property
/// of these shapes. That is stated wherever the ratio is asserted; what makes
/// it meaningful is that the shapes are the real ones and that gate bytes
/// dominate both sides of it.
fn realistic_volume() -> Scan {
    fn moment(gates: u16, word: u8) -> MomentData {
        let bytes = usize::from(gates) * usize::from(word / 8);
        MomentData::from_fixed_point(gates, 2125, 250, word, 2.0, 66.0, vec![7u8; bytes])
    }
    let sweeps = (1..=16u8)
        .map(|n| {
            let doppler = n % 2 == 0 && n <= 6;
            let radials = (0..720u16)
                .map(|i| {
                    Radial::new(
                        1_700_000_000_000 + i64::from(n) * 30_000 + i64::from(i),
                        i + 1,
                        f32::from(i) * 0.5,
                        0.5,
                        RadialStatus::IntermediateRadialData,
                        n,
                        TABLE[usize::from(n) - 1] as f32,
                        Some(moment(1832, 8)),
                        doppler.then(|| moment(1192, 8)),
                        doppler.then(|| moment(1192, 8)),
                        Some(moment(1192, 16)),
                        Some(moment(1192, 16)),
                        Some(moment(1192, 16)),
                        None,
                    )
                })
                .collect();
            Sweep::new(n, radials)
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
            TABLE.iter().copied().map(cut).collect(),
        ),
        sweeps,
    )
}

fn gate_bytes(scan: &Scan) -> usize {
    scan.sweeps()
        .iter()
        .flat_map(Sweep::radials)
        .map(|r| {
            [
                r.reflectivity().map(|m| m.raw_values().len()),
                r.velocity().map(|m| m.raw_values().len()),
                r.spectrum_width().map(|m| m.raw_values().len()),
                r.differential_reflectivity().map(|m| m.raw_values().len()),
                r.differential_phase().map(|m| m.raw_values().len()),
                r.correlation_coefficient().map(|m| m.raw_values().len()),
            ]
            .into_iter()
            .flatten()
            .sum::<usize>()
        })
        .sum()
}

/// **Every gate buffer is released and nothing else is.**
///
/// The premise the other tests rest on, asserted on the structure rather than
/// on a byte count: a volume whose moments had been DROPPED rather than
/// emptied would also weigh little, and would fail `carries` in
/// `resolve_ladder` — so presence is checked slot by slot, per radial, not in
/// aggregate.
#[test]
fn the_skeleton_keeps_every_moment_and_releases_every_buffer() {
    let volume = realistic_volume();
    let skeleton = VolumeSkeleton::of(&volume);
    let bare = skeleton.as_scan_without_gates();

    assert!(
        gate_bytes(&volume) > 0,
        "fixture: the source volume carries no gates, so releasing them \
         proves nothing",
    );
    assert_eq!(gate_bytes(bare), 0, "the skeleton still carries gate bytes");

    assert_eq!(volume.sweeps().len(), bare.sweeps().len());
    for (from, to) in volume.sweeps().iter().zip(bare.sweeps()) {
        assert_eq!(from.elevation_number(), to.elevation_number());
        assert_eq!(from.radials().len(), to.radials().len());
        for (a, b) in from.radials().iter().zip(to.radials()) {
            assert_eq!(a.collection_timestamp(), b.collection_timestamp());
            assert_eq!(a.azimuth_number(), b.azimuth_number());
            assert_eq!(a.azimuth_angle_degrees(), b.azimuth_angle_degrees());
            assert_eq!(a.azimuth_spacing_degrees(), b.azimuth_spacing_degrees());
            assert_eq!(a.elevation_number(), b.elevation_number());
            assert_eq!(a.elevation_angle_degrees(), b.elevation_angle_degrees());
            // Presence AND the scalars, slot by slot. `gate_count` is hashed
            // by `ladder_fingerprint`, so it is the one that would move a
            // re-cut key silently.
            let slots: [(Option<&MomentData>, Option<&MomentData>); 6] = [
                (a.reflectivity(), b.reflectivity()),
                (a.velocity(), b.velocity()),
                (a.spectrum_width(), b.spectrum_width()),
                (a.differential_reflectivity(), b.differential_reflectivity()),
                (a.differential_phase(), b.differential_phase()),
                (a.correlation_coefficient(), b.correlation_coefficient()),
            ];
            for (from_slot, to_slot) in slots {
                assert_eq!(
                    from_slot.is_some(),
                    to_slot.is_some(),
                    "a moment slot's PRESENCE changed, which is what \
                     `resolve_ladder` asks when it chooses a sweep",
                );
                if let (Some(m), Some(n)) = (from_slot, to_slot) {
                    assert_eq!(m.gate_count(), n.gate_count(), "gate_count moved");
                    assert_eq!(n.raw_values().len(), 0, "a buffer survived");
                    assert_eq!(m.scale(), n.scale());
                    assert_eq!(m.offset(), n.offset());
                    assert_eq!(m.data_word_size(), n.data_word_size());
                    assert_eq!(m.gate_interval_km(), n.gate_interval_km());
                    assert_eq!(m.first_gate_range_km(), n.first_gate_range_km());
                }
            }
        }
    }
}

/// **The gateless readers answer identically off a skeleton** — on a volume
/// with real shapes, not the four-gate fixture the refutation used.
///
/// This is the claim the whole route rests on. It is asserted through the
/// same entry points the application calls: `current::resolve` then
/// `ladder_fingerprint` and `newest_data_time`.
#[test]
fn the_gateless_readers_answer_the_same_off_a_skeleton() {
    use crate::types::RadarProduct;

    let volume = realistic_volume();
    let skeleton = VolumeSkeleton::of(&volume);
    let declared = crate::nyquist::DeclaredNyquist::empty();

    let whole =
        crate::current::resolve(Some(crate::nyquist::Volume::new(&volume, &declared)), None)
            .expect("a base alone resolves");
    let bare = crate::current::resolve(
        Some(crate::nyquist::Volume::new(
            skeleton.as_scan_without_gates(),
            &declared,
        )),
        None,
    )
    .expect("a base alone resolves");

    assert_eq!(
        whole.sweeps().len(),
        bare.sweeps().len(),
        "the merge admitted a different number of sweeps off the skeleton",
    );
    for product in [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::SpectrumWidth,
    ] {
        let full = whole.ladder_fingerprint(product);
        assert!(
            full.is_some(),
            "fixture: the ladder does not resolve for {product:?} on this \
             volume, so the equality below is None == None",
        );
        assert_eq!(
            full,
            bare.ladder_fingerprint(product),
            "the re-cut key for {product:?} moved when the gate buffers were \
             released",
        );
    }
    assert!(
        whole.newest_data_time().is_some(),
        "fixture: no clocked radial"
    );
    assert_eq!(
        whole.newest_data_time(),
        bare.newest_data_time(),
        "the data-through stamp moved when the gate buffers were released",
    );
}

/// **What the route is worth, measured on this shape.**
///
/// The figure this whole lane turns on, taken here rather than asserted from
/// `scan_size`'s prose. The bound is deliberately loose — the claim is an
/// order of magnitude, not a pinned constant.
///
/// **It is a fixture, and the ratio is a property of ITS shapes.** What makes
/// it worth quoting is that both sides are dominated by terms this fixture
/// takes from the corpus: 16 cuts, 720 radials a sweep, 1832 surveillance
/// gates and 1192 Doppler gates, dual-pol at 16 bits.
#[test]
fn a_skeleton_is_a_small_fraction_of_the_volume_it_came_from() {
    let volume = realistic_volume();
    let whole = crate::scan_size::scan_bytes(&volume);
    let skeleton = VolumeSkeleton::of(&volume);
    let bare = skeleton.bytes();

    println!(
        "skeleton: {bare} B of {whole} B ({:.2} %), gates {} B",
        (bare as f64 / whole as f64) * 100.0,
        gate_bytes(&volume),
    );

    assert!(
        whole > 8 * 1024 * 1024,
        "fixture: a {whole} B volume is not the shape this ratio is about",
    );
    assert!(
        bare * 4 < whole,
        "the skeleton is {bare} B against the volume's {whole} B — over a \
         quarter, which is not the order of magnitude this route was taken \
         for",
    );
}

/// **The skeleton carries the volume's own site metadata**, because losing it
/// is a WRONG readout rather than a blank one.
///
/// `Scan::new` sets `site: None`, and `crate::types::ScanInfo::from_scan`
/// reads `data.site()` to decide whether a radar's position comes from the
/// VOLUME or falls back to the station table. A skeleton built with
/// `Scan::new` demotes every volume-stated position to the table row with no
/// error and no log — a different number on the readout, which is the one
/// failure class this whole route is not allowed to have.
///
/// **This is not hypothetical: the first version of `VolumeSkeleton::of` did
/// it**, and no other test here could see it. The equivalence test compares
/// the gateless READERS, and none of them looks at the site; the structure
/// test walks sweeps and radials, and the site is on neither. So this asserts
/// the field directly.
///
/// TAMPER: use `Scan::new` in `VolumeSkeleton::of` and this fails while every
/// other test in the module stays green.
#[test]
fn the_skeleton_carries_the_volumes_own_site() {
    use nexrad_model::meta::Site;

    let stated = Site::new(*b"KTLX", 35.33, -97.28, 370, 20);
    let with_site = Scan::with_site(
        stated.clone(),
        realistic_volume().coverage_pattern().clone(),
        realistic_volume().sweeps().to_vec(),
    );
    assert!(
        with_site.site().is_some(),
        "fixture: the source volume states no site, so a skeleton that \
         dropped one would look correct here",
    );

    let skeleton = VolumeSkeleton::of(&with_site);
    let carried = skeleton
        .as_scan_without_gates()
        .site()
        .expect("the skeleton dropped the volume's stated site");
    assert_eq!(
        carried, &stated,
        "the skeleton changed the volume's stated site, so a position the \
         volume declared would be read back as something else",
    );

    // And the other direction: a volume that states nothing must not acquire
    // a site, or the skeleton would be inventing a position.
    let without = realistic_volume();
    assert!(
        without.site().is_none(),
        "fixture: the plain volume already states a site",
    );
    assert!(
        VolumeSkeleton::of(&without)
            .as_scan_without_gates()
            .site()
            .is_none(),
        "the skeleton invented a site the volume never stated",
    );
}
