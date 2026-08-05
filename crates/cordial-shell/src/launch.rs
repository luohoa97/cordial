//! Starting the client.
//!
//! The chooser used to print `no launch target wired into the standalone shell
//! yet` and return, which is the same failure as a stub that reports success:
//! the button looked live, nothing happened, and nothing said why. This module
//! is what it calls instead.
//!
//! **A separate process, not a thread.** ADR-012 makes an instance a window and
//! a window a process, and the practical half of that is crash isolation — the
//! engine bringing itself down must not take the launcher with it, because the
//! launcher is how the user gets back. It is also the shape Sober uses: its
//! engine process is separate from `sober_services`, the GTK4/libadwaita one.
//! Note that this is *not* the arrangement ADR-011 rules out; that paragraph is
//! about the engine's `wl_surface` needing to share a connection with the
//! window it is a subsurface of, and here each process builds its own window.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use cordial_shell::profile::Claim;

use crate::install::Build;
use crate::shell_config;

/// The binary this shell starts.
///
/// The Flatpak is a split app: `cordial-shell` is what a user starts and
/// `cordial-run` is what runs Roblox. Cargo's standard output layout is kept
/// deliberately for that reason — `target/release/cordial-shell` beside
/// `target/release/cordial-run` in a checkout is the same arrangement as
/// `/app/bin/cordial-shell` beside `/app/bin/cordial-run` in the package.
const LOADER: &str = "cordial-run";

/// How long the client is allowed to run. **Zero means no timer.**
///
/// This was 86400 — a day — and the comment here used to explain why, ending
/// "closing the engine's window does not end the process today. Until
/// `cordial-run` grows a close path, quitting means the timer or the task
/// manager." It has grown one, so the timer goes.
///
/// The day was never a session length. It was a backstop against a client
/// outliving its window and keeping a profile nobody could reopen, and it did
/// not even work: the launcher quitting is the *ordinary* case under ADR-012,
/// so a closed window routinely left a client reparented to `systemd --user`
/// holding a profile for the rest of the day. That happened on this
/// developer's machine — a client 31 minutes into 86400 seconds with nothing
/// on screen to close, and no launch possible until it was killed by hand.
///
/// A timer was the wrong shape for the problem. Somebody playing for an
/// afternoon should not be interrupted, and somebody who closed the window an
/// hour ago should not still be holding the profile; one number cannot satisfy
/// both. `cordial-run` now ends on its window closing, on `SIGTERM` and
/// `SIGINT`, and on `--run` when one is passed — three entry points into one
/// shutdown, and the `flock` is released by the process exiting however it
/// exits.
///
/// `--run` is unchanged and still the backstop for headless runs, CI, and
/// agents, where a client that never ends is exactly the hazard above. It is
/// opt-in now rather than the default.
const DEFAULT_RUN_SECONDS: u64 = 0;

/// Where `cordial-run` is.
///
/// The sibling of `current_exe`, and deliberately only that. One lookup covers
/// both layouts because both layouts are the same shape, which is the point of
/// keeping cargo's paths: a separate development branch that looked for
/// `target/release/` relative to the working directory would work in a
/// checkout and fail in the Flatpak, and nothing would notice until somebody
/// installed the package. `PATH` is the fallback for a deliberate install
/// elsewhere; there is no baked-in `/app/bin` and no configurable path, because
/// this binary is never separately installed.
pub fn loader_path() -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|d| d.join(LOADER)) {
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(LOADER);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "Cordial could not find {LOADER}, which should be installed beside the launcher. \
         This is a broken installation rather than a setting."
    ))
}

/// A client this launcher started.
pub struct Instance {
    child: Child,
    /// Kept so the command can be quoted back at the user if the process dies
    /// immediately — an exit code on its own says nothing about what was run.
    pub command_line: String,
}

impl Instance {
    /// Whether the client is already gone, and with what status.
    ///
    /// Polled a moment after launch rather than waited on. A `cordial-run`
    /// that exits within seconds has failed at load — a missing symbol, an APK
    /// it cannot read — and the launcher has to say so, because the only other
    /// evidence is on a stdout nobody is looking at when the shell was started
    /// from a desktop icon.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }
}

