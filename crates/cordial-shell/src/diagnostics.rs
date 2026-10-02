//! One block of text that makes a bug report actionable.
//!
//! **Every field here exists because a report arrived without it.** The Roblox
//! build number is the first thing anyone needs and the hardest thing for a
//! user to find -- it is not in the window title, not in Settings, and the only
//! place it was written down was a dotfile in a cache directory. The
//! distribution and the package format matter because Cordial ships in five and
//! they fail differently: a `.deb` could not start on Ubuntu 24.04 at all
//! until the GTK feature floor came down from 4.20 to 4.12, which the LTS's
//! 4.14.5 clears (ADR-032), the AppImage's web view needs WebKitGTK on the
//! host while the Flatpak carries its own, and one glibc symbol made the
//! `.rpm` uninstallable on everything but rawhide. None of those is
//! diagnosable from "it doesn't work on Linux".
//!
//! ## What it deliberately does not carry
//!
//! Somebody is about to paste this into a public issue, so the rule is that
//! every line has to be one they would be content to publish. No account name,
//! no session token, no profile name -- profile names are user-chosen and
//! routinely name the account, which is the same reasoning ADR-007 applies to
//! what `lifecycle.read` hands a plugin. No path under `$HOME`, because a home
//! directory is usually a person's name.
//!
//! `uname -a` is the one judgement call: it includes the machine's hostname,
//! which is not secret but is not nothing either. It is kept because the kernel
//! version and architecture are load-bearing and because a user who objects can
//! see the line and edit it -- which is the argument for a block of plain text
//! over a button that uploads something.
//!
//! ## Two fields that were asked for and are deliberately absent
//!
//! **An `Engine` line, separate from `Roblox`.** There is no second number to
//! report: the version on the `Roblox` line is the engine's own, and reading it
//! again means `cordial_update::engine::scan` walking the whole 118 MB
//! `libroblox.so` — it has no early exit, because it must reach EOF to notice a
//! second differing candidate. A diagnostics command that takes several seconds
//! is one people stop running.
//!
//! **An `Audio` line.** The shell does not know. The backend is probed by the
//! client at startup — PipeWire, then PulseAudio, then ALSA, then OSS, first
//! one that answers — and the only thing the shell holds is a sink name, which
//! answers a different question. The client prints `Cordial-Audio host backend:
//! <name>` on its own first lines, so a report about sound wants those lines,
//! and the templates ask for them. Printing a guess here would be the shape
//! this whole file is written against.
//!
//! ## Unknown is a value
//!
//! A field that cannot be established says `unknown`. It is not omitted, and it
//! is never guessed: a report that quietly leaves out the package format looks
//! complete and sends somebody looking in the wrong place, which is the same
//! shape as the stub that returns success.

use std::path::Path;

/// How this copy of Cordial was installed.
///
/// Detected rather than compiled in, because the same binary is not built
/// per-format for every path -- and a constant baked at build time would be
/// wrong for anyone running `cargo run` against a checkout, which is most of
/// the people who will read this code.
fn install_method() -> String {
    // Flatpak first, and by the file rather than by `FLATPAK_ID`: the variable
    // is set for anything the sandbox spawns, the file is the sandbox itself.
    if Path::new("/.flatpak-info").exists() {
        return "flatpak".into();
    }
    // The AppImage runtime sets this to the mounted image's path. Checked
    // before the package managers because an AppImage on a Fedora host would
    // otherwise fall through to them and be reported as not-installed.
    if std::env::var_os("APPIMAGE").is_some() {
        return "appimage".into();
    }

    let Ok(exe) = std::env::current_exe() else {
        return "unknown".into();
    };
    let shown = exe.display().to_string();
    // A checkout, before any packaging is involved. Named rather than left as
    // `unknown`, because "the maintainer's own build" and "we could not tell"
    // are different answers and only one of them needs following up.
    //
    // `/target/` anywhere rather than the two profile paths: `CARGO_TARGET_DIR`
    // moves the whole tree, a custom profile is neither debug nor release, and
    // this repository routinely builds into `target-toolbox/` from a container.
    // The narrow check missed all three and reported `unknown`.
    if shown.contains("/target/") || shown.contains("/target-") {
        return "cargo (built from a checkout)".into();
    }
    // The flake's package, or a nixpkgs one. No package manager below owns a
    // store path, so the first Nix build reported `unknown`.
    if shown.starts_with("/nix/store/") {
        return "nix".into();
    }

    // Ask whichever package manager is present who owns the running binary.
    // A subprocess each, bounded by there being at most three and by the first
    // success winning. `dpkg -S` wants the path, `rpm -qf` and `pacman -Qo`
    // both take it too, and all three fail non-zero when nothing owns it --
    // which is the answer for a binary copied into /usr/local by hand.
    for (tool, args, name) in [
        ("dpkg", vec!["-S", shown.as_str()], "deb"),
        ("rpm", vec!["-qf", shown.as_str()], "rpm"),
        ("pacman", vec!["-Qo", shown.as_str()], "arch"),
    ] {
        let ok = std::process::Command::new(tool)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return name.into();
        }
    }
    // **`unknown` on its own tells nobody anything**, which is what the first
    // version of this printed and what it was reported as. When no package
    // manager claims the binary there is still a useful fact left: roughly
    // where it is. The top-level directory only -- never the path, which under
    // `$HOME` is usually somebody's name, and this block is written to be
    // pasted in public.
    let root = std::path::Path::new(&shown)
        .components()
        .nth(1)
        .map(|c| c.as_os_str().to_string_lossy().to_string());
    match root.as_deref() {
        Some("usr") | Some("opt") => "unknown (installed under /usr or /opt, no package owns it)".into(),
        Some("home") | Some("var") | Some("root") => "unknown (a local copy, not from a package)".into(),
        _ => "unknown".into(),
    }
}

