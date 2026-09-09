//! **Every item rasterizer names why it painted nothing.**
//!
//! Seven of the eleven handlers that dispatch an overlay raster share the
//! seven functions below, and until 2026-09-09 not one of them armed a
//! [`BlankReason`]: every blank they produced settled `Unattributed`. Measured
//! over a 32-leg browser arm on 2026-09-08 — real panel, hardware adapter,
//! both engines, 7,499 pictures — that was **67.5 % of all blanks pooled, and
//! 100 % of blanks in every leg whose camera stayed over the data**. The
//! residue is what these tests exist to keep from coming back.
//!
//! **Every case here reads its answer through
//! [`RasterizeOutput::blank_reason`], never off the `blank_reason` field.**
//! That accessor is gated on the raster having actually settled blank, so a
//! fixture that paints something answers `None` and fails the assertion. The
//! alternative — reading the armed field directly — would pass on a fixture
//! that never reproduced a blank at all, which is the vacuity this whole
//! breakdown was added to end.
//!
//! **And every case settles through the production output stage**,
//! `JobOut::discard_blank_rasters`, which is the second half of
//! `offload::execute`. What decides blank-versus-painted is therefore
//! `has_ink` over the rasterizer's own bytes and never the fixture's opinion.

use super::*;
use crate::types::HatchPattern;
use squallar_source::job::JobOut;

/// Oklahoma. Every "on the texture" fixture sits inside this and every
/// "off the texture" one sits in the Gulf of Guinea, which is as far from it
/// as a lat/lon pair gets without leaving the map.
fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

const W: u32 = 64;
const H: u32 = 32;

/// Settle a raster the way the run funnel's output stage settles it, and read
/// back the reason it armed.
///
/// `None` means the raster **painted**, which for every fixture below is a
/// failure of the fixture rather than of the arming: each one is built to
/// reproduce a real blank.
fn reason_of(mut out: RasterizeOutput) -> Option<BlankReason> {
    out.discard_blank_rasters();
    out.blank_reason()
}

/// A ring of three points about `(lat, lon)`, big enough to project to
/// something a rasterizer would fill if it were in view.
fn ring(lat: f64, lon: f64) -> squallar_geo::GeoPolygon {
    vec![vec![
        (lat, lon),
        (lat + 0.5, lon),
        (lat + 0.5, lon + 0.5),
        (lat, lon + 0.5),
    ]]
}

/// A feature at `(lat, lon)` with the given fill and stroke.
///
/// **Through `OverlayFeature::new` and never by hand**, because that
/// constructor is what computes `geo_bounds` from the polygons — and
/// `geo_bounds` is the whole of what `feature_survives_cull` reads. A
/// hand-built feature would carry whatever extent the test felt like and prove
/// nothing about the cull.
fn feature(lat: f64, lon: f64, fill: [u8; 4], stroke: [u8; 4]) -> OverlayFeature {
    OverlayFeature::new(
        vec![ring(lat, lon)],
        fill,
        stroke,
        String::new(),
        String::new(),
        HatchPattern::None,
    )
}

