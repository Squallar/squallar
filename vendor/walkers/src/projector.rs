use egui::{Rect, Vec2};

use crate::{
    MapMemory, Position,
    mercator::{project_at_scale, total_pixels, unproject_at_scale},
    position::{Pixels, PixelsExt as _},
    tiles::TileId,
};

/// Projects geographical position into pixels on the viewport, suitable for [`egui::Painter`].
///
/// A projector is built for one frame from a snapshot of [`MapMemory`], and every method
/// takes `&self`, so the map's centre and the zoom cannot move underneath it. Both are
/// therefore resolved once, at construction, rather than per call: the centre costs a
/// project-and-unproject round trip and the zoom a `powf`.
#[derive(Clone)]
pub struct Projector {
    clip_rect: Rect,
    /// The map's centre, already projected. This is pixel space, not lat/lon.
    map_center_projected_position: Pixels,
    /// Width of the whole world in pixels at this projector's zoom.
    world_pixels: f64,
}

impl Projector {
    pub fn new(clip_rect: Rect, map_memory: &MapMemory, my_position: Position) -> Self {
        Self::with_map_center(
            clip_rect,
            map_memory,
            map_memory.center_mode.position(my_position),
        )
    }

    /// [`Projector::new`], for a caller that has already resolved the map's centre this
    /// frame. `Map::show` computes it to draw the tile layers with, a dozen lines before it
    /// builds the projector, and the two were each resolving it — an `unproject(project(..))`
    /// round trip — separately.
    pub(crate) fn with_map_center(
        clip_rect: Rect,
        map_memory: &MapMemory,
        map_center: Position,
    ) -> Self {
        let world_pixels = total_pixels(map_memory.zoom());

        Self {
            clip_rect,
            map_center_projected_position: project_at_scale(map_center, world_pixels),
            world_pixels,
        }
    }

    /// Project `position` into pixels on the viewport.
    pub fn project(&self, position: Position) -> Vec2 {
        // Turn that into a flat, mercator projection.
        let projected_position = project_at_scale(position, self.world_pixels);

        // From the two points above we can calculate the actual point on the screen.
        self.clip_rect.center().to_vec2()
            + (projected_position - self.map_center_projected_position).to_vec2()
    }

    /// Get coordinates from viewport's pixels position
    pub fn unproject(&self, position: Vec2) -> Position {
        // Despite being in pixel space `map_center_projected_position` is sufficiently large
        // that we must do the arithmetic in f64 to avoid imprecision.
        let clip_center = self.clip_rect.center();
        let x =
            self.map_center_projected_position.x() + (position.x as f64) - (clip_center.x as f64);
        let y =
            self.map_center_projected_position.y() + (position.y as f64) - (clip_center.y as f64);

        unproject_at_scale(Pixels::new(x, y), self.world_pixels)
    }

    /// The screen rect that `tile_id` covers, by affine arithmetic alone.
    ///
    /// A tile's corners are rational fractions of the world bitmap — tile `x` at zoom `z`
    /// starts exactly `x / 2^z` of the way across it — so placing one needs no projection at
    /// all. Going through geography instead costs a `sinh`/`atan` to turn the tile index into
    /// a latitude and then the `tan`/`asinh` inside [`Projector::project`] to turn it straight
    /// back: the two halves are exact inverses.
    ///
    /// **There is deliberately no `tile_size` parameter.** The base is walkers' own 256 px
    /// tile and never the source's: [`crate::mercator::tile_id`] has already folded a larger
    /// source tile into the zoom, reducing it by `log2(source_tile_size / 256)`. A 512 px
    /// source therefore arrives here as a tile one zoom shallower, and
    /// `256 · 2^(map_zoom − tile_zoom)` reproduces its doubled side on its own. Taking the
    /// size as a parameter as well would double-count it, by exactly 2x.
    ///
    /// The exponent is the map's zoom **minus** the tile's, so a tile from a shallower level
    /// than `round(zoom)` — a deliberately coarser layer, or an ancestor stretched over a
    /// gap — is drawn larger, and one from a deeper level smaller.
    pub fn tile_rect(&self, tile_id: TileId) -> Rect {
        // `world_pixels` is `256 · 2^map_zoom` and the grid is `2^tile_zoom` tiles across.
        // Both are exact powers of two, so the quotient is `256 · 2^(map_zoom − tile_zoom)`
        // with no rounding of its own.
        let side = self.world_pixels / 2f64.powi(i32::from(tile_id.zoom));

        // The same two lines as `project`, with the tile's own projected corner in place of
        // a projected position.
        let offset = tile_id.project(side) - self.map_center_projected_position;
        let north_west = self.clip_rect.center().to_vec2() + offset.to_vec2();

        Rect::from_min_size(north_west.to_pos2(), Vec2::splat(side as f32))
    }

