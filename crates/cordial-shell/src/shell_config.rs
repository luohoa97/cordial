//! Cordial's own shell preferences.
//!
//! Distinct from `flags_file.rs`, which speaks to the engine, and from
//! `cordial_plugins::grants`, which speaks to what a plugin may do. This is
//! the shell's own state, and for now that is exactly one thing: which
//! appearance the user asked Cordial itself to use.
//!
//! `$XDG_CONFIG_HOME/cordial/shell.json`, falling back to `$HOME/.config` —
//! the same layout `cordial_plugins::grants::path` and
//! `cordial_plugins::manifest::plugin_root` use — and the same
//! default-on-anything-wrong behaviour as `grants::load`: a missing or
//! malformed file means "use the defaults", not "refuse to start". Nobody
//! but this shell ever writes this file, so a malformed one is far likelier
//! to be an interrupted write than anything adversarial, and refusing to
//! start over that would be a worse failure than quietly falling back.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How tall the game window's header bar should be.
///
/// Exists because 3d67e59 shrank it to a 30px min-height with 24px window
/// controls, against a libadwaita default nearer 47px, and the result was
/// reported as "the x in the title bar looks off and the titlebar looks short
/// now compared to earlier builds". The sizing went back to the platform's, and
/// this is the way to ask for the compact one on purpose.
///
/// **Two named sizes rather than a pixel value, deliberately.** A free number
/// produces chrome that matches nothing else on the desktop and, below about
/// 30px, clips the window controls -- which is exactly the "looks off" that
/// started this. Somebody who wants the game to fill the screen wants
/// fullscreen, which F11 already gives and which persists per profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleBar {
    /// Whatever the desktop's theme says. The default, and what every other
    /// window on the machine looks like.
    #[default]
    Default,
    /// A shorter bar with smaller window controls, for people who want the
    /// pixels back and accept chrome that does not match.
    Compact,
}

impl TitleBar {
    /// Order matches the `AdwComboRow` model in `settings.rs`.
    pub fn index(self) -> u32 {
        match self {
            TitleBar::Default => 0,
            TitleBar::Compact => 1,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            1 => TitleBar::Compact,
            _ => TitleBar::Default,
        }
    }

    /// What the client is told. Absent means the platform default, so an older
    /// client that does not know this variable behaves as it always has.
    pub fn env_value(self) -> Option<&'static str> {
        match self {
            TitleBar::Default => None,
            TitleBar::Compact => Some("compact"),
        }
    }
}

/// What appearance Cordial itself should use.
///
/// Not a desktop-wide setting — `AdwStyleManager::set_color_scheme` applies
/// this to this application only. `System` means *follow*
/// `org.freedesktop.appearance color-scheme`, live, the way ADR-011 already
/// relies on for the canvas background; it must never mean *write* that
/// setting. Cordial has no business changing the desktop's theme to satisfy
/// its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceScheme {
    Light,
    Dark,
    System,
}

impl AppearanceScheme {
    /// Order matches the `AdwComboRow` model in `settings.rs` — `index` and
    /// `from_index` are the seam between the two, kept as plain position
    /// rather than a second name-keyed lookup that could drift from the
    /// model's actual contents.
    pub fn index(self) -> u32 {
        match self {
            AppearanceScheme::Light => 0,
            AppearanceScheme::Dark => 1,
            AppearanceScheme::System => 2,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            0 => AppearanceScheme::Light,
            1 => AppearanceScheme::Dark,
            _ => AppearanceScheme::System,
        }
    }

    /// Applies to this process only. `ColorScheme::Default` is what makes
    /// `System` live — libadwaita keeps tracking the portal itself once the
    /// override is lifted, nothing here has to poll or resubscribe.
    pub fn apply(self) {
        let scheme = match self {
            AppearanceScheme::Light => libadwaita::ColorScheme::ForceLight,
            AppearanceScheme::Dark => libadwaita::ColorScheme::ForceDark,
            AppearanceScheme::System => system_scheme(portal_colour_scheme()),
        };
        libadwaita::StyleManager::default().set_color_scheme(scheme);
    }
}

impl Default for AppearanceScheme {
    fn default() -> Self {
        AppearanceScheme::System
    }
}

/// How long to wait for the settings portal before deciding nobody is there.
///
/// ADR-002 budgets the shell's first paint in milliseconds and this call sits in
/// front of it, so the number is a compromise rather than a safety margin: on a
/// session with a portal the reply comes back in about a millisecond and this is
/// never reached, and on one without it the bus refuses immediately rather than
/// hanging. What it actually bounds is the case in between — a portal that is
/// starting, or wedged — where half a second of default theming is a better
/// outcome than a launcher that appears to have failed to start.
const PORTAL_TIMEOUT_MS: i32 = 500;

/// What `System` has to resolve to right now, given what the desktop said.
///
/// **A preference, not a correctness fix, and the branch looks wrong until you
/// know why.** `ColorScheme::Default` is the value that means "follow the
/// desktop", and it is what `System` should be whenever the desktop can be
/// asked — someone on a light desktop must still get light, and `Default` is
/// also what keeps the window tracking a change made while it is open, which is
/// worth more than any startup decision taken once.
///
/// The single source libadwaita consults for that is the settings portal's
/// `org.freedesktop.appearance color-scheme`. When nothing answers it, there is
/// no preference to follow — but `Default` does not mean "unknown", it renders
/// light, so an unreachable portal presents as a deliberate light theme. That is
/// how the owner's launcher kept appearing in light on a `prefer-dark` desktop:
/// a process without the session bus in its environment has no portal to ask,
/// and light is what falls out. A game launcher guessing light when it has not
/// been told is the worse guess, so the unknown case is dark.
///
/// The cost is stated rather than hidden: if the portal is unreachable *and*
/// libadwaita's non-sandboxed GSettings fallback would have found a genuine
/// light preference, this overrides it. That is accepted — the owner asked for
/// dark as the answer to "we do not know", and the portal is what defines
/// knowing here.
fn system_scheme(portal: Option<u32>) -> libadwaita::ColorScheme {
    match portal {
        Some(_) => libadwaita::ColorScheme::Default,
        None => libadwaita::ColorScheme::ForceDark,
    }
}

