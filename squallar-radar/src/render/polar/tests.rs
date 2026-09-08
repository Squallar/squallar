//! The inverse rule on its own, on geometry written down by hand.

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

/// A field assembled by hand, for
/// [`the_polar_wire_layout_is_the_one_this_protocol_ships`].
fn layout_fixture() -> PolarField {
    PolarField {
        geometry: PolarGeometry {
            wedges: vec![
                Wedge {
                    azimuth_deg: 1.5,
                    half_width_deg: 0.25,
                },
                Wedge {
                    azimuth_deg: 90.25,
                    half_width_deg: 0.75,
                },
                Wedge {
                    azimuth_deg: 180.5,
                    half_width_deg: 1.125,
                },
            ],
            first_gate_slant_km: 0.125,
            gate_interval_slant_km: 0.25,
            elevation_deg: Some(0.5),
            gates: 4,
            reach_gates: 2,
        },
        // `radials * gates`, which is what `from_bytes` insists on, carrying
        // both of the two states the renderer puts on this wire beside ordinary
        // numbers: `NaN` for a gate it painted nothing at, and the range-folded
        // sentinel for one it painted the folded colour at.
        values: Values::Wide(vec![
            0.0,
            -1.5,
            2.25,
            f32::NAN,
            0.5,
            -0.75,
            super::super::RANGE_FOLDED_SENTINEL,
            3.125,
            -16.0,
            32.5,
            64.75,
            -128.25,
        ]),
    }
}

/// The bytes this protocol ships are **these** bytes.
#[test]
fn the_polar_wire_layout_is_the_one_this_protocol_ships() {
    let bytes = layout_fixture().to_bytes();
    assert_eq!(
        (bytes.len(), crate::wire::layout_digest(&bytes)),
        (112, 0x986a_92ef_b56e_c209),
        "the bytes `PolarField::to_bytes` writes are not the bytes this pin \
         was last told. Something about this payload's layout moved — a \
         field added, removed, reordered, retyped, or written at a different \
         width. These bytes are **form 0** of the polar tail: `to_tail` \
         writes the form byte in FRONT of this payload and never into it, so \
         a second form did not move them and this pin still covers what the \
         reply carries. A change to them is seen twice — here, and by \
         `squallar_worker::wire_identity::WIRE_FRAME_REPLY_ROWS`, whose \
         `frame/*/polar` rows digest this payload behind that byte and feed \
         the local build token. (The clause this replaced said nothing else \
         in the workspace could see a change to these bytes. That was true \
         the day it was written, `24f8592f8` on 2026-08-18, and stopped \
         being true the next day, when `b09c75294` gave the frame reply \
         per-tail digest rows and one of them was this payload.) If the \
         change was deliberate, re-pin \
         the length and digest here, deliberately. Deployed pages and \
         workers from opposite sides of a deploy refuse each other by \
         GITHUB_SHA at the HELLO handshake; a LOCAL pair differing only \
         here still attaches, and `from_bytes`'s length checks turn most \
         such pairs into `None` and a readout that goes quiet — the \
         accepted residual `squallar_worker::wire_identity` records, until \
         full layout identity joins the token.",
    );
}

/// A geometry over `wedges`, with gate 0 at 0.5 km and a 1 km depth so a
/// ground range of 40 km lands well inside every radial.
fn geometry_of(wedges: Vec<Wedge>) -> PolarGeometry {
    PolarGeometry::from_parts(wedges, 0.5, 1.0, None, 200)
}

/// A uniform sweep: `n` radials evenly spaced, each painted at exactly half
/// the spacing, so no two wedges overlap and none leaves a gap.
fn uniform_wedges(n: usize) -> Vec<Wedge> {
    let step = 360.0 / n as f32;
    (0..n)
        .map(|i| Wedge {
            azimuth_deg: i as f32 * step,
            half_width_deg: step / 2.0,
        })
        .collect()
}

