//! Changing a running game's settings from Settings (ADR-044), over the
//! runtime protocol (ADR-055).
//!
//! The shell hands the client its settings once, in the launch environment.
//! That left every row saying "saved" while a game already running kept the old
//! value until the next launch. This module closes the gap for the settings
//! that can change: it notices `shell.json` change, works out which live keys
//! moved, and sends only those to each client this shell started, as
//! `settings.set` on the persistent `cordial.runtime/1` connection the client
//! serves (`cordial_runtime::control`).
//!
//! **One connection per running client, kept.** It was a connection per request
//! on a socket of its own (`live/settings.sock`); it is now the same channel
//! the client's events arrive on, opened with retry after the spawn and held
//! until the client exits. The launcher window closing does not touch it
//! (ADR-031), a runtime that dropped it is reconnected to with `reattach`, and
//! a launcher started later adopts a runtime it finds listening, which is the
//! other half of "closing the launcher must not stop the game".
//!
//! **The runtime says how each key reaches it.** `settings.get` carries a
//! declaration per key, `live` or `next-launch`, and the table below is the
//! fallback for a runtime that did not answer and the check that every key in
//! `shell.json` was decided on. A change to a key the running runtime declares
//! `next-launch` is not sent; it is reported as applying at the next launch,
//! which is what the row beside it already says.
//!
//! **One path for every source of change.** The shell's own saves, a
//! hand-edited file and a second shell instance all end up as "the file
//! changed", so a directory watch feeds one reconcile and no settings row has to
//! remember to call anything. A row that forgot would be the interface shape of
//! a stub that returns success.
//!
//! **The directory is watched, not the file.** Editors and other instances save
//! by writing a temporary file and renaming it over `shell.json`, which gives
//! the path a new inode; a watch on the old inode goes quiet after the first
//! save and never says so. Events are filtered to the file's name and debounced,
//! because one save arrives as a burst (create, write, rename, done).
//!
//! **A half-written file is not the defaults.** `shell_config::load` answers
//! "defaults" for anything it cannot parse, which is right for starting up and
//! wrong here: reading a save in progress would push every live setting back to
//! its default. So this parses strictly and, on failure, changes nothing and
//! waits for the next event.

use crate::shell_config::{self, ShellConfig};
use cordial_protocol::settings::{Accel, Applies as Declared, Throttle};
use cordial_protocol::{msg, socket, Event, Update};
use cordial_shell::runtime_session::{self as session, Link, OpenError, OpenOptions, SessionDir};
use libadwaita::gio;
use libadwaita::glib;
use libadwaita::prelude::*;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

// ---- classification --------------------------------------------------------

/// When a change to a `shell.json` key reaches the thing it configures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Sent to every running client of this shell, and in force within a
    /// moment. Exactly the keys in `cordial_protocol::settings::KEYS`.
    Live,
    /// Read by the shell itself, when it next needs the value. There is no
    /// running client to tell.
    Shell,
    /// The client reads it once at startup, or it decides what the client
    /// builds at startup. The Settings row says so.
    NextLaunch,
}

/// Every `shell.json` key, with when it applies and why. A test fails if the
/// config grows a key that is not listed here, so a new setting cannot ship
/// without somebody deciding what it does to a running game.
pub const CLASSIFICATION: &[(&str, Applies, &str)] = &[
    ("appearance", Applies::Shell, "the shell's own theme, applied by the row itself"),
    ("title_bar", Applies::Live, "the header bar is revealed, hidden or restyled on the game window in place; the window keeps its size, so the canvas takes the difference"),
    ("roblox", Applies::NextLaunch, "which build to run is located at launch"),
    ("profile", Applies::NextLaunch, "names the profile the next client runs"),
    ("gamemode", Applies::Live, "registration with gamemoded is per pid and can be made or withdrawn at any time"),
    ("throttle", Applies::Live, "the pump reads the policy every tick"),
    ("pointer_acceleration", Applies::Live, "read on every locked-pointer motion event"),
    ("automatic_updates", Applies::Shell, "read by the shell when a check runs"),
    ("download_on", Applies::Shell, "read by the shell when a download starts"),
    ("graphics", Applies::NextLaunch, "the backend is settled before the engine's first dlopen of libvulkan"),
    ("graphics_optimization_mode", Applies::NextLaunch, "device profile and core count are read during engine initialisation"),
    ("present_mode", Applies::NextLaunch, "a field of the swapchain, and Cordial never rebuilds a swapchain: the engine does, when the extent it reads changes, and nothing in Cordial can ask for one at the same size"),
    ("frame_rate_limit", Applies::Live, "the client stores the choice and hands the engine the settings document again with the new DFIntTaskSchedulerTargetFps in it; the engine takes the new value at once; back to Display refresh waits for the engine's own next settings refresh, because it does not unset a flag the document stops carrying (ADR-051)"),
    ("gamepad", Applies::Live, "the pump polls it each tick; switching off sends the engine a disconnect for every announced pad, the same call an unplugged one gets"),
    ("close_on_leave", Applies::Live, "consulted when the engine's log reports leaving a game"),
    ("unpacked_plugins", Applies::NextLaunch, "the folder list is an environment variable of the client, and the hot-swap reconciler is built never to see unpacked plugins (ADR-038); edits inside a listed folder already reload"),
    ("carry_launch_ticket", Applies::Live, "consulted each time a link is translated"),
    ("mangohud", Applies::NextLaunch, "a Vulkan layer, loaded at instance creation"),
    ("vkbasalt", Applies::NextLaunch, "a Vulkan layer, loaded at instance creation"),
    ("audio_output", Applies::Live, "the running streams are re-linked to the new sink in place; a host backend with no sink to move between (ALSA, OSS, PulseAudio) applies it to streams opened afterwards"),
    ("audio_input", Applies::Live, "a recording in progress is re-linked to the new source in place; with none open nothing is opened and the choice is used the next time Roblox starts recording"),
    ("fullscreen_accel", Applies::NextLaunch, "bound when the launcher window is built; no Settings row"),
    ("marketplace_index_dir", Applies::Shell, "read when the Plugins page loads"),
    ("marketplace_public_key", Applies::Shell, "read when the Plugins page loads"),
    ("multi_instance_warning_seen", Applies::Shell, "shell state, not a setting"),
];

