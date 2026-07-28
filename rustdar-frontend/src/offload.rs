//! Where a long-running, CPU-bound job runs.
//!
//! Four places in this crate hand a closure somewhere it will not stall the
//! frame that created it: the static radar render, the loop-frame render, the
//! overlay rasterization and the radar-sites rasterization. All four have the
//! same shape — a `FnOnce` that ends by sending its result on an
//! `mpsc::Sender` and calling `notify_redraw` — and all four had the same
//! `std::thread::Builder` call written out inline.
//!
//! They are funnelled through here so the wasm arm exists once.
//!
//! # Two shapes, one funnel
//!
//! A closure cannot be posted to a Web Worker, so the funnel takes work in two
//! forms and makes one decision about both:
//!
//! * [`offload`] takes an opaque `FnOnce`. It runs on a thread natively and
//!   inline on the web, which is the best available answer for a job whose
//!   inputs cannot be described — see [`offload`]'s own note on which those are.
//! * [`offload_job`] takes a [`JobRequest`], which *is* a description. Given a
//!   worker it posts; without one it runs [`execute`] in exactly the place
//!   [`offload`] would have run the closure.
//!
//! The second is not a second code path. Both arms of [`offload_job`] call the
//! same [`execute`] and the same `deliver`, so the fallback is derived from the
//! worker path rather than written beside it, and there is no pair to drift.

use rustdar_radar::render_input::RenderInput;
use std::cell::RefCell;
use std::collections::HashMap;

/// Run `job` away from the frame that requested it.
///
/// Native spawns a named OS thread and returns immediately.
///
/// wasm32-unknown-unknown has no threads: `std::thread::Builder::spawn` there
/// returns `Err(Unsupported)` at *runtime* rather than failing to compile, so a
/// bare spawn site does not break the web build — it compiles clean and then
/// panics the first time the user asks for a radar frame. That is the failure
/// this function exists to remove. The web arm runs `job` inline.
///
/// Running inline blocks the frame. For rasterization that is a visible stall,
/// and [`offload_job`] is the answer for the paths that can describe their
/// input. The two that cannot stay here:
///
/// * `overlay-render` captures a `RasterizeFn` — a `Box<dyn FnOnce(..) -> ..>`
///   holding overlay handler state — and answers with a `HitMap` whose
///   `id_map` is a `HashMap<u32, Arc<dyn OverlayItem>>`. Neither a trait-object
///   closure nor a trait-object map crosses a message port. Making it portable
///   means returning a `u32` id image and rebuilding the map on this side, a
///   refactor of `rustdar-overlays` against a rasterizer that draws vector
///   shapes rather than the 28 M projections the radar one does.
/// * `sites-render` is portable — a `Vec<RadarSiteInfo>` in, a `Vec<u8>` out —
///   and simply is not expensive enough yet to be worth a second job kind.
///
/// Inline execution preserves the contract the callers actually depend on. Each
/// `job` delivers through a channel that is drained on a later frame, so a send
/// that happens before the caller returns is indistinguishable from one that
/// happens after it — the receiver cannot tell, and neither can the render
/// budget, whose `RenderGuard` simply drops sooner.
///
/// The `Send` bound is kept on both arms deliberately. It costs the web arm
/// nothing (every existing caller already satisfies it, since they were written
/// for threads) and dropping it would silently license a `!Send` job that then
/// fails to compile on desktop.
pub fn offload(name: &'static str, job: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(job)
            .unwrap_or_else(|e| panic!("failed to spawn {name} thread: {e}"));
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Timed because this is the one arm where the cost lands on the frame.
        // The number is what decides whether a worker is needed and how many, so
        // it is logged rather than estimated.
        let started = web_time::Instant::now();
        job();
        log::info!(
            "{name} took {} ms on the main thread",
            started.elapsed().as_millis()
        );
    }
}

