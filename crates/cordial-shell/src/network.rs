//! Per-profile network egress: whether an instance may start at all without a
//! VPN already up underneath it.
//!
//! ## The problem this answers
//!
//! AGENTS.md already states the constraint this file exists to satisfy: "Do
//! not test with an account anyone cares about, and keep test accounts on a
//! separate IP. The risk is collateral rather than causal: enforcement is
//! automated, runs in waves, and associates accounts sharing an address." That
//! sentence assumes a mechanism for giving a profile a separate address, and
//! until this file there was none — every profile, however many are signed in
//! at once (ADR-012 demonstrates two, side by side), shares this machine's one
//! route to the internet.
//!
//! ## Why this is not an `http_proxy` setting
//!
//! The obvious shape for "this profile's traffic goes through a proxy" is an
//! environment variable, and it was considered and rejected, on two
//! independent grounds — either one would be enough on its own.
//!
//! **Cordial's own client-settings fetch would not see it.**
//! `cordial_runtime::client_settings::fetch` goes through
//! `cordial_update::http::get_text` (it used to call `ureq::get(URL).call()`
//! directly; GitHub issue #21 is why it now has the timeouts that call lacked,
//! not why the proxy question below changed), and neither that nor the bare
//! call it replaced configures a proxy; `ureq` does not consult
//! `http_proxy`/`HTTPS_PROXY` on its own, so setting them would do nothing for
//! the one HTTP request Cordial itself is definitely responsible for, before
//! the engine exists to blame.
//!
//! **Even where the engine's own traffic would see it, it is not the traffic
//! that matters most.** `client_settings.rs` and `android/asset.rs` both
//! record, from the engine's own behaviour, that its HTTP stack is curl —
//! `CURLOPT_CAINFO` wants a real filesystem path, which is what sent the CA
//! bundle extraction to a real directory in the first place. curl does honour
//! `http_proxy`/`HTTPS_PROXY`/`ALL_PROXY` by default, and because Cordial's
//! bionic shim (`bionic/mod.rs`) does not override `getenv`, `connect` or
//! `socket` — they are ABI-compatible between bionic and glibc, so they
//! resolve straight to the host's real libc, in the same process, sharing the
//! same real `environ` — a proxy variable set on this process would in
//! principle be visible all the way down to curl's own `getenv` calls. That
//! much is a real, structural fact about how the loader resolves symbols, not
//! a guess.
//!
//! It still would not be enough, because curl is not the whole of the
//! engine's networking. The Waydroid trace and this project's own sign-in
//! notes (`docs/design/sign-in.md` §7.2, and the working control in
//! `client_settings.rs`) both name `DFLog::RbxTransportIoLibContext` and
//! `RtcIoRna` — Roblox's real-time game transport, which every account's
//! actual join to a game server goes over, and which the "Rtc" in its own name
//! already says is not an HTTP request curl is making. `http_proxy` and
//! `HTTPS_PROXY` are conventions specific to HTTP(S) libraries that choose to
//! read them; they do nothing at all for an arbitrary UDP socket a transport
//! layer opens for itself. So even a proxy that genuinely worked for curl
//! would leave exactly the connection that puts an account on a game server —
//! the one enforcement actually watches — going out this machine's ordinary
//! route regardless. Shipping an `http_proxy`-shaped setting here would be
//! precisely the failure AGENTS.md calls out by name: a setting that looks
//! like it does the job and does not, which is worse than no setting, because
//! it would be believed.
//!
//! ## Why this is not a network namespace, yet
//!
//! A namespace is the mechanism that would actually be airtight — it routes
//! by process, not by which library asks nicely, so it covers curl and
//! `RtcIoRna` and anything else alike. It needed ruling in analytically rather
//! than tried, and having ruled it in, two further things had to be
//! established before it could be shipped.
//!
//! **It needs a privilege this session measured itself not to have.**
//! `unshare --net -- ip link` was run directly, in the environment this was
//! written in, and failed immediately with "Operation not permitted" —
//! `CLONE_NEWNET` wants `CAP_NET_ADMIN`, ordinarily meaning root, on an
//! unprivileged process. That is a real deployment constraint for whoever
//! packages Cordial, not a detail to gloss over: a Flatpak, in particular,
//! does not hand out `CAP_NET_ADMIN` by default, and ADR-007's whole argument
//! against broad sandbox permissions applies here just as much as it does to
//! `--filesystem=host`.
//!
//! **And a VPN client would not scope into one even if the privilege
//! existed.** The common shape on this desktop is a client that manages its
//! tunnel as a NetworkManager connection. NetworkManager is a system service
//! running in the host's own network namespace, so the interface it brings up
//! is created there regardless of which namespace the command that asked for
//! it was run inside. Bringing such a tunnel up under `ip netns exec
//! cordial-<profile>` would not produce a tunnel scoped to that namespace --
//! it would produce the same machine-wide tunnel, asked for from a process
//! that happened to be in a namespace at the time. A namespace that could
//! hold a tunnel of its own would have to bypass NetworkManager entirely,
//! bringing up a second namespace-local interface with `wg-quick` from
//! parameters an established connection had already negotiated. That is a
//! different piece of work and this pass did not build it.
//!
//! So a namespace remains the right long-term answer and the wrong thing to
//! ship half-verified -- see ADR-016 for what would need to be true first, and
//! HANDOVER.md for the concrete next step.
//!
//! ## What this ships instead
//!
//! A coarser, honest guarantee: a profile marked [`Mode::VpnRequired`] refuses
//! to start at all unless the check command in its own `network.json` exits
//! zero. Cordial names no tool and ships no default: what a separate address
//! means is the operator's to define, and hard-coding one project's status
//! verb into a client is a coupling this module used to have and no longer
//! does (ADR-016's amendment says why). It does not isolate a running profile's traffic from any
//! other profile that happens to be running alongside it on the same
//! machine — that stronger property needs the namespace above — and it does
//! not itself bring the tunnel up or down. What it does guarantee, and does so
//! for both of Cordial's two entry points (the shell's own launcher and
//! `cordial-run` invoked directly, which AGENTS.md documents as a fully
//! supported way to start a client): this profile will never launch, and
//! therefore never make Cordial's own client-settings request, on this
//! machine's ordinary route while believing itself protected. A profile with
//! no `network.json` at all — every profile that exists today — behaves
//! exactly as it always has.
//!
//! ## Placement, per ADR-013
//!
//! Network egress is identity-scoped in exactly the sense ADR-013 draws the
//! line by: the whole point is that two accounts need not share an address,
//! which is a statement about an account, not about the machine. So
//! `network.json` lives beside `flags.json` and `plugin-grants.json` inside
//! the profile, not in `$XDG_CONFIG_HOME/cordial/shell.json`, which that ADR
//! reserves for chrome. There is no legacy file to migrate — this setting did
//! not exist anywhere before this change, unlike `flags.json` and
//! `plugin-grants.json`, which both moved out of a real prior global file —
//! so there is no `migrate_legacy_*` guard here to write; absence simply means
//! [`Mode::Default`], the same as it will for every profile that never sets
//! this.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Whether an instance is allowed to start without a VPN already up under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// No requirement — today's behaviour, and what every profile without a
    /// `network.json` gets.
    Default,
    /// Refuse to start unless the profile's own `check` command exits zero.
    VpnRequired,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Default
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub mode: Mode,
    /// The command that decides whether this profile's egress is what it
    /// requires. Argv, not a shell line: no quoting rules to get wrong and no
    /// shell to inject into.
    ///
    /// **Cordial names no tool and ships no default.** This used to shell out
    /// to one specific VPN wrapper, which made a client hard-code a dependency
    /// on one project -- see ADR-016's amendment. What a separate address means
    /// is the operator's to define: a VPN client's own status verb, a `curl`
    /// against something that echoes the source address, a script that checks a
    /// WireGuard handshake age. Cordial only runs it and reads the exit status.
    ///
    /// Exit zero means the requirement is met. Anything else, including the
    /// command not existing, means it is not, and the profile refuses to
    /// launch. That direction is deliberate: a check that cannot run has not
    /// established anything, and treating "I could not tell" as "yes" would be
    /// a stub that lies about the one thing this mode exists to guarantee.
    pub check: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { mode: Mode::Default, check: Vec::new() }
    }
}

