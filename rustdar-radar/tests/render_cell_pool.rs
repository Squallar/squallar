//! A render never inherits a pixel from the render before it.

use nexrad_level3::model::{RadialPacket, RadialRun};
use rustdar_radar::render::render_level3_radial_to_image;
use rustdar_radar::types::{IMAGE_SIZE, RadarProduct};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const RADIALS: usize = 360;

const SCALE_FACTOR: f32 = 4.0;

fn packet(bins: usize, painted_bins: usize) -> RadialPacket {
    assert!(
        painted_bins <= bins,
        "cannot paint more gates than there are"
    );
    let radials = (0..RADIALS)
        .map(|i| RadialRun {
            start_angle: i as f32,
            angle_delta: 1.0,
            gate_values: (0..bins)
                .map(|j| {
                    if j >= painted_bins {
                        return 0;
                    }
                    // A field that varies with both range and azimuth, so the
                    // wide packet's outer ring cannot happen to agree with the
                    // narrow one's.
                    let dbz =
                        20.0 + (j as f64 / 30.0).sin() * 25.0 + (i as f64 / 45.0).cos() * 15.0;
                    ((dbz * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 250) as u16
                })
                .collect(),
        })
        .collect();
    RadialPacket {
        first_range_bin: 0,
        num_range_bins: bins as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: SCALE_FACTOR,
        is_legacy: false,
        xdr_data_scale: None,
        xdr_data_offset: None,
        radials,
    }
}

/// A side the pool has to serve that is not the base one.
const SMALL_SIDE: usize = 1024;

fn render_at(p: &RadialPacket, side_ceiling_px: usize) -> (Vec<u8>, Vec<u32>) {
    let out =
        render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, side_ceiling_px)
            .expect("the packet renders");
    (out.image, out.values.iter().map(|v| v.to_bits()).collect())
}

fn render(p: &RadialPacket) -> (Vec<u8>, Vec<u32>) {
    render_at(p, IMAGE_SIZE)
}

/// How many pixels the render claimed.
fn painted(values: &[u32]) -> usize {
    values
        .iter()
        .filter(|&&bits| !f32::from_bits(bits).is_nan())
        .count()
}

#[test]
fn a_render_never_inherits_a_pixel_from_the_one_before_it() {
    // Every packet declares 920 bins, so every render below is a 230 km frame on the
    // same raster; what differs is how much of it is claimed.
    const BINS: usize = 920;
    let narrow = packet(BINS, 120);
    let wide = packet(BINS, BINS);
    let blank = packet(BINS, 0);

    let (narrow_image, narrow_values) = render(&narrow);
    let narrow_pixels = painted(&narrow_values);

    let (wide_image, wide_values) = render(&wide);
    let wide_pixels = painted(&wide_values);

    assert!(
        narrow_pixels > 10_000,
        "the narrow packet has to paint for this test to say anything: {narrow_pixels} pixels"
    );
    assert!(
        wide_pixels > narrow_pixels * 4,
        "the wide packet has to be much larger than the narrow one: {wide_pixels} against \
         {narrow_pixels} pixels"
    );
    assert_ne!(
        wide_image, narrow_image,
        "the two packets have to render differently, or nothing below can fail"
    );

    let (again_image, again_values) = render(&narrow);
    assert_eq!(
        painted(&again_values),
        narrow_pixels,
        "the narrow render claimed a different number of pixels when it followed a wider one"
    );
    assert_eq!(
        again_image, narrow_image,
        "the narrow render's texture changed when it followed a wider one"
    );
    assert_eq!(
        again_values, narrow_values,
        "the narrow render's value grid changed when it followed a wider one"
    );

    let (small_image, small_values) = render_at(&wide, SMALL_SIDE);
    assert_eq!(
        small_values.len(),
        SMALL_SIDE * SMALL_SIDE,
        "a render at a smaller side got a value grid of the pooled buffer's length, not its own"
    );
    assert_eq!(
        small_image.len(),
        small_values.len() * 4,
        "the texture and the value grid disagree about how many pixels were rendered"
    );
    assert!(
        painted(&small_values) > 10_000,
        "the smaller raster has to paint for the two assertions below to say anything: {} pixels",
        painted(&small_values)
    );

    let (small_blank_image, small_blank_values) = render_at(&blank, SMALL_SIDE);
    assert_eq!(
        painted(&small_blank_values),
        0,
        "a render that paints no gate must leave every value NaN, at any raster side"
    );
    assert!(
        small_blank_image.iter().all(|&b| b == 0),
        "a render that paints no gate must leave every texel zero, at any raster side"
    );

    let (grown_image, grown_values) = render(&narrow);
    assert_eq!(
        painted(&grown_values),
        narrow_pixels,
        "the narrow render claimed a different number of pixels when it followed a smaller raster"
    );
    assert_eq!(
        grown_image, narrow_image,
        "the narrow render's texture changed when the pooled buffer had to grow for it"
    );
    assert_eq!(
        grown_values, narrow_values,
        "the narrow render's value grid changed when the pooled buffer had to grow for it"
    );

    let (blank_image, blank_values) = render(&blank);
    assert_eq!(
        painted(&blank_values),
        0,
        "a render that paints no gate must leave every value NaN"
    );
    assert!(
        blank_image.iter().all(|&b| b == 0),
        "a render that paints no gate must leave every texel zero"
    );
}
