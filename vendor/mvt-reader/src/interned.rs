//! LOCAL CHANGE, and the whole module is ours — see `VENDORED.md`, change 7.
//!
//! A layer's features with their properties left **interned**, which is how
//! the wire already carries them.
//!
//! A `Layer` message holds `keys: Vec<String>` and `values: Vec<Value>` and
//! every feature's `tags` is a flat list of indices into those two tables.
//! [`Reader::get_features_as`](crate::Reader::get_features_as) expands that
//! into one `HashMap<String, Value>` per feature, with an owned key `String`
//! per property — it throws away the interning the format handed it for free.
//! Measured on `squallar-egui/testdata/monaco.pmtiles`'s z14 8529/5974 tile
//! (185,182 MVT bytes, 14 layers, 2,913 features, 14,303 properties): the
//! expanded bags are **1,447,103 B of the 2,092,002 B** a parsed tile is
//! resident for, against **166 keys and 1,848 values** in the tables they were
//! expanded from.
//!
//! This is the same decode with the tables kept. Nothing here changes
//! `get_features_as`, which `squallar-buildings` and the crate's own
//! `peak_allocation_tests` both still call.

use geo_types::{CoordNum, Geometry};

use crate::feature::Value;

/// One layer decoded with its key and value tables kept.
///
/// The tables are the layer's own, in wire order; [`InternedFeature::tags`] is
/// a window into [`InternedLayer::tags`], which holds every feature's pairs
/// back to back so that a layer costs one allocation for all of them rather
/// than one per feature.
#[derive(Debug, Clone)]
pub struct InternedLayer<T: CoordNum = f32> {
    /// The layer's key table.
    pub keys: Vec<String>,
    /// The layer's value table.
    pub values: Vec<Value>,
    /// Every feature's `(key index, value index)` pairs, back to back, in the
    /// order the wire wrote them.
    pub tags: Vec<(u32, u32)>,
    /// The layer's features, each naming its window into `tags`.
    pub features: Vec<InternedFeature<T>>,
}

/// One feature, its properties still indices into its layer's tables.
#[derive(Debug, Clone)]
pub struct InternedFeature<T: CoordNum = f32> {
    /// The geometry of the feature.
    pub geometry: Geometry<T>,

    /// Optional identifier for the feature.
    pub id: Option<u64>,

    /// Where this feature's pairs start in [`InternedLayer::tags`], and how
    /// many there are.
    ///
    /// **Wire order, with repeats kept.** A tag list that names one key twice
    /// is malformed, and `get_features_as` resolves it through
    /// `HashMap::insert`, so the *last* write wins. Nothing is dropped here,
    /// because a reader that discards a pair decides that question for every
    /// caller; a caller matching the map's behaviour reads the window
    /// backwards. `vendor/walkers`' `LayerProperties::get` is written that way
    /// and says so.
    pub tags_start: u32,
    /// How many pairs this feature has.
    pub tags_len: u32,
}

impl<T: CoordNum> InternedFeature<T> {
    /// This feature's pairs, out of its layer's arena.
    pub fn tags<'a>(&self, layer: &'a InternedLayer<T>) -> &'a [(u32, u32)] {
        let start = self.tags_start as usize;
        &layer.tags[start..start + self.tags_len as usize]
    }
}
