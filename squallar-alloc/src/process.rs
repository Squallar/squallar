//! **What the operating system has given this process**, against what the
//! allocator was asked for.
//!
//! # Why this exists
//!
//! [`crate::live_bytes`] counts *requested* sizes: every block the allocator
//! handed out, at the size the caller asked for. That is the figure that can
//! fall, and it is the right one for a governor. It is **not** what the
//! process costs the machine. Between a request and a resident page sit the
//! allocator's own chunk headers, its arena retention, the kernel's
//! transparent huge pages, and every mapping that never went through
//! `malloc` at all — the executable, the shared libraries, the font files,
//! the graphics driver's device maps.
//!
//! On one native reading of the heavy scene the gap was **874 MiB, 28.1 % of
//! a 3,108.6 MiB resident set**, and nothing in this workspace could name a
//! byte of it. `squallar_radar::scan_size::ALLOCATOR_BLOCK_OVERHEAD` says so
//! in its own doc: *"What would retire the uncertainty: an RSS-based
//! instrument … only reading what the OS has actually given the process …
//! Until that exists this constant is a documented bound, not a reading."*
//! This module is that instrument.
//!
//! # Two readings, because they cost three orders of magnitude apart
//!
//! Measured on this workspace's Linux arm against a 7.2 GB, 396-mapping
//! process, 200 reads each, p50 / p99:
//!
//! | interface | p50 | p99 | grows with RSS |
//! |---|---|---|---|
//! | `/proc/self/status` | **10.8 µs** | 15.0 µs | **no** (11.2 µs at 11 MB) |
//! | `/proc/self/maps` | 79.3 µs | 98.2 µs | with mapping count |
//! | `/proc/self/smaps_rollup` | 2,140.6 µs | 2,812.2 µs | **yes** (83.3 µs at 11 MB) |
//! | `/proc/self/smaps` | 3,311.5 µs | 5,777.9 µs | **yes** (190.6 µs at 11 MB) |
//!
//! The two `smaps` interfaces walk page tables, so their cost is a function
//! of the resident set they are being used to measure — the one scaling law
//! an instrument must never have on a frame thread. `status` is a fixed
//! record the kernel already maintains, and it does not move between an
//! 11 MB process and a 7.2 GB one.
//!
//! So [`Resident`] is the always-on reading and [`Breakdown`] is the
//! offloaded one, and the module will not let a caller confuse them: the
//! expensive one names the cost in its own doc and this note is the reason
//! the cheap one exists at all.
//!
//! # A partition, not a pile
//!
//! [`Resident`] is a real partition — the kernel maintains
//! `VmRSS = RssAnon + RssFile + RssShmem`, and
//! [`Resident::partitions`] checks it on every reading rather than trusting
//! it. [`Breakdown`] refines both halves into mapping classes and is a
//! partition too, by construction: every class is a disjoint bucket over the
//! same mapping list, so a mapping this module **misclassifies moves bytes
//! between classes and never off the total**. That is the property worth
//! having. The arena rule below is a heuristic about glibc's internals; if
//! glibc changes it, `arena_bytes` reads zero, `anon_other_bytes` absorbs
//! it, and the reconciliation to RSS still holds exactly.
//!
//! [`Breakdown::thp_bytes`] is the one figure that is **not** a class: huge
//! pages back the anonymous classes rather than sitting beside them, so it
//! overlaps `main_heap_bytes`, `arena_bytes`, `stack_bytes` and
//! `anon_other_bytes` and is excluded from the partition sum. It is reported
//! because it is the term that makes an arena experiment ambiguous, and a
//! reader who cannot see it will attribute its bytes to something else.
//!
//! # Linux, and honestly `None` everywhere else
//!
//! The readers are `cfg`-selected whole, never branched inside: a target
//! without `/proc` gets a function that returns `None`, which is the honest
//! answer and not a process of zero bytes. The **parsers are not** `cfg`-ed
//! — they are pure functions over a `&str` and are unit-tested on every
//! target, wasm included, against captured kernel output.

