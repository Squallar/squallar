use std::collections::{HashMap, HashSet};
use crate::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use crate::spc::discussion::SpcDiscussion;
use crate::nws::alert::NwsAlert;
use crate::types::{OverlayFeature, OverlayLabel};

/// Format an ISO 8601 timestamp into a shorter human-readable form.
fn format_iso_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.format("%b %d %Y %H:%M %Z").to_string())
        .unwrap_or_else(|_| iso.to_string())
}

/// Generic wrapper for overlay data that follows the fetch-cache-generation pattern.
///
/// Each overlay type (SPC outlooks, NWS alerts, SPC discussions) has the same
/// lifecycle: data is fetched asynchronously, cached locally, and invalidated
/// via a generation counter when new data arrives.  This struct captures that
/// shared pattern, reducing scattered fields on `Gui`.
pub struct OverlayState<T> {
    pub data: T,
    pub fetch_time: Option<std::time::Instant>,
    pub fetching: bool,
    pub data_generation: u64,
}

impl<T: Default> Default for OverlayState<T> {
    fn default() -> Self {
        Self {
            data: T::default(),
            fetch_time: None,
            fetching: false,
            data_generation: 0,
        }
    }
}

impl<T: Default> OverlayState<T> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> OverlayState<T> {
    /// Replace the data and bump the generation counter.
    pub fn set_data(&mut self, data: T) {
        self.data = data;
        self.fetch_time = Some(std::time::Instant::now());
        self.data_generation = self.data_generation.wrapping_add(1);
    }

    /// Whether a refresh is due (no data yet, or `interval` has elapsed since last fetch).
    pub fn needs_refresh(&self, interval_secs: u64) -> bool {
        self.fetch_time
            .map_or(true, |t| t.elapsed().as_secs() >= interval_secs)
    }
}

/// Identifies a clicked overlay item for the detail popup pager.
#[derive(Clone, Debug)]
pub enum SelectedOverlay {
    /// An NWS alert, identified by its stable API ID string.
    Alert(String),
    /// An SPC Mesoscale Discussion, identified by its stable MD number.
    Discussion(u32),
    /// An SPC convective outlook feature, identified by its short label.
    Outlook { label: String },
}

/// An overlay item that can be clicked and optionally labelled on the map.
///
/// Returned by `OverlayKind::clickable_items()` so that the UI crate can
/// perform hit-testing and label drawing without knowing overlay-specific types.
pub struct ClickableItem<'a> {
    /// Renderable polygon features for hit-testing.
    pub features: Vec<&'a OverlayFeature>,
    /// Optional map label to draw at a geographic position.
    pub label: Option<OverlayLabel>,
    /// The stable identifier to store when the user clicks this item.
    pub id: SelectedOverlay,
}

/// All shared overlay state: SPC outlooks, NWS alerts, SPC discussions,
/// and their selection/hidden state.
pub struct OverlayData {
    pub spc_outlooks: OverlayState<HashMap<(OutlookDay, OutlookProduct), SpcOutlook>>,
    /// Per-product SPC data generation (keyed separately for cache invalidation).
    pub spc_data_generation: HashMap<(OutlookDay, OutlookProduct), u64>,
    pub nws_alerts: OverlayState<Vec<NwsAlert>>,
    /// Overlay items under the click point (alerts and MDs combined) for the pager popup.
    pub selected_overlays: Vec<SelectedOverlay>,
    /// Current page index within `selected_overlays`.
    pub selected_overlay_page: usize,
    /// Alert IDs hidden by the user (not rendered on the map).
    pub hidden_alerts: HashSet<String>,
    pub spc_discussions: OverlayState<Vec<SpcDiscussion>>,
}

impl Default for OverlayData {
    fn default() -> Self {
        Self {
            spc_outlooks: OverlayState::new(),
            spc_data_generation: HashMap::new(),
            nws_alerts: OverlayState::new(),
            selected_overlays: Vec::new(),
            selected_overlay_page: 0,
            hidden_alerts: HashSet::new(),
            spc_discussions: OverlayState::new(),
        }
    }
}

impl OverlayData {
    /// Store a fetched SPC outlook in the cache.
    pub fn set_spc_outlook(&mut self, day: OutlookDay, product: OutlookProduct, outlook: SpcOutlook) {
        self.spc_outlooks.data.insert((day, product), outlook);
        self.spc_outlooks.fetch_time = Some(std::time::Instant::now());
        let generation = self.spc_data_generation.entry((day, product)).or_insert(0);
        *generation = generation.wrapping_add(1);
    }

