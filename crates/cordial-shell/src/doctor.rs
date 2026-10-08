//! What on this machine will stop Roblox working, and what to do about it.
//!
//! Adapted from DamnShabu/stacked's `doctor.rs` (GPL-3.0-or-later), which
//! supplied the structure -- a list of `Check { level, what, fix }`, an exit
//! status from failures only, output safe to paste -- and most of the file,
//! desktop, audio, keyring and GameMode checks. What was added here: the GPU
//! and a Vulkan device probe, the display backend, Deno, every profile's lock,
//! and the NVIDIA checks ([`nvidia_checks`]).
//!
//! `diagnostics.rs` is the block a bug report needs, deliberately free of
//! judgement. This is the other half: the same machine read for the things
//! that are known to break a launch, each with a verdict and what to do. A
//! user who has to open an issue to learn that their session has no Wayland
//! socket, or that Vulkan is running on the CPU, has been failed by the tool.
//!
//! **Every check reads something; none of them guesses.** Where the answer
//! depends on something this process cannot see, the check says what it looked
//! at and does not call the absence a fault. A doctor that reports a healthy
//! machine as broken teaches people to ignore it, which is the same failure as
//! a stub that returns success.
//!
//! **Why this is in the library.** The launcher's report screen and the
//! `cordial --doctor` argv path both show it, and the argv path has to run
//! before any `GApplication` exists, so neither can own it. What needs the
//! launcher's own state (the Roblox build, the shell configuration, where
//! `cordial-run` is) stays in the binary's `doctor_run.rs` and is handed in.
//!
//! Cheap by design: file reads, a handful of D-Bus calls, and one child process
//! for the Vulkan question, which a broken driver can hang and so is never
//! asked in this process. Nothing here touches the network.

use crate::nvidia::{self, FlatpakGl};
use crate::profile;
use crate::gtk_renderer;
use crate::vulkan_probe::{self, Device, Failure, Kind};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The desktop entry's id, which is also the application id. The binary's own
/// `APP_ID` must equal it; `doctor_run.rs` tests that they do.
pub const DESKTOP_ID: &str = "io.github.luohoa97.Cordial.desktop";

/// How long the Vulkan probe gets. A healthy one answers in well under a
/// second; a driver that is going to hang has done so long before this.
const VULKAN_PROBE_LIMIT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "ok  ",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub what: String,
    /// What to do about it. Empty for a check that passed, and for an `Info`
    /// line that only reports something that cannot be acted on.
    pub fix: String,
}

pub fn check(level: Level, what: impl Into<String>, fix: impl Into<String>) -> Check {
    Check { level, what: what.into(), fix: fix.into() }
}

/// What only the launcher knows, handed in by whoever runs the doctor.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    /// The shell's `gamemode` setting, which decides whether a missing GameMode
    /// is worth a line.
    pub gamemode: bool,
    /// Whether to ask Vulkan what devices there are. Off in tests, where
    /// spawning the test binary with `--vulkan-probe` would answer nothing.
    pub probe_vulkan: bool,
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs { gamemode: false, probe_vulkan: true }
    }
}

/// The environment variables the checks read, taken once, so a test can hand
/// in a session it made up and the environment is not process-wide state a
/// second test can interleave with.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub wayland_display: Option<String>,
    pub display: Option<String>,
    pub cordial_x11: bool,
    pub gdk_backend: Option<String>,
    pub session_type: Option<String>,
    pub runtime_dir: Option<PathBuf>,
    pub in_flatpak: bool,
}

impl Env {
    pub fn from_process() -> Env {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Env {
            wayland_display: var("WAYLAND_DISPLAY"),
            display: var("DISPLAY"),
            cordial_x11: std::env::var_os("CORDIAL_X11").is_some(),
            gdk_backend: var("GDK_BACKEND"),
            session_type: var("XDG_SESSION_TYPE"),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            in_flatpak: Path::new("/.flatpak-info").exists(),
        }
    }
}

/// Whether the game opens on X11 for a session with these two facts.
///
/// **The loader's rule, not the launcher window's.**
/// `cordial_runtime::android::backend()` picks Wayland when `WAYLAND_DISPLAY`
/// is set and `CORDIAL_X11` is not, and X11 otherwise. A session exporting
/// `GDK_BACKEND=x11` over a Wayland compositor runs the launcher through
/// Xwayland and the game on Wayland, so it is not asked about here. The
/// launcher's X11 banner (`x11_notice.rs`) calls this rather than repeating
/// the rule, since the two changing apart is what ADR-045 warns about.
pub fn uses_x11(cordial_x11_set: bool, wayland_display_set: bool) -> bool {
    cordial_x11_set || !wayland_display_set
}

/// The checks that read the machine and not the Roblox build. Runs the Vulkan
/// probe (a child process, bounded) unless `inputs.probe_vulkan` is off.
pub fn machine(inputs: &Inputs) -> Vec<Check> {
    let env = Env::from_process();
    let mut out = vec![build_origin(&crate::version::origin()), root(), session(&env)];
    out.extend(gpu(&env, inputs));
    out.push(audio(&env));
    let bus = session_bus();
    out.push(keyring(bus.as_ref()));
    if inputs.gamemode {
        out.push(gamemode(bus.as_ref()));
    }
    out.push(browser_handler());
    out.push(deno());
    out.extend(disk());
    out.push(profile_locks(&profile::list(), profile::is_held));
    out
}

// ---------------------------------------------------------------------------
// Rendering

/// Every check, then a one-line verdict, as text safe to paste in public.
pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        out.push_str(&format!("{}  {}\n", c.level.tag(), private(&c.what)));
        for line in c.fix.lines() {
            out.push_str(&format!("      {}\n", private(line)));
        }
    }
    out.push('\n');
    out.push_str(&verdict(checks));
    out.push('\n');
    out
}

/// The one line under the list.
pub fn verdict(checks: &[Check]) -> String {
    let fails = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warns = checks.iter().filter(|c| c.level == Level::Warn).count();
    match (fails, warns) {
        (0, 0) => "Nothing found that should stop Roblox running here.".into(),
        (0, w) => format!("{w} warning{}; Roblox should still start.", plural(w)),
        (f, _) => format!("{f} problem{} will stop Roblox starting.", plural(f)),
    }
}

/// The exit status for `cordial --doctor`: failures only. A warning is a
/// thing to read, not a reason for a script to stop.
pub fn exit_status(checks: &[Check]) -> u8 {
    u8::from(checks.iter().any(|c| c.level == Level::Fail))
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `line` with the home directory shown as `~`.
///
/// The output is pasted into public issues, and a home directory is usually
/// its owner's name -- the reason `diagnostics.rs` prints no path under
/// `$HOME` at all. Paths are still worth showing here, because "which
/// cordial-run" is a real answer; the name in them is not.
///
/// Only on a path-component boundary. The fork's version replaced the bare
/// string, so with `HOME=/home/a` the path `/home/ab/x` came out as `~b/x`,
/// which is neither private nor correct.
pub fn private(line: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) => private_with(line, &home),
        Err(_) => line.to_string(),
    }
}

