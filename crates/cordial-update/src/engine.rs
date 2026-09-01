//! Which Roblox engine is on this disk, read out of `libroblox.so`.
//!
//! This is the half of "is there an update" that used to be missing. The other
//! operand — what Roblox has published — [`changelog`](crate::changelog) has
//! answered all along, because the release notes are titled by the engine
//! major. What nothing could answer was *which engine is here*, so the updater
//! compared a known number against `None` and reported that it could not tell.
//! [`cache::recorded_version`](crate::cache::recorded_version) only knows a
//! version for a build Cordial fetched itself, and until this crate could fetch
//! one it knew nothing at all.
//!
//! The engine has always known. It stamps its version on every log file it
//! writes, and the string is in the binary as an ASCII literal.
//!
//! ## This is a string search, not disassembly
//!
//! AGENTS.md draws that line and this stays the right side of it: the scan
//! reads a literal, the same thing `strings` prints, and takes away nothing
//! about how the engine works. It moved here from `cordial-runtime`'s
//! `load.rs`, which is a binary and therefore could not be called by the
//! updater — the value it produced was already being set into
//! `CORDIAL_ENGINE_VERSION` for the user agent, so the number was reachable by
//! the engine and by nothing else.
//!
//! That function's history is the reason for the `None`s below. It was a
//! hardcoded `"2.732.0.1043"` with a comment claiming it was the engine's own
//! answer, while the engine in the APK was 2.730.0.790 — so Cordial told the
//! server one version and the client was another, and nothing caught it across
//! an APK update. Returning `None` when the shape is not unique is the point:
//! skipping the claim is honest and inventing one is what caused that.
//!
//! ## Measured on the build in this cache, 2026-08-20
//!
//! ```text
//! maximal runs of [0-9.] in libroblox.so, 9..=20 characters, four numeric
//! parts, first part 2:  ["2.734.0.917"]
//! ```
//!
//! Exactly one, which is what the uniqueness rule needs. The near misses are
//! what make the rule earn its place: the same binary carries `2.16.840.1.101.3.4.2.1`
//! and friends, and every one of them is either longer than the cap or not four
//! parts once the *whole* run is taken rather than a substring of it. Matching
//! inside a longer run is the mistake — a regexp for the four-part shape finds
//! nine candidates in this file and this scan finds one.
//!
//! ## Streaming, because the engine is 117 MB
//!
//! `load.rs` read the whole object into a `Vec`. It could afford to: it was
//! about to map the thing anyway. The updater is a background thread on a
//! launcher window, so this reads in blocks and carries the digit run across
//! the boundary.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The engine object, and the name of the file inside a lib directory.
pub const LIBRARY: &str = "libroblox.so";

/// How much of a digit run is worth keeping. Longer than the longest version
/// this shape can express, so a run that reaches it is already not one.
const MAX_RUN: usize = 20;
const MIN_RUN: usize = 9;

