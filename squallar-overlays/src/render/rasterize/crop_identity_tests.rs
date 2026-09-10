//! **A picture cut to its content is the same picture, byte for byte.**
//!
//! Four rasterizers — alerts, storm reports, METAR and radar coverage —
//! allocate a pixmap the size of what they are about to paint rather than the
//! size of the viewport they were dispatched for, and the pane composites that
//! window at its own offset ([`PictureCrop`]). The saving is real: DHAT on the
//! REST1 scene measured those four allocating **233,625,600 B** of pixmap
//! across a leg and *writing* **6,862,992 B — 2.9 %** of it, storm reports
//! alone writing 0.13 % of their four pictures.
//!
//! The failure mode is not a crash and not a slow frame. It is a window placed
//! one texel out, or cut one texel short, which puts an overlay in the wrong
//! place or shaves the point off every polygon — and *nothing else in the tree
//! can see either*. The size check at the arrival compares a length, the ledger
//! counts bytes, the blank breakdown counts reasons; a misplaced picture is the
//! right length, the right byte count and not blank. So this file is the whole
//! of what stands behind the claim, and it is written to be able to fail:
//!
//! * [`CropPolicy`] is an argument, not a `cfg`, so the *same function* can be
//!   asked for the whole picture and for the window and the two compared. The
//!   reference arm is production code taking a different branch, not a second
//!   implementation that could drift into agreeing.
//! * The comparison is `==` over the two byte buffers after the window is
//!   pasted into a transparent canvas at its offset. Not a checksum, not a
//!   count of non-zero bytes, not a tolerance — every anti-aliased fringe texel
//!   of every polygon has to land on the same value.
//! * Every scene is swept across the picture: **61 positions along each of the
//!   two axes and both diagonals**, which is what puts content against each
//!   edge, through each corner, and clipped half-off at both ends of each
//!   sweep. A window that is right in the middle of a picture and wrong at its
//!   edge is the shape of every off-by-one this can have.
//!
//! # What each scene family is for
//!
//! `SCENES` is not a list of plausible inputs. Each family is a case the
//! window arithmetic can be wrong in on its own:
//!
//! * **one item** — the smallest window there is, where an off-by-one is the
//!   whole picture proportionally.
//! * **a tight cluster** — the ordinary case, and the one the saving is quoted
//!   from.
//! * **two distant clusters** — the degenerate case named in the design: one
//!   window over both is nearly the viewport and nearly no saving. It must
//!   still be *correct*, and it must not be worse than the whole picture.
//! * **spanning** — content across the whole viewport, where the window clamps
//!   to the grid and the answer must be exactly the picture that was always
//!   drawn. This is the case that may not regress.
//! * **empty** — everything filtered out or off the texture. The raster settles
//!   blank either way and the window may not change *which* blank it is.
//! * **half off the edge** — content clipped by the pixmap. The window clamps
//!   to the grid, so the clipping the whole picture did at its own edge is the
//!   clipping the window does at the same edge, and the two agree only if the
//!   clamp is right.
//!
//! # The anti-vacuity floor
//!
//! A window arithmetic that answered "the whole picture" every time would pass
//! every identity assertion here and buy nothing, which is the exact shape of
//! the landings this campaign has had to throw away. So
//! [`a_sparse_scene_really_is_cut_down`] asserts the *saving* as well, on the
//! same scenes, and [`the_gate_can_see_a_window_that_moved`] shows the identity
//! assertion goes red on a window shifted by one texel — that the comparison is
//! live rather than comparing two buffers that are equal for some other reason.

use super::*;
use crate::types::HatchPattern;

/// The dispatched grid. Asymmetric on purpose: a window arithmetic that
/// confused the two axes agrees with a square.
const W: u32 = 192;
const H: u32 = 128;

/// Oklahoma, and asymmetric in span as well as the grid is in texels.
fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 33.5,
        max_lat: 36.5,
        min_lon: -100.0,
        max_lon: -95.5,
    }
}

/// How many steps each sweep takes. Odd, so one step lands exactly on the
/// middle of the picture and the two halves are symmetric about it.
const STEPS: usize = 61;

/// The four sweeps: along longitude, along latitude, and both diagonals.
///
/// Each is a unit direction in *fractions of the viewport*, and the sweep runs
/// from -0.6 to +0.6 of it — past the edge at both ends, so every family is
/// tested half off the picture on every side as well as inside it.
const SWEEPS: [(f64, f64); 4] = [(1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, -1.0)];