/// The desktop's `org.freedesktop.appearance color-scheme`, asked for directly.
///
/// Only the *presence* of an answer is used — see [`system_scheme`] — so this
/// deliberately does not interpret the value it returns. `ReadOne` first because
/// it is what current portals implement, then `Read`, which older ones offer and
/// which boxes the value one variant deeper; either shape is unwrapped by
/// following `v` down until something that is not a variant comes out.
///
/// Through `gio` rather than `zbus`. `zbus` is a dependency of `cordial-runtime`
/// and not of this crate, and the shell is already holding a GDBus connection
/// through GTK — adding an async runtime to this crate to ask one question the
/// toolkit can already ask would be the larger change.
fn portal_colour_scheme() -> Option<u32> {
    use libadwaita::gtk::gio;
    use libadwaita::gtk::glib::prelude::*;

    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let arguments = ("org.freedesktop.appearance", "color-scheme").to_variant();

    for method in ["ReadOne", "Read"] {
        let Ok(reply) = bus.call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            method,
            Some(&arguments),
            None,
            gio::DBusCallFlags::NONE,
            PORTAL_TIMEOUT_MS,
            gio::Cancellable::NONE,
        ) else {
            continue;
        };
        let mut value = reply.child_value(0);
        while let Some(inner) = value.as_variant() {
            value = inner;
        }
        if let Some(scheme) = value.get::<u32>() {
            return Some(scheme);
        }
    }
    None
}

/// When Cordial stops holding the engine at full rate.
///
/// **The engine has an idle throttle of its own and this decides when to stop
/// defeating it.** `input::idle_keepalive` sends `nativePassMouseMove` on every
/// pump tick while a key is held, because without it the engine collapses from
/// about sixty presents a second to exactly one about thirteen seconds after
/// the last mouse movement — a player walking in a straight line gets throttled
/// mid-play. That workaround used to run unconditionally, so Cordial held the
/// engine at full rate in the background on purpose.
///
/// **This governs Cordial's own keepalive and nothing else.**
/// `onWindowFocusChangedNative` is reported to the engine truthfully on every
/// real transition whatever this is set to; answering a platform question
/// honestly is not a setting. On [`ThrottleWhen::Off`] the engine still knows
/// it is unfocused and Cordial simply carries on driving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThrottleWhen {
    /// Throttle only when the window is not visible — minimised, on another
    /// workspace, or fully covered.
    ///
    /// The default, and deliberately not `Unfocused`. A second monitor with
    /// Roblox running on it while the user types in Discord on the first is an
    /// unfocused window somebody is watching, and throttling it would be worse
    /// than the problem this setting exists to fix; so is alt-tabbing away from
    /// a load screen, which is when a stream most wants the frames. Visibility
    /// is the question actually being asked and Wayland answers it directly —
    /// `xdg_toplevel`'s `suspended` state, which reaches this process as
    /// `GdkToplevelState::SUSPENDED`.
    Visible,
    /// Throttle whenever the window loses focus. The most saving, and wrong for
    /// anyone who watches Cordial while working in another window.
    Unfocused,
    /// Never throttle. Full rate in the background, which is what a recording
    /// or a long download inside the client wants.
    Off,
}

impl Default for ThrottleWhen {
    fn default() -> Self {
        ThrottleWhen::Visible
    }
}

impl ThrottleWhen {
    /// Order matches the `AdwComboRow` model in `settings.rs`, on the same
    /// footing as [`AppearanceScheme::index`] — see that comment for why the
    /// seam is a position rather than a name.
    pub fn index(self) -> u32 {
        match self {
            ThrottleWhen::Visible => 0,
            ThrottleWhen::Unfocused => 1,
            ThrottleWhen::Off => 2,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            0 => ThrottleWhen::Visible,
            1 => ThrottleWhen::Unfocused,
            _ => ThrottleWhen::Off,
        }
    }

    /// The word the client parses out of `CORDIAL_THROTTLE`. Passed through the
    /// environment rather than read from `shell.json` for the same reason
    /// `graphics` is: the client is a separate process with its own idea of
    /// where configuration lives, and the launch is the one place that already
    /// knows both.
    pub fn as_str(self) -> &'static str {
        match self {
            ThrottleWhen::Visible => "visible",
            ThrottleWhen::Unfocused => "unfocused",
            ThrottleWhen::Off => "off",
        }
    }
}

/// Whether the desktop's own pointer acceleration reaches the camera.
///
/// `zwp_relative_pointer_v1` delivers two deltas per event: one the
/// compositor has run through the desktop's pointer profile, and one it has
/// not. While Roblox holds the cursor -- first person, shift lock, right-drag
/// -- Cordial chooses between them, and unaccelerated is right for a camera:
/// acceleration is superlinear in speed, so a fast sweep turns further than a
/// slow one covering the same distance, and in-game sensitivity would
/// otherwise follow whatever pointer speed the desktop happens to be set to.
///
/// **There is still no `Never`, but not for the reason this comment used to
/// give.** It used to say that, with the cursor unlocked, Cordial was handed
/// an absolute position the compositor had already accelerated and so had no
/// unaccelerated absolute to fall back to -- making the desktop's setting
/// apply outside the lock whether Cordial liked it or not, and a "never" a
/// switch that would silently do nothing. That stopped being true on
/// 2026-08-28: reported as "it's set on only the cursor, it should work and
/// accelerate in roblox ui. It doesn't", `relative_pointer_motion` now feeds
/// the unlocked cursor from `zwp_relative_pointer_v1`'s own accelerated pair
/// rather than from the arithmetic difference of two absolute positions --
/// see that function and `input.rs`'s `resolve_mouse_delta`. An unaccelerated
/// *cursor* is therefore possible now, the same way the camera's is: the
/// unaccelerated pair is sitting right there in the same event. It is
/// deliberately not offered here anyway: nobody asked for a cursor that
/// ignores the desktop's pointer profile, and the report this enum exists to
/// answer was the opposite complaint. Adding it would be a third menu entry
/// with no user behind it -- if that changes, `PointerAcceleration` is a
/// two-variant enum and `NeverCursor` is a small addition, not a redesign.
///
/// Keyed on the pointer lock rather than on "first person" because first
/// person is engine state and Cordial cannot see it -- Roblox exposes no
/// accessibility tree, and reading it any other way is out of scope under
/// ADR-001. The lock is Cordial's own, and Roblox takes it for exactly the
/// three camera cases that want raw movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointerAcceleration {
    /// The desktop's setting moves the cursor, and camera movement is raw.
    /// The default, and the only honest description of the status quo.
    UnlockedCursor,
    /// The desktop's setting moves the camera too, for anyone who has tuned
    /// their pointer profile and wants the client to obey it.
    Always,
}

impl Default for PointerAcceleration {
    fn default() -> Self {
        PointerAcceleration::UnlockedCursor
    }
}

