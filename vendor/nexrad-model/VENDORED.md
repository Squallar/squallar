# nexrad-model, vendored

This directory is a copy of a crates.io crate that this workspace maintains
locally. It is not our code. Everything in it is upstream's except what is
listed under [Local changes](#local-changes) below, and keeping that list short
and true is the whole point of the file — it is what makes the directory
reviewable, and what lets a later upstream release be adopted by re-applying a
delta somebody can still read.

**These changes are not going upstream.** Same decision as the other vendored
nexrad crates (2026-08-12): upstream will not carry this delta, so this
directory is where the decomposable scan model lives, indefinitely. A later
upstream release is still worth adopting for everything else it brings, and
re-applying this delta onto it is the job the short list below exists to make
possible.

## Provenance

| Field | Value |
| --- | --- |
| Package | `nexrad-model` |
| Version | `1.0.0-rc.2` — what this workspace already pinned as a registry dependency |
| Source | crates.io, `sha256:4e2e15c56d3f5869b78eef326ca33debd6d144a4a3348ff4829e8579c6d2a9ea` |
| Unpacked from | `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/nexrad-model-1.0.0-rc.2/` |
| Upstream repo | <https://github.com/danielway/nexrad>, subdirectory `nexrad-model` |
| Upstream commit | `3d9ef8d7d1ecaa24795c53300fbdd5825852f1aa` (from the tarball's `.cargo_vcs_info.json`) |
| Author | Daniel Way `<contact@danieldway.com>` |
| License | MIT — full text in `LICENSE` next to this file |

The packaged tarball ships **no** license file, only `license = "MIT"` in the
manifest. `LICENSE` here is the same reconstruction the other vendored nexrad
crates carry — the upstream repository's own `LICENSE`, copyright line
included — because "MIT" in a metadata field is a declaration and not the
notice the license requires us to redistribute.

## Why this exists

The published nexrad-model seals a `Scan`'s sweeps behind `&[Sweep]` borrows:
`Scan::new` takes `Vec<Sweep>` by value, and no method gives them back owned.

That seal is what made a decoded volume's teardown indivisible. A decoded
volume is a measured 48.88 MiB median / 74.63 MiB maximum of live heap across
thousands of per-radial buffers, over 208 real archive volumes decoded under a
counting global allocator, and on wasm every discarded volume is freed *on the
page thread*
by `squallar_worker::offload::drain_deferred_drops`, whose budget paces turns
but cannot split a payload: the drain's real spend per frame is its budget plus
one whole payload. With the volume indivisible, that overshoot is the whole
volume.

`Scan::into_sweeps` is the seam that fixes the shape: the eviction path hands
each sweep to the drop queue as its own payload, so one drain turn frees one
sweep rather than one volume.

## Local changes

This is the complete list. `diff -rq` against the unpacked registry copy must
produce exactly these entries and nothing else.

### Removed — packaging residue

| Path | Why |
| --- | --- |
| `default_*.profraw` | Coverage output from an unrelated local run that landed in the registry checkout. Never part of the crate. |
| `.cargo-ok` | Cargo's own extraction marker. |
| `.cargo_vcs_info.json` | Recorded in the provenance table above instead. |
| `Cargo.toml.orig` | Upstream's pre-normalisation manifest, which refers to workspace inheritance that does not exist here. |
| `Cargo.lock` | A packaged crate's lockfile is inert; this workspace's root `Cargo.lock` is the one that resolves anything. |

### Removed — targets that cannot compile here

| Path | Why |
| --- | --- |
| `tests/fixture_snapshots.rs` + its `[[test]]` block | `include_bytes!("../../tests/fixtures/…")` — five archive volumes that exist only in upstream's monorepo checkout. Cannot compile here at all. |
| `tests/scan_snapshot.rs` + its `[[test]]` block | `include_bytes!("../../downloads/…")` — a file that exists only after upstream's download step. Same. |
| `tests/snapshots/` | The insta snapshots of the two deleted tests; nothing else reads them. |
| dev-dependencies `insta`, `nexrad-data`, `nexrad-decode`, `hex`, `sha2` | Used only by the two deleted tests. (`hex` and `sha2` remain as the lib's own runtime dependencies, untouched.) |

`tests/model_types.rs` — upstream's pure in-crate unit suite, 20+ tests over
`Scan`/`Sweep`/`Radial`/`MomentData` construction and `Sweep::merge` — is kept
verbatim and runs as a workspace member. It is the behaviour pin on the model
the added method must leave untouched.

### Added

| Path | What |
| --- | --- |
| `src/data/scan.rs` | Two methods. `Scan::into_sweeps(self) -> Vec<Sweep>` — the only owned decomposition of a scan. It moves the field `Scan::new` took; no representation changes. `Scan::sweeps_capacity(&self) -> usize` — reads `Vec::capacity` on the same field, because `sweeps()` hands out a slice and a slice cannot report spare capacity. Both marked `LOCAL CHANGE` at their definitions. |
| `src/data/sweep.rs` | One method, `Sweep::radials_capacity(&self) -> usize` — `Vec::capacity` on the radial vector, for the same reason and marked the same way. Reading it is what lets `squallar_radar::scan_size::scan_bytes` charge what the allocator holds rather than what the slice's length implies; the spare is ~42 % of the length in real decoded volumes. |
| `src/data/moment.rs` | Three methods, one rule, and one added public type — `GateBuffer`, whose reason is under [Changed](#the-gate-buffer-shares--srcdatamomentrs). The three methods: `MomentDataBlock::without_values(&self)` returns the same block with `values` emptied and `gate_count`, `first_gate_range`, `gate_interval`, `data_word_size`, `scale` and `offset` carried over unchanged; `MomentData::without_values` and `CFPMomentData::without_values` expose it on the two public wrappers. All three marked `LOCAL CHANGE` at their definitions. No representation change — it constructs the struct the decoder constructs, with one field empty. Written on the block rather than rebuilt through the public accessors because `first_gate_range` and `gate_interval` are stored as fixed-point `u16` and read back as `f64` kilometres, so a round trip would not reproduce the block; and because `gate_count` is HASHED by `squallar_radar::sampler::ladder_fingerprint`, so a copy that changed it would silently move a cross-section's re-cut key. It is what lets `squallar_radar::skeleton::VolumeSkeleton` keep a volume's structure resident while releasing the arrays, which on a VCP-212-shaped volume is 3.18 % of the allocator cost of the whole. |
| `Cargo.toml` | The `[lints]` tables every vendored crate here carries, so the clippy fix-bot cannot rewrite upstream source — see the comment above them and vendor/nexrad-decode/Cargo.toml for the mechanism. Also the two `[[test]]` blocks and five dev-dependencies of the deleted tests, removed. |
| `LICENSE`, `VENDORED.md` | This file and the license notice. |

### Changed — source

#### The gate buffer shares — `src/data/moment.rs`

`MomentDataBlock::values` was `BinaryData<Vec<u8>>`; it is now
`BinaryData<GateBuffer>`, where `GateBuffer` is an added newtype over
`Arc<Vec<u8>>`.

```rust
-    values: BinaryData<Vec<u8>>,
+    values: BinaryData<GateBuffer>,
```

**Why.** A decoded volume is **95.9 % per-(ray, moment) gate buffers**, ~32,400
of them at one allocator block apiece, and cloning a `Sweep` deep-copied every
one. `squallar_radar::chunks::VolumeAssembler::snapshot` does that clone on
every rebuild where it is not the volume's last owner, which on a 420 s
six-site leg (2026-09-09) was **156 of 156 rebuilds — 4,744.9 MiB across
2,944,230 blocks, a mean 30.4 MiB and 18,873 blocks apiece**, on the poller's
thread. The two owners that make `Arc::try_unwrap` fail are both legitimate and
both overlap the rebuild by construction (the bridge copy held precisely during
the away window the rebuild runs in, and the still inventory), so ownership
cannot be rearranged to fix it. Making the clone cheap is the remaining move.

**Nothing rounds and nothing is lost.** The bytes are not touched: the `Vec`'s
three words move into the `Arc`'s block and the gate bytes stay exactly where
the decoder put them. `raw_values()` still returns `&[u8]` over the same
memory, `raw_gate_values()` still `chunks_exact`es it, and `MomentData::iter`
still decodes lazily from the raw slice. Upstream's `tests/model_types.rs` — the
pin this delta must leave intact — passes unedited.

**Sharing is safe because the buffer is immutable by construction, and that is
checked rather than assumed.** `values` is a private field of a `pub(crate)`
struct. It is **written in exactly two places**: `from_fixed_point`, which
constructs it, and `without_values`, which replaces the whole field with
`GateBuffer::empty`. It is **read in exactly two**: `raw_values` and
`raw_gate_values`. There is no `&mut` path to a moment anywhere — `Radial`
hands out `Option<&MomentData>` and has no `_mut` accessor, and neither do
`Sweep` or `Scan` — so no caller inside this crate or outside it can mutate a
gate buffer today, and `Arc` cannot change semantics that nothing exercises.
`BinaryData`'s blanket `DerefMut` is reachable only from inside this module,
which is why the audit is a file-scope one and not a workspace-wide one.
`Arc` rather than `Rc` because a volume is decoded on a runtime worker and read
on the frame thread; the buffer is never written after construction, so
concurrent readers of a shared buffer race over nothing.

**Not `Arc<[u8]>`.** `Arc<[u8]>::from(Vec<u8>)` copies the bytes into a fresh
allocation, which would put a whole volume's memcpy on the decode path to save
24 bytes a buffer. `Arc<Vec<u8>>` keeps `from_fixed_point(… , values: Vec<u8>)`
free of a copy, and keeps its signature, so all 140 call sites and both
decoders are untouched.

**What it costs, measured.** One extra small block per non-empty gate buffer at
decode: two `usize` counts plus the `Vec` header, **40 B on a 64-bit target and
20 B on wasm32**. `squallar_radar::scan_size::GATE_BUFFER_SHARE_BYTES` is that
term and the module prices it.

Most of it is paid back by `size_of::<Radial>()` falling from **312 B to
200 B** — a moment stores an 8-byte pointer where it stored a 24-byte `Vec`,
and a `Radial` holds seven inline — so every radial slot a sweep's vector
holds, its ~42 % spare capacity included, got 112 B cheaper. Against a counting
allocator over 8 real archive volumes (release, 2026-09-09) the net is
**+436 KB median on a ~50–53 MB volume, +0.83 %**, and the whole decode grants
a median **481 KB fewer** bytes than before because the transient container
growth shrank by more than the `Arc` blocks added. Allocation count at decode
rises by exactly one per non-empty moment, verified on every volume. Decode
time rises **+2.2 ms median on a ~285 ms volume, +0.8 %**, over 48 pairs
interleaved round by round against a 3.0 % per-run noise floor.

Against that: every *additional* generation of a volume alive at the same time,
which was a full second copy of the gate bytes, now costs nothing. On a 420 s
six-site leg that is **4,281.3 MiB and 5,403,600 allocations not made**.

`GateBuffer::empty()` hands out one process-wide shared `Arc` rather than
allocating per released moment, which makes `without_values` — called for every
moment of every radial of a volume being reduced to a skeleton — cheaper than
it was.

**Maintenance cost of carrying this.** It is the largest of the three deltas in
this file: it changes a field's *type* rather than adding a method beside it, so
re-applying it onto a later upstream release means re-reading `moment.rs` rather
than re-appending to it. Four call sites move with it inside this file
(`from_fixed_point`, `without_values`, `raw_values`, `raw_gate_values`) plus the
`GateBuffer` definition and one trait method. **A wrapper in `squallar-radar`
was considered and cannot hold this**: the shared form has to be the storage
*inside* `MomentDataBlock` for `Sweep`'s derived `Clone` — the clone this exists
to make cheap — to be cheap, and a wrapper outside the model would have to
rebuild every moment through `from_fixed_point`, which is the deep copy again.
Nothing short of changing the field reaches it.

Not offerable upstream as written: `GateBuffer` is a public type in a public
data model, and the choice between a copy and a share belongs to whoever owns
that API.


#### The doubling slack — `src/data/sweep.rs`

`Sweep::from_radials` shrinks each sweep's radial vector, and the sweep vector
itself, before handing them over.

```rust
+                    sweep_radials.shrink_to_fit();
                     sweeps.push(Sweep::new(elevation_number, sweep_radials));
...
+        sweeps.shrink_to_fit();
         sweeps
```

Both vectors are grown by `push` with no capacity known in advance, so both end
on a power-of-two rung. A real 0.5° surveillance cut is **720 radials** and
lands in a vector of capacity **1024**: 304 unused slots of
`size_of::<Radial>()` — **94,848 B on this target** — that the allocator holds
for as long as the volume is resident, and a decoded volume lives in up to four
of squallar's caches at once (loop downloads, still inventory, derivation memo,
and whatever a pane is drawing). Measured over 208 real archive volumes the
spare runs **~42 % of the length**, which is why
`squallar_radar::scan_size::sweep_bytes` charges `radials_capacity()` and not
`len()` — the accessor listed above exists for exactly this quantity, and until
now nothing gave it back.

Measured, `squallar-radar/tests/sweep_radial_slack.rs` (its own binary with the
counting `#[global_allocator]`; the figure is `squallar_alloc::live_bytes`,
granted less returned): one 720-radial sweep off `from_radials` costs
**225,392 B** with the shrink and **320,240 B** without it — a difference of
exactly 94,848 B, the 304 slots.

The cost is one reallocation per sweep, at decode time, on a buffer about to be
handed to a cache and then not touched again — paid once, off the frame thread,
against bytes held for the volume's whole life. **`Sweep::merge` deliberately
does not shrink**: a live volume merges repeatedly as cuts arrive, and shrinking
between merges would pay that copy on every one of them and re-grow
immediately. `Sweep::new` is likewise untouched — a caller that sized its vector
deliberately keeps what it asked for.

Offerable upstream as written; `tests/model_types.rs` is the pin it leaves
intact.
