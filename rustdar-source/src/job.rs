//! The described-job envelope and codec vocabulary — the substrate's half of
//! the offload boundary. Types only, and kind-free.
//!
//! [`DescribedJob`] / [`JobInput`] carry a job's typed input through code that
//! cannot name the concrete type; equality stays value equality through
//! [`JobInput::eq_dyn`], because the retry/discard machinery compares jobs.
//! [`DescribedOut`] / [`JobOut`] are the same seam in the reply direction.
//! [`JobCodec::of`]'s shims are the ONE place erased forms meet typed ones.

use std::any::{Any, TypeId};

use crate::wire::Reader;

/// A job's typed input, seen through the erasure seam. Implement it with
/// [`impl_job_input!`](crate::impl_job_input): a hand-rolled variant would
/// silently change retry/discard equality.
pub trait JobInput: std::fmt::Debug + Send + Sync + 'static {
    fn as_any(&self) -> &dyn std::any::Any;
    fn eq_dyn(&self, other: &dyn JobInput) -> bool;
}

/// Implements [`JobInput`](crate::job::JobInput) for a `Debug + PartialEq +
/// Send + Sync + 'static` type.
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
/// the payload; equality is [`JobInput::eq_dyn`].
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

    pub fn downcast_ref<T: JobInput>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref::<T>()
    }
}

/// The ONE run envelope both halves express. Overlay rows read
/// width/height/bounds and ignore `side_ceiling_px` (fill 0); radar raster rows
/// read `side_ceiling_px` and ignore the rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JobGeometry {
    pub width: u32,
    pub height: u32,
    pub bounds: rustdar_geo::GeoBounds,
    pub side_ceiling_px: u32,
}

/// What `encode` sees beyond the input itself: the full [`JobGeometry`] is what
/// lets a row cut its payload to the texture bounds at encode time.
pub struct EncodeCtx {
    pub geometry: JobGeometry,
}

/// A job's typed output, seen through the erasure seam. Implemented by hand
/// where the output type lives; the reply direction has no equality contract.
pub trait JobOut: std::fmt::Debug + Send + 'static {
    fn as_any(&self) -> &dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
    /// Every raster this output carries in **straight** alpha, for the run
    /// funnel to premultiply in place. Required, no default: every output type
    /// states its premultiply posture.
    fn straight_rasters_mut(&mut self) -> Vec<&mut [u8]>;
}

#[derive(Debug)]
pub struct DescribedOut(pub Box<dyn JobOut>);

impl DescribedOut {
    pub fn take<T: JobOut>(self) -> Option<T> {
        self.0.into_any().downcast::<T>().ok().map(|boxed| *boxed)
    }

    pub fn downcast_ref<T: JobOut>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref::<T>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JobCost {
    Raster,
    VolumeDecode,
}

/// One row of the job registry, as plain function pointers so a registry can be
/// a `static`. Built only through [`JobCodec::of`].
pub struct JobCodec {
    pub label: &'static str,
    /// The row's input `TypeId` as a function, because `TypeId::of` is not
    /// const-stable and a `const`-built row must store `TypeId::of::<In>`.
    pub input_type: fn() -> std::any::TypeId,
    pub encode: fn(&DescribedJob, &EncodeCtx, &mut Vec<u8>),
    /// Decodes one job off the cursor. Takes AND returns the [`JobGeometry`]: a
    /// row whose payload amends the envelope returns the amended one, and that
    /// is the one the job runs under.
    pub decode:
        fn(&mut crate::wire::Reader<'_>, JobGeometry) -> Option<(DescribedJob, JobGeometry)>,
    pub run: fn(&DescribedJob, &JobGeometry) -> Option<DescribedOut>,
    /// Reply-direction encoder. CONSUMES the out and splits it across a head
    /// sink and a tails sink; see [`JobOutCodec`].
    pub encode_out: fn(DescribedOut, &mut Vec<u8>, &mut Vec<Vec<u8>>),
    /// Reply-direction decoder; the `Option` is the decode failing, never
    /// absence. Takes the tails **by value** so a decoder can ADOPT a buffer.
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

/// The reply-direction codecs. A row cannot be built without this half, so
/// every kind states both directions.
///
/// A reply is one `head` plus zero or more `tails` — the row's large flat
/// buffers, nominated to ride the browser's transfer list separately rather
/// than be concatenated at a multi-MiB memcpy each end. **Decoders REFUSE a
/// tail count they did not write**: the wire is same-build-only.
pub trait JobOutCodec: JobSpec {
    fn encode_out(v: Self::Out, head: &mut Vec<u8>, tails: &mut Vec<Vec<u8>>);
    fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<Self::Out>;
}

impl JobCodec {
    /// A row over `S`, both directions. The shims this stores are the ONE
    /// downcast in the system — see the module doc.
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
/// send and fail far from the defect.
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

/// A type mismatch answers `None` through `run`'s existing failure channel;
/// via the wire it is unreachable, `decode` having built the input as `S::In`.
fn run_shim<S: JobSpec>(job: &DescribedJob, geo: &JobGeometry) -> Option<DescribedOut> {
    let input = job.downcast_ref::<S::In>()?;
    let out = S::run(input, geo)?;
    Some(DescribedOut(Box::new(out)))
}

/// Panics on a mismatch like the encode shim. The typed check runs BEFORE the
/// consuming `take` so the panic can print the reply it refused.
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

    /// Two fixtures with the same payload shape, so a cross-type compare cannot
    /// pass by comparing bytes.
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
        // Both directions: eq_dyn is asymmetric in form (self downcasts other).
        assert_ne!(a, b);
        assert_ne!(b, a);
        assert_eq!(b, DescribedJob::new(BetaInput { value: 7 }));
    }

    #[test]
    fn take_answers_none_for_the_wrong_out_type() {
        assert!(
            DescribedOut(Box::new(AlphaOut { value: 5 }))
                .take::<BetaOut>()
                .is_none()
        );
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

        // No large buffers: everything rides the head, and a tail count this
        // spec never writes is refused.
        fn decode_out(head: &[u8], tails: Vec<Vec<u8>>) -> Option<Self::Out> {
            if !tails.is_empty() {
                return None;
            }
            let mut r = Reader::new(head);
            let v = AlphaOut { value: r.u32()? };
            r.at_end().then_some(v)
        }
    }

    /// The `const`-construction proof for the `TypeId` trap: this item failing
    /// to build IS the regression.
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

        let out = (DOUBLING_ROW.run)(&job, &test_geometry()).expect("the spec runs");
        assert_eq!(out.take::<AlphaOut>(), Some(AlphaOut { value: 42 }));
        assert!(
            (DOUBLING_ROW.run)(
                &DescribedJob::new(BetaInput { value: 21 }),
                &test_geometry()
            )
            .is_none(),
        );

        // The reply half round-trips the same way; this spec nominates no tails
        // and its decoder refuses any it is handed.
        let reply = DescribedOut(Box::new(AlphaOut { value: 42 }));
        let mut reply_head = Vec::new();
        let mut reply_tails = Vec::new();
        (DOUBLING_ROW.encode_out)(reply, &mut reply_head, &mut reply_tails);
        let back =
            (DOUBLING_ROW.decode_out)(&reply_head, reply_tails).expect("the reply round-trips");
        assert_eq!(back.take::<AlphaOut>(), Some(AlphaOut { value: 42 }));
    }
}
