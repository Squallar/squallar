//! The browser's per-tab WebGPU allowance, found by allocating until refused.
//!
//! No browser API states how much GPU memory a tab may hold, and the card's
//! size is not the answer — a tab is allowed a share, decided by the browser.
//! So on a WebGPU page the figure is **probed**: a throwaway device allocates
//! textures in doubling steps, clears each in a render pass so the memory is
//! resident rather than reserved, and stops at the first out-of-memory error or
//! device loss. The last total the device held is the allowance.
//!
//! This module is the arithmetic: which allocation comes next, what shape a
//! texture of that many bytes takes on this adapter, and when the probe stops
//! on its own — at [`ProbePlan::max_bytes`], at [`ProbePlan::time_budget_ms`],
//! or at a shape no texture can take. It never touches a device and runs on
//! the host. The half that does touch one is [`run`], compiled for wasm32
//! alone, and it drives this state machine step by step.
//!
//! WebGL2 pages — every Firefox leg today — take the same ladder through a
//! second instrument, [`webgl2`]: raw `web-sys` calls on a second canvas,
//! because the wgpu path has no error scope and no lost-device callback on
//! that backend and would report silence as a figure. Its ladder is capped
//! by policy where this one runs to 8 GiB; the module says why. Which
//! instrument a page gets is decided by the application's own backend
//! (`squallar_app::platform::gpu_probe_applies_to`), so the figure always
//! describes the API that is drawing.

#[cfg(target_arch = "wasm32")]
pub mod run;
pub mod webgl2;
#[cfg(target_arch = "wasm32")]
pub mod webgl2_run;

/// The first allocation, in bytes: 64 MiB, one 4096 x 4096 RGBA8 texture.
pub const START_BYTES: u64 = 64 << 20;

/// Each step asks for this many times the last.
pub const FACTOR: u64 = 2;

/// The most the probe will hold before calling the device unlimited: 8 GiB.
/// Above this the figure stops being a browser allowance and starts being
/// the card, which is not what the probe is for.
pub const MAX_BYTES: u64 = 8 << 30;

/// Wall time the probe may spend, in milliseconds. It runs after the first
/// frame and beside the page's own boot; a step whose predicted duration would
/// carry the total past this is not taken.
pub const TIME_BUDGET_MS: u64 = 2000;

/// Bytes per texel of the format probed, `Rgba8Unorm`.
pub const BYTES_PER_TEXEL: u64 = 4;

/// The probe's constants plus the two adapter limits that shape its textures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbePlan {
    /// See [`START_BYTES`].
    pub start_bytes: u64,
    /// See [`FACTOR`].
    pub factor: u64,
    /// See [`MAX_BYTES`].
    pub max_bytes: u64,
    /// See [`TIME_BUDGET_MS`].
    pub time_budget_ms: u64,
    /// The adapter's `maxTextureDimension2D`.
    pub max_texture_dimension_2d: u32,
    /// The adapter's `maxTextureArrayLayers`.
    pub max_texture_array_layers: u32,
}

impl ProbePlan {
    /// The shipped constants on an adapter reporting these two limits.
    pub fn for_adapter(max_texture_dimension_2d: u32, max_texture_array_layers: u32) -> Self {
        Self {
            start_bytes: START_BYTES,
            factor: FACTOR,
            max_bytes: MAX_BYTES,
            time_budget_ms: TIME_BUDGET_MS,
            max_texture_dimension_2d,
            max_texture_array_layers,
        }
    }
}

/// One texture the probe asks the device for: a square 2D array whose side is
/// a power of two, so every step from [`START_BYTES`] upward is exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocation {
    /// What the texture costs: `width * height * layers * BYTES_PER_TEXEL`.
    /// At least the bytes asked for, and equal to them whenever those are a
    /// power of two at or above one layer.
    pub bytes: u64,
    /// Texels per side.
    pub width: u32,
    /// Texels per side.
    pub height: u32,
    /// Array layers, each cleared in its own render pass.
    pub layers: u32,
}

/// The shape of a texture holding at least `bytes` on an adapter with
/// `plan`'s limits, or `None` when no single texture can: the side is the
/// largest power of two under the 2D limit that does not overshoot the bytes,
/// and layers make up the rest.
pub fn shape(bytes: u64, plan: &ProbePlan) -> Option<Allocation> {
    let texels = (bytes / BYTES_PER_TEXEL).max(1);
    // The largest power of two whose square fits the texels: half the bit
    // width, floored.
    let side_from_bytes = 1u64 << (texels.ilog2() / 2);
    let side_from_limit = 1u64 << u64::from(plan.max_texture_dimension_2d.max(1)).ilog2();
    let side = side_from_bytes.min(side_from_limit);
    let layer_bytes = side * side * BYTES_PER_TEXEL;
    let layers = bytes.div_ceil(layer_bytes).max(1);
    if layers > u64::from(plan.max_texture_array_layers) {
        return None;
    }
    Some(Allocation {
        bytes: layers * layer_bytes,
        width: side as u32,
        height: side as u32,
        layers: layers as u32,
    })
}

