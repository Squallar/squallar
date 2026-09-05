//! The half of the WebGL2 probe that touches a browser. wasm32 only.
//!
//! A **second** canvas — `document.createElement("canvas")`, never attached
//! to the document, one texel square — with a WebGL2 context of its own: the
//! app's canvas already holds the context wgpu draws through, and a second
//! `getContext("webgl2")` on it would hand that same context back, so the
//! probe's textures would sit in the app's own context and its loss would be
//! the app's. Everything here is raw `web-sys` (`super::webgl2` says why wgpu
//! cannot be the instrument on this backend), and the walk itself lives
//! there, host-tested; this file is the [`GlContext`] over a real
//! `WebGL2RenderingContext`, the yield that keeps the page's frames, and the
//! window on the 3D latch.
//!
//! **The window.** A browser's answer to exhaustion may be to lose every
//! WebGL context in the tab, the app's included, and the app counts a loss
//! with a volume on screen against the 3D view
//! (`squallar_volumetric::degrade`). The probe opens that crate's window
//! before its first allocation and drops the guard when it is done, so a
//! loss inside it is recorded and latched nowhere; the guard closes with the
//! crate's grace, and the opener's bound closes it if this future never
//! returns. **Today the window's effect is nil**, and the crate's doc says
//! so: on this tree a WebGL context loss never reaches the latch it guards.
//! The policy cap on the ladder is what keeps the probe from losing the
//! app's context; the window is the contract for the day a restore path
//! exists.
//!
//! The result is parked in a thread-local the bridge reads on the telemetry
//! tick, as `super::run` parks its own.

use super::webgl2::{self, Fence, GlContext, GlError, Webgl2Outcome};
use super::{Allocation, TIME_BUDGET_MS};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    HtmlCanvasElement, WebGl2RenderingContext as Gl, WebGlFramebuffer, WebGlSync, WebGlTexture,
};

/// How long the latch's window may stay open at most, whatever this future
/// does: the probe's time budget, a second budget for a fence still pending
/// when the first runs out, and the grace the guard would have applied.
pub const PROBE_WINDOW_AT_MOST_MS: u64 =
    2 * TIME_BUDGET_MS + squallar_volumetric::degrade::PROBE_LOSS_GRACE_MS;

// The pure module spells the GL codes it judges without `web-sys`, so the
// host can test the judgement; this is where they are held to the binding.
const _: () = {
    assert!(webgl2::GL_NO_ERROR == Gl::NO_ERROR);
    assert!(webgl2::GL_OUT_OF_MEMORY == Gl::OUT_OF_MEMORY);
    assert!(webgl2::GL_CONTEXT_LOST_WEBGL == Gl::CONTEXT_LOST_WEBGL);
};

thread_local! {
    /// What the probe found, once it has. The page thread owns the bridge and
    /// runs the probe's future, so one thread writes and reads this. A
    /// `RefCell` rather than the WebGPU probe's `Cell` because the outcome
    /// carries the renderer string.
    static OUTCOME: RefCell<Option<Webgl2Outcome>> = const { RefCell::new(None) };
}

/// The outcome, once the probe has one; `None` while it is still running or
/// was never started.
pub fn outcome() -> Option<Webgl2Outcome> {
    OUTCOME.with(|cell| cell.borrow().clone())
}

/// Start the probe, walking to `policy_cap_bytes`
/// (`webgl2::policy_cap_for` the page's form factor). Returns at once: the
/// work is a spawned future that yields to the browser while each rung's
/// fence is pending, so the page keeps its frames.
pub fn start(policy_cap_bytes: u64) {
    wasm_bindgen_futures::spawn_local(probe(policy_cap_bytes));
}

fn now_ms() -> u64 {
    // `Date.now()` is a whole number of milliseconds; the cast truncates
    // nothing that matters to a two-second budget.
    js_sys::Date::now() as u64
}