    /// Set whether an SPC fetch is currently in progress.
    pub fn set_spc_fetching(&mut self, fetching: bool) {
        self.spc_outlooks.fetching = fetching;
    }

    /// Store fetched NWS alerts, replacing the previous set.
    pub fn set_nws_alerts(&mut self, alerts: Vec<NwsAlert>) {
        let current_ids: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
        self.hidden_alerts.retain(|id| current_ids.contains(id));
        // Discard stale popup selections whose IDs no longer exist
        self.selected_overlays.retain(|sel| match sel {
            SelectedOverlay::Alert(id) => current_ids.contains(id),
            _ => true,
        });
        if self.selected_overlay_page >= self.selected_overlays.len().max(1) {
            self.selected_overlay_page = 0;
        }
        self.nws_alerts.set_data(alerts);
    }

    /// Set whether an NWS alerts fetch is currently in progress.
    pub fn set_nws_fetching(&mut self, fetching: bool) {
        self.nws_alerts.fetching = fetching;
    }

    /// Store fetched SPC Mesoscale Discussions, replacing the previous set.
    pub fn set_spc_discussions(&mut self, discussions: Vec<SpcDiscussion>) {
        // Discard stale popup selections whose MD numbers no longer exist
        let current_numbers: HashSet<u32> = discussions.iter().map(|d| d.number).collect();
        self.selected_overlays.retain(|sel| match sel {
            SelectedOverlay::Discussion(num) => current_numbers.contains(num),
            _ => true,
        });
        if self.selected_overlay_page >= self.selected_overlays.len().max(1) {
            self.selected_overlay_page = 0;
        }
        self.spc_discussions.set_data(discussions);
    }

    /// Set whether an SPC MD fetch is currently in progress.
    pub fn set_spc_md_fetching(&mut self, fetching: bool) {
        self.spc_discussions.fetching = fetching;
    }

    /// Combined SPC data generation — sum of all per-product generations.
    /// Used as a single change-detection value for the unified SPC texture.
    pub fn combined_spc_data_generation(&self) -> u64 {
        self.spc_data_generation.values().sum()
    }

    /// Apply a fetch result from the unified overlay fetch channel.
    pub fn apply_fetch_result(&mut self, result: OverlayFetchResult) {
        match result {
            OverlayFetchResult::SpcOutlook { day, product, result } => {
                match result {
                    Ok(outlook) => {
                        log::info!("Received SPC outlook: {:?} {:?}", day, product);
                        self.set_spc_outlook(day, product, outlook);
                    }
                    Err(e) => {
                        log::error!("SPC outlook fetch failed ({:?} {:?}): {}", day, product, e);
                    }
                }
                self.set_spc_fetching(false);
            }
            OverlayFetchResult::NwsAlerts(result) => {
                match result {
                    Ok(alerts) => {
                        log::info!("Received {} NWS alerts", alerts.len());
                        self.set_nws_alerts(alerts);
                    }
                    Err(e) => {
                        log::error!("NWS alerts fetch failed: {}", e);
                    }
                }
                self.set_nws_fetching(false);
            }
            OverlayFetchResult::SpcDiscussions(result) => {
                match result {
                    Ok(discussions) => {
                        log::info!("Received {} SPC Mesoscale Discussions", discussions.len());
                        self.set_spc_discussions(discussions);
                    }
                    Err(e) => {
                        log::error!("SPC MD fetch failed: {}", e);
                    }
                }
                self.set_spc_md_fetching(false);
            }
        }
    }

