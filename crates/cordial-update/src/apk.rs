//! Getting something out of an APK, on the assumption that it is hostile.
//!
//! ADR-015: "[ADR-014](../../../docs/adr/ADR-014-plugin-registry-and-unpacking.md)'s
//! extraction rules apply here in full and were written for exactly this shape
//! of problem — an APK is a zip, and every refusal in that list is about zips."
//! So this is `cordial_plugins::unpack`'s list, applied to a zip, and every one
//! of them refuses by name rather than skipping the entry: an extractor that
//! silently steps over what it did not like produces a directory that is subtly
//! not what was published, and whoever debugs it has nothing to go on.
//!
//! ## Where a zip differs from a tar, and what that costs
//!
//! ADR-014 chose `.tar.zst` for plugins partly *because* zip carries Unix mode
//! bits in an extension field that producers populate inconsistently. That was
//! a reason not to pick zip; it is not an escape from checking, because the APK
//! format is not ours to choose. Two consequences, both stated here rather than
//! discovered later:
//!
//! **An entry with no Unix mode at all is treated as a regular file.** There is
//! nothing else it can be treated as, and refusing every mode-less entry would
//! refuse most APKs — many zip producers write no Unix attributes whatsoever.
//! What is refused is an entry whose mode *does* say it is something other than
//! a file or a directory.
//!
//! **Zip has no hard links.** ADR-014 refuses them in a tar; there is no field
//! here that could express one, so there is nothing to check and this says so
//! rather than carrying a refusal that can never fire and looks like coverage.
//!
//! ## What Cordial actually takes out of an APK
//!
//! Exactly one entry: `lib/x86_64/libroblox.so`. It is written to a fixed path
//! Cordial chose, so nothing the archive says about *where* an entry goes is
//! ever acted on — which is precisely why the path refusals below are still
//! applied to the whole archive rather than to the one entry. They are how a
//! hostile APK is noticed at all. An archive built to escape somebody's
//! extractor is not an archive to take one file out of and shrug about the rest.

use crate::sha256::Sha256Hash;
use std::fmt;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// The Android ABI this build of Cordial can execute, spelled the way an APK
/// spells it.
///
/// **Cordial never translates the phone build's machine code** --
/// `docs/multiarch.md` decided that -- so the only phone library it can load
/// is the one built for the host's own architecture. Which means the ABI is a
/// property of *this binary*, fixed at compile time, and not something to
/// detect at run time or ask the user for. This used to say Cordial never
/// translates machine code at all; the VR mode runs the Quest build's arm64
/// engine under a translator (ADR-053), but that build is imported by the user
/// and stored apart, and never passes through this constant.
///
/// Two spellings, and confusing them is the trap worth naming: the directory
/// inside the APK is `lib/arm64-v8a/` with a hyphen, and Play's split archive
/// for the same ABI is `split_config.arm64_v8a.apk` with an underscore. Both
/// appear below.
#[cfg(target_arch = "x86_64")]
pub const HOST_ABI: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
pub const HOST_ABI: &str = "arm64-v8a";

/// The engine, and where it lives inside whichever APK carries it.
///
/// Per-architecture rather than built from [`HOST_ABI`] at run time, so it
/// stays a `&'static str` and every caller keeps working unchanged.
#[cfg(target_arch = "x86_64")]
pub const LIBRARY_IN_APK: &str = "lib/x86_64/libroblox.so";
#[cfg(target_arch = "aarch64")]
pub const LIBRARY_IN_APK: &str = "lib/arm64-v8a/libroblox.so";

// A host Roblox does not ship a build for cannot be served by this runtime at
// all, and failing at compile time says so once rather than leaving somebody to
// discover it from a linker error inside `dlopen`. `docs/multiarch.md` has the
// table.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!(
    "Cordial executes Roblox's own Android build natively and never translates \
     machine code, so it can only be built for an architecture Roblox ships: \
     x86-64 or aarch64. See docs/multiarch.md."
);

/// How much an archive may be, whatever it says about itself.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entries: usize,
    pub max_total_bytes: u64,
}