/// Start the client on `build`, holding `claim`'s profile.
///
/// `claim` is consumed and handed to the child: ADR-012's lock belongs to the
/// instance, and the instance is the process being spawned here. See
/// [`Claim::hand_to`].
///
/// `join_url` is a `roblox-player://` link the desktop handed the launcher,
/// already checked by [`crate::deep_link::accept`].
pub fn spawn(
    build: &Build,
    claim: Claim,
    run_seconds: Option<u64>,
    join_url: Option<&str>,
) -> Result<Instance, String> {
    // Before anything is spawned at all, not merely before the join happens.
    // `cordial-run` gates this too — see `network::ensure_launchable`'s own
    // doc for why the check has to live at both entry points — but refusing
    // here as well means a `vpn-required` profile launched with no VPN up
    // never pays for starting the 1.5 GB engine process just to have it exit
    // immediately; the user gets the same message a beat sooner and without
    // a window ever appearing.
    if let Err(refusal) = cordial_shell::network::ensure_launchable(claim.profile_dir()) {
        return Err(refusal.to_string());
    }

    let loader = loader_path()?;
    let run = run_seconds.unwrap_or(DEFAULT_RUN_SECONDS).to_string();

    let mut command = Command::new(&loader);
    command
        .arg("--lib-dir")
        .arg(&build.lib_dir)
        .arg("--apk")
        .arg(&build.apk)
        // Both are what README's own worked example passes and what every run
        // this project has recorded as working passed. `--host-libc` is marked
        // diagnostic in cordial-run's usage text and dropping it is a separate
        // experiment, not something to fold into wiring up a button.
        .arg("--host-libc")
        .arg("--game-activity")
        .arg("--run")
        .arg(&run);

    // The profile stops being a directory name and starts meaning something
    // here. `--profile` is the whole of it: the client resolves the directory
    // itself and everything inside it follows — the engine's `appData`, its
    // logs, the cookie store and the saved identity.
    //
    // `CORDIAL_FILES_DIR` alone was not enough, and the way it failed is worth
    // keeping. It moved only the engine's own data directory, while the cookie
    // and identity stores resolve through `profile::active()`, which without
    // the argument falls back to `profiles/default`. So picking any other
    // profile put the engine's files in the right place and its *session* in
    // the wrong one: cookies did not respect profiles, and every profile shared
    // one login. The bridge outlived the thing it was bridging to — `--profile`
    // landed and this was never moved over.
    //
    // The profile is passed and the settings inside it are not, deliberately.
    // One value decides where everything else lives, and an argument cannot
    // change while the client runs, which is exactly what the dynamic DFFlag
    // families exist for (ADR-013).
    let profile_name = claim
        .profile_dir()
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "the profile directory has no usable name".to_string())?;
    command.arg("--profile").arg(profile_name);

    // The deep link, if one is waiting. `--join-url` is the agreed contract with
    // `cordial-runtime`, which owns everything past this point: what a Roblox
    // launch payload means is the client's business and the launcher does not
    // parse it.
    //
    // **`Command::arg` is why this is safe to pass on.** The string came from a
    // browser acting on somebody's click, and it goes into the child's `argv`
    // directly — there is no shell anywhere in this path to quote for, and
    // nothing here interpolates it into a string or builds a path from it. The
    // scheme and the length were checked in `deep_link`; the rest is carried
    // untouched, because a launcher that rewrote the payload would be changing
    // which game it was asked to join.
    if let Some(url) = join_url {
        command.arg("--join-url").arg(url);
    }

    // ADR-011 makes Wayland the display backend and says X11 "is not developed
    // further", but `cordial-run` still defaults to X11 and takes Wayland only
    // on `CORDIAL_WAYLAND=1` — deliberately, from when that backend was not
    // real yet. It is real now: it is what the engine reached the landing page
    // and a signed-in session on. A launcher that quietly started the
    // superseded backend would mean the window this crate builds, its header
    // bar and its monitor fitting were all bypassed. `backend()` still needs
    // `WAYLAND_DISPLAY` as well, so on a host without a compositor this asks
    // for nothing and X11 is used anyway.
    command.env("CORDIAL_WAYLAND", "1");

    // Read from disk here rather than handed in, and that is a compromise
    // worth naming: the caller in `window.rs` already holds a live
    // `ShellConfig`, so this is a second read of the same thing. It is correct
    // today only because the settings window persists every toggle the moment
    // it is made, so the file is what the user last chose. Give this function a
    // `&ShellConfig` the next time `window.rs` is open for editing.
    let config = shell_config::load(&shell_config::path());

    // Feral GameMode: performance governor, raised priority, GPU performance
    // profile, screensaver inhibited, for as long as the client runs. The
    // client asks for it over D-Bus itself and defaults to on, so the only
    // thing to pass is a refusal — see `gamemode` in `cordial-run`'s
    // `load.rs`, which also reports what came of it. A machine without
    // gamemoded needs nothing here: the request fails and the launch carries
    // on, which is the whole point of it being a request rather than a wrapper.
    if !config.gamemode {
        command.env("CORDIAL_GAMEMODE", "0");
    }

    // The Graphics row, and **only when it is not Automatic**. That is not a
    // micro-optimisation: an absent variable is what tells the runtime the user
    // has no opinion, which is the one state in which a plugin's request is
    // allowed to count. Sending `automatic` explicitly would be the user
    // silently outvoting every plugin while the row says Automatic.
    //
    // A variable rather than a file because the backend has to be settled before
    // the engine's first `dlopen` of libvulkan, which is well before anything
    // opens a profile. See `cordial_runtime::graphics`.
    if config.graphics != "automatic" {
        command.env("CORDIAL_GRAPHICS", &config.graphics);
    }

    // MangoHUD is a Vulkan implicit layer, so `MANGOHUD=1` on the client's
    // environment is the entire mechanism — the loader finds the layer JSON on
    // its own and inserts it. The layer has to actually be installed, and the
    // switch is only offered when it is; see [`mangohud_layer`] for why that
    // check is not optional here.
    if config.mangohud {
        match mangohud_layer() {
            Some(layer) => {
                command.env("MANGOHUD", "1");
                // Frame rate, frame time graph and both loads — the four things
                // the owner wanted and Roblox's own overlay does not give. Set
                // rather than left to MangoHUD's default so that what the
                // switch turns on is a known overlay rather than whatever
                // happens to be in a config file somewhere.
                command.env("MANGOHUD_CONFIG", "fps,frametime,frame_timing=1,cpu_stats,gpu_stats");
                println!("  shell: MangoHUD on, via {}", layer.display());
            }
            // Reported rather than silently dropped. A switch that is on in the
            // settings file and does nothing at launch is the same defect as a
            // stub that returns success, and the settings page can only stop
            // somebody turning it on today — not stop them uninstalling
            // MangoHUD tomorrow with the switch left where it was.
            None => println!(
                "  shell: MangoHUD is switched on but its Vulkan layer is not installed; \
                 the overlay will not appear. {}", mangohud_install_hint()
            ),
        }
    }

    // Inherited rather than captured, so that a shell started from a terminal
    // still narrates the load the way it always has. Nothing here parses it.
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    claim.hand_to(&mut command);

    let command_line = describe(&loader, &build.lib_dir, &build.apk, &run, join_url);
    let child = command
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}\n\n{command_line}", loader.display()))?;

    // Dropped explicitly rather than left to fall off the end of the function,
    // because the ordering is the whole mechanism: the child now holds the
    // flock through its inherited descriptor, and the launcher must let go or
    // quitting the shell would be the thing that released it.
    drop(claim);

    Ok(Instance { child, command_line })
}