impl PointerAcceleration {
    /// Order matches the `AdwComboRow` model in `settings.rs`, as
    /// [`ThrottleWhen::index`] does.
    pub fn index(self) -> u32 {
        match self {
            PointerAcceleration::UnlockedCursor => 0,
            PointerAcceleration::Always => 1,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            0 => PointerAcceleration::UnlockedCursor,
            _ => PointerAcceleration::Always,
        }
    }

    /// The word the client parses out of `CORDIAL_POINTER_ACCEL`.
    pub fn as_str(self) -> &'static str {
        match self {
            PointerAcceleration::UnlockedCursor => "unlocked",
            PointerAcceleration::Always => "always",
        }
    }
}

/// Which Vulkan present mode the client asks the driver for.
///
/// **It is a latency setting and a power setting at the same time, and those
/// pull opposite ways.** FIFO queues one image per display refresh, so the GPU
/// renders exactly the frames that get shown and wastes nothing -- and the
/// cursor and camera lag the hand by however deep that queue is. MAILBOX has no
/// queue to wait behind, it replaces the pending image, so it is the
/// responsive one and it burns power drawing frames the display never scans
/// out. IMMEDIATE does not synchronise at all: the lowest latency there is, and
/// the one that tears.
///
/// **MAILBOX is the default because the latency was measured and the power was
/// not.** This shipped as FIFO for about an hour on the power argument, and the
/// report came straight back -- "the mouse feels floaty and weird in roblox",
/// then the control run, "switching back to Mailbox fixes the floaty fealing".
/// The power cost of MAILBOX is real and nobody here has a watt meter; the
/// latency cost of FIFO is something a person felt within minutes. FIFO is one
/// row away for anyone who would rather pay it.
///
/// FIFO is also the only mode `VkSurfaceKHR` guarantees -- the other two may
/// simply not be advertised, in which case
/// `cordial_runtime::android::vulkan` leaves the engine's own choice alone
/// rather than substituting something nobody asked for.
///
/// **[`PresentMode::Automatic`] is not a fourth mode, it is the absence of an
/// opinion**, and it is here for the same reason `graphics`'s "automatic" is:
/// an absent `CORDIAL_PRESENT_MODE` is the one state in which a plugin's
/// `CordialPresentMode` entry counts (ADR-007, ADR-020). Without it, shipping
/// this row would have quietly made a documented plugin capability
/// unreachable for everybody, which is the kind of silent contradiction
/// AGENTS.md asks to be argued in an ADR rather than introduced in a widget.
/// Choosing Automatic still lands on FIFO when no plugin says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresentMode {
    /// One image per refresh: no tearing, no wasted frames, and the latency of
    /// a queue. The power-saving choice, and not the default -- see above.
    Fifo,
    /// No queue, no tearing, where the driver advertises it. The default:
    /// responsive, and it costs power.
    Mailbox,
    /// Uncapped, unsynchronised, tears. The lowest latency there is.
    Immediate,
    /// No opinion, so a plugin may have one. Falls back to FIFO.
    Automatic,
}

impl Default for PresentMode {
    fn default() -> Self {
        PresentMode::Mailbox
    }
}

impl PresentMode {
    /// Order matches the `AdwComboRow` model in `settings.rs`, as
    /// [`ThrottleWhen::index`] does.
    /// Mailbox is 0 because it is the default and the row lists it first.
    pub fn index(self) -> u32 {
        match self {
            PresentMode::Mailbox => 0,
            PresentMode::Fifo => 1,
            PresentMode::Immediate => 2,
            PresentMode::Automatic => 3,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            0 => PresentMode::Mailbox,
            1 => PresentMode::Fifo,
            2 => PresentMode::Immediate,
            _ => PresentMode::Automatic,
        }
    }

    /// The word `cordial_runtime::android::vulkan::parse_present_mode` takes
    /// out of `CORDIAL_PRESENT_MODE`, or `None` for Automatic.
    ///
    /// `None` rather than the string "auto" because the two are not the same
    /// thing to the runtime's precedence rules -- an absent variable and an
    /// explicit `auto` both let a plugin through, but only an absent one keeps
    /// the launcher out of a decision it was not asked to make. Sending
    /// nothing is the smaller claim.
    pub fn as_env(self) -> Option<&'static str> {
        match self {
            PresentMode::Fifo => Some("fifo"),
            PresentMode::Mailbox => Some("mailbox"),
            PresentMode::Immediate => Some("immediate"),
            PresentMode::Automatic => None,
        }
    }
}

/// Which PipeWire sink Roblox's audio goes to, by stable `node.name`.
///
/// Empty — the default — means *follow the system default sink*, and it has to
/// keep meaning that rather than being resolved to a name once. A stream with
/// no `PW_KEY_TARGET_OBJECT` is moved by PipeWire when the default changes, so
/// storing today's default here would quietly pin somebody to the speakers
/// they happened to be using the day they opened settings, and they would find
/// out by plugging in a headset that no longer worked.
///
/// **`node.name`, not an index and not a description.** A PipeWire global id
/// renumbers across an unplug/replug, so a stored index eventually names a
/// different device; `node.description` is localised and is what a user's
/// volume control renames when they rename a device. `node.name` is the
/// routing target PipeWire itself takes and is the only one of the three meant
/// to be persisted.
///
/// **Global rather than per profile, deliberately, and it is worth saying
/// which side of ADR-013's line this falls.** That ADR splits configuration
/// from code by asking whether a thing belongs to an account or to the
/// machine: grants and flags moved into the profile because an approval given
/// on a throwaway account was silently in force on the one somebody plays. An
/// audio device is neither an approval nor an identity — it is the hardware in
/// front of the person sitting there, on the same footing as `graphics` and
/// `roblox` above. Switching profiles must not move the sound to a different
/// speaker.
///
/// **This is Cordial's choice of sink, not Roblox's device picker.** Roblox
/// does have one — `FmodAudioDevice::setOutputDevice`, `GetOutputDevices` — but
/// it is populated by FMOD's own output backend, which sees a single device on
/// every path Cordial provides, and the AAudio path has no
/// `AAudioStreamBuilder_setDeviceId` among the 25 symbols the engine looks up.
/// So the in-game list cannot be filled from here; see
/// `docs/analysis/aaudio-contract.md`. What this does instead is decide where
/// the one stream Roblox opens actually lands, which is what somebody asking
/// to "choose my audio device" wants either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct AudioOutput(pub String);

