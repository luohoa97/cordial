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

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use cordial_shell::profile::Claim;
use cordial_shell::secrets::Store;

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

/// How many of the client's last output lines are kept for the crash page.
///
/// Enough to carry a panic with its backtrace-less message and the dozen lines
/// of load narration before it, and few enough that the buffer cannot grow with
/// a session: a client that runs for an afternoon prints tens of thousands of
/// lines and this must not be a slow leak in the launcher, which is the process
/// that has to still be alive to report the crash.
const KEPT_LINES: usize = 200;

/// The client's own output, most recent last, for the crash page to show.
///
/// A `Mutex` rather than a channel because two reader threads write into it and
/// the GTK main loop reads it, at a moment neither thread knows about.
type Tail = Arc<Mutex<VecDeque<String>>>;

/// Lines that are never kept, replaced by a marker rather than dropped.
///
/// **`[cookies]` and `[identity]` appear in ordinary runs**, carry a signed-in
/// user and the paths their session is stored at, and are exactly the lines
/// somebody would paste into a bug report without reading. Redacted here, at
/// capture, rather than when the copy button is pressed, so that what the crash
/// page shows and what it copies are the same text -- a copy button that
/// quietly produced something other than what was on screen is its own trap.
/// A marker rather than a deletion because a gap in a log is a lie about what
/// the client printed.
///
/// The pass-through below is untouched by this: the shell's own stdout and
/// stderr get the line verbatim, exactly as they did when the child inherited
/// them. This adds no new place the text is written; it only decides what the
/// launcher keeps in memory to put on a screen.
fn redact(line: &str) -> Option<&'static str> {
    for marker in ["[cookies]", "[identity]"] {
        if line.contains(marker) {
            return Some("    (a line about your saved session was left out)");
        }
    }
    None
}

/// A client this launcher started.
pub struct Instance {
    child: Child,
    /// Kept so the command can be quoted back at the user if the process dies
    /// immediately — an exit code on its own says nothing about what was run.
    pub command_line: String,
    tail: Tail,
    /// Where this client listens for live setting changes, and what it was
    /// started with, so `live` can send it only what differs (ADR-044).
    pub live_socket: PathBuf,
    pub launched_with: Vec<cordial_protocol::Update>,
}

/// Per-launch values that may be absent for an ordinary button launch.
pub struct LaunchRequest<'a> {
    pub run_seconds: Option<u64>,
    pub join_url: Option<&'a str>,
    pub secret_store: Option<Store>,
    /// Present for "Play in VR": the Quest build, under the translator, in an
    /// OpenXR session (ADR-053). `None` is the phone build, as it always was.
    pub vr: Option<VrLaunch>,
}

/// What a VR launch adds to an ordinary one.
#[derive(Clone)]
pub struct VrLaunch {
    /// The OpenXR runtime manifest to hand the client in `XR_RUNTIME_JSON`, for
    /// this launch only. `None` leaves the loader to the system's active
    /// runtime. Cordial never writes `active_runtime.json`: switching the
    /// machine's runtime to play one game is a change to every other OpenXR
    /// application the user has, made by something they did not ask to do it.
    pub openxr_runtime: Option<PathBuf>,
}

