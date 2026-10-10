//! Unpacking a plugin, on the assumption that it is hostile.
//!
//! A distribution archive is a `.tar.zst`: a tar of the plugin directory's
//! contents, zstd-compressed. Zstd rather than gzip for ratio and speed with a
//! mature Rust binding; tar rather than zip because zip's Unix mode bits are
//! carried inconsistently between producers, which would mean the same archive
//! unpacking differently depending on what wrote it. See
//! [ADR-014](../../../docs/adr/ADR-014-plugin-registry-and-unpacking.md).
//!
//! Everything below is written on the assumption that whoever produced the
//! archive wants to get out of the directory it is being unpacked into. The
//! rules, each of which refuses by name rather than skipping the entry:
//!
//! An entry is a regular file or a directory, and nothing else. A symlink or a
//! hard link is refused whether or not its target looks like it stays inside —
//! `plugin.json -> ../../../etc/passwd` is obvious, but `a -> .` followed by
//! `a/b` is the same attack written in two entries, and deciding which links
//! are safe means simulating the filesystem the archive is building. Refusing
//! all of them means never having to be right about that. Device nodes and
//! FIFOs are refused because a plugin has no use for one and creating one is
//! how an unpacker becomes interesting.
//!
//! A path has no `..` in it, is not absolute, and still lands inside the
//! destination once normalised. Setuid and setgid bits are refused rather than
//! stripped: nothing in a plugin needs one, and an archive carrying one is
//! telling you something about itself worth stopping for. Files are written
//! `0644` and directories `0755` regardless of what the archive asked for,
//! because Cordial never executes anything out of a plugin directory — it runs
//! `deno run` against the entry module — so an executable bit could only ever
//! be useful to something else.
//!
//! Both the number of entries and the total *uncompressed* size are capped.
//! Zstd compresses a few gigabytes of zeroes into a few hundred bytes, so the
//! size of the thing that was downloaded says nothing at all about the size of
//! the thing being written.
//!
//! The content hash is checked **before anything is decompressed**, so a
//! tampered download never reaches the tar parser. Extraction happens into a
//! dot-prefixed staging directory that `manifest::discover` will not look at,
//! and is renamed into place only once the whole archive has been read and the
//! manifest inside it has been checked against the index entry it claimed to
//! be. A failed install leaves nothing that discovery can find.

use crate::manifest;
use crate::registry::{ContentHash, Entry};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

/// How much an archive may be, whatever it says about itself.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entries: usize,
    pub max_total_bytes: u64,
}

impl Default for Limits {
    /// A plugin is source and a few assets. These are far above anything a
    /// plugin has needed and far below anything that would fill a disk before
    /// the refusal arrives; an archive that wants more than this wants
    /// discussing rather than a constant raised quietly.
    fn default() -> Self {
        Limits { max_entries: 4096, max_total_bytes: 64 * 1024 * 1024 }
    }
}

/// Why an archive was not unpacked. Every one of these is a refusal with a
/// name, because an unpacker that silently skips the entry it did not like
/// produces a plugin directory that is subtly not what was published, and the
/// person debugging it has nothing to go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    HashMismatch { expected: String, actual: String },
    AbsolutePath(String),
    ParentTraversal(String),
    EscapesRoot(String),
    EmptyPath,
    Symlink(String),
    HardLink(String),
    DeviceNode(String),
    Fifo(String),
    UnsupportedEntry(String),
    SetuidBit { path: String, mode: u32 },
    TooManyEntries { limit: usize },
    TooLarge { limit: u64 },
    NoManifest,
    ManifestMismatch(String),
    Malformed(String),
    Io(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::HashMismatch { expected, actual } => {
                write!(f, "the download hashes to {actual}, and the index says {expected}")
            }
            Refusal::AbsolutePath(p) => write!(f, "{p:?} is an absolute path"),
            Refusal::ParentTraversal(p) => write!(f, "{p:?} contains \"..\""),
            Refusal::EscapesRoot(p) => write!(f, "{p:?} lands outside the plugin directory"),
            Refusal::EmptyPath => f.write_str("an entry has no path"),
            Refusal::Symlink(p) => write!(f, "{p:?} is a symlink, and no plugin archive may hold one"),
            Refusal::HardLink(p) => write!(f, "{p:?} is a hard link, and no plugin archive may hold one"),
            Refusal::DeviceNode(p) => write!(f, "{p:?} is a device node"),
            Refusal::Fifo(p) => write!(f, "{p:?} is a FIFO"),
            Refusal::UnsupportedEntry(p) => write!(f, "{p:?} is neither a file nor a directory"),
            Refusal::SetuidBit { path, mode } => {
                write!(f, "{path:?} carries mode {mode:o}, which is setuid or setgid")
            }
            Refusal::TooManyEntries { limit } => write!(f, "the archive holds more than {limit} entries"),
            Refusal::TooLarge { limit } => {
                write!(f, "the archive unpacks to more than {limit} bytes")
            }
            Refusal::NoManifest => f.write_str("the archive has no plugin.json at its root"),
            Refusal::ManifestMismatch(what) => {
                write!(f, "the archive does not match what the index published: {what}")
            }
            Refusal::Malformed(what) => write!(f, "the archive is not readable: {what}"),
            Refusal::Io(what) => write!(f, "{what}"),
        }
    }
}

fn io_err(e: io::Error) -> Refusal {
    Refusal::Io(e.to_string())
}

/// Verify `archive` against `entry` and install it under `root`, returning the
/// installed directory.
///
/// `archive` is the bytes of the `.tar.zst` named by `entry.url`. Nothing here
/// fetches: downloading is Cordial's to do (ADR-007 — a plugin never holds the
/// channel), and keeping the fetch out of this function is what makes every
/// refusal above reachable from a test with no network.
pub fn install(archive: &[u8], entry: &Entry, root: &Path) -> Result<PathBuf, Refusal> {
    install_with(archive, entry, root, Limits::default())
}

