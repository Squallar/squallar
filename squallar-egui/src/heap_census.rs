//! **What is holding the page's linear memory, family by family, in bytes.**
//!
//! # Why this exists
//!
//! The `huge` Tier-2 scene exhausts the 1 GiB wasm page heap on both browsers.
//! Every lever the application has is priced against a *model* of what the
//! scene should cost, and the models were built family by family — the
//! overlay picture batch, the loop pool's allowance, the tile cache's budget.
//! On 2026-09-02 the picture batch was capped to the byte and measured at
//! **167-215 MB of 1024 MiB, 17 to 21 %**, and the trap did not move. So the
//! remaining ~850 MB is held by something no budget term prices, and no
//! amount of tightening a priced family can reach it.
//!
//! This is the instrument for that question and nothing else. It is a set of
//! **levels** — bytes resident right now, set rather than added — one per
//! allocation family that can sit on the page's own linear memory. It answers
//! "who is holding it", not "how much has ever been made"; the running totals
//! in [`crate::overlay_cache::ledger`] and `squallar_gpu`'s `UploadTotals`
//! answer the other question and are never added to these.
//!
//! # The denominator, said once
//!
//! Every figure summed here is **bytes on ONE wasm instance's linear
//! memory** — the page's, where the frame thread runs. The rasterization
//! worker is a second instance with a second 1 GiB ceiling and its own heap;
//! nothing on this census is the worker's, and the two are never summed.
//!
//! **Two families are the GPU's and are carried outside the sum**: `tile
//! meshes` and `gpu textures`. They are on the line because the reader at the
//! trap wants them and there is nowhere else that is always on, and they are
//! out of [`Census::resident_total`] because the residual this module exists
//! to produce is against a `byteLength` reading of linear memory, which no
//! device byte is on. The line prints them last, after the residual, and
//! labels them. A caller that adds them to the total has broken the
//! instrument.
//!
//! [`Census::residual`] against a real `byteLength` reading is the finding
//! this module exists to produce. **It is not an error term.** It is every
//! family nobody has thought to count yet, plus the allocator's own
//! fragmentation and the module's static footprint, and a census that
//! accounts for 400 MB of 1024 must be reported as accounting for 400 MB of
//! 1024 — never as if the families it does name were the whole heap.
//!
//! # Shared ownership is double counted, on purpose
//!
//! Several families hold `Arc`s of the same decoded volume: the loop
//! download cache, the still-volume inventory, the derivation memo. A census
//! that tried to attribute each byte to exactly one owner would need a graph
//! walk on the frame thread and would still have to pick an owner
//! arbitrarily. So each family reports **what it would free if it were
//! emptied**, and the sum of the radar families is an upper bound on their
//! joint footprint rather than a partition of it. [`Census::radar_total`] is
//! spelled separately for exactly that reason, and the line says so.
//!
//! # What it costs
//!
//! Every write is one `Relaxed` store of a `u64` a caller already had. No
//! family is walked to produce a figure here — a family whose size needs a
//! walk (a decoded volume's radials) is priced ONCE where it arrives and
//! carries its own running total, so the census read is a handful of atomic
//! loads whatever the scene. That matters twice: it rides the frame thread's
//! telemetry tick, and it is read from the **allocation-error hook**, which
//! runs after the allocator has already refused and must not allocate.
//!
//! **A level that lives one frame is published where it moves, not on the
//! tick.** The tick is 2 s apart and runs after the frame's own receipts, so
//! a family whose bytes arrive and leave inside one frame — a render reply in
//! its channel — would read zero at every tick and zero in the hook. Such a
//! family is published by the code that moves the bytes (`renders in
//! flight`, like `upload pending` and `loans out` before it), or, where the
//! owner cannot see this module, read straight from the owner's own atomics
//! by [`census`] (`render pools`).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Bytes the census line may take in the allocation-error hook's stack
/// buffer.
///
/// It lives here, beside the format, because the two have to move together:
/// a family added to the line without this growing is a line silently cut at
/// the one moment it is the only evidence there is, and
/// `the_widest_line_fits_the_hooks_buffer` fails rather than letting that
/// land. Sized against every figure at `u64::MAX` and the longest instance
/// name, not against a plausible reading — and sized EXACTLY: the widest line
/// is this many bytes, with no headroom, so a family added without re-deriving
/// it is cut and the test says so. The arithmetic: twenty-three families (the
/// two GPU ones included), the resident total and the linear reading are
/// twenty-five `u64::MAX` figures at 20 digits apiece, the prose between them
/// under `rasterization worker` makes up the rest — and the residual is the
/// **`none` arm**: a reading of `u64::MAX - 1` against families that saturate
/// prints `residual none (families price above it)`, 27 bytes wider than the
/// `residual 0 B` that a reading of `u64::MAX` gives. The test measures all
/// three residual arms and takes the widest.
///
/// **One family costs `name.len() + 25`**: `", "` before it, the name, the
/// space, twenty digits and `" B"`. The `deferred drops` family added 39 to
/// the 768 before it; `overlay items` added 38 and `overlay parked` 39, so
/// 807 + 77 = 884; `render pools` adds 37 and `renders in flight` adds 42,
/// so 884 + 79 = 963 on the `residual 0 B` arm; `gpu textures` is 12
/// characters, so it adds `12 + 25 = 37`, and it lands in the GPU tail where
/// the same 37 is spelled `", gpu textures "` (15) + twenty digits + `" B"`
/// (2) — so 963 + 37 = 1000 on that arm; the `none` arm's 27 make 1027.
/// `chunk feed` is 10 characters, so it adds `10 + 25 = 35`: 1027 + 35 = 1062.
/// And `overlay pictures` was DELETED — the family priced a 64-byte plan
/// record as pixels — so by the same rule it takes its 16 characters and
/// `16 + 25 = 41` back out: 1062 - 41 = 1021.
/// `cached renders` is 14 characters, so it adds `14 + 25 = 39`: 1021 + 39 =
/// 1060, and `rasters shared` is 14 too: 1060 + 39 = 1099. This chain is a DERIVATION and not a record: every term in it moves
/// when a family is added or removed, so re-derive it rather than nudging the
/// constant, and let `the_widest_line_fits_the_hooks_buffer` be the check.
/// That test asserts `<=`, so a constant that is too LARGE passes quietly.
pub const CENSUS_LINE_CAPACITY: usize = 1099;

