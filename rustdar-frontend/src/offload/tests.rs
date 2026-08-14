use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// What a `FakePort` recorded: the id it was given and the bytes it was
/// asked to post, in order.
///
/// Bytes and not `JobRequest`s, though the trait now hands over the request
/// itself. The browser's sink serialises on its way out and this one stands in
/// for it, so recording what it *would have posted* keeps the codec on the path
/// these tests walk.
type Posted = Arc<std::sync::Mutex<Vec<(u64, Vec<u8>)>>>;

/// A port that records what it was handed instead of posting anywhere.
struct FakePort {
    posted: Posted,
    accept: bool,
}

impl JobSink for FakePort {
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        if !self.accept {
            // The refusal path, and the reason it hands the request back: the
            // funnel keeps no other copy, so a sink that dropped one here would
            // lose the job rather than let it run inline.
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

/// Leave this thread with no sink at all, so the funnel's `NoSink` arm — not
/// the pool handle a native thread starts with — is what takes the next job.
///
/// The registry is **one map for the whole process** and `cargo test` runs
/// these concurrently on separate threads, so what keeps one test from tearing
/// down another's jobs is not the map: it is `Pending::sink`. Each `attach`
/// takes a fresh sink id and `abandon_worker` fails only that id's jobs. Every
/// test still tears down, so a panic mid-test cannot leak a port into the next
/// one on the same thread.
fn detach() {
    abandon_worker("test teardown");
}

/// A job that is cheap to execute and easy to recognize. It renders
/// nothing, which is fine: the funnel's contract is about *where* and
/// *whether* `deliver` runs, not what the renderer drew.
pub(super) fn a_job() -> JobRequest {
    JobRequest::Radar {
        input: Box::new(
            RenderInput::from_bytes(&sample_input_bytes()).expect("fixture payload decodes"),
        ),
        values_wanted: true,
        side_ceiling_px: 4096,
    }
}

/// The smallest real volume: two sweeps of a handful of radials, under a
/// VCP that **declares its cuts**.
///
/// The cut table is what the tilt ladder is keyed by, so a fixture without
/// one can only ever exercise the refusal path in
/// `rustdar_radar::sampler::VolumeSampler` — which would make every
/// assertion below about a section or a grid vacuously `None`.
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

/// A Level III job. The bytes are opaque here on purpose: the framing must
/// carry an arbitrary tail without a length prefix that could lie about it.
fn a_level3_job() -> JobRequest {
    JobRequest::Level3 {
        bytes: std::sync::Arc::new(vec![7, 8, 9, 0xFF, 0]),
        product: rustdar_radar::types::RadarProduct::EchoTops,
        radar_lat: 35.0,
        radar_lon: -97.0,
        side_ceiling_px: 4096,
    }
}

/// The two-object VIL density job. The two payloads differ in length *and*
/// in content, so a framing that swapped them, or one that split them at
/// the wrong offset, cannot round-trip.
fn a_level3_pair_job() -> JobRequest {
    JobRequest::Level3Pair {
        dvl: std::sync::Arc::new(vec![1, 2, 3]),
        eet: std::sync::Arc::new(vec![4, 5, 6, 7, 0xFF, 0]),
        radar_lat: 35.0,
        radar_lon: -97.0,
        side_ceiling_px: 4096,
    }
}

/// The whole-volume payload the two vertical job kinds carry.
///
/// `extract_volume` rather than `extract`, which is the difference between
/// a section cut from the ladder and one interpolated across the tilts that
/// did not travel.
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
    JobRequest::Section {
        input: Box::new(a_volume_input()),
        request: SectionRequest {
            start: (35.0, -97.5),
            end: (35.4, -96.8),
            top_km_msl: Some(18.0),
            product: rustdar_radar::types::RadarProduct::Reflectivity,
        },
    }
}

pub(super) fn a_voxel_job() -> JobRequest {
    JobRequest::Voxels {
        input: Box::new(a_volume_input()),
        request: VoxelRequest {
            centre: (35.0, -97.0),
            half_extent_km: Some(rustdar_radar::voxel::HalfExtentKm::square(60.0)),
            base_km_msl: 0.0,
            top_km_msl: 15.0,
            product: rustdar_radar::types::RadarProduct::Reflectivity,
            // Small and *asymmetric*, so a decoder that read the three axes
            // in the wrong order does not round-trip.
            shape: VoxelShape {
                nx: 8,
                ny: 6,
                nz: 4,
            },
            values_wanted: true,
        },
    }
}

/// The voxel job a pane with **no picked region** posts: the width is left
/// for `build_voxels` to take from the volume's own reach.
///
/// A separate job rather than a second field on the one above, because the
/// half-width is the only tagged optional in this encoding and `None` is the
/// case every ordinary 3D pane sends. A decoder that read the tag byte as the
/// first byte of an `f64` would round-trip the `Some` arm and hand the worker
/// a nonsense box for this one.
fn a_sourceless_voxel_job() -> JobRequest {
    match a_voxel_job() {
        JobRequest::Voxels { input, request } => JobRequest::Voxels {
            input,
            request: VoxelRequest {
                half_extent_km: None,
                ..request
            },
        },
        other => other,
    }
}

/// The sites overlay job, on a fixture with real content: two markers inside
/// the box — one of them current, so both fill colours draw — and one far
/// outside it, so the cull path runs too. The dimensions are distinct and the
/// box asymmetric, so a decoder that transposed width with height or a lat
/// with a lon cannot round-trip.
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
    JobRequest::Overlay {
        width: 96,
        height: 64,
        bounds: rustdar_overlays::types::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        input: OverlayJobInput::Sites(rustdar_overlays::render::rasterize::SitesInput {
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

/// The NWS-alert overlay job, on a fixture with real content — and with the
/// geometry the encode is most likely to flatten: a **MultiPolygon** alert
/// whose first polygon carries a **hole**. A codec that dropped a nesting
/// level, a hole ring or the ring order would still produce plausible bytes,
/// and the parity test over this fixture is what catches it.
///
/// Both filters are loaded too: two categories enabled out of the fixture's
/// two, and one alert hidden by id — so the id set and the category codes are
/// live inputs whose loss changes pixels, not dead fields a broken codec
/// could zero.
pub(super) fn an_overlay_alerts_job() -> JobRequest {
    use rustdar_overlays::nws::alert::AlertCategory;
    use rustdar_overlays::render::rasterize::{AlertPaint, AlertsInput};
    use rustdar_overlays::types::{HatchPattern, OverlayFeature};
    // One alert, two polygons; the first has a hole in it. Translucent fill
    // and an opaque stroke, so both colour fields put distinct ink down.
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
    JobRequest::Overlay {
        width: 96,
        height: 64,
        bounds: rustdar_overlays::types::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        input: OverlayJobInput::Alerts(Box::new(AlertsInput {
            alerts: vec![
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0001".into(),
                    category: AlertCategory::Warning,
                    features: vec![warned],
                },
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0002".into(),
                    category: AlertCategory::Advisory,
                    features: vec![advised],
                },
                AlertPaint {
                    id: "urn:oid:2.49.0.1.840.0003".into(),
                    category: AlertCategory::Warning,
                    features: vec![hidden],
                },
            ],
            enabled_categories: vec![AlertCategory::Warning, AlertCategory::Advisory],
            hidden_ids: std::collections::HashSet::from(["urn:oid:2.49.0.1.840.0003".to_owned()]),
            device_scale: 1.0,
        })),
    }
}

/// The SPC-outlook overlay job, with the pass the other kinds do not have:
/// **hatching**. Three features — a plain fill with a hole, a CIG1 area, and
/// a CIG3 area nested inside it — drive the hatch pass's masks, exclusions
/// and hole handling through the wire, and the hatch colour is the
/// theme-resolved page-side input that travels *on the job* rather than
/// being re-derived worker-side. Pure blue at full alpha, a colour no SPC
/// fill uses, so hatch ink is countable in the parity test.
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
    JobRequest::Overlay {
        width: 96,
        height: 64,
        bounds: rustdar_overlays::types::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        input: OverlayJobInput::Outlooks(OutlooksInput {
            features: vec![categorical, cig1, cig3],
            hatch_color: [0, 0, 255, 255],
            device_scale: 1.0,
        }),
    }
}

/// The SPC-discussion overlay job: two MDs of different types — the type is
/// what picks the fill and stroke, so a type byte misread as another decodes
/// cleanly and paints the wrong colours, and byte-parity is what notices —
/// one of them with two rings, each drawn as its own filled polygon.
pub(super) fn an_overlay_discussions_job() -> JobRequest {
    use rustdar_overlays::render::rasterize::{DiscussionPaint, DiscussionsInput};
    use rustdar_overlays::spc::discussion::MdType;
    JobRequest::Overlay {
        width: 96,
        height: 64,
        bounds: rustdar_overlays::types::GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        input: OverlayJobInput::Discussions(DiscussionsInput {
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

/// The voxel job a pane whose viewport is **not square** posts.
///
/// The two axes are two `f64`s on the wire, adjacent and same-typed, so an
/// encoder writing them in one order and a decoder reading them in the other
/// round-trips every square job in this file and none of the real ones. 92 km
/// east against 37 km north because both are distinctive: no arithmetic
/// anywhere in the encoding turns one into the other.
fn a_rectangular_voxel_job() -> JobRequest {
    match a_voxel_job() {
        JobRequest::Voxels { input, request } => JobRequest::Voxels {
            input,
            request: VoxelRequest {
                half_extent_km: Some(rustdar_radar::voxel::HalfExtentKm {
                    east_km: 92.0,
                    north_km: 37.0,
                }),
                ..request
            },
        },
        other => other,
    }
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
    ] {
        assert_eq!(
            JobRequest::from_bytes(&job.to_bytes()),
            Some(job.clone()),
            "{:?} did not survive its round trip",
            job.kind()
        );
    }
}

/// The retired SRM job tag must be refused, not resurrected: a worker
/// from a build that still posts it gets a failed job, never a render of
/// something this build would compute differently.
#[test]
fn the_retired_srm_tag_is_refused() {
    assert_eq!(JobRequest::from_bytes(&[TAG_SRM_RETIRED, 1, 2, 3]), None);
}

/// Every job tag is pinned to the literal byte it ships as.
///
/// Distinctness and the round trip are both entailed by this table, and
/// neither would be enough on its own: a check that reads the constants it
/// is checking **survives a renumbering**, and the round trip does too. Swap
/// [`TAG_LEVEL3_PAIR`]'s 4 with [`TAG_VOXELS`]'s 6 and every tag is still
/// distinct, every job still round-trips through this build, and the whole
/// workspace still passes.
///
/// What that costs is already written down above the constants: a job
/// landing in the `TAG_LEVEL3_PAIR` arm reads two `f64`s and a `u32`
/// length and then takes the rest, so on another kind's plausible bytes it
/// *succeeds* and renders a VIL-density product out of the wrong geometry.
/// The tag is a contract between two builds — a page that renumbers is
/// talking to workers that did not — so the numbers have to be written
/// out, not read back.
#[test]
fn every_job_tag_is_the_literal_byte_it_ships_as() {
    // Deliberately spelled out. Do not regenerate this from the constants.
    let table: [(&str, u8, u8); 8] = [
        ("TAG_RADAR", TAG_RADAR, 1),
        ("TAG_LEVEL3", TAG_LEVEL3, 2),
        ("TAG_SRM_RETIRED", TAG_SRM_RETIRED, 3),
        ("TAG_LEVEL3_PAIR", TAG_LEVEL3_PAIR, 4),
        ("TAG_SECTION", TAG_SECTION, 5),
        ("TAG_VOXELS", TAG_VOXELS, 6),
        ("TAG_DECODE", TAG_DECODE, 7),
        ("TAG_OVERLAY", TAG_OVERLAY, 8),
    ];
    for (name, actual, expected) in table {
        assert_eq!(
            actual, expected,
            "{name} moved on the wire: it is {actual} now, not {expected}",
        );
    }

    // And the encoder really posts those bytes — the constant could be
    // right while the arm that writes it is not. Every constructible kind,
    // framed against its literal rather than against its own constant.
    let framing: [(JobRequest, u8); 10] = [
        (a_job(), 1),
        (a_level3_job(), 2),
        (a_level3_pair_job(), 4),
        (a_section_job(), 5),
        (a_voxel_job(), 6),
        (a_decode_job(), 7),
        (an_overlay_sites_job(), 8),
        (an_overlay_alerts_job(), 8),
        (an_overlay_outlooks_job(), 8),
        (an_overlay_discussions_job(), 8),
    ];
    for (job, tag) in framing {
        let bytes = job.to_bytes();
        assert_eq!(
            bytes[0],
            tag,
            "{:?} posts tag {}, not {tag} — a worker of another build \
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

    // The unallocated bytes on either end of the table stay unallocated.
    // A ninth kind added without a line in the table above makes 9
    // decode, and this is what says so. **7 left this list when the decode
    // job took it, and 8 when the overlay job did**, which is the whole point
    // of the list: a new kind cannot be added without coming here and saying
    // so.
    let mut bytes = a_voxel_job().to_bytes();
    for unallocated in [0u8, 9] {
        bytes[0] = unallocated;
        assert_eq!(
            JobRequest::from_bytes(&bytes),
            None,
            "tag {unallocated} decodes, so the table above has stopped \
                 being the whole wire",
        );
    }
}

/// The overlay job's **inner** code space, pinned the way the outer tags are
/// and for the same reason: the code is a contract between two builds, so
/// the numbers are written out, not read back from the constants they check.
///
/// A renumbering here is quieter than one of the outer tags — every overlay
/// input is length-counted and refuses trailing bytes, so a swapped pair of
/// codes mostly fails to decode — but "mostly" is not a guard: two kinds
/// whose leading fields happen to parse under each other's layout would
/// rasterize the wrong layer, and the version 10 worker split exists
/// precisely because a version 9 worker must refuse codes 2-4 rather than
/// misread them.
#[test]
fn every_overlay_input_code_is_the_literal_byte_it_ships_as() {
    // Deliberately spelled out. Do not regenerate this from the constants.
    let table: [(&str, u8, u8); 4] = [
        ("OVERLAY_INPUT_SITES", OVERLAY_INPUT_SITES, 1),
        ("OVERLAY_INPUT_ALERTS", OVERLAY_INPUT_ALERTS, 2),
        ("OVERLAY_INPUT_OUTLOOKS", OVERLAY_INPUT_OUTLOOKS, 3),
        ("OVERLAY_INPUT_DISCUSSIONS", OVERLAY_INPUT_DISCUSSIONS, 4),
    ];
    for (name, actual, expected) in table {
        assert_eq!(
            actual, expected,
            "{name} moved on the wire: it is {actual} now, not {expected}",
        );
    }

    // And the encoder really writes those bytes where the decoder reads them:
    // the input-kind byte sits after the fixed header — tag(1) + width(4) +
    // height(4) + bounds(32) = offset 41 — for every overlay kind alike.
    let by_fixture: [(JobRequest, u8); 4] = [
        (an_overlay_sites_job(), 1),
        (an_overlay_alerts_job(), 2),
        (an_overlay_outlooks_job(), 3),
        (an_overlay_discussions_job(), 4),
    ];
    for (job, code) in by_fixture {
        let bytes = job.to_bytes();
        assert_eq!(
            bytes[41],
            code,
            "{:?} posts inner code {}, not {code} — a worker of another build \
             rasterizes it as whatever {} names there, or refuses it",
            job.kind(),
            bytes[41],
            bytes[41],
        );
    }

    // The unallocated bytes on either end stay unallocated: 0 so a zeroed
    // buffer never decodes, and 5 so a fifth kind cannot arrive without a
    // line in the table above. **2 through 4 left this list when the polygon
    // kinds took them.**
    let mut bytes = an_overlay_sites_job().to_bytes();
    for unallocated in [0u8, 5] {
        bytes[41] = unallocated;
        assert_eq!(
            JobRequest::from_bytes(&bytes),
            None,
            "overlay input code {unallocated} decodes, so the table above has \
             stopped being the whole inner wire",
        );
    }
}

/// The product is on the wire twice — in the request geometry and inside
/// the payload — and a disagreement is refused rather than drawn.
///
/// It has to be refused *here*, because downstream it does not fail:
/// `VolumeSampler` builds no rung for a moment the payload does not carry,
/// every sample reads `NoCoverage`, and the result is a full-size,
/// correctly-shaped raster of clear air — indistinguishable from a section
/// through genuinely empty sky.
#[test]
fn a_request_naming_a_different_product_from_its_payload_is_refused() {
    for (job, product_offset) in [(a_section_job(), 1), (a_voxel_job(), 2)] {
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
            // Truncation anywhere must be a clean refusal. The tail is a
            // `RenderInput`, which refuses trailing bytes, so unlike the
            // Level III jobs every cut can be asserted rather than merely
            // exercised.
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
        let at = if matches!(job, JobRequest::Section { .. }) {
            1
        } else {
            2
        };
        bad_product[at] = 0xFE;
        bad_product[at + 1] = 0xFF;
        assert_eq!(JobRequest::from_bytes(&bad_product), None, "product code");
    }

    // The voxel job's `values_wanted` is a bool, not a byte.
    let mut bad_flag = a_voxel_job().to_bytes();
    bad_flag[1] = 2;
    assert_eq!(JobRequest::from_bytes(&bad_flag), None, "values_wanted");

    // And a shape with a zero axis is refused at the boundary rather than
    // deep inside `build_voxels`: a renderer dividing an extent by a zero
    // dimension gets an infinity.
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
    // precondition: the offset arithmetic above really points at the shape,
    // so the assertions are about the guard rather than about corrupting
    // some other field into invalidity.
    let mut same = bytes.clone();
    same[shape_at] = 8;
    same[shape_at + 1] = 0;
    assert_eq!(
        JobRequest::from_bytes(&same),
        Some(a_voxel_job()),
        "the shape is not where this test thinks it is",
    );
}

/// The two vertical arms of [`execute`] actually run, end to end, on a
/// volume with a cut table.
///
/// Without this the wire could round-trip perfectly and `execute` could
/// answer `None` for both kinds forever — which is what the
/// `assert_eq!(execute(&…), None)` assertions elsewhere in this module
/// would look like if they were the only evidence.
#[test]
fn the_vertical_jobs_produce_their_own_output_kinds() {
    let section = execute(&a_section_job()).expect("the section job draws");
    assert_eq!(section.out_kind(), Some(crate::offload::OUT_KIND_SECTION));
    assert!(section.section().is_some());

    let voxels = execute(&a_voxel_job()).expect("the voxel job builds");
    assert_eq!(voxels.out_kind(), Some(crate::offload::OUT_KIND_VOXELS));
    let grid = voxels.voxels().expect("the voxel job answers a grid");
    assert_eq!(grid.shape().cells(), 8 * 6 * 4);

    // And the same jobs off the wire, which is the path a worker takes.
    assert_eq!(
        execute_bytes(&a_section_job().to_bytes()).map(|o| o.out_kind()),
        Some(Some(crate::offload::OUT_KIND_SECTION)),
    );
    assert_eq!(
        execute_bytes(&a_voxel_job().to_bytes()).map(|o| o.out_kind()),
        Some(Some(crate::offload::OUT_KIND_VOXELS)),
    );
}

/// A frame consumer handed an output of another kind sees `None` — the
/// "nothing to draw" every render path already handles — and **never** a
/// wrong-shaped buffer.
///
/// This is the accessor the whole widening rests on. `RenderedFrame` was
/// deliberately not given a width and a height, so every consumer of one reads
/// its side back off the buffer, checked against the *square* shape and the
/// bounds a plan view has; a section's raster is not square and would be
/// refused, but a refusal is a blank pane, and this is what keeps one from ever
/// getting that far.
#[test]
fn a_frame_consumer_sees_nothing_rather_than_another_kinds_buffers() {
    let section = execute(&a_section_job()).expect("the section job draws");
    assert_eq!(section.frame(), None);
    let voxels = execute(&a_voxel_job()).expect("the voxel job builds");
    assert_eq!(voxels.frame(), None);
    // And the frame arm still yields its frame, so the accessor is not
    // simply always `None`.
    assert!(
        execute(&a_job())
            .and_then(JobOutput::frame)
            .is_some_and(|f| !f.image.is_empty()),
    );
    // The two vertical accessors are equally narrow.
    assert!(execute(&a_job()).and_then(JobOutput::section).is_none());
    assert!(execute(&a_job()).and_then(JobOutput::voxels).is_none());
    assert!(
        execute(&a_section_job())
            .and_then(JobOutput::voxels)
            .is_none()
    );
}

/// The worker reply's non-frame half, both directions.
///
/// This is the whole `OUT` field: `rustdar-web` copies bytes out of a
/// `Uint8Array` and hands them here with the kind tag, so everything that
/// can go wrong with that field can be exercised on a host.
#[test]
fn an_out_of_band_payload_round_trips_and_refuses_what_it_should() {
    use rustdar_radar::types::RenderView;

    let section = execute(&a_section_job())
        .and_then(JobOutput::section)
        .expect("the section job draws");
    let grid = execute(&a_voxel_job())
        .and_then(JobOutput::voxels)
        .expect("the voxel job builds");

    let section_bytes = section.to_bytes();
    let grid_bytes = grid.to_bytes();
    assert_eq!(
        decode_output(RenderView::CrossSection.wire_code(), &section_bytes),
        Some(JobOutput::Section(section)),
    );
    assert_eq!(
        decode_output(RenderView::Volume.wire_code(), &grid_bytes),
        Some(JobOutput::Voxels(grid)),
    );

    // A kind byte this build does not have.
    assert_eq!(decode_output(0, &section_bytes), None);
    assert_eq!(decode_output(u8::MAX, &section_bytes), None);
    // A frame does not travel this way; a reply claiming it does is from a
    // build whose protocol is not this one.
    assert_eq!(
        decode_output(RenderView::PlanView.wire_code(), &section_bytes),
        None,
    );
    // The two payload codecs each have their own magic, so the tag naming
    // the wrong decoder is a refusal rather than a reinterpretation.
    assert_eq!(
        decode_output(RenderView::Volume.wire_code(), &section_bytes),
        None,
    );
    assert_eq!(
        decode_output(RenderView::CrossSection.wire_code(), &grid_bytes),
        None,
    );
    assert_eq!(
        decode_output(RenderView::CrossSection.wire_code(), &[]),
        None
    );
}

/// **The invariant the render budget depends on: every `deliver` sends on
/// its channel on every arm, including the wrong-kind arm.**
///
/// A pane takes a render slot and an in-flight mark when it dispatches, and
/// only `deliver` running unwinds them. A wrong-kind result that returned
/// early instead of delivering would leak one slot per occurrence, and with
/// `MAX_CONCURRENT_RENDERS` at **1 on wasm** the first leak stops every
/// render in the tab, permanently — the pane wedges with no error.
#[test]
fn a_job_answered_with_the_wrong_output_kind_still_delivers() {
    for job in [a_section_job(), a_voxel_job()] {
        let kind = job.kind();
        detach();
        let (tx, rx) = mpsc::channel();
        // The consumer is shaped for a frame — the shape both production
        // `offload_job` callers have — and the job answers a section.
        offload_job("test", Job::Described(job), move |output| {
            let _ = tx.send(output.and_then(JobOutput::frame).is_some());
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)),
            Ok(false),
            "{kind}: a wrong-kind result did not reach deliver, so the \
                 render budget just leaked a slot",
        );
    }

    // The same across the worker boundary, where the reply is what carries
    // the result: `abandon_worker` must fail a posted vertical job too.
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
    assert_eq!(JobRequest::from_bytes(&[0xFF, 1, 2]), None, "unknown tag");
    assert_eq!(JobRequest::from_bytes(&[TAG_RADAR]), None, "no flag");
    assert_eq!(
        JobRequest::from_bytes(&[TAG_RADAR, 1]),
        None,
        "no second flag"
    );
    assert_eq!(
        JobRequest::from_bytes(&[TAG_RADAR, 1, 1]),
        None,
        "no payload"
    );
    // `values_wanted` is a boolean, and a byte outside `{0, 1}` is a build
    // whose protocol is not this one — refused rather than guessed at.
    assert_eq!(
        JobRequest::from_bytes(&[TAG_RADAR, 2, 0, 0, 0, 0]),
        None,
        "values_wanted is a bool, not a byte"
    );
    // The side ceiling beside it is four bytes, not one. A header holding
    // fewer is a build that wrote the flag this replaced, and reading its one
    // byte as a size would put a 1 px ceiling on every render rather than
    // refusing the message.
    assert_eq!(
        JobRequest::from_bytes(&[TAG_RADAR, 1, 2]),
        None,
        "a side ceiling short of its four bytes"
    );
    for (tag, what) in [(TAG_LEVEL3, "level3"), (TAG_LEVEL3_PAIR, "level3 pair")] {
        assert_eq!(
            JobRequest::from_bytes(&[tag, 2]),
            None,
            "{what}: a side ceiling short of its four bytes"
        );
    }

    // A length prefix that claims more than the payload holds must be
    // refused, not read as a short object: the pair's first length is the
    // one number on the wire that could lie. Byte 21: the tag, the four-byte
    // side ceiling and two `f64`s precede it.
    let mut overlong = a_level3_pair_job().to_bytes();
    overlong[21] = 0xFF;
    assert_eq!(
        JobRequest::from_bytes(&overlong),
        None,
        "a DVL length past the end of the payload",
    );

    // A truncated header must not be read as a short one. The variable tail
    // is whatever is left, so only the fixed part can be checked this way.
    for job in [a_job(), a_level3_job(), a_level3_pair_job()] {
        let bytes = job.to_bytes();
        for cut in 1..bytes.len().min(20) {
            let _ = JobRequest::from_bytes(&bytes[..cut]);
        }
        assert_eq!(
            JobRequest::from_bytes(&bytes[..1]),
            None,
            "a tag with no header must be refused"
        );
    }

    // Bytes 5 and 6: the tag and the four-byte side ceiling precede the
    // product code.
    let mut bad_product = a_level3_job().to_bytes();
    bad_product[5] = 0xFE;
    bad_product[6] = 0xFF;
    assert_eq!(
        JobRequest::from_bytes(&bad_product),
        None,
        "a product code this build does not have"
    );
}

/// A Level III payload that does not decode is a render that drew nothing,
/// not a panic — the bytes come off a message port.
#[test]
fn an_undecodable_level3_payload_renders_nothing() {
    assert_eq!(execute(&a_level3_job()), None);
    assert_eq!(
        execute(&a_level3_pair_job()),
        None,
        "neither object of the pair decodes",
    );
}

/// With no sink installed, `offload_job` falls through to [`offload`] and
/// `deliver` still sees the result.
///
/// That fallthrough is the browser-without-a-worker case, and it is the one
/// path that stays inline: on a native thread [`offload`] hands the closure to
/// the pool's opaque lane instead, which is why this asserts on the *result*
/// arriving rather than on where it arrived from.
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

/// **The convergence, asserted where it is visible**: a native thread starts
/// with a sink already installed, so a described job goes through the same
/// registry, the same id space and the same `deliver_job_reply` the browser's
/// worker replies through — and *not* through a thread spawned for it.
///
/// The wasm arm has no default sink (there is nowhere for a job to run until
/// `worker_port::attach` proves a `Worker`), so this is a native claim and says
/// so.
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
        // job crossed the transport and came back — not merely that
        // `offload_job` returned.
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

/// The cancellation contract, across the worker boundary. `deliver` carries
/// the pane's flag, so a render abandoned while the worker held it must
/// deliver nothing — exactly as the inline arm's `wanted` check does.
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

    // Two references while the job is outstanding: the pane's list and the
    // one inside `deliver`. That is what `want_result`'s pruning reads.
    assert_eq!(Arc::strong_count(&wanted), 2);

    wanted.store(false, Ordering::Relaxed);
    let id = posted.lock().unwrap()[0].0;
    deliver_job_reply(
        id,
        Some(JobOutput::Frame(RenderedFrame {
            image: vec![0; 4],
            max_range_km: 230.0,
            polar: Default::default(),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        })),
    );

    assert!(rx.try_recv().is_err(), "an abandoned render must not send");
    assert_eq!(
        Arc::strong_count(&wanted),
        1,
        "retiring the job must drop deliver's reference, or want_result never prunes"
    );
    detach();
}

/// A worker that dies owes replies that will never come. Those jobs have to
/// be failed, not forgotten: `deliver` holds the render budget's guard and
/// the pane's in-flight mark.
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
/// `abandon_worker` — must be dropped, not delivered a second time.
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

/// A `Decode` request whose archive is bytes no decoder will accept. The point
/// is the *framing*, which has to survive whatever the payload turns out to be.
fn a_decode_job() -> JobRequest {
    JobRequest::Decode {
        archive: std::sync::Arc::new(b"AR2V0006.001not-a-real-volume".to_vec()),
    }
}

#[test]
fn a_decode_job_round_trips_its_archive_whole() {
    let job = a_decode_job();
    let back = JobRequest::from_bytes(&job.to_bytes()).expect("this build wrote it");
    assert_eq!(back, job);
}

/// The tag space is shared with five render kinds, and a decode's payload is
/// arbitrary bytes — so a `Decode` posted to a build that read it as any other
/// kind is exactly the misparse the tag byte exists to prevent.
#[test]
fn a_decode_job_is_not_readable_as_another_kind() {
    let mut bytes = a_decode_job().to_bytes();
    for tag in [1u8, 2, 3, 4, 5, 6] {
        bytes[0] = tag;
        // Whatever it decodes to, it must not decode to a `Decode`.
        assert!(
            !matches!(
                JobRequest::from_bytes(&bytes),
                Some(JobRequest::Decode { .. })
            ),
            "tag {tag} produced a decode job"
        );
    }
}

/// An archive this build cannot read is "nothing", which is what a failed
/// render has always answered — not a panic in a browser tab where nobody
/// would see it.
#[test]
fn an_archive_that_does_not_decode_produces_nothing() {
    assert_eq!(execute(&a_decode_job()), None);
    assert_eq!(execute_bytes(&a_decode_job().to_bytes()), None);
}

/// The reply half: a decoded volume comes back through `decode_output` under
/// its own kind byte, and under nobody else's.
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

    let back = decode_output(crate::offload::OUT_KIND_VOLUME, &bytes)
        .expect("a volume payload under the volume kind");
    assert_eq!(back.out_kind(), Some(crate::offload::OUT_KIND_VOLUME));
    assert_eq!(*back.volume().expect("it is a volume"), volume);

    // The same bytes under the other kinds' tags are refused by those types'
    // own magic rather than half-decoded into something plausible.
    for kind in [
        crate::offload::OUT_KIND_SECTION,
        crate::offload::OUT_KIND_VOXELS,
    ] {
        assert!(
            decode_output(kind, &bytes).is_none(),
            "kind {kind} accepted"
        );
    }
}

/// **Every storm motion rung has a stable, distinct byte, and it survives the
/// round trip.**
///
/// The only boundary a [`rustdar_radar::srv::StormMotionSource`] crosses as a
/// number is the browser's page↔worker port, and what is on the far side of
/// that number is which vector an SRV field was shifted by. A renumbering that
/// went one way and not the other would not blank the notice — it would move
/// it, and the page would caption a Bunkers right-mover as the RPG's own
/// applied vector, or the reverse. That is not a degraded picture; the two are
/// different quantities and the whole path exists to keep them apart.
///
/// So this asserts three things at once: the codes are the ones the wire is
/// documented to carry, no two rungs share one, and `from_wire_code` is the
/// genuine inverse rather than a second table that agrees today. The exhaustive
/// walk over `ALL` is what makes it a property of the enum: a fifth rung added
/// upstream fails the match arms in `StormMotionWire` first, and this row-count
/// assertion second.
#[test]
fn every_storm_motion_rung_has_a_stable_distinct_wire_code() {
    use rustdar_radar::srv::StormMotionSource as S;

    // Declaration order, which is fallback order, which is the numbering.
    const ALL: [(S, u8); 4] = [
        (S::UserOverride, 0),
        (S::RpgScitAverage, 1),
        (S::BunkersRightMover, 2),
        (S::MeanWind, 3),
    ];

    let mut seen = std::collections::HashSet::new();
    for (source, expected) in ALL {
        let code = StormMotionWire(source).wire_code();
        assert_eq!(
            code, expected,
            "{source:?} moved on the wire: a page and a worker built either \
             side of that change caption one rung with another's words",
        );
        assert!(
            seen.insert(code),
            "{source:?} shares byte {code} with another rung",
        );
        assert_eq!(
            StormMotionWire::from_wire_code(code),
            Some(StormMotionWire(source)),
            "byte {code} did not decode back to {source:?}",
        );
    }
    assert_eq!(
        seen.len(),
        4,
        "a rung was added or removed without this table moving",
    );

    // A byte this build does not have reads as "no source stated" — a page and
    // a worker on opposite sides of a deploy, which the protocol token already
    // refuses — rather than as some rung picked by arithmetic.
    assert_eq!(StormMotionWire::from_wire_code(4), None);
    assert_eq!(StormMotionWire::from_wire_code(u8::MAX), None);
}

// ── The job framing's layout ────────────────────────────────────────────────

/// FNV-1a 64 over a payload, for [`the_job_framing_is_the_one_this_protocol_ships`].
///
/// A copy of `rustdar_radar::wire::layout_digest` rather than a call to it: that
/// one is `#[cfg(test)] pub(crate)`, so it does not exist outside its own crate,
/// and making it public would ship test-only code in the library to save six
/// lines. Copying a *hash* costs nothing — the thing that must not be duplicated
/// is an encoder, because a second encoder has to be kept in step; a second
/// FNV-1a either agrees with the first or is obviously broken.
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
///
/// Three of the six variants put a `RenderInput` last, and its bytes are
/// already pinned by `rustdar_radar`'s
/// `render_input::tests::the_wire_layout_is_the_one_this_version_ships`.
/// Digesting them again here would say nothing new and would make this test
/// fail for a `RenderInput` change that the other pin has already reported —
/// two red tests for one edit, one of which names the wrong file. Worse, the
/// `RenderInput` these fixtures carry comes out of `RenderInput::extract`,
/// which walks the beam geometry, so its bytes are whatever the platform's
/// libm said and a digest over them would go red on a target nobody changed.
///
/// **The denominator, stated:** what follows is a pin on the *framing* — the
/// tag byte and the request's own fields — and on nothing else. The nested
/// payload's length still moves this test's `len` column if the framing's own
/// size changes, because the framing is measured as a prefix of the whole.
///
/// The match is exhaustive on purpose: a seventh job kind cannot reach the wire
/// without someone deciding here whether it nests a payload with a pin of its
/// own.
fn framing_of(request: &JobRequest) -> Vec<u8> {
    let bytes = request.to_bytes();
    let nested = match request {
        JobRequest::Radar { input, .. }
        | JobRequest::Section { input, .. }
        | JobRequest::Voxels { input, .. } => input.to_bytes().len(),
        // Opaque payloads: an archive or a Level III object, which this codec
        // frames and never interprets. There is nothing under them to pin
        // separately, so the whole buffer is framing as far as this is
        // concerned.
        JobRequest::Level3 { .. } | JobRequest::Level3Pair { .. } | JobRequest::Decode { .. } => 0,
        // Every byte is this codec's own: the geometry header and the sites
        // rows are both written by `encode_overlay_input`'s file, with no
        // nested payload carrying a pin of its own. The whole buffer is
        // framing.
        JobRequest::Overlay { .. } => 0,
    };
    bytes[..bytes.len() - nested].to_vec()
}

/// The framing this protocol version ships is **this** framing.
///
/// # What was blind, exactly
///
/// This encoding has no version and no magic — one tag byte and then the
/// variant's own fields. The number that governs it is `rustdar_web`'s
/// `PROTOCOL_VERSION`, through `build_token`, and the two guards standing over
/// that number both watch the *reply* direction: one asserts the literal in the
/// source (which fires only for the person who raises it), the other scrapes
/// the field names of a `done` message. `pwa_assets.rs` says so itself, in as
/// many words — "What this does not cover: the page->worker `job` direction".
/// This is that direction.
///
/// [`every_job_tag_is_the_literal_byte_it_ships_as`] pins the six tag bytes,
/// which is the first byte of each of these. Everything
/// after it — the two `f64` coordinates, the `u32` ceiling, the section's four
/// corners and its optional top, the voxel request's tagged half-extent and its
/// three axes — was unpinned, and every round-trip test in this file is written
/// against `to_bytes` and `from_bytes` together, so a same-width reorder made to
/// both in step passes all of them.
///
/// # A list and not one digest
///
/// Six rows rather than one hash of all of them, so the failure names the
/// variant. The diff then reads `- "voxels | 47 | 0x..."` beside the row that
/// moved, which is the sentence the author needs; a single number would say
/// only that something, somewhere, is different.
///
/// # What this cannot check
///
/// That `PROTOCOL_VERSION` was bumped. It is `#[cfg(target_arch = "wasm32")]`
/// in `rustdar-web`, which depends on this crate, so nothing here can name it.
/// What this can do is fail for the person who changes the framing, and say
/// what they owe.
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
        [
            "radar | 6 | 0x7d65cbf16a7b2ab7",
            "level3 | 28 | 0xff9d9dbb7736ea4e",
            "level3/vild | 34 | 0x4f9fb6a0901a3704",
            "section | 44 | 0x4fe286e53034800e",
            "voxels | 59 | 0x625ac8a3330ec11f",
            "voxels | 43 | 0xdbe9f7d560c79fd4",
            "decode | 30 | 0x2aad0634cde46e59",
            "overlay/sites | 131 | 0xf24942f4dd119e16",
            "overlay/alerts | 760 | 0xa89f5057b3b51a4a",
            "overlay/outlooks | 557 | 0x62486e4e14434bb0",
            "overlay/discussions | 264 | 0x3d28f1976f832a20",
        ]
        .map(str::to_string),
        "the framing `JobRequest::to_bytes` writes is not the framing protocol \
         version 10 shipped. Left is what this build posts; right is what this \
         list was last told. Something about a request's layout moved — a \
         field added, removed, reordered, retyped, or written at a different \
         width, or a tag renumbered. This encoding carries no version of its \
         own: the number that governs it is `PROTOCOL_VERSION` in \
         `rustdar-web/src/worker_protocol.rs`, folded into `build_token`. If \
         the change was deliberate, bump it there FIRST and then re-pin the \
         row here, in that order and never the numbers alone. A page and a \
         worker on opposite sides of a deploy share a build token whenever \
         `GITHUB_SHA` is absent (which it always is outside CI), and the \
         worker will read the new bytes in the old order: a job framed one way \
         and read another renders the wrong region, or the wrong product, or \
         fails to decode and strands the pane that posted it."
    );
}

// ── The overlay job ─────────────────────────────────────────────────────────

/// **The parity gate for the sites render: direct call and via-wire execution
/// are byte-identical.** This is what makes describing the job a move of the
/// same work rather than a second implementation of it — the wire decodes back
/// into the very struct the direct call takes
/// (`rustdar_overlays::render::rasterize::SitesInput`), and both paths run the
/// one rasterizer.
///
/// The fixture has real content and the test refuses to pass on an empty
/// raster: two identical all-zero buffers would satisfy `assert_eq!` while
/// proving only that nothing was drawn anywhere, so a positive painted-pixel
/// floor comes first. Perturbing the encoder — a byte order swapped, a field
/// dropped — fails this by name: the decoded input differs, the markers land
/// elsewhere or not at all, and the byte comparison reports it.
#[test]
fn the_sites_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest::Overlay {
        width,
        height,
        bounds,
        input,
    } = an_overlay_sites_job()
    else {
        unreachable!("the fixture is an overlay job");
    };
    let OverlayJobInput::Sites(sites) = input else {
        unreachable!("the fixture is a sites job");
    };

