//! The map floor: the 2D pane's ground, resampled onto the 3D box's footprint.
//!
//! # What the floor is
//!
//! GR2Analyst draws its volume standing on the 2D map — basemap, radar and
//! labels — and that is issue #7's ask: "the floor of the 3d viewer should be
//! the 2d map (literally, the pane content)." [`compose_floor`] builds that
//! composition: **the basemap tiles the 2D panes draw, the lowest-tilt base
//! reflectivity exactly as the 2D pane's own rasterizer draws it, the panes'
//! vector overlays, and the city-label tiles**, on the dark ground colour,
//! registered to the box footprint, in the panes' own stacking order. The
//! tiles come from the panes' own [`HttpsTiles`] sources, which keep each
//! tile's compressed bytes beside the texture for exactly this consumer —
//! the "tile pipeline keeps no CPU bytes" blocker that deferred the first
//! cut was rustdar's own fetch path, and was turned off at its root.
//!
//! # The vector overlays
//!
//! The panes' vector overlays are **not** captured from egui — they are
//! rasterized here from the same *geographic* geometry the panes' own
//! rasterizers consume, one level before any projector: `OverlayFeature`
//! polygons in (lat, lon), gathered from the overlay registry as
//! [`FloorVectors`] and drawn by [`compose_floor`] through the inverse of the
//! same mapping every other floor layer reads with (see below). The layers,
//! in the panes' draw order (`OverlayKind::all`):
//!
//! * **SPC outlook polygons** — under the radar, exactly where the pane
//!   stacks them; exterior-ring fill and stroke in the features' own colours,
//!   as the pane's `draw_feature` fills them. CIG hatching is not reproduced:
//!   at 0.9 km per texel a hatch pattern is noise, and the fill + stroke are
//!   what carry the outlook's identity.
//! * **The 230 km range ring** — over the radar, where
//!   `render_radar_range_ring` paints it, at the ring's own colour. Analytic:
//!   a 256-gon at [`rustdar_radar::types::MAX_RANGE_KM`] kilometres from the
//!   site, its vertices produced by the mapping's own forward lines and drawn
//!   through the same consumer every polygon uses.
//! * **SPC Mesoscale Discussion polygons** — over the radar and the ring,
//!   under the alerts: the pane's own slot for `SpcDiscussions` in
//!   `OverlayKind::all`. Exterior-ring fill and stroke in
//!   `md_fill_color`/`md_stroke_color`, the colours the pane's
//!   `rasterize_spc_discussions` paints.
//! * **NWS warning/watch/advisory polygons** — over the radar, the ring and
//!   the discussions, under the city labels, exactly the pane's order; fill
//!   and stroke from `alert_color`, the standard colours. The "county lines" the 2D pane
//!   shows are these same polygons: rustdar's watch/advisory geometry is
//!   county/zone-shaped (see `features.md`), and no separate county-boundary
//!   vector layer exists — the basemap tiles carry the administrative lines
//!   that are not weather.
//!
//! Per the shipped decision on the floor toggle (`ui_map.rs`), the floor
//! ignores the map panes' per-pane layer toggles; it does follow the
//! handlers' own global state (an alert category turned off everywhere, a
//! hidden alert), because the registry's `clickable_items` — the same filter
//! the pane rasterizer applies — is where the geometry is read from.
//!
//! [`HttpsTiles`]: rustdar_egui::tile_source::HttpsTiles
//!
//! # Registration: one projection, run backwards
//!
//! The 2D raster places a point `(dx, dy)` kilometres east/north of the site
//! at
//!
//! ```text
//! px = W/2 + dx · (cos φ₀ / cos φ) · px_per_km
//! py = (mercᵧ(max_lat) − mercᵧ(φ)) · W / (mercᵧ(max_lat) − mercᵧ(min_lat))
//! φ  = φ₀ + dy / R
//! ```
//!
//! (`render::MercatorProjection::render_gate`, with `ImageBounds` supplying
//! the Mercator span). [`compose_floor`] evaluates the same three lines for
//! every floor texel — the box's own `x_range_km`/`y_range_km` are already
//! kilometres east/north of the site, so there is no second projection to
//! disagree with the first: the floor lands where the 2D pane would draw it
//! because it is computed *by* the 2D pane's arithmetic. The one approximation
//! is the raster's own (`φ = φ₀ + dy/R`, spherical), which is the codebase's
//! standing convention (`ImageBounds`, `corners_for`).
//!
//! The **tiles** read through the same mapping, not a second one. The
//! raster's x axis is linear in longitude (`ImageBounds` spans
//! `±MAX_RANGE_KM / (111.32 cos φ₀)` degrees over the image), and
//! substituting that span into the `px` line above gives the raster's own
//! longitude for a floor texel:
//!
//! ```text
//! λ = λ₀ + dx / (111.32 · cos φ)
//! ```
//!
//! — the same `cos_correction` reappearing as algebra, not a new convention.
//! `(λ, φ)` then indexes the slippy-tile pyramid the standard way
//! (`x = (λ+180)/360 · 2^z`, `y = (1 − mercᵧ(φ)/π)/2 · 2^z`, the same
//! [`mercator_y`]), so a tile pixel and a radar gate that name the same
//! ground land on the same texel; `a_tile_pixel_and_a_radar_gate_at_the_same_
//! ground_land_on_the_same_texel` pins that agreement through both consumers.
//!
//! The **vector shapes** read through the same mapping too — run backwards.
//! A shape vertex arrives as `(φ, λ)`, and [`geo_to_floor_texel`] solves the
//! two lines above for the texel instead of for the ground:
//!
//! ```text
//! dy = (φ − φ₀) · R          dx = (λ − λ₀) · 111.32 · cos φ
//! ```
//!
//! then the footprint's own linear km→texel step, which is those same lines'
//! `x_range`/`y_range` interpolation inverted. Nothing is approximated in the
//! inversion — each line is the forward line solved for its other variable,
//! with `cos φ` at the *vertex's* latitude exactly as the forward evaluates
//! it at the row's. A third consumer therefore shares the one mapping, and
//! `a_warning_vertex_lands_on_the_texel_the_gate_and_the_tile_name` pins all
//! three against each other, mid-box and at the box corner where a smuggled
//! `cos φ₀` would drift furthest.
//!
//! # Refresh
//!
//! A floor is rendered when a voxel build completes — the same cadence the
//! volume itself refreshes at, once per sealed sweep per site — and never per
//! frame. The store holds one per `(site, region)` and replaces it in place;
//! the GPU upload is keyed by the floor's id and reused until it changes.

use std::f64::consts::PI;

/// Texels along each edge of the floor image.
///
/// 512 over a footprint of at most 460 km is 0.9 km per texel — the raster
/// underneath resolves ~0.22 km, but the floor is viewed obliquely at pane
/// scale where 512 reads clean, and one floor is 1 MiB against the grid's 8.
pub const FLOOR_TEXELS: u32 = 512;

/// The ground where no echo painted: near-black, the colour GR's floor and
/// the dark basemap read as. Opaque, because the floor is ground — a
/// transparent gap in it would read as a hole in the earth.
pub const FLOOR_GROUND_RGBA: [u8; 4] = [16, 18, 22, 255];

/// The range ring's colour — `render_radar_range_ring`'s own stroke, faint
/// grey, restated here because that function paints per-frame egui and this
/// consumer bakes texels. A drift between the two is a visible colour
/// difference between the pane's ring and the floor's, nothing subtler.
pub const RANGE_RING_RGBA: [u8; 4] = [150, 150, 150, 80];

/// One vector shape for the floor: a polygon's **exterior ring** in
/// `(latitude, longitude)` degrees, with the colours the pane's own
/// rasterizer would draw it in.
///
/// Exterior ring only, because that is exactly what the pane draws:
/// `draw_feature` builds its path from `polygon.first()` and ignores holes,
/// for outlooks, MDs and alerts alike. A closing duplicate vertex (GeoJSON's
/// `last == first`) is tolerated and stripped at draw time.
#[derive(Clone, Debug, PartialEq)]
pub struct FloorShape {
    /// The ring's vertices as `(lat_deg, lon_deg)`.
    pub ring: Vec<(f64, f64)>,
    /// Straight-alpha fill; alpha 0 fills nothing.
    pub fill_rgba: [u8; 4],
    /// Straight-alpha stroke; alpha 0 strokes nothing.
    pub stroke_rgba: [u8; 4],
}

