/*
 * Behavioural gates on `heap.js`'s reading of the module's declared memory
 * minimum -- the figure this file used to hold as a hand-copied constant.
 *
 * ============================================================================
 * Why this is not covered by reading the file
 * ============================================================================
 *
 * `linear_memory_ceiling.rs` holds that `heap.js` no longer STATES an
 * `initial:` figure and does read one off the module. It cannot hold that the
 * reading is right: that takes a wasm binary with a known memory import and
 * the parser run over it. Nor can it hold the rule the reading serves -- that
 * a supplied memory one page under the declared minimum is a `LinkError` and
 * one at it is not -- which is the engine's rule and needs an engine. Node has
 * one.
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
  MODULE_PATH,
  PAGE_BYTES,
  DESKTOP_PAGE_BYTES,
  declaredMinimumPages,
  initWithHeap,
  readDeclaredMinimum,
} from "../heap.js";

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

describe("readDeclaredMinimum", () => {
  it("reads the memory import's minimum past a custom section and a function import", () => {
    const read = readDeclaredMinimum(moduleImportingMemory(66, 16384));
    assert.deepEqual(read, { kind: "min", pages: 66 });
  });

  it("reads a minimum wider than one LEB128 byte", () => {
    const read = readDeclaredMinimum(moduleImportingMemory(300, 16384));
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
    const whole = moduleImportingMemory(66, 16384);
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
    const cut = moduleImportingMemory(66, 16384).subarray(0, 12);
    await assert.rejects(() => declaredMinimumPages(wasmResponse(cut)), /ended before/);
  });
});

describe("the import rule the reading serves", () => {
  /** Instantiate `bytes` with a supplied memory of `initial` pages. */
  async function instantiateWith(bytes, initial) {
    const memory = new WebAssembly.Memory({ initial, maximum: 16384, shared: true });
    return WebAssembly.instantiate(bytes, { env: { memory, f: () => {} } });
  }

  it("a memory at the declared minimum links, one page under is a LinkError", async () => {
    const bytes = moduleImportingMemory(66, 16384);
    const pages = (await declaredMinimumPages(wasmResponse(bytes)));
    await instantiateWith(bytes, pages);
    await assert.rejects(() => instantiateWith(bytes, pages - 1), WebAssembly.LinkError);
  });
});

describe("initWithHeap", () => {
  /**
   * A stand-in for the glue's `init`: instantiates the module it is handed
   * the way the glue does -- from the `Response`, with the supplied memory
   * under the import the module names -- and records what it was given.
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

  it("instantiates with a memory at the module's declared minimum and the asked maximum, from ONE fetch", async () => {
    const bytes = moduleImportingMemory(66, 16384);
    const seen = [];
    let fetches = 0;
    const maxBytes = 512 * 1024 * 1024;
    const got = await initWithHeap(fakeInit(bytes, seen), maxBytes, undefined, () => {
      fetches += 1;
      return Promise.resolve(wasmResponse(bytes));
    });
    assert.equal(got, maxBytes, "the supplied memory was not the one in force");
    assert.equal(fetches, 1, "the module was fetched more than once");
    assert.equal(seen.length, 1, "init was called more than once: the fallback ran");
    assert.ok(seen[0].memory instanceof WebAssembly.Memory);
    assert.equal(seen[0].memory.buffer.byteLength, 66 * PAGE_BYTES, "initial is not the declared minimum");
    assert.ok(seen[0].module_or_path instanceof Response, "the glue was not handed the fetched Response");
  });

  /** Run `body` with `console.warn` captured; returns what it printed. */
  async function capturingWarn(body) {
    const warned = [];
    const realWarn = console.warn;
    console.warn = (...args) => warned.push(args.join(" "));
    try {
      await body();
    } finally {
      console.warn = realWarn;
    }
    return warned;
  }

  it("falls back to the glue's own memory, once and at warn, when the module cannot be fetched", async () => {
    const bytes = moduleImportingMemory(66, 16384);
    const seen = [];
    const warned = await capturingWarn(async () => {
      const got = await initWithHeap(fakeInit(bytes, seen), 512 * 1024 * 1024, undefined, () =>
        Promise.resolve(new Response(null, { status: 404, statusText: "Not Found" })),
      );
      assert.equal(got, DESKTOP_PAGE_BYTES);
    });
    // The first attempt died at the fetch, before `init`; only the fallback
    // reached it, and the fallback supplies no memory of its own.
    assert.equal(seen.length, 1, "init ran a different number of times than the fallback alone");
    assert.equal(seen[0].memory, undefined, "the fallback supplied a memory");
    assert.equal(warned.length, 1);
    assert.match(warned[0], /^squallar: could not instantiate with a 512 MiB linear memory \(/);
    assert.match(warned[0], /404 Not Found/);
  });

  it("falls back, once and at warn, when the engine refuses the supplied memory", async () => {
    const bytes = moduleImportingMemory(66, 16384);
    const seen = [];
    const real = fakeInit(bytes, seen);
    // The glue as it behaves on a module whose minimum is above what was
    // supplied: the first `init` links and fails, the retry links clean.
    const refusing = async (options) => {
      if (options && options.memory) {
        seen.push(options);
        throw new WebAssembly.LinkError("imported Memory with incompatible size");
      }
      return real(options);
    };
    const warned = await capturingWarn(async () => {
      const got = await initWithHeap(refusing, 512 * 1024 * 1024, undefined, () =>
        Promise.resolve(wasmResponse(bytes)),
      );
      assert.equal(got, DESKTOP_PAGE_BYTES);
    });
    assert.equal(seen.length, 2, "the fallback init did not run");
    assert.ok(seen[0].memory instanceof WebAssembly.Memory);
    assert.equal(seen[1].memory, undefined, "the fallback supplied a memory");
    assert.equal(warned.length, 1);
    assert.match(warned[0], /could not instantiate with a 512 MiB linear memory \(LinkError/);
  });
});

describe("MODULE_PATH", () => {
  it("names the module beside the glue, the way the glue resolves it", () => {
    assert.equal(MODULE_PATH, "./pkg/squallar_web_bg.wasm");
  });
});
