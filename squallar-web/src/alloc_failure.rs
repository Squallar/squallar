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
pub const LINE_CAPACITY: usize = 128;

/// One line of ASCII in a fixed buffer of `N` bytes, written through
/// [`fmt::Write`] with no heap behind it.
///
/// Const-generic over the capacity because the hook writes **two** lines of
/// very different lengths — the refusal itself, and the per-family heap
/// census behind it — and a single buffer sized for the longer would put
/// half a kilobyte of stack under every refusal that only needs a hundred
/// bytes. The write behaviour is identical at either size; only the room is.
pub struct Line<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Line<N> {
    fn new() -> Self {
        Self {
            buf: [0; N],
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

impl<const N: usize> fmt::Write for Line<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = N - self.len;
        let mut take = s.len().min(room);
        while take > 0 && !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// `alloc failed: <requested> B requested, <linear> of <max> MiB linear in
/// <instance>` — the request the allocator refused, where the instance's
/// linear memory stood, in MiB by integer division, and **which instance it
/// was**. `unread` where the memory could not be read, which a hook that must
/// not allocate has to allow for.
///
/// The instance is on the line because the page and the workers run the same
/// module and the same hook, so an unnamed refusal is a refusal on one of
/// three heaps with no way to tell which — and the page's heap is the only
/// one any lever reaches. The name is **appended**, so anything reading the
/// figures ahead of it by position reads them unchanged.
pub fn line(
    requested: usize,
    linear_bytes: Option<u64>,
    max_bytes: u64,
    instance: &str,
) -> Line<LINE_CAPACITY> {
    use fmt::Write;

    let mut out = Line::new();
    let max_mib = max_bytes >> 20;
    let _ = match linear_bytes {
        Some(linear) => write!(
            out,
            "alloc failed: {requested} B requested, {} of {max_mib} MiB linear in {instance}",
            linear >> 20
        ),
        None => write!(
            out,
            "alloc failed: {requested} B requested, unread of {max_mib} MiB linear in {instance}"
        ),
    };
    out
}

/// Which module instance this one is, for the refusal line and the census
/// behind it.
///
/// Three, because three wasm entry points install the hook and each is a
/// different heap question: the page's is the heap every budget is priced
/// against and every lever reaches, the rasterization worker's is a second
/// 1 GiB nothing prices, and the tile lane runs inside the worker's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instance {
    /// The main thread's instance, where the frame loop and every budget live.
    Page,
    /// The rasterization worker's own instance and its own 1 GiB.
    RasterWorker,
    /// The tile lane, which runs on the rasterization worker's memory.
    TileLane,
}

impl Instance {
    /// The word the lines print. Static strings, because the hook cannot
    /// build one.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::RasterWorker => "raster worker",
            Self::TileLane => "tile lane",
        }
    }

    /// The code stored in the hook's atomic, and its inverse. A `u8` rather
    /// than a pointer so the store is lock-free and the read inside the hook
    /// cannot fault.
    ///
    /// Public because the pair IS the crossing: the only consumer is the
    /// wasm32-only hook module, so on every other target these would be dead
    /// code, and the mapping they define is a property worth pinning on a
    /// host where a test can run — `an_instance_survives_the_hooks_atomic`.
    pub fn code(self) -> u8 {
        match self {
            Self::Page => 0,
            Self::RasterWorker => 1,
            Self::TileLane => 2,
        }
    }

    /// The instance a stored [`Self::code`] names. An unknown code cannot
    /// arise — nothing but `code` writes the atomic — and reads as the page,
    /// which is the arm a reader is most likely to be looking at anyway.
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::RasterWorker,
            2 => Self::TileLane,
            _ => Self::Page,
        }
    }
}

/// The hook itself: installed once per module instance — the page in
/// `entry::start`, the worker in `worker::squallar_worker_main`, the tile
/// lane in `worker::squallar_tile_lane_main` — and run by std on the way to
/// `rust_oom`.
#[cfg(target_arch = "wasm32")]
pub mod hook {
    use super::Instance;
    use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

    /// Which instance this module is running as, for [`report`] to name.
    ///
    /// Written once at [`install`] and read inside the hook. An atomic and
    /// not a `Mutex`, a `RefCell` or a `OnceCell<String>`, because the hook
    /// runs at the moment allocation failed: a lock could block, a `RefCell`
    /// could already be borrowed, and a `String` cannot be built. A `u8`
    /// load can do none of those things.
    static INSTANCE: AtomicU8 = AtomicU8::new(0);

    /// Install [`report`] as this instance's alloc-error hook, naming which
    /// instance this is.
    pub fn install(instance: Instance) {
        INSTANCE.store(instance.code(), Relaxed);
        std::alloc::set_alloc_error_hook(report);
    }

