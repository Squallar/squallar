//! The Add-layer catalog: one modal over everything, four groups, one search.
//!
//! Opened by the stack's two `+ Add layer` buttons (plan §1.3), presented as
//! an [`egui::Modal`] at every width for now — M6's phone shell re-hosts it
//! as a sheet page. The body is four groups the search filters across:
//! **Presets** (the compiled-in three plus the user's own, §3.11), the 12
//! **overlays**, the 17 **radar products** and the 16 **HRRR parameters** —
//! the real app's real options and nothing else (decision §0: no planned
//! groups, no SOON badges).
//!
//! Clicking a tile *applies* it and closes the catalog: an overlay tile turns
//! the layer on through the shared enable-fetch helper
//! ([`Gui::set_pane_overlay_with_fetch`](super::Gui)) and selects it in the
//! inspector; a product tile aims the active pane at that product (converting
//! it back to a map if it was not one) and selects the Radar layer; an HRRR
//! tile enables the model layer, sets the parameter through the handler's own
//! control route, and selects the model layer. A preset tile rebuilds the
//! whole layout — see [`Gui::apply_preset`](super::Gui).
//!
//! # The catalog renders after everything else
//!
//! [`Gui::ui`](super::Gui::ui) calls this after the pane loop, the pending
//! appliers and the feature popup: every `mem::take` window has closed, so
//! applying a tile may write panes directly, and a preset may grow the pane
//! count on the same terms as the region applier. The late draw also stacks
//! the modal above a feature popup left open, which is the order
//! `dismiss_top_layer` closes them in.

use crate::actions::GuiAction;
use rustdar_overlays::hrrr::ModelParameter;
use rustdar_overlays::render::controls::{
    ControlEffect, ControlUpdate, ControlValue, PaneControlContextMut,
};
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::types::RadarProduct;
use serde::{Deserialize, Serialize};

/// The catalog's roomy width, narrowed by
/// [`LayoutCtx::dialog_width`](crate::ui_layout::LayoutCtx) on a screen that
/// cannot afford it.
const CATALOG_WIDTH: f32 = 520.0;

/// What the header and its separator cost over the scroll body, plus the
/// modal's own margins — charged against the body's ceiling so the whole
/// modal stays inside the content rect.
const HEADER_ALLOWANCE: f32 = 160.0;

/// The close button's glyph — the same ✕ the inspector's deselect uses.
const CLOSE_LABEL: &str = "\u{2715}";

/// The save tile's label. Drawn only while the search box is empty: the
/// search is for *finding* tiles, and a save offer matching the query "save"
/// would be the one tile that is not a result.
const SAVE_TILE_LABEL: &str = "\u{ff0b} Save current view\u{2026}";

/// One saved multi-pane setup (plan §3.11): how many panes, what each shows,
/// and which overlays the layout runs with.
///
/// Both the domain type ([`Gui::presets`](super::Gui) holds these) and the
/// wire type (`UiConfig.presets` persists them verbatim) — the shape is flat
/// enough that a separate config mirror would only be a copy to drift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PresetConfig {
    pub name: String,
    pub pane_count: usize,
    /// Per-pane product and tilt, index-aligned with the layout.
    pub panes: Vec<PresetPane>,
    /// The enabled-overlay set, applied to every pane.
    pub overlays: Vec<OverlayKind>,
}

impl Default for PresetConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            pane_count: 1,
            panes: Vec::new(),
            overlays: Vec::new(),
        }
    }
}

/// One pane of a preset: what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PresetPane {
    /// Tolerant of product names this build does not know, exactly as the
    /// pane configs are — one product from a newer build must not poison the
    /// whole file. See [`product_or_default`](super::config).
    #[serde(deserialize_with = "super::config::product_or_default")]
    pub product: RadarProduct,
    pub elevation: f32,
}

impl Default for PresetPane {
    fn default() -> Self {
        Self {
            product: RadarProduct::Reflectivity,
            elevation: 0.0,
        }
    }
}

