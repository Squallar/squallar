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

/// The colour ladder **every layer that draws reflectivity paints through**,
/// in dBZ, ascending.
///
/// Radar tilts, the MRMS national mosaic and the HRRR forecast composite are
/// read side by side in one pane. Before this table they were three separate
/// stop lists, and the radar one was offset by roughly one 5 dBZ band through
/// the green-to-red region — green began at 25 rather than 20, red at 45 rather
/// than 50, and 55 dBZ was pink where the other two were dark red. A reader
/// comparing an observed tilt against the mosaic beside it was reading two
/// different bars.
///
/// **The provenance of these colours is not recorded anywhere, and this comment
/// will not invent one.** What is known: the 5 dBZ-and-up stops are the ones
/// MRMS and HRRR have always drawn, described in their own docs as "the classic
/// NWS reflectivity ladder" — a claim in prose, with no transcription and no
/// cited source of the kind `SPECTRUM_WIDTH` carries (an ORPG build and a file
/// path). They were chosen as the survivor because two of the three layers
/// already drew them, not because they are the more authoritative table. The
/// two stops below 5 dBZ are radar's own, and those *do* have a traceable
/// shape: they are the discretisation of the `dbz / 5.0 * 128.0` grey ramp that
/// a hand-written `if`-chain painted the low end with until `8334139b` turned
/// the chain into a table.
///
/// **How each layer slices it:**
///
/// | layer | slice | banding |
/// |---|---|---|
/// | radar | the whole table, 0 dBZ up | gradient |
/// | MRMS | from 5 dBZ up | bands |
/// | HRRR | from 5 dBZ up | bands |
///
/// The table starts at 0 rather than at 5 because radar's own bar needs a stop
/// there: `voxel::tests::the_table_is_the_palette_function_not_its_stops`
/// asserts the reflectivity stops carry a `0.0` entry, which is what keeps the
/// 3D transfer table from being built out of them and painting everything below
/// zero opaque. The overlays start at 5 because their first stop is a
/// transparency floor — clear air in a mosaic or a forecast grid is a small
/// number rather than a missing value, and a bar starting at 0 would paint the
/// whole CONUS domain.
///
/// **Banding is deliberately NOT unified.** A radar tilt is a continuous field
/// read as a wash; a mosaic and a forecast composite are read by which band a
/// pixel is in. That decision stays with each source, in
/// [`LegendScale::is_gradient`].
///
/// **The top stop is 75 dBZ for every layer.** Radar's old table ran to 95 with
/// four stops above 75 that no observed reflectivity reaches; they went with
/// the unification rather than being carried into a table two layers cap at 75.
///
/// Held equal across the three by
/// `hrrr::fields::tests::the_three_reflectivity_ladders_agree`,
/// `palette::tests::the_reflectivity_ladder_is_the_substrates` and
/// `ui::map::pane_render::legend_ladder_tests::
/// every_layer_that_draws_dbz_paints_the_same_ladder`.
pub const REFLECTIVITY_STOPS: [(f32, [u8; 3]); 17] = [
    (0.0, [0x00, 0x00, 0x00]),
    (2.5, [0x40, 0x40, 0x40]),
    (5.0, [0x40, 0xe8, 0xe3]),
    (10.0, [0x26, 0xa4, 0xfa]),
    (15.0, [0x0a, 0x2f, 0xc0]),
    (20.0, [0x00, 0xff, 0x00]),
    (25.0, [0x00, 0xc8, 0x00]),
    (30.0, [0x00, 0x90, 0x00]),
    (35.0, [0xff, 0xff, 0x00]),
    (40.0, [0xe7, 0xc0, 0x00]),
    (45.0, [0xff, 0x90, 0x00]),
    (50.0, [0xff, 0x00, 0x00]),
    (55.0, [0xd6, 0x00, 0x00]),
    (60.0, [0xc0, 0x00, 0x00]),
    (65.0, [0xff, 0x00, 0xff]),
    (70.0, [0x99, 0x55, 0xc9]),
    (75.0, [0xff, 0xff, 0xff]),
];

/// The index in [`REFLECTIVITY_STOPS`] the overlay layers' bars begin at: the
/// 5 dBZ stop, which is their transparency floor.
///
/// A named constant rather than a `2` at each of the two call sites, so a stop
/// added to the low end moves both slices at once instead of silently shifting
/// MRMS's floor to 2.5 dBZ.
pub const REFLECTIVITY_OVERLAY_FLOOR: usize = 2;

