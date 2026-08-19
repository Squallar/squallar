use chrono::NaiveDateTime;
use nexrad_model::data::Scan;
use rustdar_egui::pane::RenderTarget;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use rustdar_overlays::render::rasterize::HitMap;
use rustdar_overlays::types::GeoBounds;
use rustdar_radar::archive::Identifier;
use rustdar_radar::level3::Level3Product;
use rustdar_radar::types::RadarProduct;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

/// Successful scan data returned from a background fetch.
pub struct ScanData {
    pub scan: Scan,
    /// What the volume's cuts declared their Nyquist velocities to be.
    ///
    /// Carried beside the `Scan` rather than in it because the model type has
    /// no field for it — see [`rustdar_radar::nyquist`] — and dropping it here
    /// would leave the section worker estimating velocity fold limits that the
    /// archive stated outright, with no symptom to notice.
    pub declared_nyquist: rustdar_radar::nyquist::DeclaredNyquist,
    pub site: String,
    pub timestamp: NaiveDateTime,
}

/// Result from a background radar scan fetch, with generation tracking.
pub struct ScanResponse {
    pub generation: u64,
    /// Site this fetch was for (needed for per-site generation checking).
    pub site: String,
    pub result: Result<ScanData, String>,
    /// True when this result originated from an auto-poll check (not manual navigation).
    pub is_auto_poll: bool,
}

