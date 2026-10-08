//! Shared conformance cases: line vectors, and a harness either side can run
//! against a peer over any `Read`/`Write` pair. Behind the `conformance`
//! feature.
//!
//! **One set of cases, both directions.** A launcher author runs
//! [`run_against_runtime`] against a runtime under test; a runtime author runs
//! [`run_against_launcher`] against a launcher. Both sides run [`run_vectors`]
//! on their own decoder. The vectors are plain files (`vectors/accept.jsonl`
//! and `vectors/reject.jsonl`, one JSON object per line) so an implementation in
//! another language can read them without this crate.
//!
//! The harness talks through a [`Link`], a boxed reader and writer, so the same
//! cases cover the codec in-process ([`duplex`]) and the transport over a Unix
//! socket ([`Link::unix`]). **Give the link a read timeout.** The harness has
//! no clock of its own: a read that times out is how "no reply" is observed, and
//! without one a silent peer hangs the run.

use crate::error::Code;
use crate::frame::{decode_line, encode_line, DecodeError, Event, Frame, Reply, Request};
use crate::lines::{LineReader, Next, MAX_LINE};
use crate::msg::{self, names, Hello, HelloReply, Kind};
use crate::version::{negotiate, Capabilities, Negotiated, Protocol};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

// ---- vectors ----------------------------------------------------------------

/// One line the codec must accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accept {
    pub name: String,
    pub line: String,
    /// `request`, `reply`, `error` or `event`.
    pub kind: String,
    /// The verb or event name, where there is one.
    pub verb: Option<String>,
}

/// One line the codec must refuse, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    pub name: String,
    pub line: String,
    /// A [`DecodeError::kind`] word (`too_long`, `not_json`, `not_object`,
    /// `ambiguous`, `field`, `limit`), or `payload` for a well-formed frame
    /// whose typed payload breaks a rule.
    pub error: String,
}

impl Reject {
    /// What a receiver does with it. A line that is not a frame at all closes
    /// the connection; a well-formed frame carrying an out-of-range value is
    /// dropped and counted (spec section 2).
    pub fn closes_connection(&self) -> bool {
        !matches!(self.error.as_str(), "limit" | "payload")
    }
}

const ACCEPT: &str = include_str!("../vectors/accept.jsonl");
const REJECT: &str = include_str!("../vectors/reject.jsonl");

fn rows(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).expect("a vector file is JSON lines"))
}

fn text(row: &Value, key: &str) -> String {
    row[key].as_str().unwrap_or_else(|| panic!("vector row has no {key}")).to_string()
}

/// The accept vectors.
pub fn accept_vectors() -> Vec<Accept> {
    rows(ACCEPT)
        .map(|r| Accept { name: text(&r, "name"), line: text(&r, "line"), kind: text(&r, "kind"), verb: r["verb"].as_str().map(str::to_string) })
        .collect()
}

