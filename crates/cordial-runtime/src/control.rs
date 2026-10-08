//! The runtime's end of `cordial.runtime/1` (ADR-055, `docs/runtime-spec.md`).
//!
//! The launcher and this process are two programs, and this is the channel they
//! talk over: JSON lines on a Unix socket, `ctl.sock`, in a `0700` session
//! directory under the profile. **The runtime listens and the launcher
//! connects**, because the launcher can be restarted while a game runs and has
//! to find the same runtime again. That is the whole of why this is a path and
//! not a descriptor handed over at spawn.
//!
//! What it serves, and what it will not:
//!
//! * `hello`, and version negotiation: the live set is the intersection of the
//!   two sides' capabilities, each at the lower version, and a request for
//!   anything outside it is `unsupported`. A request before `hello` is
//!   `not_ready`.
//! * `settings.set` and `settings.get` over the closed key set, through
//!   [`crate::live_settings::apply`], which is the same code the version-0
//!   socket reached, so one setting has one implementation however it arrives.
//!   `settings.get` carries the per-key declaration (`live` or `next-launch`),
//!   which ADR-044's table used to hold only in the launcher.
//! * `lifecycle.stop`, mapped to [`crate::android::looper::request_quit`], the
//!   door the window's close button and `close_on_leave` already use. The grace
//!   is the launcher's: it sends `SIGTERM` after it, and `SIGTERM` lands on the
//!   same flag.
//! * `state.get`: the latest value of each event below, for a launcher that
//!   attached late or reattached.
//! * Events: `game.joined`, `game.left`, `session.state`, `engine.version` and
//!   `game.presence`. Nothing else is declared, because a declared event that
//!   nothing publishes is a stub that lies. `lifecycle.ready` and `health` are
//!   not sent: the engine has no ready signal anyone publishes, and the
//!   launcher's freeze recovery reads the engine's log itself.
//!
//! **Events never block an engine thread.** They are produced on the log
//! watcher's thread and on whichever thread resolves an icon, and go into
//! [`EventQueue`], whose `push` takes a lock for a `VecDeque` operation and
//! returns. At 256 waiting it drops the newest and counts, and the count goes
//! out as `events.dropped`. A pump thread of this module's own does the
//! blocking write, with a timeout, so a launcher that stops reading costs a
//! dropped connection and not a stalled engine.
//!
//! **Closing the socket does not stop the runtime.** The launcher closing its
//! window is the ordinary case (ADR-031), and a launcher that crashes should
//! not take a game with it. On EOF the connection thread ends and the listener
//! keeps listening; the next `hello` is a reattach. A newer controller replaces
//! an older one, which is sent `bye {reason:"superseded"}`.
//!
//! **A line that does not parse closes the connection, never the runtime.** A
//! frame that parses but carries an out-of-range value is dropped and counted,
//! and the connection stays, which is the split `docs/runtime-spec.md` section 9
//! draws.
//!
//! Nothing here can run anything, read a file on a peer's say-so or reach the
//! engine: the verb set is closed (ADR-001, ADR-003). It is also not `devctl`,
//! which is opt-in and can inject input and capture frames (ADR-019) and is not
//! part of the protocol. Access is by directory: `0700`, inside a profile the
//! plugin sandbox does not bind.