/// **`draw_edges` is the identity on wedges that do not overlap.**
///
/// The promise the whole substitution rests on: the fan may only change what
/// the picture shows inside the slivers the raster was already ambiguous
/// about. If it moved a non-overlapping edge it would be re-drawing gates that
/// were never in question.
#[test]
fn drawn_edges_leave_a_non_overlapping_sweep_alone() {
    for n in [360usize, 720, 17] {
        let wedges = uniform_wedges(n);
        let edges = draw_edges(&wedges);
        assert_eq!(edges.len(), n);
        for (i, w) in wedges.iter().enumerate() {
            let natural_lo = w.azimuth_deg - w.half_width_deg;
            let natural_hi = w.azimuth_deg + w.half_width_deg;
            assert!(
                (edges[i].lo_deg - natural_lo).abs() < 1e-3
                    && (edges[i].hi_deg - natural_hi).abs() < 1e-3,
                "{n} radials, radial {i}: drawn {:?} but the wedge was \
                 [{natural_lo}, {natural_hi}] and nothing overlaps it",
                edges[i],
            );
        }
    }
}

/// **Overlapping wedges are trimmed to the bisector, and the sliver is split
/// rather than given to whoever was written last.**
#[test]
fn an_overlap_is_removed_at_the_midpoint() {
    // Two radials 1 degree apart, each painted 2 degrees wide: they overlap
    // over most of their span.
    let wedges = vec![
        Wedge {
            azimuth_deg: 10.0,
            half_width_deg: 1.0,
        },
        Wedge {
            azimuth_deg: 11.0,
            half_width_deg: 1.0,
        },
    ];
    let edges = draw_edges(&wedges);
    // The bisector is 10.5 on the near side; on the far side the gap is 359
    // degrees, so neither is trimmed there.
    assert!((edges[0].hi_deg - 10.5).abs() < 1e-4, "{:?}", edges[0]);
    assert!((edges[1].lo_deg - 10.5).abs() < 1e-4, "{:?}", edges[1]);
    assert!((edges[0].lo_deg - 9.0).abs() < 1e-4, "{:?}", edges[0]);
    assert!((edges[1].hi_deg - 12.0).abs() < 1e-4, "{:?}", edges[1]);
    // And they abut exactly: no gap opened where the overlap was.
    assert!((edges[0].hi_deg - edges[1].lo_deg).abs() < 1e-4);
}

/// **No point on the circle is claimed by two radials**, for overlapping and
/// non-overlapping input alike.
///
/// The property the fan needs and the raster never had: with no depth buffer,
/// two triangles claiming one pixel is whichever the rasteriser reached last.
/// A dense walk of the whole circle, not a pairwise argument.
#[test]
fn the_drawn_sweep_claims_every_azimuth_at_most_once() {
    let arms: Vec<(&str, Vec<Wedge>)> = vec![
        ("uniform 720", uniform_wedges(720)),
        ("uniform 360", uniform_wedges(360)),
        (
            "widened past the spacing, so every radial overlaps both neighbours",
            (0..360)
                .map(|i| Wedge {
                    azimuth_deg: i as f32,
                    half_width_deg: 2.0,
                })
                .collect(),
        ),
        (
            "ragged: a jittered sweep with uneven gaps",
            (0..360)
                .map(|i| Wedge {
                    azimuth_deg: i as f32 + ((i * 7) % 5) as f32 * 0.1,
                    half_width_deg: 0.9,
                })
                .collect(),
        ),
    ];
    for (name, wedges) in arms {
        let edges = draw_edges(&wedges);
        let mut covered = 0usize;
        for step in 0..36_000 {
            let az = f64::from(step) / 100.0;
            let claims = edges.iter().filter(|e| e.contains(az)).count();
            assert!(
                claims <= 1,
                "{name}: azimuth {az} is claimed by {claims} radials; with no depth buffer \
                 the picture there is whichever triangle the rasteriser reached last",
            );
            covered += claims;
        }
        // And the sweep still covers the sky it did before: trimming removes
        // overlap, never coverage. A rule that emptied every wedge would pass
        // the at-most-once check above on its own.
        assert!(
            covered > 35_900,
            "{name}: only {covered} of 36000 probes are covered, so trimming removed coverage \
             rather than overlap",
        );
    }
}

