//! Fetching the plugin interpreter, for the hosts that cannot get one.
//!
//! Plugins are Deno programs (ADR-008) and Cordial execs `deno` off `PATH`.
//! **On most installs there is nothing to exec**, and that is not a gap in one
//! distribution: `deno` is in Arch's `extra`, and it is in neither Fedora
//! (`dnf5 list deno` on 44 returns nothing) nor Debian (its source index has no
//! exact match). So the deb, the rpm, the AppImage and the Flatpak all ship a
//! plugin system with no way to run a plugin, which is what users reported on
//! 2026-09-02.
//!
//! ## Why fetched rather than bundled
//!
//! The Flatpak decides it. Inside one, `PATH` is the runtime's, and Cordial
//! deliberately takes no route to the host -- `flatpak-spawn --host` needs a
//! name that ADR-002 refuses because it is a sandbox escape. So a Flatpak user
//! *cannot* install an interpreter by any means available to them, and Cordial
//! has to provide it or plugins are permanently dead there.
//!
//! Given it must be provided somewhere, one mechanism beats four packaging
//! changes: 39 MB fetched once covers Flatpak, AppImage, deb, rpm and a source
//! build, where bundling adds ~40 MB compressed to each of four artifacts, puts
//! 91 MB on disk for the ones that install extracted, and turns a Deno CVE into
//! four re-releases. Arch never fetches: its package is a real dependency and
//! `PATH` is searched first.
//!
//! **This is not a new class of trust.** Cordial already downloads a 118 MB
//! `libroblox.so` and loads it as native code in-process. A pinned, hashed
//! interpreter is a smaller decision than the one the client is built on.
//!
//! ## What is pinned, and why both halves
//!
//! The version *and* its hash are constants here. Nothing asks the network what
//! the current release is, so there is no answer to be lied to about, and the
//! hash is checked over the stream before the file gets the name anything looks
//! for -- `download` does that, and it is the reason this module is thirty
//! lines rather than three hundred. Bumping Deno is a commit that changes two
//! constants, which is the point: a plugin that works today does not stop
//! working because somebody else cut a release.

use crate::download::{self, Progress, Refusal, Source};
use crate::sha256::Sha256Hash;
use std::path::{Path, PathBuf};

/// The pinned release. Keep in step with
/// `cordial_plugins::sandbox::MANAGED_DENO_VERSION`, which is where the
/// install path's version component comes from; the test below pins that they
/// agree, because two constants that must match eventually do not.
pub const VERSION: &str = "2.9.6";

/// The only build Cordial fetches. Cordial is x86-64 only -- the whole project
/// exists to run an x86-64 Android library -- so there is no architecture to
/// choose between.
const ASSET: &str = "deno-x86_64-unknown-linux-gnu.zip";

/// `deno-x86_64-unknown-linux-gnu.zip.sha256sum` for [`VERSION`], read from
/// Deno's own release and checked here on 2026-09-02:
///
/// ```text
/// 394f07f4da2bebe6ce6f1e7ce0fa16429b29b08c35e3fac3fe25972676dff4b2  deno-x86_64-unknown-linux-gnu.zip
/// ```
///
/// Written down rather than fetched alongside the archive, because a checksum
/// served from the same place as the file it describes proves only that the
/// two agree.
const SHA256: &str = "sha256:394f07f4da2bebe6ce6f1e7ce0fa16429b29b08c35e3fac3fe25972676dff4b2";

fn url() -> String {
    format!("https://github.com/denoland/deno/releases/download/v{VERSION}/{ASSET}")
}

/// Where the archive says the interpreter is, inside itself.
const ENTRY: &str = "deno";

/// Fetch and install the interpreter into `dir`, returning the binary's path.
///
/// `dir` comes from the caller -- `cordial_plugins::sandbox::managed_deno_dir`
/// -- rather than being decided here. Neither crate depends on the other, and
/// a path computed in two places is a path that drifts; passing it keeps one
/// definition without inventing a dependency edge between a downloader and a
/// sandbox.
///
/// Already-installed is success, not an error, so a caller may run this
/// unconditionally.
pub fn install(dir: &Path, progress: Progress<'_>) -> Result<PathBuf, String> {
    let binary = dir.join(ENTRY);
    if binary.is_file() {
        return Ok(binary);
    }

    let hash = Sha256Hash::parse(SHA256).map_err(|why| format!("pinned hash: {why}"))?;
    let source = Source::new(url(), hash).map_err(describe)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let archive = download::fetch_named(&source, dir, ASSET, progress).map_err(describe)?;
    let unpacked = unzip_interpreter(&archive, dir);
    // The archive is 39 MB and has done its job either way.
    let _ = std::fs::remove_file(&archive);
    unpacked
}

