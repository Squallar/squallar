use super::*;
use crate::budget::{PrismBudget, PrismCeilings};
use crate::footprint::BuildingFootprint;

/// A generous budget, so a test about geometry is not accidentally a test
/// about the shed.
fn no_shed() -> PrismBudget {
    PrismBudget::fit(PrismCeilings {
        vram_bytes: u64::MAX,
        max_buffer_bytes: u64::MAX,
    })
}

/// A counter-clockwise square of `side_km`, centred on `centre`.
fn square(centre: [f64; 2], side_km: f64) -> Vec<[f64; 2]> {
    let h = side_km / 2.0;
    vec![
        [centre[0] - h, centre[1] - h],
        [centre[0] + h, centre[1] - h],
        [centre[0] + h, centre[1] + h],
        [centre[0] - h, centre[1] + h],
    ]
}

fn footprint(rings: Vec<Ring>, base_m: f64, height_m: f64) -> BuildingFootprint {
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for ring in &rings {
        for p in &ring.points {
            bbox[0] = bbox[0].min(p[0]);
            bbox[1] = bbox[1].min(p[1]);
            bbox[2] = bbox[2].max(p[0]);
            bbox[3] = bbox[3].max(p[1]);
        }
    }
    BuildingFootprint {
        rings,
        base_m,
        height_m,
        bbox,
    }
}

/// A plain box building: one exterior ring, standing on the ground.
fn a_box(centre: [f64; 2], side_km: f64, height_m: f64) -> BuildingFootprint {
    footprint(
        vec![Ring {
            points: square(centre, side_km),
            exterior: true,
        }],
        0.0,
        height_m,
    )
}

/// The same with a courtyard through it.
fn a_box_with_a_courtyard(
    centre: [f64; 2],
    side_km: f64,
    hole_km: f64,
    height_m: f64,
) -> BuildingFootprint {
    let mut hole = square(centre, hole_km);
    hole.reverse();
    footprint(
        vec![
            Ring {
                points: square(centre, side_km),
                exterior: true,
            },
            Ring {
                points: hole,
                exterior: false,
            },
        ],
        0.0,
        height_m,
    )
}

/// Every triangle's own geometric normal, from its winding.
fn geometric_normals(mesh: &BuildingMesh) -> Vec<[f64; 3]> {
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            let p = |i: u32| {
                let v = mesh.positions[i as usize];
                [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
            };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ]
        })
        .collect()
}

/// The signed area of every triangle at height `z`, projected onto the ground.
fn cap_area_km2(mesh: &BuildingMesh, z: f32) -> f64 {
    let mut area = 0.0;
    for t in mesh.indices.chunks_exact(3) {
        let p: Vec<[f32; 3]> = t.iter().map(|&i| mesh.positions[i as usize]).collect();
        if p.iter().any(|v| (v[2] - z).abs() > 1e-9) {
            continue;
        }
        let (a, b, c) = (p[0], p[1], p[2]);
        area += 0.5 * f64::from((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]));
    }
    area
}

// ── Shape ───────────────────────────────────────────────────────────────────

/// A square becomes a roof of two triangles and four wall quads, and no floor.
#[test]
fn a_square_on_the_ground_is_a_roof_and_four_walls() {
    let mesh = extrude(&[a_box([0.0, 0.0], 0.1, 30.0)], &no_shed());
    assert_eq!(mesh.kept, 1);
    assert_eq!(mesh.shed, 0);
    assert!(mesh.is_coherent());
    // Four cap vertices plus four quads of four.
    assert_eq!(mesh.positions.len(), 4 + 16);
    // Two cap triangles plus two per quad.
    assert_eq!(mesh.indices.len() / 3, 2 + 8);
    assert_eq!(
        mesh.positions.iter().filter(|v| v[2] == 0.0).count(),
        8,
        "the walls' feet are the only vertices on the ground, two per quad",
    );
}

