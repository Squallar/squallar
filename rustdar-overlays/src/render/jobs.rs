//! The seven overlay codec rows — the overlay half of the job boundary.
//!
//! **The refusal contract**, shared by every `decode` here: `None` for a
//! flag or enum byte outside this build's values, a string that is not
//! UTF-8, or a buffer shorter than its own counts claim.
//!
//! **No clock.** GLM's `now` is captured at dispatch and travels on the
//! wire; nothing in this module may read a clock of its own — a worker that
//! did would render a picture the direct call would not.

use std::collections::{HashMap, HashSet};

use rustdar_source::job::{EncodeCtx, JobCodec, JobCost, JobGeometry, JobOutCodec, JobSpec};
use rustdar_source::wire::Reader;

use crate::render::rasterize::{
    AlertsInput, AlphaMode, DiscussionsInput, GlmStrikesInput, GriddedInput, HitCells,
    OutlooksInput, RasterizeOutput, ReportsInput, SitesInput, rasterize_glm_strikes,
    rasterize_gridded, rasterize_nws_alerts, rasterize_radar_sites, rasterize_spc_discussions,
    rasterize_spc_outlooks, rasterize_storm_reports,
};

/// The seven overlay rows, in dispatch order: **sites, alerts, outlooks,
/// discussions, reports, glm, model**. The order is load-bearing.
///
/// **Two `#[cfg]`'d definitions rather than one list with a `#[cfg]`'d
/// element**, because `#[cfg]` on an array element is not stable. The fake row
/// is APPENDED, never inserted: `rustdar_worker::job_registry::job_codecs`
/// numbers rows by position across the composed chain, so a row inserted
/// anywhere else would renumber the shipped wire codes.
#[cfg(not(feature = "fake-source"))]
pub static JOB_CODECS: &[JobCodec] = &[
    JobCodec::of::<SitesJob>(),
    JobCodec::of::<AlertsJob>(),
    JobCodec::of::<OutlooksJob>(),
    JobCodec::of::<DiscussionsJob>(),
    JobCodec::of::<ReportsJob>(),
    JobCodec::of::<GlmJob>(),
    JobCodec::of::<GriddedJob>(),
];

/// The same seven, plus the fake source's own row. See the note above for why
/// this is a second definition and why the extra row goes last.
#[cfg(feature = "fake-source")]
pub static JOB_CODECS: &[JobCodec] = &[
    JobCodec::of::<SitesJob>(),
    JobCodec::of::<AlertsJob>(),
    JobCodec::of::<OutlooksJob>(),
    JobCodec::of::<DiscussionsJob>(),
    JobCodec::of::<ReportsJob>(),
    JobCodec::of::<GlmJob>(),
    JobCodec::of::<GriddedJob>(),
    JobCodec::of::<FakeJob>(),
];

/// The fake row's label, named once so the handler and the registry cannot
/// disagree about it.
#[cfg(feature = "fake-source")]
pub const FAKE_LABEL: &str = "overlay/fake";

/// The fake source's row.
///
/// Deliberately **unpinned in the framing digest**: no row of
/// `rustdar_worker::wire_identity::WIRE_FRAMING_ROWS` names it, so the local
/// page/worker build token is byte-identical whether this feature is on or off.
/// A token that moved with a test-only feature would make a feature-enabled
/// page refuse a feature-enabled worker for no reason.
#[cfg(feature = "fake-source")]
pub struct FakeJob;

