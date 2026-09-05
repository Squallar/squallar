//! The seam between the portable app and whatever OS it is running on.
//!
//! Only the trait lives here. Every concrete implementation lives beside the
//! entry point that constructs it, because this crate must build for targets
//! whose bridges it has never heard of.

use egui_wgpu::wgpu;
pub use squallar_device_profile::budget::FormFactor;

/// What the host can say about itself before any adapter has answered — the
/// capacity-shaped signals a bridge reads once, at construction, and hands
/// over as plain data. Every field is `Option` because `None` is the majority
/// arm on at least one target, and a signal a platform cannot read is absent
/// rather than invented.
///
/// **Two RAM figures, never one.** [`Self::system_ram_bytes`] is *measured*
/// (`/proc/meminfo`, `NSProcessInfo`, `GlobalMemoryStatusEx`);
/// [`Self::declared_ram_bytes`] is a browser's `navigator.deviceMemory`, a
/// coarse bucket the page asserts about itself. A declaration may lower a
/// presumption and never raises one, so the two are kept apart at the seam.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostSignals {
    /// Measured system RAM, where an API answers. `None` in every browser.
    pub system_ram_bytes: Option<u64>,
    /// Declared system RAM — `navigator.deviceMemory` in GiB, scaled to
    /// bytes. `None` natively and on every browser that does not expose it.
    pub declared_ram_bytes: Option<u64>,
    /// Threads the host reports: `available_parallelism()` natively,
    /// `navigator.hardwareConcurrency` in a browser. The machine's own figure,
    /// not what any pool was built with; unknown is `None`, never `1`.
    pub parallelism: Option<usize>,
    /// A build fact natively (the desktop bridge is a desktop; Android and
    /// iOS are handheld); a pointer-media classification in a browser.
    pub form_factor: Option<FormFactor>,
    /// **The maximum this instance's wasm linear memory was constructed
    /// with**, in bytes. `None` natively — a native heap declares no ceiling —
    /// and `None` in a browser instance nobody told, which is not the same as
    /// the build's link flag and must never be replaced by it.
    ///
    /// A browser is the one platform whose host capacity is *known* without a
    /// reader, and this is that figure. It used to be a constant; it is a
    /// value because the page chooses it per device before the module is
    /// instantiated, so the same binary can give a 2 GiB phone a small wall
    /// and a desktop the full one (`squallar-web/heap.js`).
    pub linear_memory_max_bytes: Option<u64>,
}

/// How full the wasm linear memories are. **Two instances, two ceilings**: the
/// page and the rasterization worker each hold their own `WebAssembly.Memory`
/// under their own `--max-memory`, so the two figures are never one and
/// never added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinearMemory {
    /// `memory().buffer().byteLength` of the instance the bridge runs in.
    pub page_bytes: u64,
    /// **What that instance's memory was constructed with**, in bytes — the
    /// wall [`Self::page_bytes`] is judged against. Carried beside the reading
    /// rather than looked up, because no engine will say what a memory's
    /// maximum is and the two instances need not have the same one.
    pub page_max_bytes: u64,
    /// **What the worker's memory was constructed with**, in bytes, as it
    /// reported on its hello — or what the page chose for it before one
    /// answered. 0 where nobody has said, which the watermark reads as no
    /// ceiling rather than as a wall of zero.
    pub worker_max_bytes: u64,
    /// The worker's own reading, as it last reported on its hello or a reply
    /// envelope. `None` until a worker has said.
    pub worker_bytes: Option<u64>,
    /// **The worker's live bytes** — what its allocator has handed out and
    /// not been handed back (`squallar_alloc::live_bytes` on that instance),
    /// as it last reported beside [`Self::worker_bytes`]. The one figure of
    /// the worker's heap that can fall: its memory never shrinks, so the gap
    /// between the two is freed-but-reserved headroom. `None` until a worker
    /// has said, and on a worker whose build predates the field. The page's
    /// own live bytes are not here: the bridge reads the instance it runs in,
    /// and the app reads its own allocator directly.
    pub worker_live_bytes: Option<u64>,
}

/// What the browser's WebGPU probe found: the **per-tab allowance**, as the
/// last total a throwaway device held resident before refusing — see
/// `squallar_web::gpu_probe`. The browser's figure and not the card's: a tab
/// is allowed a share, and no API states it. Plain data, integers only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbedCapacity {
    /// The last total held without refusal, in bytes. Never zero: a probe
    /// that held nothing reports no figure at all.
    pub bytes: u64,
    /// The total the device refused to reach, in bytes, or `None` when it
    /// never refused and the probe stopped at its own bound.
    pub failed_at: Option<u64>,
    /// Allocations attempted.
    pub steps: u32,
    /// Wall time the probe took, in milliseconds.
    pub elapsed_ms: u32,
    /// Whether the probe stopped at its own byte ceiling, time budget or a
    /// shape no texture takes, rather than at a refusal — in which case
    /// [`Self::bytes`] is a floor on the allowance, not the allowance.
    pub capped: bool,
}