/// The reject vectors in the files, plus the ones too large to keep in a file:
/// an over-length line, and a string one byte over the bound.
pub fn reject_vectors() -> Vec<Reject> {
    let mut v: Vec<Reject> =
        rows(REJECT).map(|r| Reject { name: text(&r, "name"), line: text(&r, "line"), error: text(&r, "error") }).collect();
    v.push(Reject {
        name: "a line one byte over 64 KiB".into(),
        line: oversize_line(),
        error: "too_long".into(),
    });
    v.push(Reject {
        name: "a string of 513 bytes in a payload".into(),
        line: format!(r#"{{"ev":"engine.version","n":1,"p":{{"version":"{}"}}}}"#, "x".repeat(513)),
        error: "limit".into(),
    });
    v.push(Reject {
        name: "an object key of 513 bytes".into(),
        line: format!(r#"{{"id":1,"m":"x-vendor.thing","p":{{"{}":1}}}}"#, "k".repeat(513)),
        error: "limit".into(),
    });
    v
}

/// A line of exactly `MAX_LINE + 1` bytes that is otherwise a valid frame.
pub fn oversize_line() -> String {
    let head = r#"{"id":1,"m":"x-vendor.pad","p":{"pad":""#;
    let tail = r#""}}"#;
    let pad = MAX_LINE + 1 - head.len() - tail.len();
    format!("{head}{}{tail}", "x".repeat(pad))
}

/// What a case did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// Not applicable to this peer, with the reason (a capability it does not
    /// offer).
    Skip(String),
    Fail(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub name: String,
    pub outcome: Outcome,
}

/// The result of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub cases: Vec<Case>,
}

impl Report {
    fn record(&mut self, name: impl Into<String>, result: Result<(), String>) {
        self.cases.push(Case { name: name.into(), outcome: result.map_or_else(Outcome::Fail, |()| Outcome::Pass) });
    }

    fn skip(&mut self, name: impl Into<String>, why: impl Into<String>) {
        self.cases.push(Case { name: name.into(), outcome: Outcome::Skip(why.into()) });
    }

    /// No case failed. A skipped case is not a failure.
    pub fn ok(&self) -> bool {
        !self.cases.iter().any(|c| matches!(c.outcome, Outcome::Fail(_)))
    }

    pub fn failures(&self) -> Vec<&Case> {
        self.cases.iter().filter(|c| matches!(c.outcome, Outcome::Fail(_))).collect()
    }

    pub fn passed(&self) -> usize {
        self.cases.iter().filter(|c| c.outcome == Outcome::Pass).count()
    }

    fn extend(&mut self, other: Report) {
        self.cases.extend(other.cases);
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in &self.cases {
            match &c.outcome {
                Outcome::Pass => writeln!(f, "pass  {}", c.name)?,
                Outcome::Skip(why) => writeln!(f, "skip  {} ({why})", c.name)?,
                Outcome::Fail(why) => writeln!(f, "FAIL  {}: {why}", c.name)?,
            }
        }
        let skipped = self.cases.iter().filter(|c| matches!(c.outcome, Outcome::Skip(_))).count();
        write!(f, "{} passed, {} failed, {} skipped", self.passed(), self.failures().len(), skipped)
    }
}

/// Run every vector through this crate's codec. Both sides can run it on their
/// own build to confirm the version of the crate they depend on still agrees
/// with the files.
pub fn run_vectors() -> Report {
    let mut report = Report::default();
    for a in accept_vectors() {
        report.record(format!("accept: {}", a.name), check_accept(&a));
    }
    for r in reject_vectors() {
        report.record(format!("reject: {}", r.name), check_reject(&r));
    }
    report
}

/// The disposition of a line according to this crate, as the vectors name it.
pub fn classify(line: &str) -> Result<Frame, String> {
    let frame = match decode_line(line) {
        Ok(f) => f,
        Err(e) => return Err(e.kind().to_string()),
    };
    let checked = match &frame {
        Frame::Request(r) => msg::check(Kind::Request, &r.m, &r.p),
        Frame::Event(e) => msg::check(Kind::Event, &e.ev, &e.p),
        Frame::Reply(_) => Ok(()),
    };
    match checked {
        Ok(()) => Ok(frame),
        Err(_) => Err("payload".to_string()),
    }
}

fn check_accept(a: &Accept) -> Result<(), String> {
    let frame = classify(&a.line).map_err(|e| format!("refused ({e})"))?;
    let (kind, verb) = match &frame {
        Frame::Request(r) => ("request", Some(r.m.as_str())),
        Frame::Event(e) => ("event", Some(e.ev.as_str())),
        Frame::Reply(Reply { result: Ok(_), .. }) => ("reply", None),
        Frame::Reply(Reply { result: Err(_), .. }) => ("error", None),
    };
    if kind != a.kind || verb != a.verb.as_deref() {
        return Err(format!("decoded as {kind} {verb:?}"));
    }
    let again = decode_line(&encode_line(&frame)).map_err(|e| format!("re-encoded line refused: {e}"))?;
    if again != frame {
        return Err("decode, encode, decode changed the value".into());
    }
    Ok(())
}

fn check_reject(r: &Reject) -> Result<(), String> {
    match classify(&r.line) {
        Ok(_) => Err("accepted".into()),
        Err(kind) if kind == r.error => Ok(()),
        Err(kind) => Err(format!("refused as {kind}, expected {}", r.error)),
    }
}

// ---- links ------------------------------------------------------------------

/// One connection: a reader and a writer, from anything.
pub struct Link {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

impl Link {
    pub fn new<R: Read + Send + 'static, W: Write + Send + 'static>(reader: R, writer: W) -> Self {
        Link { reader: Box::new(reader), writer: Box::new(writer) }
    }

    /// A Unix stream, with a read and write timeout so a silent peer fails a
    /// case instead of hanging the run.
    #[cfg(unix)]
    pub fn unix(stream: std::os::unix::net::UnixStream, timeout: Duration) -> io::Result<Link> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Link::new(stream.try_clone()?, stream))
    }
}

