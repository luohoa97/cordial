//! Asking `pvpn` whether a tunnel is actually up, without reimplementing it.
//!
//! `pvpn` (github.com/luohoa97/protun-unblocked) is a separate project: a
//! wrapper around Proton VPN's own Linux client that restores routing on a
//! failed connect, steers a free account onto the fastest reachable server,
//! and tells the truth about a tunnel that looks connected but is not
//! forwarding anything. Cordial has no business re-deriving any of that, so
//! this module shells out to it — the same instruction AGENTS.md gives for
//! Roblox's own client-settings CDN and cookie handling: brokered, never
//! reimplemented.
//!
//! **This module runs exactly one command, `pvpn status`, and nothing else.**
//! It does not call `pvpn up`, `pvpn down` or `pvpn hop`. Bringing a tunnel up
//! is a slow, disruptive act on its own terms — `pvpn`'s own README measures
//! ordinary connects at 12 to 45-plus seconds before the grace period even
//! starts, and a failed one briefly rewrites the default route before falling
//! back. Making a profile launch button also decide, silently, when to start
//! or stop that is a second surprising thing happening at the moment somebody
//! expected only a game to open. Whether and when to connect stays a decision
//! for whoever is running Cordial; this module only ever reads.
//!
//! ## Why the check is `Traffic: passing`, not `Status: Connected`
//!
//! Reading `pvpn`'s own `cmd_status` (`bin/pvpn`) matters here, not guessing at
//! its output. It asks two different questions and reports both: whether
//! Proton's client believes it holds a tunnel (`is_connected`), and whether
//! anything is actually passing through it (`net_works`). Its own comment
//! explains why the second one exists: "It keeps saying Connected after a
//! suspend while the transport underneath is dead, and a status command that
//! agrees with it is worse than useless — you act on it." A stale tunnel that
//! *presents* as connected is exactly the shape of failure this whole feature
//! exists to avoid — a profile believing it is on a separate address while it
//! is not — so this module only ever treats `Traffic: passing` as good news.
//! `Status: Connected` on its own is not enough, and is not tested for.
//!
//! ## What is measured here versus read
//!
//! `parse_status`'s `Traffic: passing` branch was exercised against pvpn's
//! real output, captured live on 2026-08-05 while actually connected to a free
//! server. The absence of any `Traffic:` line when disconnected is read
//! directly out of `cmd_status`, which only prints one at all inside its
//! `if is_connected` branch — not independently reproduced by disconnecting a
//! real tunnel, which this session deliberately avoided disturbing (see
//! ADR-016). Both are exercised by `parse_status`'s own tests below, each
//! labelled with which kind of evidence it rests on.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::process::Command;

/// What `pvpn status` actually told us, boiled down to the one question this
/// feature cares about: would a packet leaving right now go out the tunnel?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// A `Traffic: passing` line was present. The only state this module
    /// treats as "safe to launch".
    Passing,
    /// Anything else — disconnected, or connected but dead — carrying
    /// whatever `pvpn status` printed, so a refusal can show it.
    NotPassing(String),
}

/// Why `pvpn status` could not be asked at all.
#[derive(Debug)]
pub enum Error {
    /// Nothing named `pvpn` is on `PATH` (or at `CORDIAL_PVPN_BIN`). Distinct
    /// from a connection failure: this profile cannot be helped by waiting,
    /// it needs `pvpn` installed, and the refusal has to say so rather than
    /// read as an ordinary "not connected".
    NotInstalled,
    /// `pvpn` exists but the process could not be run or read for some other
    /// reason — carries what the OS said.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotInstalled => write!(
                f,
                "pvpn is not installed (nothing named {:?} on PATH). \
                 Install it from https://github.com/luohoa97/protun-unblocked, \
                 or point CORDIAL_PVPN_BIN at it.",
                binary_name().to_string_lossy()
            ),
            Error::Failed(detail) => write!(f, "could not run pvpn status: {detail}"),
        }
    }
}

/// `CORDIAL_PVPN_BIN` overrides the executable outright, the same override
/// shape as `CORDIAL_FLAGS` and `CORDIAL_PLUGIN_GRANTS` — a development switch
/// for tests and for pointing at a `pvpn` that is not on `PATH`, not a
/// supported per-profile setting. Which `pvpn` a profile uses is not a thing
/// two profiles could sensibly disagree about on one machine, unlike flags or
/// grants: it is one system-wide tool, not per-account state.
fn binary_name() -> OsString {
    std::env::var_os("CORDIAL_PVPN_BIN").unwrap_or_else(|| OsString::from("pvpn"))
}

