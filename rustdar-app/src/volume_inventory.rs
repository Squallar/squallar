//! The site-keyed decoded volumes the app holds, behind the questions asked of
//! them rather than behind the maps that answer.
//!
//! Two volumes are held per site, and they are **not** the same volume:
//!
//! * **The still** — what a pane's static (non-loop) render draws. It is
//!   whatever arrived most recently for the site, complete or not, and it has
//!   no time of its own: the pane's own `scan_info` carries that.
//! * **The base** — the most recent *complete* volume, with the time its first
//!   radial was collected. It is the base of the current merged volume
//!   ([`rustdar_radar::current::resolve`]) that sections, the 3D view and every
//!   other whole-volume reader stand on.
//!
//! Both are keyed by site alone and hold exactly one volume each, so neither
//! can answer a question about a *past* volume. The loop's own cache is keyed
//! `(site, timestamp)` and is a different subsystem for that reason; it is not
//! held here and this module deliberately knows nothing about it.
//!
//! Every entry is a whole decoded volume — tens of megabytes across thousands
//! of per-radial buffers — so eviction hands the values back **owned**, for the
//! caller to pass to the deferred-drop path rather than free on the frame
//! thread.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDateTime;
use nexrad_model::data::Scan;
use rustdar_radar::nyquist::DeclaredNyquist;

/// A decoded volume and what its cuts declared their Nyquist velocity to be.
pub(crate) type Still = (Arc<Scan>, Arc<DeclaredNyquist>);

/// A [`Still`] plus the time the volume's first radial was collected.
pub(crate) type Base = (Arc<Scan>, Arc<DeclaredNyquist>, NaiveDateTime);

/// The decoded volumes held for each site, and the one owner of both.
#[derive(Default)]
pub(crate) struct VolumeInventory {
    /// The volume each pane's static render draws from, by site.
    still: HashMap<String, Still>,
    /// The most recent complete volume for each site, with its collection time.
    base: HashMap<String, Base>,
}

impl VolumeInventory {
    // ---- the still ----------------------------------------------------

    /// What `site`'s static render draws from, if anything has arrived.
    ///
    /// Handed back as refcounts rather than borrows: every caller clones out
    /// of the map before touching the dispatcher, because the dispatcher is
    /// borrowed mutably in the same statement.
    pub(crate) fn still_for(&self, site: &str) -> Option<Still> {
        self.still
            .get(site)
            .map(|(scan, declared)| (Arc::clone(scan), Arc::clone(declared)))
    }

    /// Make `volume` what `site`'s static render draws from.
    pub(crate) fn install_still(&mut self, site: String, volume: Still) {
        self.still.insert(site, volume);
    }

    // ---- the base -----------------------------------------------------

    /// `site`'s merge base — the volume half of it, which is what
    /// [`rustdar_radar::current::resolve`] takes.
    pub(crate) fn base_for(&self, site: &str) -> Option<Still> {
        self.base
            .get(site)
            .map(|(scan, declared, _)| (Arc::clone(scan), Arc::clone(declared)))
    }

    /// `site`'s merge base together with the time it was collected.
    pub(crate) fn base_with_time(&self, site: &str) -> Option<Base> {
        self.base
            .get(site)
            .map(|(scan, declared, at)| (Arc::clone(scan), Arc::clone(declared), *at))
    }

    /// When `site`'s merge base was collected, if it has one.
    pub(crate) fn base_collected_at(&self, site: &str) -> Option<NaiveDateTime> {
        self.base.get(site).map(|(_, _, at)| *at)
    }

    /// Whether `site`'s merge base is *exactly* the volume collected at `when`
    /// — the question a 3D target asks to learn it has been navigated to a
    /// volume the base still holds, rather than to a loop frame.
    pub(crate) fn base_is_from(&self, site: &str, when: NaiveDateTime) -> bool {
        self.base_collected_at(site) == Some(when)
    }

    /// Whether a volume collected at `when` would move `site`'s merge base
    /// forward. True when there is no base yet: the first complete volume
    /// always advances one.
    pub(crate) fn base_advances_to(&self, site: &str, when: NaiveDateTime) -> bool {
        self.base_collected_at(site).is_none_or(|held| when > held)
    }

    /// Make `volume` `site`'s merge base.
    pub(crate) fn install_base(&mut self, site: String, volume: Base) {
        self.base.insert(site, volume);
    }

    /// Every site holding a merge base.
    pub(crate) fn sites_with_base(&self) -> impl Iterator<Item = &str> {
        self.base.keys().map(String::as_str)
    }

    // ---- eviction and residency ---------------------------------------

    /// Take out every still volume whose site `doomed` names, **owned**, for
    /// the caller to hand to the deferred-drop path.
    pub(crate) fn evict_still(&mut self, doomed: &impl Fn(&String) -> bool) -> Vec<Still> {
        crate::app::evicted(&mut self.still, doomed)
    }

    /// [`evict_still`](Self::evict_still) for the merge bases.
    pub(crate) fn evict_base(&mut self, doomed: &impl Fn(&String) -> bool) -> Vec<Base> {
        crate::app::evicted(&mut self.base, doomed)
    }

