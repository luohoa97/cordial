//! An OS-level sandbox under the plugin process, when the host has one.
//!
//! ## What this is not
//!
//! **It does not replace the broker, and it cannot.** A sub-sandbox only ever
//! *subtracts* from what its parent holds — `bwrap` and `flatpak-spawn
//! --sandbox` both narrow, neither grants — so no amount of confinement lets a
//! plugin do something Cordial cannot. Every effect a plugin causes still has to
//! be performed by Cordial, which is [ADR-007] unchanged.
//!
//! That matters because "we sandbox the plugins now" is exactly the reasoning
//! someone will one day use to justify passing one a file descriptor. The
//! Flatpak manifest grants **Cordial** Discord's IPC socket; handing that socket
//! to a confined plugin still hands it a Discord IPC connection, and Discord's
//! IPC does considerably more than set presence. A sandbox does not narrow a
//! channel. Do not let this file become an argument for opening one.
//!
//! ## What it is
//!
//! A third layer under the two that already exist. A plugin is a Deno process
//! started with **no permissions at all** — no file, network, environment or
//! subprocess access ([`crate::host`]) — and everything it asks for is checked
//! by the broker. This adds kernel-enforced confinement *below* Deno, so a Deno
//! escape lands in an empty namespace rather than on the host.
//!
//! ## Why its absence is not a failure
//!
//! **A missing sandbox binary is a downgrade, not a hole**, and that is the only
//! reason it is safe to make this optional. The security model does not rest on
//! it: with `bwrap` absent, a plugin still has zero Deno permissions and still
//! reaches nothing except through the broker. So this is belt-and-braces, and a
//! missing belt is worth reporting rather than refusing to start over.
//!
//! The alternative — requiring `bwrap` before a plugin may run — would make a
//! packaging detail into a permission, which is the thing this deliberately does
//! not do. What is *not* acceptable is running without the layer and implying it
//! is there: [`Sandbox::describe`] is printed at spawn so the answer to "was this
//! plugin confined" is in the log rather than inferred from whether the host
//! happened to have a binary.
//!
//! ## Flatpak gets no OS layer, deliberately
//!
//! Inside a Flatpak `bwrap` cannot run — the outer sandbox blocks the
//! user-namespace nesting it needs — so the only route is `flatpak-spawn
//! --sandbox`, which requires `--talk-name=org.freedesktop.Flatpak` on
//! Cordial's manifest.
//!
//! **That grant is not taken, and the reason is that it is a bigger hole than
//! the one it would plug.** `--sandbox` and `--host` are the same D-Bus
//! interface: a name that can create a narrower sandbox can also run an
//! arbitrary command on the host, outside the sandbox entirely. Cordial's
//! manifest header has said so since it was written — "arbitrary command
//! execution on the host, which would hand every plugin the sandbox escape the
//! capability model exists to prevent — below the level any broker can see"
//! (ADR-002 §2) — and adding a sandbox escape to Cordial in order to sandbox
//! plugins is a net loss however it is framed.
//!
//! So a Flatpak install gets [`Sandbox::None`]: Deno's zero permissions and the
//! broker, which is what it had before this module existed. That is the
//! downgrade-not-a-hole case above, and it is reported at spawn like any other.
//!
//! [ADR-007]: ../../../docs/adr/ADR-007-host-resources-are-brokered.md

use std::path::{Path, PathBuf};
use std::process::Command;

/// Which containment a plugin actually got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    /// `bwrap`, on a host install.
    Bubblewrap,
    /// No OS layer. Deno's zero permissions and the broker only — which is
    /// every Flatpak install, deliberately, and any host without `bwrap`.
    None,
}

impl Sandbox {
    pub fn describe(self) -> &'static str {
        match self {
            Sandbox::Bubblewrap => "bwrap + Deno permissions + broker",
            // Says what is still true rather than only what is missing: a plugin
            // here is not unconfined, it is confined by one layer fewer.
            Sandbox::None => {
                "Deno permissions + broker (no OS sandbox available on this install)"
            }
        }
    }
}

