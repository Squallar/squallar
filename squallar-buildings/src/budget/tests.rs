use super::*;
use crate::footprint::Ring;

/// A footprint of a given height, with a ring big enough to be real but small
/// enough to be cheap.
fn a_footprint(height_m: f64) -> BuildingFootprint {
    a_footprint_of(height_m, 4)
}

/// The same, with a chosen number of ring vertices, so a test can make one
/// building cost many times another.
fn a_footprint_of(height_m: f64, vertices: usize) -> BuildingFootprint {
    let points: Vec<[f64; 2]> = (0..vertices)
        .map(|i| {
            let angle = std::f64::consts::TAU * i as f64 / vertices as f64;
            [0.02 * angle.cos(), 0.02 * angle.sin()]
        })
        .collect();
    BuildingFootprint {
        rings: vec![Ring {
            points,
            exterior: true,
        }],
        base_m: 0.0,
        height_m,
        bbox: [-0.02, -0.02, 0.02, 0.02],
    }
}

// ── The ladder ──────────────────────────────────────────────────────────────

#[test]
fn a_generous_row_gets_the_finest_rung_and_says_so() {
    let budget = PrismBudget::fit(PrismCeilings {
        vram_bytes: u64::MAX,
        max_buffer_bytes: u64::MAX,
    });
    assert_eq!(budget.rung, PrismRung::FINEST);
    assert_eq!(budget.max_vertices, FINEST_VERTEX_CEILING);
    assert_eq!(budget.limit, PrismLimit::VertexCeiling);
    assert_eq!(
        budget.budgeted_bytes(),
        u64::from(FINEST_VERTEX_CEILING) * (PRISM_VERTEX_BYTES + 3 * PRISM_INDEX_BYTES),
    );
}

/// The row this crate asks for when nothing has measured a frame.
///
/// The figure in the module's own budget table, asserted rather than written
/// down twice.
#[test]
fn the_default_row_fits_at_the_rung_the_budget_table_records() {
    let budget = PrismBudget::fit(PrismCeilings::DEFAULT);
    assert_eq!(budget.max_vertices, 262_144);
    assert_eq!(budget.max_indices, 786_432);
    assert_eq!(budget.rung, PrismRung::from_halvings(2));
    assert_eq!(budget.limit, PrismLimit::Vram);
    assert_eq!(
        budget.budgeted_bytes(),
        9_437_184,
        "the budget table's 9.44 MB row has moved",
    );
    assert!(
        budget.budgeted_bytes() <= DEFAULT_PRISM_VRAM_BYTES,
        "the fit answered a rung that does not fit the row it was fitted \
         against",
    );
    // And one rung finer genuinely does not fit, so the answer is the
    // *finest* that does rather than merely one that does.
    assert!(
        PrismRung::from_halvings(1).budgeted_bytes() > DEFAULT_PRISM_VRAM_BYTES,
        "rung 1 fits too, so the ladder stopped early",
    );
}

/// **Monotone in the VRAM row.** More budget never answers fewer vertices,
/// which is the property `squallar_elevation::plan`'s ladder had to be
/// rewritten twice to get.
#[test]
fn the_ladder_is_monotone_in_the_row_it_is_given() {
    let mut previous = 0u32;
    let mut distinct = std::collections::BTreeSet::new();
    let mut row = 1u64 << 10;
    while row <= (1u64 << 34) {
        let budget = PrismBudget::fit(PrismCeilings {
            vram_bytes: row,
            max_buffer_bytes: u64::MAX,
        });
        assert!(
            budget.max_vertices >= previous,
            "a row of {row} bytes answered {} vertices where {} bytes \
             answered {previous}",
            budget.max_vertices,
            row / 2,
        );
        previous = budget.max_vertices;
        distinct.insert(budget.max_vertices);
        row *= 2;
    }
    assert!(
        distinct.len() >= 8,
        "the sweep produced only {} distinct answers, so a `fit` that ignored \
         its argument would have passed the monotonicity above",
        distinct.len(),
    );
}

/// Total at the floor: a row that affords almost nothing still gets a budget,
/// and the limit says what is still unmet.
#[test]
fn a_row_that_affords_nothing_still_gets_the_coarsest_rung() {
    let budget = PrismBudget::fit(PrismCeilings {
        vram_bytes: 1,
        max_buffer_bytes: u64::MAX,
    });
    assert_eq!(budget.max_vertices, MIN_VERTEX_CEILING);
    assert_eq!(budget.limit, PrismLimit::Vram);
    assert!(
        budget.budgeted_bytes() > 1,
        "the floor is deliberately allowed to exceed the row; if it did fit, \
         `limit` would be lying about what was unmet",
    );
}

