//! The closed set of settings a runtime can change in a running game.
//!
//! **A fixed vocabulary, not a command channel.** Each [`Update`] is one
//! setting that a client reads on a hot path, or can act on in place, and can
//! therefore change without a restart (ADR-044 has the classification and the
//! reasons). There is no verb that runs anything, reads a file, or reaches the
//! engine, and an unknown key is reported back rather than acted on.
//!
//! Values travel as the same words the launch environment already uses
//! (`CORDIAL_POINTER_ACCEL=unlocked`, `CORDIAL_THROTTLE=off`), so a setting has
//! one spelling whether it arrives at spawn or afterwards. The version-0 line
//! protocol that carries these is in [`crate::v0`]; the version-1 verb
//! `settings.set` carries the same keys as its payload.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The camera-acceleration choice, in the words `CORDIAL_POINTER_ACCEL` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accel {
    Unlocked,
    Always,
}

impl Accel {
    pub fn as_str(self) -> &'static str {
        match self {
            Accel::Unlocked => "unlocked",
            Accel::Always => "always",
        }
    }
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "unlocked" => Some(Accel::Unlocked),
            "always" => Some(Accel::Always),
            _ => None,
        }
    }
}

/// When the keepalive stops, in the words `CORDIAL_THROTTLE` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Throttle {
    Visible,
    Unfocused,
    Off,
}

impl Throttle {
    pub fn as_str(self) -> &'static str {
        match self {
            Throttle::Visible => "visible",
            Throttle::Unfocused => "unfocused",
            Throttle::Off => "off",
        }
    }
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "visible" => Some(Throttle::Visible),
            "unfocused" => Some(Throttle::Unfocused),
            "off" => Some(Throttle::Off),
            _ => None,
        }
    }
}

// `TitleBar` and `FrameRateLimit` came from `cordial-shell`, where they are also
// the types behind two Settings rows, so they live here with the methods those
// rows use (`index`, `from_index`, `LABELS`, `row_label`): pure data, no GTK.
// The wire type and the row type are deliberately one type. An inherent method
// cannot be added to a type re-exported from another crate, and a second copy
// with a `From` between them is how two spellings of one setting drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleBar {
    #[default]
    Default,
    Compact,
    Hidden,
}

impl TitleBar {
    pub const LABELS: &'static [&'static str] = &["Default", "Compact", "Hidden"];

    pub const fn index(self) -> u32 {
        match self {
            Self::Default => 0,
            Self::Compact => 1,
            Self::Hidden => 2,
        }
    }

    pub const fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Compact,
            2 => Self::Hidden,
            _ => Self::Default,
        }
    }

    pub const fn env_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Compact => Some("compact"),
            Self::Hidden => Some("hidden"),
        }
    }

    pub fn from_env() -> Self {
        match std::env::var("CORDIAL_TITLE_BAR").as_deref() {
            Ok("compact") => Self::Compact,
            Ok("hidden") => Self::Hidden,
            _ => Self::Default,
        }
    }

    /// Leaving fullscreen must not reveal chrome the user explicitly hid.
    pub const fn revealed(self, fullscreen: bool) -> bool {
        match self {
            Self::Hidden => false,
            Self::Default | Self::Compact => !fullscreen,
        }
    }
}

/// The Frame rate limit row: what `DFIntTaskSchedulerTargetFps` is held at.
///
/// Display refresh sets nothing, which is the engine's own behaviour. A number
/// sets the flag to it, and `cordial_runtime::flag_reapply` puts it back after
/// each of the engine's own settings refreshes, which would otherwise revert it
/// to Roblox's value a couple of minutes in (ADR-051). The row is live: the
/// choice reaches a running client over the settings socket and is applied at
/// once (ADR-044).
///
/// **Separate from `PresentMode`, not a replacement for it.** `fastflags.md`
/// documents these as two levers -- Frame pacing is
/// `VkSwapchainCreateInfoKHR::presentMode`, this is the engine's own
/// scheduler target -- and `PresentMode::default()` has been `Mailbox`
/// since before this row existed. What was missing was a way to raise the
/// *engine's* own cap and have it stay raised.
///
/// **There is no "Unlimited", and nothing above 240.** An earlier draft sent
/// 9999, a number nothing had been measured near. A contributor then tried
/// raising the engine's frame-rate settings to 1000 on a 144 Hz monitor and the
/// frame rate still held at 240, so 240 is where the engine stops (reported in
/// the FPS Flex pull request; not reproduced here, this project has no monitor
/// that fast). A choice above 240 would be a row that does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameRateLimit {
    /// Sets nothing. The engine's own default, and what every Cordial before
    /// this row shipped.
    #[default]
    Display,
    Cap90,
    Cap120,
    Cap144,
    Cap165,
    /// `unlimited` is what a pre-release build wrote here, when the row had one;
    /// a config that names it still has to load, and 240 is where the engine
    /// stops anyway.
    #[serde(alias = "unlimited")]
    Cap240,
}

