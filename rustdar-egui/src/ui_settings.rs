use crate::actions::GuiAction;
use rustdar_gps::HeadingSource;
use rustdar_units::{
    DistanceUnit, HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit,
    TimezonePreference, UnitLabel, UserPreferences,
};

/// Width of the settings window when there is room for a fixed-width one.
/// Narrower screens get a full-bleed window instead — see
/// [`crate::ui_layout::LayoutCtx::dialog_width`].
const SETTINGS_POPUP_ROOMY_WIDTH: f32 = 340.0;
const SETTINGS_SMALL_SPACING: f32 = 4.0;
const SETTINGS_LARGE_SPACING: f32 = 8.0;
#[cfg(feature = "gps-serial")]
const GPS_BAUD_RATES: &[u32] = &[4800, 9600, 38400, 115200];

/// The storm motion override switch's label.
///
/// It read "Override average storm motion", which named the RPG's own SCIT
/// average from the `N0S` Product Description Block — a source that left the
/// app with the five Level III SRM fetches. There is no RPG average to
/// override any more: with the switch off, storm-relative velocity is derived
/// from the Bunkers right-mover fitted to the volume's own winds. Named here
/// so the wording has one home and a test can pin it.
pub(crate) const STORM_MOTION_OVERRIDE_LABEL: &str = "Override the storm motion vector";

/// What actually leaves the machine when the user says yes, in the pane that
/// asks.
///
/// The button says "Use my location" and every platform's own dialog says some
/// variant of "allow access to your location", and none of that tells the user
/// that finding a position may mean **sending data about their surroundings to
/// a third party**. It routinely does: every desktop and mobile location
/// service resolves a position from the public IP address, from the identifiers
/// of nearby Wi-Fi access points, or both.
///
/// Linux names the destination because on Linux it is knowable and fixed: the
/// portal that answers rustdar proxies to GeoClue, whose Wi-Fi backend POSTs
/// the BSSIDs it can see to `api.beacondb.net`, and the user can turn that off
/// in `geoclue.conf` — advice that is only useful if they know the request
/// exists. Windows, macOS and Android send comparable data to endpoints their
/// vendors do not publish and the user cannot configure, so naming a host there
/// would be an invention. Web is whichever service the browser uses, which the
/// browser chooses.
///
/// A `cfg` on `target_os` and not on anything else: `target_os = "android"` is
/// distinct from `"linux"`, and wasm32 is neither, so the general sentence is
/// what those builds get.
#[cfg(target_os = "linux")]
const LOCATION_EGRESS_NOTE: &str = "Approximate, from your system's location \
    service. Finding a position sends your IP address, and — if the Wi-Fi \
    backend is enabled — the identifiers of nearby wireless networks, to \
    api.beacondb.net.";
#[cfg(not(target_os = "linux"))]
const LOCATION_EGRESS_NOTE: &str = "Approximate, from your device's location \
    service. Finding a position may send your IP address and details of nearby \
    wireless networks to that service's provider.";

/// Where a user actually goes to undo a refusal, in the pane that reports one.
///
/// "It can be turned back on in your system settings" is true wherever the
/// platform has a location page, and on Linux it is not: what refuses rustdar
/// is xdg-desktop-portal's `disable-location`, and the backend that implements
/// it — `xdg-desktop-portal-gtk`, which almost every desktop installs for its
/// file chooser — answers that property from the GSettings key
/// `org.gnome.system.location enabled`. That key **defaults to false** and has
/// a UI only on GNOME. So on a stock KDE, Sway or Hyprland machine the generic
/// sentence points at a page that does not exist, for a switch that is already
/// the reason nothing works.
///
/// Naming the command is the honest answer, and it is also why
/// `OsLocationReader::settings_available()` is `false` on Linux: there is a
/// thing to *type*, not a page to open, and a button that launched GNOME's
/// control centre on Plasma would be a worse lie than no button.
///
/// Same `cfg` axis as [`LOCATION_EGRESS_NOTE`], for the same reason: Android is
/// not `linux` and wasm32 is neither.
/// `pub(crate)` so `input_harness` can assert the pane paints *this* — the
/// property worth testing is "a refusal is explained", and a test that spelled
/// the sentence out again would only pin whichever platform ran it.
#[cfg(target_os = "linux")]
pub(crate) const LOCATION_DENIED_NOTE: &str = "Your desktop's location switch \
    is off, so the portal refused. GNOME has this under Settings \u{203a} \
    Privacy; most other desktops have no page for it, and this works \
    everywhere:\n\
    \n\
    gsettings set org.gnome.system.location enabled true";