/// Ask `pvpn status` what is actually true right now.
///
/// Read-only. `pvpn status` is documented and observed to make no change to
/// routing, the kill switch, or the account session — it only reports.
pub fn status() -> Result<Status, Error> {
    let output = Command::new(binary_name()).arg("status").output();
    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(Error::NotInstalled),
        Err(e) => return Err(Error::Failed(e.to_string())),
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(parse_status(&text))
}

/// The parsing, split out from the process spawn so it can be tested against
/// captured text rather than against whatever this machine's tunnel happens to
/// be doing right now.
fn parse_status(text: &str) -> Status {
    if text.lines().any(|line| line.trim() == "Traffic: passing") {
        Status::Passing
    } else {
        Status::NotPassing(text.trim().to_string())
    }
}

/// Guards `CORDIAL_PVPN_BIN`, which is process-wide while cargo runs tests in
/// parallel threads of one process. `pub(crate)` rather than private to this
/// module's own `tests`: `network.rs`'s tests set the same variable to reach
/// the same "not installed" path, and a second, independent mutex in that
/// module would not stop the two files' tests interleaving with each other —
/// only a single shared lock does. Same hazard `flags.rs`'s and `profile.rs`'s
/// own test mutexes exist for.
#[cfg(test)]
pub(crate) static PVPN_BIN_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_connected_run_is_read_as_passing() {
        // Captured verbatim: `pvpn status` on this developer's machine,
        // 2026-08-05, genuinely connected to a free Proton server. Not a
        // guess at the shape — the actual bytes a real run produced.
        let real = "Server list is outdated, updating... This may take a moment.\n\
                     Status: Connected\n\
                     Server: SG-FREE#5 in Singapore, Singapore\n\
                     Protocol: protun-tls\n\
                     Traffic: passing\n";
        assert_eq!(parse_status(real), Status::Passing);
    }

    #[test]
    fn a_stale_tunnel_that_claims_connected_is_not_passing() {
        // `cmd_status` in `bin/pvpn`: when `is_connected` is true but
        // `net_works` is false, it prints "Traffic: NONE - the tunnel is up
        // but dead." instead of "Traffic: passing" — read directly out of
        // that function, not reproduced by killing a real tunnel. This is the
        // exact case its own comment calls "worse than useless" to trust.
        let stale = "Status: Connected\nServer: US-FREE#15\nProtocol: protun-tls\n\
                      Traffic: NONE - the tunnel is up but dead.\n";
        assert!(matches!(parse_status(stale), Status::NotPassing(_)));
    }

    #[test]
    fn disconnected_has_no_traffic_line_at_all() {
        // `cmd_status` only enters its `if is_connected` branch — the one
        // that can print a `Traffic:` line at all — when connected. Read
        // directly out of that function rather than independently
        // reproduced: this session deliberately avoided disconnecting the
        // real tunnel already in use on the machine it was written on.
        let disconnected = "Status: Disconnected\nProtocol: protun-tls\n";
        assert!(matches!(parse_status(disconnected), Status::NotPassing(_)));
    }

    #[test]
    fn the_refusal_text_is_carried_for_the_launch_message() {
        let text = "Status: Disconnected\nProtocol: protun-tls";
        match parse_status(text) {
            Status::NotPassing(detail) => assert!(detail.contains("Disconnected"), "{detail}"),
            Status::Passing => panic!("disconnected must not read as passing"),
        }
    }

    #[test]
    fn a_pvpn_that_does_not_exist_is_reported_distinctly_from_a_failure() {
        // Measured, not assumed: CORDIAL_PVPN_BIN pointed at a path nothing
        // occupies, on this machine, right now.
        let _g = PVPN_BIN_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CORDIAL_PVPN_BIN", "/nonexistent/definitely-not-here/pvpn");
        let result = status();
        std::env::remove_var("CORDIAL_PVPN_BIN");
        assert!(matches!(result, Err(Error::NotInstalled)), "{result:?}");
    }

    /// Runs the real `pvpn` on machines that have it, skips everywhere else —
    /// same shape as `tests/profile_configuration.rs`'s `deno --version` check
    /// in `cordial-runtime`. Deliberately does not assert which `Status` comes
    /// back: that depends on this machine's actual connection at the moment
    /// the test runs, which a committed test must not assume. What it proves
    /// is narrower and still real — that asking a genuinely installed `pvpn`
    /// for its status does not error.
    #[test]
    fn a_real_installed_pvpn_answers_without_erroring() {
        let _g = PVPN_BIN_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CORDIAL_PVPN_BIN");
        if Command::new("pvpn").arg("version").output().is_err() {
            eprintln!("skipping: pvpn is not installed on this machine");
            return;
        }
        match status() {
            Ok(_) => {}
            Err(e) => panic!("a real, installed pvpn should answer status, got: {e}"),
        }
    }
}
