use crate::channels::{Level3Response, OverlayRenderResponse, ScanData, ScanResponse};
use crate::constants::LOOP_IMAGE_SIZE;
use crate::render_dispatch::RenderGuard;
use chrono::NaiveDateTime;
use chrono::TimeZone;
use rustdar_egui::actions::GuiAction;
use rustdar_egui::shell_api::GuiEvent;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use rustdar_radar::types::RadarProduct;
use std::sync::atomic::Ordering;
use winit::event_loop::ActiveEventLoop;

/// Parameters for a background overlay rasterization request.
pub(super) struct OverlayRenderRequest {
    /// The pane's viewport, *before* overdraw is applied.
    pub geo_bounds: rustdar_overlays::types::GeoBounds,
    /// Pixel dimensions and the overdraw fraction they were sized for, already
    /// reconciled with the adapter's `max_texture_dimension_2d` by
    /// `rustdar_egui::overlay_cache::plan_overlay_texture`.
    pub texture: rustdar_egui::overlay_cache::OverlayTexturePlan,
    pub data_generation: u64,
    pub zoom: i32,
}
use rustdar_radar::scan;
use std::future::Future;
use std::sync::mpsc::Sender;

/// `Send` on native, no constraint on web.
///
/// The fetch path crosses threads on native — `tokio::spawn` *requires*
/// `Send + 'static` on a multi-threaded runtime — and cannot on web, where
/// reqwest's futures hold `Rc<RefCell<..>>` internally and are `!Send` by
/// construction.
///
/// This exists so the bound can vary while the *code* does not: without it,
/// every `spawn_async_task` caller would need a cfg'd twin, and twinned bodies
/// drift. The blanket impls make it a no-op at every call site.
///
/// **This is the fetch path only.** `render_dispatch` spawns real OS threads
/// and requires `Send` on every target — do not relax it to match. An earlier
/// plan conflated the two, which yields something that compiles for web while
/// quietly breaking desktop threading.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}

/// See the native variant above.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// Whether an RPG publishes the Level III objects this app fetches for `site`.
///
/// The fetch-side twin of the offering gate in
/// `rustdar_radar::types::ScanInfo::from_scan`, and it has to exist separately:
/// **not offering a product is not the same as not fetching it.**
/// [`super::App::spawn_level3_fetches`] walks
/// [`RadarProduct::level3_codes_for`] over the whole product table, not over
/// what a pane offers, so without this a TDWR pane asked S3 for four objects
/// that do not exist on every scan load and every poll — each a *doubled*
/// request, since `crate::level3`'s day listing falls back to yesterday when
/// today's prefix comes back empty — and filled the log with fetch-failed
/// warnings for it.
///
/// Same rule as the offering gate, for the same reason: a TDWR's Supplemental
/// Product Generator makes its own short list of products and none of
/// `N0K`/`EET`/`DVL`/`DPR` (evidence recorded at the offering gate). A site that
/// is not in `rustdar_radar::sites::radars()` at all fetches, which is what it
/// did before this gate existed — an unrecognised id is far more likely to be a
/// new WSR-88D than a TDWR. The table is resolved at runtime and can grow, so
/// this asks it rather than a compiled-in array, and a site learned this
/// session is gated on exactly the same rule as a seeded one.
pub(super) fn site_offers_level3(site: &str) -> bool {
    rustdar_radar::sites::get_radar_site(site).is_none_or(|radar| radar.is_wsr88d())
}

/// The AWIPS id of the RPG's Melting Layer product (Level III code 166).
///
/// Deliberately not in [`RadarProduct::level3_codes_for`]'s walk: nothing draws
/// this object. It is a *render input* to the hybrid classification, paired to
/// one volume, and the by-code cache that walk fills is keyed by code alone
/// and takes the latest — which for this object is the wrong answer four
/// minutes out of every five.
const MELTING_LAYER_CODE: &str = "N0M";

/// The AWIPS id of the RPG's Storm Relative Velocity product (Level III 56).
///
/// Out of [`RadarProduct::level3_codes_for`]'s walk for exactly the reason
/// [`MELTING_LAYER_CODE`] is: nothing draws this object either. SRV is derived
/// locally from the Level II velocity volume
/// (`rustdar_radar::srv`) — what is wanted from `N0S` is the two scalars in its
/// Product Description Block, the vector the RPG itself applied, and that is a
/// *render input* paired to one volume. The by-code cache is keyed by code
/// alone and takes the latest, which for this object is the wrong answer four
/// minutes out of every five.
const STORM_MOTION_CODE: &str = "N0S";

/// What a loop frame's pixels stood on, carried out of `spawn_loop_frame_render`'s
/// delivery closure as one value.
///
/// Both fields are the same kind of fact: a render input that changes what the
/// picture *means* and that nothing downstream can recover from the pixels. A
/// pair rather than two more slots in that closure's already-wide tuple, and
/// the reason is not width — it is that the refused-image arm has to clear
/// them **together**. A frame whose buffer the loop rejected depicts nothing,
/// so it stood on nothing, and [`Default`] is the one spelling of that; two
/// loose `None`s in a tuple are two chances to clear one and forget the other.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct FrameProvenance {
    melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

