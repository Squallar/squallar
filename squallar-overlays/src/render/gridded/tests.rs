//! The registry answers for every field it registers, refuses every code it
//! does not, and its generic ramp is the one its visibility test describes.

use super::*;
use crate::hrrr::ModelParameter;

/// A sweep wide enough to cross every posture: raw GRIB2 values from deeply
/// negative (CIN, lifted index) through the model's largest (visibility in
/// metres, temperature in kelvin).
fn sweep() -> Vec<f32> {
    let mut values: Vec<f32> = Vec::new();
    let mut v = -1000.0f32;
    while v <= 20_000.0 {
        values.push(v);
        v += if v.abs() < 1000.0 { 0.25 } else { 7.0 };
    }
    values.extend([f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0]);
    values
}

/// The registry paints a model field exactly as the parameter itself does.
/// This is what licenses `rasterize_gridded` naming only a `FieldId`.
#[test]
fn every_model_field_paints_what_its_parameter_paints() {
    let sweep = sweep();
    assert!(sweep.len() > 5_000, "the sweep must be able to disagree");
    for &p in ModelParameter::all() {
        let paint = paint_for_code(p.as_str())
            .unwrap_or_else(|| panic!("{p:?} is registered under {:?}", p.as_str()));
        assert_eq!(paint.id.as_str(), p.as_str());
        for &v in &sweep {
            assert_eq!(
                paint.color_for_value(v),
                p.color_for_value(v),
                "{p:?} at {v}: the registry's ramp is not the parameter's",
            );
            assert_eq!(
                paint.paints(v),
                p.paints(v),
                "{p:?} at {v}: the registry's visibility test is not the \
                 parameter's",
            );
        }
    }
}

/// The refusal, from both doors. A code this build does not register resolves
/// to nothing rather than to some other field's colours.
#[test]
fn a_code_this_build_does_not_register_is_refused() {
    for code in ["sbcap", "SBCAPE", "", "mrms/reflectivity", "vis "] {
        assert!(
            paint_for_code(code).is_none(),
            "{code:?} resolved to a paint this build never registered",
        );
        assert!(field_scale(&FieldId::new(code)).is_none(), "{code:?}");
    }
    // Control: the lookup is not simply answering `None` to everything.
    assert!(paint_for_code("sbcape").is_some());
    assert!(
        field_scale(&FieldId::from_static("vis")).is_some(),
        "the scale door must resolve a registered code",
    );
}

fn a_scale(is_gradient: bool) -> LegendScale {
    LegendScale {
        thresholds: vec![
            (10.0, [0, 0, 0]),
            (20.0, [100, 100, 100]),
            (40.0, [200, 40, 0]),
        ],
        is_gradient,
        min_value: 10.0,
        max_value: 40.0,
    }
}

/// The generic ramp's three regions: nothing below the first stop, the stops'
/// own colours at the stops, and the last stop's colour held above it.
#[test]
fn the_generic_ramp_fades_out_below_and_clamps_above() {
    let scale = a_scale(true);
    assert_eq!(color_for(&scale, 9.9), [0, 0, 0, 0]);
    assert_eq!(color_for(&scale, f32::NAN), [0, 0, 0, 0]);
    assert_eq!(color_for(&scale, f32::NEG_INFINITY), [0, 0, 0, 0]);
    assert_eq!(color_for(&scale, f32::INFINITY), [0, 0, 0, 0]);

    assert_eq!(color_for(&scale, 10.0), [0, 0, 0, OPAQUE]);
    assert_eq!(color_for(&scale, 20.0), [100, 100, 100, OPAQUE]);
    assert_eq!(color_for(&scale, 40.0), [200, 40, 0, OPAQUE]);
    assert_eq!(
        color_for(&scale, 4_000.0),
        [200, 40, 0, OPAQUE],
        "above the last stop the ramp holds, it does not wrap or fade",
    );

    // Half way between the first two stops.
    assert_eq!(color_for(&scale, 15.0), [50, 50, 50, OPAQUE]);
}

