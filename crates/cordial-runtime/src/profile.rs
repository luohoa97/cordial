//! Profiles: which one this instance runs, resolved through the shell's claim.
//!
//! ADR-012's profile lock lived here and in `cordial_shell::profile` at once,
//! because `cordial-runtime` depends on `cordial-shell` for `host_window` and
//! the reverse edge would cycle. The shell copy is the one that has to take the
//! claim (the launcher), and it grew holder identification the runtime never
//! had. Keeping two implementations of a lock that guards a stored login is
//! exactly the pair that drifts — so this module is now the runtime-only
//! process state (`set_active` / `active`) on top of the shell's lock, not a
//! second flock.
//!
//! See [ADR-012](../../../docs/adr/ADR-012-profiles-and-instances.md).

use std::path::PathBuf;
use std::sync::OnceLock;

pub use cordial_shell::profile::{
    acquire, dir, is_valid_name, list, migrate_legacy_layout, root, Claim, Error, Holder,
};

/// Historical name for [`Claim`]. The runtime never called `acquire` outside
/// tests; keep the alias so older call sites and docs stay readable.
pub type Lock = Claim;

/// The profile an instance runs when it was told nothing else.
///
/// Not arbitrary: `migrate_legacy_layout` lands pre-existing storage here, so
/// picking any other name would present as being logged out.
pub const DEFAULT_NAME: &str = "default";

/// The profile this instance is running, once something has said which.
///
/// One process runs one profile for its whole life — that is ADR-012's
/// definition of an instance, and the `flock` in [`acquire`] is what makes it
/// true rather than a convention — so this is a fact about the process and is
/// recorded once as one.
static ACTIVE: OnceLock<PathBuf> = OnceLock::new();

/// Record which profile this instance runs.
///
/// **The profile arrives as a command-line argument, and everything else lives
/// underneath it.** Flag overrides, plugin grants and each plugin's settings
/// are all resolved from this one directory, so a second argument naming any
/// of them would be a second source of truth for something already decided.
///
/// Refuses a second, different answer rather than taking it. Changing profile
/// under a running engine would mean two `appData` directories in one session,
/// which is the corruption ADR-012's lock exists to prevent — arriving by a
/// different door.
pub fn set_active(dir: PathBuf) -> Result<(), String> {
    // Create and tighten here as well as in `acquire`, because they are not the
    // same door. The launcher calls `acquire`, which does both; a hand-started
    // `cordial-run --profile <name>` calls only this, and so ran against a
    // directory `create_dir_all` had left at the umask's `0755`.
    let _ = std::fs::create_dir_all(&dir);
    restrict_to_owner(&dir);
    match ACTIVE.set(dir.clone()) {
        Ok(()) => Ok(()),
        Err(_) if ACTIVE.get() == Some(&dir) => Ok(()),
        Err(_) => Err(format!(
            "this instance already runs {}; a profile cannot be changed while the client is up",
            ACTIVE.get().expect("set failed, so it is set").display()
        )),
    }
}

/// The profile directory everything else in this process hangs off.
///
/// Falls back to [`DEFAULT_NAME`] for a `cordial-run` started by hand, which
/// has been told no profile and must not therefore write somewhere new — that
/// would look exactly like being logged out.
pub fn active() -> PathBuf {
    ACTIVE.get().cloned().unwrap_or_else(|| root().join(DEFAULT_NAME))
}

/// Make a profile directory readable only by its owner.
///
/// Same contract as the private helper in `cordial_shell::profile`: best-effort
/// `0700`, kept here because `set_active` is the door that does not go through
/// `acquire`.
fn restrict_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
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

    /// `CORDIAL_PROFILE_ROOT` is process-wide and cargo runs tests in parallel
    /// threads of one process, so two tests pointing it at different scratch
    /// directories will interleave and read each other's.
    static ENV: Mutex<()> = Mutex::new(());

    fn scratch(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!("cordial-runtime-profile-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        std::env::set_var("CORDIAL_PROFILE_ROOT", &p);
        (p, guard)
    }

    #[test]
    fn the_active_profile_is_decided_once_and_defaults_to_the_migrated_one() {
        // One test rather than three, because `ACTIVE` is a `OnceLock` and the
        // fallback can only be observed before anything has set it. Lock
        // behaviour lives in `cordial_shell::profile` tests now.
        let (_root, _g) = scratch("active");
        assert_eq!(
            active(),
            root().join(DEFAULT_NAME),
            "a client told nothing must use the profile the migration lands storage in, \
             or a hand-started run presents as being logged out"
        );

        let chosen = dir("alt_account").unwrap();
        set_active(chosen.clone()).unwrap();
        assert_eq!(active(), chosen);

        assert!(set_active(chosen.clone()).is_ok());

        let refused = set_active(dir("main").unwrap());
        assert!(refused.is_err(), "a second, different profile must be refused");
        assert_eq!(active(), chosen, "and the first answer must still stand");
    }
}