/// Whether this process is inside a Flatpak sandbox.
///
/// `/.flatpak-info` is the documented marker and is present in every sandbox
/// regardless of how the application was started, which `FLATPAK_ID` is not —
/// that one is absent when the entry point is `flatpak run --command=sh`.
pub fn in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// What to tell somebody who wants MangoHUD and has not got it.
///
/// **The two packages are not alternatives, and offering them as a pair sent
/// this developer to install the wrong one twice.** They install the same
/// overlay in two places that cannot see each other, and which one is right is
/// decided by how *Cordial* was installed, not by preference.
///
/// The Flatpak runtime extension's manifest declares
/// `library_path: /usr/lib/extensions/vulkan/MangoHud/lib/.../libMangoHud.so`,
/// which exists only inside a sandbox where the extension is mounted. Install
/// it while running a host build and the result is silence: the layer is not on
/// the host search path, and even if it were, the loader could not resolve the
/// library it names. The earlier wording listed both with "or", which reads as
/// two ways to accomplish one thing.
///
/// So the hint names one, and it is chosen rather than guessed.
pub fn mangohud_install_hint() -> &'static str {
    if in_flatpak() {
        "Install the Flatpak runtime extension: flatpak install \
         org.freedesktop.Platform.VulkanLayer.MangoHud — this build of Cordial runs inside a \
         Flatpak sandbox, so a distribution package installed on the host will not be visible to it."
    } else {
        "Install it with your package manager (Fedora: dnf install mangohud, Arch: pacman -S \
         mangohud). This build of Cordial runs on the host, so the Flatpak runtime extension \
         org.freedesktop.Platform.VulkanLayer.MangoHud will not work for it — that layer's library \
         path only exists inside a Flatpak sandbox."
    }
}

