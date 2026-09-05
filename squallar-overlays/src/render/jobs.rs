//! The seven overlay codec rows — the overlay half of the job boundary.
//!
//! **The refusal contract**, shared by every `decode` here: `None` for a
//! flag or enum byte outside this build's values, a string that is not
//! UTF-8, or a buffer shorter than its own counts claim.
//!
//! **No clock.** GLM's `now` is captured at dispatch and travels on the
//! wire; nothing in this module may read a clock of its own — a worker that
//! did would render a picture the direct call would not.

use std::collections::HashSet;

use squallar_source::job::{EncodeCtx, JobCodec, JobCost, JobGeometry, JobOutCodec, JobSpec};
use squallar_source::wire::Reader;

use crate::render::rasterize::{
    AlertsInput, AlphaMode, CoverageInput, DiscussionsInput, GlmStrikesInput, GriddedInput,
    HitCells, OutlooksInput, RasterizeOutput, ReportsInput, rasterize_glm_strikes,
    rasterize_gridded, rasterize_nws_alerts, rasterize_radar_coverage, rasterize_spc_discussions,
    rasterize_spc_outlooks, rasterize_storm_reports,
};

/// The seven overlay rows, in dispatch order: **coverage, alerts, outlooks,
/// discussions, reports, glm, model**. The order is load-bearing:
/// `squallar_worker::job_registry::job_codecs` numbers rows by position across
/// the composed chain, so a row inserted anywhere but the end would renumber
/// the shipped wire codes.
///
/// Row 0 was `overlay/sites` until the site markers became a per-frame layer.
/// It is **repurposed in place** rather than removed and re-appended, precisely
/// because of the numbering above: it is still the radar network's ground, now
/// narrowed to the coverage the sites raster was really drawing.
pub static JOB_CODECS: &[JobCodec] = &[
    JobCodec::of::<CoverageJob>(),
    JobCodec::of::<AlertsJob>(),
    JobCodec::of::<OutlooksJob>(),
    JobCodec::of::<DiscussionsJob>(),
    JobCodec::of::<ReportsJob>(),
    JobCodec::of::<GlmJob>(),
    JobCodec::of::<GriddedJob>(),
    JobCodec::of::<MetarJob>(),
];

/// The radar-network coverage row.
pub struct CoverageJob;

