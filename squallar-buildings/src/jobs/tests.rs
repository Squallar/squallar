//! The building row's own wire tests: the layout it writes, the payloads it
//! refuses, and the reply's round trip.
//!
//! The byte-identity of a direct call against one through the wire lives with
//! the funnel that runs both (`squallar-worker`'s
//! `the_building_mesh_is_byte_identical_direct_and_via_the_wire`); what is here
//! is the row in isolation.

use super::*;
use crate::footprint::tests::{REAL_BUILDING_TILE, REAL_TILE_ID, a_frame};

/// The ceilings every fixture rides with, and **the two figures differ** for
/// the reason the box extents do: a wire layout is only pinned against a field
/// swap if the two fields are not the same number.
const CEILINGS: PrismCeilings = PrismCeilings {
    vram_bytes: 16 << 20,
    max_buffer_bytes: 1 << 28,
};

fn a_building_job() -> BuildingMeshJob {
    BuildingMeshJob {
        frame: a_frame(),
        ceilings: CEILINGS,
        tiles: vec![BuildingTile {
            tile: REAL_TILE_ID,
            mvt: Arc::new(REAL_BUILDING_TILE.to_vec()),
        }],
    }
}

/// The envelope this row ignores entirely: a mesh is not a raster and carries
/// no bounds of its own beyond the box on the input.
fn bare_geometry() -> JobGeometry {
    JobGeometry {
        width: 0,
        height: 0,
        bounds: squallar_geo::GeoBounds {
            min_lat: 0.0,
            max_lat: 0.0,
            min_lon: 0.0,
            max_lon: 0.0,
        },
        side_ceiling_px: 0,
    }
}

fn encoded(job: &BuildingMeshJob) -> Vec<u8> {
    let mut out = Vec::new();
    BuildingMeshJob::encode(
        job,
        &EncodeCtx {
            geometry: bare_geometry(),
        },
        &mut out,
    );
    out
}

fn decoded(bytes: &[u8]) -> Option<BuildingMeshJob> {
    let mut r = Reader::new(bytes);
    BuildingMeshJob::decode(&mut r, bare_geometry()).map(|(job, _)| job)
}

// ── The request ─────────────────────────────────────────────────────────────

#[test]
fn the_request_survives_its_own_wire_form() {
    let job = a_building_job();
    // Non-triviality first: an empty tile list, or empty bodies, would round
    // trip through almost any encoder.
    assert!(
        !job.tiles.is_empty() && job.tiles.iter().all(|tile| tile.mvt.len() > 1000),
        "the fixture carries {} tile(s) of {:?} bytes",
        job.tiles.len(),
        job.tiles.iter().map(|t| t.mvt.len()).collect::<Vec<_>>(),
    );
    assert_eq!(decoded(&encoded(&job)).as_ref(), Some(&job));
}

#[test]
fn the_request_is_its_prefix_plus_one_header_and_body_per_tile() {
    let job = a_building_job();
    let bodies: usize = job.tiles.iter().map(|tile| tile.mvt.len()).sum();
    assert_eq!(
        encoded(&job).len(),
        REQUEST_PREFIX_BYTES + job.tiles.len() * TILE_HEADER_BYTES + bodies,
    );
}

/// Byte offsets into the request prefix, stated once: six `f64` box terms fill
/// 0..48, the two `u64` ceilings 48..64, and the tile count 64..68.
const VRAM_AT: usize = 48;
const MAX_BUFFER_AT: usize = 56;
const COUNT_AT: usize = 64;

