//! The WebGL2 arm of the capacity probe: allocate-and-check on a second
//! canvas, host-testable against a scripted context.
//!
//! WebGL2 has no error scope and no device-lost callback, so the WebGPU
//! probe's instrument (`super::run`) cannot run on a WebGL2 page — and the
//! wgpu path it goes through would not help if it could. In wgpu-hal 29.0.4's
//! gles backend `create_texture` issues `texStorage2D`/`texStorage3D` with no
//! `glGetError` behind it, `OutOfMemory` is mapped only onto name generation
//! (`create_buffer`, `create_query`, the swapchain's own texture), and
//! nothing observes a lost context; an out-of-memory scope on `Backend::Gl`
//! pops empty forever, and a probe built on it would walk to its cap and call
//! the result a figure. This arm therefore speaks raw WebGL2: `texStorage2D`,
//! a clear so the storage is written and not merely reserved, a fence behind
//! the clear so the GPU has really done it, then `getError()` and
//! `isContextLost()`.
//!
//! **`Probed` means the GPU refused.** The one signal that makes a figure is
//! `OUT_OF_MEMORY` from `getError()`: the browser's own bound, reached with
//! the context intact, and the figure is the total held before it. A walk
//! that saw no refusal has measured nothing, however far it went — silence
//! is absence of evidence, not evidence of capacity: a software rasterizer,
//! or a driver that hands back host memory rather than refusing, stays
//! silent past any real limit, and the probe cannot tell that case from a
//! genuine allowance above what it asked for. So every silent ending is no
//! figure ([`capacity_from`]) and the presumption stands, **unmeasured**; a
//! walk that reached its cap in silence says so, and up to what
//! ([`Ending::SilentToCap`]), so the readout can say "no limit found up to
//! 1 GiB" — which is the true statement. Whether a silent walk should ever
//! be promoted is for the hardware leg to decide from evidence.
//!
//! **The ladder is conservative here where it is not on WebGPU.** On WebGPU
//! a refused step is a caught error and the walk may run to 8 GiB. On WebGL2
//! exhaustion has a second face: a lost context — and a browser that loses
//! one context to exhaustion may lose **every** context in the tab, the app's
//! own included, which on this tree is a dead canvas nobody restores
//! (wgpu-hal 29.0.4's web surface never reports it and nothing listens for
//! `webglcontextlost`). A probe that measured capacity by killing the app
//! would have measured nothing the user can use. So the walk never sets out
//! to find the loss: it stops at a **policy cap** ([`policy_cap_for`]) chosen
//! so that holding it resident is no more than the application itself is
//! prepared to hold on that form factor, and beyond which the probe cannot
//! tell whether the next rung fails cleanly or takes the tab's contexts with
//! it. The cap bounds what the probe *asks for*; it is never a value it
//! *reports*. A loss that happens anyway is recorded as what ended the walk
//! ([`Ending::ContextLost`]) so a reader knows the figure was reached by
//! exhaustion; it is a failure the cap exists to avoid, never the signal the
//! probe walks toward.
//!
//! **A software renderer is not walked.** SwiftShader, llvmpipe and their
//! kin hand back host memory and answer a number describing no machine a
//! user has; the renderer string is read first ([`is_software_renderer`])
//! and such a context is released unwalked, recorded as the reason. The
//! string is kept on the outcome either way, so the report says which device
//! answered.
//!
//! **Scope: Linux Firefox, selected by backend and never by browser name.**
//! Measured 2026-09-04: Firefox 154 on Linux exposes no `navigator.gpu`
//! under any forced preference, on the software and the hardware arm alike,
//! so the app binds `Gl` there and this is the probe that runs; Firefox 155
//! on macOS exposes WebGPU and the app takes it (`BrowserWebGpu`, a hardware
//! adapter, no fallback), so there the WebGPU probe applies and this arm is
//! never selected. The choice is made on the backend the app's own adapter
//! answered with (`squallar_app::platform::gpu_probe_applies_to`, the web
//! bridge's dispatch), which is why a Firefox that gains WebGPU on a platform
//! retires this arm there without a line changing.
//!
//! This module is the walk and its judgement over a [`GlContext`] the host
//! can script; `super::webgl2_run` (wasm32 only) is the one implementation
//! that touches a browser.

use super::{Allocation, Probe, ProbeOutcome, ProbePlan, StepResult};
use squallar_device_profile::budget::FormFactor;
use std::future::Future;

/// The largest side a probe texture takes, whatever `MAX_TEXTURE_SIZE` says:
/// 8192 texels, so one texture is at most 256 MiB — an RGBA8 8192², the
/// largest texture the application itself creates — and a rung past that is
/// several textures rather than one large one. Browsers may hold a
/// per-texture ceiling apart from any memory limit (Firefox's is the
/// `webgl.max-size-per-texture-mib` preference, and a texture over it is
/// refused with `OUT_OF_MEMORY` whatever the card holds); a probe made of the
/// pieces the app already draws with cannot be refused for a size the app
/// never asks for and read that as exhaustion.
pub const MAX_TEXTURE_SIDE: u32 = 8192;

/// How many textures a rung may be split across: 256. At the 8192 side that
/// is 64 GiB, so the policy cap decides and not this; at a 1024 maximum
/// (4 MiB textures) the 1 GiB rung is exactly 256 textures and the next has
/// no shape, which stops the walk short rather than asking for a count no
/// driver was measured holding.
pub const MAX_TEXTURES_PER_STEP: u32 = 256;

