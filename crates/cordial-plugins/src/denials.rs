//! A record of capabilities a plugin actually tried to use and was refused,
//! kept where a process outside the one that refused them can read it.
//!
//! [`crate::broker::Broker`] already tracks this — its own doc comment says
//! why: *"A plugin quietly failing because it lacks a capability is otherwise
//! indistinguishable from a plugin that is broken, and that distinction is the
//! difference between a two-minute fix and an afternoon."* But a `Broker` is
//! built fresh every launch, lives inside one plugin's serving thread in
//! `cordial-runtime`, and is gone the moment that process exits — so
//! `denials()` answered a question nobody outside that thread could ask.
//! Settings, where a person could actually read the answer, runs in
//! `cordial-shell`, a different process entirely. This file is the join: the
//! runtime records a denial here when it happens, and Settings reads the same
//! file later, the same division `grants` and `enablement` already draw
//! between "decided at runtime" and "shown in a window that is not running".
//!
//! ```json
//! { "discord-presence": ["presence.set"] }
//! ```
//!
//! **Cleared by granting, not by time.** A capability that was refused last
//! week and has since been granted is no longer telling the user anything true
//! — the plugin will succeed the next time it asks — so `grants::set` clears
//! the matching entry here when it turns a capability on. Nothing else clears
//! an entry: a denial that is still accurate should keep being shown, and
//! guessing at an expiry would trade one silent state for another.
//!
//! **Cumulative rather than timestamped.** The question this answers is
//! binary — has this plugin ever asked for something it does not have — and a
//! plugin that is denied the same capability on every launch would otherwise
//! grow an entry per launch for no reader's benefit. The one thing that does
//! reset the record is being granted, above.

use crate::capability::Capability;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// This profile's denial record.
pub fn path_in(profile_dir: &Path) -> PathBuf {
    profile_dir.join("plugin-denials.json")
}

/// Parse a denial document. An unknown capability name is dropped rather than
/// refused outright — unlike `grants::parse`, a stale name here cannot widen
/// what a plugin may do, so the safer failure is to lose one historical entry
/// rather than the whole file's worth of otherwise-legible ones.
fn parse(text: &str) -> Result<BTreeMap<String, BTreeSet<Capability>>, String> {
    let raw: BTreeMap<String, Vec<String>> =
        serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for (plugin, names) in raw {
        let caps: BTreeSet<Capability> = names.iter().filter_map(|n| Capability::parse(n)).collect();
        if !caps.is_empty() {
            out.insert(plugin, caps);
        }
    }
    Ok(out)
}

/// Load the record, or nothing at all. A missing or malformed file reads as
/// "nothing recorded yet" — the same posture `grants::load` takes, and for the
/// same reason: this file exists purely to be informative, and a read failure
/// must never look like a plugin misbehaving.
pub fn load(path: &Path) -> BTreeMap<String, BTreeSet<Capability>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    parse(&text).unwrap_or_default()
}

fn render(record: &BTreeMap<String, BTreeSet<Capability>>) -> String {
    let raw: BTreeMap<&str, Vec<&str>> = record
        .iter()
        .map(|(id, caps)| (id.as_str(), caps.iter().map(|c| c.name()).collect()))
        .collect();
    serde_json::to_string_pretty(&raw).expect("a set of capability names always serialises")
}

fn save(path: &Path, record: &BTreeMap<String, BTreeSet<Capability>>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, render(record))?;
    std::fs::rename(&tmp, path)
}

/// Record that `plugin` was refused `cap`. Idempotent — recording the same
/// denial twice leaves the file exactly as it was after the first time, so a
/// plugin retrying a call it does not have does not turn into repeated disk
/// writes for no new information; callers still ought to call this only once
/// per newly-observed denial rather than on every refusal, which is why
/// `cordial-runtime`'s serving loop tracks what it has already recorded for
/// its own plugin before calling this at all.
pub fn record(path: &Path, plugin: &str, cap: Capability) -> std::io::Result<()> {
    let mut rec = load(path);
    rec.entry(plugin.to_string()).or_default().insert(cap);
    save(path, &rec)
}

