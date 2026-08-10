//! One menu model, its renderers, one dispatcher.
//!
//! The menu used to exist twice: as a real `MenuBar` in `ui_desktop.rs`, and
//! hand-rolled again as a "Controls" block of buttons inside `ui_mobile.rs`'s
//! layers panel. The two drifted — the mobile copy had Refresh and Auto-poll,
//! the desktop one had the overlay toggles — and every new entry had to be
//! added in both places, in the right one of two files, or it silently existed
//! on only one platform.
//!
//! So the menu is described once as data ([`MenuNode`]), rendered by whichever
//! presentation is hosting it — the top bar's ☰ dropdown
//! ([`render_menu_popup`]) on the two wide widths, the phone sheet's Menu
//! page ([`render_menu_drawer`]) below the Compact breakpoint — and the
//! resulting [`MenuEvent`]s are applied in exactly one place. A new entry is
//! one line in [`super::Gui::menu_model`] and one arm in
//! [`super::Gui::apply_menu_event`], and it appears in every presentation by
//! construction.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

/// The label on the 3D-pane toggle.
///
/// A constant because two tests name it — the drawer-coverage list and the
/// end-to-end conversion — and a test that carried its own copy of the string
/// would go on passing after the entry was renamed out from under it.
pub(crate) const VOLUME_PANE_LABEL: &str = "3D volume view";

/// The label on the region-drag toggle.
///
/// Names the gesture rather than the mode ("Pick…" and where to drag, not
/// "Region mode"), because the one thing a user has to learn from it is that a
/// *drag on a map pane* is what does the picking — there is nothing on the 3D
/// pane itself to try, and that is exactly the discovery problem this feature
/// exists to fix.
pub(crate) const REGION_ARM_LABEL: &str = "Pick 3D region (drag on a map)";

/// The label on the cross-section arming toggle.
///
/// Phrased as the gesture it arms rather than as the pane it produces, because
/// the pane is not what the user does next: they draw a line. A constant for the
/// reason [`VOLUME_PANE_LABEL`] is one.
pub(crate) const DRAW_CROSS_SECTION_LABEL: &str = "Draw cross-section";

/// A command the user can invoke from the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuAction {
    Exit,
    RefreshRadar,
    OpenTimeDialog,
    OpenSettings,
}

/// A boolean the menu can flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuToggle {
    /// Show/hide a map overlay on the active pane.
    Overlay(OverlayKind),
    /// Automatic polling for new scans.
    AutoPoll,
    /// Feed live panes from the real-time chunk bucket rather than polling the
    /// archive for completed volumes.
    LiveChunks,
    /// Subscribe to the push-notification service so a chunk is fetched the
    /// moment it exists rather than on the next poll.
    ChunkNotifications,
    /// Make the active pane a 3D volume view, or turn it back into a map.
    ///
    /// A checkbox rather than a command, and per pane rather than global,
    /// because that is what it is: the pane either is a volume view or it is
    /// not, and the state has to be visible or a user who converted a pane by
    /// accident has nothing to un-tick. Unticking it returns the pane to a map,
    /// which is also the only route out of a section pane restored from a config
    /// — two clicks rather than one, but never a trap.
    ///
    /// The companion entry, [`MenuToggle::DrawCrossSection`], is deliberately
    /// *not* the same shape: it arms a gesture rather than converting the pane
    /// under the cursor, because which pane a section lands in is decided by
    /// where the line is drawn.
    VolumePane,
    /// Arm the region drag: while it is on, a drag on a **map** pane draws the
    /// patch of ground a 3D pane resamples instead of panning the map.
    ///
    /// A checkbox rather than a command for the same reason `VolumePane` is one:
    /// it is a mode, it changes what dragging does, and a mode a user cannot see
    /// is a mouse that has stopped working. Committing a box disarms it — the
    /// checkbox un-ticks itself, see `Gui::region_arm` — while a discarded
    /// mis-drag leaves it armed, so this and a back press are the ways out of a
    /// mode that has not yet done its job.
    ///
    /// Ticking it un-ticks [`DrawCrossSection`](Self::DrawCrossSection), which is
    /// the other armed drag on a map pane: one drag cannot be two gestures. See
    /// `Gui::set_region_arm`.
    RegionArm,
    /// Arm the cross-section draw: the next drag on a map pane becomes a
    /// vertical slice instead of a pan.
    ///
    /// A checkbox rather than a command, and for a reason the volume toggle does
    /// not have. This one arms a **mode**, and the classic failure of a mode is
    /// that the user forgets they are in it and then cannot work out why the map
    /// will not pan. A checkbox makes the state visible and puts the way out in
    /// the place the way in was, which is the only affordance that helps someone
    /// who does not know what happened.
    ///
    /// Not a modifier-drag. A shift-drag is the obvious desktop spelling and has
    /// no touch equivalent whatsoever, and one wasm binary serves phones and
    /// desktop browsers alike.
    ///
    /// Global rather than per-pane, unlike [`VolumePane`](Self::VolumePane),
    /// because the pane it applies to is not knowable when it is ticked: the
    /// user arms the mode and *then* chooses a map to draw on, and choosing it is
    /// the same press that starts the line.
    ///
    /// Ticking it un-ticks [`RegionArm`](Self::RegionArm), for the reason that
    /// entry gives.
    DrawCrossSection,
}