/// The version of the engine extracted into `lib_dir`, if it can be read.
///
/// **Answered from a stamp file when the engine has not changed, because the
/// scan is 8.8% of Cordial's startup CPU.** Measured with `perf` on
/// 2026-09-01: `version_of` and `version_in_run` together were the largest
/// named cost in Cordial's own code during startup, larger than the looper,
/// and more than half of everything attributed to `cordial-run`. The client
/// calls this once per launch through `load.rs`'s `engine_version` to tell the
/// server which build it is, so every launch walked 118 MB looking for a
/// string that only changes when the APK does.
///
/// The scan has no early exit by design -- it must reach EOF to notice a
/// second differing candidate -- so it cannot be made cheap, only skipped.
/// `diagnostics.rs` already declined to show an `Engine` line for this reason,
/// on the grounds that "a diagnostics command that takes several seconds is
/// one people stop running"; the cost was understood there and unmeasured
/// here.
///
/// Keyed on the library's length and modification time. That is the pair every
/// build tool uses for the same job, and the failure it admits -- a file
/// replaced within the same timestamp granularity and at exactly the same
/// length -- would be a Roblox build byte-identical in size to the one it
/// replaced, extracted inside the same nanosecond. A wrong answer there is a
/// stale version string, which is the bug this function's caller was written
/// to fix, so [`version_of`] stays available for anything that must not guess.
pub fn installed_version(lib_dir: &Path) -> Option<String> {
    let library = lib_dir.join(LIBRARY);
    let stamp = stamp_of(&library);
    if let Some(stamp) = &stamp {
        if let Some(cached) = cached_version(lib_dir, stamp) {
            // An empty record is a remembered "the shape was not unique here".
            // Kept, because rescanning 118 MB every launch to reach the same
            // answer is the case this exists to avoid.
            return if cached.is_empty() { None } else { Some(cached) };
        }
    }
    let found = version_of(&library);
    if let Some(stamp) = &stamp {
        remember(lib_dir, stamp, found.as_deref().unwrap_or(""));
    }
    found
}

/// The stamp file's name. Beside the engine, because that is the directory
/// that is rewritten whenever the engine is.
const VERSION_STAMP: &str = ".cordial-engine-version";

/// `<len> <mtime_nanos>` for the library, or `None` if it cannot be read.
fn stamp_of(library: &Path) -> Option<String> {
    let meta = std::fs::metadata(library).ok()?;
    let modified = meta.modified().ok()?;
    let nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(format!("{} {}", meta.len(), nanos))
}

/// The remembered version for this stamp, if the stamp still matches.
fn cached_version(lib_dir: &Path, stamp: &str) -> Option<String> {
    let text = std::fs::read_to_string(lib_dir.join(VERSION_STAMP)).ok()?;
    // `<stamp>\n<version>`, and the version may be empty.
    let (recorded, version) = text.split_once('\n')?;
    (recorded == stamp).then(|| version.trim_end().to_string())
}

/// Record the answer. Best effort: a read-only lib directory is a slow launch,
/// not a broken one, so a failure here is not reported and not an error.
fn remember(lib_dir: &Path, stamp: &str, version: &str) {
    let path = lib_dir.join(VERSION_STAMP);
    let tmp = path.with_extension("new");
    // Written and renamed like every other document this project keeps, so a
    // process killed mid-write leaves the old stamp rather than a torn one
    // that would be read back as a version.
    if std::fs::write(&tmp, format!("{stamp}\n{version}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Whether there is an engine in `lib_dir` at all.
pub fn present(lib_dir: &Path) -> bool {
    lib_dir.join(LIBRARY).is_file()
}

/// The path of the engine in `lib_dir`, whether or not it exists.
pub fn library_in(lib_dir: &Path) -> PathBuf {
    lib_dir.join(LIBRARY)
}

/// The version literal in one `libroblox.so`.
pub fn version_of(library: &Path) -> Option<String> {
    let file = std::fs::File::open(library).ok()?;
    scan(std::io::BufReader::new(file))
}

/// The scan itself, over anything readable.
///
/// Separate from the file so the rules can be tested against bytes rather than
/// against a Roblox binary. Nothing in this repository may hold one — ADR-015
/// and the README's rule about never committing a Roblox byte — so a test
/// fixture here is a handful of ASCII this crate wrote itself, and the real
/// build is measured by the probe instead.
pub fn scan<R: Read>(mut source: R) -> Option<String> {
    let mut found: Option<String> = None;
    let mut run: Vec<u8> = Vec::with_capacity(MAX_RUN + 1);
    // A run longer than the cap is not a version, and holding it would let a
    // file of nothing but digits decide how much memory this uses.
    let mut overlong = false;
    let mut buf = vec![0u8; 256 * 1024];

    loop {
        let n = match source.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            // A read error part way through is not "no version": it is not
            // knowing, which is the same answer and reached honestly.
            Err(_) => return None,
        };
        for &b in &buf[..n] {
            if b.is_ascii_digit() || b == b'.' {
                if run.len() == MAX_RUN {
                    overlong = true;
                    run.clear();
                } else if !overlong {
                    run.push(b);
                }
                continue;
            }
            if !overlong {
                if let Some(candidate) = version_in_run(&run) {
                    match &found {
                        // Two distinct candidates means the shape is not unique
                        // in this build and the assumption behind reading it
                        // has stopped holding. Say nothing rather than pick.
                        Some(previous) if *previous != candidate => return None,
                        _ => found = Some(candidate),
                    }
                }
            }
            run.clear();
            overlong = false;
        }
    }
    // The last run, if the file ends inside one.
    if !overlong {
        if let Some(candidate) = version_in_run(&run) {
            match &found {
                Some(previous) if *previous != candidate => return None,
                _ => found = Some(candidate),
            }
        }
    }
    found
}

/// Whether a whole run of `[0-9.]` is the engine's version literal.
///
/// The whole run, never a substring of it. `2.16.840.1.101.3.4.2.1` contains
/// `2.16.840.1`, which is the four-part shape and is an OID.
fn version_in_run(run: &[u8]) -> Option<String> {
    if run.len() < MIN_RUN || run.len() > MAX_RUN {
        return None;
    }
    let text = std::str::from_utf8(run).ok()?;
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    if !parts.iter().all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit())) {
        return None;
    }
    // The engine's own versions have been `2.NNN.x.y` for the whole life of
    // this project, and the leading component is what keeps timestamps and
    // OIDs out.
    (parts[0] == "2").then(|| text.to_string())
}