struct Pipe {
    state: Mutex<PipeState>,
    ready: Condvar,
}

#[derive(Default)]
struct PipeState {
    buf: VecDeque<u8>,
    writer_gone: bool,
    reader_gone: bool,
}

struct PipeReader {
    pipe: Arc<Pipe>,
    timeout: Duration,
}

struct PipeWriter {
    pipe: Arc<Pipe>,
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut st = self.pipe.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if !st.buf.is_empty() {
                let n = out.len().min(st.buf.len());
                for slot in out.iter_mut().take(n) {
                    *slot = st.buf.pop_front().expect("length checked");
                }
                return Ok(n);
            }
            if st.writer_gone {
                return Ok(0);
            }
            let (guard, timed_out) = self.pipe.ready.wait_timeout(st, self.timeout).unwrap_or_else(|e| e.into_inner());
            st = guard;
            if timed_out.timed_out() && st.buf.is_empty() && !st.writer_gone {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "read timed out"));
            }
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut st = self.pipe.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.reader_gone {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the peer closed"));
        }
        st.buf.extend(data);
        self.pipe.ready.notify_all();
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.pipe.state.lock().unwrap_or_else(|e| e.into_inner()).reader_gone = true;
        self.pipe.ready.notify_all();
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.pipe.state.lock().unwrap_or_else(|e| e.into_inner()).writer_gone = true;
        self.pipe.ready.notify_all();
    }
}

/// An in-process connection: two [`Link`]s, each reading what the other writes.
/// Dropping a link closes it, so the peer reads end-of-file, as a closed socket
/// does. Reads time out after two seconds.
pub fn duplex() -> (Link, Link) {
    duplex_with_timeout(Duration::from_secs(2))
}

pub fn duplex_with_timeout(timeout: Duration) -> (Link, Link) {
    let pipe = || Arc::new(Pipe { state: Mutex::new(PipeState::default()), ready: Condvar::new() });
    let (a_to_b, b_to_a) = (pipe(), pipe());
    (
        Link::new(PipeReader { pipe: b_to_a.clone(), timeout }, PipeWriter { pipe: a_to_b.clone() }),
        Link::new(PipeReader { pipe: a_to_b, timeout }, PipeWriter { pipe: b_to_a }),
    )
}

// ---- a framed connection ----------------------------------------------------

enum Got {
    Frame(Frame),
    /// The peer closed, or the connection broke.
    Closed,
    /// No line within the link's read timeout.
    Silent,
    /// A line that is not a frame, or a line the reader refused.
    Bad(String),
}

struct Conn {
    reader: LineReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    next_id: u64,
    last_n: Option<u64>,
    /// Faults in what the peer sent that are not the failure of the case in
    /// hand: a malformed event, a counter that went backwards.
    faults: Vec<String>,
}

impl Conn {
    fn new(link: Link) -> Self {
        Conn { reader: LineReader::new(link.reader), writer: link.writer, next_id: 1, last_n: None, faults: Vec::new() }
    }