/// The pane's vector overlays for one floor composite, split by where the
/// pane stacks them against the radar (`OverlayKind::all`'s draw order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FloorVectors {
    /// Drawn under the radar echo: the SPC outlook polygons.
    pub under_radar: Vec<FloorShape>,
    /// Drawn over the radar echo (and over the ring), under the city labels,
    /// in the pane's own draw order: the SPC Mesoscale Discussion polygons
    /// first, then the NWS warning/watch/advisory polygons — later shapes
    /// blend over earlier ones, as the pane's rasterizers iterate.
    pub over_radar: Vec<FloorShape>,
    /// Whether to draw the 230 km range ring. The production composite
    /// always does — the pane always does; the flag exists so the plain
    /// radar-only [`resample_floor`] stays exactly what it was.
    pub range_ring: bool,
}

impl FloorVectors {
    /// No shapes and no ring — the layer composites as nothing.
    pub fn none() -> Self {
        Self::default()
    }
}

/// A floor ready for upload: straight, gamma-encoded RGBA, row 0 the box
/// footprint's north edge.
#[derive(Clone, Debug, PartialEq)]
pub struct FloorImage {
    /// Texels along x (east) and y (north-to-south rows).
    pub size: [u32; 2],
    /// `size[0] * size[1] * 4` bytes, row-major from the north-west corner.
    pub rgba: Vec<u8>,
}

/// `ln(tan(π/4 + φ/2))` — Web Mercator's y, exactly as
/// `rustdar_radar::types::lat_rad_to_mercator_y` computes it. Restated here
/// because that function is `pub(crate)` to its own crate; the formula is the
/// projection's definition, so the two cannot drift without one of them
/// ceasing to be Web Mercator.
fn mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// Resample the site-centred radar raster onto the box footprint.
///
/// * `source` — the raster's RGBA bytes, a square image as
///   `render_radar_to_image` produces: linear in longitude and Mercator y,
///   **`±MAX_RANGE_KM` about the site** — the raster's half-extent is
///   [`rustdar_radar::types::MAX_RANGE_KM`], read directly below rather than
///   taken as a parameter. `ImageBounds::from_radar_site` builds every raster
///   at that constant unconditionally, and the render's *returned*
///   `max_range_km` is a different number — the product's **data reach**
///   (super-res reflectivity gates run to ~460 km), kept for the range ring
///   and the hover readout. Feeding that reach in as the half-extent halves
///   the sampler's pixels-per-km and zooms the floor in 2×, which is exactly
///   the shipped bug the 2026-08-09 report's screenshot shows: strong cores
///   drawn at twice their true distance from the site, under an empty sky.
///   With the constant read at the definition, no caller can repeat it.
/// * `site_lat_deg` — the site's latitude; the raster's projection constants
///   derive from it and nothing needs the longitude, because the box's x is
///   already kilometres east of the site.
/// * `x_range_km`, `y_range_km` — the box footprint, kilometres east/north of
///   the site: `VoxelGrid::x_range_km()`/`y_range_km()`, so the floor is
///   registered to the grid actually built rather than to a re-derivation.
///
/// `None` for a source that is not a square RGBA image or a degenerate
/// footprint — both impossible from the production callers, and refused
/// rather than clamped for the usual reason: every arm below divides.
pub fn resample_floor(
    source: &[u8],
    site_lat_deg: f64,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
) -> Option<FloorImage> {
    // The site longitude only places tiles and shapes, and there are none
    // here.
    compose_floor(
        source,
        site_lat_deg,
        0.0,
        x_range_km,
        y_range_km,
        &TileLayer::empty(),
        &TileLayer::empty(),
        &FloorVectors::none(),
    )
}

/// One decoded map tile: its slippy-pyramid coordinates at the layer's zoom,
/// and its straight-alpha RGBA pixels.
pub struct DecodedTile {
    /// Slippy x — west to east, `0..2^zoom`.
    pub x: u32,
    /// Slippy y — north to south, `0..2^zoom`.
    pub y: u32,
    /// Pixels along each edge (tiles are square; 256 for the shipped sources).
    pub side: u32,
    /// `side * side * 4` straight, gamma-encoded RGBA bytes, row 0 the tile's
    /// north edge.
    pub rgba: Vec<u8>,
}

/// A zoom level's worth of decoded tiles for one floor composite.
pub struct TileLayer {
    pub zoom: u8,
    pub tiles: Vec<DecodedTile>,
}

impl TileLayer {
    /// No tiles at all — the layer composites as nothing.
    pub fn empty() -> Self {
        TileLayer {
            zoom: 0,
            tiles: Vec::new(),
        }
    }
}

/// A zoom level's worth of **compressed** tiles, as gathered on the frame
/// thread from the panes' own [`HttpsTiles`] caches — `(slippy x, slippy y,
/// the tile's PNG bytes)`. Decoding is [`TileBytesLayer::decode`], run in the
/// floor job's delivery rather than here, so the frame thread hands over
/// `Arc`s and the decode cost lands where the resample's already does.
///
/// [`HttpsTiles`]: rustdar_egui::tile_source::HttpsTiles
pub struct TileBytesLayer {
    pub zoom: u8,
    pub tiles: Vec<(u32, u32, std::sync::Arc<Vec<u8>>)>,
}

impl TileBytesLayer {
    /// No tiles — decodes to [`TileLayer::empty`].
    pub fn empty() -> Self {
        TileBytesLayer {
            zoom: 0,
            tiles: Vec::new(),
        }
    }

    /// Decode every tile to straight RGBA. A tile that does not decode is
    /// dropped with a log line rather than failing the floor: the map under
    /// the box missing one tile beats a box standing on nothing.
    pub fn decode(&self) -> TileLayer {
        TileLayer {
            zoom: self.zoom,
            tiles: self
                .tiles
                .iter()
                .filter_map(|(x, y, bytes)| match image::load_from_memory(bytes) {
                    Ok(decoded) => {
                        let rgba = decoded.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        if w != h || w == 0 {
                            log::warn!("floor tile {x}/{y} is {w}x{h}, not square; dropped");
                            return None;
                        }
                        Some(DecodedTile {
                            x: *x,
                            y: *y,
                            side: w,
                            rgba: rgba.into_raw(),
                        })
                    }
                    Err(error) => {
                        log::warn!("floor tile {x}/{y} failed to decode: {error}");
                        None
                    }
                })
                .collect(),
        }
    }
}

