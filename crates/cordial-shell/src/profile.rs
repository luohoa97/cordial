//! Profiles, and the claim an instance takes on one.
//!
//! [ADR-012](../../../docs/adr/ADR-012-profiles-and-instances.md) draws the
//! distinction this module exists to enforce: an *instance* is a running
//! process — a window — and a *profile* is the directory holding one account's
//! Roblox storage. An instance runs a profile, and a profile may be held by at
//! most one instance at a time. Two instances on one profile are two processes
//! writing one `appData` and one cookie store; the corruption that follows
//! presents as a Cordial bug rather than as unsupported use, which is why the
//! lock is not left to convention.
//!
//! **This is now the only implementation of that lock, and it is reached from
//! both processes.** It used to be written twice — once here and once in
//! `cordial_runtime::profile` — with a note saying the runtime could adopt this
//! copy and delete its own. That is what happened, on 2026-08-22, and for a
//! sharper reason than tidiness: the runtime's copy was never called, so
//! `cordial-run --profile X` took no lock at all and four of them ran at once
//! against one profile without a single refusal. `cordial-runtime` already
//! depends on this crate for `host_window`, so the client reaches
//! [`claim_for_instance`] here rather than reimplementing it. The duplicate is
//! gone rather than merely deprecated, because the pair that drifted was two
//! locks guarding a credential and the drift was total.
//!
//! Two doors into that lock, and which one a process uses is decided by who
//! started it. A launcher calls [`acquire`] and then [`Claim::hand_to`], so the
//! *instance* holds the claim rather than the shell that spawned it. A client
//! calls [`claim_for_instance`], which adopts the handed-down descriptor when
//! there is one and takes its own lock when there is not. The distinction is
//! not cosmetic — see [`claim_for_instance`] for why a client that
//! unconditionally re-locked would refuse itself.
//!
//! The precedent for reimplementing rather than linking is `flags_file.rs`,
//! which does the same for the flags path and says why in its own header;
//! profiles no longer need it in either direction.

use std::ffi::c_int;
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};

extern "C" {
    fn flock(fd: c_int, operation: c_int) -> c_int;
    // Declared variadic because it is. Wrapping a variadic function in a
    // fixed-arity declaration is what makes `CORDIAL_TRACE=1` abort the engine,
    // and the lesson is cheap enough to apply to a two-line `fcntl` call.
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

/// `SIGTERM`, `SIGKILL`, and `ESRCH` for the process having already gone. All
/// three are stable across every Linux ABI Cordial runs on.
const SIGTERM: c_int = 15;
const SIGKILL: c_int = 9;
const ESRCH: c_int = 3;

/// `LOCK_EX | LOCK_NB` from `sys/file.h`. Non-blocking on purpose: a second
/// launch should say so immediately rather than hang waiting for the first to
/// exit, which reads as the client failing to start.
const LOCK_EX_NB: c_int = 2 | 4;

/// `F_SETFD` from `fcntl.h`, and the empty flag set that clears `FD_CLOEXEC`.
const F_SETFD: c_int = 2;

/// `FD_CLOEXEC`, put *back* on the descriptor once the client that was handed
/// it has adopted it — see [`claim_for_instance`]. Clearing it is how the lock
/// crosses one `exec`; leaving it clear is how the lock would keep crossing
/// every subsequent one, and the client execs `bwrap` and `deno` for plugin
/// sandboxes (`cordial_plugins::sandbox`). A sandbox outliving the client
/// would hold the profile with no window and no engine attached to it.
const FD_CLOEXEC: c_int = 1;

/// The descriptor number [`Claim::hand_to`] passed down, named in the child's
/// environment.
///
/// A client cannot tell an inherited lock from no lock by looking at its own
/// descriptor table — fd 3 is fd 3 either way — and the difference decides
/// whether it must take a lock or must not. So the shell says which, at exactly
/// the point it hands the descriptor over, and [`claim_for_instance`] checks
/// the answer against `/proc/self/fd` rather than believing it.
pub const HANDED_LOCK: &str = "CORDIAL_PROFILE_LOCK_FD";

/// Where profiles live. `$XDG_DATA_HOME/cordial/profiles`, falling back the way
/// the rest of the tree does.
pub fn root() -> PathBuf {
    std::env::var_os("CORDIAL_PROFILE_ROOT").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(std::env::temp_dir)
            .join("cordial/profiles")
    })
}

/// A profile name that cannot escape the profile root.
///
/// Names reach this from a settings entry the user types into, so `../` and
/// absolute paths are refused rather than sanitised — quietly rewriting a name
/// would mean the profile someone selected is not the one they get.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn dir(name: &str) -> Result<PathBuf, String> {
    if !is_valid_name(name) {
        return Err(format!("{name:?} is not a usable profile name; use letters, digits, - and _"));
    }
    Ok(root().join(name))
}

/// Profiles that exist, for the settings surface to offer.
pub fn list() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_name(n))
        .collect();
    names.sort();
    names
}

/// An instance's claim on a profile.
///
/// The file is deliberately held open: `flock` is tied to the open file
/// description, so closing every handle to it releases the lock. That is also
/// what makes [`Claim::hand_to`] work — see there.
#[derive(Debug)]
pub struct Claim {
    file: File,
    dir: PathBuf,
}

impl Claim {
    pub fn profile_dir(&self) -> &Path {
        &self.dir
    }

