/*
 * How big a linear memory this device gets, decided BEFORE the module is
 * instantiated.
 *
 * # Why this is JavaScript and not Rust
 *
 * The maximum of a `shared` `WebAssembly.Memory` is fixed at construction and
 * there is no way to read it back: `WebAssembly.Memory.prototype.type()` does
 * not exist in Firefox or in Chromium (measured, 2026-09-03), and `byteLength`
 * is the CURRENT size. So the choice has to be made by whoever constructs the
 * memory, which is the caller of the glue's `init` -- before a single line of
 * Rust has run. `squallar-web/src/form_factor.rs` owns the same rule for the
 * signals the app reads AFTER boot; this file is that rule moved to the only
 * place it can run this early, and the figures below are held equal to the
 * Rust constants by `squallar-web/tests/linear_memory_ceiling.rs`.
 *
 * # What the maximum is, and is not
 *
 * It is a VALIDATION bound, not an allocation. Measured on x86-64 Linux,
 * 2026-09-03: Firefox reserves a fixed ~4 GiB `PROT_NONE` region per wasm
 * memory whatever maximum is declared (byte-identical smaps at a declared
 * 1 GiB and 4 GiB, Rss 0.00 in both), and Chromium's construction costs
 * ~2.2 MiB RSS at every maximum tested. So a SMALLER maximum buys no resident
 * memory back at construction. What it buys is the wall: an allocation past it
 * is refused, and this application's own watermark
 * (`squallar_device_profile::linear_memory`) sheds against the wall it is told
 * about. A phone given a 1 GiB wall climbs to 1 GiB and is killed by the OS
 * before its own levers ever fire; a phone given a 512 MiB wall starts
 * shedding at 384 MiB and never gets there.
 *
 * # The import limit permits shrinking, never growing
 *
 * The module is linked with `--max-memory=1073741824`
 * (`.github/scripts/wasm-threads.sh`), which is what its memory import
 * DECLARES. A supplied memory whose maximum is <= that instantiates; one
 * above it raises `LinkError: imported Memory with incompatible maximum
 * size` (54 cells plus negative controls, both engines, 2026-09-03). So every
 * figure here is at or below the link flag, and raising the flag is a
 * separate question gated on an Android measurement nobody has taken.
 */

/** wasm's page size. A memory's maximum is declared in pages, never bytes. */
export const PAGE_BYTES = 65536;

/**
 * The module the page and the rasterization worker instantiate, relative to
 * THIS file. The generated glue resolves the same file relative to itself
 * (`new URL("squallar_web_bg.wasm", import.meta.url)` inside `pkg/`); this
 * spelling is the one `sw.js` precaches under `SHELL_PATHS`, and
 * `tests/linear_memory_ceiling.rs` holds the two equal.
 */
export const MODULE_PATH = "./pkg/squallar_web_bg.wasm";

/*
 * # The pages a memory is CONSTRUCTED with, and why nothing here states them
 *
 * A supplied memory matches the module's import only when its `initial` is
 * at or above the minimum the module DECLARES; one page under is
 * `LinkError: imported Memory with incompatible size`. That minimum is the
 * linker's: the module's static data plus its shell stack, rounded up to a
 * page, and it moves whenever either grows.
 *
 * This file used to hold it as `INITIAL_PAGES = 65`, a hand-copied figure
 * read off the generated glue. It drifted: the module came to declare 66,
 * every page and every worker then instantiated through the `LinkError`
 * fallback in `initWithHeap` -- a page that boots, at the module's declared
 * bound rather than the per-device one, with one `warn` nobody was reading
 * -- and the browser rig stayed green, because nothing asserted the absence
 * of that warning (it does now: `drive.py` fails a leg that prints it).
 *
 * So the figure is not stated. It is READ, off the module's own bytes,
 * before instantiation: `declaredMinimumPages` fetches the module, parses
 * its import section (`readDeclaredMinimum`) far enough to find the memory
 * import's limits, and hands the same `Response` on to the glue for
 * `instantiateStreaming`. One download, not two -- the parse reads a
 * `clone()` of the body, which tees the one network stream, and cancels its
 * branch as soon as the limits are in hand (the import section of this
 * module ends ~54 KB in). A figure read off the module cannot drift from the
 * module; the generous-constant alternative (256 pages, say) merely moves
 * the wall to where it will not be hit for a while, and this file has
 * already shown what a wall nobody watches does.
 *
 * The fallback stays, for the reason it always had: a parse that fails on
 * some engine's `Response` is a page that boots at the declared bound, not
 * a page that does not boot. It is said once at `warn`, and the rig reads it.
 */