impl super::App {
    /// Spawn a detached future on whatever executor this target provides.
    ///
    /// Two bodies because the executors genuinely differ — native has a
    /// multi-threaded tokio runtime, the browser has its own event loop and no
    /// threads. The `Send` bound rides on [`MaybeSend`] rather than being
    /// written here, so the *callers* stay single-bodied.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn spawn_detached(&self, future: impl Future<Output = ()> + MaybeSend + 'static) {
        self.tokio_runtime.spawn(future);
    }

    /// See the native variant above.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn spawn_detached(&self, future: impl Future<Output = ()> + MaybeSend + 'static) {
        wasm_bindgen_futures::spawn_local(future);
    }

    fn spawn_async_task<T: MaybeSend + 'static>(
        &self,
        sender: Sender<T>,
        future: impl Future<Output = T> + MaybeSend + 'static,
    ) {
        let window = self.window.clone();
        self.spawn_detached(async move {
            let result = future.await;
            let _ = sender.send(result);
            super::notify_redraw(&window);
        });
    }

    /// Hand a downloaded archive's **decode** to the job funnel, then answer on
    /// `sender`.
    ///
    /// # This is the whole of the fix, and it is a split rather than a move
    ///
    /// `scan::fetch_scan` and its three siblings used to download *and* decode
    /// inside one `async fn`, and on the web that future runs on the browser's one
    /// thread — `spawn_detached` is `wasm_bindgen_futures::spawn_local` there. The
    /// frame-thread audit measured what that cost: **1021.9 ms in Firefox 153 and
    /// 911.4 ms in Chrome 151** for a 16.9 MB volume, paid on cold start, on every
    /// timeline scrub, on every "next scan", on every site switch and once per loop
    /// frame. Nothing else this application does blocks a frame for a second.
    ///
    /// The network half has to stay on the async task, because that is where the
    /// fetch stack is. The CPU half does not, and now does not: it goes through
    /// [`crate::offload::offload_job`] as a
    /// [`JobRequest::Decode`](crate::offload::JobRequest::Decode), which means a
    /// Web Worker where there is one and this thread where there is not — the same
    /// fallback every render already has, and the same behaviour the build had
    /// before any of this existed.
    ///
    /// An associated function rather than a method: it is called from inside the
    /// spawned future, which has already moved everything it needs out of `&self`
    /// and cannot borrow it again.
    ///
    /// `respond` answers `None` to send nothing at all, which is what the auto-poll
    /// path needs: a round that found no new volume must not wake the timeline.
    fn decode_offloaded<T: Send + 'static>(
        window: Option<crate::WindowRef>,
        sender: Sender<T>,
        archive: Vec<u8>,
        respond: impl FnOnce(Option<rustdar_radar::scan::DecodedScan>) -> Option<T> + Send + 'static,
    ) {
        crate::offload::offload_job(
            "level2-decode",
            crate::offload::Job::Described(crate::offload::JobRequest::Decode {
                archive: std::sync::Arc::new(archive),
            }),
            move |result| {
                // `None` here is an archive that did not decode, which
                // `execute`'s arm has already logged. Every caller treats it as
                // the failed fetch it is.
                let volume = result
                    .and_then(crate::offload::JobOutput::volume)
                    .map(|boxed| *boxed);
                if let Some(message) = respond(volume) {
                    let _ = sender.send(message);
                }
                crate::app::notify_redraw(&window);
            },
        );
    }

    /// Refresh the cached network site catalogue, once per launch, detached.
    ///
    /// # Nothing here is on the critical path, by construction
    ///
    /// The table has already been resolved from the *cached* catalogue by the
    /// time this runs — see `App::new` — so this fetch changes nothing about
    /// this session. It writes the cache and the next launch reads it. A
    /// failure is therefore not a degraded mode: the app was already running on
    /// the cache plus the compiled-in seed, and stays there.
    ///
    /// That is also why there is no timeout of its own beyond the request's, no
    /// retry, and no spinner. A refresh that takes a minute on a bad link, or
    /// never lands because the machine is offline, is indistinguishable from
    /// one that landed instantly — which is what makes "resolution before first
    /// paint" and "a catalogue from the network" compatible at all.
    ///
    /// # Once, and not per poll
    ///
    /// Two requests, ~11 KB and ~22 KB gzipped, at launch. The thing being
    /// fetched is which radars exist and where — a set that changes when a
    /// radar is commissioned or relocated, which is a step function that steps
    /// a few times a decade. Anything more often would re-download the same
    /// answer.
    pub(super) fn spawn_site_catalogue_refresh(&self) {
        let sender = self.channels.site_catalogue_sender.clone();
        let window = self.window.clone();
        self.spawn_detached(async move {
            let catalogue = rustdar_radar::catalogue::fetch(&Default::default()).await;
            let _ = sender.send(crate::channels::SiteCatalogueResponse { catalogue });
            super::notify_redraw(&window);
        });
    }

    /// Spawn an async radar data fetch on the background runtime.
    /// Handles generation tracking, result sending, and redraw requests.
    pub fn spawn_fetch(&mut self, site: String, timestamp: NaiveDateTime) {
        let generation = self.render.next_fetch_generation(&site);
        let window = self.window.clone();
        let sender = self.channels.scan_sender.clone();
        self.spawn_detached(async move {
            log::info!("Fetching {} @ {} UTC", site, timestamp);
            // The download stays on this task; the decode goes to the funnel.
            let archive = match scan::fetch_scan(&site, timestamp).await {
                Ok(archive) => archive,
                Err(e) => {
                    let err = format!("Failed to fetch radar scan: {:?}", e);
                    log::error!("{}", err);
                    let _ = sender.send(ScanResponse {
                        generation,
                        site,
                        result: Err(err),
                        is_auto_poll: false,
                    });
                    crate::app::notify_redraw(&window);
                    return;
                }
            };
            Self::decode_offloaded(window, sender, archive, move |volume| {
                let result = match volume {
                    Some(volume) => {
                        log::info!("Fetched scan: {} @ {}", site, timestamp);
                        Ok(ScanData {
                            scan: volume.scan,
                            declared_nyquist: volume.declared_nyquist,
                            site: site.clone(),
                            timestamp,
                        })
                    }
                    // The archive arrived and would not decode. `execute` has
                    // logged why; this is what the pane is told.
                    None => {
                        let err = format!("Could not decode the volume for {site} @ {timestamp}");
                        log::error!("{err}");
                        Err(err)
                    }
                };
                Some(ScanResponse {
                    generation,
                    site,
                    result,
                    is_auto_poll: false,
                })
            });
        });
    }

    /// Spawn Level III product fetches for all supported Level III products.
    /// Called after a Level II scan loads so the products are available
    /// alongside the base moments.
    ///
    /// Two spawns, gated differently, and the split is the point: the sounding
    /// goes out for every site, and the Level III objects only for a site
    /// [`site_offers_level3`] says has an RPG behind it.
    pub(super) fn spawn_level3_fetches(&self, site: &str) {
        let generation = self
            .render
            .fetch_generations
            .get(site)
            .copied()
            .unwrap_or(0);
        {
            // Environmental 0 °C / −20 °C heights, for the products
            // `RadarProduct::reads_env_heights` names.
            // TTL-gated: Open-Meteo serves hourly model rows, so refetching
            // on every poll would re-download the same numbers.
            // Stale-or-missing is the only trigger, and a failed fetch
            // stores nothing, so the gate retries it next poll.
            let now = chrono::Utc::now();
            let fresh = self
                .render
                .env_heights
                .get(site)
                .is_some_and(|h| !h.is_stale(now));
            if !fresh && let Some(radar) = rustdar_radar::sites::get_radar_site(site) {
                let (lat, lon) = (radar.lat, radar.lon);
                let site = site.to_string();
                self.spawn_async_task(self.channels.sounding_sender.clone(), async move {
                    let heights = rustdar_radar::sounding::fetch_env_heights(
                        &rustdar_radar::sources::DataSources::production(),
                        lat,
                        lon,
                    )
                    .await;
                    crate::channels::SoundingResponse {
                        generation,
                        site,
                        heights,
                    }
                });
            }
        }
        // The sounding above is deliberately outside this gate: the hail pair
        // computes locally off the Level II volume and needs those
        // environmental heights at *every* site, TDWR included.
        if !site_offers_level3(site) {
            log::debug!("{site} has no RPG, so no Level III objects are fetched for it");
            return;
        }
        // The RPG's own Melting Layer object (Level III 166, AWIPS `N0M`) for
        // the volume this site currently has loaded — the top rung of
        // `rustdar_radar::hca::resolve_melting_layer`, and the difference
        // between a classification that agrees with the RPG's own `N0H` 83–96 %
        // of the time and one that agrees 16 % of the time in winter.
        //
        // # Why here, and why the volume start is read rather than passed
        //
        // This function is called from three places and every one of them
        // installs the site's `ScanInfo` on the pane immediately before calling
        // — the archive drain, the chunk feed's volume close, and JumpToLive's
        // cached-scan path. So "the volume this site has loaded" is already on
        // screen by the time we get here, and `latest_scan_time_for_site` is
        // the app's existing answer to that question: it reads each pane's own
        // `scan_info.timestamp`, which is the same quantity a loop frame pairs
        // its Level III objects against (`spawn_loop_l3_pairing`). Threading a
        // fourth parameter through three call sites would put a second answer
        // beside that one, and the two could disagree.
        //
        // No volume loaded means nothing to pair against, so nothing is
        // fetched — which is right: an object fetched for no volume could only
        // ever be applied to a guess about which volume it belonged to.
        //
        // Inside the RPG gate, unlike the sounding above it: this is a Level
        // III object and a TDWR's Supplemental Product Generator does not make
        // one.
        if let Some(volume_start) = latest_scan_time_for_site(self.gui.panes(), site) {
            // Already in hand for this volume: the poll would re-download an
            // object we are already classifying against. Not a TTL — the
            // identity is the whole gate, and it opens exactly when the volume
            // rolls.
            if self.render.melting_layer_volume(site) != Some(volume_start) {
                let sender = self.channels.melting_layer_sender.clone();
                let site = site.to_string();
                self.spawn_async_task(sender, async move {
                    let sources = rustdar_radar::sources::DataSources::production();
                    // `VolumePick::NEAREST`: `N0M` is a once-per-volume
                    // product, so the object nearest the volume start that
                    // *names* the volume is the object. The naming is checked
                    // inside — nothing here falls back to the newest key.
                    let found = rustdar_radar::level3::fetch_product_for_volume(
                        &sources,
                        &site,
                        MELTING_LAYER_CODE,
                        volume_start,
                        rustdar_radar::level3::VolumePick::NEAREST,
                    )
                    .await;
                    match &found {
                        Some(p) => log::info!(
                            "{site} melting layer for volume {volume_start} is {}",
                            p.stamp.key
                        ),
                        None => log::info!(
                            "{site} published no {MELTING_LAYER_CODE} for volume \
                             {volume_start}; the classification falls back"
                        ),
                    }
                    crate::channels::MeltingLayerResponse {
                        generation,
                        site,
                        volume_start,
                        object: found.map(|p| p.bytes),
                    }
                });
            }
            // The RPG's own storm motion vector (Level III 56, AWIPS `N0S`)
            // for the same volume — the second rung of
            // `rustdar_radar::srv::storm_motion`, and the only rung that
            // reproduces the reference product rather than predicting a storm
            // it might have tracked.
            //
            // Everything about the schedule is the melting layer's above: same
            // gate on the volume already in hand, same per-volume pairing,
            // same side of the `site_offers_level3` return. An SPG makes no
            // `N0S` either.
            if self.render.storm_motion_volume(site) != Some(volume_start) {
                let sender = self.channels.storm_motion_sender.clone();
                let site = site.to_string();
                self.spawn_async_task(sender, async move {
                    let sources = rustdar_radar::sources::DataSources::production();
                    // `VolumePick::NEAREST` for the reason `N0M` uses it: this
                    // is a once-per-volume product, so the object nearest the
                    // volume start that *names* the volume is the object. The
                    // naming is checked inside — nothing here falls back to
                    // the newest key.
                    let found = rustdar_radar::level3::fetch_product_for_volume(
                        &sources,
                        &site,
                        STORM_MOTION_CODE,
                        volume_start,
                        rustdar_radar::level3::VolumePick::NEAREST,
                    )
                    .await;
                    // **Decoded here, and only the two scalars kept.** The one
                    // place this path does not copy `N0M`: that object travels
                    // as bytes so a worker can decode a per-azimuth field
                    // off-thread, while an `N0S` yields a pair out of its PDB
                    // and the pairing above has already decoded that PDB to
                    // check the volume. Carrying the bytes onward would decode
                    // the same header a second time, on the frame thread, for
                    // numbers this future already has in hand.
                    let motion = found.as_ref().and_then(|p| {
                        match nexrad_level3::decode::decode_product(&p.bytes) {
                            Ok(msg) => match msg.pdb.storm_motion() {
                                // `storm_motion` is `Some` only for codes 55
                                // and 56; on anything else halfword 51 is the
                                // BZ2 compression flag and would read as a
                                // plausible-looking lie. The gate lives in
                                // `nexrad-level3`; this arm simply honours it.
                                Some(m) => Some((m.speed_kt, m.direction_deg)),
                                None => {
                                    log::warn!(
                                        "{site} {STORM_MOTION_CODE} for volume \
                                         {volume_start} carries no storm motion \
                                         vector; SRV falls back"
                                    );
                                    None
                                }
                            },
                            Err(e) => {
                                log::warn!(
                                    "{site} {STORM_MOTION_CODE} for volume \
                                     {volume_start} would not decode ({e}); SRV \
                                     falls back"
                                );
                                None
                            }
                        }
                    });
                    match (&found, motion) {
                        // Logged with the numbers because a zero vector is a
                        // reading here and reads like a bug in a log that
                        // omits it: SCIT tracked no cells and the RPG painted
                        // an unshifted field.
                        (Some(p), Some((speed, direction))) => log::info!(
                            "{site} storm motion for volume {volume_start} is \
                             {speed:.1} kt from {direction:.1}° ({})",
                            p.stamp.key
                        ),
                        (Some(_), None) => {}
                        (None, _) => log::info!(
                            "{site} published no {STORM_MOTION_CODE} for volume \
                             {volume_start}; SRV falls back"
                        ),
                    }
                    crate::channels::StormMotionResponse {
                        generation,
                        site,
                        volume_start,
                        motion,
                    }
                });
            }
        }
        // One request per distinct object, not per (product, object). Three
        // products read `DVL` and `EET` between them — VIL, echo tops, and the
        // VIL density derived from both — so walking the per-product table
        // instead asked the bucket for the same two objects twice on every poll
        // of every site. See [`RadarProduct::level3_codes_for`]; the object cache
        // is keyed the same way, so one fetch serves every reader.
        for code in RadarProduct::level3_codes_for(RadarProduct::all()) {
            let site = site.to_string();
            let code = code.to_string();
            self.spawn_async_task(self.channels.level3_sender.clone(), async move {
                log::info!("Fetching Level III {} for {}", code, site);
                let result = match scan::get_level3_product(&site, &code).await {
                    Ok(msg) => {
                        log::info!("Fetched Level III {} for {}", code, site);
                        Ok(msg)
                    }
                    Err(e) => {
                        log::warn!("Level III {} fetch failed: {}", code, e);
                        Err(format!("{e}"))
                    }
                };
                Level3Response {
                    generation,
                    code,
                    site,
                    result,
                }
            });
        }
    }

    pub(super) fn local_to_utc(timestamp: NaiveDateTime) -> NaiveDateTime {
        local_to_utc_in(&chrono::Local, timestamp)
    }

    pub(super) fn handle_gui_action(
        &mut self,
        action: GuiAction,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        match action {
            GuiAction::FetchRadarScan(_)
            | GuiAction::CheckForNewScans(_)
            | GuiAction::SwitchRadarSite { .. } => self.handle_radar_action(action),
            GuiAction::Exit => {
                self.request_exit(event_loop);
            }
            GuiAction::PrepareVolume { pane_idx, target } => {
                self.handle_prepare_volume(pane_idx, target);
            }
            GuiAction::ReleaseVolume { pane_idx } => {
                self.handle_release_volume(pane_idx);
            }
            GuiAction::FetchOverlay { .. } | GuiAction::RefreshOverlay { .. } => {
                self.handle_overlay_action(action)
            }
            GuiAction::RenderOverlay { .. } => {
                // Handled in process_gui_actions() with deduplication
                unreachable!("RenderOverlay should be intercepted by process_gui_actions");
            }
            GuiAction::EnableLoop {
                pane_idx,
                lookback_secs,
            } => {
                self.handle_enable_loop(pane_idx, lookback_secs);
            }
            GuiAction::DisableLoop { pane_idx } => {
                self.handle_disable_loop(pane_idx);
            }
            GuiAction::ToggleLoopPlayback { pane_idx } => {
                if let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let ls = &mut pane.loop_state;
                    match ls.phase {
                        rustdar_egui::pane::LoopPhase::Playing => {
                            ls.phase = rustdar_egui::pane::LoopPhase::Paused;
                        }
                        rustdar_egui::pane::LoopPhase::Ready
                        | rustdar_egui::pane::LoopPhase::Paused => {
                            ls.phase = rustdar_egui::pane::LoopPhase::Playing;
                            ls.last_advance = Some(web_time::Instant::now());
                        }
                        _ => {}
                    }
                }
            }
            GuiAction::StepLoopFrame { pane_idx, forward } => {
                if let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let ls = &mut pane.loop_state;
                    if !ls.frames.is_empty() {
                        if forward {
                            ls.current_frame = (ls.current_frame + 1) % ls.frames.len();
                        } else if ls.current_frame == 0 {
                            ls.current_frame = ls.frames.len() - 1;
                        } else {
                            ls.current_frame -= 1;
                        }
                    }
                }
            }
            GuiAction::SeekLoopFrame {
                pane_idx,
                frame_index,
            } => {
                if let Some(pane) = self.gui.pane_mut(pane_idx) {
                    let ls = &mut pane.loop_state;
                    if frame_index < ls.frames.len() {
                        ls.current_frame = frame_index;
                    }
                }
            }
            GuiAction::NavigateTime {
                pane_idx,
                step_secs,
            } => {
                self.handle_navigate_time(pane_idx, step_secs);
            }
            GuiAction::NavigateOneScan { pane_idx, forward } => {
                self.handle_navigate_one_scan(pane_idx, forward);
            }
            GuiAction::JumpToLive { pane_idx } => {
                self.handle_jump_to_live(pane_idx);
            }
            GuiAction::StartGps { config } => {
                self.platform.start_gps(&config);
            }
            GuiAction::StopGps => {
                self.platform.stop_gps();
            }
            // Through the gate in both directions, never straight at the
            // bridge. The gate is where the persisted memo lives and where the
            // single `request_location` call site is; a handler that reached
            // past it would be a second way to raise a permission dialog, with
            // none of the guards.
            GuiAction::RequestLocation => {
                self.location.enable(self.platform.as_mut());
            }
            GuiAction::StopLocation => {
                self.location.disable(self.platform.as_mut());
                // The location facts reach the UI through the per-frame
                // compose (`push_frame_inputs`), which reads the gate this
                // click just changed.
                //
                // The dot is fed by whatever was delivering. Nothing else will
                // clear it — the gate has already stopped polling — so it goes
                // here, and only when a serial dongle is not also feeding it.
                if !self.platform.gps_active() {
                    self.user_gps = None;
                }
            }
            // Straight at the bridge, and this is the one location action that
            // legitimately is. The gate exists to guard the single call that can
            // raise a permission dialog; opening a settings page raises nothing,
            // changes no state this crate owns, and has nothing to remember.
            GuiAction::OpenLocationSettings => {
                self.platform.open_location_settings();
            }
        }
    }

    /// Handle radar data fetch/switch actions.
    fn handle_radar_action(&mut self, action: GuiAction) {
        match action {
            GuiAction::FetchRadarScan(radar_config) => {
                log::info!(
                    "Fetch radar scan requested: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );
                let utc_timestamp = Self::local_to_utc(radar_config.timestamp);
                self.spawn_fetch(radar_config.site, utc_timestamp);
            }
            GuiAction::CheckForNewScans(radar_config) => {
                // The chunk feed delivers this site's volume cut by cut, minutes
                // earlier, so the 60 s archive check for it is redundant. Skipped
                // here rather than suppressed in `check_auto_polls` so the GUI
                // stays unaware of which transport a site is on.
                if self.chunks_are_feeding(&radar_config.site) {
                    return;
                }
                log::info!(
                    "Check for new scans: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );

                let utc_timestamp = Self::local_to_utc(radar_config.timestamp);
                // This site's own current scan. The GUI emits one check per unique
                // live site, so the active pane's scan time is the wrong "current"
                // for every other one — see [`latest_scan_time_for_site`].
                let current_scan_timestamp =
                    latest_scan_time_for_site(self.gui.panes(), &radar_config.site);

                let generation = self.render.next_fetch_generation(&radar_config.site);
                let site = radar_config.site.clone();
                let window = self.window.clone();
                let sender = self.channels.scan_sender.clone();

                // Not using spawn_async_task: conditional send (only on new data)
                self.spawn_detached(async move {
                    match scan::fetch_latest_if_newer(
                        &site,
                        &utc_timestamp.date(),
                        current_scan_timestamp,
                    )
                    .await
                    {
                        Ok(Some((archive, timestamp))) => {
                            // The decode goes to the funnel; the answer is
                            // still conditional, which is what the `Option`
                            // `decode_offloaded` takes back is for.
                            Self::decode_offloaded(window, sender, archive, move |volume| {
                                let volume = volume?;
                                Some(crate::channels::ScanResponse {
                                    generation,
                                    site: site.clone(),
                                    result: Ok(crate::channels::ScanData {
                                        scan: volume.scan,
                                        declared_nyquist: volume.declared_nyquist,
                                        site,
                                        timestamp,
                                    }),
                                    is_auto_poll: true,
                                })
                            });
                            // `decode_offloaded` redraws when it answers.
                            return;
                        }
                        Ok(None) => { /* already latest or no data */ }
                        Err(e) => {
                            log::error!("Failed to check for new scans: {:?}", e);
                        }
                    }
                    crate::app::notify_redraw(&window);
                });
            }
            GuiAction::SwitchRadarSite { site, pane_idx } => {
                log::info!("Switch radar site requested: pane {} -> {}", pane_idx, site);

                let mut new_config = self.gui.get_radar_config().clone();
                new_config.site = site.clone();
                self.gui.apply(GuiEvent::RadarConfig(new_config.clone()));

                // The linked group moves together; an unlinked pane moves
                // alone. `layer_sync_targets` is `propagate_layer_sync`'s
                // own two-ended gate — a layer-linked source reaches the
                // layer-linked panes, an unlinked source only itself — so
                // the site switch and the egui-side convergence cannot
                // disagree about who moves.
                let moving = self.gui.layer_sync_targets(pane_idx);
                // Every field written here is flat on `PaneState`, so this reaches
                // a section or a volume pane unchanged and correctly: those move
                // site with the layout exactly as a map pane does.
                //
                // A section's drawn **line** is deliberately kept — it is stored
                // *geographically*, so it goes on naming the same ground under
                // the new radar. Whether a line the new site cannot see should be
                // cleared rather than re-cut as empty coverage is a question
                // about what the pane says while it has nothing to show, which
                // belongs with the interaction that draws the line rather than
                // here.
                //
                // The section's **picture** is the opposite and is dropped with
                // the `ScanInfo`, because it is a different radar's data rather
                // than the same radar's older data. Invalidation is not automatic
                // as it looks: `SectionTarget` does carry the site, but
                // `section_target_for_pane` reads the volume time off
                // `pane.scan_info` and returns `None` the moment there is no scan,
                // so after the clear above no target is built and no comparison
                // ever happens. What runs instead is `mark_section_unavailable`,
                // whose whole design is to leave the picture up — correctly, for
                // the case it was written for: "a section of the previous *volume*
                // is stale rather than wrong, and is labelled with its own volume
                // time". Across a site change that reasoning inverts. The caption
                // goes on telling the truth about *when* the cut was taken while
                // the pane's pills name a radar that did not take it, and the
                // hover readout answers with the previous site's values — the plan
                // view's staleness exactly, in the one pane kind that survives it,
                // and it would stand until the new site's first volume.
                //
                // `AwaitingVolume` is what the pane is actually in — its own doc
                // calls it "the ordinary startup and site-switch state" — and it
                // is what the next frame would resolve anyway; setting it here
                // means no frame is drawn claiming a cut is in flight.
                //
                // `scan_info` goes the other way and is dropped, because it is
                // the one field that answers *for the site being left*. It
                // accumulates by design — `Gui::apply_chunk_scan_info` unions a
                // partial volume's products and tilts into it and never removes
                // one, so the tilt picker does not shrink and regrow every few
                // seconds — and that design assumes one radar behind the union.
                // Carried across a switch it is a claim about the wrong one,
                // standing until a completed volume replaces it wholesale, which
                // is up to a volume period away: the pane offers the previous
                // site's products and its VCP's tilts, so a TDWR goes on listing
                // the five Level III entries and the dual-pol moments that
                // `types::discover_product_elevations` withholds for it, and the
                // gate that withholds them never gets to apply. Dropping it here
                // rather than filtering it downstream leaves that union the only
                // merge rule there is, and confines the correction to the one
                // moment the site actually changes.
                //
                // It is also what the render path steers by: `dispatch_pane_renders`
                // takes the origin coordinates and the volume it draws from
                // `scan_info.site`, while `poll_render_results` files the result
                // under `pane.site`. A product picked from the stale menu in that
                // window therefore paints the old radar's field and caches it as
                // the new site's, where every other pane on the new site can pick
                // it up. With no `ScanInfo` the pane resolves no rendering params
                // at all and nothing is dispatched.
                //
                // `data_time` is dropped with it because it describes the picture
                // that goes with it — the same `scan_info`-is-`None` branch of
                // `dispatch_pane_renders` tears the radar texture down — and a
                // status bar aging the old site's volume against a pane showing
                // nothing is the same untruth in words.
                //
                // Nothing has to stand in for them. The pickers already have a
                // written state for holding no scan — "No scan loaded", what they
                // say before a session's first volume. The map does better than
                // that: with no `ScanInfo` it centres on the new site's own
                // `sites::radars()` coordinates, so the switch arrives on the new
                // radar immediately instead of holding the old one's position.
                // The panes that really left a radar, for the volume release
                // below. Collected rather than released inside the loop because
                // the release needs `&mut self` — the store *and* egui's
                // callback resources — while `pane_mut` holds a borrow of
                // `self.gui`.
                let mut left_a_radar: Vec<usize> = Vec::new();
                for idx in moving {
                    if let Some(pane) = self.gui.pane_mut(idx) {
                        // Only where the site really moves. `moving` is the
                        // linked group, and a pane already on `site` — with its
                        // volume drawn and its menu right — is not switching.
                        if pane.site != site {
                            left_a_radar.push(idx);
                            pane.scan_info = None;
                            pane.data_time = None;
                            // The picture and the key that names what it was cut
                            // for, not the line: see above. `texture` goes with
                            // `section` because the two are a pair — the restore
                            // path re-uploads one from the other, so a texture
                            // left behind would be put back on the next surface
                            // loss from a cut that no longer exists.
                            if let Some(xsect) = pane.cross_section_mut() {
                                xsect.section = None;
                                xsect.texture = None;
                                xsect.rendered_for = None;
                                xsect.unavailable =
                                    Some(rustdar_egui::pane::SectionUnavailable::AwaitingVolume);
                            }
                            // The loop is the same judgement as the scan above
                            // and belongs behind the same guard. Its frames are
                            // one radar's files, listed for one radar's window
                            // and rendered at one radar's coordinates, so a real
                            // switch has to throw it away — but a pane re-picking
                            // the site it is already on is showing a loop that is
                            // correct in every one of those respects, and tearing
                            // it down costs the whole listing, every download and
                            // every render again. Nothing rebuilds it either: the
                            // rebuild paths are `handle_enable_loop` and
                            // `reinit_active_loops`, and a re-pick raises neither,
                            // so the pane drops out of loop mode to its static
                            // image with the transport still reading "loop on".
                            pane.loop_state = rustdar_egui::pane::LoopPlaybackState::new();
                        }
                        pane.loading_site = Some(site.clone());
                        pane.site = site.clone();
                        pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                    }
                }
                // The host-side half of the loop teardown, and it is **per
                // pane**, behind the same guard.
                //
                // This used to be `LoopDownloadManager::clear_all`, which emptied
                // the shared volume cache, the shared Level III cache, every
                // pane's download queue and every frame plan whenever *any* pane
                // left a radar. A second pane looping a different site was not
                // in `moving`, so it kept a `LoopPlaybackState` whose frames
                // named volumes that had just been dropped and whose plan was
                // gone: a loop that plays blank, has nothing queued to fill
                // itself, and raises neither of the actions that rebuild one —
                // exactly the dead loop the `pane.site != site` guard above
                // exists to prevent, arriving on the pane the user was not
                // looking at.
                //
                // `remove_pending` is the whole of what this call has to do
                // itself: it takes both of the departing pane's queues and the
                // plan they derive from, which is the state keyed by *pane
                // index* and therefore the state nothing else can attribute. The
                // caches are keyed by site and are collected by
                // `App::evict_unneeded_loop_scans` on the next frame, because
                // the pane has just lost both of its claims on that site — its
                // `scan_info` was cleared above and its `loop_state` reset — and
                // that sweep keeps exactly what live loop frames and pane
                // targets name. Doing it here as well would be a second rule for
                // one question.
                //
                // The queues are still taken *now* rather than left to the
                // sweep's predicate, which would also empty them: the sweep runs
                // in `handle_redraw`, this runs in an action, and a dispatch
                // between the two would spend the shared download budget on the
                // radar the pane just left.
                for idx in &left_a_radar {
                    self.loop_mgr.remove_pending(*idx);
                }

                // The third way a 3D pane stops needing its volume, beside the
                // kind change (`GuiAction::ReleaseVolume`) and the pane-count
                // reduction (`App::release_hidden_pane_volumes`): it changes
                // radar. Nothing downstream covered it, and the reason is the
                // order the frame is written in. `ui_map`'s volume arm returns
                // the "Downloading the first … volume" empty state as soon as
                // the site has no published stamp — *before* it emits
                // `PrepareVolume` and before it paints — so after a switch
                // nothing calls `VolumeStore::share_held`, nothing calls
                // `StoreInner::shed`, and the callback's `prepare` never runs
                // to prune the GPU side either. The pane went on holding the
                // radar it just left: 8.00 MiB of host grid and 36.6 MiB of
                // GPU texture at the desktop cell budget, plus its
                // pane-sized offscreen, until the *new* site's first volume was
                // extracted — which is seconds on a good fetch and never at all
                // on a site that has no data or whose download fails.
                //
                // It was bounded at one grid per pane rather than growing:
                // `same_scope` is site-and-product, so the new site's eventual
                // `begin_build_held` did shed the old one. That makes it a
                // question of *when* rather than of how much, and the answer
                // "when some other site finally delivers" is not an answer.
                // `VolumeStore::enforce_budget` never reclaimed it either — it
                // only fires over budget, and one stale grid never is.
                //
                // Released here rather than left to any of those: the switch is
                // the moment the bytes stop describing anything the user asked
                // for. `handle_release_volume` gives back the host entry, the
                // GPU upload and the offscreen together, and detaching also
                // drops the pane's in-flight `Building` entry, so a build
                // dispatched for the abandoned radar lands on a store that is
                // no longer waiting and is dropped by `VolumeStore::complete`
                // instead of admitting a fresh grid for a site nobody is on.
                //
                // `rendered_for` goes with it, and that half is not
                // bookkeeping. `PrepareVolume` is level-triggered on it, so a
                // pane released while it still named a target would come back
                // from a switch *back* to a site whose stamp is still published,
                // match the stale key, never ask again, and read "Building…"
                // for ever. The same pairing `release_hidden_pane_volumes`
                // makes, for the same reason.
                //
                // The cost is that returning to a site re-resamples rather than
                // finding the grid still in hand. That is the trade
                // `VolumeStore::retain_set` already states for the loop's own
                // set — release first, rebuild after, and accept the first-build
                // message for the fraction of a second it costs — applied to
                // the one holder that was not yet keeping to it.
                for idx in left_a_radar {
                    self.handle_release_volume(idx);
                    if let Some(pane) = self.gui.pane_mut(idx)
                        && let Some(volume) = pane.volume_mut()
                    {
                        volume.rendered_for = None;
                    }
                }

                let utc_timestamp = Self::local_to_utc(new_config.timestamp);
                self.spawn_fetch(site, utc_timestamp);
            }
            _ => unreachable!(),
        }
    }

    /// Handle overlay fetch/refresh actions for all overlay kinds.
    fn handle_overlay_action(&mut self, action: GuiAction) {
        match action {
            GuiAction::FetchOverlay { kind, pane_idx }
            | GuiAction::RefreshOverlay { kind, pane_idx } => {
                self.fetch_overlay(kind, pane_idx);
            }
            _ => unreachable!(),
        }
    }

    /// Fetch overlay data for the given kind, resolving parameters from current state.
    fn fetch_overlay(&mut self, kind: OverlayKind, pane_idx: usize) {
        use rustdar_overlays::render::overlay_state::FetchConfig;

        // Load the requesting pane's config so create_fetch_tasks reads the
        // correct per-pane settings (e.g. selected model parameter, SPC day).
        let pane_configs = self
            .gui
            .pane(pane_idx)
            .map(|p| p.overlay_configs.clone())
            .unwrap_or_default();
        if !pane_configs.is_empty() {
            self.gui.overlays.load_pane_configs(&pane_configs);
        }

        let config = FetchConfig {
            client: self.http_client.clone(),
            zone_cache_dir: self.platform.zone_cache_dir().map(|p| p.to_path_buf()),
            sources: rustdar_radar::sources::DataSources::production(),
            viewport: self.last_viewport,
        };

        let tasks = self.gui.overlays.create_fetch_tasks(kind, &config);
        if tasks.is_empty() {
            // A handler that cannot build a task says so, and is believed.
            //
            // This used to just `return`: nothing was set fetching and no
            // result would arrive to stamp `fetch_time`, so the layer read as
            // due on every following frame — floored at 1 Hz by
            // `App::auto_poll_delay`'s `MIN_WAKE` on native, and at the display
            // rate on web, where the same defect on the failure path cost 3089
            // SPC MD requests in 105 s. Every reason a handler returns nothing
            // here is a reason repetition cannot fix: a client that would not
            // build (`MetarHandler::create_fetch_tasks`), or no product
            // selected to fetch (`SpcOutlookHandler`). Recording it as
            // `FetchFailure::Permanent` says that, and the user actions that
            // could change the answer — Refresh, enabling the layer, picking a
            // product — all clear the ledger through `push_user_overlay_fetch`.
            //
            // "Permanent" is a claim about this attempt, not a sentence: it
            // takes `REFUSALS_BEFORE_BROKEN` in a row before the layer drops off
            // the ordinary ladder, and even then it keeps a `BROKEN_RETRY_SECS`
            // heartbeat. Which is right here too — a client that would not build
            // once may build on the next frame, and the second attempt costs one
            // request that never leaves the process.
            log::warn!("{kind:?}: no fetch task could be built; backing off");
            self.gui.overlays.record_fetch_failure(
                kind,
                &rustdar_overlays::fetch_policy::FetchError::permanent(
                    "no fetch task could be built",
                ),
            );
            return;
        }

        log::info!(
            "Fetching overlay data for {:?} ({} task(s))",
            kind,
            tasks.len()
        );
        self.gui.overlays.set_fetching(kind, true);

        for task in tasks {
            let task_kind = task.kind;
            self.spawn_async_task(self.channels.overlay_fetch_sender.clone(), async move {
                let data = task.future.await;
                OverlayFetchResult {
                    kind: task_kind,
                    data,
                }
            });
        }
    }

    /// The deliver every overlay job shares — sites and the six handler-backed
    /// kinds, which is every texture overlay — built once so the dispatches
    /// cannot drift. (The per-kind alpha convention never reaches this
    /// deliver: `offload::execute` converts the one straight-alpha rasterizer
    /// — the model grid's — inside the job, so the read below is
    /// compute-nothing on every kind alike.)
    ///
    /// `response` arrives **image-less**, which is the failure shape: the
    /// dispatch site fills in every field the poller reads, and this deliver
    /// only ever installs a raster into it. It is sent on every arm, `None`
    /// included, because this message is the only thing that clears the named
    /// panes' in-flight marks — see `OverlayRenderResponse::image`.
    ///
    /// The dispatch's own `width` and `height` are the only statement of the
    /// raster's shape — the reply's buffer carries none
    /// (`offload::JobOutput::OverlayRaster`) — so the length check here is
    /// what stands between a payload from another build and a texture of the
    /// wrong shape.
    ///
    /// `id_map` is the page-side half of a hit map: the
    /// `Arc<dyn OverlayItem>`s captured at dispatch, **index-aligned with the
    /// rows the described input carried** (`OverlayHandler::hit_items` states
    /// the invariant). A reply's cells are zipped with it here — and believed
    /// only if their grid is the quarter-res one this dispatch's dimensions
    /// imply and every index they record names an item the map has. Anything
    /// else is a reply from a build whose layout this is not, and a hit map
    /// zipped across such a mismatch is a hover that names the **wrong**
    /// report — worse than no picture, so the whole render is failed rather
    /// than shown without its clicks.
    ///
    /// The two halves are allowed to be absent together — the kinds with no
    /// hit map dispatch `id_map: None` and their replies say `None` — and a
    /// mixed pairing is refused for the mismatch it is.
    ///
    /// The pixels arrive premultiplied by `offload::execute`'s contract —
    /// converted inside the job for the one rasterizer that declares straight
    /// alpha — so the constructor below is a straight copy: the one
    /// compute-nothing `from_rgba_premultiplied` read the frame-thread
    /// conversion tests permit `app_fetch`.
    fn overlay_job_deliver(
        label: &'static str,
        width: u32,
        height: u32,
        id_map: Option<
            Vec<std::sync::Arc<dyn rustdar_overlays::render::overlay_state::OverlayItem>>,
        >,
        mut response: OverlayRenderResponse,
        sender: Sender<OverlayRenderResponse>,
        window: Option<crate::WindowRef>,
    ) -> impl FnOnce(crate::offload::JobResult) + Send + 'static {
        move |result| {
            let expected = (width as usize) * (height as usize) * 4;
            if let Some((rgba, hit_cells)) =
                result.and_then(crate::offload::JobOutput::overlay_raster)
            {
                let sized = rgba.len() == expected;
                if !sized {
                    log::error!(
                        "{label} answered {} bytes where {width}x{height} \
                         needs {expected}; treating it as a failed render",
                        rgba.len(),
                    );
                }
                let hit_map = match (hit_cells, &id_map) {
                    (None, None) => Ok(None),
                    (Some(cells), Some(items)) => {
                        let grid_agrees =
                            cells.width == width.div_ceil(4) && cells.height == height.div_ceil(4);
                        let ids_fit = cells
                            .max_id()
                            .is_none_or(|max| (max as usize) < items.len());
                        if grid_agrees && ids_fit {
                            Ok(Some(
                                rustdar_overlays::render::rasterize::HitMap::from_cells(
                                    cells, items,
                                ),
                            ))
                        } else {
                            Err("cells that do not fit this dispatch's grid or its items")
                        }
                    }
                    (Some(_), None) => Err("cells this dispatch captured no items for"),
                    (None, Some(_)) => Err("no cells where this dispatch captured items"),
                };
                match hit_map {
                    Ok(hit_map) if sized => {
                        response.hit_map = hit_map;
                        response.image = Some(std::sync::Arc::new(
                            egui::ColorImage::from_rgba_premultiplied(
                                [width as usize, height as usize],
                                &rgba,
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(what) => {
                        // A mismatched hit map zipped anyway would hand
                        // hovers to the wrong items; shown without one, the
                        // mismatch would be invisible until someone clicked.
                        // Both halves came from one build's `execute`, so a
                        // disagreement means the reply is not this build's —
                        // fail the render whole and leave the layer
                        // dispatchable.
                        log::error!("{label} answered {what}; treating it as a failed render");
                    }
                }
            }
            let _ = sender.send(response);
            super::notify_redraw(&window);
        }
    }

    /// Spawn a background thread to rasterize overlay polygons via tiny-skia.
    pub(super) fn spawn_overlay_render(
        &mut self,
        pane_indices: Vec<usize>,
        kind: OverlayKind,
        req: OverlayRenderRequest,
    ) {
        use rustdar_egui::overlay_cache::ZOOM_QUANTIZATION_FACTOR;
        use rustdar_overlays::render::rasterize;

        let OverlayRenderRequest {
            geo_bounds,
            texture,
            data_generation,
            zoom,
        } = req;
        let (width, height) = (texture.width, texture.height);

        if width == 0 || height == 0 {
            return;
        }

        if self.gui.overlays.render_mode(kind)
            != Some(rustdar_overlays::render::overlay_state::RenderMode::Texture)
        {
            log::warn!(
                "spawn_overlay_render called with non-texture kind: {:?}",
                kind
            );
            return;
        }

        // Mark in-flight on the appropriate texture cache for all target panes.
        // Every path out of here that does not reach an `offload` has to undo this
        // with `clear_overlay_render_marks` — see it.
        for &pidx in &pane_indices {
            if let Some(pane) = self.gui.pane_mut(pidx) {
                pane.overlay_cache_mut(kind).render_in_flight = true;
            }
        }

        // The plan answers this itself. There is no fraction to pass, so the one
        // substitution that would break the cache — `OVERDRAW_FRACTION` in place of
        // what the adapter actually allowed — cannot be written here.
        let render_bounds = texture.coverage(&geo_bounds);

        // Use the first target pane for data extraction (all synced panes share config).
        // Clone the pane's overlay config before mutating the registry.
        let first_pane_idx = pane_indices[0];
        let pane_configs = {
            let Some(target_pane) = self.gui.pane(first_pane_idx) else {
                self.clear_overlay_render_marks(&pane_indices, kind);
                return;
            };
            target_pane.overlay_configs.clone()
        };
        if !pane_configs.is_empty() {
            self.gui.overlays.load_pane_configs(&pane_configs);
        }

        let sender = self.channels.overlay_render_sender.clone();
        let window = self.window.clone();

        // Clone the data needed for the render job.
        //
        // **The split below is a decision per kind, spelled as a match and
        // not as a capability probe**: a kind is on the handler-backed path
        // because this list says so, and a kind added to `OverlayKind` does
        // not silently pick a path by whether its handler happens to answer
        // `prepare_job`. The described list and the handlers that implement
        // `prepare_job` must be the same six kinds —
        // `rustdar-overlays`' texture tests pin that set, and
        // `app_fetch::polygon_wire_tests` / `model_wire_tests` pin this
        // routing.
        match kind {
            // The handler-backed kinds — the three polygon kinds, the two
            // hit-map kinds and the model grid, which is every texture kind
            // a handler renders — are **described jobs**
            // (`JobRequest::Overlay`). On the web the job posts to the worker
            // instead of running inline on the browser's one thread, which is
            // where the frame-thread audit measured 224 ms of gesture-end
            // stall for the polygon layer set alone (measured at
            // main@ebe0ad3b, 2026-08-12 web-baseline campaign;
            // instrumentation 3673d316); on native it rides the
            // pool's interactive lane, the same lane the closures rode. There
            // is no other path: the opaque closure arm this match used to
            // have is deleted, with the trait method that fed it.
            OverlayKind::SpcOutlook
            | OverlayKind::SpcDiscussions
            | OverlayKind::NwsAlerts
            | OverlayKind::StormReports
            | OverlayKind::Lightning
            | OverlayKind::ModelData => {
                use rustdar_overlays::render::overlay_state::HandlerJobInput;
                let rctx = rustdar_overlays::render::overlay_state::RasterizeContext {
                    is_dark: self.cached_dark_theme.unwrap_or(false),
                    zoom: zoom as f64 / ZOOM_QUANTIZATION_FACTOR,
                    // Off the plan the pixel count came from, never re-read
                    // from the context: a rasterizer told a different density
                    // than the one its texture was sized at draws every marker
                    // at the wrong size. See `OverlayTexturePlan`.
                    device_scale: texture.pixels_per_point,
                    // THE capture of the page's clock, and the only one on
                    // this path: the GLM flash-age fade reads it off the
                    // described input wherever the job runs, so the worker
                    // renders the ages this dispatch computed rather than
                    // re-reading a clock of its own.
                    now: chrono::Utc::now().naive_utc(),
                };
                let Some(input) = self.gui.overlays.prepare_job(kind, &rctx) else {
                    // Nothing to render — clear in-flight. `has_data()` and
                    // `prepare_job` agree in every reachable state
                    // (`texture_tests`' permanent-wakeup guard), so this
                    // decline is the no-data case and not a kind that lost
                    // its input.
                    self.clear_overlay_render_marks(&pane_indices, kind);
                    return;
                };
                // The page-side half of a hit map, captured in the same
                // synchronous breath as the input above so the two index the
                // same data in the same order — `Some` for exactly the two
                // hit-map kinds. It never touches the wire; the deliver zips
                // it with the reply's cells.
                let id_map = self.gui.overlays.hit_items(kind);
                let (label, input) = match input {
                    HandlerJobInput::Alerts(input) => (
                        "alerts-render",
                        crate::offload::OverlayJobInput::Alerts(Box::new(input)),
                    ),
                    HandlerJobInput::Outlooks(input) => (
                        "outlooks-render",
                        crate::offload::OverlayJobInput::Outlooks(input),
                    ),
                    HandlerJobInput::Discussions(input) => (
                        "discussions-render",
                        crate::offload::OverlayJobInput::Discussions(input),
                    ),
                    HandlerJobInput::Reports(input) => (
                        "reports-render",
                        crate::offload::OverlayJobInput::Reports(input),
                    ),
                    HandlerJobInput::Glm(input) => {
                        ("glm-render", crate::offload::OverlayJobInput::Glm(input))
                    }
                    HandlerJobInput::ModelData(input) => (
                        "model-render",
                        crate::offload::OverlayJobInput::ModelData(Box::new(input)),
                    ),
                };
                let request = crate::offload::JobRequest::Overlay {
                    width,
                    height,
                    bounds: render_bounds,
                    input,
                };
                crate::offload::offload_job(
                    label,
                    crate::offload::Job::Described(request),
                    Self::overlay_job_deliver(
                        label,
                        width,
                        height,
                        id_map,
                        OverlayRenderResponse {
                            image: None,
                            geo_bounds: render_bounds,
                            overlay_kind: kind,
                            generation: data_generation,
                            pane_indices,
                            zoom,
                            hit_map: None,
                        },
                        sender,
                        window,
                    ),
                );
            }
            OverlayKind::RadarSites => {
                let Some(target_pane) = self.gui.pane(first_pane_idx) else {
                    self.clear_overlay_render_marks(&pane_indices, kind);
                    return;
                };
                let target_site = target_pane.site.clone();
                let target_loading = target_pane.loading_site.clone();
                let is_dark = self.cached_dark_theme.unwrap_or(false);
                let actual_zoom = zoom as f64 / ZOOM_QUANTIZATION_FACTOR;
                // See the `RasterizeContext` above: the density the pixels were
                // counted at is the density the symbols are drawn at.
                let device_scale = texture.pixels_per_point;
                let sites: Vec<rasterize::RadarSiteInfo> = rustdar_radar::sites::radars()
                    .iter()
                    .map(|s| rasterize::RadarSiteInfo {
                        name: s.name.to_string(),
                        lat: s.lat,
                        lon: s.lon,
                        is_current: s.name == target_site,
                        is_loading: target_loading.as_deref() == Some(s.name),
                    })
                    .collect();
                // Described, not closed over: the first overlay kind whose
                // dispatch is a `JobRequest`, which is what lets it run in the
                // web worker instead of inline on the browser's one thread
                // (`offload`'s wasm arm, where the handler kinds above still
                // run). On native it rides the pool's interactive lane — the
                // same lane the closure rode — so nothing about where it runs
                // changed there; see `offload::pool::Interactive`.
                let request = crate::offload::JobRequest::Overlay {
                    width,
                    height,
                    bounds: render_bounds,
                    input: crate::offload::OverlayJobInput::Sites(rasterize::SitesInput {
                        sites,
                        zoom: actual_zoom,
                        is_dark,
                        device_scale,
                    }),
                };
                crate::offload::offload_job(
                    "sites-render",
                    crate::offload::Job::Described(request),
                    Self::overlay_job_deliver(
                        "sites-render",
                        width,
                        height,
                        None,
                        OverlayRenderResponse {
                            image: None,
                            geo_bounds: render_bounds,
                            overlay_kind: kind,
                            generation: data_generation,
                            pane_indices,
                            zoom,
                            hit_map: None,
                        },
                        sender,
                        window,
                    ),
                );
            }
            // Non-texture overlay kinds are never dispatched for background rendering.
            OverlayKind::Radar
            | OverlayKind::CityLabels
            | OverlayKind::UserLocation
            | OverlayKind::Metar
            | OverlayKind::ColorScale => {
                log::warn!(
                    "spawn_overlay_render called with non-texture kind: {:?}",
                    kind
                );
                self.clear_overlay_render_marks(&pane_indices, kind);
            }
        }
    }

    /// Undo the in-flight marks [`spawn_overlay_render`](Self::spawn_overlay_render)
    /// set on its target panes.
    ///
    /// Nothing else clears them. The mark is cleared on arrival of the render
    /// response, so a dispatch that returns without offloading anything leaves
    /// every target pane believing a rasterization it will never hear about is
    /// still running — and a pane in that state never asks for that overlay
    /// again, so the layer stays blank until something else resets the cache.
    /// The marks are set for *all* `pane_indices`, so they must be cleared for
    /// all of them: the early exits below are reached by asking about one pane,
    /// which used to leave its siblings' marks behind.
    fn clear_overlay_render_marks(&mut self, pane_indices: &[usize], kind: OverlayKind) {
        for &pidx in pane_indices {
            if let Some(pane) = self.gui.pane_mut(pidx) {
                pane.overlay_cache_mut(kind).render_in_flight = false;
            }
        }
    }

    /// Enable radar loop for a pane: initializes loop state and spawns
    /// an async task to list available scans in the lookback window.
    ///
    /// Everything except the spawn lives in [`begin_loop_for_pane`], so which pane
    /// the loop is read from is one decision, made and tested in one place.
    fn handle_enable_loop(&mut self, pane_idx: usize, lookback_secs: u64) {
        let Some(request) = begin_loop_for_pane(
            self.gui.panes_mut(),
            &mut self.loop_mgr,
            pane_idx,
            lookback_secs,
        ) else {
            return;
        };
        let LoopScanRequest { site, start, end } = request;

        self.spawn_async_task(self.channels.loop_scan_list_sender.clone(), async move {
            match scan::list_scans_for_range(&site, start, end).await {
                Ok(scans) => {
                    log::info!(
                        "Loop: found {} {} scans in range for pane {}",
                        scans.len(),
                        site,
                        pane_idx
                    );
                    crate::channels::LoopScanListResponse {
                        pane_idx,
                        site,
                        scans,
                    }
                }
                Err(e) => {
                    log::error!("Loop scan listing failed for {}: {:?}", site, e);
                    // An empty list is how a failed listing reaches the pane:
                    // `accept_scan_listing` switches the loop back off, so the pane
                    // returns to its static image rather than sitting in `Rendering`
                    // with nothing to render and nothing outstanding to change that.
                    crate::channels::LoopScanListResponse {
                        pane_idx,
                        site,
                        scans: Vec::new(),
                    }
                }
            }
        });
    }

    /// Disable radar loop for a pane: resets to single-frame mode.
    fn handle_disable_loop(&mut self, pane_idx: usize) {
        if let Some(pane) = self.gui.pane_mut(pane_idx) {
            pane.loop_state = rustdar_egui::pane::LoopPlaybackState::new();
        }
        self.loop_mgr.remove_pending(pane_idx);
        // Clear last_rendered so dispatch_pane_renders will re-apply the
        // cached static render (or spawn a fresh one) on the next frame.
        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered = None;
        }
        // Global scan cache and download tracking are left intact for other panes.
        // Stale entries are cleaned up lazily when no pane references them.
    }

    /// Navigate by a relative time step (seconds). Positive = forward, negative = backward.
    fn handle_navigate_time(&mut self, pane_idx: usize, step_secs: i64) {
        let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
            return;
        };
        let site = scan_info.site.name.to_string();
        let current_utc = scan_info.timestamp;

        let target = current_utc + chrono::Duration::seconds(step_secs);
        let now_utc = chrono::Utc::now().naive_utc();

        // Cap forward navigation to now; if capped, we're "live" again
        let (target, is_live) = if step_secs > 0 && target >= now_utc {
            (now_utc, true)
        } else {
            (target, false)
        };

        self.gui.apply(GuiEvent::ViewingLiveForPane {
            pane_idx,
            live: is_live,
        });
        self.manual_nav_pending = true;

        // Update the UI config timestamp (local time for display)
        let local_ts = chrono::TimeZone::from_utc_datetime(&chrono::Local, &target).naive_local();
        let mut config = self.gui.get_radar_config().clone();
        config.timestamp = local_ts;
        self.gui.apply(GuiEvent::RadarConfig(config));
        self.gui.apply(GuiEvent::Fetching(true));

        self.spawn_fetch(site, target);
    }

    /// Navigate to the next or previous adjacent scan on AWS.
    fn handle_navigate_one_scan(&mut self, pane_idx: usize, forward: bool) {
        let Some(scan_info) = self.gui.get_scan_info_for_pane(pane_idx) else {
            return;
        };
        let site = scan_info.site.name.to_string();
        let current_utc = scan_info.timestamp;

        self.manual_nav_pending = true;
        self.gui.apply(GuiEvent::Fetching(true));

        let generation = self.render.next_fetch_generation(&site);

        let window = self.window.clone();
        let sender = self.channels.scan_sender.clone();
        self.spawn_detached(async move {
            match scan::fetch_adjacent_scan(&site, current_utc, forward).await {
                Ok((archive, timestamp)) => {
                    Self::decode_offloaded(window, sender, archive, move |volume| {
                        let result = match volume {
                            Some(volume) => Ok(crate::channels::ScanData {
                                scan: volume.scan,
                                declared_nyquist: volume.declared_nyquist,
                                site: site.clone(),
                                timestamp,
                            }),
                            None => {
                                let err =
                                    format!("Could not decode the adjacent volume for {site}");
                                log::error!("{err}");
                                Err(err)
                            }
                        };
                        Some(crate::channels::ScanResponse {
                            generation,
                            site,
                            result,
                            is_auto_poll: false,
                        })
                    });
                }
                Err(e) => {
                    let err = format!("Failed to find adjacent scan: {:?}", e);
                    log::error!("{}", err);
                    let _ = sender.send(crate::channels::ScanResponse {
                        generation,
                        site,
                        result: Err(err),
                        is_auto_poll: false,
                    });
                    crate::app::notify_redraw(&window);
                }
            }
        });
    }

    /// Jump back to live mode: apply any cached auto-poll scan, or fetch latest.
    fn handle_jump_to_live(&mut self, pane_idx: usize) {
        // The pane's site decides everything below — which cached scan is applied,
        // which site the fallback fetch names. A pane that is not there has no
        // site, and `unwrap_or_default` turned that into `spawn_fetch("")`: a
        // request for a radar with no code, whose failure the user sees as an
        // error banner. Nothing to do is the answer.
        let Some(pane_site) = self.gui.pane(pane_idx).map(|p| p.site.clone()) else {
            return;
        };

        self.gui.apply(GuiEvent::ViewingLiveForPane {
            pane_idx,
            live: true,
        });
        self.manual_nav_pending = true;

        if let Some((scan_arc, declared, scan_info, timestamp)) =
            self.latest_cached_scans.remove(&pane_site)
        {
            log::info!(
                "JumpToLive: using cached scan for {} @ {}",
                pane_site,
                timestamp
            );
            self.scan_data
                .insert(pane_site.clone(), (scan_arc, declared));

            let local_ts =
                chrono::TimeZone::from_utc_datetime(&chrono::Local, &timestamp).naive_local();
            let mut config = self.gui.get_radar_config().clone();
            config.timestamp = local_ts;
            self.gui.apply(GuiEvent::RadarConfig(config));
            self.gui.apply(GuiEvent::ScanInfoForSite {
                site: pane_site.clone(),
                info: scan_info,
            });
            self.gui.clear_loading_site_for_site(&pane_site);
            self.render.reset_panes_for_site(&pane_site, &self.gui);
            self.spawn_level3_fetches(&pane_site);

            self.manual_nav_pending = false;
            self.reinit_active_loops();
            return;
        }

        // On a site the chunk feed is serving, live is a *reattachment*, not
        // a fetch: nothing was cached above precisely because the feed has
        // been applying its volumes to this site's panes all along, so
        // flipping the flag is the whole job and the feed's own cadence takes
        // over from here. The archive fallback below would return the volume
        // *before* the one being assembled — a walk backwards for the one
        // click that means "newest" — and its result then races the scan
        // drain's feed guard, which is how Live read as inert in the M10
        // transport diagnosis. Gated on the site actually having data on
        // screen so a feed that has not delivered yet still falls through.
        if self.chunks_are_feeding(&pane_site)
            && latest_scan_time_for_site(self.gui.panes(), &pane_site).is_some()
        {
            self.gui.clear_loading_site_for_site(&pane_site);
            self.manual_nav_pending = false;
            return;
        }

        // No cached scan for this site — fetch latest
        let now = chrono::Local::now().naive_local();
        let mut config = self.gui.get_radar_config().clone();
        config.timestamp = now;
        self.gui.apply(GuiEvent::RadarConfig(config));
        self.gui.apply(GuiEvent::Fetching(true));

        let utc_timestamp = Self::local_to_utc(now);
        self.spawn_fetch(pane_site, utc_timestamp);
    }

    /// Spawn a download task for a single loop frame scan.
    ///
    /// `site` is the site the requesting pane's loop is on, and is echoed on the
    /// response: it is half the key the scan is cached and looked up under, so it
    /// has to travel with the scan rather than being re-read from the pane, whose
    /// loop may be rebuilt for another site before this lands.
    pub(super) fn spawn_loop_scan_download(
        &self,
        pane_idx: usize,
        site: String,
        timestamp: NaiveDateTime,
        identifier: rustdar_radar::archive::Identifier,
    ) {
        let window = self.window.clone();
        let sender = self.channels.loop_scan_download_sender.clone();
        self.spawn_detached(async move {
            let archive = match scan::fetch_scan_object(identifier).await {
                Ok(archive) => archive,
                Err(e) => {
                    log::error!(
                        "Loop scan download failed for pane {} ({} @ {}): {:?}",
                        pane_idx,
                        site,
                        timestamp,
                        e
                    );
                    let _ = sender.send(crate::channels::LoopScanDownloadResponse {
                        pane_idx,
                        site,
                        timestamp,
                        scan: None,
                    });
                    crate::app::notify_redraw(&window);
                    return;
                }
            };
            // **Every loop frame comes through here**, and on wasm that is up
            // to `MAX_LOOP_FRAMES` of them — 14, not the 60 desktop holds. Each
            // is its own job: the funnel posts them as they are downloaded and
            // the worker takes them in order, so a loop fills progressively
            // rather than freezing the tab once per frame.
            Self::decode_offloaded(window, sender, archive, move |volume| {
                // Both halves, because a loop frame is dealiased on the same
                // terms as the still frame beside it: NROT and SRV unfold
                // around the limit the cut declared, and a frame that arrived
                // without the declaration would unfold around an estimate
                // instead — a loop stepping through pictures of one storm
                // computed two different ways.
                let scan = volume.map(|volume| {
                    (
                        std::sync::Arc::new(volume.scan),
                        std::sync::Arc::new(volume.declared_nyquist),
                    )
                });
                Some(crate::channels::LoopScanDownloadResponse {
                    pane_idx,
                    site,
                    timestamp,
                    scan,
                })
            });
        });
    }

    /// Spawn the key listing a pane's Level III loop pairings will be ranked
    /// against: one request per UTC day the loop's window touches, for one AWIPS
    /// code.
    ///
    /// The listing is separated from the pairing on purpose. A loop pairs tens of
    /// volumes against the same code, and each pairing has to know which keys
    /// exist; listing per frame would spend two round-trips a frame answering the
    /// same question. Listed once here and cached in `loop_mgr`, every frame then
    /// ranks the same key set locally.
    ///
    /// `site` and `code` are echoed on the response: together they are the cache
    /// key, and a listing outlives the loop that asked for it.
    pub(super) fn spawn_loop_l3_listing(
        &self,
        pane_idx: usize,
        site: String,
        code: String,
        days: Vec<chrono::NaiveDate>,
    ) {
        self.spawn_async_task(self.channels.loop_l3_list_sender.clone(), async move {
            let sources = rustdar_radar::sources::DataSources::production();
            let keys = rustdar_radar::level3::list_days(&sources, &site, &code, &days).await;
            log::info!(
                "Loop: listed {} Level III {code} keys for {site} across {} day(s)",
                keys.len(),
                days.len(),
            );
            crate::channels::LoopL3ListResponse {
                pane_idx,
                site,
                code,
                keys,
            }
        });
    }

    /// Spawn the pairing for one loop frame's Level III object: rank `keys` around
    /// the frame's volume start and open candidates until one names that volume.
    ///
    /// `timestamp` is the **volume start** the frame draws, which is exactly what
    /// a Level III PDB reports for the volume it was generated from — so the
    /// pairing is an equality, not a nearest-in-time guess. The newest key is
    /// never taken: SAILS republishes cuts mid-volume and the QPE family emits
    /// partial intermediates, so recency and volume identity routinely disagree.
    /// See [`rustdar_radar::level3::product_from_candidates`].
    ///
    /// `None` on the response is an ordinary gap — a volume the site generated no
    /// object for — and is cached as the answer so the frame is retired once
    /// rather than re-paired every pass.
    pub(super) fn spawn_loop_l3_pairing(
        &self,
        pane_idx: usize,
        site: String,
        code: String,
        timestamp: NaiveDateTime,
        keys: std::sync::Arc<Vec<String>>,
        pick: rustdar_radar::level3::VolumePick,
    ) {
        self.spawn_async_task(self.channels.loop_l3_fetch_sender.clone(), async move {
            let sources = rustdar_radar::sources::DataSources::production();
            let candidates =
                rustdar_radar::level3::candidates_near(keys.iter().cloned(), timestamp);
            let product = rustdar_radar::level3::product_from_candidates(
                &sources, candidates, timestamp, pick,
            )
            .await;
            match &product {
                Some(p) => log::debug!(
                    "Loop: {site} {code} for volume {timestamp} is {}",
                    p.stamp.key
                ),
                None => log::info!(
                    "Loop: {site} generated no {code} for volume {timestamp}; frame is a gap"
                ),
            }
            crate::channels::LoopL3FetchResponse {
                pane_idx,
                site,
                code,
                timestamp,
                product: product.map(std::sync::Arc::new),
            }
        });
    }

    /// Spawn a background render thread for a single loop frame.
    ///
    /// Returns `true` if a render thread was spawned. `false` means the shared
    /// concurrency budget was exhausted and nothing was started — the caller must
    /// not mark the frame as in flight, since no response will arrive to clear it.
    ///
    /// `target` is the pane's current render target (`LoopPlaybackState::rendered_for`):
    /// the loop's site plus the *selected* product and elevation, as opposed to
    /// `params.elevation`, which is snapped to a sweep in this frame's own scan, and
    /// `params.lat`/`params.lon`, which are that same site's coordinates. It is stamped
    /// on the response so a result can be rejected if the pane retargets — or the loop
    /// is rebuilt for another site — while the render runs.
    ///
    /// `data` is the frame's bytes from whichever datasource its product comes
    /// from. That is the *only* thing the two differ in from here on: one guard,
    /// one budget slot, one send site, one response type, so a Level III frame
    /// cannot acquire a different lifecycle from a Level II one by accident.
    pub(super) fn spawn_loop_frame_render(
        &self,
        pane_idx: usize,
        timestamp: NaiveDateTime,
        data: crate::loop_downloads::LoopFrameData,
        params: crate::render_dispatch::RenderParams,
        target: rustdar_egui::pane::RenderTarget,
    ) -> bool {
        // Check concurrent render limit (the counter is shared with static pane renders)
        let current = self.render.renders_in_flight.load(Ordering::Relaxed);
        if current >= self.render.concurrent_renders() {
            return false;
        }
        self.render
            .renders_in_flight
            .fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(std::sync::Arc::clone(&self.render.renders_in_flight));

        // Both the render call and the response's `snapped` read `params`, and
        // nothing here re-derives either from `target`. `params.elevation` is the
        // sweep this frame's own scan carries; `target.elevation` is the selection
        // that was asked for. `LoopRenderRequest::render_params` makes that choice
        // once, under test — this only forwards it, so the two cannot disagree
        // about what the image depicts.
        let crate::render_dispatch::RenderParams {
            product,
            elevation: snapped,
            lat,
            lon,
        } = params;
        let sender = self.channels.loop_render_sender.clone();
        let window = self.window.clone();

        let job = match data {
            // The scan is reduced to the one sweep this frame draws before the
            // job is dispatched, so a browser worker can be handed the request
            // without the volume behind it. `None` — no sweep carries the
            // product — is the same answer the renderer gives, and takes the
            // same failure path.
            crate::loop_downloads::LoopFrameData::Volume(scan_data, declared) => {
                // The storm motion override is read from the dispatcher for the
                // same reason `spawn_level2_render` reads it there: one field
                // for both the invalidation and the vector drawn.
                let storm_motion = (product
                    == rustdar_radar::types::RadarProduct::StormRelativeVelocity)
                    .then(|| self.render.storm_motion_override_kt())
                    .flatten();
                // The environmental heights ride the same way for the hail pair
                // and the classification, keyed by the loop's own site and read
                // from the same cache the static pane render uses — so a loop
                // frame and the still frame agree about the melting layer.
                let env_heights = self.render.env_heights_km_msl_for(product, &target.site);
                // The melting layer does **not** ride the same way, and the
                // difference is `timestamp`: it is this frame's own volume
                // start, not the site's current one, so the accessor answers
                // `None` for every frame whose volume is not the one the
                // cached object names. That is the whole point of asking it
                // per frame. A loop steps back through volumes the app never
                // fetched an `N0M` for, and handing those frames the still
                // frame's object would animate one volume's measured melting
                // layer across twenty other volumes' classifications — all of
                // them reporting themselves as measured.
                //
                // # A known limitation, scoped and captioned rather than hidden
                //
                // Only one volume per site has an `N0M` object fetched for it —
                // the one the pane is on — so **every frame of a classification
                // loop but that one classifies on the fallback**. Those frames
                // are not wrong about themselves: they carry
                // `MeltingLayerSource::Sounding` or `FleetDefault` and the pane
                // says so as they play, which is the difference between a
                // limitation and a defect.
                //
                // That one frame is the one on the pane's own volume — the
                // newest while the pane follows live, and whichever frame was
                // scrubbed to otherwise, since `spawn_level3_fetches` pairs
                // against `latest_scan_time_for_site` either way. It reaches
                // the object only because `timestamp` and the cached
                // `volume_start` are paired
                // by `rustdar_radar::scan::names_same_volume` rather than by
                // `==`. They are two statements of one volume start written by
                // different code — the cache holds the first radial's time with
                // its milliseconds, a loop frame holds the archive key's time
                // truncated to the second — and measured over 108 archive
                // volumes they differ by 1–993 ms and are *never* equal. Under
                // the exact comparison this used to make, the sentence above
                // read "every other frame" and was wrong in the way that
                // mattered: the newest frame fell to the fallback too, so the
                // same volume classified one way still and another way looped,
                // with nothing on screen to say which had happened.
                //
                // Closing it is a per-frame pairing, and the machinery is
                // already here: [`Self::spawn_loop_l3_listing`] lists a code's
                // keys across the loop's span once, and
                // [`Self::spawn_loop_l3_pairing`] pairs one per frame by PDB.
                // Pointing that pair at `N0M` and widening the cache from one
                // object per site to one per (site, volume) is the whole of it.
                // Left undone deliberately: it is a fetch-budget question — a
                // twenty-frame loop would add twenty ~6 kB objects and one
                // listing — and the still frame is where a classification is
                // actually read.
                let melting_layer =
                    self.render
                        .melting_layer_product_for(product, &target.site, timestamp);
                // The RPG's storm motion is asked per frame for exactly the
                // reason the melting layer is, and the limitation is the same
                // one, scoped the same way: only the volume the pane is on has
                // an `N0S` fetched for it, so **every frame of an SRV loop but
                // the one on the pane's own volume shifts on a derived rung**.
                // Those frames are not wrong about themselves — they carry
                // `StormMotionSource::BunkersRightMover` or `MeanWind` and the
                // pane says so as they play — and closing it is the same
                // per-frame pairing (`spawn_loop_l3_listing` +
                // `spawn_loop_l3_pairing`) pointed at `N0S`.
                //
                // That frame reaches the vector on the same pairing the melting
                // layer above uses, and for the same reason it used to miss it
                // entirely. This is the sharper of the two failures: a melting
                // layer off by one rung shifts a classification boundary, while
                // a storm motion off by a volume is a solid-body shift of every
                // gate in the field, still captioned as the RPG's own.
                //
                // Handing every frame the still frame's vector would be worse
                // than the fallback rather than better: a solid-body shift of
                // one volume's storm applied to twenty other volumes' fields,
                // all of them reporting themselves as the RPG's own.
                let rpg_storm_motion =
                    self.render
                        .rpg_storm_motion_for(product, &target.site, timestamp);
                match rustdar_radar::render_input::RenderInput::extract(
                    &scan_data,
                    snapped,
                    product,
                    lat,
                    lon,
                    storm_motion,
                    env_heights,
                ) {
                    Some(input) => {
                        crate::offload::Job::Described(crate::offload::JobRequest::Radar {
                            // The same stamp the still frame takes, off this
                            // frame's own volume. Without it a loop of NROT or
                            // SRV would fold around whatever each frame's
                            // calmest sector estimated while the static render
                            // of the newest frame folded around the RDA's
                            // declaration — one storm, two pictures, no error.
                            input: Box::new(
                                input
                                    .with_declared_nyquist(&declared)
                                    .with_srv_fallback(self.render.srv_fallback())
                                    .with_melting_layer_product(melting_layer)
                                    .with_rpg_storm_motion(rpg_storm_motion),
                            ),
                            // Loop frames store an empty value grid, so asking
                            // for one would produce `LOOP_IMAGE_SIZE² × 4` bytes
                            // per frame to be dropped on arrival — and copied
                            // across a worker boundary first.
                            values_wanted: false,
                            // The same policy in the other dimension: a loop
                            // renders at `LOOP_IMAGE_SIZE` however far its
                            // sweep reaches. A desktop loop textures up to 36
                            // frames at once, and at 4096² that is
                            // 36 × 64 MiB = 2.3 GiB a pane against a 576 MiB
                            // loop budget.
                            // See `JobRequest::side_ceiling_px`.
                            side_ceiling_px: crate::constants::LOOP_IMAGE_SIZE as u32,
                        })
                    }
                    None => crate::offload::Job::renders_nothing(),
                }
            }
            // The object's *bytes*, exactly as the static Level III pane render
            // dispatches them (`try_spawn_level3_render`): a `Level3Message` has
            // no wire form, and re-decoding on the worker is cheap against the
            // rasterization it precedes.
            //
            // `first` today, because every Level III product rustdar draws is one
            // AWIPS code. A product derived from several — VIL density's
            // `DVL ÷ EET` — arrives here with all of them, paired to this frame's
            // volume and ordered by `level3_products`; what it needs is a job
            // kind that reads more than one, not a different loop path.
            crate::loop_downloads::LoopFrameData::Products(products) => match products.first() {
                Some(first) => crate::offload::Job::Described(crate::offload::JobRequest::Level3 {
                    bytes: std::sync::Arc::clone(&first.bytes),
                    product,
                    radar_lat: lat,
                    radar_lon: lon,
                    // A loop frame, so the loop size — see the Level II arm.
                    side_ceiling_px: crate::constants::LOOP_IMAGE_SIZE as u32,
                }),
                None => crate::offload::Job::renders_nothing(),
            },
        };
        crate::offload::offload_job("loop-render", job, move |output| {
            let _guard = guard;
            // An output of another kind is `None` here, which is the same
            // "nothing to draw" a failed render has always been — this consumer
            // is shaped for a plan-view frame and must never be handed a
            // section's differently-shaped buffers. See `JobOutput::frame`.
            let frame = output.and_then(crate::offload::JobOutput::frame);
            // A failed render still has to be sent, so render_in_flight gets cleared.
            let (image, max_range_km, nyquist_ms, describes, polar) = match frame {
                Some(mut frame) => {
                    // Converted here, in `deliver`, so `rgba` drops at the end
                    // of this scope and only one of the two buffers is ever in
                    // the channel. On a thread that is off the frame entirely;
                    // in a browser with a worker it is the one part of the
                    // render that lands on the main thread, and what it does
                    // there is copy 4 MiB — the premultiply behind it belongs
                    // to `offload::execute` and ran in the worker.
                    //
                    // The two provenances travel as one pair rather than as
                    // two more tuple slots: they are the same kind of fact —
                    // what this picture stood on, unrecoverable from its
                    // pixels — and the failure arm has to clear them together
                    // or not at all. See `FrameProvenance`.
                    let converted = match loop_frame_image(&frame.image) {
                        Some(image) => (
                            Some(image),
                            frame.max_range_km,
                            frame.nyquist_ms,
                            FrameProvenance {
                                melting_layer_source: frame.melting_layer_source,
                                storm_motion: frame.storm_motion,
                            },
                        ),
                        None => {
                            log::error!(
                                "Loop render for pane {pane_idx} produced {} bytes, expected {}",
                                frame.image.len(),
                                LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE * 4
                            );
                            (None, 0.0, None, FrameProvenance::default())
                        }
                    };
                    // `rgba` drops at the end of this scope either way, as the
                    // comment above says; this is that drop, spelt as a hand
                    // back to the renderer's slots. Both outcomes give both
                    // buffers up, because a frame whose length the loop refused
                    // is exactly as dead as one it accepted.
                    //
                    // The numbers are not already dropped by `offload::execute`.
                    // That one runs off `JobRequest::Radar`'s `values_wanted:
                    // false`, which only the Level II half of the dispatch above
                    // has: `JobRequest::Level3` carries no such field, so half
                    // the frames passing this site — every Level III loop —
                    // arrive still holding their gates, and this is the only
                    // place those die. One call covers both halves, because
                    // stripping a field the Level II half already stripped is a
                    // no-op; so the line is free where it is redundant and is
                    // the whole of the win where it is not.
                    //
                    // The geometry stays either way. It is 5.8 KiB, and it is
                    // what lets a hover over this frame find its gate in the
                    // volume the frame was rendered from — see
                    // `rustdar_radar::hover::SweepGates`.
                    frame.polar.strip_values();
                    rustdar_radar::render::recycle_image(frame.image);
                    let (image, max_range_km, nyquist_ms, describes) = converted;
                    (image, max_range_km, nyquist_ms, describes, frame.polar)
                }
                None => (
                    None,
                    0.0,
                    None,
                    FrameProvenance::default(),
                    Default::default(),
                ),
            };
            // One send site for both outcomes, so `snapped` cannot come to differ
            // between them. It describes the render that was dispatched — the sweep
            // `render_params` resolved — and stays true of a response carrying no
            // image, which is what makes it safe to set outside the match. The same
            // is true of the coordinates: they are the `lat`/`lon` this render was
            // just given, so the receiver places the image where it was actually
            // drawn rather than re-deriving that from its own loop.
            let _ = sender.send(crate::channels::LoopRenderResponse {
                pane_idx,
                timestamp,
                target,
                snapped,
                site_lat: lat,
                site_lon: lon,
                image,
                max_range_km,
                nyquist_ms,
                melting_layer_source: describes.melting_layer_source,
                storm_motion: describes.storm_motion,
                polar,
            });
            super::notify_redraw(&window);
        });
        true
    }

    /// Append a freshly-polled scan to any active loops, evicting frames past
    /// the lookback window.
    ///
    /// `site` is the site the *scan* came from, not any pane's — it decides both
    /// which cache entry the scan becomes and which loops may take a frame for it.
    pub(super) fn append_scan_to_active_loops(
        &mut self,
        site: &str,
        timestamp: chrono::NaiveDateTime,
        scan: std::sync::Arc<nexrad_model::data::Scan>,
        declared: std::sync::Arc<rustdar_radar::nyquist::DeclaredNyquist>,
    ) {
        // Store in the shared cache under this scan's own site, for every loop on
        // that site to use. The declarations go with it: this is the newest
        // frame of a running loop, and the still frame beside it is drawn from
        // the same volume.
        self.loop_mgr.cache_scan(site, timestamp, (scan, declared));

        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        append_polled_frame_to_loops(self.gui.panes_mut(), site, timestamp, allocation, &budgets);
    }

    /// Re-initialize radar loops on all panes that have an active loop.
    /// Called after a manual time navigation to rebase loops around the new scan time.
    pub(super) fn reinit_active_loops(&mut self) {
        let mut to_reinit = Vec::new();
        for pane_idx in 0..self.gui.pane_count() {
            if let Some(pane) = self.gui.pane_mut(pane_idx)
                && pane.loop_state.is_active()
            {
                to_reinit.push((pane_idx, pane.loop_state.lookback_secs));
            }
        }
        for (pane_idx, lookback_secs) in to_reinit {
            self.handle_enable_loop(pane_idx, lookback_secs);
        }
    }
}

/// The UTC instant `timestamp` names as a wall-clock time in `tz`.
///
/// Two of the three answers a local time can have are decided here.
///
/// An **ambiguous** time — the hour a fall-back transition names twice — resolves
/// to the later of the two instants. Either is defensible; the later one is the
/// one nearer to now, which is the one a user stepping backwards through scans
/// has just come from.
///
/// A **nonexistent** time is the hour a spring-forward skips: 02:30 does not
/// happen on a US transition day, and the time picker and every relative
/// navigation step can land on it. This used to fall back to `Local::now()`,
/// which answers a question nobody asked — a request for a scan from the small
/// hours fetched the current one instead, and the pane silently jumped to live.
/// Shifting past the gap and re-resolving lands on the instant the clock jumped
/// to, which is both the nearest time that exists and, since the shift and the
/// offset change cancel, exactly the instant the requested wall clock would have
/// named had the transition not happened.
///
/// Generic over the zone because `Local` is whatever the machine running the
/// tests is set to, and a zone with no DST — or one mid-transition — would make
/// any assertion about the gap either vacuous or flaky.
fn local_to_utc_in<Tz: TimeZone>(tz: &Tz, timestamp: NaiveDateTime) -> NaiveDateTime {
    if let Some(resolved) = tz.from_local_datetime(&timestamp).latest() {
        return resolved.with_timezone(&chrono::Utc).naive_utc();
    }
    let past_the_gap = timestamp + chrono::Duration::hours(1);
    if let Some(resolved) = tz.from_local_datetime(&past_the_gap).latest() {
        return resolved.with_timezone(&chrono::Utc).naive_utc();
    }
    // No zone in the tz database skips more than an hour, so this is unreachable
    // in practice. Applying the offset in force around then keeps the answer
    // within an hour of what was asked for, where `now()` was unbounded.
    use chrono::Offset;
    let offset = tz.offset_from_utc_datetime(&timestamp).fix();
    timestamp - chrono::Duration::seconds(offset.local_minus_utc() as i64)
}

/// The newest scan of `site` any pane is currently showing, or `None` if none is.
///
/// This is the "current" an auto-poll compares that site's latest object against:
/// `scan::check_and_fetch_latest` downloads only when the site's newest scan is
/// strictly newer. The GUI emits one `CheckForNewScans` per unique **live site**,
/// so the active pane's scan time answers the question for at most one of them.
/// Handed to the others it fails both ways round: an active pane newer than a
/// second site's scan suppresses that site's updates for as long as both stay
/// live, and an active pane parked on historic data re-downloads the other site's
/// whole Level II volume — plus five Level III refetches and a re-render — every
/// poll interval, for a scan that has not changed.
///
/// Resolved through each pane's `scan_info`, whose `site` is the site the scan in
/// hand really came from, rather than through the pane's live `site` field. The
/// scan is what carries the timestamp, and a timestamp reported under a name that
/// did not produce it is exactly the mismatch this exists to prevent. A pane
/// between a site switch and that site's first volume holds no `scan_info` at all
/// and contributes nothing, so the new site reports `None` and fetches
/// unconditionally — which is what a site with nothing loaded wants.
pub(super) fn latest_scan_time_for_site(
    panes: &[rustdar_egui::pane::PaneState],
    site: &str,
) -> Option<NaiveDateTime> {
    panes
        .iter()
        .filter_map(|p| p.scan_info.as_ref())
        .filter(|info| info.site.name == site)
        .map(|info| info.timestamp)
        .max()
}

impl super::App {
    /// Cut one cross-section loop frame, off the frame thread, and reply on
    /// `loop_section_sender`.
    ///
    /// The section counterpart of [`Self::spawn_loop_frame_render`], and it
    /// keeps every one of that function's contracts:
    ///
    /// * **It takes a slot from the one shared render budget** and returns
    ///   [`SectionDispatch::Busy`] without spending anything if none is free. A
    ///   caller that marked the frame in flight anyway would never see a
    ///   response — nothing would clear the flag — and the frame would stay
    ///   blank for the life of the loop. The three-way answer is
    ///   [`SectionDispatch`]'s own, and for its reason: "the budget is full, ask
    ///   again" and "this volume has nothing to cut, retire the frame" are
    ///   different instructions to the caller and used to be one `false`.
    ///
    /// [`SectionDispatch`]: crate::render_dispatch::SectionDispatch
    /// [`SectionDispatch::Busy`]: crate::render_dispatch::SectionDispatch::Busy
    /// * **One send site for both outcomes**, so [`ladder`] and the key describe
    ///   the cut that was *dispatched* and stay true of a reply carrying no
    ///   raster.
    /// * **The raster is converted to egui's layout in `deliver`**, so the RGBA
    ///   buffer and its `Color32` copy — 8 MiB apiece natively — never coexist
    ///   in the channel.
    ///
    /// # What runs where
    ///
    /// `extract_volume_parts` runs here, on the frame thread, and the
    /// rasterization runs on the worker. That is the same split the live section
    /// pane has always used (`RenderDispatcher::spawn_section_render` calls its
    /// `extract` closure inline for the same reason: on wasm the volume is only
    /// reachable from the main thread, and the job wire carries a `RenderInput`,
    /// not a `Scan`). What is new is the *count*, and that is why the caller
    /// dispatches at most [`crate::constants::MAX_LOOP_SECTION_CUTS_PER_FRAME`]
    /// of these per frame: measured on a real VCP-212 volume the extraction is
    /// ~1.0 ms and the rasterization ~6.1 ms, so the frame thread pays about
    /// what one live re-cut already costs it and the expensive half is off it
    /// entirely. Without that cap a desktop dispatch pass would run six
    /// extractions back to back on the frame that starts the loop.
    ///
    /// [`ladder`]: crate::channels::LoopSectionResponse::ladder
    pub(super) fn spawn_loop_section_render(
        &self,
        req: crate::app::render::LoopSectionRequest,
        scan: std::sync::Arc<nexrad_model::data::Scan>,
        declared: std::sync::Arc<rustdar_radar::nyquist::DeclaredNyquist>,
    ) -> crate::render_dispatch::SectionDispatch {
        use crate::render_dispatch::SectionDispatch;
        // Destructured rather than taken as eight parameters, and the plan is
        // what supplies them: the ladder the staleness test resolved, the key
        // the frames are cut for and the coordinates the loop's geometry was
        // captured at all belong to one decision made in `dispatch_loop_renders`.
        // Loose arguments of the same types — two `f64`s and a `u64` — are
        // exactly the shape a caller gets wrong silently.
        let crate::app::render::LoopSectionRequest {
            pane_idx,
            frame_idx: _,
            timestamp,
            target,
            key,
            ladder,
            site_lat: lat,
            site_lon: lon,
        } = req;
        let current = self.render.renders_in_flight.load(Ordering::Relaxed);
        if current >= self.render.concurrent_renders() {
            return SectionDispatch::Busy;
        }

        let product = target.product;
        // Read off the dispatcher, never from the caller, so the vector a frame
        // is *keyed* on cannot differ from the one it is derived with — the
        // rule `SectionInputKey::of` states and the reason `SectionLoopKey`
        // carries it at all.
        let motion = (product == rustdar_radar::types::RadarProduct::StormRelativeVelocity)
            .then(|| self.render.storm_motion_override_kt())
            .flatten();
        // Read off the dispatcher for the same reason and stamped on the
        // payload below, so the rung a frame is keyed on is the rung it is
        // derived with.
        let fallback = self.render.srv_fallback();
        debug_assert_eq!(
            key,
            rustdar_egui::pane::SectionLoopKey::new(key.line, motion, fallback),
            "the frame's key must name the vector the extraction is about to use",
        );

        let sweeps: Vec<&nexrad_model::data::Sweep> = scan.sweeps().iter().collect();
        let Some(input) = rustdar_radar::render_input::RenderInput::extract_volume_parts(
            scan.coverage_pattern(),
            &sweeps,
            product,
            lat,
            lon,
            motion,
        )
        // The live section's stamp, off this frame's own volume: a section of
        // NROT or SRV is dealiased on the worker, and the whole point of the
        // limits crossing the wire is that the worker folds where this thread
        // would have.
        .map(|input| {
            input
                .with_declared_nyquist(&declared)
                .with_srv_fallback(fallback)
        }) else {
            // This volume carries no field to cut under this product. Nothing
            // was spawned and no slot was taken, so the caller retires the frame
            // rather than waiting on a reply that will never come.
            return SectionDispatch::NoPayload;
        };

        self.render
            .renders_in_flight
            .fetch_add(1, Ordering::Relaxed);
        let guard = RenderGuard(std::sync::Arc::clone(&self.render.renders_in_flight));

        let request = rustdar_radar::xsect::SectionRequest {
            start: (key.line.a().lat, key.line.a().lon),
            end: (key.line.b().lat, key.line.b().lon),
            top_km_msl: None,
            product,
        };
        let job = crate::offload::Job::Described(crate::offload::JobRequest::Section {
            input: Box::new(input),
            request,
        });
        let sender = self.channels.loop_section_sender.clone();
        let window = self.window.clone();
        crate::offload::offload_job("loop-section", job, move |output| {
            let _guard = guard;
            // An output of another kind is `None`, which takes the same
            // "nothing to draw" path a refused cut does. This consumer is
            // shaped for a `SECTION_WIDTH × SECTION_HEIGHT` raster and must
            // never be handed a plan view's — see `JobOutput::section`.
            let cut = output.and_then(crate::offload::JobOutput::section);
            let (image, axes, tilt_elevations_deg, tilt_collected_ms) = match cut {
                Some(cut) => match loop_section_image(cut.image()) {
                    Some(image) => (
                        Some(image),
                        Some(*cut.axes()),
                        cut.tilt_elevations_deg().to_vec(),
                        cut.tilt_collected_ms().to_vec(),
                    ),
                    None => {
                        log::error!(
                            "Loop section for pane {pane_idx} produced {} bytes, expected {}",
                            cut.image().len(),
                            rustdar_radar::xsect::SECTION_WIDTH
                                * rustdar_radar::xsect::SECTION_HEIGHT
                                * 4
                        );
                        (None, None, Vec::new(), Vec::new())
                    }
                },
                None => (None, None, Vec::new(), Vec::new()),
            };
            let _ = sender.send(crate::channels::LoopSectionResponse {
                pane_idx,
                timestamp,
                target,
                key,
                ladder,
                image,
                axes,
                tilt_elevations_deg,
                tilt_collected_ms,
            });
            super::notify_redraw(&window);
        });
        SectionDispatch::Dispatched
    }
}

/// Convert a renderer RGBA buffer into egui's pixel layout, or `None` if it is not
/// the `LOOP_IMAGE_SIZE²` image a loop frame is supposed to be.
///
/// A constant and not a derived side, unlike the static render path: a loop
/// frame's size is a *policy* rather than a consequence of the sweep, so a
/// frame of any other size is a job that came back at the wrong ceiling, which
/// is a bug to log rather than a picture to place.
///
/// The length check is not defensive padding. `ColorImage::from_rgba_premultiplied`
/// asserts on a mismatch, and this now runs on the render worker rather than the
/// main thread: a panic there kills only that thread, so no `LoopRenderResponse`
/// would ever arrive, `render_in_flight` would never clear, and the frame would stay
/// blank and be skipped for the life of the loop. Returning `None` routes a
/// malformed buffer down the same path as "no matching sweep", which the dispatcher
/// already knows how to retire.
///
/// Premultiplied and not unmultiplied because `offload::execute` has already
/// done the per-pixel walk, in the instance the job ran in. See
/// `offload::premultiply_raster`.
fn loop_frame_image(rgba: &[u8]) -> Option<egui::ColorImage> {
    if rgba.len() != LOOP_IMAGE_SIZE * LOOP_IMAGE_SIZE * 4 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_premultiplied(
        [LOOP_IMAGE_SIZE, LOOP_IMAGE_SIZE],
        rgba,
    ))
}

/// [`loop_frame_image`] for a cross-section raster, against the section's own
/// `SECTION_WIDTH × SECTION_HEIGHT` shape.
///
/// A separate function rather than a length parameter on the one above, because
/// the whole point of both is that the shape is a *constant of the view* and not
/// something a caller supplies: a caller free to pass a length is a caller free
/// to pass the other view's, and `ColorImage::from_rgba_premultiplied` would then
/// assert on the worker with the frame left in flight for ever. The two
/// constants are named at the two call sites and nowhere else.
fn loop_section_image(rgba: &[u8]) -> Option<egui::ColorImage> {
    use rustdar_radar::xsect::{SECTION_HEIGHT, SECTION_WIDTH};
    if rgba.len() != SECTION_WIDTH * SECTION_HEIGHT * 4 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_premultiplied(
        [SECTION_WIDTH, SECTION_HEIGHT],
        rgba,
    ))
}