    /// Give this claim to a process about to be spawned, so that the *instance*
    /// holds it rather than the launcher.
    ///
    /// ADR-012 says the lock is held "for the lifetime of the instance", and an
    /// instance is the `cordial-run` process, not the shell that started it.
    /// If the shell held the lock instead, quitting the launcher while the
    /// client is still up would release a profile that is still being written
    /// to — and the shell quitting is the ordinary case, not an edge one.
    ///
    /// A `flock` belongs to the open file description, which `fork` shares and
    /// `exec` preserves, so the child inherits the lock itself rather than a
    /// copy of it. The only thing in the way is `FD_CLOEXEC`, which Rust sets
    /// on every `File` it opens; clearing it in the child, after the fork and
    /// before the exec, is the whole of the mechanism. The launcher then drops
    /// its own handle and the lock survives in the child, released when that
    /// process exits however it exits — which a lock file holding a PID would
    /// not manage.
    ///
    /// Failing here aborts the spawn rather than launching unprotected. An
    /// instance running without the claim it is supposed to hold is precisely
    /// the "stub that returns success" AGENTS.md rules out: the second launch
    /// would then be allowed, and the corruption would surface later with
    /// nothing pointing back here.
    ///
    /// [`HANDED_LOCK`] is set on the same `Command`, and set *here* rather than
    /// at the call site so that the marker and the descriptor cannot come
    /// apart. The child needs it because it has to take its own lock when it
    /// was started by hand and must not when it was started this way — a
    /// second `flock` on a second open file description conflicts with this
    /// one even inside the same process, so a client that always re-locked
    /// would refuse itself. [`claim_for_instance`] is the far end.
    pub fn hand_to(&self, command: &mut std::process::Command) {
        let fd = self.file.as_raw_fd();
        command.env(HANDED_LOCK, fd.to_string());
        // SAFETY: `pre_exec` runs between fork and exec in the child, where the
        // only rule is that the closure must be async-signal-safe. `fcntl` is.
        // `fd` is valid in the child because the fork copied the descriptor
        // table, and `self.file` is alive in the parent until after `spawn`
        // returns.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(command, move || {
                if fcntl(fd, F_SETFD, 0 as c_int) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// Take this instance's claim on `name`, creating the profile if it is new.
///
/// Fails immediately if another instance holds it. **Advisory, and honestly
/// so:** `flock` does not stop a process that never asks. It stops Cordial
/// doing this by accident, which is the actual failure mode — someone launching
/// twice, not someone defeating a lock.
pub fn acquire(name: &str) -> Result<Claim, Error> {
    let dir = dir(name).map_err(Error::Unusable)?;
    std::fs::create_dir_all(&dir).map_err(|e| Error::Unusable(format!("{}: {e}", dir.display())))?;
    restrict_to_owner(&dir);

    let lock_path = dir.join(".lock");
    let file =
        File::create(&lock_path).map_err(|e| Error::Unusable(format!("{}: {e}", lock_path.display())))?;

    // SAFETY: `file` is open for the duration of the call and outlives it.
    if unsafe { flock(file.as_raw_fd(), LOCK_EX_NB) } != 0 {
        return Err(Error::Busy(name.to_string(), holder_of(&lock_path)));
    }
    Ok(Claim { file, dir })
}

/// The claim a *client* holds on the profile it runs — inherited if a launcher
/// handed one down, taken here if nobody did.
///
/// **[`acquire`] alone is the wrong call for a client, and the way it is wrong
/// is silent.** `flock` belongs to the open file description, not to the file
/// and not to the process, so a second `open` of the same lock file conflicts
/// with a lock already held on the first one *even in the process that holds
/// it* — `flock(2)` says so, and `a_second_instance_cannot_take_a_held_profile`
/// below demonstrates it inside one process. A shell-launched client that
/// called `acquire` would therefore be refused the profile it is already
/// holding, by itself, on every launch.
///
/// So [`Claim::hand_to`] leaves [`HANDED_LOCK`] in the environment and this
/// adopts the descriptor it names. Adopting means three things, and the third
/// is the one that is easy to leave out:
///
/// * the descriptor is checked against `/proc/self/fd` before it is believed.
///   An exported-by-accident `CORDIAL_PROFILE_LOCK_FD` would otherwise make a
///   client announce a lock it does not hold, which is worse than no lock;
/// * the `flock` is re-asserted on it. On the description that already owns the
///   lock this is a no-op that succeeds, so it costs nothing and turns "the
///   shell said so" into "the kernel says so";
/// * `FD_CLOEXEC` goes back on. `hand_to` cleared it to get the lock across one
///   `exec`, and left clear it would cross every later one too — the plugin
///   sandbox execs `bwrap` and `deno`, and a sandbox that outlived the client
///   would keep the profile locked with nothing running against it.
///
/// A client that was handed nothing takes its own lock, which is the whole
/// point: `cordial-run --profile X` is a documented way to start this client
/// and until 2026-08-22 it took no lock at all, so four of them ran at once
/// against one profile — exactly the two-writers corruption ADR-012 exists to
/// prevent, in the invocation AGENTS.md tells every contributor to type.
pub fn claim_for_instance(name: &str) -> Result<Claim, Error> {
    let dir = dir(name).map_err(Error::Unusable)?;
    match adopt_handed_lock(&dir) {
        Some(file) => Ok(Claim { file, dir }),
        None => acquire(name),
    }
}

/// The descriptor a launcher handed down, if there is one and it is really it.
///
/// `None` means "take your own lock", never "you already hold one": every way
/// of failing to verify lands here, and the caller's fallback is [`acquire`],
/// which either succeeds or names whoever is holding the profile. There is no
/// path on which this reports a lock nobody took.
fn adopt_handed_lock(dir: &Path) -> Option<File> {
    let raw = std::env::var(HANDED_LOCK).ok()?;
    // Consumed rather than read. The client execs plugin sandboxes, and a
    // descriptor number is meaningless in a process that did not inherit it —
    // worse, it could name something else entirely by then.
    std::env::remove_var(HANDED_LOCK);

    let fd: c_int = raw.trim().parse().ok().filter(|fd| *fd >= 0)?;
    let expected = std::fs::canonicalize(dir.join(".lock")).ok()?;
    // Linux: the descriptor's `/proc/self/fd` link names the file directly.
    // FreeBSD has no such link, so fall back to comparing the descriptor's
    // identity (device + inode) against the lock file — the fd names the lock
    // iff `fstat` on it matches `stat` on the path.
    let names_lock = {
        let via_proc = std::fs::read_link(format!("/proc/self/fd/{fd}")).ok();
        if via_proc.as_deref() == Some(expected.as_path()) {
            true
        } else {
            use std::os::unix::fs::MetadataExt;
            let want = std::fs::metadata(&expected).ok();
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let ok = unsafe { libc::fstat(fd, &mut st) } == 0;
            matches!((want, ok), (Some(m), true)
                if st.st_dev as u64 == m.dev() as u64 && st.st_ino as u64 == m.ino() as u64)
        }
    };
    if !names_lock {
        // Loud, because it means the launcher and the client disagree about
        // which profile is being run, and the launch continues on the client's
        // answer. Taking our own lock is the honest response: if the shell's
        // descriptor really did hold this profile we will be refused by it and
        // say so, rather than proceeding on a claim we cannot see.
        println!(
            "  profiles: {HANDED_LOCK}={raw} does not name {}; taking this profile's lock instead",
            expected.display()
        );
        return None;
    }

    // SAFETY: `fd` is an open descriptor in this process — just established by
    // reading its `/proc/self/fd` link. `flock` on a descriptor is safe for any
    // value; an invalid one returns EBADF.
    if unsafe { flock(fd, LOCK_EX_NB) } != 0 {
        // Only reachable if the descriptor exists but the lock on it does not,
        // which the shell's own path cannot produce. Reported rather than
        // assumed away, and again the fallback re-asks the kernel.
        println!(
            "  profiles: the descriptor this client was handed does not hold {}; taking the lock",
            expected.display()
        );
        return None;
    }

    // SAFETY: as above. `F_SETFD` takes an int; the declaration is variadic
    // because `fcntl` is, so the argument has to be typed at the call site.
    unsafe { fcntl(fd, F_SETFD, FD_CLOEXEC) };

    // SAFETY: ownership of the descriptor transfers here, and nothing else in
    // this process holds it — it arrived across an `exec`, so there is no other
    // `File` wrapping it. From this point the lock lives exactly as long as the
    // returned `Claim`, which for a client is the whole process.
    Some(unsafe { File::from_raw_fd(fd) })
}

/// The process holding a profile's lock.
///
/// Reported so a refusal can name something the user is able to act on.
/// "Already open in another Cordial window" is accurate and useless when there
/// is no window to find, and that is the case people actually meet — see
/// [`holder_of`] for how one arises.
#[derive(Debug, Clone)]
pub struct Holder {
    pub pid: u32,
    /// `/proc/<pid>/cmdline`, NULs replaced with spaces. Empty if unreadable.
    pub command: String,
    /// How long it has been up, from the mtime of `/proc/<pid>`.
    pub running_for: Option<std::time::Duration>,
}

impl Holder {
    /// Whether this is one of ours, and so whether offering to close it is a
    /// reasonable thing for an interface to do.
    ///
    /// Deliberately narrow. A button that terminates whichever process happens
    /// to have a file open is a button that eventually terminates something
    /// else, and recovering from that is worse than the problem it solves.
    pub fn is_cordial(&self) -> bool {
        self.command.contains("cordial-run")
    }

    /// Roughly how long it has been up, for a sentence rather than a log line.
    pub fn running_for_text(&self) -> Option<String> {
        let secs = self.running_for?.as_secs();
        Some(match secs {
            0..=90 => format!("{secs} seconds"),
            91..=5400 => format!("{} minutes", (secs + 30) / 60),
            _ => format!("{} hours", (secs + 1800) / 3600),
        })
    }

    /// Whether this process is gone, so a caller can wait for the profile
    /// rather than guess at how long a shutdown takes.
    ///
    /// Answered by the presence of `/proc/<pid>` rather than by re-taking the
    /// lock. Re-taking it answers a subtly different question: the lock is free
    /// the instant the descriptor closes, which is *before* the process has
    /// finished exiting, so a relaunch keyed on that would start a new client
    /// against a profile directory the old one is still unwinding through —
    /// the two-writers case this whole module exists to prevent, reintroduced
    /// by the recovery path.
    pub fn has_exited(&self) -> bool {
        !Path::new(&format!("/proc/{}", self.pid)).exists()
    }

    /// Ask this holder to stop, releasing the profile.
    ///
    /// `SIGTERM`, never `SIGKILL`. The engine is mid-session with Roblox's
    /// storage open underneath it, and the whole reason this lock exists is
    /// that half-written `appData` presents as a Cordial bug rather than as
    /// unsupported use. A signal it can act on is the difference.
    ///
    /// Refuses anything that is not `cordial-run` even though every caller is
    /// expected to check first. The check that matters is the one next to the
    /// `kill`, because the one at the call site is the one a later refactor
    /// moves away.
    pub fn ask_to_stop(&self) -> Result<(), String> {
        if !self.is_cordial() {
            return Err(format!(
                "process {} is not a Cordial client, so Cordial will not close it: {}",
                self.pid, self.command
            ));
        }
        // SAFETY: `kill` with a signal number is safe for any pid; the worst
        // case is ESRCH, which is the process having exited already.
        if unsafe { kill(self.pid as c_int, SIGTERM) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(ESRCH) {
                return Ok(());
            }
            return Err(format!("could not close process {}: {e}", self.pid));
        }
        Ok(())
    }

    /// Stop asking and end this holder outright, releasing the profile
    /// whatever state its teardown was in.
    ///
    /// Reached only once `ask_to_stop`'s `SIGTERM` has been given a real
    /// chance and missed it — see `window.rs`'s escalation thresholds for how
    /// long that chance is and why. `SIGKILL` cannot be caught, so unlike
    /// `ask_to_stop` this does not give the engine any opportunity to finish
    /// writing `rbx-storage.db` or a cookie to the secret service; it is the
    /// trade this function's callers accept once a shutdown has run far
    /// longer than any observed here, in exchange for the thing the user
    /// actually asked for: the profile back. `_exit` drops every file
    /// descriptor including the `flock`, whatever the process was doing.
    ///
    /// Guarded by `is_cordial` for the same reason `ask_to_stop` is, and the
    /// reason is worth restating here specifically: this is the sharper of
    /// the two signals, so a caller that skipped the check upstream would do
    /// more damage here, not less.
    pub fn force_stop(&self) -> Result<(), String> {
        if !self.is_cordial() {
            return Err(format!(
                "process {} is not a Cordial client, so Cordial will not close it: {}",
                self.pid, self.command
            ));
        }
        // SAFETY: as in `ask_to_stop`.
        if unsafe { kill(self.pid as c_int, SIGKILL) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(ESRCH) {
                return Ok(());
            }
            return Err(format!("could not force-close process {}: {e}", self.pid));
        }
        Ok(())
    }

    /// Total CPU time this process has been scheduled for, in kernel jiffies
    /// — `/proc/<pid>/stat`'s `utime` plus `stime` fields.
    ///
    /// Two samples of this taken a few seconds apart are the cheap, no-debugger
    /// way to tell a process still doing something from one that is truly
    /// blocked, which is the exact distinction AGENTS.md's note on reading a
    /// backtrace warns is otherwise invisible: "a spinning pump and a blocked
    /// one produce identical backtraces... always quote the process's CPU
    /// beside the stack." A backtrace needs a debugger Cordial cannot assume
    /// is installed on a user's machine; `/proc` needs nothing.
    ///
    /// `comm` (`/proc/<pid>/stat`'s second field) is parenthesised and may
    /// itself contain spaces, digits or parentheses — a process can name
    /// itself anything via `PR_SET_NAME` — so the split point is the *last*
    /// `)` on the line rather than a fixed field index, which is what
    /// `proc(5)` itself recommends.
    pub fn cpu_ticks(&self) -> Option<u64> {
        let raw = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
        let after_comm = raw.rsplit_once(')')?.1;
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        // `state` is the first field after the closing paren, so counting
        // from there rather than from the start of the line: utime is
        // proc(5)'s field 14 and stime is field 15, which land at indices 11
        // and 12 once the two already-consumed fields (pid, comm) are out of
        // the slice.
        let utime: u64 = fields.get(11)?.parse().ok()?;
        let stime: u64 = fields.get(12)?.parse().ok()?;
        Some(utime + stime)
    }

    /// The kernel function this process's main thread is blocked in, if the
    /// kernel is willing to say — `/proc/<pid>/wchan`.
    ///
    /// A thread that is actually runnable reads back empty or `"0"`, both of
    /// which are treated as "nothing to report" rather than as a wait channel
    /// literally named that. Best-effort in the same spirit as `holder_of`:
    /// some kernels gate this behind `kernel.kptr_restrict` or refuse it
    /// outright, and a missing answer here means "could not tell", never
    /// "not blocked".
    pub fn wchan(&self) -> Option<String> {
        let raw = std::fs::read_to_string(format!("/proc/{}/wchan", self.pid)).ok()?;
        let raw = raw.trim();
        if raw.is_empty() || raw == "0" {
            return None;
        }
        Some(raw.to_string())
    }
}

/// Which process has `path` open, if it can be told.
///
/// **`/proc/locks` is the obvious instrument and it is the wrong one.** Its PID
/// column names the process that *acquired* the lock, and [`Claim::hand_to`]
/// makes that the launcher — which then drops its handle and usually exits. On
/// this developer's machine `/proc/locks` named PID 649439 as holding the
/// default profile while the descriptor was held by 649889, and 649439 no
/// longer existed. An implementation built on it would confidently report a
/// dead process. Scanning `/proc/*/fd` finds whatever actually has the file
/// open, which is the thing `flock` is tied to.
///
/// How a holder with no window arises, since that is the confusing case:
/// `cordial-run` has no run-until-the-window-closes path yet, so `--run` is a
/// hard timer and the launcher's default is a day. The launcher quitting is the
/// *ordinary* case under ADR-012 — the instance holds the claim, not the shell
/// — so a client routinely outlives the window that started it, gets reparented
/// to `systemd --user`, and keeps the profile for the rest of its timer.
///
/// Best-effort by construction: another user's `/proc/<pid>/fd` is not
/// readable, and a holder in a different mount namespace will not match.
/// **`None` means "could not tell", never "nobody holds it"** — the `flock` has
/// already established that somebody does, and a caller that reads `None` as
/// "free" would be inventing an answer.
fn holder_of(path: &Path) -> Option<Holder> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        // Our own descriptor is not open yet at the point `acquire` calls this,
        // but a caller probing a profile it already holds would find itself and
        // be told it is busy with no explanation. Cheaper to skip than explain.
        if pid == std::process::id() {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue; // Another user's process, or one that exited mid-scan.
        };
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path()).ok().as_deref() != Some(target.as_path()) {
                continue;
            }
            let command = std::fs::read(entry.path().join("cmdline"))
                .map(|raw| {
                    String::from_utf8_lossy(&raw).replace('\0', " ").trim().to_string()
                })
                .unwrap_or_default();
            let running_for = std::fs::metadata(entry.path())
                .and_then(|m| m.modified())
                .ok()
                .and_then(|start| start.elapsed().ok());
            return Some(Holder { pid, command, running_for });
        }
    }
    None
}