/// A CPU-bound job described as data, so it can be executed somewhere that does
/// not share this thread's memory.
///
/// Every variant is an *input* to a render, never its output: what travels is
/// the smallest thing the renderer can be re-run from, because re-running it is
/// how the worker and this thread stay byte-identical without a second
/// implementation to keep in step.
#[derive(Debug, Clone, PartialEq)]
pub enum JobRequest {
    /// Rasterize a Level II frame.
    Radar {
        /// Boxed because a `RenderInput` owns its gate bytes and is the largest
        /// thing in the enum by three orders of magnitude.
        input: Box<RenderInput>,
        /// Whether the caller wants the per-pixel value grid back.
        ///
        /// Static pane renders do — it is what a hover reads. Loop frames drop
        /// it on arrival, and it is the same size as the texture, so returning
        /// it would copy `IMAGE_SIZE² × 4` bytes across a worker boundary per
        /// frame purely to discard them.
        ///
        /// The texture is unaffected either way; only the grid is cleared.
        values_wanted: bool,
    },
    /// Rasterize a Level III radial product.
    ///
    /// The product's *bytes*, not its decoded form: a `Level3Message` holds
    /// run-length radial packets with no serde derives anywhere in the graph,
    /// and re-decoding is both cheap against the render and a use of the one
    /// decoder rather than a second description of the format. The decode moves
    /// off the main thread with the render as a result.
    Level3 {
        bytes: std::sync::Arc<Vec<u8>>,
        product: rustdar_radar::types::RadarProduct,
        radar_lat: f64,
        radar_lon: f64,
    },
    /// Rasterize a storm-relative velocity field derived from a Level III
    /// dealiased velocity product.
    ///
    /// The derivation travels as its two inputs — the source product's bytes
    /// and the storm motion vector — rather than as its output, for the same
    /// reason: `DerivedSrm` has no wire form, and `srm::derive` is pure, so
    /// re-running it in the worker produces the same field.
    Srm {
        bytes: std::sync::Arc<Vec<u8>>,
        motion: rustdar_radar::srm::StormMotionSample,
        radar_lat: f64,
        radar_lon: f64,
    },
}

/// What a rasterizing job produces: the RGBA texture, the range it was
/// projected at, and the per-pixel value grid (`NAN` where no gate landed).
///
/// Named fields rather than the renderer's `(Vec<u8>, f64, Vec<f32>)`: the two
/// buffers are the same shape to a message port, and transposing them would
/// swap a texture for a value grid somewhere with no type error to catch it.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFrame {
    pub image: Vec<u8>,
    pub max_range_km: f64,
    pub values: Vec<f32>,
}

/// `None` where the renderer found nothing to draw — a scan with no matching
/// sweep. Callers treat it as the failure the renderer already meant by it.
pub type JobResult = Option<RenderedFrame>;

impl From<(Vec<u8>, f64, Vec<f32>)> for RenderedFrame {
    fn from((image, max_range_km, values): (Vec<u8>, f64, Vec<f32>)) -> Self {
        Self {
            image,
            max_range_km,
            values,
        }
    }
}

/// A rasterizing job, described where it can be and opaque where it cannot.
///
/// Both arms reach [`offload_job`], which is the point: there is one place that
/// decides where work runs, and adding a job kind does not add a dispatch site.
pub enum Job {
    /// Portable. Goes to the worker when one is attached, and runs through
    /// [`execute`] when none is.
    Described(JobRequest),
    /// Not describable. Level III and derived SRM renders hold decoded product
    /// structures with no wire form yet, so they run where [`offload`] runs
    /// things — a thread natively, this frame in the browser.
    Opaque(Box<dyn FnOnce() -> JobResult + Send>),
}

impl Job {
    /// A job whose answer is "nothing to draw".
    ///
    /// Used where a request cannot even be described because there is no data
    /// behind it. It is deliberately still a *job*: the caller has already
    /// taken a slot in the render budget and marked its pane in flight, and
    /// those are unwound by `deliver` running, not by returning early.
    pub fn renders_nothing() -> Self {
        Self::Opaque(Box::new(|| None))
    }
}