/// The scan listing a freshly-built loop needs, and the site it must be requested
/// for.
///
/// One struct rather than three loose values because they have to describe a single
/// pane's single site: `site` is the code the listing is requested with *and* the
/// code the loop's geometry was captured under, and `start`/`end` are that site's
/// own scan time walked back by the lookback. Any of them coming from elsewhere
/// gives a loop that lists one radar's files and draws them at another's coordinates.
pub(super) struct LoopScanRequest {
    site: String,
    start: NaiveDateTime,
    end: NaiveDateTime,
}

/// Build `pane_idx`'s loop state and return the scan listing it now needs, or
/// `None` if that pane has no scan loaded to anchor a loop on.
///
/// This is everything enabling a loop does apart from the spawn: it indexes the
/// panes itself, so "which pane" is decided and tested here rather than at an
/// untestable call site. The active pane is deliberately never consulted —
/// `reinit_active_loops` runs this for every looping pane in turn, and a loop that
/// took the active pane's site would show that radar under its own pane's label.
///
/// Anchored on the pane's `scan_info` rather than on its `site` field, so the
/// loop's product code, its coordinates and its listing all come from the one
/// `RadarSite` the volume in hand actually is. Between a site switch and the new
/// site's first volume there is no `scan_info` to anchor on, so this answers
/// `None` and the scan that lands re-runs it — a wait rather than a loop of the
/// radar the user has just left.
fn begin_loop_for_pane(
    panes: &mut [rustdar_egui::pane::PaneState],
    loop_mgr: &mut crate::loop_downloads::LoopDownloadManager,
    pane_idx: usize,
    lookback_secs: u64,
) -> Option<LoopScanRequest> {
    let scan_info = panes.get(pane_idx)?.scan_info.as_ref()?;
    // The whole site value, so the loop's render-target code and the coordinates
    // it projects with cannot come from different sites.
    let radar_site = scan_info.site.clone();
    // The loop ends at this pane's current scan, not at wall clock, so it covers
    // where the pane is actually looking.
    let end = scan_info.timestamp;

    // Drop the previous listing's undispatched downloads; they were queued for the
    // loop this call is replacing.
    //
    // The scan cache is global and deliberately kept across this rebuild — but
    // only for as long as the new listing takes, and no longer. It is swept
    // every frame by `App::evict_unneeded_loop_scans` against the frames the
    // live loops name, and this call has just emptied *this* loop's frames. What
    // holds the window while the listing is in flight is that sweep's grace
    // rule, bounded by `constants::LOOP_LISTING_GRACE`; a listing that overruns
    // the bound loses the window and re-downloads it. Nothing here may assume
    // the cache is still whole when the listing lands.
    loop_mgr.remove_pending(pane_idx);

    // The view the pane is drawing, which is what a loop's frames are pictures
    // of. A pane that cannot loop at all is refused above this by
    // `Gui::loop_sync_targets` and the timeline's own gate; if one reached here
    // anyway it would build a loop whose frames every predicate refuses, which
    // is a spinner that never finishes — so it is refused here too, where the
    // view is in hand.
    let view = panes[pane_idx].render_view();
    if !view.can_loop() {
        return None;
    }
    panes[pane_idx].loop_state =
        rustdar_egui::pane::LoopPlaybackState::new_for_loop(lookback_secs, &radar_site, view);

    Some(LoopScanRequest {
        site: radar_site.name.to_string(),
        start: end - chrono::Duration::seconds(lookback_secs as i64),
        end,
    })
}

