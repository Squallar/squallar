// VENDORED. Four of upstream's five backends are deleted here -- `mmap`
// (`fmmap`), `s3` (`rust-s3`), `aws_s3` (`aws-sdk-s3`) and `object_store`.
// None of them is reachable from this workspace, none of them compiles on
// wasm32, and each carried an optional dependency tree that a `[patch]`ed
// workspace member would have had to resolve. `http` stays because
// `squallar-egui` enables `http-async` on the native target. `slice` is new
// and is not upstream's; see the file. VENDORED.md has the full list.
#[cfg(feature = "http-async")]
mod http;
#[cfg(feature = "http-async")]
pub use crate::backends::http::HttpBackend;
#[cfg(feature = "__async")]
mod slice;
#[cfg(feature = "__async")]
pub use crate::backends::slice::SliceBackend;
