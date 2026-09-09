//! Getting a raster onto the GPU without spending the frame on it.
//!
//! `queue.write_texture` is a blocking host write through the card's BAR window
//! (see [`crate::staging_ring`]). Measured here, RTX 3090 / Vulkan, median of
//! five: 2048² (17 MB) 7.79 ms, 4096² (67 MB) 31.30 ms, 7362² (217 MB)
//! 59.44 ms, 8192² (268 MB) 50.62 ms — and a WSR-88D surveillance cut asks for
//! 7362 px, once per distinct raster per volume, six panes at a time on a
//! resume.
//!
//! Two things answer it. **Bands**: a raster moves in row bands of at most
//! [`UPLOAD_BAND_BYTES`], which needs no adapter feature and so bounds the frame
//! on WebGL2 and GLES as well as Vulkan. **DMA**: where the device has a
//! [`crate::staging_ring`], a band is memcpy'd into cached host memory and
//! pulled across by the copy engine — per band at 7362², `write_texture`
//! 6.41 ms (13.7 MB) / 3.23 ms (6.9 MB) against staging+DMA 0.55 / 0.25 ms, i.e.
//! 2.1 GB/s against 24.7 GB/s.
//!
//! DMA without bands does not pay for itself: one un-banded DMA of a 7362²
//! raster is still 10.25 ms of frame thread, an un-banded ring slot is the whole
//! raster (437 MB resident pinned host memory at depth 2), and WebGL2 has no
//! `MAPPABLE_PRIMARY_BUFFERS` at all.
//!
//! **A band's copy is recorded on the frame's own encoder and never submitted
//! here.** The ring's slots are handed back at [`TextureUploads::after_submit`]
//! instead, which is the one thing a slot's mapping has to wait for: a map
//! asked for against a recorded-but-unsubmitted copy resolves early and panics
//! at the submission. Submitting per band is what that ordering used to cost —
//! **24.4 us of frame thread a band**, measured on the RTX 3090 / Vulkan
//! native arm on scene A at one pane, against a frame's own submission of
//! 149 us.
//!
//! A raster fills top-down over the frames its bands take (7 frames for 7362² on
//! a ring device) and this module cannot hold it back — egui mints a fresh
//! `TextureId` per `load_texture`. The drain must therefore keep asking for
//! frames: the app runs on `ControlFlow::Wait`, so "there will be another frame"
//! is not something it may assume.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;

use egui_wgpu::Renderer;
use egui_wgpu::wgpu;

use crate::staging_ring::{Ring, device_has_ring};

mod resident;

use resident::ResidentTextures;

/// The most one band carries, and so the size of one ring slot.
///
/// 8 MiB: what `write_texture` moves in 4.0 ms at the measured 2.1 GB/s through
/// the BAR window, a quarter of a 16.7 ms frame. Every band costs at most that
/// much frame thread whichever route it takes, so the DMA path can fall back to
/// `write_texture` mid-raster. Two slots of 8 MiB is 16.9 MiB of pinned host
/// memory against the 437 MB an un-banded ring would need.
///
/// **That 16.7 ms is a 60 Hz frame, and this application aims at
/// [`squallar_device_profile::constants::TARGET_FRAME_SERVICE`] — 4 ms.** What
/// survives the correction is the *ring slot*: on a device with a ring a band
/// is a memcpy into cached host memory, so its size prices pinned host memory
/// and the copy engine, not the frame thread. What does not survive is this
/// figure as a **blocking** allowance. Two places used it as one:
/// [`whole_budget`], which no longer does, and [`DECLINE_PATIENCE`]'s
/// fallback, which still writes a whole 8 MiB band with `write_texture` when
/// the ring has refused four frames running — 3.8 ms at the 2.1 GB/s below,
/// which is nearly the whole bar on one frame. That path is bounded by
/// nothing here and re-sizing it is a re-sweep of the ring slot, not a
/// re-spelling of this line.
pub const UPLOAD_BAND_BYTES: usize = 8 << 20;

/// Bands one frame moves when the copy engine is doing it.
///
/// One per ring slot, derived rather than chosen: a slot claimed on this frame
/// cannot be handed back on this frame, so asking for more slots than the ring
/// has is asking to be declined. Measured at four bands against a ring of two, a
/// 7362² raster took 13 frames with a 6.59 ms worst frame (a third of the bands
/// ran out of [`DECLINE_PATIENCE`]); at `STAGING_RING_DEPTH`, 14 frames with a
/// 0.61 ms worst frame after the first.
pub const DMA_BANDS_PER_FRAME: usize = crate::staging_ring::STAGING_RING_DEPTH;

/// The largest delta that crosses whole through `Renderer::update_texture` on
/// a device of this capability, and the band size past it. On a ring device
/// both stay [`UPLOAD_BAND_BYTES`] — the ring's own measured shape. On a
/// ringless device — all of web — every byte is a blocking `write_texture` on
/// the frame thread, so both fall to
/// [`squallar_device_profile::constants::BLOCKING_BAND_BYTES`], whose sweep
/// note carries the dry-frame cost this choice was made against.
const fn band_cap(capable: bool) -> usize {
    if capable {
        UPLOAD_BAND_BYTES
    } else {
        squallar_device_profile::constants::BLOCKING_BAND_BYTES
    }
}

/// Whether a delta of `bytes` for a texture this module does not own crosses
/// whole on this frame's queue rather than being filed as bands.
///
/// **The threshold is [`whole_budget`], not [`band_cap`]** — the largest
/// single delta that may block the frame thread is the most a whole frame may
/// block it with, because one delta is what a fresh frame can be asked for.
/// The two were the same expression until 2026-09-07 and that conflation is
/// what put a ring device's blocking allowance at 16 MiB; see [`whole_budget`]
/// for the arithmetic that produced it. `band_cap` keeps its own job, which is
/// how much of a raster one queued band carries.
const fn goes_whole(capable: bool, bytes: usize) -> bool {
    bytes <= whole_budget(capable)
}

/// **Why `upload pending` reads LOWER on six panes than on one**, as a build
/// failure rather than as prose.
///
/// This family charges the whole pixel buffer of every image with a band still
/// queued, and an image is only banded when it is **larger than
/// [`whole_budget`]**. That threshold is per *image*, and a pane's picture is
/// sized from the pane — so splitting one canvas into six does not move a byte
/// off the machine, it moves every picture under the threshold and out of the
/// family. A one-pane leg reading more than a six-pane leg is that, and not a
/// drain that fell behind: the renderer holds the frame's repaint delay at zero
/// while bands remain, so neither leg is starved of frames to drain with.
///
/// Both directions are pinned, because a `goes_whole` that answered constantly
/// would satisfy either alone.
const _: () = {
    // One pane of a 1920x1080 canvas: 8,294,400 B, ~4x the ring threshold.
    assert!(!goes_whole(true, 1920 * 1080 * 4));
    assert!(!goes_whole(false, 1920 * 1080 * 4));
    // The same canvas as four panes: 2,073,600 B, and it crosses whole.
    assert!(goes_whole(true, 960 * 540 * 4));
    assert!(goes_whole(false, 960 * 540 * 4));
    // And as six, 3x2: 1,382,400 B.
    assert!(goes_whole(true, 640 * 540 * 4));
    assert!(goes_whole(false, 640 * 540 * 4));
};

/// **The four-pane figure is 1.3 % under the ring threshold**, and that is a
/// knife edge worth failing a build over rather than discovering from a leg.
///
/// 2,073,600 B against a [`WHOLE_CROSSING_BYTES`] of 2,100,000: a canvas 2 %
/// wider, or a fifth pane, puts every picture back over the line and the
/// `upload pending` family back up by a whole batch. Nothing is wrong with
/// that — the bytes were always moving — but a reader comparing two legs across
/// such a change would be comparing two different populations, so the margin is
/// stated where it cannot rot.
const _: () = assert!(960 * 540 * 4 * 100 / WHOLE_CROSSING_BYTES >= 98);