const OPAQUE: [u8; 4] = [200, 30, 30, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn outlooks(features: Vec<OverlayFeature>) -> RasterizeOutput {
    rasterize_spc_outlooks(
        &OutlooksInput {
            features,
            hatch_color: CLEAR,
            device_scale: 1.0,
        },
        &bounds(),
        W,
        H,
    )
}

fn discussions(polygons: Vec<squallar_geo::GeoPolygon>) -> RasterizeOutput {
    rasterize_spc_discussions(
        &DiscussionsInput {
            discussions: polygons
                .into_iter()
                .map(|polygon| DiscussionPaint {
                    md_type: crate::spc::discussion::MdType::Convective,
                    polygon,
                })
                .collect(),
            device_scale: 1.0,
        },
        &bounds(),
        W,
        H,
    )
}

fn alerts(
    alerts: Vec<AlertPaint>,
    enabled: Vec<AlertCategory>,
    hidden: &[&str],
) -> RasterizeOutput {
    rasterize_nws_alerts(
        &AlertsInput {
            alerts,
            enabled_categories: enabled,
            hidden_ids: hidden.iter().map(|id| (*id).to_string()).collect(),
            device_scale: 1.0,
        },
        &bounds(),
        W,
        H,
    )
}

fn alert(id: &str, lat: f64, lon: f64, fill: [u8; 4]) -> AlertPaint {
    AlertPaint {
        id: id.to_string(),
        category: AlertCategory::Warning,
        features: std::sync::Arc::new(vec![feature(lat, lon, fill, fill)]),
    }
}

fn coverage(sites: Vec<CoverageSite>, b: GeoBounds, w: u32, h: u32) -> RasterizeOutput {
    rasterize_radar_coverage(
        &CoverageInput {
            sites,
            device_scale: 1.0,
        },
        &b,
        w,
        h,
    )
}

fn metar(obs: Vec<crate::metar::types::MetarOb>) -> RasterizeOutput {
    rasterize_metar_stations(
        &MetarInput {
            obs: std::sync::Arc::new(obs),
            zoom: 8.0,
            is_dark: true,
            device_scale: 1.0,
        },
        &bounds(),
        W,
        H,
    )
}

fn station(lat: f64, lon: f64) -> crate::metar::types::MetarOb {
    crate::metar::types::MetarOb {
        station_id: "KTST".into(),
        name: "KTST".into(),
        lat,
        lon,
        elev_m: None,
        temp_c: None,
        dewp_c: None,
        wind_dir: None,
        wind_speed_kt: None,
        wind_gust_kt: None,
        visibility: None,
        altimeter_hpa: None,
        mslp_hpa: None,
        flight_category: None,
        raw_ob: String::new(),
        clouds: Vec::new(),
        wx_string: None,
        obs_time: String::new(),
    }
}

fn at() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 9)
        .expect("a real date")
        .and_hms_opt(12, 0, 0)
        .expect("a real time")
}

fn reports(reports: Vec<ReportPaint>) -> RasterizeOutput {
    rasterize_storm_reports(
        &ReportsInput {
            reports: std::sync::Arc::new(reports),
            zoom: 8.0,
            is_dark: true,
            device_scale: 1.0,
            as_of: at(),
        },
        &bounds(),
        W,
        H,
    )
}

fn report(lat: f64, lon: f64, valid: Option<chrono::NaiveDateTime>) -> ReportPaint {
    ReportPaint {
        kind: StormReportKind::Tornado,
        lat,
        lon,
        valid,
    }
}

fn strikes(flashes: Vec<FlashPaint>, window_secs: f64) -> RasterizeOutput {
    rasterize_glm_strikes(
        &GlmStrikesInput {
            flashes: std::sync::Arc::new(flashes),
            zoom: 8.0,
            is_dark: true,
            time_window_secs: window_secs,
            now: at(),
            device_scale: 1.0,
        },
        &bounds(),
        W,
        H,
    )
}

fn flash(lat: f64, lon: f64, time: chrono::NaiveDateTime) -> FlashPaint {
    FlashPaint {
        lat,
        lon,
        time,
        energy: Some(1e-14),
    }
}

/// **Every one of the seven rasterizers, handed an empty list, says so.**
///
/// One test over all seven rather than seven tests, because the claim is about
/// the set: a row added to the funnel that does not arm is what this is
/// watching for, and a per-row test can only ever cover the rows somebody
/// remembered to write one for.
#[test]
fn an_empty_list_is_empty_input_in_every_item_rasterizer() {
    let cases: [(&str, RasterizeOutput); 7] = [
        ("spc outlooks", outlooks(Vec::new())),
        ("spc discussions", discussions(Vec::new())),
        (
            "nws alerts",
            alerts(Vec::new(), vec![AlertCategory::Warning], &[]),
        ),
        ("radar coverage", coverage(Vec::new(), bounds(), W, H)),
        ("metar stations", metar(Vec::new())),
        ("storm reports", reports(Vec::new())),
        ("glm strikes", strikes(Vec::new(), 300.0)),
    ];
    for (name, out) in cases {
        assert_eq!(
            reason_of(out),
            Some(BlankReason::EmptyInput),
            "{name} was handed nothing to draw and did not settle blank saying \
             so. A `None` here means it painted something, which the fixture \
             cannot have given it; any other reason means it named the wrong \
             cause",
        );
    }
}