pub fn install_with(
    archive: &[u8],
    entry: &Entry,
    root: &Path,
    limits: Limits,
) -> Result<PathBuf, Refusal> {
    // Before anything is decompressed, let alone written. Checking afterwards
    // would mean a tampered archive had already been through the tar parser and
    // had already put files on disk, and "we deleted them again" is a much
    // weaker statement than "they were never written".
    let actual = ContentHash::of(archive);
    if actual != entry.hash {
        return Err(Refusal::HashMismatch {
            expected: entry.hash.to_string(),
            actual: actual.to_string(),
        });
    }

    let staging = staging_path(root, &entry.id);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(io_err)?;

    let result = (|| {
        extract_into(archive, &staging, limits)?;
        check_manifest(&staging, entry)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    let target = root.join(&entry.id);
    match swap_into_place(&staging, &target) {
        Ok(()) => Ok(target),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

/// Unpack an archive the user supplied directly — a file they downloaded or
/// were sent — with no index entry to check it against.
///
/// [`install`] exists for the case ADR-014 designs for: an index told the user
/// what a release claims, and the archive is held to that claim before
/// anything is trusted. A locally supplied archive has no such claim to hold
/// it to — there is no index, so there is nothing for `plugin.json` to
/// disagree *with*. What does not change is everything [`extract_into`]
/// enforces regardless of an `Entry` ever existing: no symlink, no path that
/// escapes the destination, no setuid bit, the same entry and size caps. Only
/// the "does the archive match what was published" check is absent, because
/// there is no publication here to match.
///
/// Returns the parsed manifest alongside the installed directory so a caller
/// can show what the plugin requests **before** granting it anything — this
/// installs the code, not a capability. Nothing here writes to
/// `plugin-grants.json`; ADR-003's default deny holds exactly as it does for
/// a plugin copied into place by hand.
pub fn install_local(archive: &[u8], root: &Path) -> Result<(manifest::Plugin, PathBuf), Refusal> {
    install_local_with(archive, root, Limits::default())
}

pub fn install_local_with(
    archive: &[u8],
    root: &Path,
    limits: Limits,
) -> Result<(manifest::Plugin, PathBuf), Refusal> {
    // A nonce rather than an id: the id is not known until the manifest
    // inside the archive has been read, and it has to be read from
    // *somewhere* on disk to be read at all. The final id-named directory is
    // still only ever reached through `swap_into_place` below, once the
    // manifest is known good.
    let staging = staging_path(root, "local-install");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(io_err)?;

    let result = (|| {
        extract_into(archive, &staging, limits)?;
        read_manifest(&staging)
    })();
    let plugin = match result {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    let target = root.join(&plugin.manifest.id);
    match swap_into_place(&staging, &target) {
        Ok(()) => {
            // Re-parsed at its final location: `read_manifest` above ran
            // against the staging directory, and `Plugin::entry_path` is
            // relative to wherever `dir` says the plugin lives — a caller
            // resolving it from the returned value must get the real,
            // permanent directory, not the staging one this function is
            // about to have deleted the sibling of.
            let plugin = read_manifest(&target).map_err(|e| {
                let _ = std::fs::remove_dir_all(&target);
                e
            })?;
            Ok((plugin, target))
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

/// Remove an installed plugin's directory entirely.
///
/// Machine-level, matching the rest of this crate's split between "installed"
/// and "approved": this deletes the code from disk and nothing else.
/// `plugin-grants.json`, `plugin-enabled.json` and a plugin's own settings all
/// live inside a profile and are left exactly where they are, for the same
/// reason ADR-013 gives for surviving an uninstall — a stale document is
/// cheap, and deleting a user's saved configuration because they removed the
/// plugin that reads it is not a kindness. Reinstalling the same id later
/// finds its old grants and settings waiting, which is the intended
/// behaviour, not a leak to clean up.
pub fn uninstall(root: &Path, id: &str) -> Result<(), String> {
    if !manifest::is_valid_id(id) {
        return Err(format!("{id:?} is not a usable plugin id"));
    }
    let target = root.join(id);
    // `root.join(id)` cannot itself escape `root` once `is_valid_id` has
    // refused every character that could — no `/`, no `..` — but the
    // `is_dir` check below is what stops this from ever being asked to
    // remove a bare file or a symlink standing in for one, which is not what
    // "uninstall a plugin" should be able to do even if `id` were somehow
    // wrong.
    if !target.is_dir() {
        // Already gone is not a failure: a settings UI offering "remove" on
        // a plugin that raced its own removal should not have to distinguish
        // that from success.
        return Ok(());
    }
    std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))
}

/// Where a half-built install lives until it is whole.
///
/// The leading dot is the load-bearing part. [`install_with`] removes this
/// directory when it refuses, but a process killed mid-extraction removes
/// nothing at all, and what is left behind at that moment is a directory
/// holding a real `plugin.json` and a truncated entry module. `discover` skips
/// a name beginning with `.`, so the leftovers are inert rather than loadable —
/// and since `is_valid_id` forbids a dot, this name can never collide with a
/// plugin's own directory.
fn staging_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!(".{}-{}", id, nonce()))
}

/// Replace `target` with `staged`, keeping the old copy until the new one is
/// in place.
///
/// An upgrade that removed the old directory first would leave the plugin
/// absent for as long as the rename took, and absent permanently if the rename
/// failed. Renaming the old one aside means the failure case still has
/// something to put back.
fn swap_into_place(staged: &Path, target: &Path) -> Result<(), Refusal> {
    if !target.exists() {
        return std::fs::rename(staged, target).map_err(io_err);
    }
    let displaced = target.with_file_name(format!(
        ".{}-replaced-{}",
        target.file_name().and_then(|n| n.to_str()).unwrap_or("plugin"),
        nonce()
    ));
    std::fs::rename(target, &displaced).map_err(io_err)?;
    match std::fs::rename(staged, target) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&displaced);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&displaced, target);
            Err(io_err(e))
        }
    }
}