impl Instance {
    /// The client's process id, for the launcher's `SIGCHLD` watch.
    ///
    /// **This type deliberately offers no `try_wait`.** It used to have an
    /// `exited()` that called one, polled twice a second by `window.rs`, and
    /// that is now `glib::child_watch_add_local` — which reaps the child
    /// itself, out of GLib's own `SIGCHLD` handling. Two reapers is not a
    /// tidiness question: whichever `waitpid` loses the race gets `ECHILD`,
    /// and the exit status the crash page exists to report is gone with it.
    /// So the launcher hands the pid to GLib and never waits on the child
    /// again, and there is no method here through which it could.
    ///
    /// Dropping the `Child` is still safe and still does not reap — Rust's
    /// `Drop` for it is a no-op by design — so GLib remains the only waiter
    /// even if this `Instance` outlives its watch or dies before it.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The last lines the client printed, oldest first, redacted.
    ///
    /// Read at the moment the crash page is built rather than streamed into it:
    /// nothing wants a live log window, and by the time this is called the
    /// process has exited, so the reader threads have seen EOF and there is
    /// nothing still arriving to miss.
    pub fn recent_output(&self) -> String {
        let tail = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// Read one of the child's pipes to EOF: pass every line through to the
/// launcher's own matching stream, and keep a redacted copy of the last
/// [`KEPT_LINES`].
///
/// **The pass-through is the point of doing this with threads at all.** The
/// child's stdout and stderr used to be inherited, so a shell started from a
/// terminal narrated the whole load, and that is worth keeping -- this project
/// debugs by reading that narration. Capturing without echoing would have
/// silently taken it away, which is a regression nobody would notice until they
/// needed it.
///
/// Two threads and one buffer means the relative order of a stdout line and a
/// stderr line printed at the same instant is whichever thread took the lock
/// first. Within one stream the order is exact, which is what matters: a panic
/// message and the lines that led to it are both on stderr.
/// Local wall-clock to the millisecond, in the same shape `native/liblog.cpp`
/// uses, so a reader can sort both together.
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    // Local offset via `localtime`, not UTC: the reader is comparing this
    // against when they pressed something.
    // Plain arithmetic on the local offset rather than a date library: this
    // runs on every line the runtime prints and must not allocate more than
    // the string it returns.
    let offset: i64 = gtk4::glib::DateTime::now_local()
        // `utc_offset` is a `TimeSpan`, microseconds, not a bare integer.
        .map(|d: gtk4::glib::DateTime| d.utc_offset().0 / 1_000_000)
        .unwrap_or(0);
    let local = secs as i64 + offset;
    let secs_today = local.rem_euclid(86_400);
    let (h, m, sec) = (secs_today / 3600, (secs_today % 3600) / 60, secs_today % 60);
    format!("{h:02}:{m:02}:{sec:02}.{ms:03}")
}

/// Whether a line already begins with a `HH:MM:SS` stamp.
///
/// `liblog` stamps its own output, and doubling it would be worse than not
/// stamping at all -- a reader who sees two clocks stops trusting either.
fn already_stamped(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() >= 8
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b':'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[5] == b':'
}

fn pump(reader: impl std::io::Read + Send + 'static, tail: Tail, to_stderr: bool) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            // **A clock on every line the runtime prints.**
            //
            // `native/liblog.cpp` stamps the Android log, but Cordial's own
            // `[roblox]`/`[cordial]` narration -- `app ready: Startup` among it
            // -- is scattered `println!` with no shared emitter, so there was
            // nowhere to stamp it at the source without touching every site.
            // This is that one place: every line the child writes passes
            // through here on its way back out.
            //
            // It matters because all 265 log files on the development machine
            // carried no clock at all, which made "is startup getting slower"
            // unanswerable from history and is why no startup regression here
            // has ever been caught by reading a log.
            //
            // A line that already carries a stamp is left alone -- `liblog`'s
            // own output arrives pre-stamped and two clocks on one line is
            // worse than none. The check is the cheap one: a digit pair, a
            // colon, and the shape of a time.
            let line = if already_stamped(&line) { line } else { format!("{} {line}", stamp()) };
            if to_stderr {
                let mut out = std::io::stderr().lock();
                let _ = writeln!(out, "{line}");
            } else {
                println!("{line}");
            }
            let kept = redact(&line).map(str::to_string).unwrap_or(line);
            let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
            if tail.len() == KEPT_LINES {
                tail.pop_front();
            }
            tail.push_back(kept);
        }
    });
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
    request: LaunchRequest<'_>,
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
    let run = request.run_seconds.unwrap_or(DEFAULT_RUN_SECONDS).to_string();

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
        .arg("--host-libc");
    match &request.vr {
        // The Quest build has no GameActivity: it starts through the app
        // bridge, the way `ActivityNativeMain` starts it on the headset, and
        // its arm64 engine runs under the translator. `--guest-arm64` also
        // selects the profile's `quest/` engine storage and the meta-quest
        // device identity (ADR-053).
        Some(vr) => {
            command.arg("--guest-arm64").arg("--app-bridge");
            if let Some(manifest) = &vr.openxr_runtime {
                command.env("XR_RUNTIME_JSON", manifest);
            }
        }
        None => {
            command.arg("--game-activity");
        }
    }
    command.arg("--run").arg(&run);

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
    let profile_dir = claim.profile_dir().to_path_buf();
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
    if let Some(url) = request.join_url {
        command.arg("--join-url").arg(url);
    }
    pin_secret_store(&mut command, request.secret_store);

    // This used to set `CORDIAL_WAYLAND=1`, because `cordial-run` defaulted to
    // X11 and took Wayland only on that variable. It no longer does:
    // `android::backend()` prefers Wayland whenever `WAYLAND_DISPLAY` is set,
    // which is what ADR-011 specifies, so there is nothing left for a launcher
    // to opt into and the variable is not read anywhere.
    //
    // Deleted rather than left in place as a harmless no-op. A variable a
    // launcher sets deliberately, with a paragraph explaining why, reads as
    // load-bearing to the next person; one that no longer does anything is a
    // comment that lies, which this codebase treats as costing more than no
    // comment. What the paragraph used to argue -- that a launcher must not
    // quietly start the superseded backend, or the window this crate builds,
    // its header bar and its monitor fitting are all bypassed -- is now the
    // runtime's own default rather than something this call site enforces.

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
    if let Some(v) = config.title_bar.env_value() {
        // Absent means the platform default, so a client that predates this
        // variable behaves exactly as it always has.
        command.env("CORDIAL_TITLE_BAR", v);
    }

    // When Cordial stops holding the engine awake in the background. Passed
    // unconditionally, unlike `CORDIAL_GRAPHICS` above: there is no plugin
    // opinion for an absent variable to leave room for, and the client's own
    // default has to match the shell's or the two disagree about what a fresh
    // install does.
    command.env("CORDIAL_THROTTLE", config.throttle.as_str());
    command.env("CORDIAL_POINTER_ACCEL", config.pointer_acceleration.as_str());

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

    // The Present mode row, and **only when it is not Automatic** -- the same
    // rule as the Renderer row above and for the same reason. An absent
    // `CORDIAL_PRESENT_MODE` is the one state in which a plugin's
    // `CordialPresentMode` flag-layer entry counts (ADR-007, ADR-020), so
    // sending `fifo` unconditionally would have made that capability
    // unreachable from the shell for everybody while the row still said
    // Automatic was available.
    //
    // Note that this is not the same as "the default sends nothing": FIFO is
    // the default *selection*, so a fresh install does send `fifo` and does
    // outrank a plugin. That is deliberate. The power cost of MAILBOX is paid
    // by the person holding the machine, and a plugin should not be able to
    // spend it on somebody who never opened this page.
    if let Some(mode) = config.present_mode.as_env() {
        command.env("CORDIAL_PRESENT_MODE", mode);
    }

    // The Frame rate limit row. Sent unconditionally, unlike the present-mode
    // row just above -- there is no flag-layer entry a plugin contributes this
    // key through that an absent variable would leave room for, so there is no
    // "leave a plugin the floor" state to preserve the way `CORDIAL_GRAPHICS`
    // and `CORDIAL_PRESENT_MODE` have to. This is the launch value: the row is
    // live (ADR-044), and the settings socket changes it afterwards. Keeping
    // the flag in force against the engine's own settings refresh is the
    // client's job, not something to ask for here (ADR-051).
    command.env("CORDIAL_FRAME_RATE_LIMIT", config.frame_rate_limit.as_env());

    // The Controllers switch, and **only when it is off**. `CORDIAL_GAMEPAD`
    // is an off switch on the client side -- absent means on, and only the
    // exact string "0" disables it -- so sending nothing is how "leave it on"
    // is spelled. Sending `1` would work today and would quietly become a
    // second way of saying the same thing the day anybody gives that variable
    // another value.
    if !config.gamepad {
        command.env("CORDIAL_GAMEPAD", "0");
    }

    // Close on leave, and **only when it is on**. The client's own gate wants
    // one of "1", "true" or "yes" and reads everything else as off, so the
    // absent case and the off case agree without this having to spell either.
    if config.close_on_leave {
        command.env("CORDIAL_CLOSE_ON_LEAVE", "1");
    }

    // Plugin folders being worked on, in the shape of `PATH`. Only when there
    // are some: an empty variable and an absent one mean the same thing to
    // `manifest::unpacked_dirs`, and sending an empty one would put a
    // developer-mode marker in the environment of every ordinary launch.
    // Engine forwarding is opt-in. Automatic account routing consumes and
    // removes the ticket earlier, independently of this switch (ADR-035).
    if config.carry_launch_ticket {
        command.env("CORDIAL_DEEPLINK_CARRY_TICKET", "1");
    }

    if !config.unpacked_plugins.is_empty() {
        let joined = config.unpacked_plugins.join(":");
        command.env("CORDIAL_UNPACKED_PLUGINS", &joined);
        println!("shell: loading {} unpacked plugin(s)", config.unpacked_plugins.len());
    }

    // The Graphics optimisation row, and **only for the parameters the chosen
    // mode actually asks for** -- exactly the rule the Renderer row above
    // follows, for exactly the same reason. `CordialDeviceProfile` is a
    // flag-layer key a plugin may set (`cordial_runtime::flags`), and an
    // absent `CORDIAL_DEVICE_PROFILE` is the only state in which that entry
    // counts, because the environment wins when it is present. A mode that
    // wants the client's own default therefore sends nothing rather than
    // sending the default's name.
    //
    // Two variables from one row is not a leak of the abstraction: the row is
    // one choice, and `GraphicsOptimization` owns the mapping from that choice
    // to the two values, so this block cannot drift from what the row says it
    // does without the enum changing first.
    //
    // Not for a VR launch: the Quest build has to present as the headset it
    // was built for, and `cordial-run --guest-arm64` chooses `meta-quest`
    // itself when nothing overrides it. A phone-build performance preset sent
    // to it would make games read `VREnabled` false (flags.rs).
    if request.vr.is_none() {
        if let Some(profile) = config.graphics_optimization_mode.device_profile_env() {
            command.env("CORDIAL_DEVICE_PROFILE", profile);
        }
        if let Some(mode) = config.graphics_optimization_mode.performance_env() {
            command.env("CORDIAL_PERFORMANCE", mode);
        }
    }

    // The Audio row's chosen output sink, and **only when one was actually
    // chosen** -- an absent variable is what tells the client to follow the
    // session default and to keep following it when the default moves, which
    // is the same argument `CORDIAL_GRAPHICS` makes just above about not
    // silently outvoting a plugin.
    //
    // Without this line the setting is the exact failure this codebase keeps
    // writing rules against: a control that saves the user's choice, reports
    // success, and never acts. Everything on the far side of it -- three
    // output paths in `native/`, the fallback when the device has gone, the
    // picker -- was built and tested while nothing carried the value across
    // the process boundary.
    //
    // `env_value()` answers `None` for unset *and* for whitespace, and the
    // native reader treats an empty `CORDIAL_AUDIO_SINK` as unset, so the two
    // ends agree even if one of them is given something odd.
    if let Some(sink) = config.audio_output.env_value() {
        command.env("CORDIAL_AUDIO_SINK", sink);
    }
    // The microphone, on the same terms: only when one was chosen, so that an
    // unset variable keeps following the session's default source. It names a
    // device and opens nothing -- the capture stream is created when Roblox
    // starts recording, and not before.
    if let Some(source) = config.audio_input.env_value() {
        command.env("CORDIAL_AUDIO_SOURCE", source);
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

    // vkBasalt, the same shape as MangoHUD immediately above: an implicit
    // Vulkan layer, switched on by an environment variable the loader acts on
    // by itself, offered only when the layer is actually there. See ADR-041
    // for why a layer that never touches engine memory is in scope under
    // ADR-001, and `vkbasalt_layer`'s doc for why the check is not optional.
    if config.vkbasalt {
        match vkbasalt_layer() {
            Some(layer) => match vkbasalt_config_path(profile_name) {
                Ok(config_path) => match ensure_vkbasalt_config(&config_path) {
                    Ok(()) => {
                        command.env("ENABLE_VKBASALT", "1");
                        command.env("VKBASALT_CONFIG_FILE", config_path.as_os_str());
                        println!(
                            "  shell: vkBasalt on, via {} ({})",
                            layer.display(),
                            config_path.display()
                        );
                    }
                    // A config Cordial cannot write is a config vkBasalt cannot
                    // read either, so turning the layer on regardless would
                    // hand it a `VKBASALT_CONFIG_FILE` that does not exist —
                    // reported here rather than found by whoever reads
                    // vkBasalt's own log next.
                    Err(e) => println!(
                        "  shell: vkBasalt is switched on but its config at {} could not be \
                         written ({e}); leaving it off for this launch",
                        config_path.display()
                    ),
                },
                Err(e) => println!("  shell: vkBasalt is switched on but {e}; leaving it off for this launch"),
            },
            None => println!(
                "  shell: vkBasalt is switched on but its Vulkan layer is not installed; \
                 no shaders will run. {}", vkbasalt_install_hint()
            ),
        }
    }

    // Piped rather than inherited, and echoed straight back out by `pump`, so
    // a shell started from a terminal still narrates the load the way it always
    // has. Both streams, not stderr alone: `cordial-run` says almost everything
    // it has to say with `println!` -- the load order, the missing symbols, the
    // plugin lines -- and a crash page carrying only stderr would show the
    // panic with none of what led to it.
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    claim.hand_to(&mut command);

    let command_line = describe(&loader, &build.lib_dir, &build.apk, &run, request.join_url, request.vr.is_some());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}\n\n{command_line}", loader.display()))?;

    let tail: Tail = Arc::new(Mutex::new(VecDeque::with_capacity(KEPT_LINES)));
    // Taken out of the `Child` so the pipes close when the reader threads see
    // EOF rather than being held open by a struct nobody is reading -- a child
    // whose stdout nothing drains blocks on a full pipe, which for a process
    // that narrates as much as this one would be a hang rather than a crash.
    if let Some(out) = child.stdout.take() {
        pump(out, tail.clone(), false);
    }
    if let Some(err) = child.stderr.take() {
        pump(err, tail.clone(), true);
    }

    // Dropped explicitly rather than left to fall off the end of the function,
    // because the ordering is the whole mechanism: the child now holds the
    // flock through its inherited descriptor, and the launcher must let go or
    // quitting the shell would be the thing that released it.
    drop(claim);

    Ok(Instance {
        child,
        command_line,
        tail,
        live_socket: cordial_protocol::v0::socket_path(&profile_dir),
        launched_with: crate::live::live_updates(&config),
    })
}