fn private_with(line: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(home) {
        let after = &rest[at + home.len()..];
        let before_ok = match rest[..at].chars().next_back() {
            Some(c) => !is_path_char(c),
            None => true,
        };
        let after_ok = match after.chars().next() {
            Some(c) => !is_path_char(c) || c == '/',
            None => true,
        };
        out.push_str(&rest[..at]);
        if before_ok && after_ok {
            out.push('~');
        } else {
            out.push_str(home);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// A character that could continue a path component, so a match followed or
/// preceded by one is a different path.
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')
}

// ---------------------------------------------------------------------------
// Checks

/// Which build this is, for whoever reads the output. Never a warning: an
/// unofficial build is not a fault, only a fact the person answering a report
/// wants first. A hint and not a check -- see `version::Origin`.
pub fn build_origin(origin: &crate::version::Origin) -> Check {
    if origin.is_official() {
        return check(Level::Ok, origin.text(), "");
    }
    check(
        Level::Info,
        origin.text(),
        "Not one of the project's own releases, so the project cannot see what this build changed. \
         Say so if you report a problem.",
    )
}

pub fn root() -> Check {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        check(
            Level::Warn,
            "running as root",
            "Root usually has no desktop session, so no sound and no keyring. Run Cordial as your own user.",
        )
    } else {
        check(Level::Ok, "running as an ordinary user", "")
    }
}

/// Wayland or X11, and whether the display is there at all.
pub fn session(env: &Env) -> Check {
    let runtime = env.runtime_dir.clone().unwrap_or_default();
    if let Some(name) = &env.wayland_display {
        let socket = if Path::new(name).is_absolute() { PathBuf::from(name) } else { runtime.join(name) };
        if !socket.exists() {
            return check(
                Level::Fail,
                format!("WAYLAND_DISPLAY is {name}, and there is no socket at {}", socket.display()),
                "Run Cordial from inside your desktop session, not from ssh or a TTY.",
            );
        }
    } else if env.display.is_none() {
        return check(
            Level::Fail,
            "no display: neither WAYLAND_DISPLAY nor DISPLAY is set",
            "Run Cordial from inside your desktop session, not from ssh or a TTY.",
        );
    }

    if !uses_x11(env.cordial_x11, env.wayland_display.is_some()) {
        let mut what = "the game opens on Wayland".to_string();
        if env.gdk_backend.as_deref() == Some("x11") {
            what.push_str(" (the launcher window itself runs on X11 because GDK_BACKEND=x11)");
        }
        return check(Level::Ok, what, "");
    }

    // The X11 warning. Say *why* the game is on X11, because the two causes
    // have different fixes and a user who set CORDIAL_X11 on purpose should
    // not be told to log in to another session.
    if env.cordial_x11 {
        check(
            Level::Warn,
            "the game opens on X11 because CORDIAL_X11 is set",
            "X11 is the rougher backend, and Wayland gets the fixes first. Unset CORDIAL_X11 to use Wayland.",
        )
    } else if env.session_type.as_deref() == Some("wayland") {
        check(
            Level::Warn,
            "this is a Wayland session but WAYLAND_DISPLAY is not set, so the game opens on X11",
            "X11 is the rougher backend, and Wayland gets the fixes first. Start Cordial from a \
             program launched by the desktop, which inherits WAYLAND_DISPLAY, not from a shell that dropped it.",
        )
    } else {
        check(
            Level::Warn,
            "the game opens on X11",
            "X11 is the rougher backend, and Wayland gets the fixes first. If your desktop offers a \
             Wayland session at the login screen, use it.",
        )
    }
}

/// GPU identity and whether Vulkan works: the loader and driver files, then
/// what a real instance says.
pub fn gpu(env: &Env, inputs: &Inputs) -> Vec<Check> {
    let loader = loadable("libvulkan.so.1");
    let drivers = vulkan_drivers();
    let mut out = vec![vulkan_files(loader, &drivers, env.in_flatpak)];
    let probed = if !inputs.probe_vulkan {
        None
    } else if !loader {
        // Already said, and there is nothing to ask.
        None
    } else {
        Some(vulkan_probe::probe(VULKAN_PROBE_LIMIT))
    };
    let kernel_gpus = kernel_gpu_vendors();
    let nvidia_host = NvidiaHost::read(env);
    out.extend(vulkan_devices(probed.as_ref(), &kernel_gpus, env.in_flatpak, &nvidia_host.flatpak_gl()));
    // What Cordial will do about its own window on this machine, which is
    // otherwise a line in a client log nobody reads: no hardware Vulkan means
    // GTK's renderer is switched to cairo so a focused text box can be drawn.
    // Not asked at all when the probe was not requested.
    if inputs.probe_vulkan {
        let answer = match &probed {
            Some(answer) => answer.clone(),
            None => Err(Failure::NoLoader),
        };
        if let gtk_renderer::Decision::Cairo(why) = gtk_renderer::decide(std::env::var(gtk_renderer::ENV).ok().as_deref(), &answer) {
            out.push(check(
                Level::Info,
                format!("Cordial will draw its window with GTK's cairo renderer, because {why}"),
                "GTK's GL renderer stops presenting beside the engine's OpenGL ES, which would leave a focused text \
                 box with no editor. Set GSK_RENDERER yourself to choose another.",
            ));
        }
    }
    if let Some(Ok(devices)) = &probed {
        out.extend(nvidia_checks(devices, &nvidia_host));
    }
    out
}

/// Whether a Vulkan loader and at least one driver manifest are present.
///
/// The engine `dlopen`s `libvulkan.so.1` itself and, without one, falls
/// through to OpenGL ES -- measured, see `cordial_runtime::graphics` -- so a
/// missing driver is a warning and not a failure. **Presence only**: a
/// manifest that names a library which will not load reads as healthy here,
/// which is why [`vulkan_devices`] asks the loader for real.
fn vulkan_files(loader: bool, drivers: &[String], in_flatpak: bool) -> Check {
    match (loader, drivers.len()) {
        (true, 0) if in_flatpak => check(
            Level::Info,
            "Vulkan loader present; the Flatpak's GL extension provides the driver, which is checked below",
            "",
        ),
        (true, 0) => check(
            Level::Warn,
            "a Vulkan loader but no Vulkan driver",
            "Roblox will fall back to OpenGL ES. Install your GPU's Vulkan driver: mesa-vulkan-drivers \
             (Debian, Ubuntu, Fedora), vulkan-radeon or vulkan-intel (Arch), or NVIDIA's driver.",
        ),
        (true, n) => check(
            Level::Ok,
            format!("Vulkan driver files: {}", drivers.iter().take(4).cloned().collect::<Vec<_>>().join(", "))
                + if n > 4 { ", ..." } else { "" },
            "",
        ),
        (false, _) => check(
            Level::Warn,
            "no Vulkan loader (libvulkan.so.1)",
            "Roblox will fall back to OpenGL ES. Install libvulkan1 (Debian, Ubuntu), \
             vulkan-loader (Fedora) or vulkan-icd-loader (Arch), plus your GPU's Vulkan driver.",
        ),
    }
}

/// The GPU lines, from what a real Vulkan instance answered.
///
/// `probed` is `None` when nothing was asked (no loader, or a caller that does
/// not want a child process). `kernel_gpus` are the PCI vendor ids of the
/// display controllers the kernel sees, so a machine whose GPU is present and
/// unused can say so instead of only showing the CPU renderer that took over.
/// `nvidia_gl` is only read for that case, when the kernel sees an NVIDIA GPU
/// and Vulkan found none: a Flatpak whose GL extension does not match the
/// host's driver is the usual reason, and is worth saying here, where the
/// symptom is, because [`nvidia_checks`] never sees a device to run on.
pub fn vulkan_devices(
    probed: Option<&Result<Vec<Device>, Failure>>,
    kernel_gpus: &[u32],
    in_flatpak: bool,
    nvidia_gl: &FlatpakGl,
) -> Vec<Check> {
    let Some(probed) = probed else {
        return vec![check(Level::Info, "Vulkan devices were not listed (no loader to ask)", "")];
    };
    let hint = if in_flatpak {
        "The Flatpak gets its GPU driver from the org.freedesktop.Platform.GL extension for your GPU: install \
         the one that matches your driver (`flatpak list` shows what is installed)."
    } else {
        "Install your GPU's Vulkan driver: mesa-vulkan-drivers (Debian, Ubuntu, Fedora), vulkan-radeon or \
         vulkan-intel (Arch), or NVIDIA's driver."
    };
    let driver_fix = format!("Roblox will fall back to OpenGL ES. {hint}");
    let driver_fix = driver_fix.as_str();
    match probed {
        Ok(devices) => {
            let mut out: Vec<Check> = Vec::new();
            let real: Vec<&Device> = devices.iter().filter(|d| d.kind != Kind::Cpu).collect();
            if real.is_empty() {
                let names = devices.iter().map(|d| d.name.clone()).collect::<Vec<_>>().join(", ");
                let mut fix = String::from(
                    "Vulkan works, but only on the CPU, so Roblox would render in software and be very slow. ",
                );
                let kernel = kernel_gpu_names(kernel_gpus);
                if kernel.is_empty() {
                    fix.push_str("No GPU was found in /sys/class/drm either; if this machine has one, its driver is not loaded.");
                } else {
                    fix.push_str(&format!(
                        "The kernel reports {kernel}, so the GPU is there and its Vulkan driver is not being used. {hint}"
                    ));
                    if kernel_gpus.contains(&nvidia::VENDOR_ID) {
                        // Only a mismatch is said; a matching or unreadable extension
                        // is not a cause, and the doc covers the rest.
                        match nvidia_gl.advice() {
                            Some(advice) => fix.push_str(&format!(" {advice}")),
                            None => fix.push_str(&format!(
                                " The Flatpak and two-GPU cases are written up in {}",
                                nvidia::DOC_URL
                            )),
                        }
                    }
                }
                out.push(check(Level::Warn, format!("the only Vulkan device is a CPU renderer: {names}"), fix));
            } else {
                for d in real {
                    out.push(check(Level::Ok, format!("GPU: {}", d.summary()), ""));
                }
                for d in devices.iter().filter(|d| d.kind == Kind::Cpu) {
                    out.push(check(Level::Info, format!("also a CPU renderer: {}", d.name), ""));
                }
            }
            out
        }
        Err(Failure::NoLoader) => {
            vec![check(Level::Info, "Vulkan devices were not listed (no loader to ask)", "")]
        }
        Err(Failure::Instance(-9)) => vec![check(
            Level::Warn,
            "the Vulkan loader found no working driver (VK_ERROR_INCOMPATIBLE_DRIVER)",
            driver_fix,
        )],
        Err(Failure::Instance(code)) => vec![check(
            Level::Warn,
            format!("Vulkan could not start (vkCreateInstance returned {code})"),
            driver_fix,
        )],
        Err(Failure::NoDevices) => vec![check(
            Level::Warn,
            "Vulkan starts and lists no GPU",
            driver_fix,
        )],
        Err(Failure::TimedOut) => vec![check(
            Level::Warn,
            "asking Vulkan for its devices did not finish, so a driver is hanging",
            "Roblox may fall back to OpenGL ES or freeze at start. Try `VK_DRIVER_FILES=<path to one driver's .json>` \
             with each file in /usr/share/vulkan/icd.d to find the one that hangs, and report it.",
        )],
        Err(Failure::Crashed(why)) => vec![check(
            Level::Warn,
            format!("asking Vulkan for its devices crashed: {why}"),
            "A Vulkan driver faulted while loading. Update your GPU driver; if it persists, report it \
             with the driver and version.",
        )],
    }
}

/// What the NVIDIA checks read off this machine, taken once so a test can hand
/// in a machine it made up. Each field is `None` or empty where the machine
/// would not say, and the checks report that as "not visible from here" rather
/// than as a fault.
#[derive(Debug, Clone, Default)]
pub struct NvidiaHost {
    pub in_flatpak: bool,
    /// Whether the game opens on X11, by the loader's rule ([`uses_x11`]).
    pub uses_x11: bool,
    /// The kernel module's version, from `/proc/driver/nvidia/version` or
    /// `/sys/module/nvidia/version` ([`nvidia::host_driver_version`]).
    pub kernel_module: Option<String>,
    /// The driver versions the Flatpak sandbox carries a GL extension for.
    pub sandbox_extensions: Vec<String>,
    /// The text of `/sys/module/nvidia_drm/parameters/modeset`, `None` when it
    /// could not be read. Common inside a Flatpak, and when `nvidia_drm` is not
    /// loaded; the two look the same from here.
    pub modeset: Option<String>,
    /// `CORDIAL_FORCE_GPU_VENDOR`, test only: gate as though the device were
    /// this vendor, so the checks can be watched on a machine without one.
    pub forced: Option<nvidia::VendorOverride>,
}

impl NvidiaHost {
    pub fn read(env: &Env) -> NvidiaHost {
        NvidiaHost::read_with(env, Path::new(nvidia::SANDBOX_GL_DIR), Path::new(nvidia::MODESET_PATH))
    }

    /// [`read`](NvidiaHost::read) with the two places a test needs to point
    /// elsewhere. The kernel module's version still comes from
    /// [`nvidia::host_driver_version`], whose parsing has its own tests.
    pub fn read_with(env: &Env, gl_dir: &Path, modeset_path: &Path) -> NvidiaHost {
        NvidiaHost {
            in_flatpak: env.in_flatpak,
            uses_x11: uses_x11(env.cordial_x11, env.wayland_display.is_some()),
            kernel_module: nvidia::host_driver_version(),
            sandbox_extensions: nvidia::sandbox_extension_versions(gl_dir),
            modeset: std::fs::read_to_string(modeset_path).ok(),
            forced: nvidia::override_from_env(),
        }
    }

    fn flatpak_gl(&self) -> FlatpakGl {
        nvidia::flatpak_gl(self.in_flatpak, self.kernel_module.as_deref(), &self.sandbox_extensions)
    }
}

/// The checks that only mean something for an NVIDIA GPU, one line each.
///
/// **Gated on the device the Vulkan probe listed, not on a kernel module being
/// loaded** ([ADR-046](../../../docs/adr/ADR-046-nvidia-is-gated-on-the-vendor-id.md)):
/// a hybrid laptop has the module loaded and may render on the other GPU, and a
/// line about NVIDIA there would be about hardware the game is not using. A
/// machine whose NVIDIA GPU Vulkan cannot see at all gets no line from here;
/// [`vulkan_devices`] names that case, and adds the Flatpak extension finding to
/// it, since a missing extension is the usual reason.
///
/// **Nothing here has run against a real NVIDIA driver.** What each line claims
/// is what `docs/nvidia.md` and `docs/analysis/nvidia-support.md` already say,
/// and each says plainly what it could not read. The driver-series line is a
/// report from other people's machines (`INFERRED` that it applies to Cordial's
/// own renderer).
pub fn nvidia_checks(devices: &[Device], host: &NvidiaHost) -> Vec<Check> {
    let Some((device, gate)) = devices
        .iter()
        .filter(|d| d.kind != Kind::Cpu)
        .map(|d| (d, nvidia::gate(d.vendor_id, d.driver_version, host.forced)))
        .find(|(_, g)| g.nvidia())
    else {
        return Vec::new();
    };
    let forced = if gate.overridden {
        format!(" ({}, test only)", nvidia::FORCE_VENDOR_ENV)
    } else {
        String::new()
    };
    vec![driver_series(device, &gate, host, &forced), flatpak_extension(host), modeset(host)]
}

/// Whether the driver is one of the series reported to crash Roblox's Vulkan
/// renderer. Keyed on the major alone, because NVIDIA packs the minor into
/// eight bits and a version such as 535.309.01 wraps it
/// ([`nvidia::DriverVersion`]).
fn driver_series(device: &Device, gate: &nvidia::Gate, host: &NvidiaHost, forced: &str) -> Check {
    let major = gate.driver.major;
    let module = host.kernel_module.as_deref().map(|v| format!(", kernel module {v}")).unwrap_or_default();
    if major == 0 {
        return check(
            Level::Info,
            format!("the NVIDIA driver version was not reported by {}{forced}", device.name),
            "",
        );
    }
    if nvidia::driver_advisory(major).is_some() {
        return check(
            Level::Warn,
            format!(
                "NVIDIA driver series {major}{module} is one reported to crash Roblox's Vulkan renderer \
                 when the window is first resized{forced}"
            ),
            format!(
                "Update to driver 580 or newer, or set Settings > Graphics > Renderer to OpenGL ES and \
                 relaunch. This is what people running the same engine reported; it has not been tried on \
                 NVIDIA hardware here. {}",
                nvidia::DOC_URL
            ),
        );
    }
    check(
        Level::Ok,
        format!("NVIDIA driver series {major}{module} is not one of those reported to crash (535, 550){forced}"),
        "",
    )
}

/// Whether a Flatpak carries the NVIDIA GL extension the host's kernel module
/// needs. The comparison and the advice are [`nvidia::flatpak_gl`] and
/// [`nvidia::FlatpakGl::advice`], the same ones `cordial --diagnostics` prints.
fn flatpak_extension(host: &NvidiaHost) -> Check {
    let gl = host.flatpak_gl();
    match &gl {
        FlatpakGl::NotFlatpak => check(
            Level::Info,
            "not a Flatpak, so the machine's own NVIDIA libraries are used and there is no GL extension to match",
            "",
        ),
        FlatpakGl::NoNvidiaModule => check(
            Level::Info,
            "the host's NVIDIA kernel module version is not visible from inside the sandbox, so the Flatpak GL \
             extension could not be compared with it",
            "On the machine itself, compare `cat /proc/driver/nvidia/version` with `flatpak list | grep GL.nvidia`; \
             the two versions have to match exactly.",
        ),
        FlatpakGl::Matches(v) => {
            check(Level::Ok, format!("the Flatpak GL extension matches the host's NVIDIA driver ({v})"), "")
        }
        FlatpakGl::Missing { host } => check(
            Level::Warn,
            format!("the host's NVIDIA driver is {host} and the Flatpak has no NVIDIA GL extension"),
            gl.advice().unwrap_or_default(),
        ),
        FlatpakGl::Different { host, found } => check(
            Level::Warn,
            format!(
                "the host's NVIDIA driver is {host} and the Flatpak's GL extension is {}",
                found.join(", ")
            ),
            gl.advice().unwrap_or_default(),
        ),
    }
}

/// Whether `nvidia-drm` runs with modesetting, which Wayland needs on drivers
/// before 595 (`docs/nvidia.md`; 595 turns it on itself).
fn modeset(host: &NvidiaHost) -> Check {
    let Some(text) = &host.modeset else {
        return check(
            Level::Info,
            format!(
                "nvidia-drm modeset is not visible from here ({} could not be read)",
                nvidia::MODESET_PATH
            ),
            "That is usual inside a Flatpak, and is also what it looks like when nvidia_drm is not loaded. On the \
             machine itself, `cat /sys/module/nvidia_drm/parameters/modeset` should print Y if you use Wayland.",
        );
    };
    match nvidia::parse_modeset(text) {
        Some(true) => check(Level::Ok, "nvidia-drm modeset is on", ""),
        Some(false) if host.uses_x11 => check(
            Level::Info,
            "nvidia-drm modeset is off, which only matters on Wayland, and the game opens on X11 here",
            "",
        ),
        Some(false) => check(
            Level::Warn,
            "nvidia-drm modeset is off, and the game opens on Wayland",
            "Add nvidia-drm.modeset=1 to the kernel command line and reboot; drivers before 595 need it for \
             Wayland, and 595 and later turn it on themselves. If the game window will not appear, CORDIAL_X11=1 \
             opens it on X11 instead, at the cost of the embedded web windows. docs/nvidia.md has the detail.",
        ),
        None => check(
            Level::Info,
            format!("nvidia-drm modeset has a value this check does not recognise: {:?}", text.trim()),
            "",
        ),
    }
}

fn loadable(soname: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(soname) else { return false };
    // SAFETY: `name` is a NUL-terminated string that outlives the call, and a
    // handle that came back is closed before returning. Loading the Vulkan
    // loader runs no driver code; that happens at `vkCreateInstance`.
    unsafe {
        let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            return false;
        }
        libc::dlclose(handle);
    }
    true
}

