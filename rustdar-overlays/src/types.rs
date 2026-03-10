/// A single polygon ring: a sequence of (latitude, longitude) points.
/// The first ring is the exterior, subsequent rings are holes.
pub type GeoPolygonRing = Vec<(f64, f64)>;

/// A polygon with an exterior ring and optional holes.
pub type GeoPolygon = Vec<GeoPolygonRing>;

/// Hatching pattern for CIG (Conditional Intensity Group) areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HatchPattern {
    /// No hatching — standard filled polygon.
    None,
    /// CIG1: dotted hatch lines angled to the left (135° / backslash direction).
    Cig1,
    /// CIG2: solid hatch lines angled to the right (45° / forward-slash direction).
    Cig2,
    /// CIG3: solid hatch lines in both directions (cross-hatch).
    Cig3,
}

/// Geographic bounding box for quick viewport culling.
#[derive(Debug, Clone, Copy)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    /// Compute bounds from a set of (lat, lon) points.
    pub fn from_points(pts: &[(f64, f64)]) -> Option<Self> {
        if pts.is_empty() {
            return None;
        }
        let mut min_lat = f64::MAX;
        let mut max_lat = f64::MIN;
        let mut min_lon = f64::MAX;
        let mut max_lon = f64::MIN;
        for &(lat, lon) in pts {
            min_lat = min_lat.min(lat);
            max_lat = max_lat.max(lat);
            min_lon = min_lon.min(lon);
            max_lon = max_lon.max(lon);
        }
        Some(Self {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        })
    }

    /// Check whether this bounds intersects another.
    pub fn intersects(&self, other: &GeoBounds) -> bool {
        self.min_lat <= other.max_lat
            && self.max_lat >= other.min_lat
            && self.min_lon <= other.max_lon
            && self.max_lon >= other.min_lon
    }
}

/// Pre-computed triangulation for a single polygon ring.
/// Indices refer to the exterior ring vertices (with GeoJSON closing
/// duplicate stripped). Computed once at fetch time and reused across frames.
#[derive(Debug, Clone)]
pub struct PrecomputedTriangulation {
    /// Triangle indices into the ring's vertex list.
    pub indices: Vec<u32>,
}

/// A renderable overlay feature with geometry, styling, and metadata.
#[derive(Debug, Clone)]
pub struct OverlayFeature {
    /// One or more polygons (from GeoJSON MultiPolygon).
    pub polygons: Vec<GeoPolygon>,
    /// Fill color as RGBA (alpha controls transparency).
    pub fill_rgba: [u8; 4],
    /// Stroke/outline color as RGBA.
    pub stroke_rgba: [u8; 4],
    /// Short label (e.g. "SLGT", "0.05", "CIG1").
    pub label: String,
    /// Human-readable label (e.g. "Slight Risk", "5% Tornado Risk").
    pub label2: String,
    /// Hatching pattern for CIG areas.
    pub hatch: HatchPattern,
    /// Pre-computed triangulation for each polygon's exterior ring.
    /// Indexed parallel to `polygons` — `triangulations[i]` corresponds
    /// to `polygons[i][0]` (the exterior ring).
    pub triangulations: Vec<Option<PrecomputedTriangulation>>,
    /// Geographic bounding box encompassing all polygons in this feature.
    pub geo_bounds: Option<GeoBounds>,
}

impl OverlayFeature {
    /// Build a new `OverlayFeature` and pre-compute triangulation + geo-bounds.
    ///
    /// Triangulation is computed once in geo-coordinates (the topology is
    /// projection-invariant, so the same index buffer works after any
    /// linear coordinate transform such as Mercator projection).
    pub fn new(
        polygons: Vec<GeoPolygon>,
        fill_rgba: [u8; 4],
        stroke_rgba: [u8; 4],
        label: String,
        label2: String,
        hatch: HatchPattern,
    ) -> Self {
        let triangulations = precompute_triangulations(&polygons);
        let geo_bounds = compute_geo_bounds(&polygons);
        Self {
            polygons,
            fill_rgba,
            stroke_rgba,
            label,
            label2,
            hatch,
            triangulations,
            geo_bounds,
        }
    }