/// Pull the one entry out, make it executable, and put it in place atomically.
///
/// Atomic because the check that decides whether to download is "is there a
/// file at this path": a half-written one left by a crash mid-extract would be
/// treated as an interpreter for ever after, and would fail as
/// `Exec format error` somewhere with no relationship to the cause.
fn unzip_interpreter(archive: &Path, dir: &Path) -> Result<PathBuf, String> {
    use std::io::Read;

    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a usable archive: {e}"))?;
    let mut entry = zip
        .by_name(ENTRY)
        .map_err(|_| format!("the archive has no {ENTRY:?} in it"))?;

    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).map_err(|e| format!("reading {ENTRY}: {e}"))?;

    let temporary = dir.join("deno.partial");
    std::fs::write(&temporary, &bytes).map_err(|e| format!("{}: {e}", temporary.display()))?;
    executable(&temporary)?;

    let binary = dir.join(ENTRY);
    std::fs::rename(&temporary, &binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    Ok(binary)
}

fn executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).map_err(|e| format!("{}: {e}", path.display()))
}

/// A `Refusal` in the words a person reads, since this one reaches Settings.
fn describe(refusal: Refusal) -> String {
    match refusal {
        Refusal::NoSource(why) | Refusal::NotHttps(why) | Refusal::Blocked(why) => why,
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The pinned version and the install path must name the same release.**
    /// They live in different crates that do not depend on each other, which is
    /// exactly the arrangement where two constants drift apart and the symptom
    /// is a download that installs somewhere nothing looks.
    #[test]
    fn the_pinned_version_matches_the_path_the_sandbox_looks_in() {
        assert_eq!(VERSION, cordial_plugins::sandbox::MANAGED_DENO_VERSION);
    }

    /// The URL is built from the pinned version, over https, and names the
    /// x86-64 Linux build. Checked because a typo here is a 404 a user meets.
    #[test]
    fn the_url_is_the_pinned_release_over_https() {
        let u = url();
        assert!(u.starts_with("https://"), "{u}");
        assert!(u.contains(&format!("/v{VERSION}/")), "{u}");
        assert!(u.ends_with("deno-x86_64-unknown-linux-gnu.zip"), "{u}");
    }

    /// The pinned hash parses as a SHA-256. A malformed constant would
    /// otherwise fail only at the end of a 39 MB download.
    #[test]
    fn the_pinned_hash_is_a_sha256() {
        Sha256Hash::parse(SHA256).expect("the pinned hash must parse");
    }

    /// An interpreter already in place is success and costs no network.
    #[test]
    fn an_existing_interpreter_is_not_downloaded_again() {
        let dir = std::env::temp_dir().join(format!("cordial-deno-present-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deno"), b"not really deno").unwrap();

        let mut noop: Box<dyn FnMut(u64, Option<u64>)> = Box::new(|_, _| {});
        let got = install(&dir, &mut *noop).expect("an installed interpreter is success");
        assert_eq!(got, dir.join("deno"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A half-written interpreter must never be left at the final name.**
    /// The presence check is "is there a file here", so a torn extract would be
    /// mistaken for an interpreter for ever and fail as `Exec format error`
    /// somewhere unrelated. This drives the extract against an archive with no
    /// `deno` in it -- the failure closest to a torn write that a test can
    /// arrange without a crash -- and asserts nothing was left behind.
    #[test]
    fn a_failed_extract_leaves_no_interpreter() {
        let dir = std::env::temp_dir().join(format!("cordial-deno-torn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let archive = dir.join("empty.zip");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let mut w = zip::ZipWriter::new(f);
            w.start_file::<_, ()>("something-else", zip::write::SimpleFileOptions::default())
                .unwrap();
            w.finish().unwrap();
        }

        let err = unzip_interpreter(&archive, &dir).expect_err("no deno entry means no install");
        assert!(err.contains("deno"), "{err}");
        assert!(!dir.join("deno").is_file(), "nothing may be left at the final name");
        assert!(!dir.join("deno.partial").is_file(), "and no partial either");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole thing, against Deno's real release. Ignored by default
    /// because it moves 39 MB; run with `--ignored` when the pin changes.
    ///
    /// This is the control the unit tests above cannot be: they check the URL
    /// is well formed and the hash parses, and neither would notice a pin that
    /// 404s or an archive whose layout moved.
    #[test]
    #[ignore = "downloads 39 MB from Deno's release; run with --ignored"]
    fn the_pinned_release_really_downloads_and_runs() {
        let dir = std::env::temp_dir().join(format!("cordial-deno-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut seen: u64 = 0;
        let mut progress: Box<dyn FnMut(u64, Option<u64>)> = Box::new(|done, _| seen = seen.max(done));
        let binary = install(&dir, &mut *progress).expect("the pinned release must install");

        assert!(binary.is_file(), "no interpreter at {}", binary.display());
        assert!(!dir.join(ASSET).exists(), "the archive should be cleaned up");

        let out = std::process::Command::new(&binary)
            .arg("--version")
            .output()
            .expect("the extracted interpreter must be executable");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(VERSION), "got: {text}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