/// **The floor cap is a lane with its own fixture**, because
/// `render_min_height` is zero on 119 of the archive's 126 buildings and a
/// mesh builder that never emitted a floor would pass every test written over
/// those.
#[test]
fn a_building_that_does_not_start_on_the_ground_gets_a_floor_cap() {
    let ground = a_box([0.0, 0.0], 0.1, 30.0);
    let raised = footprint(ground.rings.clone(), 18.0, 30.0);

    let on_ground = extrude(&[ground], &no_shed());
    let above = extrude(&[raised], &no_shed());

    assert_eq!(
        above.positions.len() - on_ground.positions.len(),
        4,
        "a raised building must carry a second cap; it carries {} extra \
         vertices",
        above.positions.len() - on_ground.positions.len(),
    );
    assert_eq!(above.indices.len() - on_ground.indices.len(), 6);

    let floor_z = 0.018f32;
    assert!(
        (cap_area_km2(&above, floor_z) + 0.01).abs() < 1e-6,
        "the floor cap's signed area is {}, and it must be the roof's \
         negated: a floor wound like a roof is invisible from below, which is \
         the only place it is ever seen",
        cap_area_km2(&above, floor_z),
    );
    assert!(
        (cap_area_km2(&above, 0.03) - 0.01).abs() < 1e-6,
        "the roof is not at `render_height`",
    );
    assert!(
        !above.positions.iter().any(|v| v[2] == 0.0),
        "a building starting 18 m up has vertices on the ground",
    );
}

/// The non-zero fill rule and the canonical winding, together: a courtyard
/// stays open.
#[test]
fn a_courtyard_is_not_filled_in() {
    let mesh = extrude(
        &[a_box_with_a_courtyard([0.0, 0.0], 0.1, 0.04, 30.0)],
        &no_shed(),
    );
    let roof = cap_area_km2(&mesh, 0.03);
    assert!(
        (roof - (0.01 - 0.0016)).abs() < 1e-6,
        "the roof covers {roof} km2 where the ring set encloses {} km2; a \
         filled courtyard would read 0.01",
        0.01 - 0.0016,
    );
    // And the courtyard has walls of its own.
    assert_eq!(
        mesh.positions.iter().filter(|v| v[2] == 0.0).count(),
        16,
        "eight ring edges means eight wall quads and sixteen feet",
    );
}

/// **Two overlapping parts of one building union; they do not punch a hole in
/// each other.**
///
/// This is the fill rule and nothing else. The courtyard test above passes
/// under either rule — a single hole wound against its exterior reads the same
/// to non-zero and even-odd — so it does not pin the choice. Two overlapping
/// *exteriors* is where the two rules disagree, and it is a real shape: an
/// OpenMapTiles building whose parts were merged from separate OSM ways
/// arrives exactly like this.
#[test]
fn two_overlapping_parts_of_one_building_union() {
    // Two 0.1 km squares offset by half a side: each is 0.01 km2 and they
    // share 0.005 km2.
    let overlap = footprint(
        vec![
            Ring {
                points: square([0.0, 0.0], 0.1),
                exterior: true,
            },
            Ring {
                points: square([0.05, 0.0], 0.1),
                exterior: true,
            },
        ],
        0.0,
        30.0,
    );
    let roof = cap_area_km2(&extrude(&[overlap], &no_shed()), 0.03);
    assert!(
        (roof - 0.015).abs() < 1e-6,
        "the roof covers {roof} km2; the union of the two parts is 0.015 and \
         an even-odd fill would subtract the overlap twice for 0.010",
    );
}

// ── Orientation ─────────────────────────────────────────────────────────────

/// Every wall faces away from the solid it belongs to.
///
/// **Both kinds of ring in one test**, because they are the two halves of one
/// rule and a fixture with only an exterior would pass on a builder that had
/// the hole case exactly backwards.
#[test]
fn wall_normals_point_away_from_the_solid() {
    let centre = [0.0, 0.0];
    let mesh = extrude(
        &[a_box_with_a_courtyard(centre, 0.1, 0.04, 30.0)],
        &no_shed(),
    );

    let mut outer = 0;
    let mut inner = 0;
    for (position, normal) in mesh.positions.iter().zip(&mesh.normals) {
        if normal[2] != 0.0 {
            continue;
        }
        let radial = [
            f64::from(position[0]) - centre[0],
            f64::from(position[1]) - centre[1],
        ];
        let dot = f64::from(normal[0]) * radial[0] + f64::from(normal[1]) * radial[1];
        // Inside the courtyard's half-width is a courtyard wall; outside it is
        // an outer wall. The two are 0.02 km and 0.05 km from the centre.
        if radial[0].abs().max(radial[1].abs()) < 0.03 {
            assert!(
                dot < 0.0,
                "a courtyard wall at {position:?} has normal {normal:?}, \
                 which points out of the courtyard rather than into it",
            );
            inner += 1;
        } else {
            assert!(
                dot > 0.0,
                "an outer wall at {position:?} has normal {normal:?}, which \
                 points into the building",
            );
            outer += 1;
        }
    }
    assert_eq!(
        (outer, inner),
        (16, 16),
        "both kinds of wall have to be present or this test proves one rule \
         and assumes the other",
    );
}