/// **Every row that can be panned away from says `outside-view`.**
///
/// The fixture's property, spelled out because a fixture that does not carry
/// it makes this test unable to reach the defect: each item is at `(0, 0)` and
/// the viewport is over Oklahoma, so **the item is real, resolvable and
/// projectable** and misses the texture on position alone. A fixture with no
/// items would reach `EmptyInput` and a fixture with a bad timestamp would
/// reach `FilteredOut`, and neither would show that the *cull* is what armed.
#[test]
fn an_item_projected_off_the_texture_is_outside_view() {
    let cases: [(&str, RasterizeOutput); 6] = [
        (
            "spc outlooks",
            outlooks(vec![feature(0.0, 0.0, OPAQUE, OPAQUE)]),
        ),
        ("spc discussions", discussions(vec![ring(0.0, 0.0)])),
        (
            "nws alerts",
            alerts(
                vec![alert("a", 0.0, 0.0, OPAQUE)],
                vec![AlertCategory::Warning],
                &[],
            ),
        ),
        (
            "radar coverage",
            coverage(vec![CoverageSite { lat: 0.0, lon: 0.0 }], bounds(), W, H),
        ),
        ("metar stations", metar(vec![station(0.0, 0.0)])),
        ("storm reports", reports(vec![report(0.0, 0.0, None)])),
    ];
    for (name, out) in cases {
        assert_eq!(
            reason_of(out),
            Some(BlankReason::OutsideView),
            "{name} had one real item, off this texture, and did not name the \
             cull that dropped it",
        );
    }
}

/// GLM answers `outside-view` too, and by its **own** geographic test.
///
/// Separate from the six above because the branch is not the shared one: the
/// flash loop culls on `bounds` in degrees before it projects, so a flash south
/// of the box never reaches the texture-rect test the others use. Both arms
/// have to write the same counter or a pan over the lightning layer reads
/// `filtered-out`.
#[test]
fn a_flash_outside_the_geographic_box_is_outside_view() {
    assert_eq!(
        reason_of(strikes(vec![flash(0.0, -98.0, at())], 300.0)),
        Some(BlankReason::OutsideView),
        "a flash south of the viewport was dropped by the degree-space cull \
         and the raster did not name it",
    );
}

/// **A filter that is not about where the view is gets its own name.**
///
/// Three mechanisms, one reason: a category the pane switched off, an id the
/// user hid, and a timestamp the depicted instant has not reached. The last is
/// the one that matters most — it is what a user scrubbing backwards produces
/// on purpose — and folding any of them into a geographic reason would report
/// a correct clear as a pan defect.
#[test]
fn a_non_geographic_filter_that_removes_every_item_is_filtered_out() {
    let later = at() + chrono::Duration::hours(1);
    let long_ago = at() - chrono::Duration::hours(1);
    let cases: [(&str, RasterizeOutput); 5] = [
        (
            "every alert category off",
            alerts(vec![alert("a", 35.0, -98.0, OPAQUE)], Vec::new(), &[]),
        ),
        (
            "the only alert hidden",
            alerts(
                vec![alert("a", 35.0, -98.0, OPAQUE)],
                vec![AlertCategory::Warning],
                &["a"],
            ),
        ),
        (
            "a report later than the depicted instant",
            reports(vec![report(35.0, -98.0, Some(later))]),
        ),
        (
            "a flash later than the depicted instant",
            strikes(vec![flash(35.0, -98.0, later)], 300.0),
        ),
        (
            "a flash older than the pane's window",
            strikes(vec![flash(35.0, -98.0, long_ago)], 300.0),
        ),
    ];
    for (name, out) in cases {
        assert_eq!(
            reason_of(out),
            Some(BlankReason::FilteredOut),
            "{name}: the item is over the viewport and was removed by \
             something that is not about where the view is, and the raster did \
             not say so",
        );
    }
}