/// One family's level. A `u64` of bytes, `Relaxed` throughout: every reader
/// wants a recent figure, none wants a synchronised one, and a census torn
/// across two families is a census of two adjacent instants — which is what
/// it would be anyway.
macro_rules! families {
    (
        $($name:ident, $field:ident, $setter:ident, $doc:literal;)*
        @read $($rfield:ident = $read:expr, $rdoc:literal;)*
    ) => {
        $(
            #[doc = $doc]
            static $name: AtomicU64 = AtomicU64::new(0);

            #[doc = $doc]
            ///
            /// A level: set, never added.
            pub fn $setter(bytes: u64) {
                $name.store(bytes, Relaxed);
            }
        )*

        /// Every family's level, read together.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct Census {
            $(
                #[doc = $doc]
                pub $field: u64,
            )*
            $(
                #[doc = $rdoc]
                pub $rfield: u64,
            )*
        }

        /// Read every level. The `@read` families are read from the atomics
        /// their owner maintains, at this instant, with no publish between.
        pub fn census() -> Census {
            Census {
                $($field: $name.load(Relaxed),)*
                $($rfield: $read,)*
            }
        }

        /// Put every published level back to zero. Tests only: nothing
        /// shipped resets a level, because a level is not a running total.
        /// The `@read` families are their owners' to empty.
        #[cfg(test)]
        pub(crate) fn reset() {
            $($name.store(0, Relaxed);)*
        }
    };
}