/// **Every guard in `decode` refuses, and the unaltered fixture is accepted**,
/// so none of the refusals below can be passing because the fixture was
/// already broken.
#[test]
fn a_payload_this_row_did_not_write_is_refused_rather_than_believed() {
    let job = a_building_job();
    let good = encoded(&job);
    assert!(decoded(&good).is_some(), "the control fixture must decode");
    // The offsets end exactly at the prefix, so a field that moved would fail
    // here rather than mutating a byte in some other field and passing for the
    // wrong reason.
    assert_eq!(COUNT_AT + 4, REQUEST_PREFIX_BYTES);
    assert_eq!(
        &good[VRAM_AT..VRAM_AT + 8],
        &CEILINGS.vram_bytes.to_le_bytes(),
    );
    assert_eq!(
        &good[MAX_BUFFER_AT..MAX_BUFFER_AT + 8],
        &CEILINGS.max_buffer_bytes.to_le_bytes(),
    );

    // A non-finite box term.
    let mut nan_site = good.clone();
    nan_site[..8].copy_from_slice(&f64::NAN.to_le_bytes());
    assert!(decoded(&nan_site).is_none(), "a NaN site decoded");

    // An inverted box: `x_km.1 <= x_km.0` has no interior to cull against.
    let mut inverted = good.clone();
    inverted[16..24].copy_from_slice(&3.0f64.to_le_bytes());
    inverted[24..32].copy_from_slice(&(-3.0f64).to_le_bytes());
    assert!(decoded(&inverted).is_none(), "an inverted box decoded");

    // A tile address off its own zoom's grid.
    let mut off_grid = good.clone();
    off_grid[COUNT_AT + 4] = 2;
    assert!(
        decoded(&off_grid).is_none(),
        "z2/8529/5974 decoded, which is a column that cannot exist",
    );

    // A tile count past the ceiling, which is also past the bytes present, so
    // `bounded` refuses before the ceiling does -- and the ceiling is asserted
    // separately below.
    let mut many = good.clone();
    many[COUNT_AT..COUNT_AT + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decoded(&many).is_none(), "a u32::MAX tile count decoded");

    // A declared body longer than the bytes that follow.
    let mut long_body = good.clone();
    let len_at = COUNT_AT + 9;
    long_body[len_at..len_at + 4].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
    assert!(
        decoded(&long_body).is_none(),
        "a runaway body length decoded"
    );

    // Trailing bytes: `JobRequest::from_bytes` checks these only for the
    // `overlay/` rows, so this row checks its own.
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(decoded(&trailing).is_none(), "a trailing byte decoded");

    // And every truncation of the good payload is refused rather than
    // half-read.
    for cut in [0, 1, 17, 47, REQUEST_PREFIX_BYTES - 1, good.len() - 1] {
        assert!(
            decoded(&good[..cut]).is_none(),
            "a payload truncated to {cut} bytes decoded",
        );
    }
}

/// The tile-count ceiling is a real ceiling and not merely the byte bound
/// restated.
///
/// A payload can carry a thousand thirteen-byte headers honestly, so `bounded`
/// lets it through and [`MAX_TILES`] is the only thing that does not.
#[test]
fn a_tile_count_past_the_ceiling_is_refused_even_when_the_bytes_are_there() {
    let empty_tile = |n: u32| BuildingTile {
        tile: TileId {
            z: 14,
            x: 8529 + n,
            y: 5974,
        },
        mvt: Arc::new(Vec::new()),
    };
    let at_ceiling = BuildingMeshJob {
        frame: a_frame(),
        ceilings: CEILINGS,
        tiles: (0..MAX_TILES as u32).map(empty_tile).collect(),
    };
    assert!(
        decoded(&encoded(&at_ceiling)).is_some(),
        "exactly {MAX_TILES} tiles must be accepted, or the ceiling is off by \
         one and this test is measuring the wrong edge",
    );
    let past = BuildingMeshJob {
        tiles: (0..MAX_TILES as u32 + 1).map(empty_tile).collect(),
        ..at_ceiling
    };
    assert!(
        decoded(&encoded(&past)).is_none(),
        "one past the ceiling decoded"
    );
}

// ── The run ─────────────────────────────────────────────────────────────────

/// The whole row end to end, on the real tile.
#[test]
fn the_real_tile_runs_to_a_mesh_of_prisms() {
    let mesh = BuildingMeshJob::run(&a_building_job(), &bare_geometry()).expect("the row answers");
    assert!(mesh.is_coherent());
    assert_eq!(mesh.refused_tiles, 0);
    assert!(
        mesh.kept > 20 && mesh.shed == 0,
        "{} kept and {} shed out of a 43-feature tile under a 16 MiB row",
        mesh.kept,
        mesh.shed,
    );
    assert!(
        mesh.positions.len() > 500 && mesh.indices.len().is_multiple_of(3),
        "{} vertices is too few for two dozen buildings",
        mesh.positions.len(),
    );
    // The mesh is genuinely three-dimensional: a run that produced flat
    // geometry would satisfy every count above.
    let tops: std::collections::BTreeSet<u32> =
        mesh.positions.iter().map(|v| v[2].to_bits()).collect();
    assert!(
        tops.len() > 10,
        "the mesh carries {} distinct heights",
        tops.len(),
    );
    assert!(
        mesh.bytes() < CEILINGS.vram_bytes,
        "the mesh overran its row"
    );
}

