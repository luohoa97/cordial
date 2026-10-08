//! The conformance harness, run against a small reference runtime and a small
//! reference launcher written here, and against deliberately broken ones.
//!
//! **The broken peers are the control.** A harness that passes everything proves
//! nothing, so each case that guards a rule has a peer that breaks that rule and
//! must be caught by it.

use cordial_protocol::conformance::{
    duplex_with_timeout, run_against_launcher, run_against_runtime, run_vectors, LauncherOpts, Link, Outcome, Report,
    RuntimeOpts,
};
use cordial_protocol::msg::{self, caps, names, ClientIdent, Hello, HelloReply, RuntimeIdent};
use cordial_protocol::{decode_line, encode_line, Code, DecodeError, Frame, LineReader, Negotiated, Next, Protocol, Reply};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(150);

// ---- a reference runtime ----------------------------------------------------

/// What a reference runtime does wrong, on purpose.
#[derive(Clone, Copy, Default)]
struct Flaws {
    /// Answers an unknown request with success.
    succeeds_at_anything: bool,
    /// Does not close on a line that is not JSON.
    tolerates_garbage: bool,
    /// Answers requests before the handshake.
    skips_not_ready: bool,
    /// Never tells the old controller it was replaced.
    silent_supersede: bool,
    /// Sends events whose counter repeats.
    repeats_counters: bool,
    /// Applies an unknown settings key by failing the whole message.
    refuses_unknown_keys: bool,
    /// Offers diagnostics, as a runtime may.
    offers_diagnostics: bool,
    /// ... and returns more lines than the spec allows.
    too_many_diagnostic_lines: bool,
}

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

struct Runtime {
    id: String,
    flaws: Flaws,
    controller: Mutex<Option<(Arc<AtomicBool>, SharedWriter)>>,
    // One counter for the runtime's life, so a reattached launcher sees the
    // numbers carry on and can tell what it missed.
    n: AtomicU64,
}

/// Clears the controller slot when a connection's thread ends, so the writer it
/// holds is dropped and the peer reads the close.
struct Leave<'a>(&'a Runtime, Arc<AtomicBool>);

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        let mut slot = self.0.controller.lock().unwrap();
        if matches!(&*slot, Some((flag, _)) if Arc::ptr_eq(flag, &self.1)) {
            *slot = None;
        }
    }
}

impl Runtime {
    fn new(id: &str, flaws: Flaws) -> Arc<Runtime> {
        Arc::new(Runtime { id: id.into(), flaws, controller: Mutex::new(None), n: AtomicU64::new(1) })
    }

    /// The connector a harness calls: each call is a new connection to this runtime.
    fn connector(self: &Arc<Self>) -> impl FnMut() -> io::Result<Link> {
        let rt = self.clone();
        move || {
            let (ours, theirs) = duplex_with_timeout(TIMEOUT);
            let rt = rt.clone();
            std::thread::spawn(move || rt.serve(theirs));
            Ok(ours)
        }
    }