/// **The drawn pick agrees with the painted pick wherever the raster was
/// unambiguous, and differs only inside a contested sliver.**
///
/// Hover reads one of these and the picture shows the other, so a disagreement
/// outside a sliver would be a readout that names a gate no pixel came from.
#[test]
fn the_drawn_pick_matches_the_painted_pick_outside_the_slivers() {
    let geometry = geometry_of(uniform_wedges(720));
    let edges = draw_edges(geometry.wedges());
    let ground_km = 40.0;
    let mut compared = 0usize;
    for step in 0..36_000 {
        let az = f64::from(step) / 100.0;
        assert_eq!(
            geometry.pick_drawn(&edges, az, ground_km),
            geometry.pick(az, ground_km),
            "azimuth {az}: the fan and the raster disagree about a point no two wedges \
             contest",
        );
        compared += 1;
    }
    assert_eq!(compared, 36_000);

    // Now the contested case: every radial two degrees wide on a one-degree
    // spacing. The two picks MUST differ somewhere, or the sliver rule is not
    // doing anything and the test above proves nothing.
    let contested = geometry_of(
        (0..360)
            .map(|i| Wedge {
                azimuth_deg: i as f32,
                half_width_deg: 1.0,
            })
            .collect(),
    );
    let contested_edges = draw_edges(contested.wedges());
    let mut differ = 0usize;
    for step in 0..36_000 {
        let az = f64::from(step) / 100.0;
        if contested.pick_drawn(&contested_edges, az, ground_km) != contested.pick(az, ground_km) {
            differ += 1;
        }
    }
    assert!(
        differ > 0,
        "the two picks agree everywhere even where wedges overlap, so the trim is inert",
    );
}

/// **An unpainted radial draws nothing and does not stretch its neighbours.**
#[test]
fn an_unpainted_radial_draws_nothing() {
    let mut wedges = uniform_wedges(360);
    wedges[100] = Wedge::UNPAINTED;
    wedges[101] = Wedge::UNPAINTED;
    let edges = draw_edges(&wedges);
    assert!(edges[100].is_empty(), "{:?}", edges[100]);
    assert!(edges[101].is_empty(), "{:?}", edges[101]);
    // The live neighbours either side keep their own width rather than
    // growing across the hole: a gap in the data is a gap in the picture.
    for i in [99usize, 102] {
        let w = wedges[i];
        assert!(
            (edges[i].hi_deg - edges[i].lo_deg - 2.0 * w.half_width_deg).abs() < 1e-3,
            "radial {i} was stretched across the unpainted gap: {:?}",
            edges[i],
        );
    }
    // Nothing is claimed inside the hole the two dead radials leave. Its
    // edges are radial 99's own `hi` and radial 102's own `lo`, read off the
    // table rather than assumed, because assuming them is how this probe
    // walked out of the hole and past a live neighbour the first time.
    let (hole_lo, hole_hi) = (edges[99].hi_deg, edges[102].lo_deg);
    assert!(
        hole_hi - hole_lo > 1.0,
        "[{hole_lo}, {hole_hi}] is not a hole"
    );
    for step in 0..200 {
        let az = f64::from(hole_lo) + f64::from(hole_hi - hole_lo) * f64::from(step) / 200.0;
        assert_eq!(
            edges.iter().filter(|e| e.contains(az)).count(),
            0,
            "azimuth {az} is inside the unpainted hole and something claimed it",
        );
    }
    // A sweep of nothing but unpainted radials draws nothing at all.
    let none = draw_edges(&[Wedge::UNPAINTED; 16]);
    assert!(none.iter().all(|e| e.is_empty()));
}

