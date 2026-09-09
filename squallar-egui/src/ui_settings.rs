use crate::actions::GuiAction;
use crate::ui_hover::HoverTip;
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

/// **The switch that puts the budget system's figures on the glass.**
///
/// "figures" and not "diagnostics": these are quantities about the user's own
/// scene — what the layers they chose cost — and the row above is already the
/// developer instrument. Naming both "diagnostics" would have made one switch
/// read as a mode of the other.
pub(crate) const MEMORY_FIGURES_LABEL: &str = "Show memory figures";

/// The caption under [`MEMORY_FIGURES_LABEL`]. It names **where** the figures
/// appear rather than what they mean, because a reader meeting this row cold
/// has not seen them: the two places are the layers menu and the pane corner,
/// and a caption that only said "memory use" would leave them hunting.
pub(crate) const MEMORY_FIGURES_NOTE: &str = "Each layer's memory use under \
    its name in the Layers panel, and each pane's own total in its top-right \
    corner.";

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
    "interface.memory_figures",
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
    "memory.texture_ceiling",
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
            // Both surfaces of the budget readout under one switch, because
            // they are one thing to the reader: what the scene on screen costs
            // in memory. Off unless it is asked for.
            "interface.memory_figures" => {
                ui.checkbox(&mut self.memory_figures, MEMORY_FIGURES_LABEL);
                ui.label(egui::RichText::new(MEMORY_FIGURES_NOTE).small().weak());
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
                .hover_text(
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
                .hover_text(
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
            "memory.texture_ceiling" => {
                // Read off before the `&mut` borrow of the setting, exactly as
                // the two share rows above read their pools.
                let raster = self.budget_readout.as_ref().map(|readout| readout.raster);
                let before = self.texture_ceiling;
                texture_ceiling_combo(ui, &mut self.texture_ceiling, raster);
                if before != self.texture_ceiling {
                    actions.push(GuiAction::SetTextureCeiling(self.texture_ceiling));
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
                    // Written down only, unlike the two below: nothing outside
                    // the `Gui` reads this switch, so there is no App-side copy
                    // to tell. Off is its default, and a reset that left the
                    // figures on the glass would be a reset that did not
                    // restore one.
                    self.memory_figures = false;
                    // The App holds its own copy and prices with it, so the
                    // reset has to be told as well as written down — a reset
                    // that only moved the slider would leave this session
                    // running at the old share until the next restart.
                    self.memory_percents = squallar_device_profile::scene::PoolPercents::default();
                    actions.push(GuiAction::SetMemoryPercents(self.memory_percents));
                    // Told as well as written down, for the reason the shares
                    // above are: the App holds its own copy and re-fits on the
                    // action, so a reset that only moved the combo would leave
                    // this session rendering at the old ceiling until restart.
                    self.texture_ceiling =
                        squallar_device_profile::budget::TextureCeiling::default();
                    actions.push(GuiAction::SetTextureCeiling(self.texture_ceiling));
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

/// **The texture-size control: the ceiling the user allows any radar
/// raster, and what lowering it buys.**
///
/// A list of rungs rather than a slider, because the sides a raster can
/// actually take are powers of two and a continuous control would offer
/// hundreds of values that resolve to the same picture.
///
/// The caption is the mandatory half, on the memory sliders' terms: a bare
/// "1024 px" says nothing about why anyone would want it. What it must not
/// do is promise a frame count — how many frames the freed memory buys is a
/// property of the scene, and the line under the System memory slider above
/// is where this session's own answer is stated, measured rather than
/// predicted.
fn texture_ceiling_combo(
    ui: &mut egui::Ui,
    ceiling: &mut squallar_device_profile::budget::TextureCeiling,
    raster: Option<crate::shell_api::RasterSideReadout>,
) {
    use squallar_device_profile::budget::TextureCeiling;

    ui.horizontal(|ui| {
        ui.label("Texture size:");
        egui::ComboBox::from_id_salt("memory_texture_ceiling")
            .selected_text(texture_ceiling_label(*ceiling))
            .show_ui(ui, |ui| {
                for &px in TextureCeiling::OFFERED_PX {
                    let option = TextureCeiling::clamped(px);
                    let label = texture_ceiling_label(option);
                    ui.selectable_value(ceiling, option, label);
                }
            });
    });
    ui.label(
        egui::RichText::new(texture_ceiling_caption(raster, *ceiling))
            .small()
            .weak(),
    );
}

/// **The line under the texture-size control: what the user asked of every
/// radar raster, and the largest side one actually takes here.**
///
/// `memory_share_caption`'s shape, for `memory_share_caption`'s reason. A
/// ceiling is a figure the hardware may clamp below — the device class, the
/// fit's ladder and the adapter's own `max_texture_dimension_2d` each reach
/// lower than some rung this control offers — and a control showing only
/// what was asked for cannot say so. It **never overwrites the setting**:
/// the combo keeps reading the user's own choice whatever this line says,
/// which is what lets a user who moves to a bigger machine get the size they
/// asked for without touching the control again.
///
/// A hedge in place of the pair — "this device may already draw them
/// smaller" — is a sentence rather than a figure: true on every machine,
/// actionable on none, and silent on the one case the pair exists for.
fn texture_ceiling_caption(
    raster: Option<crate::shell_api::RasterSideReadout>,
    requested: squallar_device_profile::budget::TextureCeiling,
) -> String {
    // Absence is stated as absence rather than guessed at: a session whose
    // device has not answered has no side in force to report, and inventing
    // one is the defect `memory_share_caption` refuses in the same words.
    let no_figure = || match requested.side_px() {
        None => "Rasters take the largest size this device fits. Lower this \
                 to spend less memory on each radar picture and leave more \
                 for the rest of the scene."
            .to_owned(),
        Some(px) => format!(
            "Radar pictures are held to {px} px across. No figure yet for \
             the size this device reaches."
        ),
    };

    let Some(raster) = raster else {
        return no_figure();
    };
    if raster.requested != requested {
        // The readout is composed on the telemetry tick, not the frame, so a
        // just-changed combo is genuinely not in force yet — printing the old
        // figure under the new choice reads as a broken control.
        return match requested.side_px() {
            None => "Lifting the limit...".to_owned(),
            Some(px) => format!("Applying {px} px..."),
        };
    }
    let Some(effective) = raster.effective_side_px else {
        return no_figure();
    };
    match requested.side_px() {
        None => format!(
            "No limit asked for, at most {effective} px in force (all this \
             device reaches). Lower this to spend less memory on each radar \
             picture."
        ),
        // "at most", because the figure is a ceiling on every raster and not
        // the side each one takes: `raster_side` asks the sweep's own extent
        // and gate spacing first and is held to this only where that need
        // reaches it, so a near-range picture is smaller than this on every
        // machine.
        //
        // `effective` and not `px` on both arms, so the figure printed is the
        // one actually in force even if the invariant that puts it at or
        // below the request were ever broken upstream. The words are what
        // splits the two cases; the number is read off the readout either
        // way.
        Some(px) => {
            let held = if effective < px {
                "this device does not reach the size you asked for"
            } else {
                "your setting"
            };
            format!("{px} px asked for, at most {effective} px in force ({held}).")
        }
    }
}

/// The name one texture-size rung goes by. `None` is the neutral posture and
/// says so in words rather than as a number, because "4096" would read as a
/// choice the user made.
fn texture_ceiling_label(ceiling: squallar_device_profile::budget::TextureCeiling) -> String {
    match ceiling.side_px() {
        None => "No limit".to_owned(),
        Some(px) => format!("{px} px"),
    }
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
mod texture_ceiling_tests {
    use super::*;
    use crate::shell_api::RasterSideReadout;
    use squallar_device_profile::budget::TextureCeiling;

    /// The neutral posture is named in words. "4096 px" would read as a size
    /// the user picked, and the whole point of the default is that they have
    /// not picked one.
    #[test]
    fn the_neutral_rung_is_named_rather_than_numbered() {
        assert_eq!(texture_ceiling_label(TextureCeiling::NONE), "No limit");
        assert_eq!(texture_ceiling_label(TextureCeiling::default()), "No limit");
        assert_eq!(
            texture_ceiling_label(TextureCeiling::clamped(1024)),
            "1024 px",
        );
    }

    /// Every rung the combo offers is distinct once clamped, so the list has
    /// no two entries that select the same value and leave the user unable to
    /// tell which one is in force.
    #[test]
    fn every_offered_rung_is_a_distinct_setting() {
        let mut seen: Vec<TextureCeiling> = Vec::new();
        for px in TextureCeiling::OFFERED_PX {
            let rung = TextureCeiling::clamped(*px);
            assert!(
                !seen.contains(&rung),
                "{px} px clamps onto a rung already offered",
            );
            seen.push(rung);
        }
        assert!(
            seen.contains(&TextureCeiling::NONE),
            "the offered rungs do not include the neutral posture",
        );
    }

    /// The row is in the walked list, and the reset arm names it — a setting
    /// the reset button leaves alone is the defect this checks for.
    #[test]
    fn the_row_is_listed_and_the_reset_arm_names_it() {
        assert!(
            SETTINGS_ROWS.contains(&"memory.texture_ceiling"),
            "the texture ceiling row is not in SETTINGS_ROWS",
        );
        let source = include_str!("ui_settings.rs");
        let reset = source
            .split_once("\"reset\" => {")
            .expect("the reset arm")
            .1;
        let reset = reset.split_once("\"about.exit\"").expect("the exit arm").0;
        assert!(
            reset.contains("SetTextureCeiling"),
            "the reset arm does not reset the texture ceiling, so `Reset to \
             defaults` would leave this session at the old one",
        );
    }

    /// A readout composed under `requested`, on a device whose plan views
    /// reach `effective` px.
    fn priced(requested: TextureCeiling, effective: usize) -> RasterSideReadout {
        RasterSideReadout {
            requested,
            effective_side_px: Some(effective),
        }
    }

    /// **The mandatory arm: a device that will not reach the setting says so,
    /// beside the setting.**
    ///
    /// And the control that matters more than the arm itself — a device that
    /// *does* reach it must not be reported as falling short. Over-firing is
    /// the worse direction here: a line that cried "your machine is too
    /// small" on every machine would teach the user to stop reading it, and
    /// the one case it exists for would be the case they skipped.
    #[test]
    fn a_device_that_will_not_reach_the_setting_says_so_beside_it() {
        let asked = TextureCeiling::clamped(4096);
        let clamped = texture_ceiling_caption(Some(priced(asked, 2048)), asked);
        let reached = texture_ceiling_caption(Some(priced(asked, 4096)), asked);

        for line in [&clamped, &reached] {
            assert!(
                line.contains("4096 px asked for") && line.contains("in force"),
                "the line states neither the request nor the figure in \
                 force: {line}",
            );
        }
        assert!(
            clamped.contains("2048 px in force"),
            "the device's own figure is not on the line, so the user is \
             shown only what they asked for: {clamped}",
        );
        assert!(
            clamped.contains("does not reach"),
            "a device below the setting is not named as what held it: \
             {clamped}",
        );
        assert!(
            reached.contains("4096 px in force") && !reached.contains("does not reach"),
            "a device that reaches the setting is reported as clamping it: \
             {reached}",
        );
        assert_ne!(
            clamped, reached,
            "the clamped and the unclamped device read identically, so this \
             line cannot tell the user which they have",
        );
    }

    /// The neutral posture names the size in force too. A user who has asked
    /// for no ceiling still has one — the device's — and the whole argument
    /// for the pair is that a bare setting cannot say what it is.
    #[test]
    fn the_neutral_posture_still_names_the_size_in_force() {
        let line = texture_ceiling_caption(
            Some(priced(TextureCeiling::NONE, 8192)),
            TextureCeiling::NONE,
        );
        assert!(
            line.contains("8192 px in force"),
            "the device's own figure is not on the line: {line}",
        );
        assert!(
            !line.contains("does not reach"),
            "a session under no ceiling at all is reported as clamped: \
             {line}",
        );
    }

    /// **A ceiling just picked is not reported as in force.** The readout is
    /// composed on the telemetry tick, so for up to one period the App is
    /// genuinely still rendering at the old size — `memory_share_caption`'s
    /// "Applying 45 %..." for the same reason, and printing the old figure
    /// under the new choice would read as a control that does nothing.
    #[test]
    fn a_ceiling_the_app_has_not_priced_yet_says_so() {
        let asked = TextureCeiling::clamped(1024);
        let line = texture_ceiling_caption(Some(priced(TextureCeiling::NONE, 8192)), asked);
        assert!(
            line.contains("1024"),
            "the line names no figure at all: {line}",
        );
        assert!(
            !line.contains("in force"),
            "a ceiling the App has not seen was reported as in force: {line}",
        );
        assert!(
            !line.contains("8192"),
            "the figure from before the change was printed under the new \
             choice: {line}",
        );
    }

    /// Absence is stated as absence. A session whose device has not answered
    /// has no size in force to report, and inventing one is the defect
    /// `memory_share_caption` refuses in the same words.
    #[test]
    fn a_session_with_no_device_figure_is_not_given_one() {
        let asked = TextureCeiling::clamped(2048);
        let no_readout = texture_ceiling_caption(None, asked);
        let no_device = texture_ceiling_caption(
            Some(RasterSideReadout {
                requested: asked,
                effective_side_px: None,
            }),
            asked,
        );
        for line in [&no_readout, &no_device] {
            assert!(
                !line.contains("in force"),
                "a session with no figure was given one: {line}",
            );
            assert!(
                line.contains("2048 px"),
                "the line does not even name the setting: {line}",
            );
        }
        // The neutral posture keeps the copy that says what the control
        // buys — the one line on this control that is guidance rather than a
        // reading, and the only one a fresh install sees.
        let neutral = texture_ceiling_caption(None, TextureCeiling::NONE);
        assert!(
            neutral.contains("Lower this"),
            "a fresh install is told nothing about what this control does: \
             {neutral}",
        );
        assert!(
            !neutral.contains("in force"),
            "a session with no figure was given one: {neutral}",
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

/// **The switch that hides the budget system's figures**, as a settings row.
///
/// The figures themselves are covered where they are drawn
/// (`ui_stack::layer_memory_tests`, `ui_map_pane::pane_cost_tests`); what is
/// here is the control: that it exists, that it sits where the user asked for
/// it, that pressing it really moves the switch, and that a reset restores it.
#[cfg(test)]
mod memory_figures_tests {
    use super::*;

    /// **Directly under "Show frame diagnostics"**, which is where the user
    /// asked for it — a row that drifted to the end of the list would still
    /// draw, still persist and still be wrong.
    #[test]
    fn the_row_sits_immediately_below_the_diagnostics_switch() {
        let diagnostics = SETTINGS_ROWS
            .iter()
            .position(|id| *id == "interface.diagnostics")
            .expect("the diagnostics row is listed");
        assert_eq!(
            SETTINGS_ROWS.get(diagnostics + 1),
            Some(&"interface.memory_figures"),
            "the memory figures row is not the one under the diagnostics \
             switch; the list reads {SETTINGS_ROWS:?}",
        );
    }

    /// The reset arm names it, on the pattern
    /// `texture_ceiling_tests::the_row_is_listed_and_the_reset_arm_names_it`
    /// established: the arm is a hand-kept list of fields and nothing makes
    /// adding a setting add a line to it.
    ///
    /// It is written down and not also announced, unlike the shares and the
    /// ceiling in the same arm: those have a copy in the App that prices with
    /// them, and this is read only by the `Gui` that owns it.
    #[test]
    fn the_reset_arm_names_the_switch() {
        let source = include_str!("ui_settings.rs");
        let reset = source
            .split_once("\"reset\" => {")
            .expect("the reset arm")
            .1;
        let reset = reset.split_once("\"about.exit\"").expect("the exit arm").0;
        assert!(
            reset.contains("self.memory_figures = false;"),
            "the reset arm does not turn the memory figures off, so `Reset to \
             defaults` would leave them on the glass",
        );
    }

    /// **The switch is reachable and it works** — through the checkbox a user
    /// actually presses, found on screen by the words they actually read.
    ///
    /// Without this the setting could be listed, drawn, persisted and wired to
    /// nothing: `SETTINGS_ROWS` membership is a parity-walk property, not a
    /// proof that the widget moves the field.
    #[test]
    fn the_checkbox_turns_the_figures_on_and_off_again() {
        let mut h = crate::input_harness::InputHarness::with_screen(egui::vec2(900.0, 1600.0));
        h.open_settings();
        assert!(
            !h.gui().memory_figures,
            "premise: the switch starts off, which is what the user asked for",
        );

        let pos = h
            .inspector_rect()
            .expect("the settings body is in the inspector")
            .center();
        let found = h.scroll_until(pos, egui::vec2(0.0, -160.0), 120, |h| {
            h.painted_text_rects().iter().any(|(rect, text)| {
                text == MEMORY_FIGURES_LABEL && h.screen_rect().contains(rect.center())
            })
        });
        assert!(
            found,
            "{MEMORY_FIGURES_LABEL:?} never came on screen, so a user who \
             wants the figures back has no way to ask for them",
        );

        let label = h
            .painted_text_rects()
            .into_iter()
            .find(|(_, text)| text == MEMORY_FIGURES_LABEL)
            .expect("the label was just found on screen")
            .0;
        h.mouse_click(label.center());
        assert!(
            h.gui().memory_figures,
            "pressing the switch did not turn the figures on",
        );

        h.mouse_click(label.center());
        assert!(
            !h.gui().memory_figures,
            "the switch does not turn back off, so the user cannot undo it",
        );
    }

    /// The caption names both places the figures appear. A reader meeting the
    /// row cold has not seen them, and one that named only the layers menu
    /// would leave the pane corner unexplained.
    #[test]
    fn the_caption_names_both_surfaces() {
        assert!(
            MEMORY_FIGURES_NOTE.contains("Layers panel"),
            "the caption does not name the layers menu: {MEMORY_FIGURES_NOTE:?}",
        );
        assert!(
            MEMORY_FIGURES_NOTE.contains("pane"),
            "the caption does not name the pane's own line: {MEMORY_FIGURES_NOTE:?}",
        );
    }
}

#[cfg(test)]
#[path = "ui_settings/texture_ceiling_caption_tests.rs"]
mod texture_ceiling_caption_tests;