impl Default for Limits {
    /// An APK is not a plugin archive, so these are not ADR-014's numbers. The
    /// build this runtime loads holds a few tens of thousands of entries and
    /// around 115 MB of engine; these are well above that and well below
    /// anything that fills a disk before the refusal arrives.
    fn default() -> Self {
        Limits { max_entries: 200_000, max_total_bytes: 4 * 1024 * 1024 * 1024 }
    }
}

/// Why an archive was refused. Each of these is a name, not a skipped entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotAZip { path: String, why: String },
    AbsolutePath(String),
    ParentTraversal(String),
    EscapesRoot(String),
    EmptyPath,
    Symlink(String),
    DeviceNode(String),
    Fifo(String),
    Socket(String),
    UnsupportedEntry { path: String, mode: u32 },
    SetuidBit { path: String, mode: u32 },
    TooManyEntries { limit: usize },
    TooLarge { limit: u64 },
    /// The archive is well-formed and does not hold what was wanted.
    NoSuchEntry { path: String, wanted: String, entries: usize },
    HashMismatch { path: String, expected: String, actual: String },
    Io { path: String, why: String },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotAZip { path, why } => write!(f, "{path} is not a readable zip: {why}"),
            Refusal::AbsolutePath(p) => write!(f, "{p:?} is an absolute path"),
            Refusal::ParentTraversal(p) => write!(f, "{p:?} contains \"..\""),
            Refusal::EscapesRoot(p) => write!(f, "{p:?} lands outside the directory it is being written to"),
            Refusal::EmptyPath => f.write_str("an entry has no path"),
            Refusal::Symlink(p) => write!(f, "{p:?} is a symlink, and no archive Cordial unpacks may hold one"),
            Refusal::DeviceNode(p) => write!(f, "{p:?} is a device node"),
            Refusal::Fifo(p) => write!(f, "{p:?} is a FIFO"),
            Refusal::Socket(p) => write!(f, "{p:?} is a socket"),
            Refusal::UnsupportedEntry { path, mode } => {
                write!(f, "{path:?} carries mode {mode:o}, which is neither a file nor a directory")
            }
            Refusal::SetuidBit { path, mode } => {
                write!(f, "{path:?} carries mode {mode:o}, which is setuid or setgid")
            }
            Refusal::TooManyEntries { limit } => write!(f, "the archive holds more than {limit} entries"),
            Refusal::TooLarge { limit } => write!(f, "the archive unpacks to more than {limit} bytes"),
            Refusal::NoSuchEntry { path, wanted, entries } => {
                write!(f, "{path} has no {wanted} ({entries} entries were checked)")
            }
            Refusal::HashMismatch { path, expected, actual } => {
                write!(f, "{path} hashes to {actual}, and it was expected to be {expected}")
            }
            Refusal::Io { path, why } => write!(f, "{path}: {why}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// What an archive turned out to hold, once every refusal had a chance to fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub entries: usize,
    /// The sum of what the central directory declares, which is what the
    /// entry-size cap is applied to. What actually comes out of the
    /// decompressor is capped separately, in [`extract`].
    pub declared_bytes: u64,
}

/// Check `path` against every refusal, without writing anything.
///
/// Worth having on its own: the archive can be judged before a single byte of
/// it is extracted, and every rule below is then reachable from a test with a
/// handmade zip and no network.
pub fn inspect(path: &Path, limits: Limits) -> Result<Summary, Refusal> {
    let file = std::fs::File::open(path)
        .map_err(|e| Refusal::Io { path: path.display().to_string(), why: e.to_string() })?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| {
        Refusal::NotAZip { path: path.display().to_string(), why: e.to_string() }
    })?;

    if archive.len() > limits.max_entries {
        return Err(Refusal::TooManyEntries { limit: limits.max_entries });
    }

    let mut declared = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| Refusal::NotAZip {
            path: path.display().to_string(),
            why: format!("entry {i}: {e}"),
        })?;
        let name = entry.name().to_string();
        check_path(&name)?;
        check_mode(&name, entry.unix_mode())?;
        declared = declared.saturating_add(entry.size());
        if declared > limits.max_total_bytes {
            return Err(Refusal::TooLarge { limit: limits.max_total_bytes });
        }
    }
    Ok(Summary { entries: archive.len(), declared_bytes: declared })
}

