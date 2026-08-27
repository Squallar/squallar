use egui::{
    DragPanButtons, InnerResponse, PointerButton, Response, Sense, Ui, UiBuilder, Vec2, Widget,
};

use crate::{
    MapMemory, Options, Plugin, Position, Projector, Tiles, center::Center,
    position::AdjustedPosition, tiles::draw_tiles,
};

struct Layer<'a> {
    tiles: &'a mut dyn Tiles,
    transparency: f32,
}

/// The frame time a wheel notch is scaled by when
/// [`Map::wheel_zoom_scales_with_frame_time`] is off: 60 Hz, the rate the
/// notch's size was chosen at.
pub const NOMINAL_FRAME_TIME: f32 = 1.0 / 60.0;

/// One frame's wheel scroll as a zoom delta, given the frame time the notch is
/// scaled by.
///
/// Split out from [`Map::zoom_delta`] so it can be exercised without an
/// [`egui::Context`]: everything above it is which `frame_scale` to pass, and
/// everything in it is the arithmetic that is the same either way.
fn wheel_zoom_delta(scroll_y: f32, frame_scale: f32) -> f64 {
    1.0 + f64::from(scroll_y * frame_scale) / 4.0
}

/// The actual map widget. Instances are to be created on each frame, as all necessary state is
/// stored in [`Tiles`] and [`MapMemory`].
///
/// # Examples
///
/// ```
/// # use walkers::{Map, Tiles, MapMemory, Position, lon_lat};
///
/// fn update(ui: &mut egui::Ui, tiles: &mut dyn Tiles, map_memory: &mut MapMemory) {
///     ui.add(Map::new(
///         Some(tiles), // `None`, if you don't want to show any tiles.
///         map_memory,
///         lon_lat(17.03664, 51.09916)
///     ));
/// }
/// ```
///
/// Initially, the map follows `my_position` argument which is typically fed by a GPS sensor or
/// other geo-localization method. If user drags the map, it enters a "detached state". You can use
/// [`MapMemory`]'s methods to change the state programmatically.
pub struct Map<'a, 'b, 'c> {
    tiles: Option<&'b mut dyn Tiles>,
    layers: Vec<Layer<'b>>,
    memory: &'a mut MapMemory,
    my_position: Position,
    plugins: Vec<Box<dyn Plugin + 'c>>,
    options: Options,
}

impl<'a, 'b, 'c> Map<'a, 'b, 'c> {
    pub fn new(
        tiles: Option<&'b mut dyn Tiles>,
        memory: &'a mut MapMemory,
        my_position: Position,
    ) -> Self {
        Self {
            tiles,
            layers: Vec::default(),
            memory,
            my_position,
            plugins: Vec::default(),
            options: Options::default(),
        }
    }

