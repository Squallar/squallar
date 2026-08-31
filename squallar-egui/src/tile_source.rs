//! squallar's own basemap tile source.
//!
//! Every other HTTPS call squallar makes goes through [`squallar_radar::tls::client`]
//! (`rustls-platform-verifier` + *ring*), so the OS decides trust at handshake
//! time and nothing in the binary carries an expiry date. Basemap tiles were the
//! one path that bypassed it: [`walkers::HttpTiles`] builds its own `reqwest`
//! client internally (`walkers-0.56.0/src/io/http.rs`, `bare_client`) with
//! `features = ["rustls-tls"]`, which in reqwest 0.12 resolves to `webpki-roots`
//! — the Mozilla root store compiled into the binary.
//!
//! [`HttpsTiles`] replaces `HttpTiles`, implementing [`walkers::Tiles`], so the
//! rest of the walkers integration is untouched. It reproduces async fetching,
//! bounded concurrency at [`MAX_PARALLEL_DOWNLOADS`], decode and upload via
//! [`walkers::Tile::new`], a bounded LRU of [`TILE_CACHE_ENTRIES`] tiles,
//! in-flight de-duplication, lower-zoom interpolation, `max_zoom` clamping,
//! grid-bounds checking, repaint-on-arrival and attribution.
//!
//! Deliberate differences: our client with a [`REQUEST_TIMEOUT`] (walkers sets
//! none); squallar's `User-Agent`; a closed request channel logs instead of
//! panicking (walkers' `TilesIo::make_sure_is_fetched` calls `panic!`); no HTTP
//! disk cache, which neither has.
//!
//! The IO runtime splits per target as walkers splits it: a thread with a
//! current-thread tokio runtime on native, `spawn_local` on wasm. On wasm the
//! IO task shares the page thread, so a completed fetch hands over **compressed
//! bytes** — or, for a restyle served out of the parsed cache, the parse
//! itself — and the decode/styling + upload runs in [`HttpsTiles::pump`] under
//! [`WASM_TILE_DECODES_PER_PUMP`] — see [`FetchPayload`].
//!
//! The pump is called **once per layer**, by `ui_map_overlays::draw_tile_layer`
//! before its grid loop, and never from [`Tiles::at`] — see
//! [`HttpsTiles::pump`] for why, and for the one thing that would break.
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use egui::Context;
use futures::channel::mpsc::{Receiver, Sender, TryRecvError, channel};
use futures::stream::FuturesUnordered;
use futures::{SinkExt, StreamExt, future::Either, future::select};
use lru::LruCache;
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

/// Maximum number of tile downloads in flight at once — walkers' default.
/// Tile providers throttle or ban clients that exceed their limits, so this is
/// a term of use rather than a performance dial.
pub const MAX_PARALLEL_DOWNLOADS: usize = 6;

/// Consecutive tile reads that must fail, with none answering in between,
/// before an archive source reports that it is not drawing the map.
///
/// [`MAX_PARALLEL_DOWNLOADS`], because that is how many reads
/// [`serve_archive_continuously`] can have outstanding at once: a run this
/// long means a whole cohort of concurrently-issued reads failed with not one
/// of them answering, which is the smallest observation this loop can make
/// that is about the *source* rather than about the particular tiles it was
/// asked for. One dead tile among live neighbours ends the run at 1, because
/// the cohort completes in arbitrary order and any answer resets it.
///
/// A read *answers* with a body or with the archive's authoritative "no tile
/// at this coordinate", so a viewport of open ocean is not a fault.
const SUSTAINED_READ_FAILURES: usize = MAX_PARALLEL_DOWNLOADS;

/// Said once per run, when [`ReadFailureRun::answered`] ends one. Named
/// because both arms of the serve loop's success path say it.
const RECOVERED: &str = "the archive is answering tile reads again, so it draws and is credited \
                         again";

/// Tiles retained in one source's in-memory cache before LRU eviction starts.
///
/// **Two kinds of entry, and they do not cost the same.** This bound is a
/// *count*, so what it is worth in bytes depends on what the arm can hold:
///
/// | entry            | cost               | which arms hold it |
/// |------------------|-------------------:|--------------------|
/// | raster texture   | 256 KiB (256x256 RGBA) | all |
/// | vector tile      | **646,264 B** worst case | all |
///
/// The vector figure is measured, not derived: the committed Monaco fixture's
/// z14 city-core tile (185,182 MVT bytes) renders to 738 shapes — 708 paths of
/// 4,390 points, two coalesced meshes of 18,018 vertices and 40,812 indices,
/// and 27 labels — held as host allocations, not GPU textures. Over all 246
/// tiles the fixture holds the *mean* is 13,461 bytes and the median 456, so
/// this is a tail figure and the tail is what a cache must be sized for: a
/// viewport over a city is a viewport of city-core tiles.
///
/// **wasm32 holds vector entries too**, since `feat(web): the vector basemap
/// draws on wasm32` ungated the archive reader; the arm used to be derived
/// against the raster cost alone because there was nothing else it could hold.
/// The 646,264 B figure carries to it unmeasured but not unfounded: `mvt::render`
/// is the same code over the same fixture, and a `Tile` is host allocations on
/// every target. It is a native measurement applied to wasm32, and it is
/// recorded that way rather than restated as a wasm one.
///
/// | tier    | entries | worst case, per source        |
/// |---------|--------:|------------------------------:|
/// | desktop |     256 | ~158 MiB vector, 64 MiB raster |
/// | mobile  |      96 | ~59 MiB vector, 24 MiB raster |
/// | wasm32  |      96 | ~59 MiB vector, 24 MiB raster |
///
/// The wasm32 arm is the one where that promotion has a budget to answer to:
/// `squallar-device-profile`'s `constants::WASM_APP_TEXTURE_BUDGET_BYTES` is
/// 288 MiB for the whole application, and `MapTileState::ensure_base_tiles`
/// keeps one base source live across theme flips (re-styled in place, never
/// duplicated), so base plus labels is ~118 MiB of it worst case where the
/// raster derivation said 48. It fits, with less room than the old figure
/// implied. The count did not move, because 96 is also the working-set floor
/// below. The parsed-geometry population is priced separately —
/// [`MEASURED_PARSED_TILE_BYTES`] and [`PARSED_TILE_CACHE_ENTRIES`].
///
/// **Against the meshes this workspace shipped until 2026-08-28 those figures
/// were 8.0 GiB and 4.0 GiB.** `VertexBuffers::new()` is
/// `with_capacity(512, 1024)` and the tile held 2,257 of them, so one entry was
/// 32.77 MB rather than 0.65 MB. Neither number was ever reached, because a
/// machine would have died first; they are quoted because they are what this
/// comment's old derivation — "a tile texture costs the same 256 KiB" — was
/// silently claiming, 50x under the truth on a cost it was not measuring at all.
///
/// **The mobile arm fell from 128 to 96.** The desktop arm did *not* fall, and
/// what stopped it is worth naming rather than leaving as an unexplained round
/// number: it is pinned from below by
/// `squallar_gpu::egui_renderer::mirror`'s `MIRROR_SCALE_MAX`. That cap is only
/// legitimate if a floor strip could actually take the rung, and
/// `mirror::tests::the_rung_above_the_cap_could_never_fit_the_tile_cache`
/// holds it so: a 900-point pane at two layers keeps 242 tiles resident at
/// `tile_zoom_bias = 1`, and 72 at bias 0 against a "three times over" margin.
/// So this arm may not go below 242 without moving the mirror's deepest rung,
/// which is a different subsystem's decision. What actually fixed desktop
/// residency was the 50.7x per-*entry* reduction, not the count.
///
/// **The honest remaining gap**: this is still a count, and a count cannot
/// express "158 MiB" as a limit when entries range from 456 bytes to 646,264.
/// A byte-budgeted LRU is the answer that would; it is not this change.
///
/// "Per source" is the multiplier: each map source owns one of these caches —
/// base and labels, light and dark — so the old desktop figure on wasm32 could
/// put 256 MiB of basemap texture against the 288 MiB
/// `squallar-device-profile`'s `constants::WASM_APP_TEXTURE_BUDGET_BYTES`
/// allows the whole application. The tiers follow that crate's budget cascade,
/// spelled rather than imported.
///
/// **Every arm is still above its working set, which is the floor no budget may
/// cross.** See below, and `tiles::tests`'
/// `the_cache_holds_the_working_set_at_every_zoom`. Mobile's 96 covers the 63 a
/// 1024x1366-point tablet keeps resident — the largest handheld canvas in
/// common use — where 128 covered nothing extra that a handheld can present.
///
/// **Every arm is sized against the worst case over the whole zoom range**, not
/// against a whole zoom. A tile is drawn `256 · 2^(zoom − round(zoom))` points
/// across, down to 181 at the half step, so between two whole zooms more tiles
/// fit the same window than at either end of it: 84 per source over a
/// 1920x1080-point canvas at one layer, against 54 at a whole zoom.
/// [`crate::tiles::tiles_resident_for`] reports that larger figure and
/// `tiles::tests` holds it equal to what the `tile_span` sweep measures.
///
/// What each arm covers, at one layer, in **points** — a 4K panel at the 2x
/// scaling it is nearly always run at presents 1920x1080 points, not 3840x2160:
///
/// | canvas    | tiles | wasm 96 | mobile 96 | desktop 256 |
/// |-----------|------:|---------|-----------|-------------|
/// | 1024x1366 |    63 | fits    | fits      | fits        |
/// | 1920x1080 |    84 | fits    | fits      | fits        |
/// | 1920x1200 |    96 | fits    | fits      | fits        |
/// | 2560x1440 |   144 | overruns| overruns  | fits        |
/// | 3840x2160 |   299 | overruns| overruns  | overruns    |
///
/// The wasm arm is 96 because that is 1920x1200 — the tallest panel in common
/// use at 1920 wide — and it carries 1080p's 84 with room rather than sitting
/// exactly on it. It costs 24 MiB per source, and the app can hold four live
/// sources at once (base and labels, light and dark; a theme flip retains
/// both), so 96 MiB against the 288 MiB budget: a third of it.
///
/// An LRU below the working set is not a slower cache, it is a broken one. It
/// evicts a tile that is still on the glass, the next frame re-enters
/// `request_once` for it, and that tile is fetched over the network again and
/// re-decoded against [`WASM_TILE_DECODES_PER_PUMP`] — for something the user
/// never stopped looking at. `tiles::tests`'
/// `the_cache_holds_the_working_set_at_every_zoom` is the gate.
#[cfg(target_arch = "wasm32")]
pub const TILE_CACHE_ENTRIES: NonZeroUsize = WASM_TILE_CACHE_ENTRIES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub const TILE_CACHE_ENTRIES: NonZeroUsize = MOBILE_TILE_CACHE_ENTRIES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub const TILE_CACHE_ENTRIES: NonZeroUsize = DESKTOP_TILE_CACHE_ENTRIES;

/// The wasm32 arm of [`TILE_CACHE_ENTRIES`]. All three arms are named outside
/// the cascade because this workspace runs `cargo test` on one arm, so the other
/// two are only reachable from a test if they have names.
///
/// **96 -> 100 at WO-10**, when `draw_tile_layer` started keeping the
/// [`crate::tiles::WARM_ANCESTOR_STEPS`] net resident beside the level it
/// draws. The working set is the drawn count plus the net's, and at 1920x1200
/// — the canvas this arm is quoted against — that is 96 + 4. The arm had been
/// sitting exactly on 96, so leaving it there would have made the net evict the
/// glass, which is the broken-cache failure `6345952f` fixed and not a smaller
/// version of it. 100 costs 61.5 MiB of vector worst case per source against
/// the old 59.0, so the four-live-source figure the budget note below prices is
/// 245.8 MiB against `WASM_APP_TEXTURE_BUDGET_BYTES`' 288 MiB: still under, by
/// less than it was. The full parent level was the alternative and it is the
/// reason the net is four steps out rather than one — 131 entries, 322 MiB,
/// over the budget.
pub const WASM_TILE_CACHE_ENTRIES: NonZeroUsize = NonZeroUsize::new(100).expect("100 is not zero");
/// The mobile arm — the largest handheld working set, not a fraction of the
/// desktop figure. See [`WASM_TILE_CACHE_ENTRIES`], including the WO-10 move.
pub const MOBILE_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(100).expect("100 is not zero");
/// The desktop arm — pinned from below at 242 by `squallar_gpu`'s
/// `MIRROR_SCALE_MAX`, not by the byte budget. See [`WASM_TILE_CACHE_ENTRIES`].
pub const DESKTOP_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(256).expect("256 is not zero");

/// The measured worst-case heap of one cached vector tile, in bytes.
///
/// The committed Monaco fixture's z14 city-core tile, 2026-08-28: shape spine,
/// path points, mesh vertices and indices and label strings, counted at
/// **capacity** rather than length because capacity is what is resident. The
/// derivation of [`TILE_CACHE_ENTRIES`] is against this figure; it is named so
/// a test can hold the two together instead of the number living only in prose.
///
/// **Re-measured the same day, 646_264 to 652_112 — +5,848 bytes, +0.9%**, when
/// `walkers::Text` grew the fields carrying a label's wrapping. It is a
/// re-measurement and not a relaxation:
/// `tile_source::tests::the_vector_entry_cost_is_what_the_fixture_actually_renders`
/// re-derives it rather than trusting this line. The budget it feeds is
/// unchanged in shape — 96 entries is 62.6 MB where it was 62.0 MB, and the
/// desktop arm is pinned from below by `MIRROR_SCALE_MAX` rather than by bytes,
/// so no arm's entry count moves.
///
/// **Removing the halo later the same day did NOT move it back, and an earlier
/// version of this comment predicted that it would.** That prediction came from
/// attributing the growth to `ShapeOrText` widening by 8 bytes per shape over
/// 731 shapes, which is arithmetic that lands on 5,848 exactly and is still
/// wrong: measured, `size_of::<ShapeOrText>()` is **72 both with and without the
/// halo fields**, because `Text` is 72 with them and 64 without, and the other
/// variant (`egui::Shape`) is 64 either way. The enum never changed size, so the
/// halo was never costing the spine anything. The figure below was re-derived
/// after the removal, with a forced rebuild, and is unchanged.
///
/// The lesson worth keeping is about the test rather than the number: it asserts
/// a *band* (`heap <= CONST <= 2 * heap`) and deliberately not equality, because
/// `size_of` and allocator rounding are toolchain properties. So it cannot catch
/// this constant drifting upward into a safe over-estimate. Re-derive it by
/// forcing the assertion to fail; do not infer it from a type's field list.
pub const MEASURED_VECTOR_TILE_BYTES: usize = 652_112;

/// What one cached raster tile costs: 256x256 RGBA.
pub const RASTER_TILE_BYTES: usize = 256 * 256 * 4;

/// The measured worst-case heap of one **parsed** vector tile, in bytes —
/// the second resident population the styled-entry figure above does not
/// cover.
///
/// Same tile, same method as [`MEASURED_VECTOR_TILE_BYTES`]: the committed
/// Monaco fixture's z14 city-core tile (185,182 MVT bytes), counted at
/// **capacity** by `walkers::mvt::ParsedTile::heap_bytes` — decoded geometry,
/// per-feature property bags, key and value strings. Measured 2026-08-29 by
/// forcing
/// `tile_source::tests::the_parsed_entry_cost_is_what_the_fixture_actually_parses`
/// to fail; the band there is the derivation, this line is only its record.
/// **3.2× the styled entry**, which is why the parsed cache gets its own,
/// smaller entry count rather than inheriting [`TILE_CACHE_ENTRIES`].
///
/// The composition matters, because the first measurement was **29,903,162 B**
/// and the difference is a fixed defect, not a re-count: `mvt-reader` grows
/// every ring at `Vec::with_capacity(<whole feature's command count>)`, so
/// geometry capacity was 28.1 MB for 318 KB of shrunk content.
/// `walkers::mvt::parse` now shrinks per feature (see `shrink_geometry`
/// there); what remains is 1,400,495 B of per-feature property bags — 2,913
/// features, 14,303 properties, a `HashMap` apiece — 317,736 B of geometry,
/// and the spine.
///
/// Like its styled sibling: re-derive it by forcing the test's band to fail,
/// never by inference from a type's field list — the band cannot catch this
/// constant drifting upward into a safe over-estimate.
pub const MEASURED_PARSED_TILE_BYTES: usize = 2_092_002;

