//! What a plugin declares about itself.
//!
//! `plugin.json`, beside the plugin's entry module:
//!
//! ```json
//! {
//!   "id": "fps-tweaks",
//!   "name": "FPS Tweaks",
//!   "version": "1.2.0",
//!   "entry": "main.ts",
//!   "capabilities": ["flags.read", "flags.write"],
//!   "dependencies": { "cordial-multi-instance": "^1.0.0" }
//! }
//! ```
//!
//! Capabilities are **requested**, not granted. A manifest asking for something
//! is the start of a conversation with the user, not the end of one — nothing
//! here decides what a plugin gets, and a manifest that asks for everything is
//! not thereby entitled to anything.
//!
//! An unrecognised capability name is an error rather than something to skip
//! quietly. Skipping would mean a plugin built against a newer Cordial appears
//! to install correctly and then behaves strangely, which is a much worse
//! failure than refusing to load it.
//!
//! **`dependencies` names other Cordial plugins and nothing else.** It is not
//! npm's field under another roof: a plugin written in TypeScript may well need
//! both a Cordial plugin and a JavaScript package, and one key cannot honestly
//! carry both. A `deno.json` beside this file is the JS runtime's business and
//! Cordial neither reads nor validates it. See
//! [ADR-014](../../../docs/adr/ADR-014-plugin-registry-and-unpacking.md).

use crate::capability::Capability;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The module to run, if this plugin runs anything.
    ///
    /// **Optional, and its absence is the whole signal that this plugin is
    /// data** — a texture pack, a flag preset, a set of preferences — rather
    /// than something with code in it
    /// ([ADR-021](../../../docs/adr/ADR-021-everything-is-a-plugin.md)). There
    /// is deliberately no `type` key saying which sort of thing this is: two
    /// facts that can disagree eventually do, and the disagreement here would
    /// be a plugin declaring itself data while shipping an entry module, or an
    /// entry module that never runs because a key says it should not.
    ///
    /// It is also what the install prompt is gated on. A plugin with nothing
    /// to run and nothing to reach gets no prompt at all, because a prompt
    /// that appears for every import is answered yes by everybody by the third
    /// one, and then it is not protecting anything. See
    /// [`crate::consent`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entry: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// A semantic version, `major.minor.patch`.
    ///
    /// Optional, and deliberately so. Every plugin installed before versions
    /// existed has no such key, and making it required would have made
    /// [`discover`] refuse all of them at once — which presents to a user as
    /// every plugin they had silently vanishing, with the directories still
    /// sitting there looking correct. An unversioned plugin still loads; it
    /// simply cannot be published to an index or depended upon, and the
    /// resolver says so by name rather than guessing a version for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Other **Cordial plugins** this one needs, by id, each with a
    /// requirement. Never npm packages; see the module note.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    /// The settings this plugin wants asked, which Cordial renders as a
    /// preferences page (ADR-020).
    ///
    /// **Its presence is the entire signal that the plugin has a page**, which
    /// is why there is no companion `has-preferences` key and no capability
    /// meaning the same thing. Two facts that can disagree eventually do, and
    /// the disagreement here would be a gear button that opens nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferences: Vec<crate::preferences::Declaration>,
}

/// A version requirement on another plugin, with the operator spelled out.
///
/// Exactly two forms are accepted: `=1.2.0` for that version and nothing else,
/// and `^1.2.0` for anything compatible with it. A bare `1.2.0` is refused
/// rather than assigned a meaning, because the two ecosystems a plugin author
/// is most likely to have come from disagree about what it means — npm reads it
/// as exact, Cargo reads it as caret — and a requirement that means opposite
/// things to the author and to the resolver is worse than one that will not
/// parse. The error names both forms so the fix is to type one character.
///
/// Matching is delegated to `semver::VersionReq` rather than reimplemented,
/// including its rule that a requirement without a pre-release does not match a
/// pre-release version. Working out what `^0.2.3` means is exactly the sort of
/// thing to take from a crate that has already been argued about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    text: String,
    req: VersionReq,
}

