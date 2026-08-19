//! The described-job envelope and codec vocabulary — the substrate's half of
//! the offload boundary Phase 3 moves every job kind onto (adversary finding
//! m10: the run envelope was designed here, once, BEFORE any wire code).
//!
//! **Types only.** Nothing in this crate constructs a [`JobCodec`] row: the
//! overlay rows land in `rustdar-overlays` (WO-M6.2), the radar rows in
//! `rustdar-radar` (WO-M7.1), and the frontend flips onto the composed table
//! at WO-M6.3 / WO-M7.2. This module is the vocabulary they all spell, and it
//! is deliberately kind-free — the substrate never names a radar or overlay
//! type.
//!
//! Two erasure seams and one envelope:
//!
//! * [`DescribedJob`] / [`JobInput`] carry a job's typed input through code
//!   that cannot name the concrete type — the dispatch funnel, the wire, the
//!   worker pool. Equality stays value equality, delegated through
//!   [`JobInput::eq_dyn`], because the retry/discard machinery compares jobs
//!   and the erased form must answer the same way the typed one would.
//! * [`DescribedOut`] / [`JobOut`] are the same seam in the reply direction.
//! * [`JobGeometry`] is the one run envelope both halves of the boundary
//!   express. One envelope rather than one per kind, because the encode
//!   context and the run signature would otherwise fork per kind — and the
//!   fork is exactly where the substrate would start naming kinds.
//!
//! **The one downcast.** [`JobCodec::of`] builds monomorphized shims over a
//! [`JobOutCodec`], and those shims are the ONE place where the erased forms
//! meet the typed ones. Everything above them stays erased; everything below
//! — a row's `encode`/`decode`/`run`/`encode_out`/`decode_out` — stays typed.

use std::any::{Any, TypeId};

use crate::wire::Reader;

/// A job's typed input, seen through the erasure seam.
///
/// Implement it with [`impl_job_input!`](crate::impl_job_input) — the macro
/// writes the only correct shape (`as_any` answers `self`; `eq_dyn` downcasts
/// and compares values), and a hand-rolled variant that answered differently
/// would silently change retry/discard equality.
pub trait JobInput: std::fmt::Debug + Send + Sync + 'static {
    fn as_any(&self) -> &dyn std::any::Any;
    /// Value equality through the erased type: `false` whenever `other` is a
    /// different concrete type, the typed `==` otherwise.
    fn eq_dyn(&self, other: &dyn JobInput) -> bool;
}

/// Implements [`JobInput`](crate::job::JobInput) for a `Debug + PartialEq +
/// Send + Sync + 'static` type: `as_any` answers `self`, and `eq_dyn` is a
/// downcast followed by the type's own `==`.
#[macro_export]
macro_rules! impl_job_input {
    ($t:ty) => {
        impl $crate::job::JobInput for $t {
            fn as_any(&self) -> &dyn ::std::any::Any {
                self
            }

            fn eq_dyn(&self, other: &dyn $crate::job::JobInput) -> bool {
                other
                    .as_any()
                    .downcast_ref::<$t>()
                    .is_some_and(|o| o == self)
            }
        }
    };
}

/// A job input with its concrete type erased. Cloning clones the `Arc`, not
/// the payload; equality is [`JobInput::eq_dyn`] — value equality when the
/// types match, `false` when they do not.
#[derive(Clone, Debug)]
pub struct DescribedJob(pub std::sync::Arc<dyn JobInput>);

impl PartialEq for DescribedJob {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_dyn(&*other.0)
    }
}

impl DescribedJob {
    pub fn new<T: JobInput>(v: T) -> Self {
        Self(std::sync::Arc::new(v))
    }

    /// The typed view back, `None` if the job holds some other input type.
    pub fn downcast_ref<T: JobInput>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref::<T>()
    }
}

/// The ONE run envelope both halves express (m10). Overlay rows read
/// width/height/bounds, ignore side_ceiling_px (fill 0); radar raster rows
/// read side_ceiling_px, ignore the rest.
///
/// `bounds` is the workspace's one geographic-bounds type,
/// [`rustdar_geo::GeoBounds`], named at its one spelling since WO-G4 killed
/// the substrate's re-export — so the envelope freezes on the type every
/// crate above already names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JobGeometry {
    pub width: u32,
    pub height: u32,
    pub bounds: rustdar_geo::GeoBounds,
    pub side_ceiling_px: u32,
}

