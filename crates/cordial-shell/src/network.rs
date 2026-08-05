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
//! `cordial_runtime::client_settings::fetch` calls `ureq::get(URL).call()`
//! directly, with no proxy configured; `ureq` does not consult
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
//! **`pvpn` itself would not scope into one even if the privilege existed.**
//! Reading `bin/pvpn` in the sibling project settles this rather than
//! assuming it: `cmd_up` drives Proton's own Linux client, which manages its
//! tunnel as a NetworkManager connection (`nmcli con up`, `nmcli con show
//! --active`, and the kill-switch device `pvpnksintrf0` NetworkManager leaves
//! behind). NetworkManager is a system service running in the host's own
//! network namespace; the interface it brings up is created there regardless
//! of which namespace the command that asked for it was run inside. Running
//! `pvpn up` under `ip netns exec cordial-<profile>` would not produce a
//! tunnel scoped to that namespace — it would produce the exact same
//! machine-wide tunnel `pvpn up` always produces, asked for from a process
//! that happened to be in a namespace at the time. A namespace that could
//! actually hold a Proton tunnel of its own would need to bypass
//! NetworkManager entirely — extracting the WireGuard parameters an
//! established connection actually negotiated and bringing up a second,
//! namespace-local interface with `wg-quick` directly, which `pvpn` does not
//! expose today and this pass did not build.
//!
//! So a namespace remains the right long-term answer and the wrong thing to
//! ship half-verified this pass — see ADR-016 for what would need to be true
//! first, and HANDOVER.md for the concrete next step.
//!
//! ## What this ships instead
//!
//! A coarser, honest guarantee: a profile marked [`Mode::VpnRequired`] refuses
//! to start at all unless `pvpn` reports a tunnel that is actually passing
//! traffic right now (see `pvpn.rs` for why that check is stricter than
//! "connected"). It does not isolate a running profile's traffic from any
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
    /// Refuse to start unless `pvpn status` reports traffic actually passing.
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
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { mode: Mode::Default }
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
    PvpnNotInstalled(crate::pvpn::Error),
    VpnNotPassing(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::PvpnNotInstalled(e) => write!(
                f,
                "this profile requires a VPN (network.json: vpn-required), but {e}"
            ),
            Refusal::VpnNotPassing(detail) => write!(
                f,
                "this profile requires a VPN (network.json: vpn-required), and pvpn reports it is \
                 not passing traffic. Run `pvpn up` first, then launch this profile again.\n\
                 pvpn status said:\n{detail}"
            ),
        }
    }
}

/// The gate. Called from both of Cordial's entry points — see `launch.rs` in
/// this crate and `cordial-run`'s own `main` — so that neither can start an
/// instance a `vpn-required` profile asked not to run unprotected.
///
/// [`Mode::Default`] never touches `pvpn` at all: a profile that asked for
/// nothing pays no cost and takes no new dependency on `pvpn` being
/// installed, which matters because most profiles, and every profile that
/// exists before this change, will stay in that state.
pub fn ensure_launchable(profile_dir: &Path) -> Result<(), Refusal> {
    match load(profile_dir).mode {
        Mode::Default => Ok(()),
        Mode::VpnRequired => match crate::pvpn::status() {
            Ok(crate::pvpn::Status::Passing) => Ok(()),
            Ok(crate::pvpn::Status::NotPassing(detail)) => Err(Refusal::VpnNotPassing(detail)),
            Err(e) => Err(Refusal::PvpnNotInstalled(e)),
        },
    }
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
        save(&dir, &NetworkConfig { mode: Mode::VpnRequired }).unwrap();
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
        save(&a, &NetworkConfig { mode: Mode::VpnRequired }).unwrap();
        assert_eq!(load(&a).mode, Mode::VpnRequired);
        assert_eq!(load(&b).mode, Mode::Default, "a neighbouring profile must not inherit this");
    }

    #[test]
    fn a_vpn_required_profile_is_refused_when_pvpn_is_missing_and_the_message_says_so() {
        let dir = scratch("no-pvpn");
        save(&dir, &NetworkConfig { mode: Mode::VpnRequired }).unwrap();

        // Same override, and the same shared lock, as `pvpn.rs`'s own
        // missing-binary test: `CORDIAL_PVPN_BIN` is process-wide, and cargo
        // runs both files' tests in parallel threads of one process, so a
        // second, independent mutex here would not stop this test and
        // `pvpn.rs`'s from interleaving with each other.
        let _g = crate::pvpn::PVPN_BIN_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CORDIAL_PVPN_BIN", "/nonexistent/definitely-not-here/pvpn");
        let result = ensure_launchable(&dir);
        std::env::remove_var("CORDIAL_PVPN_BIN");

        let err = result.expect_err("must refuse rather than launch unprotected");
        assert!(matches!(err, Refusal::PvpnNotInstalled(_)), "{err}");
        assert!(err.to_string().contains("vpn-required"), "{err}");
    }
}