families! {
    LOOP_SCAN_BYTES, loop_scan_bytes, set_loop_scan_bytes,
        "Decoded Level II volumes the loop download cache is holding, summed \
         over every site and timestamp. Priced once per volume at arrival.";
    LOOP_L3_BYTES, loop_l3_bytes, set_loop_l3_bytes,
        "Level III product bytes the loop cache is holding, paired one per \
         frame with the volumes above.";
    STILL_SCAN_BYTES, still_scan_bytes, set_still_scan_bytes,
        "Decoded volumes the still-pane inventory and the per-site latest \
         cache are holding together.";
    DERIVE_MEMO_BYTES, derive_memo_bytes, set_derive_memo_bytes,
        "Derived volumes the derivation memo is holding.";
    LOOP_FRAME_SCAN_BYTES, loop_frame_scan_bytes, set_loop_frame_scan_bytes,
        "Decoded volumes the stored 2D loop frames are PINNING - each \
         plan-view frame's hover source holds the `Arc<Scan>` it was drawn \
         from so the readout can decode a gate on demand. Overlaps the loop \
         download cache while the entry lives, and is the only figure naming \
         these bytes once it is evicted.";
    RENDER_CACHE_BYTES, render_cache_bytes, set_render_cache_bytes,
        "Finished radar rasters the render cache is holding, CPU-side: the \
         `Color32` pixel buffers and their resident hover fields.";
    PANE_CACHED_RENDER_BYTES, cached_render_bytes, set_cached_render_bytes,
        "Finished plan-view rasters the PANES are holding for restore - each \
         `RenderDispatcher::pane_render[i].cached_render`, at its `Color32` \
         pixels and its hover field, the same two terms `render cache` prices. \
         Kept so a lost graphics context is an upload rather than a re-render \
         (`App::restore_cached_render`), which on mobile is a context that \
         goes without a suspend callback to warn of it. \
         DE-DUPLICATED ACROSS PANES and NOT across families: two panes showing \
         one raster hold one `Arc` and are counted once here, but the same \
         `Arc` is usually ALSO a `render cache` entry, and this figure and that \
         one then name the same bytes twice. \
         Until 2026-09-07 nothing named these bytes at all: at the empty \
         steady scene, with every overlay off, one pane held 216,796,176 B of \
         `Color32` and 5,281,920 B of hover - 211.8 MiB, 87 % of the whole \
         unaccounted heap - and `publish_heap_census` did not mention it. It \
         was found in the mapping walk rather than the census: one anonymous \
         VMA read 1,083,985,920 B against `render pools` 867,184,704 B, and \
         the 216,796,176 B difference is exactly `side * side * 4` at the \
         7362 px raster the pools were sized for.";
    RASTER_SHARED_BYTES, raster_shared_bytes, set_raster_shared_bytes,
        "**Bytes `render cache` and `cached renders` BOTH name** - the \
         correction term that turns their sum into a range, and NOT a holder \
         of anything. Left out of [`Census::resident_total`] for that reason, \
         the way the two GPU families are: nothing on this heap is these \
         bytes a second time, they are one allocation two families counted. \
         Two sources of sharing, and it measures both rather than bounding \
         them: a pane's `cached_render` is usually an `Arc` clone of a live \
         `render cache` entry's image, and `RenderCache` prices its entries \
         one at a time while several keys can hold ONE `Arc` - which is what \
         `PlanViewUploads::handle` exists to arrange, so it is the ordinary \
         case and not an edge. \
         A MEASURED UNION, not a `max`: the app walks both holders and adds \
         each distinct `Arc` once, which the radar families cannot do because \
         their holders are spread across three crates. So \
         [`Census::raster_floor`] is exact where [`Census::radar_floor`] is a \
         bound. \
         **THE QUESTION IT ANSWERS**, said exactly, because a wider one is \
         easy to read into it: *how much do `render cache` and `cached \
         renders` name twice between them*. It is NOT the wider question of \
         how much of this heap's raster bytes are double-counted. A THIRD \
         family holds the same `Arc`s while a raster is banding - `upload \
         pending`, which is handed `Arc::clone(&render.image)` by \
         `apply_render_to_pane` and keeps it until the last band lands - and \
         this walk cannot see it: `TextureUploads` lives in `squallar_gpu` \
         and the dispatcher that does the walk cannot reach it. So while an \
         upload is in flight the pair's floor is right and the CENSUS's floor \
         is still an over-estimate; see [`Census::resident_floor`]. \
         Measured, empty steady scene, 2026-09-07: `render cache` read \
         444,156,192 B for 39 ticks - exactly two entries at 222,078,096 B - \
         while the allocator held one buffer. Evicting the first freed 0 B \
         and the second freed 204.0 MiB, which is how the sharing was found. \
         The census over-reported by 211.8 MiB for 80 s and `unaccounted` \
         under-reported by the same, on the one scene precise enough \
         (~1.5 MiB) for that to be the whole story.";
    RENDER_IN_FLIGHT_BYTES, render_in_flight_bytes, set_render_in_flight_bytes,
        "Finished plan-view rasters between the render thread and the frame \
         thread: the `ColorImage` a render's reply built, priced at its \
         `Color32` pixels, from the moment it is sent until the frame thread \
         receives it. Disjoint from `render cache` and `upload pending`, \
         which price the same image only after receipt installs it and the \
         renderer bands it; at the tick this family was built for, that \
         image was 206.8 MiB and both of those read it as nothing. \
         Published at the seam - the reply closure as it sends, the receipt \
         as it settles - because a reply lives one frame and a tick would \
         read it as zero almost always. Covers all three producers: the pane \
         renders, the adjacent-tilt speculation and the loop frames. \
         A FLOOR, and under-counts in one direction only: a receipt clears a \
         pane's whole cell, so a second reply queued behind the first, or a \
         pane forgotten while a reply is still in the channel, stops being \
         priced while its raster is still resident. It never prices an image \
         that has gone.";
    OVERLAY_GRID_BYTES, overlay_grid_bytes, set_overlay_grid_bytes,
        "Decoded overlay SOURCE data the layer handlers are holding BESIDE \
         their state - MRMS mosaics, GMGSI granules, HRRR model grids, their \
         retained staging blocks, and the GLM lightning layer's S3 granule \
         cache. Not the pictures rasterized from them, and disjoint from \
         `overlay items`: this family is what a handler holds in its own \
         fields, that one is what an `OverlayState` installed.";
    OVERLAY_ITEM_BYTES, overlay_item_bytes, set_overlay_item_bytes,
        "Decoded overlay ITEM data the feature layers are holding - the \
         lightning flashes, station observations, alerts, storm reports, \
         discussions and outlook polygons every `OverlayState` installed. \
         Priced at install and DISJOINT from `overlay grids`, which the \
         gridded layers answer instead.";
    OVERLAY_PARKED_BYTES, overlay_parked_bytes, set_overlay_parked_bytes,
        "Overlay item data and built paint inputs that have been RETIRED and \
         are waiting on the discard seam - a replaced generation, and the \
         memo rows a rollover or an eviction parked. Disjoint from the two \
         above: what is parked is what the live figures no longer count, \
         except where a parked row shares an `Arc` with a live one and prices \
         only its pointers.";
    LOOP_FRAME_BYTES, loop_frame_bytes, set_loop_frame_bytes,
        "What the finished 2D loop frames hold on THIS heap. A radar or \
         section frame's pixels are the GPU's behind a `TextureHandle`; what \
         is counted is the CPU side each frame keeps beside it.";
    UPLOAD_PENDING_BYTES, upload_pending_bytes, set_upload_pending_bytes,
        "Images the renderer is still banding to the GPU. A band crosses \
         ~4 MiB a frame where no staging ring exists - which is every browser \
         - so a picture is held whole for as many frames as it has bands. \
         Already DE-DUPLICATED WITHIN ITSELF: `publish_pending_level` sweeps \
         the queue distinct by `Arc::ptr_eq`, so two bands of one image are \
         one charge. It was the only family here that did that, and `render \
         cache` is the one that did not - the pattern was in the tree and \
         unadopted, which is how 211.8 MiB of phantom survived. \
         It OVERLAPS the two raster families while a radar raster is in \
         flight: the `Arc` it holds is the one `apply_render_to_pane` handed \
         `ctx.load_texture`, which is the same `Arc` `render cache` and \
         `cached renders` hold. `raster shared` does NOT span it, so a reader \
         adding all three has counted one buffer three times. \
         **A LEVEL, SAMPLED.** It is maintained per frame off `apply` and \
         `free`, so it is right at every instant - but the census line is \
         written every 2 s, and on both FLOOR legs a 206.75 MiB raster \
         crossed this queue between two samples and it read 0 B at all 100 \
         ticks. `gpu textures` went 2,097,152 -> 218,893,332 B in one tick \
         and never moved again. The instrument is not blind - it reads 44 \
         distinct non-zero values up to 569,465,072 B at the E2 scene - so \
         its zero answers *what is pending right now* and must never be \
         quoted against *does this scene ever hold upload bytes*.";
    TILE_BODY_BYTES, tile_body_bytes, set_tile_body_bytes,
        "Undecoded vector-tile bodies the wasm-only body cache is holding. \
         Zero on every native target, where the cache does not exist.";
    TILE_PARSED_BYTES, tile_parsed_bytes, set_tile_parsed_bytes,
        "Parsed vector tiles the shared parsed cache is holding.";
    TILE_CACHE_BYTES, tile_cache_bytes, set_tile_cache_bytes,
        "Styled tiles the per-role tile caches are holding, both roles summed.";
    LOAN_OUTSTANDING_BYTES, loan_outstanding_bytes, set_loan_outstanding_bytes,
        "Job payloads this instance has lent the peer and not been released \
         from. Each is a `to_bytes` buffer the loan book holds until a \
         `RELEASE` arrives; see `squallar_web::shared_loan`.";
    JOB_IN_FLIGHT_BYTES, job_in_flight_bytes, set_job_in_flight_bytes,
        "Job payloads this instance is executing right now - the head and the \
         resident payload it was handed, held for as long as the row decodes \
         and rasterizes. Published by the rasterization worker, the only \
         instance that runs a job off the wire; zero on the page, where it is \
         a real zero.";
    TILE_MESH_BYTES, tile_mesh_bytes, set_tile_mesh_bytes,
        "Tile mesh buffers the renderer is holding. **GPU**, kept beside the \
         others for the reader; [`Census::resident_total`] leaves it out.";
    GPU_TEXTURE_BYTES, gpu_texture_bytes, set_gpu_texture_bytes,
        "Pixel bytes the DEVICE is holding in egui's texture population right \
         now, at four bytes a texel of every live texture - the radar \
         rasters, the overlay pictures, the loop frames, the raster-atlas \
         pages and the glyph atlas, all of which stop being counted by every \
         other family here the moment they upload. A LEVEL: it falls on a \
         free and on a replace, which is what `squallar_gpu`'s `UploadTotals` \
         structurally cannot do - those are cumulative flow, and one leg \
         moved 21.7 GB of uploads against a device holding a few hundred MB. \
         **GPU**, like the tile meshes above, and left out of \
         [`Census::resident_total`] for the same reason. It does NOT name \
         `squallar_volumetric`'s raymarch textures, which enter no egui \
         population and which no family here measures; the host side of those \
         is `volume store`. Maintained at the create, replace and free sites \
         in `squallar_gpu::egui_renderer::texture_upload`, never walked. \
         **A reading must name the BACKEND as well as the arm** - a Linux web \
         leg is GL, a Mac one is WebGPU on Metal - because upload is backend \
         behaviour. The LEVEL is not: it is hooked on egui's delta seam above \
         wgpu, both upload routes charge through the same arithmetic, and \
         neither backend is dark.";
    VOLUME_STORE_BYTES, volume_store_bytes, set_volume_store_bytes,
        "The 3D volume store's voxel grids on the HOST heap - each grid's \
         index plane, value plane and transfer table. The GPU textures built \
         from them are the device's and are not this figure.";
    @read
    deferred_drop_bytes = squallar_device_profile::discard_ledger::in_flight_bytes(),
        "Evicted and NOT YET FREED: what `squallar_worker::offload::discard` \
         has handed away and nothing has finished freeing, at the prices its \
         payloads were filed at - an evicted volume's sweeps at their gate \
         bytes, anything filed unpriced at its own struct size. A floor, and \
         zero exactly when nothing is in flight. Bytes an eviction has \
         already taken out of the families above and that live bytes will not \
         give back until the drop lands, so a reader of live bytes waits on \
         this before calling a fall settled. \
         BOTH ROUTES, which is the point: a discard rides the job pool's \
         one-thread `rd-free` lane on native and the frame-paced deferred \
         queue on wasm, and each target's other route is structurally zero. \
         Summed, not picked, because a payload is in exactly one. \
         This family used to read the deferred queue ALONE, which on native \
         is the route that never runs, so it reported 0 while the lane held \
         the real bytes. That is the failure worth naming: a family \
         structurally zero on a whole target does not read as broken, it \
         reads as HEALTHY - nothing in flight, nothing to worry about - and \
         the sentence above telling a reader to wait on it turns that into an \
         immediate all-clear at exactly the moment, an allocation refusal, \
         when the reader most needs the truth. 604 MiB reported as 0 is not a \
         missing measurement but a confident wrong one, which is worse: a \
         missing family shows up in the residual, a false zero does not. \
         READ THROUGH, not published, for `render pool`'s reason: `census()` \
         reads the ledger's counters at the instant of the read, so the 2 s \
         tick and the allocation-error hook see the same truth. A tick \
         publish would miss a burst almost every time one happened - the \
         window is 27.9 ms for 604 MiB natively and about 0.67 s on wasm, \
         where the drain pays out in 500 us slices. The 604 MiB is the \
         desktop resident cap's ARITHMETIC - 8 volumes at the 74.63 MiB \
         documented maximum - exercised by a probe, not an observed peak. \
         The window scales with ALLOCATION COUNT, not bytes: a volume is \
         ~43k buffers whether it is median or maximum shape, and freeing \
         391 MiB took 25.3 ms against 604 MiB's 27.9 ms. Do not size this \
         against megabytes.";
    chunk_feed_bytes = squallar_radar::chunks::feed_bytes() as u64,
        "Decoded volumes the REAL-TIME CHUNK FEED is holding - the sealed \
         cuts of the volume each live site is assembling, the deep-copied \
         snapshot built from them, and the closed volumes a poller has \
         parked. Two whole volumes a live site at the peak, and until \
         2026-09-07 no family named a byte of it: the feed's stores are \
         three levels below `App` and nothing in the tree could price them. \
         READ THROUGH, like `render pools`: radar cannot see this module, \
         and the levels are maintained inside the polling round, off the \
         frame thread, where the bytes actually move. A FLOOR - the radials \
         of a cut still arriving are not priced, at most a sixteenth of a \
         volume - and an UPPER bound against `still scans`, which prices the \
         same `Arc` once a round delivers it.";
    render_pool_bytes = squallar_radar::render::parked_bytes() as u64,
        "Render buffers `squallar_radar` is PARKING between renders - the \
         plan-view cell buffer, RGBA texture and value grid, and the section \
         planes - each at its capacity while no render has it out, zero while \
         one does. Disjoint from `render cache`: a parked buffer is what the \
         next render draws into, not a finished raster anyone is showing. \
         READ THROUGH, not published: `census()` reads radar's own maintained \
         atomics (four relaxed loads, each stored under its slot's lock), so \
         the tick and the allocation-error hook see the slots as they are at \
         the instant of the read. Radar cannot see this module, and a publish \
         from the 2 s tick would catch a buffer parked between two renders \
         almost never. PER INSTANCE, like every family here: the page and the \
         rasterization worker each instantiate this module and radar's slots \
         alike, so each reports the buffers ITS OWN renders parked and the \
         two are NEVER summed - the worker rasterizes, so its figure is \
         usually the larger, and it appears on the worker's own census line.";
}