/// Whether `apk` holds `wanted`, without taking anything out of it.
///
/// The two-APK question, asked rather than assumed: `base.apk` has no
/// `lib/x86_64/libroblox.so` on a split build and does have one on a universal
/// build, and there is no way to tell which kind of build arrived except by
/// looking. [`crate::install`] asks this before it swaps anything into place, so
/// "the wrong half was downloaded" is a refusal rather than a client that fails
/// to start.
///
/// [`inspect`] runs first here as well. An archive that breaks one of ADR-014's
/// rules is not an archive to answer questions about.
pub fn holds(apk: &Path, wanted: &str) -> Result<bool, Refusal> {
    holds_with(apk, wanted, Limits::default())
}

pub fn holds_with(apk: &Path, wanted: &str, limits: Limits) -> Result<bool, Refusal> {
    inspect(apk, limits)?;
    let file = std::fs::File::open(apk)
        .map_err(|e| Refusal::Io { path: apk.display().to_string(), why: e.to_string() })?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| {
        Refusal::NotAZip { path: apk.display().to_string(), why: e.to_string() }
    })?;
    let held = archive.by_name(wanted).is_ok();
    Ok(held)
}

/// Take one named entry out of `apk` and write it into `into`.
///
/// Everything in [`inspect`] runs first, over the whole archive. Taking the one
/// file and ignoring what the rest of the archive is doing would be reading the
/// refusals as being about the extraction rather than about the archive.
///
/// The output is written to a `.partial` sibling and renamed, because a launch
/// interrupted halfway leaves a 40 MB file that looks exactly like a complete
/// one to an `is_file` check, and the next launch would hand the loader a
/// truncated engine. `rename` within one directory is atomic.
pub fn extract(apk: &Path, wanted: &str, into: &Path) -> Result<PathBuf, Refusal> {
    extract_with(apk, wanted, into, Limits::default())
}

pub fn extract_with(
    apk: &Path,
    wanted: &str,
    into: &Path,
    limits: Limits,
) -> Result<PathBuf, Refusal> {
    let summary = inspect(apk, limits)?;

    let file = std::fs::File::open(apk)
        .map_err(|e| Refusal::Io { path: apk.display().to_string(), why: e.to_string() })?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| {
        Refusal::NotAZip { path: apk.display().to_string(), why: e.to_string() }
    })?;
    let mut entry = archive.by_name(wanted).map_err(|_| Refusal::NoSuchEntry {
        path: apk.display().to_string(),
        wanted: wanted.to_string(),
        entries: summary.entries,
    })?;

    // Checked again on the entry actually being written, rather than trusted
    // from the pass above. `by_name` is a second lookup, and a check that
    // happened on a different lookup of a different index is a check on
    // something else.
    check_path(entry.name())?;
    check_mode(entry.name(), entry.unix_mode())?;

    let leaf = Path::new(wanted)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(Refusal::EmptyPath)?
        .to_string();
    std::fs::create_dir_all(into)
        .map_err(|e| Refusal::Io { path: into.display().to_string(), why: e.to_string() })?;
    let partial = into.join(format!("{leaf}.partial"));
    let target = into.join(&leaf);

    // 0644 regardless of what the archive asked for, exactly as ADR-014 says.
    // Cordial `dlopen`s the engine; nothing ever executes it as a program, so
    // an executable bit could only be useful to something else.
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&partial)
        .map_err(|e| Refusal::Io { path: partial.display().to_string(), why: e.to_string() })?;

    let copied = copy_capped(&mut entry, &mut out, limits.max_total_bytes);
    drop(out);
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }

    std::fs::rename(&partial, &target).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        Refusal::Io { path: target.display().to_string(), why: e.to_string() }
    })?;
    Ok(target)
}

