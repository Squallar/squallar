//! The browser's [`PlatformBridge`]. Most of the trait is capabilities a tab
//! does not have, so most of this file is honest `None`s.

use egui_wgpu::wgpu;
use squallar_app::platform::{
    GpuProbeReport, HostSignals, LinearMemory, PlatformBridge, ProbedCapacity, gpu_probe_applies_to,
};
use winit::platform::web::WindowAttributesExtWebSys;

const DARK_SCHEME_QUERY: &str = "(prefers-color-scheme: dark)";

/// Which instrument a page's probe is, by the backend the application draws
/// with ([`gpu_probe_applies_to`] says which backends have one at all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Instrument {
    /// `crate::gpu_probe::run`: a throwaway WebGPU device inside error scopes,
    /// walked to the 8 GiB ceiling.
    WebGpu,
    /// `crate::gpu_probe::webgl2_run`: raw WebGL2 on a second canvas, walked
    /// to a policy cap.
    WebGl2,
}

/// Where the browser probe stands, as the bridge has driven it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuProbe {
    /// Nobody has asked yet.
    NotStarted,
    /// Started; the instrument's outcome cell is empty until it lands.
    Running(Instrument),
    /// Reported, or skipped: this is the bridge's last word, re-said on
    /// every later ask so the level line can carry it.
    Settled(GpuProbeReport),
}

pub struct WebPlatform {
    /// The canvas the winit window is bound to. Held because `window_attributes`
    /// is called after construction, on `resumed`.
    canvas: web_sys::HtmlCanvasElement,
    /// Last theme reported to the app, so `poll_theme` can answer "changed?".
    last_theme: Option<bool>,
    /// See [`GpuProbe`].
    gpu_probe: GpuProbe,
}

impl WebPlatform {
    pub fn new(canvas: web_sys::HtmlCanvasElement) -> Self {
        Self {
            canvas,
            last_theme: None,
            gpu_probe: GpuProbe::NotStarted,
        }
    }
}

impl PlatformBridge for WebPlatform {
    fn poll_theme(&mut self) -> Option<bool> {
        let current = self.detect_dark_theme();
        // Report only transitions: the app bumps every cached texture when this
        // returns `Some`.
        if self.last_theme == Some(current) {
            return None;
        }
        self.last_theme = Some(current);
        Some(current)
    }

    fn detect_dark_theme(&self) -> bool {
        // A browser too old for `matchMedia`, or a failed query, is treated as light.
        web_sys::window()
            .and_then(|w| w.match_media(DARK_SCHEME_QUERY).ok().flatten())
            .is_some_and(|list| list.matches())
    }

    /// `DeviceOrientationEvent` needs a secure context and, on iOS, a separate
    /// gesture-gated permission.
    fn poll_heading(&mut self) -> Option<f32> {
        None
    }

    /// A tab has no system bars to inset around.
    fn query_insets(&self) -> Option<(f32, f32, f32, f32)> {
        None
    }

    /// Nothing consumes a back gesture, so the app's own handling stands.
    fn handle_back(&self) -> bool {
        false
    }

    fn set_back_handler(&mut self, _handler: fn()) {}

    /// No filesystem, so no zone cache.
    fn set_zone_cache_dir(&mut self, _dir: std::path::PathBuf) {}

    fn zone_cache_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// No filesystem for the archive block cache either: it stays disabled,
    /// and the browser's HTTP cache is this target's persistence story.
    fn set_basemap_cache_dir(&mut self, _dir: std::path::PathBuf) {}

    fn basemap_cache_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// No filesystem for downloaded areas either: the web build's offline
    /// store is the service-worker cache route, which never surfaces as a
    /// directory. Answering `None` keeps the Gui's copy `None` on wasm.
    fn set_basemap_dir(&mut self, _dir: std::path::PathBuf) {}

    fn basemap_dir(&self) -> Option<&std::path::Path> {
        None
    }

    /// Inert, not an oversight: `localStorage` is available from the first frame.
    fn set_config_dir(&mut self, _dir: std::path::PathBuf) {}

    fn iana_timezone(&self) -> Option<String> {
        browser_timezone()
    }