/// [`resample_floor`], with the 2D map's tile *and vector* layers around the
/// radar: ground colour, then the basemap tiles, then the SPC outlook
/// polygons, then the radar echo, then the range ring and the NWS alert
/// polygons, then the city-label tiles — the 2D pane's own stacking order.
///
/// A tile that is missing from `base`/`labels` simply contributes nothing;
/// the ground colour (and whatever tiles are present) show instead, and the
/// caller re-composes when more tiles land. Layers sample bilinearly within
/// each tile, clamped at tile edges — the half-texel seam this admits is
/// under the floor's own texel size at the zoom [`floor_tile_zoom`] picks.
///
/// The `vectors` layers are rasterized into two floor-lattice buffers first
/// (through [`geo_to_floor_texel`] — the shared mapping run backwards, not a
/// second projection) and composited per texel at their slot in the stack,
/// so a shape and a tile pixel naming the same ground land on the same
/// texel by construction.
#[allow(clippy::too_many_arguments)]
pub fn compose_floor(
    source: &[u8],
    site_lat_deg: f64,
    site_lon_deg: f64,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
    base: &TileLayer,
    labels: &TileLayer,
    vectors: &FloorVectors,
) -> Option<FloorImage> {
    let max_range_km = rustdar_radar::types::MAX_RANGE_KM;
    let texels = source.len() / 4;
    let side = (texels as f64).sqrt() as usize;
    if side == 0 || side * side * 4 != source.len() {
        return None;
    }
    if x_range_km.1 <= x_range_km.0 || y_range_km.1 <= y_range_km.0 {
        return None;
    }
    if !(site_lat_deg.is_finite() && site_lon_deg.is_finite()) || site_lat_deg.abs() >= 89.0 {
        return None;
    }

    // The raster's own projection constants, as `MercatorProjection` and
    // `ImageBounds::from_radar_site` build them.
    let site_lat_rad = site_lat_deg.to_radians();
    let cos_site_lat = site_lat_rad.cos();
    let px_per_km = side as f64 / (2.0 * max_range_km);
    let centre_px = side as f64 / 2.0;
    let lat_deg_per_km = 1.0 / 111.32;
    let max_lat = site_lat_deg + max_range_km * lat_deg_per_km;
    let min_lat = site_lat_deg - max_range_km * lat_deg_per_km;
    let merc_top = mercator_y(max_lat.to_radians());
    let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));

    let out_w = FLOOR_TEXELS as usize;
    let out_h = FLOOR_TEXELS as usize;

    // The vector layers, rasterized onto the floor's own lattice through the
    // shared mapping's inverse. `None` where a layer holds nothing, so the
    // texel loop pays for shapes only when there are shapes.
    let footprint = FloorFootprint {
        site_lat_deg,
        site_lon_deg,
        x_range_km,
        y_range_km,
        out_w,
        out_h,
    };
    let under_shapes = rasterize_shape_layer(&vectors.under_radar, &footprint);
    let over_shapes = {
        // The ring goes into the over-radar buffer *first*: the pane draws
        // it with the radar layer, and the alert polygons (drawn later in
        // the pane's order) blend over it.
        let mut shapes: Vec<FloorShape> = Vec::new();
        if vectors.range_ring {
            shapes.push(range_ring_shape(site_lat_deg, site_lon_deg));
        }
        shapes.extend(vectors.over_radar.iter().cloned());
        rasterize_shape_layer(&shapes, &footprint)
    };
    let buffer_at = |buffer: &Option<Vec<u8>>, row: usize, col: usize| -> [f64; 4] {
        match buffer {
            None => [0.0; 4],
            Some(buffer) => {
                let at = (row * out_w + col) * 4;
                [
                    f64::from(buffer[at]),
                    f64::from(buffer[at + 1]),
                    f64::from(buffer[at + 2]),
                    f64::from(buffer[at + 3]),
                ]
            }
        }
    };

    let mut rgba = Vec::with_capacity(out_w * out_h * 4);
    for row in 0..out_h {
        // Row 0 is the footprint's north edge.
        let dy_km =
            y_range_km.1 - (row as f64 + 0.5) / out_h as f64 * (y_range_km.1 - y_range_km.0);
        let lat_rad = site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
        let cos_correction = cos_site_lat / lat_rad.cos();
        let py = (merc_top - mercator_y(lat_rad)) * merc_scale;
        // This row's slippy y is shared by every column: it depends only on
        // latitude. `merc_y` is in `(-π, π)`, π at the north pole.
        let merc_y = mercator_y(lat_rad);
        for col in 0..out_w {
            let dx_km =
                x_range_km.0 + (col as f64 + 0.5) / out_w as f64 * (x_range_km.1 - x_range_km.0);
            let px = centre_px + dx_km * cos_correction * px_per_km;
            // The raster's own longitude for this texel — the module doc
            // derives this line from the `px` line above; it is the same
            // mapping, not a second one.
            let lon_deg = site_lon_deg + dx_km / (111.32 * lat_rad.cos());

            let mut texel = [
                f64::from(FLOOR_GROUND_RGBA[0]),
                f64::from(FLOOR_GROUND_RGBA[1]),
                f64::from(FLOOR_GROUND_RGBA[2]),
            ];
            over(&mut texel, sample_layer(base, lon_deg, merc_y));
            over(&mut texel, buffer_at(&under_shapes, row, col));
            over(&mut texel, bilinear(source, side, px, py));
            over(&mut texel, buffer_at(&over_shapes, row, col));
            over(&mut texel, sample_layer(labels, lon_deg, merc_y));
            rgba.extend_from_slice(&[
                texel[0].round() as u8,
                texel[1].round() as u8,
                texel[2].round() as u8,
                255,
            ]);
        }
    }
    Some(FloorImage {
        size: [out_w as u32, out_h as u32],
        rgba,
    })
}

/// `layer` sampled at a longitude and Mercator y, straight RGBA — transparent
/// where the layer holds no tile or the tile's own pixel is transparent.
fn sample_layer(layer: &TileLayer, lon_deg: f64, merc_y: f64) -> [f64; 4] {
    if layer.tiles.is_empty() {
        return [0.0; 4];
    }
    let n = f64::from(1u32 << layer.zoom.min(31));
    let tile_x = (lon_deg + 180.0) / 360.0 * n;
    let tile_y = (1.0 - merc_y / PI) / 2.0 * n;
    for tile in &layer.tiles {
        let fx = tile_x - f64::from(tile.x);
        let fy = tile_y - f64::from(tile.y);
        if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
            continue;
        }
        let side = tile.side as usize;
        if side * side * 4 != tile.rgba.len() {
            continue;
        }
        // Clamp the bilinear kernel inside this tile: the neighbour's edge
        // pixels are a different allocation, and half a tile pixel of clamp
        // at the seam is under the floor's own texel size.
        let px = (fx * side as f64).clamp(0.5, side as f64 - 0.5);
        let py = (fy * side as f64).clamp(0.5, side as f64 - 0.5);
        return bilinear(&tile.rgba, side, px, py);
    }
    [0.0; 4]
}

/// Straight-alpha `layer` composited over opaque `under`, both gamma-encoded —
/// the same convention every raster in this codebase composites in (egui's
/// own, as the blit's doc lays out).
fn over(under: &mut [f64; 3], layer: [f64; 4]) {
    let alpha = layer[3] / 255.0;
    for (channel, ground) in under.iter_mut().enumerate() {
        *ground = layer[channel] * alpha + *ground * (1.0 - alpha);
    }
}

// ── The vector shapes ────────────────────────────────────────────────────

/// The floor's registration inputs, bundled so every shape helper reads the
/// one set [`compose_floor`] validated rather than five loose parameters.
pub struct FloorFootprint {
    pub site_lat_deg: f64,
    pub site_lon_deg: f64,
    pub x_range_km: (f64, f64),
    pub y_range_km: (f64, f64),
    pub out_w: usize,
    pub out_h: usize,
}

/// The shared mapping run **backwards**: the continuous floor texel where
/// the ground point `(lat_deg, lon_deg)` lands, as `(col, row)` with integer
/// values naming texel centres.
///
/// Each line is the forward mapping's line solved for its other variable —
/// see the module doc's derivation. In particular the `cos φ` is the
/// **vertex's** latitude, exactly as the forward evaluates it at the texel
/// row's: substituting the site's `cos φ₀` here is the classic
/// second-projection bug, and it is what the corner probe of
/// `a_warning_vertex_lands_on_the_texel_the_gate_and_the_tile_name` exists
/// to kill.
pub fn geo_to_floor_texel(lat_deg: f64, lon_deg: f64, f: &FloorFootprint) -> (f64, f64) {
    let lat_rad = lat_deg.to_radians();
    let dy_km = (lat_rad - f.site_lat_deg.to_radians()) * rustdar_radar::types::EARTH_RADIUS_KM;
    let dx_km = (lon_deg - f.site_lon_deg) * 111.32 * lat_rad.cos();
    let col = (dx_km - f.x_range_km.0) / (f.x_range_km.1 - f.x_range_km.0) * f.out_w as f64 - 0.5;
    let row = (f.y_range_km.1 - dy_km) / (f.y_range_km.1 - f.y_range_km.0) * f.out_h as f64 - 0.5;
    (col, row)
}

/// The 230 km range ring as an ordinary [`FloorShape`]: a 256-gon whose
/// vertices are produced by the mapping's own **forward** lines from
/// kilometre offsets (`φ = φ₀ + dy/R`, `λ = λ₀ + dx/(111.32 cos φ)`), so
/// the ring rides the identical rasterizer every polygon does and cannot
/// grow a projection of its own. 256 chords sag under 20 m against a 0.9 km
/// texel.
///
/// The 2D pane's ring is a screen-space circle whose radius is the projected
/// *north* offset; this one is the same 230 km measured along every bearing.
/// The two differ by the Mercator stretch across the box — fractions of a
/// texel at floor scale — and the kilometre form is the one the mapping can
/// express without a projector.
fn range_ring_shape(site_lat_deg: f64, site_lon_deg: f64) -> FloorShape {
    let radius_km = rustdar_radar::types::MAX_RANGE_KM;
    let site_lat_rad = site_lat_deg.to_radians();
    let ring = (0..256)
        .map(|i| {
            let theta = f64::from(i) / 256.0 * 2.0 * PI;
            let (dx_km, dy_km) = (radius_km * theta.sin(), radius_km * theta.cos());
            let lat_rad = site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
            let lon_deg = site_lon_deg + dx_km / (111.32 * lat_rad.cos());
            (lat_rad.to_degrees(), lon_deg)
        })
        .collect();
    FloorShape {
        ring,
        fill_rgba: [0, 0, 0, 0],
        stroke_rgba: RANGE_RING_RGBA,
    }
}

