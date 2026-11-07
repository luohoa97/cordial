//! Everything Cordial did not answer, in one place.
//!
//! ## Why this exists
//!
//! Cordial answers the Android platform for an engine that assumes Android is
//! there. When it does not have an answer, the gap shows up in four unrelated
//! places and three different formats: a generated libc stub returning zero, a
//! JNI class or method libjnivm has never heard of, an AGDK native the engine
//! called before it was registered, and a framework method here that returns a
//! placeholder because nobody has written the real thing yet.
//!
//! Each of those was already reported somewhere. None of them were reported
//! *together*, which is the form the question actually takes: **the client did
//! something wrong — what did we fail to tell it?** Answering that meant
//! grepping four kinds of line out of a log that also contains the engine's own
//! narration, and the fourth kind mostly did not exist.
//!
//! ## What it does not tell you
//!
//! **A gap listed here is not a cause.** This is a list of questions Cordial
//! answered badly or not at all; which one mattered is a separate investigation,
//! and this project's own history is largely of confident answers to that
//! question being wrong. The report is a work queue and a starting point for
//! bisecting, not a diagnosis.
//!
//! It is also **not a list of everything unimplemented** — only of what was
//! reached on that run. Something never called is never recorded, which is the
//! right behaviour for a report meant to be read after a failure, and the wrong
//! one for auditing coverage. Two runs that fail differently will list different
//! things.
//!
//! ## Where it goes
//!
//! Both stdout and a file beside the engine's own logs, because the two get read
//! in different situations: stdout when somebody is running the client from a
//! terminal, the file when they launched from the shell and are attaching
//! something to an issue afterwards. The file is overwritten each run — it
//! describes the run that just ended, and a growing file nobody trims is a file
//! nobody opens.

use std::collections::BTreeMap;
use std::ffi::{c_char, CStr};
use std::path::PathBuf;
use std::sync::Mutex;

/// Which kind of gap this is. The categories are the four seams, not a severity
/// ordering — a missing JNI method is not inherently worse than a stubbed libc
/// call, and pretending otherwise would put a guess in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A JNI class, method or field libjnivm was asked for and does not have.
    /// Captured from libjnivm's own `Constructed Unresolved symbol` line.
    Jni,
    /// A generated libc stub, from `build.rs`'s table. Returns zero.
    LibcStub,
    /// An AGDK native the engine called before, or without, it being registered.
    NativeNotRegistered,
    /// A framework method here that answered with something made up: an empty
    /// string, a zero, a default object. **The most interesting category and the
    /// least complete**, because every entry has to be written by hand at the
    /// site that does it — see [`placeholder`].
    Placeholder,
}

impl Kind {
    fn heading(self) -> &'static str {
        match self {
            Kind::Jni => "JNI classes, methods and fields libjnivm does not have",
            Kind::LibcStub => "libc stubs called (they return zero)",
            Kind::NativeNotRegistered => "AGDK natives called while unregistered",
            Kind::Placeholder => "framework calls answered with a made-up value",
        }
    }

    fn from_code(code: u32) -> Kind {
        match code {
            1 => Kind::LibcStub,
            2 => Kind::NativeNotRegistered,
            3 => Kind::Placeholder,
            _ => Kind::Jni,
        }
    }
}

/// Everything seen this run: (kind, detail) -> how many times.
///
/// A `BTreeMap` rather than a `Vec` because the same missing method is asked for
/// in a loop — `onLuaTextBoxPropertyChangedCallback` arrived tens of thousands of
/// times in one session — and a report has to say "this one, a lot" rather than
/// print it tens of thousands of times.
static SEEN: Mutex<BTreeMap<(Kind, String), u64>> = Mutex::new(BTreeMap::new());

/// Record one gap. Cheap enough for a hot path: a lock and a counter bump.
pub fn record(kind: Kind, detail: impl Into<String>) {
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    *seen.entry((kind, detail.into())).or_insert(0) += 1;
}

/// Say that this call answered with something invented.
///
/// Call it from the site that does the inventing, with what was returned:
///
/// ```ignore
/// unimplemented::placeholder("PackageManager.getPackageInfo", "an empty PackageInfo");
/// ```
///
/// **Naming the value matters more than naming the method.** "Not implemented"
/// tells the reader nothing they could act on; "returned an empty string for the
/// installer package name" tells them exactly which answer the engine then
/// reasoned from, which is the thing that goes wrong. `native/opensles.cpp`
/// makes the same argument for why a stub reports failure rather than success.
pub fn placeholder(what: &str, returned: &str) {
    record(Kind::Placeholder, format!("{what} -> {returned}"));
}