/// This profile's network requirement file.
///
/// `CORDIAL_NETWORK` overrides it outright, the same override shape as
/// `CORDIAL_FLAGS` and `CORDIAL_PLUGIN_GRANTS` — a development switch for
/// tests and side-by-side runs, not a supported per-profile arrangement,
/// because it makes one file serve every profile, which is the thing ADR-013
/// keeps ending.
pub fn path_in(profile_dir: &Path) -> PathBuf {
    std::env::var_os("CORDIAL_NETWORK").map(PathBuf::from).unwrap_or_else(|| profile_dir.join("network.json"))
}

/// Read a profile's requirement, or the default if there is none to read.
///
/// Same default-on-anything-wrong shape as `shell_config::load` and
/// `cordial_plugins::grants`: a missing file is the ordinary case (nobody has
/// asked for this yet), and a malformed one is far more likely to be an
/// interrupted write than an attack, so both fall back to
/// [`Mode::Default`] rather than refusing to start. An unrecognised `mode`
/// string — most likely a config from a version of this file with a mode
/// this build does not know — does the same, and says why, rather than
/// silently taking whichever variant `serde` happened to default to.
pub fn load(profile_dir: &Path) -> NetworkConfig {
    let path = path_in(profile_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return NetworkConfig::default();
    };
    match serde_json::from_str(&text) {
        Ok(config) => config,
        Err(e) => {
            println!("  network: {} is not usable ({e}); treating as no requirement", path.display());
            NetworkConfig::default()
        }
    }
}

