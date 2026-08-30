use crate::tile_source::HttpsTiles;
use squallar_geo::{lat_to_tile_y, lon_to_tile_x};
use walkers::sources::Attribution;

/// Where the basemap credit links. ODbL wants the notice reachable, not just
/// shown.
pub const ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";

/// The credit the panel paints for a session whose archive has been found
/// unreachable — the on-screen half of reporting the fault, drawn in the
/// corner the credit already occupies rather than as a new surface.
///
/// There is no raster provider to fall back to any more (CartoDB was deleted
/// with the flip to the archive basemap), so an unreachable archive means the
/// ground layer draws **nothing**, and this line is what keeps that state from
/// being a silent blank: [`MapTileState::base_archive_is_unreachable`] is how
/// the panel knows to paint it. It names what is wrong, not what the reader
/// should do about it; the detail that lets them fix it -- the URL and the
/// transport error -- is one `log::error!` away and would not fit here.
///
/// ASCII, deliberately: `ui_glyphs` gates every non-ASCII character UI text
/// carries against the bundled fonts, and this string owes no copyright sign
/// because no provider's pixels are on the glass while it shows.
pub const UNREACHABLE_ATTRIBUTION_TEXT: &str = "basemap archive unreachable";

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
pub const BASEMAP_ARCHIVE_URL: &str = "https://tiles.squallar.app/basemap/omt-20260828.pmtiles";