use cordial_protocol::msg::{self, caps, names, to_value, ClientIdent, Hello, HelloReply, LifecycleStop, RuntimeIdent};
use cordial_protocol::settings::{Applies, Update};
use cordial_protocol::{
    decode_line, encode_line, negotiate, socket, Capabilities, Code, DecodeError, Event, EventQueue, Frame, LineReader,
    Next, Protocol, Reply, Request,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The id this runtime gives in its handshake, and the one the launcher expects
/// of the built-in runtime. A manifest's `id` is what the launcher trusts for
/// keyring entries and its report, never this one (spec section 1); the built-in
/// runtime has no manifest, so the launcher holds the same constant.
pub use cordial_shell::runtime_session::BUILTIN_RUNTIME_ID as RUNTIME_ID;

/// The environment variable the launcher passes the session directory in. The
/// built-in runtime keeps env and argv for launch configuration in version 1
/// (spec section 6); this is the one value that is a path the launcher chose.
pub const SESSION_DIR_ENV: &str = "CORDIAL_SESSION_DIR";

/// How long a connection may sit without saying `hello`. Several can wait at
/// once, and the accept loop is not held by any of them, but an idle peer that
/// never handshakes should not live for ever.
const HELLO_WITHIN: Duration = Duration::from_secs(5);

/// A peer that takes longer than this to accept a write is not reading. The
/// pump stalls for at most this long, during which the queue fills and drops;
/// the engine is never in this path.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Connections that have not yet said `hello`, at most. A local peer opening
/// them for no reason costs a thread each otherwise.
const MAX_UNSHAKEN: usize = 8;

// ---- what the runtime can do, behind a seam ---------------------------------

/// What a connection asks of the process it is in. The seam exists so the
/// protocol handling can be tested, and the conformance harness run against it,
/// without applying a setting to the test process's globals or asking the test
/// binary to quit.
pub trait Backend: Send + Sync + 'static {
    /// Apply one setting. The `Some` is something worth telling the launcher
    /// that is not a failure.
    fn apply(&self, update: &Update) -> Option<String>;
    /// The values in force, in the wire's own words.
    fn current(&self) -> BTreeMap<String, Value>;
    /// How each key reaches a running game on this runtime.
    fn declared(&self) -> BTreeMap<String, Applies>;
    /// Ask the pump to stop, through the door every other way out uses.
    fn stop(&self);
    /// The engine client this runtime drives.
    fn client(&self) -> ClientIdent;
}

/// The real thing: this process.
struct Engine;

impl Backend for Engine {
    fn apply(&self, update: &Update) -> Option<String> {
        crate::live_settings::apply(update)
    }

    fn current(&self) -> BTreeMap<String, Value> {
        crate::live_settings::current()
    }

    fn declared(&self) -> BTreeMap<String, Applies> {
        declared_settings()
    }

    fn stop(&self) {
        say("lifecycle.stop: asking the pump to stop");
        crate::android::looper::request_quit();
    }

    fn client(&self) -> ClientIdent {
        ClientIdent {
            name: "Roblox".into(),
            // Set once the engine's version has been read from the binary, which
            // is after the socket is listening; a launcher that asks sooner is
            // told so rather than given a guess, and `engine.version` follows.
            version: std::env::var("CORDIAL_ENGINE_VERSION").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "unknown".into()),
            build: format!("android-{}", std::env::consts::ARCH),
        }
    }
}

/// ADR-044's classification, as this runtime declares it.
///
/// The ten keys the wire carries are live. The rest are the launch-environment
/// settings this process reads once, which a running game cannot take: they are
/// listed so the launcher can say "next launch" from the runtime's own word and
/// not from its private copy of the table, and they are not in
/// `cordial_protocol::settings::KEYS`, so `settings.set` cannot carry them and
/// reports them in `ignored` like any other key it does not take.
pub fn declared_settings() -> BTreeMap<String, Applies> {
    let mut m: BTreeMap<String, Applies> =
        cordial_protocol::settings::KEYS.iter().map(|k| (k.to_string(), Applies::Live)).collect();
    for key in NEXT_LAUNCH_KEYS {
        m.insert(key.to_string(), Applies::NextLaunch);
    }
    m
}

/// The keys this runtime reads from its launch environment and cannot change in
/// a running game, from ADR-044's table: the graphics backend and device
/// profile are settled before engine initialisation, the present mode is a
/// swapchain field only the engine rebuilds, the Vulkan layers load at instance
/// creation, and the unpacked-plugin list is built once (ADR-038).
pub const NEXT_LAUNCH_KEYS: [&str; 6] =
    ["graphics", "graphics_optimization_mode", "present_mode", "mangohud", "vkbasalt", "unpacked_plugins"];

fn our_caps() -> Capabilities {
    [caps::LIFECYCLE, caps::EVENTS_CORE, caps::EVENTS_PRESENCE, caps::STATE, caps::SETTINGS]
        .iter()
        .map(|c| (c.to_string(), 1))
        .collect()
}

