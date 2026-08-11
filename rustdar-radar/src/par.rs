//! The crate's parallel-iteration prelude: rayon everywhere it has threads,
//! sequential stand-ins where it does not.
//!
//! wasm32-unknown-unknown is single-threaded — rayon compiles there but cannot
//! build a thread pool — so every module that parallelises has to carry a
//! sequential arm. The stand-ins below have the same names and the same
//! signatures as the rayon entry points they replace, which is what lets the
//! call sites stay identical instead of cfg'ing a dozen rasterization loops
//! that would then drift apart. The closures need no changes either: rayon
//! requires `Fn + Send + Sync`, strictly stronger than the `FnMut` these want.
//!
//! **One `use crate::par::*;`, no `cfg` at the consumer.** That is the point of
//! this module. The fallback previously existed twice — in [`crate::render`]
//! and [`crate::nrot`] — each with its own copy of the traits *and* its own
//! pair of `cfg` attributes choosing between them. Two copies had already
//! diverged in coverage (`render` grew four traits, `nrot` kept one), and two
//! more consumers are on the way. A consumer now writes one unconditional
//! import and cannot get the target split wrong, because the split is made
//! here.
//!
//! **This is a cfg split, not a removal.** Rasterization is the hot path on
//! desktop, and this fallback silently becoming the native arm is a large
//! regression that no test catches — the native arm is a glob re-export of
//! `rayon::prelude`, and the stand-ins are not even compiled off wasm32, so
//! there is no configuration in which they can be reached by mistake.

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use rayon::prelude::*;
#[cfg(target_arch = "wasm32")]
pub(crate) use seq::*;

/// Sequential stand-ins for the rayon entry points this crate uses.
#[cfg(target_arch = "wasm32")]
mod seq {
    /// Stands in for `rayon::prelude::ParallelSlice::par_iter`. Implemented on
    /// `[T]` only; `Vec<T>` reaches it through deref.
    pub trait ParIterFallback<T> {
        fn par_iter<'a>(&'a self) -> impl Iterator<Item = &'a T>
        where
            T: 'a;
    }

    impl<T> ParIterFallback<T> for [T] {
        fn par_iter<'a>(&'a self) -> impl Iterator<Item = &'a T>
        where
            T: 'a,
        {
            self.iter()
        }
    }

    /// Stands in for `rayon::iter::IntoParallelIterator::into_par_iter`.
    pub trait IntoParIterFallback {
        type Item;
        fn into_par_iter(self) -> impl Iterator<Item = Self::Item>;
    }

    impl IntoParIterFallback for std::ops::Range<usize> {
        type Item = usize;
        fn into_par_iter(self) -> impl Iterator<Item = usize> {
            self
        }
    }

    /// The owning arm of the same trait, for a caller that builds its work
    /// items first and consumes them — `voxel`'s per-row output slices, which
    /// have to be cut out of the grid before any row runs and then moved into
    /// the tasks.
    impl<T> IntoParIterFallback for Vec<T> {
        type Item = T;
        fn into_par_iter(self) -> impl Iterator<Item = T> {
            self.into_iter()
        }
    }

    /// Stands in for `rayon::slice::ParallelSlice::par_chunks`.
    pub trait ParChunksFallback<T> {
        fn par_chunks<'a>(&'a self, n: usize) -> impl Iterator<Item = &'a [T]>
        where
            T: 'a;
    }

    impl<T> ParChunksFallback<T> for [T] {
        fn par_chunks<'a>(&'a self, n: usize) -> impl Iterator<Item = &'a [T]>
        where
            T: 'a,
        {
            self.chunks(n)
        }
    }

    /// Stands in for `rayon::slice::ParallelSliceMut::par_chunks_mut`.
    pub trait ParChunksMutFallback<T> {
        fn par_chunks_mut<'a>(&'a mut self, n: usize) -> impl Iterator<Item = &'a mut [T]>
        where
            T: 'a;
    }

    impl<T> ParChunksMutFallback<T> for [T] {
        fn par_chunks_mut<'a>(&'a mut self, n: usize) -> impl Iterator<Item = &'a mut [T]>
        where
            T: 'a,
        {
            self.chunks_mut(n)
        }
    }
}
