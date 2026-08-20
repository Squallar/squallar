use super::*;

const ANCHOR: (f64, f64) = (35.3331, -97.2778);

fn dims() -> VolumeDims {
    VolumeDims {
        nx: 4,
        ny: 3,
        nz: 2,
    }
}

/// A table whose alpha ramp has a transparent run at the bottom and a
/// see-through tail, so both facts it measures have a value other than 0 or
/// 255.
fn table(transparent_run: usize, see_through_tail: usize) -> TransferTable {
    let mut lut = vec![0u8; LUT_LEN];
    for (i, entry) in lut.chunks_exact_mut(4).enumerate() {
        entry[0] = i as u8;
        entry[3] = if i < transparent_run {
            0
        } else if i >= 256 - see_through_tail {
            SEE_THROUGH_ALPHA_CEILING
        } else {
            255
        };
    }
    TransferTable::new(
        lut,
        LutFilter::Nearest,
        false,
        (-32.0, 96.0),
        IsoShape::Sequential,
        18.0,
    )
}

fn parts(x_range_km: (f64, f64), y_range_km: (f64, f64)) -> VolumeParts {
    let dims = dims();
    VolumeParts {
        indices: vec![NO_DATA_INDEX; dims.cells()],
        values: None,
        dims,
        anchor: ANCHOR,
        x_range_km,
        y_range_km,
        z_range_km_msl: (0.125, 18.375),
        field: FieldId::from_static("Reflectivity"),
        transfer: table(3, 7),
        levels: 5,
        widest_level_gap_deg: 1.25,
    }
}

/// The storage rule, as bits rather than as prose: what a builder hands over
/// is what the grid reports, to the last bit of every f64.
#[test]
fn the_stored_placement_is_the_numbers_it_was_built_from() {
    // Values chosen so that no arithmetic could produce them by accident and
    // every one has a long mantissa.
    let x = (-233.719_482_713_9_f64, 191.004_517_286_1);
    let y = (-104.286_913_477_2_f64, 320.437_086_522_8);
    let grid = VolumeGrid::from_parts(parts(x, y));

    for (name, got, want) in [
        ("x.0", grid.x_range_km().0, x.0),
        ("x.1", grid.x_range_km().1, x.1),
        ("y.0", grid.y_range_km().0, y.0),
        ("y.1", grid.y_range_km().1, y.1),
        ("anchor.lat", grid.anchor().0, ANCHOR.0),
        ("anchor.lon", grid.anchor().1, ANCHOR.1),
        ("floor", grid.floor_km(), 0.125),
        ("ceil", grid.ceil_km(), 18.375),
    ] {
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "{name}: {got} is not the bit pattern {want} it was built with",
        );
    }
    assert_eq!(grid.z_range_km_msl(), (grid.floor_km(), grid.ceil_km()));
    assert_eq!(grid.cells(), dims().cells());
    assert_eq!(grid.cells(), 24, "the fixture must have cells to index");
}

/// Why [`VolumeGrid::footprint`] is derived and the kilometres are stored:
/// the geographic round trip does not come back with the number it left with.
///
/// This is the measurement the storage rule rests on. If it ever reads
/// bit-identical for every box, the rule has lost its reason and should be
/// re-argued rather than re-asserted.
#[test]
fn a_geographic_round_trip_does_not_return_the_kilometres_it_started_from() {
    let boxes = [
        ((-230.0_f64, 230.0_f64), (-230.0_f64, 230.0_f64)),
        ((-20.5, 19.5), (-20.5, 19.5)),
        ((-233.719_482_713_9, 191.004_517_286_1), (-104.25, 320.5)),
    ];
    let mut drifted = 0usize;
    let mut worst_m = 0.0_f64;
    for (x, y) in boxes {
        let grid = VolumeGrid::from_parts(parts(x, y));
        for (x_km, y_km) in [(x.0, y.0), (x.1, y.1), (x.0, y.1), (x.1, y.0)] {
            let bearing_deg = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);
            let (lat, lon) = rustdar_geo::great_circle_destination(
                grid.anchor().0,
                grid.anchor().1,
                bearing_deg,
                x_km.hypot(y_km),
            );
            let (back_bearing, back_range) =
                rustdar_geo::site_bearing_range_km(grid.anchor().0, grid.anchor().1, lat, lon);
            let bearing = back_bearing.to_radians();
            let (rx, ry) = (back_range * bearing.sin(), back_range * bearing.cos());
            if rx.to_bits() != x_km.to_bits() || ry.to_bits() != y_km.to_bits() {
                drifted += 1;
            }
            worst_m = worst_m.max(((rx - x_km).hypot(ry - y_km) * 1000.0).abs());
        }
    }
    assert_eq!(
        drifted, 12,
        "every one of the twelve corners must come back on a different bit \
         pattern, or the storage rule's stated reason is not the true one \
         (worst displacement {worst_m:.6} m)",
    );
    assert!(
        worst_m > 0.0,
        "the drift is real, not just a different spelling of the same number",
    );
}