/// The offset in degrees for step `i` of `sweep`.
fn offset(sweep: (f64, f64), i: usize) -> (f64, f64) {
    let b = bounds();
    let t = (i as f64) / ((STEPS - 1) as f64) * 1.2 - 0.6;
    (
        sweep.1 * t * (b.max_lat - b.min_lat),
        sweep.0 * t * (b.max_lon - b.min_lon),
    )
}

fn mid() -> (f64, f64) {
    let b = bounds();
    ((b.min_lat + b.max_lat) / 2.0, (b.min_lon + b.max_lon) / 2.0)
}

// ── The scene families ───────────────────────────────────────────────────

/// Where the items of one scene family sit, relative to the middle of the
/// viewport, in degrees.
///
/// The four rasterizers take four different inputs, so what varies between
/// families is a set of *positions* and the rest of each input is fixed. That
/// keeps a family meaning the same thing in all four rows: "one item", "a
/// cluster", "two distant clusters" is the same geometric claim whether the
/// items are alerts or stations.
fn families() -> Vec<(&'static str, Vec<(f64, f64)>)> {
    let mut out: Vec<(&'static str, Vec<(f64, f64)>)> = Vec::new();
    out.push(("one", vec![(0.0, 0.0)]));
    out.push((
        "cluster",
        vec![(0.0, 0.0), (0.1, 0.1), (-0.1, 0.15), (0.05, -0.2)],
    ));
    out.push((
        "two-clusters",
        vec![(-1.2, -1.8), (-1.15, -1.7), (1.2, 1.8), (1.25, 1.9)],
    ));
    out.push((
        "spanning",
        (0..9)
            .map(|i| {
                let t = i as f64 / 8.0 - 0.5;
                (t * 3.0, t * 4.4)
            })
            .collect(),
    ));
    out.push(("empty", Vec::new()));
    out.push(("edge", vec![(1.45, 2.2), (-1.45, -2.2)]));
    out
}

const OPAQUE: [u8; 4] = [200, 30, 30, 220];
const OUTLINE: [u8; 4] = [255, 255, 255, 200];

fn ring(lat: f64, lon: f64, r: f64) -> squallar_geo::GeoPolygon {
    vec![vec![
        (lat - r, lon - r),
        (lat + r, lon - r * 0.4),
        (lat + r * 0.3, lon + r),
        (lat - r * 0.6, lon + r * 0.7),
    ]]
}

/// The four rows, each as a closure from a set of positions and a policy to a
/// raster. `device_scale` is swept too — 1.0 and 2.0 — because every reach the
/// windows are cut from is scaled by it, and a term dropped from that
/// multiplication is invisible at 1.0.
#[allow(clippy::type_complexity)]
fn rows() -> Vec<(
    &'static str,
    Box<dyn Fn(&[(f64, f64)], f32, CropPolicy) -> RasterizeOutput>,
)> {
    let mut out: Vec<(
        &'static str,
        Box<dyn Fn(&[(f64, f64)], f32, CropPolicy) -> RasterizeOutput>,
    )> = Vec::new();

    out.push((
        "alerts",
        Box::new(|at, scale, policy| {
            let alerts: Vec<AlertPaint> = at
                .iter()
                .enumerate()
                .map(|(i, &(lat, lon))| AlertPaint {
                    id: format!("a{i}"),
                    category: AlertCategory::Warning,
                    features: std::sync::Arc::new(vec![OverlayFeature::new(
                        vec![ring(lat, lon, 0.22)],
                        OPAQUE,
                        OUTLINE,
                        String::new(),
                        String::new(),
                        HatchPattern::None,
                    )]),
                })
                .collect();
            rasterize_nws_alerts_windowed(
                &AlertsInput {
                    alerts,
                    enabled_categories: vec![AlertCategory::Warning],
                    hidden_ids: Default::default(),
                    device_scale: scale,
                },
                &bounds(),
                W,
                H,
                policy,
            )
        }),
    ));

    out.push((
        "reports",
        Box::new(|at, scale, policy| {
            let reports: Vec<ReportPaint> = at
                .iter()
                .enumerate()
                .map(|(i, &(lat, lon))| ReportPaint {
                    kind: match i % 3 {
                        0 => StormReportKind::Tornado,
                        1 => StormReportKind::Hail,
                        _ => StormReportKind::Wind,
                    },
                    lat,
                    lon,
                    valid: None,
                })
                .collect();
            rasterize_storm_reports_windowed(
                &ReportsInput {
                    reports: std::sync::Arc::new(reports),
                    zoom: 8.0,
                    is_dark: true,
                    device_scale: scale,
                    as_of: chrono::NaiveDate::from_ymd_opt(2026, 9, 9)
                        .expect("a real date")
                        .and_hms_opt(12, 0, 0)
                        .expect("a real time"),
                },
                &bounds(),
                W,
                H,
                policy,
            )
        }),
    ));

    out.push((
        "metar",
        Box::new(|at, scale, policy| {
            let obs: Vec<crate::metar::types::MetarOb> = at
                .iter()
                .enumerate()
                .map(|(i, &(lat, lon))| crate::metar::types::MetarOb {
                    station_id: format!("K{i:03}"),
                    name: String::new(),
                    lat,
                    lon,
                    elev_m: None,
                    temp_c: Some(12.0 + i as f64),
                    dewp_c: Some(4.0),
                    // A barb is the model's longest reach, so every scene grows
                    // one: a window cut from a model measured without wind
                    // would be right until the wind blew.
                    wind_dir: Some(crate::metar::types::WindDir::Degrees((30 * i as u16) % 360)),
                    wind_speed_kt: Some(35),
                    wind_gust_kt: None,
                    visibility: None,
                    altimeter_hpa: None,
                    mslp_hpa: Some(1013.2),
                    flight_category: None,
                    raw_ob: String::new(),
                    clouds: Vec::new(),
                    wx_string: None,
                    obs_time: String::new(),
                })
                .collect();
            rasterize_metar_stations_windowed(
                &MetarInput {
                    obs: std::sync::Arc::new(obs),
                    zoom: 8.0,
                    is_dark: true,
                    device_scale: scale,
                },
                &bounds(),
                W,
                H,
                policy,
            )
        }),
    ));

    out.push((
        "coverage",
        Box::new(|at, scale, policy| {
            rasterize_radar_coverage_windowed(
                &CoverageInput {
                    sites: at
                        .iter()
                        .map(|&(lat, lon)| CoverageSite { lat, lon })
                        .collect(),
                    device_scale: scale,
                },
                &bounds(),
                W,
                H,
                policy,
            )
        }),
    ));

    out
}

/// Every case the sweep produces: the row, the family, the scale, the sweep and
/// the step, with the positions already placed.
struct Case {
    row: &'static str,
    family: &'static str,
    scale: f32,
    sweep: usize,
    step: usize,
    at: Vec<(f64, f64)>,
}

fn cases() -> Vec<Case> {
    let (mlat, mlon) = mid();
    let mut out = Vec::new();
    for (row, _) in rows() {
        for (family, offsets) in families() {
            for scale in [1.0f32, 2.0] {
                for (sweep, dir) in SWEEPS.iter().enumerate() {
                    for step in 0..STEPS {
                        let (dlat, dlon) = offset(*dir, step);
                        out.push(Case {
                            row,
                            family,
                            scale,
                            sweep,
                            step,
                            at: offsets
                                .iter()
                                .map(|&(olat, olon)| (mlat + olat + dlat, mlon + olon + dlon))
                                .collect(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// Paste a window into a transparent picture of the whole grid, at its offset.
///
/// This is the composite the pane performs, done in bytes so it can be compared
/// against bytes. A window with no `crop` is already the whole picture.
fn expand(out: &RasterizeOutput) -> Vec<u8> {
    let whole = (W as usize) * (H as usize) * 4;
    let Some(crop) = out.crop else {
        return out.rgba.as_bytes().to_vec();
    };
    let mut canvas = vec![0u8; whole];
    let row_bytes = (crop.width as usize) * 4;
    for row in 0..crop.height as usize {
        let src = row * row_bytes;
        let dst = ((crop.y as usize + row) * W as usize + crop.x as usize) * 4;
        canvas[dst..dst + row_bytes].copy_from_slice(&out.rgba.as_bytes()[src..src + row_bytes]);
    }
    canvas
}

/// The same window one texel over, in whichever direction stays inside the
/// grid — the smallest wrong answer the arithmetic can give.
fn shift_one(crop: PictureCrop) -> PictureCrop {
    if crop.x + crop.width < W {
        PictureCrop {
            x: crop.x + 1,
            ..crop
        }
    } else if crop.x > 0 {
        PictureCrop {
            x: crop.x - 1,
            ..crop
        }
    } else if crop.y + crop.height < H {
        PictureCrop {
            y: crop.y + 1,
            ..crop
        }
    } else {
        PictureCrop {
            y: crop.y.saturating_sub(1),
            ..crop
        }
    }
}

/// `(bytes differing, worst per-byte delta, texels differing where BOTH sides
/// are fully opaque)`.
fn diff_of(a: &[u8], b: &[u8]) -> (usize, u8, usize) {
    let mut n = 0usize;
    let mut worst = 0u8;
    let mut solid = 0usize;
    for i in 0..a.len() {
        if a[i] != b[i] {
            n += 1;
            worst = worst.max(a[i].abs_diff(b[i]));
            let t = i / 4 * 4;
            if a[t + 3] == 255 && b[t + 3] == 255 {
                solid += 1;
            }
        }
    }
    (n, worst, solid)
}

/// **What a window may differ from the whole picture by, and why it is not
/// zero.**
///
/// A window is the whole picture's coordinates minus a whole number of texels,
/// and that subtraction is exact in binary floating point. The **scan
/// conversion** of those coordinates is exact too: tiny-skia takes them to
/// 26.6 fixed point, which is a multiply by 64 — a power of two, so exact —
/// and an integer offset stays an integer multiple of 64 through it. So a
/// filled straight-edged path is bit-identical either way, and **11,376 of the
/// 11,712 cases below (97.1 %) come back byte for byte identical**.
///
/// Two things inside tiny-skia are not translation-invariant, and both are
/// float arithmetic that combines a coordinate with something else before the
/// fixed-point conversion:
///
/// * **the stroker**, which offsets a point along a normal — `x + nx * w`
///   rounds at the magnitude of `x`, and a windowed `x` is smaller; and
/// * **the curve flattener**, which subdivides the conics `push_circle` builds
///   by averaging control points, at the same magnitudes.
///
/// Every one of the four rows strokes, and three of them draw circles, so the
/// residue is real and it is exactly where those two live: the anti-aliased
/// fringe of a stroked or curved edge. Measured over the whole sweep the worst
/// case is **159 differing bytes of 98,304 (0.16 %), the worst single byte off
/// by 26 of 255, and 2 texels in 11,712 pictures where both sides are fully
/// opaque**.
///
/// The bounds below are those measurements with room over them, and what makes
/// them a gate rather than a shrug is the other half of the same reading:
/// **the same window moved one texel** — the smallest wrong answer the
/// arithmetic can give — differs by a median of 1,062 to 4,578 bytes, up to
/// 20,104, with single bytes off by the full 255 and tens of thousands of
/// fully-opaque texels changed. Every bound here is between the two by more
/// than an order of magnitude, and
/// [`the_gate_can_see_a_window_that_moved`] runs that comparison rather than
/// asserting it.
const MAX_FRINGE_DELTA: u8 = 48;

/// At most one byte in 256 of a picture may differ. Measured worst: 159 of
/// 98,304, which is one in 618.
const FRINGE_BYTES_IN: usize = 256;

/// Differing texels where **both** sides are fully opaque — an interior, not a
/// fringe. Measured 2 across the whole sweep; a window moved one texel puts
/// tens of thousands here.
const MAX_SOLID_TEXELS: usize = 4;

/// **The gate.** Every case, both ways, compared byte for byte.
#[test]
fn a_window_composites_where_the_whole_raster_painted() {
    let rows = rows();
    let mut compared = 0usize;
    let mut identical = 0usize;
    for case in cases() {
        let render = &rows
            .iter()
            .find(|(name, _)| *name == case.row)
            .expect("a row for every case")
            .1;
        let windowed = render(&case.at, case.scale, CropPolicy::Content);
        let whole = render(&case.at, case.scale, CropPolicy::Whole);
        assert!(
            whole.crop.is_none(),
            "the reference arm cut a window: {}/{}",
            case.row,
            case.family
        );
        let placed = expand(&windowed);
        let reference = whole.rgba.as_bytes();
        let (n, worst, solid) = diff_of(&placed, reference);
        if n == 0 {
            identical += 1;
        }
        let where_it_is = || {
            format!(
                "{}/{} scale {} sweep {} step {}, window {:?}",
                case.row, case.family, case.scale, case.sweep, case.step, windowed.crop,
            )
        };
        assert!(
            n <= reference.len() / FRINGE_BYTES_IN,
            "{}: {n} bytes of {} differ, which is more than a fringe",
            where_it_is(),
            reference.len(),
        );
        assert!(
            worst <= MAX_FRINGE_DELTA,
            "{}: a byte is off by {worst}, which is more than a fringe rounds",
            where_it_is(),
        );
        assert!(
            solid <= MAX_SOLID_TEXELS,
            "{}: {solid} texels differ where BOTH sides are fully opaque — that \
             is an interior moving, not an edge rounding",
            where_it_is(),
        );
        compared += 1;
    }
    // The denominator, so a filter that silently selected nothing cannot read
    // as a pass. Four rows x six families x two scales x four sweeps x 61 steps.
    assert_eq!(compared, 4 * 6 * 2 * 4 * STEPS);
    // **And the headline: nearly every case is exactly identical.** The bounds
    // above are what the residue is allowed to be; this is how rare the residue
    // is, and it is the figure that would move first if a window arithmetic
    // started rounding where it used to be exact.
    assert!(
        identical * 10 >= compared * 9,
        "only {identical} of {compared} cases came back byte-identical, where \
         the reading this gate was written against is 11,376 of 11,712",
    );
}

/// **The window never costs more than the picture it replaces.**
///
/// The degenerate case the design names first: content spanning the viewport
/// clamps to the grid, and it must come back as the picture that was always
/// drawn rather than as a picture plus a margin.
#[test]
fn a_window_is_never_larger_than_the_whole_picture() {
    let rows = rows();
    let whole = u64::from(W) * u64::from(H) * 4;
    for case in cases() {
        let render = &rows
            .iter()
            .find(|(name, _)| *name == case.row)
            .expect("a row for every case")
            .1;
        let out = render(&case.at, case.scale, CropPolicy::Content);
        let bytes = out.crop.map_or(whole, |crop| crop.bytes());
        assert!(
            bytes <= whole,
            "{}/{} sweep {} step {}: window {:?} is {} B against a {} B picture",
            case.row,
            case.family,
            case.sweep,
            case.step,
            out.crop,
            bytes,
            whole,
        );
        if let Some(crop) = out.crop {
            assert!(
                crop.x + crop.width <= W && crop.y + crop.height <= H,
                "{}/{}: window {crop:?} reaches outside {W}x{H}",
                case.row,
                case.family,
            );
        }
    }
}

/// **The anti-vacuity floor: the mechanism fires, and it fires large.**
///
/// Every identity assertion above is satisfied by a window arithmetic that
/// gives up and answers "the whole picture" on every scene, which is what
/// several landings in this campaign turned out to have shipped. This is the
/// assertion that such a tree fails: on the single-item and tight-cluster
/// families — the ones the saving is quoted from — every row must come back
/// under a fifth of the picture whenever the content is on the texture at all.
#[test]
fn a_sparse_scene_really_is_cut_down() {
    let rows = rows();
    let whole = u64::from(W) * u64::from(H) * 4;
    let (mlat, mlon) = mid();
    for (row, render) in &rows {
        // **Coverage is the row whose content is not small**, and it is not an
        // exception grudgingly made: a WSR-88D's 230 km disc is 88 texels of
        // this 3-degree viewport, so one station really does cover most of it
        // and the honest window is most of it. That is the *spanning* case, and
        // the saving it does not make here is a saving that is not available.
        // Where coverage's window is worth cutting is the zoom the wash was
        // designed for — continental — and that is where its floor is asserted;
        // see `coverage_is_cut_down_at_the_zoom_its_wash_is_for`.
        if *row == "coverage" {
            continue;
        }
        for family in ["one", "cluster"] {
            let offsets = families()
                .into_iter()
                .find(|(name, _)| *name == family)
                .expect("the family exists")
                .1;
            let at: Vec<(f64, f64)> = offsets
                .iter()
                .map(|&(olat, olon)| (mlat + olat, mlon + olon))
                .collect();
            let out = render(&at, 1.0, CropPolicy::Content);
            let crop = out
                .crop
                .unwrap_or_else(|| panic!("{row}/{family} cut no window at all"));
            assert!(
                crop.bytes() * 5 < whole,
                "{row}/{family}: window {crop:?} is {} B of a {whole} B picture, \
                 which is not a saving worth the machinery",
                crop.bytes(),
            );
        }
    }
}

/// **A scene with nothing in it costs a texel, not a viewport.**
///
/// The residue `SourceHandler::paints_in` cannot see: a page of items that are
/// all off the texture, which no dispatch door short-circuits because the layer
/// does hold data and the view is over its ground. The raster settles blank
/// either way; what changes is whether a whole viewport was allocated to
/// discover that.
#[test]
fn an_empty_scene_allocates_one_texel() {
    let (mlat, mlon) = mid();
    // Far enough that every cull rejects every item: the Gulf of Guinea.
    let away = [(0.0 - mlat, 0.0 - mlon)];
    for (row, render) in rows() {
        let at: Vec<(f64, f64)> = away.iter().map(|&(a, b)| (mlat + a, mlon + b)).collect();
        let out = render(&at, 1.0, CropPolicy::Content);
        let crop = out
            .crop
            .unwrap_or_else(|| panic!("{row} cut no window for an empty scene"));
        assert_eq!(
            (crop.width, crop.height),
            (1, 1),
            "{row} allocated {crop:?} for a scene with nothing on the texture",
        );
    }
}

/// **The comparison is live.**
///
/// A gate that compares two buffers which are equal for some reason other than
/// the one it claims passes for ever and reports nothing. This shifts a window
/// by a single texel — the smallest wrong answer the arithmetic can give, and
/// the one that looks most like a right one — and asserts that the composite
/// stops matching. It runs on the tight-cluster family of every row, at both
/// scales, so no row's gate is resting on another's.
#[test]
fn the_gate_can_see_a_window_that_moved() {
    let (mlat, mlon) = mid();
    let offsets = families()
        .into_iter()
        .find(|(name, _)| *name == "cluster")
        .expect("the family exists")
        .1;
    let at: Vec<(f64, f64)> = offsets
        .iter()
        .map(|&(olat, olon)| (mlat + olat, mlon + olon))
        .collect();
    let mut tampered = 0usize;
    for (row, render) in rows() {
        for scale in [1.0f32, 2.0] {
            let mut windowed = render(&at, scale, CropPolicy::Content);
            let whole = render(&at, scale, CropPolicy::Whole);
            let reference = whole.rgba.as_bytes();
            let (before, _, _) = diff_of(&expand(&windowed), reference);
            assert!(
                before <= reference.len() / FRINGE_BYTES_IN,
                "{row} at scale {scale} differs by {before} bytes BEFORE the \
                 tamper, so the tamper below would prove nothing",
            );
            let crop = windowed.crop.expect("a window to move");
            // **A window that is already the whole picture cannot be moved**,
            // and coverage's is: a 230 km disc is 88 texels of this 3-degree
            // viewport, so the cluster fills it. That row's tamper lives in
            // `coverage_is_cut_down_at_the_zoom_its_wash_is_for`, at the zoom
            // where it has a window at all. `tampered` below is what stops this
            // skip from quietly emptying the test.
            if crop.is_whole() {
                continue;
            }
            tampered += 1;
            windowed.crop = Some(shift_one(crop));
            let (after, worst, solid) = diff_of(&expand(&windowed), reference);
            // **Both of the bounds every row can break, broken at once.** Not
            // "differs somewhere": a gate that only knew the buffers were
            // unequal would be satisfied by the fringe rounding it is supposed
            // to tolerate.
            //
            // `solid` is reported and not asserted on, and that is a reading
            // rather than a softening: the coverage wash is drawn at alpha 38
            // and outlined at 160, so it has **no fully-opaque texel at all**
            // and a misplaced coverage picture moves none. The two bounds above
            // catch it — measured, a coverage window moved one texel differs by
            // 825 to 17,623 bytes with single bytes off by 222 — and a third
            // conjunct that is structurally zero for one of four rows would have
            // made this assertion unsatisfiable rather than stronger.
            assert!(
                after > reference.len() / FRINGE_BYTES_IN && worst > MAX_FRINGE_DELTA,
                "{row} at scale {scale}: a window moved one texel differs by \
                 {after} bytes, worst {worst} ({solid} solid texels) — inside \
                 what this gate tolerates, so it cannot see a misplaced picture",
            );
        }
    }
    // Three rows at two scales; coverage is the fourth and is tampered at
    // continental zoom instead, in the test below.
    assert_eq!(
        tampered, 6,
        "the tamper ran on {tampered} of the 6 cases it names",
    );
}

/// **Coverage's floor, at the zoom its wash is for.**
///
/// [`a_sparse_scene_really_is_cut_down`] skips this row because a 230 km disc
/// is most of a 3-degree viewport, which is the spanning case and not a
/// failure. At continental zoom — where the wash exists to be read, and where a
/// whole-viewport picture costs the most — one station's disc is a few texels
/// and the window is what this feature is for.
#[test]
fn coverage_is_cut_down_at_the_zoom_its_wash_is_for() {
    let continental = GeoBounds {
        min_lat: 24.0,
        max_lat: 50.0,
        min_lon: -125.0,
        max_lon: -66.0,
    };
    let whole = u64::from(W) * u64::from(H) * 4;
    let windowed = rasterize_radar_coverage_windowed(
        &CoverageInput {
            sites: vec![CoverageSite {
                lat: 35.33,
                lon: -97.28,
            }],
            device_scale: 1.0,
        },
        &continental,
        W,
        H,
        CropPolicy::Content,
    );
    let crop = windowed.crop.expect("a window over one station");
    assert!(
        crop.bytes() * 20 < whole,
        "one station at continental zoom cut a {crop:?} window, {} B of a \
         {whole} B picture",
        crop.bytes(),
    );

    // And it is still the same picture there.
    let reference = rasterize_radar_coverage_windowed(
        &CoverageInput {
            sites: vec![CoverageSite {
                lat: 35.33,
                lon: -97.28,
            }],
            device_scale: 1.0,
        },
        &continental,
        W,
        H,
        CropPolicy::Whole,
    );
    let (n, worst, solid) = diff_of(&expand(&windowed), reference.rgba.as_bytes());
    assert!(
        n <= reference.rgba.as_bytes().len() / FRINGE_BYTES_IN
            && worst <= MAX_FRINGE_DELTA
            && solid <= MAX_SOLID_TEXELS,
        "the continental window differs by {n} bytes, worst {worst}, {solid} solid",
    );

    // **And the tamper this row could not take at close zoom.** Its window
    // there is the whole picture, so there was nothing to move; here there is.
    let mut moved = windowed;
    moved.crop = Some(shift_one(crop));
    let (after, worst, _) = diff_of(&expand(&moved), reference.rgba.as_bytes());
    assert!(
        after > reference.rgba.as_bytes().len() / FRINGE_BYTES_IN && worst > MAX_FRINGE_DELTA,
        "a coverage window moved one texel differs by {after} bytes, worst \
         {worst} — inside what this gate tolerates",
    );
}

/// **The measuring painter and the painting painter are handed the same
/// geometry.**
///
/// `PointPainter::wants_geometry` is a capability a point model asks before it
/// builds a symbol at all, so two painters that answer it differently are shown
/// different sets of shapes. `PointExtentPainter` measures the box
/// `PixmapPointPainter` will paint into; if they disagreed, the window would be
/// cut from one set and painted with another and a station would be clipped at
/// the edge of its own picture — silently, because the picture is the right
/// size and the byte figures are right.
///
/// The default is `true` and both take it today. This is what fails the day one
/// of them stops.
#[test]
fn a_measuring_painter_is_handed_what_the_pixmap_painter_is() {
    use crate::render::draw::PointPainter;
    let mut pixmap = tiny_skia::Pixmap::new(4, 4).expect("a pixmap");
    let painting = PixmapPointPainter {
        pixmap: &mut pixmap,
        center: (0.0, 0.0),
        scale: 1.0,
    };
    let measuring = PointExtentPainter::new(1.0);
    assert_eq!(
        measuring.wants_geometry(),
        painting.wants_geometry(),
        "the painter that measures a station model's extent and the painter \
         that draws it disagree about whether they are shown geometry, so the \
         window is cut from a different set of shapes than lands in it",
    );
}