    /// Recompute triangulation and geo-bounds from the current polygons.
    /// Call this after mutating `polygons` (e.g. after simplification).
    pub fn recompute_cache(&mut self) {
        self.triangulations = precompute_triangulations(&self.polygons);
        self.geo_bounds = compute_geo_bounds(&self.polygons);
    }
}

/// Strip the GeoJSON closing duplicate (last == first) from a ring.
fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

/// Pre-compute ear-clip triangulation for each polygon's exterior ring.
fn precompute_triangulations(polygons: &[GeoPolygon]) -> Vec<Option<PrecomputedTriangulation>> {
    polygons
        .iter()
        .map(|polygon| {
            let exterior = polygon.first()?;
            let ring = strip_closing_dup(exterior);
            if ring.len() < 3 {
                return None;
            }
            // Flatten to [lat0, lon0, lat1, lon1, ...] for earcutr
            let coords: Vec<f64> = ring.iter().flat_map(|&(lat, lon)| [lat, lon]).collect();
            let indices = earcutr::earcut(&coords, &[], 2).ok()?;
            if indices.is_empty() {
                return None;
            }
            Some(PrecomputedTriangulation {
                indices: indices.into_iter().map(|i| i as u32).collect(),
            })
        })
        .collect()
}

/// Compute the overall geographic bounding box for all polygons in a feature.
fn compute_geo_bounds(polygons: &[GeoPolygon]) -> Option<GeoBounds> {
    let mut min_lat = f64::MAX;
    let mut max_lat = f64::MIN;
    let mut min_lon = f64::MAX;
    let mut max_lon = f64::MIN;
    let mut any = false;

    for polygon in polygons {
        for ring in polygon {
            for &(lat, lon) in ring {
                min_lat = min_lat.min(lat);
                max_lat = max_lat.max(lat);
                min_lon = min_lon.min(lon);
                max_lon = max_lon.max(lon);
                any = true;
            }
        }
    }

    if any {
        Some(GeoBounds {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        })
    } else {
        None
    }
}

// ── Shared geometry utilities ────────────────────────────────────────────

/// Ramer-Douglas-Peucker polygon ring simplification.
///
/// Reduces vertex count by removing points within `epsilon` degrees of the
/// line between their neighbours. An epsilon of ~0.005 (~500 m) keeps shapes
/// visually accurate at typical map zoom levels while cutting vertex counts
/// significantly.
pub fn simplify_ring(ring: &GeoPolygonRing, epsilon: f64) -> GeoPolygonRing {
    if ring.len() <= 3 {
        return ring.clone();
    }
    rdp_simplify(ring, epsilon)
}

fn rdp_simplify(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }

    let first = points[0];
    let last = points[points.len() - 1];
    let mut max_dist = 0.0_f64;
    let mut max_idx = 0;

    for (i, &pt) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let d = perpendicular_distance(pt, first, last);
        if d > max_dist {
            max_dist = d;
            max_idx = i;
        }
    }

    if max_dist > epsilon {
        let mut left = rdp_simplify(&points[..=max_idx], epsilon);
        let right = rdp_simplify(&points[max_idx..], epsilon);
        left.pop(); // Remove duplicate junction point
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

fn perpendicular_distance(
    point: (f64, f64),
    line_start: (f64, f64),
    line_end: (f64, f64),
) -> f64 {
    let dx = line_end.0 - line_start.0;
    let dy = line_end.1 - line_start.1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        let px = point.0 - line_start.0;
        let py = point.1 - line_start.1;
        return (px * px + py * py).sqrt();
    }
    let num = ((point.0 - line_start.0) * dy - (point.1 - line_start.1) * dx).abs();
    num / len_sq.sqrt()
}

/// Simplify all rings in all polygons of a feature's polygon set.
pub fn simplify_polygons(polygons: &mut Vec<GeoPolygon>, epsilon: f64) {
    for polygon in polygons.iter_mut() {
        for ring in polygon.iter_mut() {
            if ring.len() > 3 {
                *ring = simplify_ring(ring, epsilon);
            }
        }
        // Remove degenerate rings
        polygon.retain(|r| r.len() >= 3);
    }
    // Remove empty polygons
    polygons.retain(|p| !p.is_empty());
}
