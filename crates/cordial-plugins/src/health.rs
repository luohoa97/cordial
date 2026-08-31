//! The last thing that went wrong with each plugin, for the settings window.
//!
//! **A plugin that fails is currently invisible, and that is the gap.** If its
//! process cannot start -- no Deno on the machine, an `entry` naming a file
//! that is not there, a syntax error -- or if it exits on its own mid-session,
//! the only trace is a `println!` that a packaged launch has no terminal for
//! and a line in `plugin.log` that nobody thinks to open. In Settings the row
//! looks exactly like a plugin that is working and simply has not done
//! anything yet.
//!
//! Same shape as [`crate::denials`], deliberately: a small JSON file in the
//! profile, written by the runtime and read by the shell, because those are two
//! processes and a file is the only thing they both already agree on. It is
//! per profile for the same reason grants are (ADR-013) -- a plugin can fail in
//! one profile's environment and be fine in another's, and reporting the first
//! against the second would be a lie about the profile you are looking at.
//!
//! **Only the most recent failure per plugin is kept.** A history would need
//! rotation and a cap and a policy about what to discard, which is the scope
//! `plugin_host::append_plugin_log` already declines to take on for the same
//! reason. What a person needs in a settings window is "is this thing broken,
//! and what did it say" -- and the answer to that is the last one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What went wrong, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// One line, as it would be shown to a person. Not a backtrace.
    pub message: String,
    /// Seconds since the Unix epoch, so the shell can say "just now" or a date
    /// without this module owning a time format.
    pub when: u64,
}

/// Every plugin that has failed since it last started cleanly.
pub type Record = BTreeMap<String, Failure>;

/// Where the record lives for `profile_dir`.
pub fn path_in(profile_dir: &Path) -> PathBuf {
    profile_dir.join("plugin-health.json")
}

/// Read the record, or an empty one.
///
/// **A malformed file reads as "nothing has failed" rather than as an error.**
/// This is the diagnostic; making the diagnostic itself able to fail loudly
/// would be the same inversion `append_plugin_log` avoids, and the worst case
/// is a settings window that does not warn about something it could have.
pub fn load(path: &Path) -> Record {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(path: &Path, record: &Record) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(record).unwrap_or_else(|_| "{}".into());
    // Written aside and renamed, like `denials::save` and
    // `flag_document::write`: the shell reads this file while the runtime
    // writes it, and a half-written document would read as "nothing has
    // failed" -- which is the one wrong answer this module must not give.
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, format!("{text}\n"))?;
    std::fs::rename(&tmp, path)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Record that `plugin` failed, replacing whatever it last said.
pub fn record(path: &Path, plugin: &str, message: &str) -> std::io::Result<()> {
    let mut rec = load(path);
    rec.insert(plugin.to_string(), Failure { message: message.to_owned(), when: now() });
    save(path, &rec)
}

/// Forget `plugin`'s failure, because it has just started cleanly.
///
/// **Clearing is as important as recording.** A warning that stays after the
/// thing is fixed teaches people to ignore warnings, which costs more than the
/// one it was pointing at -- and with hot reload the ordinary case is a plugin
/// that fails, gets edited, and works, several times a minute.
///
/// Writes nothing when there was nothing to clear, so a healthy launch does not
/// touch the disk once per plugin.
pub fn clear(path: &Path, plugin: &str) -> std::io::Result<()> {
    let mut rec = load(path);
    if rec.remove(plugin).is_none() {
        return Ok(());
    }
    save(path, &rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("cordial-health-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        path_in(&dir)
    }

    #[test]
    fn a_failure_round_trips() {
        let p = scratch("round");
        record(&p, "fps-flex", "Deno is not installed").unwrap();
        let rec = load(&p);
        assert_eq!(rec["fps-flex"].message, "Deno is not installed");
        assert!(rec["fps-flex"].when > 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// The newest failure replaces the older one rather than accumulating.
    #[test]
    fn only_the_most_recent_failure_is_kept() {
        let p = scratch("recent");
        record(&p, "a", "first").unwrap();
        record(&p, "a", "second").unwrap();
        let rec = load(&p);
        assert_eq!(rec.len(), 1);
        assert_eq!(rec["a"].message, "second");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// **A warning that outlives the problem is worse than none.** With hot
    /// reload a plugin fails, gets edited and works again several times a
    /// minute, so clearing has to actually clear.
    #[test]
    fn a_clean_start_clears_the_warning() {
        let p = scratch("clear");
        record(&p, "a", "boom").unwrap();
        record(&p, "b", "bang").unwrap();
        clear(&p, "a").unwrap();
        let rec = load(&p);
        assert!(!rec.contains_key("a"), "a's failure must be gone");
        assert!(rec.contains_key("b"), "b's must not be");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// Clearing a plugin that never failed writes nothing at all.
    #[test]
    fn clearing_nothing_does_not_create_a_file() {
        let p = scratch("noop");
        clear(&p, "never-failed").unwrap();
        assert!(!p.exists(), "a healthy launch must not create the file");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A corrupt file reads as "nothing has failed".
    ///
    /// The diagnostic must not itself be able to break the page it appears on.
    #[test]
    fn a_malformed_file_is_not_an_error() {
        let p = scratch("corrupt");
        std::fs::write(&p, "{ this is not json").unwrap();
        assert!(load(&p).is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