    /// Build the popup content for the given selected overlay item.
    /// Returns `None` if the item no longer exists in the data.
    pub fn popup_content(&self, selected: &SelectedOverlay) -> Option<PopupContent> {
        match selected {
            SelectedOverlay::Alert(alert_id) => {
                let alert = self.nws_alerts.data.iter().find(|a| a.id == *alert_id)?;
                let [r, g, b, _] = alert.features.first()
                    .map(|f| f.stroke_rgba)
                    .unwrap_or([200, 200, 200, 255]);

                let mut sections = Vec::new();

                // Headline
                if let Some(headline) = &alert.headline {
                    sections.push(PopupSection::Heading(headline.clone()));
                }

                // Metadata grid
                sections.push(PopupSection::KeyValueGrid(vec![
                    ("Areas".into(), alert.area_desc.clone()),
                    ("Issued by".into(), alert.sender_name.clone()),
                    ("Effective".into(), format_iso_time(&alert.effective)),
                    ("Expires".into(), format_iso_time(&alert.expires)),
                ]));

                sections.push(PopupSection::Separator);

                // Description
                sections.push(PopupSection::ScrollableText {
                    text: alert.description.clone(),
                    monospace: false,
                    max_height: 250.0,
                });

                // Instruction
                if let Some(instruction) = &alert.instruction {
                    sections.push(PopupSection::Separator);
                    sections.push(PopupSection::ColoredText {
                        text: instruction.clone(),
                        rgb: [r, g, b],
                        bold: true,
                    });
                }

                Some(PopupContent {
                    title: alert.event.clone(),
                    accent_rgb: [r, g, b],
                    width: 380.0,
                    sections,
                    actions: vec![PopupAction {
                        label: "\u{1f6ab}  Hide from map".into(),
                        target: selected.clone(),
                        kind: PopupActionKind::HideFromMap,
                    }],
                })
            }
            SelectedOverlay::Discussion(md_number) => {
                use crate::spc::colors::md_stroke_color;

                let md = self.spc_discussions.data.iter().find(|d| d.number == *md_number)?;
                let [r, g, b, _] = md_stroke_color(&md.md_type);

                let mut sections = Vec::new();

                // Type badge
                sections.push(PopupSection::ColoredText {
                    text: format!("Type: {}", md.md_type),
                    rgb: [r, g, b],
                    bold: true,
                });

                // Concerning
                if let Some(ref concerning) = md.concerning {
                    sections.push(PopupSection::Heading(format!("Concerning: {}", concerning)));
                }

                sections.push(PopupSection::Separator);

                // Discussion text
                sections.push(PopupSection::ScrollableText {
                    text: md.text.clone(),
                    monospace: true,
                    max_height: 350.0,
                });

                sections.push(PopupSection::Separator);

                // Link
                if !md.link.is_empty() {
                    sections.push(PopupSection::Link {
                        label: "Open on SPC website".into(),
                        url: md.link.clone(),
                    });
                }

                Some(PopupContent {
                    title: format!("Mesoscale Discussion #{:04}", md.number),
                    accent_rgb: [r, g, b],
                    width: 420.0,
                    sections,
                    actions: Vec::new(),
                })
            }
            SelectedOverlay::Outlook { label } => {
                Some(PopupContent {
                    title: format!("SPC Outlook: {label}"),
                    accent_rgb: [200, 200, 100],
                    width: 300.0,
                    sections: vec![PopupSection::Text("Outlook detail coming soon.".into())],
                    actions: Vec::new(),
                })
            }
        }
    }

    /// Execute a popup action. Returns `true` if the item should be removed from the pager.
    pub fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        match action.kind {
            PopupActionKind::HideFromMap => {
                if let SelectedOverlay::Alert(ref id) = action.target {
                    self.hidden_alerts.insert(id.clone());
                    self.nws_alerts.data_generation =
                        self.nws_alerts.data_generation.wrapping_add(1);
                    return true;
                }
                false
            }
        }
    }
}

// ── Generic overlay kind ─────────────────────────────────────────────────

/// Identifies each map layer that participates in the per-pane draw order.
///
/// Texture-overlay variants (SpcOutlook, SpcDiscussions, NwsAlerts) are
/// rasterized to textures on background threads. Non-texture variants
/// (Radar, CityLabels, RadarSites, UserLocation) are drawn directly.
///
/// Used as a HashMap key for per-pane texture caches, in render requests,
/// and in the per-pane `draw_order` vec. Adding a new layer type only
/// requires adding a variant here and implementing the match arms below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OverlayKind {
    SpcOutlook,
    SpcDiscussions,
    NwsAlerts,
    Radar,
    CityLabels,
    RadarSites,
    UserLocation,
}