impl AudioOutput {
    /// Nothing chosen: follow whatever the session calls the default sink.
    pub fn is_system_default(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// The value for `CORDIAL_AUDIO_SINK`, or `None` when the variable should
    /// not be set at all.
    ///
    /// Absent and empty mean the same thing to the client — see
    /// `configured_output_device()` in `native/pipewire_backend.cpp`, which
    /// treats `CORDIAL_AUDIO_SINK=` as unset for exactly this reason — but
    /// omitting it is the honest encoding of "the user expressed no opinion",
    /// on the same argument `launch.rs` makes for leaving `CORDIAL_GRAPHICS`
    /// out when the Renderer row says Automatic.
    pub fn env_value(&self) -> Option<&str> {
        if self.is_system_default() { None } else { Some(self.0.trim()) }
    }

    /// Which row of a picker listing `sinks` after a leading "System default"
    /// entry this selects.
    ///
    /// Zero when nothing is chosen, and **zero when the chosen sink is not in
    /// the list**, which is the case worth being careful about: a device that
    /// has been unplugged since the choice was made must not silently read as
    /// "System default" in a window the user is about to press a button in.
    /// `settings.rs` handles it by adding the missing device to the list
    /// before calling this, so that the row shows the choice and says it is
    /// not connected; this function's job is only to be correct about a name
    /// that genuinely is not there.
    pub fn index_in(&self, sinks: &[String]) -> u32 {
        if self.is_system_default() {
            return 0;
        }
        sinks
            .iter()
            .position(|name| name == self.0.trim())
            .map(|i| i as u32 + 1)
            .unwrap_or(0)
    }

    /// The inverse: what row `index` of that same picker means.
    pub fn from_index(index: u32, sinks: &[String]) -> Self {
        if index == 0 {
            return AudioOutput::default();
        }
        match sinks.get(index as usize - 1) {
            Some(name) => AudioOutput(name.clone()),
            // Out of range can only happen if the model changed under the
            // row. Falling back to the system default loses the choice, which
            // is the recoverable direction; storing an index-shaped guess
            // would send audio to an arbitrary device.
            None => AudioOutput::default(),
        }
    }
}

/// One choice that settles both of the things Cordial can ask the engine to do
/// differently about graphics: which device it says it is, and how many of the
/// machine's cores the engine's own worker pools may use.
///
/// **Why one setting and not two.** They are genuinely two parameters, and a
/// cross product of them would be six rows describing combinations nobody has
/// measured. But they are the same question to the person asking it — "make
/// this run better" — and a Settings page with two graphics dropdowns whose
/// interaction is undocumented is worse than one list of named intents. So the
/// list below is intents, each naming exactly what it sets, and the
/// combinations that make no sense are simply not offered.
///
/// **What the device identity is and is not.** It decides the `User-Agent` the
/// engine sends and `InitParams.isTablet` — see `native/init_params.cpp`'s
/// `device_identity`, which carries the measurement of what roblox.com serves
/// for each. It is **not** established that Roblox tiers graphics defaults off
/// any of it; `isTablet` is the only field here the engine has been seen to
/// read, and only [`GraphicsOptimization::MobileTier`] sets it. Anyone
/// choosing a mode expecting a frame rate to move should measure it, and the
/// row's subtitle says so rather than implying an effect nothing has shown.
///
/// **The CPU modes are unmeasured too**, and that is `cordial_runtime::flags`'s
/// own admission about `Performance`: its tables are adapted from mocktail's
/// policy and nothing on this project's hardware has compared them. They are
/// offered because an inference belongs behind a switch somebody chooses, which
/// is exactly the argument `flags::BUILTIN`'s comment makes at length — and for
/// the same reason the default sets neither of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum GraphicsOptimization {
    /// The client's own defaults: the `pc-windows-11` identity, and the
    /// engine's thread sizing left alone. What Cordial has shipped since
    /// 2026-08-20.
    ///
    /// **Still the default, deliberately.** A tablet identity was tried by a
    /// user and reported breaking PC features, and nothing has measured what
    /// the bare app token does to a frame rate either. The position when
    /// nothing has been measured is that the working default keeps working
    /// and the alternatives become choices.
    #[default]
    Balanced,
    /// `roblox-app`: the bare `RobloxApp/<version>(GlobalDist; Cordial)` token
    /// and nothing else — no platform words, no device block.
    ///
    /// The only identity that claims no form factor at all, which is the one
    /// Cordial can make honestly. Choosing it is safe for the in-experience
    /// web view: roblox.com serves the embedded app layout for this token as
    /// readily as for the other two, measured in
    /// `native/init_params.cpp`'s `device_identity`.
    RobloxApp,
    /// Claim an Android tablet, which is the only thing Cordial can say that
    /// asserts a mobile form factor to the engine (`InitParams.isTablet`).
    ///
    /// **Reported to break PC features** by the user who tried it. Offered
    /// because it is the only route to mobile-tier defaults if they exist,
    /// not because it is recommended.
    MobileTier,
    /// The default `pc-windows-11` identity, plus
    /// [`cordial_runtime::flags::Performance::Throughput`]:
    /// more engine worker threads and parallel prerender.
    MoreCores,
    /// The default `pc-windows-11` identity, plus
    /// [`cordial_runtime::flags::Performance::Latency`]:
    /// fewer threads and a smaller physics batch, for a machine short of cores.
    FewerCores,
}

impl GraphicsOptimization {
    /// Order matches the `AdwComboRow` model in `settings.rs`, on the same
    /// footing as [`ThrottleWhen::index`].
    pub fn index(self) -> u32 {
        match self {
            GraphicsOptimization::Balanced => 0,
            GraphicsOptimization::RobloxApp => 1,
            GraphicsOptimization::MobileTier => 2,
            GraphicsOptimization::MoreCores => 3,
            GraphicsOptimization::FewerCores => 4,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            0 => GraphicsOptimization::Balanced,
            1 => GraphicsOptimization::RobloxApp,
            2 => GraphicsOptimization::MobileTier,
            3 => GraphicsOptimization::MoreCores,
            _ => GraphicsOptimization::FewerCores,
        }
    }