async fn probe(policy_cap_bytes: u64) {
    let window = squallar_volumetric::degrade::ProbeWindow::open(now_ms, PROBE_WINDOW_AT_MOST_MS);
    let outcome = match Context::open() {
        Some(mut gl) => webgl2::walk(&mut gl, policy_cap_bytes, now_ms, yield_now).await,
        None => Webgl2Outcome::no_context(policy_cap_bytes),
    };
    drop(window);
    OUTCOME.with(|cell| *cell.borrow_mut() = Some(outcome));
}

/// Give the browser its event loop back for one turn: a `setTimeout(0)`
/// promise. A microtask would not do — a WebGL sync object's status is
/// updated between tasks, and a page that never returns to the loop never
/// sees its fence signal, and never draws either.
async fn yield_now() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let scheduled = web_sys::window().and_then(|window| {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0)
                .ok()
        });
        if scheduled.is_none() {
            // Nothing to schedule on: resolve now rather than hang the probe.
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// The probe's own WebGL2 context and what it holds.
struct Context {
    gl: Gl,
    /// Held so the canvas outlives the context made on it.
    _canvas: HtmlCanvasElement,
    framebuffer: Option<WebGlFramebuffer>,
    textures: Vec<WebGlTexture>,
    fence: Option<WebGlSync>,
}

impl Context {
    /// A second canvas and a context on it, or `None` where the browser
    /// would not make one — no document, no WebGL2, a refused framebuffer.
    fn open() -> Option<Self> {
        let document = web_sys::window()?.document()?;
        let canvas: HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
        canvas.set_width(1);
        canvas.set_height(1);
        // The same `antialias: false` the app's own context is made with, so
        // the two contexts land on the same adapter; the rest turns off the
        // default framebuffer's planes, which the probe never draws to.
        let options = js_sys::Object::new();
        for (key, value) in [
            ("antialias", false),
            ("depth", false),
            ("stencil", false),
            ("preserveDrawingBuffer", false),
        ] {
            js_sys::Reflect::set(
                &options,
                &JsValue::from_str(key),
                &JsValue::from_bool(value),
            )
            .ok()?;
        }
        let gl: Gl = canvas
            .get_context_with_context_options("webgl2", &options)
            .ok()
            .flatten()?
            .dyn_into()
            .ok()?;
        let framebuffer = gl.create_framebuffer()?;
        gl.bind_framebuffer(Gl::FRAMEBUFFER, Some(&framebuffer));
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        Some(Self {
            gl,
            _canvas: canvas,
            framebuffer: Some(framebuffer),
            textures: Vec::new(),
            fence: None,
        })
    }
}

impl GlContext for Context {
    /// `UNMASKED_RENDERER_WEBGL` through `WEBGL_debug_renderer_info` where
    /// the browser still exposes the extension, else `RENDERER` — which
    /// Firefox has answered with the real device since it retired the
    /// extension. The queue is drained afterwards so a refused query
    /// (`INVALID_ENUM`) is not read as the first rung's fault.
    fn renderer(&self) -> Option<String> {
        /// `WEBGL_debug_renderer_info`'s `UNMASKED_RENDERER_WEBGL`, spelled
        /// here because web-sys carries it on the extension's own type and
        /// the crate takes no feature for one object.
        const UNMASKED_RENDERER_WEBGL: u32 = 0x9246;
        let unmasked = self
            .gl
            .get_extension("WEBGL_debug_renderer_info")
            .ok()
            .flatten()
            .and_then(|_| self.gl.get_parameter(UNMASKED_RENDERER_WEBGL).ok())
            .and_then(|value| value.as_string());
        let renderer = unmasked.or_else(|| {
            self.gl
                .get_parameter(Gl::RENDERER)
                .ok()
                .and_then(|value| value.as_string())
        });
        for _ in 0..16 {
            if self.gl.get_error() == Gl::NO_ERROR {
                break;
            }
        }
        renderer.filter(|name| !name.is_empty())
    }

    fn max_texture_size(&self) -> Option<u32> {
        self.gl
            .get_parameter(Gl::MAX_TEXTURE_SIZE)
            .ok()?
            .as_f64()
            .filter(|n| *n >= 1.0)
            .map(|n| n as u32)
    }

    fn allocate_and_clear(&mut self, allocation: &Allocation) -> bool {
        let (Ok(width), Ok(height)) = (
            i32::try_from(allocation.width),
            i32::try_from(allocation.height),
        ) else {
            return false;
        };
        for _ in 0..allocation.layers {
            let Some(texture) = self.gl.create_texture() else {
                return false;
            };
            self.gl.bind_texture(Gl::TEXTURE_2D, Some(&texture));
            self.gl
                .tex_storage_2d(Gl::TEXTURE_2D, 1, Gl::RGBA8, width, height);
            self.gl.framebuffer_texture_2d(
                Gl::FRAMEBUFFER,
                Gl::COLOR_ATTACHMENT0,
                Gl::TEXTURE_2D,
                Some(&texture),
                0,
            );
            // The clear is what makes the storage resident rather than
            // reserved: every texel is written.
            self.gl.clear(Gl::COLOR_BUFFER_BIT);
            self.textures.push(texture);
        }
        self.gl.framebuffer_texture_2d(
            Gl::FRAMEBUFFER,
            Gl::COLOR_ATTACHMENT0,
            Gl::TEXTURE_2D,
            None,
            0,
        );
        self.gl.bind_texture(Gl::TEXTURE_2D, None);
        if let Some(previous) = self.fence.take() {
            self.gl.delete_sync(Some(&previous));
        }
        // The fence goes behind the clears, and the flush pushes both to the
        // GPU; the walk polls it between yields instead of calling `finish()`,
        // which would hold the page's thread for the whole clear.
        self.fence = self.gl.fence_sync(Gl::SYNC_GPU_COMMANDS_COMPLETE, 0);
        self.gl.flush();
        true
    }

    fn fence(&mut self) -> Fence {
        match &self.fence {
            Some(sync) => {
                let status = self.gl.get_sync_parameter(sync, Gl::SYNC_STATUS);
                if status.as_f64() == Some(f64::from(Gl::SIGNALED)) {
                    Fence::Signalled
                } else {
                    Fence::Pending
                }
            }
            // No fence could be made: fall back to `finish()`, which blocks
            // until the clears are done, and call the rung settled.
            None => {
                self.gl.finish();
                Fence::Signalled
            }
        }
    }

    fn take_error(&mut self) -> GlError {
        let mut codes = Vec::new();
        // Bounded: a healthy context drains, a lost one answers `NO_ERROR`
        // after its one `CONTEXT_LOST_WEBGL`, but a queue is a queue.
        for _ in 0..16 {
            let code = self.gl.get_error();
            if code == Gl::NO_ERROR {
                break;
            }
            codes.push(code);
        }
        webgl2::reduce_errors(codes)
    }

    fn is_context_lost(&self) -> bool {
        self.gl.is_context_lost()
    }

    fn release(&mut self) {
        for texture in self.textures.drain(..) {
            self.gl.delete_texture(Some(&texture));
        }
        if let Some(fence) = self.fence.take() {
            self.gl.delete_sync(Some(&fence));
        }
        if let Some(framebuffer) = self.framebuffer.take() {
            self.gl.bind_framebuffer(Gl::FRAMEBUFFER, None);
            self.gl.delete_framebuffer(Some(&framebuffer));
        }
        lose_context(&self.gl);
    }
}

/// `WEBGL_lose_context.loseContext()`: give the context's memory back now
/// rather than when the collector finds the canvas. Reached through
/// `Reflect`, as the bridge reads `navigator`, so the crate carries no
/// `web-sys` feature for one extension object; a browser without the
/// extension keeps the context until collection, which is slower and not
/// wrong.
fn lose_context(gl: &Gl) {
    let Ok(Some(extension)) = gl.get_extension("WEBGL_lose_context") else {
        return;
    };
    let Ok(lose) = js_sys::Reflect::get(&extension, &JsValue::from_str("loseContext")) else {
        return;
    };
    if let Ok(lose) = lose.dyn_into::<js_sys::Function>() {
        let _ = lose.call0(&extension);
    }
}