impl FrameRateLimit {
    pub const ALL: [FrameRateLimit; 6] = [
        FrameRateLimit::Display,
        FrameRateLimit::Cap90,
        FrameRateLimit::Cap120,
        FrameRateLimit::Cap144,
        FrameRateLimit::Cap165,
        FrameRateLimit::Cap240,
    ];

    /// Order matches the `AdwComboRow` model in `settings.rs`, as
    /// `PresentMode::index` does.
    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|c| *c == self).unwrap_or(0) as u32
    }

    pub fn from_index(index: u32) -> Self {
        Self::ALL.get(index as usize).copied().unwrap_or_default()
    }

    /// The word `cordial_runtime::flags::FrameRateLimit::parse` takes, out of
    /// `CORDIAL_FRAME_RATE_LIMIT` at launch and over the live socket afterwards,
    /// so a choice has one spelling either way.
    pub fn as_env(self) -> &'static str {
        match self {
            FrameRateLimit::Display => "display",
            FrameRateLimit::Cap90 => "90",
            FrameRateLimit::Cap120 => "120",
            FrameRateLimit::Cap144 => "144",
            FrameRateLimit::Cap165 => "165",
            FrameRateLimit::Cap240 => "240",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_env() == word)
    }

    pub fn row_label(self) -> &'static str {
        match self {
            FrameRateLimit::Display => "Display refresh",
            FrameRateLimit::Cap90 => "90 fps",
            FrameRateLimit::Cap120 => "120 fps",
            FrameRateLimit::Cap144 => "144 fps",
            FrameRateLimit::Cap165 => "165 fps",
            FrameRateLimit::Cap240 => "240 fps",
        }
    }
}

/// One setting a running client can change. Adding a variant here is the whole
/// of "making a setting live" on the wire; the client must also read it from a
/// place that can change (see `cordial_runtime::live_settings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    PointerAcceleration(Accel),
    Throttle(Throttle),
    CloseOnLeave(bool),
    CarryLaunchTicket(bool),
    /// The PipeWire sink's `node.name`, or empty for the system default. The
    /// same string `CORDIAL_AUDIO_SINK` carries at launch, so a choice has one
    /// spelling whether it arrives at spawn or afterwards.
    AudioOutput(String),
    /// The PipeWire source's `node.name` Roblox records from, or empty for the
    /// system default; the string `CORDIAL_AUDIO_SOURCE` carries at launch. A
    /// microphone that is recording is re-linked in place, and one that is not
    /// is left alone: applying this never opens a capture stream.
    AudioInput(String),
    /// Whether the client is registered with Feral GameMode's daemon.
    Gamemode(bool),
    /// Whether the client reads `/dev/input/js*` and feeds the engine pads.
    Gamepad(bool),
    /// The game window's header bar.
    TitleBar(TitleBar),
    /// What `DFIntTaskSchedulerTargetFps` is held at, in the words
    /// `CORDIAL_FRAME_RATE_LIMIT` uses. The client stores it and tells the
    /// engine, through the same re-apply that keeps a flag in force after the
    /// engine's own settings refresh (ADR-051).
    FrameRateLimit(FrameRateLimit),
}

/// A title-bar choice in the words `CORDIAL_TITLE_BAR` and `shell.json` use.
pub fn title_bar_word(t: TitleBar) -> &'static str {
    use TitleBar;
    match t {
        TitleBar::Default => "default",
        TitleBar::Compact => "compact",
        TitleBar::Hidden => "hidden",
    }
}

pub fn parse_title_bar(word: &str) -> Option<TitleBar> {
    use TitleBar;
    match word {
        "default" => Some(TitleBar::Default),
        "compact" => Some(TitleBar::Compact),
        "hidden" => Some(TitleBar::Hidden),
        _ => None,
    }
}

