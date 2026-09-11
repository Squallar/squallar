//! **How this process was launched**, read from the kernel and logged once at
//! startup. Nothing here sets anything.
//!
//! On macOS the launch decides the scheduling standing of the whole process,
//! and the difference is large enough to invalidate a measurement. A run that
//! does not say which side of it it was on is not comparable to one that does,
//! which is the entire reason this module exists: **a demoted process should
//! announce itself, on every launch, rather than quietly report worse
//! numbers.**
//!
//! # The two postures
//!
//! Measured on jacobs-mac-mini (M2, macOS 26.4.1) on 2026-09-10, one binary,
//! launched three ways:
//!
//! | launch | task role | thread QoS |
//! |---|---|---|
//! | `./squallar` over ssh | `TASK_UNSPECIFIED` | `DEFAULT` (0x15) |
//! | `./Squallar.app/Contents/MacOS/squallar` over ssh | `TASK_UNSPECIFIED` | `DEFAULT` (0x15) |
//! | `open -a Squallar.app` (LaunchServices) | an `*_APPLICATION` role | `USER_INTERACTIVE` (0x21) |
//!
//! **The bundle directory is not what does it.** The middle row has the whole
//! bundle and is still demoted; the launch through LaunchServices is what
//! promotes. Apple's *Energy Efficiency Guide for Mac Apps* — "in an app, the
//! main thread runs at a QoS level of user-interactive" — means *launched as*
//! one.
//!
//! What rides on it, from `sudo taskinfo` on the same binary: over ssh the
//! task is `TASK_APPTYPE_DAEMON_INTERACTIVE` in a `com.openssh.sshd`
//! coalition and carries `eff qos ceiling: THREAD_QOS_USER_INITIATED`, so no
//! thread in it can reach user-interactive at all; it spends **85.0 %** of its
//! CPU time on the efficiency cores against **65.6–71.9 %** through
//! LaunchServices, with lower E-core clocks besides (1115–1148 MHz against
//! 1392–1494 MHz). **The shipped launch was never the problem. The
//! measurement rig was.**
//!
//! # Why both signals are reported, and not just one
//!
//! The QoS class alone is a *sufficient* discriminator today and a fragile
//! one: `pthread_get_qos_class_np` answers the **requested** class, not the
//! effective one. It separates the two launches only because nothing in this
//! process ever calls the setter — and that is now load-bearing. An earlier
//! revision of this module did call `pthread_set_qos_class_self_np`, and the
//! consequence is the cautionary half of this doc: the setter returned 0, the
//! getter read back `0x21` on **both** launches, and the thread never ran at
//! `0x21` once, because the ceiling clamped it. Confirming that raise by
//! reading the value back was **a vacuous verification** — a check that could
//! not have failed, since both calls report the same request and neither
//! reports what the kernel granted. It also made the one line that should have
//! flagged a demoted run read identical on both arms.
//!
//! The task role has no such dependency. `task_policy_get` with
//! `TASK_CATEGORY_POLICY` reports what the kernel recorded about the process
//! at launch, which no call of ours can overwrite, so it stays a true
//! discriminator even if someone later adds a QoS setter somewhere. Both are
//! printed; the role is the one to trust.
//!
//! Both APIs are in the public SDK — `task_policy_get` at
//! `mach/task.h`, `TASK_CATEGORY_POLICY` and `task_role_t` at
//! `mach/task_policy.h`, `pthread_get_qos_class_np` at `pthread/qos.h`. That
//! was checked in the headers rather than inferred from the symbols linking:
//! a symbol that resolves is not a sanctioned API.

use core::ffi::c_int;
use core::fmt;

