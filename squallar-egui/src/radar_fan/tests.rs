//! **What a payload has to say about itself before anything indexes it.**
//!
//! [`FanSweep::is_well_formed`] is a transport check and not a second opinion
//! on fidelity: the producer already refuses a plane it cannot build, and what
//! these hold is that what *arrived* describes itself consistently. Every arm
//! is a real indexing hazard — a level whose bytes are not there, a table that
//! is not a table, an edge list that does not cover the radials — and each is
//! asserted against a payload that is healthy in every other respect, so a
//! check that started passing for the wrong reason shows up as the healthy
//! control going red.

use super::*;

fn geometry() -> FanGeometry {
    FanGeometry {
        site_lat: 35.33,
        site_lon: -97.28,
        first_gate_slant_km: 2.125,
        gate_interval_slant_km: 0.25,
        elevation_deg: Some(0.5),
        reach_gates: 4,
        reach_km: 3.0,
        earth_radius_km: 6371.0,
        effective_radius_km: 6371.0 * 4.0 / 3.0,
    }
}

/// A well-formed one-level payload: eight radials of four gates, no chain.
///
/// One level is the shape a categorical field really ships in — its codes are
/// ordinally meaningless and must never be reduced — so this is not a
/// degenerate fixture standing in for a real one.
fn one_level() -> FanSweep {
    FanSweep {
        field: squallar_radar::fields::known::HYDROMETEOR_CLASSIFICATION,
        radials: 8,
        gates: 4,
        codes: (0..32u32).map(|i| i as u8).collect(),
        level_offsets: vec![0],
        lut_rgba: vec![7; LUT_BYTES],
        edges: (0..8)
            .map(|i| [i as f32 * 45.0, (i + 1) as f32 * 45.0])
            .collect(),
        geometry: geometry(),
    }
}

/// The same shape with a full chain: `8x4 -> 4x2 -> 2x1 -> 1x1`, which is four
/// levels and 32 + 8 + 2 + 1 = 43 bytes.
fn chained() -> FanSweep {
    FanSweep {
        codes: (0..43u32).map(|i| i as u8).collect(),
        level_offsets: vec![0, 32, 40, 42],
        ..one_level()
    }
}

#[test]
fn a_healthy_payload_is_well_formed_in_both_shapes() {
    assert!(one_level().is_well_formed());
    assert!(chained().is_well_formed());
}

#[test]
fn the_chain_ceil_halves_both_axes_and_ends_at_one_cell() {
    let sweep = chained();
    assert_eq!(sweep.levels(), 4);
    let shapes: Vec<_> = (0..sweep.levels())
        .map(|l| sweep.level_shape(l).expect("inside the chain"))
        .collect();
    assert_eq!(shapes, vec![(8, 4), (4, 2), (2, 1), (1, 1)]);
    assert_eq!(sweep.level_shape(4), None);
    // And each level's slice is the length its own shape asks for, read
    // through the offsets rather than recomputed from them.
    for (level, (r, g)) in shapes.iter().enumerate() {
        assert_eq!(
            sweep.level(level).map(<[u8]>::len),
            Some((r * g) as usize),
            "level {level}"
        );
    }
}

/// A shape whose halving is **ragged** — 5 and 3 both leave an odd cell — so
/// the offsets are not power-of-two arithmetic that a wrong rule would agree
/// with by accident.
#[test]
fn a_ragged_chain_is_well_formed_at_the_lengths_ceil_halving_gives() {
    // 5x3 -> 3x2 -> 2x1 -> 1x1 = 15 + 6 + 2 + 1 = 24.
    let sweep = FanSweep {
        radials: 5,
        gates: 3,
        codes: vec![1; 24],
        level_offsets: vec![0, 15, 21, 23],
        edges: vec![[0.0, 72.0]; 5],
        geometry: FanGeometry {
            reach_gates: 3,
            ..geometry()
        },
        ..one_level()
    };
    assert!(sweep.is_well_formed());
    assert_eq!(
        (0..4)
            .map(|l| sweep.level_shape(l).expect("inside the chain"))
            .collect::<Vec<_>>(),
        vec![(5, 3), (3, 2), (2, 1), (1, 1)]
    );
}

