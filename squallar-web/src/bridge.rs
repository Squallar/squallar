//! The browser's [`PlatformBridge`]. Most of the trait is capabilities a tab
//! does not have, so most of this file is honest `None`s.

use egui_wgpu::wgpu;
use squallar_app::platform::{
    GpuProbeReport, HostSignals, LinearMemory, PlatformBridge, ProbedCapacity, gpu_probe_applies_to,
};
use winit::platform::web::WindowAttributesExtWebSys;

const DARK_SCHEME_QUERY: &str = "(prefers-color-scheme: dark)";

/// Where the WebGPU probe stands, as the bridge has driven it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuProbe {
    /// Nobody has asked yet.
    NotStarted,
    /// Started; the outcome cell is empty until it lands.
    Running,
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
            form_factor: crate::form_factor::classify(
                media_matches("(pointer: coarse)"),
                media_matches("(any-pointer: fine)"),
                navigator_number("maxTouchPoints")
                    .filter(|n| *n >= 0.0)
                    .map(|n| n as u32),
            ),
        }
    }

    /// This page's heap, and the rasterization worker's as it last reported
    /// — two instances under two ceilings, see [`LinearMemory`].
    fn linear_memory(&self) -> Option<LinearMemory> {
        Some(LinearMemory {
            page_bytes: crate::shared_loan::memory_bytes()?,
            page_max_bytes: crate::heap_max::this_instance().unwrap_or(0),
            worker_bytes: crate::worker_port::worker_memory_bytes(),
            // What the worker reported on its hello where it has said, else
            // what this page asked for it. A zero is "nobody said", which the
            // watermark spells `Quiet` rather than guessing a wall.
            worker_max_bytes: crate::heap_max::worker_instance().unwrap_or(0),
        })
    }

    /// The WebGPU probe, driven from the app's asks. The first ask starts it
    /// when the app's own backend is WebGPU and logs a skip once for anything
    /// else — a WebGL2 page would have the probe measure an API that is not
    /// the one drawing (`crate::gpu_probe`). Later asks read the outcome
    /// cell; once it has landed the same report is re-said on every ask, so
    /// the app's level line can carry it after the once-only lines are gone
    /// from the console ring. A probe that held nothing settles as `Empty`.
    fn gpu_probe_report(&mut self, backend: wgpu::Backend) -> GpuProbeReport {
        match self.gpu_probe {
            GpuProbe::NotStarted => {
                if gpu_probe_applies_to(backend) {
                    crate::gpu_probe::run::start();
                    self.gpu_probe = GpuProbe::Running;
                    GpuProbeReport::Pending
                } else {
                    log::info!("gpu probe: skipped (backend {backend:?})");
                    self.gpu_probe = GpuProbe::Settled(GpuProbeReport::Skipped);
                    GpuProbeReport::Skipped
                }
            }
            GpuProbe::Running => {
                let Some(outcome) = crate::gpu_probe::run::outcome() else {
                    return GpuProbeReport::Pending;
                };
                let report = match crate::gpu_probe::capacity_from(&outcome) {
                    Some(bytes) => GpuProbeReport::Found(ProbedCapacity {
                        bytes,
                        failed_at: outcome.failed_at,
                        steps: outcome.steps,
                        elapsed_ms: outcome.elapsed_ms,
                        capped: outcome.capped,
                    }),
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