    /// Add plugin to the drawing pipeline. Plugins allow drawing custom shapes on the map.
    pub fn with_plugin(mut self, plugin: impl Plugin + 'c) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    /// Add a tile layer. All layers are drawn on top of each other with given transparency.
    pub fn with_layer(mut self, tiles: &'b mut dyn Tiles, transparency: f32) -> Self {
        self.layers.push(Layer {
            tiles,
            transparency,
        });
        self
    }

    /// Set whether map should perform zoom gesture.
    ///
    /// Zoom is typically triggered by the mouse wheel while holding <kbd>ctrl</kbd> key on native
    /// and web, and by pinch gesture on Android.
    pub fn zoom_gesture(mut self, enabled: bool) -> Self {
        self.options.zoom_gesture_enabled = enabled;
        self
    }

    /// Specify which pointer buttons can be used to pan by clicking and dragging.
    pub fn drag_pan_buttons(mut self, buttons: DragPanButtons) -> Self {
        self.options.drag_pan_buttons = buttons;
        self
    }

    /// Change how far to zoom in/out.
    /// Default value is 2.0
    pub fn zoom_speed(mut self, speed: f64) -> Self {
        self.options.zoom_speed = speed;
        self
    }

    /// Set whether to enable double click primary mouse button to zoom
    pub fn double_click_to_zoom(mut self, enabled: bool) -> Self {
        self.options.double_click_to_zoom = enabled;
        self
    }

    /// Set whether to enable double click secondary mouse button to zoom out
    pub fn double_click_to_zoom_out(mut self, enabled: bool) -> Self {
        self.options.double_click_to_zoom_out = enabled;
        self
    }

    /// Sets the zoom behaviour
    ///
    /// When enabled zoom is done with mouse wheel while holding <kbd>ctrl</kbd> key on native
    /// and web. Panning is done with mouse wheel without <kbd>ctrl</kbd> key
    ///
    /// When disabled, zooming can be done without holding <kbd>ctrl</kbd> key
    /// but panning with mouse wheel is disabled
    ///
    /// Has no effect on Android
    pub fn zoom_with_ctrl(mut self, enabled: bool) -> Self {
        self.options.zoom_with_ctrl = enabled;
        self
    }

    /// Set if we can pan with mouse wheel.
    /// By default, panning is disabled when zooming with ctrl is disabled.
    /// Allow to disable panning even when zooming with ctrl is enabled.
    pub fn panning(mut self, enabled: bool) -> Self {
        self.options.panning = enabled;
        self
    }

    /// Set whether a wheel notch's zoom is multiplied by how long the frame it
    /// arrived on took. Default is `true`, which is upstream's behaviour.
    ///
    /// **This is not a switch for "remove the multiplier".** The wheel term is
    /// `smooth_scroll_delta.y * frame_time / 4.0`, so dropping `frame_time`
    /// entirely would make one notch `y/2` zoom levels instead of `y/120` — a
    /// **60x** zoom per notch. What `false` does is substitute a *nominal*
    /// frame time, [`NOMINAL_FRAME_TIME`], for the measured one: the notch
    /// keeps the size it has on a 60 Hz display and stops changing size when
    /// the frame rate does.
    ///
    /// Wanted by any app whose frame time is not a constant — one whose frames
    /// are 4 ms when idle and 300 ms while a layer rasterises will otherwise
    /// zoom the map ~75x further per notch during the slow ones.
    pub fn wheel_zoom_scales_with_frame_time(mut self, enabled: bool) -> Self {
        self.options.wheel_zoom_scales_with_frame_time = enabled;
        self
    }

    /// Set the threshold for pulling the map back to `my_position` when dragged.
    ///
    /// It can be used to prevent the map from being accidentally detached when the user clicks on
    /// something causing a small drag.
    pub fn pull_to_my_position_threshold(mut self, threshold: f32) -> Self {
        self.options.pull_to_my_position_threshold = threshold;
        self
    }

    /// Show the map widget inside a [`egui::Ui`].
    pub fn show<R>(
        mut self,
        ui: &mut Ui,
        add_contents: impl FnOnce(&mut Ui, &Response, &Projector, &MapMemory) -> R,
    ) -> InnerResponse<R> {
        let (rect, mut response) =
            ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());

        // Read above `handle_gestures`, which needs it: a release that egui has
        // no smoothed pointer velocity for falls back to this frame's own drag
        // delta over the time this frame took.
        let delta_time = ui.input(|reader| reader.stable_dt);

        let mut changed = self.handle_gestures(ui, &response, delta_time);
        let zoom = self.memory.zoom;
        changed |= self
            .memory
            .center_mode
            .update_movement(delta_time, zoom.into());

        if changed {
            response.mark_changed();
            ui.request_repaint();
        }

        let map_center = self.position();
        let painter = ui.painter().with_clip_rect(rect);

        if let Some(tiles) = self.tiles {
            draw_tiles(&painter, map_center, zoom, tiles, 1.0);
        }

        for layer in self.layers {
            draw_tiles(&painter, map_center, zoom, layer.tiles, layer.transparency);
        }

        // Run plugins. `map_center` is the centre resolved above; nothing between here and
        // there touches `self.memory`, so the projector reuses it rather than resolving it
        // a second time.
        let projector = Projector::with_map_center(response.rect, self.memory, map_center);
        for (idx, plugin) in self.plugins.into_iter().enumerate() {
            let mut child_ui = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(idx));
            plugin.run(&mut child_ui, &response, &projector, self.memory);
        }

        let mut child_ui = ui.new_child(UiBuilder::new().max_rect(rect).id_salt("inner"));
        let inner = add_contents(&mut child_ui, &response, &projector, self.memory);

        InnerResponse { inner, response }
    }
}