/// The footprint bounds the box's whole curved perimeter, not just its four
/// corners — and the corners alone provably do not.
#[test]
fn a_footprint_covers_the_whole_curved_perimeter() {
    let x = (-325.0_f64, 325.0_f64);
    let y = (-325.0_f64, 325.0_f64);
    let grid = VolumeGrid::from_parts(parts(x, y));
    let footprint = grid.footprint();

    let at = |x_km: f64, y_km: f64| {
        let bearing_deg = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);
        rustdar_geo::great_circle_destination(ANCHOR.0, ANCHOR.1, bearing_deg, x_km.hypot(y_km))
    };

    // An independent, denser walk than the implementation's own: 401 points
    // per edge against its 65, so 3 of every 4 check points are not on the
    // lattice the bounds were built from.
    let mut outside = Vec::new();
    let mut bulge_deg = 0.0_f64;
    let corners = [at(x.0, y.0), at(x.1, y.0), at(x.0, y.1), at(x.1, y.1)];
    let corner_max_lat = corners.iter().fold(f64::MIN, |m, p| m.max(p.0));
    for i in 0..=400 {
        let t = f64::from(i) / 400.0;
        let px = x.0 + (x.1 - x.0) * t;
        let py = y.0 + (y.1 - y.0) * t;
        for (lat, lon) in [at(px, y.0), at(px, y.1), at(x.0, py), at(x.1, py)] {
            if !footprint.contains_point(lat, lon) {
                outside.push((lat, lon));
            }
            bulge_deg = bulge_deg.max(lat - corner_max_lat);
        }
    }
    assert_eq!(
        outside.len(),
        0,
        "{} sampled perimeter points fall outside the footprint: {:?}",
        outside.len(),
        &outside[..outside.len().min(3)],
    );

    // The non-vacuity leg: a corners-only footprint would MISS the north
    // edge's bulge, so sampling the perimeter is load-bearing rather than
    // decorative.
    // Measured on this fixture — a 650 km box at 35.33 N — the north edge
    // rises 0.0562 deg (6.2 km) above its own corners.
    assert!(
        bulge_deg > 0.05,
        "the curved edge only bulges {bulge_deg} deg past its corners, so \
         this fixture cannot tell perimeter sampling from corner sampling",
    );
    let corners_only = GeoBounds::from_points(corners).expect("four corners are four points");
    assert!(
        footprint.max_lat > corners_only.max_lat,
        "the footprint must reach past the corner box's north edge: \
         {} vs {}",
        footprint.max_lat,
        corners_only.max_lat,
    );

    // And it is a summary, not a second placement: the anchor it was derived
    // from is inside it, and the kilometres are untouched.
    assert!(footprint.contains_point(ANCHOR.0, ANCHOR.1));
    assert_eq!(grid.x_range_km().0.to_bits(), x.0.to_bits());
}

/// The two facts the table measures off its own bytes are measured, not
/// guessed — and they move when the bytes move.
#[test]
fn the_transfer_table_measures_the_bytes_it_was_given() {
    let t = table(3, 7);
    assert_eq!(
        t.fade_band(),
        2,
        "three transparent entries at the bottom means a band of two",
    );
    assert_eq!(
        t.see_through_indices(),
        9,
        "the two remaining bottom entries plus the seven-entry tail",
    );

    let wider = table(9, 20);
    assert_eq!(wider.fade_band(), 8);
    assert_eq!(wider.see_through_indices(), 28);
    assert_ne!(
        t.fade_band(),
        wider.fade_band(),
        "a different table must not read the same",
    );

    // The degenerate ends, both of which have their own arm.
    let opaque = TransferTable::new(
        vec![255u8; LUT_LEN],
        LutFilter::Linear,
        true,
        (0.0, 1.0),
        IsoShape::AtOrBelow,
        0.9,
    );
    assert_eq!(opaque.fade_band(), 0);
    assert_eq!(opaque.see_through_indices(), 0);
    let clear = TransferTable::new(
        vec![0u8; LUT_LEN],
        LutFilter::Linear,
        false,
        (0.0, 1.0),
        IsoShape::Sequential,
        0.5,
    );
    assert_eq!(
        clear.fade_band(),
        u8::MAX,
        "a table with no opaque entry anywhere fades the whole ramp",
    );
    assert_eq!(clear.see_through_indices(), 255);
}