impl Requirement {
    pub fn parse(text: &str) -> Result<Self, String> {
        let rest = text
            .strip_prefix('^')
            .or_else(|| text.strip_prefix('='))
            .ok_or_else(|| {
                format!(
                    "{text:?} does not say which versions it means; write \"={text}\" for that \
                     version exactly, or \"^{text}\" for versions compatible with it"
                )
            })?;
        // Insisting the remainder is a whole version is what rules out `^1.2`,
        // `>=1.0` and comma-separated lists in one place. The requirement
        // language is small on purpose: every operator in it is an operator a
        // user has to understand before they can tell what an install will do.
        Version::parse(rest)
            .map_err(|e| format!("{text:?} is not a version requirement ({e})"))?;
        let req = VersionReq::parse(text)
            .map_err(|e| format!("{text:?} is not a version requirement ({e})"))?;
        Ok(Requirement { text: text.to_string(), req })
    }

    pub fn matches(&self, version: &Version) -> bool {
        self.req.matches(version)
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// One entry of a manifest's `dependencies`, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub id: String,
    pub req: Requirement,
}

impl Dependency {
    pub fn new(id: &str, req: &str) -> Result<Self, String> {
        if !is_valid_id(id) {
            return Err(format!("{id:?} is not a usable plugin id, so nothing can depend on it"));
        }
        Ok(Dependency { id: id.to_string(), req: Requirement::parse(req).map_err(|e| format!("dependency on {id:?}: {e}"))? })
    }
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.id, self.req)
    }
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub requested: BTreeSet<Capability>,
    /// The parsed `version`, absent for a plugin that declares none.
    pub version: Option<Version>,
    /// The parsed `dependencies`, in id order so an install plan built from
    /// two runs of the same manifest is the same plan.
    pub dependencies: Vec<Dependency>,
}

impl Plugin {
    /// The entry module, resolved inside the plugin's own directory.
    ///
    /// Rejects anything that escapes it. A manifest is attacker-controlled input
    /// as far as this is concerned — it arrives with the plugin — and `"entry":
    /// "../../../etc/shadow"` must not resolve.
    /// Whether this plugin contains anything to run.
    ///
    /// A property read off the manifest, never a category the manifest
    /// declares. Everything is a plugin; some plugins happen to contain code,
    /// and that is the only distinction that exists — which is what lets a
    /// texture pack grow an entry module in version 2 without changing what it
    /// is, keeping its id, its directory and the user's settings for it.
    pub fn has_code(&self) -> bool {
        !self.manifest.entry.is_empty()
    }

    pub fn entry_path(&self) -> Result<PathBuf, String> {
        if self.manifest.entry.is_empty() {
            return Err(format!(
                "{:?} declares no entry module, so there is nothing to run",
                self.manifest.id
            ));
        }
        let entry = Path::new(&self.manifest.entry);
        if entry.is_absolute() || entry.components().any(|c| c.as_os_str() == "..") {
            return Err(format!("entry {:?} must be a path inside the plugin directory", self.manifest.entry));
        }
        Ok(self.dir.join(entry))
    }
}

/// Where plugins are installed.
/// Plugin folders loaded from wherever they happen to be, for development.
///
/// **Each entry is one plugin's own folder -- the one with `plugin.json` in it
/// -- and not a folder that plugins are kept in.** That distinction is the
/// whole feature. `plugin_root` and `system_plugin_dir` below are both
/// *collections*: Cordial reads every subdirectory and each one is a plugin.
/// This is the other thing entirely, and it is what "load unpacked" means
/// elsewhere -- you point at the thing you are working on, in the checkout
/// where you are working on it, and it loads.
///
/// Getting that backwards would mean somebody has to make a containing
/// directory to hold their one plugin, and put their working copy inside it,
/// which is the packaging step this exists to remove.
///
/// `CORDIAL_UNPACKED_PLUGINS`, colon-separated in the shape of `PATH`. The
/// launcher sets it from Developer mode; nothing sets it otherwise, so an
/// ordinary run pays one `var_os` miss.
///
/// Nothing here is trusted more for having been named explicitly. A folder
/// listed here still parses through [`parse`], still starts with no grants at
/// all (ADR-003's default deny), and still has to be granted whatever it asks
/// for. The only step skipped is the copy into the plugin root -- not consent,
/// not the manifest checks, not the id rules.
pub fn unpacked_dirs() -> Vec<PathBuf> {
    let Some(raw) = std::env::var_os("CORDIAL_UNPACKED_PLUGINS") else {
        return Vec::new();
    };
    std::env::split_paths(&raw).filter(|p| !p.as_os_str().is_empty()).collect()
}