/// One entry in the menu.
pub(super) enum MenuNode {
    /// A named group. The menu bar renders it as a drop-down; the drawer
    /// renders it as a heading with its children beneath.
    Submenu {
        label: &'static str,
        children: Vec<MenuNode>,
    },
    Item {
        label: &'static str,
        action: MenuAction,
    },
    Toggle {
        label: &'static str,
        toggle: MenuToggle,
        value: bool,
    },
    Separator,
}

/// Something the user did to the menu this frame, to be handed to
/// [`super::Gui::apply_menu_event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuEvent {
    Invoked(MenuAction),
    Toggled(MenuToggle, bool),
}

/// One leaf a presentation actually put on screen: the bool `ui.checkbox` was
/// really handed, and where the widget landed so a test can click it for real.
///
/// Reported by the renderer, not rebuilt by a test from the model — for the
/// same reason `ShellOutput::excluded_rects` is an output of the chrome.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrawnMenuLeaf {
    pub label: &'static str,
    /// `Some(state)` for a toggle, `None` for a command or a submenu header.
    pub value: Option<bool>,
    pub rect: egui::Rect,
    /// The egui `Id` the widget was really registered under.
    ///
    /// Reported so a test can see this menu being *re-keyed* rather than only
    /// being moved. Every leaf here is an auto-id'd widget, so its id runs off the
    /// enclosing `Ui`'s `next_auto_id_salt` — which means anything drawn above it
    /// that allocates a varying number of widgets shifts every id in this list
    /// while the labels, the values and (once the layout settles) even the rects
    /// stay recognisable. The harness's own id-change probe cannot see it: that
    /// one matches widgets *by rect*, so a shift which also moves the rects looks
    /// like new widgets rather than re-keyed ones.
    pub id: egui::Id,
}

/// What one presentation produced this frame.
#[derive(Default)]
pub(super) struct MenuFrame {
    pub events: Vec<MenuEvent>,
    /// Every leaf drawn, in render order. See [`DrawnMenuLeaf`].
    #[cfg(test)]
    pub drawn: Vec<DrawnMenuLeaf>,
}

impl MenuFrame {
    /// Record a leaf that was drawn. A no-op outside tests.
    #[inline]
    fn record(&mut self, _label: &'static str, _value: Option<bool>, _response: &egui::Response) {
        #[cfg(test)]
        self.drawn.push(DrawnMenuLeaf {
            label: _label,
            value: _value,
            rect: _response.rect,
            id: _response.id,
        });
    }
}