/// Every triangle is wound so its front face agrees with the normal its
/// vertices carry.
///
/// A cap or a quad wound the wrong way is invisible under back-face culling
/// and lit inside-out without it, and neither shows up in a vertex count.
#[test]
fn every_triangle_is_wound_to_agree_with_its_own_normal() {
    let mesh = extrude(
        &[
            a_box_with_a_courtyard([0.0, 0.0], 0.1, 0.04, 30.0),
            footprint(
                vec![Ring {
                    points: square([0.3, 0.0], 0.08),
                    exterior: true,
                }],
                21.0,
                44.0,
            ),
        ],
        &no_shed(),
    );
    let geometric = geometric_normals(&mesh);
    assert!(!geometric.is_empty());
    for (triangle, normal) in mesh.indices.chunks_exact(3).zip(&geometric) {
        let stored = mesh.normals[triangle[0] as usize];
        let dot = f64::from(stored[0]) * normal[0]
            + f64::from(stored[1]) * normal[1]
            + f64::from(stored[2]) * normal[2];
        assert!(
            dot > 0.0,
            "a triangle wound {normal:?} carries the normal {stored:?}; its \
             front face points the other way",
        );
    }
    // The falsifiability half: the mesh really does carry faces pointing in
    // several directions, so "every triangle agrees" is not a statement about
    // one plane.
    let distinct: std::collections::BTreeSet<[u32; 3]> = mesh
        .normals
        .iter()
        .map(|n| [n[0].to_bits(), n[1].to_bits(), n[2].to_bits()])
        .collect();
    assert!(
        distinct.len() >= 6,
        "the mesh carries only {} distinct normals",
        distinct.len(),
    );
}

// ── The shed ────────────────────────────────────────────────────────────────

/// **The tallest buildings survive and the shortest go**, and every footprint
/// is accounted for either way.
#[test]
fn a_forced_low_ceiling_sheds_the_shortest_buildings_first() {
    let heights = [5.0, 92.0, 30.0, 120.0, 8.0, 43.0];
    let footprints: Vec<BuildingFootprint> = heights
        .iter()
        .enumerate()
        .map(|(i, &h)| a_box([i as f64 * 0.3, 0.0], 0.1, h))
        .collect();

    let full = extrude(&footprints, &no_shed());
    assert_eq!(full.kept, 6);
    assert_eq!(full.shed, 0);
    let per_building = full.positions.len() / 6;

    // Room for three buildings and not a vertex more.
    let budget = PrismBudget {
        max_vertices: (per_building * 3) as u32,
        max_indices: u32::MAX,
        rung: full_rung(),
        limit: crate::budget::PrismLimit::Vram,
    };
    let cut = extrude(&footprints, &budget);
    assert_eq!(cut.kept, 3);
    assert_eq!(cut.shed, 3);
    assert_eq!(
        cut.kept + cut.shed,
        footprints.len() as u32,
        "kept and shed must account for every footprint handed in",
    );

    // The three that survived are the three tallest, read back off the mesh's
    // own roof heights rather than off the loop that built it.
    let mut roofs: Vec<f64> = cut
        .positions
        .iter()
        .map(|v| f64::from(v[2]) * 1000.0)
        .filter(|z| *z > 0.001)
        .collect();
    roofs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    roofs.dedup_by(|a, b| (*a - *b).abs() < 0.001);
    assert_eq!(roofs.len(), 3);
    for (got, want) in roofs.iter().zip([43.0, 92.0, 120.0]) {
        assert!(
            (got - want).abs() < 0.01,
            "the surviving roofs are {roofs:?}, not the three tallest",
        );
    }
}