/// Parsed-geometry entries retained per **archive** source before LRU
/// eviction — the bound on the second population [`MEASURED_PARSED_TILE_BYTES`]
/// prices.
///
/// **What this cache buys and what falling short costs.** It exists so a
/// style change — a theme flip, a map-detail toggle — re-styles from the
/// cached parse with **zero fetches and zero re-parses**
/// ([`HttpsTiles::set_style`]). An entry that was evicted before the restyle
/// costs one refetch for that tile, exactly the pre-split behaviour; it never
/// costs a frame. So unlike [`TILE_CACHE_ENTRIES`], the working set is a
/// *target*, not a floor a bound below which is broken — which is what lets
/// the wasm arm sit below it where the byte budget leaves no other choice.
///
/// The arithmetic, against the measured 2,092,002 B tail
/// ([`MEASURED_PARSED_TILE_BYTES`]) and alongside the styled population it
/// joins (per archive source; the terrain source is raster and holds zero
/// parsed entries):
///
/// | tier    | parsed entries | parsed worst | styled worst | total     |
/// |---------|---------------:|-------------:|-------------:|----------:|
/// | desktop |             96 |    191.5 MiB |    159.2 MiB | 350.7 MiB |
/// | mobile  |             24 |     47.9 MiB |     59.7 MiB | 107.6 MiB |
/// | wasm32  |             24 |     47.9 MiB |     59.7 MiB | 107.6 MiB |
///
/// Desktop's 96 is the 1920x1200 worst-case working set
/// (`tiles::tiles_resident_for`, the figure the wasm styled arm is derived
/// from), so the common canvas restyles wholly from cache; the deeper
/// floor-strip working sets a zoom bias creates (242 at bias 1) are *not*
/// covered, and `ui.rs`'s `tile_zoom_bias_for_pane` gate deliberately does
/// not consult this cache — a bias overrunning it degrades restyle economy,
/// never the frame, so gating on it would trade a real frame guarantee
/// against an economy one.
///
/// The mobile/wasm 24 is a budget answer, not a working-set one:
/// `squallar-device-profile` allows the whole wasm application 288 MiB, the
/// styled population already prices at ~59.7 MiB worst case, and 96 parsed
/// entries would put 191.5 MiB more against it — two thirds of the budget for
/// an economy cache. 24 holds the most recently *decoded* two dozen tiles — a
/// phone-sized viewport is 20–35 tiles — so a flip restyles most or all of
/// the glass from cache and refetches only what had already scrolled away.
///
/// These are tail figures over every entry at once, as the styled table's
/// are: the styled population's fixture-wide mean is 48× below its tail, and
/// a parse of an ordinary tile is small for the same reason a styling of one
/// is (no per-tile mean has been measured for the parse; the tail is what
/// sizing uses). All three arms are held against their ceilings in
/// `tile_source::tests::the_tuning_constants_are_the_written_figures_on_every_tier`.
#[cfg(target_arch = "wasm32")]
pub const PARSED_TILE_CACHE_ENTRIES: NonZeroUsize = WASM_PARSED_TILE_CACHE_ENTRIES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub const PARSED_TILE_CACHE_ENTRIES: NonZeroUsize = MOBILE_PARSED_TILE_CACHE_ENTRIES;
/// See the wasm32 arm above.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub const PARSED_TILE_CACHE_ENTRIES: NonZeroUsize = DESKTOP_PARSED_TILE_CACHE_ENTRIES;

/// The wasm32 arm of [`PARSED_TILE_CACHE_ENTRIES`] — named outside the
/// cascade for the reason [`WASM_TILE_CACHE_ENTRIES`] is: `cargo test` runs
/// one arm, and the others are only reachable from a test if they have names.
pub const WASM_PARSED_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(24).expect("24 is not zero");
/// The mobile arm — the same budget arithmetic as wasm's, see
/// [`PARSED_TILE_CACHE_ENTRIES`].
pub const MOBILE_PARSED_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(24).expect("24 is not zero");
/// The desktop arm — the 1920x1200 worst-case working set, so the common
/// canvas restyles with zero fetches. See [`PARSED_TILE_CACHE_ENTRIES`].
pub const DESKTOP_PARSED_TILE_CACHE_ENTRIES: NonZeroUsize =
    NonZeroUsize::new(96).expect("96 is not zero");

/// The parsed-geometry cache one archive source's IO task and frame side
/// share: the style-independent half of every vector tile the source has
/// decoded, keyed by [`TileId`].
///
/// A `Mutex` because on native the IO runtime's blocking pool writes it while
/// the frame side owns the handle; on wasm every party is the page thread and
/// the lock is never contended.
type SharedParsedTiles = Arc<Mutex<LruCache<TileId, Arc<walkers::mvt::ParsedTile>>>>;

/// What the archive IO task styles a tile against, and which restyle
/// generation that styling belongs to. Written by [`HttpsTiles::set_style`],
/// read by [`read_one`] once per tile — together under one lock, because a
/// style observed with another generation's number would let a stale styling
/// be cached as current. Native only: on wasm32 the frame pump is the styling
/// side and owns the live style directly.
#[cfg(not(target_arch = "wasm32"))]
struct StyleSlot {
    style: Arc<Style>,
    epoch: u64,
}

/// What the archive IO task carries to style against — the slot on native,
/// nothing on wasm32, where styling happens in the frame pump. A type alias
/// so [`serve_archive_continuously`] and [`read_one`] keep one body each.
#[cfg(not(target_arch = "wasm32"))]
type IoStyleSlot = Arc<std::sync::RwLock<StyleSlot>>;
/// See the native arm above.
#[cfg(target_arch = "wasm32")]
type IoStyleSlot = ();

/// The style generation raster payloads carry: raster sources never restyle,
/// so their frame-side epoch stays at this value and every payload matches.
const RASTER_STYLE_EPOCH: u64 = 0;

/// One slot of [`HttpsTiles`]' tile cache: the styled tile — or the
/// pending/failed `None` marker — and the style generation it belongs to.
///
/// The epoch is what makes a restyle seamless: [`HttpsTiles::set_style`] bumps
/// the source's generation without clearing anything, a slot from an older
/// generation keeps **drawing** (a stale-styled tile beats a blank one) while
/// [`HttpsTiles::request_once`] re-asks for it, and the restyled arrival
/// overwrites it. A `None` marker from an older generation is re-asked too —
/// "failed, do not ask again" is a per-style verdict only as far as the next
/// restyle, which for an archive source re-asks the parsed cache, not the
/// network.
struct CachedTile {
    epoch: u64,
    tile: Option<Tile>,
    /// The tile's tessellated fills, flattened for the renderer — `None` for
    /// a raster tile, a pending marker, or a vector tile whose style produced
    /// no fills at this zoom. Built beside the styling, on the thread that
    /// did the styling, so it never costs the frame more than the styling
    /// already does; see [`crate::tile_mesh`].
    meshes: Option<Arc<crate::tile_mesh::TileMeshes>>,
}

/// The cache slot a decoded tile becomes.
///
/// **Built where the tile was decoded**, which is the IO runtime's blocking
/// pool on native and the frame pump's decode budget on wasm32 — the same two
/// places the styling itself runs, so flattening the fills is billed to the
/// side that already pays for tessellating them.
fn slot_for(tile: Tile, epoch: u64) -> CachedTile {
    let meshes = match &tile {
        Tile::Vector(shapes) => {
            let flat = crate::tile_mesh::flatten(shapes);
            (!flat.is_empty()).then(|| Arc::new(flat))
        }
        Tile::Raster(_) => None,
    };
    CachedTile {
        epoch,
        tile: Some(tile),
        meshes,
    }
}

/// One grid cell's answer: the piece [`Tiles::at`] would have given, plus the
/// flattened fills of the tile it came from.
///
/// The two travel together because they must describe the same cache slot: a
/// stretched ancestor's fills are the *ancestor's*, and a piece paired with
/// another tile's buffers would draw the wrong geography under the right
/// clip.
pub(crate) struct GroundPiece {
    pub(crate) tile: Tile,
    pub(crate) uv: egui::Rect,
    pub(crate) meshes: Option<Arc<crate::tile_mesh::TileMeshes>>,
}

/// The terrain hillshade source's cache bound.
///
/// Its own name, derived the way [`TILE_CACHE_ENTRIES`]' arms derive theirs —
/// and the derivation lands on the same three counts, because every floor
/// behind them is zoom geometry rather than content: 96 covers the 1920x1200
/// working set on wasm and mobile, and the desktop arm is pinned from below at
/// 242 by `squallar_gpu`'s `MIRROR_SCALE_MAX` (terrain draws on the 3D floor
/// strips too, so the mirror's deepest rung binds it exactly as it binds the
/// basemap). Equality is also what keeps `ui.rs`'s zoom-bias gate coherent:
/// `tile_zoom_bias_for_pane` compares one per-source working set against one
/// per-source bound, and a smaller terrain bound would let the gate admit a
/// bias that overruns terrain's cache while the basemap's absorbs it.
///
/// What differs is the **byte** worst case, and it is why the shared count is
/// cheap here: every terrain entry is a raster texture ([`RASTER_TILE_BYTES`]
/// = 256 KiB — the WebP body is decoded, remapped and uploaded, never
/// retained), so the bound is worth the raster column of the table above —
/// 64 MiB desktop, 24 MiB mobile/wasm — with no 646 KB vector tail to size
/// for.
pub const TERRAIN_TILE_CACHE_ENTRIES: NonZeroUsize = TILE_CACHE_ENTRIES;

/// How an archive source's tile bodies become [`Tile`]s — decided **once per
/// archive from its header's `tile_type`**, never by sniffing bodies.
///
/// The seam exists because two archives now flow through the same fetch loop
/// with different pixels in them: the basemap (`tile_type = 1`, MVT) and the
/// terrain hillshade (`tile_type = 4`, WebP). Before it, every body went
/// through `walkers::Tile::new`, which *guesses* — image decode if any decoder
/// recognises the bytes, MVT otherwise — so a raster archive would have
/// painted raw gdaldem grey over the map and a corrupt MVT body would be
/// sniffed rather than reported. The header names what the archive holds;
/// [`decode_archive_tile`] obeys it.
///
/// `pub(crate)` rather than `pub`: [`Self::TerrainRgb`] is a claim a *caller*
/// makes about an archive whose header cannot carry the distinction, but the
/// only thing that consumes such a claim is
/// [`HttpsTiles::from_range_source`], which is itself `pub(crate)` — so no
/// caller outside this crate could pass one even in principle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArchiveTileKind {
    /// `tile_type = 1` (MVT): tessellated against the committed style via
    /// [`Tile::from_mvt`] — an image body in a vector archive is an error,
    /// not a fallback.
    Vector,
    /// A raster `tile_type` (2 PNG, 3 JPEG, 4 WebP, 5 AVIF): decoded as an
    /// image and remapped by [`crate::terrain::decode_hillshade_tile`]. The
    /// remap is unconditional because the app's only raster archive is the
    /// terrain hillshade; a future plain-raster archive must add its own arm
    /// here rather than inherit the remap.
    Hillshade,
    /// Packed elevation in a PNG body: the terrain-RGB archives
    /// ([`crate::tiles::HEIGHT_ARCHIVE_URL`]). **Never reachable from
    /// [`Self::from_tile_type`]** — `tile_type = 2` is PNG for both a
    /// hillshade and an elevation grid, and no header field separates them —
    /// so it is only ever a *declaration*, cross-checked against the header
    /// at open (see [`serve_archive_continuously`]).
    ///
    /// It exists so that an elevation archive reaching a picture path fails
    /// loudly. There is no image in these bodies: each pixel is a base-256
    /// height triple, and a compositor handed one paints noise that looks
    /// like terrain.
    ///
    /// **Nothing in the shipped build constructs it, and that is the design,
    /// not an oversight.** The only consumer of a declaration is
    /// [`HttpsTiles::from_range_source`], and the height reader deliberately
    /// does not build an `HttpsTiles` at all — heights are read as bytes
    /// through [`crate::tiles::height_range_source`]. So the arms that receive
    /// this variant are the guard rail for the day something *does* route an
    /// elevation archive at the picture path, and only the tests reach them
    /// today. `expect` rather than `allow`, and only off the test cfg, so the
    /// first production construction makes this attribute unfulfilled and asks
    /// to be deleted.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "declared by callers of from_range_source; the height \
                      reader deliberately builds no HttpsTiles, so only the \
                      tests construct it until something does"
        )
    )]
    TerrainRgb,
    /// `tile_type = 0`: the header declares nothing, so the body is sniffed
    /// exactly as a plain HTTP tile is. No archive this app opens says this;
    /// the arm keeps "unknown" behaving as the pre-seam code did instead of
    /// picking a guess of its own.
    Undeclared,
}

/// Turn one archive tile body into a [`Tile`], the way `kind` — the archive
/// header's word — says to. See [`ArchiveTileKind`] for why the header and
/// never the bytes.
///
/// Costs where it runs: on native this is called on the IO runtime's blocking
/// pool ([`read_one`]), on wasm32 under the frame pump's
/// [`WASM_TILE_DECODES_PER_PUMP`] budget — never freely on the frame thread.
pub(crate) fn decode_archive_tile(
    bytes: &[u8],
    kind: ArchiveTileKind,
    style: &Style,
    zoom: u8,
    ctx: &Context,
) -> Result<Tile, String> {
    let tile = match kind {
        // `Tile::from_mvt` fuses the two halves (`render` IS `styled(parse())`),
        // and this arm spells them separately for one reason: so that the
        // phase ledger covers BOTH decode paths. A tile source with no parsed
        // cache reaches here, and a split that only the remembering path
        // recorded would report a figure whose denominator silently depended
        // on which source served the tile.
        ArchiveTileKind::Vector => {
            let parsed = timed_parse(bytes)?;
            Ok(timed_styled(&parsed, style, zoom))
        }
        ArchiveTileKind::Hillshade => {
            let remapped = crate::terrain::decode_hillshade_tile(bytes)?;
            Ok(Tile::Raster(ctx.load_texture(
                "terrain-hillshade",
                remapped,
                Default::default(),
            )))
        }
        ArchiveTileKind::TerrainRgb => Err(
            "this archive carries packed elevation and has no picture in it: a \
             terrain-RGB body is a base-256 height triple per pixel, so it is \
             read as data and never decoded into a tile"
                .to_owned(),
        ),
        ArchiveTileKind::Undeclared => {
            Tile::new(bytes, style, zoom, ctx).map_err(|error| error.to_string())
        }
    }?;
    note_archive_decode(kind);
    Ok(tile)
}

/// Tell [`crate::basemap_ledger`] that one archive body of `kind` decoded.
///
/// **Called only where a decode returned `Ok`**, and by both decoders, so the
/// ledger's denominator is exactly "bodies that became a [`Tile`]". The kind
/// is the archive header's word and never the bytes', which is the same rule
/// [`decode_archive_tile`] itself obeys — a ledger that sniffed would be
/// answering a different question from the decoder it is measuring.
///
/// [`ArchiveTileKind::TerrainRgb`] has no arm because it has no `Ok`: an
/// elevation body is refused, so nothing to count ever reaches here.
fn note_archive_decode(kind: ArchiveTileKind) {
    match kind {
        ArchiveTileKind::Vector => crate::basemap_ledger::note_vector_tile(),
        ArchiveTileKind::Hillshade => crate::basemap_ledger::note_raster_tile(),
        ArchiveTileKind::Undeclared => crate::basemap_ledger::note_sniffed_tile(),
        ArchiveTileKind::TerrainRgb => {}
    }
}

