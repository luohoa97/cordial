//! The server against the crate's conformance harness, against the launcher's
//! own client (`cordial_shell::runtime_session`), and against a launcher that
//! behaves badly.
//!
//! The backend is a fake: applying a real setting stores to this test process's
//! globals, and a real `lifecycle.stop` asks the pump of whichever test binary
//! this is to quit. What the fake does not replace is anything about the
//! protocol, which is all of the server under test.

use super::*;
use cordial_protocol::conformance::{run_against_runtime, Link as ConfLink, Outcome, RuntimeOpts};
use cordial_shell::runtime_session::{Link, OpenError, OpenOptions};
use std::sync::atomic::AtomicUsize;

struct Fake {
    applied: Mutex<Vec<Update>>,
    stops: AtomicUsize,
}

impl Fake {
    fn new() -> Arc<Fake> {
        Arc::new(Fake { applied: Mutex::new(Vec::new()), stops: AtomicUsize::new(0) })
    }
}

impl Backend for Fake {
    fn apply(&self, update: &Update) -> Option<String> {
        self.applied.lock().unwrap().push(update.clone());
        matches!(update, Update::AudioOutput(_)).then(|| "nothing was playing".to_string())
    }

    fn current(&self) -> BTreeMap<String, Value> {
        let mut m: BTreeMap<String, Value> = BTreeMap::new();
        m.insert("throttle".into(), "visible".into());
        m.insert("close_on_leave".into(), false.into());
        for u in self.applied.lock().unwrap().iter() {
            m.insert(u.key().to_string(), cordial_protocol::msg::SettingsSet::new(vec![u.clone()]).to_payload()[u.key()].clone());
        }
        m
    }

    fn declared(&self) -> BTreeMap<String, Applies> {
        declared_settings()
    }

    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }

    fn client(&self) -> ClientIdent {
        ClientIdent { name: "Roblox".into(), version: "9.9.9".into(), build: "test".into() }
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        // /tmp and a short name: `sun_path` holds 108 bytes.
        let dir = std::env::temp_dir().join(format!("cordial-ctl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A session directory the way the launcher names one.
        Scratch(dir.join("runtime").join(socket::session_id()))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap().parent().unwrap());
    }
}

fn serve(tag: &str) -> (Scratch, Server, Arc<Fake>) {
    let dir = Scratch::new(tag);
    let fake = Fake::new();
    let server = serve_in(&dir.0, fake.clone()).expect("the server binds");
    (dir, server, fake)
}