impl Map<'_, '_, '_> {
    /// Handle user inputs and recalculate everything accordingly. Returns whether something changed.
    fn handle_gestures(&mut self, ui: &mut Ui, response: &Response, delta_time: f32) -> bool {
        let zoom_delta = self.zoom_delta(ui, response);

        // Zooming and dragging need to be exclusive, otherwise the map will get dragged when
        // pinch gesture is used.
        let changed = if (zoom_delta - 1.0).abs() > 0.001
            && ui.ui_contains_pointer()
            && self.options.zoom_gesture_enabled
        {
            // Displacement of mouse pointer relative to widget center
            let offset = input_offset(ui, response);

            // While zooming, we want to keep the location under the mouse pointer fixed on the
            // screen. To achieve this, we first move the location to the widget's center,
            // then adjust zoom level, finally move the location back to the original screen
            // position.
            if let Some(offset) = offset {
                // If map is tracking `my_position` and the input offset is close, just let it be.
                // Only the yes/no is wanted here; `detached()` would build a position, and
                // `self.position()` below builds the one that is actually used.
                if self.memory.center_mode.is_detached()
                    || offset.length() > self.options.pull_to_my_position_threshold
                {
                    self.memory.center_mode = Center::Exact(
                        AdjustedPosition::new(self.position()).shift(-offset, self.memory.zoom()),
                    );
                }
            }

            // Shift by 1 because of the values given by zoom_delta(). Multiple by zoom_speed(defaults to 2.0),
            // because then it felt right with both mouse wheel, and an Android phone.
            self.memory
                .zoom
                .zoom_by((zoom_delta - 1.) * self.options.zoom_speed);

            if let Some(offset) = offset {
                self.memory.center_mode = self
                    .memory
                    .center_mode
                    .clone()
                    .shift(offset, self.memory.zoom());
            }

            true
        } else {
            self.memory.center_mode.handle_gestures(
                response,
                self.my_position,
                self.options.pull_to_my_position_threshold,
                self.options.drag_pan_buttons,
                crate::center::InputFrame {
                    pointer_velocity: ui.input(|input| input.pointer.velocity()),
                    delta_time,
                },
            )
        };

        // Only enable panning with mouse_wheel if we are zooming with ctrl. But always allow touch devices to pan
        let panning_enabled =
            self.options.panning && (ui.input(|i| i.any_touches()) || self.options.zoom_with_ctrl);

        // `panning_enabled` is a field read; `ui_contains_pointer` is an O(layers) reverse
        // scan of the area order under egui's memory lock. Ask the cheap one first — it is
        // `false` for every `Map` this workspace builds, both of which set `.panning(false)`.
        if panning_enabled && ui.ui_contains_pointer() {
            // Panning by scrolling, e.g. two-finger drag on a touchpad:
            let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
            if scroll_delta != Vec2::ZERO {
                self.memory.center_mode = Center::Exact(
                    AdjustedPosition::new(self.position()).shift(scroll_delta, self.memory.zoom()),
                );
            }
        }

        changed
    }

    /// Calculate the zoom delta based on the input.
    fn zoom_delta(&self, ui: &mut Ui, response: &Response) -> f64 {
        let mut zoom_delta = ui.input(|input| input.zoom_delta()) as f64;

        if self.options.double_click_to_zoom
            && ui.ui_contains_pointer()
            && response.double_clicked_by(PointerButton::Primary)
        {
            zoom_delta = 2.0;
        }

        if self.options.double_click_to_zoom_out
            && ui.ui_contains_pointer()
            && response.double_clicked_by(PointerButton::Secondary)
        {
            zoom_delta = 0.0;
        }

        if !self.options.zoom_with_ctrl && zoom_delta == 1.0 {
            // We only use the raw scroll values, if we are zooming without ctrl,
            // and zoom_delta is not already over/under 1.0 (eg. a ctrl + scroll event or a pinch zoom)
            // These values seem to correspond to the same values as one would get in `zoom_delta()`
            let scales_with_frame_time = self.options.wheel_zoom_scales_with_frame_time;
            zoom_delta = ui.input(|input| {
                // A *value* is selected here, and the arithmetic below is the
                // same expression either way.
                let frame_scale = if scales_with_frame_time {
                    input
                        .stable_dt
                        .clamp(input.predicted_dt * 0.5, input.predicted_dt * 2.0)
                } else {
                    NOMINAL_FRAME_TIME
                };
                wheel_zoom_delta(input.smooth_scroll_delta.y, frame_scale)
            });
        };

        zoom_delta
    }

    /// Get the real position at the map's center.
    fn position(&self) -> Position {
        self.memory.center_mode.position(self.my_position)
    }
}

impl Widget for Map<'_, '_, '_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui, |_, _, _, _| ()).response
    }
}

