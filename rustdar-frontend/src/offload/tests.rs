use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// What a `FakePort` recorded: the id it was given and the bytes it was
/// asked to post, in order.
type Posted = Arc<std::sync::Mutex<Vec<(u64, Vec<u8>)>>>;

/// A port that records what it was handed instead of posting anywhere.
struct FakePort {
    posted: Posted,
    accept: bool,
}

impl WorkerPort for FakePort {
    fn post(&self, id: u64, request: Vec<u8>) -> bool {
        if self.accept {
            self.posted.lock().unwrap().push((id, request));
        }
        self.accept
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

/// Every test shares one thread-local port and pending map, and `cargo
/// test` runs them concurrently on separate threads — which is precisely
/// why the state is thread-local. Each test still tears down so a panic
/// mid-test cannot leak a port into the next one on the same thread.
fn detach() {
    abandon_worker("test teardown");
}

/// A job that is cheap to execute and easy to recognize. It renders
/// nothing, which is fine: the funnel's contract is about *where* and
/// *whether* `deliver` runs, not what the renderer drew.
fn a_job() -> JobRequest {
    JobRequest::Radar {
        input: Box::new(
            RenderInput::from_bytes(&sample_input_bytes()).expect("fixture payload decodes"),
        ),
        values_wanted: true,
        full_res: true,
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
        full_res: true,
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
        full_res: true,
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

fn a_section_job() -> JobRequest {
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

fn a_voxel_job() -> JobRequest {
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

/// Every tag is distinct, and the two new ones are **not 4**.
///
/// 4 is [`TAG_LEVEL3_PAIR`], and because the new names are new consts
/// nothing would have stopped the build. Worse, nothing would have stopped
/// it at *runtime* either: that arm reads two `f64`s and a `u32` length and
/// then takes the rest, which on a section's plausible bytes succeeds — so
/// a section posted as tag 4 comes back as a VIL-density job built out of
/// cross-section geometry, and renders. The assertion below is the whole
/// guard, and it is cheap because the alternative is invisible.
#[test]
fn no_two_job_tags_collide() {
    let tags = [
        TAG_RADAR,
        TAG_LEVEL3,
        TAG_SRM_RETIRED,
        TAG_LEVEL3_PAIR,
        TAG_SECTION,
        TAG_VOXELS,
    ];
    let mut seen = std::collections::HashSet::new();
    for tag in tags {
        assert!(seen.insert(tag), "tag {tag} is used twice");
    }
    assert_ne!(TAG_SECTION, TAG_LEVEL3_PAIR);
    assert_ne!(TAG_VOXELS, TAG_LEVEL3_PAIR);
    // And the framing really is tag-first, so the byte asserted above is
    // the byte a decoder switches on.
    assert_eq!(a_section_job().to_bytes()[0], TAG_SECTION);
    assert_eq!(a_voxel_job().to_bytes()[0], TAG_VOXELS);
}

/// Every job tag is pinned to the literal byte it ships as.
///
/// [`no_two_job_tags_collide`] above asserts distinctness, and the round
/// trip asserts the two ends agree — but **both survive a renumbering**,
/// because both read the constants they are checking. Swap
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
    let table: [(&str, u8, u8); 6] = [
        ("TAG_RADAR", TAG_RADAR, 1),
        ("TAG_LEVEL3", TAG_LEVEL3, 2),
        ("TAG_SRM_RETIRED", TAG_SRM_RETIRED, 3),
        ("TAG_LEVEL3_PAIR", TAG_LEVEL3_PAIR, 4),
        ("TAG_SECTION", TAG_SECTION, 5),
        ("TAG_VOXELS", TAG_VOXELS, 6),
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
    let framing: [(JobRequest, u8); 5] = [
        (a_job(), 1),
        (a_level3_job(), 2),
        (a_level3_pair_job(), 4),
        (a_section_job(), 5),
        (a_voxel_job(), 6),
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
    // A seventh kind added without a line in the table above makes 7
    // decode, and this is what says so.
    let mut bytes = a_voxel_job().to_bytes();
    for unallocated in [0u8, 7] {
        bytes[0] = unallocated;
        assert_eq!(
            JobRequest::from_bytes(&bytes),
            None,
            "tag {unallocated} decodes, so the table above has stopped \
                 being the whole wire",
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
    assert_eq!(
        section.view(),
        rustdar_radar::types::RenderView::CrossSection
    );
    assert!(section.section().is_some());

    let voxels = execute(&a_voxel_job()).expect("the voxel job builds");
    assert_eq!(voxels.view(), rustdar_radar::types::RenderView::Volume);
    let grid = voxels.voxels().expect("the voxel job answers a grid");
    assert_eq!(grid.shape().cells(), 8 * 6 * 4);

    // And the same jobs off the wire, which is the path a worker takes.
    assert_eq!(
        execute_bytes(&a_section_job().to_bytes()).map(|o| o.view()),
        Some(rustdar_radar::types::RenderView::CrossSection),
    );
    assert_eq!(
        execute_bytes(&a_voxel_job().to_bytes()).map(|o| o.view()),
        Some(rustdar_radar::types::RenderView::Volume),
    );
}

/// A frame consumer handed an output of another kind sees `None` — the
/// "nothing to draw" every render path already handles — and **never** a
/// wrong-shaped buffer.
///
/// This is the accessor the whole widening rests on. `RenderedFrame` was
/// deliberately not given a width and a height, so every consumer of one reads
/// its side back off the buffer against the closed set of *square* plan-view
/// sizes; a section's raster is not square and would be refused, but a refusal
/// is a blank pane, and this is what keeps one from ever getting that far.
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
    // Both flags are booleans, and a byte outside `{0, 1}` is a build whose
    // protocol is not this one — refused rather than guessed at, on each of
    // the three variants that carries one.
    assert_eq!(
        JobRequest::from_bytes(&[TAG_RADAR, 2, 1]),
        None,
        "values_wanted is a bool, not a byte"
    );
    assert_eq!(
        JobRequest::from_bytes(&[TAG_RADAR, 1, 2]),
        None,
        "full_res is a bool, not a byte"
    );
    for (tag, what) in [(TAG_LEVEL3, "level3"), (TAG_LEVEL3_PAIR, "level3 pair")] {
        assert_eq!(
            JobRequest::from_bytes(&[tag, 2]),
            None,
            "{what}: full_res is a bool, not a byte"
        );
    }

    // A length prefix that claims more than the payload holds must be
    // refused, not read as a short object: the pair's first length is the
    // one number on the wire that could lie. Byte 18, not 17: the tag, the
    // `full_res` flag and two `f64`s precede it.
    let mut overlong = a_level3_pair_job().to_bytes();
    overlong[18] = 0xFF;
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

    let mut bad_product = a_level3_job().to_bytes();
    bad_product[2] = 0xFE;
    bad_product[3] = 0xFF;
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

/// With no worker installed, `offload_job` is the old behaviour: the job
/// runs and `deliver` sees its result.
#[test]
fn without_a_worker_the_job_runs_here() {
    detach();
    let (tx, rx) = mpsc::channel();
    offload_job("test", Job::Described(a_job()), move |result| {
        let _ = tx.send(result.is_some());
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(true),
        "the inline arm must deliver the rendered frame"
    );
    assert_eq!(jobs_in_worker(), 0);
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
    deliver_worker_reply(id, None);
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
    deliver_worker_reply(
        id,
        Some(JobOutput::Frame(RenderedFrame {
            image: vec![0; 4],
            max_range_km: 230.0,
            values: vec![f32::NAN],
            nyquist_ms: None,
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
    deliver_worker_reply(id, None);
    assert_eq!(
        count.load(Ordering::Relaxed),
        1,
        "a late reply must not deliver a second response for one render"
    );
}