/// Render the model as one flat dropdown list, for the top bar's ☰ popup.
///
/// The whole menu at every width, in one column: each submenu becomes a run of
/// its leaves, with a separator between runs rather than a heading over each —
/// the popup is small enough that the grouping reads off the rules alone.
pub(super) fn render_menu_popup(ui: &mut egui::Ui, nodes: &[MenuNode]) -> MenuFrame {
    let mut out = MenuFrame::default();
    for (i, node) in nodes.iter().enumerate() {
        match node {
            MenuNode::Submenu { children, .. } => {
                if i > 0 {
                    ui.separator();
                }
                render_menu_items(ui, children, &mut out, true);
            }
            // A top-level leaf is unusual but has to render *somewhere*, or
            // adding one would silently drop it from this presentation only
            // — which is the failure this module exists to remove.
            _ => render_menu_items(ui, std::slice::from_ref(node), &mut out, true),
        }
    }
    out
}

/// Render the model as a flat vertical list — the phone sheet's Menu page
/// (`ui_sheet.rs`): headings over indented leaves, because a sheet page is a
/// document the eye scans, not a dropdown the pointer sweeps.
pub(super) fn render_menu_drawer(ui: &mut egui::Ui, nodes: &[MenuNode]) -> MenuFrame {
    let mut out = MenuFrame::default();
    for node in nodes {
        match node {
            MenuNode::Submenu { label, children } => {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(*label).strong());
                ui.indent(*label, |ui| {
                    render_menu_items(ui, children, &mut out, false);
                });
            }
            _ => render_menu_items(ui, std::slice::from_ref(node), &mut out, false),
        }
    }
    out
}

/// The shared leaf rendering. `in_menu` closes the drop-down after a command,
/// which is only meaningful inside a real menu.
fn render_menu_items(ui: &mut egui::Ui, nodes: &[MenuNode], out: &mut MenuFrame, in_menu: bool) {
    for node in nodes {
        match node {
            MenuNode::Item { label, action } => {
                let response = ui.button(*label);
                out.record(label, None, &response);
                if response.clicked() {
                    out.events.push(MenuEvent::Invoked(*action));
                    if in_menu {
                        ui.close_kind(egui::UiKind::Menu);
                    }
                }
            }
            MenuNode::Toggle {
                label,
                toggle,
                value,
            } => {
                let mut current = *value;
                let response = ui.checkbox(&mut current, *label);
                // `*value`, not `current`: what the checkbox was *handed*.
                out.record(label, Some(*value), &response);
                if response.changed() {
                    out.events.push(MenuEvent::Toggled(*toggle, current));
                }
            }
            MenuNode::Separator => {
                ui.separator();
            }
            // Nesting deeper than one level is not something either
            // presentation is built for; flatten rather than drop.
            MenuNode::Submenu { children, .. } => {
                render_menu_items(ui, children, out, in_menu);
            }
        }
    }
}