/// Whether `id` is the font atlas, the one texture that crosses whole at any
/// size.
///
/// **The font atlas is never banded.** Every galley on the glass holds its
/// glyphs' positions in that texture as texel coordinates, so a texture that
/// is only partly uploaded draws every label whose rows have not landed yet
/// from whatever the fresh allocation holds — and it stays that way for as
/// long as the bands take, and for as long as no frame is asked for after
/// that. egui hands the atlas over whole on every doubling of its height, so
/// once the height crossed the band cap the banded route took it, and the
/// place names broke on every doubling — at the zooms whose new label sizes
/// forced one. One blocking write per doubling is the honest cost, and the
/// doublings are rare once no text size is a continuous function of zoom —
/// see `station_model::font_size_for_zoom`, which was the one that was, and
/// `walkers::mvt`, whose label sizes evaluate against an integer tile zoom
/// and so are bounded by construction.
///
/// # What one doubling costs, read from the arm rather than from a note
///
/// The atlas is `max_texture_side.at_most(16 * 1024)` wide (epaint
/// `text/fonts.rs`, `FontsImpl::new`), and `max_texture_side` is
/// `device.limits().max_texture_dimension_2d` (`EguiRenderer::new`). On web
/// [`crate::device::device_limits`] copies the adapter's resolution verbatim,
/// and Firefox's WebGL2 reports 32768 on a real driver — so **the web atlas is
/// 16384 wide, not the 8192 the note here used to price it at**. Every figure
/// is double what it read: 2 MiB at the initial 32 rows, 8 MiB at 128, 64 MiB
/// at 1024, and 1 GiB at the full square (`TextureAtlas::max_height` is the
/// width). A doubling costs that twice over on one frame — epaint clones the
/// whole `ColorImage` into `ImageDelta::full`, and this module then writes all
/// of it.
fn is_font_atlas(id: egui::TextureId) -> bool {
    id == egui::TextureId::default()
}

/// What `queue.write_texture` moves through the card's BAR window, in bytes a
/// second: this module's own measured figure (13.7 MB in 6.41 ms at 7362²,
/// see the module note). The staged route's 24.7 GB/s is deliberately absent —
/// the whole-crossing route never touches the ring, and pricing it at the ring's
/// bandwidth is the mistake this file already made once.
const BAR_WRITE_BYTES_PER_SEC: u64 = 2_100_000_000;

/// The share of
/// [`squallar_device_profile::constants::TARGET_FRAME_SERVICE`] the
/// whole-crossing route may hold.
///
/// A quarter — the share [`UPLOAD_BAND_BYTES`] was always sized at ("a quarter
/// of a 16.7 ms frame"). **What is corrected here is the frame, not the
/// share.**
const WHOLE_CROSSING_SHARE: u64 = 4;

/// [`WHOLE_CROSSING_SHARE`] of the target frame, priced at
/// [`BAR_WRITE_BYTES_PER_SEC`]: 1 ms of BAR window, 2,100,000 B.
const WHOLE_CROSSING_BYTES: usize = (BAR_WRITE_BYTES_PER_SEC
    * squallar_device_profile::constants::TARGET_FRAME_SERVICE.as_micros() as u64
    / WHOLE_CROSSING_SHARE
    / 1_000_000) as usize;

/// What creating a `wgpu::Texture` costs the frame thread, in bytes of texture
/// a second: this module's own measured figure — 4.82 ms for a 7362² RGBA
/// texture, which is 216,796,176 B, so 44.98 GB/s. Rounded down.
///
/// A creation is not a transfer and nothing is copied through it; the cost is
/// the driver's allocation, and it is the *size* of the texture that sets it.
/// That is the whole reason [`TEXTURE_CREATE_BUDGET_BYTES`] can exist at all.
const TEXTURE_CREATE_BYTES_PER_SEC: u64 = 44_900_000_000;

/// The share of
/// [`squallar_device_profile::constants::TARGET_FRAME_SERVICE`] the drain's
/// texture creations may hold — a quarter, [`WHOLE_CROSSING_SHARE`]'s, and for
/// the same reason.
const TEXTURE_CREATE_SHARE: u64 = 4;

/// **Texture a frame's [`TextureUploads::drain`] may create before it stops**,
/// in bytes: [`TEXTURE_CREATE_SHARE`] of the target frame at
/// [`TEXTURE_CREATE_BYTES_PER_SEC`]. 1 ms, and 44,900,000 B.
///
/// # What this replaced, and what that cost
///
/// The drain used to `break` after **any** band that allocated a texture,
/// whatever the texture was, on the reading that "creating the texture is
/// 4.82 ms for a 7362² raster against ~0.7 ms for a band by DMA, so a frame
/// that allocated one has spent its budget". The measurement is this module's
/// own and is not in doubt; what it does not carry is the *size* it was taken
/// at. 4.82 ms is 216,796,176 B of texture. A six-pane overlay picture is
/// 960x780x4 = 2,995,200 B — **72 times smaller, and 0.067 ms** — and the
/// break spent a whole frame's drain budget on it exactly as if it had cost
/// the 4.82 ms.
///
/// **Every whole-image delta allocates**, which is every overlay picture,
/// every loop frame and every basemap tile: [`TextureUploads::file`] takes the
/// `delta.pos.is_none()` arm, drops the id's old texture and files the band
/// with `allocate: Some(..)`. So the break fired on the first band of every
/// frame and [`bands_per_frame`] — which is 2 on a ring device — was **1** for
/// the whole life of the process on any scene made of pictures.
///
/// Measured on three 420 s HEAVY6 legs (6 panes, 6 sites, a playing 1 h loop,
/// `pan-zoom-2d`, 1920x1080 on Xvfb, RTX 3090 / Vulkan, base 0f067c847):
/// **8,439 bands moved across 8,473 frames** — 0.996 a frame, against a
/// [`bands_per_frame`] of 2. The drain was pinned at exactly half the ring's
/// capacity, and it was the producer's rate that lost: 9,649 inked pictures
/// over 420 s is 23 a second against a drain of 20 a second, so
/// [`TextureUploads::pending`] grew until it held **52 whole 960x780 pictures,
/// 155,750,400 B of HOST memory** (`upload residency:`, two legs of three
/// exactly, the third 49). Those bytes are `squallar_alloc`'s, not the
/// device's.
///
/// # Why a byte budget and not simply no break
///
/// The 216 MB case is real and the break was right about it: one such creation
/// is 4.82 ms, past a whole `TARGET_FRAME_SERVICE`, and a second on the same
/// frame would be a dropped frame with nothing gained. A budget in bytes keeps
/// that — 216,796,176 B is 4.8x this figure, so a frame that creates one stops
/// after it, exactly as before — and stops charging a 3 MB creation the same
/// price. The budget is checked *after* the creation, so a frame always moves
/// at least one band however large it is; nothing can be starved by this.
///
/// **On this scene the ring, not this figure, is what binds.** Two 2,995,200 B
/// creations are 0.13 ms, well under the 1 ms here, so what stops the drain
/// is [`bands_per_frame`] — [`crate::staging_ring::STAGING_RING_DEPTH`], a
/// count of staging slots and a real constraint on how many bands one frame
/// can stage. That is the honest ceiling and this budget is deliberately not
/// underneath it: a budget written low enough to bind twice would be the same
/// mistake in the other direction.
const TEXTURE_CREATE_BUDGET_BYTES: u64 = TEXTURE_CREATE_BYTES_PER_SEC
    * squallar_device_profile::constants::TARGET_FRAME_SERVICE.as_micros() as u64
    / TEXTURE_CREATE_SHARE
    / 1_000_000;

/// The break still fires on the texture it was measured at, and does not fire
/// on the picture the app is actually made of. Both directions, because a
/// budget that answered constantly would satisfy either alone.
const _: () = {
    // 7362² RGBA — the creation the 4.82 ms was measured on.
    assert!(7362 * 7362 * 4 >= TEXTURE_CREATE_BUDGET_BYTES);
    // Two six-pane overlay pictures, 960x780x4 each.
    assert!(2 * 960 * 780 * 4 < TEXTURE_CREATE_BUDGET_BYTES);
};