fn open(dir: &Path, reattach: bool) -> Result<Link, OpenError> {
    let mut o = OpenOptions::new(dir.file_name().unwrap().to_string_lossy());
    o.reattach = reattach;
    Link::open(dir, o)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn the_conformance_harness_passes_against_the_server() {
    let (dir, _server, fake) = serve("conf");
    let mut connect = || -> std::io::Result<ConfLink> {
        ConfLink::unix(socket::connect(&dir.0)?, Duration::from_secs(5))
    };
    let opts = RuntimeOpts { manifest_id: Some(RUNTIME_ID.to_string()), test_stop: true, ..RuntimeOpts::default() };
    let report = run_against_runtime(&mut connect, &opts);
    println!("{report}");
    assert!(report.ok(), "{report}");
    // Not a vacuous pass: the cases that need a capability ran, and the one
    // capability this runtime does not offer was skipped and not faked.
    assert!(report.passed() >= 14, "{report}");
    let skipped: Vec<_> = report.cases.iter().filter(|c| matches!(c.outcome, Outcome::Skip(_))).map(|c| c.name.as_str()).collect();
    assert_eq!(skipped, vec!["diagnostics.get"], "{report}");
    assert_eq!(fake.stops.load(Ordering::SeqCst), 1, "lifecycle.stop reached the backend exactly once");
}

#[test]
fn the_launchers_own_client_opens_asks_and_sets() {
    let (dir, _server, fake) = serve("link");
    let link = open(&dir.0, false).expect("a handshake");
    let est = link.established();
    assert_eq!((est.runtime.id.as_str(), est.client.version.as_str()), (RUNTIME_ID, "9.9.9"));
    // The intersection, each at the lower version: the runtime offers no flags,
    // assets or diagnostics, so none is live whatever the launcher offered.
    let live: Vec<_> = est.caps.keys().map(String::as_str).collect();
    assert_eq!(live, vec!["events.core", "events.presence", "lifecycle", "settings", "state"]);
    // The declaration travels with `settings.get`: ten live keys and the launch
    // environment's next-launch ones.
    assert_eq!(est.declared.get("throttle"), Some(&Applies::Live));
    assert_eq!(est.declared.get("present_mode"), Some(&Applies::NextLaunch));
    assert_eq!(est.declared.get("frame_rate_limit"), Some(&Applies::Live));
    assert_eq!(est.values["throttle"], "visible");

    let reply = link
        .settings_set(&[Update::Throttle(cordial_protocol::settings::Throttle::Off), Update::AudioOutput("sink".into())])
        .expect("both keys applied");
    assert_eq!(reply.applied, vec!["audio_output", "throttle"]);
    assert_eq!(reply.notes.get("audio_output").map(String::as_str), Some("nothing was playing"), "a caveat survives the trip");
    assert_eq!(fake.applied.lock().unwrap().len(), 2);
    assert_eq!(link.settings_get().unwrap().values["throttle"], "off", "get reports what set changed");
}

#[test]
fn a_request_before_hello_is_not_ready_and_hello_twice_is_refused() {
    let (dir, _server, _fake) = serve("ready");
    let mut conn = socket::connect(&dir.0).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"{\"id\":1,\"m\":\"state.get\"}\n").unwrap();
    let mut lines = LineReader::new(conn.try_clone().unwrap());
    let Next::Line(l) = lines.next_line().unwrap() else { panic!("a reply") };
    let Ok(Frame::Reply(r)) = decode_line(&l) else { panic!("a reply") };
    assert_eq!(r.result.unwrap_err().code, Code::NotReady);

    let hello = r#"{"id":2,"m":"hello","p":{"protocol":{"major":1,"minor":0},"cordial":"t","session":"abcd1234","caps":{"lifecycle":1}}}"#;
    conn.write_all(format!("{hello}\n").as_bytes()).unwrap();
    let Next::Line(l) = lines.next_line().unwrap() else { panic!("a reply") };
    assert!(matches!(decode_line(&l), Ok(Frame::Reply(Reply { result: Ok(_), .. }))));
    conn.write_all(format!("{}\n", hello.replace("\"id\":2", "\"id\":3")).as_bytes()).unwrap();
    let Next::Line(l) = lines.next_line().unwrap() else { panic!("a reply") };
    let Ok(Frame::Reply(r)) = decode_line(&l) else { panic!("a reply") };
    assert_eq!(r.result.unwrap_err().code, Code::Invalid, "a second hello is not a reattach, that is a new connection");
}

#[test]
fn a_launcher_on_another_major_is_refused_and_the_runtime_lives() {
    let (dir, _server, _fake) = serve("major");
    let mut conn = socket::connect(&dir.0).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"{\"id\":1,\"m\":\"hello\",\"p\":{\"protocol\":{\"major\":2,\"minor\":0},\"cordial\":\"t\",\"session\":\"abcd1234\",\"caps\":{}}}\n").unwrap();
    let mut lines = LineReader::new(conn);
    let Next::Line(l) = lines.next_line().unwrap() else { panic!("a reply") };
    let Ok(Frame::Reply(r)) = decode_line(&l) else { panic!("a reply") };
    let err = r.result.unwrap_err();
    assert_eq!(err.code, Code::Unsupported);
    assert!(err.detail.contains("major 2"), "{}", err.detail);
    assert!(matches!(lines.next_line().unwrap(), Next::Eof), "then it closes that connection");
    assert!(open(&dir.0, false).is_ok(), "and the next launcher is served");
}

#[test]
fn the_runtime_survives_the_launcher_closing_and_a_reattach_finds_what_it_missed() {
    let (dir, server, _fake) = serve("reattach");
    let first = open(&dir.0, false).unwrap();
    server.publish_engine_version("0.1234.5".into());
    wait_for("the version event", || first.snapshot().engine_version.is_some());
    drop(first);
    wait_for("the controller to go", || !server.controlled());

    // Events while nobody is attached are let go; the snapshot keeps them.
    server.publish_game_joined(
        msg::GameJoined { place_id: 1818, universe_id: Some(7), job_id: Some("job".into()), at: 1_700_000_000_000 },
        msg::SessionState { signed_in: true, user_id: Some(42) },
    );

    let again = open(&dir.0, true).expect("the same runtime, reattached");
    let snap = again.snapshot();
    assert_eq!(snap.engine_version.unwrap().version, "0.1234.5");
    assert_eq!(snap.game_joined.unwrap().place_id, 1818, "what happened while nobody was attached");
    assert_eq!(snap.session_state.unwrap().user_id, Some(42));

    // And the stream carries on, with numbers that did not restart.
    server.publish_game_left(msg::GameLeft { at: 1_700_000_001_000 });
    wait_for("game.left", || again.snapshot().game_left.is_some());
    assert!(again.snapshot().game_joined.is_none(), "leaving clears the join");
    // The counter carried on across the gap, so the first event this link saw is
    // not number 1. A link cannot count from a number it never saw, which is why
    // it asks for the snapshot on every attach instead of inferring what it
    // missed from `n`.
    assert!(again.events_seen() >= 1);
    assert_eq!(again.events_missed(), 0, "no gap is claimed that this link cannot have seen");
}

