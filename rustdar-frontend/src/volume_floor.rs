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

#[path = "volume_floor/tests.rs"]
#[cfg(test)]
mod tests;