/// Why a profile could not be claimed.
///
/// Two variants rather than one string, because the caller has to tell them
/// apart and matching on message text is how that goes wrong later. `Busy` is
/// not a fault — it is the lock doing its job, reached by double-clicking
/// launch — and the interface owes that case an offer of another profile
/// rather than an error to dismiss.
#[derive(Debug)]
pub enum Error {
    /// The profile, and whichever process holds it if that could be determined.
    Busy(String, Option<Holder>),
    Unusable(String),
}

impl Error {
    /// What to print underneath a refusal in a terminal, where there is no
    /// profile switcher to offer and the reader is holding a command line.
    ///
    /// Separate from `Display` because the same `Error` reaches a dialog, which
    /// offers another profile and a "close it and launch" button instead — a
    /// paragraph of shell recipes in an `AdwAlertDialog` would be noise.
    ///
    /// It explains itself at some length on purpose. `cordial-run` took no lock
    /// at all until 2026-08-22, so contributors and agents have been running
    /// several clients against `default` for months without being stopped;
    /// this refusal is new to them and will read as a regression unless it says
    /// why it exists and what to do instead. The recipe is the one in
    /// AGENTS.md, quoted rather than referenced so that nobody has to go and
    /// find it mid-launch.
    pub fn advice(&self) -> Option<&'static str> {
        match self {
            Error::Busy(..) => Some(
                "A profile is one account's Roblox storage, and two clients writing one \
                 storage directory corrupt it (ADR-012). This is a refusal, not a crash.\n\n\
                 To run a second client alongside the first, give it a profile of its own:\n\
                 \n    cordial-run --profile <another-name> ...\n\n\
                 or a whole data root of its own, which is what this repository asks agents \
                 and test runs to do:\n\
                 \n    XDG_DATA_HOME=~/.cache/cordial-<yours> cordial-run ...\n\n\
                 To take this profile back instead, close the client holding it — named \
                 above, when Cordial could see which process that is.",
            ),
            Error::Unusable(_) => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Busy(name, holder) => {
                write!(f, "profile {name:?} is already open in another Cordial client")?;
                if let Some(holder) = holder {
                    write!(f, " (process {}", holder.pid)?;
                    if let Some(text) = holder.running_for_text() {
                        write!(f, ", running for {text}")?;
                    }
                    write!(f, ")")?;
                }
                write!(f, "; use a different profile, or close that one first")
            }
            Error::Unusable(message) => f.write_str(message),
        }
    }
}