/**
 * The module's declared memory minimum, in pages, read off its leading
 * bytes -- or a request for more of them.
 *
 * A wasm binary is a magic, a version and a sequence of sections, each an id
 * byte, a LEB128 size and a payload. Non-custom sections are in ascending id
 * order, and the import section is id 2: a count, then per import a module
 * name, a field name, a kind byte and the kind's description -- for a memory
 * (kind 2), the limits: a flags byte, a LEB128 minimum, and a LEB128 maximum
 * when bit 0 of the flags is set. Nothing after the memory import is read.
 *
 * Returns `{ kind: "min", pages }` when the memory import was found,
 * `{ kind: "none" }` when the bytes are not a wasm module or the module
 * imports no memory (nothing to supply; the glue's own default is right),
 * and `{ kind: "more" }` when `bytes` ends before the answer does. Throws on
 * a malformed section, which the caller treats as "none".
 */
export function readDeclaredMinimum(bytes) {
  const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const MORE = { kind: "more" };
  let at = 0;
  const have = (n) => at + n <= u8.length;
  const byte = () => {
    if (!have(1)) throw MORE;
    return u8[at++];
  };
  // Unsigned LEB128. Multiplication rather than a shift, so a 64-bit limit
  // (memory64's) reads as a number rather than wrapping at 32 bits.
  const leb = () => {
    let value = 0;
    let scale = 1;
    for (let i = 0; i < 10; i++) {
      const b = byte();
      value += (b & 0x7f) * scale;
      if ((b & 0x80) === 0) return value;
      scale *= 128;
    }
    throw new Error("LEB128 integer longer than 10 bytes");
  };
  const limits = () => {
    const flags = byte();
    const min = leb();
    if (flags & 1) leb();
    return min;
  };
  try {
    if (!have(8)) throw MORE;
    if (u8[0] !== 0x00 || u8[1] !== 0x61 || u8[2] !== 0x73 || u8[3] !== 0x6d) {
      return { kind: "none" };
    }
    at = 8;
    for (;;) {
      const id = byte();
      const size = leb();
      if (id === 2) {
        if (!have(size)) throw MORE;
        const count = leb();
        for (let i = 0; i < count; i++) {
          // Two statements each, not `at += leb()`: `+=` reads `at` before
          // the call advances it, and would put the cursor back over the
          // length byte.
          const moduleName = leb();
          at += moduleName;
          const fieldName = leb();
          at += fieldName;
          const kind = byte();
          if (kind === 0) leb(); // function: a type index
          else if (kind === 1) {
            byte(); // table: a reference type, then limits
            limits();
          } else if (kind === 2) return { kind: "min", pages: limits() };
          else if (kind === 3) {
            byte(); // global: a value type
            byte(); // and a mutability
          } else if (kind === 4) {
            byte(); // tag: an attribute
            leb(); // and a type index
          } else throw new Error("unknown import kind " + kind);
        }
        return { kind: "none" };
      }
      // Sections are ordered: an id past 2 that is not a custom section
      // means the module has no import section at all.
      if (id > 2) return { kind: "none" };
      if (!have(size)) throw MORE;
      at += size;
    }
  } catch (e) {
    if (e === MORE) return MORE;
    throw e;
  }
}