/// Whole-crossing bytes one frame may push through `Renderer::update_texture`
/// before the rest are filed as bands.
///
/// # Why the route has to be bounded at all
///
/// `Renderer::update_texture` is `write_texture` on the frame's own queue, so
/// every byte on this route is frame thread. A route handed an *unbounded*
/// number of chunks that each fit a per-delta cap is not bounded by that cap;
/// the frame is what has to be bounded, and [`TextureUploads::apply`] used to
/// loop the whole delta set through this route with nothing counting what it
/// spent. On web `WASM_LOOP_IMAGE_SIZE` is 1024, so one loop frame's texture is
/// 4 MiB — *exactly*
/// [`squallar_device_profile::constants::BLOCKING_BAND_BYTES`], which
/// [`goes_whole`] compares with `<=`. A dispatch textures
/// `Budgets::textured_frames` of them, on web `min(14, 14)` = 14, and
/// 14 × 4 MiB = **56 MiB of blocking `write_texture` on one frame thread**. Out
/// of wasm linear memory at ~1 GB/s that is ~56 ms, and the panel that reported
/// that defect put its `prep` p99 in the **[53.8, 64.0) ms** bin. Not a
/// hypothetical arithmetic: every figure in it is a pinned constant rather than
/// a fit. And the font atlas is *not* what fills that bin — measured headless
/// over the app's own bounded size sets, the 16384-wide web atlas settles at
/// 16384×64 and its largest whole delta is 4 MiB.
///
/// # The figure this returns is a TIME, spent at a bandwidth
///
/// It was `band_cap × bands_per_frame` until 2026-09-07 — a budget written as
/// arithmetic over two constants, neither of which is a blocking allowance:
///
/// * [`UPLOAD_BAND_BYTES`] is 8 MiB *because* it is "a quarter of a 16.7 ms
///   frame", i.e. sized against **60 Hz**; and
/// * [`bands_per_frame`] is [`DMA_BANDS_PER_FRAME`] is
///   [`crate::staging_ring::STAGING_RING_DEPTH`] — a count of **staging
///   buffers**, chosen because a slot claimed on a frame cannot be handed back
///   on that frame.
///
/// Their product on a ring device was 16 MiB, which is **7.6 ms** at
/// [`BAR_WRITE_BYTES_PER_SEC`] — nearly twice
/// [`squallar_device_profile::constants::TARGET_FRAME_SERVICE`]. Nobody chose
/// 7.6 ms; it fell out. A third staging buffer, a change with nothing to do
/// with the frame thread, would silently have made this route 50% more
/// expensive.
///
/// And it was inverted between the two device classes. A ring device was
/// allowed 16 MiB of blocking while a **ringless** one was allowed 4 MiB —
/// four times as much for the class that has a copy engine to fall back on,
/// against the class where every byte is frame thread and there is no
/// alternative at all. Nothing intended that either; `bands_per_frame` is 2 on
/// one and 1 on the other, and the multiplication carried it.
///
/// # What each arm is now, and why they are derived differently
///
/// **Ring device: [`WHOLE_CROSSING_BYTES`]**, a quarter of the target frame at
/// the measured BAR bandwidth. Here the number is a real cost lever: a byte
/// displaced onto the bands is memcpy'd into a staging slot and pulled by the
/// copy engine at 24.7 GB/s against this route's 2.1 GB/s, so displacing it
/// makes it about twelve times cheaper *on the frame thread* and buys a frame
/// or two of arrival latency to do it.
///
/// **Ringless device: [`band_cap`], unchanged.** On that class both routes are
/// the same blocking `write_texture`, so this allowance does not change what a
/// frame costs per byte — only *when* it is paid. That is the dry-frame dial
/// [`squallar_device_profile::constants::BLOCKING_BAND_BYTES`] already swept
/// over 56 pan speeds, and this follows the sweep rather than re-deciding it.
/// The same arithmetic as the ring arm would say ~1 MiB there (a quarter of
/// 4 ms at the ~1 GB/s out of wasm linear memory the section above prices);
/// taking it would halve a web frame's worst blocking spend and would delay
/// basemap tiles after a pan, and **no frame-time instrument exists on either
/// web target** to say which wins — `run_tier2.sh` gates behaviour, not
/// milliseconds. So it is stated here and not taken.
///
/// # Nothing is starved, and what it costs when the budget bites
///
/// [`goes_whole`] reads this same figure, so a delta at the threshold always
/// crosses on a fresh frame. Past it a delta is filed as bands, and
/// `whole_budget <= band_cap` on both arms
/// (`a_delta_the_budget_displaces_is_at_most_one_band`) — so a displaced delta
/// is **one** band and costs one drain slot, not `ceil(bytes / band)` of them.
/// The drain moves [`bands_per_frame`] bands a frame and breaks after any frame
/// on which it allocated a texture, so the arrival it buys is a frame or two,
/// and a scene that files new rasters faster than that grows
/// [`TextureUploads::pending`] — which is published every frame as the
/// `upload pending` census family and is the figure to read if a layer ever
/// appears late after a pan.
const fn whole_budget(capable: bool) -> usize {
    if capable {
        WHOLE_CROSSING_BYTES
    } else {
        band_cap(capable)
    }
}

/// Bands one frame moves, by device capability. See
/// [`TextureUploads::bands_per_frame`], which is this with the flag read off
/// `self`.
const fn bands_per_frame(capable: bool) -> usize {
    if capable { DMA_BANDS_PER_FRAME } else { 1 }
}

/// [`goes_whole`], bounded by what this frame has already spent on the route.
///
/// The font atlas is exempt from the budget — banding it draws broken labels,
/// which is what [`is_font_atlas`] exists to prevent — but it is *charged* to
/// it, so a frame that spends its budget on a doubling defers the rest rather
/// than adding to it.
fn crosses_whole_now(id: egui::TextureId, capable: bool, bytes: usize, spent: usize) -> bool {
    is_font_atlas(id)
        || (goes_whole(capable, bytes) && spent.saturating_add(bytes) <= whole_budget(capable))
}

/// Consecutive frames the ring may decline a band before it is pushed across by
/// `write_texture` regardless.
///
/// Waiting a frame is the design, but a slot whose `map_async` errored never
/// comes back, and at depth 2 two such slots are a permanent refusal. Running
/// out of patience costs one band's [`UPLOAD_BAND_BYTES`] on the frame thread
/// rather than a pane that never draws.
const DECLINE_PATIENCE: u32 = 4;