/// Where the WebGPU probe stands, as the bridge last said. Carried on the
/// re-said `budget state:` line as `probe <code>` (`crate::budget_telemetry::
/// gpu_probe_code`), because the browser console keeps a bounded ring and a
/// once-only line is evicted within seconds of the frame telemetry that
/// follows it — a rig scrape reading it as absent cannot tell "evicted" from
/// "never ran". Every fact a row might read is therefore on the level line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpuProbeReport {
    /// No probe on this bridge — every native one — or none asked for yet.
    #[default]
    Absent,
    /// The page's own backend is not WebGPU, so the probe never ran.
    Skipped,
    /// Started; no figure yet.
    Pending,
    /// Ran and held nothing — the first allocation refused, or no second
    /// adapter or device — so there is no figure and the presumption stands.
    Empty,
    /// Held a figure; see [`ProbedCapacity::capped`] for whose bound it is.
    Found(ProbedCapacity),
}

impl GpuProbeReport {
    /// The bytes a found probe held, or `None` in every other state.
    pub fn bytes(self) -> Option<u64> {
        match self {
            Self::Found(probe) => Some(probe.bytes),
            _ => None,
        }
    }

    /// Whether the bridge has said its last word: everything but
    /// [`Self::Pending`]. An [`Self::Absent`] answer from a bridge that was
    /// asked is a bridge with no probe, and is not asked again.
    pub fn is_settled(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// Whether the WebGPU probe's figure would describe the API the application
/// draws with. wgpu binds one browser API when the instance is built, and the
/// probe measures WebGPU's allowance through a second instance of its own;
/// on a WebGL2 page — `Gl`, every Firefox/Linux leg today — it would measure
/// an API that is not the one drawing, and WebGL2 has no clean failure of its
/// own to probe (`docs/cross-platform-resource-limits.md` §8.4). Native
/// backends have readers, not probes.
pub fn gpu_probe_applies_to(backend: wgpu::Backend) -> bool {
    backend == wgpu::Backend::BrowserWebGpu
}

/// How a GPU capacity figure was obtained, in order of trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuCapacitySource {
    /// Read from the driver: a Vulkan `DEVICE_LOCAL` heap sum, a DXGI budget,
    /// Metal's recommended working set.
    Measured,
    /// Found by allocating until the API refused — the browser's per-tab
    /// allowance, which no API states.
    Probed,
    /// The bracket's constant, refined by what the adapter reports.
    Presumed,
}

/// What a [`RedrawWaker`] fires, once there is a window to fire it at. `Arc`
/// rather than `Box` so [`RedrawWaker::wake`] can drop the guard before calling.
type WakeFn = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A handle a foreign thread uses to ask the event loop for a frame.
///
/// The app runs on `ControlFlow::Wait` and `App::poll_platform_state` — the one
/// thing that drains the sensor and theme channels — runs only from
/// `handle_redraw`, so a value pushed by a thread the loop knows nothing about is
/// invisible until something else produces a frame. Five producers share the gap.
///
/// A redraw request and not the event-loop proxy: `EventLoopProxy::send_event`
/// (winit 0.30 has no `wake_up`) delivers to `ApplicationHandler::user_event`,
/// which `App` does not override, so it produces an iteration and not a frame.
/// Measured against a real winit 0.30.13 loop on `ControlFlow::Wait` under X11:
/// one `request_redraw` from a foreign thread delivered `RedrawRequested` in
/// 29–43 µs over three runs; two `send_event`s produced zero.
///
/// A slot rather than a window because producers are handed a waker while
/// `App::window` is still `None`. `App::suspended` empties it so no sensor thread
/// outlives the destroyed window; `resumed` refills it.
#[derive(Clone, Default)]
pub struct RedrawWaker {
    slot: std::sync::Arc<std::sync::Mutex<Option<WakeFn>>>,
}

const _: () = {
    const fn assert_shareable<T: Send + Sync + Clone>() {}
    assert_shareable::<RedrawWaker>();
};

impl std::fmt::Debug for RedrawWaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let attached = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        f.debug_struct("RedrawWaker")
            .field("attached", &attached)
            .finish()
    }
}