impl Census {
    /// **The families that are on THIS instance's linear memory**, summed.
    ///
    /// Not "the page's": the census is a set of statics, and the page, the
    /// rasterization worker and the tile lane each instantiate the module
    /// with a set of their own. Almost every family here is published by the
    /// application, which runs on the page, so a worker's census reads zero
    /// for them — and that is a true statement about what this instrument
    /// knows, not a bug. The line names the instance for exactly that
    /// reason.
    ///
    /// [`Census::tile_mesh_bytes`] and [`Census::gpu_texture_bytes`] are left
    /// out because they are the GPU's — a residual is `byteLength` less this
    /// total, and no device byte is on a `byteLength` — and
    /// the radar families are summed as their own upper bound
    /// ([`Self::radar_total`]) rather than partitioned — see the module note
    /// on shared ownership. Saturating, because a sum of levels read at
    /// adjacent instants has no reason to be trusted to fit if one of them is
    /// a wild reading.
    ///
    /// **An UPPER bound, and not a partition** — see [`Self::radar_total`].
    /// [`Self::resident_floor`] is the other end of the same reading, and a
    /// caller saying how much of the heap this census accounts for needs
    /// both: the truth is between them and this instrument cannot say where.
    pub fn resident_total(&self) -> u64 {
        [
            self.radar_total(),
            self.render_cache_bytes,
            self.cached_render_bytes,
            self.render_pool_bytes,
            self.render_in_flight_bytes,
            self.overlay_grid_bytes,
            self.overlay_item_bytes,
            self.overlay_parked_bytes,
            self.loop_frame_bytes,
            self.upload_pending_bytes,
            self.tile_body_bytes,
            self.tile_parsed_bytes,
            self.tile_cache_bytes,
            self.loan_outstanding_bytes,
            self.volume_store_bytes,
            self.job_in_flight_bytes,
            self.deferred_drop_bytes,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }

    /// **The decoded-volume families, summed as an upper bound.**
    ///
    /// Five holders keep `Arc`s of the same volumes — the loop download
    /// cache, the still inventory, the derivation memo, and the stored loop
    /// frames' hover sources — so a volume two of them name is counted twice
    /// here. Stated rather than corrected: the figure that matters for "what
    /// would emptying these free" is this one, and the partition it is not
    /// would take a graph walk on the frame thread.
    pub fn radar_total(&self) -> u64 {
        self.loop_scan_bytes
            .saturating_add(self.loop_l3_bytes)
            .saturating_add(self.still_scan_bytes)
            .saturating_add(self.derive_memo_bytes)
            .saturating_add(self.loop_frame_scan_bytes)
            .saturating_add(self.chunk_feed_bytes)
    }

    /// **The two plan-view raster families, summed as an upper bound.**
    ///
    /// `render cache` and `cached renders` hold `Arc`s of the same images, so
    /// a raster both name is counted twice here. [`Self::raster_floor`] is
    /// the other end, and unlike [`Self::radar_floor`] it is exact.
    pub fn raster_total(&self) -> u64 {
        self.render_cache_bytes
            .saturating_add(self.cached_render_bytes)
    }

    /// **The same two families with the sharing taken out**: the bytes the
    /// allocator actually granted for the rasters those two hold.
    ///
    /// Exact, not a bound. `raster shared` is a measured union rather than
    /// the largest-member floor [`Self::radar_floor`] has to settle for, so
    /// where the radar families give a range this gives the figure.
    pub fn raster_floor(&self) -> u64 {
        self.raster_total().saturating_sub(self.raster_shared_bytes)
    }

    /// **What this census does not account for**, against a real reading of
    /// the instance's linear memory.
    ///
    /// `None` when the families already exceed the reading, which is not an
    /// impossible state and is not an error: the radar families are an upper
    /// bound (see [`Self::radar_total`]), so a scene whose volumes are widely
    /// shared can price above the heap. A caller printing this must print
    /// the reading beside it — a residual with no denominator is the exact
    /// mistake this module exists to stop.
    pub fn residual(&self, linear_bytes: u64) -> Option<u64> {
        linear_bytes.checked_sub(self.resident_total())
    }

    /// **The decoded-volume families as a de-duplicated LOWER bound**: the
    /// largest single one.
    ///
    /// The five holders share `Arc`s, so their sum ([`Self::radar_total`]) is
    /// an upper bound. The floor of a union of overlapping sets is the
    /// largest member — every byte the biggest holder names is resident
    /// whatever the others share with it — and that needs no graph walk and
    /// no frame-thread cost, which is why this is the de-duplicated figure
    /// the census can actually publish.
    ///
    /// It is a **bound and not an estimate**. Where the families are
    /// disjoint the truth is [`Self::radar_total`]; where they all name one
    /// volume the truth is this. Nothing here says which, and a reader that
    /// quotes one end without the other has picked a number rather than read
    /// one.
    pub fn radar_floor(&self) -> u64 {
        [
            self.loop_scan_bytes,
            self.loop_l3_bytes,
            self.still_scan_bytes,
            self.derive_memo_bytes,
            self.loop_frame_scan_bytes,
            self.chunk_feed_bytes,
        ]
        .into_iter()
        .fold(0u64, u64::max)
    }

    /// **The families summed with the radar overlap taken out**: the
    /// de-duplicated lower bound on what this census accounts for.
    ///
    /// The same set as [`Self::resident_total`] with [`Self::radar_floor`]
    /// in place of [`Self::radar_total`]. Every non-radar family is already
    /// documented disjoint from its neighbours, so this end moves only
    /// where the sharing actually is.
    ///
    /// **A third overlap is known and NOT corrected**, so this is a floor of
    /// what the census can see and not of the heap. `upload pending` holds
    /// the same `Arc<ColorImage>` as the two raster families for as long as a
    /// raster is banding, and no walk can span all three: `TextureUploads` is
    /// `squallar_gpu`'s and the raster walk is the app dispatcher's. It was
    /// 0 B on every FLOOR leg measured and 72.8 MiB at the E2 scene, which is
    /// the size of the caveat, not a correction to it.
    pub fn resident_floor(&self) -> u64 {
        self.resident_total()
            .saturating_sub(self.radar_total())
            .saturating_add(self.radar_floor())
            .saturating_sub(self.raster_shared_bytes)
    }

    /// **Bytes the allocator says are live that no family here names**, as a
    /// range.
    ///
    /// This is the figure the census exists to produce on a native arm,
    /// where there is no `byteLength` to take a residual against and
    /// [`Self::residual`] therefore has no denominator at all. `live` is
    /// `squallar_alloc::live_bytes()` — bytes granted and not handed back.
    ///
    /// Two ends because the families have two ends: `.0` is
    /// `live − resident_total` (the least that can be unaccounted, since the
    /// families are an upper bound) and `.1` is `live − resident_floor` (the
    /// most). Each is `None` where the families price above `live` rather
    /// than wrapping — a real state when the radar families double-count, and
    /// exactly the reading that says the upper bound is doing so.
    ///
    /// **It is not an error term.** It is every family nobody has thought to
    /// count yet, plus whatever the census prices at zero. A caller printing
    /// it must print `live` beside it.
    pub fn unaccounted(&self, live: u64) -> (Option<u64>, Option<u64>) {
        (
            live.checked_sub(self.resident_total()),
            live.checked_sub(self.resident_floor()),
        )
    }
}

/// **The census as one line**, written through [`core::fmt::Write`] so the
/// same format serves both callers.
///
/// The two callers are a `String` on the telemetry tick and a **fixed stack
/// buffer in the allocation-error hook**, where the heap has just refused and
/// a `format!` is the one thing that cannot be done. One function, so the
/// line a developer reads at the trap is byte-identical to the line the tick
/// writes and a scrape cannot come to depend on two spellings of it.
///
/// `linear` is the instance's own `byteLength`, which is what makes the
/// residual meaningful; `None` where it could not be read, and the line says
/// `unread` rather than printing a residual against a guess. `where` names
/// the instance, because the page and the worker run the same module and a
/// figure from the wrong one is worse than no figure.
///
/// Bytes, not MiB, and every field is always present: a real zero is a real
/// zero, and the `huge` leg's whole question is which family is not zero.
pub fn write_line<W: core::fmt::Write>(
    out: &mut W,
    census: &Census,
    linear: Option<u64>,
    instance: &str,
) -> core::fmt::Result {
    write!(
        out,
        "heap census ({instance}): loop scans {} B, loop l3 {} B, still scans {} B, \
         derive memo {} B, loop frame scans {} B, chunk feed {} B, \
         render cache {} B, cached renders {} B, rasters shared {} B, \
         render pools {} B, \
         renders in flight {} B, \
         overlay grids {} B, overlay items {} B, overlay parked {} B, loop frames {} B, \
         upload pending {} B, tile bodies {} B, tile parsed {} B, \
         tile cache {} B, loans out {} B, volume store {} B, jobs in flight {} B, \
         deferred drops {} B; resident total {} B of ",
        census.loop_scan_bytes,
        census.loop_l3_bytes,
        census.still_scan_bytes,
        census.derive_memo_bytes,
        census.loop_frame_scan_bytes,
        census.chunk_feed_bytes,
        census.render_cache_bytes,
        census.cached_render_bytes,
        census.raster_shared_bytes,
        census.render_pool_bytes,
        census.render_in_flight_bytes,
        census.overlay_grid_bytes,
        census.overlay_item_bytes,
        census.overlay_parked_bytes,
        census.loop_frame_bytes,
        census.upload_pending_bytes,
        census.tile_body_bytes,
        census.tile_parsed_bytes,
        census.tile_cache_bytes,
        census.loan_outstanding_bytes,
        census.volume_store_bytes,
        census.job_in_flight_bytes,
        census.deferred_drop_bytes,
        census.resident_total(),
    )?;
    match linear {
        Some(linear) => match census.residual(linear) {
            Some(residual) => write!(out, "{linear} B linear, residual {residual} B"),
            None => write!(
                out,
                "{linear} B linear, residual none (families price above it)"
            ),
        },
        None => write!(out, "unread linear, residual unknown"),
    }?;
    // Off the page total on purpose, and last so a reader cannot mistake them
    // for part of the sum: these bytes are the GPU's.
    write!(
        out,
        "; tile meshes {} B, gpu textures {} B (GPU, not in the total)",
        census.tile_mesh_bytes, census.gpu_texture_bytes,
    )
}

/// [`write_line`] into a `String`, for the telemetry tick.
pub fn line(census: &Census, linear: Option<u64>, instance: &str) -> String {
    let mut out = String::new();
    // Writing to a `String` is infallible; the `Result` is `fmt::Write`'s
    // shape, not a case.
    let _ = write_line(&mut out, census, linear, instance);
    out
}

// ---------------------------------------------------------------------------
// The process's own denominator.
// ---------------------------------------------------------------------------

/// **What the OS gave this process**, published as levels beside the families.
///
/// # Why this is here and not on the census line
///
/// The families above answer "who is holding the heap". These answer "how big
/// is the heap, and how much of the process is not it" — a different
/// denominator, so a different line, the same way `budget state:` and the
/// census are already kept apart. Adding them to [`write_line`] would also
/// grow the allocation-error hook's fixed buffer for figures that are `None`
/// on the one target that hook runs on.
///
/// # Why the census needed them at all
///
/// [`Census::residual`] is taken against a wasm `byteLength`. **On native
/// there is no such reading**, so `linear` is `None`, the line prints
/// `unread linear, residual unknown`, and the census has no denominator of
/// any kind. That is how a measured native scene came to sit at 3,108.6 MiB
/// resident with 2,234.6 MiB live and an in-app census whose own *upper*
/// bound was 1,667.0 MiB — 568 MiB below what the allocator said was live,
/// and 1,441 MiB below the resident set — with no line anywhere saying so.
///
/// # Levels, and the sample count that says how stale they are
///
/// Like every family here these are set, never added, and read as atomics so
/// the allocation-error hook can have them without allocating. Unlike a
/// family, **an RSS reading has no seam to publish at**: no code in this
/// process moves those bytes — the kernel does, on a page fault and on a
/// `MADV_DONTNEED` — so there is nothing to hook and a sample is the only
/// thing there is. [`ProcessCensus::samples`] and
/// [`ProcessCensus::walks`] say how many of each have ever been taken, so a
/// reader can tell a fresh reading from a stale one and a never-read one
/// from a zero.
mod process_levels {
    use super::AtomicU64;