    fn kv(&self) -> Option<Box<dyn squallar_kv::KvStore>> {
        crate::kv::LocalStorageKvStore::new()
            .map(|store| Box::new(store) as Box<dyn squallar_kv::KvStore>)
    }

    /// There is no process to exit; the event loop stopping is all there is.
    fn needs_process_exit(&self) -> bool {
        false
    }

    // The browser's location service lives in `squallar_location::web`: the
    // permission is asked about before anything is asked *for*, and the watch is
    // started only from `request_location`.

    /// What the browser will say about the machine: a declared RAM bucket
    /// (Chromium exposes one, Firefox does not), the thread count the
    /// hardware reports, the form factor from pointer media, and the ceiling
    /// this page's linear memory was built with. Measured RAM is `None` on
    /// every browser, because no API answers.
    ///
    /// **Ordering**: this is called from `App::new`, at the end of boot, and
    /// three of the four signals could be read there. The heap ceiling could
    /// not — the memory it describes was constructed before the module was
    /// instantiated — which is why it travels as a value plumbed in through
    /// `entry::start` rather than as a read here.
    fn host_signals(&self) -> HostSignals {
        HostSignals {
            system_ram_bytes: None,
            declared_ram_bytes: declared_ram_bytes(),
            // Chosen per device by `heap.js` before this module existed and
            // handed to `entry::start`; see `crate::heap_max` for why it
            // cannot be read back off the memory object.
            linear_memory_max_bytes: crate::heap_max::this_instance(),
            parallelism: navigator_number("hardwareConcurrency")
                .filter(|n| *n >= 1.0)
                .map(|n| n as usize),
            form_factor: form_factor(),
        }
    }

    /// This page's heap, and the rasterization worker's as it last reported
    /// — two instances under two ceilings, see [`LinearMemory`].
    fn linear_memory(&self) -> Option<LinearMemory> {
        Some(LinearMemory {
            page_bytes: crate::shared_loan::memory_bytes()?,
            page_max_bytes: crate::heap_max::this_instance().unwrap_or(0),
            worker_bytes: crate::worker_port::worker_memory_bytes(),
            worker_live_bytes: crate::worker_port::worker_live_bytes(),
            // What the worker reported on its hello where it has said, else
            // what this page asked for it. A zero is "nobody said", which the
            // watermark spells `Quiet` rather than guessing a wall.
            worker_max_bytes: crate::heap_max::worker_instance().unwrap_or(0),
        })
    }

