use crate::channels::{
    FetchRequester, Level3Response, OverlayRenderResponse, ScanData, ScanResponse,
};
use crate::render_dispatch::RenderGuard;
use chrono::NaiveDateTime;
use chrono::TimeZone;
use rustdar_device_profile::constants::LOOP_IMAGE_SIZE;
use rustdar_egui::actions::GuiAction;
use rustdar_egui::radar_layer;
use rustdar_egui::shell_api::GuiEvent;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, SourceEvent};
use rustdar_radar::types::RadarProduct;
use rustdar_source::id::{LayerId, known};
use std::sync::atomic::Ordering;
use winit::event_loop::ActiveEventLoop;

/// Parameters for a background overlay rasterization request.
///
/// `pub(crate)` and `Clone` since WO-M13a: [`crate::render_dispatch::RenderDispatcher`]
/// keeps the last one dispatched per `(pane, layer)`, which is what lets a
/// data arrival re-dispatch **the geometry that was already agreed** rather
/// than invent one from a frame that has not been laid out yet.
#[derive(Clone)]
pub(crate) struct OverlayRenderRequest {
    /// The pane's viewport, *before* overdraw is applied.
    pub geo_bounds: rustdar_geo::GeoBounds,
    /// Pixel dimensions and the overdraw fraction they were sized for.
    pub texture: rustdar_egui::overlay_cache::OverlayTexturePlan,
    pub data_generation: u64,
    pub zoom: i32,
}
use rustdar_radar::scan;
use std::future::Future;
use std::sync::mpsc::Sender;

/// `Send` on native, no constraint on web.
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
pub(super) fn site_offers_level3(site: &str) -> bool {
    rustdar_radar::sites::get_radar_site(site).is_none_or(|radar| radar.is_wsr88d())
}

/// The AWIPS id of the RPG's Melting Layer product (Level III code 166).
const MELTING_LAYER_CODE: &str = "N0M";

/// The AWIPS id of the RPG's Storm Relative Velocity product (Level III 56).
const STORM_MOTION_CODE: &str = "N0S";

/// What a loop frame's pixels stood on, carried out of `spawn_loop_frame_render`'s
/// delivery closure as one value.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct FrameProvenance {
    melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

impl super::App {
    /// Spawn a detached future on whatever executor this target provides.
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

    /// **The one construction of the fetch context**, so a task a handler
    /// builds for its data and one it builds for its frames are built against
    /// the same origins, client and viewport.
    pub(super) fn fetch_config(&self) -> rustdar_overlays::render::overlay_state::FetchConfig {
        rustdar_overlays::render::overlay_state::FetchConfig {
            client: self.http_client.clone(),
            zone_cache_dir: self.platform.zone_cache_dir().map(|p| p.to_path_buf()),
            sources: rustdar_radar::sources::DataSources::production(),
            viewport: self.last_viewport,
        }
    }

    /// **The one construction of a pane's layer view for a source task.**
    ///
    /// **The real pane**, not `PaneRef::bare`: a handler's selection is the
    /// pane's since WO-M10c, so a task built against a bare pane would be
    /// built against the layer's defaults and fetch the wrong thing. The
    /// slots are hydrated first, because an unhydrated pane carries no state
    /// and no published selection at all.
    ///
    /// `None` for a pane index the layout does not have.
    pub(super) fn with_layer_pane<R>(
        &mut self,
        pane_idx: usize,
        id: &rustdar_source::id::LayerId,
        f: impl FnOnce(
            &mut rustdar_overlays::render::overlay_state::OverlayRegistry,
            &rustdar_source::handler::PaneRef<'_>,
        ) -> R,
    ) -> Option<R> {
        let (panes, overlays) = self.gui.panes_and_overlays_mut();
        if pane_idx >= panes.len() {
            return None;
        }
        panes[pane_idx].hydrate_layer_states(overlays, pane_idx);
        let view = panes[pane_idx].view(pane_idx);
        let pane_ref = view.layer(id);
        Some(f(overlays, &pane_ref))
    }

    /// **Why `layer` cannot build `target`, or `None` if it can.**
    ///
    /// A question about the layer and the field, and deliberately **not**
    /// about the pane: the pane index an ask carries is the volume store's
    /// holder id, which routinely names a holder the `Gui`'s own pane vector
    /// has no entry for. Asking through a pane view would refuse those asks by
    /// finding no pane, which is a silent way of never serving them.
    ///
    /// **Lives here rather than beside its caller** for two reasons that
    /// agree: this file already owns the other registry-facing helper,
    /// [`Self::with_layer_pane`], and `app.rs` was on the per-file ceiling
    /// `gui_seam_ratchet_tests` holds over App-pokes-Gui field accesses.
    /// Landing the one registry read there would have spent that file's last
    /// slot on a helper that has a better home anyway.
    pub(super) fn volume_layer_refusal(
        &self,
        layer: &rustdar_source::id::LayerId,
        target: &rustdar_egui::pane::VolumeTarget,
    ) -> Option<String> {
        let Some(handler) = self.gui.overlays.handler_by_id(layer) else {
            return Some(format!(
                "This build has no {} layer to build a 3D volume with.",
                layer.as_str(),
            ));
        };
        let Some(volume) = handler.volume() else {
            return Some(format!(
                "{} does not build 3D volumes.",
                handler.display_name(),
            ));
        };
        let Some(spec) = handler
            .products()
            .iter()
            .find(|spec| spec.id == target.product)
        else {
            return Some(format!(
                "{} does not publish a field called {}.",
                handler.display_name(),
                target.product.as_str(),
            ));
        };
        (!volume.builds(spec)).then(|| {
            format!(
                "{} has no vertical structure for {} to render in 3D.",
                spec.name,
                handler.display_name(),
            )
        })
    }

    /// **Ask `layer` to shape the job that builds `ctx`'s volume.**
    ///
    /// The frontend has paid for the payload and resolved the ground; what
    /// comes back is the layer's own work order, in a type this side cannot
    /// look inside. Nothing here matches on an id and nothing here names a
    /// request, a moment or an envelope — a second 3D source arrives as an
    /// `impl` and this function does not change.
    ///
    /// `None` on a layer this build does not have, one with no 3D half, or one
    /// that cannot shape a job from what it was handed. **Lives beside
    /// [`Self::volume_layer_refusal`]** for the reason stated there: the
    /// registry-facing helpers are this file's, and `app.rs` is the file the
    /// App-pokes-Gui ratchet is driving to zero.
    pub(super) fn volume_job(
        &self,
        layer: &rustdar_source::id::LayerId,
        ctx: rustdar_source::volume::VolumeJobContext,
    ) -> Option<rustdar_source::job::DescribedJob> {
        self.gui
            .overlays
            .handler_by_id(layer)?
            .volume()?
            .volume_job(ctx)
    }