/// A building is all or nothing: the budget never lands half a tower.
///
/// The fixture makes the next building in height order **too big to fit the
/// remaining room**, which is exactly the case a per-triangle budget would get
/// wrong while a per-building one gets right.
#[test]
fn a_building_that_does_not_fit_is_left_out_whole() {
    let small = a_box([0.0, 0.0], 0.1, 120.0);
    let big = footprint(
        vec![Ring {
            points: (0..64)
                .map(|i| {
                    let a = std::f64::consts::TAU * f64::from(i) / 64.0;
                    [0.5 + 0.05 * a.cos(), 0.05 * a.sin()]
                })
                .collect(),
            exterior: true,
        }],
        0.0,
        92.0,
    );
    let footprints = vec![small.clone(), big];

    let full = extrude(&footprints, &no_shed());
    let small_only = extrude(&[small], &no_shed());
    assert!(full.positions.len() > small_only.positions.len() * 5);

    // Room for the first building and a little more, but nowhere near the
    // second.
    let budget = PrismBudget {
        max_vertices: (small_only.positions.len() + 10) as u32,
        max_indices: u32::MAX,
        rung: full_rung(),
        limit: crate::budget::PrismLimit::Vram,
    };
    let cut = extrude(&footprints, &budget);
    assert_eq!(cut.kept, 1);
    assert_eq!(cut.shed, 1);
    assert_eq!(
        cut.positions.len(),
        small_only.positions.len(),
        "the mesh carries part of the second building",
    );
    assert!(cut.is_coherent());
}

/// **The prefix rule itself, against the greedy variant it was chosen over.**
///
/// This is the unit's headline property and it had no gate: replacing the
/// early return in [`extrude`] with `mesh.shed += 1; continue;` -- which *is*
/// the "walk past the one that does not fit and pick up smaller ones behind
/// it" that `shed_order`'s doc argues against -- survived the whole suite.
/// All three of the shed tests above are blind to it, two because every
/// building in them is the same size and the third because it puts the
/// *smaller* footprint at the *taller* height, so nothing is ever skipped over.
///
/// The discriminating fixture is the one those lack: the **taller** building
/// is also the **expensive** one, and the ceiling has room for the short cheap
/// one but not for the tall costly one. A prefix answers nothing; a greedy
/// walk answers the short building.
///
/// The behaviour under test is deliberately the *worse-looking* of the two --
/// an emptier pane. It is chosen because it is monotone: a building being
/// present implies every taller building is present, so the skyline does not
/// gain and lose a tower as the camera moves and the footprint set changes
/// underneath it.
#[test]
fn the_shed_keeps_a_prefix_and_does_not_walk_past_a_building_that_does_not_fit() {
    // Tall and expensive: a 64-sided ring is 64 cap vertices and 64 wall quads.
    let tall_and_costly = footprint(
        vec![Ring {
            points: (0..64)
                .map(|i| {
                    let a = std::f64::consts::TAU * f64::from(i) / 64.0;
                    [0.05 * a.cos(), 0.05 * a.sin()]
                })
                .collect(),
            exterior: true,
        }],
        0.0,
        120.0,
    );
    // Short and cheap: four sides.
    let short_and_cheap = a_box([0.5, 0.0], 0.1, 5.0);

    let costly_size = extrude(std::slice::from_ref(&tall_and_costly), &no_shed())
        .positions
        .len();
    let cheap_size = extrude(std::slice::from_ref(&short_and_cheap), &no_shed())
        .positions
        .len();
    assert!(
        cheap_size < costly_size,
        "the fixture is not discriminating: the cheap building costs \
         {cheap_size} vertices and the costly one {costly_size}",
    );

    // Room for the cheap building and nowhere near the costly one.
    let budget = PrismBudget {
        max_vertices: cheap_size as u32,
        max_indices: u32::MAX,
        rung: full_rung(),
        limit: crate::budget::PrismLimit::Vram,
    };
    let mesh = extrude(&[tall_and_costly, short_and_cheap], &budget);
    assert_eq!(
        (mesh.kept, mesh.shed),
        (0, 2),
        "the shed walked past the 120 m building it could not fit and picked \
         up the 5 m one behind it. That is the greedy variant `shed_order` \
         refuses: it makes the kept set depend on each footprint's tessellated \
         size rather than on its height, so a tower appears and disappears as \
         its neighbours change",
    );
    assert!(mesh.is_empty());

    // The control, so the assertion above is not passing because the ceiling
    // was too small for anything: one vertex more and the cheap building is
    // kept, in a run where it is the only footprint.
    let alone = extrude(&[a_box([0.5, 0.0], 0.1, 5.0)], &budget);
    assert_eq!((alone.kept, alone.shed), (1, 0));
}

