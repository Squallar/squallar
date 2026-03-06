use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct};

/// Identifies each toggleable overlay layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LayerKind {
    Radar,
    SpcCategorical,
    SpcTornado,
    SpcWind,
    SpcHail,
    SpcProbabilistic,
    CityLabels,
    RadarSites,
}

impl LayerKind {
    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            LayerKind::Radar => "Radar",
            LayerKind::SpcCategorical => "Categorical",
            LayerKind::SpcTornado => "Tornado",
            LayerKind::SpcWind => "Wind",
            LayerKind::SpcHail => "Hail",
            LayerKind::SpcProbabilistic => "Probabilistic",
            LayerKind::CityLabels => "City Labels",
            LayerKind::RadarSites => "Radar Sites",
        }
    }

    /// Whether this layer is an SPC outlook product.
    pub fn is_spc(self) -> bool {
        matches!(
            self,
            LayerKind::SpcCategorical
                | LayerKind::SpcTornado
                | LayerKind::SpcWind
                | LayerKind::SpcHail
                | LayerKind::SpcProbabilistic
        )
    }

    /// Convert to the corresponding `OutlookProduct`, if this is an SPC layer.
    pub fn to_outlook_product(self) -> Option<OutlookProduct> {
        match self {
            LayerKind::SpcCategorical => Some(OutlookProduct::Categorical),
            LayerKind::SpcTornado => Some(OutlookProduct::Tornado),
            LayerKind::SpcWind => Some(OutlookProduct::Wind),
            LayerKind::SpcHail => Some(OutlookProduct::Hail),
            LayerKind::SpcProbabilistic => Some(OutlookProduct::Probabilistic),
            _ => None,
        }
    }
}

/// Per-layer state (enabled, and future: opacity).
#[derive(Debug, Clone)]
pub struct LayerState {
    pub enabled: bool,
    pub opacity: f32,
}

impl LayerState {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            opacity: 1.0,
        }
    }
}

/// Manages the set of overlay layers and their toggle states.
pub struct LayerManager {
    layers: std::collections::BTreeMap<LayerKind, LayerState>,
    /// Which SPC outlook day is selected.
    pub spc_day: OutlookDay,
}

impl Default for LayerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerManager {
    pub fn new() -> Self {
        use std::collections::BTreeMap;
        let mut layers = BTreeMap::new();

        layers.insert(LayerKind::Radar, LayerState::new(true));
        layers.insert(LayerKind::SpcCategorical, LayerState::new(false));
        layers.insert(LayerKind::SpcTornado, LayerState::new(false));
        layers.insert(LayerKind::SpcWind, LayerState::new(false));
        layers.insert(LayerKind::SpcHail, LayerState::new(false));
        layers.insert(LayerKind::SpcProbabilistic, LayerState::new(false));
        layers.insert(LayerKind::CityLabels, LayerState::new(true));
        layers.insert(LayerKind::RadarSites, LayerState::new(false));

        Self {
            layers,
            spc_day: OutlookDay::Day1,
        }
    }

    pub fn is_enabled(&self, kind: LayerKind) -> bool {
        self.layers
            .get(&kind)
            .map(|s| s.enabled)
            .unwrap_or(false)
    }

    pub fn set_enabled(&mut self, kind: LayerKind, enabled: bool) {
        if let Some(state) = self.layers.get_mut(&kind) {
            state.enabled = enabled;
        }
    }

    pub fn toggle(&mut self, kind: LayerKind) {
        if let Some(state) = self.layers.get_mut(&kind) {
            state.enabled = !state.enabled;
        }
    }

    /// Get a mutable reference to the enabled flag for use with egui checkbox.
    pub fn enabled_mut(&mut self, kind: LayerKind) -> &mut bool {
        &mut self.layers.entry(kind).or_insert(LayerState::new(false)).enabled
    }

    /// Returns true if any SPC outlook layer is enabled.
    pub fn any_spc_enabled(&self) -> bool {
        self.layers
            .iter()
            .any(|(kind, state)| kind.is_spc() && state.enabled)
    }

    /// Get all currently enabled SPC product types.
    pub fn enabled_spc_products(&self) -> Vec<OutlookProduct> {
        self.layers
            .iter()
            .filter(|(kind, state)| kind.is_spc() && state.enabled)
            .filter_map(|(kind, _)| kind.to_outlook_product())
            .collect()
    }

    /// Return the SPC-relevant `LayerKind` variants for the current day.
    /// Day 1 has separate tornado/wind/hail; Day 2/3 use combined probabilistic.
    pub fn spc_layers_for_day(&self) -> Vec<LayerKind> {
        match self.spc_day {
            OutlookDay::Day1 => vec![
                LayerKind::SpcCategorical,
                LayerKind::SpcTornado,
                LayerKind::SpcWind,
                LayerKind::SpcHail,
            ],
            OutlookDay::Day2 | OutlookDay::Day3 => vec![
                LayerKind::SpcCategorical,
                LayerKind::SpcProbabilistic,
            ],
        }
    }
}