    macro_rules! levels {
        ($($name:ident;)*) => {
            $(pub(super) static $name: AtomicU64 = AtomicU64::new(0);)*

            /// Put every process level back to zero. Tests only, like the
            /// families' own `reset`.
            #[cfg(test)]
            pub(super) fn reset() {
                $($name.store(0, super::Relaxed);)*
            }
        };
    }
    levels! {
        LIVE; RSS; ANON; FILE; SHMEM; THREADS; SAMPLES;
        MAIN_HEAP; ARENA; ARENAS; STACK; ANON_OTHER; NON_HEAP; THP; WALKS;
    }
}

use process_levels as lv;

/// The process denominator, read together. Bytes throughout except the three
/// counts, which say so in their names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessCensus {
    /// `squallar_alloc::live_bytes()` — granted and not handed back.
    pub live: u64,
    /// `VmRSS`. Zero *and* `samples == 0` means unread, not empty.
    pub rss: u64,
    /// `RssAnon` — the half the allocator lives on.
    pub anon: u64,
    /// `RssFile` — code, libraries, mapped data, **and the driver's device
    /// maps**.
    pub file: u64,
    /// `RssShmem`.
    pub shmem: u64,
    /// Threads, because glibc's arena count follows it.
    pub threads: u64,
    /// How many cheap samples have ever been taken. **Zero means unread.**
    pub samples: u64,
    /// `[heap]`, the main arena.
    pub main_heap: u64,
    /// glibc secondary arenas: **the arena-retention term**.
    pub arena: u64,
    /// How many of them.
    pub arenas: u64,
    /// `[stack]` and thread stacks.
    pub stack: u64,
    /// Anonymous mappings that are neither: the allocator's own `mmap`ed
    /// blocks, which is where a large buffer lands.
    pub anon_other: u64,
    /// Code, file maps, device maps and kernel pages — **the floor no
    /// amount of freeing weather data moves**.
    pub non_heap: u64,
    /// `AnonHugePages`. **Overlaps the anonymous terms and is not in any
    /// sum here**; see [`Self::rss_over_live`].
    pub thp: u64,
    /// How many expensive walks have ever been taken. **Zero means the
    /// breakdown terms are unwalked, not zero.**
    pub walks: u64,
}