/// What this renderer's texture uploads have actually moved.
///
/// **Product telemetry, not a campaign instrument.** It is always on, it has no
/// feature gate and no debug arm, and every field is a `u64` add on a path that
/// was already touching the same cache line. The renderer owns one, so the
/// numbers are scoped to a device and an adapter rather than to a process — the
/// scope every other figure this module quotes is already in.
///
/// # Denominator
///
/// **Every texture delta egui hands this renderer**, not only the overlay
/// rasters: the font atlas, the basemap tiles, the legend ramps and the cross
/// sections are all in here. That is deliberate — this is the sink, and what it
/// counts is what the device actually paid for. The overlay-attributable slice
/// is a different instrument with a different denominator; see
/// `squallar_egui::overlay_cache::ledger`.
///
/// # Why a zero here is readable
///
/// [`Self::deltas`] is the non-vacuity floor. Every byte figure below is zero
/// on a renderer that has been shown nothing, and zero is also what an upload
/// path that had silently stopped moving bytes would read; the two are only
/// distinguishable because a delta that was *filed* is counted whatever route
/// it then took. `deltas == 0` is "egui handed this renderer nothing";
/// `deltas > 0 && bytes() == 0` is a sink that stopped working. A gate that
/// reads the byte total without reading this one cannot tell them apart —
/// the lesson `worker_port::account` records for the reply transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadTotals {
    /// Texture deltas filed, by any route. See the type's own note: this is
    /// what makes a zero byte count readable.
    pub deltas: u64,
    /// Bytes handed whole to `Renderer::update_texture` — every delta at or
    /// under [`whole_budget`] for an id this module does not own, and
    /// the font atlas at any size (see [`is_font_atlas`]), bounded per frame
    /// by that same figure. **A
    /// routing figure and a subset of [`Self::blocking_bytes`], never added to
    /// it**: `update_texture` is `write_texture` on the frame's own queue, so
    /// these bytes are blocking too, whatever the device.
    pub whole_bytes: u64,
    /// Bands [`TextureUploads::drain`] moved. The non-vacuity partner of the
    /// two banded byte figures the way [`Self::deltas`] is of all of them: a
    /// raster filed as bands but never drained shows `bands == 0` with the
    /// delta counted.
    pub bands: u64,
    /// Bytes the copy engine pulled out of a staging slot (see
    /// [`crate::staging_ring`]). Measured 24.7 GB/s against the BAR window's
    /// 2.1 GB/s, and they cost the frame a memcpy rather than a blocking host
    /// write. Always banded.
    pub staged_bytes: u64,
    /// Every byte `write_texture` pushed through the BAR window on the frame
    /// thread — whole deltas through `Renderer::update_texture` and bands on
    /// a device with no ring or past [`DECLINE_PATIENCE`] alike. **This is
    /// the figure that is frame time on every device.** It is classified by
    /// the path the bytes took, never by whether their delta straddled
    /// [`UPLOAD_BAND_BYTES`]: spike B (2026-08-30) measured Firefox's
    /// ~8.51 MB pictures banded and counted ~13 GB blocking while Chromium's
    /// ~7.57 MB pictures went whole and counted ~0.1 GB — the same ringless
    /// traffic, opposite readings, flipped by 32 px of canvas width.
    pub blocking_bytes: u64,
    /// **`wgpu::Texture`s [`TextureUploads::drain`] created**, and the
    /// non-vacuity partner of [`Self::paced_creations`]: a leg reading zero
    /// paced creations out of zero creations says the drain never ran, not
    /// that the pacing never bound.
    ///
    /// Creations only — the deltas this module hands to
    /// `Renderer::update_texture` allocate nothing here, and neither does a
    /// band into a texture egui already owns.
    pub creations: u64,
    /// Bytes of the textures counted in [`Self::creations`], four to a texel.
    /// What [`TEXTURE_CREATE_BUDGET_BYTES`] is spent in.
    pub creation_bytes: u64,
    /// **Creations that happened on a frame that had already made one** — the
    /// subset of [`Self::creations`] the drain's old unconditional
    /// allocate-break refused, and so the exact count of bands
    /// [`TEXTURE_CREATE_BUDGET_BYTES`] let through.
    ///
    /// A subset of `creations`, never added to it. Bounded above by
    /// `creations - frames the drain ran on`, and on a ring device by
    /// `bands_per_frame - 1` per frame.
    pub paced_creations: u64,
}

impl UploadTotals {
    /// Every byte this renderer has put on the GPU, by any route. The two
    /// terms are disjoint: a byte either blocked the frame thread or was
    /// pulled by the copy engine.
    pub fn bytes(&self) -> u64 {
        self.staged_bytes + self.blocking_bytes
    }

    /// Bytes that crossed as bands rather than whole.
    pub fn banded_bytes(&self) -> u64 {
        self.bytes() - self.whole_bytes
    }

    /// Count `bytes` handed whole to `Renderer::update_texture`: one
    /// `write_texture` on the frame's own queue, so whole AND blocking.
    /// The one ledger arithmetic for that route — [`TextureUploads::file`]
    /// and the host-test seam both call it, so they cannot drift apart.
    fn count_whole_write(&mut self, bytes: u64) {
        self.whole_bytes += bytes;
        self.blocking_bytes += bytes;
    }

    /// Count one texture creation of `bytes`, `paced` naming whether the frame
    /// had already made one — see [`Self::paced_creations`].
    fn count_creation(&mut self, bytes: u64, paced: bool) {
        self.creations += 1;
        self.creation_bytes += bytes;
        if paced {
            self.paced_creations += 1;
        }
    }

    /// Count one band of `bytes`, `staged` naming the route that moved it.
    /// The one ledger arithmetic for the banded routes, shared the way
    /// [`Self::count_whole_write`] is.
    fn count_band(&mut self, bytes: u64, staged: bool) {
        self.bands += 1;
        if staged {
            self.staged_bytes += bytes;
        } else {
            self.blocking_bytes += bytes;
        }
    }

    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    fn progress(&self) -> u64 {
        // Creations included: a drain that moved only blank pages counts no
        // band, and without this term a frame of them reads as no progress at
        // all.
        self.deltas + self.bands + self.creations
    }
}

/// egui's texture deltas, moved across in bounded bands.
pub struct TextureUploads {
    /// Built **eagerly**, at construction, on a device that can have one: the
    /// first upload through a cold ring measured 10.04 ms against 1.4 ms for
    /// later ones (two buffer creations, two `map_async`es and 16.9 MiB of
    /// first-touch page faults).
    ring: Option<Ring>,
    /// Whether this device could have a ring at all. See
    /// [`crate::staging_ring::device_has_ring`].
    capable: bool,
    /// Textures this module allocated and therefore owns. Ownership is sticky:
    /// egui holds a 1×1 stand-in under the same id, so routing a later delta for
    /// an owned id back to `Renderer::update_texture` would copy a full-size
    /// image into a 1×1 texture. Every arm decides on `owned.contains_key`.
    owned: HashMap<egui::TextureId, wgpu::Texture>,
    /// Bands still to move, oldest first, so a raster that arrived earlier
    /// completes before one that arrived later starts.
    pending: VecDeque<Band>,
    /// Ids every texel of whose latest delta has reached the GPU — **every** id
    /// this module is shown, not only the banded ones, or "no, because I never
    /// banded it" would be a hold that never ends. [`Self::free`] takes an id
    /// out as egui retires it, so this holds one key per live texture.
    delivered: HashSet<egui::TextureId>,
    /// What this renderer has actually moved. See [`UploadTotals`].
    totals: UploadTotals,
    /// Bytes this frame has already pushed through the whole-crossing route.
    /// Reset by [`Self::apply`], spent by [`Self::file`], bounded by
    /// [`whole_budget`]. A `usize` because that is what it is compared against.
    whole_spent: usize,
    /// What the device is holding right now. See [`ResidentTextures`] — a
    /// level, maintained at the sites below that create, replace and free,
    /// and the answer [`UploadTotals`] structurally cannot give.
    resident: ResidentTextures,
    /// [`UploadTotals::progress`] at the last line [`Self::report`] logged, so
    /// a frame that moved nothing costs one `u64` compare and says nothing.
    reported: u64,
    /// **The high-water mark of [`Self::pending_level_bytes`]**, over the life
    /// of this renderer.
    ///
    /// The level itself is a level, published every frame and read by a census
    /// line every two seconds — and a picture crosses this queue in fewer
    /// frames than that, so the sampled family is 97-99 % zeros with a p50 of
    /// 0.0 and its whole-leg maximum is a lottery over which transient a tick
    /// happened to land on. Its own census note already says so
    /// (`squallar_egui::heap_census`'s `UPLOAD_PENDING_BYTES`: "a 206.75 MiB
    /// raster crossed this queue between two samples and it read 0 B at all
    /// 100 ticks"). **This is the figure that cannot miss one**, on the same
    /// argument `squallar_alloc::live_peak_bytes` is built on: a maximum taken
    /// where the quantity moves rather than on a clock.
    ///
    /// Taken once per [`Self::apply`], between the file loop and the drain,
    /// which is exactly where a frame's queue is at its largest: [`Self::file`]
    /// is the only thing that adds to it and [`Self::drain`] the only thing
    /// that takes away. One sweep of the queue per frame, allocation-free —
    /// the same sweep [`Self::publish_pending_level`] already runs at the other
    /// end of the frame, and see its note for the cost.
    pending_peak: u64,
    /// Whether this instance publishes into `squallar_egui::heap_census`.
    ///
    /// Those families are process-wide slots, each holding one level, so each
    /// has one owner. The renderer built on a device is it. A device-less
    /// fixture is not: a process can hold several of those at once, and their
    /// stores would land in one another's slot between a publisher's store
    /// and its reader's load. That is not a hypothetical - it is where
    /// `the_census_carries_the_resident_texture_level` read another test's
    /// zero, and a level published by whoever stored last is not a level.
    census_publisher: bool,
}