/// See the Linux arm above.
#[cfg(not(target_os = "linux"))]
pub(crate) const LOCATION_DENIED_NOTE: &str = "Location for this app is turned \
    off. It can be turned back on in your system settings.";

/// Every row the settings window draws, in draw order, each under a stable id.
///
/// The renderer *iterates this table* — `render_settings_row` has one arm per
/// id — and the parity walk asserts every id here was drawn. That is the
/// contract: a row cannot be added to one side without the other noticing, and
/// an id here with no arm panics the first frame the window opens.
///
/// The GPS rows are listed unconditionally even though the widgets are gated on
/// the `gps-serial` feature: the table is the inventory, and the renderer
/// simply draws nothing (and records nothing) for them on a build without the
/// feature. The walk asserts them only where the feature compiled them in.
pub(crate) const SETTINGS_ROWS: &[&str] = &[
    "units.timezone",
    "units.temperature",
    "units.speed",
    "units.distance",
    "units.height",
    "units.precip_rate",
    "units.hail_size",
    "location",
    "gps.port",
    "gps.baud",
    "gps.connect",
    "heading",
    "storm.override",
    "storm.speed",
    "storm.direction",
    "reset",
];

/// One settings row the window actually drew: which [`SETTINGS_ROWS`] id it
/// was, and where it landed so a test can find it on screen.
///
/// Reported by the renderer, like [`crate::ui::DrawnDropdown`], rather than
/// rebuilt by a test from the table — a test that walked [`SETTINGS_ROWS`]
/// itself would agree with a renderer that had stopped drawing a row.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrawnSettingsRow {
    pub id: &'static str,
    pub rect: egui::Rect,
}

/// The chrome between two settings groups: breathing room, a rule, and the
/// smaller lead-in the next group's content sits under.
fn section_break(ui: &mut egui::Ui) {
    ui.add_space(SETTINGS_LARGE_SPACING);
    ui.separator();
    ui.add_space(SETTINGS_SMALL_SPACING);
}