/// A Darwin QoS class, by the values in `sys/qos.h`.
///
/// Spelled here rather than taken from `libc`. `libc 0.2.189` does carry these
/// bindings, at `src/new/apple/libpthread/pthread_/qos.rs`, but its Apple
/// re-export arm (`src/new/mod.rs`) lists `pthread_::introspection`,
/// `pthread_spis`, `spawn` and `stack_np` and **omits `pthread_::qos`** — so
/// `qos_class_t` and the accessors are crate-private there and no path reaches
/// them from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QosClass {
    UserInteractive,
    UserInitiated,
    Default,
    Utility,
    Background,
    /// The absence of QoS information, which is a real state and not an error.
    Unspecified,
    /// A value `sys/qos.h` does not name. Reported rather than folded into
    /// `Unspecified`, which is a class the kernel does assign.
    Unknown(u32),
}

impl QosClass {
    /// `sys/qos.h`'s `qos_class_t` values.
    ///
    /// `pub` because the only in-tree caller is the macOS module, which makes
    /// this dead code on every other target — the same reason
    /// `capacity::darwin_pages` is public. It is also the half of this module
    /// a non-Apple CI row can execute.
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            0x21 => Self::UserInteractive,
            0x19 => Self::UserInitiated,
            0x15 => Self::Default,
            0x11 => Self::Utility,
            0x09 => Self::Background,
            0x00 => Self::Unspecified,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for QosClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserInteractive => f.write_str("user-interactive"),
            Self::UserInitiated => f.write_str("user-initiated"),
            Self::Default => f.write_str("default"),
            Self::Utility => f.write_str("utility"),
            Self::Background => f.write_str("background"),
            Self::Unspecified => f.write_str("unspecified"),
            Self::Unknown(raw) => write!(f, "unknown(0x{raw:02x})"),
        }
    }
}

/// The task's role, by `mach/task_policy.h`'s `task_role_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRole {
    /// **The demoted value**, and the header's own default: "TASK_UNSPECIFIED
    /// is the default, since the role is not inherited from the parent." This
    /// is what an ssh or terminal launch reads.
    Unspecified,
    Reniced,
    ForegroundApplication,
    BackgroundApplication,
    ControlApplication,
    GraphicsServer,
    ThrottleApplication,
    NonUiApplication,
    DefaultApplication,
    DarwinBgApplication,
    UserInitApplication,
    /// A value `task_role_t` does not name.
    Unknown(i32),
}

impl TaskRole {
    /// `mach/task_policy.h`'s `task_role_t` values.
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            -1 => Self::Reniced,
            0 => Self::Unspecified,
            1 => Self::ForegroundApplication,
            2 => Self::BackgroundApplication,
            3 => Self::ControlApplication,
            4 => Self::GraphicsServer,
            5 => Self::ThrottleApplication,
            6 => Self::NonUiApplication,
            7 => Self::DefaultApplication,
            8 => Self::DarwinBgApplication,
            9 => Self::UserInitApplication,
            other => Self::Unknown(other),
        }
    }

    /// Whether the kernel recorded this process as an application at all.
    ///
    /// **`Unspecified` is the whole of the demoted case** and the only value
    /// an ssh launch was ever observed to carry; a LaunchServices launch read
    /// `DefaultApplication` without a window and `ForegroundApplication` with
    /// one, so *which* application role it is tracks focus and is not the
    /// thing worth branching on. `Reniced` and `Unknown` answer `false`
    /// because neither is evidence of an application launch.
    pub fn launched_as_application(self) -> bool {
        !matches!(self, Self::Unspecified | Self::Reniced | Self::Unknown(_))
    }
}

impl fmt::Display for TaskRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unspecified => f.write_str("unspecified"),
            Self::Reniced => f.write_str("reniced"),
            Self::ForegroundApplication => f.write_str("foreground-application"),
            Self::BackgroundApplication => f.write_str("background-application"),
            Self::ControlApplication => f.write_str("control-application"),
            Self::GraphicsServer => f.write_str("graphics-server"),
            Self::ThrottleApplication => f.write_str("throttle-application"),
            Self::NonUiApplication => f.write_str("nonui-application"),
            Self::DefaultApplication => f.write_str("default-application"),
            Self::DarwinBgApplication => f.write_str("darwinbg-application"),
            Self::UserInitApplication => f.write_str("userinit-application"),
            Self::Unknown(raw) => write!(f, "unknown({raw})"),
        }
    }
}

