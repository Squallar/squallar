/*
 * How big a linear memory this instance gets, found BEFORE the module is
 * instantiated -- by asking the engine, not by guessing.
 *
 * # Why this is JavaScript and not Rust
 *
 * The maximum of a `shared` `WebAssembly.Memory` is fixed at construction and
 * there is no way to read it back: `WebAssembly.Memory.prototype.type()` does
 * not exist in Firefox or in Chromium (measured, 2026-09-03), and `byteLength`
 * is the CURRENT size. So the choice has to be made by whoever constructs the
 * memory, which is the caller of the glue's `init` -- before a single line of
 * Rust has run. The answer is handed to Rust as a value
 * (`squallar-web/src/heap_max.rs`), and `squallar-web/tests/linear_memory_ceiling.rs`
 * holds the ladder below to the link flag.
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
 * about.
 *
 * # Why a ladder, and not a figure per device
 *
 * This file used to pick one of two arms by pointer type -- 1 GiB for a
 * desktop's page and worker, 512/256 MiB for a handheld's -- under a module
 * linked at 1 GiB. Both were guesses, and the desktop guess was measured
 * wrong: on this rig's own box Chromium 152 and Firefox 155 construct a 4 GiB
 * shared memory and touch all of it, while Chromium's six-pane scene died at
 * its 1 GiB wall with eleven refused allocations. The other end is the
 * opposite: iOS WebKit refuses large shared maxima at CONSTRUCTION (2 GiB
 * throws, 256 MiB constructs, per published reports). No compile-time figure
 * serves both.
 *
 * So the module is linked at wasm32's architectural maximum
 * (`--max-memory=4294967296`, `.github/scripts/wasm-threads.sh`), which a
 * supplied memory may sit anywhere under -- a supplied maximum at or below the
 * declared one instantiates, one above is a `LinkError` (54 cells plus
 * negative controls, both engines, 2026-09-03) -- and each instance walks
 * `LADDER_BYTES` from the top, constructing the largest memory its engine will
 * give it. A desktop gets 4 GiB; a phone gets whatever its engine constructs.
 * The page walks from the top rung; the rasterization worker walks from the
 * rung the page got, which the page hands it on the worker's `name`.
 *
 * **What an instance constructs is a reservation, not a budget.** The
 * budgets keep the per-device POLICY figures below, which this file chose
 * before the ladder existed and which never select what is constructed.
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

/**
 * **The maxima an instance tries, largest first**, in bytes.
 *
 * The top rung is the link flag, so a device whose engine accepts it loses
 * nothing to this mechanism. Each rung below is half the one above, down to
 * 128 MiB: under that the wasm bracket's own cache floors (128 MiB of tile
 * host ceiling plus 56 MiB of loop pool) could not be held at all, and an
 * engine that refuses 128 MiB is one the glue's own fallback is as good an
 * answer for as anything here. Whole pages and strictly descending, held by
 * `tests/linear_memory_ceiling.rs`.
 */
export const LADDER_BYTES = [
  4 * 1024 * 1024 * 1024,
  2 * 1024 * 1024 * 1024,
  1024 * 1024 * 1024,
  512 * 1024 * 1024,
  256 * 1024 * 1024,
  128 * 1024 * 1024,
];

const MIB = 1024 * 1024;

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
 * bound rather than the chosen one, with one `warn` nobody was reading
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
 * **Construct the largest shared memory the engine accepts**, walking
 * `LADDER_BYTES` from the first rung at or below `startBytes`.
 *
 * `construct` is the constructor, overridable so a test can refuse chosen
 * rungs; production passes nothing and gets `new WebAssembly.Memory`.
 *
 * **Only the constructor's throw is caught**, and it is caught because a
 * refusal at construction is exactly the answer being asked for: iOS WebKit
 * throws a `RangeError` for a shared maximum it will not reserve. Nothing
 * else sits inside the `try`, so anything else propagates.
 *
 * Returns `{ memory, bytes, refused }`: the memory and its maximum in bytes,
 * and one `{ bytes, error }` per rung refused on the way down. When every
 * rung at or below `startBytes` refused, `memory` and `bytes` are `null`.
 */
export function constructMemory(pages, startBytes, construct) {
  const make = construct || ((descriptor) => new WebAssembly.Memory(descriptor));
  const refused = [];
  for (const bytes of LADDER_BYTES) {
    if (bytes > startBytes) continue;
    let memory;
    try {
      memory = make({ initial: pages, maximum: bytes / PAGE_BYTES, shared: true });
    } catch (error) {
      refused.push({ bytes, error });
      continue;
    }
    return { memory, bytes, refused };
  }
  return { memory: null, bytes: null, refused };
}

