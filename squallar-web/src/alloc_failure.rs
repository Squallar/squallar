//! What an allocation failure says before the instance aborts.
//!
//! On wasm32 an allocation the engine cannot serve ends in `rust_oom`: the
//! alloc-error hook runs, then `abort`, which is an `unreachable` trap. The
//! default hook writes its one line to stderr, and a browser page has no
//! stderr, so the trap reads in the console as `RuntimeError: unreachable
//! executed` and nothing else — the `huge` leg of 2026-09-02 produced eight
//! of them before anyone could say they were out-of-memory, and the proof
//! took a disassembly. The hook installed here says so in the console, with
//! the request and the heap, and then lets the abort happen.
//!
//! **Nothing here allocates**, because the heap has just refused: the line is
//! written into a fixed buffer on the stack through `core::fmt::Write`
//! (integer formatting needs no heap), the heap reading is a JS property
//! read, and `console.error` is handed a `&str` the glue copies on the JS
//! side. `-Zoom=panic` was the other spelling and is not taken: it routes
//! the failure through the panic hook, and `console_error_panic_hook`
//! formats the message and a stack trace into a `String` — an allocation at
//! the moment allocation failed, so the line it would print is the one line
//! most likely never to be printed.
//!
//! The line is pure and tested on the host; only [`hook`], which reads the
//! instance's memory and needs the nightly `alloc_error_hook` feature the
//! wasm build carries (`.github/scripts/wasm-threads.sh`), is wasm32-only.

use core::fmt;

/// Bytes the line may take, stack-allocated. The longest line the format
/// can produce — a request past `u64` digits and two MiB figures — fits with
/// room to spare, and a longer one is cut rather than grown.
pub const LINE_CAPACITY: usize = 96;

/// One line of ASCII in a fixed buffer, written through [`fmt::Write`] with
/// no heap behind it.
pub struct Line {
    buf: [u8; LINE_CAPACITY],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            buf: [0; LINE_CAPACITY],
            len: 0,
        }
    }

    /// The line so far.
    pub fn as_str(&self) -> &str {
        // Every write was a `&str` cut at a char boundary, so this is valid
        // UTF-8; the format below is ASCII besides.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = LINE_CAPACITY - self.len;
        let mut take = s.len().min(room);
        while take > 0 && !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// `alloc failed: <requested> B requested, <linear> of <max> MiB linear` —
/// the request the allocator refused and where the instance's linear memory
/// stood, in MiB by integer division. `unread` where the memory could not be
/// read, which a hook that must not allocate has to allow for.
pub fn line(requested: usize, linear_bytes: Option<u64>, max_bytes: u64) -> Line {
    use fmt::Write;

    let mut out = Line::new();
    let max_mib = max_bytes >> 20;
    let _ = match linear_bytes {
        Some(linear) => write!(
            out,
            "alloc failed: {requested} B requested, {} of {max_mib} MiB linear",
            linear >> 20
        ),
        None => write!(
            out,
            "alloc failed: {requested} B requested, unread of {max_mib} MiB linear"
        ),
    };
    out
}

/// The hook itself: installed once per module instance — the page in
/// `entry::start`, the worker in `worker::squallar_worker_main` — and run
/// by std on the way to `rust_oom`.
#[cfg(target_arch = "wasm32")]
pub mod hook {
    use squallar_device_profile::constants::WASM_LINEAR_MEMORY_MAX_BYTES;

    /// Install [`report`] as this instance's alloc-error hook.
    pub fn install() {
        std::alloc::set_alloc_error_hook(report);
    }

    /// Say what failed and where the heap stood, through `console.error`,
    /// and return — std aborts the instance after this returns. Nothing here
    /// allocates on the wasm side: the reading is a property of the memory
    /// object, the line is a stack buffer, and `JsValue::from_str` hands the
    /// glue a pointer and a length to copy.
    fn report(layout: std::alloc::Layout) {
        let linear = crate::shared_loan::memory_bytes();
        let line = super::line(layout.size(), linear, WASM_LINEAR_MEMORY_MAX_BYTES);
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(line.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line at the `huge` leg's own trap: a 43.5 MB picture refused
    /// with the page at 1019 of 1024 MiB. Integers only, ASCII only, under
    /// the buffer.
    #[test]
    fn the_line_names_the_request_and_the_heap_in_mib() {
        let at_the_wall = line(43_518_037, Some(1019 * (1 << 20) + 12345), 1 << 30);
        assert_eq!(
            at_the_wall.as_str(),
            "alloc failed: 43518037 B requested, 1019 of 1024 MiB linear"
        );
        assert!(at_the_wall.as_str().is_ascii());
        assert!(!at_the_wall.as_str().contains('.'));
        assert_eq!(
            line(98_000_000, None, 1 << 30).as_str(),
            "alloc failed: 98000000 B requested, unread of 1024 MiB linear"
        );
    }

    /// The buffer holds the longest line the format can make, and a write
    /// past it is cut at a character boundary rather than grown or panicked
    /// on — the one place a panic would be worse than silence.
    #[test]
    fn the_buffer_never_grows_and_never_panics() {
        use fmt::Write;

        let longest = line(usize::MAX, Some(u64::MAX), u64::MAX);
        assert!(longest.as_str().len() < LINE_CAPACITY, "{}", longest.as_str());
        assert!(longest.as_str().starts_with("alloc failed: 18446744073709551615 B requested, "));

        let mut full = Line::new();
        for _ in 0..LINE_CAPACITY {
            full.write_str("x").unwrap();
        }
        assert_eq!(full.as_str().len(), LINE_CAPACITY);
        full.write_str("y").unwrap();
        assert_eq!(full.as_str().len(), LINE_CAPACITY, "a full buffer grew");

        let mut nearly = Line::new();
        for _ in 0..LINE_CAPACITY - 1 {
            nearly.write_str("x").unwrap();
        }
        nearly.write_str("\u{e9}").unwrap();
        assert_eq!(
            nearly.as_str().len(),
            LINE_CAPACITY - 1,
            "a multi-byte character was split at the buffer's end"
        );
    }
}