    fn send_line(&mut self, line: &str) -> Result<(), String> {
        self.writer.write_all(line.as_bytes()).and_then(|()| self.writer.flush()).map_err(|e| format!("write failed: {e}"))
    }

    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        self.send_line(&encode_line(frame))
    }

    fn read(&mut self) -> Got {
        match self.reader.next_line() {
            Ok(Next::Line(line)) => match decode_line(&line) {
                Ok(frame) => {
                    self.audit(&frame);
                    Got::Frame(frame)
                }
                Err(e) => Got::Bad(e.to_string()),
            },
            Ok(Next::Eof) | Ok(Next::Truncated) => Got::Closed,
            Ok(Next::TooLong) => Got::Bad("a line over 64 KiB".into()),
            Ok(Next::NotUtf8) => Got::Bad("a line that is not UTF-8".into()),
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => Got::Silent,
            Err(_) => Got::Closed,
        }
    }

    /// Everything the peer sends is held to the spec as it passes.
    fn audit(&mut self, frame: &Frame) {
        if let Frame::Event(e) = frame {
            if let Err(v) = msg::check(Kind::Event, &e.ev, &e.p) {
                self.faults.push(format!("event {} breaks the spec: {v}", e.ev));
            }
            if let Some(last) = self.last_n {
                if e.n <= last {
                    self.faults.push(format!("event counter went from {last} to {} (event {})", e.n, e.ev));
                }
            }
            self.last_n = Some(e.n);
        }
    }

    fn call(&mut self, name: &str, p: Value) -> Result<Reply, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Frame::Request(Request::new(id, name, p)))?;
        loop {
            match self.read() {
                Got::Frame(Frame::Reply(r)) if r.id == id => return Ok(r),
                Got::Frame(Frame::Reply(r)) => return Err(format!("a reply for id {}, which was never asked", r.id)),
                Got::Frame(Frame::Event(_)) => continue,
                Got::Frame(Frame::Request(r)) => return Err(format!("the peer sent a request ({}), and version 1 defines none", r.m)),
                Got::Closed => return Err("the connection closed".into()),
                Got::Silent => return Err(format!("no reply to {name} within the link's read timeout")),
                Got::Bad(why) => return Err(format!("the peer sent a bad line: {why}")),
            }
        }
    }

    /// The reply, which must be an error with `code`.
    fn expect_error(&mut self, name: &str, p: Value, code: Code) -> Result<(), String> {
        match self.call(name, p)?.result {
            Err(e) if e.code == code => Ok(()),
            Err(e) => Err(format!("{name} got {} and should have got {code}", e.code)),
            Ok(_) => Err(format!("{name} succeeded and should have been {code}")),
        }
    }

    /// Wait for the peer to close, ignoring events on the way. It gets three
    /// read timeouts to do it: closing is asynchronous, and a peer that has
    /// decided to close may still be inside a read of its own. A connection that
    /// is still open after that was never going to close.
    fn expect_closed(&mut self) -> Result<(), String> {
        let mut silent = 0;
        for _ in 0..64 {
            match self.read() {
                Got::Closed => return Ok(()),
                Got::Silent => {
                    silent += 1;
                    if silent >= 3 {
                        return Err("the connection stayed open".into());
                    }
                }
                Got::Frame(_) | Got::Bad(_) => {}
            }
        }
        Err("the peer kept talking and never closed".into())
    }

    /// Wait to see the connection stay open for one read timeout.
    fn expect_open(&mut self) -> Result<(), String> {
        for _ in 0..64 {
            match self.read() {
                Got::Closed => return Err("the connection was closed".into()),
                Got::Silent => return Ok(()),
                Got::Frame(_) | Got::Bad(_) => continue,
            }
        }
        Ok(())
    }

    fn take_faults(&mut self) -> Vec<String> {
        std::mem::take(&mut self.faults)
    }
}

// ---- launcher under test drives, runtime under test answers -----------------

/// How [`run_against_runtime`] behaves.
#[derive(Debug, Clone)]
pub struct RuntimeOpts {
    /// The `id` in the runtime's manifest. When set, a handshake that names a
    /// different runtime fails.
    pub manifest_id: Option<String>,
    /// What this side claims to offer in `hello`. The runtime's answer is
    /// intersected with it.
    pub offer: Capabilities,
    pub session: String,
    /// Also send `lifecycle.stop` and expect it to be accepted. Off by default,
    /// since a runtime that stops cannot run the cases after it.
    pub test_stop: bool,
}

impl Default for RuntimeOpts {
    fn default() -> Self {
        RuntimeOpts {
            manifest_id: None,
            offer: [
                crate::msg::caps::LIFECYCLE,
                crate::msg::caps::EVENTS_CORE,
                crate::msg::caps::EVENTS_PRESENCE,
                crate::msg::caps::STATE,
                crate::msg::caps::SETTINGS,
                crate::msg::caps::FLAGS,
                crate::msg::caps::ASSETS_OVERLAY,
                crate::msg::caps::DIAGNOSTICS,
            ]
            .iter()
            .map(|c| (c.to_string(), 1))
            .collect(),
            session: "conform0".into(),
            test_stop: false,
        }
    }
}

fn handshake(conn: &mut Conn, opts: &RuntimeOpts, reattach: bool) -> Result<(HelloReply, Negotiated), String> {
    let hello = Hello {
        protocol: Protocol::CURRENT,
        cordial: "conformance".into(),
        session: opts.session.clone(),
        caps: opts.offer.clone(),
        reattach,
    };
    let reply = conn.call(names::HELLO, serde_json::to_value(&hello).expect("hello serialises"))?;
    let answer: HelloReply = reply.payload().map_err(|e| format!("hello: {e}"))?;
    let live = negotiate(Protocol::CURRENT, &opts.offer, answer.protocol, &answer.caps).map_err(|e| e.to_string())?;
    if let Some(id) = &opts.manifest_id {
        if &answer.runtime.id != id {
            return Err(format!("the handshake says runtime.id {:?} and the manifest says {id:?}", answer.runtime.id));
        }
    }
    Ok((answer, live))
}