impl OverlayKind {
    /// All registered layer kinds in default draw order.
    pub const fn all() -> &'static [OverlayKind] {
        &[
            OverlayKind::SpcOutlook,
            OverlayKind::Radar,
            OverlayKind::SpcDiscussions,
            OverlayKind::NwsAlerts,
            OverlayKind::CityLabels,
            OverlayKind::RadarSites,
            OverlayKind::UserLocation,
        ]
    }

    /// Only the overlay kinds that get rasterized to background textures.
    pub const fn texture_overlays() -> &'static [OverlayKind] {
        &[
            OverlayKind::SpcOutlook,
            OverlayKind::SpcDiscussions,
            OverlayKind::NwsAlerts,
            OverlayKind::RadarSites,
            OverlayKind::Radar,
        ]
    }

    /// Whether this kind is a background-rasterized texture overlay.
    pub fn is_texture_overlay(self) -> bool {
        matches!(self, OverlayKind::SpcOutlook | OverlayKind::SpcDiscussions | OverlayKind::NwsAlerts | OverlayKind::RadarSites | OverlayKind::Radar)
    }

    /// Default draw order (bottom to top) for a new pane.
    pub fn default_draw_order() -> Vec<OverlayKind> {
        Self::all().to_vec()
    }

    /// Whether the relevant layer(s) for this kind are enabled.
    pub fn is_enabled(self, layers: &super::layers::LayerManager) -> bool {
        use super::layers::LayerKind;
        match self {
            OverlayKind::SpcOutlook => layers
                .spc_layers_for_day()
                .iter()
                .any(|lk| layers.is_enabled(*lk)),
            OverlayKind::SpcDiscussions => {
                layers.is_enabled(LayerKind::SpcMesoscaleDiscussions)
            }
            OverlayKind::NwsAlerts => layers.any_nws_enabled(),
            OverlayKind::Radar => layers.is_enabled(LayerKind::Radar),
            OverlayKind::CityLabels => layers.is_enabled(LayerKind::CityLabels),
            OverlayKind::RadarSites => layers.is_enabled(LayerKind::RadarSites),
            OverlayKind::UserLocation => true,
        }
    }

    /// The current data generation counter for this overlay kind.
    pub fn data_generation(self, overlays: &OverlayData) -> u64 {
        match self {
            OverlayKind::SpcOutlook => overlays.combined_spc_data_generation(),
            OverlayKind::SpcDiscussions => overlays.spc_discussions.data_generation,
            OverlayKind::NwsAlerts => overlays.nws_alerts.data_generation,
            OverlayKind::Radar | OverlayKind::CityLabels
            | OverlayKind::RadarSites | OverlayKind::UserLocation => 0,
        }
    }

    /// Whether any data exists for this overlay kind.
    pub fn has_data(self, overlays: &OverlayData) -> bool {
        match self {
            OverlayKind::SpcOutlook => !overlays.spc_outlooks.data.is_empty(),
            OverlayKind::SpcDiscussions => !overlays.spc_discussions.data.is_empty(),
            OverlayKind::NwsAlerts => !overlays.nws_alerts.data.is_empty(),
            OverlayKind::RadarSites | OverlayKind::Radar => true,
            OverlayKind::CityLabels
            | OverlayKind::UserLocation => false,
        }
    }

    /// Build the list of clickable/labelled items for this overlay kind.
    ///
    /// Encapsulates all per-overlay filtering (enabled layers, hidden alerts,
    /// etc.) so the UI crate can draw and hit-test without knowing overlay types.
    pub fn clickable_items<'a>(
        self,
        overlays: &'a OverlayData,
        layers: &super::layers::LayerManager,
    ) -> Vec<ClickableItem<'a>> {
        match self {
            OverlayKind::SpcOutlook => {
                let day = layers.spc_day;
                let mut items = Vec::new();
                for lk in layers.spc_layers_for_day() {
                    if !layers.is_enabled(lk) {
                        continue;
                    }
                    let Some(product) = lk.to_outlook_product() else { continue };
                    let Some(outlook) = overlays.spc_outlooks.data.get(&(day, product)) else { continue };
                    for feature in &outlook.features {
                        items.push(ClickableItem {
                            features: vec![feature],
                            label: None,
                            id: SelectedOverlay::Outlook { label: feature.label.clone() },
                        });
                    }
                }
                items
            }
            OverlayKind::SpcDiscussions => {
                use crate::spc::colors::md_stroke_color;

                overlays.spc_discussions.data.iter().filter(|md| !md.polygon.is_empty()).map(|md| {
                    // Compute centroid from the first ring for label placement
                    let label = md.polygon.first()
                        .filter(|ring| !ring.is_empty())
                        .map(|ring| {
                            let n = ring.len() as f64;
                            let lat = ring.iter().map(|&(lat, _)| lat).sum::<f64>() / n;
                            let lon = ring.iter().map(|&(_, lon)| lon).sum::<f64>() / n;
                            OverlayLabel {
                                lat,
                                lon,
                                text: format!("MD {}", md.number),
                                color: md_stroke_color(&md.md_type),
                            }
                        });
                    ClickableItem {
                        features: vec![&md.feature],
                        label,
                        id: SelectedOverlay::Discussion(md.number),
                    }
                }).collect()
            }
            OverlayKind::NwsAlerts => {
                let enabled_categories = layers.enabled_nws_categories();
                overlays.nws_alerts.data.iter()
                    .filter(|alert| {
                        enabled_categories.contains(&alert.category)
                            && !overlays.hidden_alerts.contains(&alert.id)
                    })
                    .map(|alert| ClickableItem {
                        features: alert.features.iter().collect(),
                        label: None,
                        id: SelectedOverlay::Alert(alert.id.clone()),
                    })
                    .collect()
            }
            // Non-texture layers have no clickable polygon items.
            OverlayKind::Radar | OverlayKind::CityLabels
            | OverlayKind::RadarSites | OverlayKind::UserLocation => Vec::new(),
        }
    }

    /// Auto-poll interval in seconds, or `None` if this kind doesn't auto-poll.
    pub fn auto_poll_interval(self) -> Option<u64> {
        match self {
            OverlayKind::NwsAlerts | OverlayKind::SpcDiscussions => Some(120),
            _ => None,
        }
    }

    /// Whether a fetch is currently in flight for this overlay kind.
    pub fn is_fetching(self, overlays: &OverlayData) -> bool {
        match self {
            OverlayKind::SpcOutlook => overlays.spc_outlooks.fetching,
            OverlayKind::SpcDiscussions => overlays.spc_discussions.fetching,
            OverlayKind::NwsAlerts => overlays.nws_alerts.fetching,
            _ => false,
        }
    }

    /// The last fetch timestamp for this overlay kind (if any data has been fetched).
    pub fn fetch_time(self, overlays: &OverlayData) -> Option<std::time::Instant> {
        match self {
            OverlayKind::SpcOutlook => overlays.spc_outlooks.fetch_time,
            OverlayKind::SpcDiscussions => overlays.spc_discussions.fetch_time,
            OverlayKind::NwsAlerts => overlays.nws_alerts.fetch_time,
            _ => None,
        }
    }

    /// Whether this overlay type should be fetched the first time its layer is turned on.
    pub fn should_fetch_on_enable(self) -> bool {
        true
    }

    /// Number of items currently loaded for display.
    pub fn item_count(self, overlays: &OverlayData) -> usize {
        match self {
            OverlayKind::SpcOutlook => overlays.spc_outlooks.data.len(),
            OverlayKind::SpcDiscussions => overlays.spc_discussions.data.len(),
            OverlayKind::NwsAlerts => overlays.nws_alerts.data.len(),
            _ => 0,
        }
    }
}