/// The directories the Vulkan loader reads ICD manifests from when no
/// `VK_DRIVER_FILES` names them, in the order it reads them.
///
/// `/run/opengl-driver/share` is NixOS's: its drivers are not in
/// `XDG_DATA_DIRS` or `/usr/share`, and the loader nixpkgs builds adds that
/// directory itself (INFERRED from nixpkgs's packaging, not run on NixOS). A
/// 0.23.0 diagnostics report from a NixOS machine said "a Vulkan loader but
/// no Vulkan driver" in the same listing that named its RTX 3080 with Vulkan
/// 1.4, because this list stopped at the FHS paths.
fn vulkan_icd_dirs(data_home: Option<PathBuf>, data_dirs: &str) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = data_home {
        dirs.push(home.join("vulkan/icd.d"));
    }
    dirs.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join("vulkan/icd.d")));
    dirs.extend(
        ["/etc/vulkan/icd.d", "/etc/xdg/vulkan/icd.d", "/usr/share/vulkan/icd.d", "/run/opengl-driver/share/vulkan/icd.d"]
            .map(PathBuf::from),
    );
    dirs
}

/// The ICD manifests the Vulkan loader would read, by file name, from the
/// places its documentation says it looks.
fn vulkan_drivers() -> Vec<String> {
    for var in ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"] {
        if let Some(list) = std::env::var_os(var) {
            return std::env::split_paths(&list)
                .filter(|p| p.exists())
                .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                .collect();
        }
    }
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let dirs = vulkan_icd_dirs(data_home, &data_dirs);
    let mut found: Vec<String> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") && !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found.sort();
    found
}