/// Style a parsed tile into the value the tile cache holds.
fn styled_tile(parsed: &walkers::mvt::ParsedTile, style: &Style, zoom: u8) -> Tile {
    timed_styled(parsed, style, zoom)
}

/// Whole microseconds a closure took, saturating into the ledger's `u32`.
fn micros_of<T>(work: impl FnOnce() -> T) -> (T, u32) {
    let began = web_time::Instant::now();
    let out = work();
    let cost = web_time::Instant::now().duration_since(began);
    (out, cost.as_micros().min(u128::from(u32::MAX)) as u32)
}

/// [`walkers::mvt::parse`], charged to [`take_ledger::VectorPhase::Parse`].
///
/// Two clock reads per vector **body**, not per layer and not per feature: a
/// per-feature clock would be thousands of reads on the hot path and is a
/// harness's job, not a ledger's.
fn timed_parse(bytes: &[u8]) -> Result<walkers::mvt::ParsedTile, String> {
    let (parsed, micros) = micros_of(|| walkers::mvt::parse(bytes));
    // Charged whether or not it succeeded: a body that failed to parse cost
    // the frame the same time, and excluding it would make a broken archive
    // read as a fast one -- the same rule the take families follow.
    take_ledger::note_vector_phase(take_ledger::VectorPhase::Parse, micros);
    parsed.map_err(|error| error.to_string())
}

/// [`walkers::mvt::styled`], charged to [`take_ledger::VectorPhase::Style`].
fn timed_styled(parsed: &walkers::mvt::ParsedTile, style: &Style, zoom: u8) -> Tile {
    let (shapes, micros) = micros_of(|| walkers::mvt::styled(parsed, style, zoom));
    take_ledger::note_vector_phase(take_ledger::VectorPhase::Style, micros);
    Tile::Vector(Arc::new(shapes))
}

/// [`decode_archive_tile`], with the parse half **remembered**: a vector
/// body's zoom- and style-independent decode lands in `parsed_tiles` before
/// the styling, so a later restyle of this tile ([`HttpsTiles::set_style`])
/// touches neither the network nor the bytes. The raster arms delegate
/// unchanged — there is nothing style-independent to keep for a pixel body.
///
/// Runs where [`decode_archive_tile`] runs: the IO runtime's blocking pool on
/// native, the frame pump's [`WASM_TILE_DECODES_PER_PUMP`] budget on wasm32.
fn decode_archive_tile_remembering(
    bytes: &[u8],
    kind: ArchiveTileKind,
    style: &Style,
    tile_id: TileId,
    ctx: &Context,
    parsed_tiles: &SharedParsedTiles,
) -> Result<Tile, String> {
    match kind {
        ArchiveTileKind::Vector => {
            let parsed = Arc::new(timed_parse(bytes)?);
            parsed_tiles
                .lock()
                .expect("the parsed-tile cache is not poisoned")
                .put(tile_id, Arc::clone(&parsed));
            // The vector arm does not delegate, so it counts for itself; the
            // raster arms are counted inside `decode_archive_tile`.
            note_archive_decode(ArchiveTileKind::Vector);
            Ok(timed_styled(&parsed, style, tile_id.zoom))
        }
        raster => decode_archive_tile(bytes, raster, style, tile_id.zoom, ctx),
    }
}

/// The `max_zoom` value that means "not read yet", not "zero".
///
/// An archive source does not learn its own depth until the IO task has read
/// the PMTiles header, and the two states have to be *different* values because
/// they mean opposite things to [`Tiles::at`]. Zero was used for both, and what
/// zero draws is why that was a defect rather than a nicety:
///
/// `ui_map_overlays::draw_tile_layer` clamps the tile zoom to
/// [`HttpsTiles::source_max_zoom`], so every pane's first frames asked for
/// `0/0/0` — and that tile then stayed in the LRU, where
/// [`HttpsTiles::cached_or_interpolated`] walked to it as the fallback ancestor
/// for any deep tile still in flight, for the rest of the session.
/// `full_rect_of_clipped_tile` places an ancestor against a rect of
/// `256 · 2^zoom` points, so the fixture's z0 tile — 7,889 bytes, six shapes —
/// was drawn 16,384 points across at app zoom 6 and 4,194,304 at zoom 14. Its
/// widest stroke measured 16 extent units, and `Shape::transform` scaled
/// `stroke.width` with the geometry, so a "blurry ancestor" painted as a
/// 64-point, 1,024-point and 16,384-point slab of solid colour.
///
/// 255 is safe as the sentinel because it is not a zoom: `tile_id_is_valid`
/// rejects every zoom at or past 32, since [`walkers::mercator::total_tiles`]
/// cannot count the grid there.
const MAX_ZOOM_UNKNOWN: u8 = u8::MAX;

/// Per-tile request timeout.
///
/// walkers sets none. A tile that never answers would otherwise hold one of the
/// [`MAX_PARALLEL_DOWNLOADS`] slots for as long as the process lives.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Tile decodes the frame side performs per **source** per **pass**, wasm32
/// only.
///
/// On native the IO thread decodes and uploads off the frame thread. On wasm
/// `spawn_local` runs the fetch loop on the page itself, so a pan exposing a
/// fresh row of tiles pays the decode+upload on the very thread the gesture
/// runs on.
///
/// **The count is a backstop; [`PUMP_TIME_BUDGET`] is the governor.** `tile_rx`
/// is `channel(MAX_PARALLEL_DOWNLOADS)` with a single sender, so the queue
/// cannot hold more than [`MAX_PARALLEL_DOWNLOADS`] and this value means
/// "whatever is queued" rather than a throttle — the same figure and the same
/// meaning as [`NATIVE_TILE_UPLOADS_PER_PUMP`], because after the time bound
/// went in the two arms want the same rule.
///
/// It was 2, unconditionally, and that was the wrong shape in both directions.
/// A frame whose takes are cheap was held to 2 when the queue held 6, so a
/// zoom filled at a quarter of the rate the network was delivering; a frame
/// whose takes are expensive still paid *two* multi-millisecond tessellations,
/// because a count cannot see what a take costs. The time bound cuts the
/// expensive case to one take and lets the cheap case empty the queue.
///
/// Deliberately **capped inline, not offloaded**: tile PNGs behind the overlay
/// worker's job round-trip would trade a bounded per-pass cost for seconds of
/// blank basemap. Bounding a *single* take below the frame budget is what that
/// offload (C5p2) would buy and this cannot.
pub const WASM_TILE_DECODES_PER_PUMP: usize = MAX_PARALLEL_DOWNLOADS + 1;

/// The wall time one [`HttpsTiles::pump`] may spend taking completions off the
/// channel.
///
/// **This is the governor the gesture latch used to be.** Until WO-10 the pump
/// took *nothing* while a map gesture was live and resumed only after a 500 ms
/// wall clock, so a zoom that ended left the map unchanged for half a second
/// before the first tile could even be put. The cost that latch was dodging is
/// real — on wasm32 a take is a PNG decode and a texture upload, on native a
/// cache put — but it is a cost per take, and a count bound cannot see it.
/// A time bound can, and it needs no notion of a gesture at all: the frame
/// budget governs whether the map moves or not.
///
/// Checked *before* each take and never mid-take, so the true worst case is
/// this plus one take. Bounding one take's own cost is not something this
/// constant can do — that is the deferred worker-side decode (C5p2).
///
/// **A pass always takes at least one completion per source**, however far past
/// the deadline the frame already is. A budget that can round down to zero work
/// stalls the map exactly when the machine is busiest, which is the failure
/// this replaces rather than a different spelling of it. The guarantee is the
/// *pass's* and not each call's: see [`PumpBudget`], where spelling it per call
/// is the defect that shape had.
///
/// **1 ms, and the value is chosen for wasm32, because that is the only
/// target it binds on.** A frame's worst case is one of these plus one take
/// **per source** — two of each in a standard configuration, which draws the
/// basemap and the terrain. It is not per *layer-draw*: a source drawn in six
/// panes is pumped six times, and [`PumpBudget`] is what holds those six to
/// the one budget, in time as well as in count. Before it did, the same
/// sentence was true only of the single-pane layout it was written against.
///
/// A native take is an `LruCache::put` costing microseconds, so neither 1 ms
/// nor 2 ms is ever reached there and the count is what bounds a native pump.
/// A wasm32 take is a PNG decode and a texture upload costing milliseconds, so
/// this is the bound that actually fires — and halving it halves the worst
/// case on the target that needs it most.
///
/// **What the value governs is bursts, not steady state**, because the first
/// take is unconditional. Every frame therefore moves at least one tile per
/// source — two in a standard configuration — however small this is, which is
/// of the order of what [`MAX_PARALLEL_DOWNLOADS`] concurrent fetches deliver:
/// in steady state the floor is already keeping up and this bound is not what
/// the map is waiting on. It binds only against a backlog, and `tile_rx` holds
/// at most [`MAX_PARALLEL_DOWNLOADS`] of those: at one take per pass — which
/// is what 1 ms means when a take is a multi-millisecond tessellation — a full
/// queue clears in six frames, 100 ms at 60 Hz. That is the whole cost of the
/// smaller number, and against it the frame carries 1 ms per source.
///
/// **Measured, and it is not the whole story.** Native scene A interact p50
/// sits one histogram bin above the pre-WO-10 build: 26,909 us against
/// 32,000 us, base 3/3 and this build 2/2 across legs under loadavg 10,
/// counterbalanced base/head/head/base, viewport 1920x1080 verified by pid at
/// 17.97 MB/picture on every row. That gap did **not** move when this constant
/// went 2 ms -> 1 ms, which is what proves it is not this value's: on native
/// the bound never binds. What the gap is, is the pump draining on every frame
/// at all where WO-9e had it draining on none — up to
/// [`NATIVE_TILE_UPLOADS_PER_PUMP`] puts per layer per frame, each bumping
/// `put_generation` and so repainting the floor strip. The `pump` frame
/// segment carries it: p99 median 6,728 us base against 9,514 us here, the
/// same one bin.
///
/// **This is the receipt for worker-side tile decode (C5p2), which was
/// deferred as measurement-conditional and now has its measurement.** The bin
/// is buyable back, and only one way: by making a *take* cheaper. It cannot be
/// tuned away from here, because the bound this constant sets is never the one
/// that binds on the arm the regression was measured on — a smaller number
/// changes nothing, which is the experiment above. Off-thread decode is the
/// change that would, and a future reader should find this note rather than
/// re-derive it.
const PUMP_TIME_BUDGET: Duration = Duration::from_millis(1);

/// Completed fetches one [`HttpsTiles::pump`] moves into the cache, native
/// only.
///
/// Not a throttle, and not the counterpart of [`WASM_TILE_DECODES_PER_PUMP`]:
/// on native [`fetch_one`] already decoded and uploaded on the IO thread, so a
/// take here is a `LruCache::put` and nothing more. `tile_tx`/`tile_rx` is
/// `channel(MAX_PARALLEL_DOWNLOADS)` with a single sender, so the queue cannot
/// hold more than this, and the bound is only ever reached with an empty queue
/// behind it.
///
/// It is spelled rather than left as "drain until empty" so the loop is bounded
/// by a named figure and cannot spin against an IO thread refilling as fast as
/// it is drained. The figure is what the old shape already moved per pass:
/// [`Tiles::at`] took one tile per call and ran once per grid cell, and a layer
/// has tens of cells — 54 over a 1920x1080-**point** canvas at a whole zoom and
/// bias 0, by [`crate::tiles::tiles_resident_at_whole_zoom`] — so a pass already
/// emptied the queue. Pumping once per layer keeps that only by taking the whole
/// queue rather than one tile.
#[cfg(not(target_arch = "wasm32"))]
const NATIVE_TILE_UPLOADS_PER_PUMP: usize = MAX_PARALLEL_DOWNLOADS + 1;

// ---------------------------------------------------------------------------
// Target-dependent bounds
// ---------------------------------------------------------------------------

/// A [`TileSource`] that can be moved into the IO task. The bound differs by
/// target and this alias is where that difference lives: on native the task runs
/// on another thread and everything it captures must be `Send + Sync`; on wasm
/// it runs on the UI thread, where `reqwest`'s types are neither.
#[cfg(not(target_arch = "wasm32"))]
pub trait AsyncTileSource: TileSource + Send + Sync + 'static {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: TileSource + Send + Sync + 'static> AsyncTileSource for T {}

#[cfg(target_arch = "wasm32")]
pub trait AsyncTileSource: TileSource + 'static {}
#[cfg(target_arch = "wasm32")]
impl<T: TileSource + 'static> AsyncTileSource for T {}

/// What one completed fetch hands the frame side.
///
/// Native: the finished cache slot — [`fetch_one`] and [`read_one`] decode,
/// upload and flatten on the IO thread — carrying the style generation it was
/// styled under, so the frame side can drop a tile styled by a style a restyle
/// has already replaced ([`CachedTile`]). wasm32: the work still owed, under
/// the frame pump's [`WASM_TILE_DECODES_PER_PUMP`] budget — see
/// [`HttpsTiles::pump`]; no generation travels with it, because the pump
/// styles against the frame's current style by construction.
#[cfg(not(target_arch = "wasm32"))]
type FetchPayload = CachedTile;
#[cfg(target_arch = "wasm32")]
enum FetchPayload {
    /// A tile body the pump must decode — a compressed PNG from a raster
    /// source, an MVT body from the archive (parsed, remembered, styled).
    Bytes(Vec<u8>),
    /// A parse the IO task found in [`SharedParsedTiles`]: the fetch was
    /// skipped entirely; only the styling remains, and it still bills the
    /// pump budget — tessellation is the heavy half.
    Parsed(Arc<walkers::mvt::ParsedTile>),
}

/// Managed IO runtime for the tile fetch task — and for
/// [`crate::basemap_download`], which owns the same kind of task for the same
/// reason and must not grow a second copy of this split.
pub(crate) mod runtime {
    #[cfg(not(target_arch = "wasm32"))]
    mod native {
        /// Owns the IO thread. Dropping it stops the fetch task.
        pub(crate) struct Runtime {
            join_handle: Option<std::thread::JoinHandle<()>>,
            quit_tx: tokio::sync::mpsc::UnboundedSender<()>,
        }