impl RedrawWaker {
    /// A waker with no window behind it yet; waking is a no-op until one arrives.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install `wake` as what every outstanding handle fires.
    pub(crate) fn install(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::sync::Arc::new(wake));
    }

    /// Empty the slot, dropping whatever it was holding — the window included.
    pub(crate) fn detach(&self) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Ask the event loop for a frame, if there is a window to ask through.
    ///
    /// The guard is dropped before the call: `notify_redraw` wraps
    /// `request_redraw` in `catch_unwind` because X11's copy panics once the loop
    /// has closed, and a `Mutex` poisoned by an unwind under the guard would
    /// silently drop every subsequent wake from every producer.
    pub fn wake(&self) {
        let wake = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// The thread winit drives the event loop on, learned when the window is made.
///
/// `App::create_window` runs inside `ApplicationHandler::resumed`, which winit
/// calls only from the loop thread, so recording `current()` there names it
/// exactly. Unset means *not proven to be the loop thread*, and
/// [`ask_for_a_frame`] reads unproven as foreign — the fail-safe direction is
/// the one that hands off.
static LOOP_THREAD: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

/// Name the loop thread. Idempotent: the first caller wins, and on a
/// suspend/resume cycle the second is the same thread anyway.
pub(crate) fn record_loop_thread() {
    let _ = LOOP_THREAD.set(std::thread::current().id());
}

/// Whether the caller is the thread winit runs the loop on.
fn on_loop_thread() -> bool {
    LOOP_THREAD.get() == Some(&std::thread::current().id())
}

/// Ask the event loop for a frame, from any thread, without ever waiting on
/// another one.
///
/// **The invariant: no thread but the loop's own performs the platform's
/// request-redraw call.** `Window::request_redraw` is not the post its name
/// suggests. `winit-0.30.13/src/window.rs:600` routes it through
/// `maybe_queue_on_main`, and the macOS copy of that
/// (`src/platform_impl/macos/window.rs:40-51`) says in its own comment that it
/// deliberately does *not* queue: it is `MainThreadBound::get_on_main` ->
/// `objc2_foundation::run_on_main`, which is
/// `dispatch::Queue::main().exec_sync` for every caller that is not already the
/// main thread (`objc2-foundation-0.2.2/src/thread.rs:107-121`) and blocks
/// until the main thread services the queue. The Linux backends pass
/// `maybe_queue_on_main` straight through
/// (`src/platform_impl/linux/mod.rs:307-313`), so only macOS pays it.
///
/// A blocking call is a deadlock as soon as its caller holds anything the loop
/// thread wants, and every producer in this app asks for frames off-thread:
/// tile decodes, basemap segments, area reconciles, the chunk sockets, the
/// sensor and theme pollers, Android's back button. One of those cycles has
/// already been paid for — an off-frame `ctx.request_repaint()` took egui's
/// context write lock and then waited here, while the loop thread sat in
/// `Gui::ui` waiting for that same lock, and the app stopped rendering
/// entirely within 2-35 s (fixed in `squallar_gpu::egui_renderer`, which keeps
/// its own guard because it is handed an arbitrary wake and cannot check this
/// one). Auditing every producer for the locks it holds is a standing
/// obligation that grows with each new thread; not making the call from those
/// threads at all is a property.
///
/// The loop thread still calls straight through, which is both the cheap path
/// and the honest one: `run_on_main` already short-circuits there, so a frame
/// asking for the next frame costs exactly what it did before.
pub(crate) fn ask_for_a_frame(window: &crate::WindowRef) {
    if on_loop_thread() {
        request_redraw(window);
    } else {
        post_off_thread(window);
    }
}

/// The platform call itself, and the only place it is spelled.
///
/// `catch_unwind` because X11's copy panics once the loop has closed, and
/// background producers outlive the loop on the way out.
fn request_redraw(window: &crate::WindowRef) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        window.request_redraw();
    }));
}

/// Hand the ask to the process's own redraw thread, which holds no lock anyone
/// wants and so may block in the platform call for as long as it likes.
///
/// One thread for the process, not one per producer: it exists to be the only
/// thing that ever waits, and the asks are interchangeable.
#[cfg(not(target_arch = "wasm32"))]
fn post_off_thread(window: &crate::WindowRef) {
    type Post = Box<dyn Fn(crate::WindowRef) + Send + Sync>;
    static POST: std::sync::OnceLock<Post> = std::sync::OnceLock::new();
    let post = POST.get_or_init(|| Box::new(coalescing_poster("squallar-redraw", request_redraw)));
    post(window.clone());
}

/// One thread, so there is nobody to hand off to and nothing that blocks.
///
/// The web build has no `dispatch` queue and no second thread to be foreign
/// from; [`on_loop_thread`] is true for every caller there, so this is the arm
/// that must exist rather than the arm that runs.
#[cfg(target_arch = "wasm32")]
fn post_off_thread(window: &crate::WindowRef) {
    request_redraw(window);
}

/// A poster whose payloads are carried out on a thread of its own, newest
/// first and the rest discarded.
///
/// **The whole point is the return: posting never runs `act`.** Whatever the
/// caller is holding is released on its own schedule, not on the schedule of
/// whatever `act` has to wait for. `act` may block for as long as it likes,
/// because the thread it blocks on holds nothing anyone else wants.
///
/// Coalescing because the asks are idempotent — *n* asks for a frame are one
/// frame — and because on macOS each one is a round trip to the loop thread, so
/// a burst that was cheap to produce is not cheap to deliver. Newest rather
/// than oldest so a window replaced across a suspend is the one asked, and the
/// payload is dropped at the end of every iteration so this thread never keeps
/// a dead window alive.
#[cfg(not(target_arch = "wasm32"))]
fn coalescing_poster<T, F>(name: &str, act: F) -> impl Fn(T) + Send + Sync + 'static
where
    T: Send + 'static,
    F: Fn(&T) + Send + 'static,
{
    let (post, asked) = std::sync::mpsc::channel::<T>();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // Ends when every `Sender` is dropped. The redraw one never is —
            // it lives in a `static` — and a parked `recv` costs nothing.
            while let Ok(mut latest) = asked.recv() {
                while let Ok(newer) = asked.try_recv() {
                    latest = newer;
                }
                act(&latest);
            }
        })
        // Louder than the alternatives. Calling `act` inline would put the
        // caller back in the blocking path this exists to keep it out of, and
        // swallowing it would leave a loop on `ControlFlow::Wait` that never
        // draws another off-thread arrival — the same stopped app, without the
        // stack that explains it.
        .expect("the redraw thread is what lets an off-thread ask reach the event loop");
    move |payload| {
        // Non-blocking: an unbounded `Sender` never waits on its receiver.
        let _ = post.send(payload);
    }
}

/// Drain all pending messages from `rx`, returning the last one (if any).
///
/// Sensor and theme channels are state, not events: only the newest value matters.
pub fn drain_latest<T>(rx: &std::sync::mpsc::Receiver<T>) -> Option<T> {
    let mut latest = None;
    while let Ok(val) = rx.try_recv() {
        latest = Some(val);
    }
    latest
}

