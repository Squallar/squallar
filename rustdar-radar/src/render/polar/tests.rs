//! The inverse rule on its own, on geometry written down by hand.
//!
//! What these cannot check is that the rule matches the rasterizer, because
//! nothing here goes through a rasterizer. That is
//! [`super::super::tests::the_polar_field_answers_what_the_value_grid_holds`]'s
//! job, and it is the one that would catch the two drifting apart; these pin
//! the edges that a whole-sweep comparison averages over.

use super::*;

/// A field of `n` wedges evenly spaced from north, each `width` wide, whose
/// value at `(radial, gate)` is `radial * 1000 + gate` so that every answer
/// says which gate it came from.
fn ring(n: usize, width: f64, gates: usize) -> PolarField {
    let step = 360.0 / n as f64;
    let wedges = (0..n)
        .map(|i| Wedge {
            azimuth_deg: (i as f64 * step) as f32,
            half_width_deg: (width / 2.0) as f32,
        })
        .collect();
    let values = (0..n)
        .flat_map(|r| (0..gates).map(move |g| (r * 1000 + g) as f32))
        .collect();
    PolarField::from_parts(
        PolarGeometry::from_parts(wedges, 0.5, 1.0, None, gates),
        values,
    )
}

/// Which gate a point resolves to, as `radial * 1000 + gate`.
fn read(f: &PolarField, az: f64, km: f64) -> Option<f32> {
    f.at(f.geometry().pick(az, km)?)
}

#[test]
fn a_gate_owns_half_an_interval_either_side_of_its_centre() {
    let f = ring(4, 90.0, 3);
    // Gate centres at 0.5, 1.5, 2.5 km with a 1 km depth: gate 0 spans
    // [0, 1), gate 1 spans [1, 2). `render_gate`'s `t ∈ [0, 1)` is what makes
    // the far edge exclusive, so 1.0 km is gate 1 and not gate 0.
    assert_eq!(read(&f, 0.0, 0.0), Some(0.0), "the near edge of gate 0");
    assert_eq!(read(&f, 0.0, 0.999), Some(0.0));
    assert_eq!(read(&f, 0.0, 1.0), Some(1.0), "the seam belongs to gate 1");
    assert_eq!(read(&f, 0.0, 2.999), Some(2.0), "the last gate");
}

#[test]
fn a_range_off_either_end_of_the_radial_is_in_no_gate() {
    let f = ring(4, 90.0, 3);
    assert_eq!(read(&f, 0.0, -0.001), None, "inside gate 0's near edge");
    assert_eq!(read(&f, 0.0, 3.0), None, "past the last gate's far edge");
    assert_eq!(read(&f, 0.0, 500.0), None);
}

#[test]
fn a_wedge_spanning_north_is_one_interval_and_not_two() {
    // Radial 0 sits at 0° and is 90° wide, so it owns 315°..360° and 0°..45°.
    // Folding the difference onto (-180, 180] is what keeps that one test.
    let f = ring(4, 90.0, 2);
    assert_eq!(read(&f, 359.9, 0.5), Some(0.0));
    assert_eq!(read(&f, 0.0, 0.5), Some(0.0));
    assert_eq!(read(&f, 44.9, 0.5), Some(0.0));
    assert_eq!(
        read(&f, 45.1, 0.5),
        Some(1000.0),
        "over the seam into radial 1"
    );
    // And the same question asked past a full turn.
    assert_eq!(read(&f, 720.0, 0.5), Some(0.0));
    assert_eq!(read(&f, -0.1, 0.5), Some(0.0));
}

#[test]
fn the_seam_between_two_tiling_wedges_belongs_to_the_one_that_starts_there() {
    let f = ring(4, 90.0, 2);
    // Radial 1 is centred at 90° and spans [45, 135). Exactly 45° is its
    // first sample, not radial 0's last — `render_gate` paints
    // `[centre - half, centre + half)`.
    assert_eq!(read(&f, 45.0, 0.5), Some(1000.0));
    assert_eq!(read(&f, 135.0, 0.5), Some(2000.0));
}

#[test]
fn where_two_wedges_overlap_the_later_radial_wins() {
    // The lying-declaration case: radials 0.5° apart each declaring 1.0°, so
    // every point is inside two wedges. `write_key` ranks radial-major and
    // `fetch_max` takes the greatest, so the higher index is what the raster
    // holds — and it is what this must answer.
    let wedges = (0..4)
        .map(|i| Wedge {
            azimuth_deg: i as f32 * 0.5,
            half_width_deg: 0.5,
        })
        .collect();
    let values = (0..4)
        .flat_map(|r| (0..2).map(move |g| (r * 1000 + g) as f32))
        .collect();
    let f = PolarField::from_parts(PolarGeometry::from_parts(wedges, 0.5, 1.0, None, 2), values);

    // 1.0° is inside radial 1 ([0.0, 1.0)? no — [0.0,1.0) excludes 1.0),
    // radial 2 ([0.5, 1.5)) and radial 3 ([1.0, 2.0)). The greatest wins.
    assert_eq!(read(&f, 1.0, 0.5), Some(3000.0));
    // 0.75° is inside radials 1 and 2 only.
    assert_eq!(read(&f, 0.75, 0.5), Some(2000.0));
}