    /// **The 3D asks a just-arrived volume answers** — one per pane that is
    /// *already* in Volume mode and about the volume that just landed.
    ///
    /// This is WO-M14c, and its whole content is a **moment**: the ask each of
    /// these panes would make from the draw loop some frames later, made now,
    /// at the pump row where the volume installs. Nothing here is new work.
    /// The level-trigger in the 3D arm stays exactly where it is as the
    /// fallback, and the store's `Building` entry makes the two converge —
    /// whichever asks second attaches to the build the first opened.
    ///
    /// **What gets nothing, and it is the boundary rather than an oversight:**
    ///
    /// * every pane not in Volume mode. No likely-next-product build, no
    ///   guess at a mode a pane might switch to. That refusal is **not**
    ///   spelled here: it is one of the three
    ///   [`rustdar_egui::pane::PaneState::volume_build_due`] holds, so the
    ///   arrival path and the draw-time level-trigger state the boundary once
    ///   between them rather than twice. A plan-view pane on the arriving
    ///   site is walked and then refused, which costs one hydrate per arrival
    ///   and buys one definition;
    /// * every pane past the layout's visible count, which is the same
    ///   denominator [`super::App::release_hidden_pane_volumes`] uses — a
    ///   hidden pane's grid is being *given back*, not built;
    /// * every pane whose own volume is not this arrival: another site, or a
    ///   navigated pane whose stamp is the scan it stepped back to;
    /// * every pane already rendered for the target, or playing a 3D loop —
    ///   the other two `volume_build_due` holds;
    /// * every pane at all when there is no painter, because a build nothing
    ///   can draw is the speculation this order refuses. The 3D arm returns
    ///   its empty state before reaching the level-trigger in exactly that
    ///   case, so this keeps the eager set a subset of the draw-time set.
    ///
    /// The **hydrate** is not optional: a handler answers `current_field` out
    /// of its own slot, and for the layer whose selection the pane owns the
    /// slot is only current once the pane has published it. The 3D arm runs
    /// the same hydrate immediately before its own walk.
    ///
    /// **Lives here rather than in `app.rs`** for the reason
    /// [`Self::volume_layer_refusal`] states: this file owns the
    /// registry-facing helpers, and `app.rs` is one slot under the ceiling the
    /// App-pokes-Gui ratchet is driving to zero.
    pub(super) fn arrived_volume_asks(
        &mut self,
        arrived: &std::collections::HashMap<String, rustdar_egui::CurrentVolumeStamp>,
    ) -> Vec<(
        usize,
        rustdar_source::id::LayerId,
        rustdar_egui::pane::VolumeTarget,
    )> {
        if arrived.is_empty() || self.volume_painter.is_none() {
            return Vec::new();
        }
        let visible = self.gui.panes().len();
        let (panes, overlays) = self.gui.panes_and_overlays_mut();
        let mut asks = Vec::new();
        for (pane_idx, pane) in panes.iter_mut().take(visible).enumerate() {
            let Some(current) = arrived.get(pane.site()) else {
                continue;
            };
            let current = *current;
            let Some((stamp, _)) = pane.volume_stamp(Some(current)) else {
                continue;
            };
            // A navigated pane's stamp is the scan it stepped back to, which
            // this arrival is not. Stated as a comparison rather than as a
            // `viewing_live` read so the one place that decides which volume a
            // pane is about stays `volume_stamp`.
            if stamp.collected != current.newest {
                continue;
            }
            pane.hydrate_layer_states(overlays, pane_idx);
            let Ok(ask) = pane.volume_ask(overlays, pane_idx) else {
                continue;
            };
            let target = pane.volume_target_for(&ask.field, stamp);
            if !pane.volume_build_due(&target) {
                continue;
            }
            asks.push((pane_idx, ask.layer, target));
        }
        asks
    }

    /// Spawn a listing task a layer built, and land its answer on the one
    /// source arrival path as [`SourceEvent::Frames`].
    fn spawn_frame_list_task(&self, task: rustdar_overlays::render::overlay_state::FetchTask) {
        use rustdar_overlays::render::overlay_state::FrameListingResult;
        let kind = task.kind.clone();
        let sender = self.channels.overlay_fetch_sender.clone();
        let window = self.window.clone();
        self.spawn_detached(async move {
            let data = task.future.await;
            match FrameListingResult::event(kind, data) {
                Some(event) => {
                    let _ = sender.send(event);
                }
                // Unreachable through `FrameListingResult::task`. A handler
                // that built its task some other way gets no arrival at all
                // rather than an empty listing it never produced.
                None => log::error!(
                    "a frame-list task answered with something that is not a frame listing",
                ),
            }
            super::notify_redraw(&window);
        });
    }

    /// Spawn one frame's fetch task, landing its payload as
    /// [`SourceEvent::FrameReady`] with the stamp that asked for it.
    pub(super) fn spawn_frame_fetch_task(
        &self,
        stamp: rustdar_source::time::FrameStamp,
        task: rustdar_overlays::render::overlay_state::FetchTask,
    ) {
        let kind = task.kind.clone();
        let sender = self.channels.overlay_fetch_sender.clone();
        let window = self.window.clone();
        self.spawn_detached(async move {
            let data = task.future.await;
            let _ = sender.send(SourceEvent::FrameReady {
                id: kind,
                stamp,
                data,
            });
            super::notify_redraw(&window);
        });
    }