/// **A wedge spanning north is one interval, not two.**
///
/// The seam test. `lo` is allowed to be negative and the pick wraps, because
/// the fan's vertex shader works in a continuous longitude frame and a table
/// folded onto `[0, 360)` would put a visible seam at true north.
#[test]
fn a_sweep_across_north_has_no_seam() {
    let wedges = uniform_wedges(360);
    let edges = draw_edges(&wedges);
    // Radial 0 sits on north, so its span crosses zero.
    assert!(edges[0].lo_deg < 0.0, "{:?}", edges[0]);
    assert!(edges[0].hi_deg > 0.0, "{:?}", edges[0]);
    // Points either side of north are claimed, and by the same radial.
    for az in [359.9_f64, 359.99, 0.0, 0.1] {
        let claims: Vec<usize> = (0..edges.len())
            .filter(|&i| edges[i].contains(az))
            .collect();
        assert_eq!(claims, vec![0], "azimuth {az} across the north seam");
    }
}

/// A field of `radials × gates` whose gate `(r, g)` carries
/// `values[(r * gates + g) % values.len()]`, so a caller states exactly how
/// many distinct numbers the plane holds.
fn field_of(radials: usize, gates: usize, values: &[f32]) -> PolarField {
    let wedges = uniform_wedges(radials);
    let cells = (0..radials * gates)
        .map(|i| values[i % values.len()])
        .collect();
    PolarField::from_parts(
        PolarGeometry::from_parts(wedges, 0.5, 1.0, None, gates),
        cells,
    )
}

/// Every gate of a field, read straight out of the plane rather than through a
/// pick, so a comparison covers gates no azimuth resolves to.
fn every_gate(f: &PolarField) -> Vec<Option<f32>> {
    let g = f.geometry();
    (0..g.radials())
        .flat_map(|radial| (0..g.gates()).map(move |gate| GateAt { radial, gate }))
        .map(|at| f.at(at))
        .collect()
}

/// The numbers a render can paint, including the two it paints that are NaNs
/// meaning different things and the two zeroes that compare equal.
fn every_kind_of_painted_number() -> Vec<f32> {
    vec![
        0.0,
        -0.0,
        -1.5,
        2.25,
        f32::NAN,
        super::super::RANGE_FOLDED_SENTINEL,
        f32::MIN,
        f32::MAX,
        -128.25,
    ]
}

#[test]
fn compacting_returns_every_number_unchanged() {
    let wide = field_of(9, 7, &every_kind_of_painted_number());
    let mut narrow = wide.clone();
    narrow.compact_values();

    assert!(
        narrow.resident_bytes() < wide.resident_bytes(),
        "nothing was compacted: {} B against {} B",
        narrow.resident_bytes(),
        wide.resident_bytes(),
    );
    assert_eq!(
        every_gate(&narrow),
        every_gate(&wide),
        "a compacted plane answered a gate differently from the plane it was \
         built from. The table holds the painted numbers themselves, so this \
         is an indexing and can only differ if a code was assigned to the \
         wrong pattern.",
    );
    assert!(narrow.has_values(), "compacting is not stripping");
}

#[test]
fn compacting_keeps_the_two_nans_apart() {
    // `f32` equality says these are neither equal nor unequal, and both are
    // painted: one means "no gate here", the other means "range folded". A
    // table keyed on the number rather than on its bits merges them, and the
    // wire then comes back with one where the other was.
    let unpainted = f32::NAN;
    let folded = super::super::RANGE_FOLDED_SENTINEL;
    assert_ne!(
        unpainted.to_bits(),
        folded.to_bits(),
        "this test is vacuous unless the two sentinels differ",
    );

    let wide = field_of(4, 2, &[unpainted, folded, 3.0, -0.0, 0.0, 7.5, -2.0, 9.0]);
    let mut narrow = wide.clone();
    narrow.compact_values();
    assert_eq!(
        narrow.to_bytes(),
        wide.to_bytes(),
        "two distinct bit patterns were folded into one code",
    );
}