/// **On the texture and no ink is its own answer, and not `unattributed`.**
///
/// Three unrelated mechanisms that all end in a draw call changing nothing: a
/// feature whose fill and stroke are both fully transparent, a ring of two
/// points that builds no path, and a coverage disc that projects below a
/// texel. The last is the one a real user reaches — it is what happens at
/// continental zoom — and it is why this arm is `on_texture` rather than a
/// third cull.
#[test]
fn items_on_the_texture_that_paint_nothing_are_drew_no_ink() {
    // 160 degrees of latitude across 8 texels: a 230 km coverage radius is
    // 0.08 of a texel there, which is the sub-texel arm.
    let world = GeoBounds {
        min_lat: -80.0,
        max_lat: 80.0,
        min_lon: -180.0,
        max_lon: 180.0,
    };
    let cases: [(&str, RasterizeOutput); 4] = [
        (
            "a fully transparent outlook",
            outlooks(vec![feature(35.0, -98.0, CLEAR, CLEAR)]),
        ),
        (
            "a fully transparent alert",
            alerts(
                vec![alert("a", 35.0, -98.0, CLEAR)],
                vec![AlertCategory::Warning],
                &[],
            ),
        ),
        (
            "a discussion ring of two points",
            discussions(vec![vec![vec![(35.0, -98.0), (35.1, -98.1)]]]),
        ),
        (
            "a sub-texel coverage disc",
            coverage(
                vec![CoverageSite {
                    lat: 35.0,
                    lon: -97.0,
                }],
                world,
                8,
                8,
            ),
        ),
    ];
    for (name, out) in cases {
        assert_eq!(
            reason_of(out),
            Some(BlankReason::DrewNoInk),
            "{name}: the item was in range and put no pixel down, which is a \
             painter that stopped painting and not a view that moved away",
        );
    }
}

/// **No item rasterizer settles `unattributed` on any fixture above.**
///
/// The figure this whole change exists to move, asserted as an absence over
/// every case the file builds. Written as its own test rather than trusted to
/// the equalities above because the equalities would still pass if a
/// rasterizer answered a *different* wrong reason, and what the browser arm
/// measured was specifically the unattributed share.
#[test]
fn nothing_an_item_rasterizer_produces_is_unattributed() {
    let later = at() + chrono::Duration::hours(1);
    let world = GeoBounds {
        min_lat: -80.0,
        max_lat: 80.0,
        min_lon: -180.0,
        max_lon: 180.0,
    };
    let cases: Vec<(&str, RasterizeOutput)> = vec![
        ("outlooks empty", outlooks(Vec::new())),
        (
            "outlooks away",
            outlooks(vec![feature(0.0, 0.0, OPAQUE, OPAQUE)]),
        ),
        (
            "outlooks clear",
            outlooks(vec![feature(35.0, -98.0, CLEAR, CLEAR)]),
        ),
        ("outlooks zero-sized", {
            rasterize_spc_outlooks(
                &OutlooksInput {
                    features: vec![feature(35.0, -98.0, OPAQUE, OPAQUE)],
                    hatch_color: CLEAR,
                    device_scale: 1.0,
                },
                &bounds(),
                0,
                H,
            )
        }),
        ("discussions empty", discussions(Vec::new())),
        ("discussions away", discussions(vec![ring(0.0, 0.0)])),
        (
            "alerts filtered",
            alerts(vec![alert("a", 35.0, -98.0, OPAQUE)], Vec::new(), &[]),
        ),
        (
            "alerts away",
            alerts(
                vec![alert("a", 0.0, 0.0, OPAQUE)],
                vec![AlertCategory::Warning],
                &[],
            ),
        ),
        (
            "coverage away",
            coverage(vec![CoverageSite { lat: 0.0, lon: 0.0 }], bounds(), W, H),
        ),
        (
            "coverage sub-texel",
            coverage(
                vec![CoverageSite {
                    lat: 35.0,
                    lon: -97.0,
                }],
                world,
                8,
                8,
            ),
        ),
        ("metar away", metar(vec![station(0.0, 0.0)])),
        ("reports away", reports(vec![report(0.0, 0.0, None)])),
        (
            "reports future",
            reports(vec![report(35.0, -98.0, Some(later))]),
        ),
        (
            "strikes away",
            strikes(vec![flash(0.0, -98.0, at())], 300.0),
        ),
        (
            "strikes future",
            strikes(vec![flash(35.0, -98.0, later)], 300.0),
        ),
    ];
    for (name, out) in cases {
        let reason = reason_of(out);
        assert!(
            reason.is_some(),
            "{name} did not settle blank at all, so this case proves nothing \
             about the reason it would have armed",
        );
        assert_ne!(
            reason,
            Some(BlankReason::Unattributed),
            "{name} settled blank with no reason armed. That is the 67.5 % \
             share the 2026-09-08 browser arm measured, arriving again",
        );
    }
}