/**
 * How much of the module `declaredMinimumPages` will read before giving up.
 * The import section of this module ends ~54 KB in; a module whose memory
 * import sits past this is one whose glue has changed shape enough that the
 * fallback is the right answer anyway.
 */
export const HEADER_READ_CAP_BYTES = 4 * 1024 * 1024;

/**
 * The memory minimum `response`'s module declares, in pages -- `null` when
 * it imports no memory -- reading only as much of the body as the answer
 * needs and cancelling the rest.
 *
 * Takes a `Response` the caller has already `clone()`d: the clone's body is
 * one branch of a tee over the single network stream, so reading it here
 * does not consume the branch the glue will stream from, and cancelling it
 * once the limits are read stops the tee buffering for it.
 */
export async function declaredMinimumPages(response) {
  const reader = response.body.getReader();
  let held = new Uint8Array(0);
  try {
    for (;;) {
      const read = readDeclaredMinimum(held);
      if (read.kind === "min") return read.pages;
      if (read.kind === "none") return null;
      if (held.length >= HEADER_READ_CAP_BYTES) {
        throw new Error(
          "no memory import within the first " +
            HEADER_READ_CAP_BYTES +
            " bytes of the module",
        );
      }
      const { value, done } = await reader.read();
      if (done) throw new Error("the module ended before its import section");
      const next = new Uint8Array(held.length + value.length);
      next.set(held);
      next.set(value, held.length);
      held = next;
    }
  } finally {
    reader.cancel().catch(function () {});
  }
}

/**
 * **The full declared bound**: what a desktop gets, equal to the link flag.
 * `squallar_device_profile::constants::WASM_LINEAR_MEMORY_MAX_BYTES`.
 */
export const DESKTOP_PAGE_BYTES = 1024 * 1024 * 1024;

/**
 * **What a handheld's PAGE instance gets.** The page heap holds the overlay
 * pictures (41.7 MB each on the measured `huge` legs), the loop pool and the
 * basemap's host-side tile caches; the wasm bracket's own FLOOR for the last
 * two is 128 MiB of tile host ceiling plus 56 MiB of loop pool
 * (`WASM_TILE_HOST_CEILING_BYTES[0]`, `WASM_LOOP_POOL_FLOOR_BYTES`), so a
 * ceiling under ~256 MiB would leave the watermark permanently in `Act` with
 * nothing left to shed. 512 MiB clears that floor by 328 MiB and puts the
 * warning line at 384 MiB and the percentage action line at 445 MiB, both
 * reachable while the levers still have room to work.
 */
export const HANDHELD_PAGE_BYTES = 512 * 1024 * 1024;

/**
 * **What a handheld's rasterization WORKER gets**, and it is deliberately not
 * the page's figure. The worker holds no cache: it holds the buffers of the
 * jobs in flight, plus the tile lane's parse and style scratch on the same
 * heap. The largest single allocation ever measured there is the MRMS
 * decoder's 98 MB grid. 256 MiB is two of those plus room, and it keeps the
 * PAIR at 768 MiB on a 2 GiB phone -- two heaps in two address spaces, but
 * one physical RAM, which is the only reason the two figures are ever
 * considered together.
 *
 * **The figure is a policy against the largest single allocation, not a
 * derivation from what is in flight, because what is in flight is not
 * bounded in bytes.** The radar family shares one slot
 * (`WASM_MAX_CONCURRENT_RENDERS`, which is 1 on this target); the overlay
 * rasters -- the picture-sized jobs, and the ones this heap is really sized
 * for -- do not pass through that counter at all. Their cap is per (pane,
 * layer, slot) and the aggregate is panes x texture layers, which
 * `squallar_egui::overlay_cache::InFlight` states in its own doc that the
 * budget does not bound. Nothing prices this heap and no lever of the
 * application reaches it: both the admission door and the watermark that
 * grant the work are taken against the PAGE's ceiling
 * (`squallar_app::app_render::host_spare_bytes`), so a grant made there
 * commits this instance to rasters no term of it has costed.
 */
