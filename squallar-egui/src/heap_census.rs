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
//! # **MEASURED 2026-09-08: the residual is not a holder — it is the gap
//! between the PEAK and the resting level**
//!
//! Read this before hunting a holder for the residual. Two sessions have now
//! spent a night doing exactly that, and the second was told to.
//!
//! [`Census::residual`] is taken against **`byteLength`, which only ever
//! grows**. `dlmalloc` on `wasm32-unknown-unknown` extends linear memory with
//! `memory.grow` and has no way to hand a page back, so every byte the
//! application has ever *freed* is still on the reading the residual is
//! subtracted from. [`squallar_alloc`] says it in its own module note: *"on
//! wasm it is not `byteLength` — a linear memory never shrinks, so
//! `byteLength − live_bytes` is exactly the freed-but-reserved headroom the
//! high-water mark hides."*
//!
//! The figures are from Tier-2 `long` and `huge` legs' own console rings,
//! where the `budget state:` line's `live <page>/<worker> MiB` and this line
//! are written on one tick and land in the ring at the same millisecond, so
//! they are a pairing and not an alignment:
//!
//! | arm | `byteLength` | `live` at rest | difference | this census's floor | unaccounted LIVE |
//! |---|---|---|---|---|---|
//! | firefox `long`, 4 settled ticks | 889.1 | 490 | **399** | 454.6 | **35.4** (7.2 % of live) |
//! | chromium `long`, 5 settled ticks | 877.9 | 510–518 | **359–367** | 480.4 | **29.6–37.6** |
//!
//! So the 373.0 MiB that `byteLength − resident_total` reported on the
//! firefox arm decomposes as 399.1 of headroom **less** 26.1 of this census
//! pricing *above* the live heap, and the genuinely unnamed part is ~35 MiB —
//! a residual of a completely different size from the one the subtraction
//! advertises. **No family, however carefully written, will ever close it.**
//!
//! # **And the headroom is TRANSIENTS, not fragmentation — do not give up on it**
//!
//! The paragraph above is easy to finish with "so it is fragmentation and no
//! residency lever reaches it". That conclusion is wrong, and the correction
//! is the whole reason [`ProcessCensus::live_peak`] exists.
//!
//! `byteLength ≈ live_peak + fragmentation`, and a **max over sampled `live`
//! readings is a strict lower bound on `live_peak`** — a 2 s sampler can miss
//! a peak, never invent one.
//!
//! Taken over 32 `long` and `huge` arms and **conditioned on whether the page
//! actually refused an allocation**, which is the only split that matters
//! because the ratio is a statement about the wall:
//!
//! | arms | n | min | median | max |
//! |---|---|---|---|---|
//! | refused on the page | 21 | **0.81** | **0.92** | 0.98 |
//! | never refused | 11 | 0.46 | 0.63 | 0.98 |
//!
//! **On every arm that died, at least 81 % of the page's linear memory was
//! live at some sampled instant, and typically 92 %** — against an estimator
//! that can only under-read. So the fragmentation term at the wall is at most
//! a fifth and usually under a tenth, the peak is made of live bytes, and the
//! headroom is the distance between a peak near 1000 MiB and a resting level
//! near 490. That distance is transients, and every one of them is somebody's
//! allocation.
//!
//! The healthy arms sitting lower is the same fact from the other side: a
//! page that never approached its ceiling grew once for a transient and then
//! sat well below it. It is not a counter-example, and a floor could not
//! produce one.
//!
//! **Two consequences.**
//!
//! 1. **Take the unnamed term against [`ProcessCensus::live`], never against
//!    `byteLength`.** [`Census::unaccounted`] is that reading and exists for
//!    it. A `byteLength` residual is dominated by a term no family can be
//!    written for.
//! 2. **Steer by [`ProcessCensus::live_peak`], not by `live` at rest.** Every
//!    family here is a LEVEL, and a page is killed by what was held at ONE
//!    instant. A scene whose resting level is 250 MiB still dies at 1024 if
//!    its peak touched 1000, and no level on this line would have said so.
//!    The overlay path is the worked example: on wasm a picture arrives as a
//!    `Vec<u8>` off the worker wire and `RasterBuf::into_pixels` *collects*
//!    it into a `Vec<Color32>`, so two blocks of 42,772,836 B at the
//!    4317x2477 plan are live at the same instant — 75.4 MiB that no tick
//!    will ever sample, and that `squallar_web::worker_port` calls "a
//!    property of the types, not of the transport".
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
/// it is cut and the test says so. The arithmetic: twenty-six families (the
/// two GPU ones included), the resident total and the linear reading are
/// twenty-eight `u64::MAX` figures at 20 digits apiece, the prose between them
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
/// 1060, and `rasters shared` is 14 too: 1060 + 39 = 1099.
/// `still l3` is 8 characters: `8 + 25 = 33`, so 1099 + 33 = 1132.
/// `loop archives` is 13 characters: `13 + 25 = 38`, so 1132 + 38 = 1170.
/// `font atlas` is 10 characters: `10 + 25 = 35`, so 1170 + 35 = 1205.
/// `overlay replies` is 15 characters, so it adds `15 + 25 = 40`: 1205 + 40
/// = 1245.
///
/// This chain is a DERIVATION and not a record: every term in it moves
/// when a family is added or removed, so re-derive it rather than nudging the
/// constant, and let `the_widest_line_fits_the_hooks_buffer` be the check.
/// That test asserts `<=`, so a constant that is too LARGE passes quietly.
/// **1245 was therefore checked against the measured width and not only
/// against the arithmetic**: setting it to 1 makes the test report the true
/// width, so the derivation above and the line agree exactly. Do that after
/// any change here — the chain being right twice is worth ten seconds, and
/// the `<=` will not tell you.
pub const CENSUS_LINE_CAPACITY: usize = 1245;

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
        ///
        /// **A process-global stomp, and the harness runs this binary's tests
        /// on a thread apiece.** Eight published families are written by
        /// ordinary code paths that unrelated tests drive — `Gui::frame`
        /// publishes five of them on *every* frame, and the tile source
        /// publishes three more — so a test that sets a family and then reads
        /// it back through [`census`] is racing every frame-driving test in
        /// the binary, and no lock in the test module can reach those writers.
        /// A test may read a published family from [`census`] only where it
        /// owns every writer of that family in this binary; otherwise it
        /// belongs against a hand-built [`Census`], which is what
        /// `the_font_atlas_family_prices_four_bytes_a_texel_of_the_real_atlas`
        /// was moved to after it reddened a peer's board on a correct tree.
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
    STILL_L3_BYTES, still_l3_bytes, set_still_l3_bytes,
        "**Level III products the STILL path is holding** - the latest \
         fetched object per `(AWIPS code, site)` in \
         `RenderDispatcher::level3_data`, at its envelope AND its decode. \
         Four codes per site on this build (`N0K`, `EET`, `DVL`, `DPR`), one \
         entry apiece, held until the site changes or the panes reset. \
         Until 2026-09-07 no family named a byte of this holder - not even \
         the envelope, which is the one term `loop l3` at least counts for \
         its own. \
         BOTH HALVES, because both are held: `Level3Product` keeps `bytes` \
         and `message` for its whole life, so the envelope is not freed when \
         the decode lands. That is also why this and `loop l3` disagree about \
         the same object - `loop l3` prices `bytes.len()` alone - and why \
         they OVERLAP where the loop cache and the dispatcher hold the same \
         `Arc`: at most `loop l3`'s whole figure is counted twice across the \
         two, and the sum is an upper bound like every other pair here. \
         WALKED, not maintained, and the walk is over the product's OWN \
         decoded structure, so it needs no geometry table and assumes no \
         radial or gate count. That is deliberate: the ICD estimate that \
         motivated this family (N0K 720x1200, DPR 360x920, DVL and EET \
         360x460, about 3.0 MiB for one site's four products) is an \
         ASSUMPTION, no fixture in this tree carries a real product's shape, \
         and a constant derived from it would have been a guess wearing a \
         number. It runs on the 2 s tick, never on a frame.";
    STILL_SCAN_BYTES, still_scan_bytes, set_still_scan_bytes,
        "Decoded volumes the still-pane inventory and the per-site latest \
         cache are holding together.";
    DERIVE_MEMO_BYTES, derive_memo_bytes, set_derive_memo_bytes,
        "Derived volumes the derivation memo is holding.";
    LOOP_FRAME_SCAN_BYTES, loop_frame_scan_bytes, set_loop_frame_scan_bytes,
        "Sweep gates the stored 2D loop frames are holding: each plan-view \
         frame's hover source retains the moments of the ONE sweep its \
         picture was drawn from, so the readout can decode a gate on demand. \
         Those moments are cloned out of the volume at extraction and are the \
         frame's own allocation, sharing nothing with the `Scan` they came \
         from - so this family names bytes NO other family names, and \
         dropping a stored frame is the only thing that frees them. It is a \
         partition and not a bound: two frames drawn from one volume hold two \
         sweeps and are counted twice because there are two.";
    LOOP_ARCHIVE_BYTES, loop_archive_bytes, set_loop_archive_bytes,
        "Compressed Level II archives the loop download cache is holding, so \
         a frame whose decoded volume was evicted costs a DECODE to restore \
         rather than a network round trip. Named apart from `loop scans` and \
         never added to it: the two are different orders of magnitude - \
         measured over 39 volumes, an archive is 1.0-16.1 MiB against a \
         33.7-82.7 MiB decoded volume, a median ratio of 17.1x - and they are \
         evicted by different policies, so one figure over both could not say \
         which half a fall came from. Shares nothing with `loop scans`: the \
         buffer here is the compressed object, the volume there is what was \
         decoded out of it. It DOES overlap the job funnel for the length of \
         a decode, which holds the same `Arc` while it reads it.";
    RENDER_CACHE_BYTES, render_cache_bytes, set_render_cache_bytes,
        "Finished radar rasters the render cache is holding, CPU-side: the \
         `Color32` pixel buffers and their resident hover fields.";
    PANE_CACHED_RENDER_BYTES, cached_render_bytes, set_cached_render_bytes,
        "Finished plan-view rasters the PANES are holding: NONE, since \
         2026-09-07, and the zero is what this row is for. \
         Each pane used to keep an `Arc` clone of its raster so a lost \
         graphics context could be repaired by an upload rather than a \
         re-render. At the empty steady scene, with every overlay off, that \
         was 216,796,176 B of `Color32` and 5,281,920 B of hover on ONE pane - \
         211.8 MiB, 87 % of the whole unaccounted heap - held for the life of \
         the process, and the sole holder of those bytes once the render cache \
         evicted its own entry. It was found in the mapping walk rather than \
         the census: one anonymous VMA read 1,083,985,920 B against \
         `render pools` 867,184,704 B, and the 216,796,176 B difference is \
         exactly `side * side * 4` at the 7362 px raster the pools were sized \
         for. \
         The pane now keeps `PaneRenderState::uploaded_from`, a `Weak` that \
         owns no pixels and answers one question - is this the raster my \
         texture was uploaded from - so a cache hit handing back the picture \
         already on the GPU is restamped rather than re-uploaded. \
         A REPORTED ZERO, NOT A DELETED ROW. It is the witness that the copy \
         is gone, and a row that can only read zero may be retired only after \
         a steady-state leg has PRINTED that zero. Deleting it in the land \
         that made it zero would leave the claim unfalsifiable. What can fail \
         is the behaviour test beside it, \
         `a_pane_does_not_keep_the_pixels_it_was_shown`, which lets go of \
         every other holder and requires the allocation to be gone.";
    RASTER_SHARED_BYTES, raster_shared_bytes, set_raster_shared_bytes,
        "**Bytes `render cache` and `cached renders` BOTH name** - the \
         correction term that turns their sum into a range, and NOT a holder \
         of anything. Left out of [`Census::resident_total`] for that reason, \
         the way the two GPU families are: nothing on this heap is these \
         bytes a second time, they are one allocation two families counted. \
         ONE SOURCE OF SHARING IS LEFT, and it is the cache's own: \
         `RenderCache` prices its entries one at a time while several keys can \
         hold ONE `Arc` - which is what `PlanViewUploads::handle` exists to \
         arrange, so it is the ordinary case and not an edge. The other source \
         was the panes, each holding an `Arc` clone of a live `render cache` \
         entry's image; that holder went on 2026-09-07 and `cached renders` \
         now reads zero, so this term measures the cache against itself. It is \
         still written as the difference of the two published figures rather \
         than narrowed to the cache, so a second holder would be priced \
         without anyone remembering to widen it. \
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
    OVERLAY_REPLY_BYTES, overlay_reply_bytes, set_overlay_reply_bytes,
        "**Finished OVERLAY pictures between the rasterizer and the frame \
         thread** - the `Arc<egui::ColorImage>` an `OverlayRenderResponse` \
         carries, priced at its `Color32` pixels, from the moment the deliver \
         closure sends it until `App::poll_overlay_render_results` takes it \
         off the channel. `renders in flight` is the same instrument one \
         producer over: that one covers the RADAR replies (the pane renders, \
         the adjacent-tilt speculation and the radar loop frames) and reaches \
         no overlay picture at all, so until this family existed a picture \
         that is 42,772,836 B at the 4317x2477 plan crossed that seam priced \
         by nothing. Both overlay producers are here, live pane rasters and \
         overlay LOOP frames alike, because both take the one deliver. \
         Published at the seam - the deliver as it sends, the drain as it \
         receives - for `renders in flight`'s reason: a reply lives about one \
         frame and a 2 s tick would read it as zero almost always. \
         KNOWN OMISSION, named rather than left silent: a reply's `hit_map` \
         is host bytes too - an `FxHashMap<u32, Vec<u32>>` over the touched \
         quarter-cells, on about 17 % of arrivals (the four vector layers) - \
         and this family prices the PICTURE alone. `HitMap` has no public \
         size to ask for; `squallar_app::loop_frame_store` records the same \
         gap at the same type for the same reason. A named gap lands in the \
         residual where someone finds it. \
         DISJOINT from every other family here. The picture is not in \
         `overlay grids` or `overlay items`, which price the SOURCE data a \
         handler decodes, never the raster drawn from it; and it is not yet \
         in `upload pending`, which starts naming the same allocation only \
         once `Context::load_texture` has filed it and the renderer has \
         banded it. The two windows abut and do not overlap: this one ends at \
         the take, that one begins at the file, and the frame between them - \
         while egui's own `TexturesDelta` holds it - is named by neither. \
         NOT ZERO ON ANY TARGET, and the difference is the thread rather than \
         the code: natively the deliver runs on an offload thread and the \
         window is a channel hop, on wasm32 it runs on the page thread inside \
         the worker's reply callback and the window is from that callback to \
         the next drain.";
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
    FONT_ATLAS_BYTES, font_atlas_bytes, set_font_atlas_bytes,
        "epaint's glyph atlas on the HOST heap: the `ColorImage` egui holds \
         for the life of the `Context`, at four bytes a texel of \
         `Fonts::font_image_size`. The device copy of the same picture is in \
         `gpu textures`, which has named it all along; this side had no \
         family, so an atlas that grew landed in the residual with nothing \
         saying which family had moved. \
         It is NOT a cache with a working set. epaint allocates it \
         `min(max_texture_side, 16384)` wide and DOUBLES its height on \
         demand, and `TextureAtlas::max_height` is the width - so the ceiling \
         is the width squared, and `Fonts::begin_pass` recycles only above \
         `0.8 x width` rows. Nothing evicts a cold glyph. \
         The width is the adapter's, so a figure here names its target. Read \
         off this tree's own `plan views may reach` boot line in captured rig \
         logs, as `max_texture_dimension_2d` -> width: native Linux 32768 -> \
         16384; native M2 16384 -> 16384; Firefox/WebGL2 on Linux 16384 and \
         on the M2 32768, both -> 16384; **Chromium/WebGL2 on Linux 8192 -> \
         8192**. The two web targets do NOT agree and a figure that merged \
         them would be wrong by 2x on one of them. iOS and Android are \
         unmeasured here; the boot line prints it on every launch. \
         **A LEVEL, and the doubling is not in it.** `take_delta` clones the \
         whole image into an `ImageDelta::full` on every growth, and that \
         clone lives across the whole of `EguiRenderer::end_frame` - \
         tessellate, upload, mirror, `update_buffers` - so for a whole \
         frame's prepare phase, which is the frame's own peak, the heap \
         holds this figure TWICE. The second copy is the delta's and is \
         nobody's family either. \
         **And a THIRD copy can exist that `live_bytes` structurally cannot \
         see.** The growth is a `Vec::resize`, i.e. a `realloc`, and \
         `squallar_alloc::Counting::realloc` books it as one block returned \
         and one granted - a net delta. So an allocator that satisfies it by \
         allocate-copy-free holds `S + 2S` at once and `live_bytes` reads \
         only `2S`. That is real on RSS and on a wasm `byteLength`, where it \
         is up to 1.5x this figure and never given back, and it is invisible \
         to the metric this whole campaign gates on. Read it against the \
         process families, never against `live_bytes` alone.";
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
        "Decoded volumes the REAL-TIME CHUNK FEED is holding - the cuts each \
         live site has sealed but not yet folded into a snapshot, the built \
         snapshot they fold into, and the closed volumes a poller has parked. \
         ONE whole volume a live site, and the first two terms are why: a \
         sealed cut's sweep is either staged or inside the built `Scan`, \
         never both, because the build MOVES it rather than copying it. \
         Falsifiable where it is decided, in `VolumeAssembler::snapshot`: the \
         previous volume's sweeps leave it by `Arc::try_unwrap` and the \
         staged ones by `mem::take`, so no sweep is in two places for two \
         terms to price. \
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