/// Clear one recorded denial, because `cap` has just been granted to `plugin`
/// and the record would otherwise keep claiming a refusal that is no longer
/// true. A no-op if nothing was recorded, so `grants::set` can call this
/// unconditionally on every grant rather than checking first.
pub fn clear(path: &Path, plugin: &str, cap: Capability) -> std::io::Result<()> {
    let mut rec = load(path);
    if let Some(caps) = rec.get_mut(plugin) {
        caps.remove(&cap);
        if caps.is_empty() {
            rec.remove(plugin);
        }
        return save(path, &rec);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-denials-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_file_records_nothing() {
        assert!(load(Path::new("/nonexistent/plugin-denials.json")).is_empty());
    }

    #[test]
    fn a_malformed_file_reads_as_nothing_recorded() {
        let dir = scratch("malformed");
        let path = path_in(&dir);
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_empty());
    }

    #[test]
    fn recording_a_denial_persists_it() {
        let dir = scratch("record");
        let path = path_in(&dir);
        record(&path, "discord-presence", Capability::PresenceSet).unwrap();
        let rec = load(&path);
        assert!(rec["discord-presence"].contains(&Capability::PresenceSet));
    }

    #[test]
    fn recording_the_same_denial_twice_changes_nothing() {
        let dir = scratch("idempotent");
        let path = path_in(&dir);
        record(&path, "p", Capability::Log).unwrap();
        record(&path, "p", Capability::Log).unwrap();
        assert_eq!(load(&path)["p"].len(), 1);
    }

    #[test]
    fn recording_two_capabilities_for_one_plugin_keeps_both() {
        let dir = scratch("two-caps");
        let path = path_in(&dir);
        record(&path, "p", Capability::Log).unwrap();
        record(&path, "p", Capability::PresenceSet).unwrap();
        let rec = load(&path);
        assert!(rec["p"].contains(&Capability::Log));
        assert!(rec["p"].contains(&Capability::PresenceSet));
    }

    #[test]
    fn clearing_a_denial_that_was_never_recorded_is_a_harmless_no_op() {
        let dir = scratch("clear-nothing");
        let path = path_in(&dir);
        clear(&path, "p", Capability::Log).unwrap();
        assert!(load(&path).is_empty());
    }

    #[test]
    fn granting_clears_only_the_capability_that_was_granted() {
        // The whole reason this exists apart from a bare "delete the plugin's
        // entry": a plugin denied two different capabilities and then granted
        // one of them should still show the other as refused.
        let dir = scratch("clear-one");
        let path = path_in(&dir);
        record(&path, "p", Capability::Log).unwrap();
        record(&path, "p", Capability::PresenceSet).unwrap();
        clear(&path, "p", Capability::Log).unwrap();
        let rec = load(&path);
        assert!(!rec["p"].contains(&Capability::Log));
        assert!(rec["p"].contains(&Capability::PresenceSet));
    }

    #[test]
    fn clearing_the_last_denial_drops_the_plugin_entirely() {
        let dir = scratch("clear-last");
        let path = path_in(&dir);
        record(&path, "p", Capability::Log).unwrap();
        clear(&path, "p", Capability::Log).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains('p'), "{text}");
        assert!(load(&path).is_empty());
    }

    #[test]
    fn a_denial_recorded_for_one_plugin_does_not_touch_another() {
        let dir = scratch("isolation");
        let path = path_in(&dir);
        record(&path, "a", Capability::Log).unwrap();
        record(&path, "b", Capability::PresenceSet).unwrap();
        clear(&path, "a", Capability::Log).unwrap();
        let rec = load(&path);
        assert!(!rec.contains_key("a"));
        assert!(rec["b"].contains(&Capability::PresenceSet));
    }
}
