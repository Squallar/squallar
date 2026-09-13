/*
 * Behavioural gates on `heap.js`: its reading of the module's declared memory
 * minimum, and the linear-memory ladder every page and worker walks.
 *
 * ============================================================================
 * Why this is not covered by reading the file
 * ============================================================================
 *
 * `linear_memory_ceiling.rs` holds that `heap.js` states no `initial:` figure,
 * that its ladder starts at the link flag and descends in whole pages, and
 * that the bootstraps start their walks where they should. It cannot hold
 * that the reading is right, that the walk stops at the first rung the engine
 * accepts, or that a refusal the engine really raises moves the walk down:
 * those take an engine. Node has one -- V8, so what it shows is the logic, and
 * what a browser actually constructs is read off every rig leg instead
 * (`drive.py`, `linear_memory_ladder_verdict`).
 *
 * ============================================================================
 * The fixture
 * ============================================================================
 *
 * A module built here byte by byte: magic, version, an empty type section, a
 * custom section in front of the imports (allowed anywhere, and the parser
 * must skip it), and an import section with a function import ahead of the
 * memory import so the per-kind skipping is exercised. The memory import is
 * `shared` with a maximum, the shape the threads build links.
 */

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  LADDER_BYTES,
  MODULE_PATH,
  PAGE_BYTES,
  POLICY_DESKTOP_BYTES,
  POLICY_HANDHELD_PAGE_BYTES,
  POLICY_HANDHELD_WORKER_BYTES,
  choosePolicyHeapBytes,
  classifyFormFactor,
  constructMemory,
  declaredMinimumPages,
  heapFromName,
  initWithHeap,
  ladderLine,
  policyHeapBytes,
  readDeclaredMinimum,
} from "../heap.js";

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;
/** wasm32's architectural maximum, in pages: what the module is linked at. */
const LINKED_PAGES = 65536;

/** Unsigned LEB128. */
function leb(n) {
  const out = [];
  do {
    let b = n & 0x7f;
    n = Math.floor(n / 128);
    if (n !== 0) b |= 0x80;
    out.push(b);
  } while (n !== 0);
  return out;
}

function name(s) {
  const bytes = Array.from(new TextEncoder().encode(s));
  return [...leb(bytes.length), ...bytes];
}

function section(id, payload) {
  return [id, ...leb(payload.length), ...payload];
}

/**
 * A module importing `env.memory` with `[min, max]` shared limits, and one
 * function import ahead of it. `customFirst` puts a custom section before the
 * type section as well.
 */
function moduleImportingMemory(min, max, { customFirst = true } = {}) {
  const types = section(1, [
    ...leb(1),
    0x60, // func type
    ...leb(0), // no params
    ...leb(0), // no results
  ]);
  const custom = section(0, [...name("squallar.test"), 1, 2, 3]);
  const memoryImport = [
    ...name("env"),
    ...name("memory"),
    2, // kind: memory
    0x03, // flags: has maximum, shared
    ...leb(min),
    ...leb(max),
  ];
  const funcImport = [...name("env"), ...name("f"), 0, ...leb(0)];
  const imports = section(2, [...leb(2), ...funcImport, ...memoryImport]);
  const bytes = [
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...(customFirst ? custom : []),
    ...types,
    ...imports,
  ];
  return new Uint8Array(bytes);
}

/** A module that imports nothing and defines its own memory. */
function moduleWithOwnMemory() {
  const memory = section(5, [...leb(1), 0x00, ...leb(1)]);
  return new Uint8Array([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...memory,
  ]);
}

function wasmResponse(bytes) {
  return new Response(bytes, {
    status: 200,
    headers: { "Content-Type": "application/wasm" },
  });
}

/**
 * A constructor that refuses the rungs named in `refuseMib` the way iOS
 * WebKit refuses a shared maximum it will not reserve -- a `RangeError` at
 * construction -- builds a real memory for every other rung, and records
 * every descriptor it was asked for.
 */
function refusingConstructor(refuseMib, asked) {
  return (descriptor) => {
    asked.push(descriptor);
    const mib = (descriptor.maximum * PAGE_BYTES) / MIB;
    if (refuseMib.includes(mib)) throw new RangeError("stub refusal at " + mib + " MiB");
    return new WebAssembly.Memory(descriptor);
  };
}