#[test]
fn the_wire_is_the_same_bytes_from_either_form() {
    let mut narrow = layout_fixture();
    narrow.compact_values();
    assert_eq!(
        narrow.to_bytes(),
        layout_fixture().to_bytes(),
        "compacting moved the bytes `to_bytes` writes. That encoder widens \
         whichever form it is handed, so the pin in \
         `the_polar_wire_layout_is_the_one_this_protocol_ships` covers both \
         forms; `to_tail` is the one that writes a coded plane coded, and \
         `a_tail_names_the_form_it_carries` is what covers that.",
    );
}

#[test]
fn a_compacted_field_survives_the_round_trip_the_worker_port_makes() {
    let mut sent = ring(8, 45.0, 4);
    sent.compact_values();
    let back = PolarField::from_bytes(&sent.to_bytes()).expect("its own bytes");
    assert_eq!(every_gate(&back), every_gate(&sent));
    assert_eq!(back, sent, "the two forms compare by their numbers");
}

#[test]
fn compacting_falls_the_price_to_a_byte_a_gate_and_the_table() {
    let numbers = every_kind_of_painted_number();
    let (radials, gates) = (720, 1832);
    let wide = field_of(radials, gates, &numbers);
    let mut narrow = wide.clone();
    narrow.compact_values();

    let geometry = wide.geometry().resident_bytes();
    assert_eq!(
        wide.resident_bytes() - geometry,
        radials * gates * size_of::<f32>(),
        "radials × gates × f32",
    );
    assert_eq!(
        narrow.resident_bytes() - geometry,
        radials * gates + numbers.len() * size_of::<f32>(),
        "a byte a gate, plus one f32 per distinct number this field holds",
    );
}

#[test]
fn a_plane_with_more_numbers_than_a_byte_can_name_stays_wide() {
    // 256 distinct numbers is what a byte addresses and 257 is one past it,
    // which is the boundary and not a magic number: the wire byte a radar
    // moment carries has 256 reachable values, and a computed field has as
    // many as it has gates.
    for (distinct, compacts) in [(255, true), (256, true), (257, false)] {
        let numbers: Vec<f32> = (0..distinct).map(|i| i as f32 * 0.25).collect();
        let wide = field_of(distinct, distinct, &numbers);
        let mut narrow = wide.clone();
        narrow.compact_values();

        assert_eq!(
            every_gate(&narrow),
            every_gate(&wide),
            "{distinct} distinct: a refused plane must still read its numbers",
        );
        assert_eq!(
            narrow.resident_bytes() < wide.resident_bytes(),
            compacts,
            "{distinct} distinct numbers: expected compacted = {compacts}",
        );
    }
}

#[test]
fn compacting_a_stripped_field_holds_nothing() {
    let mut f = ring(8, 45.0, 4);
    f.strip_values();
    f.compact_values();
    assert!(!f.has_values());
    assert_eq!(f.resident_bytes(), f.geometry().resident_bytes());
}

/// **A tail says which of the two forms it carries, in front of it.**
///
/// The one fact the second form could not exist without: the payloads are not
/// distinguishable from their own bytes, so what distinguishes them is a byte
/// that is not part of either.
#[test]
fn a_tail_names_the_form_it_carries() {
    let wide = ring(8, 45.0, 4);
    let mut coded = wide.clone();
    coded.compact_values();

    assert_eq!(wide.wire_form(), PolarWireForm::Wide);
    assert_eq!(coded.wire_form(), PolarWireForm::Coded);
    assert_eq!(wide.to_tail()[0], PolarWireForm::Wide.wire_code());
    assert_eq!(coded.to_tail()[0], PolarWireForm::Coded.wire_code());
    assert_ne!(
        PolarWireForm::Wide.wire_code(),
        PolarWireForm::Coded.wire_code(),
        "this test is vacuous unless the two forms spell themselves apart",
    );
}

/// **The wide payload is the tail minus its form byte, unchanged.**
///
/// `the_polar_wire_layout_is_the_one_this_protocol_ships` pins those bytes,
/// and this is what says the pin still covers what crosses the wire: the form
/// byte was added in front of that payload rather than into it.
#[test]
fn the_wide_tail_is_the_pinned_payload_behind_a_form_byte() {
    let f = layout_fixture();
    let tail = f.to_tail();
    assert_eq!(
        &tail[1..],
        f.to_bytes().as_slice(),
        "the form byte moved bytes inside the payload this protocol pins",
    );
    assert_eq!(tail.len(), f.to_bytes().len() + 1);
}