// ---- shared state -----------------------------------------------------------

/// One controller: the connection the pump writes events to.
struct Controller {
    generation: u64,
    writer: Arc<Mutex<UnixStream>>,
    /// The negotiated set, so an event whose capability was not agreed is not
    /// sent to a launcher that never asked for it.
    live: Capabilities,
}

pub struct Shared {
    dir: PathBuf,
    queue: EventQueue,
    state: Mutex<msg::StateSnapshot>,
    controller: Mutex<Option<Controller>>,
    generation: AtomicU64,
    /// Connections waiting to say `hello`.
    unshaken: AtomicU64,
    /// Frames dropped for carrying an out-of-range value, for the report.
    rejected: AtomicU64,
}

impl Shared {
    fn new(dir: PathBuf) -> Arc<Shared> {
        Arc::new(Shared {
            dir,
            queue: EventQueue::new(),
            state: Mutex::new(msg::StateSnapshot::default()),
            controller: Mutex::new(None),
            generation: AtomicU64::new(0),
            unshaken: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, msg::StateSnapshot> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn controller(&self) -> std::sync::MutexGuard<'_, Option<Controller>> {
        self.controller.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Make `writer` the controller, replacing and telling the one before it.
    fn attach(&self, writer: Arc<Mutex<UnixStream>>, live: Capabilities) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let old = self.controller().replace(Controller { generation, writer, live });
        if let Some(old) = old {
            say("controller superseded by a newer one");
            let bye = Event::new(names::BYE, self.queue.take_number(), serde_json::json!({ "reason": "superseded" }));
            send_and_close(&old.writer, &bye);
        }
        generation
    }

    /// Forget the controller if it is still the one that ended.
    fn detach(&self, generation: u64) {
        let mut slot = self.controller();
        if slot.as_ref().is_some_and(|c| c.generation == generation) {
            *slot = None;
        }
    }

    // ---- publishing: update the snapshot, then queue the event ----

    fn publish(&self, name: &str, payload: Value) {
        if !self.queue.push(name, payload) {
            // Counted by the queue and reported to the launcher as
            // `events.dropped`; not narrated here, because a launcher that has
            // stopped reading is exactly when a line per event would flood.
        }
    }

    fn game_joined(&self, joined: msg::GameJoined, session: msg::SessionState) {
        let session_changed = {
            let mut st = self.state();
            st.game_joined = Some(joined.clone());
            // A new join supersedes a previous `left`: the snapshot says where
            // the game is now, not the order things happened in.
            st.game_left = None;
            let changed = st.session_state.as_ref() != Some(&session);
            st.session_state = Some(session.clone());
            changed
        };
        self.publish(names::GAME_JOINED, to_value(&joined));
        if session_changed {
            self.publish(names::SESSION_STATE, to_value(&session));
        }
    }

    fn game_left(&self, left: msg::GameLeft) {
        {
            let mut st = self.state();
            st.game_left = Some(left.clone());
            st.game_joined = None;
            // Leaving an experience clears what was said about it; whether
            // somebody is signed in is not about the experience and stays.
            st.game_presence = None;
        }
        self.publish(names::GAME_LEFT, to_value(&left));
    }

    fn engine_version(&self, version: String) {
        let ev = msg::EngineVersion { version };
        self.state().engine_version = Some(ev.clone());
        self.publish(names::ENGINE_VERSION, to_value(&ev));
    }

    fn game_presence(&self, presence: msg::GamePresence) {
        self.state().game_presence = Some(presence.clone());
        self.publish(names::GAME_PRESENCE, to_value(&presence));
    }
}

/// Write one event and shut the connection, for a controller that is being
/// replaced or a runtime that is exiting. Bounded by the write timeout.
fn send_and_close(writer: &Arc<Mutex<UnixStream>>, event: &Event) {
    let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
    let _ = w.write_all(encode_line(&Frame::Event(event.clone())).as_bytes());
    let _ = w.flush();
    let _ = w.shutdown(std::net::Shutdown::Both);
}

/// A line on the client's own stdout that cannot panic. `println!` does when
/// stdout has gone, and the whole point of a socket that outlives its launcher
/// is a process that keeps going after the pipe it was started with closed.
fn say(line: &str) {
    let _ = writeln!(std::io::stdout().lock(), "  ctl: {line}");
}

// ---- the server -------------------------------------------------------------

static SERVER: OnceLock<Server> = OnceLock::new();

/// A running server: the listener thread, the pump, and the shared state.
#[derive(Clone)]
pub struct Server {
    shared: Arc<Shared>,
}

/// Where this process's session directory is: the one the launcher made and
/// named in [`SESSION_DIR_ENV`], or, for a client started by hand, a new one
/// under the profile. Either way the socket in it is `ctl.sock`.
fn session_dir() -> PathBuf {
    match std::env::var_os(SESSION_DIR_ENV).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => crate::profile::active().join("runtime").join(socket::session_id()),
    }
}

/// Start serving for this process. Failure is reported and ignored: a runtime
/// that cannot take a controller still plays, and the launcher then has no
/// channel and says so, rather than the game refusing to start.
///
/// Called where `live_settings::start` was, and nowhere earlier: the profile
/// has latched by then, and the profile lock has been claimed, so no other
/// runtime owns the directory a stale session is cleaned out of.
pub fn start() {
    if SERVER.get().is_some() {
        return;
    }
    let dir = session_dir();
    match serve_in(&dir, Arc::new(Engine)) {
        Ok(server) => {
            say(&format!("cordial.runtime/1 listening on {}", socket::socket_path(&dir).display()));
            let _ = SERVER.set(server);
        }
        Err(e) => say(&format!("could not listen in {} ({e}); the launcher has no channel to this client", dir.display())),
    }
}

/// Serve in `dir`, with `backend` answering. Returns once the socket is bound;
/// the accept loop and the event pump run on their own threads.
pub fn serve_in(dir: &Path, backend: Arc<dyn Backend>) -> std::io::Result<Server> {
    socket::prepare_dir(dir)?;
    remove_stale_siblings(dir);
    let listener = socket::bind(dir)?;
    let shared = Shared::new(dir.to_path_buf());
    let server = Server { shared: shared.clone() };

    {
        let (shared, backend) = (shared.clone(), backend.clone());
        std::thread::Builder::new().name("cordial-ctl".into()).spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    // Out of descriptors, say: back off and keep listening. A
                    // runtime that gives up listening cannot be reattached to.
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                };
                if shared.unshaken.load(Ordering::SeqCst) as usize >= MAX_UNSHAKEN {
                    drop(stream);
                    continue;
                }
                shared.unshaken.fetch_add(1, Ordering::SeqCst);
                let (shared, backend) = (shared.clone(), backend.clone());
                let spawned = std::thread::Builder::new().name("cordial-ctl-conn".into()).spawn({
                    let shared = shared.clone();
                    move || serve_connection(&shared, &*backend, stream)
                });
                if spawned.is_err() {
                    shared.unshaken.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })?;
    }

    {
        let shared = shared.clone();
        std::thread::Builder::new().name("cordial-ctl-events".into()).spawn(move || pump(&shared))?;
    }
    Ok(server)
}

