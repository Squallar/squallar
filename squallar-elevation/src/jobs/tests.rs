//! The height row's own wire tests: the layout it writes, the payloads it
//! refuses, and the reply's round trip.
//!
//! The byte-identity of a direct call against one through the wire lives with
//! the funnel that runs both (`squallar-worker`'s
//! `the_height_field_is_byte_identical_direct_and_via_the_wire`); what is here
//! is the row in isolation, where the fixtures can be built with the crate's
//! own PNG encoder.

use super::*;
use crate::resample::cover_for;
use crate::trgb;

/// The box every fixture below is over: 6 km east-west by 6 km north-south at
/// 39N 106W, but **not centred and not a square in its terms**, on an asymmetric
/// post grid.
///
/// Both asymmetries are load-bearing and for the same reason: a wire layout is
/// only pinned against a field swap if the two fields differ. `POSTS` was
/// written asymmetric from the start; `Y_KM` was `(-3.0, 3.0)` and review found
/// that a symmetric reorder of the six box terms in `encode` and `decode`
/// survived the whole suite *including the framing digest*, because the bytes
/// come out identical when `x_km == y_km`. Two builds either side of that edit
/// would have held the same build token and exchanged a transposed box. Keep
/// these two pairs different from each other.
const SITE: (f64, f64) = (39.0, -106.0);
const X_KM: (f64, f64) = (-3.0, 3.0);
const Y_KM: (f64, f64) = (-2.0, 4.0);
const POSTS: [u32; 2] = [5, 3];
const ZOOM: u8 = 10;
/// Eight pixels rather than 256, which is the whole fixture-size story: a tile
/// address depends on `tile_px * 2^zoom` only through the ratio, so the
/// rectangle a box needs is the same at any tile size, and eight keeps the
/// synthesised bodies small enough to be cheap in every test here.
const TILE_PX: u32 = 8;