impl ProcessCensus {
    /// Whether a cheap sample has ever landed. A `false` here means every
    /// byte figure below is unread rather than measured at zero.
    pub fn sampled(&self) -> bool {
        self.samples > 0
    }

    /// Whether an expensive walk has ever landed — the breakdown terms
    /// ([`Self::main_heap`] through [`Self::thp`]) are meaningless without it.
    pub fn walked(&self) -> bool {
        self.walks > 0
    }

    /// **What the process holds that the allocator never handed out**:
    /// `RSS − live`.
    ///
    /// `None` where `live` prices above the resident set, which is a real
    /// state and not an error — a heap whose pages have gone back to the
    /// kernel is exactly that, and a `0` would hide it.
    ///
    /// **This is not one thing.** It is the non-heap floor
    /// ([`Self::non_heap`]) plus the allocator's chunk headers, its arena
    /// retention ([`Self::arena`]) and the kernel's huge-page rounding
    /// ([`Self::thp`]) — and the last two **overlap each other**, because a
    /// huge page inside an arena is both. This instrument sizes all of them
    /// and attributes between the overlapping pair for none of them; see the
    /// module note on [`squallar_alloc::process`].
    pub fn rss_over_live(&self) -> Option<u64> {
        self.rss.checked_sub(self.live)
    }
}