impl Server {
    pub fn dir(&self) -> &Path {
        &self.shared.dir
    }

    pub fn publish_game_joined(&self, joined: msg::GameJoined, session: msg::SessionState) {
        self.shared.game_joined(joined, session);
    }

    pub fn publish_game_left(&self, left: msg::GameLeft) {
        self.shared.game_left(left);
    }

    pub fn publish_engine_version(&self, version: String) {
        self.shared.engine_version(version);
    }

    pub fn publish_game_presence(&self, presence: msg::GamePresence) {
        self.shared.game_presence(presence);
    }

    /// Frames dropped for being out of range since this server started.
    pub fn rejected(&self) -> u64 {
        self.shared.rejected.load(Ordering::SeqCst)
    }

    /// Whether a controller is attached.
    pub fn controlled(&self) -> bool {
        self.shared.controller().is_some()
    }

    /// Say goodbye to the controller and take the session directory away. A
    /// runtime that exits cleanly may send `bye` first and the launcher records
    /// it (spec section 4); removing the directory is what stops the next
    /// launch finding a socket nobody listens on.
    pub fn shut_down(&self) {
        let controller = self.shared.controller().take();
        if let Some(c) = controller {
            let bye = Event::new(names::BYE, self.shared.queue.take_number(), serde_json::json!({ "reason": "exit" }));
            send_and_close(&c.writer, &bye);
        }
        let _ = std::fs::remove_dir_all(&self.shared.dir);
    }
}