    /// The browser probe, driven from the app's asks. The first ask starts
    /// the instrument the app's own backend calls for — the WebGPU probe on
    /// `BrowserWebGpu`, the raw-WebGL2 probe on `Gl`, walked to the policy
    /// cap for this page's form factor — and logs a skip once for any other
    /// backend (`crate::gpu_probe`, both arms). Later asks read that
    /// instrument's outcome cell; once it has landed the same report is
    /// re-said on every ask, so the app's level line can carry it after the
    /// once-only lines are gone from the console ring. A probe that reached
    /// no figure settles as `Empty`, and the presumption stands.
    fn gpu_probe_report(&mut self, backend: wgpu::Backend) -> GpuProbeReport {
        match self.gpu_probe {
            GpuProbe::NotStarted => {
                if !gpu_probe_applies_to(backend) {
                    log::info!("gpu probe: skipped (backend {backend:?})");
                    self.gpu_probe = GpuProbe::Settled(GpuProbeReport::Skipped);
                    return GpuProbeReport::Skipped;
                }
                let instrument = if backend == wgpu::Backend::BrowserWebGpu {
                    Instrument::WebGpu
                } else {
                    Instrument::WebGl2
                };
                match instrument {
                    Instrument::WebGpu => crate::gpu_probe::run::start(),
                    Instrument::WebGl2 => {
                        let cap = crate::gpu_probe::webgl2::policy_cap_for(form_factor());
                        log::info!(
                            "gpu probe (webgl2): walking to a {} MiB policy cap",
                            cap / (1024 * 1024)
                        );
                        crate::gpu_probe::webgl2_run::start(cap);
                    }
                }
                self.gpu_probe = GpuProbe::Running(instrument);
                GpuProbeReport::Pending
            }
            GpuProbe::Running(Instrument::WebGpu) => {
                let Some(outcome) = crate::gpu_probe::run::outcome() else {
                    return GpuProbeReport::Pending;
                };
                // A capped WebGPU walk is a floor: every step was confirmed
                // held inside an error scope, so the figure stands.
                let report = match crate::gpu_probe::capacity_from(&outcome) {
                    Some(bytes) => found(&outcome, bytes),
                    None => GpuProbeReport::Empty,
                };
                self.gpu_probe = GpuProbe::Settled(report);
                report
            }
            GpuProbe::Running(Instrument::WebGl2) => {
                use crate::gpu_probe::webgl2::{Ending, capacity_from};

                let Some(outcome) = crate::gpu_probe::webgl2_run::outcome() else {
                    return GpuProbeReport::Pending;
                };
                log::info!("{}", webgl2_probe_line(&outcome));
                // `Probed` means the GPU refused. A walk that reached its cap
                // in silence is unmeasured, and says up to what.
                let report = match capacity_from(&outcome) {
                    Some(bytes) => found(&outcome.probe, bytes),
                    None if outcome.ending == Ending::SilentToCap => GpuProbeReport::SilentToCap {
                        cap_bytes: outcome.policy_cap_bytes,
                    },
                    None => GpuProbeReport::Empty,
                };
                self.gpu_probe = GpuProbe::Settled(report);
                report
            }
            GpuProbe::Settled(report) => report,
        }
    }

    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        // No `with_inner_size`, deliberately: it writes an inline pixel
        // `width`/`height` that outranks the stylesheet's `width: 100%`.
        attributes
            .with_canvas(Some(self.canvas.clone()))
            // Otherwise the browser also handles events egui already consumed:
            // scrolling the map scrolls the page, dragging selects text.
            .with_prevent_default(true)
            // The canvas is already in the document; appending adds a second one.
            .with_append(false)
    }
}

/// A found figure, with the arithmetic that reached it.
fn found(outcome: &crate::gpu_probe::ProbeOutcome, bytes: u64) -> GpuProbeReport {
    GpuProbeReport::Found(ProbedCapacity {
        bytes,
        failed_at: outcome.failed_at,
        steps: outcome.steps,
        elapsed_ms: outcome.elapsed_ms,
        capped: outcome.capped,
    })
}

/// `gpu probe (webgl2): 448 MiB ok, failed at 960 MiB, 4 steps, 812 ms, out
/// of memory, renderer 'NVIDIA ...'` — the WebGL2 probe's once-only line,
/// with what ended the walk in words: `out of memory`; `context lost once`
/// (the figure was reached by exhaustion); `no limit found up to 1024 MiB;
/// unmeasured, the presumption stands`; `silent short of the 1024 MiB cap;
/// unmeasured, the presumption stands`; `faulted (GL error 0x501)`; `no
/// WebGL2 context`; `software renderer, not walked; unmeasured`. Then the
/// renderer string the context reported, so the line says which device
/// answered, and a trailing clause counting the page's own context losses
/// that fell inside the probe's window when there were any. Integers only,
/// ASCII only apart from the renderer's own text, and nothing a rig row may
/// read: the level line carries `probe <code>` for that.
fn webgl2_probe_line(outcome: &crate::gpu_probe::webgl2::Webgl2Outcome) -> String {
    use crate::gpu_probe::webgl2::{Ending, Fault};

    let mib = |bytes: u64| bytes / (1024 * 1024);
    let cap = mib(outcome.policy_cap_bytes);
    let failed_at = match outcome.probe.failed_at {
        Some(bytes) => format!("{} MiB", mib(bytes)),
        None => "none".to_string(),
    };
    let ended = match outcome.ending {
        Ending::Refused => "out of memory".to_string(),
        Ending::ContextLost => "context lost once".to_string(),
        Ending::SilentToCap => {
            format!("no limit found up to {cap} MiB; unmeasured, the presumption stands")
        }
        Ending::Silent => {
            format!("silent short of the {cap} MiB cap; unmeasured, the presumption stands")
        }
        Ending::Faulted(Fault::GlError(code)) => format!("faulted (GL error {code:#x})"),
        Ending::Faulted(Fault::NullHandle) => "faulted (null texture handle)".to_string(),
        Ending::NoContext => "no WebGL2 context".to_string(),
        Ending::SoftwareRenderer => "software renderer, not walked; unmeasured".to_string(),
    };
    let renderer = match &outcome.renderer {
        Some(name) => format!("renderer '{name}'"),
        None => "renderer unknown".to_string(),
    };
    let own_losses = squallar_volumetric::degrade::losses_in_probe_window();
    let own = if own_losses > 0 {
        format!("; the page's own context was lost {own_losses} times inside the probe's window")
    } else {
        String::new()
    };
    format!(
        "gpu probe (webgl2): {} MiB ok, failed at {failed_at}, {} steps, {} ms, {ended}, {renderer}{own}",
        mib(outcome.probe.last_ok_bytes),
        outcome.probe.steps,
        outcome.probe.elapsed_ms,
    )
}