/// Enough to keep two installs running at once out of each other's way. Not a
/// security property: the staging directory is inside a root the user owns, and
/// the name only has to be unlikely to collide.
fn nonce() -> String {
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{pid}-{now}")
}

/// What the archive says it is has to be what the index said it was.
///
/// The index carries a plugin's capabilities and dependencies so the user can
/// be shown them before anything is downloaded. That is only honest if the
/// archive is then held to it: without this check an index entry could ask for
/// `log`, be approved for `log`, and unpack a manifest requesting
/// `assets.override`, and the approval the user gave would have been for a
/// different plugin than the one on disk.
/// Read and parse the `plugin.json` an extraction produced, with no index
/// entry to hold it to yet.
///
/// Split out of [`check_manifest`] so [`install_local`] — which has no
/// [`Entry`] to compare against, because there is no index behind a locally
/// supplied archive — can still get a real, parsed manifest rather than
/// re-reading the file itself and duplicating the two failure cases below.
fn read_manifest(dir: &Path) -> Result<manifest::Plugin, Refusal> {
    let path = dir.join("plugin.json");
    let text = std::fs::read_to_string(&path).map_err(|_| Refusal::NoManifest)?;
    manifest::parse(&text, dir).map_err(Refusal::ManifestMismatch)
}

fn check_manifest(dir: &Path, entry: &Entry) -> Result<(), Refusal> {
    let plugin = read_manifest(dir)?;
    if plugin.manifest.id != entry.id {
        return Err(Refusal::ManifestMismatch(format!(
            "it calls itself {:?}, published as {:?}",
            plugin.manifest.id, entry.id
        )));
    }
    match &plugin.version {
        Some(v) if v == &entry.version => {}
        Some(v) => {
            return Err(Refusal::ManifestMismatch(format!(
                "it calls itself version {v}, published as {}",
                entry.version
            )))
        }
        None => {
            return Err(Refusal::ManifestMismatch(
                "it declares no version, and a published plugin must".into(),
            ))
        }
    }
    if plugin.requested != entry.capabilities {
        return Err(Refusal::ManifestMismatch(format!(
            "it requests {:?}, published as requesting {:?}",
            plugin.requested.iter().map(|c| c.name()).collect::<Vec<_>>(),
            entry.capabilities.iter().map(|c| c.name()).collect::<Vec<_>>()
        )));
    }
    if plugin.dependencies != entry.dependencies {
        return Err(Refusal::ManifestMismatch(format!(
            "it depends on {:?}, published as depending on {:?}",
            plugin.dependencies.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            entry.dependencies.iter().map(|d| d.to_string()).collect::<Vec<_>>()
        )));
    }
    Ok(())
}

/// Decompress and unpack `archive` into `dest`, which must already exist and
/// should be empty.
///
/// Separate from [`install`] so the refusals can be tested against a directory
/// and a handmade archive, with no index entry and no hash to satisfy.
pub fn extract_into(archive_bytes: &[u8], dest: &Path, limits: Limits) -> Result<(), Refusal> {
    let decoder =
        zstd::stream::read::Decoder::new(io::Cursor::new(archive_bytes)).map_err(io_err)?;
    // A second, independent cap on the decompressed stream. The per-entry
    // budget below is computed from what each header declares, and this one is
    // computed from what actually came out of the decompressor, so an archive
    // whose headers disagree with its contents runs into one or the other.
    let mut capped = Capped::new(decoder, limits.max_total_bytes.saturating_add(SLACK));
    let mut archive = tar::Archive::new(&mut capped);

    // Decided before a byte is written, from its own pass over the archive.
    let prefix = wrapper_dir(archive_bytes, limits);

    let mut count = 0usize;
    let mut written = 0u64;
    let entries = archive.entries().map_err(|e| Refusal::Malformed(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| Refusal::Malformed(e.to_string()))?;

        count += 1;
        if count > limits.max_entries {
            return Err(Refusal::TooManyEntries { limit: limits.max_entries });
        }

        let header = entry.header().clone();
        let declared = entry.path().map_err(|e| Refusal::Malformed(e.to_string()))?.into_owned();
        let shown = declared.display().to_string();

        let kind = header.entry_type();
        if kind.is_symlink() {
            return Err(Refusal::Symlink(shown));
        }
        if kind.is_hard_link() {
            return Err(Refusal::HardLink(shown));
        }
        if kind.is_fifo() {
            return Err(Refusal::Fifo(shown));
        }
        if kind.is_character_special() || kind.is_block_special() {
            return Err(Refusal::DeviceNode(shown));
        }
        if !kind.is_file() && !kind.is_dir() {
            return Err(Refusal::UnsupportedEntry(shown));
        }

        let mode = header.mode().map_err(|e| Refusal::Malformed(e.to_string()))?;
        if mode & 0o6000 != 0 {
            return Err(Refusal::SetuidBit { path: shown, mode });
        }

        // `./` on its own is the archive's own root, which is `dest`. GNU tar
        // writes it first for `tar -C dir .`, which is the command
        // `plugins/README.md` gives for packaging a plugin -- so refusing it
        // as an entry with no path refused every archive made the documented
        // way. Only a directory: a *file* with no path is still malformed.
        if kind.is_dir() && declared.components().all(|c| matches!(c, Component::CurDir)) {
            continue;
        }
        let relative = safe_relative(&declared)?;
        // Applied after sanitising, never before: the traversal, absolute-path
        // and escape checks all run against what the archive actually declared,
        // and stripping only removes a component that survived them.
        let Some(relative) = strip_prefix(&relative, prefix.as_deref()) else {
            // The prefix directory's own entry. Nothing left to write.
            continue;
        };
        let out = dest.join(&relative);
        // Redundant given `safe_relative`, which only ever emits ordinary
        // components — and kept anyway. It is the check that still holds if
        // somebody later decides a `..` in the middle of a path is harmless
        // because it cancels out, which is true right up until it does not.
        // `within` is unit-tested directly for the same reason.
        if !within(dest, &out) {
            return Err(Refusal::EscapesRoot(shown));
        }

        if kind.is_dir() {
            std::fs::create_dir_all(&out).map_err(io_err)?;
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))
                .map_err(io_err)?;
            continue;
        }

        let size = header.size().map_err(|e| Refusal::Malformed(e.to_string()))?;
        if written.saturating_add(size) > limits.max_total_bytes {
            return Err(Refusal::TooLarge { limit: limits.max_total_bytes });
        }

        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        // `create_new` rather than `create`: every entry writes a file that did
        // not exist, because the staging directory started empty and no entry
        // may name a path another entry already took. Two entries for one path
        // is how an archive gets a checked file replaced by an unchecked one in
        // formats where the second write wins.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&out)
            .map_err(io_err)?;
        let copied = copy_capped(&mut entry, &mut file, limits.max_total_bytes - written)?;
        written += copied;
    }

    if capped.tripped {
        return Err(Refusal::TooLarge { limit: limits.max_total_bytes });
    }
    Ok(())
}

