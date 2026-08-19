//! The premultiply moved to the producer, and **no pixel moved with it**.
//!
//! `execute` now hands back rasters in egui's premultiplied convention, and
//! every consumer reads them with `ColorImage::from_rgba_premultiplied` instead
//! of `from_rgba_unmultiplied`. That is a change to *where* per-pixel arithmetic
//! runs — off the browser's main thread, and off the frame thread entirely for
//! the static cross-section — and it must be a change to nothing else.
//!
//! The claim is bit-identity, not a tolerance, and it rests on one structural
//! fact: `Color32::from_rgba_premultiplied(r, g, b, a)` is `Self([r, g, b, a])`,
//! a constructor that computes nothing. So a byte written by
//! `Color32::from_rgba_unmultiplied` and read back through it is the same
//! `Color32` the old consumer computed — not an approximation of it. The tests
//! below stop that from being an argument: the first walks every
//! (channel, alpha) pair that can exist, and the next two run a real
//! rasterization through both paths and compare the finished `ColorImage`s.

use super::*;
use egui::ColorImage;
use rustdar_radar::jobs::{RadarPlanJob, SectionJob, VoxelJob};

/// Every pixel that can exist reaches the same `Color32` by the new route as by
/// the old one.
///
/// **Exhaustive, and in a sense worth stating precisely.**
/// `Color32::from_rgba_unmultiplied` is a three-arm match on the alpha — a
/// `TRANSPARENT` short-circuit at 0, an alpha-only `from_rgb` at 255, and in
/// between a lookup of `(channel, alpha)` in a 64 KiB table `ecolor` builds
/// once. All three arms treat the three colour channels independently given the
/// alpha, so the 65,536 (channel, alpha) pairs this walks cover every one of the
/// 2³² pixels that can be written — and the second family below, whose three
/// channels hold three *different* values under the same alpha, is what stops
/// that independence from being an assumption.
#[test]
fn every_pixel_that_can_exist_reaches_the_same_color32() {
    let mut straight: Vec<u8> = Vec::with_capacity(2 * 65_536 * 4);
    for alpha in 0..=u8::MAX {
        for value in 0..=u8::MAX {
            // Grey, so a table indexed on the wrong byte of the pair still
            // agrees here and has to be caught by the family below.
            straight.extend_from_slice(&[value, value, value, alpha]);
        }
    }
    for alpha in 0..=u8::MAX {
        for value in 0..=u8::MAX {
            // Three different channels under one alpha: the case that fails if
            // the conversion is not per-channel, or if a channel is dropped or
            // transposed on the way back into the buffer.
            straight.extend_from_slice(&[value, 255 - value, value.wrapping_mul(7), alpha]);
        }
    }
    let size = [straight.len() / 4, 1];

    let mut premultiplied = straight.clone();
    premultiply_raster(&mut premultiplied);

    assert_eq!(
        ColorImage::from_rgba_premultiplied(size, &premultiplied),
        ColorImage::from_rgba_unmultiplied(size, &straight),
        "the premultiply at the producer does not reproduce what the consumer \
         used to compute. Every byte of every pixel this build can write is in \
         this buffer, so a failure here is a picture that has shifted, not a \
         corner case.",
    );
}

/// The identity holds through a **real plan-view rasterization**, not only over
/// a synthetic sweep of bytes.
///
/// The comparison is against the rasterizer's own output read the old way — the
/// exact expression `plan_view_image` and `loop_frame_image` carried before this
/// change — so what is pinned is the two paths, end to end, on pixels the
/// palette actually produces. Those are alpha 0 and alpha 180 and nothing else
/// (`palette.rs`'s `TRANSPARENCY`), which is one fast arm and one table arm.
#[test]
fn a_real_plan_view_render_lands_on_the_same_picture() {
    let request = tests::a_job();
    // The ceiling off the request's envelope, exactly as the row's `run`
    // reads it, so the two rasterizations cannot come out at two sizes.
    let side_ceiling_px = request.geometry.side_ceiling_px as usize;
    let plan = request
        .job
        .downcast_ref::<RadarPlanJob>()
        .expect("`a_job` is the radar job");

    // What the rasterizer writes, before the job's output stage touches it.
    let straight = rustdar_radar::render::render_from_sized(&plan.input, side_ceiling_px)
        .expect("the fixture sweep rasterizes")
        .image;
    let side = crate::constants::raster_side_from_rgba_len(straight.len())
        .expect("the rasterizer answers at a side this build makes");

    let frame = execute(&request)
        .and_then(JobOutput::frame)
        .expect("the same job through the funnel draws the same sweep");

    assert_eq!(
        ColorImage::from_rgba_premultiplied([side, side], &frame.image),
        ColorImage::from_rgba_unmultiplied([side, side], &straight),
        "a real plan-view render is not the picture it was before the \
         premultiply moved into `execute`",
    );
}

/// The same claim for the **cross-section** raster, which is the one that also
/// changed thread: `app_render::upload_section_raster` converted on the frame
/// thread on both targets and now converts on neither.
#[test]
fn a_real_section_cut_lands_on_the_same_picture() {
    use rustdar_radar::xsect::{SECTION_HEIGHT, SECTION_WIDTH};

    let section_job = tests::a_section_job();
    let job = section_job
        .job
        .downcast_ref::<SectionJob>()
        .expect("`a_section_job` is the section job");
    let input = &job.input;
    let (scan, declared) = (input.to_scan(), input.declared_nyquist());
    let straight = rustdar_radar::xsect::render_section(
        rustdar_radar::nyquist::Volume::new(&scan, &declared),
        &job.request,
        input.radar_lat(),
        input.radar_lon(),
        input.storm_motion(),
    )
    .expect("the fixture volume cuts")
    .image()
    .to_vec();

    let cut = execute(&tests::a_section_job())
        .and_then(JobOutput::section)
        .expect("the same job through the funnel cuts the same section");

    let size = [SECTION_WIDTH, SECTION_HEIGHT];
    assert_eq!(
        ColorImage::from_rgba_premultiplied(size, cut.image()),
        ColorImage::from_rgba_unmultiplied(size, &straight),
        "a real cross-section is not the picture it was before the premultiply \
         moved into `execute`",
    );
}

/// A voxel grid carries no raster, and the output stage must leave it exactly
/// as the builder answered.
///
/// Named rather than left to inference because [`premultiplied`] matches on the
/// output kind, and the `Voxels` arm is the one that has to *decline* to do
/// anything. A wildcard there would have been silently right today and silently
/// wrong for the next output kind that carries pixels.
#[test]
fn a_voxel_grid_passes_through_the_output_stage_untouched() {
    let voxel_job = tests::a_voxel_job();
    let job = voxel_job
        .job
        .downcast_ref::<VoxelJob>()
        .expect("`a_voxel_job` is the voxel job");
    let input = &job.input;
    let (scan, declared) = (input.to_scan(), input.declared_nyquist());
    let built = rustdar_radar::voxel::build_voxels_with_motion(
        rustdar_radar::nyquist::Volume::new(&scan, &declared),
        &job.request,
        input.radar_lat(),
        input.radar_lon(),
        input.storm_motion(),
    )
    .expect("the fixture volume builds a grid");

    let through = execute(&tests::a_voxel_job())
        .and_then(JobOutput::voxels)
        .expect("the same job through the funnel builds the same grid");

    assert_eq!(
        *through, built,
        "the output stage altered a voxel grid. It carries no raster; the \
         `Voxels` arm exists to do nothing to it.",
    );
}