/// `is_gradient: false` paints the band's own colour across the band rather
/// than interpolating into the next one.
#[test]
fn a_banded_scale_paints_flat_bands() {
    let banded = a_scale(false);
    let ramped = a_scale(true);
    assert_eq!(color_for(&banded, 15.0), [0, 0, 0, OPAQUE]);
    assert_eq!(color_for(&banded, 19.9), [0, 0, 0, OPAQUE]);
    assert_eq!(color_for(&banded, 20.0), [100, 100, 100, OPAQUE]);
    assert_ne!(
        color_for(&banded, 15.0),
        color_for(&ramped, 15.0),
        "the two flags produced the same picture, so the flag is not read",
    );
}

/// The cheap visibility test and the ramp itself agree everywhere, which is
/// what lets `summarize_values` ask the cheap one.
#[test]
fn the_generic_visibility_test_agrees_with_the_generic_ramp() {
    let mut agreed_both_ways = (false, false);
    for scale in [a_scale(true), a_scale(false)] {
        for &v in &sweep() {
            let painted = color_for(&scale, v)[3] != 0;
            assert_eq!(
                paints_over_scale(&scale, v),
                painted,
                "the short-circuit and the ramp disagree at {v}",
            );
            if painted {
                agreed_both_ways.0 = true;
            } else {
                agreed_both_ways.1 = true;
            }
        }
    }
    assert_eq!(
        agreed_both_ways,
        (true, true),
        "the sweep never produced both answers, so the agreement is vacuous",
    );
}

/// An empty colour bar paints nothing rather than indexing past its own end.
#[test]
fn a_scale_with_no_stops_paints_nothing() {
    let empty = LegendScale {
        thresholds: Vec::new(),
        is_gradient: true,
        min_value: 0.0,
        max_value: 1.0,
    };
    assert_eq!(color_for(&empty, 5.0), [0, 0, 0, 0]);
    assert!(!paints_over_scale(&empty, 5.0));
}

/// `over_scale` is the registration a source with no bespoke ramp uses, and it
/// is the generic pair — not a third behaviour.
#[test]
fn over_scale_registers_the_generic_pair() {
    static ID: FieldId = FieldId::from_static("test/field");
    static SCALE: LazyLock<LegendScale> = LazyLock::new(|| a_scale(true));
    let paint = FieldPaint::over_scale(&ID, &SCALE);
    for &v in &sweep() {
        assert_eq!(paint.color_for_value(v), color_for(&SCALE, v), "at {v}");
        assert_eq!(paint.paints(v), paints_over_scale(&SCALE, v), "at {v}");
    }
    assert_eq!(paint.id.as_str(), "test/field");
}

/// The refusal reaches the raster: a window naming a field this build does not
/// register paints **nothing**, and the identical window naming a registered
/// one paints, so the blank is the refusal rather than an empty fixture.
#[test]
fn a_raster_of_an_unregistered_field_paints_nothing() {
    use crate::render::rasterize::{GridWindow, GriddedInput, IndexWindow, rasterize_gridded};
    use squallar_geo::GeoBounds;

    let bounds = GeoBounds {
        min_lat: 34.9,
        max_lat: 35.2,
        min_lon: -97.2,
        max_lon: -96.9,
    };
    let window = |field: FieldId| {
        GriddedInput::Window(GridWindow {
            field,
            ni: 2,
            nj: 2,
            coords: crate::hrrr::GridCoords::Explicit {
                lats: vec![35.1, 35.1, 35.0, 35.0],
                lons: vec![-97.1, -97.0, -97.1, -97.0],
            },
            win: IndexWindow {
                i0: 0,
                i1: 2,
                j0: 0,
                j1: 2,
            },
            values: GridValues::F32(vec![4000.0; 4]),
        })
    };
    let painted = |input| {
        rasterize_gridded(&input, &bounds, 64, 64)
            .rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 0)
            .count()
    };
    assert!(
        painted(window(FieldId::from_static("sbcape"))) > 0,
        "control: the registered field must paint, or the zero below is the \
         fixture rather than the refusal",
    );
    assert_eq!(
        painted(window(FieldId::new("sbcape/v2"))),
        0,
        "a field this build does not register was painted anyway",
    );
}

// -- The byte store's own invariants ----------------------------------------