    /// The `CORDIAL_DEVICE_PROFILE` value this mode wants, or `None` when it
    /// wants the client's own default.
    ///
    /// `None` rather than `Some("roblox-app")` for the default, on exactly the
    /// argument `launch.rs` makes about `CORDIAL_GRAPHICS`: an absent variable
    /// is what tells the runtime the user expressed no opinion, which is the
    /// one state in which a plugin's `CordialDeviceProfile` entry is allowed
    /// to count. Sending the default explicitly would be the user silently
    /// outvoting every plugin while the row says the default.
    pub fn device_profile_env(self) -> Option<&'static str> {
        match self {
            GraphicsOptimization::Balanced
            | GraphicsOptimization::MoreCores
            | GraphicsOptimization::FewerCores => None,
            GraphicsOptimization::RobloxApp => Some("roblox-app"),
            GraphicsOptimization::MobileTier => Some("android-tablet"),
        }
    }

    /// The `CORDIAL_PERFORMANCE` value this mode wants, or `None` for the
    /// client's own default. Same reasoning as [`Self::device_profile_env`].
    pub fn performance_env(self) -> Option<&'static str> {
        match self {
            GraphicsOptimization::Balanced
            | GraphicsOptimization::RobloxApp
            | GraphicsOptimization::MobileTier => None,
            GraphicsOptimization::MoreCores => Some("throughput"),
            GraphicsOptimization::FewerCores => Some("latency"),
        }
    }
}

/// The profile a launch runs against when nobody has chosen otherwise.
///
/// ADR-012's migration lands the pre-existing storage at `profiles/default`, so
/// this name is not arbitrary — picking anything else would present as being
/// logged out.
pub const DEFAULT_PROFILE: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    pub appearance: AppearanceScheme,
    /// How tall the game window's header bar is. See [`TitleBar`].
    pub title_bar: TitleBar,
    /// Where the Roblox build is, when the user has pinned it. Empty is the
    /// normal state and means "look every time" — see `install::locate`, which
    /// explains why a remembered answer is the wrong thing to store here.
    pub roblox: crate::install::RobloxInstall,
    /// Which profile an instance started from this shell runs. ADR-012: a
    /// profile is storage, an instance is a window, and one profile is held by
    /// at most one instance.
    pub profile: String,
    /// Ask Feral GameMode to raise the CPU governor, the process priority and
    /// the GPU's performance profile while the client runs, and to hold the
    /// screensaver off.
    ///
    /// Default on, which is what Sober does and what makes it worth having: a
    /// performance setting nobody finds is a performance setting nobody gets.
    /// It costs nothing on a machine without gamemoded — the request is a D-Bus
    /// call that fails, the client says so once and carries on — so there is no
    /// population this default hurts. `false` here becomes `CORDIAL_GAMEMODE=0`
    /// on the client, which is also the control for measuring what it does.
    pub gamemode: bool,
    /// When Cordial stops defeating the engine's idle throttle. See
    /// [`ThrottleWhen`], which carries the whole of the reasoning.
    #[serde(default)]
    pub throttle: ThrottleWhen,
    /// Whether the desktop's pointer acceleration reaches the camera. See
    /// [`PointerAcceleration`], which carries the reasoning and the reason
    /// there is no "never".
    #[serde(default)]
    pub pointer_acceleration: PointerAcceleration,
    /// What Cordial does about a new Roblox build without being asked, and over
    /// which connections it may fetch one.
    ///
    /// Two fields rather than one `UpdateSettings`, because they are two rows in
    /// two places: the dropdown and the pair of switches sit in the same group
    /// but nothing else in this file nests, and a settings document is read by
    /// people as often as by serde. `updater::update_settings` puts them back
    /// together for `cordial_update::settings::UpdateSettings::plan`, which is
    /// the only thing that wants them as a pair.
    ///
    /// Neither governs anything today and the settings page says so: Roblox
    /// publishes no Android build to download, so there is nothing for the plan
    /// to act on. They are stored anyway, because the choice is the user's to
    /// make before the day it matters rather than after it.
    pub automatic_updates: cordial_update::settings::Automatic,
    pub download_on: cordial_update::settings::DownloadOn,
    /// Show MangoHUD's frame rate and frame time overlay over the client.
    ///
    /// Default off, unlike `gamemode`, and for a reason that is not timidity:
    /// this one is visible. It draws over the game whether or not the user
    /// wanted it there, so it has to be asked for. It is also the setting most
    /// likely to be switched on by somebody who has not got MangoHUD installed
    /// — see `launch::mangohud_layer`, which is what stops that being a silent
    /// no-op.
    /// Which graphics backend the client offers the engine.
    ///
    /// Stored as the same lowercase words `cordial_runtime::graphics::Backend`
    /// parses, and passed to the client as `CORDIAL_GRAPHICS` rather than
    /// written to a file: the backend has to be settled before the engine's
    /// first `dlopen`, which is long before anything opens a profile.
    ///
    /// `"automatic"` is the default and is not merely "Vulkan by another name" —
    /// it is the absence of a user opinion, which is what lets a plugin have
    /// one. See `graphics::resolve`.
    pub graphics: String,
    /// Which device Cordial says it is, and how hard the engine may push the
    /// machine's cores. See [`GraphicsOptimization`], which carries the whole
    /// of the reasoning and the reason the default sets neither.
    ///
    /// Separate from `graphics` above and deliberately so: that row picks
    /// which renderer Cordial offers the engine, which is a question about
    /// this machine's drivers. This one is about what Cordial claims to be and
    /// how many threads it asks for, and the two do not interact.
    #[serde(default)]
    pub graphics_optimization_mode: GraphicsOptimization,
    /// Which present mode the client asks the driver for. See [`PresentMode`],
    /// which carries the reasoning and the reason FIFO is the default.
    ///
    /// `#[serde(default)]`, so a `shell.json` written by an older Cordial --
    /// which had no such key at all -- loads rather than failing to parse, and
    /// reads as MAILBOX, which is what those builds were already doing. Nobody
    /// upgrading gets a different feel than they had.
    #[serde(default)]
    pub present_mode: PresentMode,
    /// Whether Cordial reads `/dev/input/js*` and tells Roblox about pads.
    ///
    /// On by default. Off is a real setting rather than a debugging knob:
    /// gamepad support ships with `gamepadType` still unestablished, so the
    /// glyphs Roblox draws may name the wrong brand (Sober #584, #1810), and
    /// somebody who would rather have no controller than the wrong buttons
    /// drawn should not have to find an environment variable to say so.
    ///
    /// It is also the escape hatch for a device that misbehaves. joydev binds
    /// to anything advertising ABS_X/ABS_Y, and while
    /// `gamepad::is_a_controller` now rejects the ones that are plainly not
    /// pads -- a virtual mouse became `/dev/input/js0` on the machine this was
    /// written on -- a filter that reads capabilities cannot anticipate every
    /// device, and the cost of it being wrong is Roblox believing a controller
    /// is plugged in.
    ///
    /// **No `#[serde(default)]` on this field, deliberately.** The container
    /// carries one, which fills a missing key from `ShellConfig::default()` --
    /// `true`, which is what builds before this key existed did. A field-level
    /// attribute would override that with `bool::default()`, silently reading
    /// as controllers-off for every existing install. The first draft had it
    /// and the test below caught it, which is why both the attribute's absence
    /// and the reason are written down.
    pub gamepad: bool,
    /// Quit the client when the user leaves a game and returns to the home
    /// screen.
    ///
    /// Off by default, and it has to be: closing somebody's session is the
    /// least reversible thing Cordial does on its own initiative, and a person
    /// who did not ask for it meets it once and loses whatever they were doing
    /// next. Whoever wants it wants it deliberately -- they launched from a
    /// deep link to play one game and have no use for the home screen.
    ///
    /// Keyed on the engine's own `leaveUGCGameInternal` and not on a
    /// disconnect; see `cordial_runtime::game_log`, which has the capture and
    /// the reason those are different questions.
    pub close_on_leave: bool,
    pub mangohud: bool,
    /// Which audio device Roblox plays through. See [`AudioOutput`], which
    /// carries the whole of the reasoning, including why the stored form is a
    /// `node.name` and why the default must stay "follow the system".
    #[serde(default)]
    pub audio_output: AudioOutput,
    /// The accelerator that toggles fullscreen, in GTK's own syntax.
    ///
    /// Configurable rather than hardcoded because F11 is not reachable on every
    /// keyboard. A laptop whose function row defaults to media keys needs Fn held
    /// to produce F11 at all, and on some of those the keypress never reaches the
    /// application — so a client that only listens for F11 cannot be
    /// fullscreened on that machine by any amount of pressing.
    ///
    /// GTK binds nothing here by default, deliberately: it offers
    /// `gtk_window_fullscreen()` and leaves the key to the application, because
    /// F11 means other things elsewhere. Apps that appear to have it "for free"
    /// — Nautilus, Eye of GNOME — each bound it themselves.
    ///
    /// GNOME does carry a compositor-level `toggle-fullscreen` in
    /// `org.gnome.desktop.wm.keybindings`, and ships it **unbound**. Setting it
    /// there works for every window and is the better answer for somebody who
    /// wants one key across their whole desktop; this setting is for the window
    /// rather than the desktop, and the two do not conflict.
    ///
    /// Empty disables the binding entirely, for exactly that case.
    #[serde(default = "default_fullscreen_accel")]
    pub fullscreen_accel: String,
    /// A directory the Marketplace section of the Plugins page reads as a
    /// [`cordial_plugins::source::LocalFileSource`] — `index.json`, an
    /// optional `index.json.minisig`, and an `archives/` directory beside it.
    ///
    /// Never set by anything but the user, and never defaulted to a real
    /// path: ADR-014 declines to name who hosts an index, so there is no
    /// index for Cordial to point at until somebody supplies a directory of
    /// their own. Machine-wide rather than per profile, on the same footing
    /// as `roblox` above — which build to run, and which index to browse, are
    /// both about the machine's software, not about an account (ADR-013).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace_index_dir: Option<PathBuf>,
    /// The base64 minisign public key the Marketplace section checks
    /// `marketplace_index_dir`'s signature against.
    ///
    /// Absent by default and not filled in with anything Cordial ships,
    /// because Cordial ships no key — see `cordial_plugins::sign` for why. An
    /// index opened with this unset still lists what it offers; installing
    /// from it is refused until a key is set here and actually verifies,
    /// which is `cordial_plugins::marketplace::install`'s doing, not this
    /// field's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace_public_key: Option<String>,
}