pub fn classify(key: &str) -> Option<Applies> {
    CLASSIFICATION.iter().find(|(k, _, _)| *k == key).map(|(_, a, _)| *a)
}

// ---- reading the live values out of a config --------------------------------

/// The live keys of a config, as wire updates, in `KEYS` order.
pub fn live_updates(config: &ShellConfig) -> Vec<Update> {
    use shell_config::{PointerAcceleration, ThrottleWhen};
    vec![
        Update::PointerAcceleration(match config.pointer_acceleration {
            PointerAcceleration::UnlockedCursor => Accel::Unlocked,
            PointerAcceleration::Always => Accel::Always,
        }),
        Update::Throttle(match config.throttle {
            ThrottleWhen::Visible => Throttle::Visible,
            ThrottleWhen::Unfocused => Throttle::Unfocused,
            ThrottleWhen::Off => Throttle::Off,
        }),
        Update::CloseOnLeave(config.close_on_leave),
        Update::CarryLaunchTicket(config.carry_launch_ticket),
        // The same string the launch environment carries, trimmed the same way,
        // and empty for "follow the default".
        Update::AudioOutput(config.audio_output.env_value().unwrap_or("").to_string()),
        Update::AudioInput(config.audio_input.env_value().unwrap_or("").to_string()),
        Update::Gamemode(config.gamemode),
        Update::Gamepad(config.gamepad),
        Update::TitleBar(config.title_bar),
        Update::FrameRateLimit(config.frame_rate_limit),
    ]
}

/// The updates in `wanted` that differ from what `have` says is in force. A key
/// `have` lacks counts as different.
pub fn changed(have: &[Update], wanted: &[Update]) -> Vec<Update> {
    wanted.iter().filter(|w| !have.contains(w)).cloned().collect()
}

/// Parse `shell.json` strictly. `None` for a missing, unreadable or malformed
/// file; see the module comment for why that is not "defaults".
pub fn read_strict(path: &Path) -> Option<ShellConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

// ---- what is running, and what it has been told ----------------------------

struct Target {
    pid: u32,
    /// Where the client's `ctl.sock` is. The shell made it for a client it
    /// spawned and found it for one it adopted.
    session: SessionDir,
    link: Option<Arc<Link>>,
    /// Whether a link to this client has ever been up, which is what makes the
    /// next `hello` a reattach.
    ever_linked: bool,
    /// What this client is known to be running with: the launch environment
    /// until the first connection, and what the runtime reports in force after.
    sent: Vec<Update>,
    /// How the runtime says each key reaches it. Empty until it has answered.
    declared: std::collections::BTreeMap<String, Declared>,
    runtime: Option<String>,
    /// Failed attempts since the wanted values last changed or a link was up.
    failures: u32,
    /// Found running rather than started by this shell, so there is no child to
    /// watch and the link ending for good is how it is known to be gone.
    adopted: bool,
    /// A newer controller replaced this launcher. Not reconnected to: two
    /// controllers each replacing the other is not control.
    superseded: bool,
}

/// A client that has not answered in this many tries stops being retried until
/// the wanted values change again. Once a second, so about half a minute: long
/// enough to cover the engine loading before the socket exists.
const GIVE_UP_AFTER: u32 = 30;

/// A change the running runtime cannot take now, kept so a window that wants to
/// say so (and a test) can read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub pid: u32,
    pub key: String,
    pub text: String,
}

const MAX_NOTICES: usize = 32;

#[derive(Default)]
struct State {
    targets: Vec<Target>,
    wanted: Vec<Update>,
    /// The whole config as JSON at the last `want`, to tell which keys moved.
    /// `None` until the first, which is a baseline and reports nothing.
    wanted_json: Option<Value>,
    notices: std::collections::VecDeque<Notice>,
}

/// One delivery to attempt, copied out so no lock is held across a socket call:
/// a client that takes its two-second timeout must not stop the GTK thread
/// registering the next one.
struct Job {
    pid: u32,
    session: SessionDir,
    link: Option<Arc<Link>>,
    reattach: bool,
}

impl State {
    fn register(&mut self, pid: u32, session: SessionDir, launched_with: Vec<Update>, link: Option<Arc<Link>>, adopted: bool) {
        self.targets.retain(|t| t.pid != pid);
        let ever_linked = link.is_some();
        self.targets.push(Target {
            pid,
            session,
            link,
            ever_linked,
            sent: launched_with,
            declared: Default::default(),
            runtime: None,
            failures: 0,
            adopted,
            superseded: false,
        });
    }

