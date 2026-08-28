//! Subprocess plumbing.
//!
//! GDAL, tippecanoe, tile-join, go-pmtiles and sqlite3 stay external binaries;
//! only the arithmetic and the orchestration are in this crate. What changes
//! from a shell is that each member of a pipeline reports its own
//! [`ExitStatus`], which is the difference between "tippecanoe found no
//! geometries because this ground is flat" and "tippecanoe found no geometries
//! because gdal_contour died before writing any".

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::Res;
use crate::log;

/// Build a command, keeping the argument list readable at the call site.
pub fn cmd<S: AsRef<OsStr>>(program: &str, args: &[S]) -> Command {
    let mut c = Command::new(program);
    c.args(args.iter().map(AsRef::as_ref));
    c
}

/// Render a command the way a shell would show it, for error messages.
pub fn show(c: &Command) -> String {
    let mut s = c.get_program().to_string_lossy().into_owned();
    for a in c.get_args() {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}

/// Run to completion, inheriting stderr, and fail on a non-zero status.
pub fn run(mut c: Command) -> Res<()> {
    let line = show(&c);
    let status = c
        .status()
        .map_err(|e| format!("could not start `{line}`: {e}"))?;
    if !status.success() {
        return Err(format!("`{line}` exited {}", code(status)).into());
    }
    Ok(())
}

/// Run to completion and return stdout as text.
pub fn capture(mut c: Command) -> Res<String> {
    let line = show(&c);
    let out = c
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("could not start `{line}`: {e}"))?;
    if !out.status.success() {
        return Err(format!("`{line}` exited {}", code(out.status)).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn code(s: ExitStatus) -> String {
    match s.code() {
        Some(c) => c.to_string(),
        None => format!("on a signal ({s})"),
    }
}

/// Feed the concatenated stdout of several producers into one consumer.
///
/// The producers' stdout is the consumer's stdin file descriptor directly, so
/// nothing is copied through this process and nothing lands on disk — the same
/// stream `producer … /vsistdout/ | consumer` makes in a shell. What a shell
/// cannot give back is the per-member status this returns.
///
/// Producers run in order, exactly as a `for` loop inside a shell pipe does:
/// the consumer sees one continuous stream.
pub struct Pipeline {
    consumer: Child,
    producers: Vec<(String, ExitStatus)>,
}

impl Pipeline {
    /// Start the consumer. Its stderr is captured; its stdout is discarded.
    pub fn to(mut consumer: Command) -> Res<Self> {
        let line = show(&consumer);
        let child = consumer
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start `{line}`: {e}"))?;
        Ok(Self {
            consumer: child,
            producers: Vec::new(),
        })
    }

    /// Run one producer with its stdout wired to the consumer's stdin.
    pub fn feed(&mut self, mut producer: Command) -> Res<()> {
        let line = show(&producer);
        let sink: OwnedFd = self
            .consumer
            .stdin
            .as_ref()
            .ok_or("pipeline stdin already closed")?
            .as_fd()
            .try_clone_to_owned()?;
        let status = producer
            .stdout(Stdio::from(sink))
            .spawn()
            .map_err(|e| format!("could not start `{line}`: {e}"))?
            .wait()?;
        self.producers.push((line, status));
        Ok(())
    }

    /// Close the stream, wait for the consumer, and report everything.
    ///
    /// The consumer's stdin is dropped first; without that it never sees EOF
    /// and this blocks forever.
    pub fn finish(mut self) -> Res<PipelineResult> {
        drop(self.consumer.stdin.take());
        let mut stderr = String::new();
        if let Some(mut e) = self.consumer.stderr.take() {
            e.read_to_string(&mut stderr)?;
        }
        let status = self.consumer.wait()?;
        for (line, s) in &self.producers {
            if !s.success() {
                return Err(format!("`{line}` exited {}", code(*s)).into());
            }
        }
        Ok(PipelineResult { status, stderr })
    }
}

/// The consumer's outcome, once every producer is known to have succeeded.
#[derive(Debug)]
pub struct PipelineResult {
    pub status: ExitStatus,
    pub stderr: String,
}

/// Run `body` over `items` on `jobs` threads, logging each failure and
/// returning how many failed.
///
/// A single failure does not abandon the run, so one unreachable S3 object does
/// not cost the other 1486 chunks — but the count comes back, and the caller
/// fails the build on it. A partially-built archive that exits 0 is the failure
/// mode this shape exists to avoid.
pub fn parallel<T, F>(items: &[T], jobs: usize, body: F) -> usize
where
    T: Sync,
    F: Fn(&T) -> Res<()> + Sync,
{
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { return };
                    if let Err(e) = body(item) {
                        log!("FAILED: {e}");
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    failed.load(Ordering::Relaxed)
}

/// Fail unless every named program is on `PATH`.
pub fn need(programs: &[&str]) -> Res<()> {
    let missing: Vec<&str> = programs
        .iter()
        .copied()
        .filter(|p| {
            !std::env::var_os("PATH")
                .map(|path| {
                    std::env::split_paths(&path).any(|d| {
                        std::fs::metadata(d.join(p))
                            .map(|m| m.is_file() || m.is_symlink())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "missing required commands: {}. Run bootstrap-al2023.sh.",
            missing.join(", ")
        )
        .into())
    }
}