/// The compiled-in presets (§1.10's three), built fresh per call — they hold
/// `String`s and `Vec`s, so a `const` table is not on offer; being a function
/// of nothing is the same guarantee. Never persisted: the user's file holds
/// only their own.
///
/// The demo named the presets and their intent; the products are this
/// codebase's own variants chosen to honour it — base tilt everywhere, since
/// a preset is a starting arrangement rather than a saved investigation.
pub(crate) fn builtin_presets() -> [PresetConfig; 3] {
    let pane = |product| PresetPane {
        product,
        elevation: 0.5,
    };
    [
        // Chasing severe convection: reflectivity beside the three velocity
        // readings of rotation, under the full severe-weather overlay set.
        PresetConfig {
            name: "Severe Wx".into(),
            pane_count: 4,
            panes: vec![
                pane(RadarProduct::Reflectivity),
                pane(RadarProduct::Velocity),
                pane(RadarProduct::StormRelativeVelocity),
                pane(RadarProduct::NormalizedRotation),
            ],
            overlays: vec![
                OverlayKind::Radar,
                OverlayKind::SpcOutlook,
                OverlayKind::SpcDiscussions,
                OverlayKind::NwsAlerts,
                OverlayKind::StormReports,
                OverlayKind::CityLabels,
                OverlayKind::ColorScale,
            ],
        },
        // Watching rain fall and add up.
        PresetConfig {
            name: "Rainfall".into(),
            pane_count: 2,
            panes: vec![
                pane(RadarProduct::PrecipitationRate),
                pane(RadarProduct::VerticallyIntegratedLiquid),
            ],
            overlays: vec![
                OverlayKind::Radar,
                OverlayKind::NwsAlerts,
                OverlayKind::CityLabels,
                OverlayKind::ColorScale,
            ],
        },
        // Flying around weather: echo tops for the vertical extent, METAR and
        // lightning for the field conditions.
        PresetConfig {
            name: "Aviation".into(),
            pane_count: 3,
            panes: vec![
                pane(RadarProduct::Reflectivity),
                pane(RadarProduct::EchoTops),
                pane(RadarProduct::SpectrumWidth),
            ],
            overlays: vec![
                OverlayKind::Radar,
                OverlayKind::Metar,
                OverlayKind::Lightning,
                OverlayKind::CityLabels,
                OverlayKind::ColorScale,
            ],
        },
    ]
}

/// Which group a drawn tile belongs to.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogGroup {
    Presets,
    Overlays,
    Products,
    Hrrr,
}

/// One tile the catalog actually drew, as it was drawn — reported by the
/// renderer, never rebuilt by a test; see `ui_menu::DrawnMenuLeaf` for the
/// pattern.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CatalogTileProbe {
    pub group: CatalogGroup,
    pub label: String,
    pub rect: egui::Rect,
    /// The ✕ delete button beside a *user* preset tile — `None` on every
    /// other tile, built-ins included: deleting a compiled-in preset is not
    /// on offer.
    pub delete: Option<egui::Rect>,
}

/// What the catalog drew last frame.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CatalogProbe {
    /// Whether the catalog was on screen this frame.
    pub open: bool,
    /// The modal content's whole rect.
    pub rect: egui::Rect,
    /// The search field.
    pub search: egui::Rect,
    /// The ✕ close button.
    pub close: egui::Rect,
    /// The "Save current view…" tile — [`egui::Rect::NOTHING`] while the
    /// search hides it.
    pub save_tile: egui::Rect,
    /// The inline name field and Save button, while the save editor is open.
    pub save_field: Option<egui::Rect>,
    pub save_button: Option<egui::Rect>,
    /// Every tile drawn, in draw order.
    pub tiles: Vec<CatalogTileProbe>,
}