impl JobRequest {
    /// Encode for a worker. The framing is one tag byte and then the variant's
    /// own bytes, so a new variant cannot be mistaken for an old one.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Radar {
                input,
                values_wanted,
            } => {
                let mut out = Vec::new();
                out.push(TAG_RADAR);
                out.push(u8::from(*values_wanted));
                out.extend_from_slice(&input.to_bytes());
                out
            }
            Self::Level3 {
                bytes,
                product,
                radar_lat,
                radar_lon,
            } => {
                let mut out = vec![TAG_LEVEL3];
                out.extend_from_slice(&product.wire_code().to_le_bytes());
                out.extend_from_slice(&radar_lat.to_le_bytes());
                out.extend_from_slice(&radar_lon.to_le_bytes());
                out.extend_from_slice(bytes);
                out
            }
            Self::Srm {
                bytes,
                motion,
                radar_lat,
                radar_lon,
            } => {
                let mut out = vec![TAG_SRM];
                out.extend_from_slice(&motion.motion.speed_kt.to_le_bytes());
                out.extend_from_slice(&motion.motion.direction_deg.to_le_bytes());
                out.push(u8::from(motion.motion.is_scit_average));
                match motion.volume {
                    // `None` is a vector the user typed in, and a derived field
                    // built from one must not claim the RPG's provenance — so
                    // the distinction has to survive the wire.
                    None => out.push(0),
                    Some((a, b)) => {
                        out.push(1);
                        out.extend_from_slice(&a.to_le_bytes());
                        out.extend_from_slice(&b.to_le_bytes());
                    }
                }
                out.extend_from_slice(&radar_lat.to_le_bytes());
                out.extend_from_slice(&radar_lon.to_le_bytes());
                out.extend_from_slice(bytes);
                out
            }
        }
    }

    /// `None` on an unknown tag or a payload this build cannot read — the two
    /// ends of a message port can be different builds, so that has to be a
    /// clean refusal rather than a misparse.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (tag, rest) = bytes.split_first()?;
        match *tag {
            TAG_RADAR => {
                let (flag, rest) = rest.split_first()?;
                Some(Self::Radar {
                    values_wanted: match flag {
                        0 => false,
                        1 => true,
                        _ => return None,
                    },
                    input: Box::new(RenderInput::from_bytes(rest)?),
                })
            }
            TAG_LEVEL3 => {
                let mut r = Reader::new(rest);
                Some(Self::Level3 {
                    product: rustdar_radar::types::RadarProduct::from_wire_code(r.u16()?)?,
                    radar_lat: r.f64()?,
                    radar_lon: r.f64()?,
                    bytes: std::sync::Arc::new(r.rest().to_vec()),
                })
            }
            TAG_SRM => {
                let mut r = Reader::new(rest);
                let motion = nexrad_level3::model::StormMotion {
                    speed_kt: r.f32()?,
                    direction_deg: r.f32()?,
                    is_scit_average: match r.u8()? {
                        0 => false,
                        1 => true,
                        _ => return None,
                    },
                };
                let volume = match r.u8()? {
                    0 => None,
                    1 => Some((r.u16()?, r.u32()?)),
                    _ => return None,
                };
                Some(Self::Srm {
                    motion: rustdar_radar::srm::StormMotionSample { motion, volume },
                    radar_lat: r.f64()?,
                    radar_lon: r.f64()?,
                    bytes: std::sync::Arc::new(r.rest().to_vec()),
                })
            }
            _ => None,
        }
    }

    /// For the timing log, so a slow job says which kind it was.
    fn kind(&self) -> &'static str {
        match self {
            Self::Radar { input, .. } => match input.product() {
                rustdar_radar::types::RadarProduct::NormalizedRotation => "radar/nrot",
                _ => "radar",
            },
            Self::Level3 { .. } => "level3",
            Self::Srm { .. } => "srm",
        }
    }
}

