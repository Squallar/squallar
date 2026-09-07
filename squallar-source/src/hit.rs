//! **The page-side half of a hit map**: what an index a rasterizer recorded
//! resolves to when a click lands on it.
//!
//! A hit map is two halves built from one iteration of one list — the cells,
//! which travel from the worker carrying *positions*, and the items those
//! positions name, which stay on the page. This module is the second half, and
//! it exists in two shapes because two layers pay very different prices for it.
//!
//! [`HitItems::Rows`] is the obvious shape — one `Arc<dyn OverlayItem>` per
//! row, handed over as a vector — and **no layer takes it today**, because
//! cloning that vector is an item-list walk and an allocation on the frame
//! thread once per dispatch, for a list of which, on any given click, exactly
//! one element is read. It is kept as the shape a new layer will reach for
//! first, and as the one place that walk is counted
//! ([`crate::walks::note_hit_walk`]).
//!
//! So both hit-map layers carry [`HitItems::Slab`]: a handle to the rows, from
//! which a click builds the one item it names. The handle owns its rows
//! outright and has no lifetime — a value a handler can park and hand back to
//! be dropped somewhere other than the frame thread.
//!
//! The two arrive at it from opposite ends. GLM lightning delivers around
//! 125,000 flashes per 20 s poll, so it never materialises items at all and
//! builds one on demand. Storm reports are a few thousand rows that already
//! exist as items when their poll lands, so their slab holds those items and
//! `get` clones a single pointer — the same allocation the layer's own data
//! holds, not a rebuilt one.

use std::fmt::Debug;
use std::sync::Arc;

use crate::handler::OverlayItem;

/// Builds the click target that one recorded index names.
///
/// `Send + Sync` and `'static` by the same requirement [`OverlayItem`] carries:
/// a [`HitItems`] rides inside the hit map a dispatch delivers, and a hit map
/// crosses the worker boundary with the response.
pub trait HitResolve: Send + Sync + Debug {
    /// How many rows there are, which is the id space the cells index into.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The item at `index`, or `None` when the index is past the end. Built
    /// here, on demand — a caller that asks for all of them has thrown the
    /// point away.
    fn get(&self, index: usize) -> Option<Arc<dyn OverlayItem>>;
}

/// The items a hit map's recorded indices name, positionally.
#[derive(Clone, Debug)]
pub enum HitItems {
    /// One `Arc` per row, materialised when the poll landed. Cloning it is a
    /// vector of pointers and one refcount bump each.
    Rows(Vec<Arc<dyn OverlayItem>>),
    /// The rows in one block, with a resolver that materialises a click's own
    /// item. Cloning it is one refcount bump.
    Slab(Arc<dyn HitResolve>),
}

impl HitItems {
    pub fn len(&self) -> usize {
        match self {
            HitItems::Rows(rows) => rows.len(),
            HitItems::Slab(slab) => slab.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The item at `index`, or `None` when the index is past the end — the
    /// bounds check that makes a cell naming a row this list does not have
    /// answer nothing rather than panic.
    pub fn get(&self, index: usize) -> Option<Arc<dyn OverlayItem>> {
        match self {
            HitItems::Rows(rows) => rows.get(index).cloned(),
            HitItems::Slab(slab) => slab.get(index),
        }
    }

    /// Every item in order. **Lazy on purpose**: a slab builds each item as the
    /// iterator reaches it, so a search that stops early pays for what it read.
    /// A caller that collects this from a slab-backed list has materialised
    /// every row, which is the cost [`HitItems::Slab`] exists to avoid.
    pub fn iter(&self) -> impl Iterator<Item = Arc<dyn OverlayItem>> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }
}

/// So a handler that already builds one item per row keeps writing
/// `Some(rows.map(..).collect())` and nothing else changes at its end.
///
/// **This is the hit-side item-list walk, and the only shape of it**: a
/// [`HitItems::Slab`] is built from a handle and walks nothing. So the count
/// [`crate::walks::note_hit_walk`] keeps is exactly the number of dispatches
/// that materialised one row per item, which is what the `hitmap` cut is
/// timing whenever it reads anything at all.
impl FromIterator<Arc<dyn OverlayItem>> for HitItems {
    fn from_iter<T: IntoIterator<Item = Arc<dyn OverlayItem>>>(iter: T) -> Self {
        crate::walks::note_hit_walk();
        HitItems::Rows(iter.into_iter().collect())
    }
}

impl From<Vec<Arc<dyn OverlayItem>>> for HitItems {
    fn from(rows: Vec<Arc<dyn OverlayItem>>) -> Self {
        HitItems::Rows(rows)
    }
}