fn pin_secret_store(command: &mut Command, store: Option<Store>) {
    if let Some(store) = store {
        command.env("CORDIAL_SECRET_STORE", store.setting_value());
    }
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
/// The branch of `org.freedesktop.Platform.VulkanLayer.*` that Cordial's
/// Flatpak runtime loads: org.gnome.Platform 50 declares the extension point at
/// version 25.08 (`flatpak info --show-metadata org.gnome.Platform//50`). The
/// hints name it because a bare `flatpak install` asks which branch to use, and
/// picking the end-of-life `stable` one installs an extension the runtime never
/// loads, so the switch stays "Not available" after a successful install. A
/// test ties this to the manifest's runtime-version.
pub const VULKAN_LAYER_BRANCH: &str = "25.08";

/// Which Vulkan layer a host install hint is for.
#[derive(Clone, Copy)]
enum Layer {
    MangoHud,
    VkBasalt,
}

/// The host distribution, as far as an install hint needs it: `/etc/os-release`'s
/// `ID`, `ID_LIKE` and `VARIANT_ID`. Only read for a host (non-Flatpak) build,
/// where the layer has to come from the host's own packages.
#[derive(Debug, Default, Clone, PartialEq)]
struct Distro {
    id: String,
    like: Vec<String>,
    variant: String,
}

impl Distro {
    fn here() -> Distro {
        std::fs::read_to_string("/etc/os-release")
            .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
            .map(|t| Distro::parse(&t))
            .unwrap_or_default()
    }

    fn parse(text: &str) -> Distro {
        let mut d = Distro::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim().trim_matches('"').trim_matches('\'').to_ascii_lowercase();
            match key.trim() {
                "ID" => d.id = value,
                "ID_LIKE" => d.like = value.split_whitespace().map(str::to_owned).collect(),
                "VARIANT_ID" => d.variant = value,
                _ => {}
            }
        }
        d
    }

    fn is(&self, family: &str) -> bool {
        self.id == family || self.like.iter().any(|l| l == family)
    }

    /// An image-based Fedora (Silverblue, Kinoite, Bluefin, Bazzite, Aurora
    /// and friends), where `dnf install` does not work on the host and packages
    /// are layered with rpm-ostree instead.
    fn is_atomic_fedora(&self) -> bool {
        const ATOMIC: [&str; 8] =
            ["silverblue", "kinoite", "sericea", "onyx", "bluefin", "bazzite", "aurora", "ublue"];
        ATOMIC.iter().any(|a| self.id.contains(a) || self.variant.contains(a))
            || std::path::Path::new("/run/ostree-booted").exists() && self.is("fedora")
    }
}

