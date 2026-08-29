use crate::tile_source::HttpsTiles;
use squallar_geo::{lat_to_tile_y, lon_to_tile_x};
use walkers::{
    TileId,
    sources::{Attribution, TileSource},
};

/// CartoDB's credit: the raster basemap's, and nothing else's.
///
/// **This is not "the" attribution any more.** The map panel draws whatever
/// [`walkers::Tiles::attribution`] answers for the source it is actually
/// fetching from, so a build reading our own archive credits OpenMapTiles
/// without this const being involved. It is the [`CartoDb`] impl's string, and
/// the panel's fallback for a frame with no source at all.
///
/// `\u{a9}` is U+00A9, registered in `ui_glyphs` as the basemap attribution
/// copyright -- never spelled `(c)`.
pub const ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap \u{a9} CartoDB";

/// Where the credit links. ODbL wants the notice reachable, not just shown.
pub const ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";

/// The credit a build that *meant* to draw the vector archive carries once it
/// has fallen back to the rasters.
///
/// **This is the on-screen half of reporting the fault**, and it is drawn in the
/// corner the credit already occupies rather than as a new surface: the panel
/// reads [`walkers::Tiles::attribution`] off whichever source it is actually
/// drawing, so replacing the source replaces the words. Without it the fallback
/// would be indistinguishable from a build configured for CartoDB in the first
/// place, which is exactly the silent-partial-success shape this workspace
/// keeps finding.
///
/// It names what is wrong, not what the reader should do about it: the detail
/// that lets them fix it -- the URL and the transport error -- is one
/// `log::error!` away and would not fit here.
///
/// ASCII apart from the two copyright signs, deliberately: `ui_glyphs` gates
/// every non-ASCII character UI text carries against the bundled fonts, and an
/// em dash is not on that inventory.
#[cfg(feature = "basemap-vector")]
pub const FALLBACK_ATTRIBUTION_TEXT: &str =
    "\u{a9} OpenStreetMap \u{a9} CartoDB (basemap archive unavailable)";

/// What [`MapTileState::ensure_base_tiles`] credits a fallback source with.
///
/// A `cfg` selecting a value, not a branch: on a build with no vector archive
/// there is nothing to fall back *from*, the fallback arm is unreachable, and
/// the ordinary credit is the one that would be honest if it were not.
#[cfg(feature = "basemap-vector")]
const FALLBACK_CREDIT: &str = FALLBACK_ATTRIBUTION_TEXT;
/// See the arm above.
#[cfg(not(feature = "basemap-vector"))]
const FALLBACK_CREDIT: &str = ATTRIBUTION_TEXT;

/// CartoDB tile source variants.
#[derive(Clone)]
pub enum CartoDbStyle {
    LightNoLabels,
    DarkNoLabels,
}

#[derive(Clone)]
pub struct CartoDb {
    style: CartoDbStyle,
}

impl CartoDb {
    pub fn light() -> Self {
        Self {
            style: CartoDbStyle::LightNoLabels,
        }
    }

    pub fn dark() -> Self {
        Self {
            style: CartoDbStyle::DarkNoLabels,
        }
    }
}

impl TileSource for CartoDb {
    fn tile_url(&self, tile_id: TileId) -> String {
        let style_name = match self.style {
            CartoDbStyle::LightNoLabels => "light_nolabels",
            CartoDbStyle::DarkNoLabels => "dark_nolabels",
        };

        let subdomain = match tile_id.x % 4 {
            0 => "a",
            1 => "b",
            2 => "c",
            _ => "d",
        };

        format!(
            "https://cartodb-basemaps-{}.global.ssl.fastly.net/{}/{}/{}/{}.png",
            subdomain, style_name, tile_id.zoom, tile_id.x, tile_id.y
        )
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            text: ATTRIBUTION_TEXT,
            url: ATTRIBUTION_URL,
            logo_light: None,
            logo_dark: None,
        }
    }
}

// Slippy-map tile coordinates (standard OSM / Web Mercator formulas), from
// the workspace's geodesy floor.