/// Rasterize `shapes` onto the floor lattice: a straight-alpha RGBA buffer
/// aligned texel-for-texel with the floor, or `None` when nothing would
/// paint. Fill first, then stroke over it, per shape — tiny-skia's own
/// order in the pane's `draw_feature` — and shapes blend over earlier
/// shapes in list order, as the pane's rasterizers iterate.
fn rasterize_shape_layer(shapes: &[FloorShape], f: &FloorFootprint) -> Option<Vec<u8>> {
    if shapes.is_empty() {
        return None;
    }
    let mut buffer = vec![0u8; f.out_w * f.out_h * 4];
    // Coverage stamps: a translucent stroke must tint each texel once per
    // shape however many segments cross it, exactly as a stroked path
    // covers a pixel once. `0` is "never stamped".
    let mut stamps = vec![0u32; f.out_w * f.out_h];
    let mut painted = false;

    for (index, shape) in shapes.iter().enumerate() {
        let ring = strip_closing_dup(&shape.ring);
        if ring.len() < 2 {
            continue;
        }
        let vertices: Vec<(f64, f64)> = ring
            .iter()
            .map(|&(lat, lon)| geo_to_floor_texel(lat, lon, f))
            .collect();
        // Cheap whole-shape cull: a shape entirely off the footprint draws
        // nothing, and warned counties a country away should cost nothing.
        let on_lattice = |&(c, r): &(f64, f64)| {
            (-1.0..=f.out_w as f64).contains(&c) && (-1.0..=f.out_h as f64).contains(&r)
        };
        let (min_c, max_c) = min_max(vertices.iter().map(|v| v.0));
        let (min_r, max_r) = min_max(vertices.iter().map(|v| v.1));
        if max_c < -1.0 || min_c > f.out_w as f64 || max_r < -1.0 || min_r > f.out_h as f64 {
            // The bounding box misses even when an edge between two
            // off-lattice vertices could cross the corner — but such an
            // edge's own box would intersect, and this test is on the
            // shape's box, which contains every edge's.
            if !vertices.iter().any(on_lattice) {
                continue;
            }
        }
        painted = true;

        // Two stamp generations per shape: the fill's, then the stroke's, so
        // the stroke blends over the shape's own fill (the pane's order) yet
        // each covers a texel at most once.
        let fill_stamp = (index as u32) * 2 + 1;
        let stroke_stamp = (index as u32) * 2 + 2;
        if shape.fill_rgba[3] > 0 && vertices.len() >= 3 {
            fill_even_odd(&vertices, f, &mut stamps, fill_stamp, |row, col| {
                blend_texel(&mut buffer, f.out_w, row, col, shape.fill_rgba);
            });
        }
        if shape.stroke_rgba[3] > 0 {
            let width = stroke_width_texels(min_c, max_c, min_r, max_r);
            stroke_ring(
                &vertices,
                width,
                f,
                &mut stamps,
                stroke_stamp,
                |row, col| {
                    blend_texel(&mut buffer, f.out_w, row, col, shape.stroke_rgba);
                },
            );
        }
    }
    painted.then_some(buffer)
}

/// An approximation of the pane's `scaled_stroke_width` rule at floor scale,
/// not its numbers: the pane thins by `min_dim / 40` from a per-layer base
/// (1.5 px for alerts, 2.0 for MDs) down to a 0.5 px floor; the floor uses
/// the one base of 2 texels for every shape, floored at 1 because a
/// sub-texel stroke on this lattice would vanish entirely.
fn stroke_width_texels(min_c: f64, max_c: f64, min_r: f64, max_r: f64) -> usize {
    let min_dim = (max_c - min_c).min(max_r - min_r).max(0.0);
    ((min_dim / 40.0 * 2.0).clamp(1.0, 2.0)).round() as usize
}

/// `(min, max)` of a non-empty iterator; the callers guarantee vertices.
fn min_max(values: impl Iterator<Item = f64>) -> (f64, f64) {
    values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
        (lo.min(v), hi.max(v))
    })
}

/// Even-odd scanline fill over texel centres: texel `(row, col)` paints when
/// a horizontal ray at `y = row` crosses the ring's edges an odd number of
/// times left of `x = col` — the *click* rule (`point_in_polygon`'s ray
/// cast), evaluated a row at a time. The pane **paints** with tiny-skia's
/// `FillRule::Winding` (nonzero), not this rule; on a simple ring the two
/// fill identically, and they diverge only on a self-intersecting exterior,
/// which valid outlook, MD and alert geometry does not produce.
fn fill_even_odd(
    vertices: &[(f64, f64)],
    f: &FloorFootprint,
    stamps: &mut [u32],
    stamp: u32,
    mut paint: impl FnMut(usize, usize),
) {
    let (min_r, max_r) = min_max(vertices.iter().map(|v| v.1));
    let row_lo = min_r.ceil().max(0.0) as usize;
    let row_hi = (max_r.floor().min(f.out_h as f64 - 1.0)) as i64;
    if row_hi < row_lo as i64 {
        return;
    }
    let mut crossings: Vec<f64> = Vec::with_capacity(8);
    for row in row_lo..=row_hi as usize {
        let y = row as f64;
        crossings.clear();
        for i in 0..vertices.len() {
            let (x0, y0) = vertices[i];
            let (x1, y1) = vertices[(i + 1) % vertices.len()];
            if (y0 > y) != (y1 > y) {
                crossings.push(x0 + (y - y0) * (x1 - x0) / (y1 - y0));
            }
        }
        crossings.sort_by(|a, b| a.total_cmp(b));
        for pair in crossings.chunks_exact(2) {
            let col_lo = pair[0].ceil().max(0.0) as usize;
            let col_hi = pair[1].floor().min(f.out_w as f64 - 1.0) as i64;
            if col_hi < col_lo as i64 {
                continue;
            }
            for col in col_lo..=col_hi as usize {
                let at = row * f.out_w + col;
                if stamps[at] != stamp {
                    stamps[at] = stamp;
                    paint(row, col);
                }
            }
        }
    }
}

/// Stroke the closed ring: each edge clipped to the lattice, walked by DDA,
/// each step stamping a `width`-texel square. The stamps keep a translucent
/// stroke from double-tinting where segments meet or overlap.
fn stroke_ring(
    vertices: &[(f64, f64)],
    width: usize,
    f: &FloorFootprint,
    stamps: &mut [u32],
    stamp: u32,
    mut paint: impl FnMut(usize, usize),
) {
    let mut stamp_square = |c: f64, r: f64| {
        // A width-2 square hangs half a texel below-right of the line; at
        // 0.9 km per texel that bias is invisible and the arithmetic stays
        // integer.
        let (c0, r0) = (c.round() as i64, r.round() as i64);
        for dr in 0..width as i64 {
            for dc in 0..width as i64 {
                let (col, row) = (c0 + dc, r0 + dr);
                if col < 0 || row < 0 || col >= f.out_w as i64 || row >= f.out_h as i64 {
                    continue;
                }
                let at = row as usize * f.out_w + col as usize;
                if stamps[at] != stamp {
                    stamps[at] = stamp;
                    paint(row as usize, col as usize);
                }
            }
        }
    };
    let closed = vertices.len() > 2;
    let edges = if closed {
        vertices.len()
    } else {
        vertices.len() - 1
    };
    for i in 0..edges {
        let p0 = vertices[i];
        let p1 = vertices[(i + 1) % vertices.len()];
        let Some(((x0, y0), (x1, y1))) = clip_segment(p0, p1, f.out_w as f64, f.out_h as f64)
        else {
            continue;
        };
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0);
        let mut t = 0.0;
        while t <= steps {
            let k = t / steps;
            stamp_square(x0 + (x1 - x0) * k, y0 + (y1 - y0) * k);
            t += 1.0;
        }
    }
}