/// A grid's value plane holds `NaN` where nothing was measured, so equality
/// compares it bitwise — and a derived `PartialEq` would make such a grid
/// unequal to itself.
#[test]
fn equality_compares_the_value_plane_bitwise() {
    let with_nan = |first: f32| {
        let d = dims();
        let mut values = vec![f32::NAN; d.cells()];
        values[0] = first;
        VolumeGrid::from_parts(VolumeParts {
            values: Some(values),
            ..parts((-10.0, 10.0), (-10.0, 10.0))
        })
    };
    let a = with_nan(f32::NAN);
    assert_eq!(a, with_nan(f32::NAN), "a NaN plane must equal itself");
    assert_ne!(a, with_nan(1.5), "a changed cell must not compare equal");
    assert_ne!(
        a,
        VolumeGrid::from_parts(parts((-10.0, 10.0), (-10.0, 10.0))),
        "a grid with no value plane is not a grid with one",
    );
    // The control that the first assertion is not passing because everything
    // compares equal.
    assert_ne!(
        a,
        VolumeGrid::from_parts(VolumeParts {
            values: Some(vec![f32::NAN; dims().cells()]),
            levels: 6,
            ..parts((-10.0, 10.0), (-10.0, 10.0))
        }),
        "a different level count must not compare equal",
    );
}

/// Cell addressing and the memory figure are the grid's own arithmetic.
#[test]
fn a_cell_is_addressed_by_the_plane_order_the_wire_writes() {
    let d = dims();
    let mut indices = vec![NO_DATA_INDEX; d.cells()];
    // (x, y, z) = (2, 1, 1) at z·(ny·nx) + y·nx + x = 1·12 + 1·4 + 2 = 18.
    indices[18] = 77;
    let grid = VolumeGrid::from_parts(VolumeParts {
        indices,
        ..parts((-10.0, 10.0), (-6.0, 6.0))
    });
    assert_eq!(grid.cell_offset(2, 1, 1), Some(18));
    assert_eq!(grid.index_at(2, 1, 1), Some(77));
    assert_eq!(grid.index_at(0, 0, 0), Some(NO_DATA_INDEX));
    assert_eq!(grid.cell_offset(4, 0, 0), None, "x is out of range");
    assert_eq!(grid.index_at(0, 3, 0), None, "y is out of range");
    assert_eq!(grid.value_at(2, 1, 1), None, "no value plane was kept");

    let (cx, cy, cz) = grid.cell_centre_km(0, 0, 0).expect("cell (0,0,0) exists");
    assert!((cx - -7.5).abs() < 1e-12, "{cx}");
    assert!((cy - -4.0).abs() < 1e-12, "{cy}");
    assert!((cz - 4.6875).abs() < 1e-12, "{cz}");

    assert_eq!(
        grid.memory_bytes(),
        d.cells() + LUT_LEN,
        "indices plus table, with no value plane",
    );
}

/// The dims bound is the wire's bound: an axis of zero and an axis past
/// [`MAX_AXIS`] are both unsupported, and a supported box always has a cell.
#[test]
fn a_supported_dims_triple_has_a_cell_and_fits_the_wire() {
    let smallest = VolumeDims {
        nx: 1,
        ny: 1,
        nz: 1,
    };
    assert!(smallest.is_supported());
    assert_eq!(smallest.cells(), 1);
    assert!(dims().is_supported());
    for bad in [
        VolumeDims { nx: 0, ..smallest },
        VolumeDims { ny: 0, ..smallest },
        VolumeDims { nz: 0, ..smallest },
        VolumeDims {
            nx: MAX_AXIS + 1,
            ..smallest
        },
    ] {
        assert!(!bad.is_supported(), "{bad:?}");
    }
    assert!(
        MAX_AXIS * MAX_AXIS * MAX_AXIS <= u32::MAX as usize,
        "MAX_AXIS cubed must fit a u32: {MAX_AXIS}",
    );
    assert!(
        (MAX_AXIS + 1).pow(3) > u32::MAX as usize,
        "MAX_AXIS must be the LARGEST such axis, not merely a safe one",
    );
}

/// The isosurface mapping reads the table's own shape, not a caller's guess,
/// and a non-finite threshold falls back to the registered default.
#[test]
fn the_iso_mapping_reads_the_tables_own_shape() {
    let sequential = table(3, 7);
    let (centre, threshold) = sequential.iso_uniform_params(18.0);
    assert_eq!(centre, -1.0, "a sequential surface has no centre");
    assert_eq!(
        threshold,
        f32::from(sequential.value_to_index(18.0)) / 255.0
    );
    assert_eq!(
        sequential.iso_uniform_params(f32::NAN),
        sequential.iso_uniform_params(18.0),
        "a non-finite threshold falls back to the stored default of 18.0",
    );

    let diverging = TransferTable::new(
        vec![0u8; LUT_LEN],
        LutFilter::Linear,
        false,
        (-64.0, 64.0),
        IsoShape::DeviationFrom { centre: 0.25 },
        4.0,
    );
    let (c, t) = diverging.iso_uniform_params(4.0);
    assert_eq!(c, f32::from(diverging.value_to_index(0.25)) / 255.0);
    assert!(t > 0.0, "a deviation surface must have a width: {t}");
    assert_ne!(
        (c, t),
        sequential.iso_uniform_params(4.0),
        "the two shapes must not answer alike",
    );
}