/// What `encode` sees beyond the input itself. Carrying the full
/// [`JobGeometry`] is what lets a row cut its payload to the texture bounds
/// at encode time (the model row's window cut) — size-cut-at-encode survives
/// the move onto the codec table.
pub struct EncodeCtx {
    pub geometry: JobGeometry,
}

/// A job's typed output, seen through the erasure seam. Implemented by hand
/// where the output type lives (`as_any` answers `self`; `into_any` answers
/// the box) — the reply direction has no equality contract, so there is no
/// macro to get wrong.
pub trait JobOut: std::fmt::Debug + Send + 'static {
    fn as_any(&self) -> &dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
    /// Every raster this output carries in **straight** alpha, for the run
    /// funnel to premultiply in place — after which the buffers are in the
    /// consumers' premultiplied convention and this method's contract is
    /// spent.
    ///
    /// **Required, no default, deliberately**: every output type states its
    /// premultiply posture, so a new kind carrying pixels has to say whether
    /// they need converting instead of silently declining to be — the same
    /// property the funnel's old exhaustive match had, now structural. An
    /// output with no rasters (a voxel grid, a decoded volume) answers an
    /// empty `Vec` and says so; one whose convention is dynamic (the overlay
    /// raster's declared `AlphaMode`) flips its own declaration to
    /// premultiplied as it hands the buffer over, so the statement and the
    /// buffer cannot come to disagree.
    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]>;
}

/// A job output with its concrete type erased.
#[derive(Debug)]
pub struct DescribedOut(pub Box<dyn JobOut>);

impl DescribedOut {
    /// The typed output back out, by value. `None` if the reply holds some
    /// other type — the mismatched payload is consumed either way.
    pub fn take<T: JobOut>(self) -> Option<T> {
        self.0.into_any().downcast::<T>().ok().map(|boxed| *boxed)
    }

    /// The typed view without consuming, `None` on a type mismatch.
    pub fn downcast_ref<T: JobOut>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref::<T>()
    }
}

/// What a job costs to run. INFORMATIONAL this phase: every row declares it,
/// nothing routes a pool lane on it yet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JobCost {
    Raster,
    VolumeDecode,
}

/// One row of the job registry: everything the boundary needs to carry one
/// job kind, as plain function pointers so a registry of rows can be a
/// `static`. Built only through [`JobCodec::of`].
pub struct JobCodec {
    pub label: &'static str,
    /// The row's input `TypeId`, held as a function rather than a value:
    /// `TypeId::of` is not const-stable, so a `const`-built row stores
    /// `TypeId::of::<In>` itself and the registry calls it when scanning
    /// rows for a job's type.
    pub input_type: fn() -> std::any::TypeId,
    pub encode: fn(&DescribedJob, &EncodeCtx, &mut Vec<u8>),
    /// Decodes one job off the cursor. Takes AND returns the [`JobGeometry`]:
    /// the caller parses the shared header and hands the envelope in; a row
    /// whose legacy layout carried a geometry field inside its payload filled
    /// it in and returned the amended envelope (the radar rasters'
    /// `side_ceiling_px`, until WO-M7b canonicalised the framing), while rows
    /// with none pass it through unchanged. The pass-through is load-bearing
    /// — the returned value is the one the job runs under; do not simplify
    /// it away.
    pub decode:
        fn(&mut crate::wire::Reader<'_>, JobGeometry) -> Option<(DescribedJob, JobGeometry)>,
    pub run: fn(&DescribedJob, &JobGeometry) -> Option<DescribedOut>,
    /// Reply-direction encoder. De-Optioned at WO-M7c: every row states its
    /// reply codec or the workspace does not compile — there is no
    /// named-field reply path left for a row to ride instead. CONSUMES the
    /// out (the one caller, the worker's `execute_encoded`, owns it) and
    /// splits it across a head sink and a tails sink (WO-M7d) — see
    /// [`JobOutCodec`] for the head/tails convention.
    pub encode_out: fn(DescribedOut, &mut Vec<u8>, &mut Vec<Vec<u8>>),
    /// Reply-direction decoder; the `Option` in its return is the decode
    /// failing, never absence. Takes the tails **by value** so a decoder
    /// can ADOPT a buffer — the page-side image copy WO-M7d killed lived
    /// exactly in a decoder that could only borrow.
    pub decode_out: fn(&[u8], Vec<Vec<u8>>) -> Option<DescribedOut>,
    pub cost: JobCost,
}

/// One job kind, fully typed. A row is this trait monomorphized behind
/// [`JobCodec`]'s function pointers.
pub trait JobSpec: 'static {
    type In: JobInput;
    type Out: JobOut;
    const LABEL: &'static str;
    const COST: JobCost;