impl super::Gui {
    /// Build this frame's menu, reading the live state the toggles reflect.
    pub(super) fn menu_model(&self) -> Vec<MenuNode> {
        let pane = self.active_pane();
        let mut file = vec![MenuNode::Item {
            label: "Refresh Radar",
            action: MenuAction::RefreshRadar,
        }];
        // Omitted where the platform has no quit (iOS): `request_exit` returns
        // early there, so the entry would be a button that does nothing.
        if self.supports_exit {
            file.push(MenuNode::Separator);
            file.push(MenuNode::Item {
                label: "Exit",
                action: MenuAction::Exit,
            });
        }
        vec![
            MenuNode::Submenu {
                label: "File",
                children: file,
            },
            MenuNode::Submenu {
                label: "View",
                children: vec![
                    // First, because it decides what the entries under it even
                    // apply to. `pane` is `self.active_pane()`, which is why
                    // every host builds this model *outside* the frame's
                    // `mem::take` windows — `render_top_bar` runs before any
                    // pane is taken. Inside such a window the slot holds a
                    // default `PaneState`, so this would read `Map` for a
                    // volume pane and draw the box unchecked.
                    MenuNode::Toggle {
                        label: VOLUME_PANE_LABEL,
                        toggle: MenuToggle::VolumePane,
                        value: pane.kind() == crate::pane::PaneKind::Volume,
                    },
                    // Directly under the entry that makes a pane a 3D view,
                    // because it is the other half of setting one up: the first
                    // says *that* you want one, this says *where* it looks.
                    MenuNode::Toggle {
                        label: REGION_ARM_LABEL,
                        toggle: MenuToggle::RegionArm,
                        value: self.region_arm,
                    },
                    // Beside it, and read off the *global* flag rather than off
                    // `pane`: it arms a gesture, and which pane the gesture ends
                    // up aiming is decided by where the line is drawn.
                    //
                    // The two armed drags are adjacent on purpose. They are
                    // mutually exclusive — ticking either un-ticks the other, see
                    // `Gui::set_region_arm` — and a user only reads that off the
                    // menu if the box that un-ticked itself is the one next door.
                    MenuNode::Toggle {
                        label: DRAW_CROSS_SECTION_LABEL,
                        toggle: MenuToggle::DrawCrossSection,
                        value: self.section_draw_armed(),
                    },
                    MenuNode::Separator,
                    MenuNode::Toggle {
                        label: "Show radar sites",
                        toggle: MenuToggle::Overlay(OverlayKind::RadarSites),
                        value: pane.is_overlay_enabled(OverlayKind::RadarSites),
                    },
                    MenuNode::Toggle {
                        label: "Show city labels",
                        toggle: MenuToggle::Overlay(OverlayKind::CityLabels),
                        value: pane.is_overlay_enabled(OverlayKind::CityLabels),
                    },
                    MenuNode::Separator,
                    MenuNode::Toggle {
                        label: "Auto-poll",
                        toggle: MenuToggle::AutoPoll,
                        value: self.auto_poll.enabled,
                    },
                    MenuNode::Toggle {
                        label: "Live: real-time chunks",
                        toggle: MenuToggle::LiveChunks,
                        value: self.live_chunks,
                    },
                    MenuNode::Toggle {
                        label: "Live: push notifications",
                        toggle: MenuToggle::ChunkNotifications,
                        value: self.chunk_notifications,
                    },
                    MenuNode::Separator,
                    MenuNode::Item {
                        label: "Time...",
                        action: MenuAction::OpenTimeDialog,
                    },
                    MenuNode::Item {
                        label: "Settings...",
                        action: MenuAction::OpenSettings,
                    },
                ],
            },
        ]
    }