        /// Run `future` on a private current-thread runtime on its own thread.
        ///
        /// `spawn`ed rather than `block_on`'d, with the thread parked on a quit
        /// channel: dropping the runtime cancels an in-flight request outright,
        /// whereas blocking on the future would make [`Runtime::drop`] wait out
        /// the request on the UI thread.
        pub(crate) fn spawn<F>(future: F) -> Runtime
        where
            F: Future<Output = ()> + Send + 'static,
        {
            let (quit_tx, mut quit_rx) = tokio::sync::mpsc::unbounded_channel();

            let join_handle = std::thread::spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        log::error!("could not start the tile IO runtime: {error}");
                        return;
                    }
                };
                runtime.spawn(future);
                runtime.block_on(quit_rx.recv());
            });

            Runtime {
                join_handle: Some(join_handle),
                quit_tx,
            }
        }

        impl Drop for Runtime {
            fn drop(&mut self) {
                // Both of these fail only if the thread is already gone, which is
                // not a condition there is anything to do about.
                let _ = self.quit_tx.send(());
                if let Some(join_handle) = self.join_handle.take() {
                    let _ = join_handle.join();
                }
            }
        }

        /// A runtime that owns no thread and never ran a task — the inert
        /// source's slot. Dropping it sends the quit onto a channel nobody
        /// holds, which is already a condition [`Runtime::drop`] shrugs at.
        pub(crate) fn inert() -> Runtime {
            let (quit_tx, _quit_rx) = tokio::sync::mpsc::unbounded_channel();
            Runtime {
                join_handle: None,
                quit_tx,
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    mod web {
        /// There is no thread to own: the task runs on the browser's event loop.
        /// It still stops when [`super::super::HttpsTiles`] is dropped.
        pub(crate) struct Runtime;

        pub(crate) fn spawn<F>(future: F) -> Runtime
        where
            F: Future<Output = ()> + 'static,
        {
            wasm_bindgen_futures::spawn_local(future);
            Runtime
        }

        /// The inert slot: on this arm there was never a thread to not own.
        pub(crate) fn inert() -> Runtime {
            Runtime
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) use native::{Runtime, inert, spawn};
    #[cfg(target_arch = "wasm32")]
    pub(crate) use web::{Runtime, inert, spawn};
}

// ---------------------------------------------------------------------------
// Slippy-map helpers walkers keeps private
// ---------------------------------------------------------------------------

/// Number of tiles along one axis at `zoom`, or `None` if `zoom` is absurd.
///
/// walkers' `mercator::total_tiles` is `pub(crate)` and computes `2u32.pow(zoom)`,
/// which overflows for `zoom >= 32`. `TileId::zoom` is a `u8`.
fn total_tiles(zoom: u8) -> Option<u32> {
    2u32.checked_pow(zoom as u32)
}

/// Is this tile inside the grid for its own zoom level?
///
/// Reproduces `TileId::valid`, which walkers also keeps `pub(crate)`. Requesting
/// an out-of-range tile would 404 forever and burn a cache slot doing it.
fn tile_id_is_valid(tile_id: TileId) -> bool {
    match total_tiles(tile_id.zoom) {
        Some(side) => tile_id.x < side && tile_id.y < side,
        None => false,
    }
}

/// Locate `tile_id` inside its ancestor at `available_zoom`.
///
/// Returns the ancestor and the sub-rectangle of it that `tile_id` covers, in
/// texture (`uv`) coordinates — how a zoomed-in view stays populated with blurry
/// tiles instead of holes. Reproduces walkers'
/// `tiles::interpolate_from_lower_zoom`, which is `pub(crate)`.
///
/// # Panics
///
/// Debug-asserts `available_zoom <= tile_id.zoom`. Every call site walks `zoom`
/// downwards or clamps to `max_zoom`, after [`tile_id_is_valid`] has rejected
/// `zoom >= 32`, so the shift cannot overflow.
fn interpolate_from_lower_zoom(tile_id: TileId, available_zoom: u8) -> (TileId, egui::Rect) {
    debug_assert!(
        tile_id.zoom >= available_zoom,
        "cannot interpolate {tile_id:?} from the higher zoom {available_zoom}"
    );

    let dzoom = 2u32.pow((tile_id.zoom.saturating_sub(available_zoom)) as u32);

    let x = (tile_id.x / dzoom, tile_id.x % dzoom);
    let y = (tile_id.y / dzoom, tile_id.y % dzoom);

    let ancestor = TileId {
        x: x.0,
        y: y.0,
        zoom: available_zoom,
    };

    let z = (dzoom as f32).recip();
    let uv = egui::Rect::from_min_max(
        egui::pos2(x.1 as f32 * z, y.1 as f32 * z),
        egui::pos2(x.1 as f32 * z + z, y.1 as f32 * z + z),
    );

    (ancestor, uv)
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// The HTTP client every basemap tile is fetched with:
/// [`squallar_radar::tls::client`] and nothing else — platform verifier, *ring*,
/// `https_only`. There is deliberately no way to inject a different client from
/// outside the crate.
pub(crate) fn tile_client() -> reqwest::Client {
    squallar_radar::tls::client(squallar_radar::tls::USER_AGENT, REQUEST_TIMEOUT)
        .build()
        .expect("the tile HTTP client should build")
}

// ---------------------------------------------------------------------------
// HttpsTiles
// ---------------------------------------------------------------------------

/// Drop-in replacement for [`walkers::HttpTiles`]. See the module docs for what
/// is reproduced and what deliberately differs.
///
/// Must persist between frames: the cache and the IO task live in it.
pub struct HttpsTiles {
    attribution: Attribution,
    tile_size: u32,

    /// The source's deepest zoom.
    ///
    /// Shared and atomic rather than a plain `u8` because an archive source
    /// does not know it at construction: the number is in the PMTiles header,
    /// which is read by the IO task. See [`HttpsTiles::from_archive_url`] for
    /// what it holds before the header arrives, and [`MAX_ZOOM_UNKNOWN`].
    max_zoom: Arc<AtomicU8>,

    /// Why this source will never serve a tile, once that is known.
    ///
    /// Written once by the IO task and read every frame, so a
    /// [`std::sync::OnceLock`] rather than a lock the frame has to take. Only
    /// the archive path ever sets it: a raster source's failures are per-tile
    /// and per-request, and a 404 on one tile says nothing about the next.
    fault: Arc<OnceLock<String>>,

    /// Whether this source's tile reads have been failing in an unbroken run
    /// of [`SUSTAINED_READ_FAILURES`] — set and cleared by the archive IO
    /// task, read every frame.
    ///
    /// **Not [`Self::fault`], and the difference is recovery.** A fault is
    /// permanent by construction — a `OnceLock`, and the slot that acts on it
    /// drops the source — which is right for an archive that will not open and
    /// wrong for reads that fail. This is the state a *per-tile* failure mode
    /// leaves the map in: a network that went, a corrupt downloaded part, an
    /// expired generation, an offset a 32-bit target cannot address. Each of
    /// them blanks the map while the archive itself logs as healthily open,
    /// because the header and the root directory fit inside the opening read.
    /// The frame side reads it so the credit corner can stop naming a provider
    /// whose bytes are not on the glass; a read that answers clears it.
    reads_failing: Arc<AtomicBool>,

    /// Set when the request channel has been found disconnected.
    ///
    /// **This is a latch, and it exists to bound a log flood.** A disconnected
    /// `Sender` reports `TrySendError::is_full() == false`, so the disconnect
    /// falls past the retry arm into an `error!`; and because the closure
    /// failed, `LruCache::try_get_or_insert` inserts nothing, so the very same
    /// tile is asked for again on the next frame. Measured against a viewport
    /// of 54 tiles that is 54 error lines per frame for as long as the app is
    /// open. The IO task being gone is a permanent condition, so it is worth
    /// exactly one line.
    requests_closed: bool,

    /// Tiles by id. A slot with `tile: None` means "asked for, not here yet"
    /// *and* "asked for, and it failed" — the two are deliberately
    /// indistinguishable, because both mean "do not ask again" — for as long
    /// as the slot's style generation is current. See [`Self::request_once`]
    /// and [`CachedTile`] for what a stale generation re-opens.
    cache: LruCache<TileId, CachedTile>,

    /// The style generation the frame side currently wants — bumped by
    /// [`Self::set_style`], stamped on every slot, compared against arriving
    /// payloads on native. Stays [`RASTER_STYLE_EPOCH`] for a source that
    /// never restyles.
    style_epoch: u64,

    /// Where a restyle lands for the IO task — the slot [`read_one`] reads a
    /// (style, generation) pair from, once per tile. `None` for a raster HTTP
    /// source, which has no style to swap. On wasm32 the frame pump is the
    /// styling side, so the slot exists only inside the task's ignored
    /// parameter and the live style is [`Self::style`].
    #[cfg(not(target_arch = "wasm32"))]
    style_slot: Option<Arc<std::sync::RwLock<StyleSlot>>>,

    /// The parsed-geometry cache shared with this source's IO task — `Some`
    /// for an archive source, `None` for a raster HTTP source. wasm32 only,
    /// because there the pump is the side that parses and remembers; on
    /// native the IO task owns the only handle, and its clone is what keeps
    /// the cache alive.
    #[cfg(target_arch = "wasm32")]
    parsed: Option<SharedParsedTiles>,

    /// Tiles the IO task should fetch.
    request_tx: Sender<TileId>,
    /// Tiles the IO task has fetched: decoded on native, compressed bytes on
    /// wasm32, where this channel doubles as the decode queue the frame pump
    /// drains under [`WASM_TILE_DECODES_PER_PUMP`].
    tile_rx: Receiver<(TileId, FetchPayload)>,

    /// wasm32 only: the context the frame pump decodes and uploads through,
    /// and asks for the queue-draining repaint on. On native the IO thread
    /// owns the context's clone instead.
    #[cfg(target_arch = "wasm32")]
    egui_ctx: Context,
    /// wasm32 only: what is left of [`WASM_TILE_DECODES_PER_PUMP`] and of
    /// [`PUMP_TIME_BUDGET`] this pass — both bounds, both per pass.
    #[cfg(target_arch = "wasm32")]
    pump_budget: PumpBudget,

    /// wasm32 only: the style [`Tile::new`] renders a vector tile against.
    /// Empty for a raster source, which never reads it; the committed style for
    /// an archive source, where it is the whole appearance of the map. On
    /// native the IO task owns its own clone -- see [`fetch_one`], [`read_one`].
    #[cfg(target_arch = "wasm32")]
    style: Arc<Style>,

    /// wasm32 only: how this source's archive bodies decode — the archive
    /// header's word, written once by the IO task at open. `None` for a
    /// raster HTTP source, whose bodies keep going through [`Tile::new`]'s
    /// sniff. On native the IO task reads the header off the archive it holds
    /// instead — see [`read_one`].
    #[cfg(target_arch = "wasm32")]
    archive_kind: Option<Arc<OnceLock<ArchiveTileKind>>>,

    /// Whether "the tile IO task is gone" has already been said for this
    /// source. See [`drain_up_to`]: the condition is permanent, so the line is
    /// not.
    io_task_gone_reported: bool,

    /// [`Self::pump`] calls since this source was built — see [`Self::pumps`].
    /// Always on, like the app's other ledgers: one `u64` add per layer per
    /// pass, against the tens of [`Tiles::at`] calls the same layer makes.
    pumps: u64,

    /// Completions the drain has taken off the channel — see [`Self::takes`].
    takes: u64,

    /// Cache puts that changed what this source can draw — one per **tile**
    /// landing in the cache, never per pending or failed marker. Per source
    /// because the basemap and the terrain are separate [`HttpsTiles`], each
    /// with its own arrivals; see [`Self::put_generation`], which the
    /// floor-strip content key reads.
    put_generation: u64,

    /// Declared last so it drops last: the channels above must close first, which
    /// is what tells the fetch loop to exit.
    #[expect(dead_code, reason = "owned for its Drop; shuts the IO task down")]
    runtime: runtime::Runtime,
}

/// One source's per-pass pump allowance, wasm32 only: the decodes it may still
/// spend, and the wall-clock deadline those decodes share.
///
/// **Both halves are per pass, and the time half is the one that was not.**
/// [`HttpsTiles::pump`] runs once per layer, and one source is drawn as a layer
/// in every pane that shows it plus the volume floor strip, so a per-*call*
/// bound lets a multi-pane layout bill one pass several times over. The count
/// half has been per-pass since it was written. The time half was computed as
/// `Instant::now() + PUMP_TIME_BUDGET` inside each `drain_completed_fetches`
/// call, so every pump of a pass opened a *fresh* budget and re-armed the
/// unconditional first take with it: a frame's tile cost was
/// `panes x (PUMP_TIME_BUDGET + one take)` per source, not `PUMP_TIME_BUDGET +
/// one take`. With six panes and a take that is a multi-millisecond
/// tessellation that is a twelvefold miss, and the constant's own note — "a
/// frame's worst case is twice this plus two takes" — held only for the
/// single-pane layout it was measured on.
///
/// The pass number is what turns both halves per-pass: the first call of a new
/// pass restores the count and stamps one deadline that every later call in
/// that pass shares. Per *source* because each [`HttpsTiles`] owns one, and the
/// caches they fill are per source too.
///
/// **Fairness needs no arbitration**, because a source is app-level and its
/// cache is shared by every pane that draws it: a take that lands for pane one
/// is resident for panes two through six in the same pass. Bounding the pass
/// rather than the call spends the budget on the *first* asker and serves all
/// of them, which is why this cannot starve a later pane.
///
/// Compiled on native for the tests (`any(test, target_arch = "wasm32")`), as
/// its predecessor was.
#[cfg(any(test, target_arch = "wasm32"))]
struct PumpBudget {
    /// The pass [`Self::spent`] and [`Self::deadline`] belong to.
    pass_nr: u64,
    /// Takes already performed in [`Self::pass_nr`].
    spent: usize,
    /// When [`Self::pass_nr`]'s takes stop being free. Stamped once, by the
    /// pass's first call.
    deadline: web_time::Instant,
}

/// What one pump call may still spend of its pass — see [`PumpBudget::open`].
#[cfg(any(test, target_arch = "wasm32"))]
struct PassAllowance {
    /// Takes still allowed in the pass.
    budget: usize,
    /// The deadline every take of the pass shares.
    deadline: web_time::Instant,
    /// Whether the pass has yet to take anything, and so still owes the
    /// unconditional first take. See [`PUMP_TIME_BUDGET`].
    first_take_free: bool,
}

#[cfg(any(test, target_arch = "wasm32"))]
impl PumpBudget {
    fn new() -> Self {
        Self {
            pass_nr: 0,
            spent: 0,
            // Replaced before it is read: the first `open` of pass 0 is a
            // fresh pass by `spent == 0`, and every later pass by `pass_nr`.
            deadline: web_time::Instant::now(),
        }
    }

    /// Open (or continue) `pass_nr`, returning what is left of it.
    ///
    /// A new pass restores the count and stamps the deadline; a later call in
    /// the same pass gets what is left of both. `first_take_free` is the
    /// pass's, not the call's — the anti-stall guarantee is "a frame always
    /// moves the map forward by one tile per source", and a per-call spelling
    /// of it is the per-call budget wearing the count's clothes.
    fn open(&mut self, pass_nr: u64) -> PassAllowance {
        if pass_nr != self.pass_nr {
            self.pass_nr = pass_nr;
            self.spent = 0;
        }
        if self.spent == 0 {
            // The budget starts when the spending does. A pass whose early
            // layers found an empty queue must not have burnt its deadline on
            // nothing before the layer with work reaches it — that would put
            // the stall back, one pane further along.
            self.deadline = web_time::Instant::now() + PUMP_TIME_BUDGET;
        }
        PassAllowance {
            budget: WASM_TILE_DECODES_PER_PUMP.saturating_sub(self.spent),
            deadline: self.deadline,
            first_take_free: self.spent == 0,
        }
    }

    /// Record `taken` takes performed against the allowance.
    fn record(&mut self, taken: usize) {
        self.spent += taken;
    }
}

/// Move at most `budget` completed fetches out of `rx`, handing each to `take`.
/// Returns how many were taken. Stops early when the channel is empty; a closed
/// channel is the IO task gone, and is logged rather than panicked on, as
/// everywhere else in this module.
///
/// Both arms of [`HttpsTiles::drain_completed_fetches`] are this loop over a
/// different `take` and a different budget — which is the whole platform
/// difference behind [`HttpsTiles::pump`].
///
/// `reported` is the source's own latch and the reason it exists is measured,
/// not defensive. The IO task ends for good when the archive will not open —
/// [`serve_archive_continuously`] logs why and returns — and after that the
/// channel is closed on every subsequent drain. [`HttpsTiles::pump`] runs once
/// per layer per frame, so the unlatched version emitted this line at frame
/// rate: **120 `console.error`s in one 40 s Firefox run against a host that
/// answers `200` to a range request**, measured 2026-08-28. The condition is
/// permanent and the user can do nothing about it, so it is said once.
fn drain_up_to<T>(
    rx: &mut Receiver<T>,
    budget: usize,
    deadline: web_time::Instant,
    first_take_free: bool,
    reported: &mut bool,
    mut take: impl FnMut(T) -> take_ledger::TakeKind,
) -> usize {
    let mut taken = 0;
    while taken < budget {
        // The first take of the **pass** is unconditional, so a frame already
        // over budget still moves the map forward by one tile per source
        // rather than stalling it. `first_take_free` is the pass's answer and
        // not this call's: spelling it `taken > 0` re-armed the exemption once
        // per layer, which is how a six-pane frame paid six of them. See
        // [`PumpBudget`] and [`PUMP_TIME_BUDGET`].
        if !(first_take_free && taken == 0) && web_time::Instant::now() >= deadline {
            break;
        }
        match rx.try_recv() {
            Ok(item) => {
                // The take, bracketed. This span is exactly what
                // [`PUMP_TIME_BUDGET`] cannot see inside — the deadline above
                // is checked between takes and never during one, so a frame
                // pays the budget plus one whole unbounded pass through here.
                // See [`take_ledger`] for the denominator and the families.
                let began = web_time::Instant::now();
                let kind = take(item);
                let cost = web_time::Instant::now().duration_since(began);
                take_ledger::note_take(kind, cost.as_micros().min(u128::from(u32::MAX)) as u32);
                taken += 1;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Closed) => {
                if !*reported {
                    *reported = true;
                    log::error!("the tile IO task is gone; this source will serve no more tiles");
                }
                break;
            }
        }
    }
    taken
}

impl HttpsTiles {
    /// Fetch `source`'s tiles through squallar's platform-verified HTTPS client.
    pub fn new<S: AsyncTileSource>(source: S, egui_ctx: Context) -> Self {
        Self::with_client(source, egui_ctx, tile_client())
    }

    /// [`Self::new`], with the HTTP client supplied. Crate-private, for the
    /// tests, which need to talk cleartext to a loopback server — [`tile_client`]
    /// refuses `http://` by design.
    pub(crate) fn with_client<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        client: reqwest::Client,
    ) -> Self {
        Self::with_client_and_cache(source, egui_ctx, client, TILE_CACHE_ENTRIES)
    }

    /// [`Self::with_client`], with the cache bound supplied. Crate-private, for
    /// the tests: [`TILE_CACHE_ENTRIES`] is cfg-selected and this workspace runs
    /// `cargo test` on the desktop arm only.
    fn with_client_and_cache<S: AsyncTileSource>(
        source: S,
        egui_ctx: Context,
        client: reqwest::Client,
        cache_entries: NonZeroUsize,
    ) -> Self {
        let attribution = source.attribution();
        let tile_size = source.tile_size();
        let max_zoom = Arc::new(AtomicU8::new(source.max_zoom()));

        // Sized to the concurrency limit, as walkers sizes them: a full request
        // channel is the backpressure that makes `request_once` retry later.
        let (request_tx, request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);

        // The frame pump needs the context too on wasm: it is the decoding
        // side there. See `FetchPayload`.
        #[cfg(target_arch = "wasm32")]
        let frame_ctx = egui_ctx.clone();

        let runtime = runtime::spawn(fetch_continuously(
            source, client, request_rx, tile_tx, egui_ctx,
        ));

        Self {
            attribution,
            tile_size,
            max_zoom,
            // A raster source has no single failure that ends it: each tile is
            // its own request, and a 404 on one says nothing about the next.
            // The run of failed reads is the archive loop's own count, and
            // `fetch_continuously` does not keep one, so this stays down for a
            // raster source rather than reading half of a rule.
            fault: Arc::new(OnceLock::new()),
            reads_failing: Arc::new(AtomicBool::new(false)),
            requests_closed: false,
            cache: LruCache::new(cache_entries),
            style_epoch: RASTER_STYLE_EPOCH,
            // A raster source has no style to swap and no parse to keep.
            #[cfg(not(target_arch = "wasm32"))]
            style_slot: None,
            #[cfg(target_arch = "wasm32")]
            parsed: None,
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx: frame_ctx,
            #[cfg(target_arch = "wasm32")]
            pump_budget: PumpBudget::new(),
            // A raster source. The wasm decode path passes this to `Tile::new`,
            // which only reads a style for a tile that is not an image.
            #[cfg(target_arch = "wasm32")]
            style: Arc::new(Style::default()),
            // Not an archive: no header to obey, so the sniff stands.
            #[cfg(target_arch = "wasm32")]
            archive_kind: None,
            io_task_gone_reported: false,
            put_generation: 0,
            pumps: 0,
            takes: 0,
            runtime,
        }
    }

    /// Adopt `style` for every tile this source serves from now on — the
    /// restyle seam a theme flip and a map-detail toggle ride
    /// (`MapTileState::ensure_base_tiles`), replacing the source rebuild that
    /// refetched every visible tile.
    ///
    /// **Nothing is cleared and nothing is blanked.** The generation
    /// ([`CachedTile`]) is bumped instead: every cached tile keeps drawing in
    /// its old style while [`Self::request_once`] re-asks for it, the IO side
    /// answers out of the parsed cache with **zero fetches and zero
    /// re-parses**, and arrivals replace the stale slots one by one. The heavy
    /// half — styling is mostly lyon tessellation — runs where a decode runs:
    /// the IO runtime's blocking pool on native, the frame pump's
    /// [`WASM_TILE_DECODES_PER_PUMP`] budget on wasm32. The frame thread pays
    /// for nothing here but the lock write.
    ///
    /// Meaningful only for an archive source; a raster source has no style
    /// slot and ignores the swap beyond the (harmless) generation bump.
    pub(crate) fn set_style(&mut self, style: Arc<Style>) {
        self.style_epoch += 1;
        self.install_style(style);
    }

    /// The native arm of the [`Self::set_style`] split: publish the pair to
    /// the IO task's slot.
    #[cfg(not(target_arch = "wasm32"))]
    fn install_style(&mut self, style: Arc<Style>) {
        if let Some(slot) = &self.style_slot {
            *slot.write().expect("the style slot is not poisoned") = StyleSlot {
                style,
                epoch: self.style_epoch,
            };
        }
    }

    /// The wasm32 arm: the frame pump is the styling side, so the live style
    /// is this field.
    #[cfg(target_arch = "wasm32")]
    fn install_style(&mut self, style: Arc<Style>) {
        self.style = style;
    }

    /// Move what the IO task has finished into the cache.
    ///
    /// Call this **once per layer, before the layer's tile loop**;
    /// `ui_map_overlays::draw_tile_layer` is the only caller. It is deliberately
    /// not called from [`Tiles::at`], which runs once per grid cell: over a
    /// 1920x1080-**point** canvas at a whole zoom and bias 0 that billed one
    /// drain per cell — 45 measured at zoom 6 over Oklahoma, and up to 54 as the
    /// grid's phase moves — for the one that had anything to move. On wasm32
    /// each of them opened by reading `Context::cumulative_pass_nr`, two
    /// `RwLock` acquisitions over the whole egui `Context`, only to learn the
    /// pass had not changed.
    ///
    /// The platform difference lives in [`Self::drain_completed_fetches`], not
    /// at the call site: on native the IO thread has already decoded and
    /// uploaded, so a take is a cache put; on wasm32 the IO task shares the page
    /// thread, so the PNG decode and the texture upload happen here, under
    /// [`WASM_TILE_DECODES_PER_PUMP`].
    ///
    /// **The one thing this shape forbids:** handing an [`HttpsTiles`] to
    /// `walkers::Map` as `dyn Tiles`. walkers' `draw_tiles`/`flood_fill_tiles`
    /// call `at` and know nothing about this method, so tiles would be requested
    /// and never arrive. Nothing does today — both `Map::new` sites in `ui_map`
    /// pass `None` for tiles and the app draws its own tile layers — and nothing
    /// plans to.
    /// **The pump does not know whether a gesture is running, and must not.**
    /// WO-9e gated this on a gesture latch that read
    /// [`crate::overlay_cache::SETTLE_REPAINT_DELAY`]: the drain took nothing
    /// while map input was live and resumed only 500 ms after it stopped. That
    /// bought a frame budget with half a second of a map that does not change
    /// — the black screen a zoom-out settles into, and the stretched ancestor a
    /// zoom-in sits on, both waited on that clock before a single tile could be
    /// put. What the latch was really dodging is the per-take cost, and
    /// [`PUMP_TIME_BUDGET`] bounds that directly, so the drain now runs on
    /// every frame whether the map is moving or not. Filling the map while the
    /// user moves *is* minimizing data latency.
    pub fn pump(&mut self) {
        self.pumps += 1;
        self.drain_completed_fetches();
    }

    /// [`Self::pump`] calls since this source was built.
    ///
    /// An always-on ledger, for the same reason the overlay and upload ledgers
    /// are: the cost this method's granularity controls is invisible from the
    /// outside otherwise, and `ui_map_overlays`' tests read it to hold the drain
    /// at one per layer rather than one per tile.
    pub fn pumps(&self) -> u64 {
        self.pumps
    }

    /// Completions the drain has taken off the channel since this source was
    /// built — an always-on ledger like [`Self::pumps`]. Each take is the
    /// per-tile cost the gesture latch defers: decode + upload on wasm, the
    /// cache put and its repaint-the-floor generation bump on native.
    pub fn takes(&self) -> u64 {
        self.takes
    }

    /// How many tiles have landed in this source's cache since it was built —
    /// the "did the ground change" input of the floor-strip content key.
    ///
    /// Moves on every put that carries a **tile**: an arrival on either
    /// target's drain, and the seam tests' direct puts. It deliberately does
    /// not move for a pending or failed marker ([`Self::request_once`]'s
    /// `tile: None` slots) or a restyle re-stamp — neither changes any pixel
    /// this source can currently serve, and a key input that moved on an ask
    /// would repaint every floor strip once per requested tile.
    pub(crate) fn put_generation(&self) -> u64 {
        self.put_generation
    }

    /// Native: move at most [`NATIVE_TILE_UPLOADS_PER_PUMP`] already-decoded
    /// tiles into the cache. [`fetch_one`] did the decode and the upload on the
    /// IO thread, so this is a queue move and nothing more.
    #[cfg(not(target_arch = "wasm32"))]
    fn drain_completed_fetches(&mut self) {
        // Split borrow: the closure needs the cache while the receiver is
        // borrowed.
        let Self {
            cache,
            tile_rx,
            io_task_gone_reported,
            style_epoch,
            put_generation,
            takes,
            ..
        } = self;
        let current = *style_epoch;
        let taken = drain_up_to(
            tile_rx,
            NATIVE_TILE_UPLOADS_PER_PUMP,
            web_time::Instant::now() + PUMP_TIME_BUDGET,
            // Per call, and deliberately still per call: the pass-wide budget
            // is [`PumpBudget`], and reaching it here would mean keeping a
            // frame-side `Context` clone this arm does not otherwise have —
            // the IO thread owns the only one — to read `cumulative_pass_nr`
            // under two `RwLock`s once per layer. A native take is an
            // `LruCache::put` costing microseconds, so `PUMP_TIME_BUDGET` is
            // never reached on this arm at all and there is nothing here for
            // a pass-wide spelling of it to bound. See [`PUMP_TIME_BUDGET`].
            true,
            io_task_gone_reported,
            |(tile_id, slot): (TileId, FetchPayload)| {
                // A tile styled under a generation a restyle has replaced is
                // dropped, not drawn: its slot keeps showing the old style and
                // [`Self::request_once`] re-asks under the current one, which
                // the IO side answers from the parsed cache.
                if slot.epoch == current {
                    *put_generation += 1;
                    cache.put(tile_id, slot);
                }
                // The whole native take, whether or not the slot survived its
                // epoch check: a dropped restyle still cost the frame the
                // move off the channel. See [`take_ledger::TakeKind::Put`] —
                // on this arm the decode already happened on the IO thread,
                // which is why one family covers the arm.
                take_ledger::TakeKind::Put
            },
        );
        *takes += taken as u64;
    }

    /// wasm32: decode, upload and cache at most this pass's remaining allowance
    /// of fetched tiles.
    ///
    /// [`PumpBudget`] keeps a burst of completed fetches from billing one pass
    /// for this source's whole backlog. The put is unconditional, exactly as a
    /// native tile arriving is: a `None` marker still means failed, do not ask
    /// again.
    #[cfg(target_arch = "wasm32")]
    fn drain_completed_fetches(&mut self) {
        let Self {
            cache,
            tile_rx,
            egui_ctx,
            pump_budget,
            style,
            archive_kind,
            parsed,
            io_task_gone_reported,
            style_epoch,
            put_generation,
            takes,
            ..
        } = self;
        let style: &Style = style;
        let archive_kind = archive_kind.as_ref();
        let parsed = parsed.as_ref();
        // The pump styles against the frame's current style, so what it puts
        // is current by construction.
        let epoch = *style_epoch;

        let allowance = pump_budget.open(egui_ctx.cumulative_pass_nr());
        let budget = allowance.budget;
        let taken = drain_up_to(
            tile_rx,
            budget,
            allowance.deadline,
            allowance.first_take_free,
            io_task_gone_reported,
            |(tile_id, payload): (TileId, FetchPayload)| {
                // The family this take belongs to, decided before the work
                // rather than after it, so a decode that FAILS is still
                // charged to what it attempted. Classified by the archive
                // header's declared kind and never by the bytes — the rule
                // [`decode_archive_tile`] itself obeys.
                let kind = match (&payload, archive_kind) {
                    (FetchPayload::Parsed(_), _) => take_ledger::TakeKind::Restyle,
                    // A plain HTTP source has no header to declare anything;
                    // its body goes through `Tile::new`'s sniff.
                    (FetchPayload::Bytes(_), None) => take_ledger::TakeKind::Sniffed,
                    (FetchPayload::Bytes(_), Some(declared)) => {
                        match declared
                            .get()
                            .copied()
                            .unwrap_or(ArchiveTileKind::Undeclared)
                        {
                            ArchiveTileKind::Vector => take_ledger::TakeKind::Vector,
                            ArchiveTileKind::Hillshade | ArchiveTileKind::TerrainRgb => {
                                take_ledger::TakeKind::Raster
                            }
                            ArchiveTileKind::Undeclared => take_ledger::TakeKind::Sniffed,
                        }
                    }
                };
                let decoded = match payload {
                    // A restyle served from the parsed cache: no bytes were
                    // fetched, only the styling is owed — still under this
                    // budget, because it is the tessellation half.
                    FetchPayload::Parsed(parse) => Ok(styled_tile(&parse, style, tile_id.zoom)),
                    FetchPayload::Bytes(bytes) => match archive_kind {
                        // A raster HTTP source: the body is whatever image the
                        // provider serves, sniffed as it always was.
                        None => Tile::new(&bytes, style, tile_id.zoom, egui_ctx)
                            .map_err(|error| error.to_string()),
                        // An archive source: the header's word decides. The IO
                        // task writes the slot at open, before any tile is
                        // served, so an empty slot cannot be reached through the
                        // normal order of events; if it ever is, sniffing is the
                        // pre-seam behaviour rather than a guess of this code's.
                        Some(kind) => {
                            let kind = kind.get().copied().unwrap_or(ArchiveTileKind::Undeclared);
                            match parsed {
                                Some(parsed) => decode_archive_tile_remembering(
                                    &bytes, kind, style, tile_id, egui_ctx, parsed,
                                ),
                                None => {
                                    decode_archive_tile(&bytes, kind, style, tile_id.zoom, egui_ctx)
                                }
                            }
                        }
                    },
                };
                match decoded {
                    Ok(tile) => {
                        *put_generation += 1;
                        cache.put(tile_id, slot_for(tile, epoch));
                    }
                    Err(error) => log::warn!("decoding tile {tile_id:?}: {error}"),
                }
                kind
            },
        );
        pump_budget.record(taken);
        *takes += taken as u64;

        if budget > 0 && taken == budget {
            // The whole allowance went, so more completions may be waiting.
            // Ask for a frame so a backlog drains while the user is idle.
            egui_ctx.request_repaint();
        }
    }

    /// Ask for `tile_id` unless it has already been asked for **under the
    /// current style generation**.
    ///
    /// The de-duplication and the cache are the same structure: a slot under
    /// the tile's id stamped with the current generation records "a request is
    /// out" (or "it is here", or "it failed") and reserves the slot. When the
    /// request channel is full nothing is recorded and the tile is retried on
    /// a later frame — recording the ask while dropping the request would
    /// strand it forever.
    ///
    /// A slot from an **older** generation is a tile styled by a style
    /// [`Self::set_style`] has replaced: it keeps drawing (see
    /// [`Self::cached_or_interpolated`]) while this re-asks for it, and the
    /// re-stamp to the current generation is the same "a request is out"
    /// record as a fresh insert.
    fn request_once(&mut self, tile_id: TileId) {
        if self.requests_closed {
            return;
        }
        let epoch = self.style_epoch;

        // Split borrow: the sender is needed while the cache is borrowed.
        let Self {
            cache, request_tx, ..
        } = self;

        // `get`, not `peek`: a hit refreshes recency exactly as the old
        // `try_get_or_insert` did.
        let known = cache.get(&tile_id).map(|slot| slot.epoch);
        if known == Some(epoch) {
            return;
        }

        let outcome = request_tx.try_send(tile_id).map(|()| {
            if known.is_some() {
                // The stale slot keeps its tile; the re-stamp is the "a
                // request is out" record.
                if let Some(slot) = cache.get_mut(&tile_id) {
                    slot.epoch = epoch;
                }
            } else {
                cache.put(
                    tile_id,
                    CachedTile {
                        epoch,
                        tile: None,
                        meshes: None,
                    },
                );
            }
        });

        match outcome {
            Ok(()) => log::trace!("requested tile {tile_id:?}"),
            Err(error) if error.is_full() => {
                log::trace!("tile request queue is full, retrying {tile_id:?} next frame");
            }
            // walkers panics here. The IO task being gone is not worth taking the
            // UI thread down for; the map simply stops fetching -- once, and
            // saying so once. See [`Self::requests_closed`] for why the latch is
            // not optional.
            Err(error) => {
                log::error!(
                    "the tile IO task is gone, so this source stops fetching \
                     (asking for {tile_id:?}: {error})"
                );
                self.requests_closed = true;
            }
        }
    }

    /// The tile itself, or the nearest cached ancestor stretched to fit.
    ///
    /// Starts at the requested zoom and walks outwards until something is cached,
    /// so a zoomed-in view shows a blurry ancestor rather than a hole. Starts no
    /// download of its own.
    ///
    /// **The style generation is deliberately not consulted**: mid-restyle, a
    /// tile styled by the outgoing style is what stands between the user and a
    /// blank beat, and its slot is already re-stamped for replacement by
    /// [`Self::request_once`].
    fn cached_or_interpolated(&mut self, tile_id: TileId) -> Option<GroundPiece> {
        let mut zoom_candidate = tile_id.zoom;

        loop {
            let (ancestor, uv) = interpolate_from_lower_zoom(tile_id, zoom_candidate);

            if let Some(CachedTile {
                tile: Some(cached),
                meshes,
                ..
            }) = self.cache.get(&ancestor)
            {
                break Some(GroundPiece {
                    tile: cached.clone(),
                    uv,
                    meshes: meshes.clone(),
                });
            }

            // Out of ancestors: nothing to draw for this tile yet.
            zoom_candidate = zoom_candidate.checked_sub(1)?;
        }
    }

    /// [`Tiles::at`]'s answer with the tile's flattened fills beside it — what
    /// the map's own tile pass draws through, since it is the only caller and
    /// walkers' trait has no room for the second half.
    pub(crate) fn ground_at(&mut self, tile_id: TileId) -> Option<GroundPiece> {
        if !tile_id_is_valid(tile_id) {
            return None;
        }
        let max_zoom = self.source_max_zoom()?;
        let to_fetch = if tile_id.zoom > max_zoom {
            interpolate_from_lower_zoom(tile_id, max_zoom).0
        } else {
            tile_id
        };
        self.request_once(to_fetch);
        self.cached_or_interpolated(tile_id)
    }

    /// Ask for `tile_id` without drawing it — the ancestor net.
    ///
    /// [`Self::ground_at`] asks only for the level it is drawing, and
    /// [`Self::cached_or_interpolated`] can only answer a miss with something
    /// *shallower* than the tile asked for. Together those two mean a zoom-out
    /// has nothing to draw: the tiles it was just looking at are descendants of
    /// what it now wants, no shallower level was ever requested, and the walk
    /// runs to zoom 0 finding nothing. The pane draws a hole for every cell —
    /// the black screen — until the network answers.
    ///
    /// So the shallow level is requested too, every frame, by
    /// `ui_map_overlays::draw_tile_layer`. It is prediction rather than repair:
    /// the fetch is off-thread on both targets, and by the time a zoom-out
    /// arrives the net is already resident and every cell has an ancestor to
    /// stretch. See [`crate::tiles::WARM_ANCESTOR_STEPS`] for why the net sits
    /// four steps out rather than one.
    ///
    /// Cheap to call every frame by construction: [`Self::request_once`]
    /// returns on an LRU hit at the current style generation, so a warm tile
    /// costs one probe after its first fetch — and the probe is what keeps the
    /// net's recency fresh, which is what stops the LRU evicting the one thing
    /// standing between the user and a hole.
    pub(crate) fn warm(&mut self, tile_id: TileId) {
        if !tile_id_is_valid(tile_id) {
            return;
        }
        // A net deeper than the source goes is not a net; the drawn level is
        // already clamped to this and the net is shallower still, so this only
        // ever fires for a source whose header has not landed.
        if self
            .source_max_zoom()
            .is_none_or(|deepest| tile_id.zoom > deepest)
        {
            return;
        }
        self.request_once(tile_id);
    }

    /// The source's deepest zoom, so a consumer picking its own zoom level
    /// (`ui_map_overlays`' tile pass, when a zoom bias asks for a level deeper
    /// than the pane's own) can clamp to what this source can serve.
    ///
    /// `None` while an archive source is still waiting on its header — see
    /// [`MAX_ZOOM_UNKNOWN`]. A caller that clamps to this must draw and request
    /// **nothing** in that state rather than substituting a number, which is
    /// what makes the difference between waiting a frame and seeding a z0 tile
    /// that becomes the session's fallback ancestor.
    pub fn source_max_zoom(&self) -> Option<u8> {
        match self.max_zoom.load(Ordering::Relaxed) {
            MAX_ZOOM_UNKNOWN => None,
            zoom => Some(zoom),
        }
    }

    /// Why this source will never serve a tile, if that is settled.
    ///
    /// `Some` only after the IO task has failed in a way no later frame can
    /// undo — today, an archive that will not open. A source that is merely
    /// slow, or that answered `404` for one tile, is `None`.
    pub fn fault(&self) -> Option<&str> {
        self.fault.get().map(String::as_str)
    }

    /// Whether this source's tile reads have been failing long enough that it
    /// is not putting the map on the glass — [`SUSTAINED_READ_FAILURES`] of
    /// them in a row with none answering.
    ///
    /// **Read it every frame; never latch it.** Unlike [`Self::fault`] it
    /// clears itself the moment a read answers, which is the whole point: the
    /// conditions that raise it are the ones a later frame can undo.
    pub fn reads_are_failing(&self) -> bool {
        self.reads_failing.load(Ordering::Relaxed)
    }

    /// Report this source's reads as failing, with no transport to fail them.
    ///
    /// For the credit-composition tests, which need the *live-source* degraded
    /// state — the one [`crate::tiles::MapTileState::latch_base_unreachable_for_test`]
    /// cannot produce, because it empties the slot. What those tests prove is
    /// therefore the composition given the state, not the state's own raising;
    /// that is `tile_source::tests::archive`'s.
    #[cfg(test)]
    pub(crate) fn fail_reads_for_test(&self) {
        self.reads_failing.store(true, Ordering::Relaxed);
    }

    /// Tiles currently held, including pending and failed markers. Exposed for
    /// the eviction test; gated off wasm32 with `mod tests`, its only caller.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn cached_entries(&self) -> usize {
        self.cache.len()
    }

    /// Whether `tile_id` currently occupies a slot, pending and failed markers
    /// included. A peek, not a use: `LruCache::contains` leaves the recency
    /// order alone.
    ///
    /// `cfg(test)` alone, NOT the `all(test, not(wasm32))` its neighbour
    /// [`Self::cached_entries`] carries, and for the reason
    /// [`Self::put_for_test`] gives: `ui_map_overlays`' seam tests read it to
    /// hold the ancestor net's request at the call site, and that module's
    /// tests compile on the wasm32 test target too. The body is portable.
    #[cfg(test)]
    pub(crate) fn tile_is_cached(&self, tile_id: TileId) -> bool {
        self.cache.contains(&tile_id)
    }

    /// Put `tile` in the cache under `tile_id`, as an arrived fetch would.
    ///
    /// Test-only, and the reason it exists is isolation rather than
    /// convenience: the seam's dispatch — `Tile::Vector` reaching the painter
    /// — stays testable without an archive open or an IO task running.
    ///
    /// `cfg(test)` alone, NOT the `all(test, not(wasm32))` its neighbours
    /// carry: its caller (`ui_map_overlays`' seam tests) compiles on the
    /// wasm32 test target too, and the body is portable. The narrower gate
    /// was an E0599 on `cargo check -p squallar-egui --all-targets --target
    /// wasm32-unknown-unknown`, found when the flip made that a default row.
    #[cfg(test)]
    pub(crate) fn put_for_test(&mut self, tile_id: TileId, tile: Tile) {
        let epoch = self.style_epoch;
        // The generation moves exactly as it does for a real arrival, so a
        // fixture's put is a tile-arrival staleness event for the strip key.
        self.put_generation += 1;
        // Through the same constructor an arrival goes through, so a fixture
        // carries the flattened fills a real tile carries.
        self.cache.put(tile_id, slot_for(tile, epoch));
    }
}

impl Tiles for HttpsTiles {
    fn attribution(&self) -> Attribution {
        self.attribution.clone()
    }

    /// Return the tile if it is available, and start a download if it is not.
    ///
    /// Called once per grid cell of the layer being drawn, by
    /// `ui_map_overlays::draw_tile_layer` — **not** by walkers' flood fill,
    /// which is unreachable here because both `Map::new` sites in `ui_map` pass
    /// `None` for tiles. Everything here is cheap and non-blocking, and what the
    /// IO task has finished arrives through [`Self::pump`], once for the whole
    /// layer.
    fn at(&mut self, tile_id: TileId) -> Option<TilePiece> {
        // An archive source that has not read its header yet does not know what
        // it can serve. Asking anyway means asking for `0/0/0`, which is a real
        // tile that then never leaves the cache; see [`MAX_ZOOM_UNKNOWN`]. The
        // IO task repaints when the header lands, so this waits one frame.
        // Above the source's deepest zoom there is nothing to download; the
        // ancestor at `max_zoom` is what gets stretched over the gap. Both
        // rules live in [`Self::ground_at`], which this narrows.
        let piece = self.ground_at(tile_id)?;
        Some(TilePiece::new(piece.tile, piece.uv))
    }

    fn tile_size(&self) -> u32 {
        self.tile_size
    }
}

// ---------------------------------------------------------------------------
// The IO task
// ---------------------------------------------------------------------------

/// Download one tile — and on native, decode and upload it too.
///
/// On native, decoding happens here on the IO runtime, as walkers does it:
/// [`Tile::new`] performs the PNG decode and the
/// [`egui::Context::load_texture`] call, and `Context` is `Send + Sync` and
/// locks internally. On wasm32 the IO "runtime" *is* the UI thread, so the bytes
/// are handed over undecoded — see [`FetchPayload`].
///
/// The error is a `String` because walkers' `TileError` is not exported.
async fn fetch_one<S: TileSource>(
    source: &S,
    client: &reqwest::Client,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            unused_variables,
            reason = "on wasm the frame pump decodes; see FetchPayload"
        )
    )]
    egui_ctx: &Context,
    tile_id: TileId,
) -> Result<(TileId, FetchPayload), String> {
    let url = source.tile_url(tile_id);
    log::trace!("downloading '{url}'");

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|error| format!("requesting '{url}': {error}"))?
        .error_for_status()
        .map_err(|error| format!("requesting '{url}': {error}"))?;

    let body = response
        .bytes()
        .await
        .map_err(|error| format!("reading '{url}': {error}"))?;

    // An empty style. This function serves a raster source, and `Tile::new`
    // only consults a style for a body that is not a recognised image. The
    // epoch is likewise the raster constant: this source never restyles.
    #[cfg(not(target_arch = "wasm32"))]
    let payload = slot_for(
        Tile::new(&body, &Style::default(), tile_id.zoom, egui_ctx)
            .map_err(|error| format!("decoding '{url}': {error}"))?,
        RASTER_STYLE_EPOCH,
    );

    // On wasm the decode belongs to the frame pump, under its budget.
    #[cfg(target_arch = "wasm32")]
    let payload = FetchPayload::Bytes(body.to_vec());

    Ok((tile_id, payload))
}