describe("readDeclaredMinimum", () => {
  it("reads the memory import's minimum past a custom section and a function import", () => {
    const read = readDeclaredMinimum(moduleImportingMemory(66, LINKED_PAGES));
    assert.deepEqual(read, { kind: "min", pages: 66 });
  });

  it("reads a minimum wider than one LEB128 byte", () => {
    const read = readDeclaredMinimum(moduleImportingMemory(300, LINKED_PAGES));
    assert.deepEqual(read, { kind: "min", pages: 300 });
  });

  it("asks for more bytes at every truncation short of the answer", () => {
    const whole = moduleImportingMemory(66, 16384);
    // The memory import's limits are the last bytes; every prefix that stops
    // before the minimum's LEB byte is "more", never a wrong number.
    for (let n = 0; n < whole.length - 1; n++) {
      const read = readDeclaredMinimum(whole.subarray(0, n));
      assert.equal(read.kind, "more", `prefix of ${n} bytes read as ${JSON.stringify(read)}`);
    }
  });

  it("answers none for a module that imports no memory", () => {
    assert.deepEqual(readDeclaredMinimum(moduleWithOwnMemory()), { kind: "none" });
  });

  it("answers none for bytes that are not a wasm module", () => {
    assert.deepEqual(
      readDeclaredMinimum(new TextEncoder().encode("<!doctype html>")),
      { kind: "none" },
    );
  });
});

describe("declaredMinimumPages", () => {
  it("reads the minimum off a Response body delivered in small chunks, and cancels the rest", async () => {
    const whole = moduleImportingMemory(66, LINKED_PAGES);
    let pulled = 0;
    let cancelled = false;
    const stream = new ReadableStream({
      pull(controller) {
        if (pulled >= whole.length) {
          controller.close();
          return;
        }
        controller.enqueue(whole.subarray(pulled, pulled + 7));
        pulled += 7;
      },
      cancel() {
        cancelled = true;
      },
    });
    const pages = await declaredMinimumPages(new Response(stream));
    assert.equal(pages, 66);
    assert.ok(cancelled, "the parse branch was not cancelled once the limits were read");
  });

  it("answers null for a module importing no memory", async () => {
    assert.equal(await declaredMinimumPages(wasmResponse(moduleWithOwnMemory())), null);
  });

  it("throws on a body that ends before the import section", async () => {
    const cut = moduleImportingMemory(66, LINKED_PAGES).subarray(0, 12);
    await assert.rejects(() => declaredMinimumPages(wasmResponse(cut)), /ended before/);
  });
});

describe("the import rules the ladder serves", () => {
  /** Instantiate `bytes` with a supplied memory of `initial` and `maximum` pages. */
  async function instantiateWith(bytes, initial, maximum) {
    const memory = new WebAssembly.Memory({ initial, maximum, shared: true });
    return WebAssembly.instantiate(bytes, { env: { memory, f: () => {} } });
  }

  it("a memory at the declared minimum links, one page under is a LinkError", async () => {
    const bytes = moduleImportingMemory(66, 16384);
    const pages = await declaredMinimumPages(wasmResponse(bytes));
    await instantiateWith(bytes, pages, 16384);
    await assert.rejects(() => instantiateWith(bytes, pages - 1, 16384), WebAssembly.LinkError);
  });

  it("a supplied maximum at or below the declared one links, one above it is a LinkError", async () => {
    const bytes = moduleImportingMemory(66, 16384);
    await instantiateWith(bytes, 66, 8192);
    await instantiateWith(bytes, 66, 16384);
    await assert.rejects(() => instantiateWith(bytes, 66, 32768), WebAssembly.LinkError);
  });
});

describe("LADDER_BYTES", () => {
  it("starts at wasm32's 65,536 pages and descends strictly, in whole pages, to 128 MiB", () => {
    assert.equal(LADDER_BYTES[0], LINKED_PAGES * PAGE_BYTES);
    for (const rung of LADDER_BYTES) assert.equal(rung % PAGE_BYTES, 0, `${rung} is not whole pages`);
    for (let i = 1; i < LADDER_BYTES.length; i++) {
      assert.ok(LADDER_BYTES[i] < LADDER_BYTES[i - 1], `rung ${i} does not descend`);
    }
    assert.equal(LADDER_BYTES[LADDER_BYTES.length - 1], 128 * MIB);
  });
});