/// Spawn a named thread that samples `read` every `interval` and forwards the
/// result, until the returned `Receiver` is dropped.
///
/// For state a platform only exposes by polling. Android's theme is the one case
/// today: NativeActivity never emits `WindowEvent::ThemeChanged`.
///
/// Every sample is sent, not just the ones that differ: that is what lets the
/// thread notice the receiver is gone, since a disconnected `mpsc::Sender` is
/// only observable by trying to send. Only a change wakes, since waking on every
/// sample would be a full frame every `interval` for the life of the process.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_state_poller<T, F>(
    name: &str,
    interval: std::time::Duration,
    read: F,
    wake: RedrawWaker,
) -> std::io::Result<std::sync::mpsc::Receiver<T>>
where
    T: Clone + PartialEq + Send + 'static,
    F: Fn() -> T + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // `None` until the first read, which makes the first sample a change.
            let mut last: Option<T> = None;
            loop {
                let sample = read();
                let changed = last.as_ref() != Some(&sample);
                if sender.send(sample.clone()).is_err() {
                    break;
                }
                last = Some(sample);
                if changed {
                    wake.wake();
                }
                std::thread::sleep(interval);
            }
        })?;
    Ok(receiver)
}

/// Platform-specific behavior abstracted behind a common trait.
pub trait PlatformBridge {
    /// Poll for theme changes from the OS. `Some(is_dark)` on a change.
    fn poll_theme(&mut self) -> Option<bool>;

    /// Poll for compass heading updates. Returns degrees (0–360) if available.
    fn poll_heading(&mut self) -> Option<f32>;

    /// Query system bar insets (top, bottom, left, right) in logical pixels.
    fn query_insets(&self) -> Option<(f32, f32, f32, f32)>;

    /// Handle the back button press. Returns `true` if the platform consumed
    /// the event (e.g. Android moveTaskToBack), `false` if it did not — see
    /// [`exits_on_unhandled_back`](Self::exits_on_unhandled_back) for what
    /// happens then.
    fn handle_back(&self) -> bool;

    /// Whether a back press with nothing open, that the platform did not take,
    /// should quit the app.
    ///
    /// `false` here, and no bridge that ships answers otherwise: with nothing
    /// open, Escape and the browser's Back are **inert**. They used to quit,
    /// with no confirmation, from a key that sits one row above the one that
    /// leaves a text field — and quitting is still reachable from the menu and
    /// the window's close button, which are the two routes a user takes on
    /// purpose. Android is unaffected either way: its bridge takes the press in
    /// [`handle_back`](Self::handle_back) and minimises.
    ///
    /// It lives on the trait rather than behind a `cfg` because it is a
    /// property of the platform, and a `cfg(target_arch)` may select a value, a
    /// dependency or a type alias but never fork behaviour inside a function
    /// body. A platform whose only way out is the back button says so here and
    /// the resolver keeps one shape. Nothing in this tree is such a platform
    /// today, so what exercises the `true` arm is the test double — that is the
    /// hook staying honest, not evidence that some platform quits.
    fn exits_on_unhandled_back(&self) -> bool {
        false
    }

    /// Take a back press the platform delivered outside the window's input queue.
    ///
    /// Android's `OnBackInvokedDispatcher` is the only source: once the app opts in
    /// to predictive back, back is handed to a Java callback on the UI thread,
    /// which parks the press and wakes the loop. Consuming, because this is polled
    /// every loop iteration.
    fn poll_back_press(&mut self) -> bool {
        false
    }

    /// Set the reader for [`poll_back_press`](Self::poll_back_press) (Android only).
    ///
    /// Injected because the flag it reads is written by a JNI entry point in the
    /// `squallar` crate's cfg(android) back module.
    fn set_back_press_taker(&mut self, _taker: fn() -> bool) {}

    /// Whether the suspend now being handled is this app going away for good,
    /// rather than going to the background.
    ///
    /// winit reports `Suspended` for both, because both destroy the window,
    /// and only Android can tell them apart — or needs to. Its glue blocks the
    /// Java **UI thread** inside `onDestroy` until this event loop ends, so a
    /// loop that keeps running through a finish deadlocks the Activity that
    /// replaces it. Every other platform answers `false`, which is the
    /// behaviour every platform had before this existed.
    fn suspend_is_terminal(&self) -> bool {
        false
    }

    /// Set the probe for [`suspend_is_terminal`](Self::suspend_is_terminal)
    /// (Android only).
    ///
    /// Injected for the same reason as the back hooks above: the read is a JNI
    /// call, and this trait is declared in a crate that compiles for targets
    /// that have never heard of JNI.
    fn set_terminal_suspend_probe(&mut self, _probe: fn() -> bool) {}

    /// Tell the platform whether the next back press has something to close.
    ///
    /// Android's predictive-back dispatcher only lets an app decline a press by
    /// not being registered when it arrives — `onBackInvoked()` returns void —
    /// and declining is what buys the system's own back-to-home preview. So the
    /// claim has to be published *before* the press, and it has to be true at
    /// every transition that opens or closes something. Pushed on change only,
    /// at the end of a frame; a per-frame JNI hop is what this shape avoids.
    ///
    /// Every other platform ignores it: back is a key there, answered when it
    /// arrives — and so, today, does Android, which has not opted into the
    /// dispatcher (see `BackHandler.kt` and the manifest for the measurement
    /// that keeps it opted out). The claim is published anyway so that the day
    /// the opt-in becomes mandatory nothing but a manifest attribute changes.
    fn set_back_claimed(&mut self, _claimed: bool) {}