/// Where MangoHUD's implicit layer manifest is, or `None` if it is not
/// installed.
///
/// **This check exists because the alternative is a switch that appears to work
/// and does nothing.** `MANGOHUD=1` is not an error when there is no MangoHUD;
/// the Vulkan loader looks for an implicit layer, finds none, and the client
/// starts perfectly normally with no overlay and nothing said. That is
/// indistinguishable from a broken setting, and this project has already
/// shipped a settings page describing software nobody had installed twice.
///
/// The layer, not the `mangohud` binary. The binary is a shell wrapper that
/// exports this same variable; it is frequently absent on a Flatpak install
/// where the layer is very much present, so looking for it would report the
/// wrong answer in exactly the configuration Cordial ships in.
///
/// The directories are the Vulkan loader's own documented implicit-layer search
/// path, plus the Flatpak extension mount point. Filenames are matched by
/// prefix rather than listed — upstream ships `MangoHud.x86_64.json`,
/// `MangoHud.x86.json` and plain `MangoHud.json` depending on version and
/// architecture, and a fixed list would go stale silently.
pub fn mangohud_layer() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(x).join("vulkan/implicit_layer.d"));
    } else if let Some(h) = &home {
        dirs.push(h.join(".local/share/vulkan/implicit_layer.d"));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        dirs.push(PathBuf::from(x).join("vulkan/implicit_layer.d"));
    } else if let Some(h) = &home {
        dirs.push(h.join(".config/vulkan/implicit_layer.d"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for d in data_dirs.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d).join("vulkan/implicit_layer.d"));
    }
    dirs.push(PathBuf::from("/etc/vulkan/implicit_layer.d"));
    // The Flatpak runtime extension, which mounts here rather than anywhere
    // XDG_DATA_DIRS points at.
    dirs.push(PathBuf::from(
        "/usr/lib/extensions/vulkan/MangoHud/share/vulkan/implicit_layer.d",
    ));

    find_mangohud_layer_in(&dirs)
}

/// The scan, split from the search path so it can be tested against a directory
/// built for the purpose rather than against whatever this machine happens to
/// have installed. A test that asserted "MangoHUD is absent here" would pass for
/// the wrong reason on the machine it was written on and fail on somebody
/// else's.
fn find_mangohud_layer_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_ascii_lowercase();
            if name.starts_with("mangohud") && name.ends_with(".json") {
                return Some(entry.path());
            }
        }
    }
    None
}