impl super::Gui {
    /// Render the settings window if `show_settings` is true.
    ///
    /// The body is [`SETTINGS_ROWS`] driven through [`Self::render_settings_row`]
    /// — data plus a match, not a hand-written sequence — so the parity walk's
    /// inventory and the drawn window cannot drift apart.
    pub(super) fn render_settings(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
        if !self.show_settings {
            return;
        }

        let popup_width = self.layout.dialog_width(SETTINGS_POPUP_ROOMY_WIDTH);

        let mut open = true;
        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .open(&mut open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(true)
            // Outer width since egui 0.35 (#7725) — content is 14px narrower at
            // the stock theme. See the note in `ui_popups.rs`; same reasoning,
            // deliberately not compensated.
            .default_width(popup_width)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(self.layout.dialog_center())
            .show(ctx, |ui| {
                for &row in SETTINGS_ROWS {
                    #[cfg(test)]
                    let row_top = ui.cursor().top();
                    let drawn = self.render_settings_row(ui, row, actions);
                    // The rect is read off the cursor rather than off a
                    // wrapping scope, because a scope would change every
                    // row's widget ids for the probe's convenience.
                    #[cfg(test)]
                    if drawn {
                        self.last_settings_rows.push(DrawnSettingsRow {
                            id: row,
                            rect: egui::Rect::from_x_y_ranges(
                                ui.max_rect().x_range(),
                                row_top..=ui.cursor().top(),
                            ),
                        });
                    }
                    #[cfg(not(test))]
                    let _ = drawn;
                }
            });

        if !open {
            self.show_settings = false;
        }
    }

    /// Draw one row of [`SETTINGS_ROWS`]. Returns whether anything was drawn,
    /// which is `false` only for a row this build compiles out (the GPS rows
    /// without the `gps-serial` feature).
    ///
    /// A row owns the group chrome that *precedes* it, so the sequence the
    /// loop produces is exactly the hand-written one this replaced: the
    /// [`section_break`] between two groups belongs to the first row of the
    /// later group — except around the feature-gated GPS block, where the
    /// break is carried as a trailing one by the row *before* the gap, so it
    /// is drawn whichever of the gated rows this build compiles in.
    fn render_settings_row(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        actions: &mut Vec<GuiAction>,
    ) -> bool {
        match id {
            "units.timezone" => {
                ui.heading("Units");
                ui.add_space(SETTINGS_SMALL_SPACING);
                unit_combo(
                    ui,
                    "Timezone",
                    &mut self.preferences.timezone,
                    TimezonePreference::ALL,
                );
                true
            }
            "units.temperature" => {
                unit_combo(
                    ui,
                    "Temperature",
                    &mut self.preferences.temperature,
                    TemperatureUnit::ALL,
                );
                true
            }
            "units.speed" => {
                unit_combo(ui, "Speed", &mut self.preferences.speed, SpeedUnit::ALL);
                true
            }
            "units.distance" => {
                unit_combo(
                    ui,
                    "Distance",
                    &mut self.preferences.distance,
                    DistanceUnit::ALL,
                );
                true
            }
            "units.height" => {
                unit_combo(ui, "Height", &mut self.preferences.height, HeightUnit::ALL);
                true
            }
            "units.precip_rate" => {
                unit_combo(
                    ui,
                    "Precip rate",
                    &mut self.preferences.precip_rate,
                    PrecipRateUnit::ALL,
                );
                true
            }
            "units.hail_size" => {
                unit_combo(
                    ui,
                    "Hail size",
                    &mut self.preferences.hail_size,
                    HailSizeUnit::ALL,
                );
                true
            }
            // --- Location (all platforms) ---
            //
            // Ungated, and above the GPS block rather than inside it, because
            // it is a different question with a different answer on every
            // platform:
            //
            //   Location — may this app know where you are, from the OS.
            //              A privilege, granted and withdrawn in system
            //              settings, and the only one rustdar asks for.
            //   GPS      — open this serial port and read NMEA from it.
            //              A device the user plugged in. No permission
            //              anywhere, and absent from four of five targets.
            //
            // Written to read as two questions, not two spellings of one:
            // "Use my location" against "Connect GPS" below.
            "location" => {
                section_break(ui);
                ui.heading("Location");
                ui.add_space(SETTINGS_SMALL_SPACING);
                self.render_location_controls(ui, actions);
                section_break(ui);
                true
            }
            // --- GPS section (serial-capable targets only) ---
            //
            // Gated on the feature rather than on `not(android)` so that the
            // gate matches the one on `detect_gps_ports` below. An OS cfg can
            // never satisfy a feature cfg, and mismatching the two is what
            // stopped this crate building standalone.
            #[cfg(feature = "gps-serial")]
            "gps.port" => {
                ui.heading("GPS");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.horizontal(|ui| {
                    ui.label("Port:");
                    // One list, read by both halves. The collapsed box used
                    // to show the bare device path while the list it opened
                    // showed "path (description)" — the same divergence the
                    // handler dropdowns had. Enumerated once per frame
                    // because `detect_gps_ports` touches the serial
                    // subsystem, so formatting the two halves separately
                    // would mean probing it twice.
                    let ports = gps_port_options(rustdar_gps::detect_gps_ports());
                    let selected = gps_port_label(&ports, self.gps_config.port_path.as_deref());
                    egui::ComboBox::from_id_salt("gps_port")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (value, label) in &ports {
                                ui.selectable_value(
                                    &mut self.gps_config.port_path,
                                    value.clone(),
                                    label.as_str(),
                                );
                            }
                        });
                });
                true
            }
            #[cfg(feature = "gps-serial")]
            "gps.baud" => {
                ui.horizontal(|ui| {
                    ui.label("Baud:");
                    let baud_label = if self.gps_config.auto_baud() {
                        "Auto-detect".to_string()
                    } else {
                        self.gps_config.baud_rate.to_string()
                    };
                    egui::ComboBox::from_id_salt("gps_baud")
                        .selected_text(baud_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.gps_config.baud_rate, 0, "Auto-detect");
                            for &rate in GPS_BAUD_RATES {
                                ui.selectable_value(
                                    &mut self.gps_config.baud_rate,
                                    rate,
                                    rate.to_string(),
                                );
                            }
                        });
                });
                true
            }
            #[cfg(feature = "gps-serial")]
            "gps.connect" => {
                ui.add_space(SETTINGS_SMALL_SPACING);

                // Start/stop button
                // Note: gps_active state is only meaningful on desktop
                if ui.button("Connect GPS").clicked() {
                    actions.push(GuiAction::StartGps {
                        config: self.gps_config.clone(),
                    });
                }
                if ui.button("Disconnect GPS").clicked() {
                    actions.push(GuiAction::StopGps);
                }

                ui.add_space(SETTINGS_SMALL_SPACING);

                // Fix status
                if let Some(ref fix) = self.user_fix {
                    ui.label(format!("Fix: {}", fix.fix_quality.label()));
                    if let Some(sats) = fix.satellites {
                        ui.label(format!("Sats: {}", sats));
                    }
                } else {
                    ui.label("No GPS fix");
                }

                section_break(ui);
                true
            }
            #[cfg(not(feature = "gps-serial"))]
            "gps.port" | "gps.baud" | "gps.connect" => false,
            // --- Heading source (all platforms) ---
            "heading" => {
                ui.horizontal(|ui| {
                    ui.label("Heading:");
                    egui::ComboBox::from_id_salt("heading_source")
                        .selected_text(self.gps_config.heading_source.label())
                        .show_ui(ui, |ui| {
                            for &src in HeadingSource::ALL {
                                ui.selectable_value(
                                    &mut self.gps_config.heading_source,
                                    src,
                                    src.label(),
                                );
                            }
                        });
                });
                true
            }
            // --- Storm motion (storm-relative velocity) ---
            //
            // Off by default, and what "off" means changed. The label
            // read "Override average storm motion", naming the RPG's own
            // SCIT average from the N0S Product Description Block — a
            // source that left with the five Level III SRM fetches. With
            // the switch off, storm-relative velocity now uses the
            // Bunkers right-mover `rustdar_radar::srv::volume_wind_profile`
            // fits from the volume's own winds; there is no RPG vector to
            // override any more. An override replaces it everywhere at
            // once: every storm-relative tilt, the 3D volume and the
            // cross-section are all derived from it.
            "storm.override" => {
                section_break(ui);
                ui.heading("Storm motion");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.checkbox(
                    &mut self.storm_motion_override.enabled,
                    STORM_MOTION_OVERRIDE_LABEL,
                )
                .on_hover_text(
                    "Off, storm-relative velocity uses the Bunkers right-mover fitted \
                     from this volume's own winds. On, it uses the vector below \
                     \u{2014} in the plan view, the 3D volume and the cross-section alike.",
                );
                true
            }
            "storm.speed" => {
                let motion = &mut self.storm_motion_override;
                ui.add_enabled_ui(motion.enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Speed:");
                        // Upper bound shared with `DERIVED_OFFSET`, which is
                        // sized so nothing this widget admits can saturate the
                        // derived gate encoding.
                        ui.add(
                            egui::DragValue::new(&mut motion.speed_kt)
                                .speed(0.5)
                                .range(0.0..=rustdar_radar::srm::MAX_OVERRIDE_SPEED_KT)
                                .suffix(" kt"),
                        );
                    });
                });
                true
            }
            "storm.direction" => {
                let motion = &mut self.storm_motion_override;
                ui.add_enabled_ui(motion.enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("From:");
                        ui.add(
                            egui::DragValue::new(&mut motion.direction_deg)
                                .speed(1.0)
                                .range(0.0..=360.0)
                                .suffix("\u{00b0}"),
                        );
                    });
                });
                true
            }
            "reset" => {
                section_break(ui);
                if ui.button("Reset to defaults").clicked() {
                    self.preferences = UserPreferences::default();
                    self.gps_config = rustdar_gps::GpsConfig::default();
                    self.storm_motion_override = crate::StormMotionOverride::default();
                    // The location memo lives outside `Gui` — it is persisted
                    // under its own key by the frontend's gate, precisely so a
                    // 3 s autosave timer cannot lose it — so resetting it is an
                    // action rather than an assignment. Included because this
                    // button is the obvious thing a user reaches for when they
                    // want a dismissed permission prompt back, and a "reset"
                    // that quietly kept one piece of state would be a lie.
                    actions.push(GuiAction::RequestLocation);
                }
                true
            }
            other => unreachable!(
                "SETTINGS_ROWS lists {other:?} but render_settings_row has no arm for it"
            ),
        }
    }

    /// The body of the Location section: one line of state, at most one button,
    /// and — on the platforms where nothing else would say so — whether a fix
    /// has actually arrived.
    fn render_location_controls(&self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        use rustdar_gps::LocationPermission;

        match self.location_permission {
            // No service to grant. No control, because there is no sequence of
            // clicks that changes this, and "open system settings" would send
            // the user hunting for a switch that does not exist.
            LocationPermission::Unavailable => {
                ui.label("Not available on this platform.");
            }
            // The startup window every platform has. Deliberately not a button:
            // offering one here is how the app ends up asking before the OS has
            // said whether anyone has been asked.
            LocationPermission::Unknown => {
                ui.label("Checking\u{2026}");
            }
            // A decision, and one only the user can reverse. No button — the
            // platform will not show a second dialog — so the only useful thing
            // here is where to go instead, and on one platform "where" is a
            // command rather than a page. See [`LOCATION_DENIED_NOTE`].
            LocationPermission::Denied => {
                ui.label("Denied.");
                ui.label(LOCATION_DENIED_NOTE);
                // A shortcut to the page, not a second way to ask — and only on
                // a platform that has one. "Open", not "Allow": a machine-wide
                // policy can leave that toggle greyed out, and this button
                // cannot promise what the user will find.
                if self.location_settings_available && ui.button("Open location settings").clicked()
                {
                    actions.push(GuiAction::OpenLocationSettings);
                }
            }
            LocationPermission::Granted if self.location_active => {
                ui.label("On.");
                // "Turn off", not "revoke": this stops the stream and nothing
                // more. No platform lets an app hand a permission back.
                if ui.button("Turn off").clicked() {
                    actions.push(GuiAction::StopLocation);
                }
            }
            // Granted-but-idle and never-asked land on the same button on
            // purpose. From the user's side they are one thing — "start using
            // my location" — and the difference between them is only whether a
            // dialog appears, which the OS decides and this pane cannot promise
            // either way.
            LocationPermission::Prompt | LocationPermission::Granted => {
                if ui.button("Use my location").clicked() {
                    actions.push(GuiAction::RequestLocation);
                }
            }
        }

        // Only where a location is on offer or already running. On a platform
        // with no service, and after a refusal, there is no request to describe
        // and the sentence would be an unprompted privacy warning about
        // something that is not happening.
        if matches!(
            self.location_permission,
            LocationPermission::Prompt | LocationPermission::Granted
        ) {
            ui.label(LOCATION_EGRESS_NOTE);
        }

        if let Some(line) = self.location_fix_summary() {
            ui.label(line);
        }
    }

    /// Whether a position has actually arrived, in one line, or `None` when
    /// there is nothing to say.
    ///
    /// # Why this is not the `Fix:` readout in the GPS block
    ///
    /// That one is inside `#[cfg(feature = "gps-serial")]`, which web, Android,
    /// iOS and every build without a serial port do not compile. On exactly
    /// those platforms — the ones where the OS location service is the *only*
    /// source — the section above would otherwise say "On." beside an empty map
    /// and explain nothing. That is also the likely Linux outcome: the portal
    /// can take a while, or answer with nothing at all.
    ///
    /// Coarse on purpose. Seconds would tick in a window nobody is watching for
    /// a value that changes every few minutes.
    fn location_fix_summary(&self) -> Option<String> {
        if !self.location_active && self.user_fix.is_none() {
            return None;
        }
        let Some(at) = self.user_fix_at else {
            return Some("Waiting for a fix\u{2026}".to_owned());
        };
        let minutes = at.elapsed().as_secs() / 60;
        Some(match minutes {
            0 => "Last fix: just now.".to_owned(),
            1 => "Last fix: 1 minute ago.".to_owned(),
            n => format!("Last fix: {n} minutes ago."),
        })
    }
}