/// One reading, or the reason there is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading<T> {
    Read(T),
    /// The call failed, with its return code.
    Failed(c_int),
    /// Not a macOS target — nothing was asked.
    NotMacos,
}

impl<T: fmt::Display> fmt::Display for Reading<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(v) => write!(f, "{v}"),
            Self::Failed(rc) => write!(f, "unreadable(rc={rc})"),
            Self::NotMacos => f.write_str("n/a"),
        }
    }
}

/// The process's scheduling standing at startup: what the launch gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchPosture {
    /// The task role. **The signal to trust** — nothing this process does can
    /// overwrite it.
    pub role: Reading<TaskRole>,
    /// The calling thread's *requested* QoS class. See the module docs for
    /// why this one is the fragile half.
    pub qos: Reading<QosClass>,
}

impl LaunchPosture {
    /// Whether this run is on the **demoted** side — not launched as an
    /// application, so QoS-ceilinged and scheduled onto the efficiency cores
    /// far more than the shipped launch is.
    ///
    /// A reading that failed answers `false`: an unread role is not evidence
    /// of demotion, and the log line reports the failure on its own.
    pub fn is_demoted(self) -> bool {
        matches!(self.role, Reading::Read(role) if !role.launched_as_application())
    }
}

impl fmt::Display for LaunchPosture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "role={}, thread qos={}", self.role, self.qos)?;
        if self.is_demoted() {
            // Spelled out rather than left for the reader to infer: this line
            // exists so a demoted measurement announces itself.
            f.write_str(
                " -- NOT LAUNCHED AS AN APPLICATION: this process is QoS-ceilinged \
                 and runs far more on the efficiency cores than the shipped launch \
                 does, so its timings are not comparable to a LaunchServices one",
            )?;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod darwin {
    use super::{QosClass, Reading, TaskRole};
    use core::ffi::{c_int, c_uint, c_void};

    type MachPortT = c_uint;

    // `mach/task.h`, `mach/task_policy.h`, `pthread/qos.h` — all public SDK.
    // No setter is declared: this module reads and never sets.
    //
    // The crate is `deny(unsafe_code)`, and in edition 2024 the `unsafe extern`
    // block is itself a site the lint fires on, separately from the calls
    // below. Declaring the signatures is the whole of what is permitted here.
    #[allow(
        unsafe_code,
        reason = "edition 2024 counts the extern block declaration as an unsafe site"
    )]
    unsafe extern "C" {
        static mach_task_self_: MachPortT;
        fn task_policy_get(
            task: MachPortT,
            flavor: c_uint,
            policy_info: *mut c_int,
            count: *mut c_uint,
            get_default: *mut c_int,
        ) -> c_int;
        fn pthread_self() -> *mut c_void;
        fn pthread_get_qos_class_np(
            thread: *mut c_void,
            qos_class: *mut u32,
            relative_priority: *mut c_int,
        ) -> c_int;
    }

    const TASK_CATEGORY_POLICY: c_uint = 1;
    /// `TASK_CATEGORY_POLICY_COUNT`: `sizeof(task_category_policy) /
    /// sizeof(integer_t)`, and `task_category_policy` is one `task_role_t`.
    const TASK_CATEGORY_POLICY_COUNT: c_uint = 1;

    /// This task's role.
    ///
    /// The crate is `deny(unsafe_code)`; the scoped allow is for a Mach call
    /// that writes one `integer_t` into a local this function owns, plus the
    /// read of `mach_task_self_`, which is the task's own name port and needs
    /// no privilege and no deallocation.
    #[allow(
        unsafe_code,
        reason = "task_policy_get writes one integer_t into a local owned here"
    )]
    pub fn role() -> Reading<TaskRole> {
        // A value `task_role_t` does not name, so a call that succeeds without
        // writing reads as `Unknown` rather than as the plausible
        // `Unspecified` -- which is the demoted verdict and must not be
        // reachable by accident.
        let mut info: c_int = i32::MIN;
        let mut count: c_uint = TASK_CATEGORY_POLICY_COUNT;
        // FALSE, meaning "the value this task actually has", not the system
        // default. The kernel writes back whether it gave a default.
        let mut get_default: c_int = 0;
        // SAFETY: `info`, `count` and `get_default` are live locals for the
        // whole call and are written only by it; `count` is the buffer's own
        // size in `integer_t` units, which is this flavour's stated contract.
        // `mach_task_self_` is the task's own name port, a right the task
        // already holds.
        let kr = unsafe {
            task_policy_get(
                mach_task_self_,
                TASK_CATEGORY_POLICY,
                &mut info,
                &mut count,
                &mut get_default,
            )
        };
        if kr != 0 {
            return Reading::Failed(kr);
        }
        Reading::Read(TaskRole::from_raw(info))
    }

    /// The calling thread's requested QoS class.
    ///
    /// The crate is `deny(unsafe_code)`; the scoped allow is because the
    /// getter writes two scalars through pointers into locals this function
    /// owns, and `pthread_self` hands back the calling thread's own handle.
    #[allow(
        unsafe_code,
        reason = "the getter writes two scalars through pointers into locals owned here"
    )]
    pub fn qos() -> Reading<QosClass> {
        let mut raw: u32 = u32::MAX;
        let mut relative: c_int = 0;
        // SAFETY: both locals are live for the whole call and written only by
        // it. `pthread_self` returns this thread's handle, and this code runs
        // on that thread.
        let rc = unsafe { pthread_get_qos_class_np(pthread_self(), &mut raw, &mut relative) };
        if rc != 0 {
            return Reading::Failed(rc);
        }
        Reading::Read(QosClass::from_raw(raw))
    }
}

