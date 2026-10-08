//! The launcher's end of `cordial.runtime/1` (ADR-055, `docs/runtime-spec.md`).
//!
//! One [`Link`] is one persistent connection to one running runtime. The
//! runtime listens on `ctl.sock` in a session directory and the launcher
//! connects, which is the way round that lets a launcher that was restarted
//! find the same runtime again; [`Link::open`] is one attempt and the caller
//! retries while the runtime loads, because that is the delivery rule ADR-044
//! already used.
//!
//! **Everything the runtime sends is bounded and validated here**, because the
//! launcher is the trusted side and the runtime is a foreign process that may
//! be hostile (spec section 2). Lines are read through the crate's bounded
//! reader; a line that is not a frame closes the connection, a frame carrying an
//! out-of-range value is dropped and counted and the connection stays, an
//! event whose counter did not go up is dropped, and a handshake that names
//! another major, or another runtime than the one that was spawned, is refused.
//!
//! **No reply is a failure.** A request that gets none within two seconds is
//! reported as one and never as success, and an error reply carries the
//! runtime's own code and detail up to whoever asked.
//!
//! **A runtime that exits is learned from the child's wait status**, not from
//! here: a crashed process cannot report its own crash, and there is no
//! `lifecycle.exit`. What this type learns is that the *connection* ended,
//! which for a launcher whose window was closed or a runtime that was killed is
//! the same observation and means different things, so the two are kept apart
//! ([`Link::alive`], [`Link::bye`]).
//!
//! GTK-free, so the whole of it is tested against the real server in
//! `cordial-runtime` and against the conformance harness here.

use cordial_protocol::msg::{self, caps, names, ClientIdent, Hello, HelloReply, Kind, RuntimeIdent};
use cordial_protocol::settings::{Applies, Update};
use cordial_protocol::{
    decode_line, encode_line, negotiate, socket, Capabilities, DecodeError, Event, Frame, LineReader, Next, Protocol, Reply,
    Request,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The id the built-in runtime gives in its handshake. A manifest's `id` is what
/// the launcher trusts, never the one a runtime claims while running (spec
/// section 1); the built-in runtime has no manifest, so this constant is its
/// manifest's `id`, held by both programs.
pub const BUILTIN_RUNTIME_ID: &str = "io.github.luohoa97.cordial.android";

/// How long a request may go unanswered before it is a failure. The same two
/// seconds the version-0 client used.
pub const REPLY_WITHIN: Duration = Duration::from_secs(2);

/// What this launcher can serve, offered in `hello`. The live set is the
/// intersection with the runtime's.
pub fn launcher_caps() -> Capabilities {
    [caps::LIFECYCLE, caps::EVENTS_CORE, caps::EVENTS_PRESENCE, caps::STATE, caps::SETTINGS]
        .iter()
        .map(|c| (c.to_string(), 1))
        .collect()
}

// ---- the session directory ---------------------------------------------------

/// A session directory the launcher made for one runtime: `<profile>/runtime/
/// <session>/`, mode `0700`, named with eight characters so the socket path
/// stays short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDir {
    pub dir: PathBuf,
    pub id: String,
}

impl SessionDir {
    /// Make a fresh session directory under `profile_dir`.
    ///
    /// **The caller must hold the profile's lock**, which is what makes it safe
    /// to remove every other session directory first: with the lock held there
    /// is no other runtime in this profile, so whatever is under `runtime/` was
    /// left by one that was killed.
    pub fn create(profile_dir: &Path) -> std::io::Result<SessionDir> {
        let runtime_dir = profile_dir.join("runtime");
        remove_sessions(&runtime_dir);
        let id = socket::session_id();
        let dir = runtime_dir.join(&id);
        socket::prepare_dir(&dir)?;
        Ok(SessionDir { dir, id })
    }

