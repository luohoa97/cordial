//! Which renderer GTK draws the host window with, decided before it asks.
//!
//! **Why this exists.** The engine's window is a GTK toplevel with the canvas
//! as a subsurface, and a focused TextBox is drawn by GTK over the game
//! (ADR-047). With no working Vulkan the engine falls back to GLES3 in the same
//! process as GTK, GTK falls back to its GL renderer, and from then on GTK
//! never attaches a buffer to the window again: no frame, no editor, whatever
//! the stacking does. `GSK_RENDERER=cairo` takes GTK off that path and was
//! measured to present normally on the same reproduction (issue #53, nested
//! KWin, `VK_ICD_FILENAMES=/nonexistent`).
//!
//! The window holds a header bar and a transparent hole where the canvas
//! shows, so software drawing costs nothing worth the name, and this chooses
//! it for exactly the machines that would otherwise get nothing.
//!
//! **Why the Vulkan probe and not the Graphics setting.** What breaks GTK is
//! that *GTK* has no Vulkan to render with, not that the engine took GLES:
//! forcing the engine to GLES on a machine whose Vulkan works leaves GTK on
//! Vulkan and presenting (ADR-047), so `CORDIAL_GRAPHICS=gles` alone is not a
//! reason. The question is whether a hardware Vulkan device exists, which is
//! what [`crate::vulkan_probe`] already answers in a child process.
//!
//! **Why NVIDIA's own driver is cairo as well.** There the failure is the
//! opposite one: GTK has a Vulkan renderer and it is the problem. Every core
//! of a crashed client on driver 620.70.06 (2026-11-15 to 17: four from the
//! 0.34.1 Flatpak, and each one a source build gave with GTK put back on
//! Vulkan) stops on the main thread at address zero, called from inside the
//! driver, called from GTK's frame (`gdk_vulkan_context_begin_frame` at its
//! `vkAcquireNextImageKHR`, or `_end_frame`), not the engine's. Cairo takes GTK
//! out of the driver altogether, which is the only one of GTK's renderers that
//! does. What the driver trips over is not known; ADR-047 has the counts.
//!
//! **When it runs.** The probe starts in `cordial-run`'s `main`, concurrent
//! with everything the client does before the engine asks for its window, and
//! [`settle`] collects it there. On a healthy machine that is no added
//! startup latency: the child finishes long before the window is wanted. A
//! user's own `GSK_RENDERER` skips the probe entirely.

use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::vulkan_probe::{self, Device, Failure, Kind};

/// The variable GTK reads. Named once so the doctor and the tests agree.
pub const ENV: &str = "GSK_RENDERER";

/// How long the probe may take. The doctor allows eight seconds because a
/// person is waiting on a report; here the answer is wanted before the engine
/// opens its window, which is a few seconds into a launch, and a driver that
/// has not answered by then is not going to be treated as a reason to change
/// anything (see [`Decision::KeepUnknown`]).
pub const PROBE_LIMIT: Duration = Duration::from_secs(8);

/// What to do about GSK_RENDERER, and the words for why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The user set `GSK_RENDERER`. Never overridden, never probed.
    KeepUsers(String),
    /// A hardware Vulkan device exists, so GTK has a renderer that works.
    KeepHealthy,
    /// The probe did not answer (hung or crashed). Nothing is known about
    /// GTK, and changing a renderer on no evidence is the wrong direction to
    /// be wrong in.
    KeepUnknown(String),
    /// Set `GSK_RENDERER=cairo` because GTK has no hardware Vulkan to render
    /// with; carries what the probe found.
    Cairo(String),
    /// Set `GSK_RENDERER=cairo` because GTK's Vulkan renderer would run on
    /// NVIDIA's own driver beside the engine; carries the device.
    CairoBesideNvidia(String),
}

impl Decision {
    pub fn is_cairo(&self) -> bool {
        self.cairo().is_some()
    }