/// What a render produced: the texture, the half-width it was projected at,
/// and the per-pixel value grid a hover reads.
pub struct RenderedImage {
    /// Already in egui's pixel layout, and already an `Arc`, because the frame
    /// thread must neither convert it nor copy it.
    ///
    /// The renderer's own output is `Vec<u8>` of RGBA; turning that into a
    /// `ColorImage` is a per-pixel unmultiply over 16 MiB at the base size and
    /// 64 MiB long range. That used to happen in `apply_render_to_pane`, on the
    /// **frame thread**, once per completed render. Measured on a 7950X,
    /// release, medians of 11 runs of a real KDMX 0.5° cut: **6.4 ms** at 2048²
    /// and **31.0 ms** at 4096², against a 16.7 ms frame budget. The second
    /// figure is two frames dropped every time a long-range render lands, which
    /// is what the binding rule about heavy work on the frame thread is for.
    ///
    /// The per-pixel half of it no longer happens anywhere near a frame.
    /// `offload::execute` premultiplies the raster inside the job — the render
    /// thread natively, the Web Worker in a browser — so `spawn_render`'s
    /// `deliver` builds this through `ColorImage::from_rgba_premultiplied`,
    /// whose per-pixel constructor is `Self([r, g, b, a])` and computes
    /// nothing. On the browser that is the whole of the win: `deliver` runs on
    /// the main thread there, and what it does on it is now a length assertion
    /// and a copy.
    ///
    /// The `Arc` is what keeps it from being copied again: the render cache,
    /// the pane's restore copy and `Context::load_texture` all take this one
    /// buffer, and `ImageData: From<Arc<ColorImage>>` means the upload does
    /// not clone it either.
    pub image: Arc<egui::ColorImage>,
    /// [`rustdar_radar::types::plan_view_extent_km`] of the sweep's reach, and
    /// so the *only* thing that says where these pixels sit on the ground.
    /// Carried this far rather than recomputed at the far end because a
    /// placement site has the site's coordinates but not the sweep.
    pub max_range_km: f64,
    /// The gates behind these pixels, for the readout under the pointer — see
    /// [`rustdar_radar::hover::HoverSource`].
    ///
    /// This is the field that was a `side²` `f32` raster grid: 206.75 MiB for a
    /// surveillance cut at the ceiling this display now reaches, carried out of
    /// the renderer so that a hover could read one number out of it. It is the
    /// same numbers at the resolution the radar took them, about a fortieth of
    /// the size, and the raster grid no longer leaves `rustdar-radar` at all.
    pub hover: Arc<rustdar_radar::hover::HoverSource>,
    /// Where the drawn sweep's cut declared its velocity folds, m/s, or `None`
    /// for a raster no single cut is behind.
    ///
    /// Metadata about the picture, exactly as `max_range_km` is, and carried
    /// the same distance for the same reason: the far end has the pane and the
    /// site but not the sweep, so a number about the sweep either travels with
    /// the pixels or is unavailable where they are drawn.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer these pixels were classified against came from,
    /// or `None` for a raster that classified nothing.
    ///
    /// Metadata that travels with the picture for the reason above, and the
    /// one where travelling matters most: the melting layer a render stood on
    /// is not recoverable from anything the far end holds — the cache it came
    /// from may already have been replaced by the next volume's — and the
    /// difference between the two answers is a classification against a guess.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector these pixels were shifted by came from,
    /// or `None` for a raster that shifted nothing.
    ///
    /// Metadata that travels with the picture for the reason above, and for
    /// the same sharpened version of it: the `N0S` a render stood on is
    /// per-volume and the cache holding it may already have rolled, so the
    /// difference between the two answers is the RPG's own applied vector
    /// against a right-mover prediction rotated clockwise of it.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

/// Result from a background radar render thread.
pub struct RenderResponse {
    /// `None` where the renderer found nothing to draw.
    ///
    /// A render that answers nothing still has to report back. `pane_render`'s
    /// `render_in_flight` is cleared on receipt of this message and nowhere else
    /// outside `reset_panes*`, and `dispatch_pane_renders` refuses to dispatch
    /// while it is set — so a render that stayed silent would leave its pane
    /// unable to ask for another one until something reset it.
    ///
    /// The ordinary source of a `None` is `Job::renders_nothing`: a pane parked
    /// on a tilt the volume does not carry. Rare against an archive volume,
    /// which holds every cut it will ever have; routine against a volume still
    /// being assembled from the real-time chunk feed, where an upper tilt has
    /// simply not been scanned yet. That change in frequency is what makes the
    /// report mandatory rather than tidy.
    ///
    /// An *abandoned* render still sends nothing at all — the send is gated on
    /// `results_wanted`, so a superseded render cannot clear the flag belonging
    /// to the render that replaced it.
    pub rendered: Option<RenderedImage>,
    pub product: RadarProduct,
    pub elevation: f32,
    pub generation: u64,
    pub pane_idx: usize,
}

/// Result from a background cross-section cut.
///
/// Carries the [`SectionTarget`](rustdar_egui::pane::SectionTarget) it was cut
/// for rather than a bare pane index, and that is what matches a result to a
/// pane. A section takes an order of magnitude longer to produce than the user
/// takes to draw another line over it, so "the pane this belongs to" and "the
/// pane that is still waiting for this" are different questions — and answering
/// only the first would let a section of the previous line arrive after the
/// current one and sit there looking authoritative.
pub struct SectionResponse {
    pub pane_idx: usize,
    pub generation: u64,
    /// What was asked for: which volume, which moment, which line.
    pub target: rustdar_egui::pane::SectionTarget,
    /// `None` where the cut answered nothing.
    ///
    /// Sent either way, for the reason [`RenderResponse::rendered`] is: this
    /// message is what clears `render_in_flight`, and a pane that never hears
    /// back stops asking.
    pub section: Option<Box<rustdar_radar::xsect::CrossSection>>,
}

/// Result from a background voxel build.
///
/// Carries the [`VolumeTarget`](rustdar_egui::pane::VolumeTarget) it was built
/// for and no pane index at all: the store refcounts grids **by target**, so
/// the result belongs to every pane attached to that target's `Building`
/// entry, and `VolumeStore::complete` is what resolves them. A stale target —
/// superseded by a newer sealed sweep while the build was in flight — finds no
/// `Building` entry and is dropped, which is the dedupe working, not a leak.
pub struct VoxelResponse {
    /// What was asked for: which site, which stamp, which moment, which region.
    pub target: rustdar_egui::pane::VolumeTarget,
    /// `None` where the resample answered nothing.
    pub grid: Option<Box<rustdar_radar::voxel::VoxelGrid>>,
}

/// Result from a Level III object fetch.
///
/// Names the AWIPS **code** and no product. One poll fetches each code once and
/// every product that reads it is served from the same object, so a `product`
/// field here would be one of several right answers — and whichever it named
/// would be the only pane redrawn and the only picker entry filled in. The
/// readers are derived on arrival instead: [`RadarProduct::level3_readers`].
pub struct Level3Response {
    pub generation: u64,
    /// AWIPS product ID this object is, e.g. `"EET"` — the cache key alongside
    /// the site, and what the readers are looked up by.
    pub code: String,
    pub site: String,
    /// The decoded product *and* the stamp of the object it came from.
    ///
    /// Carrying the stamp is what lets the UI distinguish a product from this
    /// scan from one `level3::latest_key`'s previous-day fallback found — up to
    /// ~48 h old — the same way `HrrrGridData::ref_time` distinguishes a 0–1 h
    /// forecast from an analysis.
    pub result: Result<Level3Product, String>,
}

/// Result from a background overlay rasterization thread.
pub struct OverlayRenderResponse {
    /// Already in egui's pixel layout, and already an `Arc`, for the reason
    /// [`RenderedImage::image`] gives — and at the size that makes this the
    /// larger of the two cases rather than the smaller.
    ///
    /// An overlay texture is planned against the *viewport plus overdraw*
    /// (`plan_overlay_texture`), so on a desktop it is not the radar raster's
    /// 2048² but whatever the window is: with `OVERDRAW_FRACTION` at 1.0, which
    /// is what it was when this was measured, a 1920×1080 pane planned
    /// 5760×3240 — 18.7 M pixels, **71.2 MiB** of `Color32` (74,649,600 bytes).
    /// The unmultiply over that measured **45.5 ms best, 47.6 ms median** on a
    /// 7950X — nearly three frames — and it used to run in
    /// `poll_overlay_render_results`, on the **frame thread**, once per arriving
    /// overlay, drained unbounded over every overlay kind the user has switched
    /// on.
    ///
    /// The fraction is a quarter now, so the same pane plans 2880×1620 and
    /// 17.8 MiB, and no overlay path has an unmultiply left in it at all —
    /// `offload::execute` converts inside the job and the one shared deliver
    /// copies premultiplied pixels straight through.
    /// Neither change makes this the smaller of the two cases: this buffer still
    /// scales with the window rather than being fixed, and it is still the one
    /// that must not be walked on the frame thread.
    ///
    /// # Two fifths of that was paging, not arithmetic
    ///
    /// Both buffers are far above glibc's `DEFAULT_MMAP_THRESHOLD_MAX` — 32 MiB
    /// on 64-bit — so neither is ever recycled from the heap: each is its own
    /// `mmap`/`munmap` and every page of it is faulted in on first touch.
    /// Measured by running the same per-pixel loop twice, once into a fresh
    /// destination and once into one that stays faulted in: **40.7 ms with
    /// 18,226 minor faults, against 23.7 ms with none**. So 17.0 ms of the
    /// conversion was the destination's first touch, and 18,225 of those faults
    /// are simply 74,649,600 ÷ 4096 — every page of the buffer, exactly once.
    ///
    /// That cost travels with the conversion rather than disappearing: the
    /// syscall counts either side of this change are the same to within two
    /// calls. It is now paid on the rasterizing thread, beside the
    /// rasterization that had to fault the RGBA buffer in anyway.
    ///
    /// The `Arc` is the second half — see `App::poll_overlay_render_results`,
    /// which uploads once and clones the handle to every pane in
    /// `pane_indices`. Not a link: the module is private, reached through a
    /// `#[path]` attribute, so rustdoc can resolve no path to it from here.
    ///
    /// The renderer's `Vec<u8>` is dropped in the same closure that converts it,
    /// so the RGBA buffer and its `Color32` copy never coexist in the channel.
    ///
    /// # `None` is a render that failed, and it must still be sent
    ///
    /// The described overlay path (an overlay codec row's `JobRequest`) can answer
    /// nothing — a worker died mid-job, a wait for one lapsed, a reply's
    /// buffer failed the dispatch's own length check — where the opaque
    /// closures always produced pixels. The empty response travels anyway,
    /// because this message is the **only** thing that clears the named panes'
    /// `render_in_flight` marks: `ui_map_pane` dispatches on
    /// `stale && !render_in_flight`, so a failure that went unreported would
    /// leave every named pane believing a rasterization it will never hear
    /// about is still running, and that layer could never be dispatched again
    /// for the life of the session. The poller clears the marks for a `None`
    /// exactly as for a kept result, and places nothing.
    pub image: Option<Arc<egui::ColorImage>>,
    pub geo_bounds: GeoBounds,
    pub overlay_kind: OverlayKind,
    pub generation: u64,
    pub pane_indices: Vec<usize>,
    pub zoom: i32,
    pub hit_map: Option<HitMap>,
}

/// Result from listing available scans for a loop time range.
pub struct LoopScanListResponse {
    pub pane_idx: usize,
    /// NEXRAD site the listing was requested for. Every `Identifier` below is one
    /// of this site's files.
    ///
    /// A listing is a network round-trip that cannot be cancelled, and a pane's
    /// loop can be torn down and rebuilt for another site while it is in the air —
    /// by a site switch, or by any of the routine rebuilds (`reinit_active_loops`,
    /// the lookback slider). Without this the receiver could not tell a live
    /// listing from one belonging to a loop that no longer exists, and would take
    /// one site's file list as another site's frames.
    pub site: String,
    /// Timestamps and identifiers for scans in the requested range (oldest-first).
    pub scans: Vec<(NaiveDateTime, Identifier)>,
}

/// Result from downloading a single scan for a loop frame.
pub struct LoopScanDownloadResponse {
    pub pane_idx: usize,
    /// NEXRAD site this scan was downloaded from. Half of the cache key.
    ///
    /// It is the site of the *listing the identifier came from*, carried through
    /// `PendingDownloads` and echoed here — not the site the requesting pane's loop
    /// happened to be on when the download was dispatched, and not re-read from the
    /// pane on arrival. Both of those can have moved on: the pane's loop is rebuilt
    /// on a site switch, and identifiers outlive the loop that listed them.
    pub site: String,
    /// UTC timestamp of the downloaded scan.
    pub timestamp: NaiveDateTime,
    /// The decoded volume and what its cuts declared their Nyquist velocities
    /// to be, or `None` if the download failed.
    ///
    /// The pair for the reason [`ScanData::declared_nyquist`] gives: the model
    /// type has no field for the number, and a loop frame's NROT or SRV
    /// dealiases around it exactly as the still frame beside it does.
    pub scan: Option<(Arc<Scan>, Arc<rustdar_radar::nyquist::DeclaredNyquist>)>,
}

/// The Level III bucket keys a loop's pairings will be ranked against: one
/// listing per `(site, AWIPS code)` covering the UTC days its window touches.
///
/// The Level III counterpart of [`LoopScanListResponse`], and it carries the site
/// and the code for the same reason that one carries the site: a listing is an
/// uncancellable round-trip, the pane's loop can be rebuilt for another site or
/// retargeted to another product while it is in the air, and the keys are useless
/// — worse, misleading — filed under anything but what they were listed for.
pub struct LoopL3ListResponse {
    pub pane_idx: usize,
    /// Site the listing was made for. Every key below is one of its objects.
    pub site: String,
    /// AWIPS product ID the listing was made for, e.g. `"EET"`.
    pub code: String,
    /// Every key across the listed days, unordered. Ranking per frame is
    /// [`rustdar_radar::level3::candidates_near`]'s job.
    ///
    /// An empty list is a real answer — the site served no objects for this
    /// product — and is cached as one, so every frame resolves to a gap and the
    /// loop retires rather than waiting on a listing that already happened.
    pub keys: Vec<String>,
}

/// The Level III object paired to one loop frame's volume.
///
/// `product` is `None` when the site generated no object for that volume: an
/// ordinary gap, not a failure. It is cached as the answer, so the frame is
/// retired once rather than re-paired on every dispatch pass.
pub struct LoopL3FetchResponse {
    pub pane_idx: usize,
    /// Site the object was paired against — half of the cache key, carried from
    /// the pairing rather than re-read from the pane on arrival, exactly as
    /// [`LoopScanDownloadResponse::site`] is.
    pub site: String,
    /// AWIPS product ID this object is, the second part of the cache key.
    pub code: String,
    /// The frame's **volume start** — what the pairing matched the object's PDB
    /// against, and the third part of the cache key. Not the object's own key
    /// timestamp, which is when the RPG published it.
    pub timestamp: chrono::NaiveDateTime,
    pub product: Option<Arc<Level3Product>>,
}

/// Result from rendering a single loop frame.
pub struct LoopRenderResponse {
    pub pane_idx: usize,
    pub timestamp: NaiveDateTime,
    /// The render target this render was dispatched for: the loop's site plus the
    /// pane's *selected* product and elevation — not the per-scan snapped angle the
    /// image was actually rendered at. Compared against
    /// `LoopPlaybackState::rendered_for` on arrival to reject results whose target the
    /// pane has since moved away from.
    pub target: RenderTarget,
    /// The sweep angle the image actually depicts: `target.elevation` snapped to a
    /// sweep this frame's own scan carries. Unlike the target, this is a property
    /// of the scan as well as the selection, so a pane taking this image via the
    /// sibling broadcast has to check it against what *its* scan resolves the same
    /// selection to — see `LoopPlaybackState::frame_accepting_broadcast`.
    ///
    /// Set unconditionally, on the failure path too: it describes the render that was
    /// *dispatched*, and there is only one send site to set it from.
    pub snapped: f32,
    /// The site coordinates the image was projected around — the ones the renderer
    /// was handed, straight off `LoopRenderRequest::render_params`.
    ///
    /// Carried rather than looked back up. The receiving loop's own
    /// `site_lat`/`site_lon` are the obvious substitute and are a reconstruction:
    /// they are only equal to these because a site change rebuilds the loop and
    /// clears `rendered_for`, so the target check rejects the result first. That
    /// coupling lives in another type and is invisible at the point of use, and it
    /// has to hold for sibling panes taking this image via the broadcast too. The
    /// image describes one pair of coordinates; it travels with them.
    pub site_lat: f64,
    /// See [`Self::site_lat`].
    pub site_lon: f64,
    /// The finished image, already in egui's pixel layout, or `None` when the scan
    /// carried no matching sweep and there is nothing to show.
    ///
    /// Deliberately not the renderer's `Vec<u8>`. Converting before the send means
    /// the RGBA buffer and its `Color32` copy — `LOOP_IMAGE_SIZE² × 4` bytes
    /// each, 16 MiB apiece at 2048² — never coexist in the channel; the receiver
    /// holds exactly one buffer and moves it straight into
    /// `Context::load_texture`. The transient pair is bounded by
    /// `MAX_CONCURRENT_RENDERS`.
    ///
    /// Natively that conversion is on the render thread, off the frame-pacing
    /// path entirely. In the browser, where the rasterization runs in a Web
    /// Worker that cannot build an `egui::ColorImage` and could not post one if
    /// it did, it happens in `spawn_loop_frame_render`'s `deliver` — on the main
    /// thread, and against a 1024² loop frame rather than the 2048² a still
    /// frame is. What it costs there is a copy and nothing else: the premultiply
    /// itself is `offload::execute`'s, done in the worker, so this end reads the
    /// bytes through `ColorImage::from_rgba_premultiplied`.
    ///
    /// `None` replaces the previous empty-`Vec` sentinel; the meaning is unchanged.
    /// The receiver `take`s it rather than moving it out, so the rest of the response
    /// stays borrowable for `broadcast_sweep`.
    pub image: Option<egui::ColorImage>,
    /// The half-width this frame was projected at, km. Per **frame**, not per
    /// loop: a loop steps through volumes, each sweep reaches as far as it
    /// reaches, and the frames of one loop can legitimately be different sizes
    /// on the ground. `RadarImageData` carries it forward for exactly that
    /// reason — every frame is placed by its own number.
    pub max_range_km: f64,
    /// Where this frame's cut declared its velocity folds, m/s. Per frame for
    /// the same reason the extent is: a loop steps through volumes, and the
    /// RDA reselects PRFs between them, so the limit the newest frame folded
    /// at is not necessarily the limit the oldest did.
    pub nyquist_ms: Option<f64>,
    /// Where this frame's melting layer came from, or `None` for a frame that
    /// classified nothing.
    ///
    /// Per frame, and emphatically so: a loop pairs one N0M object per volume,
    /// so the newest frame can be classified against the RPG's own layer while
    /// an older frame — whose object was never fetched — falls back to the
    /// fleet constant. One number for the whole loop would caption most of its
    /// frames with another frame's provenance.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where this frame's storm motion vector came from, or `None` for a frame
    /// that shifted nothing.
    ///
    /// Per frame, and emphatically so, for the reason the melting layer is: a
    /// loop pairs one `N0S` per volume, so the newest frame can be shifted by
    /// the RPG's own applied vector while an older frame — whose object was
    /// never fetched — falls back to a Bunkers right-mover. One number for the
    /// whole loop would caption most of its frames with another frame's
    /// provenance.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
    /// Where this frame's gates are, with **no numbers behind them**.
    ///
    /// The half of [`rustdar_radar::render::polar::PolarField`] a loop frame
    /// can afford: 5.8 KiB of wedges and a gate spacing, against 5.03 MiB of
    /// values that would be 70 MiB across a browser's loop. It is what turns
    /// the point under the pointer back into a `(radial, gate)` of the volume
    /// this frame was rendered from, which is resident anyway for as long as
    /// the loop lives — see [`rustdar_radar::hover::SweepGates`].
    ///
    /// Set on the failure path too, as an empty field, for the reason `snapped`
    /// is set there: there is one send site, and a response with no image has
    /// no gates either.
    pub polar: rustdar_radar::render::polar::PolarField,
}

/// Result from cutting a single cross-section loop frame.
///
/// The section counterpart of [`LoopRenderResponse`], and a separate type rather
/// than a variant of it because the two identify their pictures with different
/// things: a plan-view frame is pinned by the sweep its own scan snapped the
/// selection to, and a section frame by the line, the storm motion vector and
/// the tilt ladder it was cut from. A shared type would have to carry both sets
/// with half of each unused, and every receiver would have to check which half
/// applied — which is exactly the reading that lets a plan-view raster into a
/// section pane's frame list.
pub struct LoopSectionResponse {
    pub pane_idx: usize,
    pub timestamp: NaiveDateTime,
    /// The site/product half of the key this cut was dispatched for. Compared
    /// against `LoopPlaybackState::rendered_for` on arrival.
    pub target: RenderTarget,
    /// The line/storm-motion half. Compared against
    /// `LoopPlaybackState::section_key` on arrival, and both halves must match
    /// — see `LoopPlaybackState::is_cut_for`.
    pub key: rustdar_egui::pane::SectionLoopKey,
    /// The fingerprint of the tilt ladder this raster was actually cut from,
    /// from `rustdar_radar::sampler::ladder_fingerprint` over the frame's own
    /// scan at dispatch.
    ///
    /// Set unconditionally, on the failure path too, for the reason
    /// [`LoopRenderResponse::snapped`] is: it describes the cut that was
    /// dispatched, and there is one send site to set it from.
    pub ladder: u64,
    /// The finished raster, already in egui's pixel layout, or `None` when this
    /// volume carried nothing to cut.
    ///
    /// Converted before the send for the reason
    /// [`LoopRenderResponse::image`] gives — a `SECTION_WIDTH × SECTION_HEIGHT`
    /// RGBA buffer and its `Color32` copy are 8 MiB apiece natively and never
    /// coexist in the channel.
    pub image: Option<egui::ColorImage>,
    /// The height and distance scales the raster was cut against, and where the
    /// ladder's rungs are. Travel with the picture because they are labels *on*
    /// it: without them a loop would animate each frame's raster under the
    /// previous frame's axes.
    ///
    /// `None` exactly when [`Self::image`] is.
    pub axes: Option<rustdar_radar::xsect::SectionAxes>,
    /// See [`Self::axes`].
    pub tilt_elevations_deg: Vec<f64>,
    /// When each of those rungs was flown, milliseconds since the Unix epoch,
    /// in the same order — `rustdar_radar::xsect::CrossSection::tilt_collected_ms`.
    ///
    /// A label on the picture like the two above it, and it travels for the
    /// same reason: a loop frame drops the `CrossSection` behind its raster, so
    /// a frame whose ages were looked up beside it would be captioned with the
    /// live cut's ladder while showing its own.
    pub tilt_collected_ms: Vec<i64>,
}

/// One round of a site's real-time chunk feed.
///
/// Deliberately **not** a variant of [`ScanResponse`]. That type's drain bakes in
/// five behaviours that all belong to a fetch someone is waiting on and are all
/// wrong every few seconds: it takes the global `fetching` spinner down, clears
/// the pane's `loading_site`, routes an error through `set_error` (which doubles
/// the *archive* poll's backoff), stashes into `latest_cached_scans` on the
/// historic branch, and compensates for a stale discard. A `is_chunk: bool`
/// beside `is_auto_poll` would put four states in one drain, three of them
/// unreachable.
///
/// The poller travels *back* on this channel rather than being borrowed across
/// the await: it owns the assembled volume, and the fetch happens on a detached
/// task that cannot hold a reference into `App`.
pub struct ChunkResponse {
    /// The site's fetch generation at dispatch — inherited from the Level II
    /// fetch, never bumped. Bumping would let a five-second tick supersede a
    /// manual navigation, and the scan drain's stale arm would then take that
    /// navigation's spinner down early.
    pub generation: u64,
    pub site: String,
    /// The poller, handed back so the next round resumes from it.
    pub poller: Box<rustdar_radar::chunks::ChunkPoller>,
    pub result: Result<rustdar_radar::chunks::PollOutcome, String>,
}

/// The environmental 0 °C / −20 °C heights over a site, from Open-Meteo —
/// fetched when a scan loads, but TTL-gated (see
/// [`rustdar_radar::sounding::ENV_HEIGHTS_TTL`]) rather than refetched every
/// poll. Staged for the products `RadarProduct::reads_env_heights` names,
/// which will read them off `RenderDispatcher::env_heights`.
pub struct SoundingResponse {
    pub generation: u64,
    pub site: String,
    /// `None` when the fetch or parse failed. The receiver keeps whatever it
    /// already holds for the site — a stale environment beats none, and the
    /// TTL gate retries on the next poll.
    pub heights: Option<rustdar_radar::sounding::EnvHeights>,
}

/// The RPG's own Melting Layer object (Level III 166, AWIPS `N0M`) for one
/// site's currently-loaded volume.
///
/// The counterpart of [`SoundingResponse`] for the other half of the hybrid
/// classification's environment, and it differs from that one in the way that
/// matters: a sounding is a fact about a place and an hour, so a stale one is
/// merely old, while a melting-layer object is a fact about **one volume**.
/// Applied to a different volume it would place the layer confidently in the
/// wrong place — which is the defect this whole path exists to fix — so the
/// volume it names travels with it and the accessor that hands it to a render
/// refuses to apply it to any other.
///
/// Deliberately not folded into [`Level3Response`]. That one is keyed by AWIPS
/// code alone and is fetched "latest": it feeds products that *draw* an object,
/// where the newest is the right answer. This object is never drawn and the
/// newest is emphatically not the right answer.
pub struct MeltingLayerResponse {
    pub generation: u64,
    pub site: String,
    /// The Level II volume start the object was paired against, and the only
    /// volume it may be applied to. Echoed from the request rather than read
    /// back off the pane, which may have moved on while the fetch was in the
    /// air — the same discipline [`LoopL3FetchResponse::timestamp`] keeps.
    pub volume_start: NaiveDateTime,
    /// The object's bytes, or `None` when the site generated no `N0M` for that
    /// volume. An ordinary gap, not a failure: the classification falls to the
    /// next rung of `rustdar_radar::hca::resolve_melting_layer` and says so on
    /// screen.
    pub object: Option<Arc<Vec<u8>>>,
}

/// One site's RPG storm motion vector for one volume, already decoded.
///
/// [`MeltingLayerResponse`]'s sibling, on the same schedule and with the same
/// volume discipline: the vector is a fact about **one volume**, so the volume
/// it names travels with it and the accessor that hands it to a render refuses
/// to apply it to any other.
///
/// # Why this one carries no bytes
///
/// The one place this path deliberately differs from `N0M`. That object ships
/// as a blob because the worker decodes a per-azimuth field off-thread; an
/// `N0S` yields two scalars out of its Product Description Block, and the
/// pairing step in `rustdar_radar::level3::fetch_product_for_volume` has
/// already decoded that PDB to check which volume the object names. Shipping
/// the bytes onward would decode the same header a second time, on the frame
/// thread, to recover numbers the fetch already held.
pub struct StormMotionResponse {
    pub generation: u64,
    pub site: String,
    /// The Level II volume start the object was paired against, and the only
    /// volume this vector may be applied to. Echoed from the request rather
    /// than read back off the pane, on the discipline
    /// [`MeltingLayerResponse::volume_start`] keeps.
    pub volume_start: NaiveDateTime,
    /// `(speed_kt, direction_from_deg)` as the PDB stated them, or `None` when
    /// the site generated no `N0S` for that volume or its PDB carried no
    /// vector. An ordinary gap, not a failure: SRV falls to the next rung of
    /// `rustdar_radar::srv::storm_motion` and says so on screen.
    ///
    /// **`Some((0.0, 0.0))` is a reading, not a gap.** SCIT tracked no cells
    /// and the RPG painted an unshifted field; that is the vector the reference
    /// product was built with, and dropping it would replace the RPG's own
    /// answer with a derived one under the RPG's name.
    pub motion: Option<(f32, f32)>,
}

/// The network site catalogue, once the launch's one refresh has landed.
///
/// It exists to be **written to the cache**, not applied: the table was
/// resolved from the cached catalogue before the first frame and is
/// deliberately not re-resolved from this. See [`crate::site_catalogue`].
///
/// The trip through a channel is not ceremony. `PlatformBridge::config_store`
/// hands out a `Box<dyn ConfigStore>`, which is neither `Send` nor something a
/// detached future can hold, so the persist has to happen back on the thread
/// that owns the app — which is also the thread holding the cached copy the
/// write is compared against.
pub struct SiteCatalogueResponse {
    /// `None` when the fetch failed for any reason. The receiver keeps the
    /// cache it already had; there is no retry, because the next launch is one.
    pub catalogue: Option<rustdar_radar::catalogue::SiteCatalogue>,
}

/// Centralized channel hub for all async communication between the App and
/// background tasks (network fetches, radar rendering, etc.).
pub struct ChannelHub {
    pub scan_sender: Sender<ScanResponse>,
    pub scan_receiver: Receiver<ScanResponse>,
    pub render_sender: Sender<RenderResponse>,
    pub render_receiver: Receiver<RenderResponse>,
    pub section_sender: Sender<SectionResponse>,
    pub section_receiver: Receiver<SectionResponse>,
    pub voxel_sender: Sender<VoxelResponse>,
    pub voxel_receiver: Receiver<VoxelResponse>,
    pub level3_sender: Sender<Level3Response>,
    pub level3_receiver: Receiver<Level3Response>,
    pub overlay_fetch_sender: Sender<OverlayFetchResult>,
    pub overlay_fetch_receiver: Receiver<OverlayFetchResult>,
    pub overlay_render_sender: Sender<OverlayRenderResponse>,
    pub overlay_render_receiver: Receiver<OverlayRenderResponse>,
    pub loop_scan_list_sender: Sender<LoopScanListResponse>,
    pub loop_scan_list_receiver: Receiver<LoopScanListResponse>,
    pub loop_scan_download_sender: Sender<LoopScanDownloadResponse>,
    pub loop_scan_download_receiver: Receiver<LoopScanDownloadResponse>,
    pub loop_l3_list_sender: Sender<LoopL3ListResponse>,
    pub loop_l3_list_receiver: Receiver<LoopL3ListResponse>,
    pub loop_l3_fetch_sender: Sender<LoopL3FetchResponse>,
    pub loop_l3_fetch_receiver: Receiver<LoopL3FetchResponse>,
    pub loop_render_sender: Sender<LoopRenderResponse>,
    pub loop_render_receiver: Receiver<LoopRenderResponse>,
    pub loop_section_sender: Sender<LoopSectionResponse>,
    pub loop_section_receiver: Receiver<LoopSectionResponse>,
    pub chunk_sender: Sender<ChunkResponse>,
    pub chunk_receiver: Receiver<ChunkResponse>,
    pub sounding_sender: Sender<SoundingResponse>,
    pub sounding_receiver: Receiver<SoundingResponse>,
    pub melting_layer_sender: Sender<MeltingLayerResponse>,
    pub melting_layer_receiver: Receiver<MeltingLayerResponse>,
    pub storm_motion_sender: Sender<StormMotionResponse>,
    pub storm_motion_receiver: Receiver<StormMotionResponse>,
    pub site_catalogue_sender: Sender<SiteCatalogueResponse>,
    pub site_catalogue_receiver: Receiver<SiteCatalogueResponse>,
}

impl Default for ChannelHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelHub {
    pub fn new() -> Self {
        let (scan_sender, scan_receiver) = std::sync::mpsc::channel();
        let (render_sender, render_receiver) = std::sync::mpsc::channel();
        let (section_sender, section_receiver) = std::sync::mpsc::channel();
        let (voxel_sender, voxel_receiver) = std::sync::mpsc::channel();
        let (level3_sender, level3_receiver) = std::sync::mpsc::channel();
        let (overlay_fetch_sender, overlay_fetch_receiver) = std::sync::mpsc::channel();
        let (overlay_render_sender, overlay_render_receiver) = std::sync::mpsc::channel();
        let (loop_scan_list_sender, loop_scan_list_receiver) = std::sync::mpsc::channel();
        let (loop_scan_download_sender, loop_scan_download_receiver) = std::sync::mpsc::channel();
        let (loop_l3_list_sender, loop_l3_list_receiver) = std::sync::mpsc::channel();
        let (loop_l3_fetch_sender, loop_l3_fetch_receiver) = std::sync::mpsc::channel();
        let (loop_render_sender, loop_render_receiver) = std::sync::mpsc::channel();
        let (loop_section_sender, loop_section_receiver) = std::sync::mpsc::channel();
        let (sounding_sender, sounding_receiver) = std::sync::mpsc::channel();
        let (melting_layer_sender, melting_layer_receiver) = std::sync::mpsc::channel();
        let (storm_motion_sender, storm_motion_receiver) = std::sync::mpsc::channel();
        let (chunk_sender, chunk_receiver) = std::sync::mpsc::channel();
        let (site_catalogue_sender, site_catalogue_receiver) = std::sync::mpsc::channel();

        Self {
            scan_sender,
            scan_receiver,
            render_sender,
            render_receiver,
            section_sender,
            section_receiver,
            voxel_sender,
            voxel_receiver,
            level3_sender,
            level3_receiver,
            overlay_fetch_sender,
            overlay_fetch_receiver,
            overlay_render_sender,
            overlay_render_receiver,
            loop_scan_list_sender,
            loop_scan_list_receiver,
            loop_scan_download_sender,
            loop_scan_download_receiver,
            loop_l3_list_sender,
            loop_l3_list_receiver,
            loop_l3_fetch_sender,
            loop_l3_fetch_receiver,
            loop_render_sender,
            loop_render_receiver,
            loop_section_sender,
            loop_section_receiver,
            chunk_sender,
            chunk_receiver,
            sounding_sender,
            sounding_receiver,
            melting_layer_sender,
            melting_layer_receiver,
            storm_motion_sender,
            storm_motion_receiver,
            site_catalogue_sender,
            site_catalogue_receiver,
        }
    }
}