/// What the glyph atlas costs the host heap, from `Fonts::font_image_size`.
///
/// **The `font atlas` family's whole arithmetic, named once.** epaint holds
/// the atlas as a `ColorImage`, four bytes a texel, and until this was a
/// function the expression lived inline at the publisher while the test that
/// pins it kept a second copy of the same multiplication — so the two could
/// disagree and only the copy was ever gated. It is the publisher's
/// expression that has to be right, so it is the publisher's expression that
/// is pinned.
pub fn atlas_bytes(size: [usize; 2]) -> u64 {
    let [width, height] = size;
    (width as u64) * (height as u64) * 4
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
            // A real allocation that no other family names, so it is summed
            // flat rather than through `radar_total`'s upper bound: the
            // decoded-volume families share `Arc`s with each other and this
            // shares with none of them.
            self.loop_archive_bytes,
            self.still_l3_bytes,
            self.render_cache_bytes,
            self.cached_render_bytes,
            self.render_pool_bytes,
            self.render_in_flight_bytes,
            self.overlay_grid_bytes,
            self.overlay_item_bytes,
            self.overlay_parked_bytes,
            self.loop_frame_bytes,
            self.overlay_reply_bytes,
            self.upload_pending_bytes,
            self.tile_body_bytes,
            self.tile_parsed_bytes,
            self.tile_cache_bytes,
            self.loan_outstanding_bytes,
            self.volume_store_bytes,
            self.job_in_flight_bytes,
            self.font_atlas_bytes,
            self.deferred_drop_bytes,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }

    /// **The decoded-volume families, summed as an upper bound.**
    ///
    /// Holders keep `Arc`s of the same volumes — the loop download cache, the
    /// still inventory, the derivation memo — so a volume two of them name is
    /// counted twice here. `loop frame scans` is **not** one of them: a
    /// stored frame holds a sweep's moments, its own allocation, so its
    /// bytes are named once in this sum.
    /// Stated rather than corrected: the figure that matters for "what
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
    /// The two could hold `Arc`s of the same images, and did until 2026-09-07:
    /// a raster both name is counted twice here. [`Self::raster_floor`] is the
    /// other end, and unlike [`Self::radar_floor`] it is exact. With
    /// `cached renders` reading zero the two ends meet, and the pair is kept
    /// because the correction is measured rather than assumed -- it would
    /// price a second holder the day one appears.
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
    ///
    /// **This figure is NOT "how much is held by something unnamed".**
    /// `linear_bytes` is a high-water mark, so this subtraction carries every
    /// byte the allocator has freed and cannot give back — 359 to 399 MiB of
    /// a ~880 MiB page on both browsers, measured. [`Self::unaccounted`]
    /// against [`ProcessCensus::live`] answers the holder question, and
    /// [`ProcessCensus::live_peak`] answers the one this figure is usually
    /// being asked in place of: whether those bytes were ever live at once,
    /// and so whether any lever reaches them. See the module note.
    pub fn residual(&self, linear_bytes: u64) -> Option<u64> {
        linear_bytes.checked_sub(self.resident_total())
    }

    /// **The decoded-volume families as a de-duplicated LOWER bound**: the
    /// largest single one.
    ///
    /// The holders share `Arc`s — eight fields, eight allocations, seven
    /// owners of a decoded source volume (`squallar_radar::scan_size` lists
    /// them), published as the six families below — so their sum
    /// ([`Self::radar_total`]) is an upper bound. The floor of a union of overlapping sets is the
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
        "heap census ({instance}): loop scans {} B, loop archives {} B, \
         loop l3 {} B, still l3 {} B, \
         still scans {} B, \
         derive memo {} B, loop frame scans {} B, chunk feed {} B, \
         render cache {} B, cached renders {} B, rasters shared {} B, \
         render pools {} B, \
         renders in flight {} B, \
         overlay grids {} B, overlay items {} B, overlay parked {} B, loop frames {} B, \
         overlay replies {} B, \
         upload pending {} B, tile bodies {} B, tile parsed {} B, \
         tile cache {} B, loans out {} B, volume store {} B, jobs in flight {} B, \
         font atlas {} B, deferred drops {} B; resident total {} B of ",
        census.loop_scan_bytes,
        census.loop_archive_bytes,
        census.loop_l3_bytes,
        census.still_l3_bytes,
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
        census.overlay_reply_bytes,
        census.upload_pending_bytes,
        census.tile_body_bytes,
        census.tile_parsed_bytes,
        census.tile_cache_bytes,
        census.loan_outstanding_bytes,
        census.volume_store_bytes,
        census.job_in_flight_bytes,
        census.font_atlas_bytes,
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
        LIVE; LIVE_PEAK; LIVE_PEAK_LARGE; RSS; ANON; FILE; SHMEM; THREADS; SAMPLES;
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
    /// **`squallar_alloc::live_peak_bytes()` — the high-water mark of
    /// [`Self::live`], taken at the grant and never on a tick.**
    ///
    /// The term that says whether a residency lever can reach a wasm page's
    /// death at all. A linear memory never shrinks, so
    /// `byteLength ≈ live_peak + fragmentation`: the first half is bytes some
    /// instant of this program really did hold, which every retention lever
    /// in the tree moves, and the second is allocation SHAPE, which none of
    /// them touches. Reading `live` at rest against `byteLength` — which is
    /// what this campaign did until 2026-09-08 — conflates the two and cannot
    /// say which is which.
    ///
    /// Zero means unread, on [`Self::samples`]'s terms: no binary in this
    /// process declared the counting allocator.
    pub live_peak: u64,
    /// **What [`Self::live_peak`] was MADE OF** — blocks over
    /// `squallar_alloc::LARGE_GRANT_FLOOR` live at the instant the peak was
    /// set, `squallar_alloc::live_peak_large_blocks()`.
    ///
    /// Answers "the peak was 880 MiB and N large blocks were live" directly
    /// rather than by inference. **Taken at the peak, not sampled**: it is
    /// stored only when the peak advances, so it describes the instant the
    /// maximum was reached. A level read on a tick would answer the same
    /// question about a different moment — usually one where the transients
    /// that set the peak are long gone — and would look identical.
    pub live_peak_large: u64,
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
    // Read here rather than taken as a parameter, unlike `live`: the peak is
    // maintained at the allocation site and is already exact at every
    // instant, so there is no second reading to keep in step with the first.
    // Nothing here samples it — a sampled peak is a false zero for every
    // transient shorter than the sample, which is the whole class it exists
    // to catch. See `squallar_alloc::live_peak_bytes`.
    lv::LIVE_PEAK.store(squallar_alloc::live_peak_bytes().unwrap_or(0), Relaxed);
    lv::LIVE_PEAK_LARGE.store(squallar_alloc::live_peak_large_blocks(), Relaxed);
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
        live_peak: lv::LIVE_PEAK.load(Relaxed),
        live_peak_large: lv::LIVE_PEAK_LARGE.load(Relaxed),
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
/// family added there moves this too: `chunk feed` took it from 645 to 647,
/// and `loop archives` from 647 to 649 — a family costs this line two bytes,
/// not the `name.len() + 25` it costs the census line, because only the
/// saturated `unaccounted` figures widen here and not a per-family term.
///
/// **The two constants move for different reasons and neither implies the
/// other**: [`CENSUS_LINE_CAPACITY`] grows by the family's NAME
/// (`name.len() + 25`), this one only when a printed total gains a DIGIT.
/// **So a family can cost this line nothing at all, and measuring is the only
/// way to know.** `chunk feed` took it 645 to 647 and `loop archives` 647 to
/// 649; `overlay replies` moved it not at all, because the `families` and
/// `floor` totals it widens were already past the power of ten that would
/// have cost a digit. Re-derive by running the test, which asserts equality
/// rather than `<=`; never carry a delta across a rebase.
///
/// `live peak` is the first field added to this line rather than to the
/// census, and it moves the constant by its OWN prose and figure, not by a
/// digit on somebody else's total: `", live peak "` is 12 characters, plus
/// twenty digits and `" B"`, so `12 + 20 + 2 = 34` and 649 becomes 683.
/// `peak large blocks` is a COUNT and carries no `" B"`:
/// `", peak large blocks "` is 20 characters plus twenty digits, so
/// `20 + 20 = 40` and 683 becomes 723.
pub const PROCESS_LINE_CAPACITY: usize = 723;

