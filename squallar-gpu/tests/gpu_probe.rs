//! The GPU pass probe's own two obligations, held on a real adapter.
//!
//! ```text
//! cargo test -p squallar-gpu --test gpu_probe -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d and needing **no file on anyone's disk** — the volume is an
//! 8³ zero grid under an opaque LUT, the same stand-in `volume_building_cost`
//! marches. The gpu job picks this suite up through its derived target list.
//!
//! **Presence** (any adapter with `TIMESTAMP_QUERY` — lavapipe included, whose
//! `timestampComputeAndGraphics` is true): the probe's counts equal the passes
//! actually encoded, a family's bracket is handed out exactly once per frame
//! however many passes ask, every collected sample comes from monotone stamps,
//! and a family that never ran holds zero everything.
//!
//! **Absence** (the same adapter, device requested with `Features::empty()` —
//! the shape of every WebGL2 leg and every install that never asked): the
//! probe declines to exist, and the frame's whole encode path runs inside a
//! validation error scope on a device where **any** query-set creation,
//! timestamp write or resolve is a validation error. The clean scope is the
//! zero-query-submissions count — a probe that ignored the feature gate, or a
//! call site that stopped honouring the `Option`, reddens it.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_egui::pane::OrbitCamera;
use squallar_egui::volume_view::view_for;
use squallar_gpu::gpu_probe::{GpuPassProbe, PROBED_PASSES, ProbedPass};
use squallar_volumetric::raymarch::staging::{STAGING_RING_FEATURE, VolumeStaging};
use squallar_volumetric::raymarch::{
    OffscreenPlan, OffscreenTarget, VolumePipelines, VolumeTextures,
};
use squallar_volumetric::uniform::VolumeUniform;

mod gpu_harness;
use gpu_harness::{attachments, gpu_lock, opaque_white_lut};

/// Frames driven through the probe; every one must land as a sample.
const FRAMES: u64 = 12;

/// The box and camera of the stand-in scene. One camera — this suite checks
/// the instrument, not the march, and `volume_march_cost` owns the cost sweep.
const BOX_KM: [f32; 3] = [2.0, 2.0, 0.4];
const CAMERA: (f32, f32, f32, f32) = (140.0, 35.0, 0.9, 3.0);

/// The 8³ stand-in the march is pointed at, on whichever device the test made.
fn stand_in_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> (OffscreenTarget, VolumeTextures) {
    let cells = [8u32, 8, 8];
    let volume = pipelines
        .upload_volume(
            device,
            queue,
            cells,
            &vec![0u8; 8 * 8 * 8],
            &opaque_white_lut(),
            &mut VolumeStaging::new(device),
        )
        .expect("the stand-in grid and palette were refused");
    let size = [320u32, 200];
    let target = pipelines.create_offscreen(device, OffscreenPlan::native(size));
    let camera = OrbitCamera::restore(CAMERA.0, CAMERA.1, CAMERA.2, [0.0; 3], CAMERA.3)
        .expect("a finite camera");
    let view = view_for(camera, BOX_KM, size[0] as f32 / size[1] as f32)
        .expect("the camera must be viewable");
    let mut uniform = VolumeUniform::new(BOX_KM, cells);
    uniform.box_from_clip = view.box_from_clip;
    uniform.clip_from_box = view.clip_from_box;
    uniform.eye_in_box = view.eye_in_box;
    uniform.vertical_exaggeration = camera.vertical_exaggeration();
    volume.write_uniform(queue, &uniform);
    (target, volume)
}

