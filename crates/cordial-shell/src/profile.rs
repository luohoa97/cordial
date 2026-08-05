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
//! **This is the same contract `cordial_runtime::profile` implements, written
//! twice.** That is not a design; it is where the dependency graph left it.
//! `cordial-runtime` depends on this crate for `host_window`, so this crate
//! cannot depend on `cordial-runtime` without a cycle, and the launcher is the
//! process that has to take the claim — `cordial-run` never calls its own
//! `profile` module at all today, so the runtime's copy is currently unreached
//! code. Two implementations of a lock that guards a credential is exactly the
//! kind of pair that drifts, so this one lives in the *library* half of
//! `cordial-shell` rather than in the binary: the runtime can adopt it and
//! delete its copy without anything moving a second time. Wording of the
//! refusal message is deliberately identical between the two so that a user
//! searching for it finds one answer.
//!
//! The precedent for reimplementing rather than linking is `flags_file.rs`,
//! which does the same for the flags path and says why in its own header.

use std::ffi::c_int;
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

extern "C" {
    fn flock(fd: c_int, operation: c_int) -> c_int;
    // Declared variadic because it is. Wrapping a variadic function in a
    // fixed-arity declaration is what makes `CORDIAL_TRACE=1` abort the engine,
    // and the lesson is cheap enough to apply to a two-line `fcntl` call.
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

/// `SIGTERM`, and `ESRCH` for the process having already gone. Both are stable
/// across every Linux ABI Cordial runs on.
const SIGTERM: c_int = 15;
const ESRCH: c_int = 3;

/// `LOCK_EX | LOCK_NB` from `sys/file.h`. Non-blocking on purpose: a second
/// launch should say so immediately rather than hang waiting for the first to
/// exit, which reads as the client failing to start.
const LOCK_EX_NB: c_int = 2 | 4;

/// `F_SETFD` from `fcntl.h`, and the empty flag set that clears `FD_CLOEXEC`.
const F_SETFD: c_int = 2;

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
    pub fn hand_to(&self, command: &mut std::process::Command) {
        let fd = self.file.as_raw_fd();
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
}
