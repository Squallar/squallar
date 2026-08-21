use super::*;
use rustdar_overlays::render::jobs::{decode_overlay_out, encode_overlay_out};
use rustdar_overlays::render::rasterize::RasterizeOutput;
use rustdar_radar::frame::RenderedFrame;
use rustdar_radar::jobs::{
    DecodeJob, Level3Job, Level3PairJob, RadarPlanJob, SectionJob, VoxelJob,
};
use rustdar_radar::render_input::RenderInput;
use rustdar_radar::scan::DecodedScan;
use rustdar_radar::voxel::VolumeGrid;
use rustdar_radar::voxel::{VoxelRequest, VoxelShape};
use rustdar_radar::xsect::CrossSection;
use rustdar_radar::xsect::SectionRequest;
use rustdar_source::handler::{PaneMut, PaneRef};
use rustdar_source::job::{DescribedJob, DescribedOut, JobGeometry};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// What a `FakePort` recorded: the id it was given and the bytes it was asked
/// to post, in order.
type Posted = Arc<std::sync::Mutex<Vec<(u64, Vec<u8>)>>>;

/// A port that records what it was handed instead of posting anywhere.
struct FakePort {
    posted: Posted,
    accept: bool,
}

impl JobSink for FakePort {
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        if !self.accept {
            // The refusal path, and the reason it hands the request back.
            return Err(request);
        }
        self.posted.lock().unwrap().push((id, request.to_bytes()));
        Ok(())
    }
}

fn attach(accept: bool) -> Posted {
    let posted: Posted = Arc::new(std::sync::Mutex::new(Vec::new()));
    set_worker(Box::new(FakePort {
        posted: Arc::clone(&posted),
        accept,
    }));
    posted
}

/// Leave this thread with no sink at all, so the funnel's `NoSink` arm.
fn detach() {
    abandon_worker("test teardown");
}

/// A job that is cheap to execute and easy to recognize.
pub(super) fn a_job() -> JobRequest {
    JobRequest::describe(
        RadarPlanJob {
            input: Box::new(
                RenderInput::from_bytes(&sample_input_bytes()).expect("fixture payload decodes"),
            ),
            values_wanted: true,
        },
        ceiling_only_geometry(4096),
    )
}

/// The smallest real volume: two sweeps of a handful of radials, under a VCP
/// that **declares its cuts**.
fn sample_scan() -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, PulseWidth, Radial, RadialStatus, Scan, Sweep,
        VolumeCoveragePattern, WaveformType,
    };
    let cut = |angle: f64| {
        ElevationCut::new(
            angle,
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
        )
    };
    let sweep = |elevation_number: u8, elevation: f32| {
        let radials = (0..36)
            .map(|i| {
                Radial::new(
                    0,
                    i,
                    f32::from(i) * 10.0,
                    10.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation,
                    Some(nexrad_model::data::MomentData::from_fixed_point(
                        120,
                        0,
                        250,
                        8,
                        2.0,
                        66.0,
                        vec![200; 120],
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
        Sweep::new(elevation_number, radials)
    };
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
            vec![cut(0.5), cut(1.5)],
        ),
        vec![sweep(1, 0.5), sweep(2, 1.5)],
    )
}

/// The single-tilt payload the `Radar` job carries.
fn sample_input_bytes() -> Vec<u8> {
    let scan = sample_scan();
    RenderInput::extract(
        &scan,
        0.5,
        rustdar_radar::types::RadarProduct::Reflectivity,
        35.0,
        -97.0,
        None,
        None,
    )
    .expect("fixture extracts")
    .to_bytes()
}

/// A Level III job. The bytes are opaque here on purpose.
fn a_level3_job() -> JobRequest {
    JobRequest::describe(
        Level3Job {
            bytes: std::sync::Arc::new(vec![7, 8, 9, 0xFF, 0]),
            product: rustdar_radar::types::RadarProduct::EchoTops,
            radar_lat: 35.0,
            radar_lon: -97.0,
        },
        ceiling_only_geometry(4096),
    )
}

/// The two-object VIL density job. The two payloads differ in length *and* in
/// content, so a framing that swapped them, or one that split them at the
/// wrong offset, cannot round-trip.
fn a_level3_pair_job() -> JobRequest {
    JobRequest::describe(
        Level3PairJob {
            dvl: std::sync::Arc::new(vec![1, 2, 3]),
            eet: std::sync::Arc::new(vec![4, 5, 6, 7, 0xFF, 0]),
            radar_lat: 35.0,
            radar_lon: -97.0,
        },
        ceiling_only_geometry(4096),
    )
}

/// The whole-volume payload the two vertical job kinds carry.
fn a_volume_input() -> RenderInput {
    RenderInput::extract_volume(
        &sample_scan(),
        rustdar_radar::types::RadarProduct::Reflectivity,
        35.0,
        -97.0,
    )
    .expect("the fixture carries reflectivity")
}

pub(super) fn a_section_job() -> JobRequest {
    JobRequest::describe(
        SectionJob {
            input: Box::new(a_volume_input()),
            request: SectionRequest {
                start: (35.0, -97.5),
                end: (35.4, -96.8),
                top_km_msl: Some(18.0),
                product: rustdar_radar::types::RadarProduct::Reflectivity,
            },
        },
        ceiling_only_geometry(0),
    )
}

/// The voxel request every voxel fixture varies: the exact literal values the
/// enum-era fixture carried.
fn a_voxel_request() -> VoxelRequest {
    VoxelRequest {
        centre: (35.0, -97.0),
        half_extent_km: Some(rustdar_radar::voxel::HalfExtentKm::square(60.0)),
        base_km_msl: 0.0,
        top_km_msl: 15.0,
        product: rustdar_radar::types::RadarProduct::Reflectivity,
        // Small and *asymmetric*, so a decoder that read the three axes in the
        // wrong order does not round-trip.
        shape: VoxelShape {
            nx: 8,
            ny: 6,
            nz: 4,
        },
        values_wanted: true,
    }
}

pub(super) fn a_voxel_job() -> JobRequest {
    JobRequest::describe(
        VoxelJob {
            input: Box::new(a_volume_input()),
            request: a_voxel_request(),
        },
        ceiling_only_geometry(0),
    )
}

/// The voxel job a pane with **no picked region** posts.
fn a_sourceless_voxel_job() -> JobRequest {
    JobRequest::describe(
        VoxelJob {
            input: Box::new(a_volume_input()),
            request: VoxelRequest {
                half_extent_km: None,
                ..a_voxel_request()
            },
        },
        ceiling_only_geometry(0),
    )
}

/// The sites overlay job, on a fixture with real content.
pub(super) fn an_overlay_sites_job() -> JobRequest {
    let site = |name: &str, lat: f64, lon: f64, is_current: bool| {
        rustdar_overlays::render::rasterize::RadarSiteInfo {
            name: name.to_owned(),
            lat,
            lon,
            is_current,
            is_loading: false,
        }
    };
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 64,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 33.0,
                max_lat: 37.0,
                min_lon: -99.0,
                max_lon: -96.0,
            },
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(rustdar_overlays::render::rasterize::SitesInput {
            sites: vec![
                site("KTLX", 35.33, -97.28, true),
                site("KVNX", 36.74, -98.13, false),
                site("PGUA", 13.46, 144.81, false),
            ],
            zoom: 6.5,
            is_dark: false,
            device_scale: 1.0,
        }),
    }
}

/// The NWS-alert overlay job, on a fixture with real content.
pub(super) fn an_overlay_alerts_job() -> JobRequest {
    use rustdar_overlays::nws::alert::AlertCategory;
    use rustdar_overlays::render::rasterize::{AlertPaint, AlertsInput};
    use rustdar_overlays::types::{HatchPattern, OverlayFeature};
    // One alert, two polygons; the first has a hole in it.
    let warned = OverlayFeature::new(
        vec![
            vec![
                vec![(34.2, -98.8), (34.2, -97.6), (35.6, -97.6), (35.6, -98.8)],
                vec![(34.7, -98.5), (34.7, -98.0), (35.2, -98.0), (35.2, -98.5)],
            ],
            vec![vec![
                (33.3, -97.4),
                (33.3, -96.6),
                (33.9, -96.6),
                (33.9, -97.4),
            ]],
        ],
        [255, 0, 0, 128],
        [255, 255, 255, 255],
        "Tornado Warning".into(),
        "Tornado Warning until 9 PM CDT".into(),
        HatchPattern::None,
    );
    let advised = OverlayFeature::new(
        vec![vec![vec![
            (36.0, -97.4),
            (36.0, -96.4),
            (36.8, -96.4),
            (36.8, -97.4),
        ]]],
        [0, 128, 255, 96],
        [0, 0, 0, 0],
        "Wind Advisory".into(),
        String::new(),
        HatchPattern::None,
    );
    // Hidden by id below; its square sits in an otherwise-empty corner, so a
    // hidden filter that stopped travelling would *add* painted pixels.
    let hidden = OverlayFeature::new(
        vec![vec![vec![
            (33.2, -98.9),
            (33.2, -98.4),
            (33.6, -98.4),
            (33.6, -98.9),
        ]]],
        [0, 255, 0, 200],
        [0, 0, 0, 0],
        "Flood Warning".into(),
        String::new(),
        HatchPattern::None,
    );
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 64,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 33.0,
                max_lat: 37.0,
                min_lon: -99.0,
                max_lon: -96.0,
            },
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(AlertsInput {
            alerts: vec![
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0001".into(),
                    category: AlertCategory::Warning,
                    features: Arc::new(vec![warned]),
                },
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0002".into(),
                    category: AlertCategory::Advisory,
                    features: Arc::new(vec![advised]),
                },
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0003".into(),
                    category: AlertCategory::Warning,
                    features: Arc::new(vec![hidden]),
                },
            ],
            enabled_categories: vec![AlertCategory::Warning, AlertCategory::Advisory],
            hidden_ids: std::collections::HashSet::from(["urn:oid:2.49.0.1.840.0003".to_owned()]),
            device_scale: 1.0,
        }),
    }
}

/// The SPC-outlook overlay job, with the pass the other kinds do not have.
pub(super) fn an_overlay_outlooks_job() -> JobRequest {
    use rustdar_overlays::render::rasterize::OutlooksInput;
    use rustdar_overlays::types::{HatchPattern, OverlayFeature};
    let categorical = OverlayFeature::new(
        vec![vec![
            vec![(33.4, -98.8), (33.4, -96.2), (36.8, -96.2), (36.8, -98.8)],
            vec![(36.0, -96.8), (36.0, -96.5), (36.4, -96.5), (36.4, -96.8)],
        ]],
        [214, 195, 155, 100],
        [180, 140, 80, 255],
        "SLGT".into(),
        "Slight Risk".into(),
        HatchPattern::None,
    );
    let cig1 = OverlayFeature::new(
        vec![vec![vec![
            (34.0, -98.4),
            (34.0, -96.8),
            (36.2, -96.8),
            (36.2, -98.4),
        ]]],
        [255, 200, 200, 40],
        [200, 0, 0, 255],
        "CIG1".into(),
        "Conditional Intensity 1".into(),
        HatchPattern::Cig1,
    );
    let cig3 = OverlayFeature::new(
        vec![vec![vec![
            (34.6, -98.0),
            (34.6, -97.4),
            (35.5, -97.4),
            (35.5, -98.0),
        ]]],
        [255, 120, 120, 40],
        [120, 0, 0, 255],
        "CIG3".into(),
        "Conditional Intensity 3".into(),
        HatchPattern::Cig3,
    );
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 64,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 33.0,
                max_lat: 37.0,
                min_lon: -99.0,
                max_lon: -96.0,
            },
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(OutlooksInput {
            features: vec![categorical, cig1, cig3],
            hatch_color: [0, 0, 255, 255],
            device_scale: 1.0,
        }),
    }
}

/// The SPC-discussion overlay job: two MDs of different types.
pub(super) fn an_overlay_discussions_job() -> JobRequest {
    use rustdar_overlays::render::rasterize::{DiscussionPaint, DiscussionsInput};
    use rustdar_overlays::spc::discussion::MdType;
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 64,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 33.0,
                max_lat: 37.0,
                min_lon: -99.0,
                max_lon: -96.0,
            },
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(DiscussionsInput {
            discussions: vec![
                DiscussionPaint {
                    md_type: MdType::Convective,
                    polygon: vec![
                        vec![(34.1, -98.7), (34.1, -97.9), (35.0, -97.9), (35.0, -98.7)],
                        vec![(35.3, -97.7), (35.3, -97.0), (35.9, -97.0), (35.9, -97.7)],
                    ],
                },
                DiscussionPaint {
                    md_type: MdType::WinterWeather,
                    polygon: vec![vec![
                        (36.1, -98.6),
                        (36.1, -97.8),
                        (36.7, -97.8),
                        (36.7, -98.6),
                    ]],
                },
            ],
            device_scale: 1.0,
        }),
    }
}

/// The storm-reports overlay job, on a fixture with real content.
pub(super) fn an_overlay_reports_job() -> JobRequest {
    use rustdar_overlays::render::rasterize::{ReportPaint, ReportsInput};
    use rustdar_overlays::spc::reports::StormReportKind;
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 64,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 33.0,
                max_lat: 37.0,
                min_lon: -99.0,
                max_lon: -96.0,
            },
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(ReportsInput {
            reports: vec![
                ReportPaint {
                    kind: StormReportKind::Tornado,
                    lat: 35.33,
                    lon: -97.28,
                },
                ReportPaint {
                    kind: StormReportKind::Hail,
                    lat: 36.2,
                    lon: -98.5,
                },
                ReportPaint {
                    kind: StormReportKind::Wind,
                    lat: 33.8,
                    lon: -96.7,
                },
                // Far outside the box: the cull path, and an id the cells must
                // therefore never record.
                ReportPaint {
                    kind: StormReportKind::Tornado,
                    lat: 10.0,
                    lon: -150.0,
                },
            ],
            zoom: 6.5,
            is_dark: false,
            device_scale: 1.0,
        }),
    }
}

/// The wire fixture's clock — a literal, never a clock read, so the GLM
/// fixture's flash ages are the same bytes on every run.
fn glm_fixture_now() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 14)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