/// What the device did with one allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepResult {
    /// Created, cleared and submitted with no error in the scope.
    Held,
    /// A `GPUOutOfMemoryError` landed in the scope.
    Refused,
    /// The device was lost during the step. A lost device answers every
    /// later call as if it succeeded, so this is a refusal and the end.
    Lost,
}

/// What the probe found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// The last total held resident without refusal, in bytes. `0` when the
    /// first allocation was refused or the probe never ran.
    pub last_ok_bytes: u64,
    /// The total the device refused to reach — the last held total plus the
    /// allocation that failed — or `None` when it never refused.
    pub failed_at: Option<u64>,
    /// Allocations attempted, held or not.
    pub steps: u32,
    /// Wall time from the first allocation to the end, in milliseconds.
    pub elapsed_ms: u32,
    /// Whether the probe stopped at one of its own bounds — the byte ceiling,
    /// the time budget, a shape no texture takes — rather than at a refusal.
    /// A capped figure is a floor on the allowance, not the allowance.
    pub capped: bool,
}

/// The allowance an outcome amounts to: the last total held, or `None` when
/// nothing was — the first allocation refused, or the probe never ran.
pub fn capacity_from(outcome: &ProbeOutcome) -> Option<u64> {
    (outcome.last_ok_bytes > 0).then_some(outcome.last_ok_bytes)
}

/// The probe's state between steps: [`Self::next`] says what to ask for,
/// [`Self::record`] hears what happened, [`Self::finish`] sums it up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    plan: ProbePlan,
    total_ok: u64,
    steps: u32,
    next_bytes: u64,
    last_step_ms: u64,
    failed_at: Option<u64>,
    capped: bool,
    done: bool,
}

impl Probe {
    /// A probe about to ask for its first allocation.
    pub fn new(plan: ProbePlan) -> Self {
        Self {
            plan,
            total_ok: 0,
            steps: 0,
            next_bytes: plan.start_bytes,
            last_step_ms: 0,
            failed_at: None,
            capped: false,
            done: false,
        }
    }

    /// The next texture to ask for, or `None` when the probe is over — a
    /// refusal has been recorded, the byte ceiling is reached, the time budget
    /// would be spent, or the step has no shape. The last allocation under the
    /// ceiling is clamped to reach it exactly, so a device that never refuses
    /// reports [`ProbePlan::max_bytes`] and `capped`.
    ///
    /// `elapsed_ms` is wall time since the first allocation. A step is
    /// predicted to take [`ProbePlan::factor`] times the last one — a clear
    /// runs in proportion to the bytes — and is not taken when that would
    /// carry the total past the budget.
    pub fn next(&mut self, elapsed_ms: u64) -> Option<Allocation> {
        if self.done {
            return None;
        }
        let predicted = self.last_step_ms.saturating_mul(self.plan.factor);
        if elapsed_ms.saturating_add(predicted) > self.plan.time_budget_ms {
            return self.stop_capped();
        }
        let room = self.plan.max_bytes.saturating_sub(self.total_ok);
        if room == 0 {
            return self.stop_capped();
        }
        match shape(self.next_bytes.min(room), &self.plan) {
            Some(allocation) => Some(allocation),
            None => self.stop_capped(),
        }
    }

    fn stop_capped(&mut self) -> Option<Allocation> {
        self.capped = true;
        self.done = true;
        None
    }

    /// Stop the probe at a bound of the caller's — a step abandoned before
    /// it could be judged, a fault that is not a refusal. Reported as
    /// `capped`, exactly as the probe's own bounds are: nothing refused.
    pub fn cap(&mut self) {
        self.stop_capped();
    }

    /// The total held without refusal so far.
    pub fn held_bytes(&self) -> u64 {
        self.total_ok
    }

    /// What the device did with the allocation [`Self::next`] handed out, and
    /// how long the step took.
    pub fn record(&mut self, allocation: Allocation, result: StepResult, step_ms: u64) {
        self.steps = self.steps.saturating_add(1);
        self.last_step_ms = step_ms;
        match result {
            StepResult::Held => {
                self.total_ok = self.total_ok.saturating_add(allocation.bytes);
                self.next_bytes = self.next_bytes.saturating_mul(self.plan.factor);
            }
            StepResult::Refused | StepResult::Lost => {
                self.failed_at = Some(self.total_ok.saturating_add(allocation.bytes));
                self.done = true;
            }
        }
    }