    fn encode(input: &Self::In, ctx: &EncodeCtx, out: &mut Vec<u8>);
    /// Takes AND returns the [`JobGeometry`] on the same contract as
    /// [`JobCodec::decode`].
    fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(Self::In, JobGeometry)>;
    fn run(input: &Self::In, geo: &JobGeometry) -> Option<Self::Out>;
}

/// The reply-direction codecs. Separate from [`JobSpec`] in trait shape only
/// — since WO-M7c closed the reply direction, a row cannot be built without
/// this half ([`JobCodec::of`] bounds on it), so every kind states both
/// directions or does not exist.
///
/// # Heads and tails (WO-M7d)
///
/// A reply is one `head` plus zero or more `tails`. The tails are the row's
/// LARGE FLAT buffers, nominated by the encoder to ride the browser's
/// transfer list as separate buffers instead of being concatenated into the
/// head — each concatenation was a multi-MiB memcpy where the job ran, and
/// the page paid another to carve the buffer back out. A row with no large
/// buffers writes everything into `head` and leaves `tails` empty.
/// `encode_out` consumes the output so a tail can be the output's own
/// buffer, moved, never copied; `decode_out` takes the tails by value so it
/// can adopt a buffer the same way.
///
/// **Decoders REFUSE a tail count they did not write.** The wire is
/// same-build-only (the build token refuses every other pairing at the
/// handshake), so a wrong count is a corrupt or foreign message — and half
/// a frame believed is worse than none.
pub trait JobOutCodec: JobSpec {
    fn encode_out(v: Self::Out, head: &mut Vec<u8>, tails: &mut Vec<Vec<u8>>);
    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<Self::Out>;
}

impl JobCodec {
    /// A row over `S`, both directions. The shims this stores are the ONE
    /// downcast in the system — see the module doc.
    ///
    /// (This was `with_out`, beside an out-less `of`, while the frame
    /// replies still rode the browser port as named fields; WO-M7c deleted
    /// the out-less constructor with that path, so the one way to build a
    /// row states its reply codec.)
    pub const fn of<S: JobOutCodec>() -> Self {
        Self {
            label: S::LABEL,
            input_type: TypeId::of::<S::In>,
            encode: encode_shim::<S>,
            decode: decode_shim::<S>,
            run: run_shim::<S>,
            encode_out: encode_out_shim::<S>,
            decode_out: decode_out_shim::<S>,
            cost: S::COST,
        }
    }
}

/// `encode` has no error channel, so a job routed to the wrong row panics
/// rather than encoding nothing: a zero-byte payload would read as a green
/// send and fail far from the defect. Unreachable when rows are selected by
/// `input_type` — the panic exists for the registry bug that breaks that.
fn encode_shim<S: JobSpec>(job: &DescribedJob, ctx: &EncodeCtx, out: &mut Vec<u8>) {
    let input = job.downcast_ref::<S::In>().unwrap_or_else(|| {
        panic!(
            "a `{}` codec row was asked to encode {job:?} — the registry \
             routed a job to the wrong row",
            S::LABEL,
        )
    });
    S::encode(input, ctx, out);
}

fn decode_shim<S: JobSpec>(
    r: &mut Reader<'_>,
    geo: JobGeometry,
) -> Option<(DescribedJob, JobGeometry)> {
    let (input, geo) = S::decode(r, geo)?;
    Some((DescribedJob::new(input), geo))
}

/// A type mismatch answers `None` through `run`'s existing failure channel.
/// Via the wire it is unreachable (`decode` built the input as `S::In`); on
/// the direct path the registry selects the row by `input_type`.
fn run_shim<S: JobSpec>(job: &DescribedJob, geo: &JobGeometry) -> Option<DescribedOut> {
    let input = job.downcast_ref::<S::In>()?;
    let out = S::run(input, geo)?;
    Some(DescribedOut(Box::new(out)))
}