/// The GLM overlay job, on the fixture the `now` hazard demands.
pub(super) fn an_overlay_glm_job() -> JobRequest {
    use rustdar_overlays::render::rasterize::{FlashPaint, GlmStrikesInput};
    let now = glm_fixture_now();
    let at = |age_secs: i64, lat: f64, lon: f64, energy: Option<f32>| FlashPaint {
        lat,
        lon,
        time: now - chrono::Duration::seconds(age_secs),
        energy,
    };
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 64,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 33.0,
                max_lat: 37.0,
                min_lon: -99.0,
                max_lon: -96.0,
            },
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(GlmStrikesInput {
            flashes: vec![
                at(60, 35.3, -97.3, Some(1e-15)),  // first third: white-ish
                at(270, 36.2, -98.5, None),        // second third; unknown energy
                at(480, 33.8, -96.7, Some(1e-13)), // last third: red-ish
                at(570, 34.5, -98.2, Some(1e-14)), // inside, hugging the window edge
                at(660, 35.8, -96.5, Some(1e-14)), // past the window: culled
            ],
            zoom: 6.5,
            is_dark: true,
            time_window_secs: 600.0,
            now,
            device_scale: 1.0,
        }),
    }
}

/// The model-grid fixtures' Lambert constants: a 60×44 grid on HRRR's own
/// projection and 3 km step, **as stored bits**.
fn a_lambert_parts() -> rustdar_overlays::hrrr::lambert::LambertGridParts {
    rustdar_overlays::hrrr::lambert::LambertGridParts {
        a: f64::from_bits(0x41584de740000000), // 6371229
        e: 0.0,
        n: f64::from_bits(0x3fe3eba3d0b47a0e), // 0.6225146366376195
        big_f: f64::from_bits(0x3fffab310810e319), // 1.979294806964566
        rho0: f64::from_bits(0x415e8e0126f7b628), // 8009732.608869113
        lon0: f64::from_bits(0x40125371ed71b637), // 4.581489286485115 rad
        x0: f64::from_bits(0xc1449498123e289b), // -2697520.1425219304
        y0: f64::from_bits(0xc138386a270df418), // -1587306.1525566634
        dx: 3000.0,
        dy: 3000.0,
        ni: 60,
        nj: 44,
        i_consecutive: true,
        alternating: false,
        wraps_longitude: false,
    }
}

/// A viewport whose corners sit on grid points (22, 16) and (34, 26) of
/// [`a_lambert_parts`]'s grid.
fn a_model_viewport() -> rustdar_geo::GeoBounds {
    rustdar_geo::GeoBounds {
        min_lat: f64::from_bits(0x4035b080d763ce74), // 21.68946596323663
        max_lat: f64::from_bits(0x4036058f2985b42e), // 22.02171573175672
        min_lon: f64::from_bits(0xc05e8ffce7e7a000), // -122.24981114978436
        max_lon: f64::from_bits(0xc05e8006a9e45528), // -122.00040671632871
    }
}

/// The full grid behind [`an_overlay_model_whole_job`].
fn a_model_grid() -> rustdar_overlays::hrrr::HrrrGridData {
    use rustdar_overlays::hrrr::{GridCoords, HrrrGridData, ModelParameter};
    let parameter = ModelParameter::SurfaceBasedCape;
    let (ni, nj) = (60usize, 44usize);
    let values: Vec<f32> = (0..ni * nj)
        .map(|k| ((k % 4001) + (k / ni) % 997) as f32)
        .collect();
    let geometry = rustdar_overlays::hrrr::lambert::LambertGrid::from_parts(a_lambert_parts())
        .expect("the fixture constants are the ones a real template produced");
    let (visible_points, value_range) =
        rustdar_overlays::hrrr::summarize_values(&values, parameter);
    HrrrGridData {
        parameter,
        values,
        coords: GridCoords::Lambert(geometry),
        ni,
        nj,
        // The rasterizer never reads `bounds` (hover does, and hover stays on
        // the page), and it does not travel.
        bounds: a_model_viewport(),
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 8, 14)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        forecast_hour: 0,
        visible_points,
        value_range,
    }
}

/// The model overlay job as the **dispatch** builds it.
pub(super) fn an_overlay_model_whole_job() -> JobRequest {
    use rustdar_overlays::render::rasterize::ModelDataInput;
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 72,
            bounds: a_model_viewport(),
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(ModelDataInput::Whole(std::sync::Arc::new(a_model_grid()))),
    }
}

/// The model overlay job in its **wire form** — the window carry the decoder
/// produces.
pub(super) fn an_overlay_model_job() -> JobRequest {
    use rustdar_overlays::render::rasterize::{IndexWindow, ModelDataInput, ModelWindow};
    let lambert = rustdar_overlays::hrrr::lambert::LambertGrid::from_parts(a_lambert_parts())
        .expect("the fixture constants are the ones a real template produced");
    JobRequest {
        geometry: JobGeometry {
            width: 96,
            height: 72,
            bounds: a_model_viewport(),
            side_ceiling_px: 0,
        },
        job: DescribedJob::new(ModelDataInput::Window(ModelWindow {
            parameter: rustdar_overlays::hrrr::ModelParameter::SurfaceBasedCape,
            ni: 60,
            nj: 44,
            coords: rustdar_overlays::hrrr::GridCoords::Lambert(lambert),
            win: IndexWindow {
                i0: 14,
                i1: 20,
                j0: 10,
                j1: 14,
            },
            values: (0..24).map(|k| (k * 100) as f32).collect(),
        })),
    }
}

/// The voxel job a pane whose viewport is **not square** posts.
fn a_rectangular_voxel_job() -> JobRequest {
    JobRequest::describe(
        VoxelJob {
            input: Box::new(a_volume_input()),
            request: VoxelRequest {
                half_extent_km: Some(rustdar_radar::voxel::HalfExtentKm {
                    east_km: 92.0,
                    north_km: 37.0,
                }),
                ..a_voxel_request()
            },
        },
        ceiling_only_geometry(0),
    )
}

#[test]
fn every_job_kind_survives_the_wire_format() {
    for job in [
        a_job(),
        a_level3_job(),
        a_level3_pair_job(),
        a_section_job(),
        a_voxel_job(),
        a_sourceless_voxel_job(),
        a_rectangular_voxel_job(),
        an_overlay_sites_job(),
        an_overlay_alerts_job(),
        an_overlay_outlooks_job(),
        an_overlay_discussions_job(),
        an_overlay_reports_job(),
        an_overlay_glm_job(),
        // The window form; the whole-grid form deliberately does NOT
        // round-trip to itself.
        an_overlay_model_job(),
    ] {
        assert_eq!(
            JobRequest::from_bytes(&job.to_bytes()),
            Some(job.clone()),
            "{:?} did not survive its round trip",
            job.kind()
        );
    }
}

/// An unallocated code must be refused, not resurrected.
#[test]
fn an_unallocated_code_is_refused() {
    let mut bytes = a_voxel_job().to_bytes();
    for unallocated in [0u8, 14] {
        bytes[0] = unallocated;
        assert_eq!(
            JobRequest::from_bytes(&bytes),
            None,
            "code {unallocated} decodes, so the literal code table has \
             stopped being the whole wire",
        );
    }
}

/// Every wire code is pinned to the literal registry index (plus one) and
/// label it ships as.
#[test]
fn every_code_is_the_literal_index_and_label_this_registry_composes() {
    // Deliberately spelled out. Do not regenerate this from the constants.
    let table: [(u8, &str); 13] = [
        (1, "radar"),
        (2, "level3"),
        (3, "level3/vild"),
        (4, "section"),
        (5, "voxels"),
        (6, "decode"),
        (7, "overlay/sites"),
        (8, "overlay/alerts"),
        (9, "overlay/outlooks"),
        (10, "overlay/discussions"),
        (11, "overlay/reports"),
        (12, "overlay/glm"),
        (13, "overlay/model"),
    ];

    // Against the composed registry: row `i` is labelled what this table says
    // and ships as code `i + 1`.
    assert_eq!(
        table.len(),
        job_codecs().count(),
        "a codec row exists with no code pin (or one was removed): every \
         row's dense wire code is spelled out here",
    );
    for (index, row) in job_codecs().enumerate() {
        let (code, label) = table[index];
        assert_eq!(
            row.label, label,
            "registry index {index} is not {label:?}: the composition moved, \
             which renumbers every code after it",
        );
        assert_eq!(
            usize::from(code),
            index + 1,
            "{label} is pinned to code {code}, which is not its registry \
             index {index} plus one — the pin and the composition disagree",
        );
    }

    // And the encoder really posts those bytes — the table could agree with
    // the registry while the framing that writes the byte does not.
    let framing: [(JobRequest, u8); 13] = [
        (a_job(), 1),
        (a_level3_job(), 2),
        (a_level3_pair_job(), 3),
        (a_section_job(), 4),
        (a_voxel_job(), 5),
        (a_decode_job(), 6),
        (an_overlay_sites_job(), 7),
        (an_overlay_alerts_job(), 8),
        (an_overlay_outlooks_job(), 9),
        (an_overlay_discussions_job(), 10),
        (an_overlay_reports_job(), 11),
        (an_overlay_glm_job(), 12),
        (an_overlay_model_job(), 13),
    ];
    for (job, code) in framing {
        let bytes = job.to_bytes();
        assert_eq!(
            bytes[0],
            code,
            "{:?} posts code {}, not {code} — a worker of another build \
                 decodes it as whatever {} names there",
            job.kind(),
            bytes[0],
            bytes[0],
        );
        assert_eq!(
            JobRequest::from_bytes(&bytes),
            Some(job.clone()),
            "{:?} did not decode back from its own framing",
            job.kind(),
        );
    }
}

/// The product is on the wire twice — in the request geometry and inside the
/// payload.
#[test]
fn a_request_naming_a_different_product_from_its_payload_is_refused() {
    // Offsets: the code byte and the 44-byte canonical envelope precede every
    // payload, so the section's product code sits at 45 and the voxel
    // request's at 46 (its `values_wanted` byte comes first).
    for (job, product_offset) in [(a_section_job(), 45), (a_voxel_job(), 46)] {
        let mut bytes = job.to_bytes();
        let code = rustdar_radar::types::RadarProduct::Velocity.wire_code();
        bytes[product_offset..product_offset + 2].copy_from_slice(&code.to_le_bytes());
        assert_eq!(
            JobRequest::from_bytes(&bytes),
            None,
            "{}: a request for a moment the payload does not carry was accepted",
            job.kind(),
        );
    }
}

/// The vertical jobs' own malformed shapes.
#[test]
fn a_malformed_vertical_job_is_refused_rather_than_misread() {
    for job in [a_section_job(), a_voxel_job()] {
        let bytes = job.to_bytes();
        for cut in 1..bytes.len() {
            // Truncation anywhere must be a clean refusal.
            assert_eq!(
                JobRequest::from_bytes(&bytes[..cut]),
                None,
                "{} truncated to {cut} bytes was accepted",
                job.kind(),
            );
        }
        // Trailing bytes land inside `RenderInput::from_bytes`, which is
        // exactly why the payload has to be last.
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            JobRequest::from_bytes(&trailing),
            None,
            "{}: trailing bytes mean the layouts disagree",
            job.kind(),
        );
        // A product code this build does not have.
        let mut bad_product = bytes.clone();
        let at = if job.job.downcast_ref::<SectionJob>().is_some() {
            45
        } else {
            46
        };
        bad_product[at] = 0xFE;
        bad_product[at + 1] = 0xFF;
        assert_eq!(JobRequest::from_bytes(&bad_product), None, "product code");
    }

    // The voxel job's `values_wanted` is a bool, not a byte.
    let mut bad_flag = a_voxel_job().to_bytes();
    bad_flag[45] = 2;
    assert_eq!(JobRequest::from_bytes(&bad_flag), None, "values_wanted");

    // And a shape with a zero axis is refused at the boundary rather than deep
    // inside `build_voxels`.
    let bytes = a_voxel_job().to_bytes();
    let shape_at = bytes.len() - a_volume_input().to_bytes().len() - 6;
    for axis in 0..3 {
        let mut zeroed = bytes.clone();
        let at = shape_at + axis * 2;
        zeroed[at] = 0;
        zeroed[at + 1] = 0;
        assert_eq!(
            JobRequest::from_bytes(&zeroed),
            None,
            "a zero axis {axis} was accepted",
        );
    }
    // precondition: the offset arithmetic above really points at the shape, so
    // the assertions are about the guard rather than about corrupting some
    // other field into invalidity.
    let mut same = bytes.clone();
    same[shape_at] = 8;
    same[shape_at + 1] = 0;
    assert_eq!(
        JobRequest::from_bytes(&same),
        Some(a_voxel_job()),
        "the shape is not where this test thinks it is",
    );
}

/// The two vertical arms of [`execute`] actually run, end to end, on a volume
/// with a cut table.
#[test]
fn the_vertical_jobs_produce_their_own_output_kinds() {
    let section = execute(&a_section_job()).expect("the section job draws");
    assert!(section.downcast_ref::<CrossSection>().is_some());

    let voxels = execute(&a_voxel_job()).expect("the voxel job builds");
    let grid = voxels
        .take::<VolumeGrid>()
        .expect("the voxel job answers a grid");
    assert_eq!(grid.dims().cells(), 8 * 6 * 4);

    // And the same jobs off the wire, which is the path a worker takes.
    assert!(
        execute_bytes(&a_section_job().to_bytes())
            .expect("the section job draws via the wire")
            .downcast_ref::<CrossSection>()
            .is_some(),
    );
    assert!(
        execute_bytes(&a_voxel_job().to_bytes())
            .expect("the voxel job builds via the wire")
            .downcast_ref::<VolumeGrid>()
            .is_some(),
    );
}

/// A frame consumer handed an output of another kind sees `None`.
#[test]
fn a_frame_consumer_sees_nothing_rather_than_another_kinds_buffers() {
    let section = execute(&a_section_job()).expect("the section job draws");
    assert!(section.take::<RenderedFrame>().is_none());
    let voxels = execute(&a_voxel_job()).expect("the voxel job builds");
    assert!(voxels.take::<RenderedFrame>().is_none());
    // And the frame job still yields its frame, so the take is not simply
    // always `None`.
    assert!(
        execute(&a_job())
            .and_then(|out| out.take::<RenderedFrame>())
            .is_some_and(|f| !f.image.is_empty()),
    );
    // The vertical takes are equally narrow.
    assert!(
        execute(&a_job())
            .and_then(|out| out.take::<CrossSection>())
            .is_none()
    );
    assert!(
        execute(&a_job())
            .and_then(|out| out.take::<VolumeGrid>())
            .is_none()
    );
    assert!(
        execute(&a_section_job())
            .and_then(|out| out.take::<VolumeGrid>())
            .is_none()
    );
}