/// **An absent set the store cannot honour is refused, never accepted and
/// half-read.**
///
/// `ByteCodes::new` is the boundary a wire head crosses — `WireValues`
/// rebuilds a store from bytes another build wrote — so the three ways the set
/// can be wrong are checked here rather than assumed of the sender. An
/// unsorted list is the dangerous one: `value` reads it with `binary_search`,
/// so an out-of-order entry answers "present" for a missing point on some
/// samples and not others, which paints a hole as a reading with nothing
/// anywhere to say so.
#[test]
fn a_byte_store_refuses_an_absent_set_it_could_not_read_back() {
    let codes = || (0u8..16).collect::<Vec<u8>>();
    assert!(
        ByteCodes::new(codes(), vec![]).is_some(),
        "control: an empty absent set is the ordinary case",
    );
    assert!(
        ByteCodes::new(codes(), vec![0, 5, 15]).is_some(),
        "control: an ascending set inside the codes is honoured",
    );
    assert!(
        ByteCodes::new(codes(), vec![5, 0]).is_none(),
        "an unsorted set would be read with `binary_search` and answer wrongly",
    );
    assert!(
        ByteCodes::new(codes(), vec![5, 5]).is_none(),
        "a repeated index is not a set",
    );
    assert!(
        ByteCodes::new(codes(), vec![16]).is_none(),
        "an index past the codes names a point that is not in this grid",
    );
    assert!(
        ByteCodes::new(
            (0u8..=255).cycle().take(MAX_ABSENT_POINTS + 1).collect(),
            (0..=MAX_ABSENT_POINTS as u32).collect(),
        )
        .is_none(),
        "past the bound the store declines rather than making every sample \
         walk a longer list",
    );
}

/// **A byte store reads back as the bytes it holds, and prices itself as what
/// it holds.**
#[test]
fn a_byte_store_widens_exactly_and_prices_both_of_its_blocks() {
    let store = ByteCodes::new(vec![0, 1, 200, 255, 42], vec![2]).expect("a valid store");
    let values = GridValues::Bytes(store);
    assert_eq!(values.get(0).unwrap().to_bits(), 0.0f32.to_bits());
    assert_eq!(values.get(1).unwrap(), 1.0);
    assert!(
        values.get(2).unwrap().is_nan(),
        "the absent point is missing"
    );
    assert_eq!(values.get(3).unwrap(), 255.0);
    assert_eq!(values.get(4).unwrap(), 42.0);
    assert_eq!(values.get(5), None, "past the end there is nothing to read");

    // The iterator is the same answer as `get`, index for index — it walks by
    // index rather than by code precisely because the absent point's code is
    // an ordinary one.
    let walked: Vec<u32> = values.iter().map(|v| v.to_bits()).collect();
    let indexed: Vec<u32> = (0..values.len())
        .map(|k| values.get(k).unwrap().to_bits())
        .collect();
    assert_eq!(walked, indexed);
    assert_eq!(values.iter().len(), 5, "and it states its own length");

    assert_eq!(values.bytes_per_sample(), 1);
    assert_eq!(
        values.resident_bytes(),
        5 + size_of::<u32>(),
        "five codes and one absent index, both blocks priced",
    );
    assert_eq!(
        values.stored_bytes(),
        &[0, 1, 200, 255, 42],
        "and what the transport lends is the codes alone",
    );
}

// ── The tiled store ─────────────────────────────────────────────────────────

/// A plane whose tiles are a deliberate mix: whole tiles of one code (the two
/// sentinels a real mosaic is mostly made of), tiles with one point different,
/// and tiles where every point differs.
///
/// **Not a multiple of [`TILE`] on either axis.** The edge tiles are the shape
/// a padded slot has to get right, and a grid that divided evenly would leave
/// every padding read unreached.
fn mixed_plane(ni: usize, nj: usize) -> Vec<ScaledCode> {
    let mut plane = vec![0u16; ni * nj];
    for j in 0..nj {
        for i in 0..ni {
            let (ti, tj) = (i / TILE, j / TILE);
            plane[j * ni + i] = match (ti + tj) % 4 {
                // The no-coverage sentinel, whole tiles of it.
                0 => 0,
                // The no-echo sentinel.
                1 => 9000,
                // One point off an otherwise uniform tile.
                2 => {
                    if i % TILE == 3 && j % TILE == 5 {
                        12_345
                    } else {
                        9000
                    }
                }
                // Every point its own code.
                _ => ((j * ni + i) % 60_000) as u16,
            };
        }
    }
    plane
}