/// The policy cap on a desktop form factor: 1 GiB. **A chosen bound, not a
/// measurement**, and the hardware leg is what validates it: the walk to it
/// is 64 + 128 + 256 + 512 + 64 MiB, five rungs the application's own
/// desktop brackets are prepared to hold, and a desktop-class GPU or a
/// shared-memory iGPU on a machine that runs a browser holds a gibibyte of
/// textures without paging into oblivion. What lies above it — where a
/// capable card might be refused cleanly, or might not — is exactly where a
/// Firefox on native GL oversubscribes silently until something dies, so the
/// probe does not go there. A bound on what is asked, never a figure.
pub const POLICY_CAP_DESKTOP_BYTES: u64 = 1 << 30;

/// The policy cap on a handheld, or where the form factor is unknown: the
/// wasm bracket's own texture presumption, 288 MiB — what the application
/// already plans to hold on that page — so the walk adds no risk the app is
/// not already taking. The last rung is clamped to reach the cap but a rung
/// is exact texture bytes, so the total held may overshoot it by under one
/// rung.
pub const POLICY_CAP_HANDHELD_BYTES: u64 =
    squallar_device_profile::constants::WASM_APP_TEXTURE_BUDGET_BYTES as u64;

// Both caps sit under the WebGPU probe's ceiling, or they are not
// conservative; and the handheld one is the smaller, or the unknown form
// factor would not be taking the safe bound.
const _: () = {
    assert!(POLICY_CAP_DESKTOP_BYTES < super::MAX_BYTES);
    assert!(POLICY_CAP_HANDHELD_BYTES < POLICY_CAP_DESKTOP_BYTES);
};

/// The policy cap for a page's form factor: [`POLICY_CAP_DESKTOP_BYTES`] on
/// a desktop, [`POLICY_CAP_HANDHELD_BYTES`] on a handheld and where nothing
/// classified the page — the smaller bound is the safe one.
pub fn policy_cap_for(form_factor: Option<FormFactor>) -> u64 {
    match form_factor {
        Some(FormFactor::Desktop) => POLICY_CAP_DESKTOP_BYTES,
        Some(FormFactor::Handheld) | None => POLICY_CAP_HANDHELD_BYTES,
    }
}

/// `gl.NO_ERROR`.
pub const GL_NO_ERROR: u32 = 0;
/// `gl.OUT_OF_MEMORY`.
pub const GL_OUT_OF_MEMORY: u32 = 0x0505;
/// `gl.CONTEXT_LOST_WEBGL`, the one code `getError()` answers on a lost
/// context — once; `NO_ERROR` after.
pub const GL_CONTEXT_LOST_WEBGL: u32 = 0x9242;

/// Whether a renderer string names a software rasterizer: Chromium's
/// SwiftShader (under ANGLE), Mesa's llvmpipe, softpipe and lavapipe,
/// Windows' WARP ("Microsoft Basic Render Driver"), and anything that calls
/// itself a software renderer. Case-insensitive substrings, because every
/// browser wraps the name in its own prose (`ANGLE (Google, Vulkan 1.3.0
/// (SwiftShader Device (Subzero)...), SwiftShader driver)`).
pub fn is_software_renderer(renderer: &str) -> bool {
    let renderer = renderer.to_ascii_lowercase();
    [
        "swiftshader",
        "llvmpipe",
        "softpipe",
        "lavapipe",
        "basic render driver",
        "software",
    ]
    .iter()
    .any(|token| renderer.contains(token))
}

/// The plan for a context reporting `max_texture_size`, walking to
/// `policy_cap_bytes`: the WebGPU probe's start, factor and time budget, the
/// side held under [`MAX_TEXTURE_SIDE`], the layer count under
/// [`MAX_TEXTURES_PER_STEP`], and the policy cap in place of the 8 GiB
/// ceiling. Each `Allocation::layers` is one texture here, not one layer of
/// a 2D array.
pub fn plan_for(max_texture_size: u32, policy_cap_bytes: u64) -> ProbePlan {
    ProbePlan {
        max_bytes: policy_cap_bytes,
        ..ProbePlan::for_adapter(
            max_texture_size.min(MAX_TEXTURE_SIDE),
            MAX_TEXTURES_PER_STEP,
        )
    }
}

/// What `getError()` amounted to, reduced from everything it had queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlError {
    /// `NO_ERROR` alone.
    None,
    /// `OUT_OF_MEMORY` was among the codes.
    OutOfMemory,
    /// `CONTEXT_LOST_WEBGL` was among the codes.
    ContextLost,
    /// Something else, and nothing above it: the first such code.
    Other(u32),
}

/// Reduce the codes `getError()` queued to one verdict, worst first: a lost
/// context outranks everything, since every other code on a lost context is
/// noise; out-of-memory outranks the rest, since a texture whose storage was
/// refused goes on to fail its clear with `INVALID_FRAMEBUFFER_OPERATION`
/// and that is a consequence, not a second finding.
pub fn reduce_errors(codes: impl IntoIterator<Item = u32>) -> GlError {
    let mut out_of_memory = false;
    let mut other = None;
    for code in codes {
        match code {
            GL_CONTEXT_LOST_WEBGL => return GlError::ContextLost,
            GL_OUT_OF_MEMORY => out_of_memory = true,
            GL_NO_ERROR => {}
            code => other = other.or(Some(code)),
        }
    }
    if out_of_memory {
        GlError::OutOfMemory
    } else {
        other.map_or(GlError::None, GlError::Other)
    }
}

/// Whether the fence behind a rung has signalled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fence {
    /// The GPU has not finished the rung's clears.
    Pending,
    /// It has: the storage was written, so it is resident.
    Signalled,
}