/**
 * The one line each instance prints about its ladder:
 * `squallar: linear memory ladder: constructed N MiB after refusing [a, b]`,
 * every figure in MiB. The rig reads it (`drive.py`,
 * `linear_memory_ladder_verdict`) and holds `N` equal to the `heap max` the
 * app's own `budget state:` line prints for the same instance.
 */
export function ladderLine(bytes, refusedBytes) {
  return (
    "squallar: linear memory ladder: constructed " +
    bytes / MIB +
    " MiB after refusing [" +
    refusedBytes.map((b) => b / MIB).join(", ") +
    "]"
  );
}

/*
 * # The budget POLICY figures -- chosen here, never constructed
 *
 * The ladder above decides how much address space an instance reserves. It
 * decides nothing the application budgets against, and must not: an iPhone
 * 13 Pro constructs 4 GiB and iOS kills the tab near 2.3 GiB, so a watermark
 * judging against the reservation would let the phone die of an OS kill
 * before it ever shed. The budgets, the watermarks and the admission doors
 * keep judging each heap against the per-device figure this file chose before
 * the ladder existed -- 1024/1024 MiB for a desktop's page/worker, 512/256 for
 * a handheld's -- until a measured wall model replaces it. The page hands
 * both policy figures to `start` beside its ladder answer. Nothing passes a
 * policy figure to `constructMemory` or `initWithHeap`, and
 * `tests/linear_memory_ceiling.rs` holds that from the text.
 *
 * Renamed from `heapMaxBytes` / `chooseHeapMaxBytes` when the figures split:
 * those names meant "what gets constructed", and a reader written against
 * them now fails loudly instead of treating a policy as a reservation.
 */

/**
 * **A desktop's page and worker policy.**
 * `squallar_device_profile::constants::WASM_POLICY_HEAP_BYTES`.
 */
export const POLICY_DESKTOP_BYTES = 1024 * 1024 * 1024;

/**
 * **A handheld's PAGE policy.** The page heap holds the overlay pictures
 * (41.7 MB each on the measured `huge` legs), the loop pool and the basemap's
 * host-side tile caches; the wasm bracket's own FLOOR for the last two is
 * 128 MiB of tile host ceiling plus 56 MiB of loop pool
 * (`WASM_TILE_HOST_CEILING_BYTES[0]`, `WASM_LOOP_POOL_FLOOR_BYTES`), so a
 * policy under ~256 MiB would leave the watermark permanently in `Act` with
 * nothing left to shed. 512 MiB clears that floor by 328 MiB and puts the
 * warning line at 384 MiB and the percentage action line at 445 MiB.
 */
export const POLICY_HANDHELD_PAGE_BYTES = 512 * 1024 * 1024;

/**
 * **A handheld's rasterization WORKER policy**, deliberately not the page's
 * figure. The worker holds no cache: it holds the buffers of the jobs in
 * flight, plus the tile lane's parse and style scratch on the same heap. The
 * largest single allocation ever measured there is the MRMS decoder's 98 MB
 * grid; 256 MiB is two of those plus room, and it keeps the PAIR at 768 MiB.
 */
export const POLICY_HANDHELD_WORKER_BYTES = 256 * 1024 * 1024;

/**
 * The `deviceMemory` bucket at or under which a device is treated as a
 * handheld whatever its pointers say -- `DECLARED_RAM_HANDHELD_BYTES`.
 * `navigator.deviceMemory` is a coarse hint, absent in Firefox and WebKit, so
 * it may only LOWER a policy, never raise one.
 */
export const DECLARED_HANDHELD_BYTES = 2 * 1024 * 1024 * 1024;

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
 * The two budget policy figures, in bytes, for a device with these signals.
 * A desktop gets `POLICY_DESKTOP_BYTES` for both; anything else -- a handheld,
 * a device nothing classified, or a desktop-shaped device that declares a
 * handheld's memory -- gets the handheld pair. **Unclassified falls to the
 * smaller on purpose**: an over-small policy costs quality through the levers,
 * an over-large one costs the tab.
 */
export function policyHeapBytes(signals) {
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
    return { page: POLICY_DESKTOP_BYTES, worker: POLICY_DESKTOP_BYTES };
  }
  return { page: POLICY_HANDHELD_PAGE_BYTES, worker: POLICY_HANDHELD_WORKER_BYTES };
}