    /// Why cairo was chosen and what leaving GTK alone would have cost, for
    /// the client's log line and the doctor, which must not word it twice.
    pub fn cairo(&self) -> Option<(&str, &'static str)> {
        match self {
            Decision::Cairo(why) => Some((
                why,
                "GTK's GL renderer stops presenting beside the engine's GLES, and a focused text box would have \
                 no editor.",
            )),
            Decision::CairoBesideNvidia(why) => Some((
                why,
                "GTK's Vulkan renderer has crashed inside that driver while the engine draws through it too \
                 (docs/nvidia.md).",
            )),
            _ => None,
        }
    }
}

/// NVIDIA's own driver, as opposed to Mesa's NVK on the same card: NVK reports
/// the same vendor id and brings Mesa's window-system code, which is not what
/// the cores stop in. A driver that does not name itself is taken to be
/// NVIDIA's, because the wrong guess that way costs a title bar drawn in
/// software and the other way costs the client.
fn nvidias_own_driver(d: &Device) -> bool {
    crate::nvidia::is_nvidia(d.vendor_id)
        && d.kind != Kind::Cpu
        && d.driver_name.as_deref().is_none_or(|name| name.eq_ignore_ascii_case("NVIDIA"))
}

/// The decision, with both inputs passed in so it can be tested.
///
/// `user` is the value of `GSK_RENDERER` in the environment, if any. An empty
/// value is not an opinion: GTK reads it as unset.
pub fn decide(user: Option<&str>, probed: &Result<Vec<Device>, Failure>) -> Decision {
    if let Some(v) = user.map(str::trim).filter(|v| !v.is_empty()) {
        return Decision::KeepUsers(v.to_string());
    }
    match probed {
        // Any device, not the first: this is decided before GTK or the engine
        // has picked one, and a two-GPU laptop that ends up on the other chip
        // loses nothing by it that it would notice.
        Ok(devices) if devices.iter().any(nvidias_own_driver) => {
            let d = devices.iter().find(|d| nvidias_own_driver(d)).expect("just matched");
            // The driver's own spelling where it gives one: the packed number
            // loses the leading zero of 620.70.06.
            let version = d.driver_info.clone().unwrap_or_else(|| d.driver_version_text());
            Decision::CairoBesideNvidia(format!("{} is on NVIDIA's own driver ({version})", d.name))
        }
        Ok(devices) if devices.iter().any(|d| d.kind != Kind::Cpu) => Decision::KeepHealthy,
        Ok(devices) => {
            let names = devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ");
            Decision::Cairo(format!("the only Vulkan device is a CPU renderer ({names})"))
        }
        Err(Failure::NoLoader) => Decision::Cairo("there is no Vulkan loader".into()),
        Err(Failure::Instance(code)) => {
            Decision::Cairo(format!("Vulkan could not start (vkCreateInstance returned {code})"))
        }
        Err(Failure::NoDevices) => Decision::Cairo("Vulkan lists no GPU".into()),
        Err(Failure::TimedOut) => Decision::KeepUnknown("the Vulkan probe did not finish".into()),
        Err(Failure::Crashed(why)) => {
            Decision::KeepUnknown(format!("the Vulkan probe failed ({why})"))
        }
    }
}

/// The probe, started and not yet collected. `None` when [`begin`] was not
/// called, which is what a test binary or a client that never opens a GTK
/// window looks like, and [`settle`] then does nothing.
static PENDING: Mutex<Option<JoinHandle<Result<Vec<Device>, Failure>>>> = Mutex::new(None);

/// Start the probe in the background, unless the user already chose.
///
/// Called early in `cordial-run`, before the engine is loaded. A thread rather
/// than a wait: the child process is the isolation, and this only keeps the
/// launch from standing still for it.
pub fn begin() {
    if user_choice().is_some() {
        return;
    }
    let mut slot = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    if slot.is_some() {
        return;
    }
    *slot = Some(std::thread::spawn(|| vulkan_probe::probe(PROBE_LIMIT)));
}