/// **A coded tail comes back coded**, which is the whole point: a page across
/// a worker port holds one byte a gate and never materializes the wide plane.
#[test]
fn a_coded_tail_arrives_still_coded() {
    // 8 x 4 gates carry 32 distinct numbers, which a byte names.
    let mut sent = ring(8, 45.0, 4);
    sent.compact_values();
    let back = PolarField::from_tail(&sent.to_tail()).expect("its own tail");

    assert_eq!(
        back.wire_form(),
        PolarWireForm::Coded,
        "widened at the door"
    );
    assert_eq!(
        back.resident_bytes(),
        sent.resident_bytes(),
        "the receiver pays a different price from the sender for one plane",
    );
    assert_eq!(every_gate(&back), every_gate(&sent));
    assert_eq!(back, sent);
}

/// **Neither form is ever read as the other**, which is what a versionless
/// encoding could not say. Told the wrong form, the decoder declines rather
/// than reading a table of codes as `f32`s.
#[test]
fn a_tail_read_as_the_other_form_is_declined_rather_than_misread() {
    // The pinned layout fixture, which carries both NaNs — so the round trip
    // below is asserted on the bytes rather than on `PartialEq`, which says a
    // plane holding a NaN equals nothing, itself included.
    let wide = layout_fixture();
    let mut coded = wide.clone();
    coded.compact_values();

    for (name, f) in [("wide", &wide), ("coded", &coded)] {
        let mut tail = f.to_tail();
        assert_eq!(
            PolarField::from_tail(&tail).map(|back| back.to_bytes()),
            Some(f.to_bytes()),
            "{name}: its own tail does not round-trip",
        );
        // The other form's byte over the same payload.
        tail[0] = match PolarWireForm::from_wire_code(tail[0]).expect("this build wrote it") {
            PolarWireForm::Wide => PolarWireForm::Coded.wire_code(),
            PolarWireForm::Coded => PolarWireForm::Wide.wire_code(),
        };
        assert_eq!(
            PolarField::from_tail(&tail),
            None,
            "{name}: a payload read as the form it is not was accepted",
        );
    }
}

/// A form byte this build does not write, and a tail with no form byte at
/// all.
///
/// **Both payloads, because one of them alone cannot see this.** An unknown
/// code that fell back to *wide* would still decline a coded payload — the
/// two forms' length arithmetic disagrees, so the length check refuses it and
/// a coded-only fixture reads green straight through the defect. The wide
/// payload is what such a fallback accepts, and the coded payload is what a
/// fallback to *coded* would. A tamper is what found that: with
/// `from_wire_code` answering `Some(Wide)` to every code, the coded-only form
/// of this test passed.
#[test]
fn a_form_this_build_does_not_write_is_declined() {
    let wide = ring(8, 45.0, 4);
    let mut coded = wide.clone();
    coded.compact_values();
    assert_eq!(wide.wire_form(), PolarWireForm::Wide);
    assert_eq!(coded.wire_form(), PolarWireForm::Coded);

    for (name, good) in [("wide", wide.to_tail()), ("coded", coded.to_tail())] {
        for code in 2..=u8::MAX {
            let mut tail = good.clone();
            tail[0] = code;
            assert_eq!(
                PolarField::from_tail(&tail),
                None,
                "{name}: form {code} was read as a form this build writes",
            );
        }
    }
    assert_eq!(PolarField::from_tail(&[]), None, "an empty tail was read");
}