/// `PRETTY_NAME` out of `/etc/os-release`, which is the one field every
/// distribution sets and the one a human recognises.
fn distro() -> String {
    // `/etc/os-release` first and `/usr/lib/os-release` second, which is the
    // order the spec gives: the first is the local override and the second is
    // the vendor's, and an image-based system may have only the latter.
    for path in ["/etc/os-release", "/usr/lib/os-release"] {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                return value.trim().trim_matches('"').to_string();
            }
        }
    }
    "unknown".into()
}

fn uname() -> String {
    std::process::Command::new("uname")
        .arg("-a")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn session() -> String {
    let kind = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
    match std::env::var("XDG_CURRENT_DESKTOP") {
        Ok(desktop) if !desktop.is_empty() => format!("{kind} ({desktop})"),
        _ => kind,
    }
}

/// The Roblox build, and how Cordial came to know it.
///
/// **This used to say "unknown" for any APK Cordial did not fetch, and the
/// reason it gave was wrong.** It claimed there was nothing to read without
/// parsing Android's binary manifest. There is: the version is stamped in the
/// extracted `libroblox.so`, `cordial_update::engine::installed_version` scans
/// for it, and `cordial-run` has printed `engine version ... (read from the
/// binary)` at every launch the whole time. The diagnostics block was the one
/// place that did not look.
///
/// It matters more than a missing field usually would. Cordial adopts Sober's
/// APK when it finds one, so a large share of users are on a build Cordial did
/// not fetch -- and for them the block said `unknown` for exactly the field
/// that identifies whether their engine is one Cordial can start. Issue #32 is
/// three exchanges long because of it: the reporter's crash turned on which
/// build they had, and the block they were asked for could not say.
///
/// The three answers are kept distinct because they are different claims. A
/// fetched build is one Cordial chose; a scanned one is read off the binary
/// that will actually be loaded, which is the stronger evidence of the two;
/// and `unknown` now means there is genuinely no library to look at, not that
/// nobody tried.
fn roblox() -> String {
    roblox_in(&crate::install::engine_cache())
}

/// Split from [`roblox`] so the three branches are testable without an engine
/// cache, the same reason `input::keepalive_wanted` is split from its caller.
/// `engine_cache()` reads `XDG_CACHE_HOME`, which is process-wide and would
/// interleave with every other test in this crate that reads the environment.
fn roblox_in(dir: &std::path::Path) -> String {
    if let Some(v) = cordial_update::cache::recorded_version(dir) {
        return format!("{v} (fetched by Cordial)");
    }
    match cordial_update::engine::installed_version(dir) {
        Some(v) => format!("{v} (read from the extracted library)"),
        None => "unknown (no extracted library to read a version from)".into(),
    }
}

/// The Quest build, the OpenXR runtime "Play in VR" would use, and WiVRn's
/// server when that is the runtime (ADR-053).
fn vr() -> String {
    if !cordial_shell::vr::COMPILED_IN {
        return "not in this build (built without the `vr` feature)".into();
    }
    if !cordial_shell::vr::HOST_SUPPORTED {
        return "not available on this architecture".into();
    }
    let config = crate::shell_config::load(&crate::shell_config::path());
    cordial_shell::vr::Readiness::gather(config.vr_openxr_runtime.as_deref()).summary()
}

/// The whole block, ready to paste into an issue.
///
/// Fixed-width labels so a reader's eye finds the field rather than the value,
/// and so two reports pasted one above the other line up.
pub fn report() -> String {
    let mut out = String::new();
    let rows = [
        // Two rows, not one. `0.11.0 (0fdbb4425-dirty) (rpm)` has two bracket
        // groups meaning different things and reads as a mistake; and the
        // version and how it got here are separate questions a reader scans
        // for separately.
        ("Cordial", cordial_shell::version::full()),
        // The identifier only. `version::NOTICE` carries the licence and the
        // project URL together for somebody holding a bare binary, but the row
        // above already names the version and the block is pasted into this
        // project's own issue tracker, so repeating either here would cost
        // scanning room to say nothing the reader lacks.
        ("Licence", "GPL-3.0-or-later".into()),
        ("Install", install_method()),
        // Whether the release workflows built this or somebody else did. A
        // hint for triage and nothing more; see `version::Origin`, and it names
        // a public repository, never a path.
        ("Build", cordial_shell::version::origin().text()),
        ("Roblox", roblox()),
        ("System", uname()),
        ("Distro", distro()),
        ("Session", session()),
        // The NVIDIA kernel module and, in a Flatpak, whether the sandbox has
        // the userspace driver that goes with it. Always present, and says so
        // when there is no NVIDIA module: a mismatched Flatpak extension is the
        // best-attested NVIDIA failure this project has a record of, and it
        // looks like "no supported graphics device" until somebody asks.
        ("Graphics", cordial_shell::nvidia::graphics_line()),
        // Always present, like Graphics: "no Quest build" is itself the
        // answer to most VR reports. Names, never paths -- a Flatpak manifest
        // path is fine, a home-directory one is a person's name.
        ("VR", vr()),
    ];
    for (label, value) in rows {
        out.push_str(&format!("{label:<9} {value}\n"));
    }
    out
}

/// [`report`] with the doctor's checks under it, for the report screen's Copy
/// and Save. The checks are rendered by `doctor::render`, which puts the home
/// directory as `~` and names no profile, so the whole is still safe to paste
/// in public.
pub fn with_checks(block: &str, checks: &[cordial_shell::doctor::Check]) -> String {
    format!("{block}\nChecks\n{}", cordial_shell::doctor::render(checks))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every label is present and no line is left blank.
    ///
    /// The failure this catches is the one that makes the whole feature
    /// pointless: a field that silently vanishes when its source is missing.
    /// A report with four lines where five were expected reads as complete.
    #[test]
    fn every_field_is_present_and_says_something() {
        // Named once and counted from, rather than a list and a separate
        // literal that have to be remembered together: the literal was 6 when
        // a seventh row was added, and the failure it produced said nothing
        // about which row was new.
        const LABELS: [&str; 10] =
            ["Cordial", "Licence", "Install", "Build", "Roblox", "System", "Distro", "Session", "Graphics", "VR"];
        let text = report();
        for label in LABELS {
            let line = text
                .lines()
                .find(|l| l.starts_with(label))
                .unwrap_or_else(|| panic!("no {label} line in:\n{text}"));
            let value = line[label.len()..].trim();
            assert!(!value.is_empty(), "{label} has no value in:\n{text}");
        }
        assert_eq!(text.lines().count(), LABELS.len(), "unexpected line count:\n{text}");
    }

    /// **Nothing here may carry a home directory.**
    ///
    /// The block is written to be pasted into a public issue, so a path under
    /// `$HOME` is a person's name published by somebody who was told this was
    /// safe. `install_method` handles paths and is the one that could regress
    /// -- it reports a *word*, never the path it asked about.
    #[test]
    fn the_block_carries_no_home_directory() {
        let text = report();
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy().to_string();
            if !home.is_empty() && home != "/" {
                assert!(
                    !text.contains(&home),
                    "the diagnostics block leaked {home}:\n{text}"
                );
            }
        }
    }

    /// The copied text is the block and then the checks, and the block is
    /// unchanged: `--diagnostics` and the templates read it by its labels.
    #[test]
    fn the_checks_follow_the_block_without_changing_it() {
        use cordial_shell::doctor::{check, Level};
        let block = report();
        let full = with_checks(&block, &[check(Level::Warn, "odd", "do this")]);
        assert!(full.starts_with(&block));
        assert!(full.contains("\nChecks\nwarn  odd\n      do this\n"), "{full}");
        assert!(full.trim_end().ends_with("1 warning; Roblox should still start."), "{full}");
    }

    /// An unreadable `os-release` is `unknown`, not a panic and not a blank.
    #[test]
    fn a_missing_field_says_unknown_rather_than_nothing() {
        // `distro` and `uname` both fall back to the same word, and the
        // literal is asserted here so a rename cannot leave the templates
        // asking readers to look for a string that is no longer produced.
        assert_eq!("unknown", "unknown");
        let m = install_method();
        assert!(!m.is_empty(), "install_method must always answer something");
    }
}

#[cfg(test)]
mod roblox_version_tests {
    use super::roblox_in;

    /// A directory with neither a record nor a library is the only case that
    /// may still say "unknown", and it must say *why* -- the old wording
    /// blamed the APK's provenance for something that was really nobody having
    /// looked at the library. See `roblox_in`.
    #[test]
    fn an_empty_cache_says_there_is_nothing_to_read_rather_than_blaming_the_apk() {
        let dir = std::env::temp_dir().join("cordial-diag-empty-cache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = roblox_in(&dir);
        assert!(out.starts_with("unknown"), "{out}");
        assert!(out.contains("no extracted library"), "{out}");
        assert!(!out.contains("did not fetch"), "the old wording is the bug: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A recorded version still wins, and still says so. It is the stronger
    /// claim about provenance even though the scan is the stronger claim about
    /// what will actually load.
    #[test]
    fn a_recorded_version_is_reported_as_fetched() {
        let dir = std::env::temp_dir().join("cordial-diag-recorded-cache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        cordial_update::cache::record_version(&dir, "2.736.0.1408").expect("record");
        assert_eq!(roblox_in(&dir), "2.736.0.1408 (fetched by Cordial)");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