describe("constructMemory", () => {
  it("takes the top rung when nothing refuses, and asks for it once", () => {
    const asked = [];
    const got = constructMemory(66, LADDER_BYTES[0], refusingConstructor([], asked));
    assert.equal(got.bytes, 4 * GIB);
    assert.ok(got.memory instanceof WebAssembly.Memory);
    assert.deepEqual(got.refused, []);
    assert.deepEqual(asked, [{ initial: 66, maximum: LINKED_PAGES, shared: true }]);
  });

  it("walks past every rung the constructor refuses, and names each refusal", () => {
    const asked = [];
    const got = constructMemory(66, LADDER_BYTES[0], refusingConstructor([4096, 2048], asked));
    assert.equal(got.bytes, GIB);
    assert.deepEqual(got.refused.map((r) => r.bytes), [4 * GIB, 2 * GIB]);
    assert.ok(got.refused.every((r) => r.error instanceof RangeError));
    assert.deepEqual(asked.map((d) => d.maximum), [65536, 32768, 16384]);
  });

  it("starts at the first rung at or below the start figure and never asks above it", () => {
    const asked = [];
    const got = constructMemory(66, GIB, refusingConstructor([], asked));
    assert.equal(got.bytes, GIB);
    assert.deepEqual(asked.map((d) => d.maximum), [16384]);
    // A start figure between rungs starts at the rung below it.
    const between = [];
    assert.equal(constructMemory(66, 3 * GIB, refusingConstructor([], between)).bytes, 2 * GIB);
    assert.deepEqual(between.map((d) => d.maximum), [32768]);
  });

  it("answers null, with every rung named, when the whole ladder refuses", () => {
    const asked = [];
    const all = LADDER_BYTES.map((b) => b / MIB);
    const got = constructMemory(66, LADDER_BYTES[0], refusingConstructor(all, asked));
    assert.equal(got.memory, null);
    assert.equal(got.bytes, null);
    assert.deepEqual(got.refused.map((r) => r.bytes), LADDER_BYTES);
    assert.equal(asked.length, LADDER_BYTES.length);
  });

  it("constructs a REAL 4 GiB shared memory in this engine, on the default constructor", async () => {
    const got = constructMemory(66, LADDER_BYTES[0]);
    assert.equal(got.bytes, 4 * GIB, `refused: ${got.refused.map((r) => String(r.error))}`);
    assert.ok(got.memory.buffer instanceof SharedArrayBuffer);
    assert.equal(got.memory.buffer.byteLength, 66 * PAGE_BYTES);
    // And a module linked at 65,536 pages instantiates on it.
    const bytes = moduleImportingMemory(66, LINKED_PAGES);
    await WebAssembly.instantiate(bytes, { env: { memory: got.memory, f: () => {} } });
  });
});

describe("heapFromName", () => {
  it("reads the page's rung off a worker name, and nothing off anything else", () => {
    assert.equal(heapFromName("squallar-raster:" + 4 * GIB), 4 * GIB);
    assert.equal(heapFromName("squallar-raster:" + GIB), GIB);
    assert.equal(heapFromName("squallar-raster:12345"), null, "not whole pages");
    assert.equal(heapFromName("squallar-raster:"), null);
    assert.equal(heapFromName(""), null);
    assert.equal(heapFromName(undefined), null);
  });
});