/// What is left of one texture's upload.
struct Band {
    id: egui::TextureId,
    /// The pixels. An `Arc` egui is already holding, so carrying it across
    /// frames costs a refcount rather than a copy.
    image: Arc<egui::ColorImage>,
    /// Where row 0 of `image` goes in the destination texture.
    origin: [u32; 2],
    /// Rows of `image` already moved.
    done: u32,
    /// Consecutive frames the ring has declined this. See [`DECLINE_PATIENCE`].
    declined: u32,
    /// Set while this raster's texture has not been created yet, holding the
    /// sampler its `load_texture` asked for. See [`TextureUploads::allocate`].
    allocate: Option<egui::TextureOptions>,
    /// **The size to allocate for a page whose content is never transferred**,
    /// and `None` for every band that carries pixels.
    ///
    /// A band in this arm holds a 1x1 stand-in rather than the page, so it is
    /// four bytes on [`TextureUploads::publish_pending_level`] where the page
    /// it replaces was 13,046,544. The size cannot be read off `image` for
    /// that reason, and is carried here instead. See
    /// `squallar_egui::blank_page`.
    blank: Option<[usize; 2]>,
}

impl Band {
    /// **The band a noted page files**: a 1x1 stand-in and the size to
    /// allocate at.
    ///
    /// The stand-in is what makes the cut visible on the census — this band is
    /// four bytes on [`TextureUploads::publish_pending_level`] where the page
    /// it replaces was 13,046,544 — and it is a real `ColorImage` rather than
    /// an `Option` so that every other reader of `Band::image` keeps working
    /// without an arm for a band that has none.
    fn blank_page(id: egui::TextureId, size: [usize; 2], options: egui::TextureOptions) -> Self {
        Self {
            id,
            image: Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::TRANSPARENT)),
            origin: [0, 0],
            done: 0,
            declined: 0,
            allocate: Some(options),
            blank: Some(size),
        }
    }
}

impl TextureUploads {
    /// Uploads for `device`, with a ring if it can have one.
    pub fn new(device: &wgpu::Device) -> Self {
        let capable = device_has_ring(device);
        Self {
            ring: capable.then(|| {
                Ring::new(
                    device,
                    UPLOAD_BAND_BYTES as wgpu::BufferAddress,
                    "squallar.raster.staging",
                )
            }),
            capable,
            owned: HashMap::new(),
            pending: VecDeque::new(),
            delivered: HashSet::new(),
            totals: UploadTotals::default(),
            resident: ResidentTextures::default(),
            reported: 0,
            whole_spent: 0,
            pending_peak: 0,
            census_publisher: true,
        }
    }

    /// Uploads with no device to ask, for a host test: bands, never DMA.
    ///
    /// **Publishes nothing.** See [`Self::census_publisher`]: a binary may
    /// hold any number of these at once, so none of them owns a census slot.
    /// The one fixture that does is [`Self::without_device_on_census`].
    #[cfg(test)]
    pub fn without_device() -> Self {
        Self {
            ring: None,
            capable: false,
            owned: HashMap::new(),
            pending: VecDeque::new(),
            delivered: HashSet::new(),
            totals: UploadTotals::default(),
            resident: ResidentTextures::default(),
            reported: 0,
            whole_spent: 0,
            pending_peak: 0,
            census_publisher: false,
        }
    }

    /// The device-less fixture that **owns** this process's census slots, for
    /// the one test that reads one back.
    ///
    /// Exactly one may exist per test binary, and that is this constructor's
    /// job rather than a convention a reader has to keep: a second call
    /// panics. A rule kept by comment is what failed here before - the
    /// asserting test said "one test, not several" and got one *asserting*
    /// test while every sibling went on publishing.
    #[cfg(test)]
    pub fn without_device_on_census() -> Self {
        static CLAIMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        assert!(
            !CLAIMED.swap(true, std::sync::atomic::Ordering::SeqCst),
            "the census slots already have a publisher in this binary; a \
             second one stores between the first's publish and its read",
        );
        Self {
            census_publisher: true,
            ..Self::without_device()
        }
    }

    /// Whether this device can stage through host memory at all.
    pub fn has_ring(&self) -> bool {
        self.capable
    }

    /// **Whether bands are still queued**, and so whether the next frame has
    /// already been bought before anything else asks for one.
    ///
    /// A banded upload spends several frames on purpose: the queue exists so a
    /// raster reaches the GPU without one frame paying for all of it. Those
    /// frames are the queue's, and until this was readable from outside they
    /// were charged to whichever unrelated claim happened to be standing when
    /// they landed — see `squallar_app::frame_need::WakeClaim::Upload`.
    pub fn uploads_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// **The most this queue has ever held at once**, in host bytes. See
    /// [`Self::pending_peak`] — a high-water mark, never a sample, and so the
    /// figure a residency cut on the staging path is scored against.
    pub fn pending_peak_bytes(&self) -> u64 {
        self.pending_peak
    }

    /// Raise [`Self::pending_peak`] to this instant's level. A max and never
    /// a store: the whole point of the figure is that it does not fall when
    /// the drain empties the queue between two readers.
    fn note_pending_peak(&mut self) {
        self.pending_peak = self.pending_peak.max(self.pending_level_bytes());
    }

    /// Bands this may move in one frame.
    fn bands_per_frame(&self) -> usize {
        bands_per_frame(self.capable)
    }