/// One line naming the command for this distribution, falling back to a plain
/// sentence where the package name is not known for certain. Only package
/// names confirmed in each distribution's main repositories are named.
fn host_install_hint(d: &Distro, layer: Layer) -> String {
    let (fedora, arch, debian, suse, nix) = match layer {
        Layer::MangoHud => ("mangohud", "mangohud", "mangohud", "mangohud", "mangohud"),
        Layer::VkBasalt => ("vkBasalt", "vkbasalt", "vkbasalt", "vkbasalt", "vkbasalt"),
    };
    let generic = match layer {
        Layer::MangoHud => "Install MangoHud from your distribution's packages.",
        Layer::VkBasalt => "Install vkBasalt from your distribution's packages.",
    };
    if d.is("fedora") && d.is_atomic_fedora() {
        format!("Install it with: rpm-ostree install {fedora} (then reboot)")
    } else if d.is("fedora") {
        format!("Install it with: sudo dnf install {fedora}")
    } else if d.is("arch") {
        format!("Install it with: sudo pacman -S {arch}")
    } else if d.is("debian") || d.is("ubuntu") {
        format!("Install it with: sudo apt install {debian}")
    } else if d.is("suse") || d.is("opensuse") || d.id.starts_with("opensuse") {
        format!("Install it with: sudo zypper install {suse}")
    } else if d.is("nixos") {
        format!("Add pkgs.{nix} to your configuration")
    } else {
        generic.to_owned()
    }
}

/// How to get `adb` on this distribution, for the Quest set-up pages
/// (ADR-053). The same `Distro` reading as the layer hints, with the package
/// each distribution names it: `android-tools` on Fedora, Arch, openSUSE and
/// Nix, `adb` on Debian and Ubuntu.
pub fn adb_install_hint() -> String {
    // The Flatpak carries no adb and cannot run the host's: bundling one
    // would also need raw USB access (`--device=usb`), which the manifest
    // does not grant for a path nobody has run in the sandbox against a
    // headset, and `flatpak-spawn --host` is a sandbox escape it refuses
    // outright (packaging/io.github.luohoa97.Cordial.yml). So the pull is two
    // commands on the host, and the import is the file picker's.
    if in_flatpak() {
        return "Cordial's Flatpak has no adb and cannot run the one on your computer. In a terminal, with \
                the headset connected and allowed, run `adb shell pm path com.roblox.client`, then \
                `adb pull` the path it prints, and choose I Have the APK File in Settings → VR."
            .into();
    }
    adb_install_hint_for(&Distro::here())
}

fn adb_install_hint_for(d: &Distro) -> String {
    if d.is("fedora") && d.is_atomic_fedora() {
        "Install it with: rpm-ostree install android-tools (then reboot)".into()
    } else if d.is("fedora") {
        "Install it with: sudo dnf install android-tools".into()
    } else if d.is("arch") {
        "Install it with: sudo pacman -S android-tools".into()
    } else if d.is("debian") || d.is("ubuntu") {
        "Install it with: sudo apt install adb".into()
    } else if d.is("suse") || d.is("opensuse") || d.id.starts_with("opensuse") {
        "Install it with: sudo zypper install android-tools".into()
    } else if d.is("nixos") {
        "Add pkgs.android-tools to your configuration".into()
    } else {
        "Install Android's platform tools (adb) from your distribution's packages.".into()
    }
}

pub fn mangohud_install_hint() -> String {
    mangohud_install_hint_for(in_flatpak())
}