    fn serve(&self, link: Link) {
        let writer = Arc::new(Mutex::new(link.writer));
        let mine = Arc::new(AtomicBool::new(false));
        let _leave = Leave(self, mine.clone());
        let mut lines = LineReader::new(link.reader);
        let mut shaken = false;
        let mut idle = 0;
        let send = |frame: &Frame| writer.lock().unwrap().write_all(encode_line(frame).as_bytes()).is_ok();
        loop {
            if mine.load(Ordering::SeqCst) {
                return;
            }
            let line = match lines.next_line() {
                Ok(Next::Line(l)) => l,
                Ok(Next::Eof) | Ok(Next::Truncated) => return,
                Ok(Next::TooLong) | Ok(Next::NotUtf8) if self.flaws.tolerates_garbage => continue,
                Ok(_) => return,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    idle += 1;
                    if idle > 200 {
                        return;
                    }
                    continue;
                }
                Err(_) => return,
            };
            idle = 0;
            let frame = match decode_line(&line) {
                Ok(f) => f,
                Err(DecodeError::Limit(_)) => continue,
                Err(_) if self.flaws.tolerates_garbage => continue,
                Err(_) => return,
            };
            let Frame::Request(req) = frame else { continue };
            let reply = if !shaken && req.m != names::HELLO && !self.flaws.skips_not_ready {
                Reply::err(req.id, Code::NotReady, "say hello first")
            } else {
                match req.m.as_str() {
                    names::HELLO => match req.params::<Hello>() {
                        Err(v) => Reply::err(req.id, Code::Invalid, v.to_string()),
                        Ok(_) => {
                            shaken = true;
                            let answer = HelloReply {
                                protocol: Protocol::CURRENT,
                                runtime: RuntimeIdent { id: self.id.clone(), version: "0.1".into() },
                                client: ClientIdent { name: "Reference".into(), version: "1".into(), build: "1".into() },
                                caps: [caps::LIFECYCLE, caps::SETTINGS, caps::STATE, caps::EVENTS_CORE]
                                    .iter()
                                    .chain(self.flaws.offers_diagnostics.then_some(&caps::DIAGNOSTICS))
                                    .map(|c| (c.to_string(), 1))
                                    .collect(),
                            };
                            // One controller at a time: the newest replaces the old.
                            let old = self.controller.lock().unwrap().replace((mine.clone(), writer.clone()));
                            if let Some((flag, old_writer)) = old {
                                if !self.flaws.silent_supersede {
                                    let bye = Frame::Event(msg::event(names::BYE, self.n.fetch_add(1, Ordering::SeqCst), &msg::Bye { reason: "superseded".into() }));
                                    let _ = old_writer.lock().unwrap().write_all(encode_line(&bye).as_bytes());
                                }
                                flag.store(true, Ordering::SeqCst);
                            }
                            if !send(&Frame::Reply(msg::reply_ok(req.id, &answer))) {
                                return;
                            }
                            let (n1, n2) = if self.flaws.repeats_counters {
                                (1, 1)
                            } else {
                                (self.n.fetch_add(1, Ordering::SeqCst), self.n.fetch_add(1, Ordering::SeqCst))
                            };
                            let events = [
                                Frame::Event(msg::event(names::ENGINE_VERSION, n1, &msg::EngineVersion { version: "1".into() })),
                                Frame::Event(msg::event(names::GAME_LEFT, n2, &msg::GameLeft { at: 5 })),
                            ];
                            if !events.iter().all(&send) {
                                return;
                            }
                            continue;
                        }
                    },
                    names::STATE_GET => msg::reply_ok(req.id, &msg::StateSnapshot::default()),
                    names::DIAGNOSTICS_GET if self.flaws.offers_diagnostics => {
                        let count = if self.flaws.too_many_diagnostic_lines { 201 } else { 1 };
                        msg::reply_ok(req.id, &msg::DiagnosticsReply { lines: vec!["ready".into(); count] })
                    }
                    names::SETTINGS_GET => msg::reply_ok(req.id, &msg::SettingsGetReply::default()),
                    names::SETTINGS_SET => match msg::SettingsSet::from_payload(&req.p) {
                        Err(v) => Reply::err(req.id, Code::Invalid, v.to_string()),
                        Ok(set) if self.flaws.refuses_unknown_keys && !set.ignored.is_empty() => {
                            Reply::err(req.id, Code::Failed, "unknown key")
                        }
                        Ok(set) => msg::reply_ok(
                            req.id,
                            &msg::SettingsReply {
                                applied: set.updates.iter().map(|u| u.key().to_string()).collect(),
                                ignored: set.ignored,
                                notes: Default::default(),
                            },
                        ),
                    },
                    names::LIFECYCLE_STOP => Reply::ok(req.id, msg::empty()),
                    _ if self.flaws.succeeds_at_anything => Reply::ok(req.id, msg::empty()),
                    other => Reply::err(req.id, Code::Unsupported, format!("{other} is not offered")),
                }
            };
            if !send(&Frame::Reply(reply)) {
                return;
            }
        }
    }
}

fn run_runtime(flaws: Flaws, opts: RuntimeOpts) -> Report {
    let rt = Runtime::new("org.example.reference", flaws);
    run_against_runtime(&mut rt.connector(), &opts)
}

fn opts() -> RuntimeOpts {
    RuntimeOpts { manifest_id: Some("org.example.reference".into()), test_stop: true, ..RuntimeOpts::default() }
}

fn failed(report: &Report, containing: &str) -> bool {
    report.cases.iter().any(|c| matches!(c.outcome, Outcome::Fail(_)) && c.name.contains(containing))
}

#[test]
fn the_vectors_pass_on_this_crate() {
    let report = run_vectors();
    assert!(report.ok(), "{report}");
}

#[test]
fn a_correct_runtime_passes_every_case() {
    let report = run_runtime(Flaws::default(), opts());
    println!("{report}");
    assert!(report.ok(), "{report}");
    assert!(report.passed() >= 12, "too few cases ran:\n{report}");
}

#[test]
fn the_manifest_id_is_enforced_against_the_handshake() {
    let report = run_runtime(Flaws::default(), RuntimeOpts { manifest_id: Some("org.example.other".into()), ..opts() });
    assert!(failed(&report, "the handshake"), "{report}");
}

