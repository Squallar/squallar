//! Why a zone's boundary goes missing, pinned against the geometry the NWS
//! actually serves.
//!
//! [`ZoneFailure`] could say *how many* boundaries did not arrive and *what
//! kind* of failure each was, and the answer to "so why does this happen"
//! turned out to be that two of the four kinds were us.
//!
//! Measured over every one of the **11,651 published NWS zones**, fetched once
//! from `api.weather.gov` at 43 req/s, plus two full live alert rounds of 1,791
//! and 1,806 zones at production concurrency:
//!
//! | cause | zones | transient? |
//! |---|---|---|
//! | `Http`, `Unreachable`, `Unreadable` | **0** | — nothing failed in transport, twice |
//! | `NoBoundary`, geometry is a `GeometryCollection` | **227** | no: a permanent property of those zones |
//! | `NoBoundary`, every ring simplified away | **6** | no: our simplifier, every time |
//!
//! and beneath the accounting entirely, invisible to it because these zones
//! *did* resolve: **26,963 of 44,579 polygon parts** came out with an exterior
//! ring that draws nothing — 20,778 dropped below three points and 6,185 kept
//! as zero-area out-and-backs.
//!
//! The three fixtures below are the three shapes of that, copied verbatim from
//! live responses. Nothing here is synthesised; a degenerate polygon anyone can
//! construct proves nothing about whether this fires on real data, which was
//! the whole question.
//!
//! [`ZoneFailure`]: super::ZoneFailure

use super::parse_zone_polygons;
use crate::render::geo::simplify_ring;
use crate::types::SIMPLIFY_EPSILON;
use rustdar_geo::{GeoPolygon, GeoPolygonRing};

/// Verbatim `geometry` members from `api.weather.gov/zones/{kind}/{id}`.
const ZONE_GEOMETRY: &str = include_str!("../../../testdata/nws_zone_geometry.json");

fn fixture(key: &str) -> serde_json::Value {
    let all: serde_json::Value =
        serde_json::from_str(ZONE_GEOMETRY).expect("zone geometry fixture must parse");
    all.get(key)
        .unwrap_or_else(|| panic!("fixture must carry a `{key}` zone"))
        .clone()
}

/// The whole bare Feature the zones API returns is what `parse_zone_polygons`
/// reads, and each fixture entry already holds its `geometry` under that name.
fn boundary_of(key: &str) -> Option<Vec<GeoPolygon>> {
    parse_zone_polygons(&fixture(key), key)
}

/// Twice the shoelace, unsigned: zero exactly when the ring encloses nothing.
fn ring_area(ring: &GeoPolygonRing) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut twice = 0.0;
    for i in 0..n {
        let (x1, y1) = ring[i];
        let (x2, y2) = ring[(i + 1) % n];
        twice += x1 * y2 - x2 * y1;
    }
    (twice * 0.5).abs()
}

fn every_ring(key: &str) -> Vec<GeoPolygonRing> {
    let geometry = fixture(key);
    crate::nws::alert::parse_geometry(geometry.get("geometry"))
        .expect("fixture geometry must parse")
        .into_iter()
        .flatten()
        .collect()
}

/// **The commonest cause, and it is not a failure of the origin at all.**
///
/// A zone made of separate landmasses is served as a `GeometryCollection` of a
/// `Polygon` and a `MultiPolygon` rather than as one `MultiPolygon`. That is
/// valid GeoJSON and it is what the NWS publishes for 227 of its 11,651 zones —
/// the Outer Banks, the Florida Keys, Maryland's western shore, Kauai. Every
/// one of them fell down `parse_geometry`'s `_ =>` arm and came back
/// `NoBoundary`, which reads in the panel as though the zone had no shape.
///
/// Both live rounds sampled produced 28 failures and this was all 28 of them.
#[test]
fn a_zone_served_as_a_geometry_collection_still_has_a_boundary() {
    let polygons = boundary_of("collection").expect(
        "FLZ262 is a GeometryCollection of two Polygons - a zone with two \
         landmasses, not a zone with no boundary",
    );
    assert_eq!(
        polygons.len(),
        2,
        "both members of the collection must be flattened into the boundary, \
         not just the first",
    );
    for (i, polygon) in polygons.iter().enumerate() {
        assert!(
            ring_area(&polygon[0]) > 0.0,
            "part {i} of FLZ262 encloses nothing, so it paints nothing",
        );
    }
}

/// **The clause under investigation, and it fires on real data.**
///
/// Eauripik is an atoll in Yap State under WFO PQW. Its two parts are 798 m and
/// 522 m across, both under what a 0.005° (≈500 m) tolerance can resolve, so
/// RDP returned `[v0, v0]` for each, both were filtered out for having fewer
/// than three points, and the zone reported `NoBoundary` — a label that reads
/// as "the NWS sent us nothing" for a boundary the NWS sent in full.
///
/// Six of the 11,651 zones do this, all of them Yap outer-island atolls, and
/// they are not decorative: NWS issues High Surf Advisories against those zones
/// on most days. A zone this size is also exactly the size a tornado warning
/// lives at.
#[test]
fn an_atoll_smaller_than_the_simplification_tolerance_still_has_a_boundary() {
    let polygons = boundary_of("atoll").expect(
        "FMC010's geometry arrived intact and describes two islands; a \
         tolerance that cannot resolve them is not permission to delete them",
    );
    assert_eq!(polygons.len(), 2, "both islands of the atoll must survive");
    for (i, polygon) in polygons.iter().enumerate() {
        assert!(
            polygon[0].len() >= 3 && ring_area(&polygon[0]) > 0.0,
            "island {i} came back as {} points enclosing {} - it is on the map \
             in name only",
            polygon[0].len(),
            ring_area(&polygon[0]),
        );
    }
}