fn mangohud_install_hint_for(flatpak: bool) -> String {
    if flatpak {
        return format!(
            "Install the Flatpak extension: flatpak install flathub \
             org.freedesktop.Platform.VulkanLayer.MangoHud//{VULKAN_LAYER_BRANCH}"
        );
    }
    host_install_hint(&Distro::here(), Layer::MangoHud)
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
    let mut dirs = vulkan_implicit_layer_dirs();
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

/// What to tell somebody who wants vkBasalt and has not got it.
///
/// Same reasoning as [`mangohud_install_hint`], and the same trap: the Flatpak
/// extension and the host package are not interchangeable, and which one is
/// right is decided by how Cordial was installed, not by preference.
pub fn vkbasalt_install_hint() -> String {
    vkbasalt_install_hint_for(in_flatpak())
}

fn vkbasalt_install_hint_for(flatpak: bool) -> String {
    if flatpak {
        return format!(
            "Install the Flatpak extension: flatpak install flathub \
             org.freedesktop.Platform.VulkanLayer.vkBasalt//{VULKAN_LAYER_BRANCH}"
        );
    }
    host_install_hint(&Distro::here(), Layer::VkBasalt)
}

/// Where vkBasalt's implicit layer manifest is, or `None` if it is not
/// installed.
///
/// The same check as [`mangohud_layer`], for the same reason: `ENABLE_VKBASALT=1`
/// is not an error when there is no vkBasalt, so a switch that does not check
/// first is a switch that appears to work and does nothing.
///
/// The search path is identical to `mangohud_layer`'s, because both are
/// implicit layers the Vulkan loader discovers the same way, and both ship a
/// Flatpak runtime extension mounted under `/usr/lib/extensions/vulkan`.
/// Fedora's `vkBasalt` package installs `vkBasalt.json` at
/// `/usr/share/vulkan/implicit_layer.d` — confirmed by installing the package
/// and reading `rpm -ql` rather than assumed — which the prefix match below
/// finds regardless of the exact casing or architecture suffix a distribution
/// chooses.
pub fn vkbasalt_layer() -> Option<PathBuf> {
    let mut dirs = vulkan_implicit_layer_dirs();
    // The Flatpak runtime extension, which mounts here rather than anywhere
    // XDG_DATA_DIRS points at — see `mangohud_layer`'s identical entry.
    dirs.push(PathBuf::from("/usr/lib/extensions/vulkan/vkBasalt/share/vulkan/implicit_layer.d"));
    find_vkbasalt_layer_in(&dirs)
}

/// The Vulkan loader's implicit-layer search path, plus the Flatpak extension
/// mount point — shared by [`mangohud_layer`] and [`vkbasalt_layer`] so the two
/// detectors cannot drift apart on where they look, only on what they look for.
fn vulkan_implicit_layer_dirs() -> Vec<PathBuf> {
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
    dirs
}

/// The scan, split out for the same testing reason as
/// [`find_mangohud_layer_in`]: built against a directory made for the test
/// rather than whatever happens to be installed on the machine running it.
fn find_vkbasalt_layer_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_ascii_lowercase();
            if name.starts_with("vkbasalt") && name.ends_with(".json") {
                return Some(entry.path());
            }
        }
    }
    None
}

/// vkBasalt's toggle key, chosen rather than left at upstream's default.
///
/// Upstream's own example ships `toggleKey = Home`, and Home is a real Roblox
/// chat key — it moves the cursor to the start of a line while a chat box is
/// focused. vkBasalt does not consume the key or care which window has focus:
/// `keyboard_input_x11.cpp` polls the X11 keyboard globally with
/// `XQueryKeymap`, so upstream's default would toggle the shader effect on
/// every message somebody types that starts with a jump to the beginning of the
/// line. `Scroll_Lock` is not bound by Roblox, by GTK, or by Cordial's own
/// `fullscreen_accel` (F11 by default), and is one of the few keys nobody
/// reaches for while typing.
///
/// **This only matters when the toggle can fire at all.** Read from the same
/// source: `isKeyPressedX11` looks at `$DISPLAY` once and returns `false`
/// forever when it is unset, which is Cordial's own Wayland backend — the
/// primary one, per ADR-011 — with no XWayland running. On that backend the
/// key does nothing at all and `enableOnLaunch` is the only lever there is;
/// the choice below still matters on the X11 backend (ADR-024) and for anybody
/// running XWayland alongside. Documented in `docs/shaders.md` rather than left
/// for somebody to find by testing a key that appears to do nothing.
const VKBASALT_TOGGLE_KEY: &str = "Scroll_Lock";

/// The vkBasalt config Cordial writes the first time a profile turns the
/// switch on.
///
/// CAS plus SMAA, which is the pairing the settings row promises: a sharpen
/// pass and an anti-alias pass, in that order, at upstream's own documented
/// defaults for both (`config/vkBasalt.json.in` in the vkBasalt repository,
/// zlib licence) rather than any third party's tuned numbers — see
/// `docs/adr` for why VineShade's own config specifically was not read for
/// this. `enableOnLaunch = True` matters more here than it would upstream,
/// for the reason [`VKBASALT_TOGGLE_KEY`]'s doc explains: on Cordial's default
/// Wayland backend the toggle key cannot fire, so an effect that started
/// disabled would need the X11 backend just to be turned on once.
fn vkbasalt_config_template() -> String {
    format!(
        "# Written once by Cordial when vkBasalt was first switched on for this\n\
         # profile. Cordial never rewrites this file after this — edit the effects\n\
         # list, the sharpening strength, or the toggle key below and it stays\n\
         # exactly as you left it.\n\
         #\n\
         # Full key reference: https://github.com/DadSchoorse/vkBasalt/blob/master/config/vkBasalt.json.in\n\
         #\n\
         # The toggle key below only does anything when vkBasalt can see a real X11\n\
         # keyboard (`$DISPLAY` set) -- it polls the keyboard directly rather than\n\
         # through the window, so it does nothing on Cordial's default Wayland\n\
         # backend with no XWayland running. See docs/shaders.md.\n\
         \n\
         effects = cas:smaa\n\
         \n\
         toggleKey = {VKBASALT_TOGGLE_KEY}\n\
         enableOnLaunch = True\n\
         \n\
         # Contrast Adaptive Sharpening. 0.0 is barely sharpened, 1.0 is maximum.\n\
         casSharpness = 0.4\n\
         \n\
         # Enhanced Subpixel Morphological Antialiasing, upstream's own defaults.\n\
         smaaEdgeDetection = luma\n\
         smaaThreshold = 0.05\n\
         smaaMaxSearchSteps = 32\n\
         smaaMaxSearchStepsDiag = 16\n\
         smaaCornerRounding = 25\n"
    )
}

