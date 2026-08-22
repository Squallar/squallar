//! Layer identity as an open string: [`LayerId`]. The shipped values below are
//! the **exact bytes sitting in every user's config file today**; the
//! fourteenth is a reservation, registered only under
//! `rustdar-overlays/fake-source`.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// An open-string identity for one map layer.
///
/// `#[serde(transparent)]`: a `LayerId` serializes as the bare string, which is
/// byte-identical to what every existing config file already holds.
///
/// **Deliberately NOT `Copy`**: a `Cow<'static, str>` cannot be, and consumers
/// must decide where they clone (`Cow::Borrowed` clones are pointer-cheap).
///
/// **Deliberately DERIVED `Debug`**: `{:?}` prints `LayerId("Radar")`, not
/// `Radar`, so a leftover `format!("{:?}")` keying site produces a visibly
/// wrong persistence key. Key by [`LayerId::as_str`], never by `{:?}`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LayerId(Cow<'static, str>);

impl LayerId {
    /// A `LayerId` borrowing a static spelling. `const` so [`known`] can be plain
    /// `pub const` items.
    pub const fn from_static(s: &'static str) -> Self {
        LayerId(Cow::Borrowed(s))
    }

    /// A `LayerId` owning an arbitrary spelling: ids read from a config file need
    /// not appear in [`known`] to exist.
    pub fn new(s: impl Into<String>) -> Self {
        LayerId(Cow::Owned(s.into()))
    }

    /// The identity as the bare string — the ONE sanctioned spelling for
    /// persistence keys and match arms.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Every spelling was captured mechanically from the pre-M8 layer enum's
/// `Debug` output: load-bearing bytes already in every user's config file, so
/// [`LAYER_ID_LEDGER`](super::LAYER_ID_LEDGER) is append-only.
pub mod known {
    use super::LayerId;

    pub const MODEL_DATA: LayerId = LayerId::from_static("ModelData");
    pub const SPC_OUTLOOK: LayerId = LayerId::from_static("SpcOutlook");
    pub const SPC_FIRE_OUTLOOK: LayerId = LayerId::from_static("SpcFireOutlook");
    pub const MRMS: LayerId = LayerId::from_static("Mrms");
    pub const GMGSI: LayerId = LayerId::from_static("Gmgsi");
    pub const RADAR: LayerId = LayerId::from_static("Radar");
    pub const SPC_DISCUSSIONS: LayerId = LayerId::from_static("SpcDiscussions");
    pub const NWS_ALERTS: LayerId = LayerId::from_static("NwsAlerts");
    pub const STORM_REPORTS: LayerId = LayerId::from_static("StormReports");
    pub const LIGHTNING: LayerId = LayerId::from_static("Lightning");
    pub const METAR: LayerId = LayerId::from_static("Metar");
    pub const CITY_LABELS: LayerId = LayerId::from_static("CityLabels");
    pub const RADAR_SITES: LayerId = LayerId::from_static("RadarSites");
    pub const USER_LOCATION: LayerId = LayerId::from_static("UserLocation");
    pub const COLOR_SCALE: LayerId = LayerId::from_static("ColorScale");
    /// The proof layer `rustdar-overlays`' `fake-source` feature registers.
    /// The const is unconditional — a ledger row is a *reservation* of a
    /// spelling, and reserving one under a `cfg` would let a build that has
    /// the feature off hand the same string to something else.
    pub const FAKE_SOURCE: LayerId = LayerId::from_static("FakeSource");
}

/// Every layer id ever registered.
///
/// **APPEND-ONLY.** These strings are persisted in user config files. Renaming
/// a layer requires a config migration step plus a load-time alias for the old
/// spelling — never an edit to an existing row.
///
/// The first twelve rows happen to be the historical default draw order,
/// bottom to top; **that is a coincidence of when they were added and nothing
/// reads it.** The draw order is a pure function of
/// `SourceHandler::draw_order_weight`, composed by
/// `rustdar_egui::sources::default_draw_order` and pinned by
/// `draw_order_weights_encode_the_default_draw_order`. `SpcFireOutlook` draws
/// third from the bottom (weight 25), `Mrms` second (weight 15) and `Gmgsi`
/// **first** (weight 5, under every other layer); all three are appended here
/// regardless, because append-only wins.
pub const LAYER_ID_LEDGER: [&str; 16] = [
    "ModelData",
    "SpcOutlook",
    "Radar",
    "SpcDiscussions",
    "NwsAlerts",
    "StormReports",
    "Lightning",
    "Metar",
    "CityLabels",
    "RadarSites",
    "UserLocation",
    "ColorScale",
    "SpcFireOutlook",
    "Mrms",
    "Gmgsi",
    // Registered only when `rustdar-overlays/fake-source` is on, listed here
    // unconditionally: the ledger is append-only and names every spelling ever
    // claimed, including ones this build does not register. The `cfg` lives in
    // the consuming tests' expected sets, never in this table.
    "FakeSource",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `known` const appears in [`LAYER_ID_LEDGER`], with no duplicates.
    #[test]
    fn the_ledger_holds_every_known_id_exactly_once() {
        let known_ids = [
            known::MODEL_DATA,
            known::SPC_OUTLOOK,
            known::RADAR,
            known::SPC_DISCUSSIONS,
            known::NWS_ALERTS,
            known::STORM_REPORTS,
            known::LIGHTNING,
            known::METAR,
            known::CITY_LABELS,
            known::RADAR_SITES,
            known::USER_LOCATION,
            known::COLOR_SCALE,
            known::SPC_FIRE_OUTLOOK,
            known::MRMS,
            known::GMGSI,
            known::FAKE_SOURCE,
        ];
        assert_eq!(known_ids.len(), LAYER_ID_LEDGER.len());
        for id in &known_ids {
            assert!(
                LAYER_ID_LEDGER.contains(&id.as_str()),
                "known const {:?} is missing from LAYER_ID_LEDGER",
                id
            );
        }
        let mut sorted: Vec<&str> = LAYER_ID_LEDGER.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            LAYER_ID_LEDGER.len(),
            "LAYER_ID_LEDGER contains a duplicate spelling"
        );
    }

    /// `#[serde(transparent)]` emits the BARE string and reads it back.
    #[test]
    fn a_layer_id_serializes_as_the_bare_string() {
        let json = serde_json::to_string(&known::NWS_ALERTS).expect("LayerId serializes");
        assert_eq!(json, "\"NwsAlerts\"");
        let back: LayerId = serde_json::from_str(&json).expect("the bare string deserializes");
        assert_eq!(back, known::NWS_ALERTS);
    }

    /// An owned id and a borrowed id with the same spelling are the same identity.
    #[test]
    fn owned_and_static_spellings_are_equal() {
        assert_eq!(LayerId::new("Radar"), known::RADAR);
        assert_eq!(LayerId::new("MysteryLayer").as_str(), "MysteryLayer");
    }
}