/// Run the launcher-side cases against a runtime. `connect` is called once per
/// case that needs a fresh connection, so it must be able to reach the same
/// runtime repeatedly: the runtime listens, and a launcher reattaches.
pub fn run_against_runtime(connect: &mut dyn FnMut() -> io::Result<Link>, opts: &RuntimeOpts) -> Report {
    let mut report = Report::default();
    let mut dial = |report: &mut Report, case: &str| -> Option<Conn> {
        match connect() {
            Ok(link) => Some(Conn::new(link)),
            Err(e) => {
                report.record(case, Err(format!("could not connect: {e}")));
                None
            }
        }
    };

    // Before the handshake finishes, a request is `not_ready`.
    if let Some(mut c) = dial(&mut report, "a request before the handshake is not_ready") {
        report.record(
            "a request before the handshake is not_ready",
            c.expect_error(names::STATE_GET, Value::Null, Code::NotReady),
        );
    }

    let Some(mut main) = dial(&mut report, "the handshake") else { return report };
    let live = match handshake(&mut main, opts, false) {
        Ok((answer, live)) => {
            report.record("the handshake", Ok(()));
            report.record(
                "the runtime offers lifecycle and speaks this major",
                if answer.protocol.major == Protocol::CURRENT.major && live.caps.contains_key(crate::msg::caps::REQUIRED) {
                    Ok(())
                } else {
                    Err(format!("protocol {} caps {:?}", answer.protocol, live.caps))
                },
            );
            live
        }
        Err(why) => {
            report.record("the handshake", Err(why));
            return report;
        }
    };

    report.record("an unknown request gets unsupported", main.expect_error("x-conformance.unknown", json!({}), Code::Unsupported));
    // A read-only verb whose capability is not live, if there is one.
    let case = "a verb this spec defines but the handshake did not enable gets unsupported";
    match [names::DIAGNOSTICS_GET, names::STATE_GET, names::SETTINGS_GET].into_iter().find(|v| !msg::offered(v, &live.caps)) {
        Some(verb) => report.record(case, main.expect_error(verb, Value::Null, Code::Unsupported)),
        None => report.skip(case, "every probe verb is live"),
    }

    report.record("replies are matched on id", {
        let (a, b) = (main.next_id, main.next_id + 1);
        main.next_id += 2;
        (|| -> Result<(), String> {
            main.send(&Frame::Request(Request::new(a, "x-conformance.a", json!({}))))?;
            main.send(&Frame::Request(Request::new(b, "x-conformance.b", json!({}))))?;
            let mut seen = Vec::new();
            while seen.len() < 2 {
                match main.read() {
                    Got::Frame(Frame::Reply(r)) => seen.push(r.id),
                    Got::Frame(_) => {}
                    Got::Silent => return Err("fewer than two replies".into()),
                    Got::Closed => return Err("the connection closed".into()),
                    Got::Bad(w) => return Err(w),
                }
            }
            seen.sort_unstable();
            if seen == [a, b] {
                Ok(())
            } else {
                Err(format!("replies for {seen:?}, asked {a} and {b}"))
            }
        })()
    });

    if live.caps.contains_key(crate::msg::caps::SETTINGS) {
        report.record("settings.set reports an unknown key and applies nothing for it", {
            main.call(names::SETTINGS_SET, json!({"warp_drive": "on"}))
                .and_then(|r| r.result.map_err(|e| format!("refused an unknown key outright: {}", e.code)))
                .and_then(|p| msg::payload::<msg::SettingsReply>(&p).map_err(|e| e.to_string()))
                .and_then(|r| {
                    if r.ignored == ["warp_drive"] && r.applied.is_empty() {
                        Ok(())
                    } else {
                        Err(format!("applied {:?}, ignored {:?}", r.applied, r.ignored))
                    }
                })
        });
        report.record(
            "settings.set with a value outside the set is invalid",
            main.expect_error(names::SETTINGS_SET, json!({"throttle": "sometimes"}), Code::Invalid),
        );
        report.record("settings.get names every key it holds", {
            main.call(names::SETTINGS_GET, Value::Null)
                .and_then(|r| r.payload::<msg::SettingsGetReply>().map_err(|e| e.to_string()))
                .and_then(|g| match g.values.keys().find(|k| !crate::settings::KEYS.contains(&k.as_str())) {
                    Some(k) => Err(format!("a value for {k:?}, which is not in the closed key set")),
                    None => Ok(()),
                })
        });
    } else {
        report.skip("settings.*", "the runtime does not offer settings");
    }
    if live.caps.contains_key(crate::msg::caps::STATE) {
        report.record("state.get returns a snapshot", {
            main.call(names::STATE_GET, Value::Null).and_then(|r| r.payload::<msg::StateSnapshot>().map(|_| ()).map_err(|e| e.to_string()))
        });
    } else {
        report.skip("state.get", "the runtime does not offer state");
    }
    if live.caps.contains_key(crate::msg::caps::DIAGNOSTICS) {
        report.record("diagnostics.get is bounded", {
            main.call(names::DIAGNOSTICS_GET, Value::Null)
                .and_then(|r| r.payload::<msg::DiagnosticsReply>().map(|_| ()).map_err(|e| e.to_string()))
        });
    } else {
        report.skip("diagnostics.get", "the runtime does not offer diagnostics");
    }

    // Closing the connection does not stop the runtime, and a launcher can
    // reattach to the same one.
    drop(main);
    if let Some(mut again) = dial(&mut report, "after the connection closes the runtime can be reattached") {
        report.record(
            "after the connection closes the runtime can be reattached",
            handshake(&mut again, opts, true).map(|_| ()),
        );
        // A newer controller replaces an older one, which is told so.
        if let Some(mut newer) = dial(&mut report, "a new controller supersedes the old one") {
            let r = handshake(&mut newer, opts, true).map(|_| ()).and_then(|()| {
                for _ in 0..16 {
                    match again.read() {
                        Got::Frame(Frame::Event(Event { ev, p, .. })) if ev == names::BYE => {
                            return if p["reason"] == "superseded" { Ok(()) } else { Err(format!("bye with reason {}", p["reason"])) };
                        }
                        Got::Frame(_) => continue,
                        Got::Closed => return Err("closed without saying bye superseded".into()),
                        Got::Silent => return Err("the old controller was not told".into()),
                        Got::Bad(w) => return Err(w),
                    }
                }
                Err("no bye".into())
            });
            report.record("a new controller supersedes the old one", r);
            report.record("the old controller's connection ends", again.expect_closed());
            let mut faults = again.take_faults();
            faults.extend(newer.take_faults());
            report.record(
                "events the runtime sent were well-formed and numbered upward",
                if faults.is_empty() { Ok(()) } else { Err(faults.join("; ")) },
            );
        }
    }

    // A line that does not parse closes the connection, never the runtime.
    for (name, line) in [
        ("a line that is not JSON closes the connection and the runtime survives", "this is not json\n".to_string()),
        ("a line over 64 KiB closes the connection and the runtime survives", format!("{}\n", oversize_line())),
    ] {
        let Some(mut c) = dial(&mut report, name) else { continue };
        let r = handshake(&mut c, opts, true)
            .map(|_| ())
            .and_then(|()| c.send_line(&line))
            .and_then(|()| c.expect_closed());
        drop(c);
        let survived = match r {
            Ok(()) => dial(&mut report, name).map(|mut again| handshake(&mut again, opts, true).map(|_| ())),
            Err(e) => Some(Err(e)),
        };
        if let Some(r) = survived {
            report.record(name, r);
        }
    }

    if opts.test_stop {
        if let Some(mut c) = dial(&mut report, "lifecycle.stop is accepted") {
            let r = handshake(&mut c, opts, true).map(|_| ()).and_then(|()| {
                c.call(names::LIFECYCLE_STOP, json!({"grace_ms": 1000}))
                    .and_then(|r| r.result.map(|_| ()).map_err(|e| format!("refused: {}", e.code)))
            });
            report.record("lifecycle.stop is accepted", r);
        }
    }
    report
}