    /// Apply one menu event. The only place menu semantics live.
    pub(super) fn apply_menu_event(&mut self, event: MenuEvent, actions: &mut Vec<GuiAction>) {
        match event {
            MenuEvent::Invoked(MenuAction::Exit) => actions.push(GuiAction::Exit),
            MenuEvent::Invoked(MenuAction::RefreshRadar) => {
                // The active pane's site, not `radar.config`'s global one —
                // see `active_pane_fetch_config`.
                actions.push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
            }
            MenuEvent::Invoked(MenuAction::OpenTimeDialog) => {
                self.time_dialog.show = true;
                // Close the layers drawer so the dialog is not hidden behind
                // it. A no-op when the drawer is closed or not this width's
                // presentation.
                self.drawer_open = false;
            }
            MenuEvent::Invoked(MenuAction::OpenSettings) => {
                // The inspector's App › Settings body — there is no settings
                // window any more. The drawer still yields: on a narrow width
                // it covers most of the screen the user just asked to look
                // elsewhere on.
                self.open_settings();
                self.drawer_open = false;
            }
            MenuEvent::Toggled(MenuToggle::Overlay(kind), on) => {
                self.set_active_pane_overlay(kind, on);
                self.propagate_layer_sync();
            }
            MenuEvent::Toggled(MenuToggle::RegionArm, on) => {
                // Through the setter, not a bare assignment. Disarming mid-drag
                // has to throw the drag away rather than commit it — a user who
                // reaches for the menu with the button still down is cancelling,
                // and a box that appeared because of it would be one nobody asked
                // for — and *arming* has to un-arm the cross-section draw, which
                // is the other modal drag on a map pane.
                self.set_region_arm(on);
                // Closing the layers drawer on arm, exactly as the
                // cross-section entry below does and for its reason: on a
                // narrow width the drawer covers the map the box has to be
                // dragged on, so arming and leaving it open would arm a
                // gesture the user cannot make. Only on arm — disarming needs
                // no map, so the drawer stays where the user is. The ☰
                // dropdown closes itself on arm too; see `render_top_bar`.
                if on {
                    self.drawer_open = false;
                }
            }
            MenuEvent::Toggled(MenuToggle::AutoPoll, on) => self.auto_poll.enabled = on,
            MenuEvent::Toggled(MenuToggle::LiveChunks, on) => self.live_chunks = on,
            MenuEvent::Toggled(MenuToggle::ChunkNotifications, on) => self.chunk_notifications = on,
            MenuEvent::Toggled(MenuToggle::VolumePane, on) => {
                // Recorded rather than written, through the one route the UI has.
                //
                // Not because this dispatcher is inside a `mem::take` window — it
                // is not. `render_top_bar` takes no pane at all, so a direct
                // `self.panes[self.active_pane].set_kind(..)` here would work
                // today. It goes through `request_pane_kind` so that every writer
                // of a pane's kind obeys one rule, including the ones WP-G adds
                // inside `render_panes`' per-pane take, where the same direct
                // write is silently discarded. See the `pending_pane_kind` field
                // on `Gui` for both halves and for the one-frame cost.
                self.request_pane_kind(
                    self.active_pane,
                    if on {
                        crate::pane::PaneKind::Volume
                    } else {
                        crate::pane::PaneKind::Map
                    },
                );
            }
            MenuEvent::Toggled(MenuToggle::DrawCrossSection, on) => {
                // A direct write, and it may be one: the flag is on `Gui` rather
                // than on a pane, so no `mem::take` window can swallow it. The
                // setter exists because *disarming* has to drop a half-drawn
                // anchor, which a bare assignment would leave behind.
                self.set_section_draw_armed(on);
                // Closing the layers drawer is the point, not a courtesy: on a
                // narrow width it covers the map the line has to be drawn on,
                // so arming the mode and leaving it open would arm a gesture
                // the user cannot make. The ☰ dropdown closes itself on arm
                // for the same reason; see `render_top_bar`.
                if on {
                    self.drawer_open = false;
                }
            }
        }
    }
}

/// Collect every leaf label under `nodes`, submenus flattened, in model order.
#[cfg(test)]
fn collect_leaf_labels(nodes: &[MenuNode], out: &mut Vec<&'static str>) {
    for node in nodes {
        match node {
            MenuNode::Submenu { children, .. } => collect_leaf_labels(children, out),
            MenuNode::Item { label, .. } | MenuNode::Toggle { label, .. } => out.push(label),
            MenuNode::Separator => {}
        }
    }
}

#[cfg(test)]
impl super::Gui {
    /// Every leaf label the menu model currently offers, submenus flattened —
    /// the inventory the parity walk asserts against the drawn
    /// [`DrawnMenuLeaf`]s, derived from the model so a new entry joins the
    /// audit by construction.
    pub(crate) fn menu_model_leaf_labels(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        collect_leaf_labels(&self.menu_model(), &mut out);
        out
    }

