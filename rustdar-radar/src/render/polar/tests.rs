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

/// A field assembled by hand, for
/// [`the_polar_wire_layout_is_the_one_this_protocol_ships`].
///
/// Every number here is a literal and exactly representable, so the encoding is
/// the same bytes on every target: nothing in this fixture is computed, so
/// nothing in it can move with a libm. [`ring`] cannot serve — its azimuths
/// come out of a division and its values out of an arithmetic walk, and while
/// both happen to be exact today, a fixture whose numbers are *derived* is one
/// whose digest is a claim about the derivation as well as about the layout.
///
/// Written as a struct literal with no `..`, and reaching past
/// [`PolarGeometry::from_parts`] to the private fields, for two reasons that
/// are both about what the pin can see:
///
///   * **`from_parts` sets `reach_gates` equal to `gates`.** Those are two
///     separate `u32`s side by side in the header, and a fixture where they
///     hold the same number cannot tell a swap of the two apart from no change
///     at all — the pin would pass for the wrong reason, which is the failure
///     mode a digest is easiest to build wrong into. Here `gates` is 4 and
///     `reach_gates` is 2.
///   * **A new field on either struct makes this fail to compile**, so a field
///     that joins the type and is then written by `to_bytes` cannot reach the
///     wire without someone having already been stopped once.
///
/// All four header counts are distinct (3, 4, 2, 12) and all three header
/// `f64`s are distinct (0.125, 0.25, 0.5), so any reorder within either width
/// class moves the bytes. The six wedge `f32`s are likewise pairwise distinct,
/// so a swap of a wedge's azimuth and half-width is visible too.
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
        values: vec![
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
        ],
    }
}

/// The bytes this protocol ships are **these** bytes.
///
/// # What was blind, exactly
///
/// This encoding has no version of its own. What a page and a worker from
/// opposite sides of a deploy actually compare is `rustdar_web`'s
/// `build_token` — `GITHUB_SHA` in CI, the `rustdar_frontend::wire_identity`
/// framing-rows digest locally — and the guards standing over that boundary
/// are all blind here:
///
///   * `the_worker_protocol_is_versionless_and_the_token_names_the_build`
///     pins that the deleted hand-kept version stays deleted and that
///     `build_token` reads both of its halves; it says nothing about any
///     payload's bytes.
///   * `the_worker_reply_shape_is_the_one_this_build_ships`
///     scrapes the reply's **field names**. These bytes travel inside one of
///     those fields (`polar`), so the field set is identical either side of a
///     change here and that guard cannot see one.
///   * The local token digests the *framing rows* — the request and reply
///     layouts `rustdar_frontend`'s own tests pin — and deliberately not the
///     nested payloads inside the reply's fields, so a change here moves no
///     token either.
///
/// Measured, not argued: this header once grew an `f64` (the elevation) and
/// restated its two ranges as slant rather than ground, and **no guard
/// fired** — the hand-kept protocol number of that era was bumped by hand. A
/// buffer-valued field was where the author was entirely on their own; this
/// is the instrument that ends that.
///
/// # What this fails for
///
/// The encoder's own output, over a fixture nothing computes, compared with the
/// bytes recorded when the protocol version was last set. Any change to what
/// `to_bytes` writes — a field added, removed, reordered, retyped, or written
/// at a different width or endianness — moves the length or the digest, and the
/// only way past it is to write the new numbers down. That is where the bump
/// obligation is met.
///
/// # What it still cannot do
///
/// It cannot make a **local** page/worker pair differing only in these bytes
/// refuse each other: the local token deliberately folds the framing rows
/// and not the nested payloads — `rustdar_frontend::wire_identity` records
/// that accepted residual, and deployed pairs always differ by `GITHUB_SHA`
/// and refuse at the handshake. What it can do is fail for the person who
/// changes the layout, and say in the message what they owe. That is the
/// direction that was missing.
///
/// A digest is opaque about *what* moved, deliberately: the two assertions are
/// one tuple so the failure reads as one fact, and what to do about it does not
/// depend on which field it was.
#[test]
fn the_polar_wire_layout_is_the_one_this_protocol_ships() {
    let bytes = layout_fixture().to_bytes();
    assert_eq!(
        (bytes.len(), crate::wire::layout_digest(&bytes)),
        (112, 0x986a_92ef_b56e_c209),
        "the bytes `PolarField::to_bytes` writes are not the bytes this pin \
         was last told. Something about this payload's layout moved — a \
         field added, removed, reordered, retyped, or written at a different \
         width. This encoding carries no version of its own, and nothing \
         else in the workspace can see a change to these bytes — the \
         reply-shape guard watches field names and these travel inside one \
         of them, and the build token's local digest folds the framing rows \
         and not this nested payload. If the change was deliberate, re-pin \
         the length and digest here, deliberately. Deployed pages and \
         workers from opposite sides of a deploy refuse each other by \
         GITHUB_SHA at the HELLO handshake; a LOCAL pair differing only \
         here still attaches, and `from_bytes`'s length checks turn most \
         such pairs into `None` and a readout that goes quiet — the \
         accepted residual `rustdar_frontend::wire_identity` records, until \
         full layout identity joins the token.",
    );
}