/// Room for tar's own overhead on top of the payload budget: a 512-byte header
/// per entry, up to 511 bytes of padding after each, and two zero blocks at the
/// end. Generous rather than exact — this cap is a backstop, and the per-entry
/// budget is the one that gives the precise answer.
const SLACK: u64 = 8 * 1024 * 1024;

/// The single directory every entry sits inside, when stripping it is the only
/// thing standing between this archive and a valid plugin.
///
/// `tar -czf x.tar.zst fps-flex/` produces `fps-flex/plugin.json`, and so does
/// every tarball GitHub generates for a tag. Refusing those with "the archive
/// has no plugin.json at its root" is technically accurate and useless: the
/// manifest is right there, one level down, and the person who packed it did
/// the obvious thing. So one wrapper directory is stripped, the way
/// `tar --strip-components=1` and every package manager that consumes a GitHub
/// tarball does.
///
/// **The rule is deliberately narrow, so that no archive which installs today
/// can change behaviour.** All three must hold:
///
/// - there is no `plugin.json` at the archive's own root, so this archive is
///   currently refused outright;
/// - every entry shares one first component, so nothing is being merged and no
///   file can collide with another that was previously distinct;
/// - after stripping it, a `plugin.json` appears at the root, so the strip
///   actually produces a plugin rather than burrowing hopefully.
///
/// Anything else returns `None` and the caller refuses exactly as before.
/// Reading the archive twice is affordable because it is already wholly in
/// memory and capped; the alternative is deciding the layout while half of it
/// is on disk.
fn wrapper_dir(archive_bytes: &[u8], limits: Limits) -> Option<PathBuf> {
    let decoder = zstd::stream::read::Decoder::new(io::Cursor::new(archive_bytes)).ok()?;
    let mut capped = Capped::new(decoder, limits.max_total_bytes.saturating_add(SLACK));
    let mut archive = tar::Archive::new(&mut capped);

    let mut first: Option<PathBuf> = None;
    let mut inner_manifest = false;
    let mut count = 0usize;

    for entry in archive.entries().ok()? {
        let entry = entry.ok()?;
        count += 1;
        if count > limits.max_entries {
            return None;
        }
        // Sanitised here too. A path this rejects is one the extraction is
        // about to refuse anyway, and it must not influence the prefix.
        let relative = safe_relative(&entry.path().ok()?).ok()?;

        if relative == Path::new("plugin.json") {
            return None; // Already a valid root. Nothing to strip.
        }

        let mut components = relative.components();
        let head = PathBuf::from(components.next()?.as_os_str());
        match &first {
            None => first = Some(head),
            Some(seen) if *seen == head => {}
            Some(_) => return None, // More than one top-level name.
        }
        if components.as_path() == Path::new("plugin.json") {
            inner_manifest = true;
        }
    }

    inner_manifest.then_some(first?)
}

/// `relative` with `prefix` removed, or `None` if nothing would be left.
fn strip_prefix(relative: &Path, prefix: Option<&Path>) -> Option<PathBuf> {
    let Some(prefix) = prefix else { return Some(relative.to_path_buf()) };
    let stripped = relative.strip_prefix(prefix).unwrap_or(relative);
    (!stripped.as_os_str().is_empty()).then(|| stripped.to_path_buf())
}

/// The relative path an entry may be written to, or why it may not.
fn safe_relative(path: &Path) -> Result<PathBuf, Refusal> {
    let shown = path.display().to_string();
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(part) => out.push(part),
            // `./plugin.json` is what several tars emit for an ordinary file
            // and means nothing; dropping it is not a normalisation with
            // consequences.
            Component::CurDir => {}
            Component::ParentDir => return Err(Refusal::ParentTraversal(shown)),
            Component::RootDir | Component::Prefix(_) => return Err(Refusal::AbsolutePath(shown)),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(Refusal::EmptyPath);
    }
    Ok(out)
}

