//! Declarative UI control descriptors for overlay handlers.
//!
//! Handlers return [`ControlItem`] trees to describe their UI controls.
//! The egui crate renders these generically without knowing which overlay
//! type produced them. User interactions flow back via [`ControlUpdate`].

/// A single UI control element. The egui crate matches on these to render
/// checkboxes, dropdowns, sliders, etc. generically.
#[derive(Debug, Clone)]
pub enum ControlItem {
    /// A boolean toggle (rendered as a checkbox).
    Toggle {
        id: &'static str,
        label: String,
        enabled: bool,
    },
    /// A collapsible group of nested controls.
    Section {
        label: String,
        collapsible: bool,
        expanded: bool,
        items: Vec<ControlItem>,
    },
    /// A dropdown selector with `(value, display_label)` options.
    Dropdown {
        id: &'static str,
        label: String,
        options: Vec<(String, String)>,
        selected: String,
    },
    /// A numeric slider.
    Slider {
        id: &'static str,
        label: String,
        min: f64,
        max: f64,
        value: f64,
        logarithmic: bool,
        /// Format string for display (e.g. "{:.0} fps", "{:.1}°").
        format: String,
    },
    /// A horizontal row of buttons.
    ButtonRow {
        buttons: Vec<ControlButton>,
    },
    /// Informational text (not interactive).
    InfoText {
        text: String,
    },
    /// A heading/label rendered at normal size (not interactive).
    Heading {
        text: String,
    },
    /// A visual separator line.
    Separator,
}

/// A button within a [`ControlItem::ButtonRow`].
#[derive(Debug, Clone)]
pub struct ControlButton {
    pub id: &'static str,
    pub label: String,
    pub enabled: bool,
    /// Whether this button should be visually highlighted (e.g. "Live" when active).
    pub highlight: bool,
}

/// A user interaction with an overlay's control.
/// Sent from the egui crate back to the handler via `apply_control()`.
#[derive(Debug, Clone)]
pub struct ControlUpdate {
    pub id: &'static str,
    pub value: ControlValue,
}

/// The value associated with a [`ControlUpdate`].
#[derive(Debug, Clone)]
pub enum ControlValue {
    Bool(bool),
    String(String),
    Float(f64),
    /// A stateless button press (no value, just the id).
    Action,
}

/// Context provided to handlers when building their control descriptors.
///
/// The handler uses this to adapt controls to the current pane context
/// (e.g. show different products based on what data is available).
pub struct PaneControlContext<'a> {
    pub pane_idx: usize,
    /// Per-pane handler state, if this handler created one via `create_pane_state()`.
    /// The handler downcasts this to its concrete state type.
    pub pane_state: Option<&'a dyn std::any::Any>,
}

/// Mutable version of [`PaneControlContext`] for applying control updates.
pub struct PaneControlContextMut<'a> {
    pub pane_idx: usize,
    pub pane_state: Option<&'a mut dyn std::any::Any>,
}

/// Side-effect returned by [`OverlayHandler::apply_control`] to signal
/// what the caller (the generic UI renderer) should do after the state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlEffect {
    /// No additional action needed (re-render will happen automatically
    /// if data_generation changed).
    #[default]
    None,
    /// The handler needs its data (re-)fetched.
    Fetch,
}