/// **Publish the cheap reading.** ~11 µs and flat in RSS
/// (`squallar_alloc::process::resident`), so this may ride a frame.
///
/// `live` is passed rather than read here so a caller that already has it
/// does not take a second reading a few microseconds off the first.
pub fn publish_resident(live: u64, resident: Option<squallar_alloc::process::Resident>) {
    lv::LIVE.store(live, Relaxed);
    if let Some(r) = resident {
        lv::RSS.store(r.rss_bytes, Relaxed);
        lv::ANON.store(r.anon_bytes, Relaxed);
        lv::FILE.store(r.file_bytes, Relaxed);
        lv::SHMEM.store(r.shmem_bytes, Relaxed);
        lv::THREADS.store(u64::from(r.threads), Relaxed);
        lv::SAMPLES.fetch_add(1, Relaxed);
    }
}

/// **Publish the expensive walk.** `squallar_alloc::process::breakdown` is
/// **3.3 ms p50 and 5.8 ms p99 on a 7.2 GB process and grows with the
/// resident set** — a frame and a half at 250 Hz. Whoever calls this owes the
/// reader a thread that is not the frame thread; [`spawn_process_sampler`] is
/// the one this crate ships.
pub fn publish_breakdown(b: &squallar_alloc::process::Breakdown) {
    lv::MAIN_HEAP.store(b.main_heap_bytes, Relaxed);
    lv::ARENA.store(b.arena_bytes, Relaxed);
    lv::ARENAS.store(u64::from(b.arenas), Relaxed);
    lv::STACK.store(b.stack_bytes, Relaxed);
    lv::ANON_OTHER.store(b.anon_other_bytes, Relaxed);
    lv::NON_HEAP.store(b.non_heap_bytes(), Relaxed);
    lv::THP.store(b.thp_bytes, Relaxed);
    lv::WALKS.fetch_add(1, Relaxed);
}

/// Read the process denominator. Atomic loads only — hook-safe, like
/// [`census`].
pub fn process_census() -> ProcessCensus {
    ProcessCensus {
        live: lv::LIVE.load(Relaxed),
        rss: lv::RSS.load(Relaxed),
        anon: lv::ANON.load(Relaxed),
        file: lv::FILE.load(Relaxed),
        shmem: lv::SHMEM.load(Relaxed),
        threads: lv::THREADS.load(Relaxed),
        samples: lv::SAMPLES.load(Relaxed),
        main_heap: lv::MAIN_HEAP.load(Relaxed),
        arena: lv::ARENA.load(Relaxed),
        arenas: lv::ARENAS.load(Relaxed),
        stack: lv::STACK.load(Relaxed),
        anon_other: lv::ANON_OTHER.load(Relaxed),
        non_heap: lv::NON_HEAP.load(Relaxed),
        thp: lv::THP.load(Relaxed),
        walks: lv::WALKS.load(Relaxed),
    }
}

/// **Take the cheap reading now and publish it.** One `/proc` read and a
/// handful of stores; see [`publish_resident`] for the cost.
pub fn sample_process() {
    publish_resident(
        squallar_alloc::live_bytes().unwrap_or(0),
        squallar_alloc::process::resident(),
    );
}