export const HANDHELD_WORKER_BYTES = 256 * 1024 * 1024;

/**
 * The `deviceMemory` bucket at or under which a device is treated as a
 * handheld whatever its pointers say -- `DECLARED_RAM_HANDHELD_BYTES`.
 *
 * `navigator.deviceMemory` is a coarse hint the page declares about itself,
 * rounded to a power of two between 0.25 and 8 GiB, and it is absent in
 * Firefox, which governs. So it may only LOWER a presumption, never raise
 * one: a device that declares 8 GiB and reads as a handheld stays a handheld.
 */
export const DECLARED_HANDHELD_BYTES = 2 * 1024 * 1024 * 1024;

/** The `name` a rasterization worker is started under; see `heapFromName`. */
export const WORKER_NAME_PREFIX = "squallar-raster:";

/**
 * The form factor, from the same three signals and the same truth table as
 * `squallar-web/src/form_factor.rs::classify`: **handheld is a coarse primary
 * pointer with no fine pointer anywhere; a fine pointer anywhere is a
 * desktop**, and only when neither query decided does `maxTouchPoints` break
 * the tie. `null` for any signal the browser would not give, and `null` out
 * when nothing decided.
 */
export function classifyFormFactor(coarse, anyFine, maxTouchPoints) {
  if (anyFine === true) return "desktop";
  if (coarse === true && anyFine === false) return "handheld";
  if (maxTouchPoints === null || maxTouchPoints === undefined) return null;
  return maxTouchPoints > 0 ? "handheld" : "desktop";
}

/**
 * The two maxima, in bytes, for a device with these signals.
 *
 * A desktop gets the full declared bound on both instances. Anything else --
 * a handheld, a device nothing classified, or a desktop-shaped device that
 * declares a handheld's memory -- gets the handheld pair. **Unclassified
 * falls to the smaller on purpose**: an over-small ceiling costs quality
 * through the levers, an over-large one costs the tab.
 */
export function heapMaxBytes(signals) {
  const form = classifyFormFactor(
    signals.pointerCoarse,
    signals.anyPointerFine,
    signals.maxTouchPoints,
  );
  const declaredSmall =
    typeof signals.deviceMemoryBytes === "number" &&
    signals.deviceMemoryBytes > 0 &&
    signals.deviceMemoryBytes <= DECLARED_HANDHELD_BYTES;
  if (form === "desktop" && !declaredSmall) {
    return { page: DESKTOP_PAGE_BYTES, worker: DESKTOP_PAGE_BYTES };
  }
  return { page: HANDHELD_PAGE_BYTES, worker: HANDHELD_WORKER_BYTES };
}

/**
 * What the page can say about itself, synchronously, before `init()`. Every
 * read is guarded: a browser that will not run a media query answers `null`,
 * which is unknown and never "did not match".
 *
 * **There is no override.** No setting writes these, no URL parameter reads
 * them, nothing persists across sessions and nothing is user-facing -- the
 * choice is remade from the device's own signals at every startup. Forcing an
 * arm is done the way `serve.py --doctor-first-worker` forces a build token:
 * by doctoring the served asset, outside the product.
 *
 * Measured in this rig's own browsers, 2026-09-03, on this box: Firefox under
 * Xvfb answers `(any-pointer: fine)` true and classifies desktop on the first
 * rule; headless Chromium answers `(pointer: none)` and `(any-pointer: none)`
 * -- neither query decides -- and classifies desktop on the `maxTouchPoints`
 * tiebreak. Neither exposes `deviceMemory` there.
 */
export function pageSignals(global) {
  const g = global || globalThis;
  const media = (query) => {
    try {
      return g.matchMedia ? g.matchMedia(query).matches : null;
    } catch (e) {
      return null;
    }
  };
  const nav = g.navigator || {};
  const number = (value) =>
    typeof value === "number" && isFinite(value) ? value : null;
  const declared = number(nav.deviceMemory);
  return {
    pointerCoarse: media("(pointer: coarse)"),
    anyPointerFine: media("(any-pointer: fine)"),
    maxTouchPoints: number(nav.maxTouchPoints),
    deviceMemoryBytes: declared === null ? null : declared * 1024 * 1024 * 1024,
  };
}