/**
 * What the page can say about itself, synchronously. Every read is guarded: a
 * browser that will not run a media query answers `null`, which is unknown
 * and never "did not match". There is no override: the choice is remade from
 * the device's own signals at every startup.
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
 * The policy figures this page and the worker it will start are judged
 * against. The page decides for BOTH because a `DedicatedWorkerGlobalScope`
 * has neither `matchMedia` nor `maxTouchPoints`.
 */
export function choosePolicyHeapBytes(global) {
  return policyHeapBytes(pageSignals(global));
}

/** The `name` a rasterization worker is started under; see `heapFromName`. */
export const WORKER_NAME_PREFIX = "squallar-raster:";

/**
 * The byte count out of a worker `name`, or `null` for a name that carries
 * none -- a worker started by a page from a build before this, or opened
 * directly. It is the rung the PAGE constructed, handed over by
 * `worker_port::spawn`, and the worker walks the ladder from there: a
 * `WorkerNavigator` could not say anything better, and a worker never needs a
 * larger wall than the page whose jobs it runs was given.
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
 * Instantiate the module on the largest memory the ladder constructs at or
 * below `startBytes`, and say what the instance actually got.
 *
 * The memory's `initial` is the module's own declared minimum, read off the
 * module's bytes by `declaredMinimumPages` from the same fetch the glue then
 * streams the module from (the block above says why it is read and not
 * stated). `fetchModule` is the fetch, overridable for a test that has no
 * network; production passes nothing and gets `fetch` of `MODULE_PATH`.
 * `construct` is `constructMemory`'s.
 *
 * **Two refusals move down the ladder, and nothing else does.** A rung the
 * constructor refuses is skipped inside `constructMemory`. A constructed
 * memory the glue's instantiation refuses with a `LinkError` -- a module
 * that declares a smaller maximum than the rung, which is what a stale module
 * beside this file would do -- is retried one rung lower on a fresh fetch,
 * because the refused attempt consumed the `Response`. Any other throw is not
 * a statement about the memory and is not retried down the ladder.
 *
 * On success it prints `ladderLine` once at `info` and returns the rung in
 * force. **The fallback is the reason this is a function**: when every rung
 * refused, or the ladder could not run at all (the fetch, the parse, a
 * non-memory failure), it says so once at `warn` -- naming the last rung it
 * tried, which is 128 MiB when the whole ladder refused -- lets the glue build
 * its own memory at the module's declared bound, and returns that bound. A
 * page that boots there is far better than one that does not, and the rig
 * (`drive.py`) fails a leg that prints the warning, so a fallback that fires
 * on every boot is a red row and not a lost ceiling nobody notices.
 */
export async function initWithHeap(init, startBytes, options, fetchModule, construct) {
  const rest = options || {};
  const fetched = async () => {
    const response = await (fetchModule
      ? fetchModule()
      : fetch(new URL(MODULE_PATH, import.meta.url)));
    if (!response.ok) {
      throw new Error(
        "fetching the module answered " + response.status + " " + response.statusText,
      );
    }
    return response;
  };
  const top = startBytes > 0 ? startBytes : LADDER_BYTES[0];
  const refused = [];
  let tried = top;
  let cause = null;
  try {
    let response = await fetched();
    const pages = await declaredMinimumPages(response.clone());
    if (pages === null) throw new Error("the module imports no memory");
    let below = top;
    for (;;) {
      const rung = constructMemory(pages, below, construct);
      for (const r of rung.refused) {
        refused.push(r.bytes);
        tried = r.bytes;
        cause = r.error;
      }
      if (rung.memory === null) {
        if (cause === null) {
          cause = new Error("no rung of the ladder is at or below " + below / MIB + " MiB");
        }
        break;
      }
      tried = rung.bytes;
      try {
        await init({ ...rest, module_or_path: response, memory: rung.memory });
      } catch (e) {
        if (!(e instanceof WebAssembly.LinkError)) throw e;
        refused.push(rung.bytes);
        cause = e;
        below = rung.bytes - PAGE_BYTES;
        response = await fetched();
        continue;
      }
      console.info(ladderLine(rung.bytes, refused));
      return rung.bytes;
    }
  } catch (e) {
    cause = e;
  }
  // Deliberately not phrased as "the engine refused the memory": the cause
  // could have come from the fetch, the parse or the instantiation, and the
  // retry below is what distinguishes them -- if the cause was not the
  // memory it throws again and the caller reports it.
  console.warn(
    "squallar: could not instantiate with a " +
      tried / MIB +
      " MiB linear memory (" +
      String(cause) +
      "); retrying at the module's declared bound",
  );
  await init({ ...rest });
  return LADDER_BYTES[0];
}