/// The GPS port dropdown's options, as `(value, label)` — "Auto-detect" plus
/// every port given.
///
/// Takes the ports rather than calling `detect_gps_ports` itself so the
/// labelling can be tested; enumeration needs real hardware.
#[cfg(feature = "gps-serial")]
fn gps_port_options(
    ports: impl IntoIterator<Item = rustdar_gps::GpsPortInfo>,
) -> Vec<(Option<String>, String)> {
    std::iter::once((None, "Auto-detect".to_owned()))
        .chain(ports.into_iter().map(|port| {
            (
                Some(port.port_name.clone()),
                format!("{} ({})", port.port_name, port.description),
            )
        }))
        .collect()
}

/// The label the port list puts against `selected`.
///
/// Falls back to the bare device path for a configured port that is no longer
/// plugged in: it is not in the list, but naming it is better than silently
/// reading "Auto-detect" while a specific port is still configured.
#[cfg(feature = "gps-serial")]
fn gps_port_label(ports: &[(Option<String>, String)], selected: Option<&str>) -> String {
    ports
        .iter()
        .find(|(value, _)| value.as_deref() == selected)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| selected.unwrap_or("Auto-detect").to_owned())
}

/// Generic combo box for a unit preference enum.
fn unit_combo<T: Copy + PartialEq + UnitLabel>(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut T,
    options: &[T],
) {
    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        egui::ComboBox::from_id_salt(label)
            .selected_text(current.display_label())
            .show_ui(ui, |ui| {
                for &option in options {
                    ui.selectable_value(current, option, option.display_label());
                }
            });
    });
}