    fn unregister(&mut self, pid: u32) -> Option<Target> {
        let at = self.targets.iter().position(|t| t.pid == pid)?;
        Some(self.targets.remove(at))
    }

    /// Record the newest wanted values. `true` when anything needs doing.
    fn want(&mut self, wanted: Vec<Update>) -> bool {
        if wanted != self.wanted {
            self.wanted = wanted;
            for t in &mut self.targets {
                t.failures = 0;
            }
        }
        self.outstanding()
    }

    /// What `target` still needs sent: the wanted values it is not running,
    /// less the keys its runtime declared it has no use for.
    fn diff(&self, t: &Target) -> Vec<Update> {
        changed(&t.sent, &self.wanted)
            .into_iter()
            .filter(|u| t.declared.get(u.key()) != Some(&Declared::Unsupported))
            .collect()
    }

    fn needs_link(t: &Target) -> bool {
        !t.superseded && t.link.as_ref().map_or(true, |l| !l.alive())
    }

    fn outstanding(&self) -> bool {
        self.targets.iter().any(|t| t.failures < GIVE_UP_AFTER && (Self::needs_link(t) || !self.diff(t).is_empty()))
    }

    fn plan(&self) -> Vec<Job> {
        self.targets
            .iter()
            .filter(|t| t.failures < GIVE_UP_AFTER && (Self::needs_link(t) || !self.diff(t).is_empty()))
            .map(|t| Job {
                pid: t.pid,
                session: t.session.clone(),
                link: t.link.clone().filter(|l| l.alive()),
                reattach: t.ever_linked,
            })
            .collect()
    }

    /// A link is up. What the runtime says it is running replaces what the
    /// launch environment was assumed to have given it, so the diff after this
    /// is against the truth and a value it already holds is not sent again.
    fn linked(&mut self, pid: u32, link: Arc<Link>) {
        let Some(t) = self.targets.iter_mut().find(|t| t.pid == pid) else { return };
        let est = link.established();
        t.declared = est.declared.clone();
        t.runtime = Some(format!("{} {}", est.runtime.id, est.runtime.version));
        if !est.values.is_empty() {
            // One key at a time: a value this launcher does not know (a frame
            // rate cap set by hand in the environment, say) must cost that key
            // and not the rest, because `from_payload` refuses a whole object
            // for one bad value.
            let reported: Vec<Update> = est
                .values
                .iter()
                .filter_map(|(k, v)| {
                    let one = Value::Object([(k.clone(), v.clone())].into_iter().collect());
                    msg::SettingsSet::from_payload(&one).ok().and_then(|b| b.updates.into_iter().next())
                })
                .collect();
            if !reported.is_empty() {
                t.sent = reported;
            }
        }
        t.link = Some(link);
        t.ever_linked = true;
        t.failures = 0;
    }

    fn delivered(&mut self, pid: u32, updates: &[Update]) {
        if let Some(t) = self.targets.iter_mut().find(|t| t.pid == pid) {
            for u in updates {
                t.sent.retain(|s| s.key() != u.key());
                t.sent.push(u.clone());
            }
            t.failures = 0;
        }
    }

    fn failed(&mut self, pid: u32) -> u32 {
        match self.targets.iter_mut().find(|t| t.pid == pid) {
            Some(t) => {
                t.failures += 1;
                t.failures
            }
            None => 0,
        }
    }

    fn give_up(&mut self, pid: u32) {
        if let Some(t) = self.targets.iter_mut().find(|t| t.pid == pid) {
            t.failures = GIVE_UP_AFTER;
        }
    }

    /// How a key reaches this client: the runtime's own word when it has given
    /// one, and this shell's table when it has not.
    fn applies_to(t: &Target, key: &str) -> Option<Declared> {
        t.declared.get(key).copied().or_else(|| match classify(key)? {
            Applies::Live => Some(Declared::Live),
            Applies::NextLaunch => Some(Declared::NextLaunch),
            Applies::Shell => None,
        })
    }

    /// Note which changed keys a running client will not take until its next
    /// launch, or at all.
    fn note_changes(&mut self, now: &Value) {
        let before = self.wanted_json.replace(now.clone());
        let (Some(Value::Object(before)), Value::Object(after)) = (before, now) else { return };
        let moved: Vec<&String> = after.iter().filter(|(k, v)| before.get(*k) != Some(*v)).map(|(k, _)| k).collect();
        let mut fresh = Vec::new();
        for t in &self.targets {
            for key in &moved {
                let text = match Self::applies_to(t, key) {
                    Some(Declared::NextLaunch) => "applies at next launch".to_string(),
                    Some(Declared::Unsupported) => "this runtime has no use for it".to_string(),
                    _ => continue,
                };
                let by = t.runtime.as_deref().map(|r| format!(" ({r} declares it)")).unwrap_or_default();
                fresh.push(Notice { pid: t.pid, key: (*key).clone(), text: format!("{text}{by}") });
            }
        }
        for n in fresh {
            println!("  shell: pid {}: {}: {}", n.pid, n.key, n.text);
            if self.notices.len() == MAX_NOTICES {
                self.notices.pop_front();
            }
            self.notices.push_back(n);
        }
    }
}