    /// File this frame's deltas and move what the budget allows.
    pub fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        renderer: &mut Renderer,
        set: &[(egui::TextureId, egui::epaint::ImageDelta)],
    ) -> bool {
        // Per frame, and `apply` is called once per frame. Reset here rather
        // than in `file`, which is the thing being bounded.
        self.whole_spent = 0;
        for (id, delta) in set {
            self.file(device, queue, renderer, *id, delta);
        }
        // **Here and not after the drain**: this is the instant the queue is
        // at its largest on this frame. See [`Self::pending_peak`].
        self.note_pending_peak();
        self.drain(device, queue, encoder, renderer);
        self.publish_pending_level();
        self.publish_resident_level();
        self.uploads_pending()
    }

    /// Route one delta: egui's own path, or a queue of bands.
    fn file(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        id: egui::TextureId,
        delta: &egui::epaint::ImageDelta,
    ) {
        let egui::epaint::ImageData::Color(image) = &delta.image;
        // Counted here rather than on either arm below, so that it counts the
        // same thing whatever route the delta then takes. See [`UploadTotals`]:
        // this is what makes a zero byte total readable.
        self.totals.deltas += 1;
        // A queued band counts as ownership even before the drain allocates:
        // until then the renderer holds only the 1×1 stand-in.
        let mine = self.owned.contains_key(&id) || self.pending.iter().any(|band| band.id == id);

        // Not already ours, and either small-and-within-budget or the font
        // atlas: an overlay under this device's whole-delta limit goes through
        // `update_texture` untouched while the frame's `whole_budget` holds,
        // and the atlas does at any size — see `crosses_whole_now`. On a
        // ringless device the limit is the blocking band, so a
        // web-picture-sized raster spreads over frames instead of spending one
        // frame whole; so now does the N+1st loop frame of a dispatch, which
        // used to spend a whole frame each with nothing counting them.
        if !mine && crosses_whole_now(id, self.capable, image.as_raw().len(), self.whole_spent) {
            renderer.update_texture(device, queue, id, delta);
            self.whole_spent = self.whole_spent.saturating_add(image.as_raw().len());
            self.totals.count_whole_write(image.as_raw().len() as u64);
            if delta.pos.is_none() {
                // egui allocated a texture of exactly this image and dropped
                // whatever it held under `id`. A delta WITH a `pos` is charged
                // nothing on purpose: it writes into the texture already there
                // and changes no resident byte — which is how a raster atlas
                // page reads as one allocation of its full size rather than as
                // the sum of the tiles written into it.
                self.resident.egui_allocated(id, image.size);
            }
            // Whole, on this frame's queue, before anything can draw it.
            self.delivered.insert(id);
            return;
        }

        // **A page whose first upload carries no information.**
        // `squallar_egui::raster_atlas` mints an atlas page through
        // `load_texture`, which takes its size from an image, so the page
        // arrives as 13,046,544 B of transparent pixels on the shipped class —
        // over `whole_budget` on both arms, banded, and holding all of itself
        // on this queue until its last band crosses. The producer says it
        // carries nothing (`squallar_egui::blank_page`), so the texture is
        // allocated at the delta's size and the image is dropped here.
        //
        // Claimed before the whole-crossing arm because a page is never small
        // enough to reach it, and claimed *once*: the flag is removed by
        // `take`, so a later re-allocation under the same id is a real image
        // and takes the ordinary route.
        if delta.pos.is_none() && !mine && squallar_egui::blank_page::take(id) {
            self.seed(device, queue, renderer, id, delta.options);
            self.pending.retain(|band| band.id != id);
            self.owned.remove(&id);
            self.resident.owned_dropped(id);
            // Allocated on the frame the drain first has budget for it, the
            // same deferral every whole delta takes — six 217 MB creations on
            // one frame is what that deferral exists to stop, and a page is a
            // creation like any other. The 1x1 stand-in is what the queue
            // then holds instead of the page: `publish_pending_level` charges
            // this band four bytes where it charged 13,046,544.
            self.delivered.remove(&id);
            self.pending
                .push_back(Band::blank_page(id, image.size, delta.options));
            return;
        }

        let mut allocate = None;
        if delta.pos.is_none() {
            // A whole image: ours to allocate, but not now — six 217 MB
            // creations measured 27.26 ms on one frame. The drain allocates on
            // the frame that first has budget for a band, so they spread.
            self.seed(device, queue, renderer, id, delta.options);
            self.pending.retain(|band| band.id != id);
            // The texture this replaces goes now. egui's bind group still holds
            // a view of it, so wgpu keeps it alive until the drain rebinds.
            self.owned.remove(&id);
            self.resident.owned_dropped(id);
            allocate = Some(delta.options);
        } else if !mine {
            // A large *partial* into a texture egui allocated: take the texture
            // over, so the bind group needs no rebind.
            let Some(existing) = renderer.texture(&id).and_then(|held| held.texture.clone()) else {
                // No texture under this id: hand it back and let
                // `update_texture`'s own panic name the fault. Nothing is
                // charged — a partial delta allocates no texture, and this arm
                // does not return.
                renderer.update_texture(device, queue, id, delta);
                return;
            };
            // The take-over: the texture is egui's and is already charged on
            // egui's side. Filing an owned charge here would count one
            // `wgpu::Texture` twice.
            self.owned.insert(id, existing);
        }

        // Filed as bands, so texels egui has handed over are not on the GPU yet.
        self.delivered.remove(&id);

        let origin = delta.pos.unwrap_or([0, 0]);
        self.pending.push_back(Band {
            id,
            image: Arc::clone(image),
            origin: [origin[0] as u32, origin[1] as u32],
            done: 0,
            declined: 0,
            allocate,
            blank: None,
        });
    }

    /// Allocate a texture of `size` and make egui's renderer paint `id` with it.
    fn allocate(
        &mut self,
        device: &wgpu::Device,
        renderer: &mut Renderer,
        id: egui::TextureId,
        size: [usize; 2],
        options: egui::TextureOptions,
    ) -> wgpu::Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squallar.raster"),
            size: wgpu::Extent3d {
                width: size[0] as u32,
                height: size[1] as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Format and the first two usages must match what
            // `Renderer::update_texture` would have created.
            format: wgpu::TextureFormat::Rgba8Unorm,
            // `COPY_SRC` is the only way `tests/raster_upload_gpu.rs` can read
            // a band back; a wrong stride shears the picture silently.
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });

        renderer.update_egui_texture_from_wgpu_texture_with_sampler_options(
            device,
            &texture.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler_descriptor(options),
            id,
        );
        self.owned.insert(id, texture.clone());
        self.resident.owned_allocated(id, size);
        texture
    }

    /// Put a 1×1 stand-in under `id`, so egui has something to paint it with
    /// until [`Self::allocate`] runs.
    fn seed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        id: egui::TextureId,
        options: egui::TextureOptions,
    ) {
        if renderer.texture(&id).is_some() {
            return;
        }
        let seed = egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([1, 1], egui::Color32::TRANSPARENT),
            options,
        );
        renderer.update_texture(device, queue, id, &seed);
        // egui really allocated one, and it outlives the take-over: the
        // `update_egui_texture_from_wgpu_texture_with_sampler_options` in
        // [`Self::allocate`] replaces the bind group and leaves egui's own
        // `Texture.texture` exactly where it is until `free_texture` runs.
        self.resident.egui_allocated(id, [1, 1]);
    }

    /// Move as many bands as the frame's budget allows.
    fn drain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        renderer: &mut Renderer,
    ) {
        // **What this frame has already spent creating textures**, against
        // [`TEXTURE_CREATE_BUDGET_BYTES`]. Bytes and not a count, because the
        // cost of a creation is its size and the break this replaced charged
        // a 2,995,200 B picture what a 216,796,176 B one costs.
        let mut created = 0u64;
        for _ in 0..self.bands_per_frame() {
            let Some(mut band) = self.pending.pop_front() else {
                break;
            };
            // **A blank page: allocate, transfer nothing, deliver.** The
            // texture a device hands back is zero-filled, which is the
            // `Color32::TRANSPARENT` the transfer would have written; and no
            // unleased slot is ever sampled, so the two agree twice over. This
            // costs the frame one texture creation and counts as an allocating
            // frame like any other, so the `break` below still paces them.
            if let Some(size) = band.blank {
                self.allocate(
                    device,
                    renderer,
                    band.id,
                    size,
                    band.allocate.unwrap_or_default(),
                );
                self.delivered.insert(band.id);
                // A blank page is a creation and nothing else, so it is priced
                // as one — the same [`TEXTURE_CREATE_BUDGET_BYTES`] the drain's
                // other allocating arm spends, rather than a whole frame's
                // drain for a page that transferred no texels at all.
                let paced = created > 0;
                let bytes = (size[0] as u64).saturating_mul(size[1] as u64) * 4;
                created = created.saturating_add(bytes);
                self.totals.count_creation(bytes, paced);
                if created >= TEXTURE_CREATE_BUDGET_BYTES {
                    break;
                }
                continue;
            }
            let mut allocated = false;
            // The bytes of the texture this iteration created, for the budget
            // above — four to a texel, the same arithmetic
            // [`resident`](crate::egui_renderer::texture_upload::resident)
            // charges a texture at.
            let mut created_bytes = 0u64;
            let texture = match band.allocate.take() {
                // The raster's own texture, created on the frame that first has
                // budget for a band of it rather than on the frame it arrived.
                Some(options) => {
                    allocated = true;
                    let size = band.image.size;
                    created_bytes = (size[0] as u64).saturating_mul(size[1] as u64) * 4;
                    self.allocate(device, renderer, band.id, size, options)
                }
                None => {
                    let Some(texture) = self.owned.get(&band.id).cloned() else {
                        // The id was freed while its bands waited. Dropping them
                        // is the whole of the cleanup: the texture went with the
                        // free.
                        continue;
                    };
                    texture
                }
            };
            let Some(plan) = BandPlan::of(
                band.image.width(),
                band.image.height() as u32,
                band.done,
                band_cap(self.capable),
            ) else {
                // A finished or degenerate band, dropped rather than requeued —
                // and *delivered*: "no rows will ever land" answering "not yet"
                // would hold the pane's previous picture for the session.
                self.delivered.insert(band.id);
                continue;
            };

            let staged = self.capable && self.stage_band(device, encoder, &texture, &band, &plan);
            let moved = if staged {
                true
            } else if !self.capable || band.declined + 1 >= DECLINE_PATIENCE {
                write_band(queue, &texture, &band, &plan);
                true
            } else {
                false
            };

            if moved {
                // Counted where the bytes move, and split by the route that
                // moved them: `staged` cost the frame a memcpy, the other arm
                // cost it a blocking host write.
                self.totals.count_band(plan.bytes() as u64, staged);
                band.done += plan.rows;
                band.declined = 0;
                let done = band.done >= plan.height;
                if done {
                    // The last band. Its copy is on the queue this frame submits
                    // and the answer is read on a later frame.
                    self.delivered.insert(band.id);
                } else {
                    self.pending.push_front(band);
                }
                if allocated {
                    // **Charged at its size, and checked after.** Creating the
                    // texture is 4.82 ms for a 7362² raster against ~0.7 ms
                    // for a band by DMA, so a frame that created one that
                    // large has spent its budget — but the same statement made
                    // about *any* creation pinned the drain at one band a
                    // frame on a scene of ordinary pictures, and half a ring
                    // of capacity went unused for the life of the process.
                    // See [`TEXTURE_CREATE_BUDGET_BYTES`] for the measured
                    // cost of that.
                    //
                    // After, so a frame always moves at least one band however
                    // large its texture is.
                    // **The fires counter**: a creation on a frame that had
                    // already made one is exactly what the old break refused,
                    // so `paced` is the count of bands this change let
                    // through and nothing else.
                    let paced = created > 0;
                    created = created.saturating_add(created_bytes);
                    self.totals.count_creation(created_bytes, paced);
                    if created >= TEXTURE_CREATE_BUDGET_BYTES {
                        break;
                    }
                }
            } else {
                // The ring is behind. Put it back and stop: every other band
                // this frame would be told the same thing by the same ring.
                band.declined += 1;
                self.pending.push_front(band);
                break;
            }
        }
    }

    /// Memcpy one band into a staging slot and start its copy, or say `false`.
    fn stage_band(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        band: &Band,
        plan: &BandPlan,
    ) -> bool {
        let ring = self.ring.get_or_insert_with(|| {
            Ring::new(device, plan.staged_bytes(), "squallar.raster.staging")
        });
        // Only an image with a row wider than a whole band can ask for this; see
        // [`Self::new`].
        ring.fit(device, plan.staged_bytes());
        let Some(slot) = ring.claim(device) else {
            return false;
        };

        {
            let mut view = slot.buffer().get_mapped_range_mut(..plan.staged_bytes());
            let mut rest = view.slice(..);
            for row in 0..plan.rows {
                let (this, next) = rest.split_at(plan.padded_row as usize);
                // The padding past the real row is left as it was; the copy
                // reads none of it and wgpu zero-initialised the allocation.
                this.into_slice(..plan.row_bytes)
                    .copy_from_slice(plan.source_row(band, row));
                rest = next;
            }
        }
        slot.buffer().unmap();
        // **The frame's own encoder, and no submission of our own.** The copy
        // is recorded ahead of the pass that samples the texture, in the same
        // command buffer, so the order the draw needs is the order it was
        // recorded in.
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: slot.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // The padded stride, not the row's own: a buffer copy is
                    // held to `COPY_BYTES_PER_ROW_ALIGNMENT` (a 7362 px raster
                    // is 29448 bytes a row against a 29696-byte stride).
                    bytes_per_row: Some(plan.padded_row),
                    rows_per_image: Some(plan.rows),
                },
            },
            plan.destination(texture, band),
            plan.extent(),
        );
        true
    }

    /// **The frame's encoder, carrying every band this frame recorded, has
    /// been submitted**: ask the ring for its mappings back.
    ///
    /// Call once per frame, after the submission. A ring that is never told
    /// runs out of mapped slots, [`Self::stage_band`] answers `false`, and the
    /// bands take the blocking `write_texture` route this path exists to avoid
    /// — degraded, never wrong.
    pub fn after_submit(&mut self) {
        if let Some(ring) = self.ring.as_mut() {
            ring.remap_submitted();
        }
    }

    /// Publish what [`Self::pending`] is holding on this instance's own heap,
    /// for `squallar_egui::heap_census`.
    ///
    /// # Denominator
    ///
    /// **The whole pixel buffer of every distinct image with a band still
    /// queued**, at `as_raw().len()`, counted once however many bands name it.
    /// Not the bytes still to move: [`Self::drain`] advances `done` and
    /// nothing shrinks the `Arc<egui::ColorImage>` behind the band, so a
    /// raster one row from finished is still holding all of itself — which is
    /// the whole reason this family is on the census rather than being read
    /// off [`UploadTotals`], whose figures are cumulative flow and never
    /// residency. Distinct by `Arc::ptr_eq`, so two bands of one image are one
    /// charge; charging each band its image would multiply-count a partial
    /// delta stream that shares a buffer.
    ///
    /// The bytes are egui's `Arc` and this queue is what keeps it alive: egui
    /// drops its own `TexturesDelta` at the end of the frame that produced it.
    /// Nothing here is the GPU's — the texture the bands are filling is not on
    /// this figure and is not on the census's page total either.
    ///
    /// # What it costs
    ///
    /// One allocation-free sweep, `n` slice-length reads and at most
    /// `n(n-1)/2` pointer compares, where `n` is the number of large deltas
    /// egui has filed and not yet finished — a band the drain re-queues is the
    /// same entry, not a new one, so `n` counts outstanding rasters and not
    /// their bands. Run once off [`Self::apply`] and once off [`Self::free`],
    /// never per band and never inside the drain loop.
    ///
    /// A running total paid at each push and pop would be O(1) and was not
    /// taken: the queue is mutated in [`Self::file`] (a retain and a push), in
    /// [`Self::drain`] (a pop, two pushes and a drop) and in [`Self::free`]
    /// (a retain), and each of those would owe a `ptr_eq` scan of its own to
    /// decide whether the charge is the last one for its image. A level that
    /// drifts because one site forgot to pay is a worse instrument than a
    /// sweep that cannot drift.
    fn publish_pending_level(&self) {
        let bytes = self.pending_level_bytes();
        if self.census_publisher {
            squallar_egui::heap_census::set_upload_pending_bytes(bytes);
        }
    }

    /// The figure [`Self::publish_pending_level`] publishes, as a value.
    ///
    /// Split out so a suite can read it without a census to publish to — and
    /// as **one** implementation rather than two: a test-only copy of this
    /// sweep would be a mutual-consistency pin over two spellings, green while
    /// the shipped one drifted.
    pub(crate) fn pending_level_bytes(&self) -> u64 {
        self.pending
            .iter()
            .enumerate()
            .filter(|(seen, band)| {
                !self
                    .pending
                    .iter()
                    .take(*seen)
                    .any(|earlier| Arc::ptr_eq(&earlier.image, &band.image))
            })
            .map(|(_, band)| band.image.as_raw().len() as u64)
            .sum()
    }

    /// Publish what the DEVICE is holding in egui's texture population, for
    /// `squallar_egui::heap_census`.
    ///
    /// # Denominator
    ///
    /// See the `resident` module: the pixel bytes of every `wgpu::Texture` alive under
    /// an egui `TextureId`, at four bytes a texel, both sides of an id that
    /// has two. **Not on the census's page total**, and the line says so:
    /// these bytes are the device's, and the residual the census exists to
    /// produce is against one wasm instance's linear memory.
    ///
    /// # What it costs
    ///
    /// One bool test, one field read and one `Relaxed` store. The level is
    /// maintained at the create, replace and free sites above; nothing walks
    /// the population, so this may sit on the frame thread's own path the way
    /// [`Self::publish_pending_level`] does.
    fn publish_resident_level(&self) {
        if self.census_publisher {
            squallar_egui::heap_census::set_gpu_texture_bytes(self.resident.bytes());
        }
    }

    /// Forget everything egui retired this frame.
    pub fn free(&mut self, ids: &[egui::TextureId]) {
        for id in ids {
            self.owned.remove(id);
            // What keeps [`Self::delivered`] the size of the live texture set
            // rather than the size of the session.
            self.delivered.remove(id);
            // `EguiRenderer::free_textures` calls `Renderer::free_texture` for
            // the same ids in the same breath, so both sides of the charge go
            // here. **This is the falling half of the level**; without it the
            // figure is another running total.
            self.resident.freed(*id);
        }
        self.pending.retain(|band| !ids.contains(&band.id));
        // A page retired before its allocation delta was filed would otherwise
        // sit in the producer's list for the life of the session; this is what
        // bounds it. Harmless for every id that was never noted.
        squallar_egui::blank_page::forget(ids);
        self.publish_pending_level();
        self.publish_resident_level();
    }

    /// Whether every texel egui has handed over for `id` has reached the GPU.
    pub fn is_delivered(&self, id: egui::TextureId) -> bool {
        self.delivered.contains(&id)
    }

    /// Bands not yet moved, for a test that wants to say how far along an upload
    /// is without a GPU to ask.
    #[cfg(test)]
    pub fn pending_bands(&self) -> usize {
        self.pending.len()
    }

    /// Queue an ordinary whole-image band the way [`Self::file`] does, for the
    /// non-triviality half of the blank-page suite: without it a level that
    /// answered four bytes for everything would pass.
    #[cfg(test)]
    pub fn file_band_for_test(&mut self, id: egui::TextureId, image: Arc<egui::ColorImage>) {
        self.pending.push_back(Band {
            id,
            image,
            origin: [0, 0],
            done: 0,
            declined: 0,
            allocate: Some(egui::TextureOptions::default()),
            blank: None,
        });
    }

    /// Queue what [`Self::file`] queues for a noted page, through the same
    /// constructor, and publish the level over it.
    ///
    /// **The constructor and the sweep are the two halves under test**; what
    /// this stands in for is the pair of device calls around them (`seed` and
    /// the drain's `allocate`), which no test in this module can reach without
    /// a GPU. A fixture that built the `Band` itself would prove nothing about
    /// the band `file` actually files, which is the whole question.
    #[cfg(test)]
    pub fn file_blank_page_for_test(&mut self, id: egui::TextureId, size: [usize; 2]) {
        self.pending
            .push_back(Band::blank_page(id, size, egui::TextureOptions::default()));
    }

    /// File a resident charge for `id` the way [`Self::allocate`] does, with
    /// no device to create a texture on — the host-test seam for the level.
    /// Calls the same [`ResidentTextures::owned_allocated`] the real path
    /// calls, so the seam and the drain share one arithmetic; what the test
    /// then drives is the REAL [`Self::free`].
    #[cfg(test)]
    pub fn note_resident_for_test(&mut self, id: egui::TextureId, size: [usize; 2]) {
        self.resident.owned_allocated(id, size);
    }

    /// Put `id` in the delivered set without a device to deliver it with.
    #[cfg(test)]
    pub fn mark_delivered_for_test(&mut self, id: egui::TextureId) {
        self.delivered.insert(id);
    }

    /// Record a moved band the way [`Self::drain`] does, with no device to
    /// move it with — for the host test of the report's cadence. Calls the
    /// same [`UploadTotals::count_band`] the drain calls, so the seam and the
    /// real path share one arithmetic; the real arithmetic on a real adapter
    /// is `the_upload_ledger_counts_every_byte_of_a_banded_raster_once`.
    #[cfg(test)]
    pub fn note_band_for_test(&mut self, bytes: u64, staged: bool) {
        self.totals.count_band(bytes, staged);
    }

    /// Record one whole delta the way [`Self::file`]'s `update_texture` arm
    /// does, with no device to move it with — the host-test seam for the
    /// classification. Calls the same [`UploadTotals::count_whole_write`] the
    /// real arm calls; the real path on a real adapter is
    /// `a_ringless_byte_is_called_blocking_on_both_sides_of_the_band_straddle`,
    /// which is `#[ignore]`d because it needs an adapter -- run it with
    /// `cargo test -p squallar-gpu --test raster_upload_gpu -- --ignored`.
    #[cfg(test)]
    pub fn note_whole_delta_for_test(&mut self, bytes: u64) {
        self.totals.deltas += 1;
        self.totals.count_whole_write(bytes);
    }

    /// **What the device is holding in egui's texture population right now**,
    /// in bytes. A level, not a running total: it falls on a free and on a
    /// replace. See the `resident` module for the denominator and what it leaves
    /// out.
    pub fn resident_texture_bytes(&self) -> u64 {
        self.resident.bytes()
    }

    /// The maintained level against a walk of the ledger's own maps — the
    /// drift check a maintained total needs and a derived one does not.
    /// Tests only; the walk is what [`Self::resident_texture_bytes`] exists to
    /// avoid.
    #[cfg(test)]
    pub fn walked_resident_texture_bytes(&self) -> u64 {
        self.resident.walked_bytes()
    }

    /// The texture this module allocated for `id`, if it owns one.
    pub fn texture(&self, id: egui::TextureId) -> Option<&wgpu::Texture> {
        self.owned.get(&id)
    }

    /// What this renderer has moved since it was built. See [`UploadTotals`].
    pub fn totals(&self) -> UploadTotals {
        self.totals
    }

    /// [`Self::totals`], but only when something has moved since the last time
    /// this was asked — so a caller can report the line on a frame that
    /// uploaded something and stay silent on one that did not.
    ///
    /// **This crate cannot report the line itself**: `squallar-gpu` declares no
    /// `log` dependency and has never held a `log::` call. The counters
    /// therefore live where the bytes move and the sentence lives where a
    /// logger exists, which is the same split
    /// `squallar_volumetric::degrade::note_surface_loss_with_volume` already
    /// uses for the surface-loss count.
    ///
    /// An idle frame costs one `u64` add and one compare.
    pub fn totals_if_moved(&mut self) -> Option<UploadTotals> {
        let progress = self.totals.progress();
        if progress == self.reported {
            return None;
        }
        self.reported = progress;
        Some(self.totals)
    }
}