/// The adapter's buffer limit is a ceiling in its own right, and the fit says
/// when it is the binding one.
#[test]
fn the_adapters_buffer_limit_binds_separately_from_the_vram_row() {
    let budget = PrismBudget::fit(PrismCeilings {
        vram_bytes: u64::MAX,
        // Enough for 65,536 vertices of position-and-normal and no more.
        max_buffer_bytes: 65_536 * PRISM_VERTEX_BYTES,
    });
    assert_eq!(budget.limit, PrismLimit::BufferSize);
    assert!(
        u64::from(budget.max_vertices) * PRISM_VERTEX_BYTES <= 65_536 * PRISM_VERTEX_BYTES,
        "the vertex buffer alone is past the adapter's single-buffer limit",
    );
    assert!(
        u64::from(budget.max_indices) * PRISM_INDEX_BYTES <= 65_536 * PRISM_VERTEX_BYTES,
        "the index buffer alone is past the adapter's single-buffer limit",
    );
    // The control: the same VRAM row without the adapter limit answers
    // something larger, so the limit really is what cut it.
    let unbounded = PrismBudget::fit(PrismCeilings {
        vram_bytes: u64::MAX,
        max_buffer_bytes: u64::MAX,
    });
    assert!(unbounded.max_vertices > budget.max_vertices);
}

#[test]
fn the_rung_ladder_terminates_at_its_floor() {
    let mut rung = PrismRung::FINEST;
    let mut steps = 0;
    while let Some(next) = rung.next_coarser() {
        rung = next;
        steps += 1;
        assert!(steps < 64, "the ladder did not terminate");
    }
    assert_eq!(rung.vertex_ceiling(), MIN_VERTEX_CEILING);
    assert_eq!(
        steps, 8,
        "the ladder is {steps} rungs long; 1,048,576 halved to 4,096 is 8",
    );
}

// ── The shed order ──────────────────────────────────────────────────────────

/// **Tallest first, ties in arrival order.**
///
/// The fixture deliberately carries both: three distinct heights *and* a tie,
/// because 16 of the confirmation archive's 126 buildings share
/// `render_height = 5` and a comparator that only handled the strict case
/// would pass a fixture with no ties in it.
#[test]
fn the_shed_order_is_tallest_first_and_stable_on_ties() {
    let footprints = [
        a_footprint(5.0),
        a_footprint(30.0),
        a_footprint(5.0),
        a_footprint(120.0),
        a_footprint(30.0),
    ];
    assert_eq!(shed_order(&footprints), vec![3, 1, 4, 0, 2]);

    // Reversing the input reverses the ties' relative order and nothing else.
    let reversed: Vec<BuildingFootprint> = footprints.iter().rev().cloned().collect();
    assert_eq!(shed_order(&reversed), vec![1, 0, 3, 2, 4]);
}

/// **Stability at a size where an unstable sort is actually unstable.**
///
/// The five-element fixture above cannot tell the two apart and this was
/// measured, not assumed: swapping `sort_by` for `sort_unstable_by` was the one
/// mutant of twenty-four that survived the suite. Rust's pattern-defeating
/// quicksort runs plain insertion sort below twenty elements, which happens to
/// be stable, so a small fixture proves nothing about the property its own name
/// claims.
///
/// The size here is past that threshold and every height is a tie, so the whole
/// answer is the arrival order and any reordering at all fails.
#[test]
fn the_shed_order_is_stable_at_a_size_where_instability_would_show() {
    // **Past twenty on purpose**, and held as a `const` assertion so that
    // lowering it fails the *build* rather than quietly turning this test back
    // into the one it was written to replace.
    const N: usize = 256;
    const { assert!(N > 20) };
    let all_tied: Vec<BuildingFootprint> = (0..N).map(|_| a_footprint(30.0)).collect();
    assert_eq!(
        shed_order(&all_tied),
        (0..N).collect::<Vec<_>>(),
        "with every height equal the order is the arrival order entire",
    );

    // And with two heights interleaved: every 30 in arrival order, then every
    // 5 in arrival order.
    let interleaved: Vec<BuildingFootprint> = (0..N)
        .map(|i| a_footprint(if i % 2 == 0 { 30.0 } else { 5.0 }))
        .collect();
    let expected: Vec<usize> = (0..N).step_by(2).chain((1..N).step_by(2)).collect();
    assert_eq!(shed_order(&interleaved), expected);
}

#[test]
fn an_empty_set_has_an_empty_order() {
    assert!(shed_order(&[]).is_empty());
}

/// A non-finite height does not take the sort down with it.
///
/// `read_footprints` refuses one, so this is defence rather than a live path —
/// but `partial_cmp` on a `NaN` is `None`, and a comparator that unwrapped it
/// would panic on the worker, where a panic on the web arm is a dead worker
/// rather than a caught one.
#[test]
fn a_non_finite_height_does_not_panic_the_sort() {
    let footprints = [a_footprint(5.0), a_footprint(f64::NAN), a_footprint(30.0)];
    let order = shed_order(&footprints);
    assert_eq!(order.len(), 3);
    assert!(order.contains(&0) && order.contains(&1) && order.contains(&2));
}