/// Serve tile requests until [`HttpsTiles`] is dropped.
///
/// Three states, mirroring walkers' `fetch_continuously_impl`:
///
/// * nothing in flight — only a new request can make progress;
/// * under the concurrency limit — a new request *or* a completion;
/// * at the limit — only a completion, which is what enforces the limit.
///
/// Either channel closing ends the loop, which is how the task learns that
/// [`HttpsTiles`] is gone.
async fn fetch_continuously<S: TileSource>(
    source: S,
    client: reqwest::Client,
    mut request_rx: Receiver<TileId>,
    mut tile_tx: Sender<(TileId, FetchPayload)>,
    egui_ctx: Context,
) {
    let mut outstanding = FuturesUnordered::new();

    loop {
        let completed = if outstanding.is_empty() {
            match request_rx.next().await {
                Some(tile_id) => {
                    outstanding.push(fetch_one(&source, &client, &egui_ctx, tile_id));
                    continue;
                }
                None => break,
            }
        } else if outstanding.len() < MAX_PARALLEL_DOWNLOADS {
            match select(request_rx.next(), outstanding.next()).await {
                Either::Left((Some(tile_id), pending)) => {
                    // Release the borrow of `outstanding` before pushing. Dropping
                    // a `Next` does not cancel the futures inside it.
                    drop(pending);
                    outstanding.push(fetch_one(&source, &client, &egui_ctx, tile_id));
                    continue;
                }
                Either::Left((None, _)) => break,
                Either::Right((completed, _)) => completed,
            }
        } else {
            outstanding.next().await
        };

        // `outstanding` was non-empty on both paths that reach here, so this is
        // always `Some`; treating it otherwise would just spin.
        let Some(result) = completed else { continue };

        match result {
            Ok(fetched) => {
                if tile_tx.send(fetched).await.is_err() {
                    break;
                }
                // Without this the tile sits in the channel until some unrelated
                // input wakes the UI, and the map appears to stop loading.
                egui_ctx.request_repaint();
            }
            Err(error) => log::warn!("{error}"),
        }
    }

    log::debug!("tile fetch loop finished");
}