/// Liang–Barsky clip of the segment to the lattice grown by two texels, so
/// a stroke centred just off the edge still paints its in-bounds half.
/// `None` for a segment entirely outside.
fn clip_segment(
    (x0, y0): (f64, f64),
    (x1, y1): (f64, f64),
    w: f64,
    h: f64,
) -> Option<((f64, f64), (f64, f64))> {
    const PAD: f64 = 2.0;
    let (dx, dy) = (x1 - x0, y1 - y0);
    let mut t0 = 0.0f64;
    let mut t1 = 1.0f64;
    for (p, q) in [
        (-dx, x0 + PAD),
        (dx, w + PAD - x0),
        (-dy, y0 + PAD),
        (dy, h + PAD - y0),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            t0 = t0.max(r);
        } else {
            t1 = t1.min(r);
        }
        if t0 > t1 {
            return None;
        }
    }
    Some(((x0 + dx * t0, y0 + dy * t0), (x0 + dx * t1, y0 + dy * t1)))
}

/// Source-over of straight-alpha `rgba` onto the straight-alpha `buffer`
/// texel — the buffer is itself a layer, so it accumulates in straight
/// alpha and [`over`] composites it into the floor later.
fn blend_texel(buffer: &mut [u8], out_w: usize, row: usize, col: usize, rgba: [u8; 4]) {
    let at = (row * out_w + col) * 4;
    let src_a = f64::from(rgba[3]) / 255.0;
    let dst_a = f64::from(buffer[at + 3]) / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return;
    }
    for channel in 0..3 {
        let src = f64::from(rgba[channel]);
        let dst = f64::from(buffer[at + channel]);
        buffer[at + channel] = ((src * src_a + dst * dst_a * (1.0 - src_a)) / out_a).round() as u8;
    }
    buffer[at + 3] = (out_a * 255.0).round() as u8;
}

/// Drops GeoJSON's closing duplicate vertex (`last == first`) — the same
/// tolerance the pane's rasterizer applies before building its path.
fn strip_closing_dup(ring: &[(f64, f64)]) -> &[(f64, f64)] {
    if ring.len() > 3 && ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    }
}

/// The slippy zoom whose tiles resolve about one tile pixel per floor texel
/// over this footprint, clamped to what the source can serve.
///
/// At `2^z · 256` pixels around the world, the footprint's longitude span
/// covers `span/360` of that; solving for the floor's own [`FLOOR_TEXELS`]
/// gives `z = log2(360 · FLOOR_TEXELS / (256 · span))`. The default 460 km
/// box at mid-latitudes lands at z7 — two-and-a-bit tiles across, city
/// labels at their native size, which is the GR look.
pub fn floor_tile_zoom(site_lat_deg: f64, x_range_km: (f64, f64), max_zoom: u8) -> u8 {
    let width_km = (x_range_km.1 - x_range_km.0).max(1.0);
    let cos_lat = site_lat_deg.to_radians().cos().max(0.01);
    let lon_span_deg = width_km / (111.32 * cos_lat);
    let z = (360.0 * f64::from(FLOOR_TEXELS) / (256.0 * lon_span_deg)).log2();
    (z.round().max(0.0) as u8).min(max_zoom)
}

/// Every slippy tile id at `zoom` the footprint touches, as `(x, y)` pairs —
/// through the same corner mapping the composite samples with.
///
/// Longitude reach is widest at the row furthest from the equator (the
/// `cos φ` in the mapping), so both y extremes are evaluated and the wider
/// wins. Bounded by construction: [`floor_tile_zoom`] picks a zoom where the
/// box is a few tiles across, and the count is clamped to an 8×8 window
/// against a degenerate request.
pub fn floor_tile_ids(
    site_lat_deg: f64,
    site_lon_deg: f64,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
    zoom: u8,
) -> Vec<(u32, u32)> {
    let site_lat_rad = site_lat_deg.to_radians();
    let lat_at = |dy_km: f64| site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
    let lon_at = |dx_km: f64, lat_rad: f64| site_lon_deg + dx_km / (111.32 * lat_rad.cos());

    let n = f64::from(1u32 << zoom.min(31));
    let max_index = (1u64 << zoom.min(31)) - 1;
    let tile_x = |lon: f64| ((lon + 180.0) / 360.0 * n).floor();
    let tile_y = |lat_rad: f64| ((1.0 - mercator_y(lat_rad) / PI) / 2.0 * n).floor();

    let (lat_s, lat_n) = (lat_at(y_range_km.0), lat_at(y_range_km.1));
    let mut x_lo = f64::INFINITY;
    let mut x_hi = f64::NEG_INFINITY;
    for lat in [lat_s, lat_n] {
        for dx in [x_range_km.0, x_range_km.1] {
            let x = tile_x(lon_at(dx, lat));
            x_lo = x_lo.min(x);
            x_hi = x_hi.max(x);
        }
    }
    let clamp_to = |v: f64| (v.max(0.0) as u64).min(max_index) as u32;
    let (x_lo, x_hi) = (clamp_to(x_lo), clamp_to(x_hi));
    // Slippy y grows southwards, so the north edge is the smaller index.
    let (y_lo, y_hi) = (clamp_to(tile_y(lat_n)), clamp_to(tile_y(lat_s)));

    let mut ids = Vec::new();
    for y in y_lo..=y_hi.min(y_lo + 7) {
        for x in x_lo..=x_hi.min(x_lo + 7) {
            ids.push((x, y));
        }
    }
    ids
}