/// **A tile that does not decode is counted, not swallowed.**
///
/// A round that loses tiles and reports exactly as a clean one is the silent
/// partial success this counter exists to prevent.
#[test]
fn a_tile_that_does_not_decode_is_counted_and_the_rest_still_build() {
    let job = BuildingMeshJob {
        frame: a_frame(),
        ceilings: CEILINGS,
        tiles: vec![
            BuildingTile {
                tile: REAL_TILE_ID,
                mvt: Arc::new(REAL_BUILDING_TILE.to_vec()),
            },
            BuildingTile {
                tile: TileId {
                    z: 14,
                    x: 8530,
                    y: 5974,
                },
                mvt: Arc::new(b"not a vector tile".to_vec()),
            },
        ],
    };
    let mesh = BuildingMeshJob::run(&job, &bare_geometry()).expect("the row answers");
    assert_eq!(mesh.refused_tiles, 1);
    assert!(
        mesh.kept > 20,
        "the good tile's buildings were lost with the bad tile's",
    );
}

/// A tile with no `building` layer is the common case over most of the
/// archive, and it is **not** a refusal.
#[test]
fn a_tile_with_no_buildings_is_not_a_refused_tile() {
    let job = BuildingMeshJob {
        frame: a_frame(),
        ceilings: CEILINGS,
        tiles: vec![BuildingTile {
            tile: REAL_TILE_ID,
            // A valid, empty tile: no layers at all.
            mvt: Arc::new(Vec::new()),
        }],
    };
    let mesh = BuildingMeshJob::run(&job, &bare_geometry()).expect("the row answers");
    assert_eq!(
        (mesh.kept, mesh.shed, mesh.refused_tiles),
        (0, 0, 0),
        "an empty tile was reported as a refusal",
    );
    assert!(mesh.is_empty());
}

/// **One budget over every tile, never one per tile.**
///
/// The same footprints split across two tile payloads must shed the same
/// buildings as they do in one, or the shed keeps the tallest building in each
/// tile and the skyline becomes evenly spaced towers.
#[test]
fn the_budget_is_spent_across_the_whole_box_and_not_per_tile() {
    let tight = PrismCeilings {
        vram_bytes: 40_000,
        max_buffer_bytes: 1 << 28,
    };
    let one = BuildingMeshJob {
        frame: a_frame(),
        ceilings: tight,
        tiles: vec![BuildingTile {
            tile: REAL_TILE_ID,
            mvt: Arc::new(REAL_BUILDING_TILE.to_vec()),
        }],
    };
    let twice = BuildingMeshJob {
        frame: a_frame(),
        ceilings: tight,
        tiles: vec![
            BuildingTile {
                tile: REAL_TILE_ID,
                mvt: Arc::new(REAL_BUILDING_TILE.to_vec()),
            },
            BuildingTile {
                tile: REAL_TILE_ID,
                mvt: Arc::new(REAL_BUILDING_TILE.to_vec()),
            },
        ],
    };
    let single = BuildingMeshJob::run(&one, &bare_geometry()).expect("answers");
    let doubled = BuildingMeshJob::run(&twice, &bare_geometry()).expect("answers");

    assert!(
        single.shed > 0,
        "the tight row did not shed anything, so this test is not about the \
         shed at all",
    );
    assert_eq!(
        doubled.kept + doubled.shed,
        (single.kept + single.shed) * 2,
        "the doubled tile set did not produce twice the footprints",
    );
    let ceiling = PrismBudget::fit(tight).max_vertices as usize;
    assert!(
        doubled.positions.len() <= ceiling,
        "twice the tiles produced {} vertices against a ceiling of {ceiling}; \
         a per-tile budget would allow {}",
        doubled.positions.len(),
        ceiling * 2,
    );
    // The falsifiability half: the single-tile run has to be close enough to
    // the ceiling that a per-tile budget really would have overrun it. If the
    // one tile fitted with half the room to spare, the bound above would hold
    // for a per-tile budget too.
    assert!(
        single.positions.len() * 2 > ceiling,
        "the single-tile mesh is {} vertices against a {ceiling} ceiling, so \
         doubling it would not have overrun and this test cannot fail",
        single.positions.len(),
    );
}