/// Append a frame for a scan polled from `site` at `timestamp` to every active
/// loop that is on that site.
///
/// The site test is the point. A polled scan is cached under `(site, timestamp)`
/// and looked up that way at render time, so a loop on another site handed this
/// frame resolves the lookup to *its* site's scan or to nothing at all — and
/// before the cache carried a site, it resolved to this scan and drew it around
/// the other site's coordinates, which is data from one radar under another
/// radar's label. Loops on other sites get their own frames from their own polls.
///
/// The allocation and the budgets come in because the frame cap is resolved from
/// both, and this is one of the two places it has to be applied — see
/// [`append_polled_frame`].
fn append_polled_frame_to_loops(
    panes: &mut [rustdar_egui::pane::PaneState],
    site: &str,
    timestamp: chrono::NaiveDateTime,
    allocation: crate::loop_pool::LoopAllocation,
    budgets: &crate::budget::Budgets,
) {
    for (pane_idx, pane) in panes.iter_mut().enumerate() {
        let held = super::render::loop_frames_held(allocation, &pane.loop_state, budgets);
        if append_polled_frame(&mut pane.loop_state, site, timestamp, held) {
            log::info!(
                "Appended {} scan {} to loop on pane {} ({} frames)",
                site,
                timestamp,
                pane_idx,
                pane.loop_state.frames.len()
            );
        }
    }
}

