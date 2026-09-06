use crate::actions::GuiAction;
use squallar_location::HeadingSource;
use squallar_units::{
    DistanceUnit, HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit,
    TimezonePreference, UnitLabel, UserPreferences,
};

const SETTINGS_SMALL_SPACING: f32 = 4.0;
const SETTINGS_LARGE_SPACING: f32 = 8.0;
#[cfg(feature = "gps-serial")]
const GPS_BAUD_RATES: &[u32] = &[4800, 9600, 38400, 115200];

/// The storm motion override switch's label.
pub(crate) const STORM_MOTION_OVERRIDE_LABEL: &str = "Override the storm motion vector";

/// What actually leaves the machine when the user says yes, in the pane that
/// asks.
#[cfg(target_os = "linux")]
const LOCATION_EGRESS_NOTE: &str = "Approximate, from your system's location \
    service. Finding a position sends your IP address, and - if the Wi-Fi \
    backend is enabled - the identifiers of nearby wireless networks, to \
    api.beacondb.net.";
#[cfg(not(target_os = "linux"))]
const LOCATION_EGRESS_NOTE: &str = "Approximate, from your device's location \
    service. Finding a position may send your IP address and details of nearby \
    wireless networks to that service's provider.";

/// Where a user actually goes to undo a refusal, in the pane that reports one.
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
pub(crate) const SETTINGS_ROWS: &[&str] = &[
    "units.timezone",
    "units.temperature",
    "units.speed",
    "units.distance",
    "units.height",
    "units.precip_rate",
    "units.hail_size",
    "interface.pin_controls",
    "interface.diagnostics",
    "location",
    "gps.port",
    "gps.baud",
    "gps.connect",
    "heading",
    "storm.fallback",
    "storm.override",
    "storm.speed",
    "storm.direction",
    "data.auto_poll",
    "memory.gpu",
    "memory.system",
    "offline.areas",
    "about.version",
    "about.platform",
    "reset",
    "about.exit",
];

/// One settings row the window actually drew: which [`SETTINGS_ROWS`] id it
/// was, and where it landed so a test can find it on screen.
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

/// Whether a `DragValue` is mid-edit — being dragged, or holding the keyboard
/// while a number is typed into it.
fn mid_edit(response: &egui::Response) -> bool {
    response.dragged() || response.has_focus()
}