/// The GL a walk touches, narrowed to what it needs so the host can script
/// one. One implementation touches a browser (`super::webgl2_run`); the tests
/// below drive a fake through every ending.
pub trait GlContext {
    /// The renderer string — `UNMASKED_RENDERER_WEBGL` where the browser
    /// exposes it, else `RENDERER` — or `None` when neither answered.
    fn renderer(&self) -> Option<String>;

    /// `MAX_TEXTURE_SIZE`, or `None` when the query itself failed — a context
    /// that will not say has nothing to be probed with.
    fn max_texture_size(&self) -> Option<u32>;

    /// Create `allocation.layers` textures of `allocation.width` by
    /// `allocation.height` RGBA8 with `texStorage2D`, attach and clear each,
    /// then flush and put a fence behind them. `false` when a texture handle
    /// could not be made at all; errors and loss are read separately.
    fn allocate_and_clear(&mut self, allocation: &Allocation) -> bool;

    /// The state of the fence behind the last rung.
    fn fence(&mut self) -> Fence;

    /// Drain `getError()` and reduce what it queued ([`reduce_errors`]).
    fn take_error(&mut self) -> GlError;

    /// `isContextLost()`.
    fn is_context_lost(&self) -> bool;

    /// Release everything the probe holds — textures, fence, framebuffer, the
    /// context itself — so the memory goes back now and not when the
    /// collector finds the canvas.
    fn release(&mut self);
}

/// What ended a walk, beside the [`ProbeOutcome`] arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// `getError()` answered `OUT_OF_MEMORY`: the browser's own bound,
    /// reached with the context intact. The figure is the total before it.
    Refused,
    /// The context was lost during a step — the failure the cap exists to
    /// avoid, recorded so a reader knows the figure was reached by
    /// exhaustion. The figure, where earlier rungs held, is the total before
    /// it; a loss on the first rung is no figure.
    ContextLost,
    /// The walk reached the policy cap with every rung held and nothing
    /// refused. Nothing was measured: silence is absence of evidence, and
    /// the presumption stands, unmeasured — with the cap on record so the
    /// readout can say up to what no limit was found.
    SilentToCap,
    /// The walk ended short of the cap with no refusal — the time budget, a
    /// shape no texture takes, a fence still pending when the budget ran
    /// out. Silence, and no figure.
    Silent,
    /// The probe's own fault, or a limit it does not understand — never a
    /// capacity. No figure.
    Faulted(Fault),
    /// No WebGL2 context could be made on the second canvas, or it would
    /// not say its texture size. No figure.
    NoContext,
    /// The renderer string named a software rasterizer, and the ladder was
    /// not walked: a figure from host memory describes no machine a user
    /// has. No figure.
    SoftwareRenderer,
}

/// Why a walk faulted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `getError()` answered a code that is neither out-of-memory nor loss.
    GlError(u32),
    /// `createTexture()` answered null with the context intact and nothing
    /// queued, which no driver is measured doing.
    NullHandle,
}

/// What the WebGL2 probe found: the walk's arithmetic, what ended it, the
/// cap it was given and the device that answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Webgl2Outcome {
    /// The rungs held and refused, as the WebGPU probe counts them.
    pub probe: ProbeOutcome,
    /// What ended the walk.
    pub ending: Ending,
    /// The policy cap the walk was bounded by, in bytes.
    pub policy_cap_bytes: u64,
    /// The renderer string the context reported, if it did — kept whatever
    /// the ending, so the report says which device answered.
    pub renderer: Option<String>,
}

impl Webgl2Outcome {
    /// A probe that never asked: no context to ask.
    pub fn no_context(policy_cap_bytes: u64) -> Self {
        Self {
            probe: ProbeOutcome::default(),
            ending: Ending::NoContext,
            policy_cap_bytes,
            renderer: None,
        }
    }
}

/// The figure an outcome amounts to — the total held before a refusal or a
/// loss — or `None` for everything else: nothing held before the refusal,
/// no context, a software renderer, a fault, and **every silent ending**,
/// the cap reached included. That last arm is the difference from the WebGPU
/// probe's reading of the same arithmetic, where a capped walk is a floor:
/// here `Probed` means the GPU refused, and silence is not a figure.
pub fn capacity_from(outcome: &Webgl2Outcome) -> Option<u64> {
    match outcome.ending {
        Ending::Refused | Ending::ContextLost => {
            (outcome.probe.last_ok_bytes > 0).then_some(outcome.probe.last_ok_bytes)
        }
        Ending::SilentToCap
        | Ending::Silent
        | Ending::Faulted(_)
        | Ending::NoContext
        | Ending::SoftwareRenderer => None,
    }
}

/// What one step's two readings amount to. The lost flag outranks the error
/// queue: a lost context answers `getError()` with `CONTEXT_LOST_WEBGL` once
/// and `NO_ERROR` after, so by the time the probe reads it the queue may be
/// empty. `Err` is a fault: a code that is neither of the two signals.
pub fn judge(lost: bool, error: GlError) -> Result<StepResult, u32> {
    if lost {
        return Ok(StepResult::Lost);
    }
    match error {
        GlError::None => Ok(StepResult::Held),
        GlError::OutOfMemory => Ok(StepResult::Refused),
        GlError::ContextLost => Ok(StepResult::Lost),
        GlError::Other(code) => Err(code),
    }
}

/// How one rung ended, before the probe's arithmetic hears of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Held,
    Refused,
    Lost,
    Faulted(Fault),
    /// The fence was still pending when the time budget ran out.
    OutOfTime,
}