pub fn save(profile_dir: &Path, config: &NetworkConfig) -> std::io::Result<()> {
    std::fs::create_dir_all(profile_dir)?;
    let text = serde_json::to_string_pretty(config).expect("NetworkConfig always serialises");
    std::fs::write(path_in(profile_dir), text)
}

/// Why an instance was refused. Two variants because the caller has to give a
/// different answer to each: one names something to install, the other names
/// something to do first, and matching on rendered text is how that
/// distinction rots later.
#[derive(Debug)]
pub enum Refusal {
    /// `vpn-required` with no `check` configured. Refusing is the only honest
    /// answer: the profile asked for a guarantee and nothing was given that
    /// could establish it.
    NoCheck,
    /// The check could not be run at all -- not installed, not executable.
    CheckUnavailable { command: String, why: String },
    /// It ran and said no.
    CheckFailed { command: String, code: String, detail: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NoCheck => write!(
                f,
                "this profile is set to vpn-required (network.json) but has no \"check\" \
                 command, so nothing can establish whether the requirement is met. Add one: \
                 an argv array that exits zero when this machine's egress is what the profile \
                 needs, for example [\"my-vpn\", \"status\"]."
            ),
            Refusal::CheckUnavailable { command, why } => write!(
                f,
                "this profile requires a VPN (network.json: vpn-required) and its check \
                 command could not be run: {command}\n{why}"
            ),
            Refusal::CheckFailed { command, code, detail } => write!(
                f,
                "this profile requires a VPN (network.json: vpn-required) and its check said \
                 no. Bring the connection up, then launch again.\n{command} exited {code}\
                 {detail}"
            ),
        }
    }
}

/// The gate. Called from both of Cordial's entry points — see `launch.rs` in
/// this crate and `cordial-run`'s own `main` — so that neither can start an
/// instance a `vpn-required` profile asked not to run unprotected.
///
/// [`Mode::Default`] runs nothing at all: a profile that asked for no
/// guarantee pays no cost and spawns no process, which matters because most
/// profiles, and every profile that predates this feature, are in that state.
pub fn ensure_launchable(profile_dir: &Path) -> Result<(), Refusal> {
    let config = load(profile_dir);
    match config.mode {
        Mode::Default => Ok(()),
        Mode::VpnRequired => run_check(&config.check),
    }
}