/// The engine major — `734` out of `2.734.0.917`.
///
/// The same component [`crate::version::major_of`] takes out of the
/// `0.734.x.y` the version endpoint serves, and the same number Roblox titles
/// its release notes by. That the two version schemes agree on this one field
/// is what makes "installed engine 734, newest release notes 734" a comparison
/// rather than a coincidence.
pub fn major_of(version: &str) -> Option<u32> {
    crate::version::major_of(version)
}

#[cfg(test)]
mod tests {

    /// **The stamp answers, and a changed engine invalidates it.**
    ///
    /// Both halves matter: without the cache the client walks 118 MB at every
    /// launch, and without the invalidation it would report the previous
    /// build's version after an APK update -- which is exactly the bug
    /// `load.rs`'s `engine_version` was written to fix, reintroduced.
    #[test]
    fn the_version_is_remembered_until_the_engine_changes() {
        let dir = std::env::temp_dir().join(format!("cordial-engine-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let library = dir.join(super::LIBRARY);

        // A fixture this crate wrote itself: ADR-015 forbids a Roblox byte here.
        std::fs::write(&library, b"noise 2.736.0.1408 noise").unwrap();
        assert_eq!(super::installed_version(&dir).as_deref(), Some("2.736.0.1408"));
        assert!(dir.join(super::VERSION_STAMP).is_file(), "the answer must be recorded");

        // The stamp is what is read now: corrupt the library and the recorded
        // answer still stands, which is what proves the scan was skipped.
        std::fs::write(&library, b"noise 2.736.0.1408 noise").unwrap();
        let stamp = super::stamp_of(&library).unwrap();
        super::remember(&dir, &stamp, "9.9.9.9");
        assert_eq!(super::installed_version(&dir).as_deref(), Some("9.9.9.9"));

        // A different engine has a different length, so the stamp misses and
        // the real scan runs again.
        std::fs::write(&library, b"noise 2.800.0.1 and more bytes than before").unwrap();
        assert_eq!(super::installed_version(&dir).as_deref(), Some("2.800.0.1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An ambiguous build is remembered as ambiguous rather than rescanned.
    #[test]
    fn not_knowing_is_cached_too() {
        let dir = std::env::temp_dir().join(format!("cordial-engine-ambig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let library = dir.join(super::LIBRARY);
        // Two different four-part candidates: the scan answers None.
        std::fs::write(&library, b"a 2.736.0.1408 b 2.800.0.1409 c").unwrap();
        assert_eq!(super::installed_version(&dir), None);

        let recorded = std::fs::read_to_string(dir.join(super::VERSION_STAMP)).unwrap();
        assert!(recorded.ends_with("\n\n"), "an empty version is the record: {recorded:?}");
        assert_eq!(super::installed_version(&dir), None, "and it is answered from the stamp");

        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    /// Not a Roblox byte anywhere near this file: the fixture is a version
    /// string this test wrote, surrounded by the shapes that were observed
    /// beside the real one.
    fn haystack(version: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"\0\0some symbol name\0");
        v.extend_from_slice(version.as_bytes());
        v.extend_from_slice(b"\0GCC: (GNU) 14.2.0\0");
        v
    }

    #[test]
    fn the_version_literal_is_found() {
        assert_eq!(scan(&haystack("2.734.0.917")[..]), Some("2.734.0.917".into()));
    }

    #[test]
    fn an_oid_is_not_a_version() {
        // The measured near miss, and the reason the whole run is taken rather
        // than a match inside it: this string contains `2.16.840.1`, which has
        // four numeric parts and starts with 2. A substring search reports it.
        let mut v = haystack("2.734.0.917");
        v.extend_from_slice(b"\0" as &[u8]);
        v.extend_from_slice(b"2.16.840.1.101.3.4.2.1");
        v.extend_from_slice(b"\0");
        assert_eq!(scan(&v[..]), Some("2.734.0.917".into()));
    }

    #[test]
    fn two_different_candidates_answer_nothing() {
        // The rule that keeps this honest. A build where the shape is no longer
        // unique is a build this cannot read, and picking one would put a
        // number in front of the user that nothing established.
        let mut v = haystack("2.734.0.917");
        v.extend_from_slice(b"2.700.1.2\0");
        assert_eq!(scan(&v[..]), None);
    }

    #[test]
    fn the_same_candidate_twice_is_still_an_answer() {
        let mut v = haystack("2.734.0.917");
        v.extend_from_slice(b"2.734.0.917\0");
        assert_eq!(scan(&v[..]), Some("2.734.0.917".into()));
    }

    #[test]
    fn a_run_that_spans_a_read_boundary_is_still_one_run() {
        // The whole reason this streams. A version literal that happens to land
        // across a block boundary must not be read as two shorter runs, and it
        // must not be read as a version by the half that is four parts on its
        // own either.
        let mut v = vec![b'\0'; 256 * 1024 - 4];
        v.extend_from_slice(b"2.734.0.917\0");
        assert_eq!(scan(&v[..]), Some("2.734.0.917".into()));
    }

    #[test]
    fn a_file_of_digits_is_not_a_version_and_is_not_held_in_memory() {
        // The cap doing its job: one run of a megabyte is one candidate that is
        // far too long, and nothing accumulates.
        let v = vec![b'7'; 1024 * 1024];
        assert_eq!(scan(&v[..]), None);
    }

    #[test]
    fn nothing_at_all_is_none_rather_than_a_guess() {
        assert_eq!(scan(&b""[..]), None);
        assert_eq!(scan(&haystack("0.734.0.917")[..]), None, "the desktop scheme is not this one");
        assert_eq!(scan(&haystack("2.734.0")[..]), None, "three parts is not the shape");
    }

    #[test]
    fn the_major_is_the_one_the_release_notes_are_titled_by() {
        assert_eq!(major_of("2.734.0.917"), Some(734));
    }

    #[test]
    fn an_absent_library_is_not_readable_and_says_so_by_returning_nothing() {
        let dir = std::env::temp_dir().join("cordial-update-engine-absent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!present(&dir));
        assert_eq!(installed_version(&dir), None);
    }
}
