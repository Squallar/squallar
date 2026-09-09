//! What a band is, checked as arithmetic.

use super::*;

/// The widest raster a WSR-88D surveillance cut asks for at this box's ceiling.
const WIDEST: usize = 7362;

/// Every band fits the budget it was sized against.
#[test]
fn no_band_carries_more_than_one_band_of_bytes() {
    for side in [2048usize, 4096, 5561, WIDEST, 8192] {
        let height = side as u32;
        let mut done = 0u32;
        while done < height {
            let plan = BandPlan::of(side, height, done, UPLOAD_BAND_BYTES)
                .expect("rows remain, so there is a plan");
            assert!(
                plan.bytes() <= UPLOAD_BAND_BYTES,
                "a {side}px raster planned {} rows from row {done} — {} bytes, over the \
                 {UPLOAD_BAND_BYTES}-byte band that bounds a frame at 4 ms",
                plan.rows,
                plan.bytes(),
            );
            done += plan.rows;
        }
    }
}

/// Bands tile the image exactly: every row once, in order, none past the end.
#[test]
fn the_bands_of_a_raster_cover_every_row_exactly_once() {
    for side in [1usize, 2, 255, 2048, 5561, WIDEST, 8192] {
        let height = side as u32;
        let mut done = 0u32;
        let mut plans = 0u32;
        while let Some(plan) = BandPlan::of(side, height, done, UPLOAD_BAND_BYTES) {
            assert!(plan.rows > 0, "a {side}px raster planned an empty band");
            done += plan.rows;
            plans += 1;
            assert!(
                done <= height,
                "a {side}px raster planned past its last row: {done} of {height}",
            );
            assert!(plans <= height, "a {side}px raster is not making progress");
        }
        assert_eq!(
            done,
            height,
            "a {side}px raster stopped {} rows short of the bottom",
            height - done,
        );
    }
}

/// A row wider than the whole frame budget still moves, one row at a time.
#[test]
fn a_row_too_wide_for_the_budget_still_makes_one_row_of_progress() {
    let side = UPLOAD_BAND_BYTES; // one row is four times the whole band
    let plan = BandPlan::of(side, 4, 0, UPLOAD_BAND_BYTES).expect("there are rows to move");
    assert_eq!(plan.rows, 1);
    assert!(plan.bytes() > UPLOAD_BAND_BYTES);
}

/// Nothing to move is `None`, not a band of zero rows.
#[test]
fn an_image_with_no_rows_left_has_no_plan() {
    assert!(BandPlan::of(64, 4, 4, UPLOAD_BAND_BYTES).is_none());
    assert!(BandPlan::of(64, 4, 9, UPLOAD_BAND_BYTES).is_none());
    assert!(BandPlan::of(0, 4, 0, UPLOAD_BAND_BYTES).is_none());
}