const TAG_RADAR: u8 = 1;
const TAG_LEVEL3: u8 = 2;
const TAG_SRM: u8 = 3;

/// A bounds-checked cursor over a job's fixed-width header.
///
/// Every accessor answers `None` rather than panicking: these bytes arrive on a
/// message port and are not trusted. The variable-length tail is whatever
/// [`rest`](Reader::rest) is left holding, so no length prefix can lie about it.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// Do the work.
///
/// Pure, and the *only* implementation: the worker calls it, the native thread
/// calls it, and the inline fallback calls it. That is what makes a frame
/// rendered in a worker byte-identical to one rendered on this thread — the
/// two are not two renderers that agree, they are one renderer.
pub fn execute(request: &JobRequest) -> JobResult {
    match request {
        JobRequest::Radar {
            input,
            values_wanted,
        } => rustdar_radar::render::render_from(input).map(|(image, max_range_km, values)| {
            RenderedFrame {
                image,
                max_range_km,
                // Dropped rather than never produced: the grid is what the
                // rasterizer writes into, and the texture is derived from it.
                // Clearing it here costs nothing and keeps the renderer's
                // output the one thing it has always been.
                values: if *values_wanted { values } else { Vec::new() },
            }
        }),
        JobRequest::Level3 {
            bytes,
            product,
            radar_lat,
            radar_lon,
        } => decode_level3(bytes).and_then(|message| {
            rustdar_radar::render::render_level3_message_to_image(
                &message, *product, *radar_lat, *radar_lon,
            )
            .map(Into::into)
        }),
        JobRequest::Srm {
            bytes,
            motion,
            radar_lat,
            radar_lon,
        } => decode_level3(bytes)
            .and_then(|message| rustdar_radar::srm::derive(&message, motion))
            .and_then(|derived| {
                rustdar_radar::render::render_derived_srm_to_image(&derived, *radar_lat, *radar_lon)
                    .map(Into::into)
            }),
    }
}

/// The product these bytes decode to, or `None` — which the caller reports as a
/// render that drew nothing, the same answer a scan with no matching sweep gets.
fn decode_level3(bytes: &[u8]) -> Option<nexrad_level3::model::Level3Message> {
    match nexrad_level3::decode::decode_product(bytes) {
        Ok(message) => Some(message),
        Err(e) => {
            log::error!("could not decode a Level III product for rendering: {e}");
            None
        }
    }
}

/// [`execute`] straight off the wire, for a worker that holds bytes rather than
/// a `JobRequest`. `None` for a payload it cannot read, which the caller
/// reports back as a failed job rather than dropping silently.
pub fn execute_bytes(bytes: &[u8]) -> JobResult {
    execute(&JobRequest::from_bytes(bytes)?)
}

// ── The worker port ──────────────────────────────────────────────────────────

/// A place to send [`JobRequest`]s that is not this thread.
///
/// Implemented by `rustdar-web` over a dedicated `Worker`. It is a trait, and
/// installed rather than constructed here, because the dependency runs the
/// other way: `rustdar-web` depends on this crate, and nothing in this crate
/// may reach back for `web-sys`.
pub trait WorkerPort {
    /// Send `request` to be executed. `id` comes back with the reply so the
    /// funnel can pair them.
    ///
    /// `false` if it could not be posted at all, which makes the caller run the
    /// job here instead of waiting for a reply that is not coming.
    fn post(&self, id: u64, request: Vec<u8>) -> bool;
}

/// The state a posted job needs when its reply lands.
struct Pending {
    kind: &'static str,
    started: web_time::Instant,
    /// Holds the `RenderGuard`, the pane's `Arc<AtomicBool>` and the response
    /// channel. Consuming it is what decrements the render budget and clears
    /// the pane's in-flight mark, so it must run on *every* path out of the
    /// pending map — reply, worker loss, or shutdown.
    deliver: Box<dyn FnOnce(JobResult) + Send>,
}

