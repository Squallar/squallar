//! NetCDF4 reading and CF-convention unpacking.
//!
//! **This crate knows NetCDF4/HDF5 and the CF conventions. It does not know
//! what a satellite or a lightning flash is.** Grid geometry, axis extraction,
//! separability and every other question about what the numbers *mean* belong
//! to the caller; what belongs here is getting the numbers out of the file with
//! the file's own declared meaning applied and nothing else added.
//!
//! NetCDF4 *is* HDF5: every netCDF variable is an HDF5 dataset in the root
//! group and every attribute an HDF5 attribute. [`hdf5_pure`] replaces
//! `netcdf`/`netcdf-sys`/`hdf5-metno-sys` and their bundled C sources, which is
//! what lets a consumer build for wasm32 and iOS.
//!
//! # The two halves
//!
//! * [`h5`] opens a file and reads a variable's raw storage values **in the
//!   variable's declared width**, bit-reinterpreting through `_Unsigned`. Only
//!   the reader can do that, because only it can ask the library for a specific
//!   width.
//! * [`cf`] applies the rest of the CF packing rules — `_FillValue`,
//!   `valid_range`, `scale_factor`, `add_offset`. The order those come in is
//!   the whole subject; it is spelled out at the top of [`cf`].
//!
//! # Two shapes for the same values
//!
//! A decoded variable is offered in two representations, and the choice is
//! about cost, not meaning — one code path produces both, so a CF rule cannot
//! hold in one and not the other:
//!
//! * [`cf::UnpackedVar`] carries `Vec<Option<f64>>`: missing is `None`. Right
//!   for a column of records a caller walks one at a time.
//! * [`cf::UnpackedF32`] carries `Vec<f32>`: missing is `NaN`. Right for a
//!   raster, and **four times cheaper per element** — 16 bytes against 4. On a
//!   15,000,000-element variable that is 240 MB against 60 MB. Pinned by
//!   [`cf::tests::the_raster_form_costs_a_quarter_of_the_option_form`].
//!
//! The `NaN` sentinel is not ambiguous: [`cf::unpack`] already marks a
//! non-finite *unpacked* value missing, so no present value can be `NaN`. Pinned
//! by [`cf::tests::a_non_finite_unpacked_value_is_missing_in_both_representations`].
//!
//! A third form is the same values with **no array at all**:
//! [`cf::UnpackedSink`] takes them one at a time, for a consumer whose own
//! store is narrower than `f32` and which would otherwise allocate the wide
//! array only to narrow it. Same body, same CF rules — see
//! [`h5::Granule::read_unpacked_f32_to`].

pub mod cf;
pub mod h5;

pub use cf::{
    CfAttr, RawValues, RawVar, TimeUnits, UnpackedF32, UnpackedSink, UnpackedVar, VarType,
    attr_is_true, parse_cf_epoch, parse_time_units, reinterpret_unsigned, unpack, unpack_f32,
};
pub use h5::{Granule, StoredFingerprint, Variable};