fn flat_of(plane: Vec<ScaledCode>) -> ScaledU16 {
    ScaledU16 {
        codes: plane,
        ref_val: -9990.0,
        two_pow: 1.0,
        dig_factor: 0.1,
        nan_codes: vec![0, 9000],
    }
}

fn tiled_of(plane: &[ScaledCode], ni: usize, nj: usize) -> TiledU16 {
    TiledU16::from_plane(plane, ni, nj, -9990.0, 1.0, 0.1, vec![0, 9000])
        .expect("a plane of the shape beside it tiles")
}

/// **Every point, bit for bit, against the flat store it replaces** — the whole
/// claim the representation is allowed to make.
///
/// `to_bits`, not `==`: the two sentinels read back as `NaN`, and `NaN == NaN`
/// is false, so an equality comparison would pass over a store that had lost
/// every reserved point and turned it into a different `NaN`.
#[test]
fn a_tiled_store_reads_back_every_point_of_a_plane_bit_for_bit() {
    let (ni, nj) = (101usize, 67usize);
    let plane = mixed_plane(ni, nj);
    let flat = flat_of(plane.clone());
    let tiled = tiled_of(&plane, ni, nj);

    assert_eq!(tiled.len(), ni * nj);
    let mut nans = 0usize;
    for k in 0..ni * nj {
        let here = flat.get(k).expect("inside the flat store");
        let there = tiled.get(k).expect("inside the tiled store");
        assert_eq!(
            here.to_bits(),
            there.to_bits(),
            "point {k} (i={}, j={}) reads {there} where the plane holds {here}",
            k % ni,
            k / ni,
        );
        if here.is_nan() {
            nans += 1;
        }
        // And by grid coordinates, which is the door the raster reads through.
        assert_eq!(
            tiled
                .get_grid(k % ni, k / ni)
                .expect("inside the grid")
                .to_bits(),
            here.to_bits(),
        );
    }
    assert!(
        nans > ni * nj / 4,
        "premise: the plane must really carry reserved codes, or the `to_bits` \
         comparison above never reached its NaN arm ({nans} of {})",
        ni * nj,
    );
    assert_eq!(tiled.get(ni * nj), None, "one past the end is not a point");
    assert_eq!(tiled.get_grid(ni, 0), None, "and neither is one past a row");
}

/// **The elision is where the bytes go**, and it is measured against the plane
/// rather than asserted as a ratio: a store that elided nothing would satisfy
/// every equality above.
#[test]
fn a_tiled_store_holds_only_the_tiles_that_carry_more_than_one_code() {
    let (ni, nj) = (101usize, 67usize);
    let plane = mixed_plane(ni, nj);
    let tiled = tiled_of(&plane, ni, nj);
    let flat = flat_of(plane.clone()).codes.len() * ScaledU16::ELEMENT_BYTES;
    assert!(
        tiled.resident_bytes() < flat,
        "{} is not under the {flat} B plane it replaces",
        tiled.resident_bytes(),
    );

    // A plane of one code anywhere is index, prefix sum and reserved codes and
    // nothing else — the shape a mosaic's ocean is made of.
    let empty = TiledU16::from_plane(&vec![0u16; ni * nj], ni, nj, -9990.0, 1.0, 0.1, vec![0])
        .expect("tiles");
    let tiles = ni.div_ceil(TILE) * nj.div_ceil(TILE);
    assert_eq!(
        empty.resident_bytes(),
        tiles * size_of::<u32>() + (nj.div_ceil(TILE) + 1) * size_of::<u32>() + 2,
        "a uniform plane must hold no arena at all",
    );
    assert_eq!(empty.arena().len(), 0);

    // **And the worst case is bounded.** Every tile distinct: the arena is the
    // plane rounded up to whole slots, and the overhead above it is the index.
    let all_distinct: Vec<u16> = (0..(ni * nj) as u32).map(|k| (k % 65_536) as u16).collect();
    let worst =
        TiledU16::from_plane(&all_distinct, ni, nj, -9990.0, 1.0, 0.1, vec![]).expect("tiles");
    assert_eq!(worst.arena().len(), tiles * TILE_CELLS);
    assert_eq!(
        worst.resident_bytes(),
        tiles * TILE_CELLS * TiledU16::ELEMENT_BYTES
            + tiles * size_of::<u32>()
            + (nj.div_ceil(TILE) + 1) * size_of::<u32>(),
        "the ceiling every budget stated in `CONUS_TILED_CEILING_BYTES` is \
         derived from — a granule may reach it and may not pass it",
    );
}