#[test]
fn a_runtime_that_succeeds_at_anything_is_caught() {
    let report = run_runtime(Flaws { succeeds_at_anything: true, ..Flaws::default() }, opts());
    assert!(failed(&report, "unknown request gets unsupported"), "{report}");
}

#[test]
fn a_runtime_that_survives_garbage_by_ignoring_it_is_caught() {
    let report = run_runtime(Flaws { tolerates_garbage: true, ..Flaws::default() }, opts());
    assert!(failed(&report, "not JSON closes the connection"), "{report}");
    assert!(failed(&report, "over 64 KiB closes the connection"), "{report}");
}

#[test]
fn a_runtime_that_answers_before_the_handshake_is_caught() {
    let report = run_runtime(Flaws { skips_not_ready: true, ..Flaws::default() }, opts());
    assert!(failed(&report, "before the handshake is not_ready"), "{report}");
}

#[test]
fn a_runtime_that_does_not_tell_the_old_controller_is_caught() {
    let report = run_runtime(Flaws { silent_supersede: true, ..Flaws::default() }, opts());
    assert!(failed(&report, "supersedes"), "{report}");
}

#[test]
fn a_runtime_whose_event_counter_repeats_is_caught() {
    let report = run_runtime(Flaws { repeats_counters: true, ..Flaws::default() }, opts());
    assert!(failed(&report, "numbered upward"), "{report}");
}

#[test]
fn a_runtime_that_fails_a_message_for_an_unknown_key_is_caught() {
    let report = run_runtime(Flaws { refuses_unknown_keys: true, ..Flaws::default() }, opts());
    assert!(failed(&report, "settings.set reports an unknown key"), "{report}");
}

#[test]
fn diagnostics_are_checked_when_offered_and_bounded() {
    let good = run_runtime(Flaws { offers_diagnostics: true, ..Flaws::default() }, opts());
    assert!(good.ok(), "{good}");
    assert!(good.cases.iter().any(|c| c.name.contains("diagnostics.get is bounded") && c.outcome == Outcome::Pass), "{good}");
    let bad = run_runtime(Flaws { offers_diagnostics: true, too_many_diagnostic_lines: true, ..Flaws::default() }, opts());
    assert!(failed(&bad, "diagnostics.get is bounded"), "{bad}");
}

#[test]
fn a_capability_the_runtime_did_not_offer_is_skipped_not_failed() {
    // The reference runtime offers no `flags`, `assets.overlay` or presence; the
    // harness must not send those, and says nothing about them.
    let report = run_runtime(Flaws::default(), opts());
    assert!(!report.cases.iter().any(|c| c.name.contains("flags")), "{report}");
    // ... while a read-only verb it did not enable is probed, and refused.
    assert!(
        report.cases.iter().any(|c| c.name.contains("did not enable") && c.outcome == Outcome::Pass),
        "{report}"
    );
}

