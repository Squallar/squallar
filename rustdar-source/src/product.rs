//! The field a source offers, as data every consumer reads and nobody matches
//! on: [`FieldId`] and [`ProductSpec`].
//!
//! A field is one renderable quantity a source publishes — a radar moment, an
//! HRRR parameter. The UI asks this table what a field is called, what units it
//! speaks, what colours it wears and what its slider may travel over; it never
//! asks *which* field it is. That is the whole point of the split: adding a
//! field is a row in its own crate's `products()`, not an arm in the UI.

use std::borrow::Cow;

use rustdar_units::Quantity;
use serde::{Deserialize, Serialize};

/// An open-string identity for one field.
///
/// `#[serde(transparent)]`: a `FieldId` serializes as the bare string. The
/// spellings the radar crate registers are **byte-identical to the product
/// enum's own `Serialize` output**, which is what makes the move to this type a
/// zero-migration change — a config file written before it existed loads
/// unchanged, and one written after it loads on a build from before.
///
/// **Deliberately NOT `Copy`**: a `Cow<'static, str>` cannot be, and consumers
/// must decide where they clone (`Cow::Borrowed` clones are pointer-cheap).
///
/// **Deliberately DERIVED `Debug`**: `{:?}` prints `FieldId("Reflectivity")`,
/// not `Reflectivity`, so a leftover `format!("{:?}")` keying site produces a
/// visibly wrong persistence key. Key by [`FieldId::as_str`], never by `{:?}`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldId(Cow<'static, str>);

impl FieldId {
    /// A `FieldId` borrowing a static spelling. `const` so a registry can be
    /// plain `pub const` items.
    pub const fn from_static(s: &'static str) -> Self {
        FieldId(Cow::Borrowed(s))
    }

    /// A `FieldId` owning an arbitrary spelling: ids read from a config file
    /// need not be ones this build registers to exist.
    pub fn new(s: impl Into<String>) -> Self {
        FieldId(Cow::Owned(s.into()))
    }

    /// The identity as the bare string — the ONE sanctioned spelling for
    /// persistence keys and lookups.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FieldId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A colour bar: the stops a field's values are painted through.
///
/// Lives here rather than in the radar crate because [`ProductSpec::scale`]
/// names it and this crate sits below every source. The radar crate builds the
/// values (its palette tables are the physics) and re-exports this type, so no
/// consumer's spelling changed when it moved.
#[derive(Clone, Debug, PartialEq)]
pub struct LegendScale {
    /// Colour stops, sorted ascending by value.
    pub thresholds: Vec<(f32, [u8; 3])>,
    /// Whether the renderer interpolates between stops or paints flat bands.
    pub is_gradient: bool,
    pub min_value: f32,
    pub max_value: f32,
}

/// Everything a consumer may know about one field, in one row.
///
/// **No field has a `Default`, and that is deliberate**: every one of the
/// eleven is a claim about the field that only the source registering it can
/// make. A defaulted `value_domain` is a fabricated slider; a defaulted
/// `vertical` is a 3D editor offered for a field with no vertical extent. A
/// source that cannot answer one of these does not have a field to register.
pub struct ProductSpec {
    /// The field's open-string identity — the persistence key and the lookup key.
    pub id: FieldId,
    /// Display name, as the UI writes it.
    pub name: &'static str,
    /// Short lowercase identifier (`"ref"`, `"vel"`, …).
    pub code: &'static str,
    /// Order fields are listed in within their group.
    pub sort_order: u8,
    /// The group label the UI files this field under ("Radar products",
    /// "HRRR parameters"). A `&'static str` rather than an enum: a new source
    /// brings its own group and the UI needs no arm for it.
    pub group: &'static str,
    /// The unit domain this field's values live in — the binding that honours
    /// the user's unit preferences.
    pub quantity: Quantity,
    /// The colour bar, built once and borrowed. Consumers read this object;
    /// they never rebuild a colour table from values.
    pub scale: &'static LegendScale,
    /// The inclusive range a threshold slider over this field may travel —
    /// ergonomics, in the field's own units.
    pub value_domain: (f32, f32),
    /// What a threshold over this field *means*, as a short prefix (`"\u{2265}"`,
    /// `"|\u{b1}| \u{2265}"`, `"\u{2264}"`) and the unit suffix that follows the number.
    pub domain_label_ends: (&'static str, &'static str),
    /// Whether this field has vertical extent, i.e. whether it renders in the
    /// 3D views at all. The 3D editors gate on this.
    pub vertical: bool,
    /// Whether this field exists at individual tilts, as opposed to being a
    /// whole-volume composite or column integral.
    pub tilted: bool,
}