    /// The model's top-level groups: each submenu header with the leaf labels
    /// under it, in model order — how the menu-bar presentation has to be
    /// walked, one drop-down at a time. A top-level leaf outside any submenu
    /// would not appear here; the parity walk cross-checks this flattening
    /// against [`Self::menu_model_leaf_labels`] so one could not slip past it.
    pub(crate) fn menu_model_groups(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        self.menu_model()
            .iter()
            .filter_map(|node| match node {
                MenuNode::Submenu { label, children } => {
                    let mut leaves = Vec::new();
                    collect_leaf_labels(children, &mut leaves);
                    Some((*label, leaves))
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gui;

    fn leaves(nodes: &[MenuNode], out: &mut Vec<MenuEvent>) {
        for node in nodes {
            match node {
                MenuNode::Submenu { children, .. } => leaves(children, out),
                MenuNode::Item { action, .. } => out.push(MenuEvent::Invoked(*action)),
                MenuNode::Toggle { toggle, value, .. } => {
                    out.push(MenuEvent::Toggled(*toggle, !*value))
                }
                MenuNode::Separator => {}
            }
        }
    }

    /// Everything a menu entry is allowed to move, as one value. Coarse on
    /// purpose: an entry whose effect is invisible here is one whose effect is
    /// invisible to the user too.
    fn state_fingerprint(gui: &Gui) -> String {
        let mut overlays: Vec<(String, bool)> = gui
            .active_pane()
            .enabled_overlays
            .iter()
            .map(|(kind, on)| (format!("{kind:?}"), *on))
            .collect();
        overlays.sort();
        format!(
            "settings={} insp={} sel={:?} time={} drawer={} auto_poll={} live_chunks={} \
             notify={} kind={:?} pending_kind={:?} region_arm={} armed={} \
             overlays={overlays:?}",
            gui.settings_visible(),
            gui.insp_open,
            gui.inspector_sel,
            gui.time_dialog.show,
            gui.drawer_open,
            gui.auto_poll.enabled,
            gui.live_chunks,
            gui.chunk_notifications,
            gui.active_pane().kind(),
            // Both halves, because a pane conversion is deliberately a two-step
            // operation. Recording the request is the whole of what the
            // dispatcher's arm does — applying it is a separate step, deferred to
            // after the pane loop for reasons set out on the `pending_pane_kind`
            // field — so a fingerprint holding only the *applied* kind would report
            // the arm as a no-op and `every_menu_entry_has_a_dispatcher_arm` would
            // fail for a toggle that works. That the request survives being
            // recorded while a pane is held out of the vector is its own test, in
            // `ui.rs`.
            gui.pending_pane_kind_for_test(),
            gui.region_arm,
            // Both armed drags, and separately. Each is a mode with no other
            // observable — neither converts a pane until a gesture completes — so
            // without them their toggles' arms would read as no-ops and
            // `every_menu_entry_has_a_dispatcher_arm` would fail for two entries
            // that work. Separately rather than as one "is anything armed" flag
            // because they are mutually exclusive: arming one turns the other off,
            // and a single flag would report that swap as no change at all.
            gui.section_draw_armed(),
        )
    }

    /// Every command the model offers actually *does* something.
    ///
    /// The claim is about the effect, not the arm: `match` on [`MenuEvent`] is
    /// exhaustive, so an arm always exists and merely calling
    /// `apply_menu_event` can only catch a panic — `Exit => {}` sails through.
    /// Each entry must emit a [`GuiAction`] or move observable state.
    #[test]
    fn every_menu_entry_has_a_dispatcher_arm() {
        let mut gui = Gui::new();
        let mut events = Vec::new();
        leaves(&gui.menu_model(), &mut events);
        assert!(
            events.len() >= 6,
            "precondition: the model should have real content, found {}",
            events.len()
        );

        for event in events {
            let before = state_fingerprint(&gui);
            let mut actions = Vec::new();
            gui.apply_menu_event(event, &mut actions);
            let after = state_fingerprint(&gui);

            assert!(
                !actions.is_empty() || after != before,
                "{event:?} dispatched to a no-op: it emitted no GuiAction and \
                 changed nothing observable, so the menu entry is a button that \
                 does nothing when clicked"
            );
        }
    }

    /// The toggles report live state rather than a constant, so the checkbox
    /// in the menu reflects what the map is actually doing.
    #[test]
    fn the_toggles_read_back_the_state_they_write() {
        let mut gui = Gui::new();

        let before = overlay_toggle(&gui, OverlayKind::RadarSites);
        let mut actions = Vec::new();
        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::Overlay(OverlayKind::RadarSites), !before),
            &mut actions,
        );
        assert_eq!(
            overlay_toggle(&gui, OverlayKind::RadarSites),
            !before,
            "the model must re-read the pane, not report a snapshot"
        );

        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::AutoPoll, false),
            &mut actions,
        );
        assert!(!auto_poll_toggle(&gui));
        gui.apply_menu_event(MenuEvent::Toggled(MenuToggle::AutoPoll, true), &mut actions);
        assert!(auto_poll_toggle(&gui));
    }