/// Add a frame at `timestamp` to `ls` if the loop is active, is on `site`, and does
/// not already have that frame. Returns whether a frame was added.
///
/// Two evictions are part of the same step, and both can only run once the new
/// frame is in place.
///
/// The **lookback window** is measured from the newest frame, so it moves with
/// every append.
///
/// The **frame cap** was missing entirely, and its absence is why a loop could
/// print "over 120 frames" beside "keeps up to 60 frames on this platform" in
/// one caption. `accept_scan_listing` applied `held` once, to the listing, and
/// nothing applied it again — while the window eviction cannot stand in for it,
/// because a loop that had to sample its listing is by construction a loop whose
/// window holds more scans than `held`. See
/// [`LoopPlaybackState::cap_frames`](rustdar_egui::pane::LoopPlaybackState::cap_frames)
/// for why the surplus is re-sampled rather than taken off the old end.
///
/// The cadence is refreshed here too, and only from a frame that lands at the
/// newest end. That is the one case where the gap just observed is a gap between
/// two *consecutive* scans of the site — an append is the newest scan the poll
/// found, and the frame before it is the newest the loop held. A frame inserted
/// anywhere else is a backfill across a hole whose width says nothing about the
/// site's cadence, and folding it in would drag the figure toward a value no
/// radar ran at, which is the same reason `median_step_secs` takes a median.
fn append_polled_frame(
    ls: &mut rustdar_egui::pane::LoopPlaybackState,
    site: &str,
    timestamp: chrono::NaiveDateTime,
    held: usize,
) -> bool {
    use rustdar_egui::pane::LoopFrame;

    if !ls.is_active() {
        return false;
    }
    // `LoopPlaybackState::site` is the loop's *geometry* site, captured when the
    // loop was built — not the pane's live `site` field, which is re-synced across
    // panes without rebuilding their loops.
    if ls.site != site {
        return false;
    }
    // Skip if this timestamp already exists
    if ls.frames.iter().any(|f| f.timestamp == timestamp) {
        return false;
    }

    // Insert in sorted order
    let insert_pos = ls.frames.partition_point(|f| f.timestamp < timestamp);
    ls.frames.insert(
        insert_pos,
        LoopFrame {
            timestamp,
            image: None,
            render_in_flight: false,
            render_failed: false,
        },
    );

    // Evict frames outside the lookback window
    let lookback = chrono::Duration::seconds(ls.lookback_secs as i64);
    if let Some(newest) = ls.frames.last().map(|f| f.timestamp) {
        let cutoff = newest - lookback;
        ls.frames.retain(|f| f.timestamp >= cutoff);
        // Adjust current_frame if the playhead fell off the end
        if ls.current_frame >= ls.frames.len() {
            ls.current_frame = ls.frames.len().saturating_sub(1);
        }
    }

    // Re-measure the site's cadence, while it is still measurable. On a loop
    // holding every scan the frame list *is* the listing, so its median gap is
    // the same statistic `accept_scan_listing` recorded — now over a window that
    // has moved. A loop left on a stale figure quotes the cadence the site ran
    // at when its listing was taken, which every VCP change makes wrong: on
    // 2026-08-11 every measured site but TDFW alternated VCPs during the day,
    // and a WSR-88D going VCP 212 to VCP 35 moves from 259 s to 517 s.
    //
    // A sampled loop is deliberately left alone, and its figure is still the
    // honest one. Every gap in a sampled frame list is a sampled gap — the
    // reason `scan_step_secs` was recorded before the sampling in the first
    // place — so there is nothing here to measure, and the listing's median is
    // exactly what "sampled from ~4 min scans" claims: the cadence of the
    // listing this loop is a sample of.
    if ls.listing_sampled != Some(true) {
        let times: Vec<_> = ls.frames.iter().map(|f| f.timestamp).collect();
        if let Some(step) = super::render::median_step_secs(&times) {
            ls.scan_step_secs = Some(step);
        }
    }

    // And back inside the frame cap. Last, so the cadence above is read off the
    // full scan list on the append that first overruns the cap.
    if ls.cap_frames(held) {
        log::info!("Loop: live appends took the frame list past {held}; re-sampled for {site}");
    }

    true
}