/// **A row band is one unbroken run of the arena**, which is the property the
/// wire's zero-copy lend rests on, and it rebuilds into a store that answers
/// the band's points in the grid's own numbering.
#[test]
fn a_row_band_is_contiguous_and_rebuilds_to_the_same_values() {
    let (ni, nj) = (101usize, 67usize);
    let plane = mixed_plane(ni, nj);
    let flat = flat_of(plane.clone());
    let tiled = tiled_of(&plane, ni, nj);

    // An INTERIOR band: one starting at row zero is the shape that masks a
    // rebasing error, because every slot number would already be right.
    let (j0, j1) = (20usize, 50usize);
    let (tj0, tj1, range) = tiled.band_for(j0, j1).expect("a band inside the grid");
    assert!(
        tj0 > 0,
        "premise: the band must not start at the first tile row"
    );
    assert!(!range.is_empty(), "premise: it must carry stored tiles");

    let band = TiledU16::from_band(
        ni,
        (tj1 - tj0) * TILE,
        tj0 * TILE,
        tiled.band_index(tj0, tj1),
        tiled.arena()[range].to_vec(),
        tiled.ref_val,
        tiled.two_pow,
        tiled.dig_factor,
        tiled.nan_codes.clone(),
    )
    .expect("the band the lend cut is the band the index names");

    for j in tj0 * TILE..(tj1 * TILE).min(nj) {
        for i in 0..ni {
            let here = flat.get(j * ni + i).expect("inside the plane");
            let there = band.get_grid(i, j).expect("inside the band");
            assert_eq!(
                here.to_bits(),
                there.to_bits(),
                "band point ({i}, {j}) reads {there} where the plane holds {here}",
            );
        }
    }
    assert_eq!(
        band.get_grid(0, tj0 * TILE - 1),
        None,
        "and the band answers nothing for a row it does not carry",
    );
}

/// **A band whose index does not describe the arena beside it is refused**, not
/// sampled at whatever tile the arithmetic lands on.
#[test]
fn a_band_index_that_does_not_match_its_arena_is_refused() {
    let (ni, nj) = (101usize, 67usize);
    let plane = mixed_plane(ni, nj);
    let tiled = tiled_of(&plane, ni, nj);
    let (tj0, tj1, range) = tiled.band_for(20, 50).expect("a band");
    let index = tiled.band_index(tj0, tj1);
    let arena = tiled.arena()[range].to_vec();
    let rows = (tj1 - tj0) * TILE;
    let build = |index: Vec<u32>, arena: Vec<u16>| {
        TiledU16::from_band(
            ni,
            rows,
            tj0 * TILE,
            index,
            arena,
            tiled.ref_val,
            tiled.two_pow,
            tiled.dig_factor,
            tiled.nan_codes.clone(),
        )
    };
    assert!(
        build(index.clone(), arena.clone()).is_some(),
        "premise: the untampered pair builds, so every refusal below is the \
         tamper and not the fixture",
    );

    // Two stored tiles swapped: the arena is the right length and every entry
    // is in range, so nothing but the ORDER is wrong — which is the failure a
    // lenient read turns into a band drawn from another band's rows.
    let mut swapped = index.clone();
    let stored: Vec<usize> = swapped
        .iter()
        .enumerate()
        .filter(|&(_, &e)| e & (1 << 31) == 0)
        .map(|(k, _)| k)
        .collect();
    assert!(stored.len() >= 2, "premise: two tiles to swap");
    swapped.swap(stored[0], stored[1]);
    assert!(build(swapped, arena.clone()).is_none());

    // An arena one slot short of what the index names.
    assert!(build(index.clone(), arena[..arena.len() - TILE_CELLS].to_vec()).is_none());
    // An arena that is not a whole number of slots.
    assert!(build(index.clone(), arena[..arena.len() - 1].to_vec()).is_none());
    // An index of the wrong length for the shape beside it.
    assert!(build(index[..index.len() - 1].to_vec(), arena).is_none());
}