/// Move a pre-ADR-012 layout into place, once.
///
/// Storage used to live at `cordial/instances/default`, which named a window
/// and contained a login, and `cordial-run` still writes there when nobody
/// tells it otherwise. The launcher does tell it otherwise — it points
/// `CORDIAL_FILES_DIR` at the profile — so without this the first launch from
/// the shell starts against an empty directory and presents as being logged out
/// for no reason. That is precisely the class of failure ADR-012 says the
/// migration exists to prevent, so the directory is moved rather than
/// abandoned.
///
/// Runs only when the old path exists and the new one does not, so it cannot
/// clobber a profile someone has already used.
///
/// **Deferred while a client is running.** The legacy layout was never locked
/// by anything, so a rename can land underneath a live engine that is holding
/// paths inside it — and on this developer's machine that has meant a client
/// signed in at the time. There is nothing to ask, so the only available check
/// is whether such a process exists at all; deferring costs one launch against
/// an empty profile, and getting it wrong costs a session.
/// Everything the Roblox engine keeps in `<profile>/data`, and what removing it
/// costs.
///
/// **Not "the cache", though that is most of it by size.** `data/` holds the
/// engine's downloaded assets and flag caches, and also its `LocalStorage`,
/// `rbx-storage.db` and `ClientSettings` -- state the engine wrote and expects
/// to find. Calling the control "clear cached data" and quietly deleting local
/// storage would be the kind of label this project treats as a lie, so the
/// caller says what it is.
///
/// What it does **not** touch: the sign-in, which lives in the desktop secret
/// service rather than here, and Cordial's own per-profile files -- window
/// geometry, plugin grants, enablement -- which sit beside `data/` rather than
/// inside it. Somebody who clears this stays signed in and keeps what they
/// allowed their plugins to do.
///
/// This exists because it is the only thing known to clear the freeze reported
/// on 2026-08-24, where a signed-in client reached Home and then presented
/// nothing. Moving `data/` aside fixed it twice running; **why** is still
/// unknown, and an attempt to narrow it to the 2.0 GB cache alone was invalid.
/// So the control removes the whole directory, which is the thing that was
/// actually measured to work, rather than a subset that was not.
pub fn engine_data_dir(profile_dir: &Path) -> PathBuf {
    profile_dir.join("data")
}