/// The index ceiling binds on its own, not only through the vertex one.
#[test]
fn the_index_ceiling_sheds_even_when_the_vertex_ceiling_does_not() {
    let footprints: Vec<BuildingFootprint> = (0..4)
        .map(|i| a_box([f64::from(i) * 0.3, 0.0], 0.1, 10.0 * f64::from(i + 1)))
        .collect();
    let full = extrude(&footprints, &no_shed());
    let per_building = full.indices.len() / 4;

    let budget = PrismBudget {
        max_vertices: u32::MAX,
        max_indices: (per_building * 2) as u32,
        rung: full_rung(),
        limit: crate::budget::PrismLimit::Vram,
    };
    let cut = extrude(&footprints, &budget);
    assert_eq!(cut.kept, 2);
    assert_eq!(cut.shed, 2);
    assert!(cut.indices.len() <= per_building * 2);
}

/// **`is_coherent`'s index bound is exclusive**, and the boundary is where it
/// matters: `positions.len()` is one past the last addressable vertex, so `<`
/// against `<=` is the difference between refusing an out-of-bounds read on
/// the GPU and shipping it. The doc calls this the one check that is not
/// merely defensive and it had no test at all.
#[test]
fn a_mesh_whose_index_is_one_past_its_last_vertex_is_incoherent() {
    let sound = BuildingMesh {
        positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        indices: vec![0, 1, 2],
        kept: 1,
        shed: 0,
        refused_tiles: 0,
    };
    assert!(sound.is_coherent(), "the control mesh must be coherent");

    // Exactly one past the end. `<=` would accept this.
    let one_past = BuildingMesh {
        indices: vec![0, 1, 3],
        ..sound.clone()
    };
    assert!(
        !one_past.is_coherent(),
        "index 3 addresses a fourth vertex of a three-vertex mesh",
    );

    // The other two arms of the same check, so a fixture sitting on one of
    // them cannot stand in for the rest.
    assert!(
        !BuildingMesh {
            normals: vec![[0.0, 0.0, 1.0]; 2],
            ..sound.clone()
        }
        .is_coherent(),
        "a mesh with fewer normals than positions is coherent",
    );
    assert!(
        !BuildingMesh {
            indices: vec![0, 1],
            ..sound.clone()
        }
        .is_coherent(),
        "two indices are not a whole number of triangles",
    );
    // And an empty mesh is coherent, so the check is not simply refusing
    // everything.
    assert!(BuildingMesh::default().is_coherent());
}

#[test]
fn an_empty_footprint_set_is_an_empty_mesh() {
    let mesh = extrude(&[], &no_shed());
    assert!(mesh.is_empty());
    assert!(mesh.is_coherent());
    assert_eq!((mesh.kept, mesh.shed, mesh.refused_tiles), (0, 0, 0));
    assert_eq!(mesh.bytes(), 0);
}

/// A footprint with no rings costs nothing and is still counted as kept: the
/// budget did not refuse it.
#[test]
fn a_footprint_with_nothing_in_it_is_kept_rather_than_shed() {
    let mesh = extrude(
        &[
            footprint(Vec::new(), 0.0, 30.0),
            a_box([0.0, 0.0], 0.1, 5.0),
        ],
        &no_shed(),
    );
    assert_eq!((mesh.kept, mesh.shed), (2, 0));
    assert!(mesh.is_coherent());
}

fn full_rung() -> crate::budget::PrismRung {
    crate::budget::PrismRung::FINEST
}