static STATE: Mutex<State> = Mutex::new(State {
    targets: Vec::new(),
    wanted: Vec::new(),
    wanted_json: None,
    notices: std::collections::VecDeque::new(),
});
static WAKE: OnceLock<Sender<()>> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn wake() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.send(());
    }
}

/// Start the delivery thread. Idempotent.
fn ensure_worker() {
    WAKE.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("cordial-live".into())
            .spawn(move || worker(rx))
            .expect("a thread can be spawned");
        tx
    });
}

fn worker(rx: Receiver<()>) {
    loop {
        deliver_once();
        let retry = state().outstanding();
        if retry {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else if rx.recv().is_err() {
            return;
        }
    }
}

/// What an event is worth a line in the shell's output. Never an id of a person.
fn narrate_event(pid: u32, ev: &Event) {
    let what = match ev.ev.as_str() {
        msg::names::GAME_JOINED => match ev.payload::<msg::GameJoined>() {
            Ok(j) => format!("game.joined place {}", j.place_id),
            Err(_) => return,
        },
        msg::names::GAME_LEFT => "game.left".to_string(),
        msg::names::SESSION_STATE => match ev.payload::<msg::SessionState>() {
            Ok(s) => format!("session.state signed_in={}", s.signed_in),
            Err(_) => return,
        },
        msg::names::ENGINE_VERSION => match ev.payload::<msg::EngineVersion>() {
            Ok(v) => format!("engine.version {}", v.version),
            Err(_) => return,
        },
        msg::names::GAME_PRESENCE => "game.presence".to_string(),
        msg::names::EVENTS_DROPPED => format!("events.dropped {}", ev.p["count"]),
        msg::names::BYE => format!("bye {}", ev.p["reason"].as_str().unwrap_or("")),
        // Anything else, including a runtime's private `x-` events, is the
        // report's business and not the terminal's.
        _ => return,
    };
    println!("  shell: pid {pid}: runtime event: {what}");
}

fn options(pid: u32, session_id: &str, reattach: bool) -> OpenOptions {
    let mut o = OpenOptions::new(session_id);
    o.reattach = reattach;
    o.on_event = Arc::new(move |ev| narrate_event(pid, ev));
    // The reader thread ends when the connection does. Waking the worker is
    // what turns that into a reconnect within the second, instead of whenever
    // the next settings change happens to come.
    o.on_close = Arc::new(wake);
    o
}

/// Connect to a job's runtime, or say why not.
fn connect(job: &Job) -> Option<Arc<Link>> {
    match Link::open(&job.session.dir, options(job.pid, &job.session.id, job.reattach)) {
        Ok(link) => {
            let est = link.established();
            println!(
                "  shell: pid {}: runtime {} {} ({}), {}, protocol {}, live: {}",
                job.pid,
                est.runtime.id,
                est.runtime.version,
                est.client.name,
                if job.reattach { "reattached" } else { "attached" },
                est.protocol,
                est.caps.keys().cloned().collect::<Vec<_>>().join(" ")
            );
            Some(Arc::new(link))
        }
        Err(OpenError::Unreachable(e)) => {
            let n = state().failed(job.pid);
            // The first failure is expected while the engine is still loading,
            // so say nothing until it has gone on a while.
            if n == 5 || n == GIVE_UP_AFTER {
                println!("  shell: live settings could not reach pid {} ({e}); try {n}", job.pid);
            }
            None
        }
        Err(OpenError::Failed(why)) => {
            // An exchange that happened and failed will not improve by being
            // repeated, and repeating it would be a stream of refused hellos at
            // a process that already answered.
            state().give_up(job.pid);
            println!("  shell: pid {}: the runtime's handshake failed ({why}); not retrying", job.pid);
            None
        }
    }
}

fn deliver_once() {
    let plan = state().plan();
    for job in plan {
        let link = match job.link.clone() {
            Some(l) => l,
            None => match connect(&job) {
                Some(l) => {
                    state().linked(job.pid, l.clone());
                    l
                }
                None => {
                    drop_if_gone(&job);
                    continue;
                }
            },
        };
        if link.superseded() {
            if let Some(t) = state().targets.iter_mut().find(|t| t.pid == job.pid) {
                t.superseded = true;
            }
            println!("  shell: pid {}: another controller took over this runtime; leaving it", job.pid);
            continue;
        }
        let diff = {
            let s = state();
            s.targets.iter().find(|t| t.pid == job.pid).map(|t| s.diff(t)).unwrap_or_default()
        };
        if diff.is_empty() {
            continue;
        }
        match link.settings_set(&diff) {
            Ok(reply) => {
                state().delivered(job.pid, &diff);
                // What the client applied but could not fully do, such as a sink
                // change with nothing playing. Said here because the settings
                // window has no place to show it and "applied" alone would be
                // read as "you will hear it".
                for (key, note) in &reply.notes {
                    println!("  shell: pid {}: {key}: {note}", job.pid);
                }
                println!(
                    "  shell: live settings -> pid {}: {}",
                    job.pid,
                    diff.iter().map(|u| u.key()).collect::<Vec<_>>().join(", ")
                );
            }
            Err(why) => {
                let n = state().failed(job.pid);
                if n == 5 || n == GIVE_UP_AFTER {
                    println!("  shell: live settings could not reach pid {} ({why}); try {n}", job.pid);
                }
            }
        }
    }
}

/// A client found running that has stopped answering for good is gone, and
/// nothing else will say so: it has no child to reap.
fn drop_if_gone(job: &Job) {
    let mut s = state();
    let gone = s.targets.iter().any(|t| t.pid == job.pid && t.adopted && t.failures >= GIVE_UP_AFTER);
    if gone {
        s.unregister(job.pid);
        println!("  shell: pid {}: no longer answering; no longer controlling it", job.pid);
    }
}

/// Remember a client this shell started, with the values its environment gave
/// it. If the file has moved on since, the difference is sent as soon as its
/// socket answers.
pub fn register(pid: u32, session: SessionDir, launched_with: Vec<Update>) {
    ensure_worker();
    let outstanding = {
        let mut s = state();
        s.register(pid, session, launched_with, None, false);
        s.outstanding()
    };
    if outstanding {
        wake();
    }
}

pub fn unregister(pid: u32) {
    let gone = state().unregister(pid);
    // Dropping the target closes its link; the runtime is exiting or has, and
    // the directory it made is no use to the next launch.
    if let Some(t) = gone {
        t.session.remove();
    }
}

/// What the Settings window says about the game that is running, from the
/// runtime's own declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub title: String,
    pub detail: String,
}

