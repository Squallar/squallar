//! One menu model, two renderers, one dispatcher.
//!
//! The menu used to exist twice: as a real `MenuBar` in `ui_desktop.rs`, and
//! hand-rolled again as a "Controls" block of buttons inside `ui_mobile.rs`'s
//! layers panel. The two drifted — the mobile copy had Refresh and Auto-poll,
//! the desktop one had the overlay toggles — and every new entry had to be
//! added in both places, in the right one of two files, or it silently existed
//! on only one platform.
//!
//! So the menu is described once as data ([`MenuNode`]), rendered by whichever
//! of the two presentations the current [`WidthClass`](crate::ui_layout::WidthClass)
//! calls for, and the resulting [`MenuEvent`]s are applied in exactly one
//! place. A new entry is one line in [`super::Gui::menu_model`] and one arm in
//! [`super::Gui::apply_menu_event`], and it appears in both presentations by
//! construction.

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

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
/// same reason `ChromeOutput::excluded_rects` is an output of the chrome.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrawnMenuLeaf {
    pub label: &'static str,
    /// `Some(state)` for a toggle, `None` for a command or a submenu header.
    pub value: Option<bool>,
    pub rect: egui::Rect,
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
    fn record(&mut self, _label: &'static str, _value: Option<bool>, _rect: egui::Rect) {
        #[cfg(test)]
        self.drawn.push(DrawnMenuLeaf {
            label: _label,
            value: _value,
            rect: _rect,
        });
    }
}

/// Render the model as a horizontal menu bar with drop-downs.
///
/// Used when there is room for one — see `WidthClass::has_menu_bar`.
pub(super) fn render_menu_bar(ui: &mut egui::Ui, nodes: &[MenuNode]) -> MenuFrame {
    let mut out = MenuFrame::default();
    egui::MenuBar::new().ui(ui, |ui| {
        for node in nodes {
            match node {
                MenuNode::Submenu { label, children } => {
                    let header = ui
                        .menu_button(*label, |ui| {
                            render_menu_items(ui, children, &mut out, true);
                        })
                        .response;
                    // The header's own rect, so a test can open the drop-down
                    // the way a user does instead of reaching into egui memory.
                    out.record(label, None, header.rect);
                }
                // A top-level leaf is unusual but has to render *somewhere*, or
                // adding one would silently drop it from this presentation only
                // — which is the failure this module exists to remove.
                _ => render_menu_items(ui, std::slice::from_ref(node), &mut out, true),
            }
        }
    });
    out
}

/// Render the model as a flat vertical list, for the slide-out drawer.
///
/// Used when there is no menu bar, and reached through the hamburger.
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
                out.record(label, None, response.rect);
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
                out.record(label, Some(*value), response.rect);
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
                // Close the drawer so the dialog is not hidden behind it. A
                // no-op when the drawer is not the current presentation.
                self.drawer_open = false;
            }
            MenuEvent::Invoked(MenuAction::OpenSettings) => {
                self.show_settings = true;
                self.drawer_open = false;
            }
            MenuEvent::Toggled(MenuToggle::Overlay(kind), on) => {
                self.set_active_pane_overlay(kind, on);
                self.propagate_layer_sync();
            }
            MenuEvent::Toggled(MenuToggle::AutoPoll, on) => self.auto_poll.enabled = on,
            MenuEvent::Toggled(MenuToggle::LiveChunks, on) => self.live_chunks = on,
            MenuEvent::Toggled(MenuToggle::ChunkNotifications, on) => self.chunk_notifications = on,
        }
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
            "settings={} time={} drawer={} auto_poll={} live_chunks={} notify={} \
             overlays={overlays:?}",
            gui.show_settings,
            gui.time_dialog.show,
            gui.drawer_open,
            gui.auto_poll.enabled,
            gui.live_chunks,
            gui.chunk_notifications,
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
        gui.show_settings = true;
        gui.time_dialog.show = true;
        gui.overlays.selected_overlays = vec![std::sync::Arc::new(StubOverlayItem)];
        gui.overlays.selected_overlay_page = 0;

        assert!(gui.dismiss_top_layer(), "the overlay pager was open");
        assert!(
            gui.overlays.selected_overlays.is_empty(),
            "the overlay pager did not close"
        );
        assert!(
            gui.show_settings && gui.time_dialog.show && gui.drawer_open,
            "closing the pager took a layer under it with it: {}",
            state_fingerprint(&gui)
        );

        assert!(gui.dismiss_top_layer(), "the settings window was open");
        assert!(!gui.show_settings, "settings did not close");
        assert!(
            gui.time_dialog.show && gui.drawer_open,
            "one press closed more than one layer: {}",
            state_fingerprint(&gui)
        );

        assert!(gui.dismiss_top_layer(), "the time dialog was open");
        assert!(!gui.time_dialog.show, "the time dialog did not close");
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