thread_local! {
    /// Single-threaded by construction: only the browser build installs a port,
    /// and the browser's main thread is the only place these are registered or
    /// retired.
    static WORKER: RefCell<Option<Box<dyn WorkerPort>>> = const { RefCell::new(None) };
    static PENDING: RefCell<HashMap<u64, Pending>> = RefCell::new(HashMap::new());
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// Route [`offload_job`] through `port` from now on.
///
/// Called once, from `rustdar-web`'s entry point, after the worker has proved
/// itself with a build-token handshake. Until then — and forever, on a browser
/// where the worker could not start — [`offload_job`] runs jobs inline, which
/// is the behaviour the web build had before any of this existed.
pub fn set_worker(port: Box<dyn WorkerPort>) {
    WORKER.with(|w| *w.borrow_mut() = Some(port));
}

/// Give up on the worker: it died, or answered the handshake with a build that
/// is not this one.
///
/// Every job it still owes is failed rather than forgotten. Dropping them would
/// leak the render budget and leave panes marked in-flight forever; failing
/// them clears both, and the next frame re-dispatches — inline now, because the
/// port is gone.
pub fn abandon_worker(reason: &str) {
    let had_port = WORKER.with(|w| w.borrow_mut().take().is_some());
    let orphaned: Vec<Pending> = PENDING.with(|p| p.borrow_mut().drain().map(|(_, v)| v).collect());
    if had_port || !orphaned.is_empty() {
        log::warn!(
            "rasterization worker abandoned ({reason}); failing {} in-flight job(s)",
            orphaned.len()
        );
    }
    for pending in orphaned {
        (pending.deliver)(None);
    }
}

/// Whether jobs are currently going to a worker. For diagnostics and tests.
pub fn worker_attached() -> bool {
    WORKER.with(|w| w.borrow().is_some())
}

/// Run `request` away from the frame that requested it, and hand the result to
/// `deliver`.
///
/// `deliver` runs where the result can be used: on the spawned thread natively,
/// and on the main thread in the browser. It is the whole tail of the old
/// closure — the `RenderGuard`, the cancellation check, the channel send and
/// the redraw — so the cancellation semantics are not reimplemented here, they
/// are carried inside it.
///
/// That is also what keeps `PaneRenderState::want_result`'s pruning honest. It
/// treats `Arc::strong_count(flag) > 1` as "still running", and the second
/// reference used to be the one the offloaded closure held. It is now the one
/// `deliver` holds, kept alive by the pending map for exactly as long as the
/// job is outstanding.
pub fn offload_job(name: &'static str, job: Job, deliver: impl FnOnce(JobResult) + Send + 'static) {
    let request = match job {
        Job::Described(request) => request,
        // Nothing to post. This is the same `offload` the opaque callers use
        // directly, reached through the funnel rather than around it.
        Job::Opaque(run) => return offload(name, move || deliver(run())),
    };
    let kind = request.kind();
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id.wrapping_add(1));
        id
    });

    // Try the worker on every target. Nothing installs one outside the browser,
    // so this is a single load of a `None` on desktop — and it means the
    // browser path is reachable from a host test with a fake port rather than
    // only from a browser.
    let posted = WORKER.with(|w| {
        w.borrow()
            .as_ref()
            .map(|port| port.post(id, request.to_bytes()))
    });
    match posted {
        Some(true) => {
            PENDING.with(|p| {
                p.borrow_mut().insert(
                    id,
                    Pending {
                        kind,
                        started: web_time::Instant::now(),
                        deliver: Box::new(deliver),
                    },
                );
            });
            return;
        }
        // The port exists but would not take the job. Falling through runs it
        // here, which is slow but correct; a port that keeps refusing is a
        // worker that has died, and `abandon_worker` is what retires it.
        Some(false) => log::warn!("{name}: worker refused the job; running it here"),
        None => {}
    }

    offload(name, move || deliver(execute(&request)));
}