/// The reply codecs' cross-kind refusals, on the rows themselves.
#[test]
fn a_reply_payload_of_the_wrong_kind_is_refused_by_the_rows_codec() {
    let section = execute(&a_section_job())
        .and_then(|out| out.take::<CrossSection>())
        .expect("the section job draws");
    let grid = execute(&a_voxel_job())
        .and_then(|out| out.take::<VolumeGrid>())
        .expect("the voxel job builds");

    let row_decode_out = |label: &str| {
        job_codecs()
            .find(|row| row.label == label)
            .expect("the label names a composed row")
            .decode_out
    };
    let section_bytes = section.to_bytes();
    let grid_bytes =
        rustdar_radar::voxel::to_bytes(&grid).expect("a registered field has a wire code");

    // Each row decodes its own payload — the control that the refusals below
    // are about the pairing, not the bytes.
    assert_eq!(
        row_decode_out("section")(&section_bytes, Vec::new())
            .and_then(|out| out.take::<CrossSection>()),
        Some(section),
    );
    assert_eq!(
        row_decode_out("voxels")(&grid_bytes, Vec::new()).and_then(|out| out.take::<VolumeGrid>()),
        Some(grid),
    );

    // The payload codecs each have their own magic, so a decoder handed the
    // wrong kind's bytes is a refusal rather than a reinterpretation.
    assert!(row_decode_out("voxels")(&section_bytes, Vec::new()).is_none());
    assert!(row_decode_out("section")(&grid_bytes, Vec::new()).is_none());
    assert!(row_decode_out("decode")(&section_bytes, Vec::new()).is_none());
    // The frame codec refuses too — a tail-less frame reply is refused at its
    // tail count before a byte is read (WO-M7d).
    assert!(row_decode_out("radar")(&section_bytes, Vec::new()).is_none());
    assert!(row_decode_out("section")(&[], Vec::new()).is_none());
    assert!(row_decode_out("radar")(&[], Vec::new()).is_none());
}

/// The deliver-side half of the reply pairing: the funnel decodes a reply
/// through the row recorded at dispatch and **verifies the reply's kind
/// against that row's code**.
#[test]
fn a_reply_of_the_wrong_kind_is_refused_at_the_deliver() {
    let section_code = 4u8; // section = composed index 3 + 1, pinned by the code table test.
    for (delivered_kind, decodes) in [(section_code, true), (section_code + 1, false), (0, false)] {
        let posted = attach(true);
        let (tx, rx) = mpsc::channel();
        offload_job("test", Job::Described(a_section_job()), move |result| {
            let _ = tx.send(result.and_then(|out| out.take::<CrossSection>()).is_some());
        });
        let id = posted.lock().unwrap()[0].0;
        let bytes = execute(&a_section_job())
            .and_then(|out| out.take::<CrossSection>())
            .expect("the section job draws")
            .to_bytes();
        // The section reply rides the head alone — no tails (WO-M7d).
        deliver_encoded_reply(id, Some((delivered_kind, bytes, Vec::new())));
        assert_eq!(
            rx.try_recv(),
            Ok(decodes),
            "kind {delivered_kind}: the deliver must decode exactly the \
             dispatched row's code and refuse every other as nothing-to-draw",
        );
        detach();
    }

    // Explicit nulls — a job that produced nothing — deliver `None` through
    // the same path, and the entry is still released.
    let posted = attach(true);
    let (tx, rx) = mpsc::channel();
    offload_job("test", Job::Described(a_section_job()), move |result| {
        let _ = tx.send(result.is_some());
    });
    let id = posted.lock().unwrap()[0].0;
    deliver_encoded_reply(id, None);
    assert_eq!(rx.try_recv(), Ok(false), "a null reply still delivers");
    assert_eq!(jobs_in_worker(), 0, "the null reply must release the entry");
    detach();
}

/// **The invariant the render budget depends on: every `deliver` sends on its
/// channel on every arm, including the wrong-kind arm.** A pane takes a render
/// slot and an in-flight mark when it dispatches, and only `deliver` running
/// unwinds them.
#[test]
fn a_job_answered_with_the_wrong_output_kind_still_delivers() {
    for job in [a_section_job(), a_voxel_job()] {
        let kind = job.kind();
        detach();
        let (tx, rx) = mpsc::channel();
        // The consumer is shaped for a frame — the shape both production
        // `offload_job` callers have.
        offload_job("test", Job::Described(job), move |output| {
            let _ = tx.send(output.and_then(|out| out.take::<RenderedFrame>()).is_some());
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)),
            Ok(false),
            "{kind}: a wrong-kind result did not reach deliver, so the \
                 render budget just leaked a slot",
        );
    }

    // The same across the worker boundary, where the reply is what carries the
    // result.
    let posted = attach(true);
    let (tx, rx) = mpsc::channel();
    offload_job("test", Job::Described(a_section_job()), move |output| {
        let _ = tx.send(output.is_some());
    });
    assert_eq!(posted.lock().unwrap().len(), 1);
    abandon_worker("test");
    assert_eq!(
        rx.try_recv(),
        Ok(false),
        "a posted section job the worker never answered was forgotten \
             rather than failed",
    );
    assert_eq!(jobs_in_worker(), 0);
}

#[test]
fn a_malformed_job_is_refused_rather_than_misread() {
    assert_eq!(JobRequest::from_bytes(&[]), None, "empty");
    assert_eq!(JobRequest::from_bytes(&[0xFF, 1, 2]), None, "unknown code");

    // The 44-byte canonical envelope is fixed, so every cut inside it.
    for job in [a_job(), a_level3_job(), a_level3_pair_job()] {
        let bytes = job.to_bytes();
        for cut in 1..45 {
            assert_eq!(
                JobRequest::from_bytes(&bytes[..cut]),
                None,
                "{} cut to {cut} bytes — inside the envelope — was accepted",
                job.kind(),
            );
        }
    }

    // A radar code over a complete envelope and nothing else.
    let envelope_only: Vec<u8> = {
        let mut b = vec![1u8]; // the radar row's code
        b.extend_from_slice(&[0; 44]);
        b
    };
    assert_eq!(JobRequest::from_bytes(&envelope_only), None, "no flag");
    // `values_wanted` is a boolean, and a byte outside `{0, 1}` is a build
    // whose protocol is not this one.
    let mut bad_flag = envelope_only.clone();
    bad_flag.push(2);
    assert_eq!(
        JobRequest::from_bytes(&bad_flag),
        None,
        "values_wanted is a bool, not a byte"
    );
    // A well-formed flag over an empty payload: `RenderInput::from_bytes` has
    // nothing to read and must refuse.
    let mut no_payload = envelope_only.clone();
    no_payload.push(1);
    assert_eq!(JobRequest::from_bytes(&no_payload), None, "no payload");

    // A length prefix that claims more than the payload holds must be refused,
    // not read as a short object.
    let mut overlong = a_level3_pair_job().to_bytes();
    overlong[61] = 0xFF;
    assert_eq!(
        JobRequest::from_bytes(&overlong),
        None,
        "a DVL length past the end of the payload",
    );

    // Bytes 45 and 46: the code and the 44-byte envelope precede the product
    // code.
    let mut bad_product = a_level3_job().to_bytes();
    bad_product[45] = 0xFE;
    bad_product[46] = 0xFF;
    assert_eq!(
        JobRequest::from_bytes(&bad_product),
        None,
        "a product code this build does not have"
    );
}

/// A Level III payload that does not decode is a render that drew nothing, not
/// a panic.
#[test]
fn an_undecodable_level3_payload_renders_nothing() {
    assert!(execute(&a_level3_job()).is_none());
    assert!(
        execute(&a_level3_pair_job()).is_none(),
        "neither object of the pair decodes",
    );
}

/// With no sink installed, `offload_job` falls through to `run_here` and
/// `deliver` still sees the result.
#[test]
fn without_a_sink_the_job_still_runs_and_delivers() {
    detach();
    let (tx, rx) = mpsc::channel();
    offload_job("test", Job::Described(a_job()), move |result| {
        let _ = tx.send(result.is_some());
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(true),
        "the fallthrough arm must deliver the rendered frame"
    );
    assert_eq!(jobs_in_worker(), 0);
}

/// **The convergence, asserted where it is visible**.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_native_thread_starts_with_the_pool_as_its_sink() {
    assert!(
        worker_attached(),
        "a native thread must reach the job pool through the same trait the \
         browser's worker is installed behind",
    );
    let (tx, rx) = mpsc::channel();
    offload_job("test", Job::Described(a_job()), move |result| {
        // The pool answers on a pool thread, so this send is what proves the
        // job crossed the transport and came back.
        let _ = tx.send(result.is_some());
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(30)),
        Ok(true),
        "the pool must run the job and deliver its frame",
    );
    assert_eq!(
        jobs_in_worker(),
        0,
        "a delivered job must be out of the registry, or its render slot leaks",
    );
}

/// With a worker, nothing runs here — the job is posted and `deliver` waits
/// for the reply.
#[test]
fn with_a_worker_the_job_is_posted_and_deferred() {
    let posted = attach(true);
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    offload_job("test", Job::Described(a_job()), move |_| {
        flag.store(true, Ordering::Relaxed)
    });

    assert_eq!(posted.lock().unwrap().len(), 1, "the job should be posted");
    assert_eq!(jobs_in_worker(), 1);
    assert!(
        !ran.load(Ordering::Relaxed),
        "deliver must wait for a reply"
    );

    let id = posted.lock().unwrap()[0].0;
    deliver_job_reply(id, None);
    assert!(ran.load(Ordering::Relaxed), "the reply must reach deliver");
    assert_eq!(jobs_in_worker(), 0, "the pending entry must be retired");
    detach();
}

/// The cancellation contract, across the worker boundary.
#[test]
fn a_reply_to_an_abandoned_render_is_not_delivered() {
    let posted = attach(true);
    let wanted = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::channel();
    let flag = Arc::clone(&wanted);
    offload_job("test", Job::Described(a_job()), move |result| {
        if result.is_some() && flag.load(Ordering::Relaxed) {
            let _ = tx.send(());
        }
    });

    // Two references while the job is outstanding: the pane's list and the one
    // inside `deliver`.
    assert_eq!(Arc::strong_count(&wanted), 2);

    wanted.store(false, Ordering::Relaxed);
    let id = posted.lock().unwrap()[0].0;
    deliver_job_reply(
        id,
        Some(DescribedOut(Box::new(RenderedFrame {
            image: vec![0; 4],
            max_range_km: 230.0,
            polar: Default::default(),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        }))),
    );

    assert!(rx.try_recv().is_err(), "an abandoned render must not send");
    assert_eq!(
        Arc::strong_count(&wanted),
        1,
        "retiring the job must drop deliver's reference, or want_result never prunes"
    );
    detach();
}

/// A worker that dies owes replies that will never come.
#[test]
fn losing_the_worker_fails_every_job_it_owed() {
    attach(true);
    let (tx, rx) = mpsc::channel();
    offload_job("test", Job::Described(a_job()), move |result| {
        let _ = tx.send(result.is_some());
    });
    assert_eq!(jobs_in_worker(), 1);

    abandon_worker("test");
    assert_eq!(rx.try_recv(), Ok(false), "the owed job must be failed");
    assert_eq!(jobs_in_worker(), 0);
    assert!(!worker_attached());
}

