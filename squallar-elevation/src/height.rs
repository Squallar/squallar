//! [`HeightField`]: heights on the volume box's own post grid, two bytes each.
//!
//! The box's grid, not the tile pyramid's, because that is what the ground pass
//! consumes: a fixed-topology mesh whose posts are texels one-for-one, so there
//! is no sampler and no filtering on the GPU side.
//!
//! **The `u16` encoding.** `HEIGHT_BASE_M = -500` at `HEIGHT_QUANTUM_M = 0.25`
//! reaches +15,883.75 m — Death Valley (−86 m) to Everest (8,849 m) with room
//! to spare — for 2 bytes per post against 4 for `f32`. A height field is one
//! texture per box and the budget it comes out of is shared with the radar
//! grid, so the halving is the reason this is not just `f32`.

/// Lowest height the field can carry.
pub const HEIGHT_BASE_M: f64 = -500.0;

/// Metres per count.
pub const HEIGHT_QUANTUM_M: f64 = 0.25;

/// Highest height the field can carry: `-500 + 65535 * 0.25`.
pub const HEIGHT_CEILING_M: f64 = HEIGHT_BASE_M + 65_535.0 * HEIGHT_QUANTUM_M;

/// Metres to a stored count, saturating at both ends of the encoding.
///
/// Rounds, so the error is half a quantum either way rather than a whole one
/// downward. A `NaN` — which a hole in the source DEM would produce — encodes
/// as the floor, matching the builder's rule for the same case.
#[inline]
pub fn encode_height_m(height_m: f64) -> u16 {
    if height_m.is_nan() {
        return 0;
    }
    ((height_m - HEIGHT_BASE_M) / HEIGHT_QUANTUM_M)
        .round()
        .clamp(0.0, 65_535.0) as u16
}

/// The height a stored count carries.
#[inline]
pub fn decode_height_m(v: u16) -> f64 {
    HEIGHT_BASE_M + f64::from(v) * HEIGHT_QUANTUM_M
}

/// Heights over one volume box, on that box's own post grid.
///
/// `x_km`/`y_km` are the box's east and north extents about `site`, in the same
/// box space the volume grid is built in, so a field and the volume it sits
/// under are registered by construction rather than by agreement.
#[derive(Clone, Debug, PartialEq)]
pub struct HeightField {
    /// `(latitude, longitude)` of the box's origin, in degrees.
    pub site: (f64, f64),
    /// East extent as `(low, high)` kilometres about the site.
    pub x_km: (f64, f64),
    /// North extent as `(low, high)` kilometres about the site.
    pub y_km: (f64, f64),
    /// Posts along east and north, in that order.
    pub posts: [u32; 2],
    /// Row-major from the low `y_km` edge, `posts[0] * posts[1]` entries.
    pub samples: Vec<u16>,
}

impl HeightField {
    /// Posts along east.
    #[inline]
    pub fn posts_x(&self) -> u32 {
        self.posts[0]
    }

    /// Posts along north.
    #[inline]
    pub fn posts_y(&self) -> u32 {
        self.posts[1]
    }

    /// The height at post `(i, j)`, in metres. `None` off the grid.
    pub fn height_m(&self, i: u32, j: u32) -> Option<f64> {
        if i >= self.posts[0] || j >= self.posts[1] {
            return None;
        }
        let at = (j as usize) * (self.posts[0] as usize) + (i as usize);
        self.samples.get(at).copied().map(decode_height_m)
    }

    /// The box-space centre of post `(i, j)`, in kilometres.
    ///
    /// Post **centres**, so the field covers the box's extent evenly and the
    /// first and last posts sit half a cell inside the edges rather than on
    /// them. The ground mesh reads the same rule, which is what keeps the two
    /// from disagreeing about where a post is.
    pub fn post_center_km(&self, i: u32, j: u32) -> (f64, f64) {
        (
            self.x_km.0
                + (f64::from(i) + 0.5) * (self.x_km.1 - self.x_km.0) / f64::from(self.posts[0]),
            self.y_km.0
                + (f64::from(j) + 0.5) * (self.y_km.1 - self.y_km.0) / f64::from(self.posts[1]),
        )
    }