    /// Every volume still held here, for the derived-product cache's retain
    /// sweep. Both stores, because a `Scan` reachable from either one must
    /// keep its derivations.
    pub(crate) fn resident(&self) -> impl Iterator<Item = &Scan> {
        self.still
            .values()
            .map(|(scan, _)| scan.as_ref())
            .chain(self.base.values().map(|(scan, _, _)| scan.as_ref()))
    }

    /// Drop every merge base. Test scaffolding: production drops a base only
    /// through [`evict_base`](Self::evict_base), which is bounded by what the
    /// panes are showing.
    #[cfg(test)]
    pub(crate) fn forget_all_bases(&mut self) {
        self.base.clear();
    }

    /// Whether any site has a still volume.
    #[cfg(test)]
    pub(crate) fn holds_no_still(&self) -> bool {
        self.still.is_empty()
    }

    /// Whether `site` has anything for a static render to draw. Production
    /// asks for the volume itself ([`still_for`](Self::still_for)) and never
    /// merely whether one is there; the tests assert residency directly.
    #[cfg(test)]
    pub(crate) fn holds_still(&self, site: &str) -> bool {
        self.still.contains_key(site)
    }

    /// [`holds_still`](Self::holds_still) for the merge bases.
    #[cfg(test)]
    pub(crate) fn holds_base(&self, site: &str) -> bool {
        self.base.contains_key(site)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two volumes that are distinguishable by pointer — `ready_scan` builds a
    /// fresh one per call, so `Arc::ptr_eq` tells them apart.
    fn two_volumes() -> (Arc<Scan>, Arc<Scan>) {
        (
            crate::volume_fixture::ready_scan(),
            crate::volume_fixture::ready_scan(),
        )
    }

    fn at(minute: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
            .expect("a real date")
            .and_hms_opt(12, minute, 0)
            .expect("a real time")
    }

    /// **The two stores are two stores.** One owner holding both is exactly the
    /// shape whose defect is a write landing in one and the read going to the
    /// other, so the pin is that an install reaches its own store and *only*
    /// its own.
    #[test]
    fn a_still_install_does_not_reach_the_base_and_the_reverse() {
        let (still, base) = two_volumes();
        let mut inv = VolumeInventory::default();

        inv.install_still("KTLX".to_owned(), (Arc::clone(&still), Arc::default()));
        assert!(inv.holds_still("KTLX"), "the still install did not land");
        assert!(
            !inv.holds_base("KTLX"),
            "installing a still volume also wrote the merge base, so a partial \
             volume would be handed to every whole-volume reader",
        );
        assert_eq!(inv.base_collected_at("KTLX"), None);

        inv.install_base(
            "KTLX".to_owned(),
            (Arc::clone(&base), Arc::default(), at(10)),
        );
        assert!(
            Arc::ptr_eq(&inv.still_for("KTLX").expect("a still").0, &still),
            "installing the merge base overwrote the still volume, so the map \
             panes would jump to whatever the base last was",
        );
        assert!(
            Arc::ptr_eq(&inv.base_for("KTLX").expect("a base").0, &base),
            "the merge base is not the volume it was installed with",
        );
        assert_eq!(inv.base_collected_at("KTLX"), Some(at(10)));
    }

    /// The two time questions the archive drain and the 3D target ask.
    #[test]
    fn the_base_advances_forward_and_is_from_its_own_collection_time() {
        let mut inv = VolumeInventory::default();

        assert!(
            inv.base_advances_to("KTLX", at(10)),
            "the first complete volume for a site must advance a base it has \
             not got yet, or a site never gets one at all",
        );
        assert!(!inv.base_is_from("KTLX", at(10)));

        let (base, _) = two_volumes();
        inv.install_base("KTLX".to_owned(), (base, Arc::default(), at(10)));

        assert!(inv.base_is_from("KTLX", at(10)));
        assert!(!inv.base_is_from("KTLX", at(5)));
        assert!(inv.base_advances_to("KTLX", at(15)));
        assert!(
            !inv.base_advances_to("KTLX", at(5)),
            "an older archive volume walks the merge base backwards",
        );
        assert!(
            !inv.base_advances_to("KTLX", at(10)),
            "re-fetching the volume already held counts as advancing, so a \
             refresh would reinstall it over a live feed's newer sweeps",
        );
    }

    /// Eviction hands the volumes back **owned**, from both stores, and only
    /// for the sites named.
    #[test]
    fn eviction_takes_the_doomed_sites_out_of_both_stores() {
        let mut inv = VolumeInventory::default();
        for site in ["KTLX", "KOUN"] {
            let (still, base) = two_volumes();
            inv.install_still(site.to_owned(), (still, Arc::default()));
            inv.install_base(site.to_owned(), (base, Arc::default(), at(10)));
        }
        assert_eq!(
            inv.resident().count(),
            4,
            "precondition: two sites, two stores"
        );

        let doomed = |site: &String| site == "KOUN";
        assert_eq!(inv.evict_still(&doomed).len(), 1);
        assert_eq!(inv.evict_base(&doomed).len(), 1);

        assert!(
            inv.holds_still("KTLX") && inv.holds_base("KTLX"),
            "the shown site was evicted"
        );
        assert!(
            !inv.holds_still("KOUN") && !inv.holds_base("KOUN"),
            "an unshown site kept a whole decoded volume in one of the stores",
        );
        assert_eq!(
            inv.resident().count(),
            2,
            "the retain sweep would still see the evicted site's volumes, so \
             their derived products are never released either",
        );
    }
}