#[cfg(all(test, feature = "gps-serial"))]
mod tests {
    use super::*;

    /// Built by the shipped `gps_port_options`, so the labels under test are
    /// the ones the dropdown really offers.
    fn ports() -> Vec<(Option<String>, String)> {
        gps_port_options([rustdar_gps::GpsPortInfo {
            port_name: "/dev/ttyUSB0".to_owned(),
            description: "FT232R USB UART".to_owned(),
        }])
    }

    /// A port is offered under its description, not its bare device path —
    /// `/dev/ttyUSB0` alone does not tell you which of two dongles it is.
    #[test]
    fn the_port_list_describes_each_port() {
        let ports = ports();
        assert_eq!(ports[0], (None, "Auto-detect".to_owned()));
        assert_eq!(
            ports[1],
            (
                Some("/dev/ttyUSB0".to_owned()),
                "/dev/ttyUSB0 (FT232R USB UART)".to_owned()
            ),
        );
    }

    /// The collapsed box shows what the open list shows.
    ///
    /// It used to show the raw `port_path`, so a chosen port read
    /// `/dev/ttyUSB0` until you opened the list and found it described there
    /// as `/dev/ttyUSB0 (FT232R USB UART)`. The same defect the handler
    /// dropdowns had, hidden behind a non-default feature that wasm and
    /// Android never build.
    #[test]
    fn the_gps_port_box_shows_the_label_its_list_shows() {
        let ports = ports();
        for (value, label) in &ports {
            assert_eq!(
                gps_port_label(&ports, value.as_deref()),
                *label,
                "the collapsed box disagrees with the list entry for {value:?}"
            );
        }
    }