/// A Terrain-RGB tile carrying an analytic ramp, encoded losslessly.
///
/// Packed through the same base-256 arithmetic `trgb::unpack` reverses, spelled
/// out here rather than calling a `pack`: this crate deliberately has none
/// (`the_constants_match_the_builders_source_text` asserts its absence), so a
/// fixture encoder is written where it is used and cannot be mistaken for a
/// production path.
fn tile_png(px: u32, base_m: f64) -> Vec<u8> {
    let mut img = image::RgbImage::new(px, px);
    for (col, row, pixel) in img.enumerate_pixels_mut() {
        let height_m = base_m + 100.0 * f64::from(col) + 50.0 * f64::from(row);
        let packed = ((height_m - trgb::BASE_M) / trgb::QUANTUM_M).round() as u32;
        *pixel = image::Rgb([
            ((packed >> 16) & 255) as u8,
            ((packed >> 8) & 255) as u8,
            (packed & 255) as u8,
        ]);
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the encoder runs");
    png
}

/// The cover the fixture box actually needs, from the crate's own arithmetic.
fn fixture_cover() -> TileCover {
    cover_for(SITE, X_KM, Y_KM, POSTS, ZOOM, TILE_PX).expect("the fixture box has a cover")
}

fn a_height_job() -> TerrainHeightJob {
    let cover = fixture_cover();
    let tiles = cover
        .addresses()
        .map(|(x, y)| HeightTile {
            x,
            y,
            png: Arc::new(tile_png(TILE_PX, 2000.0)),
        })
        .collect();
    TerrainHeightJob {
        site: SITE,
        x_km: X_KM,
        y_km: Y_KM,
        posts: POSTS,
        cover,
        tiles,
    }
}

/// Encode a job the way `JobRequest` does, minus the envelope this row never
/// reads.
fn encoded(job: &TerrainHeightJob) -> Vec<u8> {
    let mut out = Vec::new();
    TerrainHeightJob::encode(
        job,
        &EncodeCtx {
            geometry: bare_geometry(),
        },
        &mut out,
    );
    out
}

/// The envelope this row ignores entirely: a height field is not a raster and
/// carries no bounds of its own beyond the box on the input.
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

fn decoded(bytes: &[u8]) -> Option<TerrainHeightJob> {
    let mut r = Reader::new(bytes);
    TerrainHeightJob::decode(&mut r, bare_geometry()).map(|(job, _)| job)
}

#[test]
fn the_request_survives_its_own_wire_form() {
    let job = a_height_job();
    // Non-triviality first: a cover of no tiles, or a fixture whose bodies were
    // empty, would round-trip through almost any encoder.
    assert!(
        !job.tiles.is_empty() && job.tiles.iter().all(|tile| tile.png.len() > 60),
        "the fixture carries {} tile(s) of {:?} bytes",
        job.tiles.len(),
        job.tiles
            .iter()
            .map(|tile| tile.png.len())
            .collect::<Vec<_>>(),
    );
    assert_eq!(decoded(&encoded(&job)).as_ref(), Some(&job));
}

/// The prefix is the arithmetic the module states, not whatever the encoder
/// happened to write.
#[test]
fn the_request_is_its_prefix_plus_one_header_and_body_per_tile() {
    let job = a_height_job();
    let bodies: usize = job.tiles.iter().map(|tile| tile.png.len()).sum();
    assert_eq!(
        encoded(&job).len(),
        REQUEST_PREFIX_BYTES + job.tiles.len() * TILE_HEADER_BYTES + bodies,
    );
}

/// Byte offsets into the request prefix, stated once: six `f64` box terms fill
/// 0..48, the two post counts 48..56, the cover's zoom byte 56 and its five
/// `u32`s 57..77, and the tile count 77..81.
const POSTS_AT: usize = 48;
const TILE_PX_AT: usize = 57;
const TX0_AT: usize = 61;
const TX1_AT: usize = 69;
const TY1_AT: usize = 73;
const COUNT_AT: usize = 77;

/// **Every guard in `decode` refuses, and the unaltered fixture is accepted**,
/// so none of the refusals below can be passing because the fixture was already
/// broken.
#[test]
fn a_payload_this_row_did_not_write_is_refused_rather_than_believed() {
    let job = a_height_job();
    let good = encoded(&job);
    assert!(decoded(&good).is_some(), "the control fixture must decode");
    // The offsets above are inside the prefix and end exactly at it, so an
    // edit that moved a field would fail here rather than mutating a byte in
    // some other field and passing for the wrong reason.
    assert_eq!(COUNT_AT + 4, REQUEST_PREFIX_BYTES);
    assert_eq!(
        &good[POSTS_AT..POSTS_AT + 8],
        [POSTS[0].to_le_bytes(), POSTS[1].to_le_bytes()].concat(),
    );
    assert_eq!(
        good[COUNT_AT..COUNT_AT + 4],
        u32::try_from(job.tiles.len())
            .expect("a small fixture")
            .to_le_bytes(),
    );

    // A non-finite box term. `f64::NAN`'s bits at the site latitude.
    let mut nan_site = good.clone();
    nan_site[..8].copy_from_slice(&f64::NAN.to_le_bytes());
    assert!(decoded(&nan_site).is_none(), "a NaN site decoded");

    // A zero post count: the resample would have nothing to fill.
    let mut no_posts = good.clone();
    no_posts[POSTS_AT..POSTS_AT + 4].copy_from_slice(&0u32.to_le_bytes());
    assert!(decoded(&no_posts).is_none(), "a zero post count decoded");

    // A zero tile size: `assemble` would divide the plane by it.
    let mut no_pixels = good.clone();
    no_pixels[TILE_PX_AT..TILE_PX_AT + 4].copy_from_slice(&0u32.to_le_bytes());
    assert!(decoded(&no_pixels).is_none(), "a zero tile_px decoded");

    // An inverted rectangle: `tx1 < tx0`, which names no tiles at all.
    let mut inverted = good.clone();
    inverted[TX0_AT..TX0_AT + 4].copy_from_slice(&5u32.to_le_bytes());
    inverted[TX1_AT..TX1_AT + 4].copy_from_slice(&4u32.to_le_bytes());
    assert!(decoded(&inverted).is_none(), "an inverted cover decoded");

    // One byte short: the last tile body cannot be taken whole.
    assert!(
        decoded(&good[..good.len() - 1]).is_none(),
        "a truncated body decoded",
    );

    // One byte long. Nothing above this row checks it for this kind -
    // `JobRequest::from_bytes` only tests trailing bytes on the `overlay/`
    // rows - so the row checks its own.
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(decoded(&trailing).is_none(), "a trailing byte decoded");
}

/// A tile count no buffer could hold must not reserve for it.
///
/// The refusal is `Reader::bounded`'s, and the assertion is that the decode
/// answers rather than allocating: a `Vec::with_capacity(u32::MAX)` here would
/// be 51 GiB of headers asked for on a message-port payload.
#[test]
fn a_tile_count_larger_than_the_buffer_is_refused_before_anything_is_reserved() {
    let job = a_height_job();
    let mut bytes = encoded(&job);
    assert!(decoded(&bytes).is_some(), "the control fixture must decode");
    bytes[COUNT_AT..COUNT_AT + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decoded(&bytes).is_none());
}

/// **A cover the tile list does not fill is refused, and so is a geometry that
/// would allocate.**
///
/// Each of these decoded before review. The first two reached
/// `TilePlane::assemble` and died there in a way nothing catches on the web
/// target: `tx1 = u32::MAX - 1` overflowed the plane-size multiply, and
/// `tx1 = ty1 = 65_535` asked the allocator for 1,125,899,906,842,624 bytes,
/// which is an abort and not a panic. `posts` was unbounded the same way, with
/// the cover walk's own cost in front of it.
#[test]
fn a_geometry_that_would_allocate_is_refused_while_it_is_still_bytes() {
    let good = encoded(&a_height_job());
    assert!(decoded(&good).is_some(), "the control fixture must decode");

    // A cover naming more tiles than the list holds. The fixture is one tile,
    // so any widening is now a refusal.
    let mut wide = good.clone();
    wide[TX1_AT..TX1_AT + 4].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
    assert!(decoded(&wide).is_none(), "an unfillable cover decoded");

    let mut square = good.clone();
    square[TX1_AT..TX1_AT + 4].copy_from_slice(&65_535u32.to_le_bytes());
    square[TY1_AT..TY1_AT + 4].copy_from_slice(&65_535u32.to_le_bytes());
    assert!(decoded(&square).is_none(), "a petabyte cover decoded");

    // The cover-to-list equality both ways: one tile too few is a refusal too,
    // not just one too many.
    let mut short = good.clone();
    short[COUNT_AT..COUNT_AT + 4].copy_from_slice(&0u32.to_le_bytes());
    assert!(decoded(&short).is_none(), "a cover with no bodies decoded");

    // The tile side.
    let mut huge_tile = good.clone();
    huge_tile[TILE_PX_AT..TILE_PX_AT + 4].copy_from_slice(&(MAX_TILE_PX + 1).to_le_bytes());
    assert!(decoded(&huge_tile).is_none(), "an oversize tile_px decoded");

    // The post grid, per axis and in total.
    let mut wide_axis = good.clone();
    wide_axis[POSTS_AT..POSTS_AT + 4].copy_from_slice(&(MAX_POSTS_PER_AXIS + 1).to_le_bytes());
    assert!(
        decoded(&wide_axis).is_none(),
        "an oversize post axis decoded"
    );

    let mut many_posts = good.clone();
    many_posts[POSTS_AT..POSTS_AT + 4].copy_from_slice(&MAX_POSTS_PER_AXIS.to_le_bytes());
    many_posts[POSTS_AT + 4..POSTS_AT + 8].copy_from_slice(&MAX_POSTS_PER_AXIS.to_le_bytes());
    assert!(
        decoded(&many_posts).is_none(),
        "an oversize post grid decoded"
    );

    // And the ceilings admit their own limit, so none of the above is passing
    // because the row refuses everything.
    let mut at_the_line = good.clone();
    at_the_line[TILE_PX_AT..TILE_PX_AT + 4].copy_from_slice(&MAX_TILE_PX.to_le_bytes());
    assert!(
        decoded(&at_the_line).is_some(),
        "tile_px exactly at MAX_TILE_PX was refused, so the ceiling is off by one",
    );
    let mut posts_at_the_line = good;
    posts_at_the_line[POSTS_AT..POSTS_AT + 4].copy_from_slice(&MAX_POSTS_PER_AXIS.to_le_bytes());
    assert!(
        decoded(&posts_at_the_line).is_some(),
        "a post axis exactly at MAX_POSTS_PER_AXIS was refused",
    );
}

/// The ceilings sit above the boxes this crate is sized for, with room, and are
/// not so generous that they are decoration.
///
/// `const` blocks rather than runtime assertions: every term is a literal or a
/// constant, so this fails the **build** rather than a test row, which is what
/// a ceiling sanity check should do.
#[test]
fn the_refusal_ceilings_sit_above_the_boxes_this_crate_is_for() {
    // A 920 km box at z10 needs 1056 tiles of 256 px -- the plan's own figure.
    const _: () = assert!(1056 * 256 * 256 < MAX_PLANE_PX as usize);
    // ... and is over half the ceiling, so the ceiling is not decoration.
    const _: () = assert!(2 * 1056 * 256 * 256 > MAX_PLANE_PX as usize);
    // A 921x921 post grid is the 1 km-post form of the same box.
    const _: () = assert!(921 < MAX_POSTS_PER_AXIS);
    const _: () = assert!(921 * 921 < MAX_POSTS_TOTAL);
    const _: () = assert!(5 * 921 * 921 > MAX_POSTS_TOTAL);
    // A real Terrain-RGB tile is 256, and 512 is the largest anyone ships.
    const _: () = assert!(512 < MAX_TILE_PX);

    // The runtime half, which the const block cannot express: the fixture box
    // really is admitted, so the ceilings are not merely arithmetically ordered
    // but actually let this crate's own work through.
    assert!(decoded(&encoded(&a_height_job())).is_some());
}

/// The run really resamples: a field over the fixture's ramp, on the asked-for
/// grid, and **not constant** - a resampler answering one number everywhere
/// would satisfy every shape assertion here.
#[test]
fn the_run_answers_a_field_on_the_boxs_own_posts() {
    let job = a_height_job();
    let field = TerrainHeightJob::run(&job, &bare_geometry()).expect("the fixture resamples");

    assert_eq!(field.site, SITE);
    assert_eq!(field.x_km, X_KM);
    assert_eq!(field.y_km, Y_KM);
    assert_eq!(field.posts, POSTS);
    assert_eq!(field.samples.len(), (POSTS[0] * POSTS[1]) as usize);

    let (lo, hi) = field.range_m().expect("a filled field has a range");
    assert!(
        hi - lo > 1.0,
        "the field runs {lo} m to {hi} m, which is flat enough that a \
         resampler reading one pixel would pass",
    );
    // The ramp is 100 m per pixel eastward over a tile 6 km of box wide, so the
    // whole box is a fraction of a pixel and the heights stay near the ramp's
    // own range. A field built off the sampler's edge clamp, or off the wrong
    // tile, would sit outside it.
    assert!(
        (2000.0..=3100.0).contains(&lo) && (2000.0..=3100.0).contains(&hi),
        "the field runs {lo} m to {hi} m, outside the fixture ramp's own range",
    );
}

/// A tile set that does not cover the box answers **nothing**, not a plausible
/// field: the failure mode the `PlaneDoesNotCoverBox` refusal exists for.
#[test]
fn a_short_tile_set_answers_nothing_rather_than_an_edge_clamped_field() {
    let mut job = a_height_job();
    // A cover naming a rectangle the box is nowhere near, with matching bodies,
    // so `assemble` succeeds and only `resample`'s cover check can catch it.
    job.cover = TileCover {
        zoom: ZOOM,
        tile_px: TILE_PX,
        tx0: 0,
        ty0: 0,
        tx1: 0,
        ty1: 0,
    };
    job.tiles = vec![HeightTile {
        x: 0,
        y: 0,
        png: Arc::new(tile_png(TILE_PX, 2000.0)),
    }];
    assert!(TerrainHeightJob::run(&job, &bare_geometry()).is_none());
}

/// A missing body is a refusal and not a hole filled from its neighbours.
///
/// **The cover is widened first, and that is the point of the test.** The
/// fixture box needs exactly one tile, so popping its only body left a cover
/// with nothing in it — the empty case, not the hole this test is named for.
/// Review caught that. Here the cover names two tiles with two bodies (which
/// runs, as the control below asserts), and then one body is removed, so what
/// is exercised is `MissingTile` beside a neighbour that could have been read
/// instead.
#[test]
fn a_cover_with_a_body_missing_answers_nothing() {
    let mut job = a_height_job();
    let cover = fixture_cover();
    job.cover = TileCover {
        tx1: cover.tx1 + 1,
        ..cover
    };
    job.tiles = job
        .cover
        .addresses()
        .map(|(x, y)| HeightTile {
            x,
            y,
            png: Arc::new(tile_png(TILE_PX, 2000.0)),
        })
        .collect();
    assert_eq!(job.tiles.len(), 2, "the widened cover names two tiles");
    assert!(
        TerrainHeightJob::run(&job, &bare_geometry()).is_some(),
        "the widened control must still resample, or the refusal below is \
         about the widening and not about the hole",
    );

    job.tiles.pop().expect("the widened fixture has two tiles");
    assert!(
        TerrainHeightJob::run(&job, &bare_geometry()).is_none(),
        "a cover with a hole in it produced a field anyway",
    );
}

/// The reply's round trip, head and tail alike.
#[test]
fn the_field_survives_the_reply_wire() {
    let job = a_height_job();
    let field = TerrainHeightJob::run(&job, &bare_geometry()).expect("the fixture resamples");

    let mut head = Vec::new();
    let mut tails = Vec::new();
    TerrainHeightJob::encode_out(field.clone(), &mut head, &mut tails);

    assert_eq!(head.len(), REPLY_HEAD_BYTES, "the reply head moved");
    assert_eq!(tails.len(), 1, "the samples are the row's one tail");
    assert_eq!(tails[0].len(), field.samples.len() * 2);

    assert_eq!(
        TerrainHeightJob::decode_out(&head, tails).as_ref(),
        Some(&field),
    );
}

/// A reply of another shape is refused rather than salvaged.
#[test]
fn a_reply_this_build_did_not_write_is_refused() {
    let job = a_height_job();
    let field = TerrainHeightJob::run(&job, &bare_geometry()).expect("the fixture resamples");
    let mut head = Vec::new();
    let mut tails = Vec::new();
    TerrainHeightJob::encode_out(field, &mut head, &mut tails);

    assert!(
        TerrainHeightJob::decode_out(&head, Vec::new()).is_none(),
        "a reply with no tail decoded",
    );
    let mut two = tails.clone();
    two.push(Vec::new());
    assert!(
        TerrainHeightJob::decode_out(&head, two).is_none(),
        "a reply with a tail this row never wrote decoded",
    );

    let mut short = tails.clone();
    short[0].pop();
    assert!(
        TerrainHeightJob::decode_out(&head, short).is_none(),
        "a sample buffer that is not the declared grid decoded",
    );

    let mut long_head = head.clone();
    long_head.push(0);
    assert!(
        TerrainHeightJob::decode_out(&long_head, tails.clone()).is_none(),
        "a head with a trailing byte decoded",
    );

    // The control: the untouched pair still decodes, so the four refusals above
    // are about what was changed.
    assert!(TerrainHeightJob::decode_out(&head, tails).is_some());
}

/// A height field is numbers: nothing here is a straight-alpha raster for the
/// run funnel to premultiply, and saying so is required rather than defaulted.
#[test]
fn a_height_field_nominates_no_raster_to_premultiply() {
    use squallar_source::job::JobOut;
    let mut field = HeightField {
        site: SITE,
        x_km: X_KM,
        y_km: Y_KM,
        posts: [1, 1],
        samples: vec![7],
    };
    assert!(field.straight_rasters_mut().is_empty());
}

/// The row this crate publishes is one row, under the label the composed
/// registry and the pinned framing both name it by.
#[test]
fn the_registry_is_one_row_called_terrain_heights() {
    assert_eq!(JOB_CODECS.len(), 1);
    assert_eq!(JOB_CODECS[0].label, "terrain/heights");
    assert_eq!(
        (JOB_CODECS[0].input_type)(),
        std::any::TypeId::of::<TerrainHeightJob>(),
    );
}