// ---------------------------------------------------------------------------
// The archive source
// ---------------------------------------------------------------------------

/// Tiles served out of a self-hosted PMTiles v3 archive rather than one HTTP
/// GET per tile.
///
/// Everything downstream of the fetch is the raster path unchanged: the same
/// LRU, the same de-duplication, the same interpolation from a shallower
/// ancestor, the same [`HttpsTiles::pump`] contract. Only where the bytes come
/// from, and what they decode into, differ.
impl HttpsTiles {
    /// Serve tiles from the PMTiles archive at `url`, rendered against `style`.
    ///
    /// **The archive is opened by the IO task, not here.** Reading the header
    /// and the root directory is two range requests over the network, and this
    /// is called from a frame.
    ///
    /// So [`Self::source_max_zoom`] answers `None` until the header lands, and
    /// [`Tiles::at`] draws and requests nothing while it does — see
    /// [`MAX_ZOOM_UNKNOWN`]. **It used to answer `0`, and that was a defect
    /// dressed as caution**: `at` clamps a deeper request down to the maximum,
    /// so every pane's first frames asked for `0/0/0`, that tile stayed in the
    /// LRU, and from then on it was the fallback ancestor
    /// [`Self::cached_or_interpolated`] walked to for any deep tile in flight —
    /// drawn stretched over the whole viewport. The doc that argued for it
    /// reasoned about rasters ("a real tile drawn stretched"), and a stretched
    /// raster is blurry while a stretched vector tile has its stroke widths
    /// stretched too. `MAX_ZOOM_UNKNOWN` records the header's absence for what
    /// it is; the IO task repaints the moment it lands, so this costs a frame.
    ///
    /// # Errors
    ///
    /// [`crate::basemap_archive::RangeError`] if `url` will not parse. A URL
    /// that parses but does not answer is a *runtime* failure of the IO task;
    /// it cannot be reported here, because nothing has been asked for yet, so
    /// it is recorded on [`Self::fault`] instead of only in the log.
    /// `cache` is the persistent block cache's configuration, decided by the
    /// caller because deriving it takes *every* archive URL the build reads
    /// (the GC's live-generation set, from `tiles::live_archive_urls`) —
    /// `None` reads uncached, exactly as before the cache existed. The basemap source is also the one that seeds: the z0–z5
    /// warm-up runs through this source's cache, once per generation.
    pub fn from_archive_url(
        url: &str,
        style: Arc<Style>,
        attribution: Attribution,
        egui_ctx: Context,
        cache: Option<crate::basemap_archive::block_cache::BlockCacheConfig>,
        offline: Option<crate::basemap_download::PlatformSegmentStore>,
    ) -> Result<Self, crate::basemap_archive::RangeError> {
        use crate::basemap_archive::{HttpRangeSource, archive_client, block_cache};

        let source = block_cache::BlockCachedSource::new(
            HttpRangeSource::new(archive_client(), url)?,
            cache.clone(),
        );
        Ok(Self::from_range_source(
            source,
            style,
            attribution,
            egui_ctx,
            TILE_CACHE_ENTRIES,
            // The header says `tile_type = 1` and that is the whole truth
            // about a vector archive; nothing here knows better.
            None,
            ArchiveStores {
                seed: cache,
                offline,
                offline_archive: crate::basemap_download::AreaArchive::Basemap,
            },
        ))
    }