/// Whether this process is itself inside a Flatpak.
///
/// `/.flatpak-info` is present in every Flatpak sandbox and nowhere else, which
/// is the same check `launch.rs` uses for the MangoHud hint. `FLATPAK_ID` is not
/// used: it is inherited by child processes, so a terminal launched from a
/// Flatpak reports itself as one.
fn in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// Where `deno` actually lives, and the prefix its own libraries live under.
///
/// **Not a detail that can be skipped.** The first version of this bound only
/// `/usr`, `/lib`, `/lib64` and `/bin`, which is where a distribution package
/// puts an interpreter and is not where anything else does. On the machine this
/// was written on `deno` is a Homebrew install under
/// `/var/home/linuxbrew/.linuxbrew/Cellar/deno/.../bin/deno`, so every sandboxed
/// plugin failed with `execvp deno: No such file or directory` — the plugin did
/// not start at all, which an integration test caught and a unit test would not
/// have.
///
/// Returns the binary's real path plus the directories that have to be visible
/// for it to run. Read-only, all of them: a plugin gets to execute the
/// interpreter, not to modify it.
fn interpreter() -> Option<(PathBuf, Vec<PathBuf>)> {
    let found = which_deno()?;
    let real = std::fs::canonicalize(&found).unwrap_or_else(|_| found.clone());
    let mut binds = vec![real.clone()];

    // A package manager that versions its installs keeps the shared libraries
    // one level up from the versioned directory, so binding the versioned
    // directory alone is not enough. Homebrew and Linuxbrew both use `Cellar`;
    // strip from there and bind the prefix that contains `lib`.
    let text = real.display().to_string();
    if let Some(cut) = text.find("/Cellar/") {
        binds.push(PathBuf::from(&text[..cut]));
    } else if let Some(bin) = real.parent().filter(|p| p.file_name().is_some_and(|n| n == "bin")) {
        if let Some(prefix) = bin.parent() {
            binds.push(prefix.to_path_buf());
        }
    }
    // The path as it appears on `PATH` too, when that is a symlink into the
    // above: resolving it here does not stop the loader from following it.
    if found != real {
        if let Some(parent) = found.parent().and_then(|p| p.parent()) {
            binds.push(parent.to_path_buf());
        }
    }
    Some((real, binds))
}

/// Whether `deno` can be found at all.
///
/// **Checked before spawning, because otherwise the failure is invisible.**
/// Under `bwrap` the command Cordial builds is `bwrap ... deno ...`, so
/// `Command::spawn` succeeds as soon as *bwrap* starts and the missing
/// interpreter only fails inside the sandbox, asynchronously. Cordial then
/// printed "started" for a plugin that never ran a line and recorded no health
/// error, so Settings showed nothing wrong either -- measured on 2026-09-02 by
/// running with `deno` off `PATH`: three plugins reported started, no
/// `plugin.log` was created, and no health file was written.
///
/// That is the same defect as a stub returning success, and it is why this is
/// a check rather than a comment.
pub fn interpreter_present() -> bool {
    which_deno().is_some()
}

/// The interpreter version Cordial fetches when the host has none.
///
/// Pinned rather than tracking latest, so a plugin that works today does not
/// stop working because somebody else cut a release. Bumping it is a commit.
pub const MANAGED_DENO_VERSION: &str = "2.9.6";

/// Where Cordial keeps an interpreter it fetched itself.
///
/// **The data directory, not the cache.** A cache is a thing a user is
/// encouraged to delete, and re-downloading 39 MB because somebody cleaned up
/// is rude. Versioned, so a future bump lands beside the old one rather than
/// overwriting a binary that a running plugin has open.
///
/// Under Flatpak this resolves inside the sandbox to
/// `~/.var/app/io.github.luohoa97.Cordial/data/...`, which is where it has to
/// be: the Flatpak deliberately takes no route to the host (see the module
/// note on `flatpak-spawn`), so an interpreter on the host's `PATH` is
/// invisible to it and this is the only place one can come from. Measured
/// 2026-09-02 that the directory is mounted `rw,nosuid,nodev` with **no
/// `noexec`**, and that a downloaded Deno 2.9.6 runs there against the GNOME
/// runtime's glibc 2.42 and executes a plugin-shaped module.
pub fn managed_deno_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("cordial").join("deno").join(MANAGED_DENO_VERSION))
}

