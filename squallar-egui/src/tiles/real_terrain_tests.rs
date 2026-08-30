//! **From published-archive bytes to real Colorado heights, through the whole
//! reader.**
//!
//! # What this proves and what it does not
//!
//! B3's stated done-when is "real ground draws from a real archive", and half
//! of that **cannot be met and is not faked here**. No terrain-RGB archive has
//! been published: `HEIGHT_ARCHIVE_URL` still carries
//! [`HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER`](super::HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER),
//! and `the_height_archives_are_still_unpublished` reddens the day a real one
//! is configured. What is proven here is the other half, end to end and with no
//! synthesis anywhere in it:
//!
//! * a **real** Terrain-RGB tile — the committed
//!   `squallar-elevation/testdata/terrain-rgb-z10-210-391.png`, produced by the
//!   shipped builder from the real Copernicus GLO-30 bucket and marked "do not
//!   re-encode" — written into a real PMTiles v3 archive by the same writer the
//!   download engine uses;
//! * served over loopback **in parts**, the layout a published archive is
//!   served in;
//! * read back through the chain the plan names, undecoded, into
//!   [`squallar_elevation::resample`];
//! * resampled onto a volume box's post grid through the same forward
//!   projection `build_voxels` makes the box with;
//! * and arriving as a [`HeightField`](squallar_elevation::HeightField) whose
//!   heights are the Colorado Rockies' own — 2396 m to 4054 m, the range the
//!   fixture's README records an independent decoder reading from the same
//!   bytes.
//!
//! **What is owed to a real archive**: that the published objects exist, that
//! they carry terrain-RGB rather than hillshade pixels at the zooms the box
//! needs, and that a box anywhere but this one degree tile finds tiles at all.
//! Everything between the bytes and the height field is here.
//!
//! **What is owed elsewhere**: the *scheduler*. Nothing in the app yet decides
//! when a pane asks for a field, because there is no archive to ask. The step
//! after this one — a `u16` field becoming drawn, draped terrain — is proven on
//! a real GPU by `squallar-gpu/tests/volume_drape.rs` and
//! `squallar-gpu/tests/volume_occluder.rs`.
//!
//! # Why no child process
//!
//! [`super::height_tests`] re-executes the test binary because it drives
//! [`super::height_archive_url`], which reads the process environment. This file
//! does not: it is about the *bytes*, not about the override, so it spells the
//! same chain against the harness's plain client — the one argument that
//! differs, and the one that decides the scheme rather than the read path.
//! `archive_identity` is what holds the two spellings together.

use squallar_elevation::{HEIGHT_BASE_M, HEIGHT_QUANTUM_M, TilePlane, cover_for};

use crate::basemap_archive::tests as harness;
use crate::basemap_archive::{BasemapArchives, HttpRangeSource, PART_BYTES, TileBytes};

/// The committed real tile's own address: WebMercatorQuad z10, x 210, y 391.
const TILE: (u8, u32, u32) = (10, 210, 391);

/// The tile's own bytes. `include_bytes!` from the elevation crate's testdata,
/// not a re-encode: the README there records why touching a pixel of this file
/// turns metres into kilometres.
const REAL_TILE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../squallar-elevation/testdata/terrain-rgb-z10-210-391.png"
));

/// The heights the fixture's README records an independent decoder reading
/// from these bytes, in metres.
const RECORDED_RANGE_M: (f64, f64) = (2396.2, 4053.7);

/// A site inside the tile's footprint: the tile covers -106.171875 to
/// -105.8203125 longitude and 38.822591 to 39.095963 latitude, so its middle
/// is a radar the box can be centred on.
const SITE: (f64, f64) = (38.959277, -105.99609375);

/// The box's half-extent, kilometres.
///
/// The tile is about 30 km across at this latitude, and the resample reads one
/// pixel outside the outermost post — so a 10 km half-extent keeps the whole
/// cover inside the one tile this archive holds. A larger box would need tiles
/// the fixture does not have, and `TilePlane::assemble` would refuse it, which
/// is the correct behaviour but not the thing under test here.
const HALF_KM: f64 = 10.0;

/// Posts a side. Small: this file is about the bytes reaching real metres, and
/// a 64-post grid over 20 km is a post every 313 m, finer than the tile's own
/// 76 m only in the sense that it does not need to be.
const POSTS: [u32; 2] = [64, 64];

/// A one-tile archive holding the real tile at its real address.
fn real_terrain_archive() -> Vec<u8> {
    let (z, x, y) = TILE;
    let mut sink = std::io::Cursor::new(Vec::new());
    let mut writer = pmtiles::PmTilesWriter::new(pmtiles::TileType::Png)
        // `None` so the bytes the reader hands back are the bytes that went in.
        .tile_compression(pmtiles::Compression::None)
        .min_zoom(z)
        .max_zoom(z)
        .bounds(-180.0, -85.0, 180.0, 85.0)
        .create(&mut sink)
        .expect("the writer opens");
    writer
        .add_tile(
            pmtiles::TileCoord::new(z, x, y).expect("the fixture coordinate is a tile"),
            REAL_TILE,
        )
        .expect("the tile writes");
    writer.finalize().expect("the archive finalizes");
    sink.into_inner()
}