#[test]
fn a_newer_controller_supersedes_the_older_and_the_older_does_not_fight_back() {
    let (dir, server, _fake) = serve("supersede");
    let old = open(&dir.0, false).unwrap();
    let newer = open(&dir.0, true).unwrap();
    wait_for("the old link to be told", || old.superseded());
    assert_eq!(old.bye().as_deref(), Some("superseded"));
    assert!(!old.alive());
    assert!(newer.alive() && !newer.superseded());
    server.publish_engine_version("1".into());
    wait_for("the event on the newer link", || newer.snapshot().engine_version.is_some());
    assert!(old.snapshot().engine_version.is_none(), "the old one hears nothing more");
}

#[test]
fn unsupported_is_never_faked() {
    let (dir, _server, _fake) = serve("unsupported");
    let link = open(&dir.0, false).unwrap();
    for verb in ["flags.apply", "flags.live", "assets.overlay.set", "diagnostics.get", "x-nothing.here"] {
        let err = link.call(verb, serde_json::json!({})).unwrap_err();
        assert!(err.contains("unsupported"), "{verb}: {err}");
    }
    // An unknown settings key is named and the known ones still apply.
    let p = link.call("settings.set", serde_json::json!({"warp_drive": "on", "throttle": "off"})).unwrap();
    let r: msg::SettingsReply = msg::payload(&p).unwrap();
    assert_eq!((r.applied, r.ignored), (vec!["throttle".to_string()], vec!["warp_drive".to_string()]));
    // A value outside the set refuses the whole message and applies none of it.
    let err = link.call("settings.set", serde_json::json!({"throttle": "sometimes", "close_on_leave": true})).unwrap_err();
    assert!(err.contains("invalid"), "{err}");
}

#[test]
fn a_launcher_that_stops_reading_never_stalls_the_publisher() {
    let (dir, server, _fake) = serve("stall");
    // Say hello and then never read again, with a socket buffer that fills.
    let mut conn = socket::connect(&dir.0).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let hello = r#"{"id":1,"m":"hello","p":{"protocol":{"major":1,"minor":0},"cordial":"t","session":"abcd1234","caps":{"lifecycle":1,"events.core":1,"events.presence":1}}}"#;
    conn.write_all(format!("{hello}\n").as_bytes()).unwrap();
    let mut lines = LineReader::new(conn.try_clone().unwrap());
    let Next::Line(_) = lines.next_line().unwrap() else { panic!("a reply") };
    wait_for("attach", || server.controlled());

    let big = "x".repeat(400);
    let started = Instant::now();
    for i in 0..20_000u64 {
        server.publish_game_presence(msg::GamePresence { details: Some(format!("{big}{i}")), ..Default::default() });
    }
    // 20,000 publishes of 400 bytes is 8 MB, far past a socket buffer, with
    // nobody reading. The producer is the engine's side, and it did not wait.
    assert!(started.elapsed() < Duration::from_millis(1500), "publishing blocked for {:?}", started.elapsed());
    // The queue is bounded and says how much it lost.
    wait_for("the queue to hit its bound", || server.shared.queue.len() >= 256);
}