/// Parse the plugin in each of [`unpacked_dirs`], skipping what does not.
///
/// Separate from [`discover`] rather than folded into it, because they answer
/// different questions: `discover` answers "what is under this root", and
/// every caller -- the tests, `marketplace::installed` -- relies on the answer
/// being confined to the root it passed. Returning folders from elsewhere
/// would make all of them quietly wrong.
///
/// A folder that does not parse is reported and skipped, as in `discover`.
/// Being named by a developer is a reason to say so loudly, not a reason to
/// load something broken.
pub fn discover_unpacked() -> Vec<Plugin> {
    let mut found = Vec::new();
    for dir in unpacked_dirs() {
        let path = dir.join("plugin.json");
        match std::fs::read_to_string(&path) {
            Err(e) => println!("  plugin: unpacked {} could not be read ({e})", path.display()),
            Ok(text) => match parse(&text, &dir) {
                Ok(p) => {
                    println!("  plugin {}: loaded unpacked from {}", p.manifest.id, dir.display());
                    found.push(p);
                }
                Err(e) => println!("  plugin: unpacked {} is unusable ({e})", path.display()),
            },
        }
    }
    found
}

pub fn plugin_root() -> PathBuf {
    std::env::var_os("CORDIAL_PLUGIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .unwrap_or_else(std::env::temp_dir)
                .join("cordial/plugins")
        })
}

/// Where first-party plugins ship: read-only, installed alongside the binary.
///
/// `/app/share/cordial/plugins` inside the Flatpak; `$CORDIAL_SYSTEM_PLUGIN_DIR`
/// overrides it for a distribution that packages Cordial somewhere else, and
/// for the tests.
///
/// **This is the one definition.** `cordial_runtime::flags::system_plugin_dir`
/// used to hold its own copy of the path and the variable name, and the shell
/// has to know the same answer to list built-in plugins beside user ones --
/// three copies of one path is the pair that drifts and only one of them gets
/// fixed, which is the argument `is_valid_id` below already makes about path
/// checks. `flags.rs` now calls this.
pub fn system_plugin_root() -> PathBuf {
    if let Some(explicit) = std::env::var_os("CORDIAL_SYSTEM_PLUGIN_DIR") {
        return PathBuf::from(explicit);
    }
    // **`/app` is Flatpak's prefix and nothing else's.** This returned it
    // unconditionally, which was correct while the Flatpak was the only way to
    // install Cordial and became wrong the day the deb, rpm, Arch package and
    // AppImage started building -- on all four, the built-in plugins are
    // installed under the real prefix and this looked for them somewhere that
    // does not exist, so the settings window would list none of them.
    //
    // Derived from the running binary rather than compiled in, because an
    // AppImage is unpacked to a different path on every launch and a fixed
    // prefix cannot be right for it. `…/bin/cordial-shell` gives `…/share/
    // cordial/plugins`, which is correct for /usr, for /usr/local, for /app and
    // for whatever temporary directory an AppImage mounted itself at.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(|bin| bin.parent()) {
            let beside = prefix.join("share/cordial/plugins");
            if beside.is_dir() {
                return beside;
            }
        }
    }
    // Flatpak last and still present: inside the sandbox the check above
    // already finds `/app/share/cordial/plugins`, and this keeps a build that
    // somehow runs the binary from elsewhere behaving as it always did.
    PathBuf::from("/app/share/cordial/plugins")
}