    /// Take the directory away, once the runtime that owned it is gone. The
    /// runtime removes its own on a clean exit; this is for the ones that did
    /// not get to.
    pub fn remove(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every session directory under `runtime_dir`, named the way
/// [`socket::session_id`] names them. Anything else a user left there is not
/// touched.
pub fn list_sessions(runtime_dir: &Path) -> Vec<SessionDir> {
    let Ok(entries) = std::fs::read_dir(runtime_dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            (socket::is_session_id(&name) && e.path().is_dir()).then(|| SessionDir { dir: e.path(), id: name })
        })
        .collect()
}

fn remove_sessions(runtime_dir: &Path) {
    for s in list_sessions(runtime_dir) {
        s.remove();
    }
}

/// The process on the other end of a connected Unix socket, from `SO_PEERCRED`.
/// A launcher that finds a runtime it did not spawn has no child to watch, and
/// this is how it learns whose it is.
pub fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are valid for writes of their sizes for the
    // duration of the call, and the descriptor is the live socket borrowed here.
    let rc = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len)
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid as u32)
}

// ---- opening a link ---------------------------------------------------------

/// What the handshake and the first two questions established.
#[derive(Debug, Clone)]
pub struct Established {
    pub protocol: Protocol,
    pub runtime: RuntimeIdent,
    pub client: ClientIdent,
    /// The live set: what both sides have, each at the lower version.
    pub caps: Capabilities,
    /// How each key reaches a running game on this runtime. Empty when the
    /// runtime does not offer `settings`, or did not answer, and the launcher
    /// then falls back to its own table.
    pub declared: BTreeMap<String, Applies>,
    /// The settings in force when the link opened.
    pub values: BTreeMap<String, Value>,
    /// The process on the other end, when the kernel would say.
    pub peer_pid: Option<u32>,
}

#[derive(Debug)]
pub enum OpenError {
    /// There is no socket, or nothing is listening on it: the runtime is still
    /// loading, or is gone. Worth retrying for a while.
    Unreachable(std::io::Error),
    /// Something answered and the exchange failed: a refused handshake, another
    /// major, another runtime, no reply. Retrying will not change it.
    Failed(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Unreachable(e) => write!(f, "connect: {e}"),
            OpenError::Failed(why) => f.write_str(why),
        }
    }
}

/// How a link is opened.
pub struct OpenOptions {
    /// The session name given in `hello`, which is the directory's name.
    pub session: String,
    /// Set by a launcher that found the runtime already running.
    pub reattach: bool,
    /// The runtime id the manifest (or, for the built-in runtime, the launcher)
    /// says this should be. A handshake that names another is refused.
    pub expect_id: Option<String>,
    /// What this side can serve.
    pub offer: Capabilities,
    /// Called on the reader thread for every event that passed validation.
    pub on_event: Arc<dyn Fn(&Event) + Send + Sync>,
    /// Called once, on the reader thread, when the connection ends for any
    /// reason, so the owner can look at [`Link::alive`] without polling.
    pub on_close: Arc<dyn Fn() + Send + Sync>,
}

impl OpenOptions {
    pub fn new(session: impl Into<String>) -> Self {
        OpenOptions {
            session: session.into(),
            reattach: false,
            expect_id: Some(BUILTIN_RUNTIME_ID.to_string()),
            offer: launcher_caps(),
            on_event: Arc::new(|_| {}),
            on_close: Arc::new(|| {}),
        }
    }
}

struct Inner {
    writer: Mutex<UnixStream>,
    pending: Mutex<HashMap<u64, Sender<Reply>>>,
    next_id: AtomicU64,
    alive: AtomicBool,
    superseded: AtomicBool,
    bye: Mutex<Option<String>>,
    snapshot: Mutex<msg::StateSnapshot>,
    last_n: AtomicU64,
    /// Events whose numbers skipped: sent while no controller was attached, or
    /// dropped by the runtime's queue. The snapshot, not the stream, is the
    /// truth after one.
    missed: AtomicU64,
    /// What the runtime reported dropping, from `events.dropped`.
    reported_dropped: AtomicU64,
    /// Frames and events thrown away for breaking a bound or a rule.
    rejected: AtomicU64,
    events: AtomicU64,
}