describe("initWithHeap", () => {
  /**
   * A stand-in for the glue's `init`: instantiates the module it is handed
   * the way the glue does -- from the `Response`, with the supplied memory
   * under the import the module names -- and records what it was given. An
   * engine refusal is the engine's own: a supplied memory the module's
   * declared limits do not admit raises a real `LinkError` here.
   */
  function fakeInit(bytes, seen) {
    return async function init(options) {
      seen.push(options);
      const imports = { env: { f: () => {} } };
      if (options && options.memory) imports.env.memory = options.memory;
      else imports.env.memory = new WebAssembly.Memory({ initial: 66, maximum: 16384, shared: true });
      const source = options && options.module_or_path;
      const body = source instanceof Response ? await source.arrayBuffer() : bytes;
      await WebAssembly.instantiate(body, imports);
    };
  }

  /** Run `body` with `console.info` and `console.warn` captured. */
  async function capturing(body) {
    const info = [];
    const warn = [];
    const realInfo = console.info;
    const realWarn = console.warn;
    console.info = (...args) => info.push(args.join(" "));
    console.warn = (...args) => warn.push(args.join(" "));
    try {
      await body();
    } finally {
      console.info = realInfo;
      console.warn = realWarn;
    }
    return { info, warn };
  }

  it("instantiates on the top rung from ONE fetch, and prints the ladder line once", async () => {
    const bytes = moduleImportingMemory(66, LINKED_PAGES);
    const seen = [];
    let fetches = 0;
    const printed = await capturing(async () => {
      const got = await initWithHeap(fakeInit(bytes, seen), LADDER_BYTES[0], undefined, () => {
        fetches += 1;
        return Promise.resolve(wasmResponse(bytes));
      });
      assert.equal(got, 4 * GIB, "the top rung was not the one in force");
    });
    assert.equal(fetches, 1, "the module was fetched more than once");
    assert.equal(seen.length, 1, "init was called more than once");
    assert.ok(seen[0].memory instanceof WebAssembly.Memory);
    assert.equal(seen[0].memory.buffer.byteLength, 66 * PAGE_BYTES, "initial is not the declared minimum");
    assert.ok(seen[0].module_or_path instanceof Response, "the glue was not handed the fetched Response");
    assert.deepEqual(printed.info, ["squallar: linear memory ladder: constructed 4096 MiB after refusing []"]);
    assert.deepEqual(printed.warn, []);
  });

  it("moves down past constructor refusals, and the one line names every refusal", async () => {
    const bytes = moduleImportingMemory(66, LINKED_PAGES);
    const seen = [];
    const asked = [];
    const printed = await capturing(async () => {
      const got = await initWithHeap(
        fakeInit(bytes, seen),
        LADDER_BYTES[0],
        undefined,
        () => Promise.resolve(wasmResponse(bytes)),
        refusingConstructor([4096, 2048], asked),
      );
      assert.equal(got, GIB);
    });
    assert.equal(seen.length, 1, "a constructor refusal reached init");
    assert.deepEqual(asked.map((d) => d.maximum), [65536, 32768, 16384]);
    assert.deepEqual(printed.info, [
      "squallar: linear memory ladder: constructed 1024 MiB after refusing [4096, 2048]",
    ]);
    assert.deepEqual(printed.warn, []);
  });

  it("a worker's walk starts at the page's rung, not at the top", async () => {
    const bytes = moduleImportingMemory(66, LINKED_PAGES);
    const asked = [];
    const printed = await capturing(async () => {
      const got = await initWithHeap(
        fakeInit(bytes, []),
        heapFromName("squallar-raster:" + 512 * MIB),
        undefined,
        () => Promise.resolve(wasmResponse(bytes)),
        refusingConstructor([], asked),
      );
      assert.equal(got, 512 * MIB);
    });
    assert.deepEqual(asked.map((d) => d.maximum), [8192]);
    assert.deepEqual(printed.info, ["squallar: linear memory ladder: constructed 512 MiB after refusing []"]);
  });

  it("an engine LinkError at instantiation retries one rung lower, on a fresh fetch", async () => {
    // A module declaring 1 GiB beside this file -- a stale module. The engine
    // refuses the 4 GiB and 2 GiB memories at link time, for real.
    const bytes = moduleImportingMemory(66, 16384);
    const seen = [];
    let fetches = 0;
    const printed = await capturing(async () => {
      const got = await initWithHeap(fakeInit(bytes, seen), LADDER_BYTES[0], undefined, () => {
        fetches += 1;
        return Promise.resolve(wasmResponse(bytes));
      });
      assert.equal(got, GIB);
    });
    assert.equal(seen.length, 3, "init did not run once per rung tried");
    assert.equal(fetches, 3, "a retry reused a consumed Response instead of fetching");
    assert.deepEqual(printed.info, [
      "squallar: linear memory ladder: constructed 1024 MiB after refusing [4096, 2048]",
    ]);
    assert.deepEqual(printed.warn, []);
  });

  it("any other throw from init is not walked down the ladder: one warn, then the glue's own memory", async () => {
    const bytes = moduleImportingMemory(66, LINKED_PAGES);
    const seen = [];
    const real = fakeInit(bytes, seen);
    const broken = async (options) => {
      if (options && options.memory) {
        seen.push(options);
        throw new TypeError("not a memory problem");
      }
      return real(options);
    };
    const printed = await capturing(async () => {
      const got = await initWithHeap(broken, LADDER_BYTES[0], undefined, () =>
        Promise.resolve(wasmResponse(bytes)),
      );
      assert.equal(got, LADDER_BYTES[0], "the fallback's figure is not the module's declared bound");
    });
    assert.equal(seen.length, 2, "the throw was retried at a lower rung, or the fallback did not run");
    assert.equal(seen[1].memory, undefined, "the fallback supplied a memory");
    assert.deepEqual(printed.info, [], "a ladder line was printed for an instance that took no rung");
    assert.equal(printed.warn.length, 1);
    assert.match(printed.warn[0], /^squallar: could not instantiate with a 4096 MiB linear memory \(TypeError: not a memory problem\)/);
  });

  it("falls back to the glue's own memory, once and at warn, when the module cannot be fetched", async () => {
    const bytes = moduleImportingMemory(66, LINKED_PAGES);
    const seen = [];
    const printed = await capturing(async () => {
      const got = await initWithHeap(fakeInit(bytes, seen), 512 * MIB, undefined, () =>
        Promise.resolve(new Response(null, { status: 404, statusText: "Not Found" })),
      );
      assert.equal(got, LADDER_BYTES[0]);
    });
    // The first attempt died at the fetch, before `init`; only the fallback
    // reached it, and the fallback supplies no memory of its own.
    assert.equal(seen.length, 1, "init ran a different number of times than the fallback alone");
    assert.equal(seen[0].memory, undefined, "the fallback supplied a memory");
    assert.deepEqual(printed.info, []);
    assert.equal(printed.warn.length, 1);
    assert.match(printed.warn[0], /^squallar: could not instantiate with a 512 MiB linear memory \(/);
    assert.match(printed.warn[0], /404 Not Found/);
  });

  it("the warn survives only when even the 128 MiB rung is refused, and names that rung", async () => {
    const bytes = moduleImportingMemory(66, LINKED_PAGES);
    const seen = [];
    const asked = [];
    const printed = await capturing(async () => {
      const got = await initWithHeap(
        fakeInit(bytes, seen),
        LADDER_BYTES[0],
        undefined,
        () => Promise.resolve(wasmResponse(bytes)),
        refusingConstructor(LADDER_BYTES.map((b) => b / MIB), asked),
      );
      assert.equal(got, LADDER_BYTES[0]);
    });
    assert.equal(asked.length, LADDER_BYTES.length, "the walk stopped before the bottom rung");
    assert.equal(seen.length, 1, "init ran before the fallback");
    assert.equal(seen[0].memory, undefined, "the fallback supplied a memory");
    assert.deepEqual(printed.info, []);
    assert.equal(printed.warn.length, 1);
    assert.match(printed.warn[0], /^squallar: could not instantiate with a 128 MiB linear memory \(RangeError: stub refusal at 128 MiB\)/);
  });
});

describe("ladderLine", () => {
  it("states the rung and every refusal in MiB", () => {
    assert.equal(
      ladderLine(256 * MIB, [4 * GIB, 2 * GIB, GIB, 512 * MIB]),
      "squallar: linear memory ladder: constructed 256 MiB after refusing [4096, 2048, 1024, 512]",
    );
  });
});

describe("the budget policy figures", () => {
  const signals = (pointerCoarse, anyPointerFine, maxTouchPoints, deviceMemoryBytes = null) => ({
    pointerCoarse,
    anyPointerFine,
    maxTouchPoints,
    deviceMemoryBytes,
  });

  it("a desktop is judged against 1024/1024 MiB and a handheld against 512/256", () => {
    assert.equal(POLICY_DESKTOP_BYTES, GIB);
    assert.deepEqual(policyHeapBytes(signals(false, true, 0)), { page: GIB, worker: GIB });
    assert.deepEqual(policyHeapBytes(signals(true, false, 5)), {
      page: POLICY_HANDHELD_PAGE_BYTES,
      worker: POLICY_HANDHELD_WORKER_BYTES,
    });
    assert.equal(POLICY_HANDHELD_PAGE_BYTES, 512 * MIB);
    assert.equal(POLICY_HANDHELD_WORKER_BYTES, 256 * MIB);
  });

  it("an unclassified device takes the smaller pair, and a declared small memory only lowers", () => {
    assert.equal(classifyFormFactor(null, null, null), null);
    assert.deepEqual(policyHeapBytes(signals(null, null, null)), { page: 512 * MIB, worker: 256 * MIB });
    assert.deepEqual(policyHeapBytes(signals(false, true, 0, 2 * GIB)), { page: 512 * MIB, worker: 256 * MIB });
    assert.deepEqual(policyHeapBytes(signals(true, false, 5, 8 * GIB)), { page: 512 * MIB, worker: 256 * MIB });
  });

  it("reads the page's own signals, and a global with no media answers unknown", () => {
    const desktop = {
      matchMedia: (query) => ({ matches: query === "(any-pointer: fine)" }),
      navigator: { maxTouchPoints: 0 },
    };
    assert.deepEqual(choosePolicyHeapBytes(desktop), { page: GIB, worker: GIB });
    assert.deepEqual(choosePolicyHeapBytes({ navigator: {} }), { page: 512 * MIB, worker: 256 * MIB });
  });

  it("no policy figure is the top rung: a policy never stands in for a reservation", () => {
    for (const figure of [POLICY_DESKTOP_BYTES, POLICY_HANDHELD_PAGE_BYTES, POLICY_HANDHELD_WORKER_BYTES]) {
      assert.ok(figure < LADDER_BYTES[0], `${figure} is not below the link flag`);
    }
  });
});

describe("MODULE_PATH", () => {
  it("names the module beside the glue, the way the glue resolves it", () => {
    assert.equal(MODULE_PATH, "./pkg/squallar_web_bg.wasm");
  });
});