/// The staging stride is the copy alignment, and the widest cut really needs it.
#[test]
fn the_staging_stride_is_aligned_and_the_widest_cut_is_not() {
    let plan = BandPlan::of(WIDEST, WIDEST as u32, 0, UPLOAD_BAND_BYTES).expect("a plan");
    assert_eq!(plan.row_bytes, WIDEST * 4);
    assert_eq!(plan.row_bytes, 29448);
    assert_eq!(plan.padded_row, 29696);
    assert_eq!(plan.padded_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
    assert!(u64::from(plan.padded_row) >= plan.row_bytes as u64);

    // A power-of-two side pads by nothing, which is why the buffer for one is
    // exactly the band it was sized against.
    let square = BandPlan::of(2048, 2048, 0, UPLOAD_BAND_BYTES).expect("a plan");
    assert_eq!(square.padded_row as usize, square.row_bytes);
}

/// A ring slot is one band, and the pair is what the module claims it costs.
#[test]
fn the_ring_a_band_needs_is_two_slots_of_a_band() {
    let plan = BandPlan::of(WIDEST, WIDEST as u32, 0, UPLOAD_BAND_BYTES).expect("a plan");
    let both = plan.staged_bytes() * crate::staging_ring::STAGING_RING_DEPTH as u64;
    assert!(
        both < 18 << 20,
        "the ring for a {WIDEST}px raster is {both} bytes, over the 16.9 MiB the \
         module docs quote",
    );
    let unbanded = (WIDEST * WIDEST * 4) as u64 * crate::staging_ring::STAGING_RING_DEPTH as u64;
    assert!(unbanded > 400_000_000, "the figure the banding avoids");
}

/// A frame never asks the ring for more slots than it has.
#[test]
fn a_frame_never_claims_more_slots_than_the_ring_has() {
    let depth = crate::staging_ring::STAGING_RING_DEPTH;
    assert!(
        (1..=depth).contains(&DMA_BANDS_PER_FRAME),
        "a frame moves {DMA_BANDS_PER_FRAME} bands against a ring {depth} deep",
    );
    // And the ringless arm, whose whole point is that one band *is* the frame.
    assert_eq!(TextureUploads::without_device().bands_per_frame(), 1);
}

/// A device with no ring spends one band a frame, and one with a ring four.
#[test]
fn the_frame_budget_follows_the_device_and_not_the_target() {
    let ringless = TextureUploads::without_device();
    assert!(!ringless.has_ring());
    assert_eq!(ringless.bands_per_frame(), 1);
    assert_eq!(ringless.pending_bands(), 0);
}

/// **A web-picture-sized delta no longer blocks a ringless frame whole.**
/// Spike B (2026-08-30) measured Firefox's whole-picture overlay raster at
/// 8.51 MB; on a ringless device — all of web — that byte count used to
/// cross in one blocking `write_texture` on the frame thread. It now files
/// as bands of at most `BLOCKING_BAND_BYTES`, drained one per frame, and
/// every byte lands in exactly one band.
#[test]
fn a_web_picture_bands_on_a_ringless_device_instead_of_blocking_whole() {
    let cap = band_cap(false);
    assert!(
        cap < UPLOAD_BAND_BYTES,
        "the ringless band cap ({cap} B) is not below the ring's \
         ({UPLOAD_BAND_BYTES} B), so a blocking chunk costs a ring-sized \
         stall on the one device class where every chunk is frame thread",
    );

    // The routing: spike B's Firefox picture goes whole on a ring device and
    // bands on a ringless one.
    let picture = 8_510_000usize;
    assert!(
        !goes_whole(false, picture),
        "an 8.51 MB picture crosses whole on a ringless device: one blocking \
         write_texture spends the frame it lands on",
    );
    assert!(
        goes_whole(false, cap),
        "a delta at the cap itself must still go whole — banding it buys a \
         frame of latency for the same one write",
    );

    // The planner: a 1024px-wide picture of that byte count tiles into
    // exactly ceil(size / cap) bands, none over the cap, every byte once.
    let width = 1024usize;
    let height = (picture / (width * 4)) as u32; // 2077 rows, 8.5 MB
    let bytes = width * 4 * height as usize;
    let expected = bytes.div_ceil(cap) as u32;
    let mut done = 0u32;
    let mut bands = 0u32;
    let mut moved = 0usize;
    while let Some(plan) = BandPlan::of(width, height, done, cap) {
        assert!(plan.bytes() <= cap, "a band over its own cap");
        done += plan.rows;
        bands += 1;
        moved += plan.bytes();
        assert!(bands <= height, "not making progress");
    }
    assert_eq!(
        bands, expected,
        "a {bytes} B picture filed {bands} bands at a {cap} B cap",
    );
    assert_eq!(
        moved, bytes,
        "every byte of the picture in exactly one band"
    );

    // And the drain moves one band per frame there — the frame budget the
    // sweep beside `BLOCKING_BAND_BYTES` priced its dry-frame rows against.
    assert_eq!(TextureUploads::without_device().bands_per_frame(), 1);
}

/// The font atlas crosses whole at every size on every device class, and
/// nothing else past the cap does.
///
/// The texel coordinates every galley holds are into the *whole* atlas, so a
/// band-by-band arrival is frames of labels drawn from an allocation whose
/// rows have not landed; the overlay pictures have no such reader and keep
/// banding.
#[test]
fn the_font_atlas_crosses_whole_however_large_it_has_grown() {
    let atlas = egui::TextureId::default();
    let picture = egui::TextureId::Managed(7);
    // The atlas at the full square it reaches on the arm that governs: the web
    // atlas is 16384 wide (`is_font_atlas` carries the chain), so 1 GiB.
    let full_square = 16384 * 16384 * 4;
    for capable in [false, true] {
        let device = if capable {
            "a ring device"
        } else {
            "a ringless device"
        };
        assert!(
            crosses_whole_now(atlas, capable, full_square, 0),
            "on {device} the font atlas at {full_square} B was filed as bands, \
             so every label whose glyphs sit past the first band draws from \
             rows that have not landed",
        );
        assert!(
            !crosses_whole_now(picture, capable, full_square, 0),
            "on {device} a picture of {full_square} B crossed whole: the atlas \
             exemption leaked onto the rasters this module exists to band",
        );
        // Re-pointed 2026-09-07 from `band_cap` to `whole_budget`. The claim
        // is unchanged — a delta AT the whole-crossing threshold must still
        // cross on a fresh frame, or the route can be starved by its own
        // bound. What moved is which symbol IS that threshold: `goes_whole`
        // read `band_cap` and now reads `whole_budget`, because a ring band is
        // a memcpy and a whole delta is a blocking write, and only one of the
        // two is an allowance on the frame thread.
        assert!(
            crosses_whole_now(picture, capable, whole_budget(capable), 0),
            "on {device} a picture at the whole-crossing threshold must still \
             go whole, exactly as `goes_whole` says",
        );
    }
}

/// One frame's whole-crossing route spends at most [`whole_budget`], however
/// many deltas egui hands over.
///
/// **The bound is the frame, not the delta.** [`goes_whole`] caps one delta,
/// and that was read as though it capped the frame; `apply` loops the whole
/// delta set through the route with nothing counting what it spends, so N
/// deltas at the cap cost N times the cap on one frame thread. On web that is
/// 4 MiB each with no N — see [`whole_budget`] for the loop frames that supply
/// them.
#[test]
fn one_frame_cannot_spend_more_than_its_budget_on_whole_writes() {
    for (capable, each) in [false, true].into_iter().flat_map(|capable| {
        // The largest a single delta may be and still take this route (the
        // worst case per delta), and a quarter of it — so the bound is shown
        // to bite on a frame that admits several deltas and not only on one
        // that admits exactly one.
        [whole_budget(capable), whole_budget(capable) / 4].map(move |each| (capable, each))
    }) {
        let mut spent = 0usize;
        let mut crossed = 0usize;
        for i in 0..64u64 {
            if crosses_whole_now(egui::TextureId::Managed(i), capable, each, spent) {
                spent += each;
                crossed += 1;
            }
        }
        // The non-vacuity floor: a predicate that refused everything would
        // satisfy the bound below and move no bytes at all.
        assert!(
            crossed >= 1,
            "capable={capable}, each={each} B: nothing crossed whole, so the \
             bound below is vacuous — a delta at the threshold must always \
             cross on a fresh frame",
        );
        assert!(
            spent <= whole_budget(capable),
            "capable={capable}: 64 deltas of {each} B put {spent} B of blocking \
             `write_texture` on one frame thread, over the {} B that frame is \
             allowed — this is the unbounded route, and at the web figures it \
             is the 53.8-64.0 ms `prep` bin the panel reported",
            whole_budget(capable),
        );
    }
}

/// **The whole-crossing allowance is a share of the frame this application
/// aims at**, and it is checked as a TIME rather than as a byte count.
///
/// The defect this pins is not a value, it is a *shape*: the allowance used to
/// be `band_cap × bands_per_frame`, a product of one constant sized against a
/// 16.7 ms frame and one that counts staging buffers. Neither factor is a
/// blocking allowance, nobody chose their product, and moving either — a third
/// ring slot, say — moved this route's cost on the frame thread silently. A
/// gate on the byte figure would have been satisfied by the old value on the
/// day it was written; this one asks what the bytes COST against what the frame
/// has, which is the thing that was wrong.
///
/// The share and the bandwidth are both named by the module, so this is the
/// derived quantity checked against what it describes and not against a magic
/// number.
#[test]
fn the_whole_crossing_allowance_is_a_share_of_the_frame_the_app_aims_at() {
    let frame = squallar_device_profile::constants::TARGET_FRAME_SERVICE;
    let slice_us = frame.as_micros() as u64 / WHOLE_CROSSING_SHARE;

    // Non-vacuity first: an allowance of zero satisfies every bound below and
    // sends every delta to the bands.
    assert!(
        whole_budget(true) > 0 && whole_budget(false) > 0,
        "an allowance of zero bytes bounds the frame by starving the route",
    );

    // The ring arm, which is the one this file derives.
    let us = whole_budget(true) as u64 * 1_000_000 / BAR_WRITE_BYTES_PER_SEC;
    assert!(
        us <= slice_us,
        "a ring device's whole-crossing route may put {} B on the frame \
         thread, which is {us} us at the {BAR_WRITE_BYTES_PER_SEC} B/s this \
         module measured — over the {slice_us} us that is one \
         {WHOLE_CROSSING_SHARE}th of the {} us frame this application aims at",
        whole_budget(true),
        frame.as_micros(),
    );

    // And the arithmetic that used to produce it is shown to fail the same
    // test, so the gate is known to fire on the defect rather than only to
    // pass on the fix. 8 MiB x 2 slots = 16 MiB = 7.6 ms against a 4 ms frame.
    let old = band_cap(true) * bands_per_frame(true);
    let old_us = old as u64 * 1_000_000 / BAR_WRITE_BYTES_PER_SEC;
    assert!(
        old_us > frame.as_micros() as u64,
        "the superseded arithmetic `band_cap x bands_per_frame` = {old} B = \
         {old_us} us now fits inside the whole {} us frame, so this test no \
         longer demonstrates the defect it was written against and the bound \
         above is unwitnessed",
        frame.as_micros(),
    );
}

/// **A delta the budget displaces costs one band, not a queue of them.**
///
/// This is what keeps the new failure mode small. Past the allowance a delta is
/// filed as bands and arrives on a later frame; `whole_budget <= band_cap` is
/// what makes that "a later frame" rather than `ceil(bytes / band_cap)` of
/// them. Lowering `band_cap` under the allowance — or raising the allowance
/// over it — would multiply the arrival latency of every displaced raster
/// without changing a line of the routing, and neither constant's own note
/// mentions the other.
#[test]
fn a_delta_the_budget_displaces_is_at_most_one_band() {
    for capable in [false, true] {
        assert!(
            whole_budget(capable) <= band_cap(capable),
            "capable={capable}: the whole-crossing allowance is {} B against a \
             {} B band, so a delta the allowance turns away is filed as {} \
             bands and waits that many drain slots rather than one",
            whole_budget(capable),
            band_cap(capable),
            whole_budget(capable).div_ceil(band_cap(capable)),
        );
        assert!(
            bands_per_frame(capable) >= 1,
            "capable={capable}: the drain moves no bands, so a displaced delta \
             never arrives at all",
        );
    }
}

/// **The class with a cheap alternative is never allowed more blocking than
/// the class with none.**
///
/// A ring device can memcpy a band into a staging slot and let the copy engine
/// pull it at 24.7 GB/s; a ringless device — all of web — has one route and it
/// is `write_texture` on the frame thread. So an allowance that is LARGER on
/// the ring arm is backwards, and it was: 16 MiB against 4 MiB, four times as
/// much blocking for the class that did not need any of it. Nothing intended
/// that — `bands_per_frame` is 2 on one arm and 1 on the other, and the
/// multiplication carried it into the frame's allowance.
#[test]
fn a_ring_device_is_not_allowed_more_blocking_than_a_ringless_one() {
    assert!(
        whole_budget(true) <= whole_budget(false),
        "a ring device may block the frame thread with {} B and a ringless one \
         with {} B: the class that can fall back to the copy engine is allowed \
         more of the route that cannot",
        whole_budget(true),
        whole_budget(false),
    );
}

/// The atlas exemption outlives a spent budget, and takes nothing with it.
///
/// The budget must not become a second way to band the atlas: that is the
/// defect `is_font_atlas` exists to prevent, and it would be reintroduced
/// silently by a frame that happened to have spent its allowance first.
#[test]
fn the_font_atlas_still_crosses_whole_on_a_frame_whose_budget_is_gone() {
    for capable in [false, true] {
        let spent = whole_budget(capable) * 4;
        assert!(
            crosses_whole_now(
                egui::TextureId::default(),
                capable,
                16384 * 16384 * 4,
                spent
            ),
            "capable={capable}: a frame that had already spent {spent} B banded \
             the font atlas, so every label drew from rows that had not landed",
        );
        assert!(
            !crosses_whole_now(egui::TextureId::Managed(7), capable, 1024, spent),
            "capable={capable}: the atlas's exemption leaked onto a picture on a \
             frame whose budget was gone",
        );
    }
}

/// A web loop frame sits *exactly* on the whole/banded boundary, and one frame
/// admits exactly one of them.
///
/// The two constants are owned by different crates and were never written down
/// beside each other: `WASM_LOOP_IMAGE_SIZE` is 1024, so a loop frame's texture
/// is 4 MiB, and `BLOCKING_BAND_BYTES` is 4 MiB, and [`goes_whole`] compares
/// with `<=`. Every loop frame takes the whole route by one byte. If either
/// constant moves this test says so, because the routing it decides is not
/// visible from either side alone.
#[test]
fn a_web_loop_frame_is_exactly_the_blocking_band_and_one_frame_admits_one() {
    use squallar_device_profile::constants::{BLOCKING_BAND_BYTES, WASM_LOOP_IMAGE_SIZE};

    let bytes = WASM_LOOP_IMAGE_SIZE * WASM_LOOP_IMAGE_SIZE * 4;
    assert_eq!(
        bytes, BLOCKING_BAND_BYTES,
        "a {WASM_LOOP_IMAGE_SIZE} px loop frame is {bytes} B against a \
         {BLOCKING_BAND_BYTES} B blocking band",
    );
    assert!(
        goes_whole(false, bytes),
        "a loop frame stopped taking the whole route, so this test no longer \
         pins the traffic it was written for",
    );
    assert_eq!(
        whole_budget(false) / bytes,
        1,
        "a ringless frame admitted {} loop frames whole; one is the budget, and \
         the rest must band",
        whole_budget(false) / bytes,
    );

    // What the route used to cost, from the same constants: a dispatch textures
    // `min(render budget, frames held)` of them, and every one crossed whole.
    let textured = squallar_device_profile::constants::WASM_MAX_LOOP_RENDER_BUDGET
        .min(squallar_device_profile::constants::WASM_MAX_LOOP_FRAMES);
    let unbounded = textured * bytes;
    assert_eq!(
        (textured, unbounded),
        (14, 58_720_256),
        "the loop's textured-frame count or its frame size moved, so the 56 MiB \
         this route used to put on one frame thread — the arithmetic behind the \
         reported [53.8, 64.0) ms `prep` bin — is no longer what is pinned here",
    );
    assert!(
        unbounded > whole_budget(false) * 13,
        "the bounded route must be more than an order below the {unbounded} B \
         it replaced, or this fix bought nothing",
    );
}

/// A raster the app loaded `NEAREST` is bound `NEAREST`.
#[test]
fn the_sampler_says_what_the_texture_options_said() {
    let nearest = sampler_descriptor(egui::TextureOptions::NEAREST);
    assert_eq!(nearest.mag_filter, wgpu::FilterMode::Nearest);
    assert_eq!(nearest.min_filter, wgpu::FilterMode::Nearest);
    assert_eq!(nearest.address_mode_u, wgpu::AddressMode::ClampToEdge);
    assert_eq!(nearest.address_mode_v, wgpu::AddressMode::ClampToEdge);

    let linear = sampler_descriptor(egui::TextureOptions::LINEAR);
    assert_eq!(linear.mag_filter, wgpu::FilterMode::Linear);
    assert_eq!(linear.min_filter, wgpu::FilterMode::Linear);

    let repeat = sampler_descriptor(egui::TextureOptions::LINEAR_REPEAT);
    assert_eq!(repeat.address_mode_u, wgpu::AddressMode::Repeat);
    assert_eq!(repeat.address_mode_v, wgpu::AddressMode::Repeat);

    let mirrored = sampler_descriptor(egui::TextureOptions {
        wrap_mode: egui::TextureWrapMode::MirroredRepeat,
        ..egui::TextureOptions::NEAREST
    });
    assert_eq!(mirrored.address_mode_u, wgpu::AddressMode::MirrorRepeat);

    // A compare function would make this a comparison sampler and change what
    // the shader gets back; egui's own never sets one.
    assert!(nearest.compare.is_none());
}

/// What the widest raster costs a frame, and how many frames it takes, at
/// each device shape's own band cap.
#[test]
fn the_widest_raster_takes_fourteen_frames_on_a_ring_and_fifty_three_without() {
    for (bands, cap, expected) in [
        (DMA_BANDS_PER_FRAME, band_cap(true), 14u32),
        (1, band_cap(false), 53),
    ] {
        let height = WIDEST as u32;
        let mut done = 0u32;
        let mut frames = 0u32;
        while done < height {
            for _ in 0..bands {
                let Some(plan) = BandPlan::of(WIDEST, height, done, cap) else {
                    break;
                };
                done += plan.rows;
            }
            frames += 1;
            assert!(frames <= height, "not making progress");
        }
        assert_eq!(
            frames, expected,
            "a {WIDEST}px raster took {frames} frames at {bands} bands a frame",
        );
    }
}

/// An id this module has never been shown has not been delivered.
#[test]
fn an_id_that_was_never_filed_has_not_been_delivered() {
    let uploads = TextureUploads::without_device();
    assert!(!uploads.is_delivered(egui::TextureId::Managed(0)));
    assert!(!uploads.is_delivered(egui::TextureId::Managed(7)));
    assert!(!uploads.is_delivered(egui::TextureId::User(3)));
}

/// **A renderer that has been shown nothing says so, rather than saying zero
/// bytes.**
///
/// The distinction is the whole point of `UploadTotals::deltas`: a byte total
/// of zero is what an idle renderer reports and also what one whose upload path
/// had stopped moving bytes would report, and a gate that read only the bytes
/// could not tell them apart.
#[test]
fn a_renderer_that_has_uploaded_nothing_says_so_rather_than_saying_zero_bytes() {
    let uploads = TextureUploads::without_device();
    let totals = uploads.totals();
    assert_eq!(
        totals.deltas, 0,
        "the floor is what distinguishes the cases"
    );
    assert_eq!(totals.bytes(), 0);
    assert_eq!(totals.banded_bytes(), 0);
    assert_eq!(totals, UploadTotals::default());
}

/// **The report is asked for once a frame and answers `None` on a frame that
/// moved nothing**, which is what keeps the line off an idle frame without
/// anything having to remember whether it was logged.
#[test]
fn an_idle_frame_is_told_there_is_nothing_new_to_report() {
    let mut uploads = TextureUploads::without_device();
    assert_eq!(
        uploads.totals_if_moved(),
        None,
        "a renderer that has never uploaded anything offered a line to write",
    );

    // The only thing a device-less fixture can move is the ledger itself; move
    // it the way `drain` does and check the report follows.
    uploads.note_band_for_test(4096, true);
    let first = uploads
        .totals_if_moved()
        .expect("a band moved and the ledger offered no line");
    assert_eq!(first.bands, 1);
    assert_eq!(first.staged_bytes, 4096);
    assert_eq!(
        uploads.totals_if_moved(),
        None,
        "the same totals were offered twice, so an idle frame would keep \
         writing the line it already wrote",
    );

    uploads.note_band_for_test(2048, false);
    let second = uploads
        .totals_if_moved()
        .expect("a second band moved and the ledger offered no line");
    assert_eq!(second.bands, 2);
    assert_eq!(second.blocking_bytes, 2048);
    assert_eq!(
        second.bytes(),
        4096 + 2048,
        "the two routes did not add up to what crossed",
    );
}

/// **The band straddle does not decide what a ringless device's bytes are
/// called.**
///
/// Spike B's pair (2026-08-30): Firefox's ~8.51 MB pictures banded and were
/// counted ~13 GB blocking, Chromium's ~7.57 MB went whole and were counted
/// ~0.1 GB — identical ringless traffic, opposite classifications, flipped by
/// 32 px of canvas width. Every byte of both moves through a blocking
/// `write_texture` on the frame thread, so the ledger must call every byte of
/// both blocking. Driven through the seams that share their arithmetic with
/// `file` and `drain` (`count_whole_write` / `count_band`); the same property
/// on the real path on a real adapter is
/// `a_ringless_byte_is_called_blocking_on_both_sides_of_the_band_straddle`
/// in `tests/raster_upload_gpu.rs`, which was run RED against the size-straddle
/// classification before this arithmetic replaced it. That one is `#[ignore]`d
/// because it needs an adapter, so the default row does not carry it -- run it
/// with `cargo test -p squallar-gpu --test raster_upload_gpu -- --ignored`.
#[test]
fn the_band_straddle_does_not_decide_what_a_ringless_byte_is_called() {
    // Chromium's side: 7 570 000 B, under the band, moved whole.
    let under = 7_570_000u64;
    let mut a = TextureUploads::without_device();
    a.note_whole_delta_for_test(under);
    let a = a.totals();
    assert_eq!(a.bytes(), under);
    assert_eq!(a.whole_bytes, under);
    assert_eq!(a.banded_bytes(), 0);

    // Firefox's side: 8 510 000 B, over the band, moved as bands — and a
    // ringless device never stages one.
    let over = 8_510_000u64;
    let mut b = TextureUploads::without_device();
    b.note_band_for_test(UPLOAD_BAND_BYTES as u64, false);
    b.note_band_for_test(over - UPLOAD_BAND_BYTES as u64, false);
    let b = b.totals();
    assert_eq!(b.bytes(), over);
    assert_eq!(b.whole_bytes, 0);
    assert_eq!(b.banded_bytes(), over);

    // The property: on both sides of the straddle, every byte is blocking.
    for (name, t) in [("under", a), ("over", b)] {
        assert_eq!(
            t.blocking_bytes,
            t.bytes(),
            "the {name}-the-band traffic moved wholly through blocking \
             `write_texture` on the frame thread and the ledger called {} of \
             {} B blocking",
            t.blocking_bytes,
            t.bytes(),
        );
    }
}

/// The whole-delta route counts once into each of its two figures: `whole`
/// as the routing, `blocking` as the frame time — and `bytes()` does not
/// double-count the overlap.
#[test]
fn a_whole_delta_is_whole_and_blocking_and_counted_once() {
    let mut uploads = TextureUploads::without_device();
    uploads.note_whole_delta_for_test(100);
    uploads.note_band_for_test(7, true);
    let t = uploads.totals();
    assert_eq!(t.deltas, 1);
    assert_eq!(t.whole_bytes, 100);
    assert_eq!(t.blocking_bytes, 100);
    assert_eq!(t.staged_bytes, 7);
    assert_eq!(t.bytes(), 107, "the whole bytes were added twice");
    assert_eq!(t.banded_bytes(), 7);
}

/// A freed id stops being delivered, which is what bounds the set.
#[test]
fn freeing_an_id_takes_it_back_out_of_the_delivered_set() {
    let mut uploads = TextureUploads::without_device();
    let id = egui::TextureId::Managed(11);
    uploads.mark_delivered_for_test(id);
    assert!(uploads.is_delivered(id));
    uploads.free(&[id]);
    assert!(
        !uploads.is_delivered(id),
        "a retired id stayed in the set, so the set grows with the session",
    );
}

/// **The resident level falls when egui retires a texture**, driven through
/// the real [`TextureUploads::free`] — the path `EguiRenderer::free_textures`
/// calls after every submit.
///
/// The falling half is the whole point. [`UploadTotals`] already answers "how
/// much has crossed"; nothing before this answered "how much is held", and a
/// figure that only rose would be the counter that already exists under a new
/// name. The charge is filed through the seam that shares
/// `ResidentTextures::owned_allocated` with the drain's `allocate`; what is
/// under test here is the free, which needs no device.
#[test]
fn the_resident_level_falls_when_egui_retires_a_texture() {
    let mut uploads = TextureUploads::without_device();
    assert_eq!(uploads.resident_texture_bytes(), 0);

    let a = egui::TextureId::Managed(1);
    let b = egui::TextureId::Managed(2);
    uploads.note_resident_for_test(a, [1806, 1806]);
    uploads.note_resident_for_test(b, [256, 256]);
    let held = uploads.resident_texture_bytes();
    assert_eq!(held, 1806 * 1806 * 4 + 256 * 256 * 4);

    uploads.free(&[b]);
    assert_eq!(
        uploads.resident_texture_bytes(),
        1806 * 1806 * 4,
        "`free` did not give the retired texture's bytes back, so the level is \
         a running total wearing a level's name",
    );
    uploads.free(&[a]);
    assert_eq!(
        uploads.resident_texture_bytes(),
        0,
        "every texture was retired and the level did not return to zero",
    );
    assert_eq!(
        uploads.resident_texture_bytes(),
        uploads.walked_resident_texture_bytes(),
        "the maintained total parted from the ledger's own maps at `free`",
    );
}

/// **A raster-atlas page is one resident allocation of its full size**, and
/// the tiles written into it are free.
///
/// `squallar_egui::raster_atlas` creates a page as one
/// `Context::load_texture` of a 1806x1806 transparent image (7x7 slots at a
/// 258 pitch inside `MAX_PAGE_SIDE`), then writes each 256x256 tile into it as
/// a `set_partial` of a 258x258 gutter-padded patch. A resident figure that
/// charged the partials would climb with every tile the map scrolls over and
/// read as a leak; the cumulative upload total genuinely does climb that way
/// and cannot say otherwise. Checked here as the ledger's arithmetic — a full
/// delta charges, a partial does not — because that routing decision is
/// `file`'s `delta.pos.is_none()` guard, and the same property on the real
/// path is `an_atlas_page_is_one_resident_charge_and_its_tiles_are_free` in
/// `tests/raster_upload_gpu.rs` — which is `#[ignore]`d because it needs a real
/// adapter, so run it with
/// `cargo test -p squallar-gpu --test raster_upload_gpu -- --ignored`.
#[test]
fn an_atlas_page_costs_its_page_and_not_the_sum_of_its_tiles() {
    const PAGE: usize = 1806;
    const SLOT: usize = 258;
    const TILE: usize = 256;

    let page_bytes = (PAGE * PAGE * 4) as u64;
    assert_eq!(
        page_bytes, 13_046_544,
        "the page this family must see as one"
    );

    let mut uploads = TextureUploads::without_device();
    let page = egui::TextureId::Managed(40);
    uploads.note_resident_for_test(page, [PAGE, PAGE]);
    assert_eq!(uploads.resident_texture_bytes(), page_bytes);

    // The page's shape, so a page that stopped being one texture would show
    // here: 7x7 slots of 258 inside the 2048 ceiling, 49 tiles to a page, and
    // one tile's own patch is 1.57 % larger than the tile because of the
    // gutter — an upload-bytes figure, and not a resident one, since every one
    // of those patches is a `set_partial` into the page above.
    assert_eq!(PAGE, (2048 / SLOT) * SLOT);
    assert_eq!((PAGE / SLOT) * (PAGE / SLOT), 49);
    assert_eq!(SLOT * SLOT * 4, 266_256);
    assert!(
        (SLOT * SLOT * 4) as f64 / (TILE * TILE * 4) as f64 - 1.0 < 0.02,
        "the gutter grows a tile's upload bytes by more than 2 %",
    );

    // And what the atlas actually bought, which only a resident figure can
    // say: 47 single-tile textures against one page.
    let unshared = (47 * TILE * TILE * 4) as u64;
    assert_eq!(unshared, 12_320_768);
    assert!(
        page_bytes > unshared,
        "the atlas is a draw-call win and not a byte win at 47 tiles, and the \
         resident family is the only instrument that can say so: one page is \
         {page_bytes} B against {unshared} B of separate tile textures",
    );
}

/// **The published census level is the renderer's own resident figure.**
///
/// The census is a set of process-wide statics, so two publishers in one test
/// binary would store into each other's slots. The fixture is what keeps that
/// from happening: [`TextureUploads::without_device_on_census`] is the only
/// publisher this binary can build and refuses to be built twice, and every
/// other fixture here publishes nothing. The comment this replaced asked for
/// "one test, not several" and got one *asserting* test - the siblings' five
/// `free` sites went on publishing, and this assertion read one of their
/// zeroes roughly one workspace run in five.
#[test]
fn the_census_carries_the_resident_texture_level() {
    let mut uploads = TextureUploads::without_device_on_census();
    let id = egui::TextureId::User(5);
    uploads.note_resident_for_test(id, [512, 512]);
    // `free` publishes, and it is the only publish this fixture can reach
    // without a device.
    uploads.free(&[egui::TextureId::User(6)]);
    assert_eq!(
        squallar_egui::heap_census::census().gpu_texture_bytes,
        512 * 512 * 4,
        "the renderer holds {} B and the census published something else",
        uploads.resident_texture_bytes(),
    );

    uploads.free(&[id]);
    assert_eq!(
        squallar_egui::heap_census::census().gpu_texture_bytes,
        0,
        "the census level did not follow the free down",
    );
    // And the family is on the line, out of the page total, beside the meshes.
    let census = squallar_egui::heap_census::Census {
        gpu_texture_bytes: 777,
        ..Default::default()
    };
    let line = squallar_egui::heap_census::line(&census, Some(1_000), "page");
    assert!(
        line.contains("gpu textures 777 B (GPU, not in the total)"),
        "{line}",
    );
    assert!(
        line.contains("resident total 0 B of 1000 B linear, residual 1000 B"),
        "a GPU family entered the linear-memory residual: {line}",
    );
}

/// **A noted page files no pixels**, so the `upload pending` family charges it
/// four bytes where it charged the whole page.
///
/// The two halves this reaches are the constructor `TextureUploads::file` uses
/// for a page `squallar_egui::blank_page` has noted, and the census sweep that
/// prices the queue. The pair of device calls around them — `seed`, and the
/// drain's `allocate` — no test in this module can reach without a GPU, and
/// nothing here claims to.
///
/// **The fixture property that lets this fail**: the page must be a size that
/// would otherwise be BANDED. A delta under `whole_budget` crosses whole and
/// never reaches the queue at all, so a small fixture would report a low level
/// whether the cut existed or not — and the assertion would be green over a
/// `blank_page` arm that did nothing. The premise below asserts that, on both
/// device arms, before anything else.
#[test]
fn a_noted_page_is_four_bytes_on_the_queue_where_it_was_thirteen_megabytes() {
    // The shipped 256-texel class: 7 slots of pitch 258 a side.
    let page = [1806usize, 1806];
    let page_bytes = page[0] * page[1] * 4;
    assert_eq!(page_bytes, 13_046_544, "the shipped page, spelled out");
    assert!(
        !goes_whole(true, page_bytes) && !goes_whole(false, page_bytes),
        "premise: a page is over the whole-crossing threshold on BOTH arms, so \
         it really would be banded and the level below really has something to \
         be lower than",
    );

    let mut uploads = TextureUploads::without_device();
    assert_eq!(uploads.pending_level_bytes(), 0, "premise: an empty queue");

    uploads.file_blank_page_for_test(egui::TextureId::Managed(4_001), page);
    assert_eq!(
        uploads.pending_bands(),
        1,
        "the page still takes a queue slot — the texture is allocated on the \
         frame the drain first has budget for it, the same deferral every \
         whole delta takes",
    );
    assert_eq!(
        uploads.pending_level_bytes(),
        4,
        "the queue is holding the page's pixels, so the census still charges \
         {page_bytes} B for content nothing samples",
    );

    // Non-triviality: an ordinary band of the same page is still charged in
    // full, so the low figure above is this arm's doing and not a level that
    // reads four bytes for everything.
    let mut ordinary = TextureUploads::without_device();
    ordinary.file_band_for_test(
        egui::TextureId::Managed(4_002),
        std::sync::Arc::new(egui::ColorImage::filled(page, egui::Color32::TRANSPARENT)),
    );
    assert_eq!(
        ordinary.pending_level_bytes(),
        page_bytes as u64,
        "a band carrying the page is charged the page, which is what the arm \
         above is measured against",
    );
}

/// **The queue's peak does not fall when the queue does**, which is the whole
/// of why the high-water mark exists beside the level.
///
/// `upload pending` — the same quantity, published every frame and read by a
/// census line every two seconds — is 97-99 % zeros with a p50 of 0.0 on a
/// 420 s leg, because a picture crosses this queue in fewer frames than the
/// tick. Its own census note carries the reproduction: "a 206.75 MiB raster
/// crossed this queue between two samples and it read 0 B at all 100 ticks".
/// A cut to the queue's simultaneous residency cannot be scored against a
/// figure like that.
///
/// Three readings, and the third is the one a plain store would fail: an empty
/// queue after a full one still answers what the full one held.
#[test]
fn the_pending_peak_survives_the_queue_emptying() {
    let page = [1806usize, 1806];
    let page_bytes = (page[0] * page[1] * 4) as u64;
    let mut uploads = TextureUploads::without_device();
    uploads.note_pending_peak();
    assert_eq!(uploads.pending_peak_bytes(), 0, "premise: an empty queue");

    uploads.file_band_for_test(
        egui::TextureId::Managed(4_101),
        std::sync::Arc::new(egui::ColorImage::filled(page, egui::Color32::TRANSPARENT)),
    );
    uploads.file_band_for_test(
        egui::TextureId::Managed(4_102),
        std::sync::Arc::new(egui::ColorImage::filled(page, egui::Color32::TRANSPARENT)),
    );
    uploads.note_pending_peak();
    assert_eq!(
        uploads.pending_peak_bytes(),
        2 * page_bytes,
        "the peak is the level at the instant it was taken, over BOTH \
         pictures — the simultaneous residency, which is the quantity",
    );

    // What the drain does, and what a level published after it reads.
    uploads.pending.clear();
    assert_eq!(
        uploads.pending_level_bytes(),
        0,
        "premise: the level really did fall to nothing",
    );
    uploads.note_pending_peak();
    assert_eq!(
        uploads.pending_peak_bytes(),
        2 * page_bytes,
        "the peak fell with the queue, so it is a second spelling of the \
         sampled level and scores nothing the level could not",
    );
}

/// **The peak is taken between the file loop and the drain**, pinned
/// structurally for the reason [`the_drain_allocates_for_a_noted_page_and_delivers_it`]
/// gives: `apply` takes a `wgpu::Device`, a `Queue` and an
/// `egui_wgpu::Renderer`, and this suite has `TextureUploads::without_device()`
/// instead.
///
/// The order is the whole figure. `file` is the only thing that adds to the
/// queue and `drain` the only thing that takes away, so a reading moved after
/// the drain misses every picture that arrived and finished on one frame — and
/// on a device with a staging ring that is two whole bands of it.
#[test]
fn the_peak_is_read_before_the_drain() {
    const SOURCE: &str = include_str!("../texture_upload.rs");
    let body = SOURCE
        .split_once("        self.whole_spent = 0;")
        .expect("`apply` resets the whole-crossing budget at its head")
        .1;
    let peak = body.find("self.note_pending_peak();").expect(
        "`apply` no longer takes the queue's high-water mark at all, so \
         `upload residency:` is a constant zero",
    );
    let drain = body
        .find("self.drain(device, queue, renderer);")
        .expect("`apply` no longer drains");
    assert!(
        peak < drain,
        "the peak is taken after the drain, so a picture filed and finished \
         on one frame is missing from it entirely",
    );
}

/// **The drain really allocates for a noted page**, pinned structurally
/// because no test in this module can reach the drain.
///
/// This is a weaker gate than the rest of this file and it is here for a
/// measured reason: deleting the drain's blank arm outright — so a noted page
/// is never allocated, its texture stays the 1x1 seed, and every tile written
/// into it goes nowhere — passes all 126 tests of this crate. `drain` takes a
/// `wgpu::Device`, a `Queue` and an `egui_wgpu::Renderer`, and
/// `TextureUploads::without_device()` is what this suite has instead. A
/// structural pin turns a silent deletion into a failure; it does not turn it
/// into a behavioural one, and nothing here pretends otherwise.
///
/// The presence control is the other half: a needle that rotted would pass
/// over anything, so the arm's own marker is asserted to exist before its
/// contents are.
#[test]
fn the_drain_allocates_for_a_noted_page_and_delivers_it() {
    const SOURCE: &str = include_str!("../texture_upload.rs");
    assert!(
        SOURCE.contains("if let Some(size) = band.blank {"),
        "the drain's blank-page arm is gone or renamed, so the two pins below \
         read nothing",
    );
    let arm = SOURCE
        .split_once("if let Some(size) = band.blank {")
        .expect("the arm was just found")
        .1;
    let arm = &arm[..arm.find('}').unwrap_or(arm.len())];
    assert!(
        arm.contains("self.allocate("),
        "the blank arm no longer creates the page's texture. The seed under \
         this id is 1x1, so every `set_partial` a tile makes into it writes \
         past the texture and the layer draws nothing at all",
    );
    assert!(
        arm.contains("self.delivered.insert("),
        "the blank arm no longer marks the page delivered, so a pane holding \
         its previous raster waits for texels that will never be filed",
    );
}