/// The C entry point, for `native/`.
///
/// # Safety
/// `detail` must be a NUL-terminated C string valid for the duration of the
/// call. It is copied before returning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cordial_unimplemented_record(kind: u32, detail: *const c_char) {
    if detail.is_null() {
        return;
    }
    // SAFETY: the caller's contract, and the bytes are copied here.
    let text = unsafe { CStr::from_ptr(detail) }.to_string_lossy().into_owned();
    record(Kind::from_code(kind), text);
}

/// Where the report is written, beside the engine's own logs.
///
/// The same directory Roblox narrates itself into, because somebody collecting
/// diagnostics for an issue is already being pointed at that folder and a second
/// location is a second thing to forget.
fn report_path() -> PathBuf {
    // `XDG_DATA_HOME` first, then `$HOME/.local/share`, which is what every
    // other path derivation in the tree does and what this one did not. Inside
    // a Flatpak that variable is the app's own data directory; going straight
    // to `$HOME/.local/share` there names the real home, which the sandbox
    // holds no `--filesystem=home` for, so the last-resort fallback would fail
    // to write in exactly the case it exists to cover.
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("cordial");
    // `files/appData/logs` is relative to the client's working directory, which
    // is the profile's data dir; the engine resolves it itself. This mirrors it
    // rather than deriving it, because the profile in force is `profile.rs`'s
    // answer and this module has no business re-deriving it wrongly.
    std::env::var_os("CORDIAL_UNIMPLEMENTED_LOG").map(PathBuf::from).unwrap_or_else(|| {
        std::path::Path::new("files/appData/logs/cordial-unimplemented.log")
            .exists()
            .then(|| PathBuf::from("files/appData/logs/cordial-unimplemented.log"))
            .unwrap_or_else(|| {
                let dir = PathBuf::from("files/appData/logs");
                if std::fs::create_dir_all(&dir).is_ok() {
                    dir.join("cordial-unimplemented.log")
                } else {
                    base.join("cordial-unimplemented.log")
                }
            })
    })
}

/// The report, grouped by kind and most-called first within each.
///
/// Returns the text as well as printing it, so a test can read it without
/// capturing stdout.
pub fn render() -> String {
    let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    render_from(&seen)
}

/// The formatting, over a map passed in.
///
/// Split from [`render`] because the register is a process-global and tests
/// that share one interfere: the first version of these tests passed run alone
/// and failed under `cargo test --workspace`, where a test recording a JNI entry
/// raced one asserting the JNI section was absent. Same arrangement, and the
/// same reason, as `window::busy_body` and `updater::dressing`.
fn render_from(seen: &BTreeMap<(Kind, String), u64>) -> String {
    if seen.is_empty() {
        return "=== nothing went unanswered this run ===\n".to_string();
    }

    let mut out = String::new();
    out.push_str("=== what Cordial did not answer this run ===\n");
    out.push_str(
        "Each line is a question the engine asked that Cordial answered badly or not at\n\
         all. This is a work queue, not a diagnosis: a gap here is not evidence that it\n\
         is the gap that broke anything. Only what was reached this run is listed.\n\n",
    );
    // The caveat that has to be on the report rather than only in this file's
    // documentation. Without it a run that lists no JNI gaps reads as "the JNI
    // surface is complete", which is exactly the false negative this report
    // produced the first time it was run: libjnivm's `Constructed Unresolved
    // symbol` sits inside `#ifdef JNI_TRACE`, so with the trace off there is
    // nothing to capture and the section is empty for the wrong reason.
    if !seen.keys().any(|(k, _)| *k == Kind::Jni) {
        out.push_str(
            "No JNI gaps are listed, and that is NOT evidence there were none: libjnivm only\n\
             emits `Constructed Unresolved symbol` when built with the JNI trace on. Rebuild\n\
             with -DCORDIAL_JNI_TRACE=ON to populate that section. It is very slow.\n\n",
        );
    }

    for kind in [Kind::Placeholder, Kind::Jni, Kind::NativeNotRegistered, Kind::LibcStub] {
        let mut rows: Vec<(&String, u64)> = seen
            .iter()
            .filter(|((k, _), _)| *k == kind)
            .map(|((_, detail), count)| (detail, *count))
            .collect();
        if rows.is_empty() {
            continue;
        }
        // Most-called first: the thing being asked for in a loop is usually the
        // thing the engine is stuck retrying.
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out.push_str(&format!("{} ({}):\n", kind.heading(), rows.len()));
        for (detail, count) in rows {
            out.push_str(&format!("  {count:>9}  {detail}\n"));
        }
        out.push('\n');
    }
    out
}

