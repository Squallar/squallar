//! The layer catalog: one modal over everything, four groups, one search.

use crate::actions::GuiAction;
use serde::{Deserialize, Serialize};
use squallar_overlays::render::controls::{ControlEffect, ControlUpdate, ControlValue};
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::handler::PaneMut;
use squallar_source::id::{LayerId, known};
use squallar_source::product::{FieldId, ProductSpec};

/// The catalog's roomy width, narrowed by
/// [`LayoutCtx::dialog_width`](crate::ui_layout::LayoutCtx) on a screen that
/// cannot afford it.
const CATALOG_WIDTH: f32 = 520.0;

/// What the header and its separator cost over the scroll body, plus the
/// modal's own margins — charged against the body's ceiling so the whole
/// modal stays inside the content rect.
const HEADER_ALLOWANCE: f32 = 160.0;

const CLOSE_LABEL: &str = "\u{d7}";

/// The modal's heading, and the sheet page's title — one string, so the two
/// hosts cannot come to say different things.
///
/// **"Add layer" again, and now the word is earned.** W21 renamed this to
/// "Show a layer" because a pane's stack held a row for every registered layer,
/// so a tile could only turn one on. A stack is curated now, a tile for a layer
/// the pane does not hold really does insert a row at its draw-order weight,
/// and the heading says so.
pub(crate) const CATALOG_HEADING: &str = "Add layer";

/// The saved-preset delete control's label.
const DELETE_LABEL: &str = "Delete";

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PresetPane {
    /// The field this pane shows, as an open id.
    ///
    /// **No tolerant deserializer, because none is needed any more.** `FieldId`
    /// is serde-transparent, so *any* spelling loads — including one this build
    /// does not register. Such an entry is preserved inert: it is written back
    /// verbatim on the next save and simply does not resolve when the preset is
    /// applied, so a preset authored on a newer build survives a session on an
    /// older one instead of being silently rewritten to Reflectivity. That is
    /// the open-id doctrine replacing `product_or_default`'s substitution.
    pub product: FieldId,
    pub elevation: f32,
}

impl Default for PresetPane {
    fn default() -> Self {
        Self {
            // The on-disk spelling, not a derivation: this is the byte string
            // already in every preset file. `the_builtin_presets_name_registered_fields`
            // is what stops it from silently becoming an id nothing registers.
            product: FieldId::from_static("Reflectivity"),
            elevation: 0.0,
        }
    }
}