/// One frame's worth of one band: which rows, and how they sit in a buffer.
struct BandPlan {
    /// Rows to move now.
    rows: u32,
    /// Rows in the whole image, so the caller can tell "done" from "more".
    height: u32,
    /// Bytes of real texels in a row.
    row_bytes: usize,
    /// That, rounded up to [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`].
    padded_row: u32,
}

impl BandPlan {
    /// What to move of a `width` × `height` image with `done` rows already
    /// across at a band budget of `cap` bytes, or `None` when there is
    /// nothing left to move.
    fn of(width: usize, height: u32, done: u32, cap: usize) -> Option<Self> {
        if width == 0 || done >= height {
            return None;
        }
        let row_bytes = width * 4;
        let padded_row = u32::try_from(row_bytes)
            .ok()?
            .next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        // Against the **padded** stride, so a band never needs a slot larger
        // than its cap and the ring can be built at a fixed size before any
        // raster is known.
        let capped = (cap / padded_row as usize).max(1);
        let rows = u32::try_from(capped).unwrap_or(u32::MAX).min(height - done);
        Some(Self {
            rows,
            height,
            row_bytes,
            padded_row,
        })
    }

    /// Bytes of real texels this plan moves.
    fn bytes(&self) -> usize {
        self.row_bytes * self.rows as usize
    }