/// Whether `path` is inside `root`, lexically.
///
/// Lexically on purpose: `canonicalize` would answer a question about files
/// that do not exist yet, and would follow a symlink to do it.
fn within(root: &Path, path: &Path) -> bool {
    let mut normalised = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                if !normalised.pop() {
                    return false;
                }
            }
            Component::CurDir => {}
            other => normalised.push(other),
        }
    }
    normalised.starts_with(root)
}

/// Copy at most `budget` bytes, refusing rather than truncating.
///
/// The header already said how big this entry is and that was checked against
/// the budget, so this only fires when the header lied. Truncating instead
/// would install a plugin that is quietly not the one that was published.
fn copy_capped(from: &mut impl Read, to: &mut impl Write, budget: u64) -> Result<u64, Refusal> {
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = from.read(&mut buf).map_err(io_err)?;
        if n == 0 {
            return Ok(total);
        }
        total += n as u64;
        if total > budget {
            return Err(Refusal::TooLarge { limit: budget });
        }
        to.write_all(&buf[..n]).map_err(io_err)?;
    }
}

/// A reader that stops at a byte count and records that it did.
///
/// The flag rather than a distinctive error, because the error would have to
/// travel out through the tar parser, which is entitled to turn it into
/// whatever it likes on the way — "unexpected end of archive" is what it
/// usually says, and that would report a zstd bomb as a corrupt file.
struct Capped<R> {
    inner: R,
    left: u64,
    tripped: bool,
}

