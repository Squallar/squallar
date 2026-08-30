//! The resample against an analytic oracle, and the three pins that hold this
//! crate's re-spelled Terrain-RGB constants to the builder's.
//!
//! **The oracle alone would be worthless.** Synthesise tiles carrying a known
//! `h = f(lat, lon)`, resample, and assert every post lands within a quantum,
//! and a resampler using the *wrong* projection passes too — because the same
//! wrong projection was used to decide where the truth was. So the oracle is
//! written with the truth computed from the great-circle post position
//! independently of the crate, and then run a second time through an
//! equirectangular box→geo map, where it must **fail**:
//! `the_equirectangular_twin_fails_the_same_assertion_it_was_written_against`
//! asserts the failure rather than noting it.
//!
//! Measured on this work unit's tree, 2026-08-30, by forcing each assertion
//! red once and reading the reported figure: the correct projection's worst
//! post error is **0.207 m against the 0.250 m budget**, and the
//! equirectangular twin's is **70.2 m** — 281× the same budget. The budget is
//! therefore tight rather than slack, and the gap between the two arms is three
//! orders of magnitude wide.
//!
//! No GPU, no network, no fixtures beyond one committed tile: this is a plain
//! `cargo test` row.

use std::f64::consts::PI;

use squallar_elevation::height::{HEIGHT_QUANTUM_M, decode_height_m, encode_height_m};
use squallar_elevation::resample::{TileCover, TilePlane, cover_for, post_center_km};
use squallar_elevation::trgb;

// ---------------------------------------------------------------------------
// Pin 1 — the constants and `unpack` are the builder's, verbatim.

/// The builder's own source, compiled in. `tools/squallar-terrain` is a
/// separate Cargo workspace, so its `trgb` module cannot be `use`d; this reads
/// its text instead, and a move of the file is a compile error here.
const BUILDER_TRGB: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tools/squallar-terrain/src/trgb.rs"
));

/// This crate's own copy, read the same way so the comparison is text-to-text.
const OUR_TRGB: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/trgb.rs"));