/// PCI vendor ids of the display controllers the kernel sees, from
/// `/sys/class/drm/card*/device/vendor`. Read from the kernel rather than from
/// Vulkan, so it can disagree with Vulkan, which is the point.
fn kernel_gpu_vendors() -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut ids: Vec<u32> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `card0`, not `card0-HDMI-A-1` (a connector) or `renderD128`.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path().join("device/vendor")) else { continue };
        if let Some(id) = parse_hex(text.trim()) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    ids
}

fn parse_hex(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn kernel_gpu_names(ids: &[u32]) -> String {
    let named: Vec<String> = ids
        .iter()
        .map(|&id| match id {
            0x10de => "an NVIDIA GPU".to_string(),
            0x1002 => "an AMD GPU".to_string(),
            0x8086 => "an Intel GPU".to_string(),
            other => format!("a GPU with PCI vendor {other:#06x}"),
        })
        .collect();
    named.join(" and ")
}

/// What is in `$XDG_RUNTIME_DIR` that a sound server leaves there. The client
/// probes PipeWire, PulseAudio, ALSA and OSS in that order; this says which
/// sockets exist and claims nothing about which one the client will pick.
pub fn audio(env: &Env) -> Check {
    let Some(dir) = &env.runtime_dir else {
        return check(
            Level::Warn,
            "no XDG_RUNTIME_DIR, so no sound server can be found",
            "Run Cordial from inside your desktop session.",
        );
    };
    if dir.join("pipewire-0").exists() {
        check(Level::Ok, "a PipeWire socket is present", "")
    } else if dir.join("pulse/native").exists() {
        check(Level::Ok, "a PulseAudio socket is present", "")
    } else {
        check(
            Level::Warn,
            "no PipeWire or PulseAudio socket in this session",
            "Roblox will try ALSA directly, which may leave you without sound. \
             Start PipeWire (systemctl --user start pipewire pipewire-pulse).",
        )
    }
}

fn session_bus() -> Option<zbus::blocking::Connection> {
    zbus::blocking::Connection::session().ok()
}

/// Whether `name` is owned now or can be started on demand.
fn bus_has(bus: &zbus::blocking::Connection, name: &str) -> bool {
    let proxy = match zbus::blocking::fdo::DBusProxy::new(bus) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let listed = |names: zbus::fdo::Result<Vec<zbus::names::OwnedBusName>>| {
        names.map(|n| n.iter().any(|b| b.as_str() == name)).unwrap_or(false)
    };
    listed(proxy.list_names()) || listed(proxy.list_activatable_names())
}

pub fn keyring(bus: Option<&zbus::blocking::Connection>) -> Check {
    match bus {
        None => check(
            Level::Warn,
            "no D-Bus session bus",
            "Your sign-in will be kept in a plain file, and notifications and GameMode will not work. \
             Run Cordial from inside your desktop session.",
        ),
        Some(bus) if bus_has(bus, "org.freedesktop.secrets") => {
            check(Level::Ok, "a keyring (Secret Service) is available for your sign-in", "")
        }
        Some(_) => check(
            Level::Warn,
            "no keyring (Secret Service) in this session",
            "Your sign-in will be kept in a 0600 file in the profile instead. Install and start \
             gnome-keyring or KeePassXC's Secret Service if you want it in a keyring.",
        ),
    }
}

pub fn gamemode(bus: Option<&zbus::blocking::Connection>) -> Check {
    match bus {
        // Without a bus this cannot tell, and the keyring line has already
        // said GameMode will not work -- "not installed" would be a guess.
        None => check(Level::Info, "could not check for GameMode without a session bus", ""),
        Some(bus) if bus_has(bus, "com.feralinteractive.GameMode") => check(Level::Ok, "GameMode is available", ""),
        Some(_) => check(
            Level::Info,
            "GameMode is not installed",
            "Optional. With it, the CPU governor is set to performance while you play: install gamemode, \
             or turn the GameMode setting off to stop asking for it.",
        ),
    }
}

/// Which desktop entry the browser's Play button opens.
///
/// `xdg-mime` when it is installed, since it knows each desktop's own lookup
/// order; otherwise the user's `mimeapps.list`.
fn browser_handler() -> Check {
    let answer = std::process::Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/roblox-player"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .or_else(|| user_default("x-scheme-handler/roblox-player"));
    handler_check(answer.as_deref())
}

fn handler_check(answer: Option<&str>) -> Check {
    const FIX: &str = "Installing Cordial from a package registers it. From a source build, copy \
                       packaging/io.github.luohoa97.Cordial.desktop to ~/.local/share/applications/ and run \
                       `xdg-mime default io.github.luohoa97.Cordial.desktop x-scheme-handler/roblox-player x-scheme-handler/roblox`.";
    match answer {
        Some(a) if a == DESKTOP_ID => check(Level::Ok, "the website's Play button opens Cordial", ""),
        Some("") | None => check(Level::Warn, "nothing is registered for the website's Play button", FIX),
        Some(other) => check(Level::Warn, format!("the website's Play button opens {other}, not Cordial"), FIX),
    }
}

/// The user's own default for `scheme`, from `mimeapps.list`.
fn user_default(scheme: &str) -> Option<String> {
    let home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let text = std::fs::read_to_string(home.join("mimeapps.list")).ok()?;
    default_in(&text, scheme)
}

/// `scheme`'s default in a `mimeapps.list`'s `[Default Applications]`, and only
/// there: `[Added Associations]` names apps that *may* open it, which is not a
/// default. Adapted from DamnShabu/stacked's `desktop.rs`.
fn default_in(text: &str, scheme: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == "[Default Applications]";
        } else if in_section {
            if let Some((key, value)) = trimmed.split_once('=') {
                if key.trim() == scheme {
                    // A list is allowed; the first entry is the default.
                    return value.split(';').map(str::trim).find(|v| !v.is_empty()).map(String::from);
                }
            }
        }
    }
    None
}