    /// Serve the terrain hillshade archive at `url`.
    ///
    /// [`Self::from_archive_url`]'s twin with terrain's facts: an empty style
    /// (the bodies are raster; nothing tessellates, and the raster decode
    /// never reads a style) and [`TERRAIN_TILE_CACHE_ENTRIES`] as the bound.
    /// The hillshade remap is **not** chosen here: the archive's own header
    /// (`tile_type = 4`) routes its bodies through it — see
    /// [`ArchiveTileKind`].
    ///
    /// # Errors
    ///
    /// As [`Self::from_archive_url`]: only a URL that will not parse fails at
    /// construction; everything later lands on [`Self::fault`].
    /// `cache` as on [`Self::from_archive_url`]. The terrain source shares
    /// the cache root (its generation directory sits beside the basemap's)
    /// but never seeds: the seed is the basemap's z0–z5, by design.
    pub fn from_terrain_archive_url(
        url: &str,
        attribution: Attribution,
        egui_ctx: Context,
        cache: Option<crate::basemap_archive::block_cache::BlockCacheConfig>,
        offline: Option<crate::basemap_download::PlatformSegmentStore>,
    ) -> Result<Self, crate::basemap_archive::RangeError> {
        use crate::basemap_archive::{HttpRangeSource, archive_client, block_cache};

        let source = block_cache::BlockCachedSource::new(
            HttpRangeSource::new(archive_client(), url)?,
            cache,
        );
        Ok(Self::from_range_source(
            source,
            Arc::new(Style::default()),
            attribution,
            egui_ctx,
            TERRAIN_TILE_CACHE_ENTRIES,
            // The hillshade is the app's only *pictorial* raster archive, so
            // the header's `tile_type = 4` already routes it correctly. A
            // declaration here would claim a fact the header carries.
            None,
            ArchiveStores {
                // No seed: the warm-up is the basemap's shallow zooms, not
                // the hillshade's.
                seed: None,
                offline,
                offline_archive: crate::basemap_download::AreaArchive::Terrain,
            },
        ))
    }

    /// [`Self::from_archive_url`] with the range source and the cache bound
    /// supplied. Crate-private, for the tests, which read the committed Monaco
    /// fixture off disk rather than over a network.
    ///
    /// The two stores travel as one [`ArchiveStores`] rather than as three
    /// arguments, because they mean one thing — what this source consults
    /// before the network — and the archive kind is meaningless apart from the
    /// store it filters.
    ///
    /// `declared_kind` is what the caller claims the bodies are, for the one
    /// case the header cannot say: `Some(ArchiveTileKind::TerrainRgb)` over a
    /// PNG archive. It is verified against the header at open and is not a
    /// pre-set answer — see [`ArchiveSlots::declared`]. `None` leaves
    /// the header's word standing, which is what both published archives use.
    pub(crate) fn from_range_source<S, St>(
        source: S,
        style: Arc<Style>,
        attribution: Attribution,
        egui_ctx: Context,
        cache_entries: NonZeroUsize,
        declared_kind: Option<ArchiveTileKind>,
        stores: ArchiveStores<St>,
    ) -> Self
    where
        S: crate::basemap_archive::ArchiveRangeSource + 'static,
        St: crate::basemap_download::OfflineSegments,
    {
        let (request_tx, request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);

        let max_zoom = Arc::new(AtomicU8::new(MAX_ZOOM_UNKNOWN));
        let fault: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
        let reads_failing = Arc::new(AtomicBool::new(false));
        // How this archive's bodies decode -- filled in by the IO task from
        // the header, at open, before it serves a tile. See [`ArchiveTileKind`].
        let archive_kind: Arc<OnceLock<ArchiveTileKind>> = Arc::new(OnceLock::new());

        // The parsed-geometry cache the IO task and the frame side share, and
        // the slot a restyle publishes the new style through. See
        // [`SharedParsedTiles`] and [`HttpsTiles::set_style`].
        let parsed: SharedParsedTiles =
            Arc::new(Mutex::new(LruCache::new(PARSED_TILE_CACHE_ENTRIES)));
        #[cfg(not(target_arch = "wasm32"))]
        let style_slot: IoStyleSlot = Arc::new(std::sync::RwLock::new(StyleSlot {
            style: Arc::clone(&style),
            epoch: RASTER_STYLE_EPOCH,
        }));
        #[cfg(target_arch = "wasm32")]
        let style_slot: IoStyleSlot = ();

        // Both clones exist for the reason `with_client_and_cache` clones the
        // context: on wasm32 the frame pump is the tessellating side, so it
        // needs the context to upload through and the style to render against.
        // The IO task is handed its own pair either way -- `read_one` takes
        // both and ignores them on the target that does not decode there.
        #[cfg(target_arch = "wasm32")]
        let frame_ctx = egui_ctx.clone();
        #[cfg(target_arch = "wasm32")]
        let frame_style = Arc::clone(&style);
        #[cfg(target_arch = "wasm32")]
        let frame_archive_kind = Arc::clone(&archive_kind);
        #[cfg(target_arch = "wasm32")]
        let frame_parsed = Arc::clone(&parsed);
        #[cfg(not(target_arch = "wasm32"))]
        let frame_style_slot = Arc::clone(&style_slot);

        let runtime = runtime::spawn(serve_archive_continuously(
            source,
            ArchiveStyling {
                style: style_slot,
                parsed_tiles: parsed,
            },
            ArchiveSlots {
                max_zoom: Arc::clone(&max_zoom),
                fault: Arc::clone(&fault),
                reads_failing: Arc::clone(&reads_failing),
                kind: archive_kind,
                declared: declared_kind,
            },
            request_rx,
            tile_tx,
            egui_ctx,
            stores,
        ));

        Self {
            attribution,
            // The side a vector tile's extent is mapped onto. 256 is what
            // `crate::tiles::TILE_SIDE_POINTS` places a tile at, and the two
            // must agree: `walkers::mercator::tile_id` folds a larger declared
            // size into the zoom, which would ask the archive for a level
            // shallower than the one being drawn.
            tile_size: crate::tiles::TILE_SIDE_POINTS as u32,
            max_zoom,
            fault,
            reads_failing,
            requests_closed: false,
            cache: LruCache::new(cache_entries),
            style_epoch: RASTER_STYLE_EPOCH,
            #[cfg(not(target_arch = "wasm32"))]
            style_slot: Some(frame_style_slot),
            #[cfg(target_arch = "wasm32")]
            parsed: Some(frame_parsed),
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx: frame_ctx,
            #[cfg(target_arch = "wasm32")]
            pump_budget: PumpBudget::new(),
            // NOT the raster path's `Style::default()`: this is the committed
            // style, and it is what `Tile::new` renders the MVT body against.
            // An empty one would hand back a blank tile for every road and
            // every label.
            #[cfg(target_arch = "wasm32")]
            style: frame_style,
            #[cfg(target_arch = "wasm32")]
            archive_kind: Some(frame_archive_kind),
            io_task_gone_reported: false,
            put_generation: 0,
            pumps: 0,
            takes: 0,
            runtime,
        }
    }

    /// A source that will never fetch: the shape of a just-built source —
    /// its credit line carried, its header unread, its cache empty — held
    /// forever, because no IO task exists behind it.
    ///
    /// **For [`crate::tiles::MapTileState::go_offline_for_tests`], and
    /// nothing else.** A live source races the frames of whatever built it:
    /// tiles that arrive paint label `TextShape`s at arbitrary positions,
    /// and a transport fault latches the unreachable credit — either way a
    /// unit test's glass changes with how much wall-clock time the test
    /// took. Inert, the glass is the fast-test glass on every run: ground
    /// pending, credit the provider's own, no thread, no socket.
    ///
    /// The far channel ends are dropped, so the first tile request trips the
    /// once-only "tile IO task is gone" latch instead of queueing — the same
    /// quiet the frame side already keeps when a real IO task has exited.
    pub(crate) fn inert(attribution: Attribution, egui_ctx: Context) -> Self {
        let (request_tx, _request_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let (_tile_tx, tile_rx) = channel(MAX_PARALLEL_DOWNLOADS);
        let _ = &egui_ctx;

        Self {
            attribution,
            tile_size: crate::tiles::TILE_SIDE_POINTS as u32,
            max_zoom: Arc::new(AtomicU8::new(MAX_ZOOM_UNKNOWN)),
            fault: Arc::new(OnceLock::new()),
            reads_failing: Arc::new(AtomicBool::new(false)),
            requests_closed: false,
            cache: LruCache::new(TILE_CACHE_ENTRIES),
            // An inert source never restyles and never parses -- the same
            // never-restyles spelling as a raster source, which is what the
            // epoch constant's doc names for this case.
            style_epoch: RASTER_STYLE_EPOCH,
            #[cfg(not(target_arch = "wasm32"))]
            style_slot: None,
            #[cfg(target_arch = "wasm32")]
            parsed: None,
            request_tx,
            tile_rx,
            #[cfg(target_arch = "wasm32")]
            egui_ctx,
            #[cfg(target_arch = "wasm32")]
            pump_budget: PumpBudget::new(),
            #[cfg(target_arch = "wasm32")]
            style: Arc::new(Style::default()),
            #[cfg(target_arch = "wasm32")]
            archive_kind: None,
            io_task_gone_reported: false,
            put_generation: 0,
            pumps: 0,
            takes: 0,
            runtime: runtime::inert(),
        }
    }
}

/// The two halves of the restyle seam, as the archive IO task carries them:
/// what a tile is styled against ([`IoStyleSlot`]; nothing on wasm32) and
/// where its parse is remembered. One parameter because every [`read_one`]
/// needs both together.
struct ArchiveStyling {
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            dead_code,
            reason = "the wasm arm of IoStyleSlot is (); the frame pump owns the style"
        )
    )]
    style: IoStyleSlot,
    parsed_tiles: SharedParsedTiles,
}

/// What this device already holds of the archive, and where.
///
/// One parameter rather than two because they are the same kind of fact —
/// bytes on the device rather than on the wire — and because the archive loop
/// does the same thing with both: consults them before the network, once, at
/// open. The block cache is a *tier* under the reads; a downloaded area is a
/// *source* in front of them.
pub(crate) struct ArchiveStores<St> {
    /// The persistent block cache to seed with the shallow zooms, or `None`
    /// for a source that must not seed (terrain) or a target with no
    /// filesystem.
    pub(crate) seed: Option<crate::basemap_archive::block_cache::BlockCacheConfig>,
    /// The downloaded offline areas to put in front of the live archive, or
    /// `None` when this device holds none.
    pub(crate) offline: Option<St>,
    /// Which archive's segments this source composes. A store holds an area's
    /// base map and its hillshade side by side, and each reader takes only its
    /// own: a segment of the other kind would be refused on its tile type
    /// anyway, but only after an open, which is a read per segment per launch
    /// to learn what its name already says.
    pub(crate) offline_archive: crate::basemap_download::AreaArchive,
}

/// What the IO task publishes to the frame side — created by
/// [`HttpsTiles::from_range_source`], filled in by
/// [`serve_archive_continuously`]. One parameter rather than four, because
/// they are the same kind of fact: what the frame may ask this source for, and
/// whether it is answering.
///
/// All but one are written exactly once, at open. [`Self::reads_failing`] is
/// the exception and says so on itself: it is a *condition*, raised and
/// dropped by the serve loop for as long as the source lives.
struct ArchiveSlots {
    max_zoom: Arc<AtomicU8>,
    fault: Arc<OnceLock<String>>,
    /// [`HttpsTiles::reads_failing`]'s far end — the only slot here the serve
    /// loop writes more than once, and the only one that can go back down.
    reads_failing: Arc<AtomicBool>,
    kind: Arc<OnceLock<ArchiveTileKind>>,
    /// What the *caller* says this archive holds, when the header cannot say
    /// it — the only way [`ArchiveTileKind::TerrainRgb`] is ever reached.
    ///
    /// **A declaration, not a pre-set `OnceLock`.** `kind` is still written
    /// once by the IO task at open, and writing it is what
    /// [`serve_archive_continuously`] does *after* cross-checking this against
    /// the header. Pre-setting `kind` from the constructor would make the
    /// declaration unfalsifiable: a `TerrainRgb` sitting over an MVT archive
    /// would decode nothing and report no fault.
    declared: Option<ArchiveTileKind>,
}

impl ArchiveTileKind {
    /// The header's `tile_type`, as a decode decision. See the enum's own
    /// docs for the arms; the mapping is total so a new pmtiles `TileType`
    /// variant is a compile error here rather than a silent sniff.
    fn from_tile_type(tile_type: pmtiles::TileType) -> Self {
        use pmtiles::TileType;
        match tile_type {
            TileType::Mvt | TileType::Mlt => Self::Vector,
            TileType::Png | TileType::Jpeg | TileType::Webp | TileType::Avif => Self::Hillshade,
            TileType::Unknown => Self::Undeclared,
        }
    }

    /// Whether a caller may declare an archive whose header says `tile_type`
    /// to be this kind.
    ///
    /// [`Self::TerrainRgb`] is the whole reason declarations exist, and it is
    /// the only kind the header cannot confirm: `tile_type = 2` is PNG for a
    /// hillshade and for an elevation grid alike, so all this can check is
    /// that the bodies are PNG at all. Every other kind is checkable exactly,
    /// so it is checked exactly — a declaration nobody could falsify would be
    /// a claim rather than a mechanism.
    fn accepts_tile_type(self, tile_type: pmtiles::TileType) -> bool {
        match self {
            Self::TerrainRgb => tile_type == pmtiles::TileType::Png,
            exact => exact == Self::from_tile_type(tile_type),
        }
    }
}