fn user_choice() -> Option<String> {
    std::env::var(ENV).ok().filter(|v| !v.trim().is_empty())
}

/// Collect the probe and set `GSK_RENDERER` if it says to. Must run before
/// GTK creates the window's renderer, which happens when the toplevel is
/// realised, so `host_window::init_wayland` is the latest it can be called.
///
/// Prints one line saying what it decided and why, whichever way. `None` when
/// no probe was started, which includes the user having chosen already.
pub fn settle() -> Option<Decision> {
    let handle = PENDING.lock().unwrap_or_else(|p| p.into_inner()).take()?;
    let asked = std::time::Instant::now();
    let probed = handle
        .join()
        .unwrap_or_else(|_| Err(Failure::Crashed("the probe thread panicked".into())));
    // How long the launch stood still for the answer, which is nothing when
    // the probe finished while the engine was loading. In the line below so a
    // slow driver is a number and not an unexplained pause.
    let waited = asked.elapsed().as_millis();
    let user = user_choice();
    let decision = decide(user.as_deref(), &probed);
    match &decision {
        Decision::Cairo(_) | Decision::CairoBesideNvidia(_) => {
            let (why, cost) = decision.cairo().expect("both arms are cairo");
            // SAFETY: `g_setenv` is not thread-safe against a concurrent
            // `getenv`, and the engine's threads are already running here --
            // the same compromise `init_wayland` makes for `GDK_BACKEND`, and
            // for the same reason: nothing in `libroblox.so` reads this
            // variable, GTK reads it once at realisation, and this is the
            // last moment before that.
            match unsafe { gtk4::glib::setenv(ENV, "cairo", true) } {
                Ok(()) => println!(
                    "[gtk] renderer: cairo, because {why} (probe wait {waited} ms); {cost} Set {ENV} \
                     yourself to choose another."
                ),
                Err(e) => eprintln!("[gtk] could not set {ENV}=cairo: {e}"),
            }
        }
        Decision::KeepUnknown(why) => println!(
            "[gtk] renderer: left to GTK, because {why} (probe wait {waited} ms); if a focused \
             text box shows no editor, try {ENV}=cairo"
        ),
        Decision::KeepHealthy => println!(
            "[gtk] renderer: left to GTK, a hardware Vulkan device exists (probe wait {waited} ms)"
        ),
        // The user's own choice is never probed, so `settle` is not reached
        // with this today; kept total so a change to `begin` cannot make it a
        // silent override.
        Decision::KeepUsers(v) => println!("[gtk] renderer: {ENV}={v}, set by the user"),
    }
    Some(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(kind: Kind, name: &str) -> Device {
        Device {
            vendor_id: 0x1002,
            device_id: 1,
            kind,
            name: name.into(),
            driver_name: None,
            driver_info: None,
            driver_version: 0,
            api_version: 0,
        }
    }

    #[test]
    fn a_hardware_device_leaves_gtk_alone() {
        for kind in [Kind::Integrated, Kind::Discrete, Kind::Virtual, Kind::Other] {
            let probed = Ok(vec![device(kind, "gpu")]);
            assert_eq!(decide(None, &probed), Decision::KeepHealthy, "{kind:?}");
        }
    }

    #[test]
    fn a_cpu_renderer_beside_a_real_gpu_is_still_healthy() {
        let probed = Ok(vec![device(Kind::Cpu, "llvmpipe"), device(Kind::Discrete, "RTX")]);
        assert_eq!(decide(None, &probed), Decision::KeepHealthy);
    }

    /// What the probe printed on the machine the cores came from.
    fn nvidia(driver_name: Option<&str>) -> Device {
        Device {
            vendor_id: 0x10de,
            device_id: 0x2203,
            name: "NVIDIA GeForce RTX 3090 Ti".into(),
            driver_name: driver_name.map(str::to_string),
            driver_info: driver_name.map(|_| "620.70.06".to_string()),
            driver_version: (620 << 22) | (70 << 14) | (6 << 6),
            ..device(Kind::Discrete, "")
        }
    }

    #[test]
    fn nvidias_own_driver_is_cairo_and_says_which() {
        // A driver that says nothing about itself still has a packed version.
        for (name, version) in [(Some("NVIDIA"), "(620.70.06)"), (None, "(620.70.6)")] {
            match decide(None, &Ok(vec![nvidia(name)])) {
                Decision::CairoBesideNvidia(why) => {
                    assert!(why.contains("RTX 3090 Ti") && why.contains(version), "{why}")
                }
                other => panic!("{name:?} -> {other:?}"),
            }
        }
    }

    #[test]
    fn nvidias_own_driver_anywhere_in_the_list_is_cairo() {
        let probed = Ok(vec![device(Kind::Integrated, "Intel"), nvidia(Some("NVIDIA")), device(Kind::Cpu, "llvmpipe")]);
        assert!(matches!(decide(None, &probed), Decision::CairoBesideNvidia(_)));
    }

    #[test]
    fn mesa_on_an_nvidia_card_is_left_alone() {
        assert_eq!(decide(None, &Ok(vec![nvidia(Some("NVK"))])), Decision::KeepHealthy);
    }

    #[test]
    fn the_two_reasons_for_cairo_do_not_share_a_sentence() {
        let none = decide(None, &Err(Failure::NoLoader));
        let nv = decide(None, &Ok(vec![nvidia(Some("NVIDIA"))]));
        let (_, without_vulkan) = none.cairo().unwrap();
        let (_, beside_nvidia) = nv.cairo().unwrap();
        assert!(without_vulkan.contains("GLES") && !beside_nvidia.contains("GLES"));
        assert!(beside_nvidia.contains("docs/nvidia.md"));
        assert_eq!(decide(None, &Ok(vec![device(Kind::Discrete, "gpu")])).cairo(), None);
    }

    #[test]
    fn only_a_cpu_renderer_is_cairo() {
        let probed = Ok(vec![device(Kind::Cpu, "llvmpipe (LLVM 19.1.7, 256 bits)")]);
        match decide(None, &probed) {
            Decision::Cairo(why) => assert!(why.contains("llvmpipe"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_loader_no_driver_and_no_devices_are_each_cairo() {
        for failure in [Failure::NoLoader, Failure::Instance(-9), Failure::NoDevices] {
            let d = decide(None, &Err(failure.clone()));
            assert!(d.is_cairo(), "{failure:?} -> {d:?}");
        }
    }

    #[test]
    fn a_probe_that_did_not_answer_changes_nothing() {
        for failure in [Failure::TimedOut, Failure::Crashed("signal 11".into())] {
            let d = decide(None, &Err(failure.clone()));
            assert!(matches!(d, Decision::KeepUnknown(_)), "{failure:?} -> {d:?}");
        }
    }

    #[test]
    fn the_users_own_renderer_beats_every_probe_outcome() {
        let outcomes: Vec<Result<Vec<Device>, Failure>> = vec![
            Ok(vec![device(Kind::Discrete, "gpu")]),
            Ok(vec![device(Kind::Cpu, "llvmpipe")]),
            Ok(vec![nvidia(Some("NVIDIA"))]),
            Err(Failure::NoLoader),
            Err(Failure::Instance(-9)),
            Err(Failure::TimedOut),
        ];
        for probed in &outcomes {
            assert_eq!(decide(Some("ngl"), probed), Decision::KeepUsers("ngl".into()), "{probed:?}");
        }
    }

    #[test]
    fn an_empty_variable_is_not_an_opinion() {
        let probed = Err(Failure::NoLoader);
        assert!(decide(Some(""), &probed).is_cairo());
        assert!(decide(Some("  "), &probed).is_cairo());
    }
}