/// **The process denominator as one line.**
///
/// # The exact format, for whoever writes the regex
///
/// Written down here rather than left to be read off the `write!` calls,
/// because the scraper lives in another repository's `drive.py` and the two
/// drift the moment one of them is the only record.
///
/// ```text
/// process memory (page): live 123 B, live peak 456 B, peak large blocks 7, \
/// families 8 B floor 9 B, unaccounted 1 B to 2 B; rss unread
/// ```
///
/// * The prefix is `process memory (<instance>): `, and `<instance>` is one
///   of `page`, `rasterization worker` or `tile lane` — **always match it**,
///   because the page and the worker are two heaps under two ceilings and a
///   figure from the wrong one is worse than no figure. A fourth spelling,
///   `process`, is the native sampler thread's own (it logs this line at
///   `debug!` on the walk's cadence, where the frame thread's telemetry tick
///   says `page`); on the web the sampler has no thread and the tick is the
///   only emitter, so a web scrape sees `page` alone.
/// * Fields are `<name> <integer>`, a byte figure carrying a trailing ` B`
///   and a count carrying none. Every integer is a plain decimal `u64` with
///   no separators and no units other than that ` B`.
/// * `, ` separates fields; `; ` separates the three groups.
///
/// **The first group is unconditional and everything after it is not.** `rss`
/// collapses to `rss unread` where there is no `/proc` — which is every web
/// target — and the breakdown collapses to `breakdown unwalked` until a walk
/// has landed. `unaccounted` has a second arm, `unaccounted none (families
/// price above live)`, which is a real state and not an error.
///
/// So **`live`, `live peak` and `peak large blocks` sit in that first group,
/// immediately after the prefix**, and that is a deliberate choice against
/// this workspace's usual "append at the end" rule for scraped lines. The
/// usual rule exists because `budget state:` is read by an unanchored
/// positional regex; here the opposite applies, because the *end* of this
/// line is the conditional part. A field appended after the breakdown would
/// be present on a native tick and absent on a web one, at two different
/// offsets. In the first group it is at a fixed offset on every target and
/// every tick, and
/// `process memory \((\w[\w ]*)\): live (\d+) B, live peak (\d+) B, peak large blocks (\d+)`
/// matches all of them.
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
    write!(
        out,
        "process memory ({instance}): live {} B, live peak {} B, \
         peak large blocks {}",
        process.live, process.live_peak, process.live_peak_large
    )?;
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