/// Print the report and write it beside the engine's logs.
pub fn report() {
    let text = render();
    print!("\n{text}");
    let path = report_path();
    match std::fs::write(&path, &text) {
        Ok(()) => println!("[unimplemented] also written to {}", path.display()),
        // Not worth failing a shutdown over, and the report is on stdout anyway.
        Err(e) => println!("[unimplemented] could not write {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A register of its own, so these do not race each other through the
    /// process-global one. That interference is not hypothetical: the first
    /// version of these tests passed individually and failed under
    /// `cargo test --workspace`.
    fn map(entries: &[(Kind, &str, u64)]) -> BTreeMap<(Kind, String), u64> {
        entries.iter().map(|(k, d, n)| ((*k, (*d).to_string()), *n)).collect()
    }

    #[test]
    fn repeats_are_counted_rather_than_repeated() {
        // The behaviour that makes this readable at all: one missing callback
        // arrived tens of thousands of times in a single session, and a report
        // that printed it once per call would be the log it was meant to replace.
        let text = render_from(&map(&[(Kind::Jni, "Class=`Foo`, Method=`bar`", 3)]));
        assert_eq!(text.matches("Class=`Foo`, Method=`bar`").count(), 1, "{text}");
        assert!(text.contains("3  Class=`Foo`, Method=`bar`"), "{text}");
    }

    #[test]
    fn a_placeholder_records_what_it_returned_not_just_that_it_failed() {
        // "Not implemented" is not actionable. The value handed back is what the
        // engine then reasons from, so it is the thing worth writing down -- the
        // same argument `native/opensles.cpp` makes for reporting failure rather
        // than returning a dead object.
        let text = render_from(&map(&[(
            Kind::Placeholder,
            "PackageManager.getInstallerPackageName -> an empty string",
            1,
        )]));
        assert!(text.contains("an empty string"), "{text}");
        assert!(text.contains("PackageManager.getInstallerPackageName"), "{text}");
    }

    #[test]
    fn the_report_says_it_is_a_work_queue_rather_than_a_cause() {
        // Load-bearing wording. Nine consecutive conclusions in this project's
        // history came from reading a plausible artefact and deciding it
        // explained the bug; a list of gaps invites exactly that, so it has to
        // say what it is not.
        let text = render_from(&map(&[(Kind::LibcStub, "some_symbol", 1)]));
        assert!(text.contains("not a diagnosis"), "{text}");
        assert!(text.contains("Only what was reached this run"), "{text}");
    }

    #[test]
    fn an_empty_jni_section_says_the_trace_was_off_rather_than_implying_none() {
        // The false negative this report produced on its first real run: it
        // listed no JNI gaps on a full signed-in session, which reads as "the
        // JNI surface is complete" and was really "the marker is behind
        // `#ifdef JNI_TRACE` and the trace was off".
        let text = render_from(&map(&[(Kind::LibcStub, "anything", 1)]));
        assert!(text.contains("NOT evidence there were none"), "{text}");
        assert!(text.contains("CORDIAL_JNI_TRACE"), "{text}");

        // And it does not say it when the section has something in it.
        let with_jni = render_from(&map(&[(Kind::Jni, "Class=`X`, Method=`y`", 1)]));
        assert!(!with_jni.contains("NOT evidence there were none"), "{with_jni}");
    }

    #[test]
    fn the_c_entry_point_maps_its_codes_to_the_right_headings() {
        // Exercises the global deliberately -- it is the only way to reach the
        // C entry point -- and asserts only on its own detail string, so it
        // cannot be disturbed by, or disturb, anything else in the register.
        let name = std::ffi::CString::new("from_cpp_marker").unwrap();
        // SAFETY: a live, NUL-terminated string for the duration of the call.
        unsafe { cordial_unimplemented_record(0, name.as_ptr()) };
        let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(seen.get(&(Kind::Jni, "from_cpp_marker".to_string())), Some(&1));
    }

    #[test]
    fn every_kind_has_a_heading_so_none_can_be_added_without_one() {
        for kind in [Kind::Jni, Kind::LibcStub, Kind::NativeNotRegistered, Kind::Placeholder] {
            let text = render_from(&map(&[(kind, "detail", 1)]));
            assert!(text.contains(kind.heading()), "{kind:?} missing its heading");
        }
    }
}
