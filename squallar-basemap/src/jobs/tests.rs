//! Round-trip identity for the row itself, in both directions.
//!
//! The shape wire has its own suite (`crate::wire::tests`); this one covers
//! what the row adds around it — the style key, the tile headers, the phase
//! micros that keep the page's ledger from going silent, and the refusals.

use super::*;

const TILE: &[u8] =
    include_bytes!("../../../squallar-buildings/testdata/monaco-building-z14-8529-5974.mvt");

/// The envelope this row crosses verbatim. A batch of shapes has no raster
/// geometry of its own — extent units are the tile's, not the screen's — so
/// every term is zero and every term still rides the wire, as
/// `JobRequest::to_bytes` spells it.
fn geometry() -> JobGeometry {
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

fn a_job() -> BasemapTilesJob {
    BasemapTilesJob {
        style: StyleKey {
            is_dark: true,
            // Non-empty, so the filtering arm of `committed_filtered` is the
            // one under test — but NOT `building`, because the fixture IS the
            // building source-layer and disabling it would style the tile to
            // nothing. The non-vacuity assertion below caught exactly that.
            disabled: ["poi".to_owned()].into(),
        },
        tiles: vec![
            TileBody {
                z: 14,
                x: 8529,
                y: 5974,
                mvt: Arc::new(TILE.to_vec()),
            },
            // A body that is not MVT at all: the refusal arm, in the same
            // batch as a good tile, because the interesting property is that
            // one bad tile does not take the batch down with it.
            TileBody {
                z: 3,
                x: 4,
                y: 2,
                mvt: Arc::new(b"not a protobuf".to_vec()),
            },
        ],
    }
}

#[test]
fn the_request_round_trips_to_an_identical_job() {
    let job = a_job();
    let mut bytes = Vec::new();
    BasemapTilesJob::encode(
        &job,
        &EncodeCtx {
            geometry: geometry(),
        },
        &mut bytes,
    );
    let (back, _) = BasemapTilesJob::decode(&mut Reader::new(&bytes), geometry())
        .expect("what this row encoded, it decodes");
    assert_eq!(back, job, "the request wire did not reproduce the job");
}

#[test]
fn the_reply_round_trips_to_an_identical_styling() {
    let job = a_job();
    let out = BasemapTilesJob::run(&job, &geometry()).expect("the row answers");

    // Non-vacuity: one tile must have styled to real geometry and the other
    // must have refused, or this is not testing what it says.
    assert_eq!(out.tiles.len(), 2);
    let good = out.tiles.iter().find(|t| t.z == 14).expect("the z14 tile");
    let bad = out.tiles.iter().find(|t| t.z == 3).expect("the bad tile");
    assert!(good.shapes.is_some(), "the committed fixture must style");
    assert!(bad.shapes.is_none(), "a body that is not MVT must refuse");
    let vertices: usize = good
        .shapes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| match s {
            ShapeOrText::Shape(egui::Shape::Mesh(m)) => m.vertices.len(),
            _ => 0,
        })
        .sum();
    assert!(vertices >= 200, "the fixture must reach the tessellator");

    let before = format!("{out:?}");
    let mut head = Vec::new();
    let mut tails = Vec::new();
    BasemapTilesJob::encode_out(out, &mut head, &mut tails);
    assert_eq!(tails.len(), 4, "the row nominates four tails");
    let back = BasemapTilesJob::decode_out(&head, tails).expect("the reply decodes");
    assert_eq!(
        format!("{back:?}"),
        before,
        "the reply wire did not reproduce the styling",
    );
}

/// A body that does not parse refuses ALONE: the batch still answers, and the
/// good tile in it still styles.
///
/// This is the property the row's `None` arm exists for. Answering an empty
/// shape list instead would cache a blank tile the page would never re-ask
/// for, which is the silent-partial-success shape this workspace refuses.
#[test]
fn one_unparseable_body_does_not_take_the_batch_down() {
    let out = BasemapTilesJob::run(&a_job(), &geometry()).expect("the row answers");
    assert_eq!(out.tiles.len(), 2, "every tile asked for is answered for");
    assert!(
        out.tiles
            .iter()
            .find(|t| t.z == 14)
            .expect("z14")
            .shapes
            .is_some(),
        "the good tile in a batch with a bad one must still style",
    );
    assert!(
        out.tiles
            .iter()
            .find(|t| t.z == 3)
            .expect("z3")
            .shapes
            .is_none(),
        "a body that is not MVT must refuse, not answer an empty styling",
    );
}

