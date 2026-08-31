//! **The radar network's coverage is pixels, and this is what says so.**
//!
//! Everything else about this layer was pinned — that it describes a job, that
//! the job round-trips the wire, that the handler answers `has_data` — and none
//! of it looks at the picture. The failure that got through was a map drawing
//! site *labels* (per-frame egui text, straight off the table) with no ink at
//! all under them, which every one of those pins reads as green.
//!
//! **What this raster carries is ground, and only ground.** Two things have now
//! left it for the per-frame painter, both for the same reason — they are
//! lengths in points and this picture is placed by its geographic corners, so
//! the map stretches whatever is baked into it. First the station marker, which
//! a two-level pinch ran up to four times its size and snapped back half a
//! second after the zoom stopped. Then the selected station's ring, which is
//! selection feedback and cannot wait for a whole-picture round trip. What
//! stayed is the network's 230 km coverage, because 230 km is 230 km and
//! *should* scale with the map.
//!
//! **The three fills are gone from here and are not gone.** Blue/red/purple —
//! "a radar", "the radar this pane is on", "the radar it is loading" — is
//! screen-space vocabulary now, pinned in
//! `squallar_egui::site_marker::tests::the_three_marker_roles_are_three_colours`.
//! Nothing in this raster depends on which station a pane is on; `CoverageSite`
//! carries a position and nothing else, so a fill that varied by role could not
//! be spelled here.
//!
//! `RadarCoverage` is also the Tier-2 rig's `--expect-overlay-rasters` vehicle —
//! the one texture overlay that is off by default and reproducible with no
//! upstream — so what is asserted here is what the rig measures the pipeline
//! with.
//!
//! Positions are the real ones from `api.weather.gov/radar/stations`, the same
//! catalogue the app fetches.

use super::*;

/// `KTLX` Twin Lakes, `KINX` Tulsa, `KVNX` Vance AFB — three WSR-88Ds inside
/// one southern-Plains viewport.
const KTLX: (f64, f64) = (35.3331, -97.2775);
const KINX: (f64, f64) = (36.1750, -95.5650);
const KVNX: (f64, f64) = (36.7410, -98.1280);

/// A viewport with all three inside it, comfortably off every edge.
fn viewport() -> GeoBounds {
    GeoBounds {
        max_lat: 38.0,
        min_lat: 34.0,
        min_lon: -100.0,
        max_lon: -94.0,
    }
}

const W: u32 = 400;
const H: u32 = 300;

fn site((lat, lon): (f64, f64)) -> CoverageSite {
    CoverageSite { lat, lon }
}

fn input(sites: Vec<CoverageSite>) -> CoverageInput {
    CoverageInput {
        sites,
        device_scale: 1.0,
    }
}

/// Texels with any ink on them at all.
fn painted(out: &RasterizeOutput) -> usize {
    out.rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
}

/// Texels that are the region's **edge** rather than its wash. The wash goes
/// down at alpha 38 and the edge at 160 over it; nothing else here reaches the
/// eighties.
fn edge(out: &RasterizeOutput) -> usize {
    out.rgba.chunks_exact(4).filter(|px| px[3] > 80).count()
}

/// **The floor one drawn station has to clear.** The viewport is 6 degrees of
/// longitude across 400 px, so 230 km is tens of pixels of radius and the
/// region's edge is hundreds of pixels of perimeter. Fifty is far below what a
/// drawn disc produces and far above what a stray anti-aliased pixel could fake
/// — a disc that regressed to a dot, or to a single pixel, would fail it.
const EDGE_FLOOR: usize = 50;

/// Where the ink is, relative to the antenna: `(painted inside a quarter of the
/// coverage radius, painted in a 2 px annulus at the coverage radius, painted
/// beyond 1.3 coverage radii)`.
fn distance_bands(out: &RasterizeOutput, at: (f64, f64)) -> (usize, usize, usize) {
    let mb = MercatorBounds::from_geo(&viewport());
    let (cx, cy) = mb.project(at.0, at.1, W as f32, H as f32);
    let (_, north) = mb.project(
        at.0 + super::COVERAGE_RADIUS_DEG_LAT,
        at.1,
        W as f32,
        H as f32,
    );
    let ring_r = (cy - north).abs();

    let (mut inner, mut on_ring, mut beyond) = (0usize, 0usize, 0usize);
    for (i, px) in out.rgba.chunks_exact(4).enumerate() {
        if px[3] == 0 {
            continue;
        }
        let (x, y) = ((i as u32 % W) as f32, (i as u32 / W) as f32);
        let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        if d < ring_r * 0.25 {
            inner += 1;
        } else if (d - ring_r).abs() <= 2.0 {
            on_ring += 1;
        } else if d > ring_r * 1.3 {
            beyond += 1;
        }
    }
    (inner, on_ring, beyond)
}

