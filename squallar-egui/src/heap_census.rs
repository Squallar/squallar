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
//! Every figure here is **bytes on ONE wasm instance's linear memory** — the
//! page's, where the frame thread runs. The rasterization worker is a second
//! instance with a second 1 GiB ceiling and its own heap; nothing on this
//! census is the worker's, and the two are never summed. A family whose
//! bytes live on the GPU (a `TextureHandle`'s pixels after upload) is not
//! here either: this census is what the *allocator* is holding.
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
/// it is cut and the test says so. The arithmetic: twenty-two families (the
/// GPU one included), the resident total and the linear reading are
/// twenty-four `u64::MAX` figures at 20 digits apiece, the prose between them
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
/// so 884 + 79 = 963 on the `residual 0 B` arm; the `none` arm's 27 make
/// 990.
pub const CENSUS_LINE_CAPACITY: usize = 990;

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
    OVERLAY_PICTURE_BYTES, overlay_picture_bytes, set_overlay_picture_bytes,
        "Overlay pictures this frame's dispatch has resident - the batch \
         WO-36 capped, at `width * height * 4`.";
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
         - so a picture is held whole for as many frames as it has bands.";
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
    DEFERRED_DROP_BYTES, deferred_drop_bytes, set_deferred_drop_bytes,
        "Evicted and NOT YET FREED: what `squallar_worker::offload::discard` \
         is holding for the frame-paced drain, at the prices its entries were \
         filed at - an evicted volume's sweeps at their gate bytes, anything \
         filed unpriced at its own struct size. A floor, and zero exactly when \
         the queue is empty. Bytes an eviction has already taken out of the \
         families above and that live bytes will not give back until the \
         drain reaches them, so a reader of live bytes waits on this before \
         calling a fall settled.";
    TILE_MESH_BYTES, tile_mesh_bytes, set_tile_mesh_bytes,
        "Tile mesh buffers the renderer is holding. **GPU**, kept beside the \
         others for the reader; [`Census::resident_total`] leaves it out.";
    VOLUME_STORE_BYTES, volume_store_bytes, set_volume_store_bytes,
        "The 3D volume store's voxel grids on the HOST heap - each grid's \
         index plane, value plane and transfer table. The GPU textures built \
         from them are the device's and are not this figure.";
    @read
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
    /// [`Census::tile_mesh_bytes`] is left out because it is the GPU's, and
    /// the radar families are summed as their own upper bound
    /// ([`Self::radar_total`]) rather than partitioned — see the module note
    /// on shared ownership. Saturating, because a sum of levels read at
    /// adjacent instants has no reason to be trusted to fit if one of them is
    /// a wild reading.
    pub fn resident_total(&self) -> u64 {
        [
            self.radar_total(),
            self.render_cache_bytes,
            self.render_pool_bytes,
            self.render_in_flight_bytes,
            self.overlay_picture_bytes,
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
         derive memo {} B, loop frame scans {} B, render cache {} B, render pools {} B, \
         renders in flight {} B, overlay pictures {} B, \
         overlay grids {} B, overlay items {} B, overlay parked {} B, loop frames {} B, \
         upload pending {} B, tile bodies {} B, tile parsed {} B, \
         tile cache {} B, loans out {} B, volume store {} B, jobs in flight {} B, \
         deferred drops {} B; resident total {} B of ",
        census.loop_scan_bytes,
        census.loop_l3_bytes,
        census.still_scan_bytes,
        census.derive_memo_bytes,
        census.loop_frame_scan_bytes,
        census.render_cache_bytes,
        census.render_pool_bytes,
        census.render_in_flight_bytes,
        census.overlay_picture_bytes,
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
    // Off the page total on purpose, and last so a reader cannot mistake it
    // for part of the sum: these bytes are the GPU's.
    write!(
        out,
        "; tile meshes {} B (GPU, not in the total)",
        census.tile_mesh_bytes
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

#[cfg(test)]
#[path = "heap_census/tests.rs"]
mod tests;