/// Open the archive, then answer tile requests out of it until [`HttpsTiles`]
/// is dropped.
///
/// The same three-state loop as [`fetch_continuously`], for the same reason: a
/// bounded number of reads in flight is what keeps a pan from opening one range
/// request per visible tile at once.
///
/// **Where the tessellation happens is the target's, not this loop's**, and on
/// neither target is it this task. A vector body's per-tile cost is
/// `mvt::parse` plus `mvt::styled` ([`decode_archive_tile_remembering`]), the
/// parse remembered in [`SharedParsedTiles`] so a restyle re-runs only the
/// second half. On native [`read_one`] hands the work to the runtime's
/// blocking pool and the frame side only moves a finished `Tile` into the
/// cache; on wasm32 there is no other thread to pay it on, so the body crosses
/// undecoded and [`HttpsTiles::drain_completed_fetches`] pays it under
/// [`WASM_TILE_DECODES_PER_PUMP`]. See [`FetchPayload`].
///
/// The native render used to run inline here, and "on the IO thread" undersold
/// what that cost: this task is a *current-thread* runtime on one `std::thread`
/// ([`runtime::spawn`]), so a 24.8 ms `mvt::render` was 24.8 ms in which no
/// range request could progress either. `MAX_PARALLEL_DOWNLOADS` bounds the
/// reads and bounded nothing about the tessellations, so filling a fresh
/// 54-tile viewport was ~1.34 s of CPU serialized behind itself while the
/// fetches and the tessellations starved each other.
async fn serve_archive_continuously<S, St>(
    source: S,
    styling: ArchiveStyling,
    slots: ArchiveSlots,
    mut request_rx: Receiver<TileId>,
    mut tile_tx: Sender<(TileId, FetchPayload)>,
    egui_ctx: Context,
    stores: ArchiveStores<St>,
) where
    S: crate::basemap_archive::ArchiveRangeSource,
    St: crate::basemap_download::OfflineSegments,
{
    use crate::basemap_archive::BasemapArchives;

    let mut archive = match BasemapArchives::open(source).await {
        Ok(archive) => archive,
        Err(error) => {
            // **Recorded, not only logged.** Returning here drops `request_rx`,
            // and a dropped receiver is what the frame side sees as a
            // *disconnected* sender -- which reports `is_full() == false` and so
            // used to fall through to one `error!` per visible tile per frame,
            // for as long as the app was open, over a map that stayed blank with
            // nothing on the glass to say why. `HttpsTiles::requests_closed`
            // bounds the flood; this is what lets the UI act on it.
            let reason = error.to_string();
            log::error!("the basemap archive will not open, so it serves no tiles: {reason}");
            let _ = slots.fault.set(reason);
            // Nothing else would wake the UI to notice.
            egui_ctx.request_repaint();
            return;
        }
    };

    // The declaration, cross-checked against the header before anything is
    // served. A declared kind the header contradicts is recorded and returned
    // from exactly as an archive that would not open is: the composition is
    // wrong about what it is reading, and serving bodies to the wrong decoder
    // would put noise on the glass instead of saying so.
    let kind = match slots.declared {
        Some(declared) if !declared.accepts_tile_type(archive.tile_type()) => {
            let reason = format!(
                "the archive was opened as {declared:?}, but its header says \
                 tile_type {:?}",
                archive.tile_type()
            );
            log::error!("the basemap archive is not what it was declared to be: {reason}");
            let _ = slots.fault.set(reason);
            egui_ctx.request_repaint();
            return;
        }
        // The caller knows something the header cannot carry.
        Some(declared) => declared,
        // Nobody claimed anything, so the header's word stands.
        None => ArchiveTileKind::from_tile_type(archive.tile_type()),
    };

    slots.max_zoom.store(archive.max_zoom(), Ordering::Relaxed);
    // What the bodies are, published before the first tile is served: on
    // wasm32 the frame pump decodes, and this is how it knows which decoder
    // the archive calls for.
    let _ = slots.kind.set(kind);
    log::info!(
        "basemap archive open: zooms {}-{}, tiles {:?}, compression {:?}",
        archive.min_zoom(),
        archive.max_zoom(),
        archive.tile_type(),
        archive.tile_compression()
    );
    // The header changed what `at` may ask for, and nothing else would wake the
    // UI to ask for it.
    egui_ctx.request_repaint();

    // Downloaded areas go in front of the live archive **after** the header
    // is published and the repaint asked for, so opening them can never delay
    // first paint. Each is local storage, not the network; a store that will
    // not list, and a segment that will not open, are logged inside and leave
    // the composition serving from the live archive alone.
    if let Some(store) = stores.offline {
        for (label, source) in store.open_all(stores.offline_archive).await {
            archive.attach_offline(label, source).await;
        }
        if archive.offline_count() > 0 {
            // The ceiling can only have risen, and `Tiles::at` reads it.
            slots.max_zoom.store(archive.max_zoom(), Ordering::Relaxed);
            log::info!(
                "{} downloaded basemap segments serve before the network",
                archive.offline_count()
            );
        }
    }

    // In an `Arc` so the seed below can hold the archive while this loop
    // serves from it; costs nothing when there is no seed.
    let archive = Arc::new(archive);

    // Warm the block cache with the shallow zooms, in the background on this
    // same runtime — after the header is published and the repaint is asked
    // for, so the seed can never delay first paint. A no-op when `seed` is
    // `None` (terrain, no cache dir) and on wasm32 (a cfg-selected body).
    // **The live archive's**: the seed is what fills the persistent block
    // cache for the remote generation, and a downloaded segment is already
    // resident by definition.
    crate::basemap_archive::block_cache::maybe_seed(archive.live(), stores.seed);

    let mut outstanding = FuturesUnordered::new();
    let mut failures = ReadFailureRun::new(slots.reads_failing);

    loop {
        let completed = if outstanding.is_empty() {
            match request_rx.next().await {
                Some(tile_id) => {
                    outstanding.push(read_one(&archive, &styling, &egui_ctx, kind, tile_id));
                    continue;
                }
                None => break,
            }
        } else if outstanding.len() < MAX_PARALLEL_DOWNLOADS {
            match select(request_rx.next(), outstanding.next()).await {
                Either::Left((Some(tile_id), pending)) => {
                    // Release the borrow of `outstanding` before pushing.
                    drop(pending);
                    outstanding.push(read_one(&archive, &styling, &egui_ctx, kind, tile_id));
                    continue;
                }
                Either::Left((None, _)) => break,
                Either::Right((completed, _)) => completed,
            }
        } else {
            outstanding.next().await
        };

        let Some(result) = completed else { continue };

        match result {
            // The archive positively holds no tile there. Not an error and not
            // a retry: the `None` the cache already carries under this id is
            // the right answer forever. It is also the archive *answering*,
            // which is why it ends a failure run — a viewport of open ocean
            // must not read as an archive that is not there.
            Ok(None) => {
                if failures.answered() {
                    log::info!("{RECOVERED}");
                    // An absent tile has nothing to arrive, so without this the
                    // recovered credit would wait for whatever asked next.
                    egui_ctx.request_repaint();
                }
            }
            Ok(Some(fetched)) => {
                if failures.answered() {
                    log::info!("{RECOVERED}");
                }
                if tile_tx.send(fetched).await.is_err() {
                    break;
                }
                egui_ctx.request_repaint();
            }
            Err(error) => {
                log::warn!("{error}");
                if failures.failed() {
                    log::error!(
                        "{} tile reads in a row failed with none answering, so this archive is \
                         drawing nothing; the credit corner says so until one answers",
                        failures.len(),
                    );
                    // Nothing else would wake the UI to repaint the credit: a
                    // failed read has no tile to arrive.
                    egui_ctx.request_repaint();
                }
            }
        }
    }

    log::debug!("archive tile loop finished");
}

/// The unbroken run of failed tile reads, and the verdict it publishes to the
/// frame side through [`HttpsTiles::reads_failing`].
///
/// **A type rather than three statements inside the serve loop**, because the
/// rule is the whole of what keeps a map drawing nothing from being credited
/// to a provider, and a rule spelled inline can only ever be exercised through
/// a real archive on a real clock — which is the one shape that cannot pin a
/// threshold: a run that raises the verdict and a read that clears it can both
/// land between two frames, so an end-to-end observation misses an over-eager
/// rule instead of reddening on it.
///
/// Recovery lives here too, and it is [`Self::answered`] and nothing else: no
/// session-lifetime latch stands between a read that answers and the credit
/// coming back. **What supplies that read is the frame side, not this loop.**
/// A tile whose read failed keeps its cache slot and is never asked for again
/// (see [`HttpsTiles::cache`]), so the reads that clear the verdict come from
/// ids the source has not been asked for — a pan, a zoom, an eviction. That is
/// not a gap: while nothing re-asks, nothing draws either, so the notice stays
/// true for exactly as long as the map stays blank.
struct ReadFailureRun {
    run: usize,
    failing: Arc<AtomicBool>,
}

impl ReadFailureRun {
    fn new(failing: Arc<AtomicBool>) -> Self {
        Self { run: 0, failing }
    }

    /// A read answered — with a body, or with the archive's authoritative "no
    /// tile at this coordinate". Answers whether that *changed* the verdict,
    /// so the caller says the recovery once per run rather than once per tile.
    fn answered(&mut self) -> bool {
        self.run = 0;
        self.failing.swap(false, Ordering::Relaxed)
    }

    /// A read failed. Answers whether that changed the verdict, which is at
    /// most once per run for the same reason.
    fn failed(&mut self) -> bool {
        self.run += 1;
        self.run >= SUSTAINED_READ_FAILURES && !self.failing.swap(true, Ordering::Relaxed)
    }

    /// How long the current run is, for the line that reports it.
    fn len(&self) -> usize {
        self.run
    }
}

/// Read one tile out of the archive -- and on native, tessellate and upload it
/// too. Or skip the archive entirely: a tile whose parse is already in
/// [`SharedParsedTiles`] is re-styled from it, which is the whole of what a
/// theme flip or a detail toggle costs since [`HttpsTiles::set_style`].
///
/// `kind` is decided once, at open, by [`serve_archive_continuously`] —
/// **not re-derived here**. On native this function is the decoding side, so a
/// second derivation from the header would silently overrule a declaration and
/// hand a terrain-RGB body to the hillshade decoder; see [`ArchiveTileKind`].
///
/// `Ok(None)` is the archive positively holding nothing at that coordinate --
/// an ocean tile at zoom 14 -- which is why
/// [`crate::basemap_archive::TileBytes`] is a type rather than an empty `Vec`.
///
/// The payload split is [`fetch_one`]'s, for [`FetchPayload`]'s reason and not
/// a second one: on native the tessellation happens here, on wasm32 the IO task
/// *is* the page thread, so the MVT body (or the remembered parse) crosses the
/// channel unstyled and [`HttpsTiles::drain_completed_fetches`] renders it
/// under [`WASM_TILE_DECODES_PER_PUMP`]. Doing it here on wasm32 would put an
/// unbounded tessellation on the frame thread; doing it there puts a bounded
/// one.
///
/// **On native the render runs on the runtime's blocking pool, not on the
/// calling task**, measured at **24.8 ms** for the committed Monaco fixture's
/// z14 city-core tile against the 95-layer committed style, release build,
/// 2026-08-28 (parse and styling fused; the split does the same work in two
/// named halves). There is no await point across it, so before this
/// [`MAX_PARALLEL_DOWNLOADS`] bounded the range requests and bounded nothing
/// about the tessellations: they ran one after another on [`runtime::spawn`]'s
/// single current-thread runtime, and during each 24.8 ms slice no range
/// request progressed either. A fresh 54-tile viewport of tiles this dense was
/// ~1.34 s of CPU serialized behind itself. `spawn_blocking` puts each render
/// on its own pool thread; `outstanding` still holds the concurrency at
/// [`MAX_PARALLEL_DOWNLOADS`], so this is bounded parallelism rather than an
/// unbounded fan-out, and the reads and the renders stop starving each other.
///
/// **Unverified, and stated so it is not a surprise later**: `Runtime::drop`
/// joins the IO thread, whose `tokio::runtime::Runtime` drop is documented to
/// wait for blocking tasks that have already started. If that holds, dropping a
/// source mid-tessellation blocks the *frame* thread for up to one render --
/// ~25 ms. A theme flip no longer drops a source (`set_style` re-styles it in
/// place), so the moments left that do are a layer release, a suspend and a
/// graphics reset. The inline spelling had a wait of the same order for the
/// same reason, so this is not believed to be a regression; neither figure has
/// been measured. The wasm32 arm never reaches `spawn_blocking` at all.
async fn read_one<S, O>(
    archive: &crate::basemap_archive::BasemapArchives<S, O>,
    styling: &ArchiveStyling,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            unused_variables,
            reason = "on wasm the frame pump tessellates; see FetchPayload"
        )
    )]
    egui_ctx: &Context,
    kind: ArchiveTileKind,
    tile_id: TileId,
) -> Result<Option<(TileId, FetchPayload)>, String>
where
    S: crate::basemap_archive::ArchiveRangeSource,
    O: crate::basemap_archive::ArchiveRangeSource,
{
    // The restyle path: parsed geometry already held means the archive — and
    // the network and disk behind it — is not consulted at all. This is what
    // `HttpsTiles::set_style` turns a theme flip and a detail toggle into.
    let remembered = if kind == ArchiveTileKind::Vector {
        styling
            .parsed_tiles
            .lock()
            .expect("the parsed-tile cache is not poisoned")
            .get(&tile_id)
            .cloned()
    } else {
        None
    };
    if let Some(parsed) = remembered {
        #[cfg(not(target_arch = "wasm32"))]
        let payload = {
            let (style, epoch) = current_style(&styling.style);
            tokio::task::spawn_blocking(move || {
                slot_for(styled_tile(&parsed, &style, tile_id.zoom), epoch)
            })
            .await
            .map_err(|error| format!("re-styling {tile_id:?} from the parsed cache: {error}"))?
        };

        // On wasm the styling belongs to the frame pump, under its budget.
        #[cfg(target_arch = "wasm32")]
        let payload = FetchPayload::Parsed(parsed);

        return Ok(Some((tile_id, payload)));
    }

    let bytes = archive
        .tile(tile_id.zoom, tile_id.x, tile_id.y)
        .await
        .map_err(|error| format!("reading {tile_id:?} from the basemap archive: {error}"))?;

    let Some(bytes) = bytes.into_bytes() else {
        log::trace!("the basemap archive holds no tile at {tile_id:?}");
        return Ok(None);
    };

    #[cfg(not(target_arch = "wasm32"))]
    let payload = {
        let (style, epoch) = current_style(&styling.style);
        let egui_ctx = egui_ctx.clone();
        let parsed_tiles = Arc::clone(&styling.parsed_tiles);

        tokio::task::spawn_blocking(move || {
            decode_archive_tile_remembering(&bytes, kind, &style, tile_id, &egui_ctx, &parsed_tiles)
                .map(|tile| slot_for(tile, epoch))
        })
        .await
        .map_err(|error| format!("rendering {tile_id:?} from the basemap archive: {error}"))?
        .map_err(|error| format!("rendering {tile_id:?} from the basemap archive: {error}"))?
    };

    // On wasm the tessellation belongs to the frame pump, under its budget.
    #[cfg(target_arch = "wasm32")]
    let payload = FetchPayload::Bytes(bytes);

    Ok(Some((tile_id, payload)))
}

/// The style the IO task must render against right now, with the restyle
/// generation it belongs to — read together under one lock so a styling can
/// never be stamped with another generation's number.
#[cfg(not(target_arch = "wasm32"))]
fn current_style(slot: &std::sync::RwLock<StyleSlot>) -> (Arc<Style>, u64) {
    let slot = slot.read().expect("the style slot is not poisoned");
    (Arc::clone(&slot.style), slot.epoch)
}

pub mod take_ledger;

// Native-only: `#[tokio::test]` (the dev-dependency is target-gated),
// `ClientBuilder::timeout` and `Error::is_connect`, which reqwest's wasm arm
// does not have, and `squallar_radar::tls::default_is_ring`, itself
// `cfg(not(wasm32))`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