/**
 * The maxima this page and the worker it will start should be given. The page
 * decides for BOTH because a `DedicatedWorkerGlobalScope` has neither
 * `matchMedia` nor `maxTouchPoints` -- a `WorkerNavigator` carries
 * `hardwareConcurrency` and `deviceMemory` and nothing else this rule reads --
 * so a worker left to classify itself would read `null` on the governing
 * engine and take the handheld arm on every desktop. The page hands its answer
 * over on the worker's `name` -- `WORKER_NAME_PREFIX` followed by the byte
 * count, written by `worker_port::spawn` and read by `heapFromName` -- which
 * costs no URL, no query string and no cache entry.
 */
export function chooseHeapMaxBytes(global) {
  return heapMaxBytes(pageSignals(global));
}

/**
 * The byte count out of a worker `name`, or `null` for a name that carries
 * none -- a worker started by a page from a build before this, or opened
 * directly. `null` is unknown, and the caller falls back to letting the glue
 * construct the memory at the module's declared bound.
 */
export function heapFromName(name) {
  if (typeof name !== "string" || !name.startsWith(WORKER_NAME_PREFIX)) {
    return null;
  }
  const bytes = Number(name.slice(WORKER_NAME_PREFIX.length));
  if (!isFinite(bytes) || bytes <= 0 || bytes % PAGE_BYTES !== 0) return null;
  return bytes;
}

/**
 * Instantiate the module with a memory of exactly `maxBytes`, and say what the
 * instance actually got.
 *
 * The memory's `initial` is the module's own declared minimum, read off the
 * module's bytes by `declaredMinimumPages` from the same fetch the glue then
 * streams the module from (the block above says why it is read and not
 * stated). `fetchModule` is the fetch, overridable for a test that has no
 * network; production passes nothing and gets `fetch` of `MODULE_PATH`.
 *
 * Returns the maximum in force, which is `maxBytes` when the supplied memory
 * was accepted and `DESKTOP_PAGE_BYTES` -- the module's own declared bound --
 * when it was not and the glue built its own. **The fallback is the whole
 * reason this is a function**: a page that would not boot because the parse
 * or the engine refused is a far worse outcome than a page that boots at the
 * declared bound. A refusal is said once, at `warn`, and the number it
 * returns is the truth the app is then told -- never the number that was
 * asked for. The rig (`drive.py`) fails a leg that prints that warning, so a
 * fallback that fires on every boot is a red row and not a lost ceiling
 * nobody notices.
 */
export async function initWithHeap(init, maxBytes, options, fetchModule) {
  const rest = options || {};
  try {
    const response = await (fetchModule
      ? fetchModule()
      : fetch(new URL(MODULE_PATH, import.meta.url)));
    if (!response.ok) {
      throw new Error(
        "fetching the module answered " + response.status + " " + response.statusText,
      );
    }
    const pages = await declaredMinimumPages(response.clone());
    if (pages === null) throw new Error("the module imports no memory");
    const memory = new WebAssembly.Memory({
      initial: pages,
      maximum: maxBytes / PAGE_BYTES,
      shared: true,
    });
    await init({ ...rest, module_or_path: response, memory });
    return maxBytes;
  } catch (e) {
    // Deliberately not phrased as "the engine refused the memory": the throw
    // could have come from anywhere inside the fetch, the parse or the
    // instantiation, and the retry below is what distinguishes them -- if
    // the cause was not the memory it throws again and the caller reports it.
    console.warn(
      "squallar: could not instantiate with a " +
        maxBytes / (1024 * 1024) +
        " MiB linear memory (" +
        String(e) +
        "); retrying at the module's declared bound",
    );
  }
  await init({ ...rest });
  return DESKTOP_PAGE_BYTES;
}