/// Bytes under `<profile>/data`, or 0 if it is absent or unreadable.
///
/// Best effort: this exists to put a number on a button, and a dialog that
/// refused to open because a directory walk hit a permission error would be
/// worse than one that under-reports.
pub fn engine_data_bytes(profile_dir: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                walk(&e.path(), total);
            } else {
                *total = total.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0;
    walk(&engine_data_dir(profile_dir), &mut total);
    total
}

/// Delete `<profile>/data`, returning what it freed.
///
/// **Renamed aside first, then deleted.** A half-removed `data/` is worse than
/// either state -- the engine would find some of its files and not others, and
/// this project has spent a day on a bug that presents as exactly that kind of
/// inconsistency. The rename is atomic within the profile, so an interrupted
/// call leaves either the old directory or none, never half of one.
pub fn clear_engine_data(profile_dir: &Path) -> Result<u64, String> {
    let data = engine_data_dir(profile_dir);
    if !data.exists() {
        return Ok(0);
    }
    let freed = engine_data_bytes(profile_dir);
    let aside = profile_dir.join("data.clearing");
    let _ = std::fs::remove_dir_all(&aside);
    std::fs::rename(&data, &aside).map_err(|e| format!("{}: {e}", data.display()))?;
    std::fs::remove_dir_all(&aside).map_err(|e| format!("{}: {e}", aside.display()))?;
    Ok(freed)
}

/// A size for a sentence.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", 1_000_000_000), ("MB", 1_000_000), ("kB", 1_000)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} bytes")
}