/// The style is built once per key and then shared, not re-parsed per job.
/// `committed_filtered` re-parses ~95 internally-tagged layers whenever the
/// filter is non-empty, and the shipping default IS non-empty.
#[test]
fn a_style_key_is_built_once_and_then_shared() {
    let key = StyleKey {
        is_dark: false,
        disabled: ["water".to_owned()].into(),
    };
    let first = style_for(&key);
    let second = style_for(&key);
    assert!(
        Arc::ptr_eq(&first, &second),
        "the same style key was parsed twice; `committed_filtered` walks every \
         layer and the shipping default filter is non-empty",
    );
    // Non-vacuity: a different key must NOT hand back the same allocation, or
    // the assertion above would pass for a memo that ignores its key.
    let other = style_for(&StyleKey {
        is_dark: true,
        disabled: ["water".to_owned()].into(),
    });
    assert!(
        !Arc::ptr_eq(&first, &other),
        "the memo ignored its theme bit"
    );
}

/// A theme byte that is not a bool is another build's layout.
#[test]
fn a_non_bool_theme_byte_is_refused() {
    let mut bytes = vec![7u8];
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    assert!(BasemapTilesJob::decode(&mut Reader::new(&bytes), geometry()).is_none());
}

/// A tile count past the ceiling answers `None` rather than reserving.
#[test]
fn a_tile_count_past_the_ceiling_is_refused() {
    let mut bytes = vec![1u8];
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(MAX_TILES as u32 + 1).to_le_bytes());
    // Enough filler that `bounded` cannot refuse it on length alone — the
    // ceiling is what must fire here, not the buffer running out.
    bytes.extend(std::iter::repeat_n(
        0u8,
        (MAX_TILES + 1) * TILE_HEADER_BYTES,
    ));
    assert!(BasemapTilesJob::decode(&mut Reader::new(&bytes), geometry()).is_none());
}

/// A coordinate off the grid at its own zoom is not a tile.
#[test]
fn an_unaddressable_tile_is_refused() {
    for (z, x, y) in [(2u8, 4u32, 0u32), (2, 0, 4), (31, 0, 0)] {
        let mut bytes = vec![1u8];
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(z);
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert!(
            BasemapTilesJob::decode(&mut Reader::new(&bytes), geometry()).is_none(),
            "z{z}/{x}/{y} was accepted",
        );
    }
}

/// Trailing bytes are refused: `JobRequest::from_bytes` checks them only for
/// the overlay rows, so this row checks its own.
#[test]
fn trailing_request_bytes_are_refused() {
    let job = a_job();
    let mut bytes = Vec::new();
    BasemapTilesJob::encode(
        &job,
        &EncodeCtx {
            geometry: geometry(),
        },
        &mut bytes,
    );
    bytes.push(0);
    assert!(BasemapTilesJob::decode(&mut Reader::new(&bytes), geometry()).is_none());
}

/// A tail count this row did not write is refused.
#[test]
fn a_foreign_reply_tail_count_is_refused() {
    let head = 0u32.to_le_bytes().to_vec();
    for count in [0usize, 3, 5] {
        assert!(
            BasemapTilesJob::decode_out(&head, vec![Vec::new(); count]).is_none(),
            "{count} tails was accepted; this row writes exactly four",
        );
    }
    assert!(
        BasemapTilesJob::decode_out(&head, vec![Vec::new(); 4]).is_some(),
        "an empty batch over four tails is a valid reply",
    );
}

/// A tail carrying bytes the head never described is another build's layout,
/// not a batch to salvage.
#[test]
fn an_undescribed_tail_is_refused() {
    let head = 0u32.to_le_bytes().to_vec();
    let mut tails = vec![Vec::new(); 4];
    tails[2].push(1);
    assert!(BasemapTilesJob::decode_out(&head, tails).is_none());
}