// ── Unified overlay fetch result ──────────────────────────────────────────

/// A fetch result from any overlay background task, sent through the unified
/// overlay fetch channel. Replaces the previous per-type channels.
pub enum OverlayFetchResult {
    SpcOutlook {
        day: OutlookDay,
        product: OutlookProduct,
        result: Result<SpcOutlook, String>,
    },
    NwsAlerts(Result<Vec<NwsAlert>, String>),
    SpcDiscussions(Result<Vec<SpcDiscussion>, String>),
}

// ── Popup content descriptors ─────────────────────────────────────────────

/// Describes the full content of an overlay detail popup, to be rendered
/// generically by the UI crate. The overlay crate builds these; the UI crate
/// draws them without knowing what overlay type produced them.
pub struct PopupContent {
    /// Popup window title text.
    pub title: String,
    /// Accent color `[r, g, b]` for the title and highlights.
    pub accent_rgb: [u8; 3],
    /// Desktop popup width (mobile auto-sizes to screen).
    pub width: f32,
    /// Ordered content sections.
    pub sections: Vec<PopupSection>,
    /// Actions the popup can trigger (rendered as buttons at the bottom).
    pub actions: Vec<PopupAction>,
}

/// A single section of popup content.
pub enum PopupSection {
    /// Bold heading text.
    Heading(String),
    /// Normal text label.
    Text(String),
    /// Colored text.
    ColoredText { text: String, rgb: [u8; 3], bold: bool },
    /// Key-value metadata rows.
    KeyValueGrid(Vec<(String, String)>),
    /// Long text in a scroll area, optionally monospace.
    ScrollableText { text: String, monospace: bool, max_height: f32 },
    /// A horizontal rule separator.
    Separator,
    /// A clickable hyperlink.
    Link { label: String, url: String },
}

/// An action button in the popup. The UI crate renders it; the overlay crate
/// defines what it means.
pub struct PopupAction {
    /// Button label.
    pub label: String,
    /// Which overlay item this action targets.
    pub target: SelectedOverlay,
    /// The kind of action.
    pub kind: PopupActionKind,
}

/// What a popup action button does when clicked.
pub enum PopupActionKind {
    /// Hide this item from the map (NWS alerts).
    HideFromMap,
}