/// Read how this process was launched. **Reads only**; changes no scheduling
/// state and is safe to call more than once.
///
/// Call it on the thread that runs the event loop: the role is a property of
/// the task, but the QoS class is the calling thread's.
#[cfg(target_os = "macos")]
pub fn read() -> LaunchPosture {
    LaunchPosture {
        role: darwin::role(),
        qos: darwin::qos(),
    }
}

/// See the macOS arm. Compiles to a constant everywhere else.
///
/// iOS is deliberately not on the macOS arm. Every iOS process is launched by
/// the system as an app, so the answer would be a foregone one — but no arm
/// here has run an iOS binary, and a `cfg` that claims a platform nobody
/// executed is the kind of prose this tree treats as unevidenced.
#[cfg(not(target_os = "macos"))]
pub fn read() -> LaunchPosture {
    LaunchPosture {
        role: Reading::NotMacos,
        qos: Reading::NotMacos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `sys/qos.h` values, pinned on **every** arm.
    ///
    /// These `from_raw` maps are the halves of this module that carry no
    /// platform type, so they are the only halves a Linux CI row can execute —
    /// the same reason `capacity::darwin_pages` is mounted everywhere. A
    /// number transcribed wrongly from a header would otherwise be caught by
    /// no row at all, because no CI row runs an Apple test binary.
    #[test]
    fn the_qos_header_values_map_to_the_classes_they_name() {
        assert_eq!(QosClass::from_raw(0x21), QosClass::UserInteractive);
        assert_eq!(QosClass::from_raw(0x19), QosClass::UserInitiated);
        assert_eq!(QosClass::from_raw(0x15), QosClass::Default);
        assert_eq!(QosClass::from_raw(0x11), QosClass::Utility);
        assert_eq!(QosClass::from_raw(0x09), QosClass::Background);
        assert_eq!(QosClass::from_raw(0x00), QosClass::Unspecified);
        assert_eq!(QosClass::from_raw(0x07), QosClass::Unknown(0x07));
    }

    /// `task_role_t`, same reason.
    #[test]
    fn the_task_role_values_map_to_the_roles_they_name() {
        assert_eq!(TaskRole::from_raw(-1), TaskRole::Reniced);
        assert_eq!(TaskRole::from_raw(0), TaskRole::Unspecified);
        assert_eq!(TaskRole::from_raw(1), TaskRole::ForegroundApplication);
        assert_eq!(TaskRole::from_raw(7), TaskRole::DefaultApplication);
        assert_eq!(TaskRole::from_raw(9), TaskRole::UserInitApplication);
        assert_eq!(TaskRole::from_raw(42), TaskRole::Unknown(42));
    }

    /// **The demoted set is exactly the roles that are not an application.**
    /// This is the predicate the warning hangs off, so it is pinned by value
    /// rather than left to the reader of `launched_as_application`.
    #[test]
    fn only_a_non_application_role_counts_as_demoted() {
        for role in [
            TaskRole::ForegroundApplication,
            TaskRole::BackgroundApplication,
            TaskRole::ControlApplication,
            TaskRole::GraphicsServer,
            TaskRole::ThrottleApplication,
            TaskRole::NonUiApplication,
            TaskRole::DefaultApplication,
            TaskRole::DarwinBgApplication,
            TaskRole::UserInitApplication,
        ] {
            assert!(role.launched_as_application(), "{role:?}");
        }
        for role in [
            TaskRole::Unspecified,
            TaskRole::Reniced,
            TaskRole::Unknown(42),
        ] {
            assert!(!role.launched_as_application(), "{role:?}");
        }
    }

    /// **A demoted run says so in words, and a promoted one does not.**
    ///
    /// The whole point of the line: it has to be legible to someone who was
    /// not looking for it.
    #[test]
    fn the_demoted_posture_announces_itself_and_the_promoted_one_stays_quiet() {
        let ssh = LaunchPosture {
            role: Reading::Read(TaskRole::Unspecified),
            qos: Reading::Read(QosClass::Default),
        };
        assert!(ssh.is_demoted());
        let line = ssh.to_string();
        assert!(line.contains("role=unspecified"), "{line}");
        assert!(line.contains("thread qos=default"), "{line}");
        assert!(line.contains("NOT LAUNCHED AS AN APPLICATION"), "{line}");

        let app = LaunchPosture {
            role: Reading::Read(TaskRole::ForegroundApplication),
            qos: Reading::Read(QosClass::UserInteractive),
        };
        assert!(!app.is_demoted());
        let line = app.to_string();
        assert_eq!(
            line,
            "role=foreground-application, thread qos=user-interactive"
        );
    }

    /// An unread role is not evidence of demotion, and must not raise the
    /// alarm on its own — but it must still be visible.
    #[test]
    fn a_failed_reading_is_reported_without_being_called_demoted() {
        let broken = LaunchPosture {
            role: Reading::Failed(5),
            qos: Reading::Failed(22),
        };
        assert!(!broken.is_demoted());
        assert_eq!(
            broken.to_string(),
            "role=unreadable(rc=5), thread qos=unreadable(rc=22)"
        );
    }

    /// **The reader answers on the target that has the API**, and names which
    /// target this build is everywhere else.
    ///
    /// The macOS arm asserts the calls succeeded, never *which* role or class
    /// came back: those are properties of how the test binary was launched,
    /// and pinning one would pin the harness rather than this code. That
    /// distinction is the whole subject of this module.
    ///
    /// No CI row executes an Apple test binary, so the macOS arm of this test
    /// runs only where someone runs it. Executed on jacobs-mac-mini
    /// (M2, macOS 26.4.1) on 2026-09-10.
    #[test]
    fn the_reader_answers_where_the_api_exists_and_declines_to_lie_elsewhere() {
        let posture = read();
        if cfg!(target_os = "macos") {
            assert!(
                matches!(posture.role, Reading::Read(_)),
                "macOS should read a role, got: {posture:?}"
            );
            assert!(
                matches!(posture.qos, Reading::Read(_)),
                "macOS should read a qos class, got: {posture:?}"
            );
        } else {
            assert_eq!(posture.role, Reading::NotMacos);
            assert_eq!(posture.qos, Reading::NotMacos);
            assert_eq!(posture.to_string(), "role=n/a, thread qos=n/a");
        }
    }
}