    fn find_toggle(gui: &Gui, want: MenuToggle) -> bool {
        fn walk(nodes: &[MenuNode], want: MenuToggle) -> Option<bool> {
            for node in nodes {
                match node {
                    MenuNode::Submenu { children, .. } => {
                        if let Some(v) = walk(children, want) {
                            return Some(v);
                        }
                    }
                    MenuNode::Toggle { toggle, value, .. } if *toggle == want => {
                        return Some(*value);
                    }
                    _ => {}
                }
            }
            None
        }
        walk(&gui.menu_model(), want).expect("toggle missing from the menu model")
    }

    fn overlay_toggle(gui: &Gui, kind: OverlayKind) -> bool {
        find_toggle(gui, MenuToggle::Overlay(kind))
    }

    fn auto_poll_toggle(gui: &Gui) -> bool {
        find_toggle(gui, MenuToggle::AutoPoll)
    }

    /// The 3D toggle reads the *active pane's* kind, not a global flag.
    ///
    /// With several panes on screen the entry describes one of them, and a
    /// version keyed on "is any pane a volume view" would show checked for the
    /// map the user is actually working in — then convert *that* one when they
    /// unticked it to make the box match what they were looking at.
    #[test]
    fn the_volume_toggle_describes_the_active_pane_and_no_other() {
        use crate::pane::PaneKind;

        let mut gui = Gui::new();
        gui.set_pane_count_for_test(2);
        assert!(
            !find_toggle(&gui, MenuToggle::VolumePane),
            "precondition: two fresh map panes"
        );

        gui.pane_mut(1).unwrap().set_kind(PaneKind::Volume);
        assert!(
            !find_toggle(&gui, MenuToggle::VolumePane),
            "the toggle read some other pane's kind: pane 0 is the active one and \
             it is still a map"
        );

        gui.active_pane = 1;
        assert!(find_toggle(&gui, MenuToggle::VolumePane));
    }