/// A coded tail this build did not write is declined rather than indexed
/// into — at every truncation, one byte long, and with a code naming a table
/// entry that is not there.
#[test]
fn a_coded_tail_this_build_did_not_write_is_declined_rather_than_indexed_into() {
    let mut coded = ring(5, 60.0, 4);
    coded.compact_values();
    let good = coded.to_tail();
    assert!(PolarField::from_tail(&good).is_some());

    for n in 0..good.len() {
        assert!(
            PolarField::from_tail(&good[..n]).is_none(),
            "a {n}-byte prefix was accepted"
        );
    }
    let mut long = good.clone();
    long.push(0);
    assert!(PolarField::from_tail(&long).is_none());

    // The last byte is a code; a table this field's plane cannot fill names
    // nothing, and indexing it would be out of bounds on the hover thread.
    let mut past_the_table = good.clone();
    *past_the_table.last_mut().expect("a coded tail has codes") = u8::MAX;
    assert!(
        PolarField::from_tail(&past_the_table).is_none(),
        "a code past the end of its own table was accepted",
    );

    // A table longer than a byte can address.
    let mut huge_table = good.clone();
    let table_at = 1 + 4 * 4 + 8 * 3 + 5 * 8;
    huge_table[table_at..table_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(PolarField::from_tail(&huge_table).is_none());
}

/// **The two NaNs stay apart across the wire, in the coded form.**
///
/// `compacting_keeps_the_two_nans_apart` says the compaction preserves them;
/// this says the *tail* does. `at` answers `None` for both the unpainted
/// marker and the range-folded sentinel, so the only way to tell them apart
/// is the bytes, and the assertion is made against the wide bytes of the
/// plane the coded one was built from.
#[test]
fn a_coded_tail_keeps_the_two_nans_apart() {
    let unpainted = f32::NAN;
    let folded = super::super::RANGE_FOLDED_SENTINEL;
    assert_ne!(
        unpainted.to_bits(),
        folded.to_bits(),
        "this test is vacuous unless the two sentinels differ",
    );

    let wide = field_of(4, 2, &[unpainted, folded, 3.0, -0.0, 0.0, 7.5, -2.0, 9.0]);
    let mut coded = wide.clone();
    coded.compact_values();
    assert_eq!(coded.wire_form(), PolarWireForm::Coded, "nothing was coded");

    let back = PolarField::from_tail(&coded.to_tail()).expect("its own tail");
    assert_eq!(
        back.to_bytes(),
        wide.to_bytes(),
        "a bit pattern came back as another one across the coded tail",
    );
}

/// The coded tail is a byte a gate plus its table, against four bytes a gate
/// — computed off the shape, not a figure recorded here.
#[test]
fn the_coded_tail_costs_a_byte_a_gate_and_the_table() {
    let numbers = every_kind_of_painted_number();
    let (radials, gates) = (8, 32);
    let wide = field_of(radials, gates, &numbers);
    let mut coded = wide.clone();
    coded.compact_values();

    let front = 1 + 4 * 4 + 8 * 3 + radials * 8;
    assert_eq!(wide.to_tail().len(), front + radials * gates * 4);
    assert_eq!(
        coded.to_tail().len(),
        front + 4 + numbers.len() * size_of::<f32>() + radials * gates,
        "a byte a gate, plus the table and its count",
    );
}

/// A plane too various to code crosses the wire wide, and says so.
#[test]
fn a_plane_that_cannot_be_coded_crosses_the_wire_wide() {
    let numbers: Vec<f32> = (0..257).map(|i| i as f32 * 0.25).collect();
    let mut f = field_of(257, 257, &numbers);
    f.compact_values();
    assert_eq!(f.wire_form(), PolarWireForm::Wide);
    let back = PolarField::from_tail(&f.to_tail()).expect("its own tail");
    assert_eq!(back.wire_form(), PolarWireForm::Wide);
    assert_eq!(every_gate(&back), every_gate(&f));
}

/// A stripped field — a loop frame's — crosses as the wide form with no
/// values, which is the form it holds.
#[test]
fn a_stripped_field_crosses_the_wire_with_no_numbers() {
    let mut f = ring(8, 45.0, 4);
    f.strip_values();
    let back = PolarField::from_tail(&f.to_tail()).expect("its own tail");
    assert!(!back.has_values());
    assert_eq!(back.geometry().wedges(), f.geometry().wedges());
}