/// The keys [`Update`] can carry, which are also the `shell.json` field names.
pub const KEYS: [&str; 10] = [
    "pointer_acceleration",
    "throttle",
    "close_on_leave",
    "carry_launch_ticket",
    "audio_output",
    "audio_input",
    "gamemode",
    "gamepad",
    "title_bar",
    "frame_rate_limit",
];

/// Longest sink name accepted. PipeWire node names are short; the bound is
/// there so a value cannot approach [`MAX_LINE`] and so the client never hands
/// a hostile length to the native side.
pub const MAX_SINK_NAME: usize = 256;

/// Whether `name` is a sink name the client will pass on: no control
/// characters, bounded. Empty is valid and means the system default.
pub fn valid_sink_name(name: &str) -> bool {
    name.len() <= MAX_SINK_NAME && !name.chars().any(char::is_control)
}

impl Update {
    pub fn key(&self) -> &'static str {
        match self {
            Update::PointerAcceleration(_) => "pointer_acceleration",
            Update::Throttle(_) => "throttle",
            Update::CloseOnLeave(_) => "close_on_leave",
            Update::CarryLaunchTicket(_) => "carry_launch_ticket",
            Update::AudioOutput(_) => "audio_output",
            Update::AudioInput(_) => "audio_input",
            Update::Gamemode(_) => "gamemode",
            Update::Gamepad(_) => "gamepad",
            Update::TitleBar(_) => "title_bar",
            Update::FrameRateLimit(_) => "frame_rate_limit",
        }
    }

    pub(crate) fn value(&self) -> Value {
        match self {
            Update::PointerAcceleration(a) => Value::from(a.as_str()),
            Update::Throttle(t) => Value::from(t.as_str()),
            Update::CloseOnLeave(b) | Update::CarryLaunchTicket(b) | Update::Gamemode(b) | Update::Gamepad(b) => {
                Value::from(*b)
            }
            Update::AudioOutput(name) | Update::AudioInput(name) => Value::from(name.as_str()),
            Update::TitleBar(t) => Value::from(title_bar_word(*t)),
            Update::FrameRateLimit(l) => Value::from(l.as_env()),
        }
    }

    /// A known key with its value, or why the value is unusable. `Ok(None)` is
    /// not returned: the caller has already split unknown keys off.
    pub(crate) fn from_pair(key: &str, value: &Value) -> Result<Self, String> {
        let bad = || format!("{key}: {value} is not a value this setting takes");
        match key {
            "pointer_acceleration" => value
                .as_str()
                .and_then(Accel::parse)
                .map(Update::PointerAcceleration)
                .ok_or_else(bad),
            "throttle" => {
                value.as_str().and_then(Throttle::parse).map(Update::Throttle).ok_or_else(bad)
            }
            "close_on_leave" => value.as_bool().map(Update::CloseOnLeave).ok_or_else(bad),
            "carry_launch_ticket" => value.as_bool().map(Update::CarryLaunchTicket).ok_or_else(bad),
            "gamemode" => value.as_bool().map(Update::Gamemode).ok_or_else(bad),
            "gamepad" => value.as_bool().map(Update::Gamepad).ok_or_else(bad),
            "title_bar" => {
                value.as_str().and_then(parse_title_bar).map(Update::TitleBar).ok_or_else(bad)
            }
            "frame_rate_limit" => value
                .as_str()
                .and_then(FrameRateLimit::parse)
                .map(Update::FrameRateLimit)
                .ok_or_else(bad),
            "audio_output" => value
                .as_str()
                .filter(|n| valid_sink_name(n))
                .map(|n| Update::AudioOutput(n.to_string()))
                .ok_or_else(bad),
            // A source is a `node.name` like a sink is, so the same bound and
            // the same refusal of control characters apply.
            "audio_input" => value
                .as_str()
                .filter(|n| valid_sink_name(n))
                .map(|n| Update::AudioInput(n.to_string()))
                .ok_or_else(bad),
            _ => Err(format!("{key}: not a live setting")),
        }
    }
}

/// What [`parse_set_body`] found in a `set` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetBody {
    pub updates: Vec<Update>,
    /// Keys this side does not know. Reported back rather than failing the whole
    /// message, so a newer launcher talking to an older runtime still gets its
    /// known keys applied.
    pub ignored: Vec<String>,
}