/// **The large-grant histogram as one line**, for the telemetry tick.
///
/// # The exact format, for whoever writes the regex
///
/// ```text
/// large grants (page): 1048576..1179648 B x3 = 3145728 B, largest 1100000 B; \
/// 33554432..37748736 B x12 = 460000000 B, largest 37000000 B
/// large grants (page): none
/// ```
///
/// * The prefix is `large grants (<instance>): `, and `<instance>` is the
///   same three-way choice [`write_process_line`] documents, for the same
///   reason.
/// * Then either the literal `none`, or one or more bucket clauses separated
///   by `; `.
/// * A clause is `<low>..<high> B x<count> = <bytes> B, largest <max> B`.
///   `low` and `high` bound the bucket (`low` inclusive, `high` exclusive);
///   `count` and `bytes` are cumulative over the whole session; **`max` is
///   the exact largest single grant in that bucket**, which is the figure a
///   reader wants — it decides whether a free chunk can serve a repeat, and
///   it is exact where the bucket is not.
/// * Empty buckets are omitted, so the clause count varies from tick to tick
///   and a positional regex will not do. Match clauses with
///   `(\d+)\.\.(\d+) B x(\d+) = (\d+) B, largest (\d+) B` and take every
///   match on the line.
///
/// **Cumulative flow, never a level, and never added to anything on the
/// census line.**
///
/// A `String` and therefore NOT hook-safe, unlike everything else in this
/// module: the bucket set is unbounded prose and the allocation-error hook
/// writes into a fixed buffer. That is deliberate — this answers "what shape
/// are the blocks that grew this heap", which is a question you ask before
/// the wall, not at it.
///
/// **Cumulative flow, not a level**, and it is the only such figure this
/// module carries. Every other family here says what is held now; a wasm
/// linear memory is grown by what was ever held *at once* and never shrinks,
/// so the shape of the large grants is what a level structurally cannot say.
/// Empty buckets are left out, so a quiet process says
/// `large grants (page): none`.
pub fn large_grants_line(instance: &str) -> String {
    use core::fmt::Write;

    let mut out = String::new();
    let _ = write!(out, "large grants ({instance}):");
    let mut said = 0usize;
    for idx in 0..squallar_alloc::LARGE_GRANT_BUCKETS {
        let Some(b) = squallar_alloc::large_grant(idx) else {
            break;
        };
        if b.count == 0 {
            continue;
        }
        // The range, then what landed in it, then the largest single one
        // EXACTLY — that last figure is what decides whether a free chunk can
        // serve a repeat, and it is readable at finer resolution than the
        // bucket it sits in.
        let _ = write!(
            out,
            "{} {}..{} B x{} = {} B, largest {} B",
            if said == 0 { "" } else { ";" },
            b.low,
            b.high,
            b.count,
            b.bytes,
            b.max,
        );
        said += 1;
    }
    if said == 0 {
        let _ = write!(out, " none");
    }
    out
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