// ---- runtime under test drives, launcher under test answers -----------------

/// How [`run_against_launcher`] behaves.
#[derive(Debug, Clone)]
pub struct LauncherOpts {
    /// The id this scripted runtime answers with. If the launcher under test
    /// was given a manifest, this is its `id`.
    pub runtime_id: String,
    /// Run the case where the handshake names a different id than the manifest.
    /// Only meaningful when the launcher was given a manifest with `runtime_id`.
    pub test_identity: bool,
    /// What the scripted runtime offers.
    pub offer: Capabilities,
}

impl Default for LauncherOpts {
    fn default() -> Self {
        LauncherOpts {
            runtime_id: "org.example.conformance".into(),
            test_identity: false,
            offer: [(crate::msg::caps::LIFECYCLE.to_string(), 1), (crate::msg::caps::SETTINGS.to_string(), 1)].into(),
        }
    }
}

fn answer(c: &mut Conn, opts: &LauncherOpts, hello_id: u64, major: u32, runtime_id: &str) -> Result<(), String> {
    let reply = HelloReply {
        protocol: Protocol::new(major, 0),
        runtime: msg::RuntimeIdent { id: runtime_id.into(), version: "0".into() },
        client: msg::ClientIdent { name: "conformance".into(), version: "0".into(), build: "0".into() },
        caps: opts.offer.clone(),
    };
    c.send(&Frame::Reply(msg::reply_ok(hello_id, &reply)))
}