/// A leftover session from a runtime that was killed. The profile lock the
/// caller holds is what makes removing them safe: nobody else is running in this
/// profile. Only directories named the way [`socket::session_id`] names them are
/// touched, and never `keep`.
fn remove_stale_siblings(keep: &Path) {
    let Some(runtime_dir) = keep.parent() else { return };
    // Only inside a directory called `runtime`, which is where a launcher and
    // this module both put sessions. `CORDIAL_SESSION_DIR` is trusted as a path
    // the launcher chose, not as a licence to sweep whatever directory it
    // happens to sit in.
    if runtime_dir.file_name().and_then(|n| n.to_str()) != Some("runtime") {
        return;
    }
    let Ok(entries) = std::fs::read_dir(runtime_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let named = path.file_name().and_then(|n| n.to_str()).is_some_and(socket::is_session_id);
        if path != keep && named && path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

// ---- the process-wide publishers --------------------------------------------

/// Called from the log watcher when the engine's log says a game was joined.
/// A no-op when no server is running, so it can sit in `game_log` without
/// caring whether this process is one.
pub fn game_joined(state: &cordial_plugins::state::SessionState) {
    let (Some(server), Some(place_id)) = (SERVER.get(), state.place_id) else { return };
    let joined = msg::GameJoined {
        place_id,
        universe_id: state.universe_id,
        job_id: state.job_id.clone(),
        // The log watcher keeps seconds; the wire is milliseconds.
        at: state.joined_at.map(|s| s * 1000).unwrap_or_else(now_ms),
    };
    // The log line that says a game was joined names the user, which is the
    // only way this runtime ever learns somebody is signed in. There is no
    // source for "signed out", so it is never claimed.
    let session = msg::SessionState { signed_in: state.user_id.is_some(), user_id: state.user_id };
    server.publish_game_joined(joined, session);
}

pub fn game_left() {
    if let Some(server) = SERVER.get() {
        server.publish_game_left(msg::GameLeft { at: now_ms() });
    }
}

/// Mirror a core event that plugins also receive. Called from
/// `plugin_host::publish_core`, which every producer already goes through, so
/// there is one place that knows which of them the launcher wants. Events with
/// no protocol counterpart (`client.launch` and `client.shutdown` are the
/// launcher's own to synthesise) fall through.
pub fn mirror_core_event(name: &str, payload: &Value) {
    let Some(server) = SERVER.get() else { return };
    match name {
        cordial_plugins::core_events::ENGINE_VERSION => {
            if let Some(v) = payload.get("version").and_then(Value::as_str).filter(|v| !v.is_empty()) {
                server.publish_engine_version(bound(v));
            }
        }
        cordial_plugins::core_events::GAME_PRESENCE => server.publish_game_presence(presence_from(payload)),
        cordial_plugins::core_events::CLIENT_SHUTDOWN => server.shut_down(),
        _ => {}
    }
}

/// The wire's presence from the plugin payload. The payload also carries
/// `place_id` and `job_id`, which the wire's presence does not, and strings the
/// protocol bounds at 512 bytes, so the projection is typed and truncated here
/// rather than leaving the launcher to drop a whole presence for one long
/// `details`.
fn presence_from(payload: &Value) -> msg::GamePresence {
    let text = |k: &str| payload.get(k).and_then(Value::as_str).map(bound);
    msg::GamePresence {
        details: text("details"),
        state: text("state"),
        start: payload.get("start").and_then(Value::as_i64),
        end: payload.get("end").and_then(Value::as_i64),
        large_image_key: text("large_image_key"),
        large_text: text("large_text"),
        small_image_key: text("small_image_key"),
        small_text: text("small_text"),
    }
}

/// `s` cut to the protocol's string bound at a character boundary.
fn bound(s: &str) -> String {
    let max = cordial_protocol::MAX_STRING;
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---- the event pump ---------------------------------------------------------

/// Drain the queue to the controller. With no controller attached the events are
/// let go: the snapshot already holds what they said and a launcher that attaches
/// asks `state.get`. They still use up their numbers, so the counter carries on
/// across a reattach and never repeats.
fn pump(shared: &Shared) {
    loop {
        let Some(event) = shared.queue.pop_timeout(Duration::from_secs(5)) else { continue };
        let target = shared.controller().as_ref().map(|c| (c.generation, c.writer.clone(), msg::offered(&event.ev, &c.live)));
        let Some((generation, writer, offered)) = target else { continue };
        if !offered {
            continue;
        }
        let line = encode_line(&Frame::Event(event));
        let sent = {
            let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
            w.write_all(line.as_bytes()).and_then(|()| w.flush())
        };
        if sent.is_err() {
            // Not reading, or gone. Ending this controller leaves the runtime
            // listening for the launcher to come back.
            say("a controller stopped reading; dropping it");
            let mut slot = shared.controller();
            if slot.as_ref().is_some_and(|c| c.generation == generation) {
                if let Some(c) = slot.take() {
                    let _ = c.writer.lock().unwrap_or_else(|e| e.into_inner()).shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }
}

// ---- one connection ---------------------------------------------------------

/// Clears the pre-handshake count however a connection ends.
struct Unshaken<'a>(&'a Shared, bool);

impl Unshaken<'_> {
    fn shaken(&mut self) {
        if std::mem::replace(&mut self.1, false) {
            self.0.unshaken.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for Unshaken<'_> {
    fn drop(&mut self) {
        self.shaken();
    }
}

fn serve_connection(shared: &Shared, backend: &dyn Backend, stream: UnixStream) {
    let mut unshaken = Unshaken(shared, true);
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    // Short reads until the handshake, so a peer that never speaks is noticed.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let Ok(write_half) = stream.try_clone() else { return };
    let writer = Arc::new(Mutex::new(write_half));
    let mut lines = LineReader::new(stream);
    let opened = Instant::now();
    let mut conn = Conn { shaken: None, refused: false };

    let send = |frame: Frame| -> bool {
        let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(encode_line(&frame).as_bytes()).and_then(|()| w.flush()).is_ok()
    };

    loop {
        let line = match lines.next_line() {
            Ok(Next::Line(l)) => l,
            // The launcher closed, or its window did. The runtime carries on.
            Ok(Next::Eof) => break,
            // A line that is not a frame at all closes this connection.
            Ok(Next::Truncated | Next::TooLong | Next::NotUtf8) => break,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
                if conn.shaken.is_none() && opened.elapsed() > HELLO_WITHIN {
                    break;
                }
                continue;
            }
            Err(_) => break,
        };
        let frame = match decode_line(&line) {
            Ok(f) => f,
            Err(DecodeError::Limit(why)) => {
                // Well-formed, out of range: dropped and counted, connection kept.
                shared.rejected.fetch_add(1, Ordering::SeqCst);
                say(&format!("dropped an over-limit frame: {why}"));
                continue;
            }
            Err(_) => break,
        };
        match frame {
            Frame::Request(req) => {
                let (reply, handshook) = conn.handle(shared, backend, &req);
                if !send(Frame::Reply(reply)) {
                    break;
                }
                if let Some(live) = handshook {
                    // Attached only after the reply is on the wire, so the pump
                    // cannot put an event ahead of the handshake's own answer.
                    unshaken.shaken();
                    let _ = lines.get_ref().set_read_timeout(None);
                    if let Some(s) = &mut conn.shaken {
                        s.generation = shared.attach(writer.clone(), live);
                    }
                }
                if conn.refused {
                    break;
                }
            }
            // `bye` is the one event either side may send.
            Frame::Event(ev) if ev.ev == names::BYE => break,
            // Version 1 defines no launcher-to-runtime events and no
            // runtime-to-launcher requests, so a reply or another event has
            // nothing to be matched with and is ignored.
            Frame::Event(_) | Frame::Reply(_) => {}
        }
    }
    if let Some(s) = &conn.shaken {
        shared.detach(s.generation);
    }
    let _ = writer.lock().unwrap_or_else(|e| e.into_inner()).shutdown(std::net::Shutdown::Both);
}

/// What a connection learned at its handshake.
struct Shaken {
    generation: u64,
    live: Capabilities,
}

struct Conn {
    shaken: Option<Shaken>,
    /// Set when the handshake itself failed and the connection should end.
    refused: bool,
}

impl Conn {
    /// One request to one reply, and the negotiated set if this one completed
    /// the handshake.
    fn handle(&mut self, shared: &Shared, backend: &dyn Backend, req: &Request) -> (Reply, Option<Capabilities>) {
        let id = req.id;
        if req.m == names::HELLO {
            return self.hello(backend, req);
        }
        let Some(shaken) = &self.shaken else {
            return (Reply::err(id, Code::NotReady, "say hello first"), None);
        };
        // Defined by the spec but not negotiated, or not defined at all: the
        // same answer, and never a silent success.
        if !msg::offered(&req.m, &shaken.live) {
            return (Reply::err(id, Code::Unsupported, format!("{} is not offered", req.m)), None);
        }
        let reply = match req.m.as_str() {
            names::STATE_GET => msg::reply_ok(id, &*shared.state()),
            names::SETTINGS_GET => msg::reply_ok(
                id,
                &msg::SettingsGetReply { values: backend.current(), declared: backend.declared() },
            ),
            names::SETTINGS_SET => match msg::SettingsSet::from_payload(&req.p) {
                Err(why) => Reply::err(id, Code::Invalid, why.to_string()),
                Ok(set) => {
                    let mut applied = Vec::new();
                    let mut notes = BTreeMap::new();
                    for update in &set.updates {
                        if let Some(note) = backend.apply(update) {
                            notes.insert(update.key().to_string(), bound(&note));
                        }
                        applied.push(update.key().to_string());
                    }
                    msg::reply_ok(id, &msg::SettingsReply { applied, ignored: set.ignored, notes })
                }
            },
            names::LIFECYCLE_STOP => match req.params::<LifecycleStop>() {
                Err(why) => Reply::err(id, Code::Invalid, why.to_string()),
                Ok(_) => {
                    backend.stop();
                    Reply::ok(id, msg::empty())
                }
            },
            other => Reply::err(id, Code::Unsupported, format!("{other} is not served by this runtime")),
        };
        (reply, None)
    }

    fn hello(&mut self, backend: &dyn Backend, req: &Request) -> (Reply, Option<Capabilities>) {
        let id = req.id;
        if self.shaken.is_some() {
            return (Reply::err(id, Code::Invalid, "hello was already said on this connection"), None);
        }
        let hello = match req.params::<Hello>() {
            Ok(h) => h,
            Err(why) => {
                self.refused = true;
                return (Reply::err(id, Code::Invalid, why.to_string()), None);
            }
        };
        let ours = our_caps();
        let negotiated = match negotiate(Protocol::CURRENT, &ours, hello.protocol, &hello.caps) {
            Ok(n) => n,
            Err(why) => {
                // A launcher that speaks another major cannot be served, and is
                // told which, once.
                self.refused = true;
                return (Reply::err(id, Code::Unsupported, why.to_string()), None);
            }
        };
        let answer = HelloReply {
            protocol: Protocol::CURRENT,
            runtime: RuntimeIdent { id: RUNTIME_ID.into(), version: env!("CARGO_PKG_VERSION").into() },
            client: backend.client(),
            caps: ours,
        };
        let reply = msg::reply_ok(id, &answer);
        say(&format!(
            "controller attached (session {}, cordial {}, {}, caps {:?})",
            hello.session,
            hello.cordial,
            if hello.reattach { "reattach" } else { "first attach" },
            negotiated.caps.keys().collect::<Vec<_>>()
        ));
        self.shaken = Some(Shaken { generation: 0, live: negotiated.caps.clone() });
        (reply, Some(negotiated.caps))
    }
}

#[cfg(test)]
mod tests;