#[cfg(test)]
impl Default for CatalogProbe {
    fn default() -> Self {
        Self {
            open: false,
            rect: egui::Rect::NOTHING,
            search: egui::Rect::NOTHING,
            close: egui::Rect::NOTHING,
            save_tile: egui::Rect::NOTHING,
            save_field: None,
            save_button: None,
            tiles: Vec::new(),
        }
    }
}

/// Whether `label` survives the filter `query` — case-insensitive substring,
/// the same reading a user gives a search box.
fn matches_query(query: &str, label: &str) -> bool {
    query.is_empty() || label.to_lowercase().contains(query)
}

/// Queue a fetch for `kind` unless this frame already queued one — the
/// handlers are global, so one fetch serves every pane that asked.
fn push_overlay_fetch_once(actions: &mut Vec<GuiAction>, kind: OverlayKind, pane_idx: usize) {
    if !actions
        .iter()
        .any(|a| matches!(a, GuiAction::FetchOverlay { kind: k, .. } if *k == kind))
    {
        actions.push(GuiAction::FetchOverlay { kind, pane_idx });
    }
}

impl super::Gui {
    /// Draw the catalog, when it is open.
    ///
    /// Runs from [`Gui::ui`](super::Gui::ui) after the pane loop and the
    /// appliers — see the module note for why that ordering is load-bearing.
    pub(super) fn render_catalog(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
        if !self.catalog_open {
            return;
        }

        #[cfg(test)]
        let mut probe = CatalogProbe {
            open: true,
            ..CatalogProbe::default()
        };

        let width = self.layout.dialog_width(CATALOG_WIDTH);
        let max_body = (self.layout.content_rect.height() - HEADER_ALLOWANCE).max(120.0);

        let modal = egui::Modal::new(egui::Id::new("add_layer_catalog")).show(ctx, |ui| {
            ui.set_width(width);

            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let close = ui.button(CLOSE_LABEL).on_hover_text("Close the catalog");
                    #[cfg(test)]
                    {
                        probe.close = close.rect;
                    }
                    if close.clicked() {
                        self.catalog_open = false;
                    }

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("Add layer").strong());
                        let search = ui.add_sized(
                            egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                            egui::TextEdit::singleline(&mut self.catalog_query)
                                .id_salt("catalog_search")
                                .hint_text("Search"),
                        );
                        #[cfg(test)]
                        {
                            probe.search = search.rect;
                        }
                        #[cfg(not(test))]
                        let _ = search;
                    });
                });
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("catalog_scroll")
                .max_height(max_body)
                .show(ui, |ui| {
                    self.render_catalog_groups(
                        ui,
                        actions,
                        #[cfg(test)]
                        &mut probe,
                    );
                });
        });

        if modal.backdrop_response.clicked() {
            // The backdrop half of the dismissal contract; Escape goes
            // through `dismiss_top_layer`, which the frontend resolves
            // outside egui's own handling.
            self.catalog_open = false;
        }

        #[cfg(test)]
        {
            probe.rect = modal.response.rect;
            self.last_catalog = probe;
        }
        #[cfg(not(test))]
        let _ = modal;
    }

    /// The four groups, filtered by the search. A group whose every tile the
    /// filter removed draws nothing at all — heading included — so a search
    /// result is results and only results.
    fn render_catalog_groups(
        &mut self,
        ui: &mut egui::Ui,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut CatalogProbe,
    ) {
        let query = self.catalog_query.trim().to_lowercase();

        self.render_preset_group(
            ui,
            &query,
            actions,
            #[cfg(test)]
            probe,
        );

        // -- Overlays --
        let overlays: Vec<OverlayKind> = OverlayKind::all()
            .iter()
            .copied()
            .filter(|&kind| matches_query(&query, self.overlays.display_name(kind)))
            .collect();
        if !overlays.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("Overlays ({})", overlays.len())).strong(),
            );
            ui.horizontal_wrapped(|ui| {
                for kind in overlays {
                    let name = self.overlays.display_name(kind).to_owned();
                    let tile = ui.button(name.as_str());
                    #[cfg(test)]
                    probe.tiles.push(CatalogTileProbe {
                        group: CatalogGroup::Overlays,
                        label: name.clone(),
                        rect: tile.rect,
                        delete: None,
                    });
                    if tile.clicked() {
                        self.catalog_apply_overlay(kind, actions);
                        self.catalog_open = false;
                    }
                }
            });
        }

        // -- Radar products --
        let products: Vec<RadarProduct> = RadarProduct::all()
            .iter()
            .copied()
            .filter(|p| matches_query(&query, p.name()))
            .collect();
        if !products.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("Radar products ({})", products.len())).strong(),
            );
            ui.horizontal_wrapped(|ui| {
                for product in products {
                    let tile = ui.button(product.name());
                    #[cfg(test)]
                    probe.tiles.push(CatalogTileProbe {
                        group: CatalogGroup::Products,
                        label: product.name().to_owned(),
                        rect: tile.rect,
                        delete: None,
                    });
                    if tile.clicked() {
                        self.catalog_apply_product(product);
                        self.catalog_open = false;
                    }
                }
            });
        }

        // -- HRRR parameters --
        let params: Vec<ModelParameter> = ModelParameter::all()
            .iter()
            .copied()
            .filter(|p| matches_query(&query, p.display_name()))
            .collect();
        if !params.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("HRRR parameters ({})", params.len())).strong(),
            );
            ui.horizontal_wrapped(|ui| {
                for param in params {
                    let tile = ui.button(param.display_name());
                    #[cfg(test)]
                    probe.tiles.push(CatalogTileProbe {
                        group: CatalogGroup::Hrrr,
                        label: param.display_name().to_owned(),
                        rect: tile.rect,
                        delete: None,
                    });
                    if tile.clicked() {
                        self.catalog_apply_hrrr(param, actions);
                        self.catalog_open = false;
                    }
                }
            });
        }
    }

    /// The Presets group: built-in tiles, the user's tiles with their ✕, and
    /// the save tile with its inline name editor.
    fn render_preset_group(
        &mut self,
        ui: &mut egui::Ui,
        query: &str,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut CatalogProbe,
    ) {
        let builtins = builtin_presets();
        let shown_builtin: Vec<&PresetConfig> = builtins
            .iter()
            .filter(|p| matches_query(query, &p.name))
            .collect();
        let shown_user: Vec<usize> = (0..self.presets.len())
            .filter(|&i| matches_query(query, &self.presets[i].name))
            .collect();
        // The save tile shows only on the unfiltered view — see
        // [`SAVE_TILE_LABEL`].
        let offer_save = query.is_empty();
        if shown_builtin.is_empty() && shown_user.is_empty() && !offer_save {
            return;
        }

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "Presets ({})",
                shown_builtin.len() + shown_user.len()
            ))
            .strong(),
        );

        let mut apply: Option<PresetConfig> = None;
        let mut delete: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            for preset in shown_builtin {
                let tile = ui
                    .button(preset.name.as_str())
                    .on_hover_text(preset_hover(preset));
                #[cfg(test)]
                probe.tiles.push(CatalogTileProbe {
                    group: CatalogGroup::Presets,
                    label: preset.name.clone(),
                    rect: tile.rect,
                    delete: None,
                });
                if tile.clicked() {
                    apply = Some(preset.clone());
                }
            }
            for i in shown_user {
                let preset = &self.presets[i];
                let tile = ui
                    .button(preset.name.as_str())
                    .on_hover_text(preset_hover(preset));
                let remove = ui
                    .add(
                        egui::Button::new(egui::RichText::new(CLOSE_LABEL).small()).frame(false),
                    )
                    .on_hover_text(format!("Delete \u{201c}{}\u{201d}", preset.name));
                #[cfg(test)]
                probe.tiles.push(CatalogTileProbe {
                    group: CatalogGroup::Presets,
                    label: preset.name.clone(),
                    rect: tile.rect,
                    delete: Some(remove.rect),
                });
                if tile.clicked() {
                    apply = Some(preset.clone());
                }
                if remove.clicked() {
                    delete = Some(i);
                }
            }
            if offer_save {
                let save_tile = ui.button(SAVE_TILE_LABEL);
                #[cfg(test)]
                {
                    probe.save_tile = save_tile.rect;
                }
                if save_tile.clicked() {
                    self.catalog_saving = !self.catalog_saving;
                }
            }
        });

        if self.catalog_saving && offer_save {
            ui.horizontal(|ui| {
                ui.label("Name:");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.catalog_save_name)
                        .id_salt("preset_name")
                        .desired_width(160.0),
                );
                let name = self.catalog_save_name.trim().to_owned();
                let save = ui.add_enabled(!name.is_empty(), egui::Button::new("Save"));
                #[cfg(test)]
                {
                    probe.save_field = Some(field.rect);
                    probe.save_button = Some(save.rect);
                }
                #[cfg(not(test))]
                let _ = field;
                if save.clicked() {
                    let preset = self.capture_preset(name.clone());
                    // Saving under an existing user preset's name replaces
                    // it: two tiles with one name would be two buttons the
                    // user cannot tell apart.
                    if let Some(existing) =
                        self.presets.iter_mut().find(|p| p.name == name)
                    {
                        *existing = preset;
                    } else {
                        self.presets.push(preset);
                    }
                    self.catalog_saving = false;
                    self.catalog_save_name.clear();
                }
            });
        }

        if let Some(i) = delete {
            self.presets.remove(i);
        }
        if let Some(preset) = apply {
            self.apply_preset(&preset, actions);
            self.catalog_open = false;
        }
    }

    /// Enable `kind` on the active pane and select it in the inspector —
    /// what clicking an overlay tile means.
    fn catalog_apply_overlay(&mut self, kind: OverlayKind, actions: &mut Vec<GuiAction>) {
        let idx = self.active_pane;
        let mut pane = std::mem::take(&mut self.panes[idx]);
        self.set_pane_overlay_with_fetch(&mut pane, idx, kind, true, actions);
        self.panes[idx] = pane;
        self.propagate_layer_sync();
        self.select_layer(kind);
    }

    /// Aim the active pane at `product` — converting it back to a map if it
    /// is not one — and select the Radar layer.
    fn catalog_apply_product(&mut self, product: RadarProduct) {
        let idx = self.active_pane;
        if !self.panes[idx].is_map() {
            // Through the deferred applier like every other kind writer: the
            // direct write would be safe *here*, but one rule for all of them
            // costs only a frame.
            self.request_pane_kind(idx, crate::pane::PaneKind::Map);
        }
        // A product tile means "show me this picture", so the Radar layer
        // turns on with it — a product under a hidden radar layer is a click
        // that visibly did nothing. No fetch rule: radar data arrives through
        // the scan path, not `FetchOverlay`.
        Self::write_pane_overlay(
            &mut self.overlays,
            &mut self.panes[idx],
            OverlayKind::Radar,
            true,
        );
        let pane = &mut self.panes[idx];
        if pane.selected_product != product {
            pane.selected_product = product;
            // The tilt belonged to the old product — the same reset the
            // inspector's product combo makes.
            pane.selected_elevation = 0.0;
        }
        self.propagate_layer_sync();
        self.select_layer(OverlayKind::Radar);
    }

    /// Enable the model layer, set its parameter through the handler's own
    /// control route, and select the layer — what clicking an HRRR tile
    /// means.
    fn catalog_apply_hrrr(&mut self, param: ModelParameter, actions: &mut Vec<GuiAction>) {
        let idx = self.active_pane;
        let mut pane = std::mem::take(&mut self.panes[idx]);
        self.set_pane_overlay_with_fetch(&mut pane, idx, OverlayKind::ModelData, true, actions);

        // Through `apply_control` rather than a field write, so the handler's
        // own rules hold: a cached parameter re-renders without a fetch, an
        // uncached one asks for one.
        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }
        let update = ControlUpdate {
            id: "parameter",
            value: ControlValue::String(param.as_str().to_owned()),
        };
        let mut pane_ctx = PaneControlContextMut {
            pane_idx: idx,
            pane_state: None,
        };
        let effect = self
            .overlays
            .apply_control(OverlayKind::ModelData, &update, &mut pane_ctx);
        if matches!(effect, ControlEffect::Fetch) {
            push_overlay_fetch_once(actions, OverlayKind::ModelData, idx);
        }
        pane.overlay_configs = self.overlays.save_pane_configs();
        pane.enabled_overlays = self.overlays.save_enabled_map();

        self.panes[idx] = pane;
        self.propagate_layer_sync();
        self.select_layer(OverlayKind::ModelData);
    }

    /// The current view as a preset: pane count, each visible pane's product
    /// and tilt, and the **active** pane's enabled-overlay set (§3.11 — the
    /// active pane is the one whose layers the user has been arranging).
    fn capture_preset(&self, name: String) -> PresetConfig {
        let finite = |e: f32| if e.is_finite() { e } else { 0.0 };
        let active = self.active_pane();
        PresetConfig {
            name,
            pane_count: self.pane_layout.pane_count,
            panes: self
                .panes()
                .iter()
                .map(|pane| PresetPane {
                    product: pane.selected_product,
                    elevation: finite(pane.selected_elevation),
                })
                .collect(),
            overlays: OverlayKind::all()
                .iter()
                .copied()
                .filter(|&kind| active.is_overlay_enabled(kind))
                .collect(),
        }
    }

    /// Rebuild the layout from `preset`: pane count, per-pane product and
    /// tilt with every pane a map again, and the overlay set on each pane.
    ///
    /// Runs outside every `mem::take` window (the module note), so the kind
    /// writes are direct on the same terms as `apply_pending_region`'s. A
    /// volume pane converted away releases its voxel grid exactly as the
    /// deferred applier would have.
    fn apply_preset(&mut self, preset: &PresetConfig, actions: &mut Vec<GuiAction>) {
        let count = preset.pane_count.clamp(1, self.layout.width.max_panes());
        let _ = self.set_pane_count(count);
        let count = self.pane_layout.pane_count;

        for idx in 0..count {
            if self.panes[idx].kind() == crate::pane::PaneKind::Volume {
                actions.push(GuiAction::ReleaseVolume { pane_idx: idx });
            }
            let pane = &mut self.panes[idx];
            pane.set_kind(crate::pane::PaneKind::Map);
            if let Some(pp) = preset.panes.get(idx) {
                pane.selected_product = pp.product;
                pane.selected_elevation = pp.elevation;
            }
        }

        // The overlay set, pane by pane through the shared helper — both
        // halves of every flip, and at most one fetch per newly-enabled kind
        // (the helper's dedupe).
        for idx in 0..count {
            let mut pane = std::mem::take(&mut self.panes[idx]);
            for &kind in OverlayKind::all() {
                let on = preset.overlays.contains(&kind);
                self.set_pane_overlay_with_fetch(&mut pane, idx, kind, on, actions);
            }
            self.panes[idx] = pane;
        }

        self.propagate_layer_sync();
    }
}

/// The sentence a preset tile offers on hover: what applying it builds.
fn preset_hover(preset: &PresetConfig) -> String {
    let products: Vec<&str> = preset
        .panes
        .iter()
        .map(|pane| pane.product.name())
        .collect();
    format!(
        "{} pane{}: {}",
        preset.pane_count,
        if preset.pane_count == 1 { "" } else { "s" },
        products.join(" \u{b7} ")
    )
}