/// Whether a string may be used as a plugin id.
///
/// An id names directories — the plugin's own installed directory, and the one
/// its settings live in inside a profile — and it is the key the broker, the
/// event registry and the grants file all index by. Keeping it to boring
/// characters is the whole reason `<profile>/plugins/<id>/settings.json` cannot
/// be talked into being somewhere else: `..`, `/` and a leading `/` all fail
/// here, so nothing downstream has to sanitise a path it was handed.
///
/// One definition on purpose. `settings.rs` calls this rather than writing a
/// second copy, because two path checks guarding one directory is the pair that
/// drifts and only one of them gets fixed.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn parse(text: &str, dir: &Path) -> Result<Plugin, String> {
    let manifest: Manifest = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if manifest.id.is_empty() {
        return Err("id must not be empty".into());
    }
    if !is_valid_id(&manifest.id) {
        return Err(format!(
            "id {:?} may only contain letters, digits, dashes and underscores",
            manifest.id
        ));
    }
    let mut requested = BTreeSet::new();
    for name in &manifest.capabilities {
        match Capability::parse(name) {
            Some(c) => {
                requested.insert(c);
            }
            None => return Err(format!("unknown capability {name:?}")),
        }
    }
    // Refused with the plugin rather than pruned, the same as an unknown
    // capability above. A plugin whose page is quietly missing the one row it
    // needed installs looking correct and then behaves strangely, and the
    // author debugs their own code for it.
    crate::preferences::check_all(&manifest.preferences)?;
    let version = match &manifest.version {
        Some(v) => Some(
            Version::parse(v).map_err(|e| format!("version {v:?} is not a semantic version ({e})"))?,
        ),
        None => None,
    };
    // `BTreeMap` iterates in key order, so the dependency list is sorted
    // without sorting it — which is what makes two runs of the resolver over
    // the same manifest produce the same install order.
    let mut dependencies = Vec::new();
    for (id, req) in &manifest.dependencies {
        dependencies.push(Dependency::new(id, req)?);
    }
    Ok(Plugin { manifest, dir: dir.to_path_buf(), requested, version, dependencies })
}