/// The managed interpreter, if it has been fetched.
pub fn managed_deno() -> Option<PathBuf> {
    managed_deno_dir().map(|d| d.join("deno")).filter(|p| p.is_file())
}

/// `deno`, from the host first and Cordial's own copy second.
///
/// **`PATH` wins deliberately.** A distribution's `deno` is updated by the
/// distribution and is the one the user chose; Cordial's copy exists for hosts
/// that have none -- every packaging format but Arch, since `dnf5 list deno`
/// on Fedora and Debian's own source index both come back empty.
fn which_deno() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|d| d.join("deno")).find(|c| c.is_file())
    });
    on_path.or_else(managed_deno)
}

fn have(binary: &str) -> bool {
    Command::new(binary)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Pick the strongest layer this host can actually provide.
pub fn available() -> Sandbox {
    if in_flatpak() {
        // Not `flatpak-spawn --sandbox`: reaching it needs
        // `--talk-name=org.freedesktop.Flatpak`, and that same name grants
        // `--host`, which is arbitrary command execution outside the sandbox.
        // See the header. bwrap is not attempted either — inside a Flatpak it is
        // present and fails, and a failed spawn is a plugin that does not start
        // rather than one that starts unconfined.
        return Sandbox::None;
    }
    if have("bwrap") {
        return Sandbox::Bubblewrap;
    }
    Sandbox::None
}

/// Build the command that runs `deno run <entry>`, wrapped in whatever the host
/// can enforce.
///
/// The confinement is deliberately severe, because a plugin has no legitimate
/// use for any of it: no network, no writable filesystem, no session bus. The
/// entry module is bound read-only, and `/tmp` is a private empty tmpfs rather
/// than the host's — a plugin that writes there is writing into something that
/// vanishes with it.
///
/// **This used to say "no access to the user's home", and that was not true.**
/// [`interpreter`] binds the interpreter's enclosing prefix read-only, because a
/// versioned install keeps its shared libraries a level up from the binary, and
/// nothing caps that bind to somewhere outside `$HOME`. Where `deno` lives
/// decides what a plugin can read: `/home/linuxbrew/.linuxbrew/Cellar/deno/…`
/// binds Linuxbrew's prefix and nothing of the user's, which is the arrangement
/// on this machine — but `~/.local/bin/deno` binds `~/.local`, and that contains
/// `~/.local/share/cordial/profiles/` with the session token in it.
///
/// It is read-only in every case, and the reachable set is the interpreter's
/// prefix rather than the home directory. Narrowing the bind is the fix, and it
/// wants doing; until then the honest sentence is this one, because a comment
/// promising an isolation the code does not provide is the shape somebody
/// reasons about a plugin's reach from and gets wrong.
///
/// `--die-with-parent` is not decoration: without it a sandboxed plugin outlives
/// a Cordial that crashed, holding a pipe nothing reads.
/// Build the command that runs `entry`, optionally reloading it as it changes.
///
/// **`reload` is Deno's own `--watch`, not a watcher of Cordial's.** Deno
/// already restarts the module when its files change and has done since 1.x;
/// writing a second one here would mean a thread, a poll interval, and a
/// restart path to get wrong, to reimplement something the interpreter does
/// better. `--watch` rather than `--watch-hmr` because a plugin is a stdio
/// process rather than a module graph with hot-swappable state: its whole
/// contract is the handshake it performs on startup, so re-running it is the
/// correct reload, and `--watch-hmr` falls back to exactly that when
/// replacement fails anyway.
///
/// `--no-clear-screen` because Deno otherwise wipes the terminal Cordial is
/// logging to on every reload, taking the client's own output with it.
///
/// Only set for unpacked plugins in Developer mode -- see
/// `manifest::unpacked_dirs`. An installed plugin does not change under a
/// running client, so watching one is a thread doing nothing.
pub fn command(sandbox: Sandbox, entry: &Path, reload: bool) -> Command {
    let mut deno_args: Vec<&str> = vec!["run", "--no-prompt", "--quiet"];
    if reload {
        deno_args.push("--watch");
        deno_args.push("--no-clear-screen");
    }
    match sandbox {
        Sandbox::Bubblewrap => {
            let mut c = Command::new("bwrap");
            c.args(["--unshare-all", "--die-with-parent", "--new-session"])
                // Read-only system, so Deno itself and its libraries resolve.
                .args(["--ro-bind", "/usr", "/usr"])
                .args(["--ro-bind-try", "/lib", "/lib"])
                .args(["--ro-bind-try", "/lib64", "/lib64"])
                .args(["--ro-bind-try", "/bin", "/bin"])
                .args(["--ro-bind-try", "/etc/ssl", "/etc/ssl"])
                .args(["--proc", "/proc"])
                .args(["--dev", "/dev"])
                .args(["--tmpfs", "/tmp"]);

            // Wherever the interpreter actually is — see `interpreter`.
            let deno = match interpreter() {
                Some((real, binds)) => {
                    for b in binds {
                        let p = b.display().to_string();
                        c.args(["--ro-bind-try", &p, &p]);
                    }
                    real.display().to_string()
                }
                None => "deno".to_string(),
            };

            // Deno wants somewhere to put its cache. A tmpfs one keeps it off
            // the host and out of the user's real `DENO_DIR`, at the cost of a
            // cold cache per launch, which for a single local module is nothing.
            c.args(["--setenv", "HOME", "/tmp"])
                .args(["--setenv", "DENO_DIR", "/tmp/deno"]);

            // The one thing from the user's world, read-only.
            //
            // **The directory, not the file, when watching.** A bind mount of
            // a single file follows that inode, and every editor worth using
            // writes a temporary and renames over the original -- so the
            // sandbox would go on showing the old contents and `--watch` would
            // see nothing. Binding the plugin's own directory means the rename
            // is visible, and it is also the only arrangement in which a
            // plugin can have more than one module at all.
            //
            // Still read-only, still nothing else from the user's world.
            let inside = if reload {
                let dir = entry.parent().unwrap_or(Path::new("/"));
                let name = entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("entry.ts")
                    .to_string();
                c.args(["--ro-bind", &dir.display().to_string(), "/plugin"]);
                format!("/plugin/{name}")
            } else {
                c.args(["--ro-bind", &entry.display().to_string(), "/plugin/entry.ts"]);
                "/plugin/entry.ts".to_string()
            };
            c.args(["--chdir", "/"]).arg(deno).args(&deno_args).arg(inside);
            c
        }
        Sandbox::None => {
            let mut c = Command::new("deno");
            c.args(&deno_args).arg(entry);
            c
        }
    }
}

#[cfg(test)]
mod tests {

    /// **The interpreter check must not be skipped when `bwrap` exists.**
    ///
    /// The bug: under bubblewrap the command is `bwrap ... deno ...`, so
    /// `Command::spawn` succeeds the moment bwrap starts and a missing Deno
    /// fails inside the sandbox, out of sight. Cordial printed "started" for
    /// three plugins that never ran a line and wrote no health record, so
    /// Settings showed nothing wrong either -- measured 2026-09-02 with `deno`
    /// off `PATH`. This pins the predicate the spawn path now checks first.
    #[test]
    fn the_interpreter_is_looked_for_on_path_and_nowhere_else() {
        let dir = std::env::temp_dir().join(format!("cordial-deno-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // `which_deno` reads the process PATH, which is shared state, so this
        // restores it rather than leaving the suite in a strange world.
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", &dir);
        assert!(!super::interpreter_present(), "nothing named deno is in this directory");

        let fake = dir.join("deno");
        std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").unwrap();
        assert!(super::interpreter_present(), "a `deno` on PATH must be found");

        match original {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    #[test]
    fn every_layer_says_what_is_still_true_not_only_what_is_missing() {
        // The unconfined case is the one that matters: "no OS sandbox" must not
        // read as "no sandbox", because the Deno process still holds no
        // permissions at all and the broker still checks every request.
        let none = Sandbox::None.describe();
        assert!(none.contains("Deno permissions"), "{none}");
        assert!(none.contains("broker"), "{none}");
        assert!(Sandbox::Bubblewrap.describe().contains("broker"));
    }

    #[test]
    fn the_sandboxed_commands_still_run_the_same_deno_with_no_permissions() {
        // The layer must not quietly change what it wraps. Both forms run deno
        // with `--no-prompt` and no `--allow-*`; a sandbox that also handed the
        // plugin a permission would be a downgrade wearing the word "sandbox".
        for s in [Sandbox::Bubblewrap, Sandbox::None] {
            let c = command(s, Path::new("/plugins/x/main.ts"), false);
            let args: Vec<String> =
                c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert!(args.contains(&"--no-prompt".to_string()), "{s:?}: {args:?}");
            assert!(
                !args.iter().any(|a| a.starts_with("--allow-")),
                "{s:?} granted a Deno permission: {args:?}"
            );
        }
    }

    #[test]
    fn the_sandbox_shares_nothing_and_dies_with_cordial() {
        let args: Vec<String> = command(Sandbox::Bubblewrap, Path::new("/p/main.ts"), false)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--unshare-all".to_string()), "{args:?}");
        // Without this a sandboxed plugin outlives a Cordial that crashed.
        assert!(args.contains(&"--die-with-parent".to_string()), "{args:?}");
        // The host's /tmp is not the plugin's /tmp.
        assert!(args.contains(&"--tmpfs".to_string()), "{args:?}");
        // The entry module goes in read-only. A writable bind would let a
        // plugin rewrite its own entry point.
        let ro = args.windows(3).any(|w| w[0] == "--ro-bind" && w[2] == "/plugin/entry.ts");
        assert!(ro, "the entry module is not bound read-only: {args:?}");
    }

    /// **Reload binds the plugin's directory, and only when reloading.**
    ///
    /// A bind mount of a single file follows that inode, so an editor writing
    /// a temporary and renaming over the original leaves the sandbox showing
    /// the old contents -- `--watch` would see nothing and the whole feature
    /// would appear to work and never reload. The directory bind is what makes
    /// it work, and it is confined to the reloading case because it widens
    /// what the plugin can read.
    #[test]
    fn reloading_binds_the_directory_and_watches() {
        let on: Vec<String> = command(Sandbox::Bubblewrap, Path::new("/p/main.ts"), true)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(on.contains(&"--watch".to_string()), "{on:?}");
        // Deno otherwise wipes the terminal Cordial is logging to.
        assert!(on.contains(&"--no-clear-screen".to_string()), "{on:?}");
        assert!(
            on.windows(3).any(|w| w[0] == "--ro-bind" && w[1] == "/p" && w[2] == "/plugin"),
            "the plugin directory is not bound: {on:?}"
        );
        assert!(on.contains(&"/plugin/main.ts".to_string()), "{on:?}");

        // And an installed plugin gets neither: watching one is a thread doing
        // nothing, and the wider bind would be reach it has no use for.
        let off: Vec<String> = command(Sandbox::Bubblewrap, Path::new("/p/main.ts"), false)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!off.contains(&"--watch".to_string()), "{off:?}");
        assert!(
            !off.windows(3).any(|w| w[0] == "--ro-bind" && w[2] == "/plugin"),
            "an installed plugin must not get the directory: {off:?}"
        );
    }

    #[test]
    fn a_flatpak_install_takes_no_os_layer() {
        // Pinning the decision rather than the mechanism: the portal that would
        // provide one also grants host command execution, so `available()` must
        // not reach for it. This asserts the reasoning survives -- if someone
        // adds a Flatpak branch, this is what should stop them without reading
        // ADR-018 first.
        if in_flatpak() {
            assert_eq!(available(), Sandbox::None);
        }
    }
}