impl super::Gui {
    /// The settings content — the inspector's App › Settings body.
    pub(super) fn render_settings_body(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        self.storm_motion_editing = false;
        for &row in SETTINGS_ROWS {
            #[cfg(test)]
            let row_top = ui.cursor().top();
            let drawn = self.render_settings_row(ui, row, actions);
            #[cfg(test)]
            if drawn {
                self.probes.last_settings_rows.push(DrawnSettingsRow {
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
    }

    /// Draw one row of [`SETTINGS_ROWS`]. Returns whether anything was drawn,
    /// which is `false` only for a row this build compiles out (the GPS rows
    /// without the `gps-serial` feature) or this platform withholds (the Exit
    /// row where [`Gui::supports_exit`](super::Gui::supports_exit) says no —
    /// the same gate that drops the menu's Exit entry).
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
            "interface.pin_controls" => {
                section_break(ui);
                ui.heading("Interface");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.checkbox(&mut self.pin_pane_controls, "Pin pane controls");
                ui.label(
                    egui::RichText::new(
                        "Unpinned, each pane's pill row idles translucent and \
                         wakes when the pointer is over the pane - or, \
                         on touch, on a first tap.",
                    )
                    .small()
                    .weak(),
                );
                true
            }
            "interface.diagnostics" => {
                ui.checkbox(&mut self.diagnostics_panel, "Show frame diagnostics");
                ui.label(
                    egui::RichText::new(
                        "An overlay of frame service and cadence percentiles \
                         over a trailing two-second window.",
                    )
                    .small()
                    .weak(),
                );
                true
            }
            "location" => {
                section_break(ui);
                ui.heading("Location");
                ui.add_space(SETTINGS_SMALL_SPACING);
                self.render_location_controls(ui, actions);
                section_break(ui);
                true
            }
            #[cfg(feature = "gps-serial")]
            "gps.port" => {
                ui.heading("GPS");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.horizontal(|ui| {
                    ui.label("Port:");
                    // The scanner, never `detect_gps_ports` directly: that walk
                    // is 25 ms of udev on a desktop, and this row draws every
                    // frame the pane is open.
                    let ports = gps_port_options(self.gps_ports.ports());
                    let selected = gps_port_label(&ports, self.serial_config.port_path.as_deref());
                    egui::ComboBox::from_id_salt("gps_port")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (value, label) in &ports {
                                ui.selectable_value(
                                    &mut self.serial_config.port_path,
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
                    let baud_label = if self.serial_config.auto_baud() {
                        "Auto-detect".to_string()
                    } else {
                        self.serial_config.baud_rate.to_string()
                    };
                    egui::ComboBox::from_id_salt("gps_baud")
                        .selected_text(baud_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.serial_config.baud_rate,
                                0,
                                "Auto-detect",
                            );
                            for &rate in GPS_BAUD_RATES {
                                ui.selectable_value(
                                    &mut self.serial_config.baud_rate,
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

                if ui.button("Connect GPS").clicked() {
                    actions.push(GuiAction::StartGps {
                        config: self.serial_config.clone(),
                    });
                }
                if ui.button("Disconnect GPS").clicked() {
                    actions.push(GuiAction::StopGps);
                }

                ui.add_space(SETTINGS_SMALL_SPACING);

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
            "heading" => {
                ui.horizontal(|ui| {
                    ui.label("Heading:");
                    egui::ComboBox::from_id_salt("heading_source")
                        .selected_text(self.heading_source.label())
                        .show_ui(ui, |ui| {
                            for &src in HeadingSource::ALL {
                                ui.selectable_value(&mut self.heading_source, src, src.label());
                            }
                        });
                });
                true
            }
            "storm.fallback" => {
                section_break(ui);
                ui.heading("Storm motion");
                ui.add_space(SETTINGS_SMALL_SPACING);
                let fallback = &mut self.srv_fallback;
                ui.horizontal(|ui| {
                    ui.label("When none is published:");
                    egui::ComboBox::from_id_salt("srv_fallback")
                        .selected_text(fallback.source().label())
                        .show_ui(ui, |ui| {
                            for choice in [
                                squallar_radar::srv::SrvFallback::MeanWind,
                                squallar_radar::srv::SrvFallback::BunkersRightMover,
                            ] {
                                ui.selectable_value(fallback, choice, choice.source().label());
                            }
                        });
                })
                .response
                .on_hover_text(
                    "Most volumes carry the National Weather Service's own storm motion \
                     and it is used whenever it does. This is what stands in when one \
                     does not: the 0-6 km mean wind, which measures closest to it, or \
                     the Bunkers right-mover, a supercell motion prediction that can \
                     point somewhere quite different in weak flow.",
                );
                true
            }
            "storm.override" => {
                ui.checkbox(
                    &mut self.storm_motion_override.enabled,
                    STORM_MOTION_OVERRIDE_LABEL,
                )
                .on_hover_text(
                    "On, storm-relative velocity uses the vector below and nothing else \
                     - in the plan view, the 3D volume and the cross-section alike, and \
                     ahead of the National Weather Service's own.",
                );
                true
            }
            "storm.speed" => {
                let motion = &mut self.storm_motion_override;
                let widget = ui
                    .add_enabled_ui(motion.enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Speed:");
                            ui.add(
                                egui::DragValue::new(&mut motion.speed_kt)
                                    .speed(0.5)
                                    .range(0.0..=squallar_radar::srm::MAX_OVERRIDE_SPEED_KT)
                                    .suffix(" kt"),
                            )
                        })
                        .inner
                    })
                    .inner;
                self.storm_motion_editing |= mid_edit(&widget);
                true
            }
            "storm.direction" => {
                let motion = &mut self.storm_motion_override;
                let widget = ui
                    .add_enabled_ui(motion.enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("From:");
                            ui.add(
                                egui::DragValue::new(&mut motion.direction_deg)
                                    .speed(1.0)
                                    .range(0.0..=360.0)
                                    .suffix("\u{00b0}"),
                            )
                        })
                        .inner
                    })
                    .inner;
                self.storm_motion_editing |= mid_edit(&widget);
                true
            }
            "data.auto_poll" => {
                section_break(ui);
                ui.heading("Data & live");
                ui.add_space(SETTINGS_SMALL_SPACING);
                // Through the layer's own control, not a field beside it: this
                // row and the ☰ menu's leaf are two surfaces over one switch,
                // and a copy here is the drift the no-copy rule forbids.
                let mut enabled = crate::radar_layer::auto_poll_enabled(&self.overlays);
                if ui
                    .checkbox(&mut enabled, squallar_radar::source::AUTO_POLL_LABEL)
                    .changed()
                {
                    self.set_auto_poll_enabled(enabled);
                }
                true
            }
            "memory.gpu" => {
                section_break(ui);
                ui.heading("Memory");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.label(
                    egui::RichText::new(
                        // Deliberately not an ordered list of what sheds
                        // first: `squallar_device_profile::budget::LADDER`
                        // decides that, it differs per capacity arm, and the
                        // tile caches move off the economy allowance rather
                        // than off the ladder at all. A caption naming an
                        // order would be wrong on some machine.
                        // **Not the loop's length.** It said "and the
                        // loop's length step down too" until 2026-09-06;
                        // ruling 15 made a loop's frame count tier 1, so no
                        // rung of the ladder touches it and a caption saying
                        // otherwise would be describing a lever that no
                        // longer exists. What a loop cannot reach is said
                        // below, per pane, in frames.
                        "The most of each memory this app will take. Lowering \
                         one leaves it less room to work in: the map tile \
                         caches shrink, and past that the picture quality \
                         and the 3D detail step down too.",
                    )
                    .small()
                    .weak(),
                );
                let pool = self.budget_readout.as_ref().map(|readout| readout.gpu);
                let before = self.memory_percents;
                memory_share_widget(
                    ui,
                    "memory_gpu",
                    "GPU memory",
                    &mut self.memory_percents.gpu,
                    pool,
                );
                if before != self.memory_percents {
                    actions.push(GuiAction::SetMemoryPercents(self.memory_percents));
                }
                true
            }
            "memory.system" => {
                let pool = self
                    .budget_readout
                    .as_ref()
                    .and_then(|readout| readout.host);
                let before = self.memory_percents;
                memory_share_widget(
                    ui,
                    "memory_host",
                    "System memory",
                    &mut self.memory_percents.host,
                    pool,
                );
                if before != self.memory_percents {
                    actions.push(GuiAction::SetMemoryPercents(self.memory_percents));
                }
                if let Some(line) = self
                    .budget_readout
                    .as_ref()
                    .and_then(|readout| loop_span_caption(&readout.panes))
                {
                    ui.label(egui::RichText::new(line).small().weak());
                }
                true
            }
            "offline.areas" => {
                section_break(ui);
                // Every area is a SUB-row of this one id: `SETTINGS_ROWS` is
                // `&'static` and the area list is not, so one id per area
                // cannot be spelled. The screen draws its own empty state,
                // which is what keeps this row on the glass for the parity
                // walk's fresh `Gui`.
                self.render_downloaded_areas(ui);
                true
            }
            "about.version" => {
                section_break(ui);
                ui.heading("About");
                ui.add_space(SETTINGS_SMALL_SPACING);
                ui.label(concat!("Squallar ", env!("CARGO_PKG_VERSION")));
                true
            }
            "about.platform" => {
                ui.label(
                    egui::RichText::new(
                        "Runs on Linux, macOS, Windows, the web, Android, iOS and BSD.",
                    )
                    .small()
                    .weak(),
                );
                true
            }
            "reset" => {
                ui.add_space(SETTINGS_SMALL_SPACING);
                if ui.button("Reset to defaults").clicked() {
                    self.preferences = UserPreferences::default();
                    self.serial_config = squallar_nmea_serial::SerialConfig::default();
                    self.heading_source = squallar_location::HeadingSource::default();
                    self.storm_motion_override = crate::StormMotionOverride::default();
                    self.srv_fallback = squallar_radar::srv::SrvFallback::default();
                    // The App holds its own copy and prices with it, so the
                    // reset has to be told as well as written down — a reset
                    // that only moved the slider would leave this session
                    // running at the old share until the next restart.
                    self.memory_percents = squallar_device_profile::scene::PoolPercents::default();
                    actions.push(GuiAction::SetMemoryPercents(self.memory_percents));
                    actions.push(GuiAction::RequestLocation);
                }
                true
            }
            "about.exit" => {
                if !self.supports_exit {
                    return false;
                }
                ui.add_space(SETTINGS_SMALL_SPACING);
                if ui.button("Exit").clicked() {
                    actions.push(GuiAction::Exit);
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
        use squallar_location::LocationPermission;

        match self.location_permission {
            LocationPermission::Unavailable => {
                ui.label("Not available on this platform.");
            }
            LocationPermission::Unknown => {
                ui.label("Checking...");
            }
            LocationPermission::Denied => {
                ui.label("Denied.");
                ui.label(LOCATION_DENIED_NOTE);
                if self.location_settings_available && ui.button("Open location settings").clicked()
                {
                    actions.push(GuiAction::OpenLocationSettings);
                }
            }
            LocationPermission::Granted if self.location_active => {
                ui.label("On.");
                if ui.button("Turn off").clicked() {
                    actions.push(GuiAction::StopLocation);
                }
            }
            LocationPermission::Prompt | LocationPermission::Granted => {
                if ui.button("Use my location").clicked() {
                    actions.push(GuiAction::RequestLocation);
                }
            }
        }

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
    fn location_fix_summary(&self) -> Option<String> {
        if !self.location_active && self.user_fix.is_none() {
            return None;
        }
        let Some(at) = self.user_fix_at else {
            return Some("Waiting for a fix...".to_owned());
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
#[cfg(feature = "gps-serial")]
fn gps_port_options(
    ports: impl IntoIterator<Item = squallar_nmea_serial::GpsPortInfo>,
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
#[cfg(feature = "gps-serial")]
fn gps_port_label(ports: &[(Option<String>, String)], selected: Option<&str>) -> String {
    ports
        .iter()
        .find(|(value, _)| value.as_deref() == selected)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| selected.unwrap_or("Auto-detect").to_owned())
}

/// **One memory-share control: the slider, and what the machine actually
/// allowed beside it.**
///
/// The caption is the mandatory half. A percentage alone is unreadable now
/// that three terms can lower a pool — the user's own setting, the hardware,
/// and the page heap's governor — and it is the whole mitigation for someone
/// who set 20 % months ago and forgot: the figure in force is stated beside
/// the figure asked for, with the binding term named.
///
/// **The UI paints this; it does not price it.** Every figure in the caption
/// is read off `crate::shell_api::PoolReadout`, composed in `squallar-app`.
fn memory_share_widget(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    percent: &mut u8,
    pool: Option<crate::shell_api::PoolReadout>,
) {
    use squallar_device_profile::scene::PoolPercents;

    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        // A pushed id rather than the label's: two sliders over the same
        // range in one window would otherwise share a drag state.
        ui.push_id(id, |ui| {
            ui.add(
                egui::Slider::new(percent, PoolPercents::FLOOR..=100)
                    .step_by(5.0)
                    .suffix(" %"),
            );
        });
    });
    ui.label(
        egui::RichText::new(memory_share_caption(pool, *percent))
            .small()
            .weak(),
    );
}

/// The line under one memory-share slider: what is actually in force, and
/// which of the three terms is holding it there.
///
/// Absence is stated as absence rather than guessed at — a machine that
/// reports no figure for a pool is told so, not shown an invented percentage.
fn memory_share_caption(pool: Option<crate::shell_api::PoolReadout>, requested: u8) -> String {
    use squallar_device_profile::scene::PoolBinder;

    let Some(pool) = pool else {
        return "No figure for this memory yet.".to_owned();
    };
    if pool.requested_percent != Some(requested) {
        // The readout is composed on the telemetry tick, not the frame, so a
        // just-moved slider is genuinely not in force yet. Saying so beats
        // printing the old figure under the new number.
        return format!("Applying {requested} %...");
    }
    let Some(effective) = pool.effective_percent else {
        return "This machine reports no size for this memory, so the share \
                applies to nothing here."
            .to_owned();
    };
    let held = match pool.binder {
        PoolBinder::Hardware => "all this machine reports",
        PoolBinder::UserPercent => "your setting",
        PoolBinder::Governor if pool.recovering => "memory pressure, recovering",
        PoolBinder::Governor => "memory pressure",
    };
    format!(
        "{requested} % asked for, {effective} % in force ({held}): {} MiB.",
        pool.capacity_bytes / (1024 * 1024),
    )
}

/// **What a pane's loop asked for and what it got**, when the two differ —
/// `None` when every looping pane reaches its whole span, which is the
/// ordinary case and wants no line at all.
///
/// Ruling 13 makes a loop's lookback tier 1: no governor shortens it, and a
/// span this machine cannot reach is *"refused at admission, visibly"*. The
/// door that refuses it is not built yet; this is the visibility half, and it
/// is here rather than beside the pane because what it names is a property of
/// the memory the sliders above hand out.
///
/// One line for the whole scene, naming the worst pane, because a per-pane
/// list under a settings slider is a table nobody reads and the figure a user
/// acts on is the largest cut.
fn loop_span_caption(panes: &[crate::shell_api::PaneBudget]) -> Option<String> {
    let worst = panes
        .iter()
        .enumerate()
        .filter(|(_, pane)| pane.loop_span_clamped())
        .max_by_key(|(_, pane)| pane.loop_frames_requested - pane.loop_frames_effective)?;
    let (idx, pane) = worst;
    Some(format!(
        "Pane {} asked for {} loop frames and this machine holds {}.",
        idx + 1,
        pane.loop_frames_requested,
        pane.loop_frames_effective,
    ))
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
        gps_port_options([squallar_nmea_serial::GpsPortInfo {
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
    #[test]
    fn an_unplugged_port_is_still_named() {
        assert_eq!(gps_port_label(&ports(), Some("/dev/ttyS9")), "/dev/ttyS9");
        assert_eq!(gps_port_label(&[], None), "Auto-detect");
    }
}

#[cfg(test)]
mod memory_share_tests {
    use super::*;
    use crate::shell_api::PoolReadout;
    use squallar_device_profile::scene::{PoolBinder, PoolPercents};

    /// A pool the App has priced, at `requested` %, held by `binder`.
    fn pool(requested: u8, effective: u8, binder: PoolBinder, recovering: bool) -> PoolReadout {
        PoolReadout {
            capacity_bytes: 4 * 1024 * 1024 * 1024,
            requested_percent: Some(requested),
            effective_percent: Some(effective),
            binder,
            recovering,
            ..PoolReadout::default()
        }
    }

    /// **Every binder is named, and each is named differently.** The whole
    /// point of the line is that three terms can lower a pool and a bare
    /// percentage cannot say which did.
    #[test]
    fn each_binding_term_is_named_and_no_two_read_alike() {
        let lines: Vec<String> = [
            (PoolBinder::Hardware, false),
            (PoolBinder::UserPercent, false),
            (PoolBinder::Governor, false),
            (PoolBinder::Governor, true),
        ]
        .into_iter()
        .map(|(binder, recovering)| {
            memory_share_caption(Some(pool(60, 60, binder, recovering)), 60)
        })
        .collect();
        for line in &lines {
            assert!(
                line.contains("60 % asked for") && line.contains("60 % in force"),
                "the line states neither the request nor the figure in force: {line}",
            );
        }
        let mut distinct = lines.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            lines.len(),
            "two binding terms read identically, so the reader cannot tell \
             them apart: {lines:?}",
        );
    }

    /// **A governor that is coming back up says so.** The modulation can rise
    /// again; a bare "memory pressure" reads like a wall.
    #[test]
    fn a_recovering_governor_is_not_reported_as_a_wall() {
        let stuck = memory_share_caption(Some(pool(80, 40, PoolBinder::Governor, false)), 80);
        let lifting = memory_share_caption(Some(pool(80, 40, PoolBinder::Governor, true)), 80);
        assert!(stuck.contains("memory pressure"), "{stuck}");
        assert!(
            lifting.contains("recovering"),
            "a governor with readings banked toward a promotion reads as \
             terminal: {lifting}",
        );
        assert!(
            !stuck.contains("recovering"),
            "a governor with nothing banked claims to be recovering: {stuck}",
        );
    }

    /// **A slider just moved is not reported as in force.** The readout is
    /// composed on the telemetry tick, not the frame, so for up to one period
    /// the App is genuinely still pricing the old figure — printing it under
    /// the new number would read as a broken control.
    #[test]
    fn a_share_the_app_has_not_priced_yet_says_so() {
        let line = memory_share_caption(Some(pool(100, 100, PoolBinder::Hardware, false)), 45);
        assert!(
            line.contains("45"),
            "the line names no figure at all: {line}"
        );
        assert!(
            !line.contains("in force"),
            "a share the App has not seen was reported as in force: {line}",
        );
    }

    /// Absence is stated as absence. A machine that reports no size for a
    /// pool is told so, not shown an invented percentage.
    #[test]
    fn a_pool_with_no_figure_is_not_given_one() {
        assert!(memory_share_caption(None, 100).contains("No figure"));
        let unsized_pool = PoolReadout {
            requested_percent: Some(100),
            effective_percent: None,
            ..PoolReadout::default()
        };
        let line = memory_share_caption(Some(unsized_pool), 100);
        assert!(
            line.contains("no size"),
            "a pool with no figure was given one: {line}",
        );
    }

    /// **"Reset to defaults" resets this too**, through the button a user
    /// actually presses.
    ///
    /// The `"reset"` arm is a hand-kept list of fields; nothing makes adding a
    /// setting add a line to it, so a reset that silently forgets one is the
    /// failure mode, and it is invisible without this.
    #[test]
    fn resetting_to_defaults_restores_the_whole_of_both_pools() {
        let mut h = crate::input_harness::InputHarness::with_screen(egui::vec2(900.0, 1600.0));
        h.gui_mut().memory_percents = PoolPercents { gpu: 20, host: 30 };
        h.open_settings();

        let pos = h
            .inspector_rect()
            .expect("the settings body is in the inspector")
            .center();
        let found = h.scroll_until(pos, egui::vec2(0.0, -160.0), 120, |h| {
            h.painted_text_rects().iter().any(|(rect, text)| {
                text == "Reset to defaults" && h.screen_rect().contains(rect.center())
            })
        });
        assert!(found, "the Reset to defaults button never came on screen");

        let button = h
            .painted_text_rects()
            .into_iter()
            .find(|(_, text)| text == "Reset to defaults")
            .expect("the button was just found on screen")
            .0;
        h.clear_actions();
        h.mouse_click(button.center());

        assert_eq!(
            h.gui().memory_percents,
            PoolPercents::FULL,
            "a reset left the user on a share they had asked to forget",
        );
        assert!(
            h.last_actions().iter().any(|action| matches!(
                action,
                GuiAction::SetMemoryPercents(percents) if *percents == PoolPercents::FULL
            )),
            "the reset never told the App, so this session keeps pricing at \
             the old share until a restart; it emitted [{}]",
            h.last_actions()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    /// The storm motion switch must not name a source the app no longer has.
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