    /// Set the sink [`set_back_claimed`](Self::set_back_claimed) forwards to
    /// (Android only).
    ///
    /// Injected for the same reason as
    /// [`set_back_press_taker`](Self::set_back_press_taker): the far end is a
    /// JNI static call in the `squallar` crate's cfg(android) back module, and
    /// this trait has to compile for targets that never heard of JNI.
    fn set_back_claim_reporter(&mut self, _reporter: fn(bool)) {}

    fn detect_dark_theme(&self) -> bool;

    fn set_back_handler(&mut self, handler: fn());

    fn set_zone_cache_dir(&mut self, dir: std::path::PathBuf);

    fn zone_cache_dir(&self) -> Option<&std::path::Path>;

    /// Where the archive block cache (basemap and terrain tile bytes)
    /// persists, or `None` on a platform with no filesystem for it — the
    /// cache then simply stays disabled. `zone_cache_dir`'s twin, platform
    /// for platform.
    fn set_basemap_cache_dir(&mut self, dir: std::path::PathBuf);

    fn basemap_cache_dir(&self) -> Option<&std::path::Path>;

    /// Where downloaded offline basemap areas persist, or `None` on a
    /// platform with no filesystem for them — the download feature then has
    /// no home and stays disabled. [`basemap_cache_dir`](Self::basemap_cache_dir)'s
    /// durable sibling: that one is an evictable block cache, this one holds
    /// bytes the user asked to keep.
    ///
    /// The Gui learns this once, at construction, and there is deliberately
    /// no Gui setter to push it through afterwards — a bridge that will ever
    /// answer `Some` must be populated before `App::new` (Android sets it on
    /// the bridge in `android_main`, *before* handing the bridge over, unlike
    /// its two late-set siblings). The directory may not exist yet; creating
    /// it is the download engine's job, not the bridge's.
    fn set_basemap_dir(&mut self, dir: std::path::PathBuf);

    fn basemap_dir(&self) -> Option<&std::path::Path>;

    fn set_config_dir(&mut self, dir: std::path::PathBuf);

    /// Where this platform persists small blobs, or `None` if the platform has not
    /// been told where yet (Android learns its data path only after startup).
    /// A store rather than a directory, so a web bridge can hand back
    /// `localStorage`.
    fn kv(&self) -> Option<Box<dyn squallar_kv::KvStore>>;

    /// This device's IANA timezone name, e.g. `"America/Denver"`.
    ///
    /// Used to pick a starting radar site on a first run, without asking for a
    /// location permission — see [`crate::location_hint`].
    fn iana_timezone(&self) -> Option<String> {
        None
    }

    /// Request application exit. Returns `true` if the platform requires
    /// `std::process::exit` (Android), `false` for normal event-loop exit.
    fn needs_process_exit(&self) -> bool;

    /// Whether quitting is something this platform lets an app do at all.
    /// `false` on iOS: `exit()` is an App Store rejection, and UIKit's run loop
    /// never unwinds back to `run_app`'s caller.
    fn supports_exit(&self) -> bool {
        true
    }

    /// Adjust the attributes the main window is created with.
    ///
    /// Only the web bridge has anything to add: winit's web backend must be told
    /// which `<canvas>` the window is before it exists.
    fn window_attributes(
        &self,
        attributes: winit::window::WindowAttributes,
    ) -> winit::window::WindowAttributes {
        attributes
    }

    /// Hand the bridge the handle its own background threads wake the loop with.
    ///
    /// Called once from `App::new`, before any window exists — which is
    /// why [`RedrawWaker`] is a slot rather than a window.
    fn set_redraw_waker(&mut self, _waker: RedrawWaker) {}

    /// Set a receiver for compass heading updates (Android only, no-op on desktop).
    fn set_heading_receiver(&mut self, _receiver: std::sync::mpsc::Receiver<f32>) {}

    /// Set a callback that queries system bar insets (Android only, no-op on desktop).
    fn set_insets_querier(&mut self, _querier: fn() -> (f32, f32, f32, f32)) {}

    /// Set a callback that reads the OS dark-theme preference (Android only).
    ///
    /// Android reads this over JNI, which needs `unsafe` and the process
    /// `JavaVM`; both stay in the `squallar` crate's cfg(android) modules because
    /// this crate must compile for targets that have never heard of JNI.
    fn set_theme_detector(&mut self, _detector: fn() -> bool) {}

    /// What the host says about itself: RAM, threads, form factor. Read once,
    /// from `App::new`, and copied into the device profile as plain data.
    ///
    /// The readers live beside each bridge — `/proc/meminfo` in the shell's
    /// Linux module, `matchMedia` in the web bridge — because this crate must
    /// compile for targets whose APIs it has never heard of. A bridge that
    /// reads nothing answers the default, which is every field `None`.
    fn host_signals(&self) -> HostSignals {
        HostSignals::default()
    }

    /// How full the wasm linear memories are, or `None` on a target that has
    /// no linear memory to fill — which is every native bridge.
    fn linear_memory(&self) -> Option<LinearMemory> {
        None
    }