    let direct =
        rustdar_overlays::render::rasterize::rasterize_radar_sites(&sites, &bounds, width, height);
    // The premise the via-wire contract ("always premultiplied") rides on for
    // this kind: the direct path already answers premultiplied bytes, so the
    // wire's conversion arm is a no-op and identity is exact.
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

    let via_wire = execute_bytes(
        &JobRequest::Overlay {
            width,
            height,
            bounds,
            input: OverlayJobInput::Sites(sites),
        }
        .to_bytes(),
    )
    .and_then(JobOutput::overlay_raster)
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
}

/// Painted pixels — the non-vacuity floor every parity test below stands on:
/// two identical all-zero buffers satisfy `assert_eq!` while proving only
/// that nothing was drawn anywhere.
fn painted(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|px| px[3] != 0).count()
}

/// [`execute_bytes`] on an overlay job's own wire form, down to the raster.
fn overlay_raster_via_wire(job: &JobRequest) -> Vec<u8> {
    execute_bytes(&job.to_bytes())
        .and_then(JobOutput::overlay_raster)
        .expect("the described overlay job rasterizes")
}

/// **The parity gate for the alert render**, the sites gate's shape on the
/// kind whose inline rasterization was the measured 224 ms gesture-end stall:
/// direct call and via-wire execution are byte-identical, on the fixture
/// whose alert is a MultiPolygon with a hole — the geometry an encoder
/// flattens plausibly, and exactly what this comparison exists to catch.
#[test]
fn the_alerts_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest::Overlay {
        width,
        height,
        bounds,
        input,
    } = an_overlay_alerts_job()
    else {
        unreachable!("the fixture is an overlay job");
    };
    let OverlayJobInput::Alerts(alerts) = input else {
        unreachable!("the fixture is an alerts job");
    };

    let direct =
        rustdar_overlays::render::rasterize::rasterize_nws_alerts(&alerts, &bounds, width, height);
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

    let via_wire = overlay_raster_via_wire(&JobRequest::Overlay {
        width,
        height,
        bounds,
        input: OverlayJobInput::Alerts(alerts.clone()),
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
    // broken codec could zero without anyone noticing: un-hiding the third
    // alert must add painted pixels, since its square sits in an
    // otherwise-empty corner of the fixture.
    let unhidden = rustdar_overlays::render::rasterize::AlertsInput {
        hidden_ids: std::collections::HashSet::new(),
        ..*alerts
    };
    let more = overlay_raster_via_wire(&JobRequest::Overlay {
        width,
        height,
        bounds,
        input: OverlayJobInput::Alerts(Box::new(unhidden)),
    });
    assert!(
        painted(&more) > painted(&via_wire),
        "un-hiding an alert did not add pixels through the wire, so the \
         hidden-id set is not reaching the rasterizer and the parity above \
         says nothing about it",
    );
}

/// **The parity gate for the outlook render** — the kind with the pass the
/// others do not have: hatching. Byte-identity plus a floor of hatch-coloured
/// ink, so the hatch inputs (each feature's own pattern, and the
/// theme-resolved colour riding the job) are proven to travel rather than
/// assumed to.
#[test]
fn the_outlooks_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest::Overlay {
        width,
        height,
        bounds,
        input,
    } = an_overlay_outlooks_job()
    else {
        unreachable!("the fixture is an overlay job");
    };
    let OverlayJobInput::Outlooks(outlooks) = input else {
        unreachable!("the fixture is an outlooks job");
    };

    let direct = rustdar_overlays::render::rasterize::rasterize_spc_outlooks(
        &outlooks, &bounds, width, height,
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
    // it approaches: blue-dominant pixels are the hatch pass's own ink, and a
    // wire that dropped the pattern bytes or the colour would zero this.
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

    let via_wire = overlay_raster_via_wire(&JobRequest::Overlay {
        width,
        height,
        bounds,
        input: OverlayJobInput::Outlooks(outlooks),
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
/// floor, and a proof that *every row* travels: dropping the second MD from
/// the described input must lose pixels through the wire.
#[test]
fn the_discussions_render_is_byte_identical_direct_and_via_the_wire() {
    let JobRequest::Overlay {
        width,
        height,
        bounds,
        input,
    } = an_overlay_discussions_job()
    else {
        unreachable!("the fixture is an overlay job");
    };
    let OverlayJobInput::Discussions(discussions) = input else {
        unreachable!("the fixture is a discussions job");
    };

    let direct = rustdar_overlays::render::rasterize::rasterize_spc_discussions(
        &discussions,
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

    let via_wire = overlay_raster_via_wire(&JobRequest::Overlay {
        width,
        height,
        bounds,
        input: OverlayJobInput::Discussions(discussions.clone()),
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

    // Every row travels: the winter-weather MD sits apart from the
    // convective one, so a wire that lost the second row loses its pixels.
    let mut first_only = discussions;
    first_only.discussions.truncate(1);
    let fewer = overlay_raster_via_wire(&JobRequest::Overlay {
        width,
        height,
        bounds,
        input: OverlayJobInput::Discussions(first_only),
    });
    assert!(
        painted(&fewer) < painted(&via_wire),
        "dropping the second MD did not lose pixels through the wire, so the \
         row list is not reaching the rasterizer whole and the parity above \
         says nothing about it",
    );
}

/// The overlay reply's kind code and its raw round trip — plus the accessor
/// narrowness every other output kind already pins.
#[test]
fn an_overlay_reply_travels_as_its_own_out_kind() {
    let output = execute(&an_overlay_sites_job()).expect("the sites job draws");
    assert_eq!(output.out_kind(), Some(OUT_KIND_OVERLAY));

    // A frame consumer, a section consumer and a voxel consumer all see
    // "nothing to draw" — never a wrong-shaped buffer.
    assert!(
        execute(&an_overlay_sites_job())
            .and_then(JobOutput::frame)
            .is_none()
    );
    assert!(
        execute(&an_overlay_sites_job())
            .and_then(JobOutput::section)
            .is_none()
    );
    assert!(
        execute(&an_overlay_sites_job())
            .and_then(JobOutput::voxels)
            .is_none()
    );

    // The raw payload round-trips through `decode_output` unchanged: there is
    // no codec under code 5, deliberately — see `OUT_KIND_OVERLAY` — so what
    // goes in is exactly what comes out, and acceptance is the dispatcher's
    // length check rather than a magic.
    let rgba = output.overlay_raster().expect("an overlay raster");
    assert_eq!(
        decode_output(OUT_KIND_OVERLAY, &rgba),
        Some(JobOutput::OverlayRaster(rgba.clone())),
    );
    // And the accessor is as narrow as its four siblings.
    assert!(
        execute(&a_job())
            .and_then(JobOutput::overlay_raster)
            .is_none()
    );
}

/// The overlay job's malformed shapes: every truncation, a trailing byte, a
/// zero or absurd raster size, a flag outside `{0, 1}`, an input kind this
/// build does not have, and a site name that is not UTF-8 — each a clean
/// refusal, with a paired positive control proving the mutation landed where
/// this test believes it did.
#[test]
fn a_malformed_overlay_job_is_refused_rather_than_misread() {
    let job = an_overlay_sites_job();
    let bytes = job.to_bytes();

    // Control first: untouched bytes decode to the job itself, so every
    // refusal below is the mutation's doing.
    assert_eq!(JobRequest::from_bytes(&bytes), Some(job.clone()));

    // The site list is count-prefixed and nothing follows it, so a cut
    // anywhere is a refusal — including cuts that leave whole site rows.
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

    // A zero side, and a size the raster ceiling refuses: without the bound,
    // `execute` would allocate `width x height` pixels off a message port's
    // say-so. `u32::MAX` squared also proves the pixel arithmetic is checked
    // in a width that cannot wrap.
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

    // Offsets, stated once: tag(1) + width(4) + height(4) + bounds(32) = 41
    // is the input kind byte; + 1 + zoom(8) = 50 is `is_dark`.
    let mut bad_kind = bytes.clone();
    bad_kind[41] = 0;
    assert_eq!(
        JobRequest::from_bytes(&bad_kind),
        None,
        "input kind 0 must stay unallocated",
    );
    bad_kind[41] = 2;
    assert_eq!(
        JobRequest::from_bytes(&bad_kind),
        None,
        "input kind 2 is a build this one is not",
    );

    let mut bad_flag = bytes.clone();
    bad_flag[50] = 2;
    assert_eq!(
        JobRequest::from_bytes(&bad_flag),
        None,
        "is_dark is a bool, not a byte",
    );

    // The first site's name: 50 + 1 + count(4) + lat(8) + lon(8) + two flags
    // + name_len(2) = 79. Positive control first — a *different ASCII byte*
    // still decodes, proving 79 really is inside the name — then a byte no
    // UTF-8 string contains.
    let mut renamed = bytes.clone();
    renamed[79] = b'Q';
    match JobRequest::from_bytes(&renamed) {
        Some(JobRequest::Overlay {
            input: OverlayJobInput::Sites(sites),
            ..
        }) => assert_eq!(
            sites.sites[0].name, "QTLX",
            "byte 79 is not the first name byte; the refusal below would be \
             about some other field",
        ),
        other => panic!("the renamed control failed to decode: {other:?}"),
    }
    let mut bad_name = bytes;
    bad_name[79] = 0xFF;
    assert_eq!(
        JobRequest::from_bytes(&bad_name),
        None,
        "a site name that is not UTF-8 was accepted",
    );
}

/// Every truncation of an overlay job refused, and a trailing byte refused:
/// the shared half of the malformed suite, run per kind so a decoder arm
/// that started tolerating short or long buffers is named by its kind.
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
/// the shared walk; then the two byte positions this kind's own decoder
/// judges — a category code and a hidden-id's UTF-8 — each mutated to a
/// *valid different value* first, read back, and only then to the refused
/// one, so the refusal is proven to be about the byte this test believes
/// it is about.
#[test]
fn a_malformed_alerts_job_is_refused_rather_than_misread() {
    use rustdar_overlays::nws::alert::AlertCategory;
    let job = an_overlay_alerts_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets, stated once: tag(1) + width(4) + height(4) + bounds(32) = 41
    // is the input-kind byte; + 1 + device_scale(4) + category_count(4) = 50
    // is the first enabled-category code; the two categories, the hidden
    // count (4) and the first hidden id's length prefix (2) put the id's
    // first byte at 58.
    let first_category = 50;
    let first_hidden_byte = 58;

    // Positive control: a *different valid* category code decodes, and to the
    // category that code names — so 50 really is the category byte.
    let mut retagged = bytes.clone();
    assert_eq!(
        retagged[first_category], 0,
        "the fixture leads with Warning"
    );
    retagged[first_category] = 1;
    match JobRequest::from_bytes(&retagged) {
        Some(JobRequest::Overlay {
            input: OverlayJobInput::Alerts(alerts),
            ..
        }) => assert_eq!(
            alerts.enabled_categories[0],
            AlertCategory::Watch,
            "byte {first_category} is not the first category code; the \
             refusal below would be about some other field",
        ),
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
        Some(JobRequest::Overlay {
            input: OverlayJobInput::Alerts(alerts),
            ..
        }) => assert!(
            alerts.hidden_ids.iter().any(|id| id.starts_with('Q')),
            "byte {first_hidden_byte} is not the first hidden-id byte; the \
             refusal below would be about some other field",
        ),
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
/// this kind's own decoder judges — a feature's hatch code and its
/// geo-bounds option tag — each with the read-back control the alert test
/// describes.
#[test]
fn a_malformed_outlooks_job_is_refused_rather_than_misread() {
    use rustdar_overlays::types::HatchPattern;
    let job = an_overlay_outlooks_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets, stated once: the fixed header and input-kind byte end at 42;
    // + device_scale(4) + hatch_color(4) + feature_count(4) = 54 opens the
    // first feature; its labels are "SLGT" (2+4) and "Slight Risk" (2+11),
    // then fill(4) + stroke(4) put the hatch code at 81 and the geo-bounds
    // option tag at 82.
    let first_hatch = 54 + 2 + 4 + 2 + 11 + 4 + 4;
    let bounds_tag = first_hatch + 1;

    let mut rehatched = bytes.clone();
    assert_eq!(
        rehatched[first_hatch], 0,
        "the fixture's first feature is unhatched"
    );
    rehatched[first_hatch] = 2;
    match JobRequest::from_bytes(&rehatched) {
        Some(JobRequest::Overlay {
            input: OverlayJobInput::Outlooks(outlooks),
            ..
        }) => assert_eq!(
            outlooks.features[0].hatch,
            HatchPattern::Cig2,
            "byte {first_hatch} is not the first hatch code; the refusal \
             below would be about some other field",
        ),
        other => panic!("the rehatched control failed to decode: {other:?}"),
    }
    rehatched[first_hatch] = 4;
    assert_eq!(
        JobRequest::from_bytes(&rehatched),
        None,
        "hatch code 4 is a build this one is not",
    );

    // The option tag: 1 on the fixture (`OverlayFeature::new` computes its
    // AABB), read back as `Some` — then a tag outside {0, 1}.
    let mut untagged = bytes;
    assert_eq!(
        untagged[bounds_tag], 1,
        "the fixture's feature carries its AABB"
    );
    match JobRequest::from_bytes(&untagged) {
        Some(JobRequest::Overlay {
            input: OverlayJobInput::Outlooks(outlooks),
            ..
        }) => assert!(
            outlooks.features[0].geo_bounds.is_some(),
            "byte {bounds_tag} is not the geo-bounds tag; the refusal below \
             would be about some other field",
        ),
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
/// this kind's own decoder judges — the MD type — with its read-back control.
#[test]
fn a_malformed_discussions_job_is_refused_rather_than_misread() {
    use rustdar_overlays::spc::discussion::MdType;
    let job = an_overlay_discussions_job();
    assert_refuses_cuts_and_trailing(&job);
    let bytes = job.to_bytes();

    // Offsets, stated once: the fixed header and input-kind byte end at 42;
    // + device_scale(4) + md_count(4) = 50 is the first MD's type code.
    let first_md_type = 50;

    let mut retyped = bytes;
    assert_eq!(retyped[first_md_type], 0, "the fixture leads Convective");
    retyped[first_md_type] = 1;
    match JobRequest::from_bytes(&retyped) {
        Some(JobRequest::Overlay {
            input: OverlayJobInput::Discussions(discussions),
            ..
        }) => assert_eq!(
            discussions.discussions[0].md_type,
            MdType::WinterWeather,
            "byte {first_md_type} is not the first MD-type code; the refusal \
             below would be about some other field",
        ),
        other => panic!("the retyped control failed to decode: {other:?}"),
    }
    retyped[first_md_type] = 3;
    assert_eq!(
        JobRequest::from_bytes(&retyped),
        None,
        "MD-type code 3 is a build this one is not",
    );
}

/// On native, a described overlay job rides the pool's **interactive** lane —
/// the one the overlay closures have always ridden — and a radar job rides the
/// described lane, so an overlay can never queue behind a slate of radar
/// renders. The two halves are one test because either alone proves nothing:
/// every job on `rd-opaque` would pass the first, and every job on `rd-job`
/// the second.
///
/// Asserted by the name of the thread `deliver` runs on, which is the pool's
/// own statement of which lane executed the job.
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
    let radar_lane = lane_of(a_job());
    assert!(
        radar_lane.starts_with("rd-job"),
        "the radar control delivered on {radar_lane}, so the routing is not \
         by deadline and the assertion above proves nothing",
    );
}
