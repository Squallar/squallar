//! The Add-layer catalog: one modal over everything, four groups, one search.

use crate::actions::GuiAction;
use rustdar_overlays::hrrr::ModelParameter;
use rustdar_overlays::render::controls::{
    ControlEffect, ControlUpdate, ControlValue, PaneControlContextMut,
};
use rustdar_radar::types::RadarProduct;
use rustdar_source::id::{LayerId, known};
use serde::{Deserialize, Serialize};

/// The catalog's roomy width, narrowed by
/// [`LayoutCtx::dialog_width`](crate::ui_layout::LayoutCtx) on a screen that
/// cannot afford it.
const CATALOG_WIDTH: f32 = 520.0;

/// What the header and its separator cost over the scroll body, plus the
/// modal's own margins — charged against the body's ceiling so the whole
/// modal stays inside the content rect.
const HEADER_ALLOWANCE: f32 = 160.0;

const CLOSE_LABEL: &str = "\u{d7}";

/// The save tile's label. Drawn only while the search box is empty: the
/// search is for *finding* tiles, and a save offer matching the query "save"
/// would be the one tile that is not a result.
const SAVE_TILE_LABEL: &str = "+ Save current view...";

/// One saved multi-pane setup (plan §3.11): how many panes, what each shows,
/// and which overlays the layout runs with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PresetConfig {
    pub name: String,
    pub pane_count: usize,
    pub panes: Vec<PresetPane>,
    /// The enabled-overlay set, applied to every pane. A
    /// [`KindList`](super::config::KindList): the names are open [`LayerId`]s,
    /// so one from a newer build rides through save; only ids the registry
    /// serves are applied.
    pub overlays: super::config::KindList,
}

impl Default for PresetConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            pane_count: 1,
            panes: Vec::new(),
            overlays: super::config::KindList::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PresetPane {
    /// Tolerant of product names this build does not know. See
    /// [`product_or_default`](super::config).
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
/// `String`s and `Vec`s, so a `const` table is not on offer. Never persisted.
pub(crate) fn builtin_presets() -> [PresetConfig; 3] {
    let pane = |product| PresetPane {
        product,
        elevation: 0.5,
    };
    [
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
                known::RADAR,
                known::SPC_OUTLOOK,
                known::SPC_DISCUSSIONS,
                known::NWS_ALERTS,
                known::STORM_REPORTS,
                known::CITY_LABELS,
                known::COLOR_SCALE,
            ]
            .into(),
        },
        PresetConfig {
            name: "Rainfall".into(),
            pane_count: 2,
            panes: vec![
                pane(RadarProduct::PrecipitationRate),
                pane(RadarProduct::VerticallyIntegratedLiquid),
            ],
            overlays: vec![
                known::RADAR,
                known::NWS_ALERTS,
                known::CITY_LABELS,
                known::COLOR_SCALE,
            ]
            .into(),
        },
        PresetConfig {
            name: "Aviation".into(),
            pane_count: 3,
            panes: vec![
                pane(RadarProduct::Reflectivity),
                pane(RadarProduct::EchoTops),
                pane(RadarProduct::SpectrumWidth),
            ],
            overlays: vec![
                known::RADAR,
                known::METAR,
                known::LIGHTNING,
                known::CITY_LABELS,
                known::COLOR_SCALE,
            ]
            .into(),
        },
    ]
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogGroup {
    Presets,
    Overlays,
    Products,
    Hrrr,
}