/// **The same defect wearing a green status line.**
///
/// Utrok's smaller islet is 1.33 km across — big enough that RDP's top-level
/// split fires, small enough that both halves are flat against the chord, so it
/// comes back as `[v0, vfar, v0]`. Three points, so it clears every `len() >= 3`
/// check in the pipeline; zero area, so even-odd fills none of it. It was
/// cached, counted as resolved, reported as a complete zone, and drew nothing.
///
/// 6,185 exterior rings across the corpus were in exactly this state, which is
/// why the guard is "encloses area" and not "has three points".
#[test]
fn a_part_the_tolerance_cannot_resolve_is_kept_as_a_ring_with_area() {
    let polygons = boundary_of("islet").expect("MHC410 has two parts");
    assert_eq!(polygons.len(), 2);
    let areas: Vec<f64> = polygons.iter().map(|p| ring_area(&p[0])).collect();
    assert!(
        areas.iter().all(|a| *a > 0.0),
        "one of Utrok's islets encloses zero area: {areas:?} - it is stored, \
         counted and invisible",
    );
}

/// The invariant the three cases above are instances of, stated once.
///
/// Simplification is a fidelity operation with a tolerance. It is not a filter,
/// and it does not get to decide that a shape is too small to exist — that
/// decision needs the zoom, which this code does not have and the rasterizer
/// does.
#[test]
fn simplification_never_turns_a_ring_into_something_that_is_not_a_ring() {
    for key in ["collection", "atoll", "islet"] {
        for (i, ring) in every_ring(key).into_iter().enumerate() {
            assert!(
                ring_area(&ring) > 0.0,
                "premise: {key} ring {i} encloses area before simplification",
            );
            let simplified = simplify_ring(&ring, SIMPLIFY_EPSILON);
            assert!(
                simplified.len() >= 3,
                "{key} ring {i}: {} points in, {} out",
                ring.len(),
                simplified.len(),
            );
            assert!(
                ring_area(&simplified) > 0.0,
                "{key} ring {i}: {} points in, {} out, and they enclose nothing",
                ring.len(),
                simplified.len(),
            );
        }
    }
}

/// **The counterweight.** Every test above is satisfied by not simplifying at
/// all, and that would put 8 million vertices through the projector on every
/// frame. A ring the tolerance *can* resolve must still be reduced by it, and
/// reduced by the same amount as before — the tolerance only gives way where it
/// would otherwise destroy the ring.
#[test]
fn a_ring_the_tolerance_can_resolve_is_still_simplified_as_hard_as_ever() {
    let rings = every_ring("collection");
    let mainland = rings
        .iter()
        .max_by_key(|r| r.len())
        .expect("FLZ262 has rings");
    assert_eq!(mainland.len(), 66, "premise: the fixture is unchanged");
    assert_eq!(
        simplify_ring(mainland, SIMPLIFY_EPSILON).len(),
        9,
        "the mainland part of FLZ262 must still come down to 9 points; \
         tightening the tolerance for the parts that need it must not loosen \
         the reduction anywhere else",
    );
}

/// A cache entry written by a *different* simplification is not the answer to
/// this question, and reading it as one is how a fix fails to arrive.
///
/// The disk cache stores simplified rings for a year. Every entry on every
/// machine that has ever run this was written by the simplifier that deleted
/// small islands, so without a schema stamp the fix would reach a zone the
/// first time anyone looked at it after August 2027.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_cache_entry_from_before_the_schema_existed_is_refused() {
    let dir = std::env::temp_dir().join(format!(
        "rustdar-zone-cache-schema-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).expect("scratch cache dir");
    let url = "https://api.weather.gov/zones/county/FMC010";
    let path = dir.join("county_FMC010.json");

    // Exactly the shape written before `ZONE_CACHE_SCHEMA` existed, holding the
    // one-point rings the old simplifier produced for this very zone.
    std::fs::write(
        &path,
        r#"{"fetched_at":32503680000,"polygons":[[[[7.0,143.0],[7.0,143.0]]]]}"#,
    )
    .expect("write legacy entry");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let read = runtime.block_on(super::read_cached_zone(&dir, url));

    assert!(
        read.is_none(),
        "a pre-schema entry was trusted, so this machine keeps drawing the \
         geometry the old simplifier produced until its year-long TTL expires",
    );
    assert!(
        !path.exists(),
        "the stale entry must be removed, not merely ignored, or it is \
         re-examined on every poll for a year",
    );

    let _ = std::fs::remove_dir_all(&dir);
}