// The tile transforms and `MERCATOR_LAT_LIMIT_DEG` live in `squallar-geo`.
// The mercantile reference vectors in `tiles/tests.rs` pin their exact bits.

// ---------------------------------------------------------------------------
// Which source the base map is drawn from
// ---------------------------------------------------------------------------

/// The self-hosted PMTiles v3 basemap archive.
///
/// The generation is compiled in. `https://tiles.squallar.app/status/latest.json`
/// is the stable pointer at the same host and is what a client should resolve
/// once it can afford an extra round trip before the first tile; nothing does
/// yet, and a pointer nobody reads would be a claim rather than a mechanism.
#[cfg(feature = "basemap-vector")]
pub const BASEMAP_ARCHIVE_URL: &str = "https://tiles.squallar.app/basemap/omt-20260828.pmtiles";

/// An archive URL that replaces [`BASEMAP_ARCHIVE_URL`] when it is set.
///
/// Native only, and read once per source construction. It exists because the
/// published archive is the *only* other way to exercise this path, and a
/// change to the draw seam that can only be checked against 83 GB over a
/// network is a change that will stop being checked. `file://` is not a scheme
/// [`crate::basemap_archive::HttpRangeSource`] serves, so a local archive is
/// pointed at through a plain HTTP file server.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
pub const BASEMAP_ARCHIVE_URL_ENV: &str = "SQUALLAR_BASEMAP_ARCHIVE";