/// One tile the catalog actually drew, as it was drawn — reported by the
/// renderer, never rebuilt by a test.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CatalogTileProbe {
    pub group: CatalogGroup,
    pub label: String,
    pub rect: egui::Rect,
    pub delete: Option<egui::Rect>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CatalogProbe {
    pub open: bool,
    pub rect: egui::Rect,
    pub search: egui::Rect,
    pub close: egui::Rect,
    pub save_tile: egui::Rect,
    pub save_field: Option<egui::Rect>,
    pub save_button: Option<egui::Rect>,
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

impl super::Gui {
    /// Draw the catalog, when it is open — as the centred modal the two wide
    /// widths get. On Compact the sheet's Catalog page hosts the same body.
    pub(super) fn render_catalog(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
        if !self.catalog_open || self.layout.width == crate::ui_layout::WidthClass::Compact {
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
            self.render_catalog_body(
                ui,
                max_body,
                actions,
                #[cfg(test)]
                &mut probe,
            );
        });

        if modal.backdrop_response.clicked() {
            self.catalog_open = false;
        }

        #[cfg(test)]
        {
            probe.rect = modal.response.rect;
            self.probes.last_catalog = probe;
        }
        #[cfg(not(test))]
        let _ = modal;
    }

    /// The catalog's content, host-free: header (title, search, ✕), then the
    /// scrolling groups, shared by the modal and the sheet's Catalog page.
    pub(super) fn render_catalog_body(
        &mut self,
        ui: &mut egui::Ui,
        max_body: f32,
        actions: &mut Vec<GuiAction>,
        #[cfg(test)] probe: &mut CatalogProbe,
    ) {
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
                    // A deliberate exception to the id doctrine, shared with
                    // `catalog_scroll` below and `sheet_feature_scroll`
                    // (`ui_sheet.rs`): the salts are stable, but the parent
                    // layer is the modal above 600 pt and the phone sheet
                    // below it, so the ids resolve differently either side and
                    // egui-side state does not carry across the breakpoint.
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
            .scroll_source(super::shell::panel_scroll_source())
            .id_salt("catalog_scroll")
            .max_height(max_body)
            .show(ui, |ui| {
                self.render_catalog_groups(
                    ui,
                    actions,
                    #[cfg(test)]
                    probe,
                );
            });
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
        let overlays: Vec<LayerId> = self
            .overlays
            .default_draw_order()
            .into_iter()
            .filter(|kind| matches_query(&query, self.overlays.display_name(kind)))
            .collect();
        if !overlays.is_empty() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(format!("Overlays ({})", overlays.len())).strong());
            ui.horizontal_wrapped(|ui| {
                for kind in overlays {
                    let name = self.overlays.display_name(&kind).to_owned();
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
            ui.label(egui::RichText::new(format!("Radar products ({})", products.len())).strong());
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
            ui.label(egui::RichText::new(format!("HRRR parameters ({})", params.len())).strong());
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
                    .add(egui::Button::new(egui::RichText::new(CLOSE_LABEL).small()).frame(false))
                    .on_hover_text(format!("Delete \"{}\"", preset.name));
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
            // A name a built-in already owns is refused, with the reason
            // inline: a user preset named "Severe Wx" would put two
            // identical tiles on screen with only one deletable.
            // Case-insensitive, because "severe wx" would too.
            let name = self.catalog_save_name.trim().to_owned();
            let shadows_builtin = builtin_presets()
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&name));
            ui.horizontal(|ui| {
                ui.label("Name:");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.catalog_save_name)
                        .id_salt("preset_name")
                        .desired_width(160.0),
                );
                let save = ui.add_enabled(
                    !name.is_empty() && !shadows_builtin,
                    egui::Button::new("Save"),
                );
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
                    // it: two tiles with one name would be two buttons the user
                    // cannot tell apart. Case-insensitive; the replacement takes
                    // the newly typed casing. Built-in names never get this far.
                    if let Some(existing) = self
                        .presets
                        .iter_mut()
                        .find(|p| p.name.eq_ignore_ascii_case(&name))
                    {
                        *existing = preset;
                    } else {
                        self.presets.push(preset);
                    }
                    self.catalog_saving = false;
                    self.catalog_save_name.clear();
                }
            });
            if shadows_builtin {
                ui.label(
                    egui::RichText::new(format!(
                        "\"{name}\" is a built-in preset - pick another name"
                    ))
                    .small()
                    .weak(),
                );
            }
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
    fn catalog_apply_overlay(&mut self, kind: LayerId, actions: &mut Vec<GuiAction>) {
        let idx = self.active_pane;
        let mut pane = std::mem::take(&mut self.panes[idx]);
        self.set_pane_overlay_with_fetch(&mut pane, idx, &kind, true, actions);
        self.panes[idx] = pane;
        self.propagate_layer_sync();
        self.select_layer(kind);
    }

    /// Aim the active pane at `product` — converting it back to a map if it
    /// is not one — and select the Radar layer.
    fn catalog_apply_product(&mut self, product: RadarProduct) {
        let idx = self.active_pane;
        if !self.panes[idx].is_map() {
            self.request_pane_view(idx, rustdar_radar::types::RenderView::PlanView);
        }
        // A product tile means "show me this picture", so the Radar layer
        // turns on with it — a product under a hidden radar layer is a click
        // that visibly did nothing. No fetch rule: radar data arrives through
        // the scan path, not `FetchOverlay`.
        Self::write_pane_overlay(
            &mut self.overlays,
            &mut self.panes[idx],
            &known::RADAR,
            true,
        );
        let pane = &mut self.panes[idx];
        if pane.selected_product() != product {
            pane.set_selected_product(product);
            pane.set_selected_elevation(0.0);
        }
        self.propagate_layer_sync();
        self.select_layer(known::RADAR);
    }

    /// Enable the model layer, set its parameter through the handler's own
    /// control route, and select the layer — what clicking an HRRR tile
    /// means.
    fn catalog_apply_hrrr(&mut self, param: ModelParameter, actions: &mut Vec<GuiAction>) {
        let idx = self.active_pane;
        let mut pane = std::mem::take(&mut self.panes[idx]);
        self.set_pane_overlay_with_fetch(&mut pane, idx, &known::MODEL_DATA, true, actions);

        // Through `apply_control` rather than a field write, so the handler's
        // own rules hold: a cached parameter re-renders without a fetch, an
        // uncached one asks for one.
        if pane.has_slot_configs() {
            self.overlays.load_pane_configs(&pane.slot_config_map());
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
            .apply_control(&known::MODEL_DATA, &update, &mut pane_ctx);
        if matches!(effect, ControlEffect::Fetch) {
            crate::ui::push_user_overlay_fetch(&mut self.overlays, actions, known::MODEL_DATA, idx);
        }
        pane.adopt_handler_state(&self.overlays);

        self.panes[idx] = pane;
        self.propagate_layer_sync();
        self.select_layer(known::MODEL_DATA);
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
                    product: pane.selected_product(),
                    elevation: finite(pane.selected_elevation()),
                })
                .collect(),
            overlays: self
                .overlays
                .default_draw_order()
                .into_iter()
                .filter(|kind| active.is_overlay_enabled(kind))
                .collect::<Vec<_>>()
                .into(),
        }
    }

    /// Rebuild the layout from `preset`: pane count, per-pane product and
    /// tilt with every pane a map again, and the overlay set on each pane.
    fn apply_preset(&mut self, preset: &PresetConfig, actions: &mut Vec<GuiAction>) {
        let count = preset.pane_count.clamp(1, self.layout.width.max_panes());
        let _ = self.set_pane_count(count);
        let count = self.pane_layout.pane_count;

        for idx in 0..count {
            if self.panes[idx].render_view() == rustdar_radar::types::RenderView::Volume {
                actions.push(GuiAction::ReleaseVolume { pane_idx: idx });
            }
            let pane = &mut self.panes[idx];
            pane.set_view(rustdar_radar::types::RenderView::PlanView);
            if let Some(pp) = preset.panes.get(idx) {
                pane.set_selected_product(pp.product);
                pane.set_selected_elevation(pp.elevation);
            }
        }

        for idx in 0..count {
            let mut pane = std::mem::take(&mut self.panes[idx]);
            for kind in self.overlays.default_draw_order() {
                let on = preset.overlays.known.contains(&kind);
                self.set_pane_overlay_with_fetch(&mut pane, idx, &kind, on, actions);
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
        products.join(" - ")
    )
}