/// Panics on a mismatch on the same grounds as the encode shim. The typed
/// check runs BEFORE the consuming `take` so the panic can still print the
/// foreign reply it refused to encode.
fn encode_out_shim<S: JobOutCodec>(v: DescribedOut, head: &mut Vec<u8>, tails: &mut Vec<Vec<u8>>) {
    if v.downcast_ref::<S::Out>().is_none() {
        panic!(
            "a `{}` codec row was asked to encode the reply {v:?} — the \
             registry routed a reply to the wrong row",
            S::LABEL,
        );
    }
    let v = v.take::<S::Out>().expect("the downcast above succeeded");
    S::encode_out(v, head, tails);
}

fn decode_out_shim<S: JobOutCodec>(head: &[u8], tails: Vec<Vec<u8>>) -> Option<DescribedOut> {
    let v = S::decode_out(head, tails)?;
    Some(DescribedOut(Box::new(v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two input fixtures with the same payload shape, so a cross-type
    /// compare cannot pass by comparing bytes.
    #[derive(Debug, PartialEq)]
    struct AlphaInput {
        value: u32,
    }
    impl_job_input!(AlphaInput);

    #[derive(Debug, PartialEq)]
    struct BetaInput {
        value: u32,
    }
    impl_job_input!(BetaInput);

    #[derive(Debug, PartialEq)]
    struct AlphaOut {
        value: u32,
    }
    impl JobOut for AlphaOut {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn into_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }

        fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
            Vec::new()
        }
    }

    #[derive(Debug)]
    struct BetaOut;
    impl JobOut for BetaOut {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn into_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }

        fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]> {
            Vec::new()
        }
    }

    #[test]
    fn eq_dyn_is_reflexive_and_value_based() {
        let a = DescribedJob::new(AlphaInput { value: 7 });
        assert_eq!(a, a.clone(), "a job must equal its own clone");
        assert_eq!(
            a,
            DescribedJob::new(AlphaInput { value: 7 }),
            "equal values behind the same type must compare equal",
        );
        assert_ne!(
            a,
            DescribedJob::new(AlphaInput { value: 8 }),
            "unequal values must compare unequal — an eq_dyn answering a \
             constant `true` would green every weaker check",
        );
    }

    #[test]
    fn eq_dyn_is_false_across_input_types() {
        let a = DescribedJob::new(AlphaInput { value: 7 });
        let b = DescribedJob::new(BetaInput { value: 7 });
        // Both directions: eq_dyn is asymmetric in form (self downcasts
        // other), so one green direction does not imply the other.
        assert_ne!(a, b);
        assert_ne!(b, a);
        // The control that keeps the asserts above about the TYPE: the same
        // payload behind the same type is equal.
        assert_eq!(b, DescribedJob::new(BetaInput { value: 7 }));
    }

    #[test]
    fn take_answers_none_for_the_wrong_out_type() {
        assert!(
            DescribedOut(Box::new(AlphaOut { value: 5 }))
                .take::<BetaOut>()
                .is_none()
        );
        // The control: the right type gets the payload back out by value — a
        // take that always answered None would green the assert above.
        assert_eq!(
            DescribedOut(Box::new(AlphaOut { value: 5 })).take::<AlphaOut>(),
            Some(AlphaOut { value: 5 }),
        );
    }

    #[test]
    fn downcast_ref_answers_the_typed_view_only_for_the_right_type() {
        let job = DescribedJob::new(AlphaInput { value: 9 });
        assert_eq!(
            job.downcast_ref::<AlphaInput>(),
            Some(&AlphaInput { value: 9 }),
        );
        assert!(job.downcast_ref::<BetaInput>().is_none());

        let out = DescribedOut(Box::new(AlphaOut { value: 9 }));
        assert_eq!(out.downcast_ref::<AlphaOut>(), Some(&AlphaOut { value: 9 }));
        assert!(out.downcast_ref::<BetaOut>().is_none());
    }

    /// A minimal spec over the fixtures: one `u32` on the wire, `run` doubles
    /// it, the geometry passes through untouched.
    struct DoublingSpec;
    impl JobSpec for DoublingSpec {
        type In = AlphaInput;
        type Out = AlphaOut;
        const LABEL: &'static str = "test/doubling";
        const COST: JobCost = JobCost::Raster;

        fn encode(input: &Self::In, _ctx: &EncodeCtx, out: &mut Vec<u8>) {
            out.extend_from_slice(&input.value.to_le_bytes());
        }

        fn decode(r: &mut Reader<'_>, geo: JobGeometry) -> Option<(Self::In, JobGeometry)> {
            Some((AlphaInput { value: r.u32()? }, geo))
        }

        fn run(input: &Self::In, _geo: &JobGeometry) -> Option<Self::Out> {
            Some(AlphaOut {
                value: input.value * 2,
            })
        }
    }
    impl JobOutCodec for DoublingSpec {
        fn encode_out(v: Self::Out, head: &mut Vec<u8>, _tails: &mut Vec<Vec<u8>>) {
            head.extend_from_slice(&v.value.to_le_bytes());
        }

        // No large buffers: everything rides the head, and a tail count
        // this spec never writes is refused per the trait's convention.
        fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<Self::Out> {
            if !tails.is_empty() {
                return None;
            }
            let mut r = Reader::new(head);
            let v = AlphaOut { value: r.u32()? };
            r.at_end().then_some(v)
        }
    }

    /// The `const`-construction proof for the `TypeId` trap: `TypeId::of` is
    /// not const-stable, so a row must store `fn() -> TypeId` — this item
    /// failing to build IS the regression, no assertion needed for that
    /// half.
    const DOUBLING_ROW: JobCodec = JobCodec::of::<DoublingSpec>();

    fn test_geometry() -> JobGeometry {
        JobGeometry {
            width: 4,
            height: 2,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 1.0,
                max_lat: 2.0,
                min_lon: 3.0,
                max_lon: 4.0,
            },
            side_ceiling_px: 0,
        }
    }

    #[test]
    fn a_const_built_row_reports_its_spec_and_routes_through_it() {
        assert_eq!(DOUBLING_ROW.label, "test/doubling");
        assert_eq!(DOUBLING_ROW.cost, JobCost::Raster);
        assert_eq!((DOUBLING_ROW.input_type)(), TypeId::of::<AlphaInput>());

        // encode -> decode round-trips through the shims, and decode hands
        // back the geometry it was given (this spec amends nothing).
        let job = DescribedJob::new(AlphaInput { value: 21 });
        let mut bytes = Vec::new();
        (DOUBLING_ROW.encode)(
            &job,
            &EncodeCtx {
                geometry: test_geometry(),
            },
            &mut bytes,
        );
        let mut r = Reader::new(&bytes);
        let (decoded, geo) =
            (DOUBLING_ROW.decode)(&mut r, test_geometry()).expect("the round-trip decodes");
        assert_eq!(decoded, job);
        assert_eq!(geo, test_geometry());
        assert!(r.at_end(), "decode consumed exactly what encode wrote");

        // run: the typed answer comes back erased; a foreign input type is
        // not this row's to run and answers None through run's own channel.
        let out = (DOUBLING_ROW.run)(&job, &test_geometry()).expect("the spec runs");
        assert_eq!(out.take::<AlphaOut>(), Some(AlphaOut { value: 42 }));
        assert!(
            (DOUBLING_ROW.run)(
                &DescribedJob::new(BetaInput { value: 21 }),
                &test_geometry()
            )
            .is_none(),
        );

        // The reply half round-trips the same way — carried by every row,
        // not an optional half, since WO-M7c closed the reply direction.
        // Encode consumes the out and splits head/tails (WO-M7d); this spec
        // nominates no tails, and its decoder refuses any it is handed, so
        // handing the tails straight back through is itself the check.
        let reply = DescribedOut(Box::new(AlphaOut { value: 42 }));
        let mut reply_head = Vec::new();
        let mut reply_tails = Vec::new();
        (DOUBLING_ROW.encode_out)(reply, &mut reply_head, &mut reply_tails);
        let back =
            (DOUBLING_ROW.decode_out)(&reply_head, reply_tails).expect("the reply round-trips");
        assert_eq!(back.take::<AlphaOut>(), Some(AlphaOut { value: 42 }));
    }
}