pub fn migrate_legacy_layout() -> Option<PathBuf> {
    let legacy = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cordial/instances/default");
    let target = root().join("default");
    if !legacy.is_dir() || target.exists() {
        return None;
    }
    if a_client_is_running() {
        println!(
            "  profiles: leaving {} where it is; a client is still running against it (ADR-012)",
            legacy.display()
        );
        return None;
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    match std::fs::rename(&legacy, &target) {
        Ok(()) => {
            // The legacy directory was created with the old umask, so tighten it
            // on the way in rather than inheriting a world-readable cookie.
            restrict_to_owner(&target);
            println!("  profiles: moved {} to {} (ADR-012)", legacy.display(), target.display());
            Some(target)
        }
        // A cross-device rename is the one plausible failure. Leaving the old
        // directory in place and saying so beats a half-copied login.
        Err(e) => {
            println!(
                "  profiles: could not move {} to {} ({e}); the old location is untouched",
                legacy.display(),
                target.display()
            );
            None
        }
    }
}

/// Whether any `cordial-run` is up, by asking `/proc` rather than by keeping a
/// record that could be stale. Only used to decide whether it is safe to move a
/// directory nothing has locked; a false negative costs a deferred migration.
fn a_client_is_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("comm")).is_ok_and(|comm| comm.trim() == "cordial-run")
    })
}

