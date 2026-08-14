//! Which alpha convention each rasterizer writes, checked against the bytes
//! rather than against the field.
//!
//! [`AlphaMode`] exists because this module produces both, and the consumer
//! (`app_fetch::overlay_color_image`) cannot tell them apart by looking: the two
//! egui constructors accept identical input and neither can fail. A wrong
//! declaration is therefore silent all the way to the screen, where it shows up
//! as every translucent pixel being the wrong colour — which is a thing nobody
//! diffs.
//!
//! So each test below asserts an invariant of the *bytes* that only one
//! convention can satisfy, and that is what makes the declaration checkable.
//! Premultiplied RGB is `round(c · a / 255)`, so no channel can exceed alpha;
//! straight RGB is the colour table's own value, so a bright translucent entry
//! has channels far above it. One buffer cannot satisfy both.

use std::collections::HashSet;

use super::{AlertPaint, AlertsInput, AlphaMode, rasterize_model_data, rasterize_nws_alerts};
use crate::hrrr::{HrrrGridData, ModelParameter};
use crate::nws::alert::AlertCategory;
use crate::types::{GeoBounds, HatchPattern, OverlayFeature};

const W: u32 = 96;
const H: u32 = 96;

const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 34.0,
    max_lat: 36.0,
    min_lon: -99.0,
    max_lon: -97.0,
};

/// Every pixel the raster actually drew — alpha above zero.
fn drawn(rgba: &[u8]) -> Vec<[u8; 4]> {
    rgba.chunks_exact(4)
        .filter(|p| p[3] > 0)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect()
}

/// A square covering most of the texture, in a saturated translucent colour, so
/// the two conventions are far apart on every channel.
fn alert_fixture() -> AlertPaint {
    let ring = vec![
        (34.2, -98.8),
        (34.2, -97.2),
        (35.8, -97.2),
        (35.8, -98.8),
        (34.2, -98.8),
    ];
    AlertPaint {
        id: "urn:test".into(),
        category: AlertCategory::Warning,
        features: vec![OverlayFeature::new(
            vec![vec![ring]],
            [255, 0, 0, 128],
            [0, 0, 0, 0],
            "T".into(),
            String::new(),
            HatchPattern::None,
        )],
    }
}

/// tiny-skia's own layout, unconverted: no channel above alpha.
///
/// Before this, `rasterize_nws_alerts` divided the whole buffer back out to
/// straight alpha and `ColorImage::from_rgba_unmultiplied` multiplied it
/// straight back in on the very next statement — 22.3 ms a render at 5760×3240
/// on the measurement box, plus one lossy `u8` round trip, to arrive where it
/// started. The buffer is handed over as drawn now, and this is the shape of
/// "as drawn".
#[test]
fn the_polygon_rasterizers_hand_over_premultiplied_pixels() {
    let out = rasterize_nws_alerts(
        &AlertsInput {
            alerts: vec![alert_fixture()],
            enabled_categories: vec![AlertCategory::Warning],
            hidden_ids: HashSet::new(),
            device_scale: 1.0,
        },
        &BOUNDS,
        W,
        H,
    );
    assert_eq!(
        out.alpha,
        AlphaMode::Premultiplied,
        "the alert rasterizer stopped declaring tiny-skia's own convention",
    );

    let pixels = drawn(&out.rgba);
    assert!(
        pixels.len() > 1000,
        "fixture drew {} pixels; the invariant below says nothing about an \
         empty texture",
        pixels.len()
    );
    for p in &pixels {
        assert!(
            p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3],
            "pixel {p:?} has a colour channel above its alpha, which \
             premultiplied RGBA cannot: the un-premultiply is back, or the \
             fill is being written straight",
        );
    }
    // …and specifically the premultiply of the fixture's own fill, so this
    // cannot be satisfied by a texture that is merely dark.
    assert!(
        pixels.contains(&[128, 0, 0, 128]),
        "no pixel is the premultiply of the fixture's 255,0,0 @ 128 fill",
    );
}

/// **The trap.** `rasterize_model_data` never went through tiny-skia and never
/// called the un-premultiply, so it was already writing straight alpha while
/// every neighbour wrote premultiplied-then-converted. It reaches the uploader
/// through the same `prepare_rasterize` arm as the polygon overlays, so one
/// global choice of egui constructor is wrong about one of them whichever way
/// it is made — and HRRR is the half that would go wrong silently, because
/// nobody has a reference picture of it.
///
/// The assertion is that the bytes are the palette's own, unscaled: a colour
/// channel *above* alpha, which premultiplied RGBA cannot represent.
#[test]
fn model_data_hands_over_straight_alpha() {
    let out = rasterize_model_data(&cape_grid(), &BOUNDS, W, H);
    assert_eq!(
        out.alpha,
        AlphaMode::Straight,
        "rasterize_model_data has been declared premultiplied. It writes \
         `parameter.color_for_value` bytes into the buffer directly — the \
         palette's own straight RGBA — so declaring it premultiplied darkens \
         every HRRR pixel by its own alpha.",
    );

    let pixels = drawn(&out.rgba);
    assert!(!pixels.is_empty(), "fixture drew nothing");
    // CAPE 1000 J/kg is `[255, 255, 0, 160]`: bright, and translucent at an
    // alpha every colour channel clears.
    assert!(
        pixels.contains(&[255, 255, 0, 160]),
        "the CAPE palette entry this fixture was built on has moved, so the \
         test no longer distinguishes the two conventions. Pick another entry \
         that is bright and translucent. Found e.g. {:?}",
        &pixels[..pixels.len().min(4)]
    );
}

/// 4×4 of uniform 1000 J/kg CAPE over [`BOUNDS`], summarised the way the fetch
/// path does.
fn cape_grid() -> HrrrGridData {
    let parameter = ModelParameter::SurfaceBasedCape;
    let (ni, nj) = (4usize, 4usize);
    let values = vec![1000.0f32; ni * nj];
    let mut lats = Vec::with_capacity(ni * nj);
    let mut lons = Vec::with_capacity(ni * nj);
    for j in 0..nj {
        for i in 0..ni {
            // Row 0 is the northern edge, matching the fixture grids next door.
            lats.push(BOUNDS.max_lat - (BOUNDS.max_lat - BOUNDS.min_lat) * (j as f64 / 3.0));
            lons.push(BOUNDS.min_lon + (BOUNDS.max_lon - BOUNDS.min_lon) * (i as f64 / 3.0));
        }
    }
    let (visible_points, value_range) = crate::hrrr::summarize_values(&values, parameter);
    HrrrGridData {
        parameter,
        values,
        coords: crate::hrrr::GridCoords::Explicit { lats, lons },
        ni,
        nj,
        bounds: BOUNDS,
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap(),
        forecast_hour: parameter.forecast_hour(),
        visible_points,
        value_range,
    }
}