/// Walk the ladder on `gl` to `policy_cap_bytes`: read the renderer and
/// stop if it is software; otherwise ask [`Probe`] what comes next,
/// allocate and clear it, wait for the fence — yielding to the browser
/// through `yield_now` between polls so the page keeps its frames — and
/// judge the step. `now_ms` is a wall clock in milliseconds. Whatever ends
/// the walk, `gl` is released before the outcome is returned.
pub async fn walk<G, Y, F>(
    gl: &mut G,
    policy_cap_bytes: u64,
    now_ms: impl Fn() -> u64,
    yield_now: Y,
) -> Webgl2Outcome
where
    G: GlContext,
    Y: Fn() -> F,
    F: Future<Output = ()>,
{
    let renderer = gl.renderer();
    if renderer.as_deref().is_some_and(is_software_renderer) {
        gl.release();
        return Webgl2Outcome {
            probe: ProbeOutcome::default(),
            ending: Ending::SoftwareRenderer,
            policy_cap_bytes,
            renderer,
        };
    }
    let Some(max_texture_size) = gl.max_texture_size() else {
        gl.release();
        return Webgl2Outcome {
            renderer,
            ..Webgl2Outcome::no_context(policy_cap_bytes)
        };
    };
    let plan = plan_for(max_texture_size, policy_cap_bytes);
    let mut probe = Probe::new(plan);
    let started = now_ms();
    let mut ending = None;
    while let Some(allocation) = probe.next(now_ms().saturating_sub(started)) {
        let step_started = now_ms();
        let out_of_time = || now_ms().saturating_sub(started) > plan.time_budget_ms;
        let result = step(gl, &allocation, out_of_time, &yield_now).await;
        let step_ms = now_ms().saturating_sub(step_started);
        match result {
            Step::Held => probe.record(allocation, StepResult::Held, step_ms),
            Step::Refused => {
                probe.record(allocation, StepResult::Refused, step_ms);
                ending = Some(Ending::Refused);
            }
            Step::Lost => {
                probe.record(allocation, StepResult::Lost, step_ms);
                ending = Some(Ending::ContextLost);
            }
            Step::Faulted(fault) => {
                probe.cap();
                ending = Some(Ending::Faulted(fault));
            }
            Step::OutOfTime => {
                probe.cap();
                ending = Some(Ending::Silent);
            }
        }
    }
    gl.release();
    // Nothing refused and nothing faulted: the probe's own bound ended the
    // walk, and which bound is worth recording — the cap it set out for, or
    // the time budget and shapes short of it. Neither is a figure.
    let ending = match ending {
        Some(ending) => ending,
        None if probe.held_bytes() >= plan.max_bytes => Ending::SilentToCap,
        None => Ending::Silent,
    };
    Webgl2Outcome {
        probe: probe.finish(now_ms().saturating_sub(started)),
        ending,
        policy_cap_bytes,
        renderer,
    }
}