/// An archive URL that replaces [`BASEMAP_ARCHIVE_URL`] when it is set.
///
/// Native only, and read once per source construction. It exists because the
/// published archive is the *only* other way to exercise this path, and a
/// change to the draw seam that can only be checked against 83 GB over a
/// network is a change that will stop being checked. `file://` is not a scheme
/// [`crate::basemap_archive::HttpRangeSource`] serves, so a local archive is
/// pointed at through a plain HTTP file server.
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn archive_url() -> String {
    std::env::var(BASEMAP_ARCHIVE_URL_ENV).unwrap_or_else(|_| BASEMAP_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`archive_url`]: the compiled-in archive, always.
#[cfg(target_arch = "wasm32")]
pub(crate) fn archive_url() -> String {
    BASEMAP_ARCHIVE_URL.to_owned()
}

/// The self-hosted terrain hillshade PMTiles archive.
///
/// Published as parts (`.part000`..): `HttpRangeSource` probes `<url>.part000`
/// at open and selects parts mode on its own, so this names the logical
/// archive and nothing here knows parts exist. The generation
/// (`4ca64469750e-20260829`) is compiled in, like the basemap's.
pub const TERRAIN_ARCHIVE_URL: &str =
    "https://tiles.squallar.app/terrain/4ca64469750e-20260829/squallar-terrain-hillshade.pmtiles";

/// An archive URL that replaces [`TERRAIN_ARCHIVE_URL`] when it is set.
/// Native only, for the reason [`BASEMAP_ARCHIVE_URL_ENV`] is: the draw seam
/// must stay checkable against a local archive behind a plain HTTP server.
#[cfg(not(target_arch = "wasm32"))]
pub const TERRAIN_ARCHIVE_URL_ENV: &str = "SQUALLAR_TERRAIN_ARCHIVE";

/// Which archive the terrain source opens — [`archive_url`]'s split, for
/// [`archive_url`]'s reason.
#[cfg(not(target_arch = "wasm32"))]
fn terrain_archive_url() -> String {
    std::env::var(TERRAIN_ARCHIVE_URL_ENV).unwrap_or_else(|_| TERRAIN_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`terrain_archive_url`]: the compiled-in archive, always.
#[cfg(target_arch = "wasm32")]
fn terrain_archive_url() -> String {
    TERRAIN_ARCHIVE_URL.to_owned()
}

/// Where the persistent archive block cache lives, installed once by the
/// application at construction from `PlatformBridge::basemap_cache_dir` —
/// the same platform fact `zone_cache_dir` is, reaching its consumer the way
/// that one does: decided by the platform, handed over once, never poked
/// through a UI setter.
///
/// A process-wide `OnceLock` rather than a `Gui` field because it is process
/// configuration, not UI state: [`base_source`] and the terrain source are
/// free functions rebuilding sources across theme flips and layer toggles,
/// and every rebuild must see the same answer. Ungated and target-shared: a
/// build without the archive reader, or a platform with no filesystem,
/// simply never reads it.
static BASEMAP_CACHE_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Install the archive block cache directory. First installation wins;
/// installing nothing leaves every archive read uncached, which is the
/// degraded mode the cache is documented to fall back to.
pub fn install_basemap_cache_dir(dir: std::path::PathBuf) {
    let _ = BASEMAP_CACHE_DIR.set(dir);
}

/// The block cache configuration for the archive at `url`, or `None` when no
/// cache directory was installed.
///
/// **The one place the GC's live-generation set is derived**, and it is
/// derived from *both* archive URLs no matter which source is being built:
/// basemap and terrain have different generations both alive at once, the
/// terrain source is built lazily, and a live set that only knew the opening
/// source would cost the other its cache every launch.
#[cfg(not(target_arch = "wasm32"))]
fn archive_block_cache(url: &str) -> Option<crate::basemap_archive::block_cache::BlockCacheConfig> {
    use crate::basemap_archive::block_cache;

    let root = BASEMAP_CACHE_DIR.get()?.clone();
    Some(block_cache::BlockCacheConfig {
        root,
        generation: block_cache::generation_for_url(url),
        live_generations: vec![
            block_cache::generation_for_url(&archive_url()),
            block_cache::generation_for_url(&terrain_archive_url()),
        ],
        cap_bytes: block_cache::BLOCK_CACHE_BYTES,
    })
}

/// The wasm32 arm of [`archive_block_cache`]: no filesystem, no cache — the
/// same selection-of-a-body split as [`archive_url`].
#[cfg(target_arch = "wasm32")]
fn archive_block_cache(
    _url: &str,
) -> Option<crate::basemap_archive::block_cache::BlockCacheConfig> {
    None
}

/// The store the downloaded areas on this device are read back through, or
/// `None` when this platform has nowhere to keep them.
///
/// The same selection-of-a-body split as [`archive_block_cache`] and
/// [`archive_url`]: native reads plain files out of the basemap directory the
/// bridge chose; the web store is the service worker's route, and until that
/// route exists there is nothing to read back. Neither arm branches inside a
/// body — each *is* the body its target selects.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn offline_store(
    basemap_dir: Option<&std::path::Path>,
) -> Option<crate::basemap_download::PlatformSegmentStore> {
    Some(crate::basemap_download::FsSegmentStore::new(
        basemap_dir?.to_path_buf(),
    ))
}

/// The wasm32 arm of [`offline_store`]. The service-worker routes the store
/// would read through are not built yet, and a store pointed at a route that
/// does not answer would spend a request per launch to be told so.
#[cfg(target_arch = "wasm32")]
pub(crate) fn offline_store(
    _basemap_dir: Option<&std::path::Path>,
) -> Option<crate::basemap_download::PlatformSegmentStore> {
    None
}

/// A range source over the base map archive, for the two readers that are not
/// the render path: the size measurement and the download engine.
///
/// **The same URL and the same client the map itself reads through**, so a
/// figure is quoted against the archive the download will actually fetch from.
/// Deliberately *not* `HttpsTiles`: its request channel is bounded at 6 and
/// its LRU is the live map's working set, so measuring or downloading through
/// it would evict what the user is looking at.
///
/// # Errors
///
/// [`crate::basemap_archive::RangeError`] if the archive URL will not parse —
/// the same and only construction-time failure `from_archive_url` has.
pub(crate) fn archive_range_source()
-> Result<crate::basemap_archive::HttpRangeSource, crate::basemap_archive::RangeError> {
    crate::basemap_archive::HttpRangeSource::new(
        crate::basemap_archive::archive_client(),
        &archive_url(),
    )
}

/// The credit the hillshade's elevation data requires.
///
/// The DEM is `COP-DEM_GLO-30 Public, 2021 release` — see
/// `tools/squallar-terrain/README.md` for the pinned provenance. Carried on
/// the source like every other credit, and read off it by the panel's one
/// notice: while the terrain slot holds a source, `ui_map` appends this line
/// to the base credit — one notice per panel still — and drops it the frame
/// the slot is released, because an idle credit is clutter that dilutes the
/// required ones.
pub const TERRAIN_ATTRIBUTION_TEXT: &str = "\u{a9} Copernicus DEM 2021";

/// The terrain hillshade tile source, or `None` when the archive URL will not
/// parse — the only failure visible at construction; the rest surface on
/// `HttpsTiles::fault` inside the IO task.
fn terrain_source(ctx: &egui::Context) -> Option<HttpsTiles> {
    let url = terrain_archive_url();
    let attribution = Attribution {
        text: TERRAIN_ATTRIBUTION_TEXT,
        url: "https://spacedata.copernicus.eu/collections/copernicus-digital-elevation-model",
        logo_light: None,
        logo_dark: None,
    };

    match HttpsTiles::from_terrain_archive_url(
        &url,
        attribution,
        ctx.to_owned(),
        archive_block_cache(&url),
    ) {
        Ok(tiles) => Some(tiles),
        Err(error) => {
            log::error!("{url} is not a usable terrain archive URL: {error}");
            None
        }
    }
}

/// The credit the vector basemap carries, in the generator's own words.
///
/// Planetiler prints this pair as the required credit for what it built, so it
/// is sourced rather than composed. The panel draws it because the archive
/// source reports it, not because anything here selects between two consts.
pub const ARCHIVE_ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap contributors \u{a9} OpenMapTiles";

/// The base-map tile source for `is_dark`: our own archive, rendered here, or
/// `None` if the archive URL will not parse — the only failure visible at
/// construction. The failures that are not — the host being down, a stale
/// `omt-YYYYMMDD` generation, the body not being PMTiles — happen inside the
/// IO task, which records them on `HttpsTiles::fault`;
/// [`MapTileState::ensure_base_tiles`] is where a frame acts on that. There is
/// no raster provider to fall back to.
fn base_source(
    is_dark: bool,
    disabled_source_layers: &std::collections::BTreeSet<String>,
    ctx: &egui::Context,
    basemap_dir: Option<&std::path::Path>,
) -> Option<HttpsTiles> {
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
        archive_block_cache(&url),
        offline_store(basemap_dir),
    ) {
        Ok(tiles) => Some(tiles),
        Err(error) => {
            log::error!("{url} is not a usable basemap archive URL: {error}");
            None
        }
    }
}