/// **A zero-sized texture is `empty-input`, and a refused allocation is not.**
///
/// `Pixmap::new` answers `None` to both and they are not the same fault: one
/// is an input with nothing to draw onto, the other a well-formed request the
/// allocator would not serve. The zero case runs through a real rasterizer;
/// the allocation case is asked of the classifier directly, because the only
/// sizes that reach it overflow the byte count the failure arm allocates.
#[test]
fn a_zero_sized_texture_and_a_refused_allocation_are_different_reasons() {
    for (w, h) in [(0, H), (W, 0), (0, 0)] {
        assert_eq!(
            no_pixmap_reason(w, h),
            BlankReason::EmptyInput,
            "a {w}x{h} texture is nothing to draw onto",
        );
    }
    assert_eq!(
        no_pixmap_reason(W, H),
        BlankReason::AllocationFailed,
        "a pixmap of a positive size that could not be built is an allocation \
         this target refused, and calling it an empty input would file an \
         out-of-memory browser tab as a layer with no data in it",
    );
}

/// **The classifier's arms, and the order it asks them in.**
///
/// `ItemTally::reason` is the one place seven rasterizers agree on what their
/// counters mean, so the ladder is pinned here rather than inferred from the
/// seven call sites. The mixed cases are the point: a tally with items on the
/// texture answers `DrewNoInk` **whatever else it counted**, and a tally with
/// nothing on the texture prefers position over filtering.
#[test]
fn the_tally_names_the_arm_that_ran_out_of_items() {
    let tally = |filtered, off_texture, on_texture| ItemTally {
        filtered,
        off_texture,
        on_texture,
    };

    assert_eq!(
        tally(0, 0, 0).reason(0),
        BlankReason::EmptyInput,
        "an empty list is an empty input whatever the counters say",
    );
    assert_eq!(
        tally(0, 3, 0).reason(3),
        BlankReason::OutsideView,
        "every item was resolved and found off the texture",
    );
    assert_eq!(
        tally(3, 0, 0).reason(3),
        BlankReason::FilteredOut,
        "nothing was located at all and every item died at a filter",
    );
    assert_eq!(
        tally(0, 0, 3).reason(3),
        BlankReason::DrewNoInk,
        "items reached a draw call and the raster is still blank",
    );
    assert_eq!(
        tally(0, 0, 0).reason(3),
        BlankReason::DrewNoInk,
        "a non-empty list whose items reached no branch is a degenerate row, \
         which is a painter that drew nothing — never `Unattributed`, which \
         is reserved for a rasterizer that armed nothing at all",
    );
    assert_eq!(
        tally(2, 2, 1).reason(5),
        BlankReason::DrewNoInk,
        "one item in range makes `the view moved off the data` false, whatever \
         the other rows did",
    );
    assert_eq!(
        tally(2, 2, 0).reason(4),
        BlankReason::OutsideView,
        "a mixed raster with nothing in range is named by position, not by \
         time. Both count against covered ground, so this picks which of two \
         true sentences is printed and moves no figure across the subtotal",
    );
}

/// **Every reason the seven can arm is one the subtotal counts.**
///
/// None of the four variants added for them joined the proven-correct set, and
/// that is deliberate: `OutsideView` in particular looks like
/// `OutsideCoverage` and rests, on the two feature rows, on an extent the
/// source computed rather than on a projection this crate performed. Admitting
/// it would let a wrong `geo_bounds` file itself as a correct clear —
/// the failure `ExtentDeclaredEmpty`'s own doc is written against.
#[test]
fn no_reason_an_item_rasterizer_arms_is_counted_as_a_correct_clear() {
    for reason in [
        BlankReason::EmptyInput,
        BlankReason::FilteredOut,
        BlankReason::OutsideView,
        BlankReason::DrewNoInk,
        BlankReason::AllocationFailed,
    ] {
        assert!(
            reason.clears_covered_ground(),
            "{reason:?} was moved into the proven-correct set, which removes \
             it from `Totals::blanks_over_covered_ground` and shrinks the \
             figure a correctness reading is taken from",
        );
    }
}