/// A port that will not take the job must not strand it.
#[test]
fn a_refused_post_runs_the_job_here() {
    detach();
    attach(false);
    let (tx, rx) = mpsc::channel();
    offload_job("test", Job::Described(a_job()), move |result| {
        let _ = tx.send(result.is_some());
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(true),
        "a refused post must fall back to running here"
    );
    assert_eq!(jobs_in_worker(), 0);
    detach();
}

/// A reply nobody is waiting for — the job was already failed by
/// `abandon_worker`.
#[test]
fn a_reply_for_a_retired_job_is_ignored() {
    let posted = attach(true);
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    offload_job("test", Job::Described(a_job()), move |_| {
        seen.fetch_add(1, Ordering::Relaxed);
    });
    let id = posted.lock().unwrap()[0].0;

    abandon_worker("test");
    assert_eq!(count.load(Ordering::Relaxed), 1);
    deliver_job_reply(id, None);
    assert_eq!(
        count.load(Ordering::Relaxed),
        1,
        "a late reply must not deliver a second response for one render"
    );
}

// ── The decode job ──────────────────────────────────────────────────────────

/// A `Decode` request whose archive is bytes no decoder will accept.
fn a_decode_job() -> JobRequest {
    JobRequest::describe(
        DecodeJob {
            archive: std::sync::Arc::new(b"AR2V0006.001not-a-real-volume".to_vec()),
        },
        ceiling_only_geometry(0),
    )
}

#[test]
fn a_decode_job_round_trips_its_archive_whole() {
    let job = a_decode_job();
    let back = JobRequest::from_bytes(&job.to_bytes()).expect("this build wrote it");
    assert_eq!(back, job);
}

/// The code space is shared with twelve other kinds, and a decode's payload is
/// arbitrary bytes.
#[test]
fn a_decode_job_is_not_readable_as_another_kind() {
    let mut bytes = a_decode_job().to_bytes();
    for code in (1u8..=13).filter(|&code| code != 6) {
        bytes[0] = code;
        // Whatever it decodes to, it must not decode to a `DecodeJob`.
        assert!(
            !JobRequest::from_bytes(&bytes)
                .is_some_and(|job| job.job.downcast_ref::<DecodeJob>().is_some()),
            "code {code} produced a decode job"
        );
    }
}

/// An archive this build cannot read is "nothing", which is what a failed
/// render has always answered.
#[test]
fn an_archive_that_does_not_decode_produces_nothing() {
    assert!(execute(&a_decode_job()).is_none());
    assert!(execute_bytes(&a_decode_job().to_bytes()).is_none());
}

/// The reply half: a decoded volume comes back through the decode row's own
/// reply codec, and through nobody else's.
#[test]
fn a_decoded_volume_comes_back_under_its_own_out_kind() {
    // An empty volume: this test is about the envelope, and the payload codec
    // has its own round-trip row in `rustdar-radar`.
    let pattern = nexrad_model::data::VolumeCoveragePattern::new(
        212,
        0,
        0.5,
        nexrad_model::data::PulseWidth::Unknown,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        Vec::new(),
    );
    let volume = rustdar_radar::scan::DecodedScan {
        scan: nexrad_model::data::Scan::new(pattern, Vec::new()),
        declared_nyquist: rustdar_radar::nyquist::DeclaredNyquist::empty(),
    };
    let bytes = volume.to_bytes();

    let row_decode_out = |label: &str| {
        job_codecs()
            .find(|row| row.label == label)
            .expect("the label names a composed row")
            .decode_out
    };
    let back = row_decode_out("decode")(&bytes, Vec::new())
        .expect("a volume payload under the decode row")
        .take::<DecodedScan>()
        .expect("it is a volume");
    assert_eq!(back, volume);

    // The same bytes under the other kinds' decoders are refused by those
    // types' own magic rather than half-decoded into something plausible.
    for label in ["section", "voxels"] {
        assert!(
            row_decode_out(label)(&bytes, Vec::new()).is_none(),
            "the {label} row accepted a volume payload"
        );
    }
}

// ── The job framing's layout ────────────────────────────────────────────────

/// FNV-1a 64 over a payload, for
/// [`the_job_framing_is_the_one_this_protocol_ships`].
fn layout_digest(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The part of a request's bytes this file owns: everything before the nested
/// [`RenderInput`], which has a pin of its own.
fn framing_of(request: &JobRequest) -> Vec<u8> {
    let bytes = request.to_bytes();
    let nested = match row_for(&request.job).label {
        "radar" => request
            .job
            .downcast_ref::<RadarPlanJob>()
            .expect("the radar row owns RadarPlanJob")
            .input
            .to_bytes()
            .len(),
        "section" => request
            .job
            .downcast_ref::<SectionJob>()
            .expect("the section row owns SectionJob")
            .input
            .to_bytes()
            .len(),
        "voxels" => request
            .job
            .downcast_ref::<VoxelJob>()
            .expect("the voxels row owns VoxelJob")
            .input
            .to_bytes()
            .len(),
        // Opaque payloads: an archive or a Level III object, which this codec
        // frames and never interprets.
        "level3" | "level3/vild" | "decode" => 0,
        // Every byte is an overlay row's own: the geometry header and the
        // row's fields are all this wire's framing, with no nested payload
        // carrying a pin of its own.
        label if label.starts_with("overlay/") => 0,
        other => panic!(
            "framing_of has no nested-payload ruling for the {other:?} row: \
             decide here whether its payload nests a layout with a pin of \
             its own",
        ),
    };
    bytes[..bytes.len() - nested].to_vec()
}

/// The framing this build ships is **this** framing.
#[test]
fn the_job_framing_is_the_one_this_protocol_ships() {
    let requests = [
        a_job(),
        a_level3_job(),
        a_level3_pair_job(),
        a_section_job(),
        a_voxel_job(),
        a_sourceless_voxel_job(),
        a_decode_job(),
        an_overlay_sites_job(),
        an_overlay_alerts_job(),
        an_overlay_outlooks_job(),
        an_overlay_discussions_job(),
        an_overlay_reports_job(),
        an_overlay_glm_job(),
        // The window form: literal constants and literal values, so the digest
        // is over stored bits and not over anything a platform's libm
        // computed.
        an_overlay_model_job(),
    ];
    let rows: Vec<String> = requests
        .iter()
        .map(|request| {
            let framing = framing_of(request);
            format!(
                "{} | {} | {:#018x}",
                request.kind(),
                framing.len(),
                layout_digest(&framing),
            )
        })
        .collect();

    assert_eq!(
        rows,
        crate::wire_identity::WIRE_FRAMING_ROWS,
        "the framing `JobRequest::to_bytes` writes is not the framing \
         `wire_identity::WIRE_FRAMING_ROWS` pins. Left is what this build \
         posts; right is what that list was last told. Something about a \
         request's layout moved — a field added, removed, reordered, retyped, \
         or written at a different width, or the registry recomposed. If the \
         change was deliberate, re-pin the row in `wire_identity.rs`: the \
         governing number is the M5 build token, which the row feeds — two \
         local builds with different rows refuse each other and respawn, and \
         in CI the `GITHUB_SHA` does the same for every change. These rows \
         are within-build native->web parity pins and refactor gates, not \
         cross-version contracts — the wire is same-build-only by \
         construction, and a mispaired job would render the wrong region, or \
         the wrong product, or fail to decode and strand the pane that \
         posted it; the token is what keeps that pair from ever exchanging \
         bytes."
    );
}

// ── The overlay job ─────────────────────────────────────────────────────────

/// **The pairing gate between the registry and the parity suite** (m6).
#[test]
fn every_codec_row_has_a_parity_test() {
    // Deliberately spelled out, label beside test name, in registry order.
    let named: [(&str, &str); 13] = [
        (
            "radar",
            "the_radar_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "level3",
            "the_level3_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "level3/vild",
            "the_vild_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "section",
            "the_section_cut_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "voxels",
            "the_voxel_build_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "decode",
            "the_archive_decode_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "overlay/sites",
            "the_sites_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "overlay/alerts",
            "the_alerts_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "overlay/outlooks",
            "the_outlooks_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "overlay/discussions",
            "the_discussions_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "overlay/reports",
            "the_reports_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "overlay/glm",
            "the_glm_render_is_byte_identical_direct_and_via_the_wire",
        ),
        (
            "overlay/model",
            "the_model_render_is_byte_identical_direct_and_via_the_wire",
        ),
    ];
    assert_eq!(
        job_codecs().count(),
        named.len(),
        "the registry has {} rows where this list names {} parity tests: a \
         row without a via-wire-vs-direct parity test is a codec whose \
         byte-identity nothing proves — write the test, then add its name \
         here",
        job_codecs().count(),
        named.len(),
    );
    let labels: Vec<&str> = job_codecs().map(|row| row.label).collect();
    let expected: Vec<&str> = named.iter().map(|(label, _)| *label).collect();
    assert_eq!(
        labels, expected,
        "the registry's rows and the parity list disagree about which kinds \
         exist or their order",
    );
    // The names are real: each must appear as a test fn in this very file, so
    // the pin cannot drift into naming tests that no longer exist.
    let source = include_str!("tests.rs");
    for (label, test_name) in named {
        assert!(
            source.contains(&format!("fn {test_name}(")),
            "{label}'s parity test `{test_name}` is not defined in \
             offload/tests.rs; the row count above is only as good as the \
             names being live tests",
        );
    }
}

/// **No `run` body reads a clock** — the grep ratchet over BOTH jobs modules
/// (WO-M7.2, the pwa_assets source-scrape shape).
#[test]
fn no_run_body_reads_a_clock() {
    // `include_str!` rather than a runtime read: the compiler re-runs this
    // test's crate when either module changes, so the ratchet cannot go stale
    // against the file it scans.
    let modules: [(&str, &str); 2] = [
        (
            "rustdar-radar/src/jobs.rs",
            include_str!("../../../rustdar-radar/src/jobs.rs"),
        ),
        (
            "rustdar-overlays/src/render/jobs.rs",
            include_str!("../../../rustdar-overlays/src/render/jobs.rs"),
        ),
    ];
    for (name, source) in modules {
        // Presence controls first, so the scan below cannot be green over the
        // wrong (or an empty) file.
        assert!(
            source.contains("JOB_CODECS") && source.contains("fn run"),
            "{name} no longer holds codec rows with run bodies; this ratchet \
             is scanning the wrong file",
        );
        let code: String = source
            .lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        for clock in ["Utc::now", "Local::now", "Instant::now"] {
            assert!(
                !code.contains(clock),
                "{name} reads {clock}(): a worker running this row would \
                 render a picture the direct call would not. Capture the \
                 moment at the dispatch site and carry it on the input, the \
                 way the GLM row's `now` travels.",
            );
        }
    }
}

/// The parity gate for the radar (Level II plan-view) row.
#[test]
fn the_radar_render_is_byte_identical_direct_and_via_the_wire() {
    let job = a_job();
    let bytes = job.to_bytes();
    assert_eq!(
        JobRequest::from_bytes(&bytes).as_ref(),
        Some(&job),
        "the radar job does not survive its own wire form",
    );

    let direct = execute(&job)
        .and_then(|out| out.take::<RenderedFrame>())
        .expect("the fixture sweep renders");
    assert!(
        !direct.image.is_empty() && painted(&direct.image) > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );

    let via_wire = execute_bytes(&bytes)
        .and_then(|out| out.take::<RenderedFrame>())
        .expect("the described radar job renders off its own wire form");
    assert_eq!(
        via_wire, direct,
        "the radar frame differs between the direct call and the wire — the \
         two paths have stopped being one renderer",
    );
}

/// **The parity gate for the Level III row**, on the shape the fixture set
/// affords.
#[test]
fn the_level3_render_is_byte_identical_direct_and_via_the_wire() {
    let job = a_level3_job();
    let bytes = job.to_bytes();
    assert_eq!(
        JobRequest::from_bytes(&bytes).as_ref(),
        Some(&job),
        "the level3 job does not survive its own wire form",
    );
    // Refusal parity, arm by arm: the fixture's payload does not decode, and
    // both paths must say so. An erased output carries no equality, so the
    // parity IS the pair of refusals.
    assert!(execute(&job).is_none(), "the opaque fixture must refuse");
    assert!(
        execute_bytes(&bytes).is_none(),
        "the two arms answered differently for one level3 job",
    );
}

/// **The parity gate for the VIL-density (`level3/vild`) row**, on
/// [`the_level3_render_is_byte_identical_direct_and_via_the_wire`]'s exact
/// terms.
#[test]
fn the_vild_render_is_byte_identical_direct_and_via_the_wire() {
    let job = a_level3_pair_job();
    let bytes = job.to_bytes();
    assert_eq!(
        JobRequest::from_bytes(&bytes).as_ref(),
        Some(&job),
        "the level3/vild job does not survive its own wire form",
    );
    // Refusal parity, arm by arm — see the level3 gate for why the erased
    // reply makes the pair of refusals the comparison.
    assert!(execute(&job).is_none(), "neither opaque object decodes");
    assert!(
        execute_bytes(&bytes).is_none(),
        "the two arms answered differently for one level3/vild job",
    );
}

/// **The parity gate for the section row: direct call and via-wire execution
/// cut the identical section.** The comparison is the whole `CrossSection`.
#[test]
fn the_section_cut_is_byte_identical_direct_and_via_the_wire() {
    let job = a_section_job();
    let bytes = job.to_bytes();
    assert_eq!(
        JobRequest::from_bytes(&bytes).as_ref(),
        Some(&job),
        "the section job does not survive its own wire form",
    );

    let direct = execute(&job)
        .and_then(|out| out.take::<CrossSection>())
        .expect("the fixture volume cuts");
    assert!(
        painted(direct.image()) > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );

    let via_wire = execute_bytes(&bytes)
        .and_then(|out| out.take::<CrossSection>())
        .expect("the described section job cuts off its own wire form");
    assert_eq!(
        via_wire, direct,
        "the section differs between the direct call and the wire — the two \
         paths have stopped being one renderer",
    );
}

/// **The parity gate for the voxel row: direct call and via-wire execution
/// build the identical grid**, indices and shape alike, on the fixture volume
/// both arms genuinely resample.
#[test]
fn the_voxel_build_is_byte_identical_direct_and_via_the_wire() {
    let job = a_voxel_job();
    let bytes = job.to_bytes();
    assert_eq!(
        JobRequest::from_bytes(&bytes).as_ref(),
        Some(&job),
        "the voxel job does not survive its own wire form",
    );

    let direct = execute(&job)
        .and_then(|out| out.take::<VolumeGrid>())
        .expect("the fixture volume builds a grid");
    assert_eq!(
        direct.dims().cells(),
        8 * 6 * 4,
        "the fixture's grid is not the asked-for shape, so the comparison \
         below is not about the build this test believes it is",
    );

    let via_wire = execute_bytes(&bytes)
        .and_then(|out| out.take::<VolumeGrid>())
        .expect("the described voxel job builds off its own wire form");
    assert_eq!(
        via_wire, direct,
        "the voxel grid differs between the direct call and the wire — the \
         two paths have stopped being one builder",
    );
}

/// **The parity gate for the archive-decode row**, on the Level III gates'
/// terms.
#[test]
fn the_archive_decode_is_byte_identical_direct_and_via_the_wire() {
    let job = a_decode_job();
    let bytes = job.to_bytes();
    assert_eq!(
        JobRequest::from_bytes(&bytes).as_ref(),
        Some(&job),
        "the decode job does not survive its own wire form",
    );
    // Refusal parity, arm by arm: the fixture archive does not decode, and
    // both paths must say so. The erased reply carries no equality.
    assert!(execute(&job).is_none(), "the fixture archive must refuse");
    assert!(
        execute_bytes(&bytes).is_none(),
        "the two arms answered differently for one decode job",
    );
}

/// **The parity gate for the sites render: direct call and via-wire execution
/// are byte-identical.** This is what makes describing the job a move of the
/// same work rather than a second implementation of it.
#[test]
fn the_sites_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest { geometry, job } = an_overlay_sites_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let sites = job
        .downcast_ref::<rustdar_overlays::render::rasterize::SitesInput>()
        .expect("the fixture is a sites job");

    let direct =
        rustdar_overlays::render::rasterize::rasterize_radar_sites(sites, &bounds, width, height);
    // The premise the via-wire contract ("always premultiplied") rides on for
    // this kind.
    assert_eq!(
        direct.alpha,
        rustdar_overlays::render::rasterize::AlphaMode::Premultiplied,
        "the sites rasterizer changed its alpha convention; the parity claim \
         below is now comparing across a conversion",
    );

    let painted = direct.rgba.chunks_exact(4).filter(|px| px[3] != 0).count();
    assert!(
        painted > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );

    let RasterizeOutput {
        rgba: via_wire,
        hit_cells,
        ..
    } = execute_bytes(
        &JobRequest {
            geometry,
            job: job.clone(),
        }
        .to_bytes(),
    )
    .and_then(|out| out.take::<RasterizeOutput>())
    .expect("the described sites job rasterizes");

    assert_eq!(
        via_wire.len(),
        (width * height * 4) as usize,
        "the reply's length is the one statement of shape the consumer checks",
    );
    assert_eq!(
        via_wire, direct.rgba,
        "the sites raster differs between the direct call and the wire — the \
         two paths have stopped being one renderer",
    );
    assert_eq!(
        hit_cells, None,
        "the sites render answered hit cells; it resolves no clicks by pixel, \
         and a stray Some here would be refused at the deliver as a mismatch",
    );
}

/// Painted pixels — the non-vacuity floor every parity test below stands on.
fn painted(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|px| px[3] != 0).count()
}

/// [`execute_bytes`] on an overlay job's own wire form, down to the raster.
fn overlay_raster_via_wire(job: &JobRequest) -> Vec<u8> {
    overlay_reply_via_wire(job).0
}

/// [`execute_bytes`] on an overlay job's own wire form.
fn overlay_reply_via_wire(
    job: &JobRequest,
) -> (
    Vec<u8>,
    Option<rustdar_overlays::render::rasterize::HitCells>,
) {
    let RasterizeOutput {
        rgba, hit_cells, ..
    } = execute_bytes(&job.to_bytes())
        .and_then(|out| out.take::<RasterizeOutput>())
        .expect("the described overlay job rasterizes");
    (rgba, hit_cells)
}