/// Shared map tile state across all panes.
pub struct MapTileState {
    /// The tile source, styled for the theme currently on the glass.
    ///
    /// **One slot, not one per theme and not one per surface.** A theme flip
    /// does not replace the source: [`Self::ensure_base_tiles`] re-styles it
    /// in place ([`HttpsTiles::set_style`]), so its parsed-geometry cache
    /// survives the flip and the new theme is derived from it without a
    /// fetch. A second theme slot would hold nothing but a duplicate of this
    /// one's caches; and labels are no longer a source of their own, because
    /// the vector basemap draws them out of the same tile it draws the ground
    /// from.
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
    /// **A theme flip does NOT touch it.** The base slot is re-styled on a
    /// flip because the committed style its tiles are styled with is the
    /// theme; the hillshade remap (black shadows, white highlights, alpha
    /// from relief — `terrain::remap_hillshade`) is theme-independent by
    /// design, so its pixels are right under both themes and there is nothing
    /// for a flip to re-style or re-download.
    pub terrain: Option<HttpsTiles>,

    /// Whether the basemap archive has already been found unreachable this
    /// session, so [`Self::ensure_base_tiles`] must not build another source.
    ///
    /// Without the latch every frame after a failed open would build the
    /// source again, fail again, and keep a dead retry loop warm. There is no
    /// raster provider to fall back to since the CartoDB path was deleted:
    /// while latched the ground layer draws **nothing**, said once at
    /// `error!`, and the panel paints [`UNREACHABLE_ATTRIBUTION_TEXT`] in the
    /// credit corner (read through [`Self::base_archive_is_unreachable`]) so
    /// the degraded state is on the glass rather than only in a log. Cleared
    /// by [`Self::clear`], because a suspend/resume or a graphics reset is a
    /// plausible moment for a network to have come back.
    base_unreachable: bool,

    /// Never build a tile source: the test harnesses' switch, set through
    /// [`Self::go_offline_for_tests`]. Distinct from
    /// [`Self::base_unreachable`], which is a *found* degraded state and
    /// changes the painted credit; offline is the normal state's geometry
    /// with the network left out.
    offline: bool,

    /// Which source-layers were disabled in the style [`Self::tiles`] is
    /// currently styled with — half of the comparison
    /// [`Self::ensure_base_tiles`] makes to notice a toggle flip (the other
    /// half is [`Self::current_theme_is_dark`]). Meaningful only while the
    /// slot holds the vector source; the raster fallback has no style to
    /// filter.
    base_disabled_source_layers: std::collections::BTreeSet<String>,

    /// How many times a base source has been constructed — the probe the
    /// restyle-not-rebuild pin reads, because two `HttpsTiles` cannot be
    /// compared for identity once moved.
    #[cfg(test)]
    pub(crate) base_builds: usize,

    /// How many times the live base source has been re-styled in place — the
    /// other half of that pin: a theme flip or a toggle flip must move this,
    /// not [`Self::base_builds`].
    #[cfg(test)]
    pub(crate) base_restyles: usize,