    /// **What the OS says is free for this process to take right now**, in
    /// bytes, or `None` on a platform with no such reader — which is every
    /// browser, where a page is told nothing about the machine and its wall
    /// is [`HostSignals::linear_memory_max_bytes`] instead.
    ///
    /// Polled, not read once, and that is why it is a method here rather than
    /// a field on [`HostSignals`]: the figure moves with every other program
    /// on the machine, and a value taken at construction would be exactly the
    /// high-water mark this reading exists to replace. Asked on the telemetry
    /// tick, never on the frame thread — `/proc/meminfo` is a file read.
    ///
    /// **Not a pool on its own.** Every OS's answer already excludes this
    /// process, so a percentage taken of it directly recedes as the app grows;
    /// what a percentage is taken of is this figure plus the app's own live
    /// bytes (`squallar_device_profile::scene::host_pool_bytes`).
    fn available_memory_bytes(&self) -> Option<u64> {
        None
    }

    /// The GPU's capacity in bytes and how that figure was obtained, or
    /// `None` where no reader exists for this adapter.
    ///
    /// The seam only: no bridge answers yet, and the app spends nothing on
    /// the answer. The readers (Vulkan heaps, DXGI budgets, Metal's working
    /// set, the WebGPU probe) land behind this signature so that landing them
    /// changes one bridge each and nothing in this crate.
    fn gpu_capacity(
        &self,
        _adapter: &wgpu::Adapter,
        _device: &wgpu::Device,
    ) -> Option<(u64, GpuCapacitySource)> {
        None
    }

    /// Where the WebGPU probe of the browser's per-tab GPU allowance stands:
    /// [`GpuProbeReport::Absent`] on every native bridge, and on the web the
    /// probe's own state — skipped on a WebGL2 page, pending, empty, or found
    /// with its figure. `backend` is the one the application renders with;
    /// the web bridge starts the probe on the first ask that names
    /// `BrowserWebGpu` ([`gpu_probe_applies_to`]) and logs one skip for
    /// anything else. Asked on the telemetry tick until the answer settles,
    /// so the first ask follows the first presented frame and the probe never
    /// competes with the page's own boot.
    fn gpu_probe_report(&mut self, _backend: wgpu::Backend) -> GpuProbeReport {
        GpuProbeReport::Absent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // No test here can build a `winit::Window`; what `App` puts in the slot is
    // pinned by a source probe in `app.rs`.

    /// `Arc<AtomicUsize>` because the slot's contents must be `Send + Sync`.
    fn counting_wake(waker: &RedrawWaker) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe = std::sync::Arc::clone(&count);
        waker.install(move || {
            probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        count
    }

    fn woke(count: &std::sync::atomic::AtomicUsize) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The probe describes WebGPU's allowance and nothing else: it applies to
    /// `BrowserWebGpu` alone. A WebGL2 page never runs it, and no native
    /// backend does — those have readers.
    #[test]
    fn a_probe_on_a_webgl2_page_never_runs() {
        assert!(!gpu_probe_applies_to(wgpu::Backend::Gl));
        assert!(gpu_probe_applies_to(wgpu::Backend::BrowserWebGpu));
        for native in [
            wgpu::Backend::Vulkan,
            wgpu::Backend::Metal,
            wgpu::Backend::Dx12,
            wgpu::Backend::Noop,
        ] {
            assert!(
                !gpu_probe_applies_to(native),
                "{native:?} has a reader, not a probe"
            );
        }
        let applicable: Vec<_> = wgpu::Backend::ALL
            .into_iter()
            .filter(|b| gpu_probe_applies_to(*b))
            .collect();
        assert_eq!(applicable, [wgpu::Backend::BrowserWebGpu]);
    }

    /// Producers are handed their waker while `App::window` is still `None`.
    #[test]
    fn a_waker_handed_out_before_the_window_exists_still_finds_it() {
        let waker = RedrawWaker::new();
        let held_by_the_producer = waker.clone();
        held_by_the_producer.wake();

        let woken = counting_wake(&waker);

        held_by_the_producer.wake();
        assert_eq!(
            woke(&woken),
            1,
            "the copy taken before the window existed never saw it appear, so \
             every fix that producer ever sends is invisible until something \
             else draws a frame"
        );
    }

    /// Waking before there is anything to wake must be quiet, not fatal.
    #[test]
    fn a_wake_with_no_window_yet_is_a_no_op() {
        RedrawWaker::new().wake();
    }

    /// `App::suspended` clears `window` and `state`, so the drop is the assertion.
    #[test]
    fn a_waker_stops_holding_the_window_once_the_app_is_suspended() {
        let waker = RedrawWaker::new();
        let window = std::sync::Arc::new(());
        let held = std::sync::Arc::clone(&window);
        waker.install(move || {
            let _ = &held;
        });
        assert_eq!(std::sync::Arc::strong_count(&window), 2);

        waker.detach();

        assert_eq!(
            std::sync::Arc::strong_count(&window),
            1,
            "the window is still referenced from the slot after a suspend, so \
             the surface it belongs to outlives it"
        );
        waker.clone().wake();
    }

    /// `notify_redraw` wraps `request_redraw` in `catch_unwind` because X11's
    /// panics once the loop has closed; unwinding under a held guard would poison
    /// the mutex, so `wake` releases the guard before it calls.
    #[test]
    fn a_panicking_wake_does_not_silence_later_ones() {
        let waker = RedrawWaker::new();
        waker.install(|| panic!("request_redraw on a closed X11 loop"));

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker.wake()));
        assert!(unwound.is_err(), "the fixture did not actually panic");

        let woken = counting_wake(&waker);
        waker.wake();
        assert_eq!(
            woke(&woken),
            1,
            "one unwinding wake poisoned the slot, so every producer's every \
             later wake is dropped"
        );
    }