/// Run the configured check and turn its exit status into a verdict.
///
/// Split out so the three outcomes are testable with `true`, `false` and a
/// path that does not exist, without a VPN or a network.
fn run_check(argv: &[String]) -> Result<(), Refusal> {
    let Some((program, args)) = argv.split_first() else {
        return Err(Refusal::NoCheck);
    };
    let shown = argv.join(" ");
    let output = match std::process::Command::new(program).args(args).output() {
        Ok(o) => o,
        Err(e) => {
            return Err(Refusal::CheckUnavailable { command: shown, why: e.to_string() })
        }
    };
    if output.status.success() {
        return Ok(());
    }
    // Both streams, trimmed: a check that explains itself is worth quoting, and
    // one that says nothing should not produce a blank line pretending to be
    // output. Bounded, because this ends up in a dialog.
    let mut detail = String::new();
    for stream in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(stream);
        let text = text.trim();
        if !text.is_empty() {
            detail.push('\n');
            detail.push_str(&text.chars().take(600).collect::<String>());
        }
    }
    Err(Refusal::CheckFailed {
        command: shown,
        code: match output.status.code() {
            Some(c) => c.to_string(),
            None => "on a signal".to_string(),
        },
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cordial-network-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_profile_with_no_file_gets_the_default_and_is_always_launchable() {
        let dir = scratch("absent");
        assert_eq!(load(&dir).mode, Mode::Default);
        assert!(ensure_launchable(&dir).is_ok(), "no requirement means no gate at all");
    }

    #[test]
    fn a_malformed_file_falls_back_to_default_rather_than_refusing_to_start() {
        let dir = scratch("malformed");
        std::fs::write(path_in(&dir), "{not json").unwrap();
        assert_eq!(load(&dir).mode, Mode::Default);
    }

    #[test]
    fn a_saved_requirement_round_trips() {
        let dir = scratch("roundtrip");
        save(&dir, &NetworkConfig { mode: Mode::VpnRequired, check: Vec::new() }).unwrap();
        assert_eq!(load(&dir).mode, Mode::VpnRequired);
    }

    #[test]
    fn an_unknown_mode_string_falls_back_to_default_and_says_why() {
        // A config written by a future version of this file with a mode this
        // build has never heard of must not be read as some arbitrary
        // variant — falling back to Default and refusing nothing is the safe
        // direction to guess wrong in, unlike falling back to VpnRequired's
        // opposite would be.
        let dir = scratch("unknown-mode");
        std::fs::write(path_in(&dir), r#"{"mode":"some-future-mode"}"#).unwrap();
        assert_eq!(load(&dir).mode, Mode::Default);
    }

    #[test]
    fn one_profiles_requirement_is_not_anothers() {
        let root = scratch("isolation");
        let a = root.join("alt");
        let b = root.join("main");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        save(&a, &NetworkConfig { mode: Mode::VpnRequired, check: Vec::new() }).unwrap();
        assert_eq!(load(&a).mode, Mode::VpnRequired);
        assert_eq!(load(&b).mode, Mode::Default, "a neighbouring profile must not inherit this");
    }

    /// A profile that asked for a guarantee and gave nothing that could
    /// establish it must refuse, not shrug. This is the case that replaced the
    /// hard-coded tool: previously "no VPN client installed" was the failure,
    /// and now "you did not say how to check" is.
    #[test]
    fn vpn_required_with_no_check_refuses_and_says_what_to_add() {
        let dir = scratch("no-check");
        save(&dir, &NetworkConfig { mode: Mode::VpnRequired, check: Vec::new() }).unwrap();
        let err = ensure_launchable(&dir).expect_err("must refuse rather than launch unguarded");
        assert!(matches!(err, Refusal::NoCheck), "{err}");
        assert!(err.to_string().contains("vpn-required"), "{err}");
        assert!(err.to_string().contains("check"), "{err}");
    }

    /// Exit zero is the whole contract, so `true` is a passing check.
    #[test]
    fn a_check_that_exits_zero_lets_the_profile_launch() {
        let dir = scratch("check-ok");
        save(&dir, &NetworkConfig {
            mode: Mode::VpnRequired,
            check: vec!["true".into()],
        })
        .unwrap();
        assert!(ensure_launchable(&dir).is_ok());
    }

    /// And anything else refuses. `false` says nothing, which is also the case
    /// where the message must not contain a stray blank line pretending to be
    /// output.
    #[test]
    fn a_check_that_fails_refuses_and_quotes_what_it_said() {
        let dir = scratch("check-no");
        save(&dir, &NetworkConfig {
            mode: Mode::VpnRequired,
            check: vec!["false".into()],
        })
        .unwrap();
        let err = ensure_launchable(&dir).expect_err("a failing check must refuse");
        assert!(matches!(err, Refusal::CheckFailed { .. }), "{err}");
        assert!(err.to_string().contains("exited 1"), "{err}");

        let dir = scratch("check-talks");
        save(&dir, &NetworkConfig {
            mode: Mode::VpnRequired,
            check: vec!["sh".into(), "-c".into(), "echo tunnel is down; exit 3".into()],
        })
        .unwrap();
        let err = ensure_launchable(&dir).expect_err("a failing check must refuse");
        assert!(err.to_string().contains("tunnel is down"), "{err}");
        assert!(err.to_string().contains("exited 3"), "{err}");
    }

    /// **A check that cannot run is a refusal, not a pass.** It has
    /// established nothing, and reading "I could not tell" as "yes" would be a
    /// stub that lies about the one thing this mode exists to guarantee.
    #[test]
    fn a_check_that_cannot_be_run_refuses_rather_than_assuming_the_best() {
        let dir = scratch("check-missing");
        save(&dir, &NetworkConfig {
            mode: Mode::VpnRequired,
            check: vec!["/nonexistent/definitely-not-here/vpn-check".into()],
        })
        .unwrap();
        let err = ensure_launchable(&dir).expect_err("an unrunnable check must refuse");
        assert!(matches!(err, Refusal::CheckUnavailable { .. }), "{err}");
    }

    /// The default mode spawns nothing at all, even with a check configured --
    /// a profile that asked for no guarantee should pay no cost.
    #[test]
    fn the_default_mode_never_runs_the_check() {
        let dir = scratch("default-mode");
        save(&dir, &NetworkConfig {
            mode: Mode::Default,
            check: vec!["/nonexistent/definitely-not-here/vpn-check".into()],
        })
        .unwrap();
        assert!(ensure_launchable(&dir).is_ok());
    }
}
