//! Changing a running game's settings from Settings (ADR-044).
//!
//! The shell hands the client its settings once, in the launch environment.
//! That left every row saying "saved" while a game already running kept the old
//! value until the next launch. This module closes the gap for the settings
//! that can change: it notices `shell.json` change, works out which live keys
//! moved, and sends only those to each client this shell started, over the
//! socket `cordial_runtime::live_settings` listens on.
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
use cordial_protocol::settings::{Accel, Throttle};
use cordial_protocol::v0::{self, Reply};
use cordial_protocol::{LineReader, Next, Update};
use libadwaita::gio;
use libadwaita::glib;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
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

// ---- talking to a client ----------------------------------------------------

const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Send `updates` to the client listening at `socket` and check it took them
/// all. An `Err` means the caller should assume nothing changed.
pub fn send(socket: &Path, updates: &[Update]) -> Result<Reply, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).map_err(|e| e.to_string())?;
    stream
        .write_all(v0::encode_set(updates).as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    // Bounded: a client that answers with noise and no newline costs one cap's
    // worth of buffer and an error, not however long it keeps writing. The cap
    // is the protocol's 64 KiB, well above the biggest reply a client sends.
    let line = match LineReader::new(stream).next_line().map_err(|e| format!("read: {e}"))? {
        Next::Line(line) => line,
        Next::Eof => return Err("read: the client closed without replying".to_string()),
        Next::Truncated => return Err("read: the reply was cut off".to_string()),
        Next::TooLong => return Err("read: the reply was longer than the protocol allows".to_string()),
        Next::NotUtf8 => return Err("read: the reply was not UTF-8".to_string()),
    };
    let reply = Reply::decode(&line)?;
    if !reply.ok {
        return Err(reply.error.clone().unwrap_or_else(|| "refused".to_string()));
    }
    // A client that ignored a key it should know is an older client. That is
    // worth saying rather than counting as success.
    let missing: Vec<_> = updates
        .iter()
        .map(|u| u.key())
        .filter(|k| !reply.applied.iter().any(|a| a == k))
        .collect();
    if missing.is_empty() {
        Ok(reply)
    } else {
        Err(format!("the client did not apply {}", missing.join(", ")))
    }
}

// ---- what is running, and what it has been told ----------------------------

struct Target {
    pid: u32,
    socket: PathBuf,
    /// What this client is known to be running with.
    sent: Vec<Update>,
    /// Failed deliveries since the wanted values last changed.
    failures: u32,
}

/// A client that has not answered in this many tries stops being retried until
/// the wanted values change again. Once a second, so about half a minute: long
/// enough to cover the engine loading before the socket exists.
const GIVE_UP_AFTER: u32 = 30;

#[derive(Default)]
struct State {
    targets: Vec<Target>,
    wanted: Vec<Update>,
}

impl State {
    fn register(&mut self, pid: u32, socket: PathBuf, launched_with: Vec<Update>) {
        self.targets.retain(|t| t.pid != pid);
        self.targets.push(Target { pid, socket, sent: launched_with, failures: 0 });
    }

    fn unregister(&mut self, pid: u32) {
        self.targets.retain(|t| t.pid != pid);
    }

    /// Record the newest wanted values. `true` when anything needs sending.
    fn want(&mut self, wanted: Vec<Update>) -> bool {
        if wanted != self.wanted {
            self.wanted = wanted;
            for t in &mut self.targets {
                t.failures = 0;
            }
        }
        self.outstanding()
    }

    fn outstanding(&self) -> bool {
        self.targets
            .iter()
            .any(|t| t.failures < GIVE_UP_AFTER && !changed(&t.sent, &self.wanted).is_empty())
    }