/// One persistent connection to one runtime.
pub struct Link {
    inner: Arc<Inner>,
    established: Established,
}

impl Link {
    /// One attempt: connect to the runtime in `dir`, say `hello`, and ask for
    /// the snapshot and the settings if they are offered.
    pub fn open(dir: &Path, opts: OpenOptions) -> Result<Link, OpenError> {
        let stream = socket::connect(dir).map_err(OpenError::Unreachable)?;
        Link::open_stream(stream, opts)
    }

    /// As [`open`](Self::open), on a stream already connected.
    pub fn open_stream(stream: UnixStream, opts: OpenOptions) -> Result<Link, OpenError> {
        let failed = |why: String| OpenError::Failed(why);
        stream.set_read_timeout(Some(REPLY_WITHIN)).map_err(|e| failed(e.to_string()))?;
        stream.set_write_timeout(Some(REPLY_WITHIN)).map_err(|e| failed(e.to_string()))?;
        let peer = peer_pid(&stream);
        let mut write_half = stream.try_clone().map_err(|e| failed(e.to_string()))?;
        let mut lines = LineReader::new(stream);

        let hello = Hello {
            protocol: Protocol::CURRENT,
            cordial: env!("CARGO_PKG_VERSION").to_string(),
            session: opts.session.clone(),
            caps: opts.offer.clone(),
            reattach: opts.reattach,
        };
        let request = Frame::Request(msg::request(1, names::HELLO, &hello));
        write_half.write_all(encode_line(&request).as_bytes()).map_err(|e| failed(format!("hello: {e}")))?;

        // The handshake is read here, before the reader thread exists, so a
        // refusal can end the connection without anything else running. Nothing
        // but the reply is expected before it.
        let answer = loop {
            match lines.next_line() {
                Ok(Next::Line(line)) => match decode_line(&line) {
                    Ok(Frame::Reply(r)) if r.id == 1 => break r,
                    Ok(_) | Err(DecodeError::Limit(_)) => continue,
                    Err(e) => return Err(failed(format!("the runtime's first line was not a frame: {e}"))),
                },
                Ok(Next::Eof | Next::Truncated) => return Err(failed("the runtime closed without answering hello".into())),
                Ok(Next::TooLong) => return Err(failed("the runtime sent a line over 64 KiB".into())),
                Ok(Next::NotUtf8) => return Err(failed("the runtime sent a line that is not UTF-8".into())),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
                    return Err(failed("no reply to hello within two seconds".into()))
                }
                Err(e) => return Err(failed(format!("hello: {e}"))),
            }
        };
        let answer: HelloReply = match answer.result {
            Ok(p) => msg::payload(&p).map_err(|e| failed(format!("hello reply: {e}")))?,
            Err(e) => return Err(failed(format!("the runtime refused hello: {} ({})", e.code, e.detail))),
        };
        let negotiated = negotiate(Protocol::CURRENT, &opts.offer, answer.protocol, &answer.caps)
            .map_err(|e| failed(format!("{}: {e}", answer.runtime.id)))?;
        if let Some(expect) = &opts.expect_id {
            if &answer.runtime.id != expect {
                return Err(failed(format!(
                    "the runtime says it is {:?} and this launcher started {expect:?}, so it was refused",
                    answer.runtime.id
                )));
            }
        }