fn read_hello(c: &mut Conn) -> Result<(u64, Hello), String> {
    match c.read() {
        Got::Frame(Frame::Request(r)) if r.m == names::HELLO => {
            let hello = r.params::<Hello>().map_err(|e| format!("hello: {e}"))?;
            Ok((r.id, hello))
        }
        Got::Frame(other) => Err(format!("the first line was {other:?}, not a hello request")),
        Got::Closed => Err("the launcher closed before saying hello".into()),
        Got::Silent => Err("the launcher said nothing within the link's read timeout".into()),
        Got::Bad(w) => Err(format!("the launcher's first line was bad: {w}")),
    }
}

/// Run the runtime-side cases against a launcher. `launch` is called once per
/// case and must start a fresh launcher session whose controller connects to the
/// returned link, as the launcher would connect to a runtime it spawned.
pub fn run_against_launcher(launch: &mut dyn FnMut() -> io::Result<Link>, opts: &LauncherOpts) -> Report {
    let mut report = Report::default();
    let mut session = |report: &mut Report, case: &str| -> Option<Conn> {
        match launch() {
            Ok(l) => Some(Conn::new(l)),
            Err(e) => {
                report.record(case, Err(format!("could not start a launcher session: {e}")));
                None
            }
        }
    };

    let case = "the launcher opens with a valid hello for this major";
    if let Some(mut c) = session(&mut report, case) {
        report.record(
            case,
            read_hello(&mut c).and_then(|(_, h)| {
                if h.protocol.major == Protocol::CURRENT.major {
                    Ok(())
                } else {
                    Err(format!("protocol {}", h.protocol))
                }
            }),
        );
    }

    let case = "after a handshake the launcher sends only requests the handshake enabled, and keeps the connection";
    if let Some(mut c) = session(&mut report, case) {
        let r = read_hello(&mut c)
            .and_then(|(id, _)| answer(&mut c, opts, id, Protocol::CURRENT.major, &opts.runtime_id))
            .and_then(|()| {
                let live = opts.offer.clone();
                for _ in 0..32 {
                    match c.read() {
                        Got::Frame(Frame::Request(r)) => {
                            if !msg::offered(&r.m, &live) {
                                return Err(format!("sent {}, which the handshake did not enable", r.m));
                            }
                            if let Err(v) = msg::check(Kind::Request, &r.m, &r.p) {
                                return Err(format!("sent a bad {}: {v}", r.m));
                            }
                            // Answer so the launcher is not left waiting.
                            let reply = match r.m.as_str() {
                                names::SETTINGS_SET => {
                                    let set = msg::SettingsSet::from_payload(&r.p).map_err(|e| e.to_string())?;
                                    let applied = set.updates.iter().map(|u| u.key().to_string()).collect();
                                    msg::reply_ok(r.id, &msg::SettingsReply { applied, ignored: set.ignored, notes: Default::default() })
                                }
                                names::LIFECYCLE_STOP => Reply::ok(r.id, msg::empty()),
                                _ => Reply::err(r.id, Code::Unsupported, "conformance"),
                            };
                            c.send(&Frame::Reply(reply))?;
                        }
                        Got::Frame(_) => {}
                        Got::Closed => return Err("the launcher closed a good connection".into()),
                        Got::Silent => return Ok(()),
                        Got::Bad(w) => return Err(format!("the launcher sent a bad line: {w}")),
                    }
                }
                Ok(())
            });
        report.record(case, r);
    }

    let case = "the launcher refuses a runtime whose protocol major it does not speak";
    if let Some(mut c) = session(&mut report, case) {
        report.record(
            case,
            read_hello(&mut c)
                .and_then(|(id, _)| answer(&mut c, opts, id, Protocol::CURRENT.major + 1, &opts.runtime_id))
                .and_then(|()| c.expect_closed()),
        );
    }

    if opts.test_identity {
        let case = "the launcher refuses a handshake that renames the runtime";
        if let Some(mut c) = session(&mut report, case) {
            report.record(
                case,
                read_hello(&mut c)
                    .and_then(|(id, _)| answer(&mut c, opts, id, Protocol::CURRENT.major, "org.example.someone-else"))
                    .and_then(|()| c.expect_closed()),
            );
        }
    } else {
        report.skip("the launcher refuses a handshake that renames the runtime", "no manifest id was given");
    }

    // Hostile lines after a good handshake. Not a frame: the connection closes.
    // A frame that is well-formed but out of range: dropped, connection kept.
    for r in reject_vectors() {
        // Version 1 defines no runtime-to-launcher request, so what a launcher
        // does with a well-formed one is not specified; only events are tried
        // on the "dropped and counted" side. Anything that is not a frame at
        // all closes the connection whatever it looked like.
        let is_event = matches!(decode_line(&r.line), Ok(Frame::Event(_)) | Err(DecodeError::Limit(_)))
            && r.line.contains("\"ev\"");
        if !r.closes_connection() && !is_event {
            continue;
        }
        let what = if r.closes_connection() { "closes" } else { "drops the message and keeps the connection" };
        let case = format!("the launcher {what} on: {}", r.name);
        let Some(mut c) = session(&mut report, &case) else { continue };
        let res = read_hello(&mut c)
            .and_then(|(id, _)| answer(&mut c, opts, id, Protocol::CURRENT.major, &opts.runtime_id))
            .and_then(|()| c.send_line(&format!("{}\n", r.line)))
            .and_then(|()| if r.closes_connection() { c.expect_closed() } else { c.expect_open() });
        report.record(case, res);
    }

    // A flood of events does not stall the launcher's reader.
    let case = "a flood of events is read without stalling the writer";
    if let Some(mut c) = session(&mut report, case) {
        let res = read_hello(&mut c)
            .and_then(|(id, _)| answer(&mut c, opts, id, Protocol::CURRENT.major, &opts.runtime_id))
            .and_then(|()| {
                for n in 1..=5000u64 {
                    c.send(&Frame::Event(Event::new("x-conformance.flood", n, json!({"i": n}))))?;
                }
                c.send(&Frame::Event(Event::new(names::EVENTS_DROPPED, 5001, json!({"count": 0}))))
            })
            .and_then(|()| c.expect_open());
        report.record(case, res);
    }
    report
}