/// The page's form factor from pointer media and touch points
/// (`crate::form_factor::classify`), read where it is asked for: the host
/// signals at boot, and the WebGL2 probe's policy cap.
fn form_factor() -> Option<squallar_device_profile::budget::FormFactor> {
    crate::form_factor::classify(
        media_matches("(pointer: coarse)"),
        media_matches("(any-pointer: fine)"),
        navigator_number("maxTouchPoints")
            .filter(|n| *n >= 0.0)
            .map(|n| n as u32),
    )
}

/// One numeric property off `navigator`, read through `Reflect` because the
/// page's global is a `Navigator` and a worker's is a `WorkerNavigator`, the
/// properties this crate reads are the same on both, and this way the crate
/// carries no `web-sys` feature for each. `None` for a browser that does not
/// expose the key — Firefox has no `deviceMemory` — or answers something
/// that is not a finite number.
pub(crate) fn navigator_number(key: &str) -> Option<f64> {
    use wasm_bindgen::JsValue;

    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))
        .ok()
        .and_then(|nav| js_sys::Reflect::get(&nav, &JsValue::from_str(key)).ok())
        .and_then(|n| n.as_f64())
        .filter(|n| n.is_finite())
}

/// `navigator.deviceMemory`, scaled to bytes. The Device Memory spec rounds
/// the value to a power of two between 0.25 and 8 GiB, so this is a bucket
/// the page declares about itself and never a measurement — which is why it
/// lands in `declared_ram_bytes` and not beside the native readers.
fn declared_ram_bytes() -> Option<u64> {
    const GIB: f64 = (1u64 << 30) as f64;
    navigator_number("deviceMemory")
        .filter(|gib| *gib > 0.0)
        .map(|gib| (gib * GIB) as u64)
}

/// `matchMedia(query).matches`, or `None` where the browser would not run
/// the query at all — so a refused query is unknown, never "did not match".
fn media_matches(query: &str) -> Option<bool> {
    web_sys::window()?
        .match_media(query)
        .ok()
        .flatten()
        .map(|list| list.matches())
}

/// The browser's IANA timezone, e.g. `"America/Denver"`.
///
/// `Intl.DateTimeFormat().resolvedOptions().timeZone` is the whole mechanism:
/// no permission, no prompt, no network, and an answer before the first frame.
///
/// Reached through `js_sys::Reflect` because `ResolvedDateTimeFormatOptions`
/// is an anonymous object in the spec and `web_sys` exposes it as `Object`.
#[cfg(target_arch = "wasm32")]
fn browser_timezone() -> Option<String> {
    use wasm_bindgen::JsValue;

    let resolved = js_sys::Intl::DateTimeFormat::default().resolved_options();
    let zone = js_sys::Reflect::get(&resolved, &JsValue::from_str("timeZone")).ok()?;
    // A browser too old for the `timeZone` key returns `undefined`.
    let zone = zone.as_string()?;
    // An empty string is not a zone, and would reach the anchor table as a lookup
    // that misses in a way that looks deliberate.
    (!zone.is_empty()).then_some(zone)
}