/// Where the per-profile vkBasalt config for `profile_name` lives.
///
/// Inside the profile's own directory (`cordial_shell::profile::dir`), on the
/// same footing as the engine's `appData` and the cookie store: which shaders
/// somebody has picked is a per-account preference in the same way their
/// pointer acceleration is not, and two profiles on one machine should not
/// fight over one shared file.
pub fn vkbasalt_config_path(profile_name: &str) -> Result<PathBuf, String> {
    Ok(cordial_shell::profile::dir(profile_name)?.join("vkBasalt.conf"))
}

/// Write the generated config to `path`, but only if nothing is there yet.
///
/// **Never overwrites.** A config that replaced a file the user had already
/// edited would be the settings-page equivalent of a stub that returns
/// success: the switch would appear to respect a choice it just discarded.
/// `create_new` is what enforces this — not a `path.exists()` check beforehand,
/// which would leave a window between the check and the write for another
/// launch of the same profile to land in.
fn ensure_vkbasalt_config(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(vkbasalt_config_template().as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// The command line quoted back when the client dies at once.
///
/// It carries `--join-url` when there was one, because a launch that fails only
/// with a link on it is exactly the launch somebody needs to be able to repeat
/// in a terminal.
fn describe(loader: &Path, lib_dir: &Path, apk: &Path, run: &str, join_url: Option<&str>, vr: bool) -> String {
    let join = join_url.map(|u| format!(" --join-url {u}")).unwrap_or_default();
    let mode = if vr { "--guest-arm64 --app-bridge" } else { "--game-activity" };
    format!(
        "{} --lib-dir {} --apk {} --host-libc {mode} --run {run}{join}",
        loader.display(),
        lib_dir.display(),
        apk.display()
    )
}

#[cfg(test)]
mod tests {

    /// The layer branch in the install hints has to follow the Flatpak runtime.
    /// GNOME 50 loads VulkanLayer 25.08; a runtime bump must revisit both.
    #[test]
    fn the_vulkan_layer_branch_follows_the_flatpak_runtime() {
        let manifest = include_str!("../../../packaging/io.github.luohoa97.Cordial.yml");
        assert!(
            manifest.lines().any(|l| l.trim() == "runtime-version: '50'"),
            "the Flatpak runtime changed; update VULKAN_LAYER_BRANCH to the VulkanLayer \
             version it declares (flatpak info --show-metadata org.gnome.Platform//N)"
        );
        assert_eq!(VULKAN_LAYER_BRANCH, "25.08");
        assert!(vkbasalt_install_hint_for(true).ends_with(&format!("//{VULKAN_LAYER_BRANCH}")));
        assert!(mangohud_install_hint_for(true).ends_with(&format!("//{VULKAN_LAYER_BRANCH}")));
    }

    #[test]
    fn a_session_line_is_replaced_rather_than_kept_or_dropped() {
        // The two prefixes that appear in ordinary runs and carry a signed-in
        // user. Neither may reach the buffer the crash page shows and copies,
        // and neither may vanish silently -- a gap in a log is a lie about what
        // the client printed.
        for line in [
            "  [cookies] roblox.com: saved 4 domain(s), 900 bytes to /home/x/cookies.json",
            "  [identity] signed in; saved to /home/x/identity.json (username 7 bytes)",
        ] {
            let marker = redact(line).expect(line);
            assert!(marker.contains("left out"), "{marker}");
            assert!(!marker.contains("roblox.com") && !marker.contains("/home/x"), "{marker}");
        }
        // And an ordinary line is untouched, or the page would show nothing
        // useful at all.
        assert!(redact("LOAD FAILED after 80us: dlopen failed").is_none());
    }
    use super::*;

    /// A local mutex, because the only process-wide variable the VPN-gate test
    /// still sets is `CORDIAL_PROFILE_ROOT` -- the check command now lives in
    /// the profile's own `network.json` rather than in an environment
    /// variable, so there is nothing else here to serialise.
    ///
    /// `CORDIAL_PROFILE_ROOT` is a different matter: it
    /// used to have one here too, private to this file, until that turned
    /// out to be exactly the shape of the flake `crate::PROFILE_ROOT_ENV`'s
    /// own doc comment records — two independent mutexes guarding one
    /// process-wide variable serialise nothing against each other. This test
    /// now shares that lock with `profile_switcher.rs` instead.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_vpn_required_profile_whose_check_fails_refuses_before_the_loader_is_looked_for() {
        let _env_guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _root_guard = crate::PROFILE_ROOT_ENV.lock().unwrap_or_else(|e| e.into_inner());

        let root = std::env::temp_dir().join("cordial-launch-gate-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("CORDIAL_PROFILE_ROOT", &root);

        let claim = cordial_shell::profile::acquire("vpn-test").expect("a fresh profile is free");
        cordial_shell::network::save(
            claim.profile_dir(),
            &cordial_shell::network::NetworkConfig {
                mode: cordial_shell::network::Mode::VpnRequired,
                // Exits non-zero, so the requirement is not met. No tool has to
                // be installed for this to be a faithful test of the gate --
                // which is the point of the check being argv rather than one
                // named program.
                check: vec!["false".into()],
            },
        )
        .unwrap();

        let build = Build { apk: PathBuf::from("/nonexistent.apk"), lib_dir: PathBuf::from("/nonexistent") };
        let result = spawn(
            &build,
            claim,
            LaunchRequest { run_seconds: Some(1), join_url: None, secret_store: None, vr: None },
        );

        std::env::remove_var("CORDIAL_PROFILE_ROOT");
        let _ = std::fs::remove_dir_all(&root);

        // The message names the actual gap, not a made-up APK path or loader
        // error -- proof the refusal happened before `spawn` got anywhere near
        // looking for `cordial-run` or the build. `Result::expect_err` wants
        // `Instance: Debug` for its own panic message, which `Instance`
        // deliberately does not derive (it holds a live `Child`), so this
        // matches instead.
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("a vpn-required profile whose check fails must refuse to launch"),
        };
        assert!(err.contains("vpn-required"), "{err}");
        assert!(err.contains("check"), "{err}");
    }

    #[test]
    fn the_shell_actually_hands_vkbasalt_env_to_the_client() {
        // Every other test above proves the pieces -- detection, the hint, the
        // toggle key, the config that is never overwritten. None of them prove
        // the thing the settings row promises: that switching it on in
        // `shell.json` actually reaches the client's environment when a launch
        // goes through `spawn`, which is the one path a user's toggle takes.
        //
        // A stub `cordial-run` on `PATH` stands in for the 115 MB engine --
        // real for `loader_path`'s lookup, and cheap enough to run inside
        // `cargo test --workspace` rather than behind `--ignored`, unlike
        // `a_launch_really_starts_the_client` below.
        let _env_guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _root_guard = crate::PROFILE_ROOT_ENV.lock().unwrap_or_else(|e| e.into_inner());

        let root = std::env::temp_dir().join("cordial-vkbasalt-env-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // The fake layer, so `vkbasalt_layer()` finds one and the switch is not
        // reported as unavailable -- exactly the guard `mangohud_layer` has,
        // exercised rather than bypassed. `vulkan_implicit_layer_dirs` joins
        // each `XDG_DATA_DIRS` entry with `vulkan/implicit_layer.d` directly
        // (the same shape as the real `/usr/share`), so the manifest goes
        // straight under `root` rather than under a `share/` of its own.
        let layer_dir = root.join("vulkan/implicit_layer.d");
        std::fs::create_dir_all(&layer_dir).unwrap();
        std::fs::write(layer_dir.join("vkBasalt.json"), "{}").unwrap();

        let bin_dir = root.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let capture = root.join("captured-env");
        std::fs::write(
            bin_dir.join(LOADER),
            format!("#!/bin/sh\nenv > {}\n", capture.display()),
        )
        .unwrap();
        std::fs::set_permissions(
            bin_dir.join(LOADER),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let profile_root = root.join("profiles");
        std::fs::create_dir_all(&profile_root).unwrap();
        std::env::set_var("CORDIAL_PROFILE_ROOT", &profile_root);
        std::env::set_var("XDG_DATA_DIRS", &root);
        let shell_config_path = root.join("shell.json");
        std::env::set_var("CORDIAL_SHELL_CONFIG", &shell_config_path);
        shell_config::save(
            &shell_config_path,
            &crate::shell_config::ShellConfig { vkbasalt: true, ..Default::default() },
        )
        .unwrap();

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![bin_dir.clone()];
        dirs.extend(std::env::split_paths(&old_path));
        std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());

        let claim = cordial_shell::profile::acquire("vkbasalt-env-test").expect("a fresh profile is free");
        let build = Build { apk: PathBuf::from("/nonexistent.apk"), lib_dir: PathBuf::from("/nonexistent") };
        let result = spawn(
            &build,
            claim,
            LaunchRequest { run_seconds: Some(1), join_url: None, secret_store: None, vr: None },
        );

        let mut instance = result.expect("the stub loader must be found and spawned");
        let _ = instance.child.wait();

        std::env::set_var("PATH", &old_path);
        std::env::remove_var("CORDIAL_PROFILE_ROOT");
        std::env::remove_var("CORDIAL_SHELL_CONFIG");
        std::env::remove_var("XDG_DATA_DIRS");

        let captured = std::fs::read_to_string(&capture)
            .unwrap_or_else(|e| panic!("the stub never ran or never wrote {}: {e}", capture.display()));

        assert!(captured.contains("ENABLE_VKBASALT=1"), "{captured}");
        let config_line = captured
            .lines()
            .find(|l| l.starts_with("VKBASALT_CONFIG_FILE="))
            .unwrap_or_else(|| panic!("no VKBASALT_CONFIG_FILE in:\n{captured}"))
            .to_string();
        assert!(config_line.ends_with("vkBasalt.conf"), "{config_line}");
        // And the config the client was pointed at genuinely exists -- checked
        // before `root` is removed below, since it lives under it -- a path
        // handed to the engine that nothing wrote would be the same failure as
        // no variable at all, just one step further along.
        let path_str = config_line.trim_start_matches("VKBASALT_CONFIG_FILE=");
        assert!(Path::new(path_str).is_file(), "{path_str} was never written");

        let _ = std::fs::remove_dir_all(&root);
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
            false,
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
            false,
        );
        assert!(line.contains("--join-url roblox-player://placeId=1818"), "{line}");
    }

    #[test]
    fn a_vr_launch_quotes_the_translator_and_the_app_bridge_instead_of_game_activity() {
        let line = describe(
            Path::new("/app/bin/cordial-run"),
            Path::new("/c/builds/arm64-v8a/2.740.0.927"),
            Path::new("/c/builds/arm64-v8a/2.740.0.927/base.apk"),
            "0",
            None,
            true,
        );
        assert!(line.contains("--guest-arm64 --app-bridge"), "{line}");
        assert!(!line.contains("--game-activity"), "{line}");
    }

    #[test]
    fn routed_launch_pins_the_backend_selected_during_lookup() {
        // Given commands for auto selections that resolved differently.
        for (store, expected) in [(Store::Keyring, "keyring"), (Store::File, "file")] {
            let mut command = Command::new("cordial-run");

            // When routing pins the selected store for the child.
            pin_secret_store(&mut command, Some(store));

            // Then a fresh runtime cannot make a different automatic choice.
            let value = command
                .get_envs()
                .find(|(name, _)| *name == "CORDIAL_SECRET_STORE")
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str());
            assert_eq!(value, Some(expected));
        }
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
        let hint = mangohud_install_hint_for(true);
        assert!(hint.contains("flatpak install"), "{hint}");
        assert!(!hint.contains("dnf install"), "{hint}");
        let fedora = Distro::parse("ID=fedora\nVARIANT_ID=workstation\n");
        let hint = host_install_hint(&fedora, Layer::MangoHud);
        {
            assert!(hint.contains("dnf install"), "{hint}");
            // It used to name the Flatpak extension in order to rule it out,
            // and that clause was most of what made this string too long to
            // render in the insensitive settings row it is shown in. Naming
            // neither route is safe; naming the wrong one as an option is the
            // failure this test exists for, so that is what is asserted now.
            assert!(!hint.contains("flatpak install"), "{hint}");
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

    #[test]
    fn the_vkbasalt_hint_names_one_package_and_it_matches_how_cordial_was_installed() {
        // Same failure mode `the_mangohud_hint_names_one_package...` guards
        // against, for the same two packages that cannot see each other.
        let hint = vkbasalt_install_hint_for(true);
        assert!(hint.contains("flatpak install"), "{hint}");
        assert!(!hint.contains("dnf install"), "{hint}");
        let fedora = Distro::parse("ID=fedora\nVARIANT_ID=workstation\n");
        let hint = host_install_hint(&fedora, Layer::VkBasalt);
        assert!(hint.contains("dnf install vkBasalt"), "{hint}");
        assert!(!hint.contains("flatpak install"), "{hint}");
    }

    #[test]
    fn the_host_hint_follows_the_distribution() {
        let cases = [
            ("ID=arch\n", "pacman -S vkbasalt"),
            ("ID=cachyos\nID_LIKE=arch\n", "pacman -S vkbasalt"),
            ("ID=ubuntu\nID_LIKE=debian\n", "apt install vkbasalt"),
            ("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n", "apt install vkbasalt"),
            ("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n", "zypper install vkbasalt"),
            ("ID=nixos\n", "pkgs.vkbasalt"),
            ("ID=bluefin\nID_LIKE=\"fedora\"\nVARIANT_ID=bluefin-dx\n", "rpm-ostree install vkBasalt"),
            ("ID=fedora\nVARIANT_ID=silverblue\n", "rpm-ostree install vkBasalt"),
            ("ID=gentoo\n", "from your distribution"),
            ("", "from your distribution"),
        ];
        for (os_release, want) in cases {
            let hint = host_install_hint(&Distro::parse(os_release), Layer::VkBasalt);
            assert!(hint.contains(want), "{os_release:?} gave {hint:?}, wanted {want:?}");
        }
    }

    #[test]
    fn the_vkbasalt_layer_is_found_by_prefix_and_not_by_an_exact_filename() {
        // Confirmed against the real Fedora package, which installs
        // `vkBasalt.json` at `/usr/share/vulkan/implicit_layer.d` --
        // `rpm -ql vkBasalt` inside the build container, not assumed -- but the
        // prefix match is what has to hold across distributions and versions.
        let root = std::env::temp_dir().join("cordial-vkbasalt-detect/implicit_layer.d");
        let _ = std::fs::remove_dir_all(root.parent().expect("has a parent"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("VkLayer_MESA_device_select.json"), "{}").unwrap();

        assert!(
            find_vkbasalt_layer_in(&[root.clone()]).is_none(),
            "an unrelated implicit layer must not read as vkBasalt"
        );

        std::fs::write(root.join("vkBasalt.json"), "{}").unwrap();
        let found = find_vkbasalt_layer_in(&[root.clone()]).expect("the layer is there now");
        assert!(found.ends_with("vkBasalt.json"), "{}", found.display());

        let missing = root.join("nowhere");
        assert!(find_vkbasalt_layer_in(&[missing, root.clone()]).is_some());

        let _ = std::fs::remove_dir_all(root.parent().expect("has a parent"));
    }

    #[test]
    fn vkbasalt_toggle_key_is_not_a_key_roblox_chat_uses() {
        // The whole reason upstream's own `Home` default was not kept: it is a
        // real Roblox chat key (cursor to start of line), and vkBasalt polls
        // the keyboard globally rather than through the focused window, so
        // typing in chat would have toggled the effect. Pinned so nobody
        // reintroduces Home while simplifying the config template.
        assert_ne!(VKBASALT_TOGGLE_KEY, "Home");
        assert_eq!(VKBASALT_TOGGLE_KEY, "Scroll_Lock");
    }

    #[test]
    fn the_generated_vkbasalt_config_is_never_overwritten() {
        // The whole point of `ensure_vkbasalt_config`: a user who has edited
        // their config must not have it silently replaced the next time the
        // switch happens to be read at launch.
        let dir = std::env::temp_dir().join("cordial-vkbasalt-config-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("vkBasalt.conf");

        ensure_vkbasalt_config(&path).expect("first write succeeds");
        let generated = std::fs::read_to_string(&path).unwrap();
        assert!(generated.contains("effects = cas:smaa"));
        assert!(generated.contains(VKBASALT_TOGGLE_KEY));

        std::fs::write(&path, "# a user's own config\neffects = fxaa\n").unwrap();
        ensure_vkbasalt_config(&path).expect("second call must not fail");
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "# a user's own config\neffects = fxaa\n", "the user's edit must survive");

        let _ = std::fs::remove_dir_all(&dir);
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

        let build = match profile::dir("e2e").map_err(|e| install::NotFound::Unusable(e)).and_then(|d| install::resolve(&d)) {
            Ok(build) => build,
            Err(e) => {
                println!("no Roblox build on this machine, nothing to prove: {e:?}");
                return;
            }
        };
        println!("build: {} + {}", build.apk.display(), build.lib_dir.display());

        let claim = profile::acquire("e2e").expect("a fresh profile is free");
        let profile_dir = claim.profile_dir().to_path_buf();
        let mut instance = spawn(
            &build,
            claim,
            LaunchRequest { run_seconds: Some(40), join_url: None, secret_store: None, vr: None },
        )
        .expect("the client starts");

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
        // `try_wait` directly rather than through a method on `Instance`: this
        // test is the only reaper in this process — there is no GTK main loop
        // here and so no child watch — and `Instance::pid`'s doc explains why
        // the type no longer offers one to the launcher.
        assert!(
            instance.child.try_wait().expect("waiting on the client works").is_none(),
            "the client must still be up after 27 seconds"
        );

        let logs = profile_dir.join("data/files/appData/logs");
        let wrote = std::fs::read_dir(&logs).map(|d| d.count()).unwrap_or(0);
        assert!(wrote > 0, "the engine wrote nothing to {}", logs.display());
        println!("engine wrote {wrote} log file(s) into {}", logs.display());

        instance.child.kill().ok();
        instance.child.wait().ok();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_adb_hint_names_each_distributions_package() {
        for (release, want) in [
            ("ID=fedora\n", "dnf install android-tools"),
            ("ID=kinoite\nID_LIKE=fedora\n", "rpm-ostree install android-tools"),
            ("ID=arch\n", "pacman -S android-tools"),
            ("ID=ubuntu\nID_LIKE=debian\n", "apt install adb"),
            ("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n", "zypper install android-tools"),
        ] {
            let hint = adb_install_hint_for(&Distro::parse(release));
            assert!(hint.contains(want), "{release:?}: {hint}");
        }
    }

}
