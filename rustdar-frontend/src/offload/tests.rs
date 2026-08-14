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
    let table: [(&str, u8, u8); 7] = [
        ("TAG_RADAR", TAG_RADAR, 1),
        ("TAG_LEVEL3", TAG_LEVEL3, 2),
        ("TAG_SRM_RETIRED", TAG_SRM_RETIRED, 3),
        ("TAG_LEVEL3_PAIR", TAG_LEVEL3_PAIR, 4),
        ("TAG_SECTION", TAG_SECTION, 5),
        ("TAG_VOXELS", TAG_VOXELS, 6),
        ("TAG_DECODE", TAG_DECODE, 7),
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
    let framing: [(JobRequest, u8); 6] = [
        (a_job(), 1),
        (a_level3_job(), 2),
        (a_level3_pair_job(), 4),
        (a_section_job(), 5),
        (a_voxel_job(), 6),
        (a_decode_job(), 7),
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
    // An eighth kind added without a line in the table above makes 8
    // decode, and this is what says so. **7 left this list when the decode
    // job took it**, which is the whole point of the list: a new kind cannot
    // be added without coming here and saying so.
    let mut bytes = a_voxel_job().to_bytes();
    for unallocated in [0u8, 8] {
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
        ]
        .map(str::to_string),
        "the framing `JobRequest::to_bytes` writes is not the framing protocol \
         version 8 shipped. Left is what this build posts; right is what this \
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