/// One rung: allocate and clear, read the errors at once — an
/// `OUT_OF_MEMORY` the browser raises at `texStorage2D` ends the step here,
/// before the GPU is asked to write a byte of it — then wait for the fence
/// and read again.
async fn step<G, Y, F>(
    gl: &mut G,
    allocation: &Allocation,
    out_of_time: impl Fn() -> bool,
    yield_now: &Y,
) -> Step
where
    G: GlContext,
    Y: Fn() -> F,
    F: Future<Output = ()>,
{
    let created = gl.allocate_and_clear(allocation);
    match judge(gl.is_context_lost(), gl.take_error()) {
        Ok(StepResult::Held) if created => {}
        Ok(StepResult::Held) => return Step::Faulted(Fault::NullHandle),
        Ok(StepResult::Refused) => return Step::Refused,
        Ok(StepResult::Lost) => return Step::Lost,
        Err(code) => return Step::Faulted(Fault::GlError(code)),
    }
    while gl.fence() == Fence::Pending {
        if gl.is_context_lost() {
            return Step::Lost;
        }
        if out_of_time() {
            return Step::OutOfTime;
        }
        yield_now().await;
    }
    match judge(gl.is_context_lost(), gl.take_error()) {
        Ok(StepResult::Held) => Step::Held,
        Ok(StepResult::Refused) => Step::Refused,
        Ok(StepResult::Lost) => Step::Lost,
        Err(code) => Step::Faulted(Fault::GlError(code)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    /// `gl.INVALID_VALUE`.
    const GL_INVALID_VALUE: u32 = 0x0501;
    /// `gl.INVALID_FRAMEBUFFER_OPERATION`, what a clear onto a texture whose
    /// storage was refused goes on to raise.
    const GL_INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;
    /// A hardware renderer string, as Chromium under ANGLE spells one.
    const HARDWARE: &str =
        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3070 Direct3D11 vs_5_0 ps_5_0, D3D11)";
    /// Chromium's software fallback, as the rig's headless leg reports it.
    const SWIFTSHADER: &str = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) (0x0000C0DE)), SwiftShader driver)";
    /// Firefox on Xvfb, as the rig's headless leg reports it.
    const LLVMPIPE: &str = "llvmpipe (LLVM 17.0.6, 256 bits)";

    /// What the fake does with one rung.
    #[derive(Clone, Copy, Debug)]
    enum Rung {
        /// Storage granted, clear done, fence signals after one poll.
        Hold,
        /// `OUT_OF_MEMORY` queued at `texStorage2D`, with the clear's
        /// consequence behind it.
        RefuseAtIssue,
        /// Nothing queued at issue; `OUT_OF_MEMORY` queued once the fence
        /// signals — a driver that only knows it is out when it writes.
        RefuseAfterFence,
        /// The context is lost at issue: `CONTEXT_LOST_WEBGL` queued once,
        /// `isContextLost()` true after.
        LoseAtIssue,
        /// The context is lost while the fence is pending, with nothing
        /// queued — the flag alone says so.
        LoseWhilePending,
        /// `createTexture()` answers null with the context intact.
        NullHandle,
        /// Some other code queued at issue.
        Fault(u32),
        /// The fence never signals.
        HangFence,
    }

    /// A WebGL2 context the test scripts rung by rung. Rungs past the end
    /// of the script hold.
    struct FakeGl {
        renderer: Option<String>,
        max_texture_size: Option<u32>,
        script: VecDeque<Rung>,
        /// Textures created and not yet deleted.
        textures: u32,
        released: bool,
        errors: Vec<u32>,
        lost: bool,
        pending_polls: u32,
        after_fence: Option<Rung>,
        hung: bool,
        asked: Vec<Allocation>,
    }

    impl FakeGl {
        fn scripted(rungs: &[Rung]) -> Self {
            Self {
                renderer: Some(HARDWARE.to_string()),
                max_texture_size: Some(8192),
                script: rungs.iter().copied().collect(),
                textures: 0,
                released: false,
                errors: Vec::new(),
                lost: false,
                pending_polls: 0,
                after_fence: None,
                hung: false,
                asked: Vec::new(),
            }
        }

        fn held_total(&self) -> u64 {
            self.asked.iter().map(|a| a.bytes).sum()
        }
    }

    impl GlContext for FakeGl {
        fn renderer(&self) -> Option<String> {
            self.renderer.clone()
        }

        fn max_texture_size(&self) -> Option<u32> {
            self.max_texture_size
        }

        fn allocate_and_clear(&mut self, allocation: &Allocation) -> bool {
            self.asked.push(*allocation);
            let rung = self.script.pop_front().unwrap_or(Rung::Hold);
            self.pending_polls = 1;
            self.after_fence = None;
            match rung {
                Rung::Hold => {}
                Rung::RefuseAtIssue => {
                    self.errors
                        .extend([GL_OUT_OF_MEMORY, GL_INVALID_FRAMEBUFFER_OPERATION]);
                }
                Rung::RefuseAfterFence | Rung::LoseWhilePending => {
                    self.after_fence = Some(rung);
                }
                Rung::LoseAtIssue => {
                    self.lost = true;
                    self.errors.push(GL_CONTEXT_LOST_WEBGL);
                }
                Rung::NullHandle => return false,
                Rung::Fault(code) => self.errors.push(code),
                Rung::HangFence => self.hung = true,
            }
            self.textures += allocation.layers;
            true
        }

        fn fence(&mut self) -> Fence {
            if self.hung {
                return Fence::Pending;
            }
            if self.pending_polls > 0 {
                self.pending_polls -= 1;
                // What happens to the context while the GPU works on the rung.
                if matches!(self.after_fence, Some(Rung::LoseWhilePending)) {
                    self.after_fence = None;
                    self.lost = true;
                }
                return Fence::Pending;
            }
            if matches!(self.after_fence, Some(Rung::RefuseAfterFence)) {
                self.after_fence = None;
                self.errors.push(GL_OUT_OF_MEMORY);
            }
            Fence::Signalled
        }

        fn take_error(&mut self) -> GlError {
            reduce_errors(self.errors.drain(..))
        }

        fn is_context_lost(&self) -> bool {
            self.lost
        }

        fn release(&mut self) {
            self.textures = 0;
            self.released = true;
        }
    }

    /// Drive a future to completion on the host. No executor crate: the
    /// fake's yield is ready at once, so the walk never returns `Pending`
    /// and a noop waker is all the context it needs — and this module's
    /// tests are compiled for wasm32 too under `--all-targets`, where a
    /// host-only dev-dependency would not resolve.
    fn block_on<F: Future>(future: F) -> F::Output {
        use std::pin::pin;
        use std::task::{Context, Poll, Waker};

        let mut future = pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
        }
    }

    /// Run the walk to the end on the host under `cap`: a clock that
    /// advances one millisecond per read, and a yield that is ready at once.
    fn run_to(gl: &mut FakeGl, cap: u64) -> Webgl2Outcome {
        let clock = Cell::new(0u64);
        let now_ms = || {
            let now = clock.get();
            clock.set(now + 1);
            now
        };
        block_on(walk(gl, cap, now_ms, || std::future::ready(())))
    }

    /// The desktop cap, the one with room for a refusal to land under it.
    fn run(gl: &mut FakeGl) -> Webgl2Outcome {
        run_to(gl, POLICY_CAP_DESKTOP_BYTES)
    }

    /// Held, held, held, refused at `texStorage2D`: the figure is the total
    /// before the refusal, the refused total is recorded, the walk stops
    /// there — before any fence is waited on — and every texture is released.
    #[test]
    fn out_of_memory_at_a_rung_is_the_figure_at_the_total_before_it() {
        let mut gl = FakeGl::scripted(&[Rung::Hold, Rung::Hold, Rung::Hold, Rung::RefuseAtIssue]);
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::Refused);
        assert_eq!(capacity_from(&outcome), Some((64 + 128 + 256) * MIB));
        assert_eq!(outcome.probe.failed_at, Some((64 + 128 + 256 + 512) * MIB));
        assert_eq!(outcome.probe.steps, 4);
        assert!(
            !outcome.probe.capped,
            "a refusal is the browser's bound, not the probe's"
        );
        assert_eq!(gl.asked.len(), 4, "nothing is asked for after a refusal");
        assert_eq!(outcome.renderer.as_deref(), Some(HARDWARE));
        assert!(gl.released);
        assert_eq!(gl.textures, 0);
    }

    /// A driver that only knows it is out of memory once it writes: the
    /// refusal shows after the fence, and is still the answer.
    #[test]
    fn a_refusal_that_only_shows_after_the_fence_is_still_the_answer() {
        let mut gl = FakeGl::scripted(&[Rung::Hold, Rung::RefuseAfterFence]);
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::Refused);
        assert_eq!(capacity_from(&outcome), Some(64 * MIB));
        assert_eq!(outcome.probe.failed_at, Some(192 * MIB));
    }

    /// **The test that tells this probe from the WebGPU-on-GL one.** Every
    /// rung held and `getError()` silent: the walk never asks past the policy
    /// cap — the last ask is clamped to land on it — and the arithmetic reads
    /// as the WebGPU probe's would, 1 GiB capped, which that probe would
    /// report as a floor. Here it is no figure: the ending says the cap was
    /// reached in silence, the cap is on record for the readout, and
    /// `Probed` is left to mean the GPU refused. A probe that answered `Some`
    /// on this walk would hand a software renderer's host memory to the
    /// budget as a measurement.
    #[test]
    fn a_silent_walk_to_the_policy_cap_is_not_a_figure() {
        let mut gl = FakeGl::scripted(&[]);
        let outcome = run(&mut gl);
        let sizes: Vec<u64> = gl.asked.iter().map(|a| a.bytes / MIB).collect();
        assert_eq!(sizes, [64, 128, 256, 512, 64]);
        assert_eq!(gl.held_total(), POLICY_CAP_DESKTOP_BYTES);
        assert_eq!(outcome.ending, Ending::SilentToCap);
        assert_eq!(outcome.policy_cap_bytes, POLICY_CAP_DESKTOP_BYTES);
        assert_eq!(outcome.probe.last_ok_bytes, GIB);
        assert_eq!(outcome.probe.failed_at, None);
        assert!(outcome.probe.capped);
        assert_eq!(
            super::super::capacity_from(&outcome.probe),
            Some(GIB),
            "the WebGPU reading of this walk is a capped floor — that is the arm this test guards"
        );
        assert_eq!(
            capacity_from(&outcome),
            None,
            "a WebGL2 walk that was never refused manufactured a figure from silence"
        );
        assert!(gl.released);
    }

    /// The handheld cap is the wasm presumption: 64 + 128 hold, the third
    /// ask is clamped toward the cap and overshoots it by under one rung
    /// because a rung is exact texture bytes, nothing is asked once the cap
    /// is reached, and — silent — it is no figure.
    #[test]
    fn the_handheld_cap_bounds_what_is_asked_and_a_silent_walk_to_it_is_no_figure() {
        let mut gl = FakeGl::scripted(&[]);
        let outcome = run_to(&mut gl, POLICY_CAP_HANDHELD_BYTES);
        let sizes: Vec<u64> = gl.asked.iter().map(|a| a.bytes / MIB).collect();
        assert_eq!(sizes, [64, 128, 128]);
        assert_eq!(outcome.ending, Ending::SilentToCap);
        assert_eq!(outcome.probe.last_ok_bytes, 320 * MIB);
        assert!(outcome.probe.last_ok_bytes >= POLICY_CAP_HANDHELD_BYTES);
        assert!(outcome.probe.last_ok_bytes < POLICY_CAP_HANDHELD_BYTES + 256 * MIB);
        assert_eq!(capacity_from(&outcome), None);
    }

    /// The policy caps are what the doc names, and the smaller one is the
    /// answer wherever the page's form factor is unknown.
    #[test]
    fn the_policy_caps_are_pinned_and_the_unknown_form_factor_takes_the_smaller() {
        assert_eq!(POLICY_CAP_DESKTOP_BYTES, GIB);
        assert_eq!(POLICY_CAP_HANDHELD_BYTES, 288 * MIB);
        assert_eq!(policy_cap_for(Some(FormFactor::Desktop)), GIB);
        assert_eq!(policy_cap_for(Some(FormFactor::Handheld)), 288 * MIB);
        assert_eq!(policy_cap_for(None), 288 * MIB);
    }

    /// A software renderer is not walked: nothing is asked, the ending says
    /// why, the string is on record, and the context is released.
    #[test]
    fn a_software_renderer_is_released_unwalked_and_is_no_figure() {
        for software in [SWIFTSHADER, LLVMPIPE, "Microsoft Basic Render Driver"] {
            let mut gl = FakeGl::scripted(&[]);
            gl.renderer = Some(software.to_string());
            let outcome = run(&mut gl);
            assert!(gl.asked.is_empty(), "{software:?} was walked");
            assert_eq!(outcome.ending, Ending::SoftwareRenderer, "{software:?}");
            assert_eq!(capacity_from(&outcome), None);
            assert_eq!(outcome.probe.steps, 0);
            assert_eq!(outcome.renderer.as_deref(), Some(software));
            assert!(gl.released);
        }
        // An unknown renderer is not evidence of software, and is walked.
        let mut gl = FakeGl::scripted(&[Rung::RefuseAtIssue]);
        gl.renderer = None;
        let outcome = run(&mut gl);
        assert_eq!(gl.asked.len(), 1);
        assert_eq!(outcome.ending, Ending::Refused);
        assert_eq!(outcome.renderer, None);
    }

    /// The software table, both arms: every rasterizer the rig has met, and
    /// none of the hardware strings the browsers spell.
    #[test]
    fn the_software_renderer_table_names_rasterizers_and_no_hardware() {
        for software in [
            SWIFTSHADER,
            LLVMPIPE,
            "Mesa/X.org, llvmpipe (LLVM 15.0.7, 256 bits)",
            "softpipe",
            "ANGLE (Mesa, llvmpipe (LLVM 17.0.6, 256 bits), OpenGL 4.5)",
            "lavapipe",
            "ANGLE (Microsoft, Microsoft Basic Render Driver Direct3D11 vs_5_0 ps_5_0, D3D11)",
            "Apple Software Renderer",
        ] {
            assert!(is_software_renderer(software), "{software:?}");
        }
        for hardware in [
            HARDWARE,
            "NVIDIA GeForce RTX 3070/PCIe/SSE2",
            "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)",
            "Apple M2",
            "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)",
            "Adreno (TM) 740",
            "Mali-G715-Immortalis MC11",
            "AMD Radeon RX 7900 XTX (radeonsi, navi31, LLVM 17.0.6, DRM 3.54, 6.6.0)",
        ] {
            assert!(!is_software_renderer(hardware), "{hardware:?}");
        }
    }

    /// A lost context is recorded as what ended the walk: the figure is
    /// what was held before it, the ending says it was a loss, and
    /// `getError()` need not have said anything — the flag alone is read.
    #[test]
    fn a_context_lost_at_a_rung_reports_the_total_before_it_as_lost() {
        let mut gl = FakeGl::scripted(&[Rung::Hold, Rung::Hold, Rung::LoseWhilePending]);
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::ContextLost);
        assert_eq!(capacity_from(&outcome), Some(192 * MIB));
        assert_eq!(outcome.probe.failed_at, Some(448 * MIB));
        assert_eq!(outcome.probe.steps, 3);
        assert!(!outcome.probe.capped);
        assert!(gl.released, "a lost context is still released");

        let mut gl = FakeGl::scripted(&[Rung::Hold, Rung::LoseAtIssue]);
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::ContextLost);
        assert_eq!(capacity_from(&outcome), Some(64 * MIB));
    }

    /// A loss on the first rung held nothing: no figure, and the ending
    /// still says why, so the log can say "context lost, no figure".
    #[test]
    fn a_context_lost_on_the_first_rung_is_no_figure() {
        let mut gl = FakeGl::scripted(&[Rung::LoseAtIssue]);
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::ContextLost);
        assert_eq!(capacity_from(&outcome), None);
        assert_eq!(outcome.probe.last_ok_bytes, 0);
        assert_eq!(outcome.probe.failed_at, Some(64 * MIB));
    }

    /// No WebGL2 context, or one that will not say its texture size: no
    /// figure, no steps, the ending names it, and the renderer — read
    /// first — is still on record.
    #[test]
    fn no_webgl2_context_is_no_figure() {
        let mut gl = FakeGl::scripted(&[]);
        gl.max_texture_size = None;
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::NoContext);
        assert_eq!(capacity_from(&outcome), None);
        assert_eq!(outcome.probe, ProbeOutcome::default());
        assert_eq!(outcome.renderer.as_deref(), Some(HARDWARE));
        assert!(gl.released);
        assert_eq!(Webgl2Outcome::no_context(GIB).ending, Ending::NoContext);
    }

    /// Silence short of the cap is not a figure either: a fence that never
    /// signals runs the budget out, the walk stops capped with the cap
    /// unreached, and the ending is plain silence rather than the cap.
    #[test]
    fn silence_short_of_the_cap_is_not_a_figure() {
        let mut gl = FakeGl::scripted(&[Rung::Hold, Rung::HangFence]);
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::Silent);
        assert!(outcome.probe.capped);
        assert_eq!(outcome.probe.last_ok_bytes, 64 * MIB);
        assert_eq!(outcome.probe.steps, 1, "the hung rung was never judged");
        assert_eq!(
            super::super::capacity_from(&outcome.probe),
            Some(64 * MIB),
            "the WebGPU reading of this walk would be a floor"
        );
        assert_eq!(capacity_from(&outcome), None);
        assert!(gl.released);
    }

    /// A GL error that is neither out-of-memory nor loss is the probe's own
    /// fault — a wrong argument, a limit it does not know — and never a
    /// capacity.
    #[test]
    fn a_gl_error_that_is_not_out_of_memory_faults_the_probe_without_a_figure() {
        let mut gl = FakeGl::scripted(&[Rung::Hold, Rung::Fault(GL_INVALID_VALUE)]);
        let outcome = run(&mut gl);
        assert_eq!(
            outcome.ending,
            Ending::Faulted(Fault::GlError(GL_INVALID_VALUE))
        );
        assert_eq!(capacity_from(&outcome), None);
        assert!(outcome.probe.capped);
        assert_eq!(outcome.probe.last_ok_bytes, 64 * MIB);
        assert_eq!(outcome.probe.failed_at, None, "a fault is not a refusal");
    }

    /// A null handle with the context intact is a fault, not a refusal:
    /// nothing was asked of memory.
    #[test]
    fn a_null_texture_handle_with_the_context_intact_is_a_fault() {
        let mut gl = FakeGl::scripted(&[Rung::NullHandle]);
        let outcome = run(&mut gl);
        assert_eq!(outcome.ending, Ending::Faulted(Fault::NullHandle));
        assert_eq!(capacity_from(&outcome), None);
    }

    /// Whatever ends the walk, the context is released and holds nothing.
    #[test]
    fn the_context_is_released_however_the_walk_ends() {
        for script in [
            vec![],
            vec![Rung::RefuseAtIssue],
            vec![Rung::Hold, Rung::LoseAtIssue],
            vec![Rung::Hold, Rung::HangFence],
            vec![Rung::Fault(GL_INVALID_VALUE)],
            vec![Rung::NullHandle],
        ] {
            let mut gl = FakeGl::scripted(&script);
            let _ = run(&mut gl);
            assert!(gl.released, "{script:?} left the context unreleased");
            assert_eq!(gl.textures, 0, "{script:?} left textures held");
        }
        let mut gl = FakeGl::scripted(&[]);
        gl.renderer = Some(LLVMPIPE.to_string());
        let _ = run(&mut gl);
        assert!(gl.released, "a software context was left unreleased");
    }

    /// Every rung is made of textures no larger than the app's own: at a
    /// 16384 maximum the side is still 8192, and the bytes are exact. Run
    /// under the WebGPU ceiling here, not a policy cap, so every shape the
    /// ladder can take is exercised.
    #[test]
    fn every_rung_is_split_into_textures_no_larger_than_the_apps_own() {
        let mut gl = FakeGl::scripted(&[]);
        gl.max_texture_size = Some(16384);
        let _ = run_to(&mut gl, super::super::MAX_BYTES);
        let shapes: Vec<(u32, u32, u32)> = gl
            .asked
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
        for allocation in &gl.asked {
            let texture_bytes = u64::from(allocation.width)
                * u64::from(allocation.height)
                * super::super::BYTES_PER_TEXEL;
            assert!(texture_bytes <= 256 * MIB, "{allocation:?}");
            assert_eq!(
                allocation.bytes,
                texture_bytes * u64::from(allocation.layers)
            );
        }
    }

    /// A narrow context (2048 maximum, 16 MiB textures) splits the 1 GiB cap
    /// into rungs of up to 32 textures and still lands on it; and a narrower
    /// one (1024 maximum, 4 MiB textures) under a cap the texture-count bound
    /// cannot reach — the 1 GiB rung is exactly 256 textures, the 2 GiB rung
    /// has no shape — stops the walk short in plain silence.
    #[test]
    fn a_narrow_context_splits_rungs_finer_and_a_shape_it_cannot_take_is_silence() {
        let mut gl = FakeGl::scripted(&[]);
        gl.max_texture_size = Some(2048);
        let outcome = run(&mut gl);
        assert_eq!(gl.asked.iter().map(|a| a.layers).max(), Some(32));
        assert_eq!(outcome.ending, Ending::SilentToCap);
        assert_eq!(capacity_from(&outcome), None);

        let mut gl = FakeGl::scripted(&[]);
        gl.max_texture_size = Some(1024);
        let outcome = run_to(&mut gl, super::super::MAX_BYTES);
        assert_eq!(
            gl.asked.last().map(|a| a.layers),
            Some(MAX_TEXTURES_PER_STEP),
            "the last rung asked is the one at the texture-count bound"
        );
        assert_eq!(
            outcome.probe.last_ok_bytes,
            (64 + 128 + 256 + 512 + 1024) * MIB
        );
        assert!(outcome.probe.capped);
        assert_eq!(outcome.ending, Ending::Silent);
        assert_eq!(capacity_from(&outcome), None);
    }

    /// The reduction ranks loss over refusal over anything else, and reads
    /// `NO_ERROR` as nothing.
    #[test]
    fn reduce_errors_ranks_loss_over_refusal_over_anything_else() {
        assert_eq!(reduce_errors([]), GlError::None);
        assert_eq!(reduce_errors([GL_NO_ERROR]), GlError::None);
        assert_eq!(
            reduce_errors([GL_INVALID_FRAMEBUFFER_OPERATION, GL_OUT_OF_MEMORY]),
            GlError::OutOfMemory
        );
        assert_eq!(
            reduce_errors([GL_OUT_OF_MEMORY, GL_CONTEXT_LOST_WEBGL]),
            GlError::ContextLost
        );
        assert_eq!(
            reduce_errors([GL_INVALID_VALUE, GL_INVALID_FRAMEBUFFER_OPERATION]),
            GlError::Other(GL_INVALID_VALUE)
        );
    }

    /// The lost flag outranks an empty queue and a refusal alike.
    #[test]
    fn the_lost_flag_outranks_whatever_the_error_queue_says() {
        assert_eq!(judge(true, GlError::None), Ok(StepResult::Lost));
        assert_eq!(judge(true, GlError::OutOfMemory), Ok(StepResult::Lost));
        assert_eq!(judge(false, GlError::None), Ok(StepResult::Held));
        assert_eq!(judge(false, GlError::OutOfMemory), Ok(StepResult::Refused));
        assert_eq!(judge(false, GlError::ContextLost), Ok(StepResult::Lost));
        assert_eq!(
            judge(false, GlError::Other(GL_INVALID_VALUE)),
            Err(GL_INVALID_VALUE)
        );
    }

    /// The plan is the WebGPU probe's, held under the two caps, with the
    /// policy cap in place of the 8 GiB ceiling.
    #[test]
    fn the_plan_holds_the_side_and_the_texture_count_under_their_caps() {
        let wide = plan_for(32767, POLICY_CAP_DESKTOP_BYTES);
        assert_eq!(wide.max_texture_dimension_2d, MAX_TEXTURE_SIDE);
        assert_eq!(wide.max_texture_array_layers, MAX_TEXTURES_PER_STEP);
        assert_eq!(wide.start_bytes, 64 * MIB);
        assert_eq!(wide.max_bytes, GIB);
        assert_eq!(wide.time_budget_ms, super::super::TIME_BUDGET_MS);
        let narrow = plan_for(4096, POLICY_CAP_HANDHELD_BYTES);
        assert_eq!(narrow.max_texture_dimension_2d, 4096);
        assert_eq!(narrow.max_bytes, 288 * MIB);
    }
}