/// Make a profile directory readable only by its owner.
///
/// Roblox keeps its session cookie inside the profile, so the directory holds a
/// live credential even though Cordial itself never reads or handles one.
/// `create_dir_all` applies the process umask, which on a normal desktop yields
/// `0755` — world-readable, so any other account on the machine could take the
/// session. ADR-012 records this as the whole of Cordial's credential handling,
/// deliberately, and it has to hold whichever process creates the directory
/// first. The launcher now usually does.
///
/// Best-effort: a filesystem without Unix permissions is not a reason to refuse
/// to launch.
fn restrict_to_owner(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        if perms.mode() & 0o077 != 0 {
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `CORDIAL_PROFILE_ROOT` is process-wide and cargo runs a test binary's
    /// tests in parallel threads of one process, so two tests pointing it at
    /// different scratch directories will interleave and read each other's.
    /// Same local-per-file pattern `install.rs` and `profile_switcher.rs`
    /// already use for this crate's binary; this module's tests are compiled
    /// into the library's own separate test binary, so this mutex only has
    /// to cover this file's own tests, not theirs.
    static ENV: Mutex<()> = Mutex::new(());

    fn scratch(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!("cordial-shell-profile-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        std::env::set_var("CORDIAL_PROFILE_ROOT", &p);
        (p, guard)
    }

    #[test]
    fn a_name_cannot_escape_the_profile_root() {
        assert!(!is_valid_name("../../etc"));
        assert!(!is_valid_name("/absolute"));
        assert!(!is_valid_name("has/slash"));
        assert!(!is_valid_name(""));
        assert!(is_valid_name("default"));
        assert!(is_valid_name("alt_account-2"));
    }

    #[test]
    fn a_second_instance_cannot_take_a_held_profile() {
        let (_root, _g) = scratch("held");
        let first = acquire("default").expect("first instance takes the profile");
        let second = acquire("default");
        assert!(matches!(second, Err(Error::Busy(..))), "a second instance must be refused");
        assert!(second.unwrap_err().to_string().contains("already open"));
        drop(first);
    }

    #[test]
    fn a_profile_is_not_readable_by_other_users() {
        let (_root, _g) = scratch("perms");
        let claim = acquire("default").unwrap();
        let mode = std::fs::metadata(claim.profile_dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "profile must not be group- or world-accessible");
    }

    #[test]
    fn a_spawned_instance_holds_the_claim_after_the_launcher_lets_go() {
        // The point of `hand_to`, and the reason it is worth a test that spawns
        // a real process: the shell exits all the time while a client is still
        // running, and if the lock went with it the profile would be claimable
        // by a second instance mid-session. `sleep` stands in for cordial-run
        // because what is being tested is the descriptor, not the engine.
        let (_root, _g) = scratch("handoff");
        let claim = acquire("default").unwrap();

        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        claim.hand_to(&mut command);
        let mut child = command.spawn().expect("sleep is on PATH");

        // The launcher letting go is the whole scenario; without this the test
        // would pass on the parent's own lock and prove nothing.
        drop(claim);

        let refused = acquire("default");
        assert!(refused.is_err(), "the spawned instance must still hold the profile");

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_refusal_names_the_process_actually_holding_the_profile() {
        // The PID in that message is the entire recovery path for somebody who
        // cannot find a window to close, so it has to be the process with the
        // descriptor rather than the one that opened it. This is the same
        // hand-off shape as the test above precisely because that is where the
        // obvious implementation goes wrong: `/proc/locks` would name the
        // launcher, which by this point in the scenario has let go.
        let (_root, _g) = scratch("holder");
        let claim = acquire("default").unwrap();

        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        claim.hand_to(&mut command);
        let mut child = command.spawn().expect("sleep is on PATH");
        drop(claim);

        let Err(Error::Busy(_, holder)) = acquire("default") else {
            panic!("a held profile must be refused");
        };
        let holder = holder.expect("a holder in this process's own namespace is identifiable");
        assert_eq!(holder.pid, child.id(), "the refusal must name the descriptor's owner");
        assert!(holder.command.contains("sleep"), "{}", holder.command);

        // And the guard that keeps the "Close It and Launch" button honest:
        // `sleep` is not a client, so Cordial must neither offer to close it
        // nor do so if asked. A button that signals whatever holds a file open
        // is one that eventually signals something else.
        assert!(!holder.is_cordial());
        assert!(
            holder.ask_to_stop().is_err(),
            "Cordial must refuse to signal a process it did not start"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_claim_is_released_when_that_instance_exits() {
        // The other half: a lock nobody can ever release is worse than no lock,
        // because the recovery is to find and delete a file. `flock` is tied to
        // the open file description, so the kernel does this on exit — but only
        // if nothing else is holding the descriptor, which is what this checks.
        let (_root, _g) = scratch("released");
        let claim = acquire("default").unwrap();
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        claim.hand_to(&mut command);
        let mut child = command.spawn().unwrap();
        drop(claim);

        let _ = child.kill();
        let _ = child.wait();

        assert!(acquire("default").is_ok(), "the profile must be reusable once the instance is gone");
    }

    /// A stand-in for `cordial-run`: a second process that claims a profile
    /// through [`claim_for_instance`], exactly as the client does.
    ///
    /// **It has to be a real process, and it has to be this code.** The
    /// mechanism under test is a descriptor crossing an `exec` and a `flock`
    /// belonging to an open file description rather than to a process; neither
    /// survives being modelled in-process, and the failure this whole change
    /// fixes was precisely that the client's half was never run at all. `sleep`
    /// serves for the tests above because there what is tested is the
    /// descriptor; here what is tested is what the client *does* with it, so
    /// the test binary re-executes itself with a filter that selects this one
    /// ignored test. Ignored so an ordinary `cargo test` skips it, and it
    /// returns immediately when nobody set the environment that drives it.
    #[test]
    #[ignore = "spawned as a child by the claim tests; it is not a test on its own"]
    fn instance_helper() {
        let Ok(name) = std::env::var("CORDIAL_TEST_CLAIM") else { return };
        let report = PathBuf::from(std::env::var("CORDIAL_TEST_REPORT").unwrap());
        match claim_for_instance(&name) {
            Ok(claim) => {
                // Before the report, so the parent cannot look for the
                // grandchild's inherited descriptor before it exists.
                let grandchild = std::env::var_os("CORDIAL_TEST_GRANDCHILD")
                    .map(|_| std::process::Command::new("sleep").arg("10").spawn().unwrap());
                std::fs::write(&report, format!("claimed {}", claim.profile_dir().display()))
                    .unwrap();
                if grandchild.is_some() {
                    // Exit while it lives: whether the lock goes with this
                    // process is the question.
                    return;
                }
                // Long enough for the parent to make its assertions and kill
                // this, short enough that a parent which panicked first does
                // not leave a process behind for the rest of the day.
                std::thread::sleep(std::time::Duration::from_secs(20));
                drop(claim);
            }
            Err(e) => {
                let advice = e.advice().unwrap_or_default();
                std::fs::write(&report, format!("refused {e}\n{advice}")).unwrap();
            }
        }
    }

    /// Start [`instance_helper`] in a process of its own, optionally handing it
    /// `claim` the way `launch.rs` hands one to `cordial-run`.
    fn spawn_instance(
        tag: &str,
        name: &str,
        handed: Option<&Claim>,
        grandchild: bool,
    ) -> (std::process::Child, PathBuf) {
        let report = std::env::temp_dir().join(format!("cordial-shell-claim-report-{tag}"));
        let _ = std::fs::remove_file(&report);

        let mut command = std::process::Command::new(
            std::env::current_exe().expect("a test binary knows its own path"),
        );
        command
            .args(["--exact", "profile::tests::instance_helper", "--ignored", "--test-threads=1"])
            .env("CORDIAL_TEST_CLAIM", name)
            .env("CORDIAL_TEST_REPORT", &report)
            // `CORDIAL_PROFILE_ROOT` is inherited, which is what points the
            // child at this test's scratch directory rather than at a real one.
            .stdout(std::process::Stdio::null());
        if grandchild {
            command.env("CORDIAL_TEST_GRANDCHILD", "1");
        }
        match handed {
            Some(claim) => claim.hand_to(&mut command),
            // A hand-run client, so the marker must be absent however this test
            // process was started.
            None => {
                command.env_remove(HANDED_LOCK);
            }
        }
        let child = command.spawn().expect("the test binary re-executes");
        (child, report)
    }

    /// What the helper wrote, once it has written it.
    fn report(path: &Path) -> String {
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return text;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("the spawned instance never reported (looked for {})", path.display());
    }

    #[test]
    fn a_client_handed_a_lock_adopts_it_rather_than_refusing_itself() {
        // The crux of the whole mechanism. `flock` is tied to the open file
        // description, so a client that called `acquire` unconditionally would
        // open the lock file a second time and be refused by the lock it is
        // already holding — every shell launch would fail. Adoption is what
        // makes "claim when nobody handed you one" safe to say.
        let (_root, _g) = scratch("adopt");
        let claim = acquire("default").unwrap();
        let (mut child, report_path) = spawn_instance("adopt", "default", Some(&claim), false);
        drop(claim);

        let text = report(&report_path);
        assert!(text.starts_with("claimed"), "the handed-down lock must be adopted: {text}");

        // And it is really held, by the child, after the launcher let go.
        let Err(Error::Busy(_, holder)) = acquire("default") else {
            panic!("the instance must still hold the profile");
        };
        assert_eq!(
            holder.expect("a holder in our own namespace is identifiable").pid,
            child.id(),
            "the adopted claim must belong to the instance, not to the launcher"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_client_nobody_handed_a_lock_takes_one() {
        // The bug this change fixes: `cordial-run --profile X` is a documented
        // invocation and it took no lock at all, so four engines ran against
        // one profile on 2026-08-22 and not one was refused.
        let (_root, _g) = scratch("selfclaim");
        let (mut child, report_path) = spawn_instance("selfclaim", "default", None, false);
        assert!(report(&report_path).starts_with("claimed"));

        let Err(Error::Busy(_, holder)) = acquire("default") else {
            panic!("a hand-run client must hold its own profile");
        };
        assert_eq!(holder.expect("identifiable").pid, child.id());

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_second_hand_run_client_is_refused_and_told_what_to_do() {
        // Loud, specific, and not a crash. Everyone who has been running two
        // clients against `default` starts being refused here, so the refusal
        // has to carry its own explanation — see `Error::advice`.
        let (_root, _g) = scratch("refused");
        let held = acquire("default").expect("the first claim is free");
        let (mut child, report_path) = spawn_instance("refused", "default", None, false);

        let text = report(&report_path);
        assert!(text.starts_with("refused"), "{text}");
        assert!(text.contains("already open"), "{text}");
        assert!(text.contains(&format!("process {}", std::process::id())), "{text}");
        assert!(text.contains("XDG_DATA_HOME"), "{text}");
        assert!(text.contains("--profile"), "{text}");

        let _ = child.wait();
        drop(held);
    }

    #[test]
    fn a_clients_claim_dies_with_it_however_it_dies() {
        // Killed, not asked to stop: a lock that survives its holder is worse
        // than no lock, because the recovery is to find and delete a file.
        let (_root, _g) = scratch("clientexit");
        let (mut child, report_path) = spawn_instance("clientexit", "default", None, false);
        assert!(report(&report_path).starts_with("claimed"));
        let _ = child.kill();
        let _ = child.wait();

        acquire("default").expect("the profile must be free once the instance is gone");
    }

    #[test]
    fn a_plugin_sandbox_does_not_inherit_the_profile_lock() {
        // `hand_to` clears FD_CLOEXEC to get the lock across one exec, and the
        // client puts it back. Without that the descriptor keeps crossing:
        // `cordial_plugins::sandbox` execs `bwrap` and `deno`, and a sandbox
        // outliving the client would hold the profile with nothing running
        // against it — recoverable only by killing a process whose connection
        // to Roblox is not obvious from its command line.
        //
        // Handed a lock rather than taking one, because that is the only shape
        // in which the flag is clear to begin with: a lock this client opened
        // itself is `FD_CLOEXEC` from birth, so acquiring would test nothing.
        let (_root, _g) = scratch("cloexec");
        let claim = acquire("default").unwrap();
        let (mut child, report_path) = spawn_instance("cloexec", "default", Some(&claim), true);
        drop(claim);
        assert!(report(&report_path).starts_with("claimed"));
        let _ = child.wait(); // It exits immediately, leaving its `sleep` up.

        // The grandchild is still running; the claim must not be.
        acquire("default").expect("a sandbox must not keep the profile after the client exits");
    }

    #[test]
    fn a_stale_marker_is_not_believed() {
        // `CORDIAL_PROFILE_LOCK_FD` left in somebody's environment must not
        // make a client announce a lock it does not hold. Checked against
        // /proc/self/fd rather than trusted, so this falls through to taking a
        // real lock — which succeeds here, and would name the holder if one
        // existed.
        let (_root, _g) = scratch("stale");
        std::env::set_var(HANDED_LOCK, "1"); // stdout: open, and not the lock.
        let claim = claim_for_instance("default").expect("a free profile is still claimable");
        assert_eq!(claim.profile_dir(), dir("default").unwrap());
        assert!(
            std::env::var_os(HANDED_LOCK).is_none(),
            "the marker must be consumed, or a plugin sandbox inherits a meaningless one"
        );
        // And it is a real lock, not a wrapper around fd 1.
        assert!(matches!(acquire("default"), Err(Error::Busy(..))));
        drop(claim);
    }

    #[test]
    fn different_profiles_do_not_contend() {
        // Multi-instance is the point: two windows on two profiles is the
        // supported shape and must not block. Moved here from
        // `cordial_runtime::profile` when that module's duplicate lock was
        // deleted; the behaviour it guards is unchanged.
        let (_root, _g) = scratch("distinct");
        let a = acquire("main").unwrap();
        let b = acquire("alt").unwrap();
        assert_ne!(a.profile_dir(), b.profile_dir());
    }

    #[test]
    fn force_stop_refuses_a_process_it_did_not_start() {
        // Same guard as `ask_to_stop`, and worth its own test here rather than
        // trusting that the check was only copied correctly: `force_stop` is
        // the sharper signal, so a slip in this one does more damage.
        let holder = Holder { pid: 649889, command: "/usr/bin/grep -r something".into(), running_for: None };
        let err = holder.force_stop().expect_err("a stranger's process must be refused");
        assert!(err.contains("not a Cordial client"), "{err}");
    }

    #[test]
    fn cpu_ticks_reads_a_real_processs_scheduled_time() {
        // `sleep` barely runs, but it does get scheduled at least once to be
        // exec'd at all, so this only has to show the parse succeeds against a
        // real `/proc/<pid>/stat` line rather than assert a particular value.
        let mut child = std::process::Command::new("sleep").arg("2").spawn().expect("sleep is on PATH");
        let holder = Holder { pid: child.id(), command: "sleep".into(), running_for: None };
        assert!(holder.cpu_ticks().is_some(), "a running process must have readable /proc/<pid>/stat");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn cpu_ticks_is_none_for_a_pid_that_does_not_exist() {
        // The escalation logic in `window.rs` samples this across a wait and
        // has to treat "the process is gone" and "the process used no CPU" as
        // different things, so the missing case has to actually be `None`
        // rather than, say, `Some(0)`.
        let holder = Holder { pid: 0, command: String::new(), running_for: None };
        // pid 0 is never a real process from userspace's point of view, and
        // /proc/0 does not exist.
        assert!(holder.cpu_ticks().is_none());
    }

    #[test]
    fn wchan_names_where_a_sleeping_process_is_blocked() {
        // Best-effort, per the doc comment: some kernels refuse this. When it
        // answers at all the two placeholder values ("0", empty) must read as
        // `None` rather than as a wait channel literally named that, because a
        // caller reporting "blocked in 0" would be inventing a diagnosis, not
        // reading one.
        let mut child = std::process::Command::new("sleep").arg("2").spawn().expect("sleep is on PATH");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let holder = Holder { pid: child.id(), command: "sleep".into(), running_for: None };
        if let Some(w) = holder.wchan() {
            assert!(!w.is_empty() && w != "0", "{w}");
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