    /// The deliveries to attempt now, as (pid, socket, only the changed keys).
    fn plan(&self) -> Vec<(u32, PathBuf, Vec<Update>)> {
        self.targets
            .iter()
            .filter(|t| t.failures < GIVE_UP_AFTER)
            .filter_map(|t| {
                let diff = changed(&t.sent, &self.wanted);
                (!diff.is_empty()).then(|| (t.pid, t.socket.clone(), diff))
            })
            .collect()
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
}

static STATE: Mutex<State> = Mutex::new(State { targets: Vec::new(), wanted: Vec::new() });
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

fn deliver_once() {
    // The lock is not held across the socket calls: a client that takes its
    // two-second timeout must not stop the GTK thread registering the next one.
    let plan = state().plan();
    for (pid, socket, updates) in plan {
        match send(&socket, &updates) {
            Ok(reply) => {
                state().delivered(pid, &updates);
                // What the client applied but could not fully do, such as a sink
                // change with nothing playing. Said here because the settings
                // window has no place to show it and "applied" alone would be
                // read as "you will hear it".
                for (key, note) in &reply.notes {
                    println!("  shell: pid {pid}: {key}: {note}");
                }
                println!(
                    "  shell: live settings -> pid {pid}: {}",
                    updates.iter().map(|u| u.key()).collect::<Vec<_>>().join(", ")
                );
            }
            Err(why) => {
                let n = state().failed(pid);
                // The first failure is expected while the engine is still
                // loading, so say nothing until it has gone on a while.
                if n == 5 || n == GIVE_UP_AFTER {
                    println!("  shell: live settings could not reach pid {pid} ({why}); try {n}");
                }
            }
        }
    }
}

/// Remember a client this shell started, with the values its environment gave
/// it. If the file has moved on since, the difference is sent as soon as its
/// socket answers.
pub fn register(pid: u32, socket: PathBuf, launched_with: Vec<Update>) {
    ensure_worker();
    let outstanding = {
        let mut s = state();
        s.register(pid, socket, launched_with);
        s.outstanding()
    };
    if outstanding {
        wake();
    }
}

pub fn unregister(pid: u32) {
    state().unregister(pid);
}

/// The values the shell currently wants running clients to have.
pub fn want(config: &ShellConfig) {
    ensure_worker();
    let outstanding = state().want(live_updates(config));
    if outstanding {
        wake();
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
    use std::io::{BufRead, BufReader};
    use std::io::Read;
    use std::os::unix::net::UnixListener;

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

    #[test]
    fn a_new_client_is_only_sent_what_differs_from_its_launch_environment() {
        let mut s = State::default();
        let launched = vec![Update::Throttle(Throttle::Visible), Update::CloseOnLeave(false)];
        s.register(7, PathBuf::from("/x"), launched.clone());
        // Nothing wanted differs: nothing outstanding.
        assert!(!s.want(launched.clone()));
        // The file moved on while the client was loading.
        let wanted = vec![Update::Throttle(Throttle::Off), Update::CloseOnLeave(false)];
        assert!(s.want(wanted));
        let plan = s.plan();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].2, vec![Update::Throttle(Throttle::Off)], "only the changed key");
        s.delivered(7, &plan[0].2);
        assert!(!s.outstanding());
        s.unregister(7);
        assert!(s.plan().is_empty());
    }

    #[test]
    fn an_unreachable_client_is_retried_for_a_while_then_left_until_the_values_change() {
        let mut s = State::default();
        s.register(1, PathBuf::from("/nope"), vec![Update::CloseOnLeave(false)]);
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

    /// A listener that plays the client's part, answering with the keys it was
    /// given, or with a refusal.
    fn fake_client(dir: &Path, refuse: bool) -> (PathBuf, std::thread::JoinHandle<String>) {
        let socket = v0::socket_path(dir);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            let reply = if refuse {
                Reply::failure("no")
            } else {
                match v0::decode(&line).unwrap() {
                    v0::Request::Set { updates, .. } => Reply {
                        ok: true,
                        applied: updates.iter().map(|u| u.key().to_string()).collect(),
                        ..Reply::default()
                    },
                    v0::Request::Get => Reply { ok: true, ..Reply::default() },
                }
            };
            stream.write_all(reply.encode().as_bytes()).unwrap();
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest);
            line
        });
        (socket, handle)
    }

    #[test]
    fn send_delivers_one_line_and_checks_the_client_took_every_key() {
        let dir = scratch("send");
        let (socket, seen) = fake_client(&dir, false);
        let updates = [Update::Throttle(Throttle::Off), Update::CloseOnLeave(true)];
        let reply = send(&socket, &updates).expect("delivered");
        assert_eq!(reply.applied.len(), 2);
        let line = seen.join().unwrap();
        assert_eq!(line.matches('\n').count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_does_not_buffer_a_reply_that_never_ends() {
        // A client that answers with more than the protocol's line cap and no
        // newline used to make `read_line` grow its buffer for as long as the
        // client kept writing. Now it costs one error.
        let dir = scratch("noise");
        let socket = v0::socket_path(&dir);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut request).unwrap();
            stream.write_all(&vec![b'x'; 70_000]).unwrap();
        });
        let err = send(&socket, &[Update::CloseOnLeave(true)]).unwrap_err();
        assert!(err.contains("cut off"), "{err}");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_reports_a_refusal_and_an_absent_socket() {
        let dir = scratch("refuse");
        let (socket, seen) = fake_client(&dir, true);
        assert_eq!(send(&socket, &[Update::CloseOnLeave(true)]).unwrap_err(), "no");
        let _ = seen.join();
        assert!(send(&dir.join("absent.sock"), &[Update::CloseOnLeave(true)])
            .unwrap_err()
            .starts_with("connect"));
        let _ = std::fs::remove_dir_all(&dir);
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