/// **Archive bytes in, Colorado in metres out.**
#[test]
fn a_box_over_the_rockies_resamples_to_real_heights_from_a_served_archive() {
    let body = real_terrain_archive();
    // Parts, and nothing at the bare path: the layout a published archive is
    // served in, so a reader that skipped the probe would 404 rather than pass
    // by accident.
    let server = harness::RangeServer::parted(&body, PART_BYTES as usize);
    let url = server.url();

    let source =
        HttpRangeSource::new(harness::loopback_client(), &url).expect("the archive URL parses");
    let archives: BasemapArchives<_, HttpRangeSource> =
        harness::block_on(BasemapArchives::open(source)).expect("the archive opens");
    assert_eq!(
        archives.tile_type(),
        pmtiles::TileType::Png,
        "a terrain-RGB archive declares PNG bodies",
    );

    // The cover the box needs, from the box — not from the tile. If the
    // resampler's own arithmetic asked for a different rectangle than the one
    // this archive holds, the read below would come back `Absent` and the
    // assertion on it would fire.
    let (z, _, _) = TILE;
    let cover = cover_for(
        SITE,
        (-HALF_KM, HALF_KM),
        (-HALF_KM, HALF_KM),
        POSTS,
        z,
        256,
    )
    .expect("a 20 km box over the Rockies has a cover");
    assert_eq!(
        (cover.tx0, cover.ty0, cover.tx1, cover.ty1),
        (TILE.1, TILE.2, TILE.1, TILE.2),
        "the box's own cover is not the one tile this archive holds, so this \
         test would be measuring the archive's absence rather than its bytes",
    );

    // Every tile the cover names, read undecoded through the archive.
    let mut bodies = Vec::new();
    for (x, y) in cover.addresses() {
        let read = harness::block_on(archives.tile(z, x, y)).expect("the height tile reads");
        let TileBytes::Present(png) = read else {
            panic!("the archive answered Absent for {z}/{x}/{y}, which the cover names");
        };
        assert_eq!(
            png, REAL_TILE,
            "the tile did not round-trip byte for byte, so what is resampled \
             below is not what the builder produced",
        );
        bodies.push((x, y, png));
    }

    // The resample: tiles assembled into one contiguous pixel plane first —
    // per-tile bilinear with a per-tile clamp puts a visible seam at every tile
    // edge — then the forward projection per post.
    let borrowed: Vec<(u32, u32, &[u8])> = bodies
        .iter()
        .map(|(x, y, png)| (*x, *y, png.as_slice()))
        .collect();
    let plane = TilePlane::assemble(cover, &borrowed).expect("the plane assembles");
    let field = plane
        .resample(SITE, (-HALF_KM, HALF_KM), (-HALF_KM, HALF_KM), POSTS)
        .expect("the box resamples");

    assert_eq!(field.posts, POSTS);
    assert_eq!(field.samples.len(), (POSTS[0] * POSTS[1]) as usize);

    // **The heights are the Rockies'.** The range is not asserted equal to the
    // tile's own — a 20 km box does not cover a 30 km tile, so it sees a
    // sub-range — but it must sit inside the recorded one and it must actually
    // be mountains rather than a plane, a clamp or a decode that lost a digit.
    let (low, high) = field.range_m().expect("a field with posts has a range");
    assert!(
        low >= RECORDED_RANGE_M.0 - HEIGHT_QUANTUM_M
            && high <= RECORDED_RANGE_M.1 + HEIGHT_QUANTUM_M,
        "the box resampled to {low:.1}..{high:.1} m, which is outside the \
         {:.1}..{:.1} m an independent decoder read from these very bytes. \
         Terrain-RGB is a base-256 positional number and one count of R is \
         6553.6 m, so a decode that averaged digits or lost one lands here",
        RECORDED_RANGE_M.0,
        RECORDED_RANGE_M.1,
    );
    assert!(
        high - low > 300.0,
        "the box resampled to {low:.1}..{high:.1} m, a relief of {:.1} m over \
         20 km of the Colorado front range. That is a plane, not terrain: a \
         clamped sampler, a plane assembled from the wrong tile, or a resample \
         that read one post everywhere would all look like this",
        high - low,
    );

    // And the encoding survives the trip into the renderer's own carrier.
    // `GroundHeightField` deliberately does not hold a `HeightField` — the
    // renderer's crates do not declare `squallar-elevation` — so the encoding
    // travels as data, and this is where the two are held to being one
    // definition rather than two constants that agree today.
    let carried = crate::volume_view::GroundHeightField {
        id: 0,
        site: field.site,
        x_km: field.x_km,
        y_km: field.y_km,
        posts: field.posts,
        samples: std::sync::Arc::new(field.samples.clone()),
        base_m: HEIGHT_BASE_M,
        quantum_m: HEIGHT_QUANTUM_M,
        range_m: (low, high),
    };
    let decoded = |i: u32, j: u32| {
        let at = (j * carried.posts[0] + i) as usize;
        carried.base_m + f64::from(carried.samples[at]) * carried.quantum_m
    };
    for (i, j) in [(0, 0), (7, 13), (31, 31), (63, 63)] {
        let through_the_carrier = decoded(i, j);
        let through_the_crate = field.height_m(i, j).expect("a post inside the field");
        assert!(
            (through_the_carrier - through_the_crate).abs() < 1e-9,
            "post ({i}, {j}) decodes to {through_the_carrier} m through the \
             renderer's carrier and {through_the_crate} m through the \
             elevation crate. The carrier exists so the encoding travels as \
             data with one definition; two that disagree is a terrain drawn at \
             the wrong height with nothing to notice",
        );
    }
}