/// Hand a worker's answer to the job that asked for it.
///
/// Called by `rustdar-web` from the worker's `onmessage`, on the main thread.
/// An `id` with no pending entry is ignored: it is a reply to a job that
/// [`abandon_worker`] already failed, and delivering it twice would send two
/// responses for one render.
pub fn deliver_worker_reply(id: u64, result: JobResult) {
    let Some(pending) = PENDING.with(|p| p.borrow_mut().remove(&id)) else {
        log::debug!("worker reply {id} has no pending job; already abandoned");
        return;
    };
    // The counterpart of `offload`'s wasm log line: the same measurement, for
    // the arm where the time is *not* spent on this thread.
    log::info!(
        "{} took {} ms in the worker",
        pending.kind,
        pending.started.elapsed().as_millis()
    );
    (pending.deliver)(result);
}

/// How many jobs a worker owes an answer for. For diagnostics and tests.
pub fn jobs_in_worker() -> usize {
    PENDING.with(|p| p.borrow().len())
}

#[cfg(test)]
mod tests {
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
        }
    }

    /// The smallest real payload: one sweep of one radial carrying no moment.
    fn sample_input_bytes() -> Vec<u8> {
        use nexrad_model::data::{
            PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
        };
        let radial = Radial::new(
            0,
            0,
            0.0,
            1.0,
            RadialStatus::IntermediateRadialData,
            1,
            0.5,
            Some(nexrad_model::data::MomentData::from_fixed_point(
                4,
                0,
                250,
                8,
                2.0,
                66.0,
                vec![200; 4],
            )),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let scan = Scan::new(
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
                Vec::new(),
            ),
            vec![Sweep::new(1, vec![radial])],
        );
        RenderInput::extract(
            &scan,
            0.5,
            rustdar_radar::types::RadarProduct::Reflectivity,
            35.0,
            -97.0,
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
        }
    }

    fn an_srm_job(volume: Option<(u16, u32)>) -> JobRequest {
        JobRequest::Srm {
            bytes: std::sync::Arc::new(vec![1, 2, 3]),
            motion: rustdar_radar::srm::StormMotionSample {
                motion: nexrad_level3::model::StormMotion {
                    speed_kt: 42.5,
                    direction_deg: 231.25,
                    is_scit_average: true,
                },
                volume,
            },
            radar_lat: 35.0,
            radar_lon: -97.0,
        }
    }

    #[test]
    fn every_job_kind_survives_the_wire_format() {
        for job in [
            a_job(),
            a_level3_job(),
            // Both arms: `None` is a storm motion the user typed in, and a
            // derived field built from one must not claim the RPG's provenance.
            an_srm_job(Some((12, 34567))),
            an_srm_job(None),
        ] {
            assert_eq!(
                JobRequest::from_bytes(&job.to_bytes()),
                Some(job.clone()),
                "{:?} did not survive its round trip",
                job.kind()
            );
        }
    }

    #[test]
    fn a_malformed_job_is_refused_rather_than_misread() {
        assert_eq!(JobRequest::from_bytes(&[]), None, "empty");
        assert_eq!(JobRequest::from_bytes(&[0xFF, 1, 2]), None, "unknown tag");
        assert_eq!(JobRequest::from_bytes(&[TAG_RADAR]), None, "no flag");
        assert_eq!(JobRequest::from_bytes(&[TAG_RADAR, 1]), None, "no payload");
        assert_eq!(
            JobRequest::from_bytes(&[TAG_RADAR, 2]),
            None,
            "the flag is a bool, not a byte"
        );

        // A truncated header must not be read as a short one. The variable tail
        // is whatever is left, so only the fixed part can be checked this way.
        for job in [a_job(), a_level3_job(), an_srm_job(Some((1, 2)))] {
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
        bad_product[1] = 0xFE;
        bad_product[2] = 0xFF;
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
        assert_eq!(execute(&an_srm_job(None)), None);
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
            Some(RenderedFrame {
                image: vec![0; 4],
                max_range_km: 230.0,
                values: vec![f32::NAN],
            }),
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
}