/// `present_mode` as "Present mode": the key is the only name the runtime gives
/// a setting, and it is not a label.
fn label(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

fn summarize(t: &Target, last: Option<&Notice>) -> Option<Running> {
    let runtime = t.runtime.as_deref()?;
    let list = |want: Declared| {
        let keys: Vec<String> = t.declared.iter().filter(|(_, a)| **a == want).map(|(k, _)| label(k)).collect();
        (!keys.is_empty()).then(|| keys.join(", "))
    };
    let mut detail = Vec::new();
    if let Some(now) = list(Declared::Live) {
        detail.push(format!("Applies now: {now}."));
    }
    if let Some(later) = list(Declared::NextLaunch) {
        detail.push(format!("Applies at next launch: {later}."));
    }
    if let Some(none) = list(Declared::Unsupported) {
        detail.push(format!("Not used by this runtime: {none}."));
    }
    if let Some(n) = last.filter(|n| n.pid == t.pid) {
        detail.push(format!("{} {}.", label(&n.key), n.text.split(" (").next().unwrap_or(&n.text)));
    }
    Some(Running { title: format!("{runtime} is running (pid {})", t.pid), detail: detail.join(" ") })
}

/// The first running client that has told us how it takes settings, for the
/// Settings window. `None` when nothing is running, and the window then shows
/// nothing, which is what it showed before there was a running client to ask.
pub fn running_summary() -> Option<Running> {
    let s = state();
    s.targets.iter().find_map(|t| summarize(t, s.notices.back()))
}

/// The values the shell currently wants running clients to have.
pub fn want(config: &ShellConfig) {
    ensure_worker();
    let json = serde_json::to_value(config).unwrap_or(Value::Null);
    let outstanding = {
        let mut s = state();
        s.note_changes(&json);
        s.want(live_updates(config))
    };
    if outstanding {
        wake();
    }
}

/// Find runtimes already running in this user's profiles and take them over.
///
/// **This is what "reopening the launcher reattaches" means for a launcher that
/// was restarted.** A launcher whose window closed is still the same process
/// holding the same link (ADR-031), so nothing is found here for it; a new
/// process starts with no children and no links, and the runtime, which kept
/// listening, is waiting. It is run once, off the GTK thread, because opening a
/// link can take its two seconds. A runtime whose socket does not answer is left
/// alone: it may be loading, and removing its directory would take its channel
/// away.
pub fn adopt_running() {
    ensure_worker();
    let spawned = std::thread::Builder::new().name("cordial-adopt".into()).spawn(|| {
        for name in cordial_shell::profile::list() {
            let Ok(profile_dir) = cordial_shell::profile::dir(&name) else { continue };
            for found in session::list_sessions(&profile_dir.join("runtime")) {
                let Ok(stream) = socket::connect(&found.dir) else { continue };
                let Some(pid) = session::peer_pid(&stream) else { continue };
                if state().targets.iter().any(|t| t.pid == pid) {
                    continue;
                }
                match Link::open_stream(stream, options(pid, &found.id, true)) {
                    Ok(link) => {
                        let link = Arc::new(link);
                        let est = link.established();
                        println!(
                            "  shell: found {} {} running as pid {pid} in profile {name:?}; reattached, live: {}",
                            est.runtime.id,
                            est.runtime.version,
                            est.caps.keys().cloned().collect::<Vec<_>>().join(" ")
                        );
                        let outstanding = {
                            let mut s = state();
                            s.register(pid, found, Vec::new(), Some(link.clone()), true);
                            s.linked(pid, link);
                            s.outstanding()
                        };
                        if outstanding {
                            wake();
                        }
                    }
                    Err(e) => println!("  shell: pid {pid} in profile {name:?} did not take a reattach ({e})"),
                }
            }
        }
    });
    if let Err(e) = spawned {
        println!("  shell: could not look for running clients ({e})");
    }
}

// ---- watching the file ------------------------------------------------------

/// How long the directory must be quiet before the file is read. A save is a
/// burst of events; reading in the middle of it is the half-written case.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Keeps a directory monitor alive. Dropping it stops the watch.
pub struct FileWatch {
    _monitor: gio::FileMonitor,
}

/// Call `on_settled` once after `path` has changed and things have gone quiet.
///
/// Must be called with the GLib main context that will run `on_settled`
/// acquired by the calling thread (the GTK thread, in the shell).
pub fn watch_file(path: &Path, on_settled: impl Fn() + 'static) -> Result<FileWatch, String> {
    let name = path.file_name().ok_or("the config path has no file name")?.to_owned();
    let dir = path.parent().ok_or("the config path has no directory")?;
    // Created if absent: nobody has saved a setting yet on a fresh install, and
    // a watch that failed for that reason would never start.
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let monitor = gio::File::for_path(dir)
        .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    monitor.set_rate_limit(50);

    let latest: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    let on_settled = Rc::new(on_settled);
    monitor.connect_changed(move |_, file, other, event| {
        use gio::FileMonitorEvent as E;
        if !matches!(
            event,
            E::Created | E::Changed | E::ChangesDoneHint | E::Renamed | E::MovedIn | E::Deleted
        ) {
            return;
        }
        let mentions = |f: Option<&gio::File>| {
            f.and_then(|f| f.basename()).is_some_and(|b| b.as_os_str() == name.as_os_str())
        };
        if !(mentions(Some(file)) || mentions(other)) {
            return;
        }
        // Trailing edge: each event pushes the read back. A generation counter
        // rather than cancelling a timer, because a GLib timeout source must be
        // `Send` and these handles are not; a future spawned on the
        // thread-default context is not, and is also what keeps a test that
        // spins its own context honest (`timeout_add_local_once` would attach
        // to the process default instead).
        let generation = latest.get().wrapping_add(1);
        latest.set(generation);
        let latest = Rc::clone(&latest);
        let done = Rc::clone(&on_settled);
        glib::MainContext::ref_thread_default().spawn_local(async move {
            glib::timeout_future(DEBOUNCE).await;
            if latest.get() == generation {
                done();
            }
        });
    });
    Ok(FileWatch { _monitor: monitor })
}

/// Watch `shell.json` and keep the running clients in step with it. The
/// returned guard must be kept for the life of the application.
pub fn start(config_path: &Path, current: &ShellConfig) -> Option<FileWatch> {
    want(current);
    // A runtime that outlived the launcher that started it is still listening.
    adopt_running();
    let path = config_path.to_path_buf();
    // What the last successful read said, so an unchanged save (the shell
    // rewrites the whole file for any row) sends nothing.
    let seen = Rc::new(RefCell::new(live_updates(current)));
    let announced_bad = Rc::new(Cell::new(false));
    let watch_path = path.clone();
    let result = watch_file(&watch_path, move || match read_strict(&path) {
        Some(config) => {
            announced_bad.set(false);
            let now = live_updates(&config);
            if *seen.borrow() != now {
                *seen.borrow_mut() = now;
                want(&config);
            }
        }
        None => {
            if !announced_bad.replace(true) {
                println!(
                    "  shell: {} changed but could not be read; keeping the running settings",
                    path.display()
                );
            }
        }
    });
    match result {
        Ok(w) => Some(w),
        Err(e) => {
            println!("  shell: not watching {} ({e}); running games keep their settings", watch_path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_protocol::settings::KEYS;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("cordial-live-shell-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_config_key_is_classified_and_the_live_ones_are_exactly_the_wire_keys() {
        let value = serde_json::to_value(ShellConfig::default()).unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(classify(key).is_some(), "{key} is in shell.json and not in CLASSIFICATION");
        }
        let mut live: Vec<_> = CLASSIFICATION
            .iter()
            .filter(|(_, a, _)| *a == Applies::Live)
            .map(|(k, _, _)| *k)
            .collect();
        live.sort_unstable();
        let mut wire = KEYS.to_vec();
        wire.sort_unstable();
        assert_eq!(live, wire, "a live key with no wire message applies to nothing");
        // Optional keys are skipped when unset, so also check the table against
        // itself: no key listed twice.
        let mut names: Vec<_> = CLASSIFICATION.iter().map(|(k, _, _)| *k).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), CLASSIFICATION.len());
    }

    #[test]
    fn a_config_maps_onto_the_wire_and_only_moved_keys_are_reported() {
        let mut a = ShellConfig::default();
        let before = live_updates(&a);
        assert!(changed(&before, &live_updates(&a)).is_empty());
        a.pointer_acceleration = shell_config::PointerAcceleration::UnlockedCursor;
        a.close_on_leave = true;
        let moved = changed(&before, &live_updates(&a));
        assert_eq!(
            moved,
            vec![Update::PointerAcceleration(Accel::Unlocked), Update::CloseOnLeave(true)]
        );
    }

    #[test]
    fn the_audio_output_reaches_the_wire_as_the_launch_environment_spells_it() {
        let audio = |c: &ShellConfig| {
            live_updates(c).into_iter().find(|u| u.key() == "audio_output").unwrap()
        };
        let mut c = ShellConfig::default();
        assert_eq!(audio(&c), Update::AudioOutput(String::new()), "no choice is the empty string");

        // Stored with stray whitespace, sent the way CORDIAL_AUDIO_SINK is: trimmed.
        c.audio_output = shell_config::AudioOutput("  alsa_output.usb-Headset  ".into());
        assert_eq!(audio(&c), Update::AudioOutput("alsa_output.usb-Headset".into()));

        // Control: of everything in the config, only this key moved.
        let before = live_updates(&ShellConfig::default());
        assert_eq!(
            changed(&before, &live_updates(&c)),
            vec![Update::AudioOutput("alsa_output.usb-Headset".into())]
        );
        // And choosing the default again is a change too, back to empty.
        assert_eq!(
            changed(&live_updates(&c), &before),
            vec![Update::AudioOutput(String::new())]
        );
    }

    #[test]
    fn the_audio_input_is_its_own_key_and_does_not_move_with_the_output() {
        let input = |c: &ShellConfig| {
            live_updates(c).into_iter().find(|u| u.key() == "audio_input").unwrap()
        };
        let mut c = ShellConfig::default();
        assert_eq!(input(&c), Update::AudioInput(String::new()), "no choice is the empty string");

        c.audio_input = shell_config::AudioOutput("  alsa_input.usb-Headset  ".into());
        assert_eq!(input(&c), Update::AudioInput("alsa_input.usb-Headset".into()));

        // Control: choosing a microphone moves that key and nothing else, in
        // particular not the sink.
        let before = live_updates(&ShellConfig::default());
        assert_eq!(
            changed(&before, &live_updates(&c)),
            vec![Update::AudioInput("alsa_input.usb-Headset".into())]
        );
        assert_eq!(changed(&live_updates(&c), &before), vec![Update::AudioInput(String::new())]);
    }

    #[test]
    fn a_torn_or_missing_file_reads_as_nothing_rather_than_as_defaults() {
        let dir = scratch("strict");
        let p = dir.join("shell.json");
        assert!(read_strict(&p).is_none(), "missing");
        std::fs::write(&p, br#"{"pointer_acceleration":"unloc"#).unwrap();
        assert!(read_strict(&p).is_none(), "cut off mid-write");
        std::fs::write(&p, br#"{"pointer_acceleration":"unlockedcursor"}"#).unwrap();
        let c = read_strict(&p).expect("a valid partial file fills the rest from defaults");
        assert_eq!(c.pointer_acceleration, shell_config::PointerAcceleration::UnlockedCursor);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn session(tag: &str) -> SessionDir {
        SessionDir { dir: PathBuf::from(format!("/nonexistent/{tag}")), id: "abcd1234".into() }
    }

    #[test]
    fn a_new_client_is_only_sent_what_differs_from_its_launch_environment() {
        let mut s = State::default();
        let launched = vec![Update::Throttle(Throttle::Visible), Update::CloseOnLeave(false)];
        s.register(7, session("a"), launched.clone(), None, false);
        // Nothing wanted differs, but there is no link yet, so there is a job.
        assert!(!s.want(launched.clone()) || State::needs_link(&s.targets[0]));
        // The file moved on while the client was loading.
        let wanted = vec![Update::Throttle(Throttle::Off), Update::CloseOnLeave(false)];
        assert!(s.want(wanted));
        let diff = s.diff(&s.targets[0]);
        assert_eq!(diff, vec![Update::Throttle(Throttle::Off)], "only the changed key");
        s.delivered(7, &diff);
        assert!(s.diff(&s.targets[0]).is_empty());
        assert!(s.unregister(7).is_some());
        assert!(s.plan().is_empty());
    }

    #[test]
    fn an_unreachable_client_is_retried_for_a_while_then_left_until_the_values_change() {
        let mut s = State::default();
        s.register(1, session("b"), vec![Update::CloseOnLeave(false)], None, false);
        assert!(s.want(vec![Update::CloseOnLeave(true)]));
        for _ in 0..GIVE_UP_AFTER {
            assert!(s.outstanding());
            s.failed(1);
        }
        assert!(!s.outstanding(), "given up");
        assert!(s.plan().is_empty());
        // New wanted values start the count again.
        assert!(s.want(vec![Update::CloseOnLeave(false), Update::Throttle(Throttle::Off)]));
    }

    #[test]
    fn a_key_the_runtime_declares_unsupported_is_not_sent_to_it() {
        let mut s = State::default();
        s.register(2, session("c"), vec![Update::Gamepad(true), Update::Throttle(Throttle::Visible)], None, false);
        s.targets[0].declared.insert("gamepad".into(), Declared::Unsupported);
        s.want(vec![Update::Gamepad(false), Update::Throttle(Throttle::Off)]);
        assert_eq!(s.diff(&s.targets[0]), vec![Update::Throttle(Throttle::Off)], "gamepad is the runtime's to refuse, and it did");
    }

    #[test]
    fn a_next_launch_key_is_reported_from_the_runtimes_declaration_and_not_sent() {
        let mut s = State::default();
        s.register(3, session("d"), Vec::new(), None, false);
        s.targets[0].declared.insert("present_mode".into(), Declared::NextLaunch);
        s.targets[0].runtime = Some("org.example.rt 1".into());
        let mut config = ShellConfig::default();
        s.note_changes(&serde_json::to_value(&config).unwrap());
        assert!(s.notices.is_empty(), "the first config is a baseline and reports nothing");

        config.present_mode = shell_config::PresentMode::Fifo;
        s.note_changes(&serde_json::to_value(&config).unwrap());
        let n = s.notices.back().expect("a next-launch change is reported");
        assert_eq!((n.pid, n.key.as_str()), (3, "present_mode"));
        assert!(n.text.starts_with("applies at next launch") && n.text.contains("org.example.rt 1 declares it"), "{n:?}");

        // Control: a live key moving reports nothing, because it was sent and
        // took effect; a next-launch notice for it would be a lie the other way.
        let before = s.notices.len();
        config.throttle = shell_config::ThrottleWhen::Off;
        s.note_changes(&serde_json::to_value(&config).unwrap());
        assert_eq!(s.notices.len(), before, "{:?}", s.notices);
    }

    #[test]
    fn the_running_summary_is_the_runtimes_declaration_in_words() {
        let mut s = State::default();
        s.register(9, session("g"), Vec::new(), None, false);
        assert!(summarize(&s.targets[0], None).is_none(), "nothing is said before the runtime has said how it takes settings");
        s.targets[0].runtime = Some("org.example.rt 2.0".into());
        s.targets[0].declared.insert("throttle".into(), Declared::Live);
        s.targets[0].declared.insert("present_mode".into(), Declared::NextLaunch);
        s.targets[0].declared.insert("gamepad".into(), Declared::Unsupported);
        let notice = Notice { pid: 9, key: "present_mode".into(), text: "applies at next launch (org.example.rt 2.0 declares it)".into() };
        let r = summarize(&s.targets[0], Some(&notice)).unwrap();
        assert_eq!(r.title, "org.example.rt 2.0 is running (pid 9)");
        assert_eq!(
            r.detail,
            "Applies now: Throttle. Applies at next launch: Present mode. Not used by this runtime: Gamepad. Present mode applies at next launch."
        );
    }

    #[test]
    fn without_a_declaration_the_shells_own_table_decides() {
        let mut s = State::default();
        s.register(4, session("e"), Vec::new(), None, false);
        let mut config = ShellConfig::default();
        s.note_changes(&serde_json::to_value(&config).unwrap());
        config.mangohud = !config.mangohud;
        s.note_changes(&serde_json::to_value(&config).unwrap());
        assert_eq!(s.notices.back().map(|n| n.key.as_str()), Some("mangohud"), "{:?}", s.notices);
    }

    #[test]
    fn a_runtime_that_reports_what_it_runs_replaces_what_the_environment_was_assumed_to_give() {
        // `linked` is exercised against a real runtime in cordial-runtime's
        // control tests; here, the part that needs no socket.
        let mut s = State::default();
        s.register(5, session("f"), vec![Update::Throttle(Throttle::Visible)], None, false);
        s.want(vec![Update::Throttle(Throttle::Visible)]);
        assert!(s.diff(&s.targets[0]).is_empty(), "the launch environment already says Visible");
        s.targets[0].sent = vec![Update::Throttle(Throttle::Off)];
        assert_eq!(s.diff(&s.targets[0]), vec![Update::Throttle(Throttle::Visible)], "the runtime said Off, so Visible is a change");
    }

    /// Run `ctx` until `done()` or `limit` passes.
    fn spin(ctx: &glib::MainContext, limit: Duration, done: impl Fn() -> bool) {
        let end = std::time::Instant::now() + limit;
        while std::time::Instant::now() < end && !done() {
            ctx.iteration(false);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_rename_save_is_noticed_once_and_keeps_being_noticed() {
        let dir = scratch("rename");
        let path = dir.join("shell.json");
        std::fs::write(&path, b"{}").unwrap();
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let hits = Rc::new(Cell::new(0u32));
            let counter = Rc::clone(&hits);
            let _watch = watch_file(&path, move || counter.set(counter.get() + 1)).unwrap();

            // Let the monitor come up before the first save.
            spin(&ctx, Duration::from_millis(200), || false);

            // The way an editor saves: a temporary file renamed over the target.
            for round in 1..=3u32 {
                let tmp = dir.join(".shell.json.swp");
                std::fs::write(&tmp, format!("{{\"round\":{round}}}")).unwrap();
                std::fs::rename(&tmp, &path).unwrap();
                spin(&ctx, Duration::from_secs(3), || hits.get() >= round);
                assert_eq!(
                    hits.get(),
                    round,
                    "save {round} should settle to one callback (a file watch would have died \
                     after the first rename)"
                );
                // Let any stragglers arrive: the burst must not fire twice.
                spin(&ctx, Duration::from_millis(400), || false);
                assert_eq!(hits.get(), round, "save {round} fired more than once");
            }
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_burst_of_writes_collapses_into_one_callback_and_other_files_are_ignored() {
        let dir = scratch("burst");
        let path = dir.join("shell.json");
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let hits = Rc::new(Cell::new(0u32));
            let counter = Rc::clone(&hits);
            let _watch = watch_file(&path, move || counter.set(counter.get() + 1)).unwrap();
            spin(&ctx, Duration::from_millis(200), || false);

            std::fs::write(dir.join("unrelated.json"), b"x").unwrap();
            spin(&ctx, Duration::from_millis(500), || false);
            assert_eq!(hits.get(), 0, "another file in the directory is not shell.json");

            for i in 0..5 {
                std::fs::write(&path, format!("{{\"n\":{i}}}")).unwrap();
                spin(&ctx, Duration::from_millis(20), || false);
            }
            spin(&ctx, Duration::from_secs(2), || hits.get() >= 1);
            spin(&ctx, Duration::from_millis(500), || false);
            assert_eq!(hits.get(), 1, "five writes inside the debounce window are one read");
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
