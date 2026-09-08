//! **Where a finished loop render becomes a reply**, and the fork between its
//! two surfaces.
//!
//! What is at stake is that exactly one surface reaches the pane and that the
//! plane's turn into a payload happens *here* — off the frame thread — rather
//! than at the arrival. A plane that arrived and was not turned would reach
//! `loop_frame_fan` as nothing at all, and since a polar reply carries no
//! raster either, the frame would retire as failed with no line saying why.

use super::{LoopReplySurface, loop_reply_surface};
use squallar_radar::frame::{RasterImage, RenderedFrame};

const SIDE: usize = 4;
const SITE_LAT: f64 = 35.33;
const SITE_LON: f64 = -97.28;

/// A one-tilt volume of 12 radials carrying an eight-bit reflectivity moment.
fn a_scan() -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let radials = (0..12u16)
        .map(|i| {
            let bytes: Vec<u8> = (0..40usize).map(|g| ((g % 200) + 2) as u8).collect();
            Radial::new(
                0,
                i,
                f32::from(i) * 30.0,
                30.0,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                Some(MomentData::from_fixed_point(
                    40, 2125, 250, 8, 2.0, 66.0, bytes,
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    let cut = ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        20,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    );
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![cut],
        ),
        vec![Sweep::new(1, radials)],
    )
}

/// The frame the polar renderer produces for that volume.
fn a_polar_frame() -> RenderedFrame {
    let render = squallar_radar::render::render_sweep_plane(
        &a_scan(),
        0.5,
        squallar_radar::types::RadarProduct::Reflectivity,
        &squallar_radar::nyquist::DeclaredNyquist::empty(),
    )
    .expect("the fixture renders as a plane");
    RenderedFrame::from(render)
}

/// A frame whose surface is a raster of exactly the dispatched side.
fn a_raster_frame() -> RenderedFrame {
    let mut frame = a_polar_frame();
    frame.codes = None;
    frame.image = RasterImage::Bytes(vec![7u8; SIDE * SIDE * 4]);
    frame
}

/// **A polar reply carries the payload and no picture; a raster reply carries
/// the picture and no payload.**
///
/// The pairing is the test. "No image" alone would also be true of a delivery
/// that had stopped producing anything, and "an image" alone would be true of
/// one that ignored the plane; each arm is the other's control.
#[test]
fn exactly_one_surface_reaches_the_reply() {
    let polar = loop_reply_surface(Some(a_polar_frame()), SIDE, SITE_LAT, SITE_LON, 0);
    let payload = polar
        .codes
        .as_ref()
        .expect("the plane became a payload here");
    assert!(
        payload.is_well_formed(),
        "the payload built off the frame thread does not describe itself consistently",
    );
    assert!(
        polar.image.is_none(),
        "a polar reply also carried a picture, which is both surfaces at once",
    );
    assert!(
        !polar.polar.geometry().is_empty(),
        "the geometry a pane places the fan by did not travel",
    );

    let raster = loop_reply_surface(Some(a_raster_frame()), SIDE, SITE_LAT, SITE_LON, 0);
    assert!(raster.image.is_some(), "the raster arm lost its picture");
    assert!(raster.codes.is_none());

    // **A frame carrying both is a producer bug, and the plane wins.** Nothing
    // this build writes produces one — `render_sweep_plane` allocates no
    // raster — so it is built by hand here, which is the only way this branch
    // can be reached at all. Sending both would put two representations of one
    // picture on the pane, which costs more than the raster the plane replaced.
    let mut both = a_polar_frame();
    both.image = RasterImage::Bytes(vec![7u8; SIDE * SIDE * 4]);
    let reply = loop_reply_surface(Some(both), SIDE, SITE_LAT, SITE_LON, 0);
    assert!(
        reply.codes.is_some(),
        "the plane lost to a raster beside it"
    );
    assert!(
        reply.image.is_none(),
        "a reply carried a plane and a picture, so a pane would hold both",
    );
}

/// The frame's own numbers ride whichever surface it carried.
///
/// Both arms, because the raster arm reads them out of a different branch: a
/// polar reply that reported `0.0` would place its fan at the site and a pane
/// would draw a dot.
#[test]
fn both_surfaces_carry_the_frames_own_range_and_fold() {
    for (label, mut frame) in [("polar", a_polar_frame()), ("raster", a_raster_frame())] {
        frame.max_range_km = 123.5;
        frame.nyquist_ms = Some(26.5);
        let reply = loop_reply_surface(Some(frame), SIDE, SITE_LAT, SITE_LON, 0);
        assert_eq!(reply.max_range_km, 123.5, "{label} lost its extent");
        assert_eq!(reply.nyquist_ms, Some(26.5), "{label} lost its fold limit");
    }
}

/// A reply with neither surface is the failure arm, and it discards the
/// frame's numbers rather than reporting a range no picture was drawn at.
#[test]
fn a_picture_of_the_wrong_length_is_the_failure_arm() {
    let mut frame = a_raster_frame();
    frame.image = RasterImage::Bytes(vec![7u8; SIDE * SIDE * 4 - 1]);
    frame.max_range_km = 123.5;
    let reply = loop_reply_surface(Some(frame), SIDE, SITE_LAT, SITE_LON, 0);
    assert!(reply.image.is_none());
    assert!(reply.codes.is_none());
    assert_eq!(reply.max_range_km, 0.0);
}

/// A render that produced nothing still answers, so the pane's in-flight mark
/// is cleared.
#[test]
fn a_render_that_produced_nothing_still_answers() {
    let LoopReplySurface {
        image,
        codes,
        max_range_km,
        nyquist_ms,
        ..
    } = loop_reply_surface(None, SIDE, SITE_LAT, SITE_LON, 0);
    assert!(image.is_none());
    assert!(codes.is_none());
    assert_eq!(max_range_km, 0.0);
    assert_eq!(nyquist_ms, None);
}