/// Get the offset of the input (either mouse or touch) relative to the center.
fn input_offset(ui: &mut Ui, response: &Response) -> Option<Vec2> {
    let mouse_offset = response.hover_pos();
    let touch_offset = ui
        .input(|input| input.multi_touch())
        .map(|multi_touch| multi_touch.center_pos);

    // On touch we get both, so make touch the priority.
    touch_offset
        .or(mouse_offset)
        .map(|pos| pos - response.rect.center())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lon_lat;

    /// `Map::show` resolves the centre once, for the tile layers, and now hands the same
    /// value to the projector instead of letting it resolve its own. That is only sound
    /// while `Map::position` is exactly what `Projector::new` would have computed from
    /// `my_position` — this pins the two expressions together.
    #[test]
    fn the_maps_centre_is_the_one_the_projector_would_have_resolved() {
        let my_position = lon_lat(21., 52.);

        for detached_at in [None, Some(lon_lat(-122.4194, 37.7749))] {
            let mut memory = MapMemory::default();
            memory.set_zoom(10.).unwrap();
            if let Some(position) = detached_at {
                memory.center_at(position);
            }

            let expected = memory.center_mode.position(my_position);
            let map = Map::new(None, &mut memory, my_position);

            assert_eq!(expected, map.position());
        }
    }

    /// Following `my_position` and detached are genuinely different centres here, so the
    /// test above is not comparing a constant with itself.
    #[test]
    fn a_detached_map_is_not_centred_on_my_position() {
        let my_position = lon_lat(21., 52.);

        let mut memory = MapMemory::default();
        memory.set_zoom(10.).unwrap();
        assert_eq!(
            my_position,
            Map::new(None, &mut memory, my_position).position()
        );

        memory.center_at(lon_lat(-122.4194, 37.7749));
        assert_ne!(
            my_position,
            Map::new(None, &mut memory, my_position).position()
        );
    }

    /// The whole zoom a notch produces, in **zoom levels**: `zoom_by` is handed
    /// `(zoom_delta - 1) * zoom_speed`, and `zoom_speed` defaults to 2.
    fn notch_levels(scroll_y: f32, frame_scale: f32) -> f64 {
        (wheel_zoom_delta(scroll_y, frame_scale) - 1.0) * Options::default().zoom_speed
    }

    /// **The trap this option exists to avoid.** Turning the multiplier off is
    /// not deleting it: `frame_scale = 1.0` is a 60x zoom per notch, which is
    /// why the option substitutes a nominal frame time instead.
    #[test]
    fn dropping_the_frame_time_entirely_would_be_a_sixty_times_zoom() {
        let with_nominal = notch_levels(120.0, NOMINAL_FRAME_TIME);
        let with_none = notch_levels(120.0, 1.0);

        assert!(
            (with_nominal - 1.0).abs() < 1e-9,
            "a 120-point notch at the nominal frame time must be one zoom \
             level, not {with_nominal}",
        );
        assert!(
            (with_none / with_nominal - 60.0).abs() < 1e-6,
            "deleting the multiplier would zoom {}x per notch, not the 60x this \
             test claims - the arithmetic moved",
            with_none / with_nominal,
        );
    }

    /// With the option off, the notch is the same size at every frame time the
    /// app can hand it — which is the point, and is what the measured multiplier
    /// does not do.
    #[test]
    fn a_nominal_frame_time_makes_the_notch_the_same_at_any_frame_rate() {
        // A 4 ms idle frame and a 300 ms one mid-raster, and everything between.
        const MEASURED: [f32; 5] = [1.0 / 240.0, 1.0 / 120.0, 1.0 / 60.0, 1.0 / 30.0, 0.2895];

        for measured in MEASURED {
            assert!(
                (notch_levels(120.0, NOMINAL_FRAME_TIME) - 1.0).abs() < 1e-9,
                "the nominal notch moved on a {measured} s frame",
            );
        }

        // The control: the measured multiplier really does vary, or the
        // assertion above is about nothing.
        let measured: Vec<f64> = MEASURED.iter().map(|&s| notch_levels(120.0, s)).collect();
        let widest = measured.iter().copied().fold(f64::MIN, f64::max);
        let tightest = measured.iter().copied().fold(f64::MAX, f64::min);
        assert!(
            widest / tightest > 60.0,
            "the measured frame time spread the notch only {}x, so this test \
             is not comparing against a moving target: {measured:?}",
            widest / tightest,
        );
    }

    /// The option's default is upstream's behaviour, so a `Map` nobody
    /// configured behaves exactly as it did.
    #[test]
    fn the_frame_time_multiplier_is_on_by_default() {
        assert!(Options::default().wheel_zoom_scales_with_frame_time);

        let mut memory = MapMemory::default();
        let map = Map::new(None, &mut memory, lon_lat(21., 52.));
        assert!(map.options.wheel_zoom_scales_with_frame_time);

        let mut memory = MapMemory::default();
        let map =
            Map::new(None, &mut memory, lon_lat(21., 52.)).wheel_zoom_scales_with_frame_time(false);
        assert!(!map.options.wheel_zoom_scales_with_frame_time);
    }

    /// The gesture stays linear in how far the wheel turned, and signed.
    #[test]
    fn the_notch_follows_how_far_the_wheel_turned() {
        for (points, want) in [(240.0, 2.0), (120.0, 1.0), (60.0, 0.5), (-120.0, -1.0)] {
            let levels = notch_levels(points, NOMINAL_FRAME_TIME);
            assert!(
                (levels - want).abs() < 1e-9,
                "{points} points moved {levels} zoom levels, not {want}",
            );
        }
    }
}