fn default_fullscreen_accel() -> String {
    "F11".to_string()
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            appearance: AppearanceScheme::default(),
            title_bar: TitleBar::default(),
            roblox: crate::install::RobloxInstall::default(),
            profile: DEFAULT_PROFILE.to_string(),
            automatic_updates: cordial_update::settings::Automatic::default(),
            download_on: cordial_update::settings::DownloadOn::default(),
            gamemode: true,
            throttle: ThrottleWhen::default(),
            pointer_acceleration: PointerAcceleration::default(),
            graphics: "automatic".to_string(),
            graphics_optimization_mode: GraphicsOptimization::default(),
            present_mode: PresentMode::default(),
            gamepad: true,
            close_on_leave: false,
            audio_output: AudioOutput::default(),
            mangohud: false,
            fullscreen_accel: default_fullscreen_accel(),
            marketplace_index_dir: None,
            marketplace_public_key: None,
        }
    }
}

/// `CORDIAL_SHELL_CONFIG` overrides the path outright, the same override
/// pattern `cordial_plugins::grants::path` and `manifest::plugin_root` use —
/// useful for tests and for running more than one Cordial config side by
/// side without them fighting over the same file.
pub fn path() -> PathBuf {
    std::env::var_os("CORDIAL_SHELL_CONFIG").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(std::env::temp_dir)
            .join("cordial/shell.json")
    })
}

/// Load the config, or the defaults. A missing file is the ordinary case —
/// most people never open settings — and a malformed one is reported and
/// treated the same as missing, per the module docs above.
pub fn load(path: &Path) -> ShellConfig {
    let Ok(text) = std::fs::read_to_string(path) else {
        return ShellConfig::default();
    };
    match serde_json::from_str(&text) {
        Ok(config) => config,
        Err(e) => {
            println!("  shell: {} is not usable ({e}); using defaults", path.display());
            ShellConfig::default()
        }
    }
}