#[cfg(feature = "fake-source")]
impl JobSpec for FakeJob {
    type In = crate::render::handlers::fake::FakeInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = FAKE_LABEL;
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &Self::In, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.push(match input.tint {
            crate::render::handlers::fake::FakeTint::Warm => 0,
            crate::render::handlers::fake::FakeTint::Cool => 1,
        });
        out.extend_from_slice(&input.level.to_le_bytes());
        out.extend_from_slice(&input.device_scale.to_le_bytes());
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(Self::In, JobGeometry)> {
        let tint = match r.u8()? {
            0 => crate::render::handlers::fake::FakeTint::Warm,
            1 => crate::render::handlers::fake::FakeTint::Cool,
            // The refusal contract: an enum byte outside this build's values.
            _ => return None,
        };
        let level = r.f32()?;
        let device_scale = r.f32()?;
        Some((
            crate::render::handlers::fake::FakeInput {
                tint,
                level,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &Self::In, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(crate::render::handlers::fake::rasterize_fake(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

#[cfg(feature = "fake-source")]
impl JobOutCodec for FakeJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

/// The radar-site markers row.
pub struct SitesJob;

impl JobSpec for SitesJob {
    type In = SitesInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/sites";
    const COST: JobCost = JobCost::Raster;

    fn encode(sites: &SitesInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.extend_from_slice(&sites.zoom.to_le_bytes());
        out.push(u8::from(sites.is_dark));
        out.extend_from_slice(&sites.device_scale.to_le_bytes());
        out.extend_from_slice(&(sites.sites.len() as u32).to_le_bytes());
        for site in &sites.sites {
            out.extend_from_slice(&site.lat.to_le_bytes());
            out.extend_from_slice(&site.lon.to_le_bytes());
            out.push(u8::from(site.is_current));
            out.push(u8::from(site.is_loading));
            encode_str(out, &site.name);
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(SitesInput, JobGeometry)> {
        let zoom = r.f64()?;
        let is_dark = flag(r.u8()?)?;
        let device_scale = r.f32()?;
        let count = r.u32()? as usize;
        let mut sites = Vec::new();
        for _ in 0..count {
            let lat = r.f64()?;
            let lon = r.f64()?;
            let is_current = flag(r.u8()?)?;
            let is_loading = flag(r.u8()?)?;
            let name = decode_str(r)?;
            sites.push(crate::render::rasterize::RadarSiteInfo {
                name,
                lat,
                lon,
                is_current,
                is_loading,
            });
        }
        Some((
            crate::render::rasterize::SitesInput {
                sites,
                zoom,
                is_dark,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &SitesInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_radar_sites(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for SitesJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

/// The NWS-alerts row.
pub struct AlertsJob;

impl JobSpec for AlertsJob {
    type In = AlertsInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/alerts";
    const COST: JobCost = JobCost::Raster;

    fn encode(alerts: &AlertsInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.extend_from_slice(&alerts.device_scale.to_le_bytes());
        out.extend_from_slice(&(alerts.enabled_categories.len() as u32).to_le_bytes());
        for category in &alerts.enabled_categories {
            out.push(AlertCategoryWire(*category).wire_code());
        }
        // Sorted, because a `HashSet`'s iteration order is seeded per
        // process: the *set* round-trips either way, but the bytes have
        // to be a function of the value for the framing digest to pin
        // them and for two encodes of one input to agree.
        let mut hidden: Vec<&String> = alerts.hidden_ids.iter().collect();
        hidden.sort();
        out.extend_from_slice(&(hidden.len() as u32).to_le_bytes());
        for id in hidden {
            encode_str(out, id);
        }
        out.extend_from_slice(&(alerts.alerts.len() as u32).to_le_bytes());
        for alert in &alerts.alerts {
            encode_str(out, &alert.id);
            out.push(AlertCategoryWire(alert.category).wire_code());
            out.extend_from_slice(&(alert.features.len() as u32).to_le_bytes());
            for feature in alert.features.iter() {
                encode_feature(out, feature);
            }
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(AlertsInput, JobGeometry)> {
        let device_scale = r.f32()?;
        let category_count = r.u32()? as usize;
        let mut enabled_categories = Vec::new();
        for _ in 0..category_count {
            enabled_categories.push(AlertCategoryWire::from_wire_code(r.u8()?)?.0);
        }
        let hidden_count = r.u32()? as usize;
        let mut hidden_ids = HashSet::new();
        for _ in 0..hidden_count {
            hidden_ids.insert(decode_str(r)?);
        }
        let alert_count = r.u32()? as usize;
        let mut alerts = Vec::new();
        for _ in 0..alert_count {
            let id = decode_str(r)?;
            let category = AlertCategoryWire::from_wire_code(r.u8()?)?.0;
            let feature_count = r.u32()? as usize;
            let mut features = Vec::new();
            for _ in 0..feature_count {
                features.push(decode_feature(r)?);
            }
            alerts.push(crate::render::rasterize::AlertPaint {
                id,
                category,
                features: std::sync::Arc::new(features),
            });
        }
        Some((
            crate::render::rasterize::AlertsInput {
                alerts,
                enabled_categories,
                hidden_ids,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &AlertsInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_nws_alerts(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for AlertsJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

/// The SPC-outlooks row.
pub struct OutlooksJob;

impl JobSpec for OutlooksJob {
    type In = OutlooksInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/outlooks";
    const COST: JobCost = JobCost::Raster;

    fn encode(outlooks: &OutlooksInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.extend_from_slice(&outlooks.device_scale.to_le_bytes());
        out.extend_from_slice(&outlooks.hatch_color);
        out.extend_from_slice(&(outlooks.features.len() as u32).to_le_bytes());
        for feature in &outlooks.features {
            encode_feature(out, feature);
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(OutlooksInput, JobGeometry)> {
        let device_scale = r.f32()?;
        let hatch_color: [u8; 4] = r.take(4)?.try_into().ok()?;
        let feature_count = r.u32()? as usize;
        let mut features = Vec::new();
        for _ in 0..feature_count {
            features.push(decode_feature(r)?);
        }
        Some((
            crate::render::rasterize::OutlooksInput {
                features,
                hatch_color,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &OutlooksInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_spc_outlooks(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for OutlooksJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

/// The SPC mesoscale-discussions row.
pub struct DiscussionsJob;

impl JobSpec for DiscussionsJob {
    type In = DiscussionsInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/discussions";
    const COST: JobCost = JobCost::Raster;

    fn encode(discussions: &DiscussionsInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.extend_from_slice(&discussions.device_scale.to_le_bytes());
        out.extend_from_slice(&(discussions.discussions.len() as u32).to_le_bytes());
        for md in &discussions.discussions {
            out.push(MdTypeWire(md.md_type).wire_code());
            encode_polygon(out, &md.polygon);
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(DiscussionsInput, JobGeometry)> {
        let device_scale = r.f32()?;
        let md_count = r.u32()? as usize;
        let mut discussions = Vec::new();
        for _ in 0..md_count {
            let md_type = MdTypeWire::from_wire_code(r.u8()?)?.0;
            let polygon = decode_polygon(r)?;
            discussions.push(crate::render::rasterize::DiscussionPaint { md_type, polygon });
        }
        Some((
            crate::render::rasterize::DiscussionsInput {
                discussions,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &DiscussionsInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_spc_discussions(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for DiscussionsJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

/// The storm-reports row — the first of the two hit-map kinds.
pub struct ReportsJob;

impl JobSpec for ReportsJob {
    type In = ReportsInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/reports";
    const COST: JobCost = JobCost::Raster;

    fn encode(reports: &ReportsInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        // One of the two hit-map kinds. **Row order is load-bearing**: a
        // row's position is its hit-map id, so a reorder would hand hovers to
        // the wrong items.
        out.extend_from_slice(&reports.zoom.to_le_bytes());
        out.push(u8::from(reports.is_dark));
        out.extend_from_slice(&reports.device_scale.to_le_bytes());
        out.extend_from_slice(&(reports.reports.len() as u32).to_le_bytes());
        for report in &reports.reports {
            out.push(StormReportKindWire(report.kind).wire_code());
            out.extend_from_slice(&report.lat.to_le_bytes());
            out.extend_from_slice(&report.lon.to_le_bytes());
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(ReportsInput, JobGeometry)> {
        let zoom = r.f64()?;
        let is_dark = flag(r.u8()?)?;
        let device_scale = r.f32()?;
        let count = r.u32()? as usize;
        let mut reports = Vec::new();
        for _ in 0..count {
            reports.push(crate::render::rasterize::ReportPaint {
                kind: StormReportKindWire::from_wire_code(r.u8()?)?.0,
                lat: r.f64()?,
                lon: r.f64()?,
            });
        }
        Some((
            crate::render::rasterize::ReportsInput {
                reports,
                zoom,
                is_dark,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &ReportsInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_storm_reports(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for ReportsJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

/// The GLM-lightning row — the second hit-map kind. Its `now` is the page's
/// clock at dispatch, on the wire ([`GlmStrikesInput::now`]); `run` receives
/// no clock and must never read one.
pub struct GlmJob;

impl JobSpec for GlmJob {
    type In = GlmStrikesInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/glm";
    const COST: JobCost = JobCost::Raster;

    fn encode(glm: &GlmStrikesInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        // One of the two hit-map kinds. **Row order is load-bearing**: a
        // row's position is its hit-map id, so a reorder would hand hovers to
        // the wrong items.
        out.extend_from_slice(&glm.zoom.to_le_bytes());
        out.push(u8::from(glm.is_dark));
        out.extend_from_slice(&glm.device_scale.to_le_bytes());
        out.extend_from_slice(&glm.time_window_secs.to_le_bytes());
        // The page's clock, on the wire. The worker ages every flash
        // against this and never against a clock of its own — the parity
        // gates over an age-straddling fixture are what enforce it.
        encode_datetime(out, &glm.now);
        out.extend_from_slice(&(glm.flashes.len() as u32).to_le_bytes());
        for flash in &glm.flashes {
            out.extend_from_slice(&flash.lat.to_le_bytes());
            out.extend_from_slice(&flash.lon.to_le_bytes());
            encode_datetime(out, &flash.time);
            match flash.energy {
                None => out.push(0),
                Some(energy) => {
                    out.push(1);
                    out.extend_from_slice(&energy.to_le_bytes());
                }
            }
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(GlmStrikesInput, JobGeometry)> {
        let zoom = r.f64()?;
        let is_dark = flag(r.u8()?)?;
        let device_scale = r.f32()?;
        let time_window_secs = r.f64()?;
        let now = decode_datetime(r)?;
        let count = r.u32()? as usize;
        let mut flashes = Vec::new();
        for _ in 0..count {
            flashes.push(crate::render::rasterize::FlashPaint {
                lat: r.f64()?,
                lon: r.f64()?,
                time: decode_datetime(r)?,
                energy: match r.u8()? {
                    0 => None,
                    1 => Some(r.f32()?),
                    _ => return None,
                },
            });
        }
        Some((
            crate::render::rasterize::GlmStrikesInput {
                flashes,
                zoom,
                is_dark,
                time_window_secs,
                now,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &GlmStrikesInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_glm_strikes(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for GlmJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

/// The HRRR model-grid row — the one row that reads its [`EncodeCtx`]: the
/// grid is **cut to its projection window at encode time**, the one moment
/// that knows what ground the texture covers, so what travels is the
/// window's values rather than 7.62 MB of grid per gesture-settle re-render.
pub struct GriddedJob;

impl JobSpec for GriddedJob {
    type In = GriddedInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/model";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &GriddedInput, ctx: &EncodeCtx, out: &mut Vec<u8>) {
        // The window cut. Scalars first as everywhere: the field's identity
        // (its `FieldId`, which for a model parameter is byte-identical to the
        // `as_str` code the persisted pane config already round-trips), the
        // grid shape, the coordinates, the window, and then the window's values
        // as the one bulk block.
        encode_str(out, input.field().as_str());
        let (ni, nj) = input.shape();
        out.extend_from_slice(&(ni as u32).to_le_bytes());
        out.extend_from_slice(&(nj as u32).to_le_bytes());
        encode_grid_coords(out, input.coords());
        let win = input.window_for(
            &ctx.geometry.bounds,
            ctx.geometry.width,
            ctx.geometry.height,
        );
        for edge in [win.i0, win.i1, win.j0, win.j1] {
            out.extend_from_slice(&(edge as u32).to_le_bytes());
        }
        // No count: the length is the window's area, already on the wire
        // as the four edges, and a second statement of it could lie.
        input.for_each_window_row(&win, |row| encode_f32s(out, row));
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(GriddedInput, JobGeometry)> {
        use crate::render::rasterize::{GridWindow, IndexWindow};
        // The field code is believed only if this build **registers** it —
        // `field_paint` answering is exactly the condition under which
        // `rasterize_gridded` can paint it. A code this build does not know is
        // a newer build's field, and defaulting it would rasterize one field's
        // values through another's colours with nothing to say so.
        let code = decode_str(r)?;
        let field = crate::render::gridded::paint_for_code(&code)?.id.clone();
        let ni = r.u32()? as usize;
        let nj = r.u32()? as usize;
        let coords = decode_grid_coords(r)?;
        let win = IndexWindow {
            i0: r.u32()? as usize,
            i1: r.u32()? as usize,
            j0: r.u32()? as usize,
            j1: r.u32()? as usize,
        };
        // A window past the grid it indexes, or inside-out, is a layout
        // this build never writes: refused, not clamped, so a raster of
        // it cannot silently draw a different region than was asked. An
        // empty window (a viewport the grid never reaches) is legitimate
        // and its area — and so its values block — is zero.
        if win.i0 > win.i1 || win.j0 > win.j1 || win.i1 > ni || win.j1 > nj {
            return None;
        }
        // The values length is the window's own area — no second count
        // on the wire to disagree with it, and `take` inside refuses a
        // buffer shorter than the area claims before anything allocates.
        let values = decode_f32s(r, win.area())?;
        Some((
            GriddedInput::Window(GridWindow {
                field,
                ni,
                nj,
                coords,
                win,
                values,
            }),
            geo,
        ))
    }

    fn run(input: &GriddedInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_gridded(input, &geo.bounds, geo.width, geo.height))
    }
}

impl JobOutCodec for GriddedJob {
    fn encode_out(v: RasterizeOutput, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
        encode_raster_reply(v, head);
    }

    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
        decode_raster_reply(head, tails)
    }
}

// ── The shared reply pair ────────────────────────────────────────────────

fn encode_raster_reply(v: RasterizeOutput, head: &mut Vec<u8>) {
    encode_overlay_out(&v.rgba, v.hit_cells.as_ref(), head);
}

/// The reply adapter every row's [`JobOutCodec::decode_out`] shares.
///
/// Refuses a non-empty `tails` first: this adapter writes none, and on a
/// same-build wire a tail count the encoder did not write is a corrupt or
/// foreign message (the `JobOutCodec` convention, WO-M7d).
fn decode_raster_reply(head: &[u8], tails: Vec<Vec<u8>>) -> Option<RasterizeOutput> {
    if !tails.is_empty() {
        return None;
    }
    let (rgba, hit_cells) = decode_overlay_out(head)?;
    Some(RasterizeOutput {
        rgba,
        hit_cells,
        alpha: AlphaMode::Premultiplied,
    })
}

/// The overlay reply's bytes: a hit-cells tag, the framed cells when the tag
/// says so, and the raw RGBA as the rest.
///
/// The cells are written **sorted by cell index** — a `HashMap`'s iteration
/// order is seeded per process, and these bytes have to be a function of the
/// value for two encodes of one reply to agree. The RGBA takes the rest.
pub fn encode_overlay_out(rgba: &[u8], hit_cells: Option<&HitCells>, out: &mut Vec<u8>) {
    out.reserve(rgba.len() + 64);
    match hit_cells {
        None => out.push(0),
        Some(cells) => {
            out.push(1);
            out.extend_from_slice(&cells.width.to_le_bytes());
            out.extend_from_slice(&cells.height.to_le_bytes());
            let mut occupied: Vec<(&u32, &Vec<u32>)> = cells.cells.iter().collect();
            occupied.sort_by_key(|(idx, _)| **idx);
            out.extend_from_slice(&(occupied.len() as u32).to_le_bytes());
            for (idx, ids) in occupied {
                out.extend_from_slice(&idx.to_le_bytes());
                out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
                for id in ids {
                    out.extend_from_slice(&id.to_le_bytes());
                }
            }
        }
    }
    out.extend_from_slice(rgba);
}

/// The inverse of [`encode_overlay_out`]. `None` for a tag outside `{0, 1}`,
/// a cell index at or past the grid the stated dimensions span, indices out
/// of ascending order or repeated (the canonical form the encoder writes and
/// the only one accepted, so one value has one byte string), an empty id
/// list (the rasterizer never records one), or a buffer shorter than its own
/// counts claim. The RGBA tail is handed back **unjudged**: only the
/// dispatch knows the dimensions it must match.
pub fn decode_overlay_out(bytes: &[u8]) -> Option<(Vec<u8>, Option<HitCells>)> {
    let mut r = Reader::new(bytes);
    let hit_cells = match r.u8()? {
        0 => None,
        1 => {
            let width = r.u32()?;
            let height = r.u32()?;
            let grid = u64::from(width) * u64::from(height);
            let occupied = r.u32()? as usize;
            let mut cells = HashMap::new();
            let mut previous: Option<u32> = None;
            for _ in 0..occupied {
                let idx = r.u32()?;
                if u64::from(idx) >= grid || previous.is_some_and(|p| p >= idx) {
                    return None;
                }
                previous = Some(idx);
                let id_count = r.u32()? as usize;
                if id_count == 0 {
                    return None;
                }
                let mut ids = Vec::new();
                for _ in 0..id_count {
                    ids.push(r.u32()?);
                }
                cells.insert(idx, ids);
            }
            Some(crate::render::rasterize::HitCells {
                width,
                height,
                cells,
            })
        }
        _ => return None,
    };
    Some((r.rest().to_vec(), hit_cells))
}

// ── The field codecs the rows share ──────────────────────────────────────

/// A byte that must be a `bool`: `None` for anything but `{0, 1}`, refused rather than coerced.
fn flag(byte: u8) -> Option<bool> {
    match byte {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// A row of `f32`s, little-endian, straight off the grid's own storage.
fn encode_f32s(out: &mut Vec<u8>, values: &[f32]) {
    out.reserve(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// The inverse of [`encode_f32s`] over exactly `count` values: `None` on a
/// short buffer, checked by [`Reader::take`] **before** anything is sized
/// from `count` — so a count claiming four billion points fails on the first
/// short read rather than reserving for it.
fn decode_f32s(r: &mut Reader, count: usize) -> Option<Vec<f32>> {
    let bytes = r.take(count.checked_mul(4)?)?;
    Some(
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().expect("chunks of four")))
            .collect(),
    )
}

/// Grid-coordinate tag: computed Lambert constants. The stored fields of
/// [`crate::hrrr::lambert::LambertGrid`] travel **as stored** — derived
/// constants and the measured wrap flag included — so the far side restores
/// bits and never consults libm; see `LambertGridParts`.
const GRID_COORDS_LAMBERT: u8 = 1;
/// Grid-coordinate tag: materialised per-point arrays; no proper-subset window is possible.
const GRID_COORDS_EXPLICIT: u8 = 2;
/// Grid-coordinate tag: a regular lat/lon grid's seven scalars. **Appended, not
/// inserted** — the two tags above keep their numbers, so a payload written
/// before this arm existed decodes unchanged.
const GRID_COORDS_REGULAR: u8 = 3;

fn encode_grid_coords(out: &mut Vec<u8>, coords: &crate::hrrr::GridCoords) {
    use crate::hrrr::GridCoords;
    match coords {
        GridCoords::Lambert(grid) => {
            out.push(GRID_COORDS_LAMBERT);
            let parts = grid.to_parts();
            for v in [
                parts.a,
                parts.e,
                parts.n,
                parts.big_f,
                parts.rho0,
                parts.lon0,
                parts.x0,
                parts.y0,
                parts.dx,
                parts.dy,
            ] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(parts.ni as u32).to_le_bytes());
            out.extend_from_slice(&(parts.nj as u32).to_le_bytes());
            out.push(u8::from(parts.i_consecutive));
            out.push(u8::from(parts.alternating));
            out.push(u8::from(parts.wraps_longitude));
        }
        GridCoords::Regular {
            lat0,
            lon0,
            dlat,
            dlon,
            ni,
            nj,
            scan_mode,
        } => {
            out.push(GRID_COORDS_REGULAR);
            for v in [lat0, lon0, dlat, dlon] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(*ni as u32).to_le_bytes());
            out.extend_from_slice(&(*nj as u32).to_le_bytes());
            out.push(*scan_mode);
        }
        GridCoords::Explicit { lats, lons } => {
            out.push(GRID_COORDS_EXPLICIT);
            // Two counts, not one: the two arrays are allowed to disagree in
            // length (`GridCoords::len` takes the min), and an encoder that
            // wrote one count would silently reshape such a grid.
            out.extend_from_slice(&(lats.len() as u32).to_le_bytes());
            for v in lats {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(lons.len() as u32).to_le_bytes());
            for v in lons {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// The inverse of [`encode_grid_coords`]. `None` for a tag this build does
/// not have, constants `LambertGrid::from_parts` refuses (non-finite, a
/// degenerate cone), a regular grid with an empty shape or a non-finite or
/// zero step, a flag byte outside `{0, 1}`, or a buffer shorter than its own
/// counts claim — each checked by `take` before any allocation is sized from
/// a count.
fn decode_grid_coords(r: &mut Reader) -> Option<crate::hrrr::GridCoords> {
    use crate::hrrr::GridCoords;
    use crate::hrrr::lambert::{LambertGrid, LambertGridParts};
    match r.u8()? {
        GRID_COORDS_LAMBERT => {
            let mut constants = [0.0f64; 10];
            for v in &mut constants {
                *v = r.f64()?;
            }
            let [a, e, n, big_f, rho0, lon0, x0, y0, dx, dy] = constants;
            let parts = LambertGridParts {
                a,
                e,
                n,
                big_f,
                rho0,
                lon0,
                x0,
                y0,
                dx,
                dy,
                ni: r.u32()? as usize,
                nj: r.u32()? as usize,
                i_consecutive: flag(r.u8()?)?,
                alternating: flag(r.u8()?)?,
                wraps_longitude: flag(r.u8()?)?,
            };
            Some(GridCoords::Lambert(LambertGrid::from_parts(parts)?))
        }
        GRID_COORDS_REGULAR => {
            let mut scalars = [0.0f64; 4];
            for v in &mut scalars {
                *v = r.f64()?;
            }
            let [lat0, lon0, dlat, dlon] = scalars;
            let (ni, nj) = (r.u32()? as usize, r.u32()? as usize);
            let scan_mode = r.u8()?;
            // Refused, not clamped — the same posture the window edges above
            // take. An empty shape or a step that is not a finite non-zero
            // number is a layout this build never writes, and every method on
            // the arm divides by the steps: a clamped zero would silently place
            // every point of the grid at its origin.
            if ni == 0
                || nj == 0
                || !lat0.is_finite()
                || !lon0.is_finite()
                || !dlat.is_finite()
                || !dlon.is_finite()
                || dlat == 0.0
                || dlon == 0.0
            {
                return None;
            }
            Some(GridCoords::Regular {
                lat0,
                lon0,
                dlat,
                dlon,
                ni,
                nj,
                scan_mode,
            })
        }
        GRID_COORDS_EXPLICIT => {
            let lat_count = r.u32()? as usize;
            let lats = decode_f64s(r, lat_count)?;
            let lon_count = r.u32()? as usize;
            let lons = decode_f64s(r, lon_count)?;
            Some(GridCoords::Explicit { lats, lons })
        }
        _ => None,
    }
}

/// [`decode_f32s`]'s shape at `f64` width, for the explicit coordinate arrays.
fn decode_f64s(r: &mut Reader, count: usize) -> Option<Vec<f64>> {
    let bytes = r.take(count.checked_mul(8)?)?;
    Some(
        bytes
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes(b.try_into().expect("chunks of eight")))
            .collect(),
    )
}

/// A UTC timestamp as twelve bytes: the `i64` Unix seconds and the `u32`
/// subsecond nanoseconds. Two fields rather than one `i64` of nanoseconds
/// because the pair is total — every `NaiveDateTime` chrono can represent
/// encodes, where nanoseconds-since-epoch overflows in 2262 — and exact.
fn encode_datetime(out: &mut Vec<u8>, t: &chrono::NaiveDateTime) {
    let utc = t.and_utc();
    out.extend_from_slice(&utc.timestamp().to_le_bytes());
    out.extend_from_slice(&utc.timestamp_subsec_nanos().to_le_bytes());
}

/// The inverse of [`encode_datetime`]: `None` for a short buffer or a
/// (seconds, nanos) pair outside chrono's representable range — which is a
/// payload from a build whose layout this is not, refused rather than
/// clamped to some time nobody stated.
fn decode_datetime(r: &mut Reader) -> Option<chrono::NaiveDateTime> {
    let secs = i64::from_le_bytes(r.take(8)?.try_into().ok()?);
    let nanos = r.u32()?;
    Some(chrono::DateTime::from_timestamp(secs, nanos)?.naive_utc())
}

/// A `u16` length prefix and then the bytes, truncated to what the prefix can
/// carry — the sites-name convention, spelled once for every string on this
/// wire. A cut that split a multi-byte character is refused by [`decode_str`]'s UTF-8 check.
fn encode_str(out: &mut Vec<u8>, s: &str) {
    let bytes = &s.as_bytes()[..s.len().min(usize::from(u16::MAX))];
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// The inverse of [`encode_str`]: `None` for a short buffer or bytes that are
/// not UTF-8.
fn decode_str(r: &mut Reader) -> Option<String> {
    let len = usize::from(r.u16()?);
    Some(std::str::from_utf8(r.take(len)?).ok()?.to_owned())
}

/// One overlay feature: the two labels, the two colours, the hatch, the
/// optional geo-AABB, and last the multi-polygon — every ring of every
/// polygon, holes included, since a hole dropped here is a hole filled on
/// the far side and the parity tests are what would catch it.
fn encode_feature(out: &mut Vec<u8>, feature: &crate::types::OverlayFeature) {
    encode_str(out, &feature.label);
    encode_str(out, &feature.label2);
    out.extend_from_slice(&feature.fill_rgba);
    out.extend_from_slice(&feature.stroke_rgba);
    out.push(HatchWire(feature.hatch).wire_code());
    match &feature.geo_bounds {
        None => out.push(0),
        Some(b) => {
            out.push(1);
            out.extend_from_slice(&b.min_lat.to_le_bytes());
            out.extend_from_slice(&b.max_lat.to_le_bytes());
            out.extend_from_slice(&b.min_lon.to_le_bytes());
            out.extend_from_slice(&b.max_lon.to_le_bytes());
        }
    }
    out.extend_from_slice(&(feature.polygons.len() as u32).to_le_bytes());
    for polygon in &feature.polygons {
        encode_polygon(out, polygon);
    }
}

/// The inverse of [`encode_feature`], with every refusal the per-kind
/// `decode` impls promise (see the module doc): `None` on a hatch byte or an
/// option tag this build does not have, a label that is not UTF-8, or counts
/// the buffer cannot honour.
fn decode_feature(r: &mut Reader) -> Option<crate::types::OverlayFeature> {
    let label = decode_str(r)?;
    let label2 = decode_str(r)?;
    let fill_rgba: [u8; 4] = r.take(4)?.try_into().ok()?;
    let stroke_rgba: [u8; 4] = r.take(4)?.try_into().ok()?;
    let hatch = HatchWire::from_wire_code(r.u8()?)?.0;
    let geo_bounds = match r.u8()? {
        0 => None,
        1 => Some(rustdar_geo::GeoBounds {
            min_lat: r.f64()?,
            max_lat: r.f64()?,
            min_lon: r.f64()?,
            max_lon: r.f64()?,
        }),
        _ => return None,
    };
    let polygon_count = r.u32()? as usize;
    let mut polygons = Vec::new();
    for _ in 0..polygon_count {
        polygons.push(decode_polygon(r)?);
    }
    Some(crate::types::OverlayFeature {
        polygons,
        fill_rgba,
        stroke_rgba,
        label,
        label2,
        hatch,
        geo_bounds,
    })
}

/// One polygon: a ring count, then each ring's point count and its
/// `(lat, lon)` pairs in that order — the crate's own `(f64, f64)`
/// convention, stated here and in [`decode_polygon`] and nowhere else. The
/// first ring is the exterior and the rest are holes, an ordering the codec
/// preserves by never reordering anything.
fn encode_polygon(out: &mut Vec<u8>, polygon: &rustdar_geo::GeoPolygon) {
    out.extend_from_slice(&(polygon.len() as u32).to_le_bytes());
    for ring in polygon {
        out.extend_from_slice(&(ring.len() as u32).to_le_bytes());
        for &(lat, lon) in ring {
            out.extend_from_slice(&lat.to_le_bytes());
            out.extend_from_slice(&lon.to_le_bytes());
        }
    }
}

/// The inverse of [`encode_polygon`].
fn decode_polygon(r: &mut Reader) -> Option<rustdar_geo::GeoPolygon> {
    let ring_count = r.u32()? as usize;
    let mut polygon = Vec::new();
    for _ in 0..ring_count {
        let point_count = r.u32()? as usize;
        let mut ring = Vec::new();
        for _ in 0..point_count {
            ring.push((r.f64()?, r.f64()?));
        }
        polygon.push(ring);
    }
    Some(polygon)
}

// ── The wire enums ───────────────────────────────────────────────────────

/// An [`AlertCategory`](crate::nws::alert::AlertCategory) as a number: both directions are exhaustive
/// over the same arms, so a variant added upstream fails this build rather
/// than silently encoding as something else. The numbering is the enum's own
/// declaration order, most severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AlertCategoryWire(crate::nws::alert::AlertCategory);

impl AlertCategoryWire {
    fn wire_code(self) -> u8 {
        use crate::nws::alert::AlertCategory as C;
        match self.0 {
            C::Warning => 0,
            C::Watch => 1,
            C::Advisory => 2,
            C::Other => 3,
        }
    }

    fn from_wire_code(code: u8) -> Option<Self> {
        use crate::nws::alert::AlertCategory as C;
        let category = match code {
            0 => C::Warning,
            1 => C::Watch,
            2 => C::Advisory,
            3 => C::Other,
            _ => return None,
        };
        Some(Self(category))
    }
}

/// A [`StormReportKind`](crate::spc::reports::StormReportKind) as a number.
/// See [`AlertCategoryWire`]. The numbering is the enum's own declaration
/// order — a kind byte misread as another decodes cleanly and paints a
/// tornado as hail, which is what the per-kind parity fixture with all three
/// kinds exists to catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StormReportKindWire(crate::spc::reports::StormReportKind);

impl StormReportKindWire {
    fn wire_code(self) -> u8 {
        use crate::spc::reports::StormReportKind as K;
        match self.0 {
            K::Tornado => 0,
            K::Hail => 1,
            K::Wind => 2,
        }
    }

    fn from_wire_code(code: u8) -> Option<Self> {
        use crate::spc::reports::StormReportKind as K;
        let kind = match code {
            0 => K::Tornado,
            1 => K::Hail,
            2 => K::Wind,
            _ => return None,
        };
        Some(Self(kind))
    }
}

/// An [`MdType`](crate::spc::discussion::MdType) as a number. See
/// [`AlertCategoryWire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MdTypeWire(crate::spc::discussion::MdType);

impl MdTypeWire {
    fn wire_code(self) -> u8 {
        use crate::spc::discussion::MdType as M;
        match self.0 {
            M::Convective => 0,
            M::WinterWeather => 1,
            M::Other => 2,
        }
    }

    fn from_wire_code(code: u8) -> Option<Self> {
        use crate::spc::discussion::MdType as M;
        let md_type = match code {
            0 => M::Convective,
            1 => M::WinterWeather,
            2 => M::Other,
            _ => return None,
        };
        Some(Self(md_type))
    }
}

/// A [`HatchPattern`](crate::types::HatchPattern) as a number. See
/// [`AlertCategoryWire`]. `None` maps to 0 — the quiet value for the quiet
/// variant — and the three Conditional Intensity Groups take their own
/// numbers, because a CIG3 read as a CIG1 is a cross-hatch silently demoted
/// to dots on the layer that qualifies SPC's strongest areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HatchWire(crate::types::HatchPattern);

impl HatchWire {
    fn wire_code(self) -> u8 {
        use crate::types::HatchPattern as H;
        match self.0 {
            H::None => 0,
            H::Cig1 => 1,
            H::Cig2 => 2,
            H::Cig3 => 3,
        }
    }

    fn from_wire_code(code: u8) -> Option<Self> {
        use crate::types::HatchPattern as H;
        let hatch = match code {
            0 => H::None,
            1 => H::Cig1,
            2 => H::Cig2,
            3 => H::Cig3,
            _ => return None,
        };
        Some(Self(hatch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nws::alert::AlertCategory;
    use crate::render::rasterize::{
        AlertPaint, DiscussionPaint, FlashPaint, GridWindow, IndexWindow, RadarSiteInfo,
        ReportPaint,
    };
    use crate::spc::discussion::MdType;
    use crate::spc::reports::StormReportKind;
    use crate::types::{HatchPattern, OverlayFeature};
    use rustdar_geo::GeoBounds;
    use rustdar_source::job::{DescribedJob, DescribedOut};

    fn test_geometry() -> JobGeometry {
        JobGeometry {
            width: 64,
            height: 32,
            bounds: GeoBounds {
                min_lat: 30.0,
                max_lat: 40.0,
                min_lon: -100.0,
                max_lon: -90.0,
            },
            side_ceiling_px: 0,
        }
    }

    fn assert_round_trips(row: &JobCodec, job: &DescribedJob) {
        let geo = test_geometry();
        let mut bytes = Vec::new();
        (row.encode)(job, &EncodeCtx { geometry: geo }, &mut bytes);
        let mut r = Reader::new(&bytes);
        let (decoded, geo_out) =
            (row.decode)(&mut r, geo).expect("a row must decode its own encode");
        assert_eq!(
            &decoded, job,
            "decode ∘ encode must be the identity for `{}`",
            row.label,
        );
        assert_eq!(
            geo_out, geo,
            "`{}` amends nothing: the geometry passes through unchanged",
            row.label,
        );
        assert!(
            r.at_end(),
            "`{}` decode must consume exactly what encode wrote",
            row.label,
        );
    }

    fn feature_fixture() -> OverlayFeature {
        OverlayFeature {
            polygons: vec![vec![vec![(35.0, -97.5), (36.25, -97.5), (36.25, -96.0)]]],
            fill_rgba: [255, 40, 0, 80],
            stroke_rgba: [255, 40, 0, 255],
            label: "Tornado Warning".to_owned(),
            label2: "until 22:00 UTC".to_owned(),
            hatch: HatchPattern::None,
            geo_bounds: Some(GeoBounds {
                min_lat: 35.0,
                max_lat: 36.25,
                min_lon: -97.5,
                max_lon: -96.0,
            }),
        }
    }

    fn hatched_boundless_feature_fixture() -> OverlayFeature {
        OverlayFeature {
            polygons: vec![vec![
                vec![(30.5, -99.0), (31.5, -99.0), (31.5, -98.0), (30.5, -98.0)],
                vec![(30.75, -98.75), (31.25, -98.75), (31.25, -98.25)],
            ]],
            fill_rgba: [0, 0, 0, 0],
            stroke_rgba: [128, 0, 128, 255],
            label: "SIG".to_owned(),
            label2: String::new(),
            hatch: HatchPattern::Cig3,
            geo_bounds: None,
        }
    }

    fn ts(secs: i64, nanos: u32) -> chrono::NaiveDateTime {
        chrono::DateTime::from_timestamp(secs, nanos)
            .expect("a literal in chrono's range")
            .naive_utc()
    }

    /// The labels of the rows this build registers, spelled out per feature
    /// arm. **Two `#[cfg]`'d definitions**, matching the two definitions of
    /// `JOB_CODECS` itself — which is what makes this pin catch a row that
    /// drifted between them.
    #[cfg(not(feature = "fake-source"))]
    const EXPECTED_LABELS: [&str; 7] = [
        "overlay/sites",
        "overlay/alerts",
        "overlay/outlooks",
        "overlay/discussions",
        "overlay/reports",
        "overlay/glm",
        "overlay/model",
    ];

    #[cfg(feature = "fake-source")]
    const EXPECTED_LABELS: [&str; 8] = [
        "overlay/sites",
        "overlay/alerts",
        "overlay/outlooks",
        "overlay/discussions",
        "overlay/reports",
        "overlay/glm",
        "overlay/model",
        // Appended, never inserted: the composed registry numbers rows by
        // position and an insert would renumber every shipped wire code.
        "overlay/fake",
    ];

    #[test]
    fn the_registry_is_the_seven_rows_in_dispatch_order() {
        assert_eq!(
            JOB_CODECS.iter().map(|row| row.label).collect::<Vec<_>>(),
            EXPECTED_LABELS,
            "the labels are the shipped kind strings and the order is \
             load-bearing: WO-M7b's dense code flip assigns codes by index",
        );
        for row in JOB_CODECS {
            assert_eq!(
                row.cost,
                JobCost::Raster,
                "every overlay row is a raster job (`{}`)",
                row.label,
            );
        }
        let distinct: std::collections::HashSet<std::any::TypeId> =
            JOB_CODECS.iter().map(|row| (row.input_type)()).collect();
        assert_eq!(
            distinct.len(),
            JOB_CODECS.len(),
            "rows are selected by input type; two rows sharing one would \
             make routing ambiguous",
        );
    }

    #[test]
    fn the_sites_row_round_trips() {
        let job = DescribedJob::new(SitesInput {
            sites: vec![
                RadarSiteInfo {
                    name: "KTLX".to_owned(),
                    lat: 35.333,
                    lon: -97.278,
                    is_current: true,
                    is_loading: false,
                },
                RadarSiteInfo {
                    name: "KFDR".to_owned(),
                    lat: 34.362,
                    lon: -98.976,
                    is_current: false,
                    is_loading: true,
                },
            ],
            zoom: 7.5,
            is_dark: true,
            device_scale: 2.0,
        });
        assert_round_trips(&JOB_CODECS[0], &job);
    }

    #[test]
    fn the_alerts_row_round_trips() {
        let job = DescribedJob::new(AlertsInput {
            alerts: vec![
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0001".to_owned(),
                    category: AlertCategory::Warning,
                    features: std::sync::Arc::new(vec![feature_fixture()]),
                },
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0002".to_owned(),
                    category: AlertCategory::Other,
                    features: std::sync::Arc::new(vec![hatched_boundless_feature_fixture()]),
                },
            ],
            enabled_categories: vec![AlertCategory::Warning, AlertCategory::Watch],
            hidden_ids: ["urn:oid:2.49.0.1.840.0002".to_owned()]
                .into_iter()
                .collect(),
            device_scale: 1.0,
        });
        assert_round_trips(&JOB_CODECS[1], &job);
    }

    #[test]
    fn the_outlooks_row_round_trips() {
        let job = DescribedJob::new(OutlooksInput {
            features: vec![feature_fixture(), hatched_boundless_feature_fixture()],
            hatch_color: [10, 20, 30, 40],
            device_scale: 1.5,
        });
        assert_round_trips(&JOB_CODECS[2], &job);
    }

    #[test]
    fn the_discussions_row_round_trips() {
        let job = DescribedJob::new(DiscussionsInput {
            discussions: vec![
                DiscussionPaint {
                    md_type: MdType::Convective,
                    polygon: vec![vec![(33.0, -98.0), (34.0, -98.0), (34.0, -97.0)]],
                },
                DiscussionPaint {
                    md_type: MdType::WinterWeather,
                    polygon: vec![vec![(41.0, -94.0), (42.0, -94.0), (42.0, -93.0)]],
                },
            ],
            device_scale: 1.0,
        });
        assert_round_trips(&JOB_CODECS[3], &job);
    }

    #[test]
    fn the_reports_row_round_trips_in_order() {
        let job = DescribedJob::new(ReportsInput {
            reports: vec![
                ReportPaint {
                    kind: StormReportKind::Wind,
                    lat: 35.5,
                    lon: -97.5,
                },
                ReportPaint {
                    kind: StormReportKind::Tornado,
                    lat: 35.25,
                    lon: -97.75,
                },
                ReportPaint {
                    kind: StormReportKind::Hail,
                    lat: 36.0,
                    lon: -96.5,
                },
            ],
            zoom: 6.0,
            is_dark: false,
            device_scale: 1.0,
        });
        assert_round_trips(&JOB_CODECS[4], &job);
    }

    #[test]
    fn the_glm_row_round_trips_in_order() {
        let job = DescribedJob::new(GlmStrikesInput {
            flashes: vec![
                FlashPaint {
                    lat: 34.5,
                    lon: -99.0,
                    time: ts(1_755_216_000, 250_000_000),
                    energy: Some(1.5),
                },
                FlashPaint {
                    lat: 34.75,
                    lon: -98.5,
                    time: ts(1_755_215_400, 0),
                    energy: None,
                },
            ],
            zoom: 5.0,
            is_dark: true,
            time_window_secs: 600.0,
            now: ts(1_755_216_030, 0),
            device_scale: 2.0,
        });
        assert_round_trips(&JOB_CODECS[5], &job);
    }

    #[test]
    fn the_model_row_round_trips_an_explicit_grid_window() {
        let job = DescribedJob::new(GriddedInput::Window(GridWindow {
            field: crate::hrrr::fields::spec(crate::hrrr::ModelParameter::SurfaceBasedCape)
                .id
                .clone(),
            ni: 4,
            nj: 3,
            coords: crate::hrrr::GridCoords::Explicit {
                lats: vec![30.0, 30.5, 31.0, 31.5],
                lons: vec![-99.0, -98.5, -98.0, -97.5],
            },
            win: IndexWindow {
                i0: 1,
                i1: 3,
                j0: 0,
                j1: 2,
            },
            values: vec![100.0, 250.0, 500.0, 1250.0],
        }));
        assert_round_trips(&JOB_CODECS[6], &job);
    }

    #[test]
    fn the_model_row_round_trips_lambert_constants() {
        let grid =
            crate::hrrr::lambert::LambertGrid::from_parts(crate::hrrr::lambert::LambertGridParts {
                a: 6371229.0,
                e: 0.0,
                n: 0.5,
                big_f: 1.5,
                rho0: 2.5,
                lon0: -1.75,
                x0: -2500.0,
                y0: 1500.0,
                dx: 3000.0,
                dy: 2000.0,
                ni: 4,
                nj: 3,
                i_consecutive: true,
                alternating: false,
                wraps_longitude: false,
            })
            .expect("valid literal constants");
        let job = DescribedJob::new(GriddedInput::Window(GridWindow {
            field: crate::hrrr::fields::spec(crate::hrrr::ModelParameter::MixedLayerCin)
                .id
                .clone(),
            ni: 4,
            nj: 3,
            coords: crate::hrrr::GridCoords::Lambert(grid),
            win: IndexWindow {
                i0: 0,
                i1: 4,
                j0: 1,
                j1: 3,
            },
            values: vec![-25.0, -50.0, 0.0, -12.5, -75.0, -100.0, -6.25, -3.0],
        }));
        assert_round_trips(&JOB_CODECS[6], &job);
    }

    /// The regular arm travels as its seven scalars and nothing else — the whole
    /// point of the arm — and comes back as the same grid.
    #[test]
    fn a_regular_grid_coords_round_trips() {
        for scan_mode in [0b0100_0000u8, 0b0110_0000, 0b0101_0000] {
            let job = DescribedJob::new(GriddedInput::Window(GridWindow {
                field: crate::hrrr::fields::spec(crate::hrrr::ModelParameter::SurfaceBasedCape)
                    .id
                    .clone(),
                ni: 4,
                nj: 3,
                coords: crate::hrrr::GridCoords::Regular {
                    lat0: 54.995,
                    lon0: -129.995,
                    dlat: -0.01,
                    dlon: 0.01,
                    ni: 4,
                    nj: 3,
                    scan_mode,
                },
                win: IndexWindow {
                    i0: 1,
                    i1: 3,
                    j0: 0,
                    j1: 2,
                },
                values: vec![100.0, 250.0, 500.0, 1250.0],
            }));
            assert_round_trips(&JOB_CODECS[6], &job);
        }
    }

    /// A regular grid this build would never write is **refused, not clamped**:
    /// every method on the arm divides by the steps, so a zero step silently
    /// stacks the whole grid on its own origin, and an empty shape indexes a
    /// lattice with no points.
    #[test]
    fn a_regular_grid_with_a_degenerate_shape_or_step_is_refused() {
        let honest = |scalars: [f64; 4], ni: u32, nj: u32| {
            let mut bytes = vec![super::GRID_COORDS_REGULAR];
            for v in scalars {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            bytes.extend_from_slice(&ni.to_le_bytes());
            bytes.extend_from_slice(&nj.to_le_bytes());
            bytes.push(0b0100_0000);
            bytes
        };
        // Control: the well-formed spelling of exactly these bytes decodes, so
        // every refusal below is the value and not the framing.
        let good = honest([54.995, -129.995, -0.01, 0.01], 4, 3);
        assert!(
            super::decode_grid_coords(&mut Reader::new(&good)).is_some(),
            "the control must decode, or the refusals prove nothing",
        );

        for (what, bytes) in [
            ("ni == 0", honest([54.995, -129.995, -0.01, 0.01], 0, 3)),
            ("nj == 0", honest([54.995, -129.995, -0.01, 0.01], 4, 0)),
            ("dlat == 0", honest([54.995, -129.995, 0.0, 0.01], 4, 3)),
            ("dlon == 0", honest([54.995, -129.995, -0.01, 0.0], 4, 3)),
            (
                "dlon is NaN",
                honest([54.995, -129.995, -0.01, f64::NAN], 4, 3),
            ),
            (
                "lat0 is infinite",
                honest([f64::INFINITY, -129.995, -0.01, 0.01], 4, 3),
            ),
            ("a short buffer", good[..good.len() - 1].to_vec()),
        ] {
            assert!(
                super::decode_grid_coords(&mut Reader::new(&bytes)).is_none(),
                "{what} was accepted rather than refused",
            );
        }
    }

    fn assert_reply_round_trips(row: &JobCodec, rgba: Vec<u8>, hit_cells: Option<HitCells>) {
        let reply = DescribedOut(Box::new(RasterizeOutput {
            rgba: rgba.clone(),
            hit_cells: hit_cells.clone(),
            alpha: AlphaMode::Premultiplied,
        }));
        let mut head = Vec::new();
        let mut tails = Vec::new();
        (row.encode_out)(reply, &mut head, &mut tails);
        let back = (row.decode_out)(&head, tails)
            .expect("a row must decode its own reply encode")
            .take::<RasterizeOutput>()
            .expect("the reply is a raster");
        assert_eq!(back.rgba, rgba, "the RGBA tail must survive unjudged");
        assert_eq!(back.hit_cells, hit_cells, "the cells must survive framed");
        assert_eq!(
            back.alpha,
            AlphaMode::Premultiplied,
            "the reply wire carries premultiplied bytes only — the frontend \
             converts before any reply is encoded",
        );
    }

    #[test]
    fn the_reply_round_trips_with_hit_cells() {
        let mut cells = HashMap::new();
        cells.insert(0u32, vec![0u32, 2]);
        cells.insert(3u32, vec![1u32]);
        assert_reply_round_trips(
            &JOB_CODECS[4],
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            Some(HitCells {
                width: 2,
                height: 2,
                cells,
            }),
        );
    }

    #[test]
    fn the_reply_round_trips_without_hit_cells() {
        assert_reply_round_trips(&JOB_CODECS[2], vec![9, 8, 7, 6], None);
    }

    #[test]
    fn two_encodes_of_one_reply_value_agree() {
        let indices = [21u32, 3, 17, 8, 30, 11, 26, 5];
        let forward: HashMap<u32, Vec<u32>> =
            indices.iter().map(|&idx| (idx, vec![idx * 2])).collect();
        let backward: HashMap<u32, Vec<u32>> = indices
            .iter()
            .rev()
            .map(|&idx| (idx, vec![idx * 2]))
            .collect();
        let rgba = [0u8; 4];
        let mut a = Vec::new();
        encode_overlay_out(
            &rgba,
            Some(&HitCells {
                width: 8,
                height: 4,
                cells: forward,
            }),
            &mut a,
        );
        let mut b = Vec::new();
        encode_overlay_out(
            &rgba,
            Some(&HitCells {
                width: 8,
                height: 4,
                cells: backward,
            }),
            &mut b,
        );
        assert_eq!(
            a, b,
            "one reply value must have one byte string, whatever order its \
             map iterates in",
        );
    }
}