/// **Start the one thread that takes both readings**, so that neither is on
/// the frame thread.
///
/// Idempotent — a second call does nothing, so every frame may call it and
/// only the first starts anything. After the first the whole cost at the call
/// site is one atomic load.
///
/// **Two cadences, because the two readings cost three orders of magnitude
/// apart.** The cheap resident reading (~11 µs, flat in RSS) is taken every
/// `sample_period`; the mapping walk (3.3 ms p50, 5.8 ms p99, growing with
/// RSS) every `walk_every`th sample. At the shipped 250 ms and 8 that is
/// 11 µs four times a second and 3.3 ms every two seconds, none of it on a
/// frame.
///
/// The thread sleeps between readings and holds no lock. It is native-only:
/// there is no `/proc` on wasm and no thread to spawn there either.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_process_sampler(sample_period: std::time::Duration, walk_every: u32) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        // A named thread: 141 of them were counted on the measured arm, and
        // one more that nobody can attribute is how that number got there.
        let spawned = std::thread::Builder::new()
            .name("squallar.mem.census".into())
            .spawn(move || {
                let mut n: u32 = 0;
                loop {
                    sample_process();
                    // The walk on the first iteration too, so a reader has a
                    // breakdown within one period rather than `walk_every` of
                    // them.
                    if walk_every > 0 && n.is_multiple_of(walk_every) {
                        if let Some(b) = squallar_alloc::process::breakdown() {
                            publish_breakdown(&b);
                        }
                        // **And say it**, on the walk's own cadence rather
                        // than the frame thread's telemetry tick - this
                        // thread is the only place that knows a fresh
                        // reading just landed, and it is not a frame.
                        //
                        // `debug!`, matching the quiet arm of the shell's
                        // `say_telemetry`: the figures are always collected,
                        // and a reader turns them up rather than the
                        // instrument shouting by default.
                        log::debug!("{}", process_line(&census(), &process_census(), "process"));
                    }
                    n = n.wrapping_add(1);
                    std::thread::sleep(sample_period);
                }
            });
        // A refusal to spawn leaves `samples` and `walks` at zero, which the
        // line prints as `rss unread` and `breakdown unwalked`. That is the
        // honest outcome, and an instrument is the last thing that should
        // panic.
        drop(spawned);
    });
}

/// **No `/proc` and no threads on wasm**, so the resident reading does not
/// exist there — but `live_bytes` does, and it is two atomic loads. This
/// publishes that much and leaves the rest reading `rss unread`, which is
/// what it is.
#[cfg(target_arch = "wasm32")]
pub fn spawn_process_sampler(_sample_period: core::time::Duration, _walk_every: u32) {
    publish_resident(squallar_alloc::live_bytes().unwrap_or(0), None);
}

/// The cadence the application runs the sampler at: a resident reading four
/// times a second and a mapping walk every two seconds. Named here, beside
/// the costs they are chosen against, rather than at the call site.
pub const PROCESS_SAMPLE_PERIOD: core::time::Duration = core::time::Duration::from_millis(250);

/// See [`PROCESS_SAMPLE_PERIOD`]: every eighth sample takes the walk.
pub const PROCESS_WALK_EVERY: u32 = 8;

/// Bytes [`write_process_line`] can take, for a caller writing it into a
/// fixed buffer.
///
/// Sized the way [`CENSUS_LINE_CAPACITY`] is — against every figure at
/// `u64::MAX` and the longest instance name, and **exactly**, with no
/// headroom, so a field added without re-deriving it is a cut line and
/// `the_widest_process_line_fits_its_buffer` says so rather than letting it
/// land.
///
/// The widest arm is the one where **both** the unaccounted range and the
/// `rss over live` term print their figures rather than their `none` prose:
/// the range's `unaccounted <20> B to <20> B` is wider than
/// `unaccounted none (families price above live)`, so the widest line is a
/// census whose families price *below* `live`. Seventeen `u64::MAX` figures
/// at 20 digits, the three counts among them, plus the prose — and the two
/// census ends it prints (`families` and `floor`) grow with the census, so a
/// family added there moves this too: `chunk feed` took it from 645 to 647.
pub const PROCESS_LINE_CAPACITY: usize = 647;

/// **The process denominator as one line.**
///
/// Three groups, in the order a reader needs them: what the allocator was
/// asked for and what this census could name of it; what the OS actually
/// gave; and the split of the difference — with the **overlapping** pair
/// marked, because arena retention and huge-page rounding are the same bytes
/// twice and a reader who adds them has double-counted.
pub fn write_process_line<W: core::fmt::Write>(
    out: &mut W,
    census: &Census,
    process: &ProcessCensus,
    instance: &str,
) -> core::fmt::Result {
    let (least, most) = census.unaccounted(process.live);
    write!(out, "process memory ({instance}): live {} B", process.live)?;
    // The census's two ends against the allocator, which is the whole point
    // of the line on a native arm.
    match (least, most) {
        (Some(least), Some(most)) => write!(
            out,
            ", families {} B floor {} B, unaccounted {least} B to {most} B",
            census.resident_total(),
            census.resident_floor(),
        ),
        _ => write!(
            out,
            ", families {} B floor {} B, unaccounted none (families price above live)",
            census.resident_total(),
            census.resident_floor(),
        ),
    }?;
    if !process.sampled() {
        return write!(out, "; rss unread");
    }
    write!(
        out,
        "; rss {} B, anon {} B, file {} B, shmem {} B, threads {}",
        process.rss, process.anon, process.file, process.shmem, process.threads
    )?;
    match process.rss_over_live() {
        Some(over) => write!(out, ", rss over live {over} B"),
        None => write!(out, ", rss over live none (live prices above rss)"),
    }?;
    if !process.walked() {
        return write!(out, "; breakdown unwalked");
    }
    write!(
        out,
        "; main heap {} B, arenas {} at {} B, stacks {} B, anon other {} B, \
         non-heap {} B, thp {} B (overlaps the anon terms, not in the sum), \
         samples {}, walks {}",
        process.main_heap,
        process.arenas,
        process.arena,
        process.stack,
        process.anon_other,
        process.non_heap,
        process.thp,
        process.samples,
        process.walks,
    )
}

/// [`write_process_line`] into a `String`, for the telemetry tick.
pub fn process_line(census: &Census, process: &ProcessCensus, instance: &str) -> String {
    let mut out = String::new();
    let _ = write_process_line(&mut out, census, process, instance);
    out
}

#[cfg(test)]
#[path = "heap_census/tests.rs"]
mod tests;