/// Extract, and refuse unless the result hashes to `expected`.
///
/// For the case where the APK arrived with a published hash for the engine
/// inside it. Nothing uses it today — [`crate::download`] verifies the APK
/// itself, which is the stronger statement — and it exists because the check is
/// one line and the alternative is somebody adding it later without the test.
pub fn extract_verified(
    apk: &Path,
    wanted: &str,
    into: &Path,
    expected: Sha256Hash,
) -> Result<PathBuf, Refusal> {
    let path = extract(apk, wanted, into)?;
    let bytes = std::fs::read(&path)
        .map_err(|e| Refusal::Io { path: path.display().to_string(), why: e.to_string() })?;
    let actual = Sha256Hash::of(&bytes);
    if actual != expected {
        let _ = std::fs::remove_file(&path);
        return Err(Refusal::HashMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(path)
}

/// A path with no `..`, not absolute, and landing inside its destination once
/// normalised.
///
/// The third is redundant given the first two and is kept anyway; it is the
/// check that still holds if somebody later decides a `..` in the middle of a
/// path is harmless because it cancels out.
fn check_path(name: &str) -> Result<(), Refusal> {
    if name.is_empty() {
        return Err(Refusal::EmptyPath);
    }
    // A backslash is a separator on the system that wrote the archive and an
    // ordinary character here, which is how `..\..\x` gets past a check written
    // in terms of components.
    let normalised = name.replace('\\', "/");
    let path = Path::new(&normalised);
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err(Refusal::ParentTraversal(name.to_string())),
            Component::RootDir | Component::Prefix(_) => {
                return Err(Refusal::AbsolutePath(name.to_string()))
            }
        }
    }
    if out.as_os_str().is_empty() {
        // `./` and `/` both normalise away to nothing, and a directory entry
        // named `/` is not a directory entry.
        return Err(Refusal::EmptyPath);
    }
    if !within(Path::new(""), &out) {
        return Err(Refusal::EscapesRoot(name.to_string()));
    }
    Ok(())
}

/// Whether `path` stays inside `root`, lexically.
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

const S_IFMT: u32 = 0o170_000;
const S_IFSOCK: u32 = 0o140_000;
const S_IFLNK: u32 = 0o120_000;
const S_IFREG: u32 = 0o100_000;
const S_IFBLK: u32 = 0o060_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFCHR: u32 = 0o020_000;
const S_IFIFO: u32 = 0o010_000;

/// An entry is a regular file or a directory, carries no setuid or setgid bit,
/// and nothing else is accepted.
///
/// `None` — the producer wrote no Unix attributes — is a regular file, because
/// that is what most zip writers produce and there is no other reading
/// available. It is the concession ADR-014 predicted when it said zip's mode
/// bits are populated inconsistently, and it is the reason a zip can only ever
/// be checked for what it *claims*.
fn check_mode(name: &str, mode: Option<u32>) -> Result<(), Refusal> {
    let Some(mode) = mode else { return Ok(()) };
    if mode & 0o6000 != 0 {
        return Err(Refusal::SetuidBit { path: name.to_string(), mode });
    }
    match mode & S_IFMT {
        S_IFLNK => Err(Refusal::Symlink(name.to_string())),
        S_IFBLK | S_IFCHR => Err(Refusal::DeviceNode(name.to_string())),
        S_IFIFO => Err(Refusal::Fifo(name.to_string())),
        S_IFSOCK => Err(Refusal::Socket(name.to_string())),
        S_IFREG | S_IFDIR => Ok(()),
        // Zero means the extension field held permission bits and no file type,
        // which several Android build tools produce. Anything else is a type
        // this does not know, and an unknown type is not a file.
        0 => Ok(()),
        _ => Err(Refusal::UnsupportedEntry { path: name.to_string(), mode }),
    }
}