impl<R: Read> Capped<R> {
    fn new(inner: R, limit: u64) -> Self {
        Capped { inner, left: limit, tripped: false }
    }
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            self.tripped = true;
            return Ok(0);
        }
        let want = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;
    use crate::registry::Index;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-unpack-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Compress a tar somebody built with [`tar::Builder`].
    fn compress(tar: Vec<u8>) -> Vec<u8> {
        zstd::stream::encode_all(io::Cursor::new(tar), 3).unwrap()
    }

    /// An ordinary, honest archive of a plugin directory.
    fn good_archive(manifest: &str) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "plugin.json", manifest.as_bytes(), 0o644);
        append_file(&mut b, "main.ts", b"console.log('hello');\n", 0o644);
        compress(b.into_inner().unwrap())
    }

    /// **The archive `plugins/README.md` tells an author to make installs.**
    /// `tar --zstd -cf x.tar.zst -C dir .` writes a `./` entry for the root
    /// before the files, and that entry used to be refused as having no path.
    #[test]
    fn an_archive_made_with_tar_dash_c_dot_installs() {
        let mut b = tar::Builder::new(Vec::new());
        append_dir(&mut b, "./");
        append_file(&mut b, "./plugin.json", MANIFEST.as_bytes(), 0o644);
        append_file(&mut b, "./main.ts", b"console.log('hello');\n", 0o644);
        let archive = compress(b.into_inner().unwrap());
        let root = scratch("install-dot-root");
        let (plugin, dir) = install_local(&archive, &root).expect("the documented packaging must install");
        assert_eq!(plugin.manifest.id, "demo");
        assert!(dir.join("main.ts").is_file());
    }

    #[test]
    fn a_file_with_no_path_is_still_refused() {
        let archive = archive_of(tar::EntryType::Regular, "./", 0o644);
        assert!(extract_into(&archive, &scratch("file-no-path"), Limits::default()).is_err());
    }

    fn append_dir(b: &mut tar::Builder<Vec<u8>>, path: &str) {
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(tar::EntryType::Directory);
        h.set_cksum();
        b.append_data(&mut h, path, io::empty()).unwrap();
    }

    fn append_file(b: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8], mode: u32) {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, path, data).unwrap();
    }

    /// One entry, built by hand so it can be as unpleasant as the test needs.
    fn archive_of(kind: tar::EntryType, path: &str, mode: u32) -> Vec<u8> {
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(mode);
        h.set_entry_type(kind);
        if kind.is_symlink() || kind.is_hard_link() {
            // Deliberately a target inside the plugin directory: the refusal
            // must not depend on the link looking dangerous.
            h.set_link_name("main.ts").unwrap();
        }
        h.set_path("placeholder").unwrap();
        h.set_cksum();
        let mut b = tar::Builder::new(Vec::new());
        b.append(&h, io::empty()).unwrap();
        let mut tar = b.into_inner().unwrap();
        rewrite_first_path(&mut tar, path);
        compress(tar)
    }

    /// Overwrite the first entry's stored path, checksum and all.
    ///
    /// `tar::Header::set_path` refuses `..` and a leading `/` outright, which is
    /// the right behaviour in a producer and useless in a test: the archives
    /// worth defending against were not written by this crate. Patching the name
    /// field and recomputing the checksum is precisely what a hostile producer
    /// does, so it is what the fixture does — building the archive with a
    /// well-behaved library and then asserting the unpacker is safe would be a
    /// test that could never fail.
    fn rewrite_first_path(tar: &mut [u8], path: &str) {
        assert!(path.len() < 100, "the fixture writes into the old-style name field");
        tar[0..100].fill(0);
        tar[..path.len()].copy_from_slice(path.as_bytes());
        // The checksum is computed with its own eight bytes read as spaces.
        tar[148..156].fill(b' ');
        let sum: u32 = tar[0..512].iter().map(|b| *b as u32).sum();
        tar[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    fn extract(tag: &str, archive: &[u8]) -> Result<PathBuf, Refusal> {
        let dir = scratch(tag);
        extract_into(archive, &dir, Limits::default())?;
        Ok(dir)
    }

    const MANIFEST: &str = r#"{"id":"demo","name":"Demo","version":"1.0.0",
        "entry":"main.ts","capabilities":["log"]}"#;

    fn entry_for(archive: &[u8]) -> crate::registry::Entry {
        let text = format!(
            r#"{{"format":1,"plugins":[{{"id":"demo","name":"Demo","version":"1.0.0",
               "capabilities":["log"],"dependencies":{{}},
               "url":"https://x.invalid/demo-1.0.0.tar.zst","hash":"{}"}}]}}"#,
            ContentHash::of(archive)
        );
        Index::parse_unverified(&text).unwrap().entries.remove(0)
    }

    #[test]
    fn an_ordinary_archive_unpacks() {
        // The control for every refusal below: the same shape of archive, with
        // nothing wrong with it, has to arrive on disk.
        let dir = extract("good", &good_archive(MANIFEST)).unwrap();
        assert!(dir.join("plugin.json").is_file());
        assert_eq!(std::fs::read_to_string(dir.join("main.ts")).unwrap().trim(), "console.log('hello');");
        let mode = std::fs::metadata(dir.join("main.ts")).unwrap().permissions().mode();
        // The host umask may remove group/other read bits (for example 0077),
        // but no archive may add execute bits or make a plugin file writable
        // by another user. Those are the properties the unpacker promises.
        assert_eq!(mode & 0o111, 0, "nothing in a plugin is ever executed directly");
        assert_eq!(mode & 0o022, 0, "plugin files must not be writable by others");
        assert_ne!(mode & 0o400, 0, "the installing user must be able to read the file");
    }

    #[test]
    fn a_parent_traversal_is_refused() {
        assert_eq!(
            extract("dotdot", &archive_of(tar::EntryType::Regular, "../evil.ts", 0o644)),
            Err(Refusal::ParentTraversal("../evil.ts".into()))
        );
    }

    #[test]
    fn a_path_that_escapes_only_after_normalisation_is_refused() {
        // Every prefix of this looks like it stays inside. The refusal has to
        // be about the components, not about how the string begins.
        let a = archive_of(tar::EntryType::Regular, "a/b/../../../etc/passwd", 0o644);
        assert!(matches!(extract("normalised", &a), Err(Refusal::ParentTraversal(_))));
    }

    #[test]
    fn an_absolute_path_is_refused() {
        let a = archive_of(tar::EntryType::Regular, "/etc/cron.d/evil", 0o644);
        assert!(matches!(extract("absolute", &a), Err(Refusal::AbsolutePath(_))));
    }

    #[test]
    fn a_symlink_is_refused_even_pointing_inside() {
        // `a -> .` followed by `a/b` escapes using two entries neither of which
        // looks wrong on its own, so no link is allowed at all.
        let a = archive_of(tar::EntryType::Symlink, "link.ts", 0o644);
        assert!(matches!(extract("symlink", &a), Err(Refusal::Symlink(_))));
    }

    #[test]
    fn a_hard_link_is_refused() {
        let a = archive_of(tar::EntryType::Link, "hard.ts", 0o644);
        assert!(matches!(extract("hardlink", &a), Err(Refusal::HardLink(_))));
    }

    #[test]
    fn a_device_node_is_refused() {
        for kind in [tar::EntryType::Char, tar::EntryType::Block] {
            let a = archive_of(kind, "dev/null", 0o644);
            assert!(matches!(extract("device", &a), Err(Refusal::DeviceNode(_))));
        }
    }

    #[test]
    fn a_fifo_is_refused() {
        let a = archive_of(tar::EntryType::Fifo, "pipe", 0o644);
        assert!(matches!(extract("fifo", &a), Err(Refusal::Fifo(_))));
    }

    #[test]
    fn a_setuid_bit_is_refused_rather_than_stripped() {
        // Stripping it would install the archive anyway. An archive asking for
        // setuid is saying something about itself worth stopping for.
        let a = archive_of(tar::EntryType::Regular, "helper", 0o4755);
        assert!(matches!(extract("setuid", &a), Err(Refusal::SetuidBit { .. })));
        let a = archive_of(tar::EntryType::Regular, "helper", 0o2755);
        assert!(matches!(extract("setgid", &a), Err(Refusal::SetuidBit { .. })));
    }

    #[test]
    fn too_many_entries_are_refused() {
        let mut b = tar::Builder::new(Vec::new());
        for i in 0..40 {
            append_file(&mut b, &format!("f{i}.ts"), b"x", 0o644);
        }
        let archive = compress(b.into_inner().unwrap());
        let dir = scratch("entries");
        let limits = Limits { max_entries: 8, ..Limits::default() };
        assert_eq!(
            extract_into(&archive, &dir, limits),
            Err(Refusal::TooManyEntries { limit: 8 })
        );
    }

    #[test]
    fn a_compressed_bomb_is_refused_on_its_uncompressed_size() {
        // Four megabytes of zeroes compress to a few hundred bytes, which is
        // the whole problem: nothing about the download says how much will be
        // written.
        let payload = vec![0u8; 4 * 1024 * 1024];
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "big.bin", &payload, 0o644);
        let archive = compress(b.into_inner().unwrap());
        assert!(
            archive.len() < 4096,
            "the fixture is only interesting if it compresses well; it is {} bytes",
            archive.len()
        );
        let dir = scratch("bomb");
        let limits = Limits { max_total_bytes: 1024 * 1024, ..Limits::default() };
        assert_eq!(
            extract_into(&archive, &dir, limits),
            Err(Refusal::TooLarge { limit: 1024 * 1024 })
        );
        assert!(
            !dir.join("big.bin").exists(),
            "the refusal has to come before the bytes land"
        );
    }

    #[test]
    fn within_refuses_a_path_that_climbs_out() {
        // `safe_relative` means this cannot be reached through `extract_into`
        // as it stands, and it is tested directly because it is the check that
        // survives somebody deciding a `..` in the middle of a path cancels
        // out harmlessly.
        let root = Path::new("/plugins/demo");
        assert!(within(root, Path::new("/plugins/demo/main.ts")));
        assert!(within(root, Path::new("/plugins/demo/a/../main.ts")));
        assert!(!within(root, Path::new("/plugins/demo/../other/main.ts")));
        assert!(!within(root, Path::new("/plugins/demo/a/../../other")));
        assert!(!within(root, Path::new("/etc/passwd")));
    }

    #[test]
    fn a_tampered_download_is_refused_before_anything_is_written() {
        // The assertion names `HashMismatch` and must stay that specific.
        // Remove the hash check and this archive is still refused — zstd
        // notices its own corrupt frame and the refusal comes back as
        // `Malformed("Data corruption detected")` — so an assertion that
        // accepted any refusal would pass with the check gone, and the check
        // is the whole point. That very reading nearly went in: this test was
        // committed with the check disabled by a mutation run, and the failure
        // was read as the test being too strict rather than the code being
        // broken.
        let good = good_archive(MANIFEST);
        let entry = entry_for(&good);
        let mut tampered = good.clone();
        *tampered.last_mut().unwrap() ^= 0xff;

        let root = scratch("hash");
        let e = install(&tampered, &entry, &root).unwrap_err();
        assert!(matches!(e, Refusal::HashMismatch { .. }), "{e}");
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            0,
            "not even a staging directory should exist"
        );
    }

    #[test]
    fn an_install_lands_at_the_plugin_id_and_is_discoverable() {
        let good = good_archive(MANIFEST);
        let entry = entry_for(&good);
        let root = scratch("install");
        let dir = install(&good, &entry, &root).unwrap();
        assert_eq!(dir, root.join("demo"));
        let found = manifest::discover(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.id, "demo");
    }

    #[test]
    fn an_archive_asking_for_more_than_the_index_published_is_refused() {
        // The index is what the user was shown and approved. If the archive
        // could request something else, the approval was for a different
        // plugin than the one that ends up on disk.
        let sneaky = good_archive(
            r#"{"id":"demo","name":"Demo","version":"1.0.0","entry":"main.ts",
                "capabilities":["log","assets.override"]}"#,
        );
        let mut entry = entry_for(&sneaky);
        entry.hash = ContentHash::of(&sneaky);
        let root = scratch("mismatch");
        let e = install(&sneaky, &entry, &root).unwrap_err();
        assert!(matches!(e, Refusal::ManifestMismatch(_)), "{e}");
        assert!(!root.join("demo").exists());
        assert!(manifest::discover(&root).is_empty(), "and nothing is left for discovery");
    }

    #[test]
    fn a_crash_mid_install_leaves_nothing_discovery_can_find() {
        // `install` clears its staging directory when it refuses, so the test
        // below only ever exercises the tidy path. A process killed part way
        // through clears nothing, and the dot prefix is the only thing standing
        // between that and Cordial loading half a plugin on the next launch.
        let root = scratch("crashed");
        let staged = staging_path(&root, "demo");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("plugin.json"), MANIFEST).unwrap();
        // No main.ts: this is what an interrupted extraction looks like.
        assert!(
            manifest::discover(&root).is_empty(),
            "a staging directory must not be loadable as a plugin"
        );
    }

    #[test]
    fn a_refused_install_leaves_nothing_behind_at_all() {
        // The reason extraction is staged at all. A half-written plugin that
        // discovery loads is worse than no plugin, because it looks installed.
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "plugin.json", MANIFEST.as_bytes(), 0o644);
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_link_name("main.ts").unwrap();
        h.set_path("shortcut.ts").unwrap();
        h.set_cksum();
        b.append(&h, io::empty()).unwrap();
        let archive = compress(b.into_inner().unwrap());

        let mut entry = entry_for(&archive);
        entry.hash = ContentHash::of(&archive);
        let root = scratch("staged");
        assert!(matches!(install(&archive, &entry, &root), Err(Refusal::Symlink(_))));
        assert!(manifest::discover(&root).is_empty());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn an_upgrade_replaces_the_old_directory_whole() {
        let root = scratch("upgrade");
        let first = good_archive(MANIFEST);
        install(&first, &entry_for(&first), &root).unwrap();
        std::fs::write(root.join("demo/leftover.ts"), "stale").unwrap();

        let next_manifest = MANIFEST.replace("1.0.0", "1.1.0");
        let next = good_archive(&next_manifest);
        let mut entry = entry_for(&next);
        entry.version = semver::Version::new(1, 1, 0);
        entry.hash = ContentHash::of(&next);
        install(&next, &entry, &root).unwrap();

        assert!(
            !root.join("demo/leftover.ts").exists(),
            "an upgrade must not leave the previous version's files behind"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "and no staging left over");
    }

    #[test]
    fn a_single_wrapper_directory_is_stripped() {
        // `tar -czf x.tar.zst fps-flex/` and every GitHub tag tarball look like
        // this. Refusing them was accurate and useless -- reported as "plugin
        // installation is broken, it says plugin root not found, but it has a
        // root file".
        let mut b = tar::Builder::new(Vec::new());
        append_dir(&mut b, "fps-flex/");
        append_file(&mut b, "fps-flex/plugin.json", MANIFEST.as_bytes(), 0o644);
        append_file(&mut b, "fps-flex/main.ts", b"export default {}", 0o644);
        let archive = compress(b.into_inner().unwrap());

        let root = scratch("wrapper-stripped");
        let (_manifest, dir) = install_local(&archive, &root).expect("one wrapper is stripped");
        assert!(dir.join("plugin.json").is_file(), "the manifest lands at the root");
        assert!(dir.join("main.ts").is_file(), "and so does everything beside it");
        assert!(!dir.join("fps-flex").exists(), "the wrapper itself is gone");
    }

    #[test]
    fn a_root_manifest_is_never_second_guessed() {
        // The guard that keeps this change from touching any archive that
        // already worked: a root plugin.json means no stripping is considered
        // at all, even where a sibling directory shares the only other name.
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "plugin.json", MANIFEST.as_bytes(), 0o644);
        append_file(&mut b, "vendor/thing.ts", b"export default {}", 0o644);
        let archive = compress(b.into_inner().unwrap());

        let root = scratch("root-manifest-untouched");
        let (_m, dir) = install_local(&archive, &root).expect("installs as before");
        assert!(dir.join("plugin.json").is_file());
        assert!(dir.join("vendor/thing.ts").is_file(), "the subdirectory survives intact");
    }

    #[test]
    fn two_top_level_names_are_not_merged() {
        // Stripping here would silently fuse two trees, and a file from one
        // could take the path of a file from the other. Refuse instead.
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "one/plugin.json", MANIFEST.as_bytes(), 0o644);
        append_file(&mut b, "two/main.ts", b"export default {}", 0o644);
        let archive = compress(b.into_inner().unwrap());

        let root = scratch("two-tops");
        assert!(matches!(install_local(&archive, &root), Err(Refusal::NoManifest)));
    }

    #[test]
    fn burrowing_further_than_one_level_is_refused() {
        // One wrapper, not "keep going until a manifest turns up".
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "a/b/plugin.json", MANIFEST.as_bytes(), 0o644);
        let archive = compress(b.into_inner().unwrap());

        let root = scratch("too-deep");
        assert!(matches!(install_local(&archive, &root), Err(Refusal::NoManifest)));
    }

    #[test]
    fn a_wrapper_does_not_smuggle_a_hostile_entry_past_the_checks() {
        // The refusals run on the declared path, before any stripping, so a
        // wrapper cannot be used to launder one.
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "fps-flex/plugin.json", MANIFEST.as_bytes(), 0o644);
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_link_name("/etc/passwd").unwrap();
        b.append_data(&mut h, "fps-flex/sneaky.ts", std::io::empty()).unwrap();
        let archive = compress(b.into_inner().unwrap());

        let root = scratch("wrapper-symlink");
        assert!(matches!(install_local(&archive, &root), Err(Refusal::Symlink(_))));
    }

    #[test]
    fn install_local_lands_at_the_plugin_id_with_no_index_involved() {
        let good = good_archive(MANIFEST);
        let root = scratch("install-local");
        let (plugin, dir) = install_local(&good, &root).unwrap();
        assert_eq!(dir, root.join("demo"));
        assert_eq!(plugin.manifest.id, "demo");
        assert_eq!(plugin.requested, [Capability::Log].into_iter().collect());
        // The returned manifest resolves against its real, permanent
        // directory rather than the staging one — a caller building the
        // entry path from it must not be handed somewhere already deleted.
        assert_eq!(plugin.entry_path().unwrap(), root.join("demo/main.ts"));

        let found = manifest::discover(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.id, "demo");
    }

    #[test]
    fn install_local_still_refuses_a_hostile_archive() {
        // No index entry to check does not mean no checking at all —
        // `extract_into`'s own defences apply exactly as they do to `install`.
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "plugin.json", MANIFEST.as_bytes(), 0o644);
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_link_name("main.ts").unwrap();
        h.set_path("shortcut.ts").unwrap();
        h.set_cksum();
        b.append(&h, io::empty()).unwrap();
        let archive = compress(b.into_inner().unwrap());

        let root = scratch("install-local-hostile");
        assert!(matches!(install_local(&archive, &root), Err(Refusal::Symlink(_))));
        assert!(manifest::discover(&root).is_empty());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0, "nothing staged should be left behind");
    }

    #[test]
    fn install_local_refuses_an_archive_with_no_manifest_at_all() {
        let mut b = tar::Builder::new(Vec::new());
        append_file(&mut b, "main.ts", b"console.log('hi');", 0o644);
        let archive = compress(b.into_inner().unwrap());

        let root = scratch("install-local-no-manifest");
        assert!(matches!(install_local(&archive, &root), Err(Refusal::NoManifest)));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn uninstalling_removes_the_directory_and_nothing_else_in_root() {
        let root = scratch("uninstall");
        let good = good_archive(MANIFEST);
        install(&good, &entry_for(&good), &root).unwrap();
        std::fs::create_dir_all(root.join("other-plugin")).unwrap();
        std::fs::write(root.join("other-plugin/plugin.json"), r#"{"id":"other-plugin","entry":"m.ts"}"#).unwrap();

        uninstall(&root, "demo").unwrap();

        assert!(!root.join("demo").exists());
        assert!(root.join("other-plugin").exists(), "uninstall must not touch a plugin it was not asked about");
        assert!(manifest::discover(&root).iter().all(|p| p.manifest.id != "demo"));
    }

    #[test]
    fn uninstalling_a_plugin_that_is_already_gone_is_not_an_error() {
        let root = scratch("uninstall-missing");
        assert!(uninstall(&root, "never-installed").is_ok());
    }

    #[test]
    fn uninstalling_refuses_an_id_that_is_not_a_usable_plugin_id() {
        // `root.join(id)` is exactly the join `settings.rs` warns about
        // trusting on an unchecked id — refused here for the same reason,
        // before it ever reaches `remove_dir_all`.
        let root = scratch("uninstall-bad-id");
        for bad in ["..", "../../etc", "a/b", "/etc"] {
            assert!(uninstall(&root, bad).is_err(), "{bad:?} should be refused");
        }
    }
}