/// Whitespace-collapsed, so a rustfmt line break is not a difference.
fn collapsed(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `pub fn unpack(` block, through the first line that is exactly `}`.
fn unpack_block(src: &str) -> String {
    let start = src
        .find("pub fn unpack(")
        .expect("both files define `pub fn unpack(`");
    let rest = &src[start..];
    let end = rest
        .find("\n}\n")
        .expect("`unpack` is closed by a line holding only `}`");
    collapsed(&rest[..end + 3])
}

/// One line, collapsed, or a panic naming what was missing.
fn line_with(src: &str, needle: &str) -> String {
    let line = src
        .lines()
        .find(|l| l.trim_start().starts_with(needle))
        .unwrap_or_else(|| panic!("no line starts with {needle:?}"));
    collapsed(line)
}

#[test]
fn the_constants_match_the_builders_source_text() {
    for needle in [
        "pub const QUANTUM_M",
        "pub const BASE_M",
        "pub const MAX_PACKED",
    ] {
        assert_eq!(
            line_with(OUR_TRGB, needle),
            line_with(BUILDER_TRGB, needle),
            "{needle} has drifted from tools/squallar-terrain/src/trgb.rs",
        );
    }
    assert_eq!(
        unpack_block(OUR_TRGB),
        unpack_block(BUILDER_TRGB),
        "`unpack` has drifted from tools/squallar-terrain/src/trgb.rs",
    );

    // Falsifiability floor: the extraction really found the code, so an
    // extractor that silently returned two empty strings cannot pass.
    let ours = unpack_block(OUR_TRGB);
    assert!(
        ours.len() > 80 && ours.contains("BASE_M") && ours.contains("<< 16"),
        "the unpack extractor produced {ours:?}",
    );
    assert!(
        line_with(OUR_TRGB, "pub const BASE_M").contains("-10_000.0"),
        "the constant extractor produced the wrong line",
    );

    // And the builder's `pack` stays where it is: an uncalled encoder here
    // would be a second definition with no caller to notice it drifting.
    assert!(
        BUILDER_TRGB.contains("pub fn pack("),
        "the builder no longer defines `pack`; this pin is reading the wrong file",
    );
    assert!(
        !OUR_TRGB.contains("pub fn pack("),
        "squallar-elevation has grown a `pack`; nothing in the app encodes a height",
    );
}

// ---------------------------------------------------------------------------
// Pin 2 — the nine base-256 carries.

/// The builder's `pack`, restated **for fixture synthesis only**.
///
/// This is not a second production encoder: nothing outside this file calls it,
/// and `the_base_two_hundred_and_fifty_six_carries_are_exact` walks it against
/// the crate's `unpack` over every channel boundary, so a fixture generator
/// that had drifted could not leave the oracle green.
fn pack_for_fixture(height: f64) -> [u8; 3] {
    if height.is_nan() {
        return [0, 0, 0];
    }
    let v = ((height - trgb::BASE_M) * 10.0)
        .round()
        .clamp(0.0, f64::from(trgb::MAX_PACKED)) as u32;
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// One count of R is 6553.6 m, so a carry dropped between the digits is a
/// catastrophic error rather than a soft one. This walks each of them.
#[test]
fn the_base_two_hundred_and_fifty_six_carries_are_exact() {
    for v in [
        0u32, 255, 256, 257, 65_535, 65_536, 65_537, 16_777_214, 16_777_215,
    ] {
        let h = trgb::BASE_M + f64::from(v) * trgb::QUANTUM_M;
        let rgb = pack_for_fixture(h);
        let got = (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
        assert_eq!(got, v, "{h} m packed as {rgb:?}");
        assert!(
            (trgb::unpack(rgb) - h).abs() <= 1e-6,
            "{v} did not survive the round trip"
        );
    }
    assert_eq!(trgb::MAX_PACKED, 16_777_215);
}

// ---------------------------------------------------------------------------
// Pin 3 — a tile the builder actually produced.

/// Colorado, z10/210/391, produced 2026-08-30 by
/// `RASTER_ENCODING=terrain-rgb RASTER_MINZOOM=10 RASTER_MAXZOOM=10 SUPERCELL=1
/// ONLY_SUPERCELL=sc_z10_000210_000391 squallar-terrain build raster` against
/// the real Copernicus GLO-30 bucket. See `testdata/README.md`; the file must
/// not be re-encoded or optimised.
const REAL_TILE: &[u8] = include_bytes!("../testdata/terrain-rgb-z10-210-391.png");

#[test]
fn the_committed_real_tile_decodes_to_its_recorded_heights() {
    // Byte length first: a PNG optimiser run over the tree would change this
    // before it changed any decoded value, and the point of the fixture is that
    // the bytes are the builder's.
    assert_eq!(
        REAL_TILE.len(),
        96_049,
        "testdata/terrain-rgb-z10-210-391.png has been re-encoded; see its README"
    );

    let cover = TileCover {
        zoom: 10,
        tile_px: 256,
        tx0: 210,
        ty0: 391,
        tx1: 210,
        ty1: 391,
    };
    let plane = TilePlane::assemble(cover, &[(210, 391, REAL_TILE)]).expect("the tile decodes");
    assert_eq!(plane.size_px(), (256, 256));

    // Recorded 2026-08-30 by an independent decode (PIL + numpy) of the same
    // bytes: `h = -10000 + (R<<16 | G<<8 | B) * 0.1`.
    for (col, row, expect) in [
        (0u32, 0u32, 3693.7_f64),
        (255, 0, 2743.6),
        (0, 255, 2489.3),
        (255, 255, 2828.0),
        (128, 128, 2810.0),
        (192, 64, 2765.5),
    ] {
        let got = plane.pixel_m(col, row).expect("an in-range pixel");
        // 1e-3 m and not 1e-6: the plane stores metres as `f32`, which is
        // ~2.4e-4 m at these heights — three orders inside the 0.1 m encoding
        // quantum, so a re-encode of the fixture still fails this.
        assert!(
            (got - expect).abs() < 1e-3,
            "pixel ({col},{row}) is {got} m, recorded {expect} m"
        );
    }

    let mut lo = f64::MAX;
    let mut hi = f64::MIN;
    for row in 0..256 {
        for col in 0..256 {
            let h = plane.pixel_m(col, row).expect("an in-range pixel");
            lo = lo.min(h);
            hi = hi.max(h);
        }
    }
    assert!((lo - 2396.2).abs() < 1e-3, "min is {lo} m, recorded 2396.2");
    assert!((hi - 4053.7).abs() < 1e-3, "max is {hi} m, recorded 4053.7");

    // The fixture is real terrain and not a flat or degenerate image, so the
    // pins above are reading something.
    assert!(
        hi - lo > 1000.0,
        "the fixture carries only {} m of relief",
        hi - lo
    );
    assert_eq!(plane.cover(), cover);
}

/// A tile that is not 8-bit RGB is refused rather than silently converted: a
/// 16-bit PNG squeezed into `Rgb8` decodes to heights that look plausible and
/// are wrong by kilometres.
#[test]
fn a_tile_that_is_not_eight_bit_rgb_is_refused() {
    let grey = image::DynamicImage::ImageLuma8(image::GrayImage::new(256, 256));
    let mut png = Vec::new();
    grey.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the encoder runs");
    let cover = TileCover {
        zoom: 10,
        tile_px: 256,
        tx0: 210,
        ty0: 391,
        tx1: 210,
        ty1: 391,
    };
    let err = TilePlane::assemble(cover, &[(210, 391, &png)]).expect_err("greyscale is refused");
    assert!(
        matches!(
            err,
            squallar_elevation::ElevationError::NotEightBitRgb { .. }
        ),
        "got {err:?}"
    );

    // Control: the same cover with the real RGB tile is accepted, so the
    // refusal above is about the colour type and not about the cover.
    assert!(TilePlane::assemble(cover, &[(210, 391, REAL_TILE)]).is_ok());
}

#[test]
fn a_cover_with_a_tile_missing_is_an_error_and_not_a_hole() {
    let cover = TileCover {
        zoom: 10,
        tile_px: 256,
        tx0: 210,
        ty0: 391,
        tx1: 211,
        ty1: 391,
    };
    let err = TilePlane::assemble(cover, &[(210, 391, REAL_TILE)]).expect_err("one tile short");
    assert_eq!(
        err,
        squallar_elevation::ElevationError::MissingTile { x: 211, y: 391 }
    );
}

// ---------------------------------------------------------------------------
// The analytic field, and the tiles that carry it.

/// The site the oracle is built around: KFTG's neighbourhood, mid-latitude and
/// well away from the antimeridian.
const SITE: (f64, f64) = (39.0, -106.0);

/// Half a default box: 920 km across, which is the span the plan's ">5 km at
/// the corners" figure is quoted for.
const HALF_KM: f64 = 460.0;

/// The plane is assembled over a wider box so the equirectangular twin's posts
/// — up to 31 km off the great-circle ones — still land on real pixels rather
/// than on the sampler's edge clamp.
const PLANE_HALF_KM: f64 = 520.0;

const POSTS: [u32; 2] = [129, 129];

/// Coarse enough that nine tiles cover a 1040 km box, which is what keeps this
/// a fast plain `cargo test`. The resample does not know the zoom.
const ZOOM: u8 = 6;
const TILE_PX: u32 = 256;

const FIELD_BASE_M: f64 = 1500.0;
/// Metres per radian of Web Mercator `y` — exactly linear in the plane's pixel
/// row, so it contributes no bilinear error at all.
const MERC_SLOPE_M: f64 = 1500.0;
/// Metres per degree of longitude — exactly linear in the plane's pixel column,
/// for the same reason. 200 m/° is 1.8 m/km, which is what makes a projection
/// error of a few kilometres show up as metres of height.
const LON_SLOPE_M_PER_DEG: f64 = 200.0;
/// The bump that stops the field being degenerate: a resampler that only got
/// the affine part right would pass without it.
const BUMP_M: f64 = 800.0;
/// Its width, in radians of Mercator `y` and of longitude alike — about 100 km.
const BUMP_SIGMA: f64 = 0.02;
const BUMP_OFFSET_U: f64 = 0.03;
const BUMP_OFFSET_V: f64 = -0.04;

/// Web Mercator `y`, re-derived here rather than taken from the crate under
/// test, so the oracle does not agree with the resampler by construction.
fn merc_y(lat_deg: f64) -> f64 {
    (PI / 4.0 + lat_deg.to_radians() / 2.0).tan().ln()
}

/// `h = f(lat, lon)`: a plane in Mercator `y` and longitude, plus a Gaussian.
fn analytic_h(lat: f64, lon: f64) -> f64 {
    let du = merc_y(lat) - merc_y(SITE.0);
    let dv = (lon - SITE.1).to_radians();
    let (bu, bv) = (du - BUMP_OFFSET_U, dv - BUMP_OFFSET_V);
    FIELD_BASE_M
        + MERC_SLOPE_M * du
        + LON_SLOPE_M_PER_DEG * (lon - SITE.1)
        + BUMP_M * (-(bu * bu + bv * bv) / (2.0 * BUMP_SIGMA * BUMP_SIGMA)).exp()
}

/// The lat/lon of the centre of one global Web Mercator pixel.
fn pixel_center_geo(gx: u32, gy: u32) -> (f64, f64) {
    let world = f64::from(TILE_PX) * 2f64.powi(i32::from(ZOOM));
    let lon = (f64::from(gx) + 0.5) / world * 360.0 - 180.0;
    let merc = PI * (1.0 - 2.0 * (f64::from(gy) + 0.5) / world);
    (merc.sinh().atan().to_degrees(), lon)
}

/// One Terrain-RGB tile carrying [`analytic_h`] at every pixel centre.
fn synth_tile(tx: u32, ty: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(TILE_PX, TILE_PX, |col, row| {
        let (lat, lon) = pixel_center_geo(tx * TILE_PX + col, ty * TILE_PX + row);
        image::Rgb(pack_for_fixture(analytic_h(lat, lon)))
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the encoder runs");
    png
}

/// The plane covering [`PLANE_HALF_KM`] about [`SITE`].
fn synth_plane() -> TilePlane {
    let half = (-PLANE_HALF_KM, PLANE_HALF_KM);
    let cover = cover_for(SITE, half, half, POSTS, ZOOM, TILE_PX).expect("a Colorado box covers");
    let bodies: Vec<(u32, u32, Vec<u8>)> = cover
        .addresses()
        .map(|(x, y)| (x, y, synth_tile(x, y)))
        .collect();
    let borrowed: Vec<(u32, u32, &[u8])> = bodies
        .iter()
        .map(|(x, y, b)| (*x, *y, b.as_slice()))
        .collect();
    TilePlane::assemble(cover, &borrowed).expect("every synthesised tile decodes")
}

/// The error budget the oracle asserts against, term by term.
///
/// * the Terrain-RGB quantum, half of it, and bilinear over four quantised
///   corners is a convex combination so it cannot exceed one corner's error;
/// * the height field's own quantum, half of it;
/// * bilinear's own error. The affine part of [`analytic_h`] is exactly linear
///   in the plane's pixel coordinates and contributes nothing, so the bound is
///   the Gaussian's: `max|f''| / 4` on a unit grid, with `max|f''| = A/σ²` and
///   `σ` in pixels;
/// * slack for the plane's `f32` storage, which is ~2.4e-4 m at these heights.
fn tolerance_m() -> f64 {
    let world = f64::from(TILE_PX) * 2f64.powi(i32::from(ZOOM));
    let sigma_px = BUMP_SIGMA * world / (2.0 * PI);
    let bilinear = BUMP_M / (4.0 * sigma_px * sigma_px);
    trgb::QUANTUM_M / 2.0 + HEIGHT_QUANTUM_M / 2.0 + bilinear + 1e-3
}

/// The naive box→geo map: degrees per kilometre about the site, latitude
/// scaled by `cos φ₀`. This is the "anything simpler" the design refuses.
fn equirectangular(x_km: f64, y_km: f64) -> (f64, f64) {
    let lat = SITE.0 + y_km / squallar_geo::KM_PER_DEGREE_LAT;
    let lon = SITE.1 + x_km / (squallar_geo::KM_PER_DEGREE_LAT * SITE.0.to_radians().cos());
    (lat, lon)
}

#[test]
fn the_resample_matches_the_analytic_oracle() {
    let plane = synth_plane();
    let half = (-HALF_KM, HALF_KM);
    let field = plane
        .resample(SITE, half, half, POSTS)
        .expect("a non-empty post grid");

    let tol = tolerance_m();
    let mut worst = 0.0f64;
    let mut worst_at = (0u32, 0u32);
    for j in 0..POSTS[1] {
        for i in 0..POSTS[0] {
            // The truth is taken at the post's TRUE position, computed here
            // from the plan's formula rather than from the crate.
            let (x, y) = post_center_km(half, half, POSTS, i, j);
            let (lat, lon) = squallar_geo::great_circle_destination(
                SITE.0,
                SITE.1,
                x.atan2(y).to_degrees(),
                x.hypot(y),
            );
            let truth = analytic_h(lat, lon);
            let got = field.height_m(i, j).expect("an in-range post");
            let err = (got - truth).abs();
            if err > worst {
                worst = err;
                worst_at = (i, j);
            }
        }
    }
    assert!(
        worst <= tol,
        "worst post error {worst} m at {worst_at:?} exceeds the {tol} m budget"
    );

    // Non-triviality on the field itself: it has to carry real relief, or a
    // resampler returning a constant would sit inside the budget.
    let (lo, hi) = field.range_m().expect("a non-empty field");
    assert!(
        hi - lo > 1500.0,
        "the synthesised field carries only {} m of relief",
        hi - lo
    );
    // And the bump must actually be in the box, or only the affine part is
    // under test.
    let mut saw_bump = false;
    for j in 0..POSTS[1] {
        for i in 0..POSTS[0] {
            let (x, y) = post_center_km(half, half, POSTS, i, j);
            let (lat, lon) = squallar_geo::great_circle_destination(
                SITE.0,
                SITE.1,
                x.atan2(y).to_degrees(),
                x.hypot(y),
            );
            let du = merc_y(lat) - merc_y(SITE.0) - BUMP_OFFSET_U;
            let dv = (lon - SITE.1).to_radians() - BUMP_OFFSET_V;
            if du.hypot(dv) < BUMP_SIGMA / 2.0 {
                saw_bump = true;
            }
        }
    }
    assert!(saw_bump, "no post landed inside the Gaussian's core");
}

/// The non-triviality half, and the reason the oracle above is worth anything.
///
/// The identical assertion is run against an equirectangular box→geo map. It
/// must fail, and it must fail by a wide margin — not by a quantum.
#[test]
fn the_equirectangular_twin_fails_the_same_assertion_it_was_written_against() {
    let plane = synth_plane();
    let half = (-HALF_KM, HALF_KM);
    let tol = tolerance_m();

    let mut worst_height = 0.0f64;
    let mut worst_km = 0.0f64;
    let mut worst_corner_km = 0.0f64;
    for j in 0..POSTS[1] {
        for i in 0..POSTS[0] {
            let (x, y) = post_center_km(half, half, POSTS, i, j);
            let (true_lat, true_lon) = squallar_geo::great_circle_destination(
                SITE.0,
                SITE.1,
                x.atan2(y).to_degrees(),
                x.hypot(y),
            );
            let (eq_lat, eq_lon) = equirectangular(x, y);

            // The same assertion, with the wrong map deciding where to sample.
            let sampled = decode_height_m(encode_height_m(plane.sample_height_m(eq_lat, eq_lon)));
            worst_height = worst_height.max((sampled - analytic_h(true_lat, true_lon)).abs());

            let (_, off_km) =
                squallar_geo::site_bearing_range_km(true_lat, true_lon, eq_lat, eq_lon);
            worst_km = worst_km.max(off_km);
            let corner = i == 0 || j == 0 || i == POSTS[0] - 1 || j == POSTS[1] - 1;
            if corner && (i == 0 || i == POSTS[0] - 1) && (j == 0 || j == POSTS[1] - 1) {
                worst_corner_km = worst_corner_km.max(off_km);
            }
        }
    }

    assert!(
        worst_corner_km > 5.0,
        "the equirectangular map is only {worst_corner_km} km out at the corners of a \
         {} km box; the oracle would then pass for the wrong projection",
        HALF_KM * 2.0
    );
    assert!(
        worst_height > tol,
        "the equirectangular map's worst height error is {worst_height} m, inside the \
         {tol} m budget — the oracle cannot tell the two projections apart"
    );
    // Not merely outside the budget: an order of magnitude outside it, so this
    // is a projection difference and not a rounding one.
    assert!(
        worst_height > 20.0 * tol,
        "the equirectangular map's worst height error is only {worst_height} m against a \
         {tol} m budget"
    );
    // Recorded, not asserted to a digit: 30.8 km at the corners and ~31 km
    // worst overall at 39°N/106°W with HALF_KM = 460.
    assert!(worst_km >= worst_corner_km);
}

// ---------------------------------------------------------------------------
// The seam.

/// A post landing exactly on a tile boundary reads the same height whichever
/// side it is approached from — because the tiles were assembled first.
///
/// The twin half is what gives this teeth: the same point sampled from each
/// tile *alone*, with that tile's own edge clamp, lands metres apart. That is
/// the seam this crate's design point 2 exists to remove.
#[test]
fn a_post_on_a_tile_boundary_reads_the_same_height_from_either_side() {
    let world = f64::from(TILE_PX) * 2f64.powi(i32::from(ZOOM));
    // The tile pair straddling the site's meridian.
    let tx = (((SITE.1 + 180.0) / 360.0) * 2f64.powi(i32::from(ZOOM))).floor() as u32;
    let ty = squallar_geo::lat_to_tile_y(SITE.0, ZOOM);

    // The shared edge: the longitude whose global pixel x is exactly the
    // boundary between tile `tx` and tile `tx + 1`.
    let edge_lon = f64::from((tx + 1) * TILE_PX) / world * 360.0 - 180.0;
    let (lat, _) = pixel_center_geo(0, ty * TILE_PX + TILE_PX / 2);

    let both = TileCover {
        zoom: ZOOM,
        tile_px: TILE_PX,
        tx0: tx,
        ty0: ty,
        tx1: tx + 1,
        ty1: ty,
    };
    let left_png = synth_tile(tx, ty);
    let right_png = synth_tile(tx + 1, ty);
    let assembled = TilePlane::assemble(both, &[(tx, ty, &left_png), (tx + 1, ty, &right_png)])
        .expect("both tiles decode");

    let one = |x: u32, png: &[u8]| {
        TilePlane::assemble(
            TileCover {
                zoom: ZOOM,
                tile_px: TILE_PX,
                tx0: x,
                ty0: ty,
                tx1: x,
                ty1: ty,
            },
            &[(x, ty, png)],
        )
        .expect("one tile decodes")
    };
    let left_only = one(tx, &left_png);
    let right_only = one(tx + 1, &right_png);

    let truth = analytic_h(lat, edge_lon);
    let seamless = assembled.sample_height_m(lat, edge_lon);
    let from_left = left_only.sample_height_m(lat, edge_lon);
    let from_right = right_only.sample_height_m(lat, edge_lon);

    let tol = tolerance_m();
    assert!(
        (seamless - truth).abs() <= tol,
        "the assembled plane reads {seamless} m at the boundary against {truth} m"
    );

    // The twin: per-tile clamping holds each side at its own edge pixel's
    // centre, so the two answers sit one whole pixel of gradient apart. At this
    // zoom that is `360/world` degrees of longitude times the field's
    // longitudinal slope — the seam's size is predicted, not merely observed.
    let expected_gap = 360.0 / world * LON_SLOPE_M_PER_DEG;
    let gap = (from_left - from_right).abs();
    assert!(
        (gap - expected_gap).abs() < 0.25 * expected_gap,
        "per-tile sampling disagrees by {gap} m; one pixel of this field's \
         longitudinal gradient is {expected_gap} m, so the seam is not being \
         reproduced by the mechanism this test claims"
    );
    assert!(
        gap > 10.0 * tol,
        "per-tile sampling disagrees by only {gap} m against a {tol} m budget; \
         the assertion above is not measuring anything"
    );
    assert!(
        (from_left - truth).abs() > tol && (from_right - truth).abs() > tol,
        "per-tile sampling landed inside the budget ({from_left} / {from_right} \
         against {truth}); the seam is not being reproduced"
    );
}