#[path = "app_fetch/level3_site_gate_tests.rs"]
#[cfg(test)]
mod level3_site_gate_tests;

#[path = "app_fetch/local_time_tests.rs"]
#[cfg(test)]
mod local_time_tests;

#[path = "app_fetch/loop_frame_image_tests.rs"]
#[cfg(test)]
mod loop_frame_image_tests;

#[path = "app_fetch/loop_raster_ceiling_tests.rs"]
#[cfg(test)]
mod loop_raster_ceiling_tests;

#[path = "app_fetch/loop_pane_tests.rs"]
#[cfg(test)]
mod loop_pane_tests;

#[path = "app_fetch/melting_layer_dispatch_tests.rs"]
#[cfg(test)]
mod melting_layer_dispatch_tests;

/// The sites overlay dispatch is a described job that reaches the installed
/// sink, and a job the worker never answers still un-wedges the pane.
#[cfg(test)]
#[path = "app_fetch/sites_wire_tests.rs"]
mod sites_wire_tests;

/// The three polygon overlay dispatches are described jobs that reach the
/// installed sink — each carrying its own kind's input — and a job the
/// worker never answers still un-wedges the pane.
#[cfg(test)]
#[path = "app_fetch/polygon_wire_tests.rs"]
mod polygon_wire_tests;

/// The two hit-map overlay dispatches are described jobs whose delivered
/// cells zip with the dispatch-captured items — and a mismatched reply is a
/// failed render, never a wrong hit map.
#[cfg(test)]
#[path = "app_fetch/hitmap_wire_tests.rs"]
mod hitmap_wire_tests;

/// The model-grid dispatch is a described job carrying the grid by `Arc`,
/// whose wire form is the projection window — the last kind through the
/// wire, and the routing pin that closes the opaque path's door from the
/// dispatch side.
#[cfg(test)]
#[path = "app_fetch/model_wire_tests.rs"]
mod model_wire_tests;

#[path = "app_fetch/site_switch_tests.rs"]
#[cfg(test)]
mod site_switch_tests;