impl JobSpec for CoverageJob {
    type In = CoverageInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/coverage";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &CoverageInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        out.extend_from_slice(&input.device_scale.to_le_bytes());
        out.extend_from_slice(&(input.sites.len() as u32).to_le_bytes());
        for site in &input.sites {
            out.extend_from_slice(&site.lat.to_le_bytes());
            out.extend_from_slice(&site.lon.to_le_bytes());
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(CoverageInput, JobGeometry)> {
        let device_scale = r.f32()?;
        let count = r.u32()? as usize;
        let mut sites = Vec::new();
        for _ in 0..count {
            let lat = r.f64()?;
            let lon = r.f64()?;
            sites.push(crate::render::rasterize::CoverageSite { lat, lon });
        }
        Some((
            crate::render::rasterize::CoverageInput {
                sites,
                device_scale,
            },
            geo,
        ))
    }

    fn run(input: &CoverageInput, geo: &JobGeometry) -> Option<RasterizeOutput> {
        Some(rasterize_radar_coverage(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for CoverageJob {
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
        // The depicted instant, on the wire (WB-2): the worker culls
        // not-yet-happened reports against this and never against a clock of
        // its own — the same rule as GLM's `now`.
        encode_datetime(out, &reports.as_of);
        out.extend_from_slice(&(reports.reports.len() as u32).to_le_bytes());
        for report in reports.reports.iter() {
            out.push(StormReportKindWire(report.kind).wire_code());
            out.extend_from_slice(&report.lat.to_le_bytes());
            out.extend_from_slice(&report.lon.to_le_bytes());
            match report.valid {
                None => out.push(0),
                Some(valid) => {
                    out.push(1);
                    encode_datetime(out, &valid);
                }
            }
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(ReportsInput, JobGeometry)> {
        let zoom = r.f64()?;
        let is_dark = flag(r.u8()?)?;
        let device_scale = r.f32()?;
        let as_of = decode_datetime(r)?;
        let count = r.u32()? as usize;
        let mut reports = Vec::new();
        for _ in 0..count {
            reports.push(crate::render::rasterize::ReportPaint {
                kind: StormReportKindWire::from_wire_code(r.u8()?)?.0,
                lat: r.f64()?,
                lon: r.f64()?,
                valid: match r.u8()? {
                    0 => None,
                    1 => Some(decode_datetime(r)?),
                    _ => return None,
                },
            });
        }
        Some((
            crate::render::rasterize::ReportsInput {
                reports: std::sync::Arc::new(reports),
                zoom,
                is_dark,
                device_scale,
                as_of,
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

/// The METAR station-model row.
///
/// **The last layer to leave the frame thread.** Every other data overlay
/// already rasterizes into a picture; METAR alone drew per station through
/// `egui`, and a scene D leg carries 799 of them at 46 shapes each — 28 lines,
/// 7 stroked circles, 6 filled circles, 4 polygons and 5 texts. Measured on a
/// 175 Hz leg that was 98,815 vertices and 401,072 indices staged EVERY FRAME,
/// with `epaint::tessellator::stroke_and_fill_path` the largest symbol in the
/// app, for observations that change every twenty minutes.
///
/// **The wire carries the twelve fields the drawing reads and no others.**
/// `name`, `elev_m`, `wind_gust_kt`, `altimeter_hpa`, `raw_ob` and `obs_time`
/// are hover content, answered page-side from the real items that
/// `hit_items` holds, and `draw_metar_station` never reads them — which is a
/// claim under test rather than a comment: see
/// `two_observations_differing_only_in_dropped_fields_paint_identically`.
pub struct MetarJob;

/// The station's own `WindDir`, on the wire.
struct WindDirWire(Option<crate::metar::types::WindDir>);

impl WindDirWire {
    fn write(&self, out: &mut Vec<u8>) {
        use crate::metar::types::WindDir;
        match self.0 {
            None => out.push(0),
            Some(WindDir::Calm) => out.push(1),
            Some(WindDir::Variable) => out.push(2),
            Some(WindDir::Degrees(d)) => {
                out.push(3);
                out.extend_from_slice(&d.to_le_bytes());
            }
        }
    }

    fn read(r: &mut Reader<'_>) -> Option<Option<crate::metar::types::WindDir>> {
        use crate::metar::types::WindDir;
        Some(match r.u8()? {
            0 => None,
            1 => Some(WindDir::Calm),
            2 => Some(WindDir::Variable),
            3 => Some(WindDir::Degrees(r.u16()?)),
            _ => return None,
        })
    }
}

fn encode_opt_f64(out: &mut Vec<u8>, v: Option<f64>) {
    match v {
        None => out.push(0),
        Some(x) => {
            out.push(1);
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
}

fn decode_opt_f64(r: &mut Reader<'_>) -> Option<Option<f64>> {
    Some(match r.u8()? {
        0 => None,
        1 => Some(r.f64()?),
        _ => return None,
    })
}

impl JobSpec for MetarJob {
    type In = crate::render::rasterize::MetarInput;
    type Out = RasterizeOutput;
    const LABEL: &'static str = "overlay/metar";
    const COST: JobCost = JobCost::Raster;

    fn encode(input: &crate::render::rasterize::MetarInput, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
        use crate::metar::types::FlightCategory;
        out.extend_from_slice(&input.zoom.to_le_bytes());
        out.push(u8::from(input.is_dark));
        out.extend_from_slice(&input.device_scale.to_le_bytes());
        out.extend_from_slice(&(input.obs.len() as u32).to_le_bytes());
        // **Row order is load-bearing**: a row's position is its hit-map id,
        // so a reorder hands hovers to the wrong stations.
        for ob in input.obs.iter() {
            encode_str(out, &ob.station_id);
            out.extend_from_slice(&ob.lat.to_le_bytes());
            out.extend_from_slice(&ob.lon.to_le_bytes());
            encode_opt_f64(out, ob.temp_c);
            encode_opt_f64(out, ob.dewp_c);
            encode_opt_f64(out, ob.mslp_hpa);
            WindDirWire(ob.wind_dir).write(out);
            match ob.wind_speed_kt {
                None => out.push(0),
                Some(kt) => {
                    out.push(1);
                    out.extend_from_slice(&kt.to_le_bytes());
                }
            }
            match ob.visibility {
                None => out.push(0),
                Some(v) => {
                    out.push(1);
                    out.extend_from_slice(&v.miles.to_le_bytes());
                    out.push(u8::from(v.or_greater));
                }
            }
            out.push(match ob.flight_category {
                None => 0,
                Some(FlightCategory::VFR) => 1,
                Some(FlightCategory::MVFR) => 2,
                Some(FlightCategory::IFR) => 3,
                Some(FlightCategory::LIFR) => 4,
            });
            match &ob.wx_string {
                None => out.push(0),
                Some(wx) => {
                    out.push(1);
                    encode_str(out, wx);
                }
            }
            out.extend_from_slice(&(ob.clouds.len() as u32).to_le_bytes());
            for layer in &ob.clouds {
                encode_str(out, &layer.cover);
                match layer.base_ft {
                    None => out.push(0),
                    Some(ft) => {
                        out.push(1);
                        out.extend_from_slice(&ft.to_le_bytes());
                    }
                }
            }
        }
    }

    fn decode(
        r: &mut Reader<'_>,
        geo: JobGeometry,
    ) -> Option<(crate::render::rasterize::MetarInput, JobGeometry)> {
        use crate::metar::types::{CloudLayer, FlightCategory, MetarOb, Visibility};
        let zoom = r.f64()?;
        let is_dark = flag(r.u8()?)?;
        let device_scale = r.f32()?;
        let count = r.u32()? as usize;
        let mut obs = Vec::new();
        for _ in 0..count {
            let station_id = decode_str(r)?;
            let lat = r.f64()?;
            let lon = r.f64()?;
            let temp_c = decode_opt_f64(r)?;
            let dewp_c = decode_opt_f64(r)?;
            let mslp_hpa = decode_opt_f64(r)?;
            let wind_dir = WindDirWire::read(r)?;
            let wind_speed_kt = match r.u8()? {
                0 => None,
                1 => Some(r.u16()?),
                _ => return None,
            };
            let visibility = match r.u8()? {
                0 => None,
                1 => Some(Visibility {
                    miles: r.f64()?,
                    or_greater: flag(r.u8()?)?,
                }),
                _ => return None,
            };
            let flight_category = match r.u8()? {
                0 => None,
                1 => Some(FlightCategory::VFR),
                2 => Some(FlightCategory::MVFR),
                3 => Some(FlightCategory::IFR),
                4 => Some(FlightCategory::LIFR),
                _ => return None,
            };
            let wx_string = match r.u8()? {
                0 => None,
                1 => Some(decode_str(r)?),
                _ => return None,
            };
            let cloud_count = r.u32()? as usize;
            let mut clouds = Vec::new();
            for _ in 0..cloud_count {
                clouds.push(CloudLayer {
                    cover: decode_str(r)?,
                    base_ft: match r.u8()? {
                        0 => None,
                        1 => Some(r.u32()?),
                        _ => return None,
                    },
                });
            }
            // The six fields the wire does not carry, defaulted. The drawing
            // never reads them; the hover does, and the hover is answered
            // page-side from `hit_items`, never from a decoded picture.
            obs.push(MetarOb {
                station_id,
                name: String::new(),
                lat,
                lon,
                elev_m: None,
                temp_c,
                dewp_c,
                wind_dir,
                wind_speed_kt,
                wind_gust_kt: None,
                visibility,
                altimeter_hpa: None,
                mslp_hpa,
                flight_category,
                raw_ob: String::new(),
                clouds,
                wx_string,
                obs_time: String::new(),
            });
        }
        Some((
            crate::render::rasterize::MetarInput {
                obs: std::sync::Arc::new(obs),
                zoom,
                is_dark,
                device_scale,
            },
            geo,
        ))
    }

    fn run(
        input: &crate::render::rasterize::MetarInput,
        geo: &JobGeometry,
    ) -> Option<RasterizeOutput> {
        Some(crate::render::rasterize::rasterize_metar_stations(
            input,
            &geo.bounds,
            geo.width,
            geo.height,
        ))
    }
}

impl JobOutCodec for MetarJob {
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
        let win = encode_gridded_head(input, ctx, out);
        // No count: the length is the window's area, already on the wire
        // as the four edges, and a second statement of it could lie. The
        // same fact is what makes the reservation below computable before
        // the first value is written.
        //
        // **One allocation for the payload, before a byte of it is written.**
        // The values are `win.area()` f32s and nothing else in this row is
        // more than a few dozen bytes, so this is the size of the message.
        //
        // Growing into it instead costs more than the payload: `to_bytes`
        // starts from `Vec::new()`, and doubling from empty to a multi-MB
        // buffer copies about **133 % of the payload again** across ~23
        // reallocations — every one of them a memcpy on the FRAME THREAD,
        // because `JobRequest::to_bytes` runs at the dispatch site. The
        // figures below are OBSERVATIONS off browser legs, not derivations,
        // measured 2026-09-02: `to_bytes` is 99.3 % (Firefox) / 98.7 %
        // (Chromium) of the overlay job hand-off, and one scene D leg moved
        // 185.6 MB page->worker.
        //
        // `saturating_mul` because the area is grid-derived: a corrupt or
        // hostile shape must fail the encode's own bounds later, not overflow
        // into a small reservation here.
        //
        // The arithmetic is pinned by
        // `a_gridded_job_reserves_exactly_the_payload_it_then_writes` — if the
        // row loop ever writes something this does not price, the reservation
        // silently stops covering it and the growth comes back.
        // **In the store's own width**, which is what the head has just
        // named. Both arms write exactly what is reserved here.
        let repr = WireValues::of(input.values_ref());
        out.reserve(win.area().saturating_mul(repr.bytes_per_sample()));
        match repr {
            WireValues::F32 => input.for_each_window_row(&win, |row| encode_f32s(out, row)),
            // **No expansion and no scratch.** This runs inside
            // `JobRequest::to_bytes` on the FRAME THREAD, so a per-row
            // widening buffer here would spend the footprint win on frame
            // time. The codes are written exactly as stored.
            WireValues::ScaledU16 { .. } => {
                input.for_each_window_row_raw(&win, |row| out.extend_from_slice(row))
            }
        }
    }

    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(GriddedInput, JobGeometry)> {
        use crate::render::rasterize::GridWindow;
        let (field, ni, nj, coords, win, repr) = decode_gridded_head(r)?;
        // The values length is the window's own area **in the tag's width** —
        // no second count on the wire to disagree with it, and `take` refuses
        // a buffer shorter than the area claims before anything allocates.
        let values = repr.values_from(r.take(win.area().checked_mul(repr.bytes_per_sample())?)?)?;
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

    /// The grid this input already holds, lent rather than written.
    ///
    /// **Only the whole-grid arms.** A [`GriddedInput::Window`] has already
    /// been cut — its values are owned by the input itself, there is no
    /// longer-lived handle to hold, and it is the arm a DECODED job is in, so
    /// answering `Some` here would try to lend the worker's own copy back.
    ///
    /// What travels is the WHOLE grid, not the window: it is one contiguous
    /// allocation, so it is lendable as a single view, whereas the window's
    /// rows are strided and are not. The window still rides the head, and
    /// [`Self::decode_resident`] cuts to it at the far end — off the frame
    /// thread, which is the entire point.
    fn resident_payload(
        input: &GriddedInput,
        ctx: &squallar_source::job::EncodeCtx,
    ) -> Option<squallar_source::job::ResidentBytes> {
        use squallar_source::job::ResidentBytes;
        // Only the ROWS the window spans, not the whole grid. A row is `ni`
        // values wide and rows sit end to end, so rows `j0..j1` are one
        // unbroken run at `values[j0 * ni .. j1 * ni]` — lendable as a single
        // view exactly as the whole grid was, and on a zoomed pane a small
        // fraction of it. The columns are still cut at the far end, and the
        // head still names the window, so nothing about the far end changes
        // except where row `j0` starts.
        let (ni, _) = input.shape();
        let win = input.window_for(
            &ctx.geometry.bounds,
            ctx.geometry.width,
            ctx.geometry.height,
        );
        // **In the store's own width.** The lend is a byte range into the
        // grid's own allocation, so a narrower store simply lends half as
        // many bytes — the transport never learns what a sample is.
        let bps = input.values_ref().bytes_per_sample();
        let start = win.j0.saturating_mul(ni).saturating_mul(bps);
        let len = win
            .j1
            .saturating_sub(win.j0)
            .saturating_mul(ni)
            .saturating_mul(bps);
        match input {
            GriddedInput::Whole(grid) => Some(ResidentBytes::of_range(
                std::sync::Arc::clone(grid),
                |g| bytemuck::cast_slice(&g.values),
                start,
                len,
            )),
            GriddedInput::Resident(grid) => Some(ResidentBytes::of_range(
                std::sync::Arc::clone(grid),
                |g| g.values.stored_bytes(),
                start,
                len,
            )),
            GriddedInput::Window(_) => None,
        }
    }

    fn encode_resident_head(
        input: &GriddedInput,
        ctx: &squallar_source::job::EncodeCtx,
        out: &mut Vec<u8>,
    ) {
        let _ = encode_gridded_head(input, ctx, out);
    }

    /// The head's envelope plus a payload that is the WHOLE grid, cut here.
    ///
    /// The length check is the wire's own statement of what arrived: the
    /// payload has to be exactly `ni * nj` f32s, because that is the grid the
    /// head just described. A payload of any other length is a different grid
    /// than the envelope names, and cutting a window out of it would rasterize
    /// the wrong values with nothing to say so.
    fn decode_resident(
        r: &mut Reader<'_>,
        geo: JobGeometry,
        payload: &[u8],
    ) -> Option<(GriddedInput, JobGeometry)> {
        use crate::render::rasterize::GridWindow;
        let (field, ni, nj, coords, win, repr) = decode_gridded_head(r)?;
        let _ = nj;
        // The payload is the window's ROWS, so its length is a function of the
        // window the head just named — not of the whole grid. Checked rather
        // than trusted: a payload of any other length is a different band than
        // the envelope describes, and cutting columns out of it would
        // rasterize the wrong values with nothing to say so.
        //
        // **The width comes from the head's own tag**, never from a hardcoded
        // four: the same head that named the window named the store's width,
        // so the two cannot drift apart.
        let bps = repr.bytes_per_sample();
        if payload.len()
            != win
                .j1
                .checked_sub(win.j0)?
                .checked_mul(ni)?
                .checked_mul(bps)?
        {
            return None;
        }
        // The columns are cut here, in the STORED width — the widening, if any,
        // happens one value at a time at `GriddedInput::value_at`, so the
        // worker's own copy of the band is the same size as the page's.
        let mut cut: Vec<u8> = Vec::with_capacity(win.area() * bps);
        for j in win.j0..win.j1 {
            // `j - win.j0`: row `win.j0` is the FIRST row of the payload,
            // because only the window's band was lent.
            let start = ((j - win.j0) * ni + win.i0) * bps;
            let end = ((j - win.j0) * ni + win.i1) * bps;
            cut.extend_from_slice(payload.get(start..end)?);
        }
        let values = repr.values_from(&cut)?;
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
}

/// **How a gridded job's values travel: the wire's own statement of the store's
/// width.**
///
/// A gridded grid is `f32` for a source whose values really are floats (HRRR,
/// GMGSI) and 16-bit codes for one whose values are not (MRMS). Both ride this
/// one row, so the head says which — and every length on the wire is derived
/// from that tag rather than from a hardcoded four.
///
/// **`two_pow` and `dig_factor` travel as the precomputed operands**, never as
/// `exp` and `dec`. The far end is a different build on a different target, and
/// the whole losslessness claim is that its arithmetic is bit-identical to the
/// decoder's; recomputing `2_f32.powi(exp)` there would rest that claim on two
/// `powi` implementations agreeing in the last ULP. Carrying the operands makes
/// it identical **by construction**.
///
/// A build that does not know a tag answers `None` all the way out through
/// `JobRequest::from_parts`, which the caller reads as "nothing to draw" and
/// the pane keeps its last texture. **Refuse, never misread**: the alternative
/// is reading 16-bit codes as `f32` and painting a continent of noise.
#[derive(Debug, Clone, PartialEq)]
enum WireValues {
    F32,
    ScaledU16 {
        ref_val: f32,
        two_pow: f32,
        dig_factor: f32,
        nan_codes: Vec<u16>,
    },
}

/// Values tag: one `f32` a point.
const WIRE_VALUES_F32: u8 = 0;
/// Values tag: one 16-bit code a point, plus the affine it reads back through.
const WIRE_VALUES_SCALED_U16: u8 = 1;

impl WireValues {
    fn of(view: crate::render::gridded::ValuesRef<'_>) -> Self {
        match view {
            crate::render::gridded::ValuesRef::F32(_) => Self::F32,
            crate::render::gridded::ValuesRef::Scaled(s) => Self::ScaledU16 {
                ref_val: s.ref_val,
                two_pow: s.two_pow,
                dig_factor: s.dig_factor,
                nan_codes: s.nan_codes.clone(),
            },
        }
    }

    /// **The multiplier every length on this wire is built from.**
    fn bytes_per_sample(&self) -> usize {
        match self {
            Self::F32 => size_of::<f32>(),
            Self::ScaledU16 { .. } => size_of::<u16>(),
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::F32 => out.push(WIRE_VALUES_F32),
            Self::ScaledU16 {
                ref_val,
                two_pow,
                dig_factor,
                nan_codes,
            } => {
                out.push(WIRE_VALUES_SCALED_U16);
                out.extend_from_slice(&ref_val.to_le_bytes());
                out.extend_from_slice(&two_pow.to_le_bytes());
                out.extend_from_slice(&dig_factor.to_le_bytes());
                // A `u8` count, because the store refuses more than
                // `MAX_NAN_CODES` of them; see `gridded::ScaledU16`.
                out.push(nan_codes.len() as u8);
                for code in nan_codes {
                    out.extend_from_slice(&code.to_le_bytes());
                }
            }
        }
    }

    fn decode(r: &mut Reader<'_>) -> Option<Self> {
        match r.u8()? {
            WIRE_VALUES_F32 => Some(Self::F32),
            WIRE_VALUES_SCALED_U16 => {
                let ref_val = r.f32()?;
                let two_pow = r.f32()?;
                let dig_factor = r.f32()?;
                let count = usize::from(r.u8()?);
                // The store's own ceiling, restated as a wire refusal: a head
                // claiming more reserved codes than the narrow arm will ever
                // hold is a head this build cannot honour.
                if count > crate::render::gridded::MAX_NAN_CODES {
                    return None;
                }
                let mut nan_codes = Vec::with_capacity(count);
                for _ in 0..count {
                    nan_codes.push(r.u16()?);
                }
                Some(Self::ScaledU16 {
                    ref_val,
                    two_pow,
                    dig_factor,
                    nan_codes,
                })
            }
            _ => None,
        }
    }

    /// Rebuild `count` values from `bytes`, in this tag's width.
    ///
    /// Read through `from_le_bytes` rather than casting the slice: a lent
    /// payload arrives as a `Uint8Array::to_vec`, whose allocation carries no
    /// alignment at all, so a cast would refuse on exactly the platforms this
    /// path exists for. The bytes are the same either way — the wire is
    /// little-endian by the assertion beside [`encode_f32s`].
    fn values_from(&self, bytes: &[u8]) -> Option<crate::render::gridded::GridValues> {
        use crate::render::gridded::{GridValues, ScaledU16};
        if !bytes.len().is_multiple_of(self.bytes_per_sample()) {
            return None;
        }
        match self {
            Self::F32 => Some(GridValues::F32(f32s_from_bytes(bytes))),
            Self::ScaledU16 {
                ref_val,
                two_pow,
                dig_factor,
                nan_codes,
            } => Some(GridValues::Scaled(ScaledU16 {
                codes: bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect(),
                ref_val: *ref_val,
                two_pow: *two_pow,
                dig_factor: *dig_factor,
                nan_codes: nan_codes.clone(),
            })),
        }
    }
}

/// The gridded envelope, written once for both encoders.
///
/// Returns the window it wrote so the value-writing encoder does not recompute
/// it — and, more to the point, so the two encoders cannot come to disagree
/// about which window the head names.
fn encode_gridded_head(
    input: &GriddedInput,
    ctx: &squallar_source::job::EncodeCtx,
    out: &mut Vec<u8>,
) -> crate::render::rasterize::IndexWindow {
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
    // **Last in the head, and read back first by every values reader.** Both
    // encoders write it, so neither can produce a payload whose width the head
    // does not name.
    WireValues::of(input.values_ref()).encode(out);
    win
}

/// The gridded envelope, read once for both decoders.
#[allow(clippy::type_complexity)]
fn decode_gridded_head(
    r: &mut Reader<'_>,
) -> Option<(
    squallar_source::product::FieldId,
    usize,
    usize,
    crate::hrrr::GridCoords,
    crate::render::rasterize::IndexWindow,
    WireValues,
)> {
    use crate::render::rasterize::IndexWindow;
    // The field code is believed only if this build **registers** it —
    // `field_paint` answering is exactly the condition under which
    // `rasterize_gridded` can paint it. A code this build does not know is a
    // newer build's field, and defaulting it would rasterize one field's
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
    // A window past the grid it indexes, or inside-out, is a layout this build
    // never writes: refused, not clamped, so a raster of it cannot silently
    // draw a different region than was asked. An empty window (a viewport the
    // grid never reaches) is legitimate and its area is zero.
    if win.i0 > win.i1 || win.j0 > win.j1 || win.i1 > ni || win.j1 > nj {
        return None;
    }
    // Unknown tag -> `None`, which is the whole refusal: see `WireValues`.
    let repr = WireValues::decode(r)?;
    Some((field, ni, nj, coords, win, repr))
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
    encode_overlay_out(&v.rgba, v.blank, v.hit_cells.as_ref(), head);
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
    let (rgba, blank, hit_cells) = decode_overlay_out(head)?;
    Some(RasterizeOutput {
        rgba,
        hit_cells,
        alpha: AlphaMode::Premultiplied,
        blank,
    })
}

/// The overlay reply's bytes: a hit-cells tag, the framed cells when the tag
/// says so, a pixels tag, and then either the four bytes a blank costs or the
/// raw RGBA as the rest.
///
/// The cells are written **sorted by cell index** — a hash map's iteration
/// order is a function of its hasher, its capacity and the order things went
/// in, none of which is the value, and these bytes have to be a function of the
/// value for two encodes of one reply to agree. That was true when the map was
/// seeded per process by `RandomState` and it stays true now that
/// [`HitCellMap`](crate::render::rasterize::HitCellMap) is unseeded: the sort is
/// what makes the encoding canonical, not the hasher.
///
/// **`blank` is transported, never re-decided.** `Some(len)` writes the pixels
/// tag `0` and that length and no pixels at all — the whole of what a raster
/// with no ink in it costs the wire, against `len` bytes before
/// (8.26 MB a picture on the measured Chromium legs, 8.92 MB on the Firefox
/// ones, two targets never added). `None` writes tag `1` and the RGBA takes
/// the rest, exactly as it did. Which of the two a reply is was settled once,
/// by [`RasterizeOutput::settle_blank`](crate::render::rasterize::RasterizeOutput::settle_blank)
/// in the run funnel's output stage; asking again here could answer
/// differently and put a picture on a pane that was told to clear.
pub fn encode_overlay_out(
    rgba: &[u8],
    blank: Option<u32>,
    hit_cells: Option<&HitCells>,
    out: &mut Vec<u8>,
) {
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
    match blank {
        None => {
            out.push(1);
            out.extend_from_slice(rgba);
        }
        Some(len) => {
            out.push(0);
            out.extend_from_slice(&len.to_le_bytes());
        }
    }
}

/// The inverse of [`encode_overlay_out`], answering `(rgba, blank, cells)` on
/// the same terms the encoder took them.
///
/// `None` for a hit-cells or pixels tag outside `{0, 1}`, a cell index at or
/// past the grid the stated dimensions span, indices out of ascending order or
/// repeated (the canonical form the encoder writes and the only one accepted,
/// so one value has one byte string), an empty id list (the rasterizer never
/// records one), a buffer shorter than its own counts claim, or **bytes after
/// a blank's length** — a blank's payload is exactly those four bytes, so a
/// tail there is a corrupt or foreign message rather than pixels this build
/// should read. The RGBA tail is handed back **unjudged**: only the dispatch
/// knows the dimensions it must match.
pub fn decode_overlay_out(bytes: &[u8]) -> Option<(Vec<u8>, Option<u32>, Option<HitCells>)> {
    let mut r = Reader::new(bytes);
    let hit_cells = match r.u8()? {
        0 => None,
        1 => {
            let width = r.u32()?;
            let height = r.u32()?;
            let grid = u64::from(width) * u64::from(height);
            let occupied = r.u32()? as usize;
            let mut cells = crate::render::rasterize::HitCellMap::default();
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
    match r.u8()? {
        1 => Some((r.rest().to_vec(), None, hit_cells)),
        0 => {
            let len = r.u32()?;
            r.rest()
                .is_empty()
                .then_some((Vec::new(), Some(len), hit_cells))
        }
        _ => None,
    }
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
/// **The job wire is little-endian by definition** — [`f32s_from_bytes`]
/// reads `f32::from_le_bytes` — and every target this ships to is little-endian, so
/// an `f32` slice's own bytes ARE its wire form. A big-endian target would
/// need a byte swap and this encoder would silently write the wrong bytes, so
/// the build refuses there rather than guessing. Compile-time, and a
/// `cfg!` in a body would be the forbidden behaviour fork.
const _: () = assert!(
    cfg!(target_endian = "little"),
    "the job wire is little-endian; a big-endian target needs a swapping f32 encoder",
);

/// Every value of a grid row in ONE copy.
///
/// This was a loop calling `extend_from_slice` with four bytes per value, and
/// a gridded overlay job carries a window of them: MRMS and GMGSI both ride
/// the `overlay/model` row, and one scene D leg moved **185.6 MB page->worker
/// at an aggregate 3.3 GB/s** where a copy runs several times that. Measured
/// 2026-09-02: `JobRequest::to_bytes` is **99.3 % (Firefox) / 98.7 %
/// (Chromium)** of the overlay job hand-off, and that hand-off is 73-96 % of
/// the web dispatch cut — all of it on the frame thread.
///
/// `cast_slice` is safe and total here: `f32` and `u8` are both `Pod`, the
/// target alignment falls from 4 to 1, and the length is exact.
/// Byte-for-byte identical to what the loop wrote, which
/// `the_bulk_f32_encoding_is_byte_identical_to_the_value_at_a_time_form` pins
/// over the values that would break a lazier equivalence — NaN, both
/// infinities, negative zero and a subnormal.
fn encode_f32s(out: &mut Vec<u8>, values: &[f32]) {
    out.extend_from_slice(bytemuck::cast_slice(values));
}

/// The inverse of [`encode_f32s`]: the wide arm of [`WireValues::values_from`]
/// reads through this, and the length it is handed is [`Reader::take`]'s,
/// checked against the head's window **before** anything is sized from it.
/// Read through `from_le_bytes` rather than cast: a lent payload arrives as a
/// `Uint8Array::to_vec`, whose allocation carries no alignment at all.
fn f32s_from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().expect("chunks of four")))
        .collect()
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
/// Grid-coordinate tag: one axis per dimension. **Appended, not inserted** —
/// the three tags above keep their numbers.
const GRID_COORDS_SEPARABLE: u8 = 4;

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
        GridCoords::Separable { lat_axis, lon_axis } => {
            out.push(GRID_COORDS_SEPARABLE);
            // Two counts for the same reason the explicit arm writes two: the
            // axes are independent lengths, and here they are the grid's two
            // dimensions, so one count could not describe the shape at all.
            out.extend_from_slice(&(lat_axis.len() as u32).to_le_bytes());
            for v in lat_axis {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(lon_axis.len() as u32).to_le_bytes());
            for v in lon_axis {
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
        GRID_COORDS_SEPARABLE => {
            let lat_count = r.u32()? as usize;
            let lat_axis = decode_f64s(r, lat_count)?;
            let lon_count = r.u32()? as usize;
            let lon_axis = decode_f64s(r, lon_count)?;
            Some(GridCoords::Separable { lat_axis, lon_axis })
        }
        _ => None,
    }
}

/// [`f32s_from_bytes`]'s shape at `f64` width and count-led, for the explicit
/// coordinate arrays.
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
        1 => Some(squallar_geo::GeoBounds {
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
fn encode_polygon(out: &mut Vec<u8>, polygon: &squallar_geo::GeoPolygon) {
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
fn decode_polygon(r: &mut Reader) -> Option<squallar_geo::GeoPolygon> {
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
    /// A station with every field populated, so a round trip has something to
    /// lose in each of them.
    #[cfg(test)]
    fn a_full_station() -> crate::metar::types::MetarOb {
        use crate::metar::types::{CloudLayer, FlightCategory, MetarOb, Visibility, WindDir};
        MetarOb {
            station_id: "KTLX".into(),
            name: "Oklahoma City".into(),
            lat: 35.33,
            lon: -97.28,
            elev_m: Some(390.0),
            temp_c: Some(21.5),
            dewp_c: Some(-3.5),
            wind_dir: Some(WindDir::Degrees(230)),
            wind_speed_kt: Some(17),
            wind_gust_kt: Some(25),
            visibility: Some(Visibility {
                miles: 10.0,
                or_greater: true,
            }),
            altimeter_hpa: Some(1013.2),
            mslp_hpa: Some(1011.8),
            flight_category: Some(FlightCategory::MVFR),
            raw_ob: "KTLX 121953Z 23017KT 10SM BKN025 22/M04 A2992".into(),
            clouds: vec![
                CloudLayer {
                    cover: "BKN".into(),
                    base_ft: Some(2500),
                },
                CloudLayer {
                    cover: "CLR".into(),
                    base_ft: None,
                },
            ],
            wx_string: Some("RA".into()),
            obs_time: "2026-09-04T19:53:00Z".into(),
        }
    }

    /// **The twelve fields the picture needs survive the wire exactly.**
    ///
    /// Every nested shape is represented in the fixture — a `WindDir` variant
    /// that carries a payload, a `Visibility` with its `or_greater` flag, a
    /// `FlightCategory`, and two `CloudLayer`s of which one has no base — so a
    /// codec that dropped or reordered any of them fails here rather than
    /// drawing a subtly wrong station model.
    #[test]
    fn a_metar_job_round_trips_every_field_the_picture_reads() {
        use super::*;
        let input = crate::render::rasterize::MetarInput {
            obs: std::sync::Arc::new(vec![a_full_station()]),
            zoom: 8.5,
            is_dark: true,
            device_scale: 2.0,
        };
        let ctx = squallar_source::job::EncodeCtx {
            geometry: test_geometry(),
        };
        let mut out = Vec::new();
        <MetarJob as JobSpec>::encode(&input, &ctx, &mut out);
        let mut r = Reader::new(&out);
        let (back, _) = <MetarJob as JobSpec>::decode(&mut r, test_geometry())
            .expect("a metar job decodes its own bytes");
        assert!(r.rest().is_empty(), "the decode left bytes on the wire");

        assert_eq!(back.zoom, input.zoom);
        assert_eq!(back.is_dark, input.is_dark);
        assert_eq!(back.device_scale, input.device_scale);
        let (a, b) = (&input.obs[0], &back.obs[0]);
        assert_eq!(b.station_id, a.station_id);
        assert_eq!((b.lat, b.lon), (a.lat, a.lon));
        assert_eq!(
            (b.temp_c, b.dewp_c, b.mslp_hpa),
            (a.temp_c, a.dewp_c, a.mslp_hpa)
        );
        assert_eq!(b.wind_dir, a.wind_dir);
        assert_eq!(b.wind_speed_kt, a.wind_speed_kt);
        assert_eq!(b.visibility, a.visibility);
        assert_eq!(b.flight_category, a.flight_category);
        assert_eq!(b.wx_string, a.wx_string);
        assert_eq!(b.clouds, a.clouds);
    }

    /// Every call a station model makes, in order, as comparable data.
    ///
    /// Its own recorder rather than `station_model`'s: that one is private to
    /// its module's tests, and reaching for it would mean widening a test-only
    /// type's visibility to serve a different module's gate.
    #[derive(Default, Debug, PartialEq)]
    struct CallLog(Vec<String>);

    impl crate::render::draw::PointPainter for CallLog {
        fn circle_filled(&mut self, o: [f32; 2], r: f32, c: [u8; 4]) {
            self.0.push(format!("circle_filled {o:?} {r} {c:?}"));
        }

        fn circle_stroke(&mut self, o: [f32; 2], r: f32, c: [u8; 4], w: f32) {
            self.0.push(format!("circle_stroke {o:?} {r} {c:?} {w}"));
        }

        fn text(
            &mut self,
            o: [f32; 2],
            t: &str,
            c: [u8; 4],
            size: f32,
            anchor: crate::render::draw::TextAnchor,
        ) {
            self.0
                .push(format!("text {o:?} {t:?} {c:?} {size} {anchor:?}"));
        }

        fn line(&mut self, from: [f32; 2], to: [f32; 2], c: [u8; 4], w: f32) {
            self.0.push(format!("line {from:?} {to:?} {c:?} {w}"));
        }

        fn filled_polygon(&mut self, pts: &[[f32; 2]], c: [u8; 4]) {
            self.0.push(format!("filled_polygon {pts:?} {c:?}"));
        }
    }

    /// **The six fields the wire drops really are unread by the drawing.**
    ///
    /// The codec omits `name`, `elev_m`, `wind_gust_kt`, `altimeter_hpa`,
    /// `raw_ob` and `obs_time` on the grounds that `draw_metar_station` never
    /// reads them and a hover is answered page-side from `hit_items`. That is
    /// a claim about behaviour, and a comment asserting it would rot the first
    /// time somebody plots the gust. Here it is a test: two observations that
    /// differ in ALL SIX and in nothing else must record the same calls.
    ///
    /// If this fails, the wire is dropping something the picture draws, and
    /// the station model on screen is missing it.
    #[test]
    fn two_observations_differing_only_in_dropped_fields_paint_identically() {
        use crate::render::draw::DrawPointContext;
        use crate::render::station_model;

        let full = a_full_station();
        let mut stripped = full.clone();
        stripped.name = String::new();
        stripped.elev_m = None;
        stripped.wind_gust_kt = None;
        stripped.altimeter_hpa = None;
        stripped.raw_ob = String::new();
        stripped.obs_time = String::new();

        // Every tier, because the model is zoom-gated and a field could be
        // read at one zoom and not another.
        for zoom in [3.0f32, 6.0, 9.0, 12.0] {
            let ctx = DrawPointContext {
                zoom,
                is_dark: false,
            };
            let mut a = CallLog::default();
            let mut b = CallLog::default();
            station_model::draw_metar_station(
                &full,
                &station_model::StationText::of(&full),
                &mut a,
                &ctx,
            );
            station_model::draw_metar_station(
                &stripped,
                &station_model::StationText::of(&stripped),
                &mut b,
                &ctx,
            );
            assert_eq!(
                a, b,
                "at zoom {zoom} the station model read a field the wire does \
                 not carry, so the picture would draw a different station than \
                 the frame thread did",
            );
        }
    }

    /// **A lent grid is cut to the window its head names, origin included.**
    ///
    /// The end-to-end model-wire parity gate cannot see this. Its dispatched
    /// pane windows the full grid width, so `i0` and `j0` are zero there and a
    /// cut that ignored the window's origin produces byte-identical pixels —
    /// verified by tampering `start` to drop `win.i0`, which that gate passed.
    /// Here the window is deliberately interior on BOTH axes, so an origin the
    /// cut forgets reads a different rectangle out of the payload.
    ///
    /// This is the arithmetic the split wire moved: the copying encoder cut the
    /// window on the sending side, and the lending one cuts it here instead.
    #[test]
    fn a_lent_grid_is_cut_to_the_window_its_head_names() {
        use super::*;
        use crate::hrrr::GridCoords;
        use crate::render::rasterize::GriddedInput;

        let (ni, nj) = (6usize, 5usize);
        // Every cell says where it is, so a cut from the wrong origin lands on
        // values that name the place it actually read.
        let values: Vec<f32> = (0..nj)
            .flat_map(|j| (0..ni).map(move |i| (j * 10 + i) as f32))
            .collect();
        let coords = GridCoords::Regular {
            lat0: 30.0,
            lon0: -100.0,
            dlat: 0.5,
            dlon: 0.5,
            ni,
            nj,
            scan_mode: 0,
        };
        // A code this build registers, because `decode_resident` refuses one it
        // does not — the same check the copying decoder makes.
        let field = crate::render::gridded::paint_for_code("vis")
            .expect("this build registers the `vis` field")
            .id
            .clone();

        let (i0, i1, j0, j1) = (2usize, 5, 1, 4);
        let mut head = Vec::new();
        encode_str(&mut head, field.as_str());
        head.extend_from_slice(&(ni as u32).to_le_bytes());
        head.extend_from_slice(&(nj as u32).to_le_bytes());
        encode_grid_coords(&mut head, &coords);
        for edge in [i0, i1, j0, j1] {
            head.extend_from_slice(&(edge as u32).to_le_bytes());
        }
        WireValues::F32.encode(&mut head);

        // The payload is the window's BAND, rows j0..j1, exactly as
        // `resident_payload` now lends it — not the whole grid.
        let payload: &[u8] = bytemuck::cast_slice(&values[j0 * ni..j1 * ni]);
        let geo = JobGeometry {
            width: 8,
            height: 8,
            bounds: squallar_geo::GeoBounds {
                min_lat: 30.0,
                max_lat: 32.0,
                min_lon: -100.0,
                max_lon: -98.0,
            },
            side_ceiling_px: 0,
        };
        let mut r = Reader::new(&head);
        let (input, _) = <GriddedJob as JobSpec>::decode_resident(&mut r, geo, payload)
            .expect("a whole grid and the window naming part of it decode");

        let GriddedInput::Window(window) = input else {
            panic!("a lent grid decodes to the cut window, not to a whole-grid carry");
        };
        assert_eq!(
            window.values.to_f32(),
            vec![12.0, 13.0, 14.0, 22.0, 23.0, 24.0, 32.0, 33.0, 34.0],
            "the cut read the wrong rectangle out of the lent grid",
        );
        assert_eq!((window.win.i0, window.win.j0), (i0, j0));
    }

    /// **A narrow grid survives the whole wire at its own width, bit for bit.**
    ///
    /// The half the decoder's own losslessness proof does not reach: that proof
    /// (`mrms::decode::tests::every_mosaic_value_is_a_sixteen_bit_code_and_three_scalars`)
    /// stops at the store. This carries a `GridValues::Scaled` grid through the
    /// lent path — head, tag, band, column cut — and asserts the far end reads
    /// back the **identical `f32` bit patterns**, `NaN` sentinels included.
    ///
    /// What it pins that a value comparison would not:
    ///
    /// * the payload is **half** what the same grid would have lent as `f32`,
    ///   so the wire really did narrow rather than widening at the boundary;
    /// * the far end holds `Scaled` too, so the **worker's own copy** is narrow
    ///   and the halving is three-for-one — page, wire and worker;
    /// * `to_bits`, so a reserved code that arrived as a real reading instead
    ///   of a `NaN` is a failure rather than a rounding difference.
    #[test]
    fn a_narrow_grid_crosses_the_wire_at_its_own_width_bit_for_bit() {
        use super::*;
        use crate::hrrr::GridCoords;
        use crate::render::gridded::{GridValues, ScaledU16};
        use crate::render::rasterize::GriddedInput;

        let (ni, nj) = (6usize, 5usize);
        // MRMS's own composite packing, and code 0 is its −999 no-coverage
        // sentinel, so the grid carries a reserved code as well as readings.
        //
        // **The sentinel is planted at flat index 15, which is inside the cut
        // window below, and that placement is the whole point.** A plain
        // `(0..30)` ramp puts code 0 at index 0 — outside both the lent band
        // (rows 1..4) and the compared window (i 2..5, j 1..4) — so the `NaN`
        // half of the `to_bits` comparison would never be reached and the
        // failure this test names (a reserved code arriving as the reading
        // −999.0 because `nan_codes` did not cross the wire) could not fire.
        // A real mosaic has no-coverage holes scattered through it; this is
        // one.
        let scaled = ScaledU16 {
            codes: (0..(ni * nj) as u16)
                .map(|c| if c == 15 { 0 } else { c })
                .collect(),
            ref_val: -9990.0,
            two_pow: 1.0,
            dig_factor: 0.1,
            nan_codes: vec![0],
        };
        let coords = GridCoords::Regular {
            lat0: 30.0,
            lon0: -100.0,
            dlat: 0.5,
            dlon: 0.5,
            ni,
            nj,
            scan_mode: 0,
        };
        let field = crate::render::gridded::paint_for_code("vis")
            .expect("this build registers the `vis` field")
            .id
            .clone();

        let (i0, i1, j0, j1) = (2usize, 5, 1, 4);
        let mut head = Vec::new();
        encode_str(&mut head, field.as_str());
        head.extend_from_slice(&(ni as u32).to_le_bytes());
        head.extend_from_slice(&(nj as u32).to_le_bytes());
        encode_grid_coords(&mut head, &coords);
        for edge in [i0, i1, j0, j1] {
            head.extend_from_slice(&(edge as u32).to_le_bytes());
        }
        WireValues::of(GridValues::Scaled(scaled.clone()).view()).encode(&mut head);

        // The band, lent exactly as `resident_payload` lends it: two bytes a
        // sample, not four.
        let codes = &scaled.codes[j0 * ni..j1 * ni];
        let payload: &[u8] = bytemuck::cast_slice(codes);
        assert_eq!(
            payload.len(),
            (j1 - j0) * ni * 2,
            "the lent band must be two bytes a sample, or the narrowing never \
             reached the wire",
        );

        let geo = JobGeometry {
            width: 8,
            height: 8,
            bounds: squallar_geo::GeoBounds {
                min_lat: 30.0,
                max_lat: 32.0,
                min_lon: -100.0,
                max_lon: -98.0,
            },
            side_ceiling_px: 0,
        };
        let mut r = Reader::new(&head);
        let (input, _) = <GriddedJob as JobSpec>::decode_resident(&mut r, geo, payload)
            .expect("a narrow grid and the window naming part of it decode");

        let GriddedInput::Window(window) = input else {
            panic!("a lent grid decodes to the cut window");
        };
        assert!(
            matches!(window.values, GridValues::Scaled(_)),
            "the worker's own copy must stay narrow; widening at the boundary \
             would give back a third of the win",
        );

        // Every value, against what the page would have read at the same point.
        let page = GridValues::Scaled(scaled);
        let row_w = i1 - i0;
        for j in j0..j1 {
            for i in i0..i1 {
                let far = window
                    .values
                    .get((j - j0) * row_w + (i - i0))
                    .expect("inside the cut");
                let here = page.get(j * ni + i).expect("inside the grid");
                assert_eq!(
                    far.to_bits(),
                    here.to_bits(),
                    "point ({i}, {j}) read back as {far} where the page holds {here}",
                );
            }
        }
        // Non-vacuity, **both halves**: the window really does span readings
        // AND the reserved code, so the loop above crossed each of them.
        let (mut readings, mut sentinels) = (0usize, 0usize);
        for j in j0..j1 {
            for i in i0..i1 {
                match page.get(j * ni + i).expect("inside the grid") {
                    v if v.is_finite() => readings += 1,
                    _ => sentinels += 1,
                }
            }
        }
        assert!(
            readings > 0,
            "a window of nothing but sentinels would satisfy the loop above",
        );
        assert!(
            sentinels > 0,
            "the window spans no reserved code, so the loop above never \
             compared a `NaN` and a `nan_codes` list that failed to cross the \
             wire would read back as the finite −999.0 unnoticed",
        );
    }

    /// **The bulk encoding writes exactly what the value-at-a-time loop wrote.**
    ///
    /// The loop is kept here as the reference rather than deleted: an
    /// optimisation of a WIRE FORMAT is only correct if it is invisible on the
    /// wire, and "invisible" is a claim about bytes, not about intent. The
    /// values are chosen to break a lazier equivalence — a NaN (whose bit
    /// pattern a float comparison would call unequal to itself), both
    /// infinities, negative zero (which `==` calls equal to positive zero, so
    /// only the bytes can tell them apart) and a subnormal.
    #[test]
    fn the_bulk_f32_encoding_is_byte_identical_to_the_value_at_a_time_form() {
        fn reference(values: &[f32]) -> Vec<u8> {
            let mut out = Vec::new();
            for v in values {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out
        }
        let values = [
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            f32::MIN,
            f32::MAX,
            f32::EPSILON,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::from_bits(0x0000_0001), // subnormal
            f32::from_bits(0x7FC0_0001), // a quiet NaN with a payload
            35.7,
            -122.4,
        ];
        // Whole slice, and every prefix: a bulk copy that mishandled a length
        // would still pass on one convenient size.
        for take in 0..=values.len() {
            let slice = &values[..take];
            let mut bulk = Vec::new();
            super::encode_f32s(&mut bulk, slice);
            assert_eq!(
                bulk,
                reference(slice),
                "bulk encoding differs from the value-at-a-time form at len {take}",
            );
            assert_eq!(bulk.len(), take * 4, "wrong byte count at len {take}");
        }
    }

    /// The round trip still closes: what the fast encoder writes, the decoder
    /// reads back bit-for-bit. `to_bits` because `NAN != NAN`.
    #[test]
    fn the_bulk_encoded_values_decode_back_to_the_same_bits() {
        let values = [f32::NAN, -0.0f32, f32::INFINITY, 1.5, f32::from_bits(1)];
        let mut buf = Vec::new();
        super::encode_f32s(&mut buf, &values);
        // Through the reader that ships: the wide arm of `values_from`.
        let mut r = squallar_source::wire::Reader::new(&buf);
        let back = super::f32s_from_bytes(r.take(values.len() * 4).expect("decodes"));
        let got: Vec<u32> = back.iter().map(|v| v.to_bits()).collect();
        let want: Vec<u32> = values.iter().map(|v| v.to_bits()).collect();
        assert_eq!(got, want);
    }

    use super::*;
    use crate::nws::alert::AlertCategory;
    use crate::render::rasterize::{
        AlertPaint, CoverageSite, DiscussionPaint, FlashPaint, GridWindow, IndexWindow, ReportPaint,
    };
    use crate::spc::discussion::MdType;
    use crate::spc::reports::StormReportKind;
    use crate::types::{HatchPattern, OverlayFeature};
    use squallar_geo::GeoBounds;
    use squallar_source::job::{DescribedJob, DescribedOut};

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

    /// The labels of the rows this build registers, spelled out as literals.
    /// **Deliberately not derived from `JOB_CODECS`** — a list read off the
    /// thing it checks cannot catch a row that moved.
    const EXPECTED_LABELS: [&str; 8] = [
        "overlay/coverage",
        "overlay/alerts",
        "overlay/outlooks",
        "overlay/discussions",
        "overlay/reports",
        "overlay/glm",
        "overlay/model",
        "overlay/metar",
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
    fn the_coverage_row_round_trips() {
        let job = DescribedJob::new(CoverageInput {
            sites: vec![
                CoverageSite {
                    lat: 35.333,
                    lon: -97.278,
                },
                CoverageSite {
                    lat: 34.362,
                    lon: -98.976,
                },
            ],
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
            reports: std::sync::Arc::new(vec![
                // Both wire arms of `valid`: dated rows and an undated one.
                ReportPaint {
                    kind: StormReportKind::Wind,
                    lat: 35.5,
                    lon: -97.5,
                    valid: Some(ts(1_755_216_000, 0)),
                },
                ReportPaint {
                    kind: StormReportKind::Tornado,
                    lat: 35.25,
                    lon: -97.75,
                    valid: None,
                },
                ReportPaint {
                    kind: StormReportKind::Hail,
                    lat: 36.0,
                    lon: -96.5,
                    valid: Some(ts(1_755_219_600, 0)),
                },
            ]),
            zoom: 6.0,
            is_dark: false,
            device_scale: 1.0,
            as_of: ts(1_755_220_000, 0),
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

    /// **The gridded encoder's values block is exactly four bytes per window
    /// cell — which is what the reservation prices.**
    ///
    /// `to_bytes` starts from `Vec::new()`, so without a reservation a
    /// multi-megabyte grid message is grown by doubling: about 133 % of the
    /// payload copied again across ~23 reallocations, every one on the FRAME
    /// THREAD, since `to_bytes` runs at the dispatch site. The encoder now
    /// reserves `win.area() * 4` before the row loop.
    ///
    /// This holds that arithmetic against what the loop actually writes, by
    /// encoding one grid under two windows and measuring the difference. Every
    /// other part of the message — field code, shape, coords, the four edges —
    /// is identical between the two, so the delta IS the values block. If a
    /// field is ever added to that block, or a cell stops costing four bytes,
    /// the reservation silently stops covering the payload, the growth comes
    /// back, and nothing else in the suite would notice.
    #[test]
    fn a_gridded_job_reserves_exactly_the_payload_it_then_writes() {
        fn encoded_len(win: IndexWindow, values: Vec<f32>) -> usize {
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
                win,
                values: crate::render::gridded::GridValues::F32(values),
            }));
            let mut bytes = Vec::new();
            (JOB_CODECS[6].encode)(
                &job,
                &EncodeCtx {
                    geometry: test_geometry(),
                },
                &mut bytes,
            );
            bytes.len()
        }

        let wide = IndexWindow {
            i0: 1,
            i1: 3,
            j0: 0,
            j1: 2,
        };
        let narrow = IndexWindow {
            i0: 1,
            i1: 3,
            j0: 0,
            j1: 1,
        };
        assert_eq!(
            (wide.area(), narrow.area()),
            (4, 2),
            "the fixture windows moved"
        );

        let big = encoded_len(wide, vec![100.0, 250.0, 500.0, 1250.0]);
        let small = encoded_len(narrow, vec![100.0, 250.0]);
        assert_eq!(
            big - small,
            (wide.area() - narrow.area()) * 4,
            "two more window cells cost {} bytes, not {}; the reservation of \
             `win.area() * 4` no longer prices what the row loop writes",
            big - small,
            (wide.area() - narrow.area()) * 4,
        );
    }

    /// **The gridded row's bytes are exactly the fields it names, in the order
    /// it names them** — which is what makes reserving capacity for the values
    /// block a no-op on the wire.
    ///
    /// `Vec::reserve` sets capacity and never length, so the reservation cannot
    /// move a byte by construction. This holds that: the expected sequence is
    /// spelled out here from the fixture's own numbers rather than replayed
    /// through `encode_str` and `encode_grid_coords`, so it reddens if anyone
    /// ever "presizes" by writing into a sized buffer, if a field's width on
    /// the wire changes, or if the order of the row's parts moves. The window
    /// edges are the carried ones because `window_for` clamps a carried window
    /// to the grid shape and 4x3 does not clamp `1..3, 0..2`.
    ///
    /// **The head gained one byte when the store did**: the values tag closes
    /// the head, so this sequence carries it between the window edges and the
    /// values block.
    #[test]
    fn a_gridded_job_encodes_to_the_wire_bytes_its_fields_name() {
        let field = crate::hrrr::fields::spec(crate::hrrr::ModelParameter::SurfaceBasedCape)
            .id
            .clone();
        let lats = [30.0f64, 30.5, 31.0, 31.5];
        let lons = [-99.0f64, -98.5, -98.0, -97.5];
        let values = [100.0f32, 250.0, 500.0, 1250.0];

        let job = DescribedJob::new(GriddedInput::Window(GridWindow {
            field: field.clone(),
            ni: 4,
            nj: 3,
            coords: crate::hrrr::GridCoords::Explicit {
                lats: lats.to_vec(),
                lons: lons.to_vec(),
            },
            win: IndexWindow {
                i0: 1,
                i1: 3,
                j0: 0,
                j1: 2,
            },
            values: crate::render::gridded::GridValues::F32(values.to_vec()),
        }));
        let mut bytes = Vec::new();
        (JOB_CODECS[6].encode)(
            &job,
            &EncodeCtx {
                geometry: test_geometry(),
            },
            &mut bytes,
        );

        let code = field.as_str();
        let mut want: Vec<u8> = Vec::new();
        want.extend_from_slice(&(code.len() as u16).to_le_bytes());
        want.extend_from_slice(code.as_bytes());
        want.extend_from_slice(&4u32.to_le_bytes());
        want.extend_from_slice(&3u32.to_le_bytes());
        // The explicit-coordinates tag, spelled as the byte that travels.
        want.push(2);
        want.extend_from_slice(&(lats.len() as u32).to_le_bytes());
        for v in lats {
            want.extend_from_slice(&v.to_le_bytes());
        }
        want.extend_from_slice(&(lons.len() as u32).to_le_bytes());
        for v in lons {
            want.extend_from_slice(&v.to_le_bytes());
        }
        for edge in [1u32, 3, 0, 2] {
            want.extend_from_slice(&edge.to_le_bytes());
        }
        // The values tag, last in the head and spelled as the byte that
        // travels rather than as `WIRE_VALUES_F32`: this fixture is an `f32`
        // grid, so a store that silently changed arm — or a tag that moved out
        // of the head's tail — reddens here.
        want.push(0);
        for v in values {
            want.extend_from_slice(&v.to_le_bytes());
        }

        assert_eq!(
            bytes, want,
            "the gridded row's encoded bytes are not the fields it names, in order"
        );
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
            values: crate::render::gridded::GridValues::F32(vec![100.0, 250.0, 500.0, 1250.0]),
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
            values: crate::render::gridded::GridValues::F32(vec![
                -25.0, -50.0, 0.0, -12.5, -75.0, -100.0, -6.25, -3.0,
            ]),
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
                values: crate::render::gridded::GridValues::F32(vec![100.0, 250.0, 500.0, 1250.0]),
            }));
            assert_round_trips(&JOB_CODECS[6], &job);
        }
    }

    /// The separable arm travels as its two axes and comes back as the same
    /// grid — **in the same order**.
    ///
    /// The two axes have deliberately different lengths and disjoint value
    /// ranges here, so an encoder or decoder that swapped them could not
    /// produce an equal grid and could not even produce a well-shaped one.
    #[test]
    fn a_separable_grid_coords_round_trips() {
        let coords = crate::hrrr::GridCoords::Separable {
            lat_axis: vec![55.0, 54.5, 53.0],
            lon_axis: vec![-129.995, -129.0, -128.5, -120.25],
        };
        let job = DescribedJob::new(GriddedInput::Window(GridWindow {
            field: crate::hrrr::fields::spec(crate::hrrr::ModelParameter::SurfaceBasedCape)
                .id
                .clone(),
            ni: 4,
            nj: 3,
            coords: coords.clone(),
            win: IndexWindow {
                i0: 1,
                i1: 3,
                j0: 0,
                j1: 2,
            },
            values: crate::render::gridded::GridValues::F32(vec![100.0, 250.0, 500.0, 1250.0]),
        }));
        assert_round_trips(&JOB_CODECS[6], &job);

        // And directly, so the assertion names the axes rather than trusting
        // the whole job's equality to notice a swap.
        let mut bytes = Vec::new();
        super::encode_grid_coords(&mut bytes, &coords);
        assert_eq!(bytes[0], super::GRID_COORDS_SEPARABLE);
        let back = super::decode_grid_coords(&mut Reader::new(&bytes))
            .expect("the arm this build writes decodes");
        let crate::hrrr::GridCoords::Separable { lat_axis, lon_axis } = back else {
            panic!("tag 4 must decode as Separable");
        };
        assert_eq!(lat_axis, vec![55.0, 54.5, 53.0]);
        assert_eq!(lon_axis, vec![-129.995, -129.0, -128.5, -120.25]);
    }

    /// Tag 4 is **appended**: the three tags below it keep their numbers, so a
    /// payload written before this arm existed still decodes as what it was.
    #[test]
    fn the_separable_tag_is_appended_and_displaces_no_older_tag() {
        assert_eq!(super::GRID_COORDS_LAMBERT, 1);
        assert_eq!(super::GRID_COORDS_EXPLICIT, 2);
        assert_eq!(super::GRID_COORDS_REGULAR, 3);
        assert_eq!(super::GRID_COORDS_SEPARABLE, 4);

        // An explicit payload — tag 2 — is unchanged by tag 4 existing.
        let explicit = crate::hrrr::GridCoords::Explicit {
            lats: vec![30.0, 30.5],
            lons: vec![-99.0, -98.5],
        };
        let mut bytes = Vec::new();
        super::encode_grid_coords(&mut bytes, &explicit);
        assert_eq!(bytes[0], super::GRID_COORDS_EXPLICIT);
        assert_eq!(
            super::decode_grid_coords(&mut Reader::new(&bytes)),
            Some(explicit)
        );

        // A tag this build does not have is refused, not defaulted.
        assert!(super::decode_grid_coords(&mut Reader::new(&[5u8])).is_none());
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
            // Unjudged, which is what every producer but the output stage
            // hands over: the codec transports that answer and does not take
            // one of its own, so these bytes are the painted form whatever is
            // in them.
            blank: None,
        }));
        let mut head = Vec::new();
        let mut tails = Vec::new();
        (row.encode_out)(reply, &mut head, &mut tails);
        let back = (row.decode_out)(&head, tails)
            .expect("a row must decode its own reply encode")
            .take::<RasterizeOutput>()
            .expect("the reply is a raster");
        assert_eq!(back.rgba, rgba, "the RGBA tail must survive unjudged");
        assert_eq!(
            back.blank, None,
            "an unjudged reply came back as a blank; the codec has started \
             deciding for itself what the output stage already decided",
        );
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
        let mut cells = crate::render::rasterize::HitCellMap::default();
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
        let forward: crate::render::rasterize::HitCellMap =
            indices.iter().map(|&idx| (idx, vec![idx * 2])).collect();
        let backward: crate::render::rasterize::HitCellMap = indices
            .iter()
            .rev()
            .map(|&idx| (idx, vec![idx * 2]))
            .collect();
        let rgba = [0u8; 4];
        let mut a = Vec::new();
        encode_overlay_out(
            &rgba,
            None,
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
            None,
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