/// **The parity gate for the alert render**, the sites gate's shape on the
/// kind whose inline rasterization was a 224 ms gesture-end stall against a
/// 289.5 ms p50 gesture frame — measured in-browser on `main@ebe0ad3b`
/// (2026-08-12 web-baseline campaign), before this module's slices moved that
/// raster off the wasm frame thread.
#[test]
fn the_alerts_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest { geometry, job } = an_overlay_alerts_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let alerts = job
        .downcast_ref::<rustdar_overlays::render::rasterize::AlertsInput>()
        .expect("the fixture is an alerts job");

    let direct =
        rustdar_overlays::render::rasterize::rasterize_nws_alerts(alerts, &bounds, width, height);
    assert_eq!(
        direct.alpha,
        rustdar_overlays::render::rasterize::AlphaMode::Premultiplied,
        "the alert rasterizer changed its alpha convention; the parity claim \
         below is now comparing across a conversion",
    );
    assert!(
        painted(&direct.rgba) > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );

    let via_wire = overlay_raster_via_wire(&JobRequest {
        geometry,
        job: job.clone(),
    });
    assert_eq!(
        via_wire.len(),
        (width * height * 4) as usize,
        "the reply's length is the one statement of shape the consumer checks",
    );
    assert_eq!(
        via_wire, direct.rgba,
        "the alert raster differs between the direct call and the wire — the \
         two paths have stopped being one renderer",
    );

    // The hidden-id set is a **live** input through the wire, not a field a
    // broken codec could zero without anyone noticing.
    let unhidden = rustdar_overlays::render::rasterize::AlertsInput {
        hidden_ids: std::collections::HashSet::new(),
        ..alerts.clone()
    };
    let more = overlay_raster_via_wire(&JobRequest {
        geometry,
        job: DescribedJob::new(unhidden),
    });
    assert!(
        painted(&more) > painted(&via_wire),
        "un-hiding an alert did not add pixels through the wire, so the \
         hidden-id set is not reaching the rasterizer and the parity above \
         says nothing about it",
    );
}

/// **The parity gate for the outlook render** — the kind with the pass the
/// others do not have.
#[test]
fn the_outlooks_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest { geometry, job } = an_overlay_outlooks_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let outlooks = job
        .downcast_ref::<rustdar_overlays::render::rasterize::OutlooksInput>()
        .expect("the fixture is an outlooks job");

    let direct = rustdar_overlays::render::rasterize::rasterize_spc_outlooks(
        outlooks, &bounds, width, height,
    );
    assert_eq!(
        direct.alpha,
        rustdar_overlays::render::rasterize::AlphaMode::Premultiplied,
        "the outlook rasterizer changed its alpha convention; the parity \
         claim below is now comparing across a conversion",
    );
    assert!(
        painted(&direct.rgba) > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );
    // The fixture's hatch colour is pure blue at full alpha, which no fill in
    // it approaches.
    let hatch_ink = direct
        .rgba
        .chunks_exact(4)
        .filter(|p| p[2] > 200 && p[0] < 60 && p[1] < 60)
        .count();
    assert!(
        hatch_ink > 20,
        "the direct outlook raster has {hatch_ink} hatch-coloured pixels; \
         the hatch pass is not running on this fixture, so parity would say \
         nothing about the hatch inputs travelling",
    );

    let via_wire = overlay_raster_via_wire(&JobRequest {
        geometry,
        job: job.clone(),
    });
    assert_eq!(
        via_wire.len(),
        (width * height * 4) as usize,
        "the reply's length is the one statement of shape the consumer checks",
    );
    assert_eq!(
        via_wire, direct.rgba,
        "the outlook raster differs between the direct call and the wire — \
         the two paths have stopped being one renderer",
    );
}

/// **The parity gate for the discussion render.** Byte-identity, the painted
/// floor, and a proof that *every row* travels.
#[test]
fn the_discussions_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest { geometry, job } = an_overlay_discussions_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let discussions = job
        .downcast_ref::<rustdar_overlays::render::rasterize::DiscussionsInput>()
        .expect("the fixture is a discussions job");

    let direct = rustdar_overlays::render::rasterize::rasterize_spc_discussions(
        discussions,
        &bounds,
        width,
        height,
    );
    assert_eq!(
        direct.alpha,
        rustdar_overlays::render::rasterize::AlphaMode::Premultiplied,
        "the discussion rasterizer changed its alpha convention; the parity \
         claim below is now comparing across a conversion",
    );
    assert!(
        painted(&direct.rgba) > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );

    let via_wire = overlay_raster_via_wire(&JobRequest {
        geometry,
        job: job.clone(),
    });
    assert_eq!(
        via_wire.len(),
        (width * height * 4) as usize,
        "the reply's length is the one statement of shape the consumer checks",
    );
    assert_eq!(
        via_wire, direct.rgba,
        "the discussion raster differs between the direct call and the wire \
         — the two paths have stopped being one renderer",
    );

    // Every row travels: the winter-weather MD sits apart from the convective
    // one, so a wire that lost the second row loses its pixels.
    let mut first_only = discussions.clone();
    first_only.discussions.truncate(1);
    let fewer = overlay_raster_via_wire(&JobRequest {
        geometry,
        job: DescribedJob::new(first_only),
    });
    assert!(
        painted(&fewer) < painted(&via_wire),
        "dropping the second MD did not lose pixels through the wire, so the \
         row list is not reaching the rasterizer whole and the parity above \
         says nothing about it",
    );
}

// ── The hit-map kinds ───────────────────────────────────────────────────────

/// Every item index any cell of `cells` records.
fn ids_of(cells: &rustdar_overlays::render::rasterize::HitCells) -> HashSet<u32> {
    cells.cells.values().flatten().copied().collect()
}

/// The UV at the centre of cell `idx`, in the coordinates `HitMap::hit_test`
/// takes.
fn uv_of_cell(cells: &rustdar_overlays::render::rasterize::HitCells, idx: u32) -> (f32, f32) {
    let qx = idx % cells.width;
    let qy = idx / cells.width;
    (
        (qx as f32 + 0.5) / cells.width as f32,
        (qy as f32 + 0.5) / cells.height as f32,
    )
}

/// **The parity gate for the storm-reports render**.
#[test]
fn the_reports_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest { geometry, job } = an_overlay_reports_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let reports = job
        .downcast_ref::<rustdar_overlays::render::rasterize::ReportsInput>()
        .expect("the fixture is a reports job");

    let direct = rustdar_overlays::render::rasterize::rasterize_storm_reports(
        reports, &bounds, width, height,
    );
    assert_eq!(
        direct.alpha,
        rustdar_overlays::render::rasterize::AlphaMode::Premultiplied,
        "the reports rasterizer changed its alpha convention; the parity \
         claim below is now comparing across a conversion",
    );
    assert!(
        painted(&direct.rgba) > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );

    let (via_wire, wire_cells) = overlay_reply_via_wire(&JobRequest {
        geometry,
        job: job.clone(),
    });
    assert_eq!(
        via_wire.len(),
        (width * height * 4) as usize,
        "the reply's length is the one statement of shape the consumer checks",
    );
    assert_eq!(
        via_wire, direct.rgba,
        "the reports raster differs between the direct call and the wire — \
         the two paths have stopped being one renderer",
    );
    let wire_cells = wire_cells.expect("a hit-map kind answers cells over the wire");
    assert_eq!(
        Some(&wire_cells),
        direct.hit_cells.as_ref(),
        "the hit cells differ between the direct call and the wire — same \
         picture, different hover targets",
    );
    assert_eq!(
        ids_of(&wire_cells),
        HashSet::from([0, 1, 2]),
        "the three in-box reports must each record cells and the culled \
         fourth must not: the id space is the row order the dispatch \
         captured its items in",
    );

    // The kind byte is a live input through the wire, not a field a broken
    // codec could zero.
    let mut rekinded = reports.clone();
    rekinded.reports[0].kind = rustdar_overlays::spc::reports::StormReportKind::Hail;
    let repainted = overlay_raster_via_wire(&JobRequest {
        geometry,
        job: DescribedJob::new(rekinded),
    });
    assert_ne!(
        repainted, via_wire,
        "recolouring a report through the wire changed nothing, so the kind \
         byte is not reaching the rasterizer and the parity above says \
         nothing about it",
    );
}

/// **The parity gate for the GLM render**, on the fixture whose flash ages
/// straddle every boundary the clock decides.
#[test]
fn the_glm_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest { geometry, job } = an_overlay_glm_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let glm = job
        .downcast_ref::<rustdar_overlays::render::rasterize::GlmStrikesInput>()
        .expect("the fixture is a GLM job");

    let direct =
        rustdar_overlays::render::rasterize::rasterize_glm_strikes(glm, &bounds, width, height);
    assert_eq!(
        direct.alpha,
        rustdar_overlays::render::rasterize::AlphaMode::Premultiplied,
        "the GLM rasterizer changed its alpha convention; the parity claim \
         below is now comparing across a conversion",
    );
    assert!(
        painted(&direct.rgba) > 0,
        "the fixture painted nothing, so byte-identity would be vacuous",
    );

    let (via_wire, wire_cells) = overlay_reply_via_wire(&JobRequest {
        geometry,
        job: job.clone(),
    });
    assert_eq!(
        via_wire.len(),
        (width * height * 4) as usize,
        "the reply's length is the one statement of shape the consumer checks",
    );
    assert_eq!(
        via_wire, direct.rgba,
        "the GLM raster differs between the direct call and the wire — the \
         two paths have stopped being one renderer",
    );
    let wire_cells = wire_cells.expect("a hit-map kind answers cells over the wire");
    assert_eq!(
        Some(&wire_cells),
        direct.hit_cells.as_ref(),
        "the hit cells differ between the direct call and the wire — same \
         picture, different hover targets",
    );
    assert_eq!(
        ids_of(&wire_cells),
        HashSet::from([0, 1, 2, 3]),
        "the four in-window flashes must each record cells and the \
         past-window fifth must not: the cull ran against the clock the wire \
         carried",
    );
}

/// **The negative control the `now` capture stands on.** Hand the same
/// described GLM job to a renderer whose clock has moved 60 s.
#[test]
fn a_worker_that_re_read_its_own_clock_would_fail_the_glm_parity() {
    let JobRequest { geometry, job } = an_overlay_glm_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let glm = job
        .downcast_ref::<rustdar_overlays::render::rasterize::GlmStrikesInput>()
        .expect("the fixture is a GLM job");

    let honest =
        rustdar_overlays::render::rasterize::rasterize_glm_strikes(glm, &bounds, width, height);

    let rederived = rustdar_overlays::render::rasterize::GlmStrikesInput {
        now: glm.now + chrono::Duration::seconds(60),
        ..glm.clone()
    };
    let drifted = rustdar_overlays::render::rasterize::rasterize_glm_strikes(
        &rederived, &bounds, width, height,
    );

    assert_ne!(
        drifted.rgba, honest.rgba,
        "a 60 s clock re-read changed nothing, so the parity gate could not \
         catch a worker that re-derived `now` — the fixture has stopped \
         straddling the fade steps",
    );
    assert!(
        painted(&drifted.rgba) < painted(&honest.rgba),
        "the edge flash (570 s of 600) must fall out of the window under the \
         re-read clock — the fixture has stopped straddling the cull boundary",
    );
    let honest_ids = ids_of(honest.hit_cells.as_ref().expect("cells"));
    let drifted_ids = ids_of(drifted.hit_cells.as_ref().expect("cells"));
    assert!(
        honest_ids.contains(&3) && !drifted_ids.contains(&3),
        "the culled flash must leave the hit space too, or a hover could \
         name a flash the re-read clock no longer draws \
         (honest {honest_ids:?}, drifted {drifted_ids:?})",
    );
}

// ── The hit-map zip: cells from the wire, items from the dispatch ───────────

/// A live registry seeded with three storm reports through the production
/// ingest path, so `prepare_job` and `hit_items` are the real handler's own
/// answers.
fn a_seeded_reports_registry() -> rustdar_overlays::render::overlay_state::OverlayRegistry {
    use rustdar_overlays::render::handlers::reports::StormReportsFetchResult;
    use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayRegistry};
    use rustdar_overlays::spc::reports::{StormReport, StormReportKind, StormReportRound};
    let report = |kind, lat, lon| StormReport {
        kind,
        time: "2015".into(),
        magnitude: None,
        location: "NORMAN".into(),
        county: "CLEVELAND".into(),
        state: "OK".into(),
        lat,
        lon,
        comments: String::new(),
    };
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(
        &rustdar_source::id::known::STORM_REPORTS,
        true,
        &mut PaneMut::bare(0),
    );
    registry.apply_fetch_result(
        OverlayFetchResult {
            kind: rustdar_source::id::known::STORM_REPORTS,
            data: Box::new(StormReportsFetchResult(Ok(StormReportRound {
                reports: vec![
                    report(StormReportKind::Tornado, 35.33, -97.28),
                    report(StormReportKind::Hail, 36.2, -98.5),
                    report(StormReportKind::Wind, 33.8, -96.7),
                ],
                failed_kinds: Vec::new(),
            }))),
        },
        &PaneRef::bare(0),
    );
    registry
}

/// A live registry seeded with GLM flashes whose ages sit inside the handler's
/// default 300 s window relative to [`glm_fixture_now`].
fn a_seeded_glm_registry() -> rustdar_overlays::render::overlay_state::OverlayRegistry {
    use rustdar_overlays::glm::{
        GlmDataLevel, GlmFetchOutcome, GlmFetchResult, GlmFlash, GlmSatellite, RecordDrops,
    };
    use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayRegistry};
    let now = glm_fixture_now();
    let flash = |age_secs: i64, lat: f64, lon: f64| GlmFlash {
        lat,
        lon,
        energy: Some(1e-14),
        area: None,
        time: now - chrono::Duration::seconds(age_secs),
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    };
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(
        &rustdar_source::id::known::LIGHTNING,
        true,
        &mut PaneMut::bare(0),
    );
    registry.apply_fetch_result(
        OverlayFetchResult {
            kind: rustdar_source::id::known::LIGHTNING,
            data: Box::new(GlmFetchResult(Ok(GlmFetchOutcome {
                flashes: vec![
                    flash(30, 35.3, -97.3),
                    flash(130, 36.2, -98.5),
                    flash(230, 33.8, -96.7),
                ],
                dead_feeds: Vec::new(),
                queried: Vec::new(),
                parse_failures: None,
                transport_failures: None,
                level_failures: Vec::new(),
                evaluated_levels: Vec::new(),
                listing_failures: Vec::new(),
                window_gaps: Vec::new(),
                record_drops: RecordDrops::default(),
            }))),
        },
        &PaneRef::bare(0),
    );
    registry
}