    /// **A thread that asks for a frame must not still be asking when it lets
    /// go of what it was holding.**
    ///
    /// This is the deadlock the funnel exists to make impossible, run in
    /// process. On macOS `Window::request_redraw` blocks the caller until the
    /// main thread services a dispatch queue
    /// (`objc2-foundation-0.2.2/src/thread.rs:107-121`, reached from
    /// `winit-0.30.13/src/platform_impl/macos/window.rs:40-51`). A producer
    /// that makes that call while holding a lock the loop thread also takes
    /// waits for a loop thread that is waiting for it, and the app stops
    /// rendering — which is exactly what shipped, through egui's context write
    /// lock, freezing overlay-heavy scenes within 2-35 s.
    ///
    /// Linux cannot perform that rendezvous at all (its `maybe_queue_on_main`
    /// is `f(self)`), so the blocking stand-in below is what carries the
    /// platform's behaviour into a test CI can run. The producer holds a lock
    /// across the ask; the loop thread then takes that same lock. If posting
    /// ran the call inline, the producer would still be inside it, still
    /// holding the lock, and the loop thread would never get it.
    ///
    /// The wait is bounded so failure REDDENS rather than hanging the suite,
    /// and the gate is opened before the assertions so a red cannot leave a
    /// thread parked on a lock the message would have to read.
    // wasm32 has no threads, so the poster this exercises is absent there —
    // and with one thread there is no rendezvous to rule out.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_thread_that_asks_for_a_frame_is_not_the_thread_that_waits_for_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
        use std::time::Duration;

        // Stands in for any lock a producer and the loop thread both take. The
        // one cycle already paid for used egui's context write lock.
        let shared = Arc::new(Mutex::new(()));

        // Opened only at the end, so the stand-in platform call is still inside
        // its rendezvous for the whole of the loop thread's attempt.
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let entered = Arc::new(AtomicBool::new(false));

        let held = Arc::clone(&gate);
        let marked = Arc::clone(&entered);
        let post = coalescing_poster("test-redraw-rendezvous", move |_: &()| {
            marked.store(true, Ordering::SeqCst);
            let (lock, cvar) = &*held;
            let mut open = lock.lock().unwrap_or_else(PoisonError::into_inner);
            while !*open {
                open = cvar.wait(open).unwrap_or_else(PoisonError::into_inner);
            }
        });

        // Any of the off-thread producers — a tile decode, a chunk socket, the
        // theme poller — holding something across the ask.
        let producing = Arc::clone(&shared);
        let producer = std::thread::spawn(move || {
            let _guard = producing.lock().unwrap_or_else(PoisonError::into_inner);
            post(());
            // `_guard` drops here. In the shape this rules out, the ask would
            // still be inside the platform call and would never reach it.
        });

        // The stand-in call has to be INSIDE its block before the loop thread
        // tries, or this proves nothing about the overlap.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !entered.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let call_ran = entered.load(Ordering::SeqCst);

        let (done, waited) = mpsc::channel();
        let wanting = Arc::clone(&shared);
        let loop_thread = std::thread::spawn(move || {
            let _held = wanting.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = done.send(());
        });
        let reached = waited.recv_timeout(Duration::from_secs(5)).is_ok();

        {
            let (lock, cvar) = &*gate;
            *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
            cvar.notify_all();
        }
        let _ = producer.join();
        let _ = loop_thread.join();

