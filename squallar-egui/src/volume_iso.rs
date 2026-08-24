//! Per-field isosurface thresholds — the session state the sidebar slider edits,
//! the frame reads, and the config persists.

use std::collections::HashMap;

use squallar_source::product::FieldId;

/// The argued default threshold for a field, or `None` for a field this build
/// does not register.
///
/// **The one place a saved threshold meets radar's default.** The default is
/// still radar's own argued number rather than a registry fact, so the id is
/// resolved back through radar's projection to ask for it; nothing else here
/// names a product.
fn default_threshold(field: &FieldId) -> Option<f32> {
    squallar_radar::fields::product_for(field).map(squallar_radar::voxel::default_iso_threshold)
}

/// Every field's user-set isosurface threshold, in the field's own units.
#[derive(Default)]
pub struct IsoThresholds {
    thresholds: HashMap<FieldId, f32>,
}

impl IsoThresholds {
    /// The threshold for `field`: the user's where one is set, else the argued
    /// default. `0.0` for a field this build does not register — nothing can
    /// reach this with such an id, because no pane can select a field that is
    /// not in the registry.
    pub fn get(&self, field: &FieldId) -> f32 {
        self.thresholds
            .get(field)
            .copied()
            .unwrap_or_else(|| default_threshold(field).unwrap_or(0.0))
    }

    /// The editor's door: a value back at the default erases the exception.
    pub fn set(&mut self, field: &FieldId, threshold: f32) {
        if !threshold.is_finite() {
            return;
        }
        if Some(threshold) == default_threshold(field) {
            self.thresholds.remove(field);
        } else {
            self.thresholds.insert(field.clone(), threshold);
        }
    }

    /// **Take a persisted entry.** A field this build registers goes through
    /// the editor's own door, so a stored value equal to the default is
    /// dropped exactly as an edit back to the default is. One it does **not**
    /// register is kept verbatim: under the open-id doctrine an unknown field
    /// is preserved inert rather than dropped, so a threshold saved by a newer
    /// build survives a session under this one. It applies to nothing here —
    /// no pane can select a field the registry does not offer.
    pub fn restore(&mut self, field: FieldId, threshold: f32) {
        if !threshold.is_finite() {
            return;
        }
        if default_threshold(&field).is_some() {
            self.set(&field, threshold);
        } else {
            self.thresholds.insert(field, threshold);
        }
    }

    pub fn is_edited(&self, field: &FieldId) -> bool {
        self.thresholds.contains_key(field)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&FieldId, f32)> + '_ {
        self.thresholds.iter().map(|(field, &t)| (field, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_radar::fields as radar_fields;

    fn field(product: &FieldId) -> FieldId {
        product.clone()
    }

    /// The shipped threshold for a field, asked of the radar layer through the
    /// one id door — the voxel table is keyed by the layer's own field value.
    fn default_of(product: &FieldId) -> f32 {
        squallar_radar::voxel::default_iso_threshold(
            squallar_radar::fields::product_for(product).expect("a registered field"),
        )
    }

    /// The store holds exceptions: setting a field back to its default erases it.
    #[test]
    fn thresholds_are_stored_per_field_as_exceptions() {
        let mut store = IsoThresholds::default();
        let reflectivity = field(&radar_fields::known::REFLECTIVITY);
        let velocity = field(&radar_fields::known::VELOCITY);
        assert_eq!(
            store.get(&reflectivity),
            default_of(&radar_fields::known::REFLECTIVITY),
        );
        assert!(!store.is_edited(&reflectivity));

        store.set(&reflectivity, 35.0);
        assert_eq!(store.get(&reflectivity), 35.0);
        assert!(store.is_edited(&reflectivity));
        assert_eq!(
            store.get(&velocity),
            default_of(&radar_fields::known::VELOCITY),
            "one field's threshold must never bleed into another's",
        );

        store.set(
            &reflectivity,
            default_of(&radar_fields::known::REFLECTIVITY),
        );
        assert!(
            !store.is_edited(&reflectivity),
            "back at the default is the same as never touched",
        );
    }

    /// A non-finite threshold is refused at the door, like every persisted float
    /// in this codebase — **at BOTH doors**.
    ///
    /// The `restore` half is asserted over an UNREGISTERED id on purpose. For
    /// a field this build knows, `restore` delegates to `set`, whose own guard
    /// would refuse the value anyway; the unknown-id branch inserts directly
    /// and is the only place `restore`'s guard is load-bearing. A tamper that
    /// deleted that guard came back GREEN while this test used a known field.
    #[test]
    fn a_non_finite_threshold_is_refused() {
        use squallar_source::product::FieldId;
        let mut store = IsoThresholds::default();
        let reflectivity = field(&radar_fields::known::REFLECTIVITY);
        store.set(&reflectivity, f32::NAN);
        store.set(&reflectivity, f32::INFINITY);
        assert!(!store.is_edited(&reflectivity), "the editor's door");

        let unknown = FieldId::new("NoBuildRegistersThisField");
        assert!(
            default_threshold(&unknown).is_none(),
            "precondition: this id takes `restore`'s direct-insert branch, \
             which is the only branch its own finite guard covers",
        );
        store.restore(unknown.clone(), f32::NAN);
        store.restore(unknown.clone(), f32::NEG_INFINITY);
        assert!(
            !store.is_edited(&unknown),
            "a non-finite threshold saved under an unregistered id reached the \
             store: it would be written back and eventually resolve to a field",
        );
    }

    /// **The open-id doctrine, in the direction that used to drop the entry.**
    /// A threshold saved under a field this build does not register is kept
    /// verbatim -- it applies to nothing, and survives to be written back.
    ///
    /// This is what replaces `known_product_or_none`'s drop-on-load: the
    /// guarantee that mattered was that a threshold saved for one field is
    /// never applied to another, and an id that resolves to no field is
    /// applied to nothing at all.
    #[test]
    fn a_threshold_for_a_field_this_build_does_not_register_is_kept_inert() {
        use squallar_source::product::FieldId;
        let unknown = FieldId::new("NoBuildRegistersThisField");
        let mut store = IsoThresholds::default();
        store.restore(unknown.clone(), 12.0);
        assert!(
            store.is_edited(&unknown),
            "the entry must survive the round trip, not be dropped",
        );
        assert_eq!(store.get(&unknown), 12.0);
        for product in radar_fields::known::ALL.iter() {
            assert!(
                !store.is_edited(&field(product)),
                "an unknown id leaked onto {:?}",
                product,
            );
        }
    }

    /// A persisted entry that is already the default is dropped, exactly as an
    /// edit back to the default is -- the restore path is the editor's door
    /// for a field this build knows.
    #[test]
    fn a_persisted_threshold_at_the_default_is_not_an_exception() {
        let mut store = IsoThresholds::default();
        store.restore(
            field(&radar_fields::known::VELOCITY),
            default_of(&radar_fields::known::VELOCITY),
        );
        assert!(!store.is_edited(&field(&radar_fields::known::VELOCITY)));
    }
}