/// `source` bilinearly sampled at `(px, py)`, straight RGBA. Outside the
/// image is transparent — beyond the raster's range there is no echo, and the
/// ground colour supplies the rest.
fn bilinear(source: &[u8], side: usize, px: f64, py: f64) -> [f64; 4] {
    let sample = |ix: i64, iy: i64| -> [f64; 4] {
        if ix < 0 || iy < 0 || ix >= side as i64 || iy >= side as i64 {
            return [0.0; 4];
        }
        let at = (iy as usize * side + ix as usize) * 4;
        [
            f64::from(source[at]),
            f64::from(source[at + 1]),
            f64::from(source[at + 2]),
            f64::from(source[at + 3]),
        ]
    };
    let fx = px - 0.5;
    let fy = py - 0.5;
    let ix = fx.floor();
    let iy = fy.floor();
    let tx = fx - ix;
    let ty = fy - iy;
    let (ix, iy) = (ix as i64, iy as i64);
    let mut out = [0.0f64; 4];
    for (channel, slot) in out.iter_mut().enumerate() {
        let top = sample(ix, iy)[channel] * (1.0 - tx) + sample(ix + 1, iy)[channel] * tx;
        let bottom =
            sample(ix, iy + 1)[channel] * (1.0 - tx) + sample(ix + 1, iy + 1)[channel] * tx;
        *slot = top * (1.0 - ty) + bottom * ty;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic source: side 64, transparent everywhere except an opaque
    /// red texel at a chosen pixel.
    fn source_with_dot(side: usize, at: (usize, usize)) -> Vec<u8> {
        let mut image = vec![0u8; side * side * 4];
        let idx = (at.1 * side + at.0) * 4;
        image[idx..idx + 4].copy_from_slice(&[255, 0, 0, 255]);
        image
    }

    /// Where the floor put a colour above the ground, as (col, row) of the
    /// brightest red texel.
    fn brightest_red(floor: &FloorImage) -> (usize, usize) {
        let mut best = (0, 0);
        let mut best_red = 0u8;
        for row in 0..floor.size[1] as usize {
            for col in 0..floor.size[0] as usize {
                let at = (row * floor.size[0] as usize + col) * 4;
                if floor.rgba[at] > best_red {
                    best_red = floor.rgba[at];
                    best = (col, row);
                }
            }
        }
        assert!(
            best_red > FLOOR_GROUND_RGBA[0],
            "no echo landed on the floor"
        );
        best
    }

    /// The site's own pixel lands in the middle of a site-centred floor.
    ///
    /// The centre is the one point every projection convention agrees on, so
    /// this is the control; the offset cases below are the test.
    #[test]
    fn the_sites_pixel_lands_in_the_middle_of_a_site_centred_floor() {
        let side = 64;
        let source = source_with_dot(side, (32, 32));
        let floor = resample_floor(&source, 35.0, (-230.0, 230.0), (-230.0, 230.0))
            .expect("a resamplable floor");
        let (col, row) = brightest_red(&floor);
        let mid = FLOOR_TEXELS as usize / 2;
        assert!(
            col.abs_diff(mid) <= 8 && row.abs_diff(mid) <= 8,
            "the site's echo landed at ({col}, {row}) of {FLOOR_TEXELS}, not the centre",
        );
    }

    /// A dot north-east of the site lands in the floor's upper-right quadrant,
    /// and the vertical placement follows the raster's Mercator spacing.
    ///
    /// Two mutations this closes, both of which leave the centred case green:
    /// flipping the v axis (north landing at the bottom), and reading the
    /// footprint through a linear-latitude mapping instead of the raster's
    /// Mercator one — at 35° N the Mercator rows are measurably denser towards
    /// the equator, so the linear read puts the dot rows away from where the
    /// raster drew it.
    #[test]
    fn an_echo_north_east_of_the_site_lands_north_east_and_on_the_mercator_row() {
        let side = 256;
        // The raster pixel where its own forward projection puts a point
        // 150 km north, 100 km east of a site at 35 N: run the forward
        // arithmetic from render_gate.
        let site_lat_rad = 35.0f64.to_radians();
        let lat_rad = site_lat_rad + 150.0 / rustdar_radar::types::EARTH_RADIUS_KM;
        let px_per_km = side as f64 / 460.0;
        let px = side as f64 / 2.0 + 100.0 * (site_lat_rad.cos() / lat_rad.cos()) * px_per_km;
        let max_lat: f64 = 35.0 + 230.0 / 111.32;
        let min_lat: f64 = 35.0 - 230.0 / 111.32;
        let merc_top = mercator_y(max_lat.to_radians());
        let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));
        let py = (merc_top - mercator_y(lat_rad)) * merc_scale;

        let source = source_with_dot(side, (px as usize, py as usize));
        let floor = resample_floor(&source, 35.0, (-230.0, 230.0), (-230.0, 230.0))
            .expect("a resamplable floor");
        let (col, row) = brightest_red(&floor);

        // Expected floor texel: the footprint is linear in km, so 150 km north
        // of a ±230 km box is (230 - 150) / 460 of the way down from the top.
        let want_col = ((100.0 + 230.0) / 460.0 * FLOOR_TEXELS as f64) as usize;
        let want_row = ((230.0 - 150.0) / 460.0 * FLOOR_TEXELS as f64) as usize;
        assert!(
            col.abs_diff(want_col) <= 4,
            "east placement: got column {col}, the box arithmetic says {want_col}",
        );
        assert!(
            row.abs_diff(want_row) <= 4,
            "north placement: got row {row}, the box arithmetic says {want_row} — \
             a v flip or a linear-latitude read both move it here",
        );
    }

    /// A region footprint off the site's centre reads the matching part of
    /// the raster: the same dot, through a box that puts it at the box centre.
    #[test]
    fn an_off_centre_footprint_reads_the_matching_part_of_the_raster() {
        let side = 256;
        let site_lat_rad = 35.0f64.to_radians();
        let lat_rad = site_lat_rad + 150.0 / rustdar_radar::types::EARTH_RADIUS_KM;
        let px_per_km = side as f64 / 460.0;
        let px = side as f64 / 2.0 + 100.0 * (site_lat_rad.cos() / lat_rad.cos()) * px_per_km;
        let max_lat: f64 = 35.0 + 230.0 / 111.32;
        let min_lat: f64 = 35.0 - 230.0 / 111.32;
        let merc_top = mercator_y(max_lat.to_radians());
        let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));
        let py = (merc_top - mercator_y(lat_rad)) * merc_scale;

        let source = source_with_dot(side, (px as usize, py as usize));
        // A 80 km-wide box centred on the dot's (100, 150) km offset.
        let floor = resample_floor(&source, 35.0, (60.0, 140.0), (110.0, 190.0))
            .expect("a resamplable floor");
        let (col, row) = brightest_red(&floor);
        let mid = FLOOR_TEXELS as usize / 2;
        assert!(
            col.abs_diff(mid) <= 8 && row.abs_diff(mid) <= 8,
            "a box centred on the echo must put it at the floor's centre, got \
             ({col}, {row})",
        );
    }

    /// Where no echo painted, the floor is the opaque ground colour — never
    /// transparent, never the raster's transparent black leaking through.
    #[test]
    fn bare_ground_is_the_ground_colour_and_opaque() {
        let source = vec![0u8; 64 * 64 * 4];
        let floor = resample_floor(&source, 35.0, (-230.0, 230.0), (-230.0, 230.0))
            .expect("a resamplable floor");
        assert_eq!(&floor.rgba[..4], &FLOOR_GROUND_RGBA);
        assert!(
            floor.rgba.chunks_exact(4).all(|px| px[3] == 255),
            "the floor must be opaque ground everywhere",
        );
    }

    /// Plant the same geographic point `(dx_km, dy_km)` east/north of a
    /// 35 N site as a radar-raster dot AND a tile-pixel dot — two
    /// independent forward routes — compose, and return where each landed
    /// on the floor, as `(radar texel, tile texel)`.
    fn tile_and_gate_texels(dx_km: f64, dy_km: f64) -> ((usize, usize), (usize, usize)) {
        let side = 256;
        let (site_lat, site_lon) = (35.0f64, -97.0f64);

        // The radar raster's dot, through the raster's forward projection.
        let site_lat_rad = site_lat.to_radians();
        let lat_rad = site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
        let px_per_km = side as f64 / 460.0;
        let px = side as f64 / 2.0 + dx_km * (site_lat_rad.cos() / lat_rad.cos()) * px_per_km;
        let max_lat: f64 = site_lat + 230.0 / 111.32;
        let min_lat: f64 = site_lat - 230.0 / 111.32;
        let merc_top = mercator_y(max_lat.to_radians());
        let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));
        let py = (merc_top - mercator_y(lat_rad)) * merc_scale;
        let source = source_with_dot(side, (px as usize, py as usize));

        // The tile's dot, through the slippy pyramid's forward formulas.
        let zoom = floor_tile_zoom(site_lat, (-230.0, 230.0), 18);
        let n = f64::from(1u32 << zoom);
        let lon = site_lon + dx_km / (111.32 * lat_rad.cos());
        let tile_x_f = (lon + 180.0) / 360.0 * n;
        let tile_y_f = (1.0 - mercator_y(lat_rad) / PI) / 2.0 * n;
        let (tile_x, tile_y) = (tile_x_f.floor(), tile_y_f.floor());
        let tile_side = 256usize;
        let mut tile_rgba = vec![0u8; tile_side * tile_side * 4];
        let (dot_px, dot_py) = (
            ((tile_x_f - tile_x) * tile_side as f64) as usize,
            ((tile_y_f - tile_y) * tile_side as f64) as usize,
        );
        for row in dot_py.saturating_sub(1)..=(dot_py + 1).min(tile_side - 1) {
            for col in dot_px.saturating_sub(1)..=(dot_px + 1).min(tile_side - 1) {
                let at = (row * tile_side + col) * 4;
                tile_rgba[at..at + 4].copy_from_slice(&[0, 255, 0, 255]);
            }
        }
        let base = TileLayer {
            zoom,
            tiles: vec![DecodedTile {
                x: tile_x as u32,
                y: tile_y as u32,
                side: tile_side as u32,
                rgba: tile_rgba,
            }],
        };

        let floor = compose_floor(
            &source,
            site_lat,
            site_lon,
            (-230.0, 230.0),
            (-230.0, 230.0),
            &base,
            &TileLayer::empty(),
            &FloorVectors::none(),
        )
        .expect("a composable floor");

        let (red_col, red_row) = brightest_red(&floor);
        let mut best_green = (0usize, 0usize, 0u8);
        for row in 0..floor.size[1] as usize {
            for col in 0..floor.size[0] as usize {
                let at = (row * floor.size[0] as usize + col) * 4;
                let greenness = floor.rgba[at + 1].saturating_sub(floor.rgba[at]);
                if greenness > best_green.2 {
                    best_green = (col, row, greenness);
                }
            }
        }
        assert!(best_green.2 > 100, "the tile dot never reached the floor");
        ((red_col, red_row), (best_green.0, best_green.1))
    }

    /// The floor texels a planted **warning-polygon vertex** at the same
    /// `(dx_km, dy_km)` painted, through the third consumer: a tiny blue
    /// triangle whose first vertex is the probe point, its geo coordinates
    /// produced by the same forward lines the tile route uses, drawn as an
    /// over-radar shape into an otherwise empty floor.
    fn planted_shape_cluster(dx_km: f64, dy_km: f64) -> Vec<(usize, usize)> {
        let (site_lat, site_lon) = (35.0f64, -97.0f64);
        let site_lat_rad = site_lat.to_radians();
        let lat_rad = site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
        let lon = site_lon + dx_km / (111.32 * lat_rad.cos());
        let lat = lat_rad.to_degrees();
        // Two more vertices ~0.45 km (half a texel) north and east: the
        // stroke must stay a cluster *at* the probe, because the pin
        // measures the cluster's nearest texel and every texel of slack here
        // is a texel of projection error the pin can no longer see.
        let dlat = (0.45f64 / rustdar_radar::types::EARTH_RADIUS_KM).to_degrees();
        let dlon = 0.45 / (111.32 * lat_rad.cos());
        let shape = FloorShape {
            ring: vec![(lat, lon), (lat + dlat, lon), (lat, lon + dlon)],
            fill_rgba: [0, 0, 0, 0],
            stroke_rgba: [0, 0, 255, 255],
        };
        let floor = compose_floor(
            &vec![0u8; 64 * 64 * 4],
            site_lat,
            site_lon,
            (-230.0, 230.0),
            (-230.0, 230.0),
            &TileLayer::empty(),
            &TileLayer::empty(),
            &FloorVectors {
                under_radar: Vec::new(),
                over_radar: vec![shape],
                range_ring: false,
            },
        )
        .expect("a composable floor");
        let mut cluster = Vec::new();
        for row in 0..floor.size[1] as usize {
            for col in 0..floor.size[0] as usize {
                let at = (row * floor.size[0] as usize + col) * 4;
                let blueness =
                    floor.rgba[at + 2].saturating_sub(floor.rgba[at].max(floor.rgba[at + 1]));
                if blueness > 100 {
                    cluster.push((col, row));
                }
            }
        }
        cluster
    }

    /// One mapping, **three** consumers: a warning-polygon vertex naming the
    /// same ground as the radar gate and the tile pixel lands on the same
    /// floor texel — probed mid-box, at the box's far corner, and on the
    /// site's own parallel, because the two classic wrong-inverses die at
    /// *different* probes and each leaves the others green:
    ///
    /// * the site's `cos φ₀` where the vertex's `cos φ` belongs drifts with
    ///   distance from the site's parallel — under 2 texels at (100, 150),
    ///   5 at (−200, −190), and exactly **zero** at `dy = 0` — so only the
    ///   corner kills it (measured: 5 texels there, this test's message);
    /// * a Mercator-row read of the vertex — treating the floor's rows like
    ///   the raster image's — agrees at the box's row *edges* by
    ///   construction and misses most in the middle: ~1.9 texels at
    ///   `dy = 150`, ~1 at the corner, 3.2 on the site's parallel — so only
    ///   the `(120, 0)` probe kills it (measured: 3 texels there).
    ///
    /// The oracle is the *other two consumers*, not a restated formula: the
    /// shape cluster must sit within 2 texels of both the gate's texel and
    /// the tile's, and stay a compact cluster (a smeared or duplicated
    /// stroke is its own failure).
    #[test]
    fn a_warning_vertex_lands_on_the_texel_the_gate_and_the_tile_name() {
        for (dx_km, dy_km) in [(100.0, 150.0), (-200.0, -190.0), (120.0, 0.0)] {
            let (gate, tile) = tile_and_gate_texels(dx_km, dy_km);
            let cluster = planted_shape_cluster(dx_km, dy_km);
            assert!(
                !cluster.is_empty(),
                "at ({dx_km}, {dy_km}) km the warning vertex never reached the floor",
            );
            assert!(
                cluster.len() < 40,
                "at ({dx_km}, {dy_km}) km the shape smeared into {} texels",
                cluster.len(),
            );
            for (name, (col, row)) in [("gate", gate), ("tile", tile)] {
                let nearest = cluster
                    .iter()
                    .map(|(c, r)| c.abs_diff(col).max(r.abs_diff(row)))
                    .min()
                    .unwrap();
                assert!(
                    nearest <= 2,
                    "at ({dx_km}, {dy_km}) km the warning vertex landed {nearest} \
                     texels from the {name}'s texel — the third consumer of the \
                     mapping has parted from it",
                );
            }
        }
    }

    /// One mapping, two consumers: a tile pixel and a radar gate that name
    /// the same ground land on the same floor texel — probed mid-box AND at
    /// the box's far corner.
    ///
    /// The radar dot is planted through `render_gate`'s forward arithmetic
    /// (as the Mercator-row test above does) and the tile dot through the
    /// slippy pyramid's own forward formulas — two independent routes to the
    /// same geographic point. If the composite ever grew a second projection
    /// for tiles, this is where the two dots would part.
    ///
    /// The corner probe is not decoration: the mapping's `cos φ` is the
    /// *row's* latitude, and the plausible second-projection error — reading
    /// the site's `cos φ₀` instead — grows with distance from the site's
    /// parallel. At (100, 150) km it drifts under 2 texels and the mid-box
    /// probe alone would let it live; at (−200, −190) km it reaches ~5
    /// texels and dies here.
    #[test]
    fn a_tile_pixel_and_a_radar_gate_at_the_same_ground_land_on_the_same_texel() {
        for (dx_km, dy_km) in [(100.0, 150.0), (-200.0, -190.0)] {
            let ((red_col, red_row), (green_col, green_row)) = tile_and_gate_texels(dx_km, dy_km);
            let apart = green_col.abs_diff(red_col).max(green_row.abs_diff(red_row));
            assert!(
                apart <= 2,
                "at ({dx_km}, {dy_km}) km the radar gate and the tile pixel \
                 for the same ground landed {apart} texels apart — the two \
                 consumers of the mapping have parted",
            );
        }
    }

    /// The stacking order is the 2D pane's: ground, basemap, radar, labels.
    #[test]
    fn the_layers_stack_ground_basemap_radar_labels() {
        // A world-covering opaque blue basemap tile at zoom 0, and a radar
        // dot at the site's own pixel (the floor's centre).
        let blue_world = || TileLayer {
            zoom: 0,
            tiles: vec![DecodedTile {
                x: 0,
                y: 0,
                side: 8,
                rgba: [0u8, 0, 255, 255].repeat(64),
            }],
        };
        let green_world = TileLayer {
            zoom: 0,
            tiles: vec![DecodedTile {
                x: 0,
                y: 0,
                side: 8,
                rgba: [0u8, 255, 0, 255].repeat(64),
            }],
        };
        let source = source_with_dot(64, (32, 32));

        // Base under radar: the dot's texel is the radar's red, the rest the
        // basemap's blue — not the ground colour.
        let floor = compose_floor(
            &source,
            35.0,
            -97.0,
            (-230.0, 230.0),
            (-230.0, 230.0),
            &blue_world(),
            &TileLayer::empty(),
            &FloorVectors::none(),
        )
        .expect("a composable floor");
        let (col, row) = brightest_red(&floor);
        let mid = FLOOR_TEXELS as usize / 2;
        assert!(
            col.abs_diff(mid) <= 8 && row.abs_diff(mid) <= 8,
            "the radar dot must still land at the centre over a basemap",
        );
        let corner = &floor.rgba[..4];
        assert!(
            corner[2] > 200 && corner[0] < 50,
            "away from the echo the basemap (blue) must show, got {corner:?}",
        );

        // Labels over radar: the same dot texel turns the label layer's
        // green when an opaque label tile covers it.
        let floor = compose_floor(
            &source,
            35.0,
            -97.0,
            (-230.0, 230.0),
            (-230.0, 230.0),
            &blue_world(),
            &green_world,
            &FloorVectors::none(),
        )
        .expect("a composable floor");
        let at = (row * floor.size[0] as usize + col) * 4;
        assert!(
            floor.rgba[at + 1] > 200 && floor.rgba[at] < 50,
            "an opaque label tile must paint over the radar echo, got {:?}",
            &floor.rgba[at..at + 4],
        );
    }

    /// A square ring of `half_km` kilometres about the site, its corners'
    /// geo coordinates produced by the mapping's own forward lines — the
    /// fixtures' one way of naming ground, same as every planted dot.
    fn geo_square(site_lat: f64, site_lon: f64, half_km: f64) -> Vec<(f64, f64)> {
        let site_lat_rad = site_lat.to_radians();
        let corner = |dx: f64, dy: f64| {
            let lat_rad = site_lat_rad + dy / rustdar_radar::types::EARTH_RADIUS_KM;
            (
                lat_rad.to_degrees(),
                site_lon + dx / (111.32 * lat_rad.cos()),
            )
        };
        vec![
            corner(-half_km, half_km),
            corner(half_km, half_km),
            corner(half_km, -half_km),
            corner(-half_km, -half_km),
        ]
    }

    /// The vector layers stack where the pane stacks them: outlooks under
    /// the radar, alerts over it, label tiles over the alerts.
    ///
    /// The mutation this kills is the order swap — an alert drawn into the
    /// under-radar buffer (or an outlook into the over-) leaves every
    /// registration test green and quietly draws warnings *behind* the
    /// storm they warn about.
    #[test]
    fn the_vector_layers_stack_where_the_pane_stacks_them() {
        let (site_lat, site_lon) = (35.0, -97.0);
        let ranges = ((-230.0, 230.0), (-230.0, 230.0));
        let source = source_with_dot(64, (32, 32));
        let outlook = FloorShape {
            ring: geo_square(site_lat, site_lon, 40.0),
            fill_rgba: [0, 255, 0, 255],
            stroke_rgba: [0, 0, 0, 0],
        };
        let alert = FloorShape {
            ring: geo_square(site_lat, site_lon, 15.0),
            fill_rgba: [255, 255, 0, 255],
            stroke_rgba: [0, 0, 0, 0],
        };

        // Outlook under radar: the site's dot stays the radar's red; the
        // outlook's green shows beside it, inside its square.
        let floor = compose_floor(
            &source,
            site_lat,
            site_lon,
            ranges.0,
            ranges.1,
            &TileLayer::empty(),
            &TileLayer::empty(),
            &FloorVectors {
                under_radar: vec![outlook.clone()],
                over_radar: Vec::new(),
                range_ring: false,
            },
        )
        .expect("a composable floor");
        let (dot_col, dot_row) = brightest_red(&floor);
        let mid = FLOOR_TEXELS as usize / 2;
        assert!(
            dot_col.abs_diff(mid) <= 8 && dot_row.abs_diff(mid) <= 8,
            "the radar dot must still land at the centre over an outlook",
        );
        let beside = ((dot_row) * FLOOR_TEXELS as usize + dot_col + 20) * 4;
        let px = &floor.rgba[beside..beside + 3];
        assert!(
            px[1] > 200 && px[0] < 50,
            "beside the echo, inside the outlook square, the outlook's green \
             must show under it, got {px:?}",
        );

        // Alert over radar: the same dot texel turns the alert's yellow.
        let floor = compose_floor(
            &source,
            site_lat,
            site_lon,
            ranges.0,
            ranges.1,
            &TileLayer::empty(),
            &TileLayer::empty(),
            &FloorVectors {
                under_radar: vec![outlook.clone()],
                over_radar: vec![alert.clone()],
                range_ring: false,
            },
        )
        .expect("a composable floor");
        let at = (dot_row * FLOOR_TEXELS as usize + dot_col) * 4;
        let px = &floor.rgba[at..at + 3];
        assert!(
            px[0] > 200 && px[1] > 200 && px[2] < 50,
            "an opaque alert must paint over the radar echo, got {px:?}",
        );

        // Label tiles over the alert: the same texel turns the labels' blue.
        let blue_world = TileLayer {
            zoom: 0,
            tiles: vec![DecodedTile {
                x: 0,
                y: 0,
                side: 8,
                rgba: [0u8, 0, 255, 255].repeat(64),
            }],
        };
        let floor = compose_floor(
            &source,
            site_lat,
            site_lon,
            ranges.0,
            ranges.1,
            &TileLayer::empty(),
            &blue_world,
            &FloorVectors {
                under_radar: vec![outlook],
                over_radar: vec![alert],
                range_ring: false,
            },
        )
        .expect("a composable floor");
        let px = &floor.rgba[at..at + 3];
        assert!(
            px[2] > 200 && px[0] < 50,
            "an opaque label tile must paint over the alert, got {px:?}",
        );
    }

    /// A translucent alert fill tints the ground at its own alpha — the
    /// pane's straight-alpha compositing, not an opaque stamp and not a
    /// double-blend where scanlines meet.
    #[test]
    fn an_alert_fill_tints_the_ground_at_its_own_alpha() {
        let shape = FloorShape {
            ring: geo_square(35.0, -97.0, 60.0),
            fill_rgba: [255, 0, 0, 80],
            stroke_rgba: [0, 0, 0, 0],
        };
        let floor = compose_floor(
            &vec![0u8; 64 * 64 * 4],
            35.0,
            -97.0,
            (-230.0, 230.0),
            (-230.0, 230.0),
            &TileLayer::empty(),
            &TileLayer::empty(),
            &FloorVectors {
                under_radar: Vec::new(),
                over_radar: vec![shape],
                range_ring: false,
            },
        )
        .expect("a composable floor");
        // Interior texel: `over(ground, fill)` exactly once.
        let mid = FLOOR_TEXELS as usize / 2;
        let at = (mid * FLOOR_TEXELS as usize + mid) * 4;
        let expected = {
            let alpha = 80.0 / 255.0;
            [
                (255.0 * alpha + f64::from(FLOOR_GROUND_RGBA[0]) * (1.0 - alpha)).round() as u8,
                (f64::from(FLOOR_GROUND_RGBA[1]) * (1.0 - alpha)).round() as u8,
                (f64::from(FLOOR_GROUND_RGBA[2]) * (1.0 - alpha)).round() as u8,
            ]
        };
        for channel in 0..3 {
            assert!(
                floor.rgba[at + channel].abs_diff(expected[channel]) <= 2,
                "the fill must tint the ground once at alpha 80: texel {:?}, \
                 expected about {expected:?}",
                &floor.rgba[at..at + 3],
            );
        }
    }

    /// The range ring stands 230 km from the site — [`MAX_RANGE_KM`], the
    /// radius `render_radar_range_ring` draws — due east and due north, in
    /// its own faint grey, and nowhere near the site.
    #[test]
    fn the_range_ring_stands_at_its_radius() {
        let floor = compose_floor(
            &vec![0u8; 64 * 64 * 4],
            35.0,
            -97.0,
            (-300.0, 300.0),
            (-300.0, 300.0),
            &TileLayer::empty(),
            &TileLayer::empty(),
            &FloorVectors {
                under_radar: Vec::new(),
                over_radar: Vec::new(),
                range_ring: true,
            },
        )
        .expect("a composable floor");
        let side = FLOOR_TEXELS as usize;
        let ringish = |col: usize, row: usize| {
            let at = (row * side + col) * 4;
            let [r, g, b] = [floor.rgba[at], floor.rgba[at + 1], floor.rgba[at + 2]];
            // RANGE_RING_RGBA over the ground: a grey near (58, 59, 62).
            r > FLOOR_GROUND_RGBA[0] + 20 && r.abs_diff(g) < 8 && g.abs_diff(b) < 10
        };
        // 230 km east of a ±300 km box: col ≈ (230+300)/600·512 ≈ 452, on
        // the site's own row ≈ 255; 230 km north mirrors onto row ≈ 59.
        let found_east = (450..=454).any(|col| (252..=259).any(|row| ringish(col, row)));
        let found_north = (57..=62).any(|row| (252..=259).any(|col| ringish(col, row)));
        assert!(
            found_east && found_north,
            "the ring must stand ~452 texels east and ~59 rows north in a \
             ±300 km box (east found: {found_east}, north found: {found_north})",
        );
        let mid = side / 2;
        let at = (mid * side + mid) * 4;
        assert_eq!(
            &floor.rgba[at..at + 4],
            &FLOOR_GROUND_RGBA,
            "the site itself is not on the ring",
        );
    }

    /// Degenerate inputs are refused, not clamped.
    #[test]
    fn a_floor_that_cannot_be_registered_is_refused() {
        let source = vec![0u8; 64 * 64 * 4];
        // Not a square image.
        assert!(resample_floor(&source[..60], 35.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
        // Degenerate ranges and range order.
        assert!(resample_floor(&source, 35.0, (1.0, 1.0), (-1.0, 1.0)).is_none());
        assert!(resample_floor(&source, 35.0, (-1.0, 1.0), (1.0, -1.0)).is_none());
        // A latitude with no finite Mercator row, and a pole, where cos(lat)
        // reaches zero. The raster's half-extent is no longer an input at
        // all — it is the projection's own constant — so there is no wrong
        // extent left to refuse.
        assert!(resample_floor(&source, f64::NAN, (-1.0, 1.0), (-1.0, 1.0)).is_none());
        assert!(resample_floor(&source, 90.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
    }
}