pub fn save(path: &Path, config: &ShellConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config).expect("ShellConfig always serialises");
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("cordial-shell-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn the_throttle_row_and_the_stored_word_agree() {
        // The `AdwComboRow` seam, and the word the client parses, checked
        // together — they are two encodings of the same three states and
        // nothing else would notice them drifting apart.
        for w in [ThrottleWhen::Visible, ThrottleWhen::Unfocused, ThrottleWhen::Off] {
            assert_eq!(ThrottleWhen::from_index(w.index()), w);
        }
        assert_eq!(ThrottleWhen::default(), ThrottleWhen::Visible);
        assert_eq!(ThrottleWhen::Visible.as_str(), "visible");
        assert_eq!(ThrottleWhen::Unfocused.as_str(), "unfocused");
        assert_eq!(ThrottleWhen::Off.as_str(), "off");
    }

    #[test]
    fn an_older_config_without_a_throttle_row_gets_the_visible_default() {
        // The row is new, so every existing shell.json lacks it. Landing on
        // anything but Visible would change behaviour for people who never
        // opened the setting.
        let p = scratch("no-throttle.json");
        std::fs::write(&p, br#"{"appearance":"dark","gamemode":true}"#).unwrap();
        assert_eq!(load(&p).throttle, ThrottleWhen::Visible);
    }

    #[test]
    fn a_missing_file_defaults_to_system() {
        let p = scratch("missing.json");
        let _ = std::fs::remove_file(&p);
        assert_eq!(load(&p).appearance, AppearanceScheme::System);
    }

    #[test]
    fn a_malformed_file_falls_back_to_defaults_rather_than_refusing_to_start() {
        let p = scratch("malformed.json");
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(load(&p).appearance, AppearanceScheme::System);
    }

    #[test]
    fn a_saved_choice_round_trips() {
        let p = scratch("roundtrip.json");
        save(&p, &ShellConfig { appearance: AppearanceScheme::Dark, ..Default::default() }).unwrap();
        assert_eq!(load(&p).appearance, AppearanceScheme::Dark);
    }

    #[test]
    fn a_config_written_before_the_roblox_fields_existed_still_loads() {
        // `#[serde(default)]` is what makes this true, and it is worth a test
        // rather than a note: everyone who has run this shell already has a
        // shell.json holding nothing but `appearance`, and a launcher that
        // refuses to start over a missing field it invented is a worse failure
        // than any of the ones it is meant to report.
        let p = scratch("older-schema.json");
        std::fs::write(&p, r#"{"appearance":"dark"}"#).unwrap();
        let config = load(&p);
        assert_eq!(config.appearance, AppearanceScheme::Dark);
        assert_eq!(config.profile, DEFAULT_PROFILE);
        assert_eq!(config.roblox, crate::install::RobloxInstall::default());
        // The performance fields are newer still, and the same argument
        // applies to them: everybody's shell.json predates them.
        assert!(config.gamemode, "an older config must still get GameMode's default");
        assert!(!config.mangohud);
    }

    #[test]
    fn the_update_settings_round_trip() {
        // Same shape as `a_saved_choice_round_trips`, for the same reason: a
        // control that accepts a choice and does not keep it is worse than one
        // that refuses, because the user finds out a launch later.
        use cordial_update::settings::{Automatic, DownloadOn};
        let p = scratch("updates.json");
        save(
            &p,
            &ShellConfig {
                automatic_updates: Automatic::Manual,
                download_on: DownloadOn { metered: true },
                ..Default::default()
            },
        )
        .unwrap();
        let back = load(&p);
        assert_eq!(back.automatic_updates, Automatic::Manual);
        assert!(back.download_on.metered);
    }

    #[test]
    fn a_config_written_before_the_update_settings_existed_gets_their_defaults() {
        // Everybody's shell.json predates these three controls, and a launcher
        // that refuses to start over a field it has just invented is a worse
        // failure than any it exists to report.
        use cordial_update::settings::Automatic;
        let p = scratch("pre-updates.json");
        std::fs::write(&p, r#"{"appearance":"dark","profile":"default"}"#).unwrap();
        let config = load(&p);
        assert_eq!(config.automatic_updates, Automatic::Background);
        assert!(!config.download_on.metered, "a data allowance is not the default to spend");
    }

    #[test]
    fn the_performance_switches_round_trip() {
        // Both directions, because both defaults are worth being able to
        // reverse and a setting that only saves the value it already had would
        // pass a one-way test.
        let p = scratch("performance.json");
        save(&p, &ShellConfig { gamemode: false, mangohud: true, ..Default::default() }).unwrap();
        let back = load(&p);
        assert!(!back.gamemode);
        assert!(back.mangohud);
    }

    #[test]
    fn the_roblox_paths_round_trip() {
        let p = scratch("roblox.json");
        let mut config = ShellConfig::default();
        config.roblox.apk = Some(PathBuf::from("/somewhere/base.apk"));
        config.roblox.lib_dir = Some(PathBuf::from("/somewhere/lib/x86_64"));
        config.profile = "alt_account".into();
        save(&p, &config).unwrap();
        let back = load(&p);
        assert_eq!(back.roblox.apk, config.roblox.apk);
        assert_eq!(back.roblox.lib_dir, config.roblox.lib_dir);
        assert_eq!(back.profile, "alt_account");
    }

    #[test]
    fn an_unanswered_portal_is_dark_rather_than_light() {
        // The owner's report, in one assertion: their launcher kept opening in
        // light on a `prefer-dark` desktop, because `ColorScheme::Default`
        // renders light when nothing told it otherwise and a process without
        // the session bus has nothing to ask. Unknown is dark now.
        assert_eq!(system_scheme(None), libadwaita::ColorScheme::ForceDark);
    }

    #[test]
    fn a_desktop_that_answers_is_still_followed_live() {
        // The half that must not be lost in fixing the other one. `Default` is
        // the only value that keeps tracking a change made while the window is
        // open, and forcing dark on an answering desktop would take light away
        // from somebody who chose it. The value itself is not inspected on
        // purpose, so every answer maps the same way.
        for reported in [0, 1, 2] {
            assert_eq!(system_scheme(Some(reported)), libadwaita::ColorScheme::Default);
        }
    }

    fn sinks() -> Vec<String> {
        // The names on the machine this was written on, abbreviated only where
        // the abbreviation cannot change the answer. Two of them share a long
        // prefix on purpose: that is the ordinary case for one sound card with
        // several HDMI outputs, and a prefix match would send audio to the
        // wrong one.
        vec![
            "alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__HDMI1__sink".into(),
            "alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__HDMI2__sink".into(),
            "alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink".into(),
        ]
    }

    #[test]
    fn nothing_chosen_means_follow_the_system_and_sends_no_variable() {
        // The default, and the one that must not drift: an unset
        // `CORDIAL_AUDIO_SINK` is what leaves PipeWire free to move the stream
        // when the user changes their default sink while playing.
        let out = AudioOutput::default();
        assert!(out.is_system_default());
        assert_eq!(out.env_value(), None);
        assert_eq!(out.index_in(&sinks()), 0);
    }

    #[test]
    fn a_chosen_sink_round_trips_through_the_picker_and_the_environment() {
        let list = sinks();
        let out = AudioOutput(list[1].clone());
        assert!(!out.is_system_default());
        assert_eq!(out.env_value(), Some(list[1].as_str()));
        assert_eq!(out.index_in(&list), 2);
        assert_eq!(AudioOutput::from_index(2, &list), out);
    }

    #[test]
    fn every_row_of_the_picker_maps_back_to_itself() {
        // The `AdwComboRow` seam, on the same footing as `ThrottleWhen`'s: two
        // encodings of one state, and nothing but a test would notice them
        // drifting apart.
        let list = sinks();
        for index in 0..=list.len() as u32 {
            let chosen = AudioOutput::from_index(index, &list);
            assert_eq!(chosen.index_in(&list), index, "row {index} did not survive the trip");
        }
    }

    #[test]
    fn a_sink_that_is_no_longer_present_does_not_read_as_the_system_default() {
        // The unplugged-headset case. `index_in` has to answer *something* for
        // a name that is not in the list, and it answers 0 — but the value
        // itself must still say a device was chosen, because that is what
        // `settings.rs` keys the "not connected" row off and what stops the
        // choice being thrown away by merely opening the window.
        let gone = AudioOutput("bluez_output.AC_12_2F_9E_00_11.1".into());
        assert!(!gone.is_system_default(), "the choice must survive the device going away");
        assert_eq!(gone.env_value(), Some("bluez_output.AC_12_2F_9E_00_11.1"));
        assert_eq!(gone.index_in(&sinks()), 0);
    }

    #[test]
    fn an_out_of_range_row_falls_back_to_the_system_default() {
        // Only reachable if the model changed under the row. Losing the choice
        // is recoverable; guessing at a device is not.
        assert_eq!(AudioOutput::from_index(99, &sinks()), AudioOutput::default());
        assert_eq!(AudioOutput::from_index(1, &[]), AudioOutput::default());
    }

    #[test]
    fn whitespace_is_not_a_device() {
        // A hand-edited shell.json is the only way to produce this, and the
        // failure it would otherwise cause is the expensive one: a
        // `CORDIAL_AUDIO_SINK=" "` reaches PipeWire as a target node called
        // " ", which matches nothing, so the client falls back and logs on
        // every stream open for ever.
        let blank = AudioOutput("   ".into());
        assert!(blank.is_system_default());
        assert_eq!(blank.env_value(), None);
    }

    #[test]
    fn the_audio_output_round_trips_through_the_file() {
        let p = scratch("audio-output.json");
        let name = "alsa_output.usb-Generic_USB_Audio-00.analog-stereo";
        save(&p, &ShellConfig { audio_output: AudioOutput(name.into()), ..Default::default() })
            .unwrap();
        assert_eq!(load(&p).audio_output, AudioOutput(name.into()));
    }

    #[test]
    fn a_config_written_before_the_audio_row_existed_follows_the_system_default() {
        // Everybody's shell.json predates this field, and landing on anything
        // but "follow the system" would move somebody's game audio to another
        // speaker because they upgraded Cordial.
        let p = scratch("pre-audio.json");
        std::fs::write(&p, r#"{"appearance":"dark","profile":"default"}"#).unwrap();
        assert!(load(&p).audio_output.is_system_default());
    }

    #[test]
    fn index_and_from_index_agree_with_each_other() {
        for scheme in [AppearanceScheme::Light, AppearanceScheme::Dark, AppearanceScheme::System] {
            assert_eq!(AppearanceScheme::from_index(scheme.index()), scheme);
        }
    }

    /// Controllers work out of the box, and the switch that turns them off
    /// survives a round trip through the file.
    ///
    /// The default matters enough to pin: `#[serde(default)]` on the container
    /// means a missing key takes `ShellConfig::default()`'s value, so a plain
    /// `#[derive(Default)]` on a `bool` field would silently make this `false`
    /// for every existing install. That is the failure this asserts against.
    #[test]
    fn controllers_are_on_unless_the_switch_says_otherwise() {
        assert!(ShellConfig::default().gamepad, "a fresh install has controllers on");
        let older = r#"{"gamemode":true,"graphics":"automatic","mangohud":false}"#;
        let parsed: ShellConfig = serde_json::from_str(older).expect("an older shell.json must load");
        assert!(parsed.gamepad, "a config predating the key must not read as controllers off");
        let off: ShellConfig = serde_json::from_str(r#"{"gamepad":false}"#).unwrap();
        assert!(!off.gamepad, "an explicit false must survive");
    }

    /// **A fresh install is responsive, and pays power for it.**
    ///
    /// Pinned with the reason attached because this default has now moved
    /// twice. FIFO is the better argument on paper -- it wastes no frames and
    /// it is the only mode the specification guarantees -- and it shipped for
    /// about an hour before a user reported the mouse felt floaty and then
    /// confirmed, with the control, that Mailbox fixed it. Anybody moving it
    /// back to FIFO should have a power measurement in hand, because the
    /// latency side of this trade now has one and the power side does not.
    #[test]
    fn a_fresh_install_is_responsive_rather_than_frugal() {
        assert_eq!(ShellConfig::default().present_mode, PresentMode::Mailbox);
        assert_eq!(PresentMode::default().as_env(), Some("mailbox"));
    }

    /// Automatic must send nothing, or ADR-020's plugin path is unreachable.
    ///
    /// The runtime's precedence is environment, then flag layers, then FIFO.
    /// An empty string or the word "auto" would both also fall through today,
    /// but only sending nothing keeps the launcher out of a decision it was
    /// not asked to make -- and only `None` is checked by the `if let` in
    /// `launch.rs`, so this is the assertion that actually holds that branch.
    #[test]
    fn automatic_sends_no_variable_at_all() {
        assert_eq!(PresentMode::Automatic.as_env(), None);
        for mode in [PresentMode::Fifo, PresentMode::Mailbox, PresentMode::Immediate] {
            assert!(mode.as_env().is_some(), "{mode:?} must name itself to the client");
        }
    }

    /// The combo model's positions and the enum must not drift apart.
    ///
    /// Every other enum on this page has the same round trip for the same
    /// reason: `settings.rs` builds a `gtk::StringList` whose order is the
    /// only thing tying a row to a value, and nothing in the type system
    /// notices when somebody inserts an entry in the middle of it.
    #[test]
    fn present_mode_survives_the_combo_row_round_trip() {
        for mode in [
            PresentMode::Fifo,
            PresentMode::Mailbox,
            PresentMode::Immediate,
            PresentMode::Automatic,
        ] {
            assert_eq!(PresentMode::from_index(mode.index()), mode);
        }
    }

    /// A `shell.json` from a Cordial that predates this key must still load,
    /// and must read as FIFO rather than refusing to parse.
    #[test]
    fn an_older_config_without_the_key_keeps_the_feel_it_had() {
        let older = r#"{"gamemode":true,"graphics":"automatic","mangohud":false}"#;
        let parsed: ShellConfig = serde_json::from_str(older).expect("an older shell.json must load");
        assert_eq!(parsed.present_mode, PresentMode::Mailbox);
    }
}
