//! The premultiply moved to the producer, and **no pixel moved with it**. The
//! claim is bit-identity, not a tolerance.

use super::*;
use egui::ColorImage;
use rustdar_radar::jobs::{RadarPlanJob, SectionJob, VoxelJob};

/// Every pixel that can exist reaches the same `Color32` by the new route as
/// by the old one. `Color32::from_rgba_unmultiplied` is a three-arm match on
/// the alpha (a `TRANSPARENT` short-circuit at 0, an alpha-only `from_rgb` at
/// 255, and a 64 KiB table between), and all three arms treat the colour
/// channels independently given the alpha.
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
            // the conversion is not per-channel, or if a channel is
            // transposed.
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

/// The identity holds through a **real plan-view rasterization**. The palette
/// produces alpha 0 and alpha 180 and nothing else (`palette.rs`'s
/// `TRANSPARENCY`), which is one fast arm and one table arm.
#[test]
fn a_real_plan_view_render_lands_on_the_same_picture() {
    let request = tests::a_job();
    // The ceiling off the request's envelope, exactly as the row's `run` reads
    // it, so the two rasterizations cannot come out at two sizes.
    let side_ceiling_px = request.geometry.side_ceiling_px as usize;
    let plan = request
        .job
        .downcast_ref::<RadarPlanJob>()
        .expect("`a_job` is the radar job");

    // What the rasterizer writes, before the job's output stage touches it.
    let straight = rustdar_radar::render::render_from_sized(&plan.input, side_ceiling_px)
        .expect("the fixture sweep rasterizes")
        .image;
    let side = rustdar_device_profile::constants::raster_side_from_rgba_len(straight.len())
        .expect("the rasterizer answers at a side this build makes");

    let frame = execute(&request)
        .and_then(|out| out.take::<rustdar_radar::frame::RenderedFrame>())
        .expect("the same job through the funnel draws the same sweep");

    assert_eq!(
        ColorImage::from_rgba_premultiplied([side, side], &frame.image),
        ColorImage::from_rgba_unmultiplied([side, side], &straight),
        "a real plan-view render is not the picture it was before the \
         premultiply moved into `execute`",
    );
}

/// The same claim for the **cross-section** raster, which also changed thread.
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
        .and_then(|out| out.take::<rustdar_radar::xsect::CrossSection>())
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
        .and_then(|out| out.take::<rustdar_radar::voxel::VolumeGrid>())
        .expect("the same job through the funnel builds the same grid");

    assert_eq!(
        through, built,
        "the output stage altered a voxel grid. It carries no raster; the \
         `Voxels` arm exists to do nothing to it.",
    );
}