/// The dispatch-moment context the zip fixtures share.
fn a_zip_ctx() -> rustdar_overlays::render::overlay_state::RasterizeContext {
    rustdar_overlays::render::overlay_state::RasterizeContext {
        is_dark: false,
        zoom: 6.5,
        device_scale: 1.0,
        // Live posture: the depicted instant IS the clock.
        now: glm_fixture_now(),
        as_of: glm_fixture_now(),
    }
}

/// **The hit-map parity gate, per kind**: the cells that came back over the
/// wire, zipped with the id_map the dispatch captured, answer every probe on a
/// full quarter-cell grid with the items the direct call's zip answers.
#[test]
fn the_hit_map_zip_answers_the_direct_calls_hits_on_a_probe_grid() {
    use rustdar_overlays::render::rasterize::HitMap;
    use rustdar_source::id::known;
    let bounds = rustdar_geo::GeoBounds {
        min_lat: 33.0,
        max_lat: 37.0,
        min_lon: -99.0,
        max_lon: -96.0,
    };
    let (width, height) = (96u32, 64u32);
    let ctx = a_zip_ctx();

    for (registry, kind) in [
        (a_seeded_reports_registry(), known::STORM_REPORTS),
        (a_seeded_glm_registry(), known::LIGHTNING),
    ] {
        let job = registry
            .prepare_job(&kind, &ctx, &PaneRef::bare(0))
            .expect("the seeded registry describes a job");
        let items = registry
            .hit_items(&kind)
            .expect("a hit-map kind captures items beside its input");
        let direct = if let Some(input) =
            job.downcast_ref::<rustdar_overlays::render::rasterize::ReportsInput>()
        {
            assert_eq!(items.len(), input.reports.len(), "one item per row");
            rustdar_overlays::render::rasterize::rasterize_storm_reports(
                input, &bounds, width, height,
            )
        } else if let Some(input) =
            job.downcast_ref::<rustdar_overlays::render::rasterize::GlmStrikesInput>()
        {
            assert_eq!(items.len(), input.flashes.len(), "one item per row");
            rustdar_overlays::render::rasterize::rasterize_glm_strikes(
                input, &bounds, width, height,
            )
        } else {
            panic!("{kind:?} described {job:?}, another kind's input")
        };
        let (_, wire_cells) = overlay_reply_via_wire(&JobRequest {
            geometry: JobGeometry {
                width,
                height,
                bounds,
                side_ceiling_px: 0,
            },
            job,
        });
        let wire_cells = wire_cells.expect("a hit-map kind answers cells over the wire");
        let direct_map = HitMap::from_cells(
            direct.hit_cells.expect("the direct call builds cells"),
            &items,
        );
        let wire_map = HitMap::from_cells(wire_cells.clone(), &items);

        let mut probes_that_hit = 0;
        for idx in 0..(wire_cells.width * wire_cells.height) {
            let (u, v) = uv_of_cell(&wire_cells, idx);
            let direct_hits = direct_map.hit_test(u, v);
            let wire_hits = wire_map.hit_test(u, v);
            assert_eq!(
                direct_hits.len(),
                wire_hits.len(),
                "{kind:?}: probe {idx} answers a different number of items \
                 via the wire",
            );
            for (d, w) in direct_hits.iter().zip(&wire_hits) {
                assert!(
                    d.matches(w.as_ref()),
                    "{kind:?}: probe {idx} names a different item via the wire",
                );
            }
            if !direct_hits.is_empty() {
                probes_that_hit += 1;
            }
        }
        assert!(
            probes_that_hit > 0,
            "{kind:?}: no probe hit anything, so the agreement above is \
             vacuous — the fixture stopped drawing where the grid looks",
        );
    }
}

/// **The order-stability pin.** A hit map's cells record *positions*, so the
/// zip is only as correct as the id_map's order.
#[test]
fn a_shuffled_id_map_names_the_wrong_item_and_the_probes_can_tell() {
    use rustdar_overlays::render::rasterize::HitMap;
    use rustdar_source::id::known;
    let bounds = rustdar_geo::GeoBounds {
        min_lat: 33.0,
        max_lat: 37.0,
        min_lon: -99.0,
        max_lon: -96.0,
    };
    let (width, height) = (96u32, 64u32);
    let registry = a_seeded_reports_registry();
    let ctx = a_zip_ctx();
    let job = registry
        .prepare_job(&known::STORM_REPORTS, &ctx, &PaneRef::bare(0))
        .expect("the seeded registry describes a reports job");
    assert!(
        job.downcast_ref::<rustdar_overlays::render::rasterize::ReportsInput>()
            .is_some(),
        "the seeded registry described another kind's input: {job:?}",
    );
    let items = registry.hit_items(&known::STORM_REPORTS).expect("items");
    let (_, wire_cells) = overlay_reply_via_wire(&JobRequest {
        geometry: JobGeometry {
            width,
            height,
            bounds,
            side_ceiling_px: 0,
        },
        job,
    });
    let wire_cells = wire_cells.expect("cells");

    // A cell covered by report 0 alone: its own marker's centre.
    let (idx, _) = wire_cells
        .cells
        .iter()
        .find(|(_, ids)| ids.as_slice() == [0])
        .expect("report 0 has cells of its own");
    let (u, v) = uv_of_cell(&wire_cells, *idx);

    let straight = HitMap::from_cells(wire_cells.clone(), &items);
    let hit = straight.hit_test(u, v);
    assert_eq!(hit.len(), 1, "the probe sits on one marker");
    assert!(
        hit[0].matches(items[0].as_ref()),
        "zipped in dispatch order, report 0's marker names report 0",
    );

    let reversed: Vec<_> = items.iter().rev().cloned().collect();
    let shuffled = HitMap::from_cells(wire_cells, &reversed);
    let wrong = shuffled.hit_test(u, v);
    assert_eq!(wrong.len(), 1, "the shuffle moves identity, not coverage");
    assert!(
        !wrong[0].matches(items[0].as_ref()),
        "the reversed zip still answered the right item, so these probes \
         could never catch an order mismatch and every identity assertion \
         in this file is vacuous",
    );
    assert!(
        wrong[0].matches(items[items.len() - 1].as_ref()),
        "the reversed zip must answer exactly the mirrored item — anything \
         else means the probe is not reading the id space these tests think \
         it is",
    );
}

// ── The overlay reply's own framing ─────────────────────────────────────────

/// A literal cells fixture for the reply codec: three occupied cells on a 4×2
/// grid, one of them with two ids.
fn a_hit_cells_fixture() -> rustdar_overlays::render::rasterize::HitCells {
    let mut cells = HashMap::new();
    cells.insert(7u32, vec![2u32]);
    cells.insert(0u32, vec![0]);
    cells.insert(5u32, vec![1, 0]);
    rustdar_overlays::render::rasterize::HitCells {
        width: 4,
        height: 2,
        cells,
    }
}

/// The reply payload round-trips through its own codec, cells present or
/// absent.
#[test]
fn the_overlay_reply_round_trips_and_is_canonical() {
    let rgba: Vec<u8> = (0..32).collect();
    // `encode_overlay_out` writes into a sink since WO-M7d (the codec's head).
    let encode = |cells: Option<&rustdar_overlays::render::rasterize::HitCells>| {
        let mut out = Vec::new();
        encode_overlay_out(&rgba, cells, &mut out);
        out
    };
    for cells in [None, Some(a_hit_cells_fixture())] {
        assert_eq!(
            decode_overlay_out(&encode(cells.as_ref())),
            Some((rgba.clone(), cells.clone())),
            "the overlay reply did not survive its own codec",
        );
    }
    assert_eq!(
        encode(Some(&a_hit_cells_fixture())),
        encode(Some(&a_hit_cells_fixture())),
        "two encodes of one reply disagree: the cell walk is not canonical",
    );
}

/// The framing the overlay reply ships is **this** framing.
#[test]
fn the_overlay_reply_framing_is_the_one_this_protocol_ships() {
    let rgba: Vec<u8> = (0..16).collect();
    // Sink-shaped construction since WO-M7d; the byte VALUES these rows pin
    // are the proof the flatten changed no stream.
    let mut bare = Vec::new();
    encode_overlay_out(&rgba, None, &mut bare);
    let mut with_cells = Vec::new();
    encode_overlay_out(&rgba, Some(&a_hit_cells_fixture()), &mut with_cells);
    let rows = vec![
        format!("bare | {} | {:#018x}", bare.len(), layout_digest(&bare)),
        format!(
            "cells | {} | {:#018x}",
            with_cells.len(),
            layout_digest(&with_cells)
        ),
    ];
    assert_eq!(
        rows,
        crate::wire_identity::WIRE_REPLY_ROWS,
        "the framing `encode_overlay_out` writes is not the framing \
         `wire_identity::WIRE_REPLY_ROWS` pins. Left is what this build \
         posts; right is what that list was last told. If the change was \
         deliberate, re-pin the row in `wire_identity.rs`: the row feeds the \
         local build token, so two local builds with different rows refuse \
         each other and respawn, and in CI the `GITHUB_SHA` does the same for \
         every change. These rows are within-build parity pins and refactor \
         gates, not cross-version contracts — the wire is same-build-only by \
         construction, and a mispaired reply framing would fail the page's \
         `width x height x 4` length check whichever side were older, which \
         is a hit-map layer that silently never draws; the token is what \
         keeps that pair from ever exchanging bytes.",
    );
}

/// The framing the FRAME reply ships is **this** framing.
#[test]
fn the_frame_reply_framing_is_the_one_this_registry_ships() {
    let polar = {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(&2.125f64.to_le_bytes());
        bytes.extend_from_slice(&0.25f64.to_le_bytes());
        bytes.extend_from_slice(&0.5f64.to_le_bytes());
        for wedge in [(10.0f32, 0.5f32), (11.0, 0.5)] {
            bytes.extend_from_slice(&wedge.0.to_le_bytes());
            bytes.extend_from_slice(&wedge.1.to_le_bytes());
        }
        for value in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        rustdar_radar::render::polar::PolarField::from_bytes(&bytes)
            .expect("the literal polar block decodes")
    };
    let full = RenderedFrame {
        image: vec![10, 20, 30, 40, 50, 60, 70, 80],
        max_range_km: 230.0,
        polar,
        nyquist_ms: Some(26.4),
        melting_layer_source: Some(rustdar_radar::hca::MeltingLayerSource::RadarDetected),
        storm_motion: Some(rustdar_radar::srv::SrvMotion {
            speed_kt: 33.5,
            direction_deg: 245.0,
            source: rustdar_radar::srv::StormMotionSource::BunkersRightMover,
        }),
    };
    let bare = RenderedFrame {
        image: vec![1, 2, 3, 4],
        max_range_km: 460.0,
        polar: Default::default(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    };
    let radar_row = job_codecs()
        .find(|row| row.label == "radar")
        .expect("the radar row is composed");
    let encode = |frame: &RenderedFrame| {
        let mut head = Vec::new();
        let mut tails = Vec::new();
        (radar_row.encode_out)(DescribedOut(Box::new(frame.clone())), &mut head, &mut tails);
        assert_eq!(
            tails.len(),
            2,
            "the frame reply rides exactly two tails; a third would need \
             its own digest row",
        );
        (head, tails)
    };
    let (full_head, full_tails) = encode(&full);
    let (bare_head, bare_tails) = encode(&bare);
    let row = |name: &str, bytes: &[u8]| {
        format!("{name} | {} | {:#018x}", bytes.len(), layout_digest(bytes))
    };
    let rows = vec![
        row("frame/full/head", &full_head),
        row("frame/full/polar", &full_tails[0]),
        row("frame/full/image", &full_tails[1]),
        row("frame/bare/head", &bare_head),
        row("frame/bare/polar", &bare_tails[0]),
        row("frame/bare/image", &bare_tails[1]),
    ];
    assert_eq!(
        rows,
        crate::wire_identity::WIRE_FRAME_REPLY_ROWS,
        "the framing the frame rows' `encode_out` writes is not the framing \
         `wire_identity::WIRE_FRAME_REPLY_ROWS` pins. Left is what this \
         build posts; right is what that list was last told. If the change \
         was deliberate, re-pin the row in `wire_identity.rs`: the row feeds \
         the local build token, so two local builds with different rows \
         refuse each other and respawn, and in CI the `GITHUB_SHA` does the \
         same for every change. These rows are within-build parity pins and \
         refactor gates, not cross-version contracts — the wire is \
         same-build-only by construction, and the token is what keeps a \
         mispaired page/worker from ever exchanging bytes one of them \
         misreads.",
    );
}

/// The reply codec's malformed shapes, each with a paired positive control
/// proving the mutation landed where this test believes it did.
#[test]
fn a_malformed_overlay_reply_is_refused_rather_than_misread() {
    let rgba: Vec<u8> = (0..16).collect();
    let mut encoded = Vec::new();
    encode_overlay_out(&rgba, Some(&a_hit_cells_fixture()), &mut encoded);
    let prefix = encoded.len() - rgba.len();

    // Control first: untouched bytes decode, so every refusal below is the
    // mutation's doing.
    assert!(decode_overlay_out(&encoded).is_some());

    // Layout, stated once: tag(1) + width(4) + height(4) + count(4) = 13, then
    // the sorted entries.
    assert_eq!(
        prefix, 53,
        "the fixture's framed prefix moved; re-derive the offsets"
    );

    // Truncation anywhere inside the framed prefix is a refusal.
    for cut in 1..prefix {
        assert_eq!(
            decode_overlay_out(&encoded[..cut]),
            None,
            "the reply truncated to {cut} bytes was accepted",
        );
    }

    // A hit-cells tag this build does not have.
    let mut bad_tag = encoded.clone();
    bad_tag[0] = 2;
    assert_eq!(decode_overlay_out(&bad_tag), None, "tag 2 was accepted");

    // The last entry's index: 8 is one past the 4×2 grid.
    let mut moved = encoded.clone();
    moved[41..45].copy_from_slice(&6u32.to_le_bytes());
    let (_, cells) = decode_overlay_out(&moved).expect("index 6 is a legal cell");
    assert!(
        cells.expect("cells").cells.contains_key(&6),
        "bytes 41..45 are not the last entry's index; the refusal below \
         would be about some other field",
    );
    let mut out_of_range = encoded.clone();
    out_of_range[41..45].copy_from_slice(&8u32.to_le_bytes());
    assert_eq!(
        decode_overlay_out(&out_of_range),
        None,
        "a cell index past the stated grid was accepted",
    );

    // Order is canonical: the first entry rewritten to 6 makes the walk 6, 5,
    // 7.
    for (rewritten, what) in [(6u32, "an unsorted"), (5u32, "a duplicated")] {
        let mut disordered = encoded.clone();
        disordered[13..17].copy_from_slice(&rewritten.to_le_bytes());
        assert_eq!(
            decode_overlay_out(&disordered),
            None,
            "{what} cell index was accepted: the canonical form has stopped \
             being the only readable one, and one value has two byte strings",
        );
    }

    // An empty id list, which the rasterizer never records.
    assert_eq!(
        u32::from_le_bytes(encoded[17..21].try_into().unwrap()),
        1,
        "bytes 17..21 are not the first entry's id count; the refusal below \
         would be about some other field",
    );
    let mut emptied = encoded.clone();
    emptied[17..21].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        decode_overlay_out(&emptied),
        None,
        "a cell with no ids was accepted; nothing writes one, so it can only \
         be a layout disagreement",
    );
}