    /// [`Self::base_unreachable`]'s twin for the terrain slot, latched for
    /// the same reason. A failed terrain archive means the layer draws
    /// nothing, said once at `error!`. Cleared by [`Self::clear`], because a
    /// resume is a plausible moment for the network to be back.
    terrain_failed: bool,

    /// Where this device keeps downloaded offline areas, or `None` when the
    /// platform has nowhere for them.
    ///
    /// Handed over once, at `Gui` construction (`Gui::with_basemap_dir`),
    /// never through a per-frame argument: the path is constant for the
    /// process, and `ensure_base_tiles` is called from a frame. A source
    /// built while this is `None` reads everything from the network, which is
    /// the state every test that does not opt in is in.
    basemap_dir: Option<std::path::PathBuf>,
}

impl Default for MapTileState {
    fn default() -> Self {
        Self {
            tiles: None,
            terrain: None,
            current_theme_is_dark: true,
            base_unreachable: false,
            offline: false,
            base_disabled_source_layers: std::collections::BTreeSet::new(),
            #[cfg(test)]
            base_builds: 0,
            #[cfg(test)]
            base_restyles: 0,
            terrain_failed: false,
            basemap_dir: None,
        }
    }
}

impl MapTileState {
    /// Point the base slot at this device's downloaded basemap areas.
    ///
    /// Called once, from `Gui::with_basemap_dir`, before any frame. Not a
    /// per-frame argument and not a re-buildable setting: a source already
    /// built keeps the store it was built with, and the next rebuild — a
    /// theme flip is not one — picks this up.
    pub(crate) fn set_basemap_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.basemap_dir = dir;
    }

    /// Ensure the base-map tiles for the current theme and detail set are
    /// initialized, and retire a source that has reported itself unusable.
    ///
    /// The archive is opened by the IO task, two range requests after the
    /// frame has already been handed a source, so a DNS failure, a 404, a host
    /// that is down, a stale `omt-YYYYMMDD` generation or a typo in
    /// [`BASEMAP_ARCHIVE_URL_ENV`] all surface here rather than at
    /// construction. `HttpsTiles::fault` is how they surface; this is what
    /// acts on them, once, by latching [`Self::base_unreachable`]. There is no
    /// raster provider left to fall back to, so the honest degraded state is a
    /// ground layer that draws nothing -- not silent in either direction: it
    /// logs at `error!` with the transport's own words, and the panel paints
    /// [`UNREACHABLE_ATTRIBUTION_TEXT`] in the credit corner for as long as
    /// the latch holds.
    ///
    /// **A style change — a theme flip, or a flip of the BasemapTiles layer's
    /// per-source-layer detail set — re-styles the live source in place**
    /// ([`HttpsTiles::set_style`]): the source's parsed-geometry cache serves
    /// the new style with zero fetches and zero re-parses, the tiles on the
    /// glass keep drawing in the outgoing style until each restyled one
    /// arrives, and nothing blanks. This replaces the v1 rebuild, which
    /// dropped the source and re-downloaded every visible tile — the cost the
    /// old note here recorded as accepted with the parsed cache as future
    /// work. The terrain slot is untouched by any of it: the hillshade remap
    /// is theme-independent by design (see the `terrain` field's docs).
    pub fn ensure_base_tiles(
        &mut self,
        is_dark: bool,
        disabled_source_layers: &std::collections::BTreeSet<String>,
        ctx: &egui::Context,
    ) {
        if let Some(fault) = self.tiles.as_ref().and_then(HttpsTiles::fault) {
            log::error!(
                "the basemap archive is unusable, so the basemap draws nothing \
                 this session: {fault}"
            );
            self.base_unreachable = true;
            self.tiles = None;
        }

        let style_changed = self.current_theme_is_dark != is_dark
            || self.base_disabled_source_layers != *disabled_source_layers;
        if style_changed {
            self.current_theme_is_dark = is_dark;
            self.base_disabled_source_layers = disabled_source_layers.clone();
            if let Some(tiles) = self.tiles.as_mut() {
                tiles.set_style(crate::basemap_style::committed_filtered(
                    is_dark,
                    disabled_source_layers,
                ));
                #[cfg(test)]
                {
                    self.base_restyles += 1;
                }
            }
        }

        if self.tiles.is_none() && !self.base_unreachable {
            self.current_theme_is_dark = is_dark;
            self.base_disabled_source_layers = disabled_source_layers.clone();
            #[cfg(test)]
            {
                self.base_builds += 1;
            }
            self.tiles = if self.offline {
                Some(HttpsTiles::inert(
                    Attribution {
                        text: ARCHIVE_ATTRIBUTION_TEXT,
                        url: ATTRIBUTION_URL,
                        logo_light: None,
                        logo_dark: None,
                    },
                    ctx.to_owned(),
                ))
            } else {
                base_source(
                    is_dark,
                    disabled_source_layers,
                    ctx,
                    self.basemap_dir.as_deref(),
                )
            };
            // A URL that will not parse yields no source and never will this
            // session: latch, exactly as the terrain slot does, so the
            // per-frame cost of an unbuildable source is one bool read.
            self.base_unreachable = self.tiles.is_none();
        }
    }

    /// Whether the archive has been found unreachable this session, so the
    /// base slot is deliberately empty. The panel reads this to paint
    /// [`UNREACHABLE_ATTRIBUTION_TEXT`] instead of a provider credit: the
    /// degraded state must be on the glass, not only in the log.
    pub fn base_archive_is_unreachable(&self) -> bool {
        self.base_unreachable
    }

    /// Put the base slot in the state a dead archive leaves it in: latched
    /// unreachable, source dropped. For the credit-composition tests, which
    /// need the latch without a real transport failure to raise it; what the
    /// tests using it prove is therefore the *composition* given the latch,
    /// not the latch's own raising.
    #[cfg(test)]
    pub(crate) fn latch_base_unreachable_for_test(&mut self) {
        self.base_unreachable = true;
        self.tiles = None;
    }

    /// Whether this instance is building only inert sources.
    ///
    /// Read by the frame path's *other* archive readers — the offline-area
    /// size measurement and the download engine — so that one switch covers
    /// every route to the production archive rather than only the tile slots.
    /// Always false outside a test harness.
    pub(crate) fn is_offline(&self) -> bool {
        self.offline
    }

    /// Build only inert tile sources for the rest of this instance's life.
    ///
    /// **For test harnesses that drive `Gui::ui`, and nothing else.** A live
    /// source is an IO thread making range requests against the production
    /// archive, and what it delivers races the frames of whatever built it,
    /// with two arms: tiles that arrive paint label `TextShape`s at
    /// arbitrary map positions (measured 2026-08-29: a harness frame held
    /// open for 400 ms painted 80+ live city labels, several straddling
    /// pill-row rects), and a transport fault latches
    /// [`UNREACHABLE_ATTRIBUTION_TEXT`] over the provider credit. Either way
    /// a unit test's glass changed with how much wall-clock time the test
    /// took — the under-load flakes this switch removes.
    ///
    /// The slots still fill, with [`HttpsTiles::inert`]: every ensure/release
    /// path and the credit composition run exactly as shipped, against a
    /// source whose header never arrives and whose fault can never be set —
    /// the fast-test glass, on every run. Unreachable is NOT latched; a test
    /// that wants the degraded credit raises the latch itself through
    /// [`Self::latch_base_unreachable_for_test`].
    pub fn go_offline_for_tests(&mut self) {
        self.offline = true;
        self.tiles = None;
        self.terrain = None;
    }

    /// Drop the base source: the last visible pane switched the BasemapTiles
    /// layer off. Symmetric with [`Self::release_terrain_tiles`] — a disabled
    /// layer costs zero network, and the accepted cost is a re-download if it
    /// comes back. The unreachable latch survives, exactly as the terrain
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
            self.terrain = if self.offline {
                Some(HttpsTiles::inert(
                    Attribution {
                        text: TERRAIN_ATTRIBUTION_TEXT,
                        url: "https://spacedata.copernicus.eu/collections/copernicus-digital-elevation-model",
                        logo_light: None,
                        logo_dark: None,
                    },
                    ctx.to_owned(),
                ))
            } else {
                terrain_source(ctx)
            };
            // A build with no archive reader, or a URL that will not parse,
            // yields no source and never will this session: latch, so the
            // per-frame cost of an unbuildable source is one bool read.
            self.terrain_failed = self.terrain.is_none();
        }
    }

    /// Drop the terrain source: the last pane showing the layer switched it
    /// off. Symmetric with [`Self::release_base_tiles`]: the accepted cost is
    /// a re-download if the user switches it back on.
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
    /// Both unreachable latches go with it: a resume is a plausible moment for
    /// a network that was down to be up, and retrying the archive costs one
    /// failed open if it is not.
    pub fn clear(&mut self) {
        self.base_unreachable = false;
        self.tiles = None;
        self.terrain = None;
        self.terrain_failed = false;
        // `current_theme_is_dark` and the disabled set stay: they describe the
        // style last asked for, and the next `ensure_base_tiles` builds fresh
        // on the empty slot whatever they say.
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