#[test]
fn the_harness_works_over_a_unix_socket_too() {
    use std::os::unix::net::{UnixListener, UnixStream};
    let dir = std::env::temp_dir().join(format!("cordial-protocol-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ctl.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let rt = Runtime::new("org.example.reference", Flaws::default());
    let server = {
        let rt = rt.clone();
        std::thread::spawn(move || {
            // Serve connections until the harness stops dialling.
            listener.set_nonblocking(false).unwrap();
            for stream in listener.incoming().take(32) {
                let Ok(stream) = stream else { break };
                let _ = stream.set_read_timeout(Some(TIMEOUT));
                let link = Link::new(stream.try_clone().unwrap(), stream);
                let rt = rt.clone();
                std::thread::spawn(move || rt.serve(link));
            }
        })
    };
    let mut connect = || -> io::Result<Link> { Link::unix(UnixStream::connect(&path)?, TIMEOUT) };
    let report = run_against_runtime(&mut connect, &opts());
    println!("{report}");
    assert!(report.ok(), "{report}");
    drop(server); // the listener thread ends with the process
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- a reference launcher ---------------------------------------------------

#[derive(Clone, Copy, Default)]
struct LauncherFlaws {
    /// Does not close on a line that is not a frame.
    keeps_garbage_connections: bool,
    /// Closes on any bad thing, including an out-of-range event it should drop.
    closes_on_range_errors: bool,
    /// Accepts any major.
    accepts_any_major: bool,
    /// Accepts any runtime id.
    accepts_any_runtime_id: bool,
}

/// The launcher side of one session, running until the link closes.
fn launcher_session(link: Link, manifest_id: &str, flaws: LauncherFlaws) {
    let mut writer = link.writer;
    let mut lines = LineReader::new(link.reader);
    let ours: std::collections::BTreeMap<String, u32> = [(caps::LIFECYCLE, 1), (caps::SETTINGS, 1)].iter().map(|(n, v)| (n.to_string(), *v)).collect();
    let hello = Hello { protocol: Protocol::CURRENT, cordial: "test".into(), session: "ab12cd34".into(), caps: ours.clone(), reattach: false };
    let first = Frame::Request(msg::request(1, names::HELLO, &hello));
    if writer.write_all(encode_line(&first).as_bytes()).is_err() {
        return;
    }
    let mut live: Option<Negotiated> = None;
    let mut idle = 0;
    loop {
        let line = match lines.next_line() {
            Ok(Next::Line(l)) => l,
            Ok(Next::TooLong) | Ok(Next::NotUtf8) if flaws.keeps_garbage_connections => continue,
            Ok(_) => return,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                idle += 1;
                if idle > 100 {
                    return;
                }
                continue;
            }
            Err(_) => return,
        };
        idle = 0;
        let frame = match decode_line(&line) {
            Ok(f) => f,
            Err(DecodeError::Limit(_)) if !flaws.closes_on_range_errors => continue,
            Err(_) if flaws.keeps_garbage_connections => continue,
            Err(_) => return,
        };
        match frame {
            Frame::Reply(r) if live.is_none() => {
                let Ok(answer) = r.payload::<HelloReply>() else { return };
                let Ok(n) = cordial_protocol::negotiate(Protocol::CURRENT, &ours, answer.protocol, &answer.caps) else {
                    if flaws.accepts_any_major {
                        live = Some(Negotiated { protocol: answer.protocol, caps: Default::default() });
                        continue;
                    }
                    return;
                };
                if answer.runtime.id != manifest_id && !flaws.accepts_any_runtime_id {
                    return;
                }
                if n.caps.contains_key(caps::SETTINGS) {
                    let set = msg::SettingsSet::new(vec![cordial_protocol::Update::Throttle(cordial_protocol::settings::Throttle::Off)]);
                    let req = Frame::Request(cordial_protocol::Request::new(2, names::SETTINGS_SET, set.to_payload()));
                    if writer.write_all(encode_line(&req).as_bytes()).is_err() {
                        return;
                    }
                }
                live = Some(n);
            }
            Frame::Event(e) => {
                if msg::check(msg::Kind::Event, &e.ev, &e.p).is_err() && flaws.closes_on_range_errors {
                    return;
                }
            }
            _ => {}
        }
    }
}

fn run_launcher(flaws: LauncherFlaws, test_identity: bool) -> Report {
    let manifest_id = "org.example.conformance";
    let mut launch = move || -> io::Result<Link> {
        let (ours, theirs) = duplex_with_timeout(TIMEOUT);
        std::thread::spawn(move || launcher_session(theirs, manifest_id, flaws));
        Ok(ours)
    };
    run_against_launcher(&mut launch, &LauncherOpts { test_identity, ..LauncherOpts::default() })
}

#[test]
fn a_correct_launcher_passes_every_case() {
    let report = run_launcher(LauncherFlaws::default(), true);
    println!("{report}");
    assert!(report.ok(), "{report}");
    assert!(report.passed() >= 20, "too few cases ran:\n{report}");
}

#[test]
fn a_launcher_that_keeps_a_connection_after_garbage_is_caught() {
    let report = run_launcher(LauncherFlaws { keeps_garbage_connections: true, ..LauncherFlaws::default() }, true);
    assert!(failed(&report, "closes on: not JSON"), "{report}");
    assert!(failed(&report, "closes on: a line one byte over 64 KiB"), "{report}");
}

#[test]
fn a_launcher_that_closes_on_an_out_of_range_event_is_caught() {
    let report = run_launcher(LauncherFlaws { closes_on_range_errors: true, ..LauncherFlaws::default() }, true);
    assert!(failed(&report, "drops the message and keeps the connection"), "{report}");
}

#[test]
fn a_launcher_that_accepts_any_major_is_caught() {
    let report = run_launcher(LauncherFlaws { accepts_any_major: true, ..LauncherFlaws::default() }, true);
    assert!(failed(&report, "protocol major"), "{report}");
}

#[test]
fn a_launcher_that_accepts_a_renamed_runtime_is_caught() {
    let report = run_launcher(LauncherFlaws { accepts_any_runtime_id: true, ..LauncherFlaws::default() }, true);
    assert!(failed(&report, "renames the runtime"), "{report}");
}

#[test]
fn identity_is_skipped_when_no_manifest_id_is_given() {
    let report = run_launcher(LauncherFlaws::default(), false);
    assert!(report.ok(), "{report}");
    assert!(report.cases.iter().any(|c| matches!(c.outcome, Outcome::Skip(_)) && c.name.contains("renames")));
}