/// Plugins that run code are Deno programs. Info rather than a warning: most
/// people have no plugins, and a machine without Deno is not broken.
pub fn deno() -> Check {
    if cordial_plugins::sandbox::interpreter_present() {
        return check(Level::Ok, "Deno is installed, so plugins that run code can start", "");
    }
    check(
        Level::Info,
        "Deno is not installed, so a plugin that runs code will not start",
        "Only matters if you use such a plugin. Settings, Plugins has a Download button under \
         \"Deno is not installed\" (about 39 MB).",
    )
}

/// Free space where Roblox's files and the profiles live, in one line when
/// they share a filesystem and two when they do not.
fn disk() -> Vec<Check> {
    let cache = cordial_update::install::cache_root();
    let profiles = profile::root();
    let (cache_dev, profile_dev) = (device_of(&cache), device_of(&profiles));
    let mut out = vec![space_check("Roblox's files", &cache, 1_000_000_000)];
    if cache_dev.is_none() || cache_dev != profile_dev {
        out.push(space_check("profiles", &profiles, 200_000_000));
    }
    out
}

fn space_check(what: &str, path: &Path, warn_below: u64) -> Check {
    match free_bytes(path) {
        Some(free) if free < warn_below => check(
            Level::Warn,
            format!("{} MB free for {what}", free / 1_000_000),
            "A Roblox build needs about 1 GB. Remove old builds on Settings, Version, or free space on that disk.",
        ),
        Some(free) => check(Level::Ok, format!("{} GB free for {what}", free / 1_000_000_000), ""),
        None => check(Level::Info, format!("could not measure free disk space for {what}"), ""),
    }
}

/// The filesystem `path` (or its nearest existing parent) is on.
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    std::fs::metadata(probe).ok().map(|m| m.dev())
}

