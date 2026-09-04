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
 * The pages a memory is CONSTRUCTED with -- the module's declared minimum,
 * which is what the generated glue passes when nobody supplies a memory
 * (`__wbg_get_imports`: `new WebAssembly.Memory({initial:65, ...})`).
 *
 * A supplied memory matches the module's import only when its minimum is at
 * or above the module's; too small is a `LinkError`. This figure therefore
 * tracks the module's static data and can drift when that grows, which is why
 * every caller below falls back to letting the glue construct the memory
 * itself rather than failing to boot. The fallback is a lost per-device
 * ceiling, never a lost page.
 */
export const INITIAL_PAGES = 65;

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
 * jobs in flight, bounded by `WASM_MAX_CONCURRENT_RENDERS` (3), plus the tile
 * lane's parse and style scratch on the same heap. The largest single
 * allocation ever measured there is the MRMS decoder's 98 MB grid. 256 MiB is
 * two of those plus room, and it keeps the PAIR at 768 MiB on a 2 GiB phone
 * -- two heaps in two address spaces, but one physical RAM, which is the only
 * reason the two figures are ever considered together.
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
 * Returns the maximum in force, which is `maxBytes` when the supplied memory
 * was accepted and `DESKTOP_PAGE_BYTES` -- the module's own declared bound --
 * when it was not and the glue built its own. **The fallback is the whole
 * reason this is a function**: `INITIAL_PAGES` above tracks a figure the
 * linker chooses, and a page that would not boot because that figure drifted
 * is a far worse outcome than a page that boots at the declared bound. A
 * refusal is said once, at `warn`, and the number it returns is the truth the
 * app is then told -- never the number that was asked for.
 */
export async function initWithHeap(init, maxBytes, options) {
  const rest = options || {};
  try {
    const memory = new WebAssembly.Memory({
      initial: INITIAL_PAGES,
      maximum: maxBytes / PAGE_BYTES,
      shared: true,
    });
    await init({ ...rest, memory });
    return maxBytes;
  } catch (e) {
    // Deliberately not phrased as "the engine refused the memory": the throw
    // could have come from anywhere inside instantiation, and the retry below
    // is what distinguishes the two -- if the cause was not the memory it
    // throws again and the caller reports it.
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