/// Which archive [`base_source`] opens.
///
/// A per-target **selection of a value**, which is what keeps [`base_source`]
/// itself one body rather than two: the override is a developer affordance for
/// a machine with a shell, and a browser has neither an environment nor a way
/// for the user to set one. `std::env::var` does compile on
/// wasm32-unknown-unknown and would simply always answer `NotPresent`, so this
/// split buys honesty rather than compilation -- a const documented "native
/// only" that the web arm still read would be prose that is not evidence.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
fn archive_url() -> String {
    std::env::var(BASEMAP_ARCHIVE_URL_ENV).unwrap_or_else(|_| BASEMAP_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`archive_url`]: the compiled-in archive, always.
#[cfg(all(feature = "basemap-vector", target_arch = "wasm32"))]
fn archive_url() -> String {
    BASEMAP_ARCHIVE_URL.to_owned()
}

/// The self-hosted terrain hillshade PMTiles archive.
///
/// Published as parts (`.part000`..): `HttpRangeSource` probes `<url>.part000`
/// at open and selects parts mode on its own, so this names the logical
/// archive and nothing here knows parts exist. The generation
/// (`4ca64469750e-20260829`) is compiled in, like the basemap's.
#[cfg(feature = "basemap-vector")]
pub const TERRAIN_ARCHIVE_URL: &str =
    "https://tiles.squallar.app/terrain/4ca64469750e-20260829/squallar-terrain-hillshade.pmtiles";

/// An archive URL that replaces [`TERRAIN_ARCHIVE_URL`] when it is set.
/// Native only, for the reason [`BASEMAP_ARCHIVE_URL_ENV`] is: the draw seam
/// must stay checkable against a local archive behind a plain HTTP server.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
pub const TERRAIN_ARCHIVE_URL_ENV: &str = "SQUALLAR_TERRAIN_ARCHIVE";

/// Which archive the terrain source opens — [`archive_url`]'s split, for
/// [`archive_url`]'s reason.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
fn terrain_archive_url() -> String {
    std::env::var(TERRAIN_ARCHIVE_URL_ENV).unwrap_or_else(|_| TERRAIN_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`terrain_archive_url`]: the compiled-in archive, always.
#[cfg(all(feature = "basemap-vector", target_arch = "wasm32"))]
fn terrain_archive_url() -> String {
    TERRAIN_ARCHIVE_URL.to_owned()
}

/// The credit the hillshade's elevation data requires.
///
/// The DEM is `COP-DEM_GLO-30 Public, 2021 release` — see
/// `tools/squallar-terrain/README.md` for the pinned provenance. Carried on
/// the source like every other credit; the map panel today paints only the
/// **base** source's credit line, so this reaches the glass only when that
/// gap is closed (recorded there, not here).
#[cfg(feature = "basemap-vector")]
pub const TERRAIN_ATTRIBUTION_TEXT: &str = "\u{a9} Copernicus DEM 2021";

/// The terrain hillshade tile source, or `None` when this build cannot build
/// one. A per-feature selection of a body, like [`base_source`]'s per-target
/// splits: on a build without the archive reader there is no terrain engine,
/// and the layer's toggle simply has nothing to draw.
#[cfg(feature = "basemap-vector")]
fn terrain_source(ctx: &egui::Context) -> Option<HttpsTiles> {
    let url = terrain_archive_url();
    let attribution = Attribution {
        text: TERRAIN_ATTRIBUTION_TEXT,
        url: "https://spacedata.copernicus.eu/collections/copernicus-digital-elevation-model",
        logo_light: None,
        logo_dark: None,
    };

    match HttpsTiles::from_terrain_archive_url(&url, attribution, ctx.to_owned()) {
        Ok(tiles) => Some(tiles),
        Err(error) => {
            log::error!("{url} is not a usable terrain archive URL: {error}");
            None
        }
    }
}

/// See the arm above. Without the archive reader there is nothing to read the
/// hillshade out of; unlike the basemap there is no raster fallback to fall
/// to, so the answer is simply no source.
#[cfg(not(feature = "basemap-vector"))]
fn terrain_source(_ctx: &egui::Context) -> Option<HttpsTiles> {
    None
}

/// The credit the vector basemap carries, in the generator's own words.
///
/// Planetiler prints this pair as the required credit for what it built, so it
/// is sourced rather than composed. The panel draws it because the archive
/// source reports it, not because anything here selects between two consts.
#[cfg(feature = "basemap-vector")]
pub const ARCHIVE_ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap contributors \u{a9} OpenMapTiles";

/// CartoDB's pre-rendered rasters, for `is_dark`.
///
/// The base source on a build without the vector archive, and the fallback on
/// one that has it but cannot reach it — see [`MapTileState::ensure_base_tiles`].
/// `credit` is a parameter because a fallback has to say that it *is* one.
fn cartodb_source(is_dark: bool, ctx: &egui::Context, credit: &'static str) -> HttpsTiles {
    let source = if is_dark {
        CartoDb::dark()
    } else {
        CartoDb::light()
    };
    HttpsTiles::with_attribution(
        source,
        ctx.to_owned(),
        Attribution {
            text: credit,
            url: ATTRIBUTION_URL,
            logo_light: None,
            logo_dark: None,
        },
    )
}

/// The base-map tile source for `is_dark`: CartoDB's pre-rendered rasters.
#[cfg(not(feature = "basemap-vector"))]
fn base_source(
    is_dark: bool,
    _disabled_source_layers: &std::collections::BTreeSet<String>,
    ctx: &egui::Context,
) -> HttpsTiles {
    // The rasters are pre-rendered; there is no style to filter, so the
    // disabled set has nothing to act on in this arm.
    cartodb_source(is_dark, ctx, ATTRIBUTION_TEXT)
}

/// The base-map tile source for `is_dark`: our own archive, rendered here.
///
/// Falls back to CartoDB if the archive URL will not parse, which is the only
/// failure visible at construction. The failures that are not — the host being
/// down, a stale `omt-YYYYMMDD` generation, the body not being PMTiles — happen
/// inside the IO task, which records them on `HttpsTiles::fault`;
/// [`MapTileState::ensure_base_tiles`] is where a frame acts on that.
#[cfg(feature = "basemap-vector")]
fn base_source(
    is_dark: bool,
    disabled_source_layers: &std::collections::BTreeSet<String>,
    ctx: &egui::Context,
) -> HttpsTiles {
    let url = archive_url();

    let attribution = Attribution {
        text: ARCHIVE_ATTRIBUTION_TEXT,
        url: ATTRIBUTION_URL,
        logo_light: None,
        logo_dark: None,
    };

    match HttpsTiles::from_archive_url(
        &url,
        crate::basemap_style::committed_filtered(is_dark, disabled_source_layers),
        attribution,
        ctx.to_owned(),
    ) {
        Ok(tiles) => tiles,
        Err(error) => {
            log::error!("{url} is not a usable basemap archive URL: {error}");
            cartodb_source(is_dark, ctx, FALLBACK_ATTRIBUTION_TEXT)
        }
    }
}

/// Shared map tile state across all panes.
pub struct MapTileState {
    /// The tile source for the theme currently on the glass.
    ///
    /// **One slot, not one per theme and not one per surface.**
    /// [`Self::adopt_theme`] releases the source the instant its theme stops
    /// being drawn, so a second theme slot could only ever hold `None`; and
    /// labels are no longer a source of their own, because the vector basemap
    /// draws them out of the same tile it draws the ground from.
    pub tiles: Option<HttpsTiles>,
    pub current_theme_is_dark: bool,

    /// The terrain hillshade source — the second slot, beside the basemap's.
    ///
    /// Built lazily by [`Self::ensure_terrain_tiles`], **only while some
    /// visible pane has the Terrain layer on**: a disabled layer must cost
    /// zero network, and a source that exists is a source whose IO task will
    /// be asked for tiles. Released by [`Self::release_terrain_tiles`] when
    /// the last pane switches the layer off (the accepted cost, exactly as
    /// for a theme flip on the base slot, is a re-download if it comes back),
    /// and by [`Self::clear`] with everything else.
    ///
    /// **A theme flip does NOT touch it.** [`Self::adopt_theme`] drops the
    /// base slot because the committed style baked into its tiles is the
    /// theme; the hillshade remap (black shadows, white highlights, alpha
    /// from relief — `terrain::remap_hillshade`) is theme-independent by
    /// design, so its pixels are right under both themes and rebuilding the
    /// source on a flip would re-download 24-64 MiB for nothing.
    pub terrain: Option<HttpsTiles>,

    /// Whether the vector archive has already been found unreachable this
    /// session, so [`Self::ensure_base_tiles`] must not build another one.
    ///
    /// Without the latch a theme flip -- which drops the source and rebuilds it
    /// -- would go back to the archive, fail again, and blank the map again.
    /// Cleared by [`Self::clear`], because a suspend/resume or a graphics reset
    /// is a plausible moment for a network to have come back.
    ///
    /// Always present, never `cfg`-gated: a build with no vector archive simply
    /// never sets it, and `HttpsTiles::fault` answers `None` for a raster
    /// source on every target.
    fell_back_to_raster: bool,

    /// Which source-layers were disabled in the style [`Self::tiles`] was
    /// built with — the comparison [`Self::ensure_base_tiles`] makes to
    /// notice a toggle flip. Meaningful only while the slot holds the vector
    /// source; the raster fallback has no style to filter.
    base_disabled_source_layers: std::collections::BTreeSet<String>,

    /// How many times a base source has been constructed — the probe the
    /// rebuild-on-toggle-flip pin reads, because two `HttpsTiles` cannot be
    /// compared for identity once moved.
    #[cfg(test)]
    pub(crate) base_builds: usize,

    /// [`Self::fell_back_to_raster`]'s twin for the terrain slot, latched for
    /// the same reason — without it a frame after a failed open would build
    /// the source again, fail again, and keep a dead retry loop warm. There
    /// is no raster fallback to fall to: a failed terrain archive means the
    /// layer draws nothing, said once at `error!`. Cleared by [`Self::clear`],
    /// because a resume is a plausible moment for the network to be back.
    terrain_failed: bool,
}

impl Default for MapTileState {
    fn default() -> Self {
        Self {
            tiles: None,
            terrain: None,
            current_theme_is_dark: true,
            fell_back_to_raster: false,
            base_disabled_source_layers: std::collections::BTreeSet::new(),
            #[cfg(test)]
            base_builds: 0,
            terrain_failed: false,
        }
    }
}

impl MapTileState {
    /// Adopt `is_dark` as the theme, releasing the one no longer drawn.
    ///
    /// **Nothing on the glass is blanked.** The source dropped belongs to the
    /// theme that is no longer drawn; the pane is repainting from the other
    /// theme's source this frame regardless. The accepted cost is a re-download
    /// if the user flips back.
    ///
    /// Without it a single flip took residency from one live source to two and
    /// held it for the session, because [`Self::ensure_base_tiles`] only ever
    /// fills and [`Self::clear`] runs only on suspend and graphics reset. At
    /// [`crate::tile_source::WASM_TILE_CACHE_ENTRIES`] one source's worst case
    /// is 24 MiB of raster tiles, or ~59 MiB now that wasm32 draws the vector
    /// basemap too, against the 288 MiB `squallar-device-profile` allows the
    /// whole application on wasm32.
    fn adopt_theme(&mut self, is_dark: bool) {
        if is_dark == self.current_theme_is_dark {
            return;
        }
        self.current_theme_is_dark = is_dark;
        self.tiles = None;
        // Deliberately NOT `self.terrain = None`: the hillshade remap is
        // theme-independent (see the `terrain` field's docs), so a flip
        // invalidates nothing of it.
    }

    /// Ensure the base-map tiles for the current theme are initialized, and
    /// replace a source that has reported itself unusable.
    ///
    /// **A blank map is not an acceptable resting state.** The archive is
    /// opened by the IO task, two range requests after the frame has already
    /// been handed a source, so a DNS failure, a 404, a host that is down, a
    /// stale `omt-YYYYMMDD` generation or a typo in
    /// [`BASEMAP_ARCHIVE_URL_ENV`] all surface here rather than at
    /// construction. `HttpsTiles::fault` is how they surface; this is what acts
    /// on them, once, by falling back to the raster source the build already
    /// knows how to reach.
    ///
    /// The fallback is not silent in either direction: it logs at `error!` with
    /// the transport's own words, and the credit the panel paints changes to
    /// [`FALLBACK_ATTRIBUTION_TEXT`], because the panel reads the credit off
    /// whichever source is actually being drawn.
    /// `disabled_source_layers` is the BasemapTiles layer's per-source-layer
    /// choice set; a source built with a different set is **rebuilt**, which
    /// re-downloads the visible tiles exactly as a theme flip does — the
    /// accepted cost of a toggle flip, cheap because the CDN serves the same
    /// ranges again. Caching parsed tile geometry so a flip could re-style
    /// without refetching is real future work, recorded here and not begun.
    pub fn ensure_base_tiles(
        &mut self,
        is_dark: bool,
        disabled_source_layers: &std::collections::BTreeSet<String>,
        ctx: &egui::Context,
    ) {
        self.adopt_theme(is_dark);

        if let Some(fault) = self.tiles.as_ref().and_then(HttpsTiles::fault) {
            log::error!(
                "the basemap archive is unusable, falling back to the raster \
                 basemap for this session: {fault}"
            );
            self.fell_back_to_raster = true;
            self.tiles = None;
        }

        // A toggle flip on the raster fallback rebuilds nothing: there is no
        // style in the rasters for the set to act on.
        if self.tiles.is_some()
            && !self.fell_back_to_raster
            && self.base_disabled_source_layers != *disabled_source_layers
        {
            self.tiles = None;
        }

        if self.tiles.is_none() {
            self.base_disabled_source_layers = disabled_source_layers.clone();
            #[cfg(test)]
            {
                self.base_builds += 1;
            }
            self.tiles = Some(if self.fell_back_to_raster {
                cartodb_source(is_dark, ctx, FALLBACK_CREDIT)
            } else {
                base_source(is_dark, disabled_source_layers, ctx)
            });
        }
    }

    /// Drop the base source: the last visible pane switched the BasemapTiles
    /// layer off. Symmetric with [`Self::release_terrain_tiles`] — a disabled
    /// layer costs zero network, and the accepted cost is a re-download if it
    /// comes back. The raster-fallback latch survives, exactly as the terrain
    /// failure latch does: a release is not a network recovery.
    pub fn release_base_tiles(&mut self) {
        self.tiles = None;
    }

    /// Temporarily take the base tiles out of self for per-pane rendering.
    pub fn take_base_tiles(&mut self) -> Option<HttpsTiles> {
        self.tiles.take()
    }

    /// Restore the base tiles after per-pane rendering.
    pub fn restore_base_tiles(&mut self, tiles: Option<HttpsTiles>) {
        self.tiles = tiles;
    }

    /// Ensure the terrain hillshade source exists, and replace one that has
    /// reported itself unusable. Call **only while some visible pane draws
    /// the Terrain layer** — the lazy half of the slot's zero-cost-when-off
    /// contract; [`Self::release_terrain_tiles`] is the other half.
    ///
    /// The fault handling is [`Self::ensure_base_tiles`]'s without the
    /// fallback arm: there is nothing to fall back to, so a dead archive is
    /// logged once and the layer draws nothing until [`Self::clear`] resets
    /// the latch.
    pub fn ensure_terrain_tiles(&mut self, ctx: &egui::Context) {
        if let Some(fault) = self.terrain.as_ref().and_then(HttpsTiles::fault) {
            log::error!(
                "the terrain archive is unusable, so the Terrain layer draws                  nothing this session: {fault}"
            );
            self.terrain_failed = true;
            self.terrain = None;
        }

        if self.terrain.is_none() && !self.terrain_failed {
            self.terrain = terrain_source(ctx);
            // A build with no archive reader, or a URL that will not parse,
            // yields no source and never will this session: latch, so the
            // per-frame cost of an unbuildable source is one bool read.
            self.terrain_failed = self.terrain.is_none();
        }
    }

    /// Drop the terrain source: the last pane showing the layer switched it
    /// off. Symmetric with [`Self::adopt_theme`]'s handling of the base slot:
    /// the accepted cost is a re-download if the user switches it back on.
    pub fn release_terrain_tiles(&mut self) {
        self.terrain = None;
    }

    /// Temporarily take the terrain tiles out of self for per-pane rendering.
    pub fn take_terrain_tiles(&mut self) -> Option<HttpsTiles> {
        self.terrain.take()
    }

    /// Restore the terrain tiles after per-pane rendering.
    pub fn restore_terrain_tiles(&mut self, tiles: Option<HttpsTiles>) {
        self.terrain = tiles;
    }

    /// Clear all tile state (called on suspend/graphics reset).
    ///
    /// The fallback latch goes with it: a resume is a plausible moment for a
    /// network that was down to be up, and retrying the archive costs one
    /// failed open if it is not.
    pub fn clear(&mut self) {
        self.fell_back_to_raster = false;
        self.tiles = None;
        self.terrain = None;
        self.terrain_failed = false;
        self.current_theme_is_dark = !self.current_theme_is_dark;
    }
}

/// The side of one slippy tile in **points**, at zoom bias 0 and a whole zoom.
pub const TILE_SIDE_POINTS: f32 = 256.0;

/// The smallest a tile is ever drawn, as a fraction of [`TILE_SIDE_POINTS`].
///
/// walkers paints a tile `TILE_SIDE_POINTS · 2^(zoom − tile_zoom)` points
/// across, and `ui_map_overlays::draw_tile_layer` picks
/// `tile_zoom = zoom.round() + bias`, so the exponent is
/// `zoom − round(zoom) − bias`. `zoom − round(zoom)` runs over `[−0.5, 0.5)`
/// and **attains** `−0.5`, because Rust rounds a half away from zero: a tile is
/// at its smallest exactly at the half step, `2^−0.5` of a side — 181.02 points
/// at bias 0 — and more of them fit the same window.
pub const MIN_TILE_SCALE: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// How many tiles a rect keeps resident, at a zoom bias, across `layers` raster
/// layers — **worst case over the whole zoom range**.
///
/// This is the sizing answer, and the only one a cache may be built on: the LRU
/// has to hold what the viewport asks for at every zoom the user can be at, not
/// only at the whole ones. Between two whole steps a tile shrinks to
/// [`MIN_TILE_SCALE`] of its side and more of them fit, so this is the larger
/// figure — 84 against 54 for a 1920x1080-point canvas at bias 0, one layer.
///
/// [`tiles_resident_at_whole_zoom`] answers the narrower question, and is only
/// ever the right one for a claim that is *about* a whole zoom.
pub fn tiles_resident_for(rect: egui::Rect, zoom_bias: u8, layers: usize) -> usize {
    tiles_resident_at_tile_scale(rect, zoom_bias, layers, MIN_TILE_SCALE)
}

/// How many tiles a rect keeps resident at a **whole** zoom, where a tile is
/// drawn exactly [`TILE_SIDE_POINTS`] across. Never larger than
/// [`tiles_resident_for`], and not a cache size.
pub fn tiles_resident_at_whole_zoom(rect: egui::Rect, zoom_bias: u8, layers: usize) -> usize {
    tiles_resident_at_tile_scale(rect, zoom_bias, layers, 1.0)
}

/// The count for a tile drawn `scale · TILE_SIDE_POINTS / 2^zoom_bias` points
/// across.
///
/// `ceil(w / side) + 1`, not `ceil` alone: the grid's phase inside the viewport
/// is free, so a window `w / side` tiles wide reaches one column further than
/// its own width. That is tight rather than generous — as the phase approaches
/// a tile boundary a window of `w / side = 7.5` covers nine columns, and
/// `ceil(7.5) + 1` is nine. `tiles/tests.rs` holds both 54 and 84 against the
/// sweep that measures `tile_span` directly rather than deriving it.
fn tiles_resident_at_tile_scale(
    rect: egui::Rect,
    zoom_bias: u8,
    layers: usize,
    scale: f32,
) -> usize {
    let side = TILE_SIDE_POINTS * scale / 2f32.powi(i32::from(zoom_bias));
    if side <= 0.0 || !rect.width().is_finite() || !rect.height().is_finite() {
        return 0;
    }
    let across = (rect.width().max(0.0) / side).ceil() as usize + 1;
    let down = (rect.height().max(0.0) / side).ceil() as usize + 1;
    across.saturating_mul(down).saturating_mul(layers)
}

/// The tile indices one viewport covers at one tile zoom, both ends inclusive.
///
/// `north` is the *smaller* row index: tile `y` grows southward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileSpan {
    pub west: u32,
    pub east: u32,
    pub north: u32,
    pub south: u32,
}

impl TileSpan {
    /// How many tiles the span names.
    pub fn tiles(self) -> usize {
        let across = (self.east.saturating_sub(self.west) as usize) + 1;
        let down = (self.south.saturating_sub(self.north) as usize) + 1;
        across.saturating_mul(down)
    }
}

/// The tiles that cover `rect` on the glass at `tile_zoom`.
///
/// **Neither end is widened, and widening one would be a bug.**
/// [`lon_to_tile_x`] and [`lat_to_tile_y`] *floor*: the index each returns is
/// the tile the coordinate falls inside, so the inclusive span already carries
/// the two part-covered edge tiles. A `+ 1` on the far end appends a column
/// wholly east of `rect` and a row wholly south of it — at 1920x1080 that is
/// 10x7 tiles where 9x6 are seen — and each of the extra 16 costs a
/// `Tiles::at` call, an HTTP request through `request_once`, an LRU probe, a
/// `TextureHandle` clone and drop (a write lock on the texture manager each),
/// and a fully clipped `Painter::image`.
///
/// Both transforms also clamp rather than wrap, so a viewport reaching past the
/// antimeridian or the Mercator limit gets the edge tile and not the far side.
pub fn tile_span(projector: &walkers::Projector, rect: egui::Rect, tile_zoom: u8) -> TileSpan {
    let nw = projector.unproject(egui::vec2(rect.left(), rect.top()));
    let se = projector.unproject(egui::vec2(rect.right(), rect.bottom()));

    // walkers Position: x = longitude, y = latitude.
    let (min_lon, max_lon) = (nw.x().min(se.x()), nw.x().max(se.x()));
    let (min_lat, max_lat) = (nw.y().min(se.y()), nw.y().max(se.y()));

    TileSpan {
        west: lon_to_tile_x(min_lon, tile_zoom),
        east: lon_to_tile_x(max_lon, tile_zoom),
        north: lat_to_tile_y(max_lat, tile_zoom),
        south: lat_to_tile_y(min_lat, tile_zoom),
    }
}

#[path = "tiles/tests.rs"]
#[cfg(test)]
mod tests;