/// A colour bar: the stops a field's values are painted through.
///
/// Lives here rather than in the radar crate because [`ProductSpec::scale`]
/// names it and this crate sits below every source. The radar crate re-exports
/// the type, so no consumer's spelling changed when it moved.
///
/// **This doc used to say the radar crate builds the values, "its palette
/// tables are the physics", and that a source therefore owns its own ramp.
/// That is no longer the rule for a quantity two sources both publish.**
/// Reflectivity was drawn through three tables — radar's, MRMS's and HRRR's —
/// and the radar one was offset roughly one 5 dBZ band through the green-to-red
/// region, so the same storm read 45 dBZ red on a radar tilt and orange on the
/// mosaic beside it. The stops for such a quantity now live here as a `const`
/// (see [`REFLECTIVITY_STOPS`]) and every layer slices that one table; what
/// stays the source's own is *how* it paints them, which is
/// [`LegendScale::is_gradient`]. A ramp only one source publishes still lives
/// with that source.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder's colours, as literals, because after the unification nothing
    /// else in the tree holds them.
    ///
    /// **This is the property the old arrangement had by accident and the new
    /// one has to state.** While radar, MRMS and HRRR each kept their own
    /// transcription, the test that held the two overlay ladders equal was
    /// comparing two independent copies, so a typo in either reddened it. One
    /// table has no
    /// second copy to disagree with: every agreement test in the workspace now
    /// reads *this* array, so a stop edited here moves all three layers
    /// together and every one of them still agrees. A mutation that repaints
    /// 45 dBZ would have gone green.
    ///
    /// So the second copy lives here, spelled out, and it is meant to be
    /// annoying: changing a colour means changing it twice, on purpose, with
    /// this comment in front of you.
    #[test]
    fn the_reflectivity_ladders_colours_are_the_ones_that_were_reviewed() {
        assert_eq!(
            REFLECTIVITY_STOPS,
            [
                (0.0, [0x00, 0x00, 0x00]),
                (2.5, [0x40, 0x40, 0x40]),
                (5.0, [0x40, 0xe8, 0xe3]),
                (10.0, [0x26, 0xa4, 0xfa]),
                (15.0, [0x0a, 0x2f, 0xc0]),
                (20.0, [0x00, 0xff, 0x00]),
                (25.0, [0x00, 0xc8, 0x00]),
                (30.0, [0x00, 0x90, 0x00]),
                (35.0, [0xff, 0xff, 0x00]),
                (40.0, [0xe7, 0xc0, 0x00]),
                (45.0, [0xff, 0x90, 0x00]),
                (50.0, [0xff, 0x00, 0x00]),
                (55.0, [0xd6, 0x00, 0x00]),
                (60.0, [0xc0, 0x00, 0x00]),
                (65.0, [0xff, 0x00, 0xff]),
                (70.0, [0x99, 0x55, 0xc9]),
                (75.0, [0xff, 0xff, 0xff]),
            ],
            "a reflectivity stop moved. Every layer that draws dBZ moved with \
             it — that is what this table is for — so no agreement test can \
             tell you. If the change was meant, re-record it here and say in \
             the commit which colour the reader will now see at which dBZ.",
        );
    }

    /// The two things every slicer of [`REFLECTIVITY_STOPS`] assumes: the stops
    /// ascend, and [`REFLECTIVITY_OVERLAY_FLOOR`] still points at 5 dBZ.
    ///
    /// The floor is an index rather than a value, so a stop inserted at the low
    /// end would move MRMS's and HRRR's transparency floor down without either
    /// crate changing a line. That is the failure this catches.
    #[test]
    fn the_reflectivity_ladder_ascends_and_its_overlay_floor_is_five_dbz() {
        for pair in REFLECTIVITY_STOPS.windows(2) {
            assert!(
                pair[1].0 > pair[0].0,
                "the reflectivity stops must ascend, but {} follows {}",
                pair[1].0,
                pair[0].0,
            );
        }
        assert_eq!(
            REFLECTIVITY_STOPS[0].0, 0.0,
            "radar's 3D transfer table is built from a scale that must carry a \
             0 dBZ stop",
        );
        assert_eq!(
            REFLECTIVITY_STOPS[REFLECTIVITY_OVERLAY_FLOOR].0, 5.0,
            "the overlay floor index no longer names the 5 dBZ stop, so MRMS \
             and HRRR would begin their bars somewhere else",
        );
        assert_eq!(
            REFLECTIVITY_STOPS
                .last()
                .expect("the ladder is non-empty")
                .0,
            75.0,
            "the ladder's top stop is 75 dBZ on every layer",
        );
    }
}