// ── The reply ───────────────────────────────────────────────────────────────

fn round_tripped(mesh: BuildingMesh) -> Option<BuildingMesh> {
    let mut head = Vec::new();
    let mut tails = Vec::new();
    BuildingMeshJob::encode_out(mesh, &mut head, &mut tails);
    BuildingMeshJob::decode_out(&head, tails)
}

#[test]
fn the_reply_survives_its_own_wire_form() {
    let mesh = BuildingMeshJob::run(&a_building_job(), &bare_geometry()).expect("the row answers");
    assert!(
        mesh.positions.len() > 500,
        "a small mesh would round-trip through almost any encoder",
    );
    assert_eq!(round_tripped(mesh.clone()), Some(mesh));
}

#[test]
fn the_reply_rides_exactly_three_tails() {
    let mesh = BuildingMeshJob::run(&a_building_job(), &bare_geometry()).expect("the row answers");
    let mut head = Vec::new();
    let mut tails = Vec::new();
    BuildingMeshJob::encode_out(mesh.clone(), &mut head, &mut tails);
    assert_eq!(head.len(), REPLY_HEAD_BYTES);
    assert_eq!(tails.len(), 3);
    assert_eq!(tails[0].len(), mesh.positions.len() * 12);
    assert_eq!(tails[1].len(), mesh.normals.len() * 12);
    assert_eq!(tails[2].len(), mesh.indices.len() * 4);
}

/// The reply decoder refuses another build's layout rather than half-reading
/// it, and refuses a mesh whose indices do not address its own vertices.
#[test]
fn a_reply_this_row_did_not_write_is_refused() {
    let mesh = BuildingMesh {
        positions: vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        indices: vec![0, 1, 2],
        kept: 1,
        shed: 2,
        refused_tiles: 3,
    };
    let mut head = Vec::new();
    let mut tails = Vec::new();
    BuildingMeshJob::encode_out(mesh.clone(), &mut head, &mut tails);
    assert_eq!(
        BuildingMeshJob::decode_out(&head, tails.clone()),
        Some(mesh),
        "the control reply must decode",
    );

    // The wrong number of tails.
    assert!(BuildingMeshJob::decode_out(&head, Vec::new()).is_none());
    assert!(BuildingMeshJob::decode_out(&head, tails[..2].to_vec()).is_none());
    let mut four = tails.clone();
    four.push(Vec::new());
    assert!(BuildingMeshJob::decode_out(&head, four).is_none());

    // A tail whose length disagrees with the head.
    let mut short = tails.clone();
    short[0].truncate(12);
    assert!(BuildingMeshJob::decode_out(&head, short).is_none());
    let mut short_normals = tails.clone();
    short_normals[1].truncate(12);
    assert!(BuildingMeshJob::decode_out(&head, short_normals).is_none());

    // A head with a trailing byte.
    let mut long_head = head.clone();
    long_head.push(0);
    assert!(BuildingMeshJob::decode_out(&long_head, tails.clone()).is_none());

    // An index off the end of the position buffer -- the one refusal here that
    // is not merely defensive, because the GPU is what would read it.
    let mut wild = tails.clone();
    wild[2][0..4].copy_from_slice(&7u32.to_le_bytes());
    assert!(
        BuildingMeshJob::decode_out(&head, wild).is_none(),
        "an index past the vertex count decoded",
    );

    // An index count that is not a whole number of triangles.
    let mut partial = tails;
    partial[2].truncate(8);
    let mut head_for_two = Vec::new();
    for term in [1u32, 2, 3, 3, 2] {
        head_for_two.extend_from_slice(&term.to_le_bytes());
    }
    assert!(
        BuildingMeshJob::decode_out(&head_for_two, partial).is_none(),
        "two indices decoded as a mesh",
    );
}

/// The row's label and cost, which the composed registry indexes by.
#[test]
fn the_row_is_the_one_the_registry_composes() {
    assert_eq!(JOB_CODECS.len(), 1);
    assert_eq!(JOB_CODECS[0].label, "buildings/prisms");
    assert_eq!(BuildingMeshJob::LABEL, JOB_CODECS[0].label);
}
