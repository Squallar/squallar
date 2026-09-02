//! The half of the probe that touches a device. wasm32 only.
//!
//! A **second** wgpu instance on `navigator.gpu`, a second adapter and a
//! throwaway device: the application's own adapter cannot be reused, since a
//! `GPUAdapter` is consumed by the one `requestDevice` that already took it.
//! Everything here goes through wgpu rather than raw `web_sys::Gpu*` bindings,
//! so the crate carries no new `web-sys` feature for it — the WebGPU backend
//! surfaces `GPUOutOfMemoryError` as `wgpu::Error::OutOfMemory` out of a
//! popped `ErrorFilter::OutOfMemory` scope, and device loss through
//! `Device::set_device_lost_callback`.
//!
//! Every step runs inside two error scopes, out-of-memory and validation. The
//! first is the reading. The second is there so that the validation errors a
//! refused texture goes on to produce — an invalid view, an invalid pass, an
//! invalid submit — are popped and dropped rather than reaching the device's
//! uncaptured-error path; the probe installs no handler there, so anything
//! that did reach it would go to the browser console and nowhere else. Only
//! those two filters are ever pushed: wgpu's WebGPU backend maps a popped
//! `GPUInternalError` to a panic, and a probe that took the tab down would be
//! worse than no probe.
//!
//! The result is parked in a thread-local the bridge reads on the telemetry
//! tick — the same delivery the rasterization worker's heap reading takes —
//! because the app has no channel receiver to spare for it.

use super::{Allocation, Probe, ProbeOutcome, ProbePlan, StepResult, capacity_from};
use egui_wgpu::wgpu;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    /// What the probe found, once it has. The page thread owns the bridge and
    /// runs the probe's future, so one thread writes and reads this.
    static OUTCOME: Cell<Option<ProbeOutcome>> = const { Cell::new(None) };
}

/// The outcome, once the probe has one; `None` while it is still running or
/// was never started.
pub fn outcome() -> Option<ProbeOutcome> {
    OUTCOME.with(Cell::get)
}

/// Start the probe. Returns at once: the work is a spawned future that awaits
/// the browser between allocations, so the page keeps its frames.
pub fn start() {
    wasm_bindgen_futures::spawn_local(probe());
}

fn now_ms() -> u64 {
    // `Date.now()` is a whole number of milliseconds; the cast truncates
    // nothing that matters to a two-second budget.
    js_sys::Date::now() as u64
}

async fn probe() {
    // The same base descriptor the application's own instance is built from,
    // narrowed to WebGPU alone: this instance never meets the canvas, so the
    // detecting constructor and the WebGL2 half of the mask have no part here.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU,
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    });
    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
    {
        Ok(adapter) => adapter,
        Err(error) => {
            log::info!("gpu probe: no second adapter ({error}); the presumption stands");
            OUTCOME.set(Some(ProbeOutcome::default()));
            return;
        }
    };
    // The same limit set the application's own web device asks for: the
    // WebGL2 downlevel floor with the adapter's resolution, which is what a
    // WebGPU adapter is known to grant here.
    let limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
    let (device, queue) = match adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("squallar gpu probe"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            ..Default::default()
        })
        .await
    {
        Ok(pair) => pair,
        Err(error) => {
            log::info!("gpu probe: no throwaway device ({error}); the presumption stands");
            OUTCOME.set(Some(ProbeOutcome::default()));
            return;
        }
    };

    let lost = Arc::new(AtomicBool::new(false));
    {
        let lost = Arc::clone(&lost);
        device.set_device_lost_callback(move |_reason, _message| {
            lost.store(true, Ordering::Relaxed);
        });
    }

    let granted = device.limits();
    let plan = ProbePlan::for_adapter(
        granted.max_texture_dimension_2d,
        granted.max_texture_array_layers,
    );
    let mut probe = Probe::new(plan);
    let mut held: Vec<wgpu::Texture> = Vec::new();
    let started = now_ms();
    while let Some(allocation) = probe.next(now_ms().saturating_sub(started)) {
        let step_started = now_ms();
        let result = step(&device, &queue, &allocation, &mut held).await;
        // A lost device answers every later call as if it succeeded, so the
        // flag outranks the scope's silence.
        let result = if lost.load(Ordering::Relaxed) {
            StepResult::Lost
        } else {
            result
        };
        probe.record(allocation, result, now_ms().saturating_sub(step_started));
    }
    for texture in &held {
        texture.destroy();
    }
    held.clear();
    device.destroy();

    let outcome = probe.finish(now_ms().saturating_sub(started));
    if capacity_from(&outcome).is_none() {
        log::info!(
            "gpu probe: nothing held ({} steps, {} ms); the presumption stands",
            outcome.steps,
            outcome.elapsed_ms,
        );
    }
    OUTCOME.set(Some(outcome));
}

/// One allocation: create the texture, clear every layer in its own render
/// pass so the memory is resident and not merely reserved, submit, and read
/// the out-of-memory scope. A held texture joins `held`; a refused one is
/// destroyed here.
async fn step(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    allocation: &Allocation,
    held: &mut Vec<wgpu::Texture>,
) -> StepResult {
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("squallar gpu probe"),
        size: wgpu::Extent3d {
            width: allocation.width,
            height: allocation.height,
            depth_or_array_layers: allocation.layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("squallar gpu probe"),
    });
    for layer in 0..allocation.layers {
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        });
        // The pass is the clear: begun with `LoadOp::Clear`, ended at once.
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("squallar gpu probe clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        drop(pass);
    }
    queue.submit([encoder.finish()]);

    // Popped in reverse order of pushing, as wgpu requires. The
    // out-of-memory scope is the reading; the validation scope is drained
    // and discarded — see the module doc.
    let refused = out_of_memory.pop().await.is_some();
    let _ = validation.pop().await;

    if refused {
        texture.destroy();
        StepResult::Refused
    } else {
        held.push(texture);
        StepResult::Held
    }
}