/// Each defect on its own, against the healthy payload that differs from it by
/// exactly that one field.
#[test]
fn every_malformation_is_refused_one_at_a_time() {
    let cases: Vec<(&str, FanSweep)> = vec![
        (
            "no radials",
            FanSweep {
                radials: 0,
                ..one_level()
            },
        ),
        (
            "no gates",
            FanSweep {
                gates: 0,
                ..one_level()
            },
        ),
        (
            "no levels at all",
            FanSweep {
                level_offsets: Vec::new(),
                ..one_level()
            },
        ),
        (
            "level 0 not starting at 0",
            FanSweep {
                codes: vec![0; 33],
                level_offsets: vec![1],
                ..one_level()
            },
        ),
        (
            "a table that is not 256 entries",
            FanSweep {
                lut_rgba: vec![0; LUT_BYTES - 4],
                ..one_level()
            },
        ),
        (
            "one edge short of the radials",
            FanSweep {
                edges: vec![[0.0, 45.0]; 7],
                ..one_level()
            },
        ),
        (
            "a reach past the plane's own width",
            FanSweep {
                geometry: FanGeometry {
                    reach_gates: 5,
                    ..geometry()
                },
                ..one_level()
            },
        ),
        (
            "a reach of nothing",
            FanSweep {
                geometry: FanGeometry {
                    reach_gates: 0,
                    ..geometry()
                },
                ..one_level()
            },
        ),
        (
            "codes short of the level it declares",
            FanSweep {
                codes: vec![0; 31],
                ..one_level()
            },
        ),
        (
            "codes past the levels it declares",
            FanSweep {
                codes: vec![0; 33],
                ..one_level()
            },
        ),
        (
            "a chain whose second level overlaps the first",
            FanSweep {
                level_offsets: vec![0, 31, 40, 42],
                ..chained()
            },
        ),
        (
            "a chain with a gap between two levels",
            FanSweep {
                codes: vec![0; 44],
                level_offsets: vec![0, 33, 41, 43],
                ..chained()
            },
        ),
    ];
    for (why, sweep) in cases {
        assert!(!sweep.is_well_formed(), "accepted a payload with {why}");
    }
}

/// The one figure a budget would read, and it is read off the vectors.
#[test]
fn resident_bytes_is_the_buffers_and_not_the_shape() {
    let sweep = chained();
    assert_eq!(
        sweep.resident_bytes(),
        43 + LUT_BYTES + 8 * size_of::<[f32; 2]>()
    );
    // A payload whose codes are longer than its shape says still reports what
    // it is holding: this measures the object, so it cannot be a second
    // spelling of a price computed from `radials x gates`.
    let fat = FanSweep {
        codes: vec![0; 1_000],
        ..chained()
    };
    assert_eq!(
        fat.resident_bytes(),
        1_000 + LUT_BYTES + 8 * size_of::<[f32; 2]>()
    );
}

/// A level past the chain, and a level inside it whose bytes were truncated,
/// both answer `None` rather than a short slice.
#[test]
fn a_level_that_is_not_there_is_none_and_never_a_short_slice() {
    let sweep = chained();
    assert_eq!(sweep.level(4), None);
    let truncated = FanSweep {
        codes: vec![0; 42],
        ..chained()
    };
    assert_eq!(truncated.level(3), None);
    assert!(!truncated.is_well_formed());
}

/// The refusal arms add up to the draws, whatever order they arrived in — the
/// identity these figures are only meaningful under.
#[test]
fn the_ledger_totals_name_their_own_denominator() {
    let totals = ledger::Totals {
        draws: 9,
        painted: 4,
        no_painter: 2,
        malformed: 1,
        floor_strip: 1,
        declined: 1,
    };
    assert_eq!(totals.painted + totals.refused(), totals.draws);
}
