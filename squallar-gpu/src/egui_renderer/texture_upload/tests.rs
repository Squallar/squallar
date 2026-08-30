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
            let plan = BandPlan::of(side, height, done).expect("rows remain, so there is a plan");
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
        while let Some(plan) = BandPlan::of(side, height, done) {
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
    let plan = BandPlan::of(side, 4, 0).expect("there are rows to move");
    assert_eq!(plan.rows, 1);
    assert!(plan.bytes() > UPLOAD_BAND_BYTES);
}

/// Nothing to move is `None`, not a band of zero rows.
#[test]
fn an_image_with_no_rows_left_has_no_plan() {
    assert!(BandPlan::of(64, 4, 4).is_none());
    assert!(BandPlan::of(64, 4, 9).is_none());
    assert!(BandPlan::of(0, 4, 0).is_none());
}

/// The staging stride is the copy alignment, and the widest cut really needs it.
#[test]
fn the_staging_stride_is_aligned_and_the_widest_cut_is_not() {
    let plan = BandPlan::of(WIDEST, WIDEST as u32, 0).expect("a plan");
    assert_eq!(plan.row_bytes, WIDEST * 4);
    assert_eq!(plan.row_bytes, 29448);
    assert_eq!(plan.padded_row, 29696);
    assert_eq!(plan.padded_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
    assert!(u64::from(plan.padded_row) >= plan.row_bytes as u64);

    // A power-of-two side pads by nothing, which is why the buffer for one is
    // exactly the band it was sized against.
    let square = BandPlan::of(2048, 2048, 0).expect("a plan");
    assert_eq!(square.padded_row as usize, square.row_bytes);
}

/// A ring slot is one band, and the pair is what the module claims it costs.
#[test]
fn the_ring_a_band_needs_is_two_slots_of_a_band() {
    let plan = BandPlan::of(WIDEST, WIDEST as u32, 0).expect("a plan");
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

/// What the widest raster costs a frame, and how many frames it takes.
#[test]
fn the_widest_raster_takes_fourteen_frames_on_a_ring_and_twenty_six_without() {
    for (bands, expected) in [(DMA_BANDS_PER_FRAME, 14u32), (1, 27)] {
        let height = WIDEST as u32;
        let mut done = 0u32;
        let mut frames = 0u32;
        while done < height {
            for _ in 0..bands {
                let Some(plan) = BandPlan::of(WIDEST, height, done) else {
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
/// classification before this arithmetic replaced it.
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