/// Every plugin under `root`, one subdirectory each.
///
/// A plugin that fails to parse is reported and skipped rather than aborting
/// discovery: one bad manifest should not stop every other plugin from loading.
///
/// A plugin id must be unique across the whole root. This matters beyond
/// tidiness: the event registry (ADR-006) namespaces a plugin's declared
/// event types by its id, and grants and the broker index by id too, so two
/// on-disk plugins claiming the same id would let the second one silently
/// inherit whatever was approved for, or later declared by, the first. The
/// second claimant is reported and skipped, the same way an unparseable
/// manifest is — first one in the sorted directory listing wins, which keeps
/// discovery deterministic rather than dependent on filesystem enumeration
/// order.
///
/// **A directory whose name begins with `.` is not a plugin and is not looked
/// at.** `unpack` builds an install in a dot-prefixed sibling of the finished
/// directory and renames it into place only once it is whole, and the whole
/// point of that is defeated if discovery can see the half-written copy: an
/// interrupted install would leave a directory holding a real `plugin.json` and
/// a truncated entry module, and Cordial would load it. `is_valid_id` already
/// refuses a `.`, so no directory skipped here could ever have been a plugin
/// under its own name.
pub fn discover(root: &Path) -> Vec<Plugin> {
    let mut found = Vec::new();
    let mut seen_ids = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        })
        .collect();
    dirs.sort();

    for dir in dirs {
        let path = dir.join("plugin.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match parse(&text, &dir) {
            Ok(p) => {
                if !seen_ids.insert(p.manifest.id.clone()) {
                    println!(
                        "  plugin: {} claims id {:?}, already used by another plugin directory; skipping",
                        path.display(),
                        p.manifest.id
                    );
                    continue;
                }
                found.push(p)
            }
            Err(e) => println!("  plugin: {} is not loadable ({e})", path.display()),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/plugins/example")
    }

    #[test]
    fn a_manifest_parses_and_requests_capabilities() {
        let p = parse(
            r#"{"id":"fps","name":"FPS","entry":"main.ts","capabilities":["flags.read","log"]}"#,
            &dir(),
        )
        .unwrap();
        assert_eq!(p.manifest.id, "fps");
        assert!(p.requested.contains(&Capability::FlagsRead));
        assert!(p.requested.contains(&Capability::Log));
    }

    #[test]
    fn an_unknown_capability_is_refused_rather_than_skipped() {
        // Skipping would let a plugin built for a newer Cordial appear to
        // install and then misbehave.
        let e = parse(
            r#"{"id":"x","entry":"m.ts","capabilities":["flags.read","process.spawn"]}"#,
            &dir(),
        )
        .unwrap_err();
        assert!(e.contains("process.spawn"), "{e}");
    }

    #[test]
    fn an_entry_cannot_escape_the_plugin_directory() {
        for bad in ["../../../etc/shadow", "/etc/shadow", "sub/../../out.ts"] {
            let p = parse(
                &format!(r#"{{"id":"x","entry":{},"capabilities":[]}}"#, serde_json::to_string(bad).unwrap()),
                &dir(),
            )
            .unwrap();
            assert!(p.entry_path().is_err(), "{bad} should have been refused");
        }
    }

    #[test]
    fn a_normal_entry_resolves_inside_the_directory() {
        let p = parse(r#"{"id":"x","entry":"src/main.ts","capabilities":[]}"#, &dir()).unwrap();
        assert_eq!(p.entry_path().unwrap(), dir().join("src/main.ts"));
    }

    #[test]
    fn ids_are_restricted_to_boring_characters() {
        assert!(parse(r#"{"id":"../evil","entry":"m.ts"}"#, &dir()).is_err());
        assert!(parse(r#"{"id":"","entry":"m.ts"}"#, &dir()).is_err());
        assert!(parse(r#"{"id":"ok-name_1","entry":"m.ts"}"#, &dir()).is_ok());
    }

    #[test]
    fn an_id_can_never_be_a_path() {
        // `settings.rs` joins an id onto a profile directory, so this check is
        // load-bearing beyond tidiness: every one of these would escape the
        // plugin's own namespace if it were ever accepted as an id.
        for bad in ["..", ".", "../other", "a/b", "/etc", "a.b", ""] {
            assert!(!is_valid_id(bad), "{bad:?} must not be a usable plugin id");
        }
        assert!(is_valid_id("flag-inspector"));
        assert!(is_valid_id("ok-name_1"));
    }

    #[test]
    fn capabilities_may_be_omitted_entirely() {
        let p = parse(r#"{"id":"quiet","entry":"m.ts"}"#, &dir()).unwrap();
        assert!(p.requested.is_empty());
    }

    #[test]
    fn a_duplicate_plugin_id_across_directories_is_refused_not_merged() {
        // The event registry namespaces by plugin id (ADR-006); if two
        // directories could both present themselves as "flag-manager", the
        // second would inherit the first's declared event types and grants
        // just by claiming the same string. Discovery has to make that
        // impossible before anything downstream ever sees an id.
        let root = std::env::temp_dir().join("cordial-manifest-duplicate-id-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("aaa-first")).unwrap();
        std::fs::create_dir_all(root.join("zzz-second")).unwrap();
        std::fs::write(
            root.join("aaa-first/plugin.json"),
            r#"{"id":"flag-manager","entry":"main.ts","capabilities":["log"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("zzz-second/plugin.json"),
            r#"{"id":"flag-manager","entry":"main.ts","capabilities":["flags.write"]}"#,
        )
        .unwrap();

        let found = discover(&root);
        assert_eq!(found.len(), 1, "the duplicate must be skipped, not merged or duplicated");
        // Sorted directory order means "aaa-first" is discovered before
        // "zzz-second", so its request set (log only) is the one that wins.
        assert!(found[0].requested.contains(&Capability::Log));
        assert!(!found[0].requested.contains(&Capability::FlagsWrite));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_version_and_dependencies_parse() {
        let p = parse(
            r#"{"id":"a","entry":"m.ts","version":"1.2.3",
                "dependencies":{"b":"^1.0.0","c":"=2.0.0"}}"#,
            &dir(),
        )
        .unwrap();
        assert_eq!(p.version.unwrap(), Version::new(1, 2, 3));
        assert_eq!(p.dependencies.len(), 2);
        assert_eq!(p.dependencies[0].id, "b");
        assert!(p.dependencies[0].req.matches(&Version::new(1, 4, 0)));
        assert!(!p.dependencies[0].req.matches(&Version::new(2, 0, 0)));
        assert!(p.dependencies[1].req.matches(&Version::new(2, 0, 0)));
        assert!(!p.dependencies[1].req.matches(&Version::new(2, 0, 1)));
    }

    #[test]
    fn a_bare_version_requirement_is_refused_rather_than_guessed() {
        // npm reads "1.2.0" as exact and Cargo reads it as caret. Picking
        // either silently means an author from the other ecosystem writes a
        // requirement that means the opposite of what they intended, and
        // nothing ever tells them.
        let e = parse(
            r#"{"id":"a","entry":"m.ts","dependencies":{"b":"1.2.0"}}"#,
            &dir(),
        )
        .unwrap_err();
        assert!(e.contains("=1.2.0") && e.contains("^1.2.0"), "{e}");
    }

    #[test]
    fn a_requirement_language_larger_than_two_operators_is_refused() {
        for bad in [">=1.0.0", "~1.2.0", "*", "^1.2", "^1.0.0, <2.0.0", "latest"] {
            let text = format!(
                r#"{{"id":"a","entry":"m.ts","dependencies":{{"b":{}}}}}"#,
                serde_json::to_string(bad).unwrap()
            );
            assert!(parse(&text, &dir()).is_err(), "{bad:?} should not be a requirement");
        }
    }

    #[test]
    fn a_dependency_id_is_held_to_the_same_rules_as_a_plugin_id() {
        // A dependency id becomes a directory name at install time, by way of
        // the index. Checking it here means the resolver never carries a
        // string that `plugin_root().join(id)` could not safely take.
        assert!(parse(
            r#"{"id":"a","entry":"m.ts","dependencies":{"../evil":"^1.0.0"}}"#,
            &dir()
        )
        .is_err());
    }

    #[test]
    fn a_plugin_without_a_version_still_loads() {
        // Every plugin installed before versions existed has no such key.
        // Refusing them would present as every plugin the user had silently
        // vanishing, which is a far worse failure than one that cannot be
        // published to an index.
        let p = parse(r#"{"id":"old","entry":"m.ts"}"#, &dir()).unwrap();
        assert!(p.version.is_none());
        assert!(p.dependencies.is_empty());
    }

    #[test]
    fn a_staging_directory_is_not_discovered_as_a_plugin() {
        // `unpack` builds an install in a dot-prefixed sibling and renames it
        // into place whole. Delete the dot-prefix filter in `discover` and an
        // interrupted install becomes a loadable plugin with a truncated entry
        // module — which is the exact failure staging exists to prevent.
        let root = std::env::temp_dir().join("cordial-manifest-staging-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".half-written-1234")).unwrap();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(
            root.join(".half-written-1234/plugin.json"),
            r#"{"id":"half-written","entry":"main.ts","capabilities":["log"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("real/plugin.json"),
            r#"{"id":"real","entry":"main.ts","capabilities":["log"]}"#,
        )
        .unwrap();

        let found = discover(&root);
        assert_eq!(found.len(), 1, "only the finished plugin should be found");
        assert_eq!(found[0].manifest.id, "real");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