/// A partition of the process's resident set, as the kernel maintains it.
///
/// From `/proc/self/status`, which costs ~11 µs and does not grow with the
/// resident set (see the module note). Every field is bytes, converted from
/// the kernel's kB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resident {
    /// `VmRSS` — every resident page this process has, the figure `ps` calls
    /// RSS.
    pub rss_bytes: u64,
    /// `RssAnon` — resident anonymous memory. **This is the half the
    /// allocator lives on**: `live_bytes` is a request against this, and
    /// `rss_anon - live_bytes` is chunk headers, arena retention, huge-page
    /// rounding and the allocator's free lists together.
    pub anon_bytes: u64,
    /// `RssFile` — resident file-backed memory: the executable, every shared
    /// library, mapped fonts and data files, **and the graphics driver's
    /// device maps**, which are file-backed through `/dev`. None of it is on
    /// the allocator's books, and on this workspace's arm it is most of a
    /// 250 MiB budget before a byte of weather data.
    pub file_bytes: u64,
    /// `RssShmem` — resident shared-memory and `tmpfs` pages.
    pub shmem_bytes: u64,
    /// `Threads` — carried because arena count follows thread count in
    /// glibc, and a reader of [`Breakdown::arenas`] wants both.
    pub threads: u32,
}

impl Resident {
    /// The three classes summed. The kernel keeps this equal to
    /// [`Self::rss_bytes`]; [`Self::partitions`] is the check.
    pub fn parts_total(&self) -> u64 {
        self.anon_bytes
            .saturating_add(self.file_bytes)
            .saturating_add(self.shmem_bytes)
    }

    /// **Whether this reading actually partitions**, rather than being
    /// trusted to.
    ///
    /// The three `Rss*` fields and `VmRSS` are sampled by the kernel without
    /// a lock held across all four, so a process allocating hard while the
    /// record is generated can produce a set that does not add up. A reader
    /// that prints a partition must be able to say whether this one is one.
    pub fn partitions(&self) -> bool {
        self.parts_total() == self.rss_bytes
    }

    /// **What the allocator cannot account for on the anonymous half**:
    /// resident anonymous bytes less the bytes the allocator was asked for.
    ///
    /// `None` when `live` prices above `RssAnon`, which is a real state and
    /// not an error — a heap whose pages have been returned to the kernel
    /// (`MADV_DONTNEED` leaves the block live and the page gone) prices above
    /// its own residency. Printing `0` there would hide exactly that.
    pub fn anon_over_live(&self, live: u64) -> Option<u64> {
        self.anon_bytes.checked_sub(live)
    }
}

/// Kernel `kB` to bytes. The unit `/proc` prints is always `kB` and always
/// means 1024, whatever the page size is.
fn kib(value: u64) -> u64 {
    value.saturating_mul(1024)
}

/// The number on a `Key:\t<n> kB` line, or `None` if the line is not that
/// shape.
fn field_kib(line: &str) -> Option<u64> {
    let (_, rest) = line.split_once(':')?;
    let mut parts = rest.split_whitespace();
    let value: u64 = parts.next()?.parse().ok()?;
    Some(kib(value))
}