#[test]
fn the_flood_the_queue_dropped_is_reported_to_the_launcher_that_comes_back() {
    let (dir, server, _fake) = serve("dropped");
    let heard = Arc::new(AtomicUsize::new(0));
    let mut o = OpenOptions::new(dir.0.file_name().unwrap().to_string_lossy());
    {
        let heard = heard.clone();
        o.on_event = Arc::new(move |ev| {
            if ev.ev == names::GAME_PRESENCE {
                heard.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    let link = Link::open(&dir.0, o).unwrap();
    // Faster than one pump thread can write: some are dropped and counted.
    const SENT: usize = 5_000;
    for i in 0..SENT {
        server.publish_game_presence(msg::GamePresence { details: Some(format!("d{i}")), ..Default::default() });
    }
    // Every event is either delivered or counted, so once the pump has caught
    // up the two add to what was published: nothing vanishes unreported.
    wait_for("delivered and reported to add up", || {
        heard.load(Ordering::SeqCst) as u64 + link.events_dropped_by_runtime() == SENT as u64
    });
    assert_eq!(link.rejected(), 0, "and none of it broke a rule");
    assert_eq!(link.events_missed(), 0, "a dropped event consumes no number, so the stream has no gaps");
    // The snapshot is the truth whatever the stream lost.
    assert_eq!(server.shared.state().game_presence.as_ref().unwrap().details.as_deref(), Some("d4999"));
}

#[test]
fn an_oversize_line_closes_the_connection_and_the_runtime_still_answers() {
    let (dir, server, _fake) = serve("oversize");
    let mut conn = socket::connect(&dir.0).unwrap();
    conn.write_all(&vec![b'x'; 70_000]).unwrap();
    conn.write_all(b"\n").unwrap();
    let mut lines = LineReader::new(conn);
    assert!(matches!(lines.next_line(), Ok(Next::Eof)), "a line that is not a frame closes the connection");
    assert!(open(&dir.0, false).is_ok(), "never the runtime");
    assert_eq!(server.rejected(), 0, "a line that closes is not a dropped frame");
}

#[test]
fn a_frame_over_a_limit_is_dropped_and_counted_and_the_connection_stays() {
    let (dir, server, _fake) = serve("limit");
    let link = open(&dir.0, false).unwrap();
    let long = "k".repeat(600);
    let err = link.call("x-test.long", serde_json::json!({ "v": long })).unwrap_err();
    assert!(err.contains("no reply"), "dropped, so no answer: {err}");
    assert_eq!(server.rejected(), 1);
    assert!(link.alive(), "and the connection stayed");
    assert!(link.settings_get().is_ok(), "and still works");
}

#[test]
fn a_presence_is_projected_onto_the_wire_and_cut_to_its_bound() {
    let payload = serde_json::json!({
        "details": "d".repeat(900),
        "state": "by someone",
        "start": 1_700_000_000,
        "large_image_key": "key",
        // The plugin payload also carries these; the wire's presence does not.
        "place_id": 17625359962u64,
        "job_id": "3182c122",
    });
    let p = presence_from(&payload);
    assert_eq!(p.details.as_ref().map(String::len), Some(512));
    assert_eq!((p.state.as_deref(), p.start, p.end), (Some("by someone"), Some(1_700_000_000), None));
    assert!(serde_json::to_value(&p).unwrap().get("place_id").is_none());
    // And it survives the launcher's own validation.
    let frame = msg::event(names::GAME_PRESENCE, 1, &p);
    assert!(Frame::Event(frame).check_limits().is_ok());
    // A character boundary is respected rather than split.
    assert_eq!(bound(&"é".repeat(300)).len(), 512);
    // Leaving a game publishes `{}`, which is an empty presence and not a failure.
    assert_eq!(presence_from(&serde_json::json!({})), msg::GamePresence::default());
}

#[test]
fn shutting_down_says_bye_and_takes_the_directory() {
    let (dir, server, _fake) = serve("bye");
    let link = open(&dir.0, false).unwrap();
    server.shut_down();
    wait_for("bye", || link.bye().is_some());
    assert_eq!(link.bye().as_deref(), Some("exit"));
    assert!(!link.superseded());
    assert!(!dir.0.exists(), "the next launch finds no socket nobody listens on");
}

#[test]
fn a_stale_session_beside_ours_is_removed_and_a_non_session_is_left() {
    let dir = Scratch::new("stale");
    let runtime = dir.0.parent().unwrap();
    let stale = runtime.join("deadbeef");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::create_dir_all(runtime.join("notes")).unwrap();
    let _server = serve_in(&dir.0, Fake::new()).unwrap();
    assert!(!stale.exists(), "a killed client's leftover");
    assert!(runtime.join("notes").exists(), "something a user put there");
    assert!(dir.0.join("ctl.sock").exists());
}

#[test]
fn the_declaration_covers_every_wire_key_and_names_only_launch_time_extras() {
    let d = declared_settings();
    for key in cordial_protocol::settings::KEYS {
        assert_eq!(d.get(key), Some(&Applies::Live), "{key}");
    }
    for key in NEXT_LAUNCH_KEYS {
        assert_eq!(d.get(key), Some(&Applies::NextLaunch), "{key}");
        assert!(!cordial_protocol::settings::KEYS.contains(&key), "{key} cannot be both");
    }
    assert_eq!(d.len(), cordial_protocol::settings::KEYS.len() + NEXT_LAUNCH_KEYS.len());
}

#[test]
fn a_session_directory_outside_one_called_runtime_sweeps_nothing() {
    // The environment variable names a path the launcher chose. A session-shaped
    // directory beside it is not that launcher's to have deleted.
    let root = std::env::temp_dir().join(format!("cordial-ctl-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bystander = root.join("deadbeef");
    std::fs::create_dir_all(&bystander).unwrap();
    let _server = serve_in(&root.join("mine"), Fake::new()).unwrap();
    assert!(bystander.exists());
    let _ = std::fs::remove_dir_all(&root);
}