/// The compiled-in presets (§1.10's three), built fresh per call — they hold
/// `String`s and `Vec`s, so a `const` table is not on offer. Never persisted.
pub(crate) fn builtin_presets() -> [PresetConfig; 3] {
    let pane = |product: &'static str| PresetPane {
        product: FieldId::from_static(product),
        elevation: 0.5,
    };
    [
        PresetConfig {
            name: "Severe Wx".into(),
            pane_count: 4,
            panes: vec![
                pane("Reflectivity"),
                pane("Velocity"),
                pane("StormRelativeVelocity"),
                pane("NormalizedRotation"),
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
                pane("PrecipitationRate"),
                pane("VerticallyIntegratedLiquid"),
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
                pane("Reflectivity"),
                pane("EchoTops"),
                pane("SpectrumWidth"),
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

/// Which heading a catalogue tile was drawn under.
///
/// **`Fields` carries the group label as data rather than naming it as a
/// variant.** The label comes from the field's own `ProductSpec::group`, so a
/// source that brings a new group of fields gets its own heading with no arm
/// added anywhere in this crate — which is the whole point of the registry
/// being a read contract. The closed `{Presets, Overlays, Products, Hrrr}` set
/// this replaces could not express a group it had not been edited to know.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogGroup {
    Presets,
    /// One tile per registered layer.
    Layers,
    /// One tile per registered field, under its registration's group label.
    Fields(&'static str),
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
                    ui.label(egui::RichText::new(CATALOG_HEADING).strong());
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
                    self.focus_search_on_open(ui, super::state::SearchField::Catalog, &search);
                    #[cfg(test)]
                    {
                        probe.search = search.rect;
                    }
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
                // **Every registered layer, held or not, and the two are told
                // apart on the tile rather than by omission.**
                //
                // Filtering the held ones out is the obvious reading of "offer
                // what is not in the stack", and it is the wrong one here for a
                // reason worth writing down: this list is the *catalogue*, the
                // one surface that answers "what can this build draw", and the
                // parity walk's anti-shrink floor
                // (`REGISTERED_LAYER_COUNT`) is what stops a composition that
                // quietly lost a source crate from being met by a catalogue
                // that quietly lost its tile. A list whose length is a function
                // of the active pane's curation cannot carry that floor. So the
                // inventory stays complete — which is also what keeps "a new
                // source lights up the catalogue" unconditional — and a held
                // layer's tile is drawn weak and says so on hover.
                let held: Vec<LayerId> = self.active_pane().draw_order_vec();
                for kind in overlays {
                    let name = self.overlays.display_name(&kind).to_owned();
                    let in_stack = held.contains(&kind);
                    let tile = ui
                        .button(if in_stack {
                            egui::RichText::new(name.as_str()).weak()
                        } else {
                            egui::RichText::new(name.as_str())
                        })
                        .on_hover_text(if in_stack {
                            format!("{name} is already in this pane - show it")
                        } else {
                            format!("Add {name} to this pane")
                        });
                    #[cfg(test)]
                    probe.tiles.push(CatalogTileProbe {
                        group: CatalogGroup::Layers,
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

        // -- Fields, one heading per group label the registrations declare --
        //
        // ONE loop, not one per source. The headings, their order and their
        // contents all come from `OverlayRegistry::fields()`, so a source that
        // registers a new group of fields appears here with no edit to this
        // crate. Registry order is the drawn order: WO-E9d's rule is that the
        // catalogue does not invent an ordering of its own.
        let mut groups: Vec<&'static str> = Vec::new();
        for (_, spec) in self.overlays.fields() {
            if !groups.contains(&spec.group) {
                groups.push(spec.group);
            }
        }
        for group in groups {
            let hits: Vec<(LayerId, &'static ProductSpec)> = self
                .overlays
                .fields()
                .filter(|(_, spec)| spec.group == group && matches_query(&query, spec.name))
                .collect();
            if hits.is_empty() {
                continue;
            }
            ui.add_space(6.0);
            ui.label(egui::RichText::new(format!("{group} ({})", hits.len())).strong());
            ui.horizontal_wrapped(|ui| {
                for (owner, spec) in hits {
                    let tile = ui.button(spec.name);
                    #[cfg(test)]
                    probe.tiles.push(CatalogTileProbe {
                        group: CatalogGroup::Fields(group),
                        label: spec.name.to_owned(),
                        rect: tile.rect,
                        delete: None,
                    });
                    if tile.clicked() {
                        self.catalog_apply_field(&owner, spec, actions);
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
                    .on_hover_text(preset_hover(&self.overlays, preset));
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
                    .on_hover_text(preset_hover(&self.overlays, preset));
                // A word, not a `\u{d7}`: this deletes the user's saved preset,
                // and `\u{d7}` is the app's close glyph and nothing else
                // (`ui_glyphs.rs`). A destructive control wearing the dismissal
                // glyph is the same defect the inspector's crumb carried.
                let remove = ui
                    .add(egui::Button::new(egui::RichText::new(DELETE_LABEL).small()).frame(false))
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

    /// **Put `kind` in the active pane's stack**, show it, and select it in the
    /// inspector — what clicking an overlay tile means.
    ///
    /// The add is real: a layer the pane does not hold gains a slot at its own
    /// `draw_order_weight`, carrying whatever configuration it held the last
    /// time it was removed from this pane, and its tombstone is cleared so no
    /// later reconcile takes it away again. A layer the pane already holds is
    /// left exactly where the user put it and is only switched on — adding
    /// twice must not silently reorder a stack.
    ///
    /// Either way the tile scrolls the row into view: the modal closes on
    /// click, and without it the only visible result is a row somewhere in a
    /// list the user may have to hunt through.
    fn catalog_apply_overlay(&mut self, kind: LayerId, actions: &mut Vec<GuiAction>) {
        let idx = self.active_pane;
        let mut pane = std::mem::take(&mut self.panes[idx]);
        pane.add_layer(&self.overlays, &kind);
        self.set_pane_overlay_with_fetch(&mut pane, idx, &kind, true, actions);
        self.panes[idx] = pane;
        self.propagate_pane_sync();
        self.stack_scroll_to = Some(kind.clone());
        self.select_layer(kind);
    }

    /// Apply one field tile: turn the owning layer on, then select the field
    /// through **that layer's own route**, and select the layer.
    ///
    /// ONE apply for every source. The branch below is not on *which layer this
    /// is* — no `known::RADAR` or `known::MODEL_DATA` decides anything here —
    /// but on what the handler itself declares through
    /// [`SourceHandler::field_control_id`]: a layer whose field selection is a
    /// control of its own gets a `ControlUpdate`, and a layer whose pane owns
    /// the selection gets the pane route. A new source picks its arm by
    /// answering that one question, not by being added to a match.
    fn catalog_apply_field(
        &mut self,
        owner: &LayerId,
        spec: &'static ProductSpec,
        actions: &mut Vec<GuiAction>,
    ) {
        let idx = self.active_pane;
        // A field tile means "show me this picture", so the owning layer turns
        // on with it — a field under a hidden layer is a click that visibly did
        // nothing.
        let control = self
            .overlays
            .get_handler(owner)
            .and_then(|h| h.field_control_id());

        match control {
            Some(control_id) => {
                let mut pane = std::mem::take(&mut self.panes[idx]);
                // The stack is curated, so "turn the owning layer on" is two
                // acts now: put it in the pane if it is not there, then show
                // it. A field tile for a layer the user removed must bring the
                // layer back with it, or the click draws nothing.
                pane.add_layer(&self.overlays, owner);
                self.set_pane_overlay_with_fetch(&mut pane, idx, owner, true, actions);

                // Through `apply_control` rather than a field write, so the
                // handler's own rules hold: a cached field re-renders without a
                // fetch, an uncached one asks for one.
                pane.hydrate_layer_states(&self.overlays, idx);
                let update = ControlUpdate {
                    id: control_id,
                    value: ControlValue::String(spec.id.as_str().to_owned()),
                };
                // The other panes' state for this layer — see
                // `render_overlay_controls_one`, which builds the same view for
                // the same reason. `self.panes[idx]` is the `mem::take`n
                // placeholder while `pane` is out, so the edited pane cannot
                // appear twice.
                let peers: Vec<&dyn std::any::Any> = self
                    .panes
                    .iter()
                    .take(self.pane_layout.pane_count)
                    .filter_map(|p| p.slot(owner))
                    .filter_map(|slot| slot.state.as_deref())
                    .map(|s| s as &dyn std::any::Any)
                    .collect();
                let mut pane_ctx = PaneMut {
                    pane_idx: idx,
                    state: pane
                        .slot_mut(owner)
                        .and_then(|slot| slot.state.as_deref_mut())
                        .map(|s| s as &mut dyn std::any::Any),
                    peers: &peers,
                };
                let effect = self.overlays.apply_control(owner, &update, &mut pane_ctx);
                if matches!(effect, ControlEffect::Fetch) {
                    crate::ui::push_user_overlay_fetch(
                        &mut self.overlays,
                        actions,
                        owner.clone(),
                        idx,
                    );
                }
                pane.adopt_handler_state(&self.overlays);
                self.panes[idx] = pane;
            }
            None => {
                // The pane owns this layer's field selection, and since
                // WO-E9e it owns it as a `FieldId`, so the registry's own id
                // is written straight through with no source type named.
                if !self.panes[idx].is_map() {
                    self.request_pane_view(idx, squallar_radar::types::RenderView::PlanView);
                }
                let Self {
                    overlays, panes, ..
                } = self;
                panes[idx].add_layer(overlays, owner);
                Self::write_pane_overlay(overlays, idx, &mut panes[idx], owner, true);
                let pane = &mut self.panes[idx];
                if pane.selected_product() != spec.id {
                    pane.set_selected_product(spec.id.clone());
                    pane.set_selected_elevation(0.0);
                }
                // No fetch rule: radar data arrives through the scan path, not
                // `FetchOverlay`.
            }
        }

        self.propagate_pane_sync();
        self.select_layer(owner.clone());
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
            if self.panes[idx].render_view() == squallar_radar::types::RenderView::Volume {
                actions.push(GuiAction::ReleaseVolume { pane_idx: idx });
            }
            let pane = &mut self.panes[idx];
            pane.set_view(squallar_radar::types::RenderView::PlanView);
            if let Some(pp) = preset.panes.get(idx) {
                // An id this build does not register resolves to nothing and
                // leaves the pane's field as it was — the preserve rule. It is
                // never substituted for a default, which would silently rewrite
                // the preset on the next save.
                if squallar_radar::fields::spec_for(&pp.product).is_some() {
                    pane.set_selected_product(pp.product.clone());
                }
                pane.set_selected_elevation(pp.elevation);
            }
        }

        for idx in 0..count {
            let mut pane = std::mem::take(&mut self.panes[idx]);
            for kind in self.overlays.default_draw_order() {
                let on = preset.overlays.known.contains(&kind);
                // A preset names the layers it wants shown, so it **adds**
                // them: applying "Aviation" to a pane the user removed METAR
                // from has to bring METAR back, or the preset silently
                // delivers less than it names.
                //
                // It does **not** remove the layers it omits, and that
                // asymmetry is on purpose. A preset is a set of layers to show,
                // not a curation: `off` has always meant "hidden here", the
                // pane keeps the row and its settings, and one preset click
                // cannot throw away a stack the user built.
                if on {
                    pane.add_layer(&self.overlays, &kind);
                }
                self.set_pane_overlay_with_fetch(&mut pane, idx, &kind, on, actions);
            }
            self.panes[idx] = pane;
        }

        self.propagate_pane_sync();
    }
}

/// The sentence a preset tile offers on hover: what applying it builds.
///
/// Field names come from the registry, so a preset naming a field this build
/// does not register shows **the id it actually holds** rather than a
/// substituted default — the hover tells the truth about the file.
fn preset_hover(registry: &OverlayRegistry, preset: &PresetConfig) -> String {
    let products: Vec<&str> = preset
        .panes
        .iter()
        .map(|pane| {
            registry
                .field(&pane.product)
                .map(|(_, spec)| spec.name)
                .unwrap_or_else(|| pane.product.as_str())
        })
        .collect();
    format!(
        "{} pane{}: {}",
        preset.pane_count,
        if preset.pane_count == 1 { "" } else { "s" },
        products.join(" - ")
    )
}
