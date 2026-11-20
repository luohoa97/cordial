//! The C library's locale in a process that hosts the engine.
//!
//! **Why this exists.** `gtk_init` calls `setlocale(LC_ALL, "")`, and from then
//! on glibc reads numbers the way the user's language writes them. The engine
//! shares that libc: with `--host-libc` its `strtod`, `strtof`, `atof` and
//! `sscanf` are glibc's, and bionic, which it was built against, has no locale
//! but C (`bionic/locale.cpp`: "We only support two locales", both with `.` for
//! the decimal point). In a language that writes one half as `0,5` the engine
//! asks for `0.5`, glibc stops at the dot, and the answer is 0.
//!
//! Measured on 2026-11-17 with `LANG=ru_RU.UTF-8`, one binary and one signed-in
//! profile: Home drew its tiles on top of each other, and with `LC_NUMERIC=C`
//! added to the environment it did not. The core of a client that had been
//! running in that session names `ru_RU.UTF-8` as the process's `LC_NUMERIC`.
//!
//! **Why not `setlocale(LC_NUMERIC, "C")` after `gtk_init`.** That is the usual
//! advice and it leaves a gap: the engine's threads are already running when
//! the window opens, and between GTK's call and ours they would parse with a
//! comma. Telling GTK not to set the locale, and setting every category but
//! that one here, means the numeric locale never leaves C at all.
//!
//! **Why the rest is still the user's.** `LC_MESSAGES` is what translates GTK's
//! own widgets, and `LC_CTYPE` is what makes glibc treat text as UTF-8, which
//! bionic always does. Putting the whole process in C would lose both.

use libadwaita::gtk;

/// Every category `setlocale(LC_ALL, "")` would have set, except
/// `LC_NUMERIC`. Spelled out rather than derived, so that tidying this into
/// one `LC_ALL` call is a change somebody has to make on purpose; pinned by
/// `tests::numbers_are_never_adopted`.
const ADOPTED: [libc::c_int; 11] = [
    libc::LC_CTYPE,
    libc::LC_COLLATE,
    libc::LC_MESSAGES,
    libc::LC_MONETARY,
    libc::LC_TIME,
    libc::LC_PAPER,
    libc::LC_NAME,
    libc::LC_ADDRESS,
    libc::LC_TELEPHONE,
    libc::LC_MEASUREMENT,
    libc::LC_IDENTIFICATION,
];

/// Take the session's locale for everything but numbers, and stop GTK from
/// taking it for numbers too. Call before GTK is initialised; afterwards it
/// is too late and this does nothing.
pub fn adopt() {
    if gtk::is_initialized() {
        return;
    }
    gtk::disable_setlocale();
    let mut refused = 0;
    for category in ADOPTED {
        // SAFETY: a valid category and a NUL-terminated string. `setlocale`
        // is not safe against another thread reading the category being set,
        // and the engine's threads are running; GTK's own `LC_ALL` call ran
        // at this same point until now, so that is not new. What is new is
        // that the category the engine parses numbers with is not touched.
        if unsafe { libc::setlocale(category, c"".as_ptr()) }.is_null() {
            refused += 1;
        }
    }
    if refused > 0 {
        // What GTK would have warned about, in its place.
        eprintln!(
            "[gtk] locale: the C library does not have the locale this session asks for; {refused} of {} \
             categories stay in C",
            ADOPTED.len()
        );
    }
    let var = |name| std::env::var(name).ok();
    if let Some(asked) = numeric_asked_for(var("LC_ALL"), var("LC_NUMERIC"), var("LANG")) {
        println!("[gtk] locale: numbers stay in C for the engine; the session asks for {asked}");
    }
}

/// The numeric locale the environment names, when it is one that would have
/// changed anything. POSIX's order: `LC_ALL`, then the category, then `LANG`,
/// and an empty variable is an unset one.
fn numeric_asked_for(lc_all: Option<String>, lc_numeric: Option<String>, lang: Option<String>) -> Option<String> {
    let asked = [lc_all, lc_numeric, lang].into_iter().flatten().find(|v| !v.is_empty())?;
    let c = asked == "C" || asked == "POSIX" || asked.starts_with("C.");
    (!c).then_some(asked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_never_adopted() {
        assert!(!ADOPTED.contains(&libc::LC_NUMERIC));
        // Which would set it along with everything else.
        assert!(!ADOPTED.contains(&libc::LC_ALL));
        // And nothing is listed twice, so the count in the warning is honest.
        let mut seen = ADOPTED.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ADOPTED.len());
    }

    #[test]
    fn the_session_s_numeric_locale_is_read_in_posix_order() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(numeric_asked_for(None, None, s("ru_RU.UTF-8")), s("ru_RU.UTF-8"));
        assert_eq!(numeric_asked_for(None, s("de_DE.UTF-8"), s("en_US.UTF-8")), s("de_DE.UTF-8"));
        assert_eq!(numeric_asked_for(s("fr_FR.UTF-8"), s("C"), s("C")), s("fr_FR.UTF-8"));
        // An empty variable is an unset one.
        assert_eq!(numeric_asked_for(s(""), s(""), s("ru_RU.UTF-8")), s("ru_RU.UTF-8"));
    }

    #[test]
    fn a_c_locale_is_nothing_to_report() {
        let s = |v: &str| Some(v.to_string());
        for c in ["C", "POSIX", "C.UTF-8", "C.utf8"] {
            assert_eq!(numeric_asked_for(None, None, s(c)), None, "{c}");
        }
        assert_eq!(numeric_asked_for(None, s("C"), s("ru_RU.UTF-8")), None);
        assert_eq!(numeric_asked_for(None, None, None), None);
    }
}