#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn the_probe_counts_what_it_brackets_and_brackets_once_per_frame() {
    let _guard = gpu_lock();
    let (device, queue) = device_with_timestamps();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);
    let (target, volume) = stand_in_scene(&device, &queue, &pipelines);

    let mut probe = GpuPassProbe::new(&device, &queue)
        .expect("this device was created with TIMESTAMP_QUERY and the probe refused it");
    let handle = probe.handle();

    for frame in 0..FRAMES {
        let mut encoder = device.create_command_encoder(&Default::default());

        // Two raymarch passes a frame — the six-pane shape scaled down. The
        // FIRST ask takes the frame's bracket; the second is counted and
        // told "already bracketed", which is what keeps one query index from
        // being written twice in a frame.
        let first = handle.pass_timestamps(ProbedPass::Raymarch);
        assert!(
            first.is_some(),
            "frame {frame}: the frame's first raymarch ask did not get the bracket",
        );
        pipelines.encode_raymarch_with_timestamps(&mut encoder, &target, &volume, None, first);
        let second = handle.pass_timestamps(ProbedPass::Raymarch);
        assert!(
            second.is_none(),
            "frame {frame}: the bracket was handed out twice in one frame — \
             two passes would write the same query indices",
        );
        pipelines.encode_raymarch_with_timestamps(&mut encoder, &target, &volume, None, second);

        probe.end_frame(&mut encoder);
        queue.submit(Some(encoder.finish()));
        // The app's collect never blocks; the poll between these two is this
        // test buying determinism, not the production wiring.
        probe.collect();
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("polling the device failed");
        probe.collect();
    }

    let report = probe.report();
    assert_eq!(
        report.passes(ProbedPass::Raymarch),
        2 * FRAMES,
        "every encoded pass must be counted, bracketed or not",
    );
    assert_eq!(
        report.hist(ProbedPass::Raymarch).total(),
        FRAMES,
        "one bracketed sample per frame must have been collected",
    );
    assert_eq!(
        report.frames, FRAMES,
        "every frame's resolve must have landed"
    );
    assert_eq!(
        report.hist(ProbedPass::Raymarch).counts()[squallar_device_profile::hist::SLOTS - 1],
        0,
        "a sample in the over-64ms clamp from an 8-cubed march means a \
         non-monotone stamp pair wrapped into a huge duration",
    );
    for family in [ProbedPass::Ground, ProbedPass::Mirror, ProbedPass::Main] {
        assert_eq!(
            report.passes(family),
            0,
            "{family:?} never ran and must not have been counted",
        );
        assert_eq!(
            report.hist(family).total(),
            0,
            "{family:?} never ran and must hold no sample",
        );
    }
    let period = queue.get_timestamp_period();
    println!(
        "probe presence path on this adapter (timestamp period {period} ns): \
         {FRAMES} frames, raymarch p50 {:?} us, p99 {:?} us",
        report
            .hist(ProbedPass::Raymarch)
            .percentile_upper_micros(0.50),
        report
            .hist(ProbedPass::Raymarch)
            .percentile_upper_micros(0.99),
    );
}

#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn a_featureless_device_gets_no_probe_and_the_frame_submits_zero_query_operations() {
    let _guard = gpu_lock();
    let (device, queue) = device_without_features();

    // Everything from here to the pop runs under the scope: on this device a
    // query-set creation, a timestamp write or a resolve IS a validation
    // error, so "no error" is a count of query submissions and the count is
    // zero. This is the same `Option`-gated wiring the app runs.
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let mut probe = GpuPassProbe::new(&device, &queue);
    assert!(
        probe.is_none(),
        "the probe must decline a device without TIMESTAMP_QUERY; building \
         one here would be the validation error the scope below exists to catch",
    );

    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);
    let (target, volume) = stand_in_scene(&device, &queue, &pipelines);

    let mut encoder = device.create_command_encoder(&Default::default());
    for pass in PROBED_PASSES {
        assert!(
            probe
                .as_ref()
                .and_then(|p: &GpuPassProbe| p.pass_timestamps(pass))
                .is_none(),
            "an absent probe answered a {pass:?} bracket",
        );
    }
    let stamps = probe
        .as_ref()
        .and_then(|p| p.pass_timestamps(ProbedPass::Raymarch));
    pipelines.encode_raymarch_with_timestamps(&mut encoder, &target, &volume, None, stamps);
    if let Some(p) = probe.as_mut() {
        p.end_frame(&mut encoder);
    }
    queue.submit(Some(encoder.finish()));
    if let Some(p) = probe.as_mut() {
        p.collect();
    }

    let error = pollster::block_on(scope.pop());
    assert!(
        error.is_none(),
        "the frame submitted a query operation on a device without \
         TIMESTAMP_QUERY: {error:?}",
    );
}

/// A device that can answer timestamp queries, or a panic naming the feature.
/// The same request `volume_building_cost` makes; lavapipe grants it.
fn device_with_timestamps() -> (wgpu::Device, wgpu::Queue) {
    let (adapter, descriptor) = adapter_and_descriptor();
    assert!(
        adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY),
        "this adapter cannot answer timestamp queries, so there is nothing to measure with"
    );
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TIMESTAMP_QUERY
            | (adapter.features() & STAGING_RING_FEATURE),
        ..descriptor
    }))
    .expect("could not create a device on an adapter that was found")
}

/// The same adapter, asked for a device with no features at all — the shape
/// every WebGL2 leg and every never-asked install runs.
fn device_without_features() -> (wgpu::Device, wgpu::Queue) {
    let (adapter, descriptor) = adapter_and_descriptor();
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::empty(),
        ..descriptor
    }))
    .expect("could not create a device on an adapter that was found")
}

fn adapter_and_descriptor() -> (wgpu::Adapter, wgpu::DeviceDescriptor<'static>) {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no wgpu adapter; this test is ignored by default for that reason");
    gpu_harness::announce(&adapter);
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("squallar.gpu_probe.test.device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
        experimental_features: Default::default(),
        trace: Default::default(),
    };
    (adapter, descriptor)
}