/// **The property this raster exists for: the ink is a 230 km disc.**
///
/// Filled to the antenna, edged at the coverage radius, and *stopping* there.
/// The third band is the one that makes this a measurement of the radius rather
/// than of "something painted": a rasterizer that washed the whole texture
/// would clear the first two and fail the third.
#[test]
fn a_stations_ink_is_a_filled_disc_that_ends_at_its_coverage_radius() {
    let out = rasterize_radar_coverage(&input(vec![site(KTLX)]), &viewport(), W, H);

    let (inner, on_ring, beyond) = distance_bands(&out, KTLX);
    assert!(
        on_ring >= EDGE_FLOOR,
        "the station painted {on_ring} texels at its coverage radius; a drawn \
         disc has hundreds on its edge, so this layer is not putting coverage \
         on the map",
    );
    assert!(
        inner > 0,
        "the ground beside the antenna came back empty; coverage is the disc, \
         not an outline of it",
    );
    assert_eq!(
        beyond, 0,
        "the station painted {beyond} texels more than 1.3 coverage radii out. \
         The disc is 230 km and must end there, or this is a wash over the \
         whole viewport rather than a statement about range",
    );
}

/// **The picture cannot depend on the map zoom, and that is now structural.**
///
/// [`CoverageInput`] carries positions and a device scale. There is no zoom in
/// it to vary, so the raster this module used to check across five zoom levels
/// is one the type system will not let a caller ask for differently. What is
/// left to check is the half that is still expressible: the same input is the
/// same bytes.
#[test]
fn the_same_stations_rasterize_to_the_same_bytes() {
    let once = rasterize_radar_coverage(&input(vec![site(KTLX)]), &viewport(), W, H);
    let twice = rasterize_radar_coverage(&input(vec![site(KTLX)]), &viewport(), W, H);
    assert_eq!(
        once.rgba, twice.rgba,
        "this is the Tier-2 rig's deterministic vehicle; two rasterizations of \
         one input must be one picture",
    );
}

#[test]
fn every_station_in_view_contributes_its_coverage() {
    let one = rasterize_radar_coverage(&input(vec![site(KTLX)]), &viewport(), W, H);
    let three = rasterize_radar_coverage(
        &input(vec![site(KTLX), site(KINX), site(KVNX)]),
        &viewport(),
        W,
        H,
    );

    assert!(
        edge(&one) >= EDGE_FLOOR,
        "one radar in view painted {} edge texels; a drawn disc is hundreds, so \
         the map is drawing site labels with nothing under them",
        edge(&one),
    );
    assert!(
        painted(&three) > painted(&one),
        "three radars covered {} texels against one radar's {}; the extra two \
         stations reach ground the first does not, so the region must grow",
        painted(&three),
        painted(&one),
    );
}

/// The uncooperative control: hand the same viewport no rows and the picture
/// must be empty. Without this, a rasterizer that washed the whole texture
/// would pass the tests above.
#[test]
fn an_empty_table_paints_nothing_at_all() {
    let out = rasterize_radar_coverage(&input(Vec::new()), &viewport(), W, H);
    assert!(
        out.rgba.iter().all(|&b| b == 0),
        "an empty site list still put ink on the texture",
    );
}

/// And the other control: a viewport nowhere near these radars draws nothing,
/// so the counts above are a function of where the radars are and not of the
/// fixture merely being non-empty.
#[test]
fn radars_outside_the_viewport_paint_nothing() {
    let out = rasterize_radar_coverage(
        &input(vec![site(KTLX), site(KINX), site(KVNX)]),
        &GeoBounds {
            max_lat: 48.0,
            min_lat: 44.0,
            min_lon: -76.0,
            max_lon: -70.0,
        },
        W,
        H,
    );
    assert_eq!(
        painted(&out),
        0,
        "a New England viewport drew Oklahoma's radars",
    );
}

/// **A radar off the texture still covers ground on it**, which a cull on the
/// antenna could not express: 230 km is a long way.
#[test]
fn a_radar_just_off_the_texture_still_paints_the_ground_it_covers() {
    // KTLX's latitude, moved 1.2 degrees west of the viewport's western edge:
    // outside the picture, well inside its own coverage of it.
    let out = rasterize_radar_coverage(&input(vec![site((35.3331, -101.2))]), &viewport(), W, H);
    assert!(
        painted(&out) > 0,
        "a radar 1.2 degrees west of the viewport covers ground inside it and \
         must paint the arc that lands",
    );
}