    /// The outcome, `elapsed_ms` after the first allocation.
    pub fn finish(self, elapsed_ms: u64) -> ProbeOutcome {
        ProbeOutcome {
            last_ok_bytes: self.total_ok,
            failed_at: self.failed_at,
            steps: self.steps,
            elapsed_ms: u32::try_from(elapsed_ms).unwrap_or(u32::MAX),
            capped: self.capped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    /// A desktop WebGPU adapter at the defaults a device gets without asking:
    /// 8192 px textures, 256 layers.
    fn desktop() -> ProbePlan {
        ProbePlan::for_adapter(8192, 256)
    }

    /// Run the plan against a device that holds everything, in no time.
    fn run_to_the_cap(plan: ProbePlan) -> (Vec<Allocation>, ProbeOutcome) {
        let mut probe = Probe::new(plan);
        let mut asked = Vec::new();
        while let Some(allocation) = probe.next(0) {
            asked.push(allocation);
            probe.record(allocation, StepResult::Held, 1);
        }
        (asked, probe.finish(9))
    }

    /// The doubling sequence from 64 MiB, and the last step clamped so the
    /// total lands on the ceiling exactly: a device that never refuses reports
    /// `max_bytes`, capped.
    #[test]
    fn a_device_that_never_refuses_reports_the_ceiling_and_is_capped() {
        let (asked, outcome) = run_to_the_cap(desktop());
        let sizes: Vec<u64> = asked.iter().map(|a| a.bytes / MIB).collect();
        assert_eq!(sizes, [64, 128, 256, 512, 1024, 2048, 4096, 64]);
        assert_eq!(outcome.last_ok_bytes, 8 * GIB);
        assert_eq!(outcome.failed_at, None);
        assert_eq!(outcome.steps, 8);
        assert!(outcome.capped, "a probe stopped by its own ceiling says so");
        assert_eq!(capacity_from(&outcome), Some(8 * GIB));
        assert_eq!(outcome.elapsed_ms, 9);
    }

    /// Held, held, held, refused: the allowance is the total before the
    /// refusal, `failed_at` is the total that was refused, and the probe asks
    /// for nothing more.
    #[test]
    fn the_probe_stops_at_the_first_refusal() {
        let mut probe = Probe::new(desktop());
        for _ in 0..3 {
            let allocation = probe.next(0).expect("a step under the ceiling");
            probe.record(allocation, StepResult::Held, 10);
        }
        let refused = probe.next(30).expect("the fourth step");
        assert_eq!(refused.bytes, 512 * MIB);
        probe.record(refused, StepResult::Refused, 10);
        assert_eq!(probe.next(40), None, "a refusal ends the probe");

        let outcome = probe.finish(40);
        assert_eq!(outcome.last_ok_bytes, (64 + 128 + 256) * MIB);
        assert_eq!(outcome.failed_at, Some((64 + 128 + 256 + 512) * MIB));
        assert_eq!(outcome.steps, 4);
        assert!(
            !outcome.capped,
            "a refusal is the device's bound, not the probe's"
        );
        assert_eq!(capacity_from(&outcome), Some(448 * MIB));
    }

    /// A lost device is a refusal: it would answer every later allocation as
    /// held, so the probe must not ask.
    #[test]
    fn a_lost_device_ends_the_probe_as_a_refusal_does() {
        let mut probe = Probe::new(desktop());
        let first = probe.next(0).unwrap();
        probe.record(first, StepResult::Held, 5);
        let second = probe.next(5).unwrap();
        probe.record(second, StepResult::Lost, 5);
        assert_eq!(probe.next(10), None);
        let outcome = probe.finish(10);
        assert_eq!(outcome.last_ok_bytes, 64 * MIB);
        assert_eq!(outcome.failed_at, Some(192 * MIB));
        assert!(!outcome.capped);
    }

    /// The first allocation refused: no capacity, and not capped either.
    #[test]
    fn a_first_refusal_yields_no_capacity() {
        let mut probe = Probe::new(desktop());
        let first = probe.next(0).unwrap();
        probe.record(first, StepResult::Refused, 1);
        let outcome = probe.finish(1);
        assert_eq!(outcome.last_ok_bytes, 0);
        assert_eq!(outcome.failed_at, Some(64 * MIB));
        assert_eq!(capacity_from(&outcome), None);
        assert!(!outcome.capped);
    }

    /// A probe that never asked — no adapter, no device — has no capacity, no
    /// steps, and is not capped: nothing bounded it, nothing ran.
    #[test]
    fn a_probe_that_never_ran_has_no_capacity_and_is_not_capped() {
        let outcome = Probe::new(desktop()).finish(0);
        assert_eq!(outcome, ProbeOutcome::default());
        assert_eq!(capacity_from(&outcome), None);
    }

    /// A step predicted to overrun the time budget is not taken: the last
    /// step took 700 ms, the next is predicted at 1400, and 1400 + 1400 is
    /// past 2000. What was held stands, capped.
    #[test]
    fn the_time_budget_caps_the_probe_before_the_overrunning_step() {
        let mut probe = Probe::new(desktop());
        let first = probe.next(0).unwrap();
        probe.record(first, StepResult::Held, 600);
        let second = probe.next(600).expect("600 + 1200 fits 2000");
        probe.record(second, StepResult::Held, 700);
        assert_eq!(probe.next(1400), None, "1400 + 1400 overruns the budget");
        let outcome = probe.finish(1400);
        assert_eq!(outcome.last_ok_bytes, 192 * MIB);
        assert_eq!(outcome.failed_at, None);
        assert!(outcome.capped);
        assert_eq!(outcome.steps, 2);
    }

    /// Adapter and device requests that alone ate the budget leave a probe
    /// that asks for nothing: zero steps, capped, no capacity.
    #[test]
    fn a_budget_spent_before_the_first_step_asks_for_nothing() {
        let mut probe = Probe::new(desktop());
        assert_eq!(probe.next(TIME_BUDGET_MS + 1), None);
        let outcome = probe.finish(TIME_BUDGET_MS + 1);
        assert_eq!(outcome.steps, 0);
        assert!(outcome.capped);
        assert_eq!(capacity_from(&outcome), None);
    }

    /// Every step from 64 MiB up is a square power-of-two texture whose bytes
    /// are exact, and at the 8192 default a step past 256 MiB spreads over
    /// layers rather than growing a side the device would refuse.
    #[test]
    fn every_step_is_exact_and_inside_the_adapters_limits() {
        let plan = desktop();
        let (asked, _) = run_to_the_cap(plan);
        let shapes: Vec<(u32, u32, u32)> = asked
            .iter()
            .map(|a| (a.width, a.height, a.layers))
            .collect();
        assert_eq!(
            shapes,
            [
                (4096, 4096, 1),
                (4096, 4096, 2),
                (8192, 8192, 1),
                (8192, 8192, 2),
                (8192, 8192, 4),
                (8192, 8192, 8),
                (8192, 8192, 16),
                (4096, 4096, 1),
            ]
        );
        for allocation in &asked {
            assert!(allocation.width <= plan.max_texture_dimension_2d);
            assert!(allocation.layers <= plan.max_texture_array_layers);
            assert_eq!(
                allocation.bytes,
                u64::from(allocation.width)
                    * u64::from(allocation.height)
                    * u64::from(allocation.layers)
                    * BYTES_PER_TEXEL
            );
        }
    }

    /// A limit that is not a power of two — Firefox's WebGPU reports 32767 —
    /// rounds down to one, and a wide adapter takes each step as one layer.
    #[test]
    fn a_wide_adapter_takes_each_step_in_one_layer() {
        let plan = ProbePlan::for_adapter(32767, 256);
        let step = shape(4 * GIB, &plan).unwrap();
        assert_eq!((step.width, step.height, step.layers), (16384, 16384, 4));
        let step = shape(GIB, &plan).unwrap();
        assert_eq!((step.width, step.height, step.layers), (16384, 16384, 1));
        assert_eq!(step.bytes, GIB);
    }

    /// An adapter too narrow to hold a step in its layer budget stops the
    /// probe, capped, rather than asking for a texture it would refuse for
    /// shape and reading that as memory.
    #[test]
    fn a_step_no_texture_can_take_caps_the_probe() {
        let narrow = ProbePlan::for_adapter(2048, 8);
        assert_eq!(shape(64 * MIB, &narrow).map(|a| a.layers), Some(4));
        assert_eq!(shape(256 * MIB, &narrow), None, "16 layers against 8");
        let mut probe = Probe::new(narrow);
        for _ in 0..2 {
            let allocation = probe.next(0).unwrap();
            probe.record(allocation, StepResult::Held, 1);
        }
        assert_eq!(probe.next(2), None);
        let outcome = probe.finish(2);
        assert_eq!(outcome.last_ok_bytes, 192 * MIB);
        assert!(outcome.capped);
    }

    /// The plan's constants, spelled once here so a moved number is a moved
    /// test: 64 MiB, doubling, 8 GiB, two seconds, four bytes a texel.
    #[test]
    fn the_shipped_constants_are_what_the_design_names() {
        let plan = ProbePlan::for_adapter(8192, 256);
        assert_eq!(plan.start_bytes, 64 * MIB);
        assert_eq!(plan.factor, 2);
        assert_eq!(plan.max_bytes, 8 * GIB);
        assert_eq!(plan.time_budget_ms, 2000);
        assert_eq!(BYTES_PER_TEXEL, 4);
    }
}