/// The key-to-value object a `set` carries, for both the version-0 line and the
/// version-1 `settings.set` payload. A `BTreeMap`, so the keys come out sorted
/// whether or not another crate in the build turned on `serde_json`'s
/// `preserve_order`: the bytes on the wire must not depend on the build graph.
pub fn set_body(updates: &[Update]) -> BTreeMap<String, Value> {
    updates.iter().map(|u| (u.key().to_string(), u.value())).collect()
}

/// Read a `set` payload. A known key with an unusable value refuses the whole
/// message: half-applying one would leave the sender believing one thing and
/// the receiver another.
pub fn parse_set_body(map: &Map<String, Value>) -> Result<SetBody, String> {
    let mut updates = Vec::new();
    let mut ignored = Vec::new();
    for (key, value) in map {
        if KEYS.contains(&key.as_str()) {
            updates.push(Update::from_pair(key, value)?);
        } else {
            ignored.push(key.clone());
        }
    }
    Ok(SetBody { updates, ignored })
}

/// How a runtime says a setting reaches a running game (spec section 5: the
/// runtime declares, per key, `live`, `next-launch` or `unsupported`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applies {
    /// Takes effect in the running game.
    Live,
    /// Stored, and used the next time the runtime starts a game.
    NextLaunch,
    /// This runtime has no use for it. The launcher shows the row as
    /// unsupported, with the runtime's name, and does not send it.
    Unsupported,
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn all() -> Vec<Update> {
        vec![
            Update::PointerAcceleration(Accel::Unlocked),
            Update::Throttle(Throttle::Off),
            Update::CloseOnLeave(true),
            Update::CarryLaunchTicket(false),
            Update::AudioOutput("alsa_output.pci-0000_00_1f.3.analog-stereo".to_string()),
            Update::AudioInput("alsa_input.usb-Headset-00.mono-fallback".to_string()),
            Update::Gamemode(false),
            Update::Gamepad(false),
            Update::TitleBar(TitleBar::Hidden),
            Update::FrameRateLimit(FrameRateLimit::Cap144),
        ]
    }

    #[test]
    fn a_set_body_round_trips() {
        let body = set_body(&all());
        let map: Map<String, Value> = body.into_iter().collect();
        let SetBody { mut updates, ignored } = parse_set_body(&map).unwrap();
        assert!(ignored.is_empty());
        let mut want = all();
        updates.sort_by_key(|u| u.key());
        want.sort_by_key(|u| u.key());
        assert_eq!(updates, want);
    }

    #[test]
    fn applies_uses_the_spec_words() {
        for (a, word) in [(Applies::Live, "live"), (Applies::NextLaunch, "next-launch"), (Applies::Unsupported, "unsupported")] {
            assert_eq!(serde_json::to_value(a).unwrap(), Value::from(word));
        }
    }
}

#[cfg(test)]
mod title_bar_tests {
    use super::*;

    #[test]
    fn hidden_selection_round_trips_through_config_and_launch_environment() {
        // Given the third choice in the settings row.
        let choice = TitleBar::from_index(2);
        // When saving and restoring its value.
        let stored = serde_json::to_string(&choice).unwrap();
        let restored: TitleBar = serde_json::from_str(&stored).unwrap();
        // Then the next game receives the hidden preference.
        assert_eq!(stored, "\"hidden\"");
        assert_eq!(restored.index(), 2);
        assert_eq!(restored.env_value(), Some("hidden"));
    }

    #[test]
    fn each_mode_controls_windowed_chrome() {
        // Given each named preference.
        for (choice, expected) in [
            (TitleBar::Default, true),
            (TitleBar::Compact, true),
            (TitleBar::Hidden, false),
        ] {
            // When requesting its windowed presentation, then only Hidden removes chrome.
            assert_eq!(choice.revealed(false), expected);
        }
    }

    #[test]
    fn fullscreen_hides_chrome_in_every_mode() {
        // Given each named preference.
        for choice in [TitleBar::Default, TitleBar::Compact, TitleBar::Hidden] {
            // When requesting its fullscreen presentation, then no mode reveals chrome.
            assert!(!choice.revealed(true));
        }
    }

    #[test]
    fn existing_preferences_keep_their_values_and_default() {
        // Given old configurations and out-of-range settings indices.
        for (index, expected) in [
            (0, TitleBar::Default),
            (1, TitleBar::Compact),
            (99, TitleBar::Default),
        ] {
            // When reading the preference, then old choices retain their meaning.
            assert_eq!(TitleBar::from_index(index), expected);
        }
        assert_eq!(TitleBar::default(), TitleBar::Default);
    }
}