    /// Hand a downloaded archive's **decode** to the job funnel, then answer on
    /// `sender`.
    fn decode_offloaded<T: Send + 'static>(
        window: Option<crate::WindowRef>,
        sender: Sender<T>,
        archive: Vec<u8>,
        respond: impl FnOnce(Option<rustdar_radar::scan::DecodedScan>) -> Option<T> + Send + 'static,
    ) {
        rustdar_worker::offload::offload_job(
            "level2-decode",
            rustdar_worker::offload::Job::Described(rustdar_worker::offload::JobRequest::describe(
                rustdar_radar::jobs::DecodeJob {
                    archive: std::sync::Arc::new(archive),
                },
                // A decode draws nothing, so its envelope carries no ceiling —
                // the same effective 0 it has always had.
                rustdar_worker::offload::ceiling_only_geometry(0),
            )),
            move |result| {
                // `None` here is an archive that did not decode, which `execute`'s arm
                // has already logged.
                let volume = result.and_then(|out| out.take::<rustdar_radar::scan::DecodedScan>());
                if let Some(message) = respond(volume) {
                    let _ = sender.send(message);
                }
                crate::app::notify_redraw(&window);
            },
        );
    }

    /// Refresh the cached network site catalogue, once per launch, detached.
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
    ///
    /// `requester` rides the whole way to the decode landing, because the
    /// landing is where it is needed: it decides both which panes may take
    /// the volume and which later request supersedes this one. See
    /// [`FetchRequester`].
    pub fn spawn_fetch(
        &mut self,
        site: String,
        timestamp: NaiveDateTime,
        requester: FetchRequester,
    ) {
        let generation = self.render.next_scan_generation(&site, requester);
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
                        requester,
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
                    requester,
                    result,
                    is_auto_poll: false,
                })
            });
        });
    }

    /// Spawn Level III product fetches for all supported Level III products.
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
        if !site_offers_level3(site) {
            log::debug!("{site} has no RPG, so no Level III objects are fetched for it");
            return;
        }
        // The RPG's own Melting Layer object (Level III 166, AWIPS `N0M`) for the
        // volume this site currently has loaded.
        if let Some(volume_start) = latest_scan_time_for_site(self.gui.panes(), site) {
            // Already in hand for this volume: the poll would re-download an object we
            // are already classifying against.
            if self.render.melting_layer_volume(site) != Some(volume_start) {
                let sender = self.channels.melting_layer_sender.clone();
                let site = site.to_string();
                self.spawn_async_task(sender, async move {
                    let sources = rustdar_radar::sources::DataSources::production();
                    // `VolumePick::NEAREST`: `N0M` is a once-per-volume product.
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
            // The RPG's own storm motion vector (Level III 56, AWIPS `N0S`) for the
            // same volume.
            if self.render.storm_motion_volume(site) != Some(volume_start) {
                let sender = self.channels.storm_motion_sender.clone();
                let site = site.to_string();
                self.spawn_async_task(sender, async move {
                    let sources = rustdar_radar::sources::DataSources::production();
                    // `VolumePick::NEAREST` for the reason `N0M` uses it: this is a
                    // once-per-volume product.
                    let found = rustdar_radar::level3::fetch_product_for_volume(
                        &sources,
                        &site,
                        STORM_MOTION_CODE,
                        volume_start,
                        rustdar_radar::level3::VolumePick::NEAREST,
                    )
                    .await;
                    // **Decoded here, and only the two scalars kept.**
                    let motion = found.as_ref().and_then(|p| {
                        match nexrad_level3::decode::decode_product(&p.bytes) {
                            Ok(msg) => match msg.pdb.storm_motion() {
                                // `storm_motion` is `Some` only for codes 55 and 56.
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
                        // Logged with the numbers: a zero vector is a reading
                        // here, not a failure.
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
        // One request per distinct object, not per (product, object).
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
            GuiAction::PrepareVolume {
                pane_idx,
                layer,
                target,
            } => {
                self.handle_prepare_volume(pane_idx, &layer, target);
            }
            GuiAction::ReleaseVolume { pane_idx } => {
                self.handle_release_volume(pane_idx);
            }
            GuiAction::PaneClosed { pane_idx } => {
                // The UI has renumbered. Everything this side keys on a pane
                // *position* at or above the closed one describes a different
                // pane now, and a render already running for one of those
                // indices would land on whichever pane took its number.
                self.render.forget_panes_from(pane_idx);
            }
            GuiAction::FetchOverlay { .. } | GuiAction::RefreshOverlay { .. } => {
                self.handle_overlay_action(action)
            }
            GuiAction::RenderOverlay { .. } => {
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
                    let ls = pane.loop_state_mut();
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
                    let ls = pane.loop_state_mut();
                    let next = (!ls.frames.is_empty()).then(|| {
                        let current = ls.current_frame();
                        if forward {
                            (current + 1) % ls.frames.len()
                        } else if current == 0 {
                            ls.frames.len() - 1
                        } else {
                            current - 1
                        }
                    });
                    if let Some(next) = next {
                        pane.park_on_loop_frame(next);
                    }
                }
            }
            GuiAction::SeekLoopFrame {
                pane_idx,
                frame_index,
            } => {
                if let Some(pane) = self.gui.pane_mut(pane_idx) {
                    pane.park_on_loop_frame(frame_index);
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
                self.location.start_serial(&config);
            }
            GuiAction::StopGps => {
                self.location.stop_serial();
            }
            // Through the gate in both directions, never straight at the bridge.
            GuiAction::RequestLocation => {
                let platform = &self.platform;
                self.location.enable(&|| platform.kv());
            }
            GuiAction::StopLocation => {
                let platform = &self.platform;
                self.location.disable(&|| platform.kv());
                // The location facts reach the UI through the per-frame compose
                // (`push_frame_inputs`).
                if !self.location.serial_active() {
                    self.user_gps = None;
                }
            }
            // Around the gate, and this is the one location action that legitimately
            // is.
            GuiAction::OpenLocationSettings => {
                self.location.open_settings();
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
                // Not addressed to a pane: `RadarConfig` carries no pane index
                // — the Set Time dialog, the menu and the status bar all build
                // it from `active_pane_fetch_config` — so the volume is the
                // site's and every pane on it takes it, exactly as before.
                self.spawn_fetch(radar_config.site, utc_timestamp, FetchRequester::Site);
            }
            GuiAction::CheckForNewScans(radar_config) => {
                // The chunk feed delivers this site's volume cut by cut, minutes
                // earlier, so the 60 s archive check for it is redundant.
                if self.chunks_are_feeding(&radar_config.site) {
                    return;
                }
                log::info!(
                    "Check for new scans: {} @ {} (local)",
                    radar_config.site,
                    radar_config.timestamp
                );

                let utc_timestamp = Self::local_to_utc(radar_config.timestamp);
                // This site's own current scan.
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
                            Self::decode_offloaded(window, sender, archive, move |volume| {
                                let volume = volume?;
                                Some(crate::channels::ScanResponse {
                                    generation,
                                    site: site.clone(),
                                    // The auto-poll is about the site, not a
                                    // pane, and keeps the site-wide audience
                                    // and the site-wide supersede rule.
                                    requester: FetchRequester::Site,
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

                // The clock does not move; applying it re-renders the Set
                // Time dialog's strings from the time still selected, which is
                // what `GuiEvent::RadarConfig` did here beside writing the
                // app-wide site. There is no app-wide site left to write --
                // the panes below carry their own -- so only this half
                // remains.
                let timestamp = self.gui.selected_timestamp();
                self.gui.apply(GuiEvent::SelectedTime(timestamp));

                // The linked group moves together; an unlinked pane moves alone.
                let moving = self.gui.layer_sync_targets(pane_idx);
                // Every field written here is flat on `PaneState`, so this reaches a
                // section or a volume pane unchanged and correctly.
                let mut left_a_radar: Vec<usize> = Vec::new();
                for idx in moving {
                    if let Some(pane) = self.gui.pane_mut(idx) {
                        // Only where the site really moves.
                        if pane.site() != site {
                            left_a_radar.push(idx);
                            pane.scan_info = None;
                            pane.data_time = None;
                            // The picture and the key that names what it was cut for,
                            // not the line: see above.
                            if let Some(xsect) = pane.cross_section_mut() {
                                xsect.section = None;
                                xsect.texture = None;
                                xsect.rendered_for = None;
                                xsect.unavailable =
                                    Some(rustdar_egui::pane::SectionUnavailable::AwaitingVolume);
                            }
                            // The loop is the same judgement as the scan above and
                            // belongs behind the same guard.
                            *pane.loop_state_mut() = rustdar_egui::pane::LayerTimeState::new();
                        }
                        pane.loading_site = Some(site.clone());
                        pane.set_site(site.clone());
                        pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
                    }
                }
                // The host-side half of the loop teardown, and it is **per
                // pane**, behind the same guard.
                for idx in &left_a_radar {
                    self.loop_mgr.remove_pending(*idx);
                }

                // The third way a 3D pane stops needing its volume.
                for idx in left_a_radar {
                    self.handle_release_volume(idx);
                    if let Some(pane) = self.gui.pane_mut(idx)
                        && let Some(volume) = pane.volume_mut()
                    {
                        volume.rendered_for = None;
                    }
                }

                let utc_timestamp = Self::local_to_utc(timestamp);
                // Site-wide on purpose: the switch has just moved every pane
                // in the layer group onto this site and cleared their scan
                // info, so the volume belongs to all of them and not to the
                // one that was clicked.
                self.spawn_fetch(site, utc_timestamp, FetchRequester::Site);
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
    fn fetch_overlay(&mut self, kind: rustdar_source::id::LayerId, pane_idx: usize) {
        let config = self.fetch_config();

        let tasks = self.with_layer_pane(pane_idx, &kind.clone(), |overlays, pane_ref| {
            let tasks = overlays.create_fetch_tasks(&kind, &config, pane_ref);
            if tasks.is_empty() {
                // A handler that cannot build a task says so, and is believed.
                log::warn!("{kind:?}: no fetch task could be built; backing off");
                overlays.record_fetch_failure(
                    &kind,
                    &rustdar_overlays::fetch_policy::FetchError::permanent(
                        "no fetch task could be built",
                    ),
                    pane_ref,
                );
                return Vec::new();
            }
            log::info!(
                "Fetching overlay data for {:?} ({} task(s))",
                kind,
                tasks.len()
            );
            overlays.set_fetching(&kind, true, pane_ref);
            tasks
        });
        let Some(tasks) = tasks.filter(|tasks| !tasks.is_empty()) else {
            return;
        };

        for task in tasks {
            let task_kind = task.kind;
            self.spawn_async_task(self.channels.overlay_fetch_sender.clone(), async move {
                let data = task.future.await;
                SourceEvent::Data(OverlayFetchResult {
                    kind: task_kind,
                    data,
                })
            });
        }
    }

    /// The deliver every overlay job shares — sites and the six handler-backed kinds,
    /// which is every texture overlay.
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
    ) -> impl FnOnce(rustdar_worker::offload::JobResult) + Send + 'static {
        move |result| {
            let expected = (width as usize) * (height as usize) * 4;
            if let Some(rustdar_overlays::render::rasterize::RasterizeOutput {
                rgba,
                hit_cells,
                ..
            }) = result
                .and_then(|out| out.take::<rustdar_overlays::render::rasterize::RasterizeOutput>())
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
                        // A mismatched hit map zipped anyway would hand hovers to the
                        // wrong items; shown without one.
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
        id: LayerId,
        req: OverlayRenderRequest,
    ) {
        use rustdar_egui::overlay_cache::ZOOM_QUANTIZATION_FACTOR;

        let record = req.clone();
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

        if self.gui.overlays.render_mode(&id)
            != Some(rustdar_overlays::render::overlay_state::RenderMode::Texture)
        {
            // Also the unregistered-id exit: `render_mode` answers `None` for an id no
            // handler owns.
            log::warn!(
                "spawn_overlay_render called with a non-texture layer: {}",
                id.as_str()
            );
            return;
        }

        for &pidx in &pane_indices {
            if let Some(pane) = self.gui.pane_mut(pidx) {
                pane.overlay_cache_mut(&id).render_in_flight = true;
            }
            // **The record, written here and nowhere else** — beside the mark, so
            // "this pane has a raster of this layer out" and "this is what it was
            // asked for" are set by the same statement and cannot disagree. Both
            // paths reach this function, so both write it; the arrival path reads
            // it back for the geometry alone (WO-M13a).
            self.render
                .record_overlay_dispatch(pidx, &id, record.clone());
        }

        let render_bounds = texture.coverage(&geo_bounds);

        let first_pane_idx = pane_indices[0];
        if self.gui.pane(first_pane_idx).is_none() {
            self.clear_overlay_render_marks(&pane_indices, &id);
            return;
        }

        let sender = self.channels.overlay_render_sender.clone();
        let window = self.window.clone();

        match &id {
            // **The seven described kinds** — the three polygon kinds, the two
            // hit-map kinds, the model grid and, since WO-M10c, the site
            // table. The list is a coincidence of "answers `prepare_job`",
            // pinned in `texture_tests`; `RadarSites` joined it when the
            // handler gained a pane to read its site from.
            id if *id == known::SPC_OUTLOOK
                || *id == known::SPC_DISCUSSIONS
                || *id == known::NWS_ALERTS
                || *id == known::STORM_REPORTS
                || *id == known::LIGHTNING
                || *id == known::MODEL_DATA
                || *id == known::RADAR_SITES =>
            {
                let clock = chrono::Utc::now().naive_utc();
                let rctx = rustdar_overlays::render::overlay_state::RasterizeContext {
                    is_dark: self.cached_dark_theme.unwrap_or(false),
                    zoom: zoom as f64 / ZOOM_QUANTIZATION_FACTOR,
                    // Off the plan the pixel count came from, never re-read from the
                    // context.
                    device_scale: texture.pixels_per_point,
                    // THE capture of the page's clock, and the only one on
                    // this path — read ONCE and handed to both fields, so a
                    // live pane's depicted instant cannot drift a nanosecond
                    // from its wall clock.
                    now: clock,
                    // **The instant the picture depicts.** A live pane depicts
                    // the present, so `as_of == now` and the bytes are what
                    // they were before this field existed — which is what
                    // keeps WO-M11's dark parity true permanently, not just
                    // until someone scrubs. A scrubbed pane hands its own
                    // clock to the layers whose picture is a function of it,
                    // and `now` is left alone either way.
                    as_of: as_of_for_layer(&self.gui, first_pane_idx, id, clock),
                };
                // **The real pane, with its siblings.** `PaneView` and not
                // `layer_ref`: the site table reads the radar slot's `"site"`
                // out of `pane.slots`, and `layer_ref` carries none.
                let built = {
                    let (panes, overlays) = self.gui.panes_and_overlays_mut();
                    panes[first_pane_idx].hydrate_layer_states(overlays, first_pane_idx);
                    let view = panes[first_pane_idx].view(first_pane_idx);
                    let pane = view.layer(id);
                    let job = overlays.prepare_job(id, &rctx, &pane);
                    // The page-side half of a hit map.
                    let id_map = overlays.hit_items(id);
                    // The codec row the handler registered — its label is the
                    // job's name in the timing log.
                    let row = overlays.job_codec(id);
                    job.zip(row).map(|(job, row)| (job, id_map, row))
                };
                let Some((job, id_map, row)) = built else {
                    self.clear_overlay_render_marks(&pane_indices, id);
                    return;
                };
                let geometry = rustdar_source::job::JobGeometry {
                    width,
                    height,
                    bounds: render_bounds,
                    side_ceiling_px: 0,
                };
                let request = rustdar_worker::offload::JobRequest { geometry, job };
                rustdar_worker::offload::offload_job(
                    row.label,
                    rustdar_worker::offload::Job::Described(request),
                    Self::overlay_job_deliver(
                        row.label,
                        width,
                        height,
                        id_map,
                        OverlayRenderResponse {
                            image: None,
                            geo_bounds: render_bounds,
                            overlay_kind: id.clone(),
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
            // **Everything else — and it is NOT "the five non-texture layers".**
            // Four of those five never reach this match at all: `Metar`
            // (PerFramePoint), `CityLabels` (Tile), `UserLocation` and
            // `ColorScale` (PerFrameDirect) are refused by the `render_mode`
            // guard at the top of this function, which is also the
            // unregistered-id exit. What lands here is a layer that DOES
            // declare `RenderMode::Texture` and is not one of the seven above:
            // `Radar`, whose raster is its own pipeline's and never this
            // dispatch's — which is exactly why WO-M13a had to refuse radar by
            // name at the arrival gate — plus the fake source when its feature
            // is on.
            _ => {
                log::warn!(
                    "spawn_overlay_render reached the dispatch with a layer it \
                     cannot rasterize: {}",
                    id.as_str()
                );
                self.clear_overlay_render_marks(&pane_indices, &id);
            }
        }
    }

    /// **The rasters a data arrival has just made stale**, one ask per
    /// `(pane, layer)`, ready for the same dedupe/grouping/dispatch the action
    /// path runs.
    ///
    /// This is the arrival half of WO-M13a: the draw loop stops being the only
    /// thing that can *discover* a stale overlay raster. It is not a second
    /// dispatcher — every ask returned here goes through
    /// [`App::dispatch_overlay_renders`] and then
    /// [`Self::spawn_overlay_render`], the same two functions the action path
    /// uses, because that is where the `render_in_flight` marks are owned.
    ///
    /// **The token is recomputed, never read back.** The recorded
    /// `data_generation` is what the picture on the glass was keyed at, so it
    /// is stale by construction — that staleness *is* the trigger. The fresh
    /// one comes from [`rustdar_egui::overlay_cache_token`], the very function
    /// the draw loop's `needs_rerender` pass calls, so the two paths cannot
    /// disagree about what "the picture would be different" means.
    ///
    /// **The geometry is re-used, never recomputed.** Viewport, texture plan
    /// and zoom come off the record unchanged: this runs in `Ingest`, before
    /// the frame is laid out, and a zoom-driven rebuild is the draw loop's to
    /// discover on the frame that actually knows the new zoom.
    pub(super) fn arrived_overlay_asks(
        &mut self,
        arrived: &[LayerId],
    ) -> Vec<(usize, LayerId, OverlayRenderRequest)> {
        let mut asks: Vec<(usize, LayerId, OverlayRenderRequest)> = Vec::new();

        // Every (pane, layer, record) an arrival could bear on, collected before
        // `gui` is borrowed.
        let mut holders: Vec<(usize, LayerId, OverlayRenderRequest)> = Vec::new();
        for id in arrived {
            // **Radar is not on this path and must never be.** Radar's own
            // arrival dispatch is WO-M14c's, at the volume stamp; the overlay
            // record is the texture path's, and the draw loop excludes radar
            // from `needs_rerender` for the same reason.
            if *id == known::RADAR {
                continue;
            }
            holders.extend(
                self.render
                    .overlay_record_holders(id)
                    .into_iter()
                    .map(|(pane_idx, req)| (pane_idx, id.clone(), req)),
            );
        }
        if holders.is_empty() {
            return asks;
        }

        // The theme the dispatch would rasterize at, and the same term the draw
        // loop mixes into its token.
        let is_dark = self.cached_dark_theme.unwrap_or(false);
        // The panes the layout is showing, from the same slice `render_panes`
        // draws. A record outlives its pane going hidden, and a raster for a
        // pane nobody paints is the speculation this path refuses.
        let visible = self.gui.panes().len();
        let (panes, overlays) = self.gui.panes_and_overlays_mut();

        for (pane_idx, id, recorded) in holders {
            if pane_idx >= visible {
                continue;
            }
            let Some(pane) = panes.get_mut(pane_idx) else {
                continue;
            };
            if !pane.is_overlay_enabled(&id) {
                continue;
            }
            if overlays.render_mode(&id)
                != Some(rustdar_overlays::render::overlay_state::RenderMode::Texture)
            {
                continue;
            }
            // Not a dedupe: a second ask under an outstanding one would take a
            // render slot for a picture the first is already drawing, and the
            // draw loop declines on the same flag.
            if pane.overlay_cache_mut(&id).render_in_flight {
                continue;
            }
            // **No hydrate here, and that is measured rather than assumed.**
            // The only caller is the `SourceEvent::Data` drain, and
            // `Gui::deliver_overlay_fetch` goes through `across_panes`, which
            // hydrates every pane up to `pane_layout.pane_count` — the same
            // set `visible` above admits — one statement before this runs. A
            // hydrate here was written, tampered, and came back green because
            // of that; it is gone rather than kept as a line no test can fail.
            // If a second caller ever reaches this function from somewhere the
            // delivery door does not, it hydrates first.
            //
            // The draw loop refuses on this too. An arrival that emptied a
            // layer is not a reason to rasterize nothing.
            if !overlays.has_data(&id, &pane.layer_ref(pane_idx, &id)) {
                continue;
            }
            let fresh = rustdar_egui::overlay_cache_token(overlays, pane_idx, pane, &id, is_dark);
            if fresh == recorded.data_generation {
                continue;
            }
            // **`info!`, and the level is load-bearing.** The browser build
            // initialises `console_log` at `Level::Info`, so a `debug!` line
            // here is invisible to the only instrument the [WEB] gate has —
            // and this line is what shows a raster was dispatched from the
            // arrival drain rather than from a frame. Its cadence is one per
            // (pane, layer) per arrival that moved the token, which is the
            // cadence `offload`'s own per-job timing line already runs at.
            //
            // "ahead of this frame's draw-loop pass", precisely: the drain is
            // `Ingest`, which runs before `setup_egui_frame` builds the paint
            // list; the pass that would have noticed runs inside that build,
            // and the action it emits is not processed until after
            // `present_frame`.
            log::info!(
                "overlay raster: pane {pane_idx} asked {} to redraw as its data \
                 arrived, ahead of this frame's draw-loop pass (token {} -> {})",
                id.as_str(),
                recorded.data_generation,
                fresh,
            );
            asks.push((
                pane_idx,
                id,
                OverlayRenderRequest {
                    data_generation: fresh,
                    ..recorded
                },
            ));
        }
        asks
    }

    /// Undo the in-flight marks [`spawn_overlay_render`](Self::spawn_overlay_render)
    /// set on its target panes.
    ///
    /// (See [`as_of_for_layer`] for the depicted instant this dispatch hands
    /// the rasterizer.)
    fn clear_overlay_render_marks(&mut self, pane_indices: &[usize], id: &LayerId) {
        for &pidx in pane_indices {
            if let Some(pane) = self.gui.pane_mut(pidx) {
                pane.overlay_cache_mut(id).render_in_flight = false;
            }
        }
    }

    /// Enable radar loop for a pane: initializes loop state and spawns
    /// an async task to list available scans in the lookback window.
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

        // **The layer lists its own archive.** The identifiers the listing
        // yields stay with it — nothing here holds one — and the site it was
        // listed for is captured inside the task, at dispatch, not read back
        // off the pane when it lands.
        let config = self.fetch_config();
        let task = self
            .with_layer_pane(
                pane_idx,
                &rustdar_source::id::known::RADAR,
                |overlays, pane_ref| {
                    overlays.create_frame_list_task(
                        &rustdar_source::id::known::RADAR,
                        &config,
                        pane_ref,
                        (start, end),
                    )
                },
            )
            .flatten();
        let Some(task) = task else {
            log::warn!(
                "Loop: the radar layer could not build a {site} scan listing for pane \
                 {pane_idx}; leaving loop mode",
            );
            self.handle_disable_loop(pane_idx);
            return;
        };
        self.spawn_frame_list_task(task);
    }

    /// Disable radar loop for a pane: resets to single-frame mode.
    fn handle_disable_loop(&mut self, pane_idx: usize) {
        if let Some(pane) = self.gui.pane_mut(pane_idx) {
            *pane.loop_state_mut() = rustdar_egui::pane::LayerTimeState::new();
        }
        self.loop_mgr.remove_pending(pane_idx);
        if pane_idx < self.render.pane_render.len() {
            self.render.pane_render[pane_idx].last_rendered = None;
        }
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

        let local_ts = chrono::TimeZone::from_utc_datetime(&chrono::Local, &target).naive_local();
        self.gui.apply(GuiEvent::SelectedTime(local_ts));
        self.gui.apply(GuiEvent::Fetching(true));

        self.spawn_fetch(site, target, FetchRequester::Pane(pane_idx));
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

        let requester = FetchRequester::Pane(pane_idx);
        let generation = self.render.next_scan_generation(&site, requester);

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
                            requester,
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
                        requester,
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
        // which site the fallback fetch names.
        let Some(pane_site) = self.gui.pane(pane_idx).map(|p| p.site().to_string()) else {
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
            // `scan_info.timestamp` and not `timestamp`: the cached tuple holds
            // both, and it is the `ScanInfo`'s that lands on the pane below and
            // that the render then looks the volume up with.
            let forced = self.volumes.install_still(
                pane_site.clone(),
                scan_info.timestamp,
                (scan_arc, declared),
            );
            rustdar_worker::offload::discard_each("capped-still", forced);

            let local_ts =
                chrono::TimeZone::from_utc_datetime(&chrono::Local, &timestamp).naive_local();
            self.gui.apply(GuiEvent::SelectedTime(local_ts));
            // Addressed, like every other navigation: this pane asked to go
            // live, and a same-site pane parked in the archive is not asking.
            self.gui.apply(GuiEvent::ScanInfoForTimeGroup {
                site: pane_site.clone(),
                requester: pane_idx,
                info: scan_info,
            });
            self.gui.clear_loading_site_for_site(&pane_site);
            self.render.reset_panes_for_site(&pane_site, &self.gui);
            self.spawn_level3_fetches(&pane_site);

            self.manual_nav_pending = false;
            self.reinit_active_loops();
            return;
        }

        // On a site the chunk feed is serving, live is a *reattachment*, not a fetch.
        if self.chunks_are_feeding(&pane_site)
            && latest_scan_time_for_site(self.gui.panes(), &pane_site).is_some()
        {
            self.gui.clear_loading_site_for_site(&pane_site);
            self.manual_nav_pending = false;
            return;
        }

        let now = chrono::Local::now().naive_local();
        self.gui.apply(GuiEvent::SelectedTime(now));
        self.gui.apply(GuiEvent::Fetching(true));

        let utc_timestamp = Self::local_to_utc(now);
        self.spawn_fetch(pane_site, utc_timestamp, FetchRequester::Pane(pane_idx));
    }

    /// **Take delivery of one loop frame's archive object** and hand its
    /// decode to the job funnel.
    ///
    /// The bytes arrive on the one source path, from the task
    /// `SourceHandler::fetch_frame` built; the decode is dispatched here, from
    /// the arrival, so it is scheduled beside every other offloaded job rather
    /// than inside a network task.
    ///
    /// **Every loop frame comes through here**, and on wasm that is up to
    /// `MAX_LOOP_FRAMES` of them — 14, not the 60 desktop holds.
    pub(super) fn take_loop_frame_archive(&self, fetch: rustdar_radar::source::RadarFrameFetch) {
        let rustdar_radar::source::RadarFrameFetch {
            site,
            timestamp,
            archive,
        } = fetch;
        let sender = self.channels.loop_scan_download_sender.clone();
        // A failed download still arrives: the response is the only thing that
        // clears the frame's in-flight mark, so it can be retried.
        let Some(archive) = archive else {
            let _ = sender.send(crate::channels::LoopScanDownloadResponse {
                site,
                timestamp,
                scan: None,
            });
            return;
        };
        Self::decode_offloaded(self.window.clone(), sender, archive, move |volume| {
            // Both halves, because a loop frame is dealiased on the same terms as
            // the still frame beside it.
            let scan = volume.map(|volume| {
                (
                    std::sync::Arc::new(volume.scan),
                    std::sync::Arc::new(volume.declared_nyquist),
                )
            });
            Some(crate::channels::LoopScanDownloadResponse {
                site,
                timestamp,
                scan,
            })
        });
    }

    /// Spawn the key listing a pane's Level III loop pairings will be ranked against:
    /// one request per UTC day the loop's window touches.
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
    pub(super) fn spawn_loop_frame_render(
        &self,
        pane_idx: usize,
        timestamp: NaiveDateTime,
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

        // Both the render call and the response's `snapped` read `params`, and nothing
        // here re-derives either from `target`.
        let crate::render_dispatch::RenderParams {
            product,
            elevation: snapped,
            lat,
            lon,
        } = params;
        let sender = self.channels.loop_render_sender.clone();
        let window = self.window.clone();

        // The storm motion override is read from the dispatcher for the same
        // reason `spawn_level2_render` reads it there.
        let storm_motion = (product == rustdar_radar::types::RadarProduct::StormRelativeVelocity)
            .then(|| self.render.storm_motion_override_kt())
            .flatten();
        // The environmental heights ride the same way for the hail pair and the
        // classification.
        let env_heights = self.render.env_heights_km_msl_for(product, &target.site);
        // The melting layer does **not** ride the same way, and the difference
        // is `timestamp`.
        let melting_layer = self
            .render
            .melting_layer_product_for(product, &target.site, timestamp);
        // The RPG's storm motion is asked per frame for exactly the reason the
        // melting layer is.
        let rpg_storm_motion = self
            .render
            .rpg_storm_motion_for(product, &target.site, timestamp);
        let ctx = rustdar_radar::loop_downloads::LoopRenderContext {
            product,
            elevation: snapped,
            lat,
            lon,
            storm_motion,
            env_heights,
            srv_fallback: self.render.srv_fallback(),
            melting_layer,
            rpg_storm_motion,
        };
        // Which job input this frame's data makes is radar's answer, not this
        // crate's: the described job crosses back with its input type erased and
        // is dispatched here without either arm being named.
        let job = match self
            .loop_mgr
            .frame_render_job(&target.site, &timestamp, &ctx)
        {
            Some(described) => {
                rustdar_worker::offload::Job::Described(rustdar_worker::offload::JobRequest {
                    job: described,
                    // A loop frame, so the loop size — the one envelope both
                    // radar frame rows take.
                    geometry: rustdar_worker::offload::ceiling_only_geometry(
                        rustdar_device_profile::constants::LOOP_IMAGE_SIZE as u32,
                    ),
                })
            }
            None => rustdar_worker::offload::Job::renders_nothing(),
        };
        rustdar_worker::offload::offload_job("loop-render", job, move |output| {
            let _guard = guard;
            // An output of another kind is `None` here, which is the same "nothing to
            // draw" a failed render has always been.
            let frame = output.and_then(|out| out.take::<rustdar_radar::frame::RenderedFrame>());
            // A failed render still has to be sent, so render_in_flight gets cleared.
            let (image, max_range_km, nyquist_ms, describes, polar) = match frame {
                Some(mut frame) => {
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
            // between them.
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
    pub(super) fn append_scan_to_active_loops(
        &mut self,
        site: &str,
        timestamp: chrono::NaiveDateTime,
        scan: std::sync::Arc<nexrad_model::data::Scan>,
        declared: std::sync::Arc<rustdar_radar::nyquist::DeclaredNyquist>,
    ) {
        // Store in the shared cache under this scan's own site, for every loop on that
        // site to use.
        self.loop_mgr.cache_scan(site, timestamp, (scan, declared));

        let allocation = self.loop_allocation();
        let budgets = self.budgets;
        append_polled_frame_to_loops(self.gui.panes_mut(), site, timestamp, allocation, &budgets);
    }

    /// Re-initialize radar loops on all panes that have an active loop.
    pub(super) fn reinit_active_loops(&mut self) {
        let mut to_reinit = Vec::new();
        for pane_idx in 0..self.gui.pane_count() {
            if let Some(pane) = self.gui.pane_mut(pane_idx)
                && pane.loop_state().is_active()
            {
                to_reinit.push((pane_idx, pane.loop_state().span_secs));
            }
        }
        for (pane_idx, lookback_secs) in to_reinit {
            self.handle_enable_loop(pane_idx, lookback_secs);
        }
    }
}

/// **Which shell event a landed volume is**, from who asked for it.
///
/// A free function rather than a method because the choice is a pure function
/// of the requester: the audience is resolved inside the UI layer, at
/// `Gui::apply`, where the pane links live. The app layer names the requester
/// and nothing else.
pub(crate) fn scan_info_delivery(
    site: String,
    requester: FetchRequester,
    info: rustdar_radar::types::ScanInfo,
) -> GuiEvent {
    match requester {
        FetchRequester::Pane(requester) => GuiEvent::ScanInfoForTimeGroup {
            site,
            requester,
            info,
        },
        FetchRequester::Site => GuiEvent::ScanInfoForSite { site, info },
    }
}

/// The UTC instant `timestamp` names as a wall-clock time in `tz`.
fn local_to_utc_in<Tz: TimeZone>(tz: &Tz, timestamp: NaiveDateTime) -> NaiveDateTime {
    if let Some(resolved) = tz.from_local_datetime(&timestamp).latest() {
        return resolved.with_timezone(&chrono::Utc).naive_utc();
    }
    let past_the_gap = timestamp + chrono::Duration::hours(1);
    if let Some(resolved) = tz.from_local_datetime(&past_the_gap).latest() {
        return resolved.with_timezone(&chrono::Utc).naive_utc();
    }
    // No zone in the tz database skips more than an hour, so this is unreachable in
    // practice.
    use chrono::Offset;
    let offset = tz.offset_from_utc_datetime(&timestamp).fix();
    timestamp - chrono::Duration::seconds(offset.local_minus_utc() as i64)
}

/// The newest scan of `site` any pane is currently showing, or `None` if none is.
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
    pub(super) fn spawn_loop_section_render(
        &self,
        req: crate::app::render::LoopSectionRequest,
        scan: std::sync::Arc<nexrad_model::data::Scan>,
        declared: std::sync::Arc<rustdar_radar::nyquist::DeclaredNyquist>,
    ) -> crate::render_dispatch::SectionDispatch {
        use crate::render_dispatch::SectionDispatch;
        // Destructured rather than taken as eight parameters, and the plan is what
        // supplies them: the ladder the staleness test resolved.
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

        let field = target.product.clone();
        // The extraction is radar's own, keyed by radar's field; the target
        // names it by id.
        let Some(product) = crate::render_key::radar_field(&field) else {
            return SectionDispatch::NoPayload;
        };
        // Read off the dispatcher, never from the caller, so the vector a frame is
        // *keyed* on cannot differ from the one it is derived with.
        let motion = (field == rustdar_radar::fields::known::STORM_RELATIVE_VELOCITY)
            .then(|| self.render.storm_motion_override_kt())
            .flatten();
        // Read off the dispatcher for the same reason and stamped on the payload below.
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
        // The live section's stamp, off this frame's own volume: a section of NROT or
        // SRV is dealiased on the worker.
        .map(|input| {
            input
                .with_declared_nyquist(&declared)
                .with_srv_fallback(fallback)
        }) else {
            // This volume carries no field to cut under this product.
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
        let job =
            rustdar_worker::offload::Job::Described(rustdar_worker::offload::JobRequest::describe(
                rustdar_radar::jobs::SectionJob {
                    input: Box::new(input),
                    request,
                },
                // A section's raster is a constant of the view, so its envelope
                // carries no ceiling — the same effective 0 it has always had.
                rustdar_worker::offload::ceiling_only_geometry(0),
            ));
        let sender = self.channels.loop_section_sender.clone();
        let window = self.window.clone();
        rustdar_worker::offload::offload_job("loop-section", job, move |output| {
            let _guard = guard;
            // An output of another kind is `None`, which takes the same "nothing to
            // draw" path a refused cut does.
            let cut = output.and_then(|out| out.take::<rustdar_radar::xsect::CrossSection>());
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
pub(super) struct LoopScanRequest {
    site: String,
    start: NaiveDateTime,
    end: NaiveDateTime,
}

/// Build `pane_idx`'s loop state and return the scan listing it now needs, or
/// `None` if that pane has no scan loaded to anchor a loop on.
fn begin_loop_for_pane(
    panes: &mut [rustdar_egui::pane::PaneState],
    loop_mgr: &mut rustdar_radar::loop_downloads::LoopDownloadManager,
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
    loop_mgr.remove_pending(pane_idx);

    // The view the pane is drawing, which is what a loop's frames are pictures of.
    let view = panes[pane_idx].render_view();
    if !view.can_loop() {
        return None;
    }
    *panes[pane_idx].loop_state_mut() = radar_layer::begin_loop(lookback_secs, &radar_site, view);

    Some(LoopScanRequest {
        site: radar_site.name.to_string(),
        start: end - chrono::Duration::seconds(lookback_secs as i64),
        end,
    })
}

/// Append a frame for a scan polled from `site` at `timestamp` to every active
/// loop that is on that site.
fn append_polled_frame_to_loops(
    panes: &mut [rustdar_egui::pane::PaneState],
    site: &str,
    timestamp: chrono::NaiveDateTime,
    allocation: crate::loop_pool::LoopAllocation,
    budgets: &rustdar_device_profile::budget::Budgets,
) {
    for (pane_idx, pane) in panes.iter_mut().enumerate() {
        // The pane's whole-loop cap, divided across the layers it is
        // animating — the budget is a texture-memory allowance and a pane
        // animating two things spends it twice.
        let held = super::render::layer_share(
            super::render::loop_frames_held(allocation, pane.loop_state(), budgets),
            pane.animating_layers().count(),
        );
        // Read before the timeline is borrowed mutably: the window this
        // append evicts against is the pane's clock, not the layer's.
        let clock = pane.time.mode;
        if append_polled_frame(pane.loop_state_mut(), site, timestamp, held, clock) {
            // The frame list moved under the playhead — evicted at the front,
            // possibly re-sampled — so the pane's clock names a different
            // index now. It names the same INSTANT, which is the point.
            pane.settle_playheads();
            log::info!(
                "Appended {} scan {} to loop on pane {} ({} frames)",
                site,
                timestamp,
                pane_idx,
                pane.loop_state().frames.len()
            );
        }
    }
}

/// **The instant a layer's raster should depict, for the pane it is dispatched
/// from.**
///
/// Only a [`TimeAxis::EventLifetime`] layer reads it: its picture is *which of
/// its items are valid then*, so a scrub has to move it. A `Live` layer draws
/// whatever it last fetched and ignores the field by contract, and a
/// `FrameSeries` layer's picture is one named frame rather than a function of
/// an instant — both keep the wall clock, so neither's bytes move.
///
/// `fallback` is the page's own clock, captured once by the caller and handed
/// to `now` as well, so a live pane's two fields cannot drift apart.
fn as_of_for_layer(
    gui: &rustdar_egui::Gui,
    pane_idx: usize,
    id: &rustdar_source::id::LayerId,
    fallback: chrono::NaiveDateTime,
) -> chrono::NaiveDateTime {
    let Some(pane) = gui.pane(pane_idx) else {
        return fallback;
    };
    let Some(instant) = pane.time.mode.as_of() else {
        return fallback;
    };
    let event_lifetime = gui.overlays.handlers().any(|handler| {
        handler.id() == *id
            && matches!(
                handler.time_axis(),
                rustdar_source::time::TimeAxis::EventLifetime
            )
    });
    if event_lifetime { instant } else { fallback }
}

/// Add a frame at `timestamp` to `ls` if the loop is active, is on `site`, and does
/// not already have that frame. Returns whether a frame was added.
fn append_polled_frame(
    ls: &mut rustdar_egui::pane::LayerTimeState,
    site: &str,
    timestamp: chrono::NaiveDateTime,
    held: usize,
    clock: rustdar_egui::pane::TimeMode,
) -> bool {
    use rustdar_egui::pane::LoopFrame;

    if !ls.is_active() {
        return false;
    }
    // `LayerTimeState::site` is the loop's *geometry* site, captured when the loop
    // was built — not the pane's live `site` field.
    if radar_layer::site(ls) != site {
        return false;
    }
    if ls.frames.iter().any(|f| f.timestamp == timestamp) {
        return false;
    }

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

    // **The window is anchored on the pane's CLOCK** (WO-M12f), resolved
    // through this layer's own axis: a live pane's clock is the newest frame
    // it holds — which is what `TimeMode::Live` means to a frame series — and
    // a scrubbed pane's is the instant it is parked on. Anchored on the newest
    // frame unconditionally, as it was, one arriving live frame evicted the
    // frames a scrubbed pane was actually looking at.
    let lookback = chrono::Duration::seconds(ls.span_secs as i64);
    let anchor = match clock {
        rustdar_egui::pane::TimeMode::AsOf(instant) => Some(instant),
        rustdar_egui::pane::TimeMode::Live => ls.frames.last().map(|f| f.timestamp),
    };
    if let Some(anchor) = anchor {
        let cutoff = anchor - lookback;
        ls.frames.retain(|f| f.timestamp >= cutoff);
    }

    // Re-measure the site's cadence, while it is still measurable.
    if ls.sampled != Some(true) {
        let times: Vec<_> = ls.frames.iter().map(|f| f.timestamp).collect();
        if let Some(step) = super::render::median_step_secs(&times) {
            ls.cadence_secs = Some(step);
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

#[path = "app_fetch/as_of_dispatch_tests.rs"]
#[cfg(test)]
mod as_of_dispatch_tests;

#[path = "app_fetch/layer_budget_wiring_tests.rs"]
#[cfg(test)]
mod layer_budget_wiring_tests;

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

/// The three polygon overlay dispatches are described jobs that reach the installed
/// sink — each carrying its own kind's input.
#[cfg(test)]
#[path = "app_fetch/polygon_wire_tests.rs"]
mod polygon_wire_tests;

/// The two hit-map overlay dispatches are described jobs whose delivered cells zip with
/// the dispatch-captured items.
#[cfg(test)]
#[path = "app_fetch/hitmap_wire_tests.rs"]
mod hitmap_wire_tests;

/// The model-grid dispatch is a described job carrying the grid by `Arc`, whose wire
/// form is the projection window.
#[cfg(test)]
#[path = "app_fetch/model_wire_tests.rs"]
mod model_wire_tests;

#[path = "app_fetch/site_switch_tests.rs"]
#[cfg(test)]
mod site_switch_tests;

#[cfg(test)]
#[path = "app_fetch/overlay_arrival_tests.rs"]
mod overlay_arrival_tests;