/// The command line quoted back when the client dies at once.
///
/// It carries `--join-url` when there was one, because a launch that fails only
/// with a link on it is exactly the launch somebody needs to be able to repeat
/// in a terminal.
fn describe(loader: &Path, lib_dir: &Path, apk: &Path, run: &str, join_url: Option<&str>) -> String {
    let join = join_url.map(|u| format!(" --join-url {u}")).unwrap_or_default();
    format!(
        "{} --lib-dir {} --apk {} --host-libc --game-activity --run {run}{join}",
        loader.display(),
        lib_dir.display(),
        apk.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CORDIAL_PVPN_BIN` is only ever touched by this test, in this binary,
    /// so a local mutex is enough for it. `CORDIAL_PROFILE_ROOT` is not: it
    /// used to have one here too, private to this file, until that turned
    /// out to be exactly the shape of the flake `crate::PROFILE_ROOT_ENV`'s
    /// own doc comment records — two independent mutexes guarding one
    /// process-wide variable serialise nothing against each other. This test
    /// now shares that lock with `profile_switcher.rs` instead.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_vpn_required_profile_with_no_pvpn_refuses_before_the_loader_is_even_looked_for() {
        let _pvpn_guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _root_guard = crate::PROFILE_ROOT_ENV.lock().unwrap_or_else(|e| e.into_inner());

        let root = std::env::temp_dir().join("cordial-launch-gate-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("CORDIAL_PROFILE_ROOT", &root);
        std::env::set_var("CORDIAL_PVPN_BIN", "/nonexistent/definitely-not-here/pvpn");

        let claim = cordial_shell::profile::acquire("vpn-test").expect("a fresh profile is free");
        cordial_shell::network::save(
            claim.profile_dir(),
            &cordial_shell::network::NetworkConfig { mode: cordial_shell::network::Mode::VpnRequired },
        )
        .unwrap();

        let build = Build { apk: PathBuf::from("/nonexistent.apk"), lib_dir: PathBuf::from("/nonexistent") };
        let result = spawn(&build, claim, Some(1), None);

        std::env::remove_var("CORDIAL_PVPN_BIN");
        std::env::remove_var("CORDIAL_PROFILE_ROOT");
        let _ = std::fs::remove_dir_all(&root);

        // The message names the actual gap (pvpn missing), not a made-up APK
        // path or loader error -- proof the refusal happened before `spawn`
        // got anywhere near looking for `cordial-run` or the build.
        // `Result::expect_err` wants `Instance: Debug` for its own panic
        // message, which `Instance` deliberately does not derive (it holds a
        // live `Child`), so this matches instead.
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("a vpn-required profile with no pvpn must refuse to launch"),
        };
        assert!(err.contains("vpn-required"), "{err}");
        assert!(err.contains("pvpn"), "{err}");
    }

    #[test]
    fn the_loader_is_looked_for_beside_the_launcher_first() {
        // Under `cargo test` the test binary lives in target/debug/deps, so
        // this asserts the shape of the answer rather than a hit: either a
        // sibling or something on PATH, and a message naming the installation
        // rather than a setting when there is neither.
        match loader_path() {
            Ok(p) => assert!(p.ends_with(LOADER), "{}", p.display()),
            Err(e) => assert!(e.contains("broken installation"), "{e}"),
        }
    }

    #[test]
    fn the_quoted_command_line_is_one_someone_could_retype() {
        // It is shown when the client dies at once, which is the moment a user
        // most needs to be able to run the same thing in a terminal and read
        // what it printed.
        let line = describe(
            Path::new("/app/bin/cordial-run"),
            Path::new("/home/a/.cache/cordial/lib/x86_64"),
            Path::new("/home/a/base.apk"),
            "600",
            None,
        );
        assert!(line.contains("--lib-dir /home/a/.cache/cordial/lib/x86_64"), "{line}");
        assert!(line.contains("--apk /home/a/base.apk"), "{line}");
        assert!(line.contains("--run 600"), "{line}");
        assert!(!line.contains("--join-url"), "no link means no argument at all: {line}");
    }

    #[test]
    fn a_queued_link_is_passed_as_join_url_and_shows_up_in_the_quoted_command() {
        // The contract with `cordial-runtime`, which implements the other half.
        // Spelled out in a test because it is a string in two crates: change it
        // here and the client sees an argument it does not know, which is a
        // launch that fails for a reason nothing on screen explains.
        let line = describe(
            Path::new("/app/bin/cordial-run"),
            Path::new("/lib/x86_64"),
            Path::new("/base.apk"),
            "0",
            Some("roblox-player://placeId=1818"),
        );
        assert!(line.contains("--join-url roblox-player://placeId=1818"), "{line}");
    }

    #[test]
    fn a_launch_from_the_shell_carries_no_timer() {
        // The whole point of the close path, pinned where a well-meaning
        // change would undo it. Somebody looking at `--run 0` without the
        // history sees a placeholder and puts a "sensible" number back; what
        // they would actually be restoring is a session that ends mid-game and
        // a client that outlives its window, which is what a day of timer
        // produced here for months. `cordial-run` reads zero as no timer and
        // ends on the window closing, on SIGTERM and on SIGINT instead.
        assert_eq!(DEFAULT_RUN_SECONDS, 0, "the launcher must not impose a session length");
    }

    #[test]
    fn the_mangohud_hint_names_one_package_and_it_matches_how_cordial_was_installed() {
        // The two packages install the same overlay into two places that cannot
        // see each other, and the old hint listed both joined by "or". This
        // developer followed it and installed the Flatpak runtime extension
        // while running a host build -- twice -- and got silence, because that
        // layer's manifest names a library under /usr/lib/extensions which only
        // exists inside a sandbox.
        let hint = mangohud_install_hint();
        if in_flatpak() {
            assert!(hint.contains("flatpak install"), "{hint}");
            assert!(!hint.contains("dnf install"), "{hint}");
        } else {
            assert!(hint.contains("dnf install"), "{hint}");
            // It may name the Flatpak extension, but only to rule it out.
            assert!(hint.contains("will not work"), "{hint}");
        }
    }

    #[test]
    fn the_mangohud_layer_is_found_by_prefix_and_not_by_an_exact_filename() {
        // Upstream ships MangoHud.json, MangoHud.x86_64.json or
        // MangoHud.x86.json depending on version and architecture. A hardcoded
        // list would stop matching on some future rename and the only symptom
        // would be a settings row that says MangoHUD is not installed on a
        // machine where it is.
        let root = std::env::temp_dir().join("cordial-mangohud-detect/implicit_layer.d");
        let _ = std::fs::remove_dir_all(root.parent().expect("has a parent"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("VkLayer_MESA_device_select.json"), "{}").unwrap();

        assert!(
            find_mangohud_layer_in(&[root.clone()]).is_none(),
            "an unrelated implicit layer must not read as MangoHUD"
        );

        std::fs::write(root.join("MangoHud.x86_64.json"), "{}").unwrap();
        let found = find_mangohud_layer_in(&[root.clone()]).expect("the layer is there now");
        assert!(found.ends_with("MangoHud.x86_64.json"), "{}", found.display());

        // A directory that does not exist is the ordinary case rather than an
        // error: most of the Vulkan loader's search path is absent on any given
        // machine, and one missing entry must not stop the scan.
        let missing = root.join("nowhere");
        assert!(find_mangohud_layer_in(&[missing, root.clone()]).is_some());

        let _ = std::fs::remove_dir_all(root.parent().expect("has a parent"));
    }

    /// Everything the chooser row does, minus the click.
    ///
    /// `#[ignore]` because it starts the real 115 MB engine and needs a Roblox
    /// build, neither of which belongs in `cargo test --workspace`. It exists
    /// because the alternative evidence for "the launch button works" is
    /// somebody pressing it, and this project's rule is that a claim is worth
    /// what it was measured with — so the measurable part is written down and
    /// runnable rather than described.
    ///
    ///     cargo test --release --bin cordial-shell -- --ignored --nocapture
    ///
    /// Skips rather than fails when there is no build, and says so: a machine
    /// without one has nothing to disprove.
    #[test]
    #[ignore = "starts the real engine; needs a Roblox build"]
    fn a_launch_really_starts_the_client() {
        use crate::install;
        use cordial_shell::profile;

        // A test binary lives in `target/release/deps`, so `cordial-run` is not
        // its sibling and the production lookup correctly declines to find it.
        // Reaching it through the documented `PATH` fallback keeps that lookup
        // exactly as it ships rather than teaching it about test layouts.
        if let Ok(exe) = std::env::current_exe() {
            if let Some(release) = exe.parent().and_then(|p| p.parent()) {
                let path = std::env::var_os("PATH").unwrap_or_default();
                let mut dirs = vec![release.to_path_buf()];
                dirs.extend(std::env::split_paths(&path));
                std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
            }
        }

        // The cache rather than `temp_dir`: the engine writes its whole asset
        // and shader cache into the profile, which on a distribution where
        // `/tmp` is tmpfs means hundreds of megabytes of RAM. Removed at the
        // end, and named so that a run killed halfway is obvious.
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .unwrap_or_else(std::env::temp_dir)
            .join("cordial-shell-launch-e2e");
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("CORDIAL_PROFILE_ROOT", &root);

        let build = match install::locate(&install::RobloxInstall::default()) {
            Ok(build) => build,
            Err(e) => {
                println!("no Roblox build on this machine, nothing to prove: {e:?}");
                return;
            }
        };
        println!("build: {} + {}", build.apk.display(), build.lib_dir.display());

        let claim = profile::acquire("e2e").expect("a fresh profile is free");
        let profile_dir = claim.profile_dir().to_path_buf();
        let mut instance = spawn(&build, claim, Some(40), None).expect("the client starts");

        // The lock has to have moved to the child. Checked while it is running,
        // because that is the only moment the answer can be wrong.
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(
            profile::acquire("e2e").is_err(),
            "the spawned instance must hold the profile the launcher claimed"
        );

        // Long enough for the engine to get past loading and write something of
        // its own. Its log is the evidence that `CORDIAL_FILES_DIR` took —
        // without it the engine would be writing into the shared default and
        // the profile would be a directory name and nothing more.
        std::thread::sleep(std::time::Duration::from_secs(25));
        assert!(instance.exited().is_none(), "the client must still be up after 27 seconds");

        let logs = profile_dir.join("data/files/appData/logs");
        let wrote = std::fs::read_dir(&logs).map(|d| d.count()).unwrap_or(0);
        assert!(wrote > 0, "the engine wrote nothing to {}", logs.display());
        println!("engine wrote {wrote} log file(s) into {}", logs.display());

        instance.child.kill().ok();
        instance.child.wait().ok();
        let _ = std::fs::remove_dir_all(&root);
    }
}