/// The reports job's own malformed shapes: the shared truncation walk, then
/// the two byte positions this kind's decoder judges.
#[test]
fn a_malformed_reports_job_is_refused_rather_than_misread() {
    let job = an_overlay_reports_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets: envelope 45 (code 1 + width 4 + height 4 + bounds 32 + ceiling
    // 4), zoom 45..53, is_dark 53, device_scale 54..58, count 58..62, first
    // row's kind byte 62.
    let mut rekinded = bytes.clone();
    assert_eq!(rekinded[62], 0, "premise: row 0 travels as tornado, code 0");
    rekinded[62] = 2;
    match JobRequest::from_bytes(&rekinded) {
        Some(JobRequest { job, .. }) => {
            let reports = job
                .downcast_ref::<rustdar_overlays::render::rasterize::ReportsInput>()
                .expect("the reports row decoded");
            assert_eq!(
                reports.reports[0].kind,
                rustdar_overlays::spc::reports::StormReportKind::Wind,
                "byte 62 is not the first row's kind; the refusal below \
                 would be about some other field",
            );
        }
        other => panic!("the rekinded control failed to decode: {other:?}"),
    }
    let mut bad_kind = bytes.clone();
    bad_kind[62] = 3;
    assert_eq!(
        JobRequest::from_bytes(&bad_kind),
        None,
        "report-kind code 3 is a build this one is not",
    );

    let mut bad_flag = bytes;
    bad_flag[53] = 2;
    assert_eq!(
        JobRequest::from_bytes(&bad_flag),
        None,
        "is_dark is a bool, not a byte",
    );
}

/// The GLM job's own malformed shapes. Beside the flag and the energy tag, the
/// field this kind exists to carry gets its own pair.
#[test]
fn a_malformed_glm_job_is_refused_rather_than_misread() {
    let job = an_overlay_glm_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets: envelope 45 (code 1 + width 4 + height 4 + bounds 32 + ceiling
    // 4), zoom 45..53, is_dark 53, device_scale 54..58, time_window 58..66,
    // now 66..78 (secs then nanos), count 78..82, first flash at 82.
    let mut renow = bytes.clone();
    let secs = i64::from_le_bytes(renow[66..74].try_into().unwrap());
    renow[66..74].copy_from_slice(&(secs + 60).to_le_bytes());
    match JobRequest::from_bytes(&renow) {
        Some(JobRequest { job, .. }) => {
            let glm = job
                .downcast_ref::<rustdar_overlays::render::rasterize::GlmStrikesInput>()
                .expect("the GLM row decoded");
            assert_eq!(
                glm.now,
                glm_fixture_now() + chrono::Duration::seconds(60),
                "bytes 66..74 are not the dispatch clock; the refusal below \
                 would be about some other field",
            );
        }
        other => panic!("the re-clocked control failed to decode: {other:?}"),
    }
    let mut bad_now = bytes.clone();
    bad_now[66..74].copy_from_slice(&i64::MAX.to_le_bytes());
    assert_eq!(
        JobRequest::from_bytes(&bad_now),
        None,
        "a clock outside chrono's range was accepted rather than refused",
    );

    // The nanos half: half a second in decodes and reads back, and a value no
    // clock writes refuses.
    let mut renanos = bytes.clone();
    renanos[74..78].copy_from_slice(&500_000_000u32.to_le_bytes());
    match JobRequest::from_bytes(&renanos) {
        Some(JobRequest { job, .. }) => {
            let glm = job
                .downcast_ref::<rustdar_overlays::render::rasterize::GlmStrikesInput>()
                .expect("the GLM row decoded");
            assert_eq!(
                glm.now,
                glm_fixture_now() + chrono::Duration::milliseconds(500),
                "bytes 74..78 are not the clock's subsecond half",
            );
        }
        other => panic!("the nanos control failed to decode: {other:?}"),
    }
    let mut bad_nanos = bytes.clone();
    bad_nanos[74..78].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        JobRequest::from_bytes(&bad_nanos),
        None,
        "four billion nanoseconds is not a subsecond",
    );

    let mut bad_flag = bytes.clone();
    bad_flag[53] = 2;
    assert_eq!(
        JobRequest::from_bytes(&bad_flag),
        None,
        "is_dark is a bool, not a byte",
    );

    // The energy option tag. Read-back first: the first flash's energy is
    // present, and moving its value bytes moves the decoded energy.
    assert_eq!(bytes[110], 1, "premise: flash 0 carries an energy");
    let mut re_energized = bytes.clone();
    re_energized[111..115].copy_from_slice(&2e-15f32.to_le_bytes());
    match JobRequest::from_bytes(&re_energized) {
        Some(JobRequest { job, .. }) => {
            let glm = job
                .downcast_ref::<rustdar_overlays::render::rasterize::GlmStrikesInput>()
                .expect("the GLM row decoded");
            assert_eq!(
                glm.flashes[0].energy,
                Some(2e-15),
                "bytes 111..115 are not the first flash's energy; the \
                 refusal below would be about some other field",
            );
        }
        other => panic!("the energy control failed to decode: {other:?}"),
    }
    let mut bad_energy_tag = bytes;
    bad_energy_tag[110] = 2;
    assert_eq!(
        JobRequest::from_bytes(&bad_energy_tag),
        None,
        "energy's option tag is 0 or 1, not a byte",
    );
}

/// **The parity gate for the model render** — the last kind through the wire,
/// and the one whose wire form is a *cut* of its input rather than a copy.
#[test]
fn the_model_render_is_byte_identical_direct_and_via_the_wire() {
    use rustdar_overlays::render::rasterize::ModelDataInput;
    let JobRequest { geometry, job } = an_overlay_model_whole_job();
    let (width, height, bounds) = (geometry.width, geometry.height, geometry.bounds);
    let model = job
        .downcast_ref::<ModelDataInput>()
        .expect("the fixture is a model job");

    let grid_points = {
        let (ni, nj) = model.shape();
        ni * nj
    };
    let win = model.window_for(&bounds, width, height);
    assert!(win.area() > 0, "the viewport missed the grid");
    assert!(
        win.area() < grid_points,
        "the window is the whole grid ({} of {grid_points} points), so \
         nothing about the cut is being tested",
        win.area(),
    );

    let direct =
        rustdar_overlays::render::rasterize::rasterize_model_data(model, &bounds, width, height);
    assert_eq!(
        direct.alpha,
        rustdar_overlays::render::rasterize::AlphaMode::Straight,
        "the model rasterizer changed its alpha convention; this gate is \
         calibrated to prove the wire's premultiply seam runs for it",
    );
    let mut expected = direct.rgba;
    premultiply_raster(&mut expected);
    assert!(
        painted(&expected) > 100,
        "the fixture painted {} pixels, so byte-identity would be \
         near-vacuous",
        painted(&expected),
    );

    let request = JobRequest { geometry, job };
    // The size claim, on the actual bytes: the job must be smaller than the
    // values it declined to ship, or the cut exists only in theory.
    assert!(
        request.to_bytes().len() < grid_points * 4 / 2,
        "the encoded model job is {} bytes against {} bytes of whole-grid \
         values — the encoder is not cutting to the window",
        request.to_bytes().len(),
        grid_points * 4,
    );

    let (via_wire, hit_cells) = overlay_reply_via_wire(&request);
    assert_eq!(
        via_wire.len(),
        (width * height * 4) as usize,
        "the reply's length is the one statement of shape the consumer checks",
    );
    assert_eq!(
        via_wire, expected,
        "the model raster differs between the direct call and the wire — the \
         window cut, the codec or the premultiply seam changed the picture",
    );
    assert_eq!(
        hit_cells, None,
        "the model render answered hit cells; it resolves no clicks by \
         pixel, and a stray Some would be refused at the deliver",
    );

    // The values are a live input through the wire, not a block a broken codec
    // could zero.
    let mut moved = a_model_grid();
    let (ci, cj) = ((win.i0 + win.i1) / 2, (win.j0 + win.j1) / 2);
    moved.values[cj * moved.ni + ci] = 4000.0;
    let repainted = overlay_raster_via_wire(&JobRequest {
        geometry,
        job: DescribedJob::new(ModelDataInput::Whole(std::sync::Arc::new(moved))),
    });
    assert_ne!(
        repainted, via_wire,
        "a moved value inside the window did not move a pixel through the \
         wire, so the equality above says nothing about the values travelling",
    );
}

/// The whole-grid carry **canonicalises** to its window on the wire.
#[test]
fn the_whole_model_grid_encodes_as_exactly_its_window() {
    use rustdar_overlays::render::rasterize::{ModelDataInput, ModelWindow};
    let whole = an_overlay_model_whole_job();
    let bytes = whole.to_bytes();
    let decoded = JobRequest::from_bytes(&bytes).expect("the whole-grid job encodes decodably");

    let JobRequest { geometry, job } = &decoded;
    let model = job
        .downcast_ref::<ModelDataInput>()
        .unwrap_or_else(|| panic!("the model job decoded as something else: {decoded:?}"));

    // The expected window form, built by the same accessors the encoder uses.
    let grid = a_model_grid();
    let source = ModelDataInput::Whole(std::sync::Arc::new(grid.clone()));
    let win = source.window_for(&geometry.bounds, geometry.width, geometry.height);
    let mut values = Vec::with_capacity(win.area());
    source.for_each_window_row(&win, |row| values.extend_from_slice(row));
    assert_eq!(
        *model,
        ModelDataInput::Window(ModelWindow {
            parameter: grid.parameter,
            ni: grid.ni,
            nj: grid.nj,
            coords: grid.coords.clone(),
            win,
            values,
        }),
        "the decode is not the window of the whole grid that was encoded",
    );

    assert_eq!(
        decoded.to_bytes(),
        bytes,
        "re-encoding the decoded window produced different bytes, so the two \
         carries are two wire forms and the framing digest row only pins one \
         of them",
    );
}

/// The model job's own malformed shapes, each mutation paired with a read-back
/// control proving it landed on the byte this test believes it did.
#[test]
fn a_malformed_model_job_is_refused_rather_than_misread() {
    use rustdar_overlays::hrrr::ModelParameter;
    let job = an_overlay_model_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    let decoded_model = |bytes: &[u8]| match JobRequest::from_bytes(bytes) {
        Some(JobRequest { job, .. }) => job
            .downcast_ref::<rustdar_overlays::render::rasterize::ModelDataInput>()
            .cloned(),
        _ => None,
    };

    // The parameter string. Control first: "sbcape" rewritten to "mlcape".
    let mut reparam = bytes.clone();
    reparam[47] = b'm';
    reparam[48] = b'l';
    let model = decoded_model(&reparam).expect("the reparam control decodes");
    assert_eq!(
        model.parameter(),
        ModelParameter::MixedLayerCape,
        "bytes 47..53 are not the parameter string; the refusals below would \
         be about some other field",
    );
    // A code no build ships refuses (valid UTF-8, unknown parameter)…
    let mut unknown = bytes.clone();
    unknown[47] = b'z';
    assert_eq!(
        JobRequest::from_bytes(&unknown),
        None,
        "parameter \"zbcape\" is a build this one is not",
    );
    // …and so does a byte no UTF-8 string contains.
    let mut not_utf8 = bytes.clone();
    not_utf8[47] = 0xFF;
    assert_eq!(JobRequest::from_bytes(&not_utf8), None, "not UTF-8");

    // The coordinates tag: 0 and 3 are unallocated.
    for bad_tag in [0u8, 3] {
        let mut retagged = bytes.clone();
        retagged[61] = bad_tag;
        assert_eq!(
            JobRequest::from_bytes(&retagged),
            None,
            "coords tag {bad_tag} must stay unallocated",
        );
    }

    // A Lambert constant. Control: `dx` (the ninth f64, bytes 126..134) moved
    // to another finite value decodes and reads back moved…
    let mut restepped = bytes.clone();
    restepped[126..134].copy_from_slice(&1500.0f64.to_le_bytes());
    let model = decoded_model(&restepped).expect("the restepped control decodes");
    match model.coords() {
        rustdar_overlays::hrrr::GridCoords::Lambert(grid) => assert_eq!(
            grid.to_parts().dx,
            1500.0,
            "bytes 126..134 are not the grid step; the refusal below would \
             be about some other field",
        ),
        other => panic!("the fixture's coords are Lambert, got {other:?}"),
    }
    // …and NaN there is a projection that answers NaN for every point.
    let mut poisoned = bytes.clone();
    poisoned[126..134].copy_from_slice(&f64::NAN.to_le_bytes());
    assert_eq!(
        JobRequest::from_bytes(&poisoned),
        None,
        "a non-finite Lambert constant was accepted",
    );

    // The wrap flag (byte 152). Control: flipped to the other valid value it
    // decodes and reads back flipped.
    let mut rewrapped = bytes.clone();
    assert_eq!(rewrapped[152], 0, "premise: the fixture grid does not wrap");
    rewrapped[152] = 1;
    let model = decoded_model(&rewrapped).expect("the rewrapped control decodes");
    assert!(
        model.coords().wraps_longitude(),
        "byte 152 is not the wrap flag",
    );
    let mut bad_flag = bytes.clone();
    bad_flag[152] = 2;
    assert_eq!(
        JobRequest::from_bytes(&bad_flag),
        None,
        "wraps_longitude is a bool, not a byte",
    );

    // The window's range check. Control first: the whole window shifted one
    // column right keeps its area (so the values arithmetic still closes) and
    // decodes, reading back shifted.
    let (i0, i1) = (14u32, 20u32);
    let mut shifted = bytes.clone();
    shifted[153..157].copy_from_slice(&(i0 + 1).to_le_bytes());
    shifted[157..161].copy_from_slice(&(i1 + 1).to_le_bytes());
    let model = decoded_model(&shifted).expect("the shifted control decodes");
    match &model {
        rustdar_overlays::render::rasterize::ModelDataInput::Window(w) => {
            assert_eq!(
                (w.win.i0, w.win.i1),
                (15, 21),
                "bytes 153..161 are not the window's i edges",
            );
        }
        other => panic!("the wire only ever decodes the window form, got {other:?}"),
    }
    let mut escaped = bytes.clone();
    escaped[153..157].copy_from_slice(&55u32.to_le_bytes());
    escaped[157..161].copy_from_slice(&61u32.to_le_bytes());
    assert_eq!(
        JobRequest::from_bytes(&escaped),
        None,
        "a window past the grid's own ni was accepted",
    );
    let mut inverted = bytes.clone();
    inverted[153..157].copy_from_slice(&20u32.to_le_bytes());
    inverted[157..161].copy_from_slice(&14u32.to_le_bytes());
    assert_eq!(
        JobRequest::from_bytes(&inverted),
        None,
        "an inside-out window was accepted",
    );

    // A value byte: the first value (bytes 169..173) moved decodes and reads
    // back moved.
    let mut revalued = bytes;
    revalued[169..173].copy_from_slice(&123.5f32.to_le_bytes());
    let model = decoded_model(&revalued).expect("the revalued control decodes");
    match &model {
        rustdar_overlays::render::rasterize::ModelDataInput::Window(w) => {
            assert_eq!(w.values[0], 123.5, "bytes 169..173 are not value 0");
        }
        other => panic!("the wire only ever decodes the window form, got {other:?}"),
    }
}

/// The overlay reply through its own row's codec.
#[test]
fn an_overlay_reply_travels_as_its_own_out_kind() {
    let output = execute(&an_overlay_sites_job()).expect("the sites job draws");

    // A frame consumer, a section consumer and a voxel consumer all see
    // "nothing to draw".
    assert!(
        execute(&an_overlay_sites_job())
            .and_then(|out| out.take::<RenderedFrame>())
            .is_none()
    );
    assert!(
        execute(&an_overlay_sites_job())
            .and_then(|out| out.take::<CrossSection>())
            .is_none()
    );
    assert!(
        execute(&an_overlay_sites_job())
            .and_then(|out| out.take::<VolumeGrid>())
            .is_none()
    );

    // The payload round-trips through the sites row's own reply codec.
    let RasterizeOutput {
        rgba, hit_cells, ..
    } = output.take::<RasterizeOutput>().expect("an overlay raster");
    let sites_row = job_codecs()
        .find(|row| row.label == "overlay/sites")
        .expect("the sites row is composed");
    let mut head = Vec::new();
    let mut tails = Vec::new();
    (sites_row.encode_out)(
        DescribedOut(Box::new(RasterizeOutput {
            rgba: rgba.clone(),
            hit_cells: hit_cells.clone(),
            alpha: rustdar_overlays::render::rasterize::AlphaMode::Premultiplied,
        })),
        &mut head,
        &mut tails,
    );
    let mut expected = Vec::new();
    encode_overlay_out(&rgba, hit_cells.as_ref(), &mut expected);
    assert_eq!(head, expected);
    // Handing the (empty) tails straight back through is the emptiness check.
    let back = (sites_row.decode_out)(&head, tails)
        .expect("the sites reply decodes")
        .take::<RasterizeOutput>()
        .expect("the sites reply is a raster");
    assert_eq!(back.rgba, rgba);
    assert_eq!(back.hit_cells, hit_cells);
    // And the take is as narrow as its four siblings.
    assert!(
        execute(&a_job())
            .and_then(|out| out.take::<RasterizeOutput>())
            .is_none()
    );
}

/// The overlay job's malformed shapes: every truncation, a trailing byte, a
/// zero or absurd raster size, a flag outside `{0, 1}`, an input kind this
/// build does not have, and a site name that is not UTF-8.
#[test]
fn a_malformed_overlay_job_is_refused_rather_than_misread() {
    let job = an_overlay_sites_job();
    let bytes = job.to_bytes();

    // Control first: untouched bytes decode to the job itself, so every
    // refusal below is the mutation's doing.
    assert_eq!(JobRequest::from_bytes(&bytes), Some(job.clone()));

    // The site list is count-prefixed and nothing follows it, so a cut
    // anywhere is a refusal.
    for cut in 1..bytes.len() {
        assert_eq!(
            JobRequest::from_bytes(&bytes[..cut]),
            None,
            "the overlay job truncated to {cut} bytes was accepted",
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        JobRequest::from_bytes(&trailing),
        None,
        "trailing bytes mean the two builds' layouts disagree",
    );

    // A zero side, and a size the raster ceiling refuses.
    for (w, h, what) in [
        (0u32, 64u32, "a zero width"),
        (96, 0, "a zero height"),
        (u32::MAX, u32::MAX, "an absurd raster"),
        (
            65_536,
            65_536,
            "one pixel past any ceiling this workspace affords",
        ),
    ] {
        let mut sized = bytes.clone();
        sized[1..5].copy_from_slice(&w.to_le_bytes());
        sized[5..9].copy_from_slice(&h.to_le_bytes());
        assert_eq!(JobRequest::from_bytes(&sized), None, "{what} was accepted");
    }

    // Offsets, stated once: code(1) + width(4) + height(4) + bounds(32) +
    // ceiling(4) = 45 is the payload's first byte, `zoom`.
    let mut bad_flag = bytes.clone();
    bad_flag[53] = 2;
    assert_eq!(
        JobRequest::from_bytes(&bad_flag),
        None,
        "is_dark is a bool, not a byte",
    );

    // The first site's name: 53 + 1 + count(4) + lat(8) + lon(8) + two flags +
    // name_len(2) = 82.
    let mut renamed = bytes.clone();
    renamed[82] = b'Q';
    match JobRequest::from_bytes(&renamed) {
        Some(JobRequest { job, .. }) => {
            let sites = job
                .downcast_ref::<rustdar_overlays::render::rasterize::SitesInput>()
                .expect("the sites row decoded");
            assert_eq!(
                sites.sites[0].name, "QTLX",
                "byte 82 is not the first name byte; the refusal below would \
                 be about some other field",
            );
        }
        other => panic!("the renamed control failed to decode: {other:?}"),
    }
    let mut bad_name = bytes;
    bad_name[82] = 0xFF;
    assert_eq!(
        JobRequest::from_bytes(&bad_name),
        None,
        "a site name that is not UTF-8 was accepted",
    );
}

/// Every truncation of an overlay job refused, and a trailing byte refused.
fn assert_refuses_cuts_and_trailing(job: &JobRequest) {
    let bytes = job.to_bytes();
    // Control first: untouched bytes decode to the job itself, so every
    // refusal below is the mutation's doing.
    assert_eq!(JobRequest::from_bytes(&bytes).as_ref(), Some(job));
    for cut in 1..bytes.len() {
        assert_eq!(
            JobRequest::from_bytes(&bytes[..cut]),
            None,
            "{} truncated to {cut} bytes was accepted",
            job.kind(),
        );
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert_eq!(
        JobRequest::from_bytes(&trailing),
        None,
        "{} with a trailing byte was accepted: the two builds' layouts \
         disagree and the decoder did not say so",
        job.kind(),
    );
}

/// The alert job's malformed shapes. Truncations and trailing bytes through
/// the shared walk.
#[test]
fn a_malformed_alerts_job_is_refused_rather_than_misread() {
    use rustdar_overlays::nws::alert::AlertCategory;
    let job = an_overlay_alerts_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets, stated once: code(1) + width(4) + height(4) + bounds(32) +
    // ceiling(4) = 45 is the payload's first byte.
    let first_category = 53;
    let first_hidden_byte = 61;

    // Positive control: a *different valid* category code decodes, and to the
    // category that code names.
    let mut retagged = bytes.clone();
    assert_eq!(
        retagged[first_category], 0,
        "the fixture leads with Warning"
    );
    retagged[first_category] = 1;
    match JobRequest::from_bytes(&retagged) {
        Some(JobRequest { job, .. }) => {
            let alerts = job
                .downcast_ref::<rustdar_overlays::render::rasterize::AlertsInput>()
                .expect("the alerts row decoded");
            assert_eq!(
                alerts.enabled_categories[0],
                AlertCategory::Watch,
                "byte {first_category} is not the first category code; the \
                 refusal below would be about some other field",
            );
        }
        other => panic!("the retagged control failed to decode: {other:?}"),
    }
    retagged[first_category] = 4;
    assert_eq!(
        JobRequest::from_bytes(&retagged),
        None,
        "category code 4 is a build this one is not",
    );

    // Positive control at the hidden id, then a byte no UTF-8 string holds.
    let mut renamed = bytes;
    assert_eq!(renamed[first_hidden_byte], b'u', "the fixture id is a urn");
    renamed[first_hidden_byte] = b'Q';
    match JobRequest::from_bytes(&renamed) {
        Some(JobRequest { job, .. }) => {
            let alerts = job
                .downcast_ref::<rustdar_overlays::render::rasterize::AlertsInput>()
                .expect("the alerts row decoded");
            assert!(
                alerts.hidden_ids.iter().any(|id| id.starts_with('Q')),
                "byte {first_hidden_byte} is not the first hidden-id byte; \
                 the refusal below would be about some other field",
            );
        }
        other => panic!("the renamed control failed to decode: {other:?}"),
    }
    renamed[first_hidden_byte] = 0xFF;
    assert_eq!(
        JobRequest::from_bytes(&renamed),
        None,
        "a hidden id that is not UTF-8 was accepted",
    );
}

/// The outlook job's malformed shapes: the shared walk, then the two bytes
/// this kind's own decoder judges.
#[test]
fn a_malformed_outlooks_job_is_refused_rather_than_misread() {
    use rustdar_overlays::types::HatchPattern;
    let job = an_overlay_outlooks_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets, stated once: the code byte and the canonical envelope end at
    // 45.
    let first_hatch = 57 + 2 + 4 + 2 + 11 + 4 + 4;
    let bounds_tag = first_hatch + 1;

    let mut rehatched = bytes.clone();
    assert_eq!(
        rehatched[first_hatch], 0,
        "the fixture's first feature is unhatched"
    );
    rehatched[first_hatch] = 2;
    match JobRequest::from_bytes(&rehatched) {
        Some(JobRequest { job, .. }) => {
            let outlooks = job
                .downcast_ref::<rustdar_overlays::render::rasterize::OutlooksInput>()
                .expect("the outlooks row decoded");
            assert_eq!(
                outlooks.features[0].hatch,
                HatchPattern::Cig2,
                "byte {first_hatch} is not the first hatch code; the refusal \
                 below would be about some other field",
            );
        }
        other => panic!("the rehatched control failed to decode: {other:?}"),
    }
    rehatched[first_hatch] = 4;
    assert_eq!(
        JobRequest::from_bytes(&rehatched),
        None,
        "hatch code 4 is a build this one is not",
    );

    // The option tag: 1 on the fixture (`OverlayFeature::new` computes its
    // AABB), read back as `Some`.
    let mut untagged = bytes;
    assert_eq!(
        untagged[bounds_tag], 1,
        "the fixture's feature carries its AABB"
    );
    match JobRequest::from_bytes(&untagged) {
        Some(JobRequest { job, .. }) => {
            let outlooks = job
                .downcast_ref::<rustdar_overlays::render::rasterize::OutlooksInput>()
                .expect("the outlooks row decoded");
            assert!(
                outlooks.features[0].geo_bounds.is_some(),
                "byte {bounds_tag} is not the geo-bounds tag; the refusal \
                 below would be about some other field",
            );
        }
        other => panic!("the option-tag control failed to decode: {other:?}"),
    }
    untagged[bounds_tag] = 2;
    assert_eq!(
        JobRequest::from_bytes(&untagged),
        None,
        "a geo-bounds tag outside {{0, 1}} was accepted",
    );
}

/// The discussion job's malformed shapes: the shared walk, then the one byte
/// this kind's own decoder judges.
#[test]
fn a_malformed_discussions_job_is_refused_rather_than_misread() {
    use rustdar_overlays::spc::discussion::MdType;
    let job = an_overlay_discussions_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets, stated once: the code byte and the canonical envelope end at
    // 45.
    let first_md_type = 53;

    let mut retyped = bytes;
    assert_eq!(retyped[first_md_type], 0, "the fixture leads Convective");
    retyped[first_md_type] = 1;
    match JobRequest::from_bytes(&retyped) {
        Some(JobRequest { job, .. }) => {
            let discussions = job
                .downcast_ref::<rustdar_overlays::render::rasterize::DiscussionsInput>()
                .expect("the discussions row decoded");
            assert_eq!(
                discussions.discussions[0].md_type,
                MdType::WinterWeather,
                "byte {first_md_type} is not the first MD-type code; the \
                 refusal below would be about some other field",
            );
        }
        other => panic!("the retyped control failed to decode: {other:?}"),
    }
    retyped[first_md_type] = 3;
    assert_eq!(
        JobRequest::from_bytes(&retyped),
        None,
        "MD-type code 3 is a build this one is not",
    );
}

/// On native, a described overlay job rides the pool's **interactive** lane.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_described_overlay_job_rides_the_interactive_lane() {
    let _guard = install_test_worker(pool::sink());

    let lane_of = |job: JobRequest| {
        let (tx, rx) = mpsc::channel();
        offload_job("test", Job::Described(job), move |_| {
            let _ = tx.send(
                std::thread::current()
                    .name()
                    .unwrap_or("<unnamed>")
                    .to_owned(),
            );
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the job delivers")
    };

    let overlay_lane = lane_of(an_overlay_sites_job());
    assert!(
        overlay_lane.starts_with("rd-opaque"),
        "the sites job delivered on {overlay_lane}: it is queueing behind \
         radar renders, which is the stall the lane split exists to prevent",
    );
    // A polygon kind rides the same lane: the routing is by the `Overlay`
    // request kind, not by which overlay is inside it, and this is what says
    // the described polygon kinds kept the closures' scheduling on native.
    let alerts_lane = lane_of(an_overlay_alerts_job());
    assert!(
        alerts_lane.starts_with("rd-opaque"),
        "the alerts job delivered on {alerts_lane}: a described polygon \
         overlay is queueing behind radar renders",
    );
    // And a hit-map kind: the routing is still by the `Overlay` request kind,
    // so describing the reports render kept the closures' scheduling.
    let reports_lane = lane_of(an_overlay_reports_job());
    assert!(
        reports_lane.starts_with("rd-opaque"),
        "the reports job delivered on {reports_lane}: a described hit-map \
         overlay is queueing behind radar renders",
    );
    // And the model grid — the last kind whose closure this lane used to
    // carry.
    let model_lane = lane_of(an_overlay_model_job());
    assert!(
        model_lane.starts_with("rd-opaque"),
        "the model job delivered on {model_lane}: the described model \
         overlay is queueing behind radar renders",
    );
    let radar_lane = lane_of(a_job());
    assert!(
        radar_lane.starts_with("rd-job"),
        "the radar control delivered on {radar_lane}, so the routing is not \
         by deadline and the assertion above proves nothing",
    );
}