    /// Say what failed, where the heap stood, **which instance it was and
    /// what was holding that instance's heap**, and return — std aborts the
    /// instance after this returns.
    ///
    /// Nothing here allocates on the wasm side: the heap reading is a
    /// property of the memory object, the census is a handful of `Relaxed`
    /// atomic loads, both lines are stack buffers, and `JsValue::from_str`
    /// hands the glue a pointer and a length to copy.
    ///
    /// **The census is why this is worth doing here.** The application also
    /// writes that line on a two-second telemetry tick, and on the `huge`
    /// scene the page heap climbs hundreds of MB between two ticks — the
    /// pressure levers were measured firing 74-145 MiB above their own line
    /// for exactly that reason. A refusal is the one instant the breakdown
    /// is certainly the breakdown at the wall.
    ///
    /// **A census of zeros is a reading, not a failure.** The levels are
    /// published by the application, which runs on the page; a refusal in
    /// the rasterization worker or the tile lane prints zeros beside a real
    /// heap figure, and that says the whole of that instance's heap is
    /// unpriced — which is true, and is the reason the instance is named on
    /// both lines.
    fn report(layout: std::alloc::Layout) {
        let instance = Instance::from_code(INSTANCE.load(Relaxed));
        let linear = crate::shared_loan::memory_bytes();
        // **This instance's own ceiling, not the build's.** The two parted
        // when the maximum became a per-device choice made in JS before the
        // module existed (`crate::heap_max`); printing the link flag here
        // would say `of 1024 MiB` on a phone that was refused at 512, which
        // is the one figure a reader of this line would act on. And it is
        // per INSTANCE in the same sense the name beside it is: the page,
        // the worker and the lane are named separately here because their
        // heaps are separate, and on a handheld their ceilings differ too.
        // An instance that was never told judges against 0 and prints
        // `of 0 MiB`, which says "nobody said" rather than inventing a wall.
        let max = crate::heap_max::this_instance().unwrap_or(0);
        let line = super::line(layout.size(), linear, max, instance.as_str());
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(line.as_str()));

        let mut census = super::Line::<{ squallar_egui::heap_census::CENSUS_LINE_CAPACITY }>::new();
        // The `Result` is `fmt::Write`'s shape and not a case: a buffer that
        // ran out cuts rather than failing, and
        // `the_widest_line_fits_the_hooks_buffer` is what keeps it from
        // having to.
        let _ = squallar_egui::heap_census::write_line(
            &mut census,
            &squallar_egui::heap_census::census(),
            linear,
            instance.as_str(),
        );
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(census.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every instance survives the round trip through the hook's atomic.**
    ///
    /// The hook cannot hold a `&'static str`, so the instance crosses into it
    /// as a `u8` and comes back out by table. A code that mapped to the wrong
    /// arm would put the page's name on a worker's refusal — the one thing
    /// naming the instance was supposed to stop — and would do it only in a
    /// browser, where nothing tests it. So the mapping is pinned here, on the
    /// host, over all three.
    #[test]
    fn an_instance_survives_the_hooks_atomic() {
        for instance in [Instance::Page, Instance::RasterWorker, Instance::TileLane] {
            assert_eq!(Instance::from_code(instance.code()), instance);
        }
        let names: Vec<&str> = [Instance::Page, Instance::RasterWorker, Instance::TileLane]
            .into_iter()
            .map(Instance::as_str)
            .collect();
        assert_eq!(names, ["page", "raster worker", "tile lane"]);
        assert_eq!(
            Instance::from_code(200),
            Instance::Page,
            "a code nothing writes must still name an instance rather than panic"
        );
    }

    /// The line at the `huge` leg's own trap: a 43.5 MB picture refused
    /// with the page at 1019 of 1024 MiB, and **which instance refused it**.
    /// Integers only, ASCII only, under the buffer.
    #[test]
    fn the_line_names_the_request_and_the_heap_in_mib() {
        let at_the_wall = line(43_518_037, Some(1019 * (1 << 20) + 12345), 1 << 30, "page");
        assert_eq!(
            at_the_wall.as_str(),
            "alloc failed: 43518037 B requested, 1019 of 1024 MiB linear in page"
        );
        assert!(at_the_wall.as_str().is_ascii());
        assert!(!at_the_wall.as_str().contains('.'));
        assert_eq!(
            line(98_000_000, None, 1 << 30, "raster worker").as_str(),
            "alloc failed: 98000000 B requested, unread of 1024 MiB linear in raster worker"
        );

        // **The wall on the line is the INSTANCE's, and the same request
        // against a handheld's worker heap says so.** The hook reads
        // `heap_max::this_instance()` and not the link flag, which on this
        // arm would print `of 1024 MiB` beside a refusal that happened at
        // 256 -- the one figure a reader of this line would act on.
        assert_eq!(
            line(
                98_000_000,
                Some(255 * (1 << 20)),
                256 << 20,
                "raster worker"
            )
            .as_str(),
            "alloc failed: 98000000 B requested, 255 of 256 MiB linear in raster worker"
        );
        // An instance nobody told judges against nothing, and says nothing
        // rather than inventing a wall.
        assert_eq!(
            line(98_000_000, Some(300 << 20), 0, "tile lane").as_str(),
            "alloc failed: 98000000 B requested, 300 of 0 MiB linear in tile lane"
        );
    }

    /// The buffer holds the longest line the format can make, and a write
    /// past it is cut at a character boundary rather than grown or panicked
    /// on — the one place a panic would be worse than silence.
    #[test]
    fn the_buffer_never_grows_and_never_panics() {
        use fmt::Write;

        let longest = line(usize::MAX, Some(u64::MAX), u64::MAX, "raster worker");
        assert!(
            longest.as_str().len() < LINE_CAPACITY,
            "{}",
            longest.as_str()
        );
        assert!(
            longest
                .as_str()
                .starts_with("alloc failed: 18446744073709551615 B requested, ")
        );

        let mut full = Line::<LINE_CAPACITY>::new();
        for _ in 0..LINE_CAPACITY {
            full.write_str("x").unwrap();
        }
        assert_eq!(full.as_str().len(), LINE_CAPACITY);
        full.write_str("y").unwrap();
        assert_eq!(full.as_str().len(), LINE_CAPACITY, "a full buffer grew");

        let mut nearly = Line::<LINE_CAPACITY>::new();
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