/// Copy at most `budget` bytes, refusing rather than truncating.
///
/// The central directory already said how big this entry is and that was
/// checked, so this only fires when the header lied — which is the whole reason
/// there are two caps. Truncating instead would hand the loader an engine that
/// is quietly not the one in the archive.
fn copy_capped(from: &mut impl Read, to: &mut impl Write, budget: u64) -> Result<u64, Refusal> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = from
            .read(&mut buf)
            .map_err(|e| Refusal::Io { path: "the archive entry".into(), why: e.to_string() })?;
        if n == 0 {
            return Ok(total);
        }
        total += n as u64;
        if total > budget {
            return Err(Refusal::TooLarge { limit: budget });
        }
        to.write_all(&buf[..n])
            .map_err(|e| Refusal::Io { path: "the extracted file".into(), why: e.to_string() })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-update-apk-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An ordinary, honest APK-shaped archive.
    fn good_apk() -> Vec<u8> {
        build(|w| {
            w.start_file("AndroidManifest.xml", SimpleFileOptions::default().unix_permissions(0o644))
                .unwrap();
            w.write_all(b"not really a manifest").unwrap();
            w.start_file(LIBRARY_IN_APK, SimpleFileOptions::default().unix_permissions(0o644))
                .unwrap();
            w.write_all(b"\x7fELF pretend this is the engine").unwrap();
        })
    }

    fn build(f: impl FnOnce(&mut zip::ZipWriter<Cursor<Vec<u8>>>)) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        f(&mut w);
        w.finish().unwrap().into_inner()
    }

    /// Overwrite the first central-directory record's external attributes.
    ///
    /// `unix_permissions` masks a mode to `0o777` and `normalize` then ORs in
    /// `S_IFREG`, so a setuid bit or a device node cannot be produced through
    /// the writer at all — this crate's zip library refuses to write the
    /// archives worth defending against. Patching the field a hostile producer
    /// would set is what the fixture does instead, for exactly the reason
    /// `cordial_plugins::unpack`'s tests patch a tar header by hand: building
    /// the archive with a well-behaved library and then asserting the reader is
    /// safe would be a test that could never fail.
    ///
    /// Offset 38 in a central-directory record is the four-byte external
    /// attributes field, and a Unix producer puts the mode in its high half.
    fn set_first_mode(zip: &mut [u8], mode: u32) {
        const CENTRAL: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
        let at = zip
            .windows(CENTRAL.len())
            .position(|w| w == CENTRAL)
            .expect("the fixture has no central directory record");
        zip[at + 38..at + 42].copy_from_slice(&(mode << 16).to_le_bytes());
    }

    /// One entry whose mode is whatever the test needs it to be.
    fn archive_with_mode(name: &str, mode: u32) -> Vec<u8> {
        // Written with a real mode first so the "version made by" byte says
        // Unix; without that the reader ignores the external attributes
        // entirely and the patch above would be invisible.
        let mut bytes = build(|w| {
            w.start_file(name, SimpleFileOptions::default().unix_permissions(0o644)).unwrap();
            w.write_all(b"x").unwrap();
        });
        set_first_mode(&mut bytes, mode);
        bytes
    }

    fn written(tag: &str, bytes: &[u8]) -> PathBuf {
        let dir = scratch(tag);
        let path = dir.join("base.apk");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn an_ordinary_archive_gives_up_the_engine() {
        // The control for every refusal below: the same shape of archive with
        // nothing wrong with it has to land on disk.
        let apk = written("good", &good_apk());
        let into = apk.parent().unwrap().join("lib/x86_64");
        let out = extract(&apk, LIBRARY_IN_APK, &into).unwrap();
        assert_eq!(out, into.join("libroblox.so"));
        assert_eq!(std::fs::read(&out).unwrap(), b"\x7fELF pretend this is the engine");
        assert!(!into.join("libroblox.so.partial").exists());
        use std::os::unix::fs::PermissionsExt;
        // A restrictive umask may remove read access for other users; that is
        // not an extraction failure. What matters here is that the engine is
        // readable by its owner, never executable, and never writable by others.
        let mode = std::fs::metadata(&out).unwrap().permissions().mode();
        assert_ne!(mode & 0o400, 0, "the engine must be readable by its owner");
        assert_eq!(mode & 0o111, 0, "nothing executes the engine as a program");
        assert_eq!(mode & 0o022, 0, "other users cannot modify the engine");
    }

    #[test]
    fn a_symlink_is_refused_even_pointing_inside() {
        // `a -> .` followed by `a/b` escapes using two entries neither of which
        // looks wrong on its own, so no link is allowed at all.
        let apk = written(
            "symlink",
            &build(|w| {
                w.add_symlink("lib/x86_64/libroblox.so", "AndroidManifest.xml", SimpleFileOptions::default())
                    .unwrap();
            }),
        );
        assert!(matches!(
            inspect(&apk, Limits::default()),
            Err(Refusal::Symlink(_))
        ));
    }

    #[test]
    fn a_parent_traversal_anywhere_in_the_archive_is_refused() {
        // Refused even though Cordial writes to a path of its own choosing and
        // could not be made to follow this one. An archive built to escape
        // somebody's extractor is not an archive to take one file out of.
        let apk = written(
            "dotdot",
            &build(|w| {
                w.start_file("../../etc/cron.d/evil", SimpleFileOptions::default()).unwrap();
                w.write_all(b"x").unwrap();
            }),
        );
        assert!(matches!(inspect(&apk, Limits::default()), Err(Refusal::ParentTraversal(_))));
    }

    #[test]
    fn an_absolute_path_is_refused() {
        let apk = written(
            "absolute",
            &build(|w| {
                w.start_file("/etc/passwd", SimpleFileOptions::default()).unwrap();
                w.write_all(b"x").unwrap();
            }),
        );
        assert!(matches!(inspect(&apk, Limits::default()), Err(Refusal::AbsolutePath(_))));
    }

    #[test]
    fn a_windows_separator_does_not_smuggle_a_traversal_past_the_check() {
        // `..\..\x` has one component on Unix and is a traversal on the system
        // that wrote it. Checking components alone would let this through.
        assert!(matches!(check_path(r"..\..\evil"), Err(Refusal::ParentTraversal(_))));
        assert!(matches!(check_path(r"lib\x86_64\libroblox.so"), Ok(())));
    }

    #[test]
    fn a_device_node_a_fifo_and_a_socket_are_refused() {
        for (tag, bits, expect) in [
            ("chr", S_IFCHR, "device"),
            ("blk", S_IFBLK, "device"),
            ("fifo", S_IFIFO, "FIFO"),
            ("sock", S_IFSOCK, "socket"),
        ] {
            let apk = written(tag, &archive_with_mode(LIBRARY_IN_APK, bits | 0o644));
            let e = inspect(&apk, Limits::default()).unwrap_err();
            assert!(e.to_string().contains(expect), "{tag}: {e}");
        }
    }

    #[test]
    fn a_setuid_bit_is_refused_rather_than_stripped() {
        // Stripping it would extract the archive anyway. An archive asking for
        // setuid is saying something about itself worth stopping for.
        for bits in [0o4755u32, 0o2755] {
            let apk = written("setuid", &archive_with_mode("helper", S_IFREG | bits));
            assert!(
                matches!(inspect(&apk, Limits::default()), Err(Refusal::SetuidBit { .. })),
                "{bits:o}"
            );
        }
    }

    #[test]
    fn an_entry_with_no_unix_mode_is_a_file() {
        // Most zip producers write no Unix attributes at all, so refusing this
        // would refuse most real APKs. Stated as a test rather than left in a
        // comment, because it is the one place this is weaker than the tar
        // unpacker and somebody should have to change a test to make it weaker
        // still.
        assert!(check_mode("classes.dex", None).is_ok());
        assert!(check_mode("classes.dex", Some(0o644)).is_ok());
    }

    #[test]
    fn too_many_entries_are_refused() {
        let apk = written(
            "entries",
            &build(|w| {
                for i in 0..40 {
                    w.start_file(format!("f{i}"), SimpleFileOptions::default()).unwrap();
                    w.write_all(b"x").unwrap();
                }
            }),
        );
        let limits = Limits { max_entries: 8, ..Limits::default() };
        assert_eq!(inspect(&apk, limits), Err(Refusal::TooManyEntries { limit: 8 }));
    }

    #[test]
    fn a_compressed_bomb_is_refused_on_its_uncompressed_size() {
        // Four megabytes of zeroes deflate to a few kilobytes, which is the
        // whole problem: the size of what was downloaded says nothing about the
        // size of what is being written.
        let payload = vec![0u8; 4 * 1024 * 1024];
        let bytes = build(|w| {
            w.start_file("big.bin", SimpleFileOptions::default()).unwrap();
            w.write_all(&payload).unwrap();
        });
        assert!(bytes.len() < 64 * 1024, "the fixture is only interesting if it compresses well");
        let apk = written("bomb", &bytes);
        let limits = Limits { max_total_bytes: 1024 * 1024, ..Limits::default() };
        assert_eq!(inspect(&apk, limits), Err(Refusal::TooLarge { limit: 1024 * 1024 }));
    }

    #[test]
    fn an_archive_without_the_engine_says_how_many_entries_it_looked_at() {
        let apk = written(
            "noengine",
            &build(|w| {
                w.start_file("AndroidManifest.xml", SimpleFileOptions::default()).unwrap();
                w.write_all(b"x").unwrap();
            }),
        );
        let into = apk.parent().unwrap().join("out");
        match extract(&apk, LIBRARY_IN_APK, &into) {
            Err(Refusal::NoSuchEntry { wanted, entries, .. }) => {
                assert_eq!(wanted, LIBRARY_IN_APK);
                assert_eq!(entries, 1);
            }
            other => panic!("expected NoSuchEntry, got {other:?}"),
        }
        assert!(!into.join("libroblox.so").exists());
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_refused_by_name() {
        let apk = written("notazip", b"this is not a zip");
        assert!(matches!(inspect(&apk, Limits::default()), Err(Refusal::NotAZip { .. })));
    }

    #[test]
    fn a_hostile_entry_elsewhere_stops_the_engine_being_taken_out() {
        // The archive holds a perfectly good engine and a symlink. Extracting
        // the engine and ignoring the symlink is the "skip the entry I did not
        // like" behaviour ADR-014 refuses, and this is the test that fails if
        // `extract` stops calling `inspect`.
        let apk = written(
            "mixed",
            &build(|w| {
                w.start_file(LIBRARY_IN_APK, SimpleFileOptions::default().unix_permissions(0o644))
                    .unwrap();
                w.write_all(b"\x7fELF").unwrap();
                w.add_symlink("shortcut", "AndroidManifest.xml", SimpleFileOptions::default())
                    .unwrap();
            }),
        );
        let into = apk.parent().unwrap().join("out");
        assert!(matches!(extract(&apk, LIBRARY_IN_APK, &into), Err(Refusal::Symlink(_))));
        assert!(!into.join("libroblox.so").exists(), "and nothing was written");
    }

    #[test]
    fn a_verified_extraction_refuses_and_removes_a_mismatch() {
        let apk = written("verified", &good_apk());
        let into = apk.parent().unwrap().join("out");
        let e = extract_verified(&apk, LIBRARY_IN_APK, &into, Sha256Hash::of(b"something else"))
            .unwrap_err();
        assert!(matches!(e, Refusal::HashMismatch { .. }), "{e}");
        assert!(!into.join("libroblox.so").exists());

        // And the control: the right hash keeps it.
        let ok = extract_verified(
            &apk,
            LIBRARY_IN_APK,
            &into,
            Sha256Hash::of(b"\x7fELF pretend this is the engine"),
        )
        .unwrap();
        assert!(ok.is_file());
    }

    #[test]
    fn within_refuses_a_path_that_climbs_out() {
        assert!(within(Path::new(""), Path::new("lib/x86_64/libroblox.so")));
        assert!(within(Path::new(""), Path::new("lib/../libroblox.so")));
        assert!(!within(Path::new(""), Path::new("../libroblox.so")));
        assert!(!within(Path::new(""), Path::new("lib/../../elsewhere")));
    }
}
