//! Layer identity as an open string: [`LayerId`].
//!
//! One open string is a layer's whole identity. This shape replaced a closed
//! twelve-variant layer enum in rustdar-overlays over the M8 sequence (the
//! enum was deleted at WO-M8c). The twelve values below are not arbitrary
//! names — they are the **exact bytes sitting in every user's config file
//! today** (`draw_order`, `enabled_overlays`, `overlay_configs`, and the
//! `handler_states` map all key on them), which is why that sequence landed
//! with **zero config migration**. Two things still hold that: rustdar-
//! overlays' `handler_state_keys_are_the_twelve_names_saved_configs_file_state_under`
//! pins the live registry's ids against the twelve spellings configs are
//! written under, and the E0a fixture corpus is the proof the files
//! themselves round-trip.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// An open-string identity for one map layer.
///
/// `#[serde(transparent)]`: a `LayerId` serializes as the bare string
/// (`"NwsAlerts"`, never `{"0": "NwsAlerts"}`) — byte-identical to what every
/// existing config file already holds, which is the zero-migration property
/// the serde-form pin test below holds.
///
/// **Deliberately NOT `Copy`**: a `Cow<'static, str>` cannot be `Copy`, and
/// consumers converting from the `Copy` enum must decide where they clone
/// (`Cow::Borrowed` clones are pointer-cheap; do not intern).
///
/// **Deliberately DERIVED `Debug`**: `{:?}` prints `LayerId("Radar")`, not
/// `Radar`. Persistence keys by [`LayerId::as_str`]
/// (`serialize_handler_states`); a leftover `format!("{:?}")` keying site
/// therefore produces a VISIBLY wrong key that the E0a fixture corpus
/// catches. A "nice" hand-written `Debug` printing the bare
/// string would mask exactly that bug — never add one. Key by
/// [`LayerId::as_str`], never by `{:?}`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LayerId(Cow<'static, str>);

impl LayerId {
    /// A `LayerId` borrowing a static spelling. `const` so the [`known`]
    /// registry can be plain `pub const` items.
    pub const fn from_static(s: &'static str) -> Self {
        LayerId(Cow::Borrowed(s))
    }

    /// A `LayerId` owning an arbitrary spelling — the open half of the open
    /// string: ids read from a config file need not appear in [`known`] or
    /// [`LAYER_ID_LEDGER`] to exist.
    pub fn new(s: impl Into<String>) -> Self {
        LayerId(Cow::Owned(s.into()))
    }

    /// The identity as the bare string — the ONE sanctioned spelling for
    /// persistence keys and match arms (never `{:?}`, see the type doc).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The twelve layer ids the workspace registers today.
///
/// Every spelling below was captured mechanically from the pre-M8 layer
/// enum's `Debug` output (2026-08-19, main 7a2a78ff), never typed by hand:
/// these are load-bearing bytes already sitting in every user's config file,
/// which is why [`LAYER_ID_LEDGER`] is append-only. A new layer appends a
/// const here and a row there; editing an existing spelling orphans that
/// layer's saved state for every user, silently.
///
/// [`LAYER_ID_LEDGER`]: super::LAYER_ID_LEDGER
pub mod known {
    use super::LayerId;

    pub const MODEL_DATA: LayerId = LayerId::from_static("ModelData");
    pub const SPC_OUTLOOK: LayerId = LayerId::from_static("SpcOutlook");
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
}

/// Every layer id ever registered, in the default draw order — bottom to top.
///
/// **APPEND-ONLY.** These strings are persisted in user config files;
/// renaming or removing an entry in place orphans saved state silently.
/// Renaming a layer requires a config migration step plus a load-time alias
/// for the old spelling — never an edit to an existing row. New layers append
/// (the array length grows with them; consumers assert membership, not
/// position).
pub const LAYER_ID_LEDGER: [&str; 12] = [
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
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Test group 3 (ledger): every `known` const appears in
    /// [`LAYER_ID_LEDGER`], and the ledger holds no duplicates — the
    /// append-only register actually registers the registry.
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

    /// Test group 4 (serde form): `#[serde(transparent)]` emits the BARE
    /// string — the exact bytes every existing config file already holds —
    /// and reads it back. This is the zero-migration property in one assert.
    #[test]
    fn a_layer_id_serializes_as_the_bare_string() {
        let json = serde_json::to_string(&known::NWS_ALERTS).expect("LayerId serializes");
        assert_eq!(json, "\"NwsAlerts\"");
        let back: LayerId = serde_json::from_str(&json).expect("the bare string deserializes");
        assert_eq!(back, known::NWS_ALERTS);
    }

    /// An owned id and a borrowed id with the same spelling are the same
    /// identity — `new` is the open half of the open string.
    #[test]
    fn owned_and_static_spellings_are_equal() {
        assert_eq!(LayerId::new("Radar"), known::RADAR);
        assert_eq!(LayerId::new("MysteryLayer").as_str(), "MysteryLayer");
    }
}
