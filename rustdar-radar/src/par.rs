//! The crate's parallel-iteration prelude: rayon everywhere it has threads,
//! sequential stand-ins where it does not.

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

    impl<T> IntoParIterFallback for Vec<T> {
        type Item = T;
        fn into_par_iter(self) -> impl Iterator<Item = T> {
            self.into_iter()
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

    /// Stands in for `rayon::iter::ParallelIterator::for_each_init`.
    pub trait ForEachInitFallback: Iterator + Sized {
        fn for_each_init<T, INIT, OP>(self, init: INIT, op: OP)
        where
            INIT: Fn() -> T,
            OP: Fn(&mut T, Self::Item),
        {
            let mut state = init();
            for item in self {
                op(&mut state, item);
            }
        }
    }

    impl<I: Iterator> ForEachInitFallback for I {}
}