        let inner = Arc::new(Inner {
            writer: Mutex::new(write_half),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(2),
            alive: AtomicBool::new(true),
            superseded: AtomicBool::new(false),
            bye: Mutex::new(None),
            snapshot: Mutex::new(msg::StateSnapshot::default()),
            last_n: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            reported_dropped: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            events: AtomicU64::new(0),
        });
        // No timeout from here: a runtime that is up and quiet is the ordinary
        // case, and "no reply" is measured per request.
        let _ = lines.get_ref().set_read_timeout(None);
        {
            let (inner, on_event, on_close) = (inner.clone(), opts.on_event.clone(), opts.on_close.clone());
            std::thread::Builder::new()
                .name("cordial-ctl-read".into())
                .spawn(move || {
                    read_loop(&inner, lines, &*on_event);
                    inner.alive.store(false, Ordering::SeqCst);
                    // Whoever is waiting on a reply is told there is none.
                    inner.pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
                    on_close();
                })
                .map_err(|e| failed(format!("could not start the reader: {e}")))?;
        }

        let mut link = Link {
            inner,
            established: Established {
                protocol: negotiated.protocol,
                runtime: answer.runtime,
                client: answer.client,
                caps: negotiated.caps,
                declared: BTreeMap::new(),
                values: BTreeMap::new(),
                peer_pid: peer,
            },
        };
        // What the runtime already knows, for a launcher that attached late or
        // came back. A runtime that does not answer either is still a runtime;
        // the launcher then has less to show and says nothing it was not told.
        if link.offers(names::STATE_GET) {
            if let Ok(p) = link.call(names::STATE_GET, msg::empty()) {
                if let Ok(snap) = msg::payload::<msg::StateSnapshot>(&p) {
                    *link.inner.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = snap;
                }
            }
        }
        if link.offers(names::SETTINGS_GET) {
            if let Ok(p) = link.call(names::SETTINGS_GET, msg::empty()) {
                if let Ok(g) = msg::payload::<msg::SettingsGetReply>(&p) {
                    link.established.declared = g.declared;
                    link.established.values = g.values;
                }
            }
        }
        Ok(link)
    }

    /// Whether the handshake enabled `name`.
    pub fn offers(&self, name: &str) -> bool {
        msg::offered(name, &self.established.caps)
    }

    pub fn established(&self) -> &Established {
        &self.established
    }

    /// Whether the connection is still up.
    pub fn alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// A newer controller replaced this one. The runtime told it so, and a
    /// launcher that reconnected regardless would be two controllers each
    /// replacing the other.
    pub fn superseded(&self) -> bool {
        self.inner.superseded.load(Ordering::SeqCst)
    }

    /// Why the runtime said goodbye, if it did.
    pub fn bye(&self) -> Option<String> {
        self.inner.bye.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The runtime's state as the events since the link opened have folded it.
    pub fn snapshot(&self) -> msg::StateSnapshot {
        self.inner.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Ask for the snapshot afresh, which is what to do after a gap.
    pub fn refresh_snapshot(&self) -> Result<msg::StateSnapshot, String> {
        let snap: msg::StateSnapshot = msg::payload(&self.call(names::STATE_GET, msg::empty())?).map_err(|e| e.to_string())?;
        *self.inner.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = snap.clone();
        Ok(snap)
    }

    pub fn events_seen(&self) -> u64 {
        self.inner.events.load(Ordering::SeqCst)
    }

    /// Events the numbers show were not delivered.
    pub fn events_missed(&self) -> u64 {
        self.inner.missed.load(Ordering::SeqCst)
    }

    pub fn events_dropped_by_runtime(&self) -> u64 {
        self.inner.reported_dropped.load(Ordering::SeqCst)
    }

    /// Frames and events thrown away for breaking a bound.
    pub fn rejected(&self) -> u64 {
        self.inner.rejected.load(Ordering::SeqCst)
    }

    /// Send one request and wait for its reply, or two seconds. An error reply
    /// is `Err` with the runtime's code and detail, and so is no reply: never a
    /// success by default.
    pub fn call(&self, name: &str, payload: Value) -> Result<Value, String> {
        if !self.alive() {
            return Err("the connection to the runtime is closed".into());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        let line = encode_line(&Frame::Request(Request::new(id, name, payload)));
        {
            let mut w = self.inner.writer.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = w.write_all(line.as_bytes()).and_then(|()| w.flush()) {
                self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                return Err(format!("{name}: write: {e}"));
            }
        }
        match rx.recv_timeout(REPLY_WITHIN) {
            Ok(reply) => reply.result.map_err(|e| format!("{name}: {} ({})", e.code, e.detail)),
            Err(RecvTimeoutError::Timeout) => {
                self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                Err(format!("{name}: no reply within {} s", REPLY_WITHIN.as_secs()))
            }
            Err(RecvTimeoutError::Disconnected) => Err(format!("{name}: the connection closed before it was answered")),
        }
    }

    /// `settings.set`, and a check that the runtime took every key it was sent.
    /// A key it ignored is an older runtime, which is worth saying and is not
    /// success.
    pub fn settings_set(&self, updates: &[Update]) -> Result<msg::SettingsReply, String> {
        let p = self.call(names::SETTINGS_SET, msg::SettingsSet::new(updates.to_vec()).to_payload())?;
        let reply: msg::SettingsReply = msg::payload(&p).map_err(|e| format!("settings.set reply: {e}"))?;
        let missing: Vec<_> = updates.iter().map(|u| u.key()).filter(|k| !reply.applied.iter().any(|a| a == k)).collect();
        if missing.is_empty() {
            Ok(reply)
        } else {
            Err(format!("the runtime did not apply {}", missing.join(", ")))
        }
    }

    /// `settings.get`: what is in force, and how each key reaches a running game.
    pub fn settings_get(&self) -> Result<msg::SettingsGetReply, String> {
        let p = self.call(names::SETTINGS_GET, msg::empty())?;
        msg::payload(&p).map_err(|e| format!("settings.get reply: {e}"))
    }

    /// `lifecycle.stop`: ask for a clean exit. What follows is the caller's: it
    /// sends `SIGTERM` after `grace` and `SIGKILL` two seconds after that, and
    /// learns the result from the child's wait status.
    pub fn stop(&self, grace: Duration) -> Result<(), String> {
        self.call(names::LIFECYCLE_STOP, serde_json::json!({ "grace_ms": grace.as_millis() as u64 })).map(|_| ())
    }

    /// End the connection. The runtime keeps running.
    pub fn close(&self) {
        self.inner.alive.store(false, Ordering::SeqCst);
        let _ = self.inner.writer.lock().unwrap_or_else(|e| e.into_inner()).shutdown(std::net::Shutdown::Both);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}

fn read_loop(inner: &Inner, mut lines: LineReader<UnixStream>, on_event: &(dyn Fn(&Event) + Send + Sync)) {
    loop {
        let line = match lines.next_line() {
            Ok(Next::Line(l)) => l,
            // The runtime closed, or this side did.
            Ok(Next::Eof) => return,
            // A line that is not a frame at all closes the connection.
            Ok(Next::Truncated | Next::TooLong | Next::NotUtf8) => return,
            Err(_) => return,
        };
        match decode_line(&line) {
            Ok(Frame::Reply(r)) => {
                let waiter = inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&r.id);
                // A reply nobody asked for, or that arrived after its caller
                // gave up, is not an error of the runtime's to be punished for.
                if let Some(tx) = waiter {
                    let _ = tx.send(r);
                }
            }
            Ok(Frame::Event(ev)) => {
                if event_in(inner, &ev) {
                    on_event(&ev);
                }
                if ev.ev == names::BYE {
                    return;
                }
            }
            // Version 1 defines no runtime-to-launcher request; what a launcher
            // does with one is unspecified, so it is counted and left.
            Ok(Frame::Request(_)) => {
                inner.rejected.fetch_add(1, Ordering::SeqCst);
            }
            // Well-formed, over a bound: dropped and counted, connection kept.
            Err(DecodeError::Limit(_)) => {
                inner.rejected.fetch_add(1, Ordering::SeqCst);
            }
            Err(_) => return,
        }
    }
}

/// Validate an event and fold it into the snapshot. `false` for one that was
/// thrown away.
fn event_in(inner: &Inner, ev: &Event) -> bool {
    // The counter has to go up. One that does not is a runtime that is wrong or
    // replaying, and what it says is not trusted to be current.
    let last = inner.last_n.load(Ordering::SeqCst);
    if ev.n <= last {
        inner.rejected.fetch_add(1, Ordering::SeqCst);
        return false;
    }
    if last > 0 && ev.n > last + 1 {
        inner.missed.fetch_add(ev.n - last - 1, Ordering::SeqCst);
    }
    if msg::check(Kind::Event, &ev.ev, &ev.p).is_err() {
        inner.rejected.fetch_add(1, Ordering::SeqCst);
        // Still consumed, so the next one is not read as a gap.
        inner.last_n.store(ev.n, Ordering::SeqCst);
        return false;
    }
    inner.last_n.store(ev.n, Ordering::SeqCst);
    inner.events.fetch_add(1, Ordering::SeqCst);

    let mut snap = inner.snapshot.lock().unwrap_or_else(|e| e.into_inner());
    match ev.ev.as_str() {
        names::GAME_JOINED => {
            if let Ok(j) = ev.payload::<msg::GameJoined>() {
                snap.game_joined = Some(j);
                snap.game_left = None;
            }
        }
        names::GAME_LEFT => {
            if let Ok(l) = ev.payload::<msg::GameLeft>() {
                snap.game_left = Some(l);
                snap.game_joined = None;
                snap.game_presence = None;
            }
        }
        names::SESSION_STATE => snap.session_state = ev.payload::<msg::SessionState>().ok().or(snap.session_state.take()),
        names::ENGINE_VERSION => snap.engine_version = ev.payload::<msg::EngineVersion>().ok().or(snap.engine_version.take()),
        names::GAME_PRESENCE => snap.game_presence = ev.payload::<msg::GamePresence>().ok().or(snap.game_presence.take()),
        names::EVENTS_DROPPED => {
            if let Ok(d) = ev.payload::<msg::EventsDropped>() {
                inner.reported_dropped.fetch_add(d.count, Ordering::SeqCst);
            }
        }
        names::BYE => {
            if let Ok(b) = ev.payload::<msg::Bye>() {
                if b.reason == "superseded" {
                    inner.superseded.store(true, Ordering::SeqCst);
                }
                *inner.bye.lock().unwrap_or_else(|e| e.into_inner()) = Some(b.reason);
            }
        }
        _ => {}
    }
    true
}

/// Open with retry while the runtime loads: once every `every`, for up to
/// `within`, stopping early when `give_up` says the runtime is not coming.
/// Only an unreachable socket is retried; an exchange that failed is final.
pub fn open_with_retry(
    dir: &Path,
    mut make_opts: impl FnMut() -> OpenOptions,
    within: Duration,
    every: Duration,
    mut give_up: impl FnMut() -> bool,
) -> Result<Link, OpenError> {
    let start = Instant::now();
    loop {
        match Link::open(dir, make_opts()) {
            Ok(l) => return Ok(l),
            Err(OpenError::Unreachable(e)) => {
                if start.elapsed() >= within || give_up() {
                    return Err(OpenError::Unreachable(e));
                }
                std::thread::sleep(every);
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A runtime that answers `hello` with `reply_line` (the id filled in) and
    /// then says nothing, so what is under test is the launcher.
    fn answering(dir: &Path, reply: impl FnOnce(u64) -> String + Send + 'static) -> std::thread::JoinHandle<()> {
        let listener: UnixListener = socket::bind(dir).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            let id = match decode_line(&line).unwrap() {
                Frame::Request(r) => r.id,
                other => panic!("{other:?}"),
            };
            (&stream).write_all(reply(id).as_bytes()).unwrap();
            // Held open until the launcher is done with it.
            std::thread::sleep(Duration::from_millis(400));
        })
    }

    fn hello_reply(id: u64, major: u32, runtime_id: &str) -> String {
        format!(
            "{{\"id\":{id},\"ok\":true,\"p\":{{\"protocol\":{{\"major\":{major},\"minor\":0}},\"runtime\":{{\"id\":\"{runtime_id}\",\"version\":\"1\"}},\"client\":{{\"name\":\"x\",\"version\":\"1\",\"build\":\"1\"}},\"caps\":{{\"lifecycle\":1}}}}}}\n"
        )
    }

    #[test]
    fn a_handshake_with_another_runtime_than_the_one_started_is_refused() {
        let dir = scratch("identity");
        let server = answering(&dir, |id| hello_reply(id, 1, "org.example.someone-else"));
        let err = Link::open(&dir, OpenOptions::new("abcd1234")).err().expect("refused");
        assert!(err.to_string().contains("someone-else"), "{err}");
        server.join().unwrap();

        // The control: the same exchange with the right id is accepted, so it
        // was the id that was refused and not the exchange.
        let server = answering(&dir, |id| hello_reply(id, 1, BUILTIN_RUNTIME_ID));
        let link = Link::open(&dir, OpenOptions::new("abcd1234")).expect("accepted");
        assert_eq!(link.established().runtime.id, BUILTIN_RUNTIME_ID);
        drop(link);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_runtime_on_another_major_is_refused_and_named() {
        let dir = scratch("major");
        let server = answering(&dir, |id| hello_reply(id, 2, BUILTIN_RUNTIME_ID));
        let err = Link::open(&dir, OpenOptions::new("abcd1234")).err().expect("refused");
        assert!(err.to_string().contains("major 2"), "{err}");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absent_socket_is_unreachable_and_not_a_failed_exchange() {
        let dir = scratch("absent");
        let err = Link::open(&dir, OpenOptions::new("abcd1234")).err().unwrap();
        assert!(matches!(err, OpenError::Unreachable(_)), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_runtime_that_never_answers_is_a_failure_in_two_seconds() {
        let dir = scratch("silent");
        let listener = socket::bind(&dir).unwrap();
        let held = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(2600));
            drop(stream);
        });
        let started = Instant::now();
        let err = Link::open(&dir, OpenOptions::new("abcd1234")).err().unwrap();
        assert!(started.elapsed() < Duration::from_millis(2500), "{:?}", started.elapsed());
        assert!(err.to_string().contains("no reply"), "{err}");
        held.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_with_no_reply_is_an_error_and_not_a_success() {
        let dir = scratch("noreply");
        let server = answering(&dir, |id| hello_reply(id, 1, BUILTIN_RUNTIME_ID));
        let link = Link::open(&dir, OpenOptions::new("abcd1234")).unwrap();
        // The runtime above says nothing else, and `lifecycle` does not offer
        // settings, so ask for something it was never going to answer.
        let started = Instant::now();
        let err = link.call("x-test.silence", msg::empty()).unwrap_err();
        assert!(err.contains("no reply"), "{err}");
        assert!(started.elapsed() >= Duration::from_millis(1900));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_directories_are_listed_by_name_and_other_things_are_left() {
        let profile = scratch("list");
        let one = SessionDir::create(&profile).unwrap();
        assert!(one.dir.is_dir());
        std::fs::create_dir_all(profile.join("runtime/notes")).unwrap();
        let two = SessionDir::create(&profile).unwrap();
        // Creating one removes the stale ones that were there, which is safe
        // only because the caller holds the profile lock.
        assert!(!one.dir.exists(), "the stale session is gone");
        assert!(two.dir.is_dir());
        assert!(profile.join("runtime/notes").is_dir(), "a directory that is not a session is left alone");
        assert_eq!(list_sessions(&profile.join("runtime")), vec![two]);
        let _ = std::fs::remove_dir_all(&profile);
    }
}