    /// Bytes a staging buffer must be to hold it, padding included.
    fn staged_bytes(&self) -> wgpu::BufferAddress {
        u64::from(self.padded_row) * u64::from(self.rows)
    }

    /// Row `row` of this plan, as bytes of `band`'s image.
    fn source_row<'a>(&self, band: &'a Band, row: u32) -> &'a [u8] {
        let start = (band.done + row) as usize * self.row_bytes;
        &band.image.as_raw()[start..start + self.row_bytes]
    }

    /// Where in `texture` this plan's first row lands.
    fn destination<'a>(
        &self,
        texture: &'a wgpu::Texture,
        band: &Band,
    ) -> wgpu::TexelCopyTextureInfo<'a> {
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: band.origin[0],
                y: band.origin[1] + band.done,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        }
    }

    fn extent(&self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: (self.row_bytes / 4) as u32,
            height: self.rows,
            depth_or_array_layers: 1,
        }
    }
}

/// Push one band across with `queue.write_texture`.
fn write_band(queue: &wgpu::Queue, texture: &wgpu::Texture, band: &Band, plan: &BandPlan) {
    let start = band.done as usize * plan.row_bytes;
    let bytes = &band.image.as_raw()[start..start + plan.bytes()];
    queue.write_texture(
        plan.destination(texture, band),
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            // Packed, not padded: `write_texture` repacks internally.
            bytes_per_row: Some(plan.row_bytes as u32),
            rows_per_image: Some(plan.rows),
        },
        plan.extent(),
    );
}

/// egui's own sampler, rebuilt.
fn sampler_descriptor(options: egui::TextureOptions) -> wgpu::SamplerDescriptor<'static> {
    let filter = |f: egui::TextureFilter| match f {
        egui::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        egui::TextureFilter::Linear => wgpu::FilterMode::Linear,
    };
    let address = match options.wrap_mode {
        egui::TextureWrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        egui::TextureWrapMode::Repeat => wgpu::AddressMode::Repeat,
        egui::TextureWrapMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
    };
    wgpu::SamplerDescriptor {
        label: Some("squallar.raster.sampler"),
        mag_filter: filter(options.magnification),
        min_filter: filter(options.minification),
        address_mode_u: address,
        address_mode_v: address,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