        assert!(
            call_ran,
            "the ask never reached the stand-in platform call at all, so this \
             test never set up the overlap it is here to rule out"
        );
        assert!(
            reached,
            "a thread asking for a frame carried out the platform call itself \
             and was still inside it, holding a lock the loop thread takes: \
             this is the deadlock that stops the app rendering entirely on \
             macOS"
        );
    }

    /// A burst is one ask, and the newest payload is the one carried out.
    ///
    /// Not an optimisation to taste: every ask is a round trip to the main
    /// thread on macOS, and the producers burst — a tile decode storm asks per
    /// tile. Newest-wins is what makes a window replaced across a suspend the
    /// one asked rather than the dead one.
    // wasm32 has no threads, so the poster this exercises is absent there.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_burst_of_asks_is_carried_out_as_one_and_keeps_the_newest() {
        use std::sync::{Arc, Mutex, PoisonError, mpsc};
        use std::time::Duration;

        let seen = Arc::new(Mutex::new(Vec::new()));
        let (ran, carried) = mpsc::channel();

        let recorded = Arc::clone(&seen);
        // Parks the poster thread INSIDE `act` until released. That is what
        // makes this deterministic rather than a race: while it is parked, the
        // burst below is provably still queued, so the coalescing has
        // something to coalesce and the test is not timing the threads.
        let (release, wait) = mpsc::channel::<()>();
        let post = coalescing_poster("test-redraw-burst", move |n: &u32| {
            recorded
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(*n);
            let _ = ran.send(());
            let _ = wait.recv();
        });

        post(0);
        assert!(
            carried.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the first ask was never carried out, so nothing below is about \
             coalescing"
        );

        // The poster thread is now parked inside `act` and its queue is empty,
        // so all seven of these are waiting when it comes back round.
        for n in 1..=7 {
            post(n);
        }
        let _ = release.send(());
        assert!(
            carried.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the seven queued asks were never carried out at all, so a burst \
             is being dropped rather than coalesced — a producer's arrival \
             would never draw"
        );
        let _ = release.send(());

        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(
            seen,
            vec![0, 7],
            "eight asks were carried out as {seen:?}. Two rounds are expected \
             — the first ask, then the seven that queued behind it collapsed \
             into one carrying the NEWEST payload. More rounds means the \
             coalescing is gone and macOS pays a main-thread round trip per \
             tile; a different last value means a suspend would ask a dead \
             window."
        );
    }

    #[test]
    fn drain_latest_returns_the_newest_value() {
        let (tx, rx) = std::sync::mpsc::channel();
        for v in [1, 2, 3] {
            tx.send(v).unwrap();
        }

        assert_eq!(drain_latest(&rx), Some(3));
    }

    #[test]
    fn drain_latest_is_empty_once_drained() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(7).unwrap();

        assert_eq!(drain_latest(&rx), Some(7));
        assert_eq!(drain_latest(&rx), None, "the value must not be replayed");
    }

    #[test]
    fn drain_latest_on_an_empty_channel_is_none() {
        let (_tx, rx) = std::sync::mpsc::channel::<u8>();
        assert_eq!(drain_latest(&rx), None);
    }

    // wasm32 has no threads, so the definition is absent there.
    #[cfg(not(target_arch = "wasm32"))]
    mod poller {
        use super::super::{RedrawWaker, spawn_state_poller};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;

        fn counted() -> (RedrawWaker, Arc<AtomicUsize>) {
            let waker = RedrawWaker::new();
            let count = Arc::new(AtomicUsize::new(0));
            let probe = Arc::clone(&count);
            waker.install(move || {
                probe.fetch_add(1, Ordering::SeqCst);
            });
            (waker, count)
        }

        #[test]
        fn poller_sends_an_initial_sample_without_waiting() {
            let rx = spawn_state_poller(
                "test-initial",
                Duration::from_secs(3600),
                || true,
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(
                rx.recv_timeout(Duration::from_secs(5)),
                Ok(true),
                "first sample must not be delayed by one interval"
            );
        }

        #[test]
        fn poller_reports_a_change() {
            let state = Arc::new(AtomicBool::new(false));
            let probe = Arc::clone(&state);
            let rx = spawn_state_poller(
                "test-change",
                Duration::from_millis(5),
                move || probe.load(Ordering::Relaxed),
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(false));
            state.store(true, Ordering::Relaxed);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(true) => break,
                    Ok(false) => assert!(
                        std::time::Instant::now() < deadline,
                        "poller never reported the flipped value"
                    ),
                    Err(e) => panic!("poller stopped early: {e:?}"),
                }
            }
        }

        #[test]
        fn a_theme_change_arriving_while_the_app_is_idle_asks_for_a_frame() {
            let (waker, woke) = counted();
            let state = Arc::new(AtomicBool::new(false));
            let probe = Arc::clone(&state);
            let rx = spawn_state_poller(
                "test-wake",
                Duration::from_millis(5),
                move || probe.load(Ordering::Relaxed),
                waker,
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(false));
            let before = woke.load(Ordering::SeqCst);
            state.store(true, Ordering::Relaxed);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while rx.recv_timeout(Duration::from_secs(5)) != Ok(true) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "poller never reported the flipped value"
                );
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while woke.load(Ordering::SeqCst) <= before {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the theme changed and nothing asked for the frame that \
                     would show it"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn an_unchanged_reading_does_not_ask_for_a_frame() {
            let (waker, woke) = counted();
            let rx = spawn_state_poller(
                "test-quiet",
                Duration::from_millis(5),
                || true, // deliberately constant
                waker,
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                rx.try_recv().is_ok(),
                "the poller stopped sending, so this proves nothing about waking"
            );

            assert_eq!(
                woke.load(Ordering::SeqCst),
                1,
                "a reading that never changed woke the loop anyway, which on \
                 Android is a frame every interval for the life of the process"
            );
        }

        #[test]
        fn poller_exits_when_the_receiver_is_dropped_and_the_value_never_changes() {
            let (probe_tx, probe_rx) = std::sync::mpsc::channel();
            let rx = spawn_state_poller(
                "test-exit",
                Duration::from_millis(5),
                move || {
                    let _ = probe_tx.send(());
                    true // deliberately constant
                },
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
            drop(rx);

            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut exited = false;
            while std::time::Instant::now() < deadline {
                match probe_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(()) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        exited = true;
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                }
            }
            assert!(
                exited,
                "poller must stop once its receiver is dropped, even if the \
                 sampled value never changes"
            );
        }

        #[test]
        fn poller_stops_sampling_after_exit() {
            let calls = Arc::new(AtomicUsize::new(0));
            let probe = Arc::clone(&calls);
            let rx = spawn_state_poller(
                "test-quiesce",
                Duration::from_millis(5),
                move || {
                    probe.fetch_add(1, Ordering::SeqCst);
                    true
                },
                RedrawWaker::new(),
            )
            .unwrap();

            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
            drop(rx);

            std::thread::sleep(Duration::from_millis(200));
            let settled = calls.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));

            assert_eq!(
                calls.load(Ordering::SeqCst),
                settled,
                "detector was still being called after the receiver was dropped"
            );
        }
    }
}