fn free_bytes(path: &Path) -> Option<u64> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    let c = std::ffi::CString::new(probe.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: `statvfs` is a plain C struct of integers, for which all-zero
    // is a valid value; the call below overwrites it.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and outlives the call; `stat` is a valid
    // out-parameter of the right type.
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// Which profiles are open in a running client, as a count.
///
/// **Not named**, and not one line per profile: a profile name is often the
/// account's, and this output is pasted into public issues -- the same rule
/// `diagnostics.rs` keeps. A count says what a bug report needs (several
/// profiles open at once is its own risk, see `multi_instance_warning.rs`) and
/// nothing about whose they are. `is_held` is the same non-blocking `flock`
/// [`profile::acquire`] takes, so probing a live profile disturbs nothing.
pub fn profile_locks(names: &[String], is_held: impl Fn(&str) -> bool) -> Check {
    let held = names.iter().filter(|n| is_held(n)).count();
    match (names.len(), held) {
        (0, _) => check(Level::Info, "no profile exists yet", ""),
        (n, 0) => check(Level::Ok, format!("no profile is open in a running client ({n} exist)"), ""),
        (n, 1) => check(
            Level::Info,
            format!("1 of {n} profiles is open in a running client"),
            "A second Cordial on that profile is refused; open a different profile from the launcher.",
        ),
        (n, h) => check(
            Level::Info,
            format!("{h} of {n} profiles are open in running clients"),
            "Running several accounts at once is a Roblox account risk, and a second Cordial on an \
             open profile is refused.",
        ),
    }
}

/// "Play in VR" (ADR-053). Optional, so nothing here is worse than `Info`
/// unless the user chose a runtime that has since gone, which is `Warn`: the
/// button they set up will not work and the doctor is where they look.
///
/// One line when VR was never set up, because a launcher whose doctor lists
/// three missing VR pieces to somebody who has no headset reads as broken.
pub fn vr(readiness: &crate::vr::Readiness, runtime_chosen: bool) -> Vec<Check> {
    use crate::vr::Chosen;
    if readiness.quest_build.is_none() && !runtime_chosen {
        return vec![check(
            Level::Info,
            "VR: no Quest build imported (optional)",
            "To play in a headset, import the Quest build in Settings → VR. See docs/vr.md.",
        )];
    }
    let mut out = vec![match &readiness.quest_build {
        Some(e) => check(Level::Ok, format!("VR: Quest build {} is imported", e.version), ""),
        None => check(
            Level::Info,
            "VR: no Quest build imported",
            "Import it from your headset in Settings → VR.",
        ),
    }];
    out.push(match &readiness.runtime {
        Chosen::System(r) => check(Level::Ok, format!("VR: OpenXR runtime {} (the system's active one)", r.name), ""),
        Chosen::Manifest(r) => check(Level::Ok, format!("VR: OpenXR runtime {} (chosen in Settings)", r.name), ""),
        Chosen::Missing(why) => check(
            if runtime_chosen { Level::Warn } else { Level::Info },
            "VR: no usable OpenXR runtime",
            why.clone(),
        ),
    });
    match readiness.wivrn_server {
        Some(true) => out.push(check(Level::Ok, "VR: the WiVRn server is running", "")),
        Some(false) => out.push(check(
            Level::Info,
            "VR: the WiVRn server is not running",
            "Start WiVRn, or run its server with --no-manage-active-runtime so it leaves the \
             system's runtime alone.",
        )),
        None => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wayland_env() -> Env {
        Env {
            wayland_display: Some("wayland-1".into()),
            runtime_dir: Some(PathBuf::from("/nonexistent-runtime")),
            ..Env::default()
        }
    }

    #[test]
    fn a_failure_outranks_a_warning_and_the_exit_code_follows_failures_only() {
        assert!(Level::Fail > Level::Warn && Level::Warn > Level::Info && Level::Info > Level::Ok);
        let warn_only = [check(Level::Warn, "w", "fix")];
        assert_eq!(exit_status(&warn_only), 0);
        assert_eq!(exit_status(&[check(Level::Fail, "f", "fix")]), 1);
    }

    #[test]
    fn the_home_directory_is_never_printed() {
        assert_eq!(
            private_with("cordial-run is at /home/a/.local/bin/cordial-run", "/home/a"),
            "cordial-run is at ~/.local/bin/cordial-run"
        );
        assert_eq!(private_with("/home/a is home", "/home/a/"), "~ is home");
    }

    /// The fork replaced the bare string, so `/home/ab/x` became `~b/x` for a
    /// user called `a`: wrong for both of them.
    #[test]
    fn a_neighbouring_path_that_only_starts_the_same_is_left_alone() {
        assert_eq!(private_with("/home/ab/x and /home/a/y", "/home/a"), "/home/ab/x and ~/y");
        assert_eq!(private_with("/srv/home/a/x", "/home/a"), "/srv/home/a/x");
        assert_eq!(private_with("nothing here", "/home/a"), "nothing here");
        assert_eq!(private_with("anything", "/"), "anything", "a root home has no name to hide");
    }

    #[test]
    fn a_missing_library_is_not_loadable_and_libc_is() {
        assert!(!loadable("libcordial-there-is-no-such-library.so.9"));
        assert!(loadable("libc.so.6"));
    }

    #[test]
    fn driver_files_named_by_the_environment_win() {
        let dir = tempfile::tempdir().unwrap();
        let icd = dir.path().join("test_icd.json");
        std::fs::write(&icd, "{}").unwrap();
        std::env::set_var("VK_DRIVER_FILES", &icd);
        let found = vulkan_drivers();
        std::env::remove_var("VK_DRIVER_FILES");
        assert_eq!(found, vec!["test_icd.json".to_string()]);
    }

    #[test]
    fn nixos_driver_directory_is_searched() {
        let dirs = vulkan_icd_dirs(Some(PathBuf::from("/home/a/.local/share")), "/usr/local/share:/usr/share");
        assert!(dirs.contains(&PathBuf::from("/run/opengl-driver/share/vulkan/icd.d")));
        assert_eq!(dirs[0], PathBuf::from("/home/a/.local/share/vulkan/icd.d"), "the user's own directory is read first");
    }

    #[test]
    fn the_loader_and_gtk_backends_are_told_apart() {
        assert!(!uses_x11(false, true), "a Wayland session with nothing forced");
        assert!(uses_x11(true, true), "CORDIAL_X11 forces X11 over a compositor");
        assert!(uses_x11(false, false), "no compositor leaves X11");
    }

    #[test]
    fn the_build_origin_is_a_note_and_never_a_warning() {
        use crate::version::Origin;
        let official = build_origin(&Origin::Official);
        assert_eq!((official.level, official.what.as_str()), (Level::Ok, "Official build"));
        let fork = build_origin(&Origin::Unofficial { remote: Some("https://github.com/a/b".into()) });
        assert_eq!(fork.level, Level::Info);
        assert_eq!(fork.what, "Unofficial build from https://github.com/a/b");
        assert!(fork.fix.contains("Say so"));
    }

    #[test]
    fn a_session_with_no_display_fails() {
        let c = session(&Env::default());
        assert_eq!(c.level, Level::Fail);
        assert!(!c.fix.is_empty());
    }

    #[test]
    fn a_wayland_display_whose_socket_is_missing_fails_and_names_it() {
        let c = session(&wayland_env());
        assert_eq!(c.level, Level::Fail);
        assert!(c.what.contains("wayland-1"), "{}", c.what);
    }

    #[test]
    fn wayland_is_ok_and_x11_is_a_warning_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wayland-1"), "").unwrap();
        let wayland = Env { runtime_dir: Some(dir.path().to_path_buf()), ..wayland_env() };
        assert_eq!(session(&wayland).level, Level::Ok);

        // Forced on a Wayland session: the fix is to unset the variable.
        let forced = Env { cordial_x11: true, ..wayland.clone() };
        let c = session(&forced);
        assert_eq!(c.level, Level::Warn);
        assert!(c.what.contains("CORDIAL_X11") && c.fix.contains("Unset CORDIAL_X11"), "{c:?}");

        // A plain X11 session.
        let x11 = Env { display: Some(":0".into()), ..Env::default() };
        let c = session(&x11);
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.contains("Wayland"), "{c:?}");

        // A Wayland session that lost its socket variable is a different fix.
        let dropped = Env { display: Some(":0".into()), session_type: Some("wayland".into()), ..Env::default() };
        assert!(session(&dropped).what.contains("WAYLAND_DISPLAY is not set"));
    }

    #[test]
    fn a_launcher_on_xwayland_does_not_make_the_game_x11() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wayland-1"), "").unwrap();
        let env = Env {
            runtime_dir: Some(dir.path().to_path_buf()),
            gdk_backend: Some("x11".into()),
            ..wayland_env()
        };
        let c = session(&env);
        assert_eq!(c.level, Level::Ok, "{c:?}");
        assert!(c.what.contains("GDK_BACKEND=x11"), "{c:?}");
    }

    fn device(kind: Kind, vendor: u32, name: &str) -> Device {
        Device {
            vendor_id: vendor,
            device_id: 1,
            kind,
            name: name.into(),
            driver_name: Some("driver".into()),
            driver_info: None,
            driver_version: 1 << 22,
            api_version: (1 << 22) | (3 << 12),
        }
    }

    #[test]
    fn a_working_gpu_is_named_with_its_vendor_and_driver() {
        let devices = Ok(vec![device(Kind::Discrete, 0x10de, "RTX")]);
        let checks = vulkan_devices(Some(&devices), &[0x10de], false, &FlatpakGl::NotFlatpak);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].level, Level::Ok);
        assert!(checks[0].what.contains("RTX") && checks[0].what.contains("NVIDIA"), "{:?}", checks[0]);
    }

    /// The point of the probe: Vulkan "works" and it is the CPU.
    #[test]
    fn a_cpu_only_vulkan_is_a_warning_that_says_the_gpu_is_unused() {
        let devices = Ok(vec![device(Kind::Cpu, 0x10005, "llvmpipe (LLVM 19.1, 256 bits)")]);
        let with_gpu = vulkan_devices(Some(&devices), &[0x10de], false, &FlatpakGl::NotFlatpak);
        assert_eq!(with_gpu.len(), 1);
        assert_eq!(with_gpu[0].level, Level::Warn);
        assert!(with_gpu[0].what.contains("llvmpipe"), "{:?}", with_gpu[0]);
        assert!(with_gpu[0].fix.contains("NVIDIA GPU") && with_gpu[0].fix.contains("not being used"), "{:?}", with_gpu[0]);

        // With no GPU seen by the kernel, it does not claim one.
        let without = vulkan_devices(Some(&devices), &[], false, &FlatpakGl::NotFlatpak);
        assert!(!without[0].fix.contains("kernel reports"), "{:?}", without[0]);
    }

    #[test]
    fn a_cpu_renderer_beside_a_real_gpu_is_only_a_note() {
        let devices = Ok(vec![
            device(Kind::Integrated, 0x8086, "Intel(R) Graphics"),
            device(Kind::Cpu, 0x10005, "llvmpipe"),
        ]);
        let checks = vulkan_devices(Some(&devices), &[0x8086], false, &FlatpakGl::NotFlatpak);
        assert!(checks.iter().all(|c| c.level != Level::Warn), "{checks:?}");
        assert!(checks.iter().any(|c| c.what.contains("Intel")));
    }

    #[test]
    fn every_way_the_probe_can_fail_is_a_warning_with_a_fix() {
        for failure in [
            Failure::Instance(-9),
            Failure::Instance(-3),
            Failure::NoDevices,
            Failure::TimedOut,
            Failure::Crashed("the probe ended with signal: 11 (SIGSEGV)".into()),
        ] {
            let checks = vulkan_devices(Some(&Err(failure.clone())), &[], false, &FlatpakGl::NotFlatpak);
            assert_eq!(checks.len(), 1);
            assert_eq!(checks[0].level, Level::Warn, "{failure:?}");
            assert!(!checks[0].fix.trim().is_empty(), "{failure:?} has no fix");
        }
        // No loader was already reported by the file check; here it is a note.
        let none = vulkan_devices(Some(&Err(Failure::NoLoader)), &[], false, &FlatpakGl::NotFlatpak);
        assert_eq!(none[0].level, Level::Info);
    }

    #[test]
    fn a_flatpak_is_told_about_the_gl_extension_and_not_a_distro_package() {
        let checks = vulkan_devices(Some(&Err(Failure::Instance(-9))), &[], true, &FlatpakGl::NotFlatpak);
        assert!(checks[0].fix.contains("org.freedesktop.Platform.GL"), "{:?}", checks[0]);
        assert!(!checks[0].fix.contains("apt"), "{:?}", checks[0]);
    }

    fn nvidia_device(major: u32, minor: u32) -> Device {
        Device { driver_version: (major << 22) | (minor << 14), ..device(Kind::Discrete, 0x10de, "RTX") }
    }

    /// A Wayland session on a machine whose module is 580.82.09, in a Flatpak
    /// that carries the matching extension and has modeset on: everything
    /// passes. Each test spoils one thing.
    fn good_host() -> NvidiaHost {
        NvidiaHost {
            in_flatpak: true,
            uses_x11: false,
            kernel_module: Some("580.82.09".into()),
            sandbox_extensions: vec!["580.82.09".into()],
            modeset: Some("Y\n".into()),
            forced: None,
        }
    }

    fn run(host: &NvidiaHost) -> Vec<Check> {
        nvidia_checks(&[nvidia_device(580, 82)], host)
    }

    #[test]
    fn a_healthy_nvidia_machine_passes_every_line() {
        let checks = run(&good_host());
        assert_eq!(checks.len(), 3, "{checks:?}");
        assert!(checks.iter().all(|c| c.level == Level::Ok && c.fix.is_empty()), "{checks:?}");
    }

    #[test]
    fn nothing_is_said_unless_a_device_is_nvidia() {
        let mut host = good_host();
        host.modeset = Some("N".into());
        host.sandbox_extensions.clear();
        assert!(nvidia_checks(&[device(Kind::Discrete, 0x1002, "Radeon")], &host).is_empty());
        assert!(nvidia_checks(&[device(Kind::Integrated, 0x8086, "Intel")], &host).is_empty());
        assert!(nvidia_checks(&[], &host).is_empty());
        // A CPU renderer carrying NVIDIA's vendor id is not an NVIDIA GPU.
        assert!(nvidia_checks(&[device(Kind::Cpu, 0x10de, "llvmpipe")], &host).is_empty());
        // The module being loaded is not the gate: a hybrid laptop on Intel.
        assert!(host.kernel_module.is_some());
        assert!(nvidia_checks(&[device(Kind::Integrated, 0x8086, "Intel")], &host).is_empty());
    }

    #[test]
    fn the_reported_driver_series_warn_and_the_others_do_not() {
        for major in [535, 550] {
            let checks = nvidia_checks(&[nvidia_device(major, 1)], &good_host());
            assert_eq!(checks[0].level, Level::Warn, "{major}");
            assert!(checks[0].what.contains(&format!("series {major}")), "{:?}", checks[0]);
            assert!(checks[0].fix.contains("580") && checks[0].fix.contains("OpenGL ES"), "{:?}", checks[0]);
            assert!(checks[0].fix.contains("not been tried"), "must say it is untested: {:?}", checks[0]);
        }
        for major in [470, 545, 555, 570, 580, 595] {
            let checks = nvidia_checks(&[nvidia_device(major, 1)], &good_host());
            assert_eq!(checks[0].level, Level::Ok, "{major}: {:?}", checks[0]);
        }
        // A driver that reports no version is not read as a series.
        let none = nvidia_checks(&[nvidia_device(0, 0)], &good_host());
        assert_eq!(none[0].level, Level::Info, "{:?}", none[0]);
    }

    /// The minor wraps at 256 (535.309.01), so the line keys on the major and
    /// never prints a packed minor it cannot trust.
    #[test]
    fn the_series_line_does_not_print_the_untrustworthy_packed_minor() {
        let checks = nvidia_checks(&[nvidia_device(535, 309 & 0xff)], &good_host());
        assert!(!checks[0].what.contains(&format!("535.{}", 309 & 0xff)), "{:?}", checks[0]);
    }

    #[test]
    fn the_test_override_gates_any_gpu_as_nvidia_and_says_so() {
        let mut host = good_host();
        host.forced = nvidia::parse_override("0x10de@550.163.01");
        let intel = [Device { driver_version: 0, ..device(Kind::Integrated, 0x8086, "Intel") }];
        let checks = nvidia_checks(&intel, &host);
        assert_eq!(checks.len(), 3, "{checks:?}");
        assert_eq!(checks[0].level, Level::Warn, "{:?}", checks[0]);
        assert!(checks[0].what.contains("series 550") && checks[0].what.contains("test only"), "{:?}", checks[0]);
        // A CPU renderer is still not an NVIDIA GPU, forced or not.
        assert!(nvidia_checks(&[device(Kind::Cpu, 0x10005, "llvmpipe")], &host).is_empty());
    }

    #[test]
    fn the_flatpak_extension_is_compared_with_the_host_and_says_what_it_could_not_see() {
        let line = |host: &NvidiaHost| run(host).remove(1);

        let mut host = good_host();
        host.sandbox_extensions = vec![];
        let c = line(&host);
        assert_eq!(c.level, Level::Warn);
        assert!(c.what.contains("580.82.09") && c.what.contains("no NVIDIA GL extension"), "{c:?}");
        assert!(c.fix.contains("flatpak update") && c.fix.contains("GL.nvidia-580-82-09"), "{c:?}");

        host.sandbox_extensions = vec!["550.163.01".into(), "535.183.01".into()];
        let c = line(&host);
        assert_eq!(c.level, Level::Warn);
        assert!(c.what.contains("550.163.01, 535.183.01"), "{c:?}");
        assert!(c.fix.contains("flatpak update"), "{c:?}");

        // Dashes or dots, it is the same version.
        host.sandbox_extensions = vec!["580.82.09".into()];
        assert_eq!(line(&host).level, Level::Ok);

        // In a sandbox that cannot see the module: unknown, not a mismatch.
        host.kernel_module = None;
        host.sandbox_extensions = vec![];
        let c = line(&host);
        assert_eq!(c.level, Level::Info);
        assert!(c.what.contains("not visible"), "{c:?}");

        // Outside a Flatpak there is no second copy to disagree.
        host.in_flatpak = false;
        let c = line(&host);
        assert_eq!(c.level, Level::Info);
        assert!(c.what.contains("not a Flatpak"), "{c:?}");
    }

    #[test]
    fn modeset_warns_only_when_it_is_off_on_wayland_and_says_when_it_cannot_see() {
        let line = |host: &NvidiaHost| run(host).remove(2);
        let mut host = good_host();

        for off in ["N\n", "0"] {
            host.modeset = Some(off.into());
            let c = line(&host);
            assert_eq!(c.level, Level::Warn, "{off:?}");
            assert!(c.fix.contains("nvidia-drm.modeset=1") && c.fix.contains("595"), "{c:?}");
        }

        // On X11 it does not matter, so it is a note.
        host.uses_x11 = true;
        let c = line(&host);
        assert_eq!(c.level, Level::Info);
        assert!(c.fix.is_empty(), "{c:?}");
        host.uses_x11 = false;

        for on in ["Y\n", "1"] {
            host.modeset = Some(on.into());
            assert_eq!(line(&host).level, Level::Ok, "{on:?}");
        }

        // Unreadable (a Flatpak sandbox, or no nvidia_drm): not guessed at.
        host.modeset = None;
        let c = line(&host);
        assert_eq!(c.level, Level::Info);
        assert!(c.what.contains("not visible from here"), "{c:?}");

        host.modeset = Some("maybe".into());
        assert_eq!(line(&host).level, Level::Info);
    }

    /// The sysfs and sandbox reads, against directories a test made.
    #[test]
    fn the_host_is_read_from_the_paths_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let gl = dir.path().join("GL");
        for name in ["default", "nvidia-580-82-09"] {
            std::fs::create_dir_all(gl.join(name)).unwrap();
        }
        let modeset = dir.path().join("modeset");
        std::fs::write(&modeset, "N\n").unwrap();
        let env = Env { in_flatpak: true, wayland_display: Some("wayland-0".into()), ..Env::default() };
        let host = NvidiaHost::read_with(&env, &gl, &modeset);
        assert_eq!(host.sandbox_extensions, vec!["580.82.09".to_string()]);
        assert_eq!(host.modeset.as_deref(), Some("N\n"));
        assert!(host.in_flatpak && !host.uses_x11);

        let gone = NvidiaHost::read_with(&env, &dir.path().join("none"), &dir.path().join("nope"));
        assert!(gone.sandbox_extensions.is_empty() && gone.modeset.is_none());
    }

    /// An NVIDIA GPU the kernel sees and Vulkan does not is where a Flatpak's
    /// missing extension shows up, and no NVIDIA device exists to run the
    /// NVIDIA checks on, so the CPU-only line carries the finding.
    #[test]
    fn a_cpu_only_vulkan_beside_an_nvidia_gpu_carries_the_extension_finding() {
        let devices = Ok(vec![device(Kind::Cpu, 0x10005, "llvmpipe")]);
        let mismatch = FlatpakGl::Missing { host: "580.82.09".into() };
        let c = vulkan_devices(Some(&devices), &[0x10de], true, &mismatch);
        assert!(c[0].fix.contains("flatpak update") && c[0].fix.contains("580.82.09"), "{:?}", c[0]);

        // A matching extension is not a cause, so it is not offered as one.
        let c = vulkan_devices(Some(&devices), &[0x10de], true, &FlatpakGl::Matches("580.82.09".into()));
        assert!(!c[0].fix.contains("flatpak update"), "{:?}", c[0]);
        assert!(c[0].fix.contains("docs/nvidia.md"), "{:?}", c[0]);

        // And nothing NVIDIA-shaped for a machine whose kernel GPU is not.
        let c = vulkan_devices(Some(&devices), &[0x8086], true, &mismatch);
        assert!(!c[0].fix.contains("flatpak update") && !c[0].fix.contains("nvidia.md"), "{:?}", c[0]);
    }

    #[test]
    fn the_kernel_gpu_ids_are_read_as_hex() {
        assert_eq!(parse_hex("0x10de"), Some(0x10de));
        assert_eq!(parse_hex("8086"), Some(0x8086));
        assert_eq!(parse_hex("nonsense"), None);
    }

    #[test]
    fn profile_locks_count_and_never_name() {
        let names: Vec<String> = ["alice", "bob", "carol"].map(String::from).to_vec();
        let none = profile_locks(&names, |_| false);
        assert_eq!(none.level, Level::Ok);
        let one = profile_locks(&names, |n| n == "bob");
        assert_eq!(one.level, Level::Info);
        assert!(one.what.starts_with("1 of 3"), "{}", one.what);
        let two = profile_locks(&names, |n| n != "carol");
        assert!(two.what.starts_with("2 of 3"), "{}", two.what);
        for c in [&none, &one, &two] {
            assert!(
                !["alice", "bob", "carol"].iter().any(|n| c.what.contains(n) || c.fix.contains(n)),
                "a profile name reached the output: {c:?}"
            );
        }
        assert_eq!(profile_locks(&[], |_| true).level, Level::Info);
    }

    #[test]
    fn the_play_button_handler_is_read_from_the_default_section_only() {
        let text = "[Added Associations]\nx-scheme-handler/roblox-player=a.desktop\n\
                    [Default Applications]\nx-scheme-handler/roblox-player=b.desktop;c.desktop;\n";
        assert_eq!(default_in(text, "x-scheme-handler/roblox-player").as_deref(), Some("b.desktop"));
        assert_eq!(default_in(text, "x-scheme-handler/roblox"), None);

        assert_eq!(handler_check(Some(DESKTOP_ID)).level, Level::Ok);
        let other = handler_check(Some("org.vinegarhq.Sober.desktop"));
        assert_eq!(other.level, Level::Warn);
        assert!(other.what.contains("Sober"), "{other:?}");
        assert_eq!(handler_check(None).level, Level::Warn);
        assert_eq!(handler_check(Some("")).level, Level::Warn);
    }

    #[test]
    fn a_shared_filesystem_gets_one_disk_line() {
        let dir = tempfile::tempdir().unwrap();
        assert!(device_of(dir.path()).is_some());
        assert_eq!(device_of(&dir.path().join("a/b")), device_of(dir.path()));
        assert!(free_bytes(dir.path()).is_some());
    }

    #[test]
    fn deno_is_never_worse_than_a_note() {
        // Whichever way this machine answers.
        assert!(matches!(deno().level, Level::Ok | Level::Info));
    }

    #[test]
    fn the_verdict_counts_and_the_render_indents_the_fix() {
        let checks = [
            check(Level::Ok, "fine", ""),
            check(Level::Warn, "odd", "do this\nthen that"),
        ];
        let text = render(&checks);
        assert!(text.contains("ok    fine\n"), "{text}");
        assert!(text.contains("      do this\n      then that\n"), "{text}");
        assert!(text.trim_end().ends_with("1 warning; Roblox should still start."), "{text}");
        assert!(verdict(&[check(Level::Fail, "f", "x")]).contains("1 problem will stop"));
        assert!(verdict(&[]).starts_with("Nothing found"));
    }

    /// The whole list, on whatever machine runs the tests, with the child
    /// process left out. Every warning and failure must say what to do.
    #[test]
    fn every_check_that_warns_or_fails_says_what_to_do() {
        let inputs = Inputs { gamemode: true, probe_vulkan: false };
        for c in machine(&inputs) {
            if matches!(c.level, Level::Warn | Level::Fail) {
                assert!(!c.fix.trim().is_empty(), "{:?} has no fix", c.what);
            }
        }
    }

    #[test]
    fn vr_is_one_info_line_until_somebody_sets_it_up() {
        use crate::vr::{Chosen, Readiness, Runtime};
        let never = Readiness { quest_build: None, runtime: Chosen::Missing("none".into()), wivrn_server: None, sandboxed: false };
        let lines = vr(&never, false);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].level, Level::Info);

        let wivrn = Runtime { id: "io.github.wivrn.wivrn".into(), name: "WiVRn".into(), manifest: "/x.json".into() };
        let entry = cordial_update::store::Entry {
            version: "2.740.0.927".into(),
            dir: "/b".into(),
            loaded_by: None,
            bytes: 0,
            complete: true,
            content_hash: None,
            signer: None,
            provenance: None,
            version_code: None,
        };
        let set_up = Readiness { quest_build: Some(entry), runtime: Chosen::Manifest(wivrn), wivrn_server: Some(false), sandboxed: false };
        let lines = vr(&set_up, true);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines.iter().all(|c| c.level <= Level::Info), "VR is optional and must not fail the doctor");

        let gone = Readiness { quest_build: None, runtime: Chosen::Missing("gone".into()), wivrn_server: None, sandboxed: false };
        assert!(vr(&gone, true).iter().any(|c| c.level == Level::Warn));
    }

}