    /// A configured port that is no longer plugged in is not in the list.
    /// Naming it beats reading "Auto-detect" while a specific port is set.
    #[test]
    fn an_unplugged_port_is_still_named() {
        assert_eq!(gps_port_label(&ports(), Some("/dev/ttyS9")), "/dev/ttyS9");
        assert_eq!(gps_port_label(&[], None), "Auto-detect");
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    /// The storm motion switch must not name a source the app no longer has.
    ///
    /// It read "Override **average** storm motion" — the RPG's own SCIT
    /// average, carried in the `N0S` Product Description Block, which left
    /// with the five Level III SRM fetches. With the switch off there is no
    /// average to override: storm-relative velocity is derived from the
    /// Bunkers right-mover fitted to the volume's own winds. A control that
    /// names a vanished source tells the user their override is replacing
    /// something the app is not using.
    #[test]
    fn the_storm_motion_switch_does_not_name_an_rpg_average() {
        let label = STORM_MOTION_OVERRIDE_LABEL.to_ascii_lowercase();
        assert!(
            !label.contains("average"),
            "the switch reads {STORM_MOTION_OVERRIDE_LABEL:?}, naming the RPG \
             SCIT average that left with the Level III SRM fetches",
        );
        assert!(
            label.contains("storm motion"),
            "the switch reads {STORM_MOTION_OVERRIDE_LABEL:?}, which does not \
             say what it overrides",
        );
    }
}