/// Both sides' vectors plus whichever harness run produced `extra`, for a
/// one-call summary in a peer's own test suite.
pub fn with_vectors(extra: Report) -> Report {
    let mut all = run_vectors();
    all.extend(extra);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vector_passes_against_this_crates_own_codec() {
        let report = run_vectors();
        assert!(report.ok(), "{report}");
        assert!(report.passed() > 60, "the vector files were not read");
    }

    #[test]
    fn the_spec_closes_exactly_the_lines_that_are_not_frames() {
        for r in reject_vectors() {
            let expect_close = matches!(r.error.as_str(), "too_long" | "not_json" | "not_object" | "ambiguous" | "field");
            assert_eq!(r.closes_connection(), expect_close, "{}", r.name);
        }
        // And the codec agrees with the vectors about which errors are which.
        assert_eq!(DecodeError::Ambiguous.kind(), "ambiguous");
    }

    #[test]
    fn the_oversize_vector_is_exactly_one_byte_over() {
        assert_eq!(oversize_line().len(), MAX_LINE + 1);
    }

    #[test]
    fn a_duplex_reads_what_the_other_side_wrote_and_sees_the_close() {
        let (mut a, mut b) = duplex();
        a.writer.write_all(b"hi\n").unwrap();
        let mut buf = [0u8; 8];
        let n = b.reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hi\n");
        drop(a);
        assert_eq!(b.reader.read(&mut buf).unwrap(), 0, "the peer's close is end-of-file");
        assert!(b.writer.write_all(b"x").is_err(), "and writing to it is a broken pipe");
    }

    #[test]
    fn a_silent_duplex_times_out_rather_than_hangs() {
        let (_a, mut b) = duplex_with_timeout(Duration::from_millis(20));
        let mut buf = [0u8; 1];
        assert_eq!(b.reader.read(&mut buf).unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}