    /// Lowest and highest height in the field, in metres. `None` when empty.
    pub fn range_m(&self) -> Option<(f64, f64)> {
        let lo = self.samples.iter().copied().min()?;
        let hi = self.samples.iter().copied().max()?;
        Some((decode_height_m(lo), decode_height_m(hi)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The range the plan sizes the encoding against, and the ends it saturates
    /// at rather than wrapping through.
    #[test]
    fn the_height_encoding_reaches_death_valley_and_everest_and_saturates_outside() {
        assert_eq!(HEIGHT_CEILING_M, 15_883.75);
        for h in [-86.0_f64, -430.5, 0.0, 1609.34, 4401.2, 8848.86] {
            let back = decode_height_m(encode_height_m(h));
            assert!(
                (back - h).abs() <= HEIGHT_QUANTUM_M / 2.0 + 1e-9,
                "{h} m came back as {back} m"
            );
        }
        assert_eq!(encode_height_m(HEIGHT_BASE_M), 0);
        assert_eq!(encode_height_m(HEIGHT_BASE_M - 1000.0), 0);
        assert_eq!(encode_height_m(f64::NEG_INFINITY), 0);
        assert_eq!(encode_height_m(f64::NAN), 0);
        assert_eq!(encode_height_m(HEIGHT_CEILING_M), 65_535);
        assert_eq!(encode_height_m(HEIGHT_CEILING_M + 1000.0), 65_535);
        assert_eq!(encode_height_m(f64::INFINITY), 65_535);
    }

    /// Non-triviality on the round trip above: the sweep really does reach the
    /// worst case, or "within half a quantum" would pass for a coarser encoder
    /// that was simply never asked near the edge.
    #[test]
    fn the_round_trip_sweep_actually_reaches_half_a_quantum() {
        let mut worst: f64 = 0.0;
        let mut h = HEIGHT_BASE_M;
        while h <= 9000.0 {
            worst = worst.max((decode_height_m(encode_height_m(h)) - h).abs());
            h += 0.017;
        }
        assert!(
            worst > HEIGHT_QUANTUM_M / 2.0 - 0.002,
            "sweep never approached half a quantum: {worst}"
        );
    }

    #[test]
    fn a_post_reads_its_own_sample_and_nothing_off_the_grid() {
        let f = HeightField {
            site: (39.0, -106.0),
            x_km: (-10.0, 10.0),
            y_km: (-20.0, 20.0),
            posts: [2, 3],
            samples: vec![
                encode_height_m(100.0),
                encode_height_m(200.0),
                encode_height_m(300.0),
                encode_height_m(400.0),
                encode_height_m(500.0),
                encode_height_m(600.0),
            ],
        };
        assert_eq!(f.posts_x(), 2);
        assert_eq!(f.posts_y(), 3);
        assert_eq!(f.height_m(0, 0), Some(100.0));
        assert_eq!(f.height_m(1, 0), Some(200.0));
        assert_eq!(f.height_m(0, 1), Some(300.0));
        assert_eq!(f.height_m(1, 2), Some(600.0));
        assert_eq!(f.height_m(2, 0), None);
        assert_eq!(f.height_m(0, 3), None);
        assert_eq!(f.range_m(), Some((100.0, 600.0)));
        // Post centres: half a cell inside each edge, evenly spaced.
        let close = |got: (f64, f64), want: (f64, f64)| {
            assert!(
                (got.0 - want.0).abs() < 1e-12 && (got.1 - want.1).abs() < 1e-12,
                "{got:?} is not {want:?}"
            );
        };
        close(f.post_center_km(0, 0), (-5.0, -20.0 + 40.0 / 6.0));
        close(f.post_center_km(1, 2), (5.0, 20.0 - 40.0 / 6.0));
    }
}