    /// The viewport this projector places into.
    ///
    /// [`crate::tiles::draw_tiles`] culls against an [`egui::Painter`]'s clip rect and places
    /// through this projector, and those two are only the same map while they are the same
    /// rect. It asserts that; this is what it reads.
    pub(crate) fn clip_rect(&self) -> Rect {
        self.clip_rect
    }

    /// What is the local scale of the map at the provided position and given the current zoom
    /// level?
    pub fn scale_pixel_per_meter(&self, position: Position) -> f32 {
        // return f32 for ergonomics, as the result is typically used for egui code
        calculate_meters_per_pixel(position.y(), self.world_pixels) as f32
    }
}

/// Implementation of the scale computation, given the number of pixels for the width of the
/// world at the zoom in question ([`crate::mercator::total_pixels`]).
fn calculate_meters_per_pixel(latitude: f64, total_pixels: f64) -> f64 {
    const EARTH_CIRCUMFERENCE: f64 = 40_075_016.686;

    let pixel_per_meter_equator = total_pixels / EARTH_CIRCUMFERENCE;
    let latitude_rad = latitude.abs().to_radians();
    pixel_per_meter_equator / latitude_rad.cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        lon_lat,
        mercator::{project, unproject},
    };
    use egui::Pos2;

    fn assert_approx_eq(a: f64, b: f64) {
        let diff = (a - b).abs();
        let tolerance = 0.01;
        assert!(
            diff < tolerance,
            "Values differ by more than {tolerance}: {a} vs {b}"
        );
    }

    #[test]
    fn test_unproject_precision() {
        let original = lon_lat(21., 52.);

        let mut map_memory = MapMemory::default();
        map_memory.set_zoom(18.).unwrap();

        let projector = Projector::new(
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.)),
            &map_memory,
            original,
        );

        let mut projected = projector.project(original);
        let mut prev_x = 0.0;
        for offset in 0..10 {
            projected.x += offset as f32;
            let unprojected = projector.unproject(projected);
            assert_ne!(
                prev_x,
                unprojected.x(),
                "Input was different but projection remained the same"
            );
            prev_x = unprojected.x();
        }
    }

    #[test]
    fn test_equator_zoom_0() {
        // At zoom 0 (whole world), equator should be about 156.5km per pixel
        let scale = calculate_meters_per_pixel(0.0, total_pixels(0.));
        assert_approx_eq(scale, 1. / 156_543.03);
    }

    #[test]
    fn test_equator_zoom_19() {
        // At max zoom (19), equator should be about 0.3m per pixel
        let scale = calculate_meters_per_pixel(0.0, total_pixels(19.));
        assert_approx_eq(scale, 1. / 0.298);
    }

    #[test]
    fn unproject_is_inverse_of_project() {
        let original = lon_lat(21., 52.);

        let mut map_memory = MapMemory::default();
        map_memory.set_zoom(10.).unwrap();

        let projector = Projector::new(
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.)),
            &map_memory,
            original,
        );

        let projected = projector.project(original);
        let unprojected = projector.unproject(projected);

        assert_approx_eq(original.x(), unprojected.x());
        assert_approx_eq(original.y(), unprojected.y());
    }

    /// A guard, not a negative control. `Projector::unproject` used to resolve the map's
    /// centre for itself on every call instead of reading the copy the constructor already
    /// took, and the two expressions are identical — so this passes both before and after
    /// the caching. It exists to keep them identical: it fails if a future change lets the
    /// cached centre and the live one drift, which is the only way this refactor can be
    /// wrong. Detaching the map away from `my_position` is what makes the two spellings
    /// distinguishable at all; against the default `Center::MyPosition` they read the same
    /// field.
    #[test]
    fn unproject_is_inverse_of_project_when_the_map_is_detached() {
        let my_position = lon_lat(21., 52.);
        let map_center = lon_lat(-122.4194, 37.7749);

        let mut map_memory = MapMemory::default();
        map_memory.set_zoom(10.).unwrap();
        map_memory.center_at(map_center);

        let projector = Projector::new(
            Rect::from_min_size(Pos2::new(4., 7.), Vec2::new(640., 480.)),
            &map_memory,
            my_position,
        );

        for original in [my_position, map_center, lon_lat(-122.5, 37.8)] {
            let unprojected = projector.unproject(projector.project(original));
            assert_approx_eq(original.x(), unprojected.x());
            assert_approx_eq(original.y(), unprojected.y());
        }
    }

    /// Resolving the centre and the world scale once, at construction, must give the very
    /// same `f64`s as recomputing them per call did — not merely close ones. The operations
    /// and their order are meant to be unchanged, so every comparison here is `assert_eq!`
    /// on the bits.
    #[test]
    fn the_cached_projection_state_is_bit_identical_to_recomputing_it() {
        let clip_rect = Rect::from_min_size(Pos2::new(4., 7.), Vec2::new(640., 480.));
        let clip_center = clip_rect.center();

        for half_zoom in 0..=38u32 {
            let zoom = (half_zoom as f64 * 0.5).clamp(0., 19.);

            for (lon, lat) in [
                (17.03664, 51.09916),
                (-122.4194, 37.7749),
                (151.2093, -33.87),
            ] {
                let my_position = lon_lat(lon, lat);

                for detached_at in [None, Some(lon_lat(lon + 3.5, lat / 2.))] {
                    let mut memory = MapMemory::default();
                    memory.set_zoom(zoom).unwrap();
                    if let Some(position) = detached_at {
                        memory.center_at(position);
                    }

                    let projector = Projector::new(clip_rect, &memory, my_position);

                    // The expressions the constructor and `unproject` each evaluated for
                    // themselves, before either was cached.
                    let live_zoom = memory.zoom();
                    let live_center = project(memory.center_mode.position(my_position), live_zoom);

                    assert_eq!(
                        live_center.x().to_bits(),
                        projector.map_center_projected_position.x().to_bits()
                    );
                    assert_eq!(
                        live_center.y().to_bits(),
                        projector.map_center_projected_position.y().to_bits()
                    );
                    assert_eq!(
                        total_pixels(live_zoom).to_bits(),
                        projector.world_pixels.to_bits()
                    );

                    for probe in [my_position, lon_lat(lon + 1., lat - 1.)] {
                        // `project`, as it was spelled before the scale was hoisted.
                        let expected = clip_center.to_vec2()
                            + (project(probe, live_zoom) - live_center).to_vec2();
                        let actual = projector.project(probe);
                        assert_eq!(expected.x.to_bits(), actual.x.to_bits());
                        assert_eq!(expected.y.to_bits(), actual.y.to_bits());

                        // `scale_pixel_per_meter`, likewise.
                        let expected =
                            calculate_meters_per_pixel(probe.y(), total_pixels(live_zoom)) as f32;
                        assert_eq!(
                            expected.to_bits(),
                            projector.scale_pixel_per_meter(probe).to_bits()
                        );
                    }

                    for viewport in [
                        Vec2::new(0., 0.),
                        Vec2::new(320., 240.),
                        Vec2::new(639.5, 479.25),
                        Vec2::new(-11., 1234.),
                    ] {
                        // `unproject`, as it was spelled before the centre was cached.
                        let x = live_center.x() + (viewport.x as f64) - (clip_center.x as f64);
                        let y = live_center.y() + (viewport.y as f64) - (clip_center.y as f64);
                        let expected = unproject(Pixels::new(x, y), live_zoom);

                        let actual = projector.unproject(viewport);
                        assert_eq!(expected.x().to_bits(), actual.x().to_bits());
                        assert_eq!(expected.y().to_bits(), actual.y().to_bits());
                    }
                }
            }
        }
    }

    /// `Map::show` resolves the map's centre to draw the tile layers with and then handed
    /// `Projector::new` the ingredients to resolve it again. Handing the projector the
    /// centre it already has must build the same projector, to the bit.
    #[test]
    fn a_hoisted_map_centre_builds_the_same_projector() {
        let clip_rect = Rect::from_min_size(Pos2::new(4., 7.), Vec2::new(640., 480.));
        let my_position = lon_lat(21., 52.);

        for zoom in [0., 3.5, 10., 19.] {
            for detached_at in [None, Some(lon_lat(-122.4194, 37.7749))] {
                let mut memory = MapMemory::default();
                memory.set_zoom(zoom).unwrap();
                if let Some(position) = detached_at {
                    memory.center_at(position);
                }

                let built = Projector::new(clip_rect, &memory, my_position);
                let hoisted = Projector::with_map_center(
                    clip_rect,
                    &memory,
                    // What `Map::position` hands over, and what `Map` computed for the
                    // tile layers a dozen lines earlier.
                    memory.center_mode.position(my_position),
                );

                assert_eq!(
                    built.map_center_projected_position.x().to_bits(),
                    hoisted.map_center_projected_position.x().to_bits()
                );
                assert_eq!(
                    built.map_center_projected_position.y().to_bits(),
                    hoisted.map_center_projected_position.y().to_bits()
                );
                assert_eq!(built.world_pixels.to_bits(), hoisted.world_pixels.to_bits());

                let probe = lon_lat(20., 51.);
                assert_eq!(
                    built.project(probe).x.to_bits(),
                    hoisted.project(probe).x.to_bits()
                );
                assert_eq!(
                    built.unproject(Vec2::new(11., 13.)).y().to_bits(),
                    hoisted.unproject(Vec2::new(11., 13.)).y().to_bits()
                );

                // The agreement is about the centre, not about the constructor ignoring
                // it: a different centre must build a different projector.
                let wrong = Projector::with_map_center(clip_rect, &memory, lon_lat(0., 0.));
                assert_ne!(
                    built.map_center_projected_position.x().to_bits(),
                    wrong.map_center_projected_position.x().to_bits()
                );
            }
        }
    }
}
