//! **What a payload has to say about itself before anything indexes it.**
//!
//! [`FanSweep::is_well_formed`] is a transport check and not a second opinion
//! on fidelity: the producer already refuses a plane it cannot build, and what
//! these hold is that what *arrived* describes itself consistently. Every arm
//! is a real indexing hazard — a level whose bytes are not there, a table that
//! is not a table, an edge list that does not cover the radials.
//!
//! [`every_malformation_is_refused_one_at_a_time`] asserts only that each
//! payload is refused, which is the property that matters and is **not** the
//! same as saying which guard refused it. Some of these defects trip more than
//! one, and a fixture that trips two proves nothing about either.
//! [`each_guard_is_the_sole_reason_some_payload_is_refused`] is the one that
//! pins that down: one payload per guard, healthy in every other respect, so a
//! guard deleted from `is_well_formed` cannot go unnoticed.

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
        // Gate 0.s near edge in GROUND range: 2.125 - 0.125 slant, which at
        // half a degree is 2.0 km to well past the digits a fixture states.
        // Its distance from `reach_km` over `reach_gates` is 0.25 km, the
        // same gate the slant figures declare — so the fixture agrees with
        // itself about how deep a gate is.
        first_gate_km: 2.0,
        // The tree's own two, not a second spelling of them: a fixture that
        // wrote the numbers out would be exactly the second definition
        // `geodesy_one_definition.rs` exists to refuse, and carrying them as
        // data is the whole reason `FanGeometry` has these fields at all.
        earth_radius_km: squallar_geo::EARTH_RADIUS_KM,
        effective_radius_km: squallar_radar::beam::RE_EFF_KM,
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
        codes: std::sync::Arc::new((0..32u32).map(|i| i as u8).collect()),
        level_offsets: vec![0],
        lut_rgba: vec![7; LUT_BYTES],
        value_table: std::sync::Arc::new(vec![0.0; LUT_ENTRIES]),
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
        codes: std::sync::Arc::new((0..43u32).map(|i| i as u8).collect()),
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
fn the_chain_halves_both_axes_and_ends_at_one_cell() {
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
///
/// It is the shape that separates the two rules, and it was pinned to the
/// wrong one until 2026-09-08: `ceil` gives 5 → 3 → 2 → 1 and four levels,
/// where a texture of that size holds `max(1, extent >> level)` and admits
/// three. A payload built the other way is one `create_texture` refuses.
#[test]
fn a_ragged_chain_is_well_formed_at_the_lengths_the_texture_gives() {
    // 5x3 -> 2x1 -> 1x1 = 15 + 2 + 1 = 18, and three levels because
    // floor(log2 5) + 1 is 3.
    let sweep = FanSweep {
        radials: 5,
        gates: 3,
        codes: std::sync::Arc::new(vec![1; 18]),
        level_offsets: vec![0, 15, 17],
        edges: vec![[0.0, 72.0]; 5],
        geometry: FanGeometry {
            reach_gates: 3,
            ..geometry()
        },
        ..one_level()
    };
    assert!(sweep.is_well_formed());
    assert_eq!(
        (0..3)
            .map(|l| sweep.level_shape(l).expect("inside the chain"))
            .collect::<Vec<_>>(),
        vec![(5, 3), (2, 1), (1, 1)]
    );
    assert_eq!(sweep.level_shape(3), None);
}

/// Each defect on its own, against the healthy payload that differs from it by
/// exactly that one field.
#[test]
fn every_malformation_is_refused_one_at_a_time() {
    let cases: Vec<(&str, FanSweep)> = vec![
        ("no radials", no_radials()),
        (
            // Refused by the reach guard rather than by a stride check of its
            // own — `is_well_formed` says why there is no such check to write.
            "no gates",
            FanSweep {
                gates: 0,
                ..one_level()
            },
        ),
        ("no levels at all", no_levels()),
        (
            "level 0 not starting at 0",
            FanSweep {
                codes: std::sync::Arc::new(vec![0; 33]),
                level_offsets: vec![1],
                ..one_level()
            },
        ),
        (
            "a table one entry short of 256",
            FanSweep {
                lut_rgba: vec![0; LUT_BYTES - LUT_ENTRY_BYTES],
                ..one_level()
            },
        ),
        (
            // The long side too, because the check is an equality and a
            // producer that baked a *bigger* table is the two constants
            // disagreeing about how many colours a code can address — which
            // the short side alone would not catch.
            "a table one entry past 256",
            FanSweep {
                lut_rgba: vec![0; LUT_BYTES + LUT_ENTRY_BYTES],
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
                codes: std::sync::Arc::new(vec![0; 31]),
                ..one_level()
            },
        ),
        (
            "codes past the levels it declares",
            FanSweep {
                codes: std::sync::Arc::new(vec![0; 33]),
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
            // The codes are the length the *levels* add up to, so the total
            // still balances and only the in-order walk can see the gap. A
            // payload one byte longer would be refused by the length check
            // instead, and would say nothing about this one.
            "a chain with a gap between two levels",
            FanSweep {
                codes: std::sync::Arc::new(vec![0; 43]),
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
///
/// **The value table is a term here since 2026-09-10**, when the readout began
/// answering out of the picture's own plane and the payload started carrying
/// what the codes decode to. It is a real 1,024 B a sweep and this is the only
/// family that charges it — `squallar_radar::hover::CodedGates::resident_bytes`
/// answers 0 precisely because this figure does not.
///
/// Every term is still the extent of a buffer this object holds rather than a
/// number recorded here, which is the property the name states.
#[test]
fn resident_bytes_is_the_buffers_and_not_the_shape() {
    let sweep = chained();
    assert_eq!(
        sweep.resident_bytes(),
        43 + LUT_BYTES + 8 * size_of::<[f32; 2]>() + LUT_ENTRIES * size_of::<f32>()
    );
    // A payload whose codes are longer than its shape says still reports what
    // it is holding: this measures the object, so it cannot be a second
    // spelling of a price computed from `radials x gates`.
    let fat = FanSweep {
        codes: std::sync::Arc::new(vec![0; 1_000]),
        ..chained()
    };
    assert_eq!(
        fat.resident_bytes(),
        1_000 + LUT_BYTES + 8 * size_of::<[f32; 2]>() + LUT_ENTRIES * size_of::<f32>()
    );
}

/// A level past the chain, and a level inside it whose bytes were truncated,
/// both answer `None` rather than a short slice.
#[test]
fn a_level_that_is_not_there_is_none_and_never_a_short_slice() {
    let sweep = chained();
    assert_eq!(sweep.level(4), None);
    let truncated = FanSweep {
        codes: std::sync::Arc::new(vec![0; 42]),
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

/// A payload with **no radials** and nothing else wrong: no edges to cover
/// them, and no codes, because every level of a zero-radial plane is zero
/// cells. Every other guard passes on it, so `radials == 0` is the only thing
/// left to refuse it.
fn no_radials() -> FanSweep {
    FanSweep {
        radials: 0,
        edges: Vec::new(),
        codes: std::sync::Arc::new(Vec::new()),
        ..one_level()
    }
}

/// A payload declaring **no levels** and nothing else wrong. `want` never
/// accumulates, so its codes must be empty for the length check to pass — and
/// with that done, `level_offsets.is_empty()` is the only guard left.
fn no_levels() -> FanSweep {
    FanSweep {
        level_offsets: Vec::new(),
        codes: std::sync::Arc::new(Vec::new()),
        ..one_level()
    }
}

/// **Every guard is load-bearing on its own.**
///
/// For each, a payload healthy in every other respect: refused, and refused
/// *because of that guard*, since nothing else about it is wrong. The second
/// half of each row is the proof of the first — repair the one field and the
/// payload is well-formed, so the refusal was the guard's and not the
/// fixture's.
///
/// `gates == 0` has no row because it can have none: a zero stride forces the
/// reach guard to fire, and `is_well_formed` records that rather than carrying
/// a check no input can uniquely trip.
#[test]
fn each_guard_is_the_sole_reason_some_payload_is_refused() {
    let rows: Vec<(&str, FanSweep, FanSweep)> = vec![
        (
            "radials == 0",
            no_radials(),
            FanSweep {
                radials: 1,
                edges: vec![[0.0, 45.0]],
                codes: std::sync::Arc::new(vec![0; 4]),
                ..one_level()
            },
        ),
        (
            // Its repair is `one_level()` itself: giving the payload a level
            // back also gives it that level's bytes back, and those two
            // together are the only difference between the pair.
            "level_offsets.is_empty()",
            no_levels(),
            one_level(),
        ),
        (
            "edges cover the radials",
            FanSweep {
                edges: vec![[0.0, 45.0]; 7],
                ..one_level()
            },
            one_level(),
        ),
        (
            "the table is 256 entries",
            FanSweep {
                lut_rgba: vec![0; LUT_BYTES - LUT_ENTRY_BYTES],
                ..one_level()
            },
            one_level(),
        ),
        (
            "reach_gates != 0",
            FanSweep {
                geometry: FanGeometry {
                    reach_gates: 0,
                    ..geometry()
                },
                ..one_level()
            },
            one_level(),
        ),
        (
            "reach_gates <= gates",
            FanSweep {
                geometry: FanGeometry {
                    reach_gates: 5,
                    ..geometry()
                },
                ..one_level()
            },
            one_level(),
        ),
        (
            "the levels are laid out in order (overlap)",
            FanSweep {
                level_offsets: vec![0, 31, 40, 42],
                ..chained()
            },
            chained(),
        ),
        (
            "the levels are laid out in order (gap)",
            FanSweep {
                level_offsets: vec![0, 33, 41, 43],
                ..chained()
            },
            chained(),
        ),
        (
            "codes are exactly the levels' length",
            FanSweep {
                codes: std::sync::Arc::new(vec![0; 33]),
                ..one_level()
            },
            one_level(),
        ),
    ];
    for (guard, bad, good) in rows {
        assert!(
            !bad.is_well_formed(),
            "{guard}: accepted the payload it guards against"
        );
        assert!(
            good.is_well_formed(),
            "{guard}: the repaired payload is still refused, so the fixture is \
             wrong about which guard fired"
        );
    }
}

/// **No two outcomes share a counter.**
///
/// The ledger is the only report a running app makes of a hole in its radar
/// picture, and a hole reported under another hole's name is worse than one
/// reported under none: the figures still add up, so nothing looks wrong. The
/// arms are wired once, in `ledger::slot`, and this is the property that
/// wiring has to have — every outcome distinct, and every one inside the array
/// it indexes.
#[test]
fn every_outcome_has_a_counter_of_its_own() {
    let slots: Vec<usize> = ledger::EVERY_OUTCOME
        .iter()
        .map(|o| ledger::slot_for_test(*o))
        .collect();
    let distinct: std::collections::BTreeSet<usize> = slots.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        ledger::EVERY_OUTCOME.len(),
        "two outcomes share a counter: {slots:?}"
    );
    assert!(
        slots.iter().all(|s| *s < ledger::EVERY_OUTCOME.len()),
        "a slot indexes past the array it indexes: {slots:?}"
    );
    // And the list is the whole enum rather than a subset that happens to be
    // distinct: one arm per refusal, plus the painted arm.
    assert_eq!(
        ledger::EVERY_OUTCOME
            .iter()
            .filter(|o| matches!(o, FanOutcome::Refused(_)))
            .count(),
        ledger::EVERY_OUTCOME.len() - 1
    );
}

/// **The picture's family is the one that charges the plane**, and it charges
/// it a byte a byte.
///
/// `squallar_radar::hover::CodedGates::resident_bytes` answers 0 on purpose:
/// the readout borrows this buffer, and charging it there as well would name
/// one allocation in two census families. That is only correct while the
/// charge is really *here* — if this figure ignored `codes`, zeroing the other
/// would make the bytes vanish from the census altogether and a cut over them
/// would post a win by making memory invisible rather than absent.
///
/// So this asserts the charge moves with the buffer, by the buffer's own
/// growth, rather than asserting a remembered total.
#[test]
fn the_payloads_own_price_carries_the_code_plane() {
    let base = one_level();
    let before = base.resident_bytes();

    let grown_by = 64usize;
    let mut codes = base.codes.as_ref().clone();
    codes.extend(std::iter::repeat_n(0u8, grown_by));
    let grown = FanSweep {
        codes: std::sync::Arc::new(codes),
        ..one_level()
    };

    assert_eq!(
        grown.resident_bytes(),
        before + grown_by,
        "the code plane is not charged here, so nothing charges it: \
         `CodedGates::resident_bytes` answers 0 because it trusts this figure"
    );
    assert!(
        before >= base.codes.len(),
        "a price below the plane's own length cannot be charging for it"
    );
}

/// **A healthy payload still draws, asserted on a literal built right here.**
///
/// `is_well_formed` gained a conjunct on [`FanSweep::value_table`], and a
/// conjunct narrows what PASSES as well as what fails. That direction is the
/// dangerous one because it is invisible in a green suite: the gate has a
/// production caller (`ui_map_pane`'s draw fork), so a payload that used to be
/// accepted and is now refused is a pane with no radar on it, not a failing
/// test.
///
/// Every other positive case in this file reaches the gate through
/// `one_level` or `chained`, and the change that added the conjunct also
/// edited both — so none of them is *independent* evidence any longer. This
/// literal names every field itself.
///
/// The pair is the whole point: with the table at its full length the payload
/// must pass, and with **only** that length changed it must be refused. One
/// without the other proves nothing — the first alone cannot tell the conjunct
/// is live, and the second alone cannot tell it is not refusing healthy
/// pictures.
#[test]
fn a_hand_built_payload_passes_and_only_its_table_length_flips_that() {
    let healthy = FanSweep {
        field: squallar_radar::fields::known::REFLECTIVITY,
        radials: 8,
        gates: 4,
        codes: std::sync::Arc::new(vec![3; 32]),
        level_offsets: vec![0],
        lut_rgba: vec![9; LUT_BYTES],
        value_table: std::sync::Arc::new(vec![0.5; LUT_ENTRIES]),
        edges: (0..8)
            .map(|i| [i as f32 * 45.0, (i + 1) as f32 * 45.0])
            .collect(),
        geometry: geometry(),
    };
    assert!(
        healthy.is_well_formed(),
        "a payload carrying a full value table is refused — the new conjunct is \
         narrowing what the draw fork will accept"
    );

    for wrong in [LUT_ENTRIES - 1, LUT_ENTRIES + 1] {
        let bad = FanSweep {
            value_table: std::sync::Arc::new(vec![0.5; wrong]),
            codes: std::sync::Arc::clone(&healthy.codes),
            lut_rgba: healthy.lut_rgba.clone(),
            level_offsets: healthy.level_offsets.clone(),
            edges: healthy.edges.clone(),
            ..healthy_shape()
        };
        assert!(
            !bad.is_well_formed(),
            "a table of {wrong} entries is accepted, so a code can decode past \
             the end of it and the conjunct is not live"
        );
    }
}

/// The shape [`a_hand_built_payload_passes_and_only_its_table_length_flips_that`]
/// varies one field of, so the negative cases differ from the positive one in
/// the table alone.
fn healthy_shape() -> FanSweep {
    FanSweep {
        field: squallar_radar::fields::known::REFLECTIVITY,
        radials: 8,
        gates: 4,
        codes: std::sync::Arc::new(vec![3; 32]),
        level_offsets: vec![0],
        lut_rgba: vec![9; LUT_BYTES],
        value_table: std::sync::Arc::new(vec![0.5; LUT_ENTRIES]),
        edges: (0..8)
            .map(|i| [i as f32 * 45.0, (i + 1) as f32 * 45.0])
            .collect(),
        geometry: geometry(),
    }
}