#[test]
fn a_radial_that_painted_nothing_does_not_answer_for_its_neighbours() {
    let mut wedges: Vec<Wedge> = (0..4)
        .map(|i| Wedge {
            azimuth_deg: i as f32 * 90.0,
            half_width_deg: 45.0,
        })
        .collect();
    // Radial 3 never reached `render_gate` — every gate on it was below
    // threshold — so it has no wedge. The gap stays a gap, which is the
    // property `l2_wedge_width_deg` exists to keep.
    wedges[3] = Wedge::UNPAINTED;
    let values = (0..4)
        .flat_map(|r| (0..2).map(move |g| (r * 1000 + g) as f32))
        .collect();
    let f = PolarField::from_parts(PolarGeometry::from_parts(wedges, 0.5, 1.0, None, 2), values);

    assert_eq!(read(&f, 270.0, 0.5), None, "the silenced radial's own sky");
    assert_eq!(
        read(&f, 180.0, 0.5),
        Some(2000.0),
        "its neighbour is unaffected"
    );
}

#[test]
fn an_unpainted_gate_reads_as_no_value_rather_than_as_a_nan() {
    let wedges = vec![Wedge {
        azimuth_deg: 0.0,
        half_width_deg: 180.0,
    }];
    let f = PolarField::from_parts(
        PolarGeometry::from_parts(wedges, 0.5, 1.0, None, 3),
        vec![1.0, f32::NAN, 3.0],
    );
    assert_eq!(read(&f, 0.0, 0.5), Some(1.0));
    assert_eq!(read(&f, 0.0, 1.5), None, "the render painted nothing here");
    assert_eq!(read(&f, 0.0, 2.5), Some(3.0));
}

#[test]
fn a_field_with_no_gates_answers_nothing_rather_than_dividing_by_zero() {
    let empty = PolarField::default();
    assert!(empty.geometry().is_empty());
    assert_eq!(read(&empty, 0.0, 10.0), None);

    // A zero gate depth is the other way in: `data_limited_side_px` already
    // treats a non-positive spacing as saying nothing about sampling, and a
    // field built on one would divide by it.
    let f = PolarField::from_parts(
        PolarGeometry::from_parts(
            vec![Wedge {
                azimuth_deg: 0.0,
                half_width_deg: 1.0,
            }],
            0.0,
            0.0,
            None,
            1,
        ),
        vec![7.0],
    );
    assert_eq!(read(&f, 0.0, 0.0), None);
}

#[test]
fn stripping_the_values_keeps_every_gate_findable() {
    // What a loop frame holds: the geometry, and no numbers. `pick` still
    // resolves — that is the half a loop frame reads its own volume with —
    // and `at` declines, because this field has nothing to say.
    let mut f = ring(8, 45.0, 4);
    let at = f.geometry().pick(90.0, 2.5).expect("inside the picture");
    assert_eq!(f.at(at), Some(2002.0));
    assert!(f.has_values());

    f.strip_values();
    assert!(!f.has_values());
    assert_eq!(
        f.geometry().pick(90.0, 2.5),
        Some(at),
        "the geometry survives"
    );
    assert_eq!(f.at(at), None);
    assert_eq!(f.resident_bytes(), f.geometry().resident_bytes());
}

#[test]
fn the_geometry_is_a_thousandth_of_the_values_it_indexes() {
    // The whole reason the two are separate types. A full ring of a
    // surveillance cut: 720 radials of 1832 gates.
    let f = ring(720, 0.5, 1832);
    let values = f.resident_bytes() - f.geometry().resident_bytes();
    assert_eq!(values, 720 * 1832 * 4, "radials × gates × f32");
    assert_eq!(
        f.geometry().resident_bytes(),
        720 * 8,
        "az + half, f32 each"
    );
    assert!(
        f.geometry().resident_bytes() * 900 < values,
        "geometry {} B against values {values} B",
        f.geometry().resident_bytes(),
    );
}

#[test]
fn a_field_survives_the_round_trip_the_browsers_worker_port_makes() {
    // The page↔worker port transfers buffers, so a field's byte form is the
    // only shape it crosses in. Every field has to come back the same picture,
    // including a loop frame's — geometry with no numbers behind it.
    for mut f in [ring(37, 9.7, 23), ring(1, 180.0, 1), PolarField::default()] {
        let back = PolarField::from_bytes(&f.to_bytes()).expect("this build wrote it");
        assert_eq!(back.geometry().wedges(), f.geometry().wedges());
        assert_eq!(back.geometry().gates(), f.geometry().gates());
        assert_eq!(back.geometry().reach_gates(), f.geometry().reach_gates());
        assert_eq!(
            back.geometry().first_gate_slant_km(),
            f.geometry().first_gate_slant_km()
        );
        assert_eq!(
            back.geometry().gate_interval_slant_km(),
            f.geometry().gate_interval_slant_km()
        );
        assert_eq!(back.resident_bytes(), f.resident_bytes());
        for r in 0..f.geometry().radials() {
            for g in 0..f.geometry().gates() {
                let at = GateAt { radial: r, gate: g };
                assert_eq!(back.at(at), f.at(at), "({r}, {g})");
            }
        }

        f.strip_values();
        let stripped = PolarField::from_bytes(&f.to_bytes()).expect("this build wrote it");
        assert!(!stripped.has_values());
        assert_eq!(stripped.geometry().wedges(), f.geometry().wedges());
    }
}

#[test]
fn a_message_this_build_did_not_write_is_declined_rather_than_indexed_into() {
    let good = ring(5, 60.0, 4).to_bytes();
    assert!(PolarField::from_bytes(&good).is_some());
    // Truncated at every length short of the whole.
    for n in 0..good.len() {
        assert!(
            PolarField::from_bytes(&good[..n]).is_none(),
            "a {n}-byte prefix was accepted"
        );
    }
    // One byte too many is not this build's message either.
    let mut long = good.clone();
    long.push(0);
    assert!(PolarField::from_bytes(&long).is_none());
    // A header claiming a values buffer that is not radials × gates.
    let mut lying = good.clone();
    lying[12..16].copy_from_slice(&3u32.to_le_bytes());
    assert!(PolarField::from_bytes(&lying).is_none());
}