/// **Parse `/proc/<pid>/status`.** Pure, so it is tested on every target
/// against captured kernel output rather than only where `/proc` exists.
///
/// `None` when `VmRSS` is absent — a kernel or a `procfs` that does not
/// report a resident set is not a process of zero bytes. The `Rss*` triple
/// arrived in Linux 4.5; a kernel without it yields the total with a zero
/// split, and [`Resident::partitions`] then reads `false`, which is the
/// truthful answer rather than a fabricated partition.
pub fn parse_status(text: &str) -> Option<Resident> {
    let mut out = Resident::default();
    let mut saw_rss = false;
    for line in text.lines() {
        // `split_once` on the key keeps this a single pass with no
        // allocation: `status` is ~1.6 KB and this runs on a tick.
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        match key {
            "VmRSS" => {
                if let Some(bytes) = field_kib(line) {
                    out.rss_bytes = bytes;
                    saw_rss = true;
                }
            }
            "RssAnon" => out.anon_bytes = field_kib(line).unwrap_or(0),
            "RssFile" => out.file_bytes = field_kib(line).unwrap_or(0),
            "RssShmem" => out.shmem_bytes = field_kib(line).unwrap_or(0),
            "Threads" => {
                out.threads = line
                    .split_once(':')
                    .and_then(|(_, rest)| rest.split_whitespace().next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
            _ => {}
        }
    }
    saw_rss.then_some(out)
}

/// **Read this process's resident set.** ~11 µs, flat in RSS — see the
/// module note for the measurement and for why the expensive reading is a
/// separate function.
///
/// `None` off Linux, and `None` where `/proc` could not be read.
#[cfg(target_os = "linux")]
pub fn resident() -> Option<Resident> {
    parse_status(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// No `/proc` on this target, so there is no reading to give — `None`, not a
/// zero. See the Linux arm for what this answers there.
#[cfg(not(target_os = "linux"))]
pub fn resident() -> Option<Resident> {
    None
}

/// **The span glibc reserves for one secondary arena**, and the signature
/// this module recognises one by.
///
/// glibc's `HEAP_MAX_SIZE` is 64 MiB on 64-bit. An arena is `mmap`ed as one
/// 64 MiB region aligned to 64 MiB, and the part not yet handed out is left
/// `PROT_NONE`, so `smaps` shows the region as a run of adjacent anonymous
/// mappings that begins on a 64 MiB boundary and spans exactly 64 MiB.
///
/// **A heuristic about another project's internals, and it fails safe.** If
/// glibc changes the constant or the layout, no run matches, `arena_bytes`
/// reads zero, [`Breakdown::anon_other_bytes`] absorbs those bytes, and the
/// reconciliation to RSS is exactly as tight as before. Nothing here may be
/// spent against, only reported.
pub const GLIBC_ARENA_SPAN: u64 = 64 << 20;

/// Where every resident byte lives, by mapping class.
///
/// From `/proc/self/smaps`: **3.3 ms p50 and 5.8 ms p99** on a 7.2 GB
/// process, growing with the resident set. That is a frame and a half at
/// 250 Hz. It is never called on the frame thread; see
/// [`breakdown`] and the module note.
///
/// The eight classes partition [`Self::rss_bytes`]; `thp_bytes` overlaps the
/// anonymous ones and is outside the sum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Breakdown {
    /// Every mapping's `Rss` summed — the same quantity `VmRSS` reports, by
    /// a second route. [`Self::partitions`] holds the two against each other.
    pub rss_bytes: u64,
    /// `[heap]` — the main arena's `brk` region.
    pub main_heap_bytes: u64,
    /// Anonymous runs carrying glibc's secondary-arena signature
    /// ([`GLIBC_ARENA_SPAN`]). **This is the arena-retention term**: pages a
    /// thread's arena has faulted in and not given back, which `live_bytes`
    /// stopped counting the moment the block was freed.
    pub arena_bytes: u64,
    /// How many such runs. Reported beside [`Resident::threads`] because
    /// glibc grows arenas with threads up to a multiple of the core count,
    /// and a reader comparing two arms wants to know the count moved.
    pub arenas: u32,
    /// `[stack]` and the guard-paged thread stacks.
    pub stack_bytes: u64,
    /// Every other anonymous mapping: the allocator's own `mmap`ed blocks
    /// (glibc serves a request past `mmap_threshold` this way, so **large
    /// buffers land here and not in an arena**), plus any anonymous mapping
    /// this process made directly.
    pub anon_other_bytes: u64,
    /// File-backed and executable — the binary's text and every shared
    /// library's.
    pub code_bytes: u64,
    /// File-backed and not executable — read-only data, relocations, mapped
    /// fonts, locale archives.
    pub file_other_bytes: u64,
    /// Mappings under `/dev` — **the graphics driver's**. On this
    /// workspace's RTX 3090 arm these read 102.4 MiB and were identical in
    /// all four snapshots of a session, which is why they belong on the line:
    /// they are a fixed floor under any budget, owed before a byte of
    /// weather data is fetched.
    pub device_bytes: u64,
    /// `[vvar]`, `[vdso]`, `[vsyscall]` and their kin.
    pub kernel_bytes: u64,
    /// `AnonHugePages` summed. **Not a class and not in the partition**: a
    /// huge page backs one of the anonymous classes above rather than
    /// sitting beside it, so adding this to the sum double-counts every byte
    /// of it.
    ///
    /// It is here because it is the term that makes an arena experiment
    /// ambiguous. THP rounds a mapping's residency up to 2 MiB granularity,
    /// so a heap that requested 147 MiB can be charged far more resident
    /// without any arena having retained anything — and the same reading is
    /// equally consistent with arenas retaining it and no rounding at all.
    /// **This instrument sizes both terms and does not attribute between
    /// them**; see the module note.
    pub thp_bytes: u64,
    /// How many mappings the walk saw. A sanity figure: a truncated read
    /// shows up here before it shows up as a residual.
    pub mappings: u32,
}

impl Breakdown {
    /// The eight disjoint classes summed. Equal to [`Self::rss_bytes`] by
    /// construction — every mapping is charged to exactly one class — so a
    /// difference is a bug in this module and not a property of the process.
    pub fn parts_total(&self) -> u64 {
        [
            self.main_heap_bytes,
            self.arena_bytes,
            self.stack_bytes,
            self.anon_other_bytes,
            self.code_bytes,
            self.file_other_bytes,
            self.device_bytes,
            self.kernel_bytes,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }

    /// **Whether the classes actually add to the walk's own total.** Checked
    /// rather than asserted: a `smaps` read that raced a mapping change can
    /// produce a list this module summed correctly and the kernel did not.
    pub fn partitions(&self) -> bool {
        self.parts_total() == self.rss_bytes
    }

    /// **Everything that is not the allocator's to give back**: code,
    /// non-executable file maps, device maps and kernel pages.
    ///
    /// The floor under any resident-set budget. No amount of freeing weather
    /// data moves it, and a 250 MiB target that does not name it is a target
    /// against a number that was never available.
    pub fn non_heap_bytes(&self) -> u64 {
        self.code_bytes
            .saturating_add(self.file_other_bytes)
            .saturating_add(self.device_bytes)
            .saturating_add(self.kernel_bytes)
    }
}

/// One mapping's class, decided by its pathname and permissions.
///
/// Spelled as a function over the two fields rather than inline in the
/// parser so the classification is tested directly, one case at a time.
fn classify(pathname: &str, perms: &str) -> Class {
    match pathname {
        "[heap]" => Class::MainHeap,
        "[stack]" => Class::Stack,
        "" => Class::Anon,
        // `[vvar]`, `[vdso]`, `[vsyscall]`, `[vvar_vclock]` — and any future
        // bracketed name, which is a kernel mapping by construction.
        p if p.starts_with('[') => Class::Kernel,
        // Before the executable test: a driver's device map can be mapped
        // executable, and it is a device map either way.
        p if p.starts_with("/dev/") => Class::Device,
        _ if perms.contains('x') => Class::Code,
        _ => Class::FileOther,
    }
}

/// The buckets [`classify`] sorts into. `Anon` is provisional — an anonymous
/// mapping becomes an arena or stays `anon_other` only once the run it
/// belongs to has ended and its span is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    MainHeap,
    Stack,
    Anon,
    Kernel,
    Device,
    Code,
    FileOther,
}

/// An anonymous run being accumulated: where it starts, where it has reached,
/// and the resident bytes charged to it so far.
#[derive(Clone, Copy, Default)]
struct AnonRun {
    start: u64,
    end: u64,
    rss: u64,
    open: bool,
}

impl AnonRun {
    /// **Is this run one of glibc's secondary arenas?** Begins on a
    /// [`GLIBC_ARENA_SPAN`] boundary and spans exactly that much. See the
    /// constant for why a miss is safe.
    fn is_arena(&self) -> bool {
        self.open
            && self.start.is_multiple_of(GLIBC_ARENA_SPAN)
            && self.end.saturating_sub(self.start) == GLIBC_ARENA_SPAN
    }
}

/// A `smaps` header line's three fields we need: start, end, perms, pathname.
///
/// `55d0e2a00000-55d0e2a21000 rw-p 00000000 00:00 0    [heap]` — five
/// whitespace-separated columns and then an optional pathname which may
/// itself contain spaces, so the pathname is taken as the remainder rather
/// than as a sixth column.
fn parse_header(line: &str) -> Option<(u64, u64, &str, &str)> {
    let (range, rest) = line.split_once(' ')?;
    let (start, end) = range.split_once('-')?;
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    let mut cols = rest.splitn(4, ' ');
    let perms = cols.next()?;
    // offset, dev, then the remainder is `inode pathname`.
    let _offset = cols.next()?;
    let _dev = cols.next()?;
    let inode_and_path = cols.next().unwrap_or("");
    let pathname = inode_and_path
        .split_once(' ')
        .map(|(_inode, path)| path.trim_start())
        .unwrap_or("");
    Some((start, end, perms, pathname))
}

/// **Parse `/proc/<pid>/smaps`.** Pure, and tested against captured kernel
/// output on every target.
///
/// A header line opens a mapping; the indented `Rss:` and `AnonHugePages:`
/// lines under it are charged to whatever class the header decided.
/// Anonymous mappings are accumulated into address-contiguous runs and
/// settled when the run breaks, because glibc's arena is two adjacent
/// mappings and neither one alone carries the signature.
pub fn parse_smaps(text: &str) -> Breakdown {
    let mut out = Breakdown::default();
    let mut class = Class::Anon;
    let mut run = AnonRun::default();

    // Charge a finished anonymous run to whichever bucket its span says.
    fn settle(out: &mut Breakdown, run: &mut AnonRun) {
        if !run.open {
            return;
        }
        if run.is_arena() {
            out.arena_bytes = out.arena_bytes.saturating_add(run.rss);
            out.arenas += 1;
        } else {
            out.anon_other_bytes = out.anon_other_bytes.saturating_add(run.rss);
        }
        *run = AnonRun::default();
    }

    for line in text.lines() {
        if let Some((start, end, perms, pathname)) = parse_header(line) {
            out.mappings += 1;
            class = classify(pathname, perms);
            if class == Class::Anon {
                // Extend the open run when this mapping begins exactly where
                // the last one ended; otherwise the old run is finished.
                if run.open && run.end == start {
                    run.end = end;
                } else {
                    settle(&mut out, &mut run);
                    run = AnonRun {
                        start,
                        end,
                        rss: 0,
                        open: true,
                    };
                }
            } else {
                settle(&mut out, &mut run);
            }
            continue;
        }
        // A detail line under the mapping the header opened. Matched on the
        // exact key: `Rss` and not `Pss`, and never a prefix — `Rss:` is a
        // prefix of nothing but itself, but `Shared_` and `Private_` lines
        // sit beside it and a looser test would sweep them in.
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        match key {
            "Rss" => {
                let bytes = field_kib(line).unwrap_or(0);
                out.rss_bytes = out.rss_bytes.saturating_add(bytes);
                match class {
                    Class::MainHeap => {
                        out.main_heap_bytes = out.main_heap_bytes.saturating_add(bytes);
                    }
                    Class::Stack => out.stack_bytes = out.stack_bytes.saturating_add(bytes),
                    Class::Kernel => out.kernel_bytes = out.kernel_bytes.saturating_add(bytes),
                    Class::Device => out.device_bytes = out.device_bytes.saturating_add(bytes),
                    Class::Code => out.code_bytes = out.code_bytes.saturating_add(bytes),
                    Class::FileOther => {
                        out.file_other_bytes = out.file_other_bytes.saturating_add(bytes);
                    }
                    Class::Anon => run.rss = run.rss.saturating_add(bytes),
                }
            }
            "AnonHugePages" => {
                out.thp_bytes = out.thp_bytes.saturating_add(field_kib(line).unwrap_or(0));
            }
            _ => {}
        }
    }
    settle(&mut out, &mut run);
    out
}

/// **Read this process's mapping-by-mapping breakdown.**
///
/// **3.3 ms p50, 5.8 ms p99 on a 7.2 GB process, and it grows with the
/// resident set** — the kernel walks page tables to answer it. Never call
/// this on the frame thread; the whole reason [`resident`] exists is to be
/// the reading that can be taken there.
///
/// `None` off Linux, and `None` where `/proc` could not be read.
#[cfg(target_os = "linux")]
pub fn breakdown() -> Option<Breakdown> {
    Some(parse_smaps(
        &std::fs::read_to_string("/proc/self/smaps").ok()?,
    ))
}

/// No `/proc` on this target — `None`, not an empty breakdown.
#[cfg(not(target_os = "linux"))]
pub fn breakdown() -> Option<Breakdown> {
    None
}

#[cfg(test)]
#[path = "process/tests.rs"]
mod tests;