    /// Unticking the 3D toggle asks for a map back, rather than doing nothing.
    ///
    /// The `on` argument is the checkbox's *new* value, so an arm that ignored it
    /// and always asked for `Volume` would leave the box stuck ticked with no way
    /// out of the pane — and `every_menu_entry_has_a_dispatcher_arm` would not
    /// notice, because the first tick moves the fingerprint on its own.
    #[test]
    fn the_volume_toggle_converts_in_both_directions() {
        use crate::pane::PaneKind;

        let mut gui = Gui::new();
        let mut actions = Vec::new();

        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::VolumePane, true),
            &mut actions,
        );
        assert_eq!(
            gui.pending_pane_kind_for_test(),
            Some((0, PaneKind::Volume))
        );

        gui.apply_menu_event(
            MenuEvent::Toggled(MenuToggle::VolumePane, false),
            &mut actions,
        );
        assert_eq!(
            gui.pending_pane_kind_for_test(),
            Some((0, PaneKind::Map)),
            "unticking the box asked for a volume pane again, so a pane converted \
             by accident can never be converted back"
        );

        assert!(
            actions.is_empty(),
            "converting a pane is local to the Gui and needs nothing of the host"
        );
    }

    /// Opening a dialog closes the drawer. On a compact screen the drawer
    /// covers most of the width, so leaving it open hides the dialog the user
    /// just asked for.
    #[test]
    fn opening_a_dialog_from_the_drawer_closes_it() {
        for (event, opened) in [
            (
                MenuEvent::Invoked(MenuAction::OpenSettings),
                "settings" as &str,
            ),
            (MenuEvent::Invoked(MenuAction::OpenTimeDialog), "time"),
        ] {
            let mut gui = Gui::new();
            gui.drawer_open = true;
            let mut actions = Vec::new();
            gui.apply_menu_event(event, &mut actions);
            assert!(!gui.drawer_open, "{opened} dialog left the drawer open");
        }
    }

    /// An overlay detail item, so the pager popup can be opened without a map
    /// click. The concrete items are `pub(crate)` to `rustdar-overlays`; the
    /// trait is not.
    #[derive(Debug)]
    struct StubOverlayItem;

    impl rustdar_overlays::render::overlay_state::OverlayItem for StubOverlayItem {
        fn kind(&self) -> OverlayKind {
            OverlayKind::NwsAlerts
        }
        fn popup_content(
            &self,
            _prefs: &rustdar_units::UserPreferences,
        ) -> rustdar_overlays::render::overlay_state::PopupContent {
            rustdar_overlays::render::overlay_state::PopupContent {
                title: "Stub".to_owned(),
                accent_rgb: [255, 0, 0],
                width: 300.0,
                sections: Vec::new(),
                actions: Vec::new(),
            }
        }
        fn matches(
            &self,
            _other: &dyn rustdar_overlays::render::overlay_state::OverlayItem,
        ) -> bool {
            false
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Escape and back close what is open, one layer per press, and say so.
    ///
    /// Only when nothing is open is the press a request to leave: with the
    /// drawer open, back used to go straight to minimise, which on the phone
    /// widths this app actually runs at throws away the only route to the
    /// whole menu on a single misplaced tap.
    ///
    /// Driven top down through all four layers, so it also pins the *order*: a
    /// press must take the topmost, not whichever the function tests for first.
    /// The overlay pager sits above everything — it is what a map tap opens —
    /// and each press below it must leave the ones under it alone.
    #[test]
    fn a_back_press_closes_one_open_layer_at_a_time() {
        let mut gui = Gui::new();
        gui.drawer_open = true;
        // A non-default selection, so the reset-on-dismiss is observable.
        gui.select_layer(OverlayKind::NwsAlerts);
        gui.time_dialog.show = true;
        gui.overlays.selected_overlays = vec![std::sync::Arc::new(StubOverlayItem)];
        gui.overlays.selected_overlay_page = 0;

        assert!(gui.dismiss_top_layer(), "the overlay pager was open");
        assert!(
            gui.overlays.selected_overlays.is_empty(),
            "the overlay pager did not close"
        );
        assert!(
            gui.insp_open && gui.time_dialog.show && gui.drawer_open,
            "closing the pager took a layer under it with it: {}",
            state_fingerprint(&gui)
        );

        assert!(gui.dismiss_top_layer(), "the time dialog was open");
        assert!(!gui.time_dialog.show, "the time dialog did not close");
        assert!(
            gui.insp_open && gui.drawer_open,
            "one press closed more than one layer: {}",
            state_fingerprint(&gui)
        );

        assert!(gui.dismiss_top_layer(), "the inspector was open");
        assert!(!gui.insp_open, "the inspector did not close");
        assert_eq!(
            gui.inspector_sel,
            crate::ui::InspectorSelection::AppSettings,
            "a dismissal must reset the selection to App \u{203a} Settings \
             (plan \u{a7}3.4), not leave the layer lying in wait"
        );
        assert!(gui.drawer_open, "the drawer went with it");

        assert!(gui.dismiss_top_layer(), "the drawer was open");
        assert!(!gui.drawer_open, "the drawer did not close");

        assert!(
            !gui.dismiss_top_layer(),
            "reported something dismissed with nothing open, so a press would \
             never reach the exit path at all: {}",
            state_fingerprint(&gui)
        );
    }
}
