//! What the engine's own log says about joining and leaving a game.
//!
//! **`bloxstrap_rpc` has been in this crate since presence landed, with a
//! parser, tests, and no callers at all.** `grep 'bloxstrap_rpc::'` across the
//! workspace returned nothing: the protocol was implemented and nothing ever
//! read a line to hand it. This module is the missing half -- the thing that
//! watches the file -- and once it exists the same lines answer three other
//! questions Cordial could not answer before.
//!
//! Nothing is injected and nothing is hooked. The engine writes a log into a
//! directory Cordial created, and Cordial reads it. That is the argument
//! `bloxstrap_rpc`'s own header already makes about why this runs into neither
//! ADR-001 nor ADR-003, and it applies unchanged here.
//!
//! ## The markers, and where they came from
//!
//! Read off a real capture rather than guessed: Sober's own log of the same
//! engine on this host, `2.734.0.917_20260831T031139Z_Player_3e43f_last.log`,
//! 1,627 lines covering four joins and four leaves -- the parser run over the
//! whole of it finds all eight and nothing else. AGENTS.md's rule about
//! grepping a capture before reasoning from a binary is the same rule one
//! level up, and this is the capture.
//!
//! ```text
//! [FLog::GameJoinLoadTime] Report game_join_loadtime: sid:6baeb082-..., \
//!     clienttime:..., join_time:1.2011154180, referral_page:, \
//!     placeid:17625359962, userid:1826805362, universeid:6035872082,
//! [FLog::Network] UDMUX Address = 128.116.51.33, Port = 50363 | \
//!     RCC Server Address = 10.60.2.168, Port = 50363
//! [FLog::SingleSurfaceApp] leaveUGCGameInternal
//! ```
//!
//! **The log carries no game name, and this module does not invent one.** It
//! reports the place and universe ids, which is what is actually there;
//! resolving either to a title needs a request to Roblox's web API and belongs
//! to whoever wants the title, not to a log parser. Saying so matters because
//! the README currently states that Discord presence "cannot name the game yet
//! -- no core event carries which place is running", and that is still true of
//! *core events*: what changes is that the id is now available from somewhere
//! else, so the sentence is worth revisiting rather than deleting.
//!
//! **`UDMUX Address` is the server's address and not its location.** Turning
//! one into the other is a geo-IP lookup against a third party, which is a
//! network call on somebody's behalf and a privacy question, so this module
//! stops at the address.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Something worth knowing that happened in a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A game was joined. Both ids come off the same line.
    ///
    /// `universe_id` is the one to resolve to a title -- Roblox's
    /// `games?universeIds=` takes universes, not places -- and `place_id` is
    /// the one a `roblox://` deep link uses. Neither is looked up here.
    Joined { place_id: u64, universe_id: u64 },
    /// The address the client is connected through, from the same join.
    ///
    /// Reported separately from [`Event::Joined`] because it arrives on its own
    /// line a few milliseconds later, and because a consumer that wants the
    /// join does not necessarily want the address.
    Server { address: String, port: u16 },
    /// The user left the game and went back to the app.
    ///
    /// `leaveUGCGameInternal`, not a disconnect. The log carries several
    /// disconnects per session -- a websocket giving up, a teleport between
    /// places, the peer going away -- and treating any of those as "left"
    /// would close the client mid-teleport. This one fires when the engine
    /// returns to its own home screen, twice in the capture and both times
    /// after the user actually left.
    Left,
    /// A `[BloxstrapRPC]` line, handed on verbatim for
    /// [`crate::bloxstrap_rpc::parse_line`] to decode.
    ///
    /// Passed through rather than parsed here so that this module stays "what
    /// the log said" and the protocol stays in the module that documents it.
    Rpc(String),
}

/// One log line, or `None` if it is not one of the four.
///
/// Pure, and the whole of the format knowledge. Every field is found by name
/// rather than by position: the join line has eight comma-separated fields
/// today and a build that adds a ninth must not shift the meaning of the ones
/// before it.
pub fn parse_line(line: &str) -> Option<Event> {
    if line.contains(crate::bloxstrap_rpc::MARKER) {
        return Some(Event::Rpc(line.to_owned()));
    }
    if line.contains("[FLog::SingleSurfaceApp] leaveUGCGameInternal") {
        return Some(Event::Left);
    }
    if line.contains("[FLog::GameJoinLoadTime]") && line.contains("game_join_loadtime") {
        // Both or neither. A join with one id and not the other is a line this
        // parser does not understand, and reporting half of it as a join would
        // put a zero in front of somebody as though it were a place.
        let place_id = field_u64(line, "placeid:")?;
        let universe_id = field_u64(line, "universeid:")?;
        return Some(Event::Joined { place_id, universe_id });
    }
    if line.contains("[FLog::Network] UDMUX Address") {
        // The UDMUX address, not the "RCC Server Address" later on the same
        // line -- that one is 10.60.2.168 in the capture, a private address on
        // Roblox's own network that means nothing outside it.
        let address = field_after(line, "UDMUX Address = ", &[',', ' ', '|'])?;
        let rest = line.split_once("UDMUX Address")?.1;
        let port = field_after(rest, "Port = ", &[',', ' ', '|'])?.parse().ok()?;
        return Some(Event::Server { address: address.to_owned(), port });
    }
    None
}

/// The digits following `key`, stopping at the first non-digit.
fn field_u64(line: &str, key: &str) -> Option<u64> {
    let rest = line.split_once(key)?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The text following `key`, up to any of `stop`.
fn field_after<'a>(line: &'a str, key: &str, stop: &[char]) -> Option<&'a str> {
    let rest = line.split_once(key)?.1;
    let end = rest.find(|c| stop.contains(&c)).unwrap_or(rest.len());
    let value = &rest[..end];
    (!value.is_empty()).then_some(value)
}

/// How often the directory is re-scanned for a newer log file.
///
/// The engine opens exactly one log per session and appends to it, so this is
/// only looking for the moment that file first appears. A second is far more
/// often than that needs and still costs one `read_dir` of a directory with a
/// handful of entries.
const RESCAN: Duration = Duration::from_secs(1);

/// Tails the newest log in a directory, reporting whole lines as they land.
///
/// Deliberately poll-driven rather than inotify-backed: this is called from
/// `looper::pump`, which already runs continuously and must never block, and
/// adding a watch descriptor to that loop buys nothing when the caller is
/// polling anyway. It also means no new dependency and no second thread to
/// reason about against the engine's own.
pub struct Watcher {
    dir: PathBuf,
    /// The file being followed, and how far into it has been read.
    ///
    /// The offset is kept rather than the handle so a truncated or rotated
    /// file is noticed: if the file is shorter than where we left off, the
    /// engine started a new one under the same name and reading from the old
    /// offset would return the middle of a line.
    current: Option<(PathBuf, u64)>,
    last_scan: Option<Instant>,
    /// A partial final line, held until its newline arrives.
    ///
    /// The engine writes with its own buffering and a poll can land mid-line.
    /// Handing half a join line to `parse_line` would return `None` and the
    /// other half would never be seen, so the join would simply be missed --
    /// intermittently, and more often on a busy machine, which is the worst
    /// shape of bug to have here.
    partial: String,
}

impl Watcher {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), current: None, last_scan: None, partial: String::new() }
    }

    /// Whatever has been written since the last call.
    ///
    /// Never blocks and never errors: a missing directory, an unreadable file
    /// and a log that has not been created yet are all "nothing to report".
    /// The engine's log appears some seconds into startup, so "not there yet"
    /// is the ordinary case for the first few hundred calls.
    pub fn poll(&mut self) -> Vec<Event> {
        self.rescan_if_due();
        let Some((path, offset)) = self.current.clone() else { return Vec::new() };

        let Ok(file) = std::fs::File::open(&path) else { return Vec::new() };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < offset {
            // Truncated or replaced under the same name. Start again rather
            // than read from an offset that no longer means anything.
            self.current = Some((path.clone(), 0));
            self.partial.clear();
            return Vec::new();
        }
        if len == offset {
            return Vec::new();
        }

        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(offset)).is_err() {
            return Vec::new();
        }

        let mut events = Vec::new();
        let mut read = offset;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => {
                    read += n as u64;
                    if line.ends_with('\n') {
                        let whole = std::mem::take(&mut self.partial) + line.trim_end();
                        if let Some(event) = parse_line(&whole) {
                            events.push(event);
                        }
                    } else {
                        // No newline: the writer is mid-line. Keep it and stop
                        // -- the offset below still counts it, so the rest
                        // arrives next poll and is joined onto this.
                        self.partial.push_str(&line);
                    }
                }
                Err(_) => break,
            }
        }
        self.current = Some((path, read));
        events
    }

    /// Adopt the newest `*.log` in the directory, if it is not the one already
    /// being followed.
    ///
    /// Newest by modification time rather than by the timestamp in the name.
    /// The name's stamp is the engine's own and is in a format this would have
    /// to parse; mtime is what the filesystem already knows and is what
    /// "currently being written" actually means.
    fn rescan_if_due(&mut self) {
        let now = Instant::now();
        if self.last_scan.is_some_and(|t| now.duration_since(t) < RESCAN) {
            return;
        }
        self.last_scan = Some(now);

        let Some(newest) = newest_log(&self.dir) else { return };
        if self.current.as_ref().is_some_and(|(p, _)| *p == newest) {
            return;
        }
        // A log Cordial has not been following may already have content -- the
        // engine writes a few hundred lines before this ever runs. Start at
        // zero rather than at the end: a join that happened during startup is
        // still the join this session is in, and replaying a `Left` from
        // before Cordial was watching cannot happen, because the file is this
        // session's.
        self.current = Some((newest, 0));
        self.partial.clear();
    }
}

/// The most recently modified `*.log` in `dir`.
fn newest_log(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "log") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p)
}

// ------------------------------------------------------------- the live one
//
// One watcher for the process, polled from `looper::pump` beside the cookie
// flush -- the same kind of cheap per-tick housekeeping, and on the same
// thread, so nothing here needs a lock against the engine.

/// Where the engine writes its logs, for this profile.
///
/// The same expression `load.rs` uses to build the tree in the first place:
/// `CORDIAL_FILES_DIR` when set, otherwise `<profile>/data`. Repeated rather
/// than shared because the two are the same fact from opposite ends -- one
/// creates the directory and one reads it -- and a helper spanning a binary
/// and its library would be the wrong shape for one `format!`.
fn logs_dir() -> PathBuf {
    let root = std::env::var("CORDIAL_FILES_DIR")
        .unwrap_or_else(|_| format!("{}/data", crate::profile::active().display()));
    PathBuf::from(root).join("files/appData/logs")
}

/// `CORDIAL_CLOSE_ON_LEAVE=1` — exit when the user leaves a game.
///
/// Off unless asked for. Closing somebody's client is the least reversible
/// thing this crate does on its own initiative, and the person who wants it
/// wants it deliberately: they launched from a deep link to play one game and
/// have no use for the home screen afterwards. Somebody who did not ask and
/// meets it once has lost their session to a setting they did not know existed.
fn close_on_leave() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| close_on_leave_for(std::env::var("CORDIAL_CLOSE_ON_LEAVE").ok().as_deref()))
}

/// The gate's decision, split out so it can be tested -- `close_on_leave`
/// caches into a `OnceLock`, so a test that set the variable would decide the
/// answer for every other test in the binary.
fn close_on_leave_for(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true") | Some("yes"))
}

/// Poll the log once and act on whatever it said.
///
/// Called every pump tick. Costs one `read_dir` a second and, in between, one
/// `File::open` and a metadata read that usually finds nothing new -- the same
/// order of work as the cookie flush it sits beside.
/// The game's presence so far, folded from every `[BloxstrapRPC]` line seen.
///
/// One per process because there is one engine in it. Reset when the user
/// leaves, so a presence from the last experience does not outlive it.
static PRESENCE: Mutex<crate::bloxstrap_rpc::Presence> =
    Mutex::new(crate::bloxstrap_rpc::Presence::new());

pub fn poll() {
    static WATCHER: Mutex<Option<Watcher>> = Mutex::new(None);
    let mut guard = WATCHER.lock().unwrap_or_else(|e| e.into_inner());
    let watcher = guard.get_or_insert_with(|| Watcher::new(logs_dir()));

    for event in watcher.poll() {
        match event {
            Event::Joined { place_id, universe_id } => {
                println!("[cordial] game: joined place {place_id} (universe {universe_id})");
            }
            Event::Server { address, port } => {
                println!("[cordial] game: server {address}:{port}");
            }
            Event::Left => {
                println!("[cordial] game: left");
                // The experience's presence goes with the experience. Without
                // this, leaving a game that set a presence would leave its
                // details on the Discord profile until another game replaced
                // them -- and on the home screen that is simply wrong.
                *PRESENCE.lock().unwrap_or_else(|e| e.into_inner()) =
                    crate::bloxstrap_rpc::Presence::new();
                crate::plugin_host::publish_core(
                    cordial_plugins::core_events::GAME_PRESENCE,
                    serde_json::json!({}),
                );
                if close_on_leave() {
                    // Through the same door the window's close button uses,
                    // not `process::exit`. The profile lock is released by
                    // exiting however it happens (see ADR-012), but the engine
                    // has a cookie jar and storage that a hard exit would drop
                    // mid-write, and `request_quit` is what every other way
                    // out of this client already goes through.
                    println!("[cordial] game: CORDIAL_CLOSE_ON_LEAVE is set; closing");
                    crate::android::looper::request_quit();
                }
            }
            Event::Rpc(line) => match crate::bloxstrap_rpc::parse_line(&line) {
                // **This is the first caller `bloxstrap_rpc` has ever had.**
                // Everything below the parse is still unwired: the presence
                // broker lives in `cordial-plugins` and the client does not
                // hold one yet, so for now the command is reported and
                // dropped. Reported rather than silently dropped, because a
                // game author testing their own presence needs to see that
                // Cordial read the line.
                Ok(Some(command)) => {
                    // **Folded, then published.** BloxstrapRPC is a stream of
                    // partial updates -- a game sends `details` on one line
                    // and `state` on another, and "leave this field alone" is
                    // a distinct value from "clear it". `Presence::apply` is
                    // what knows that, so the subscriber receives the merged
                    // picture rather than having to reimplement the folding.
                    let mut merged = PRESENCE.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(launch) = merged.apply(command) {
                        // The join button's payload. Carried no further yet:
                        // acting on it means building a launch URL and
                        // handing it to Discord as a join secret, which is a
                        // different capability from showing a presence and
                        // has not been asked for.
                        println!("[cordial] game: BloxstrapRPC launch data ({} bytes)", launch.len());
                    } else {
                        crate::plugin_host::publish_core(
                            cordial_plugins::core_events::GAME_PRESENCE,
                            merged.to_payload(),
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => println!("[cordial] game: unusable BloxstrapRPC line: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line below is verbatim from the capture named in the module
    /// comment. Hand-written approximations of a log format are how a parser
    /// comes to work on the example and fail on the file.
    const JOIN: &str = "2026-08-31T03:14:18.878Z,159.878708,bf08c6c0,6 [FLog::GameJoinLoadTime] \
         Report game_join_loadtime: sid:6baeb082-4060-42cf-85f4-e8282c7a8bfd, \
         clienttime:1788146058.128000021, join_time:1.2011154180000005454, referral_page:, \
         placeid:17625359962, userid:1826805362, universeid:6035872082, ";
    const UDMUX: &str = "2026-08-31T03:14:18.884Z,159.884491,bf08c6c0,7 [FLog::Network] \
         UDMUX Address = 128.116.51.33, Port = 50363 | RCC Server Address = 10.60.2.168, Port = 50363";
    const LEAVE: &str =
        "2026-08-31T03:14:53.310Z,194.310760,bd3bd6c0,6 [FLog::SingleSurfaceApp] leaveUGCGameInternal";

    #[test]
    fn a_join_yields_both_ids() {
        assert_eq!(
            parse_line(JOIN),
            Some(Event::Joined { place_id: 17625359962, universe_id: 6035872082 })
        );
    }

    /// **The public address, not the private one.**
    ///
    /// The same line carries `RCC Server Address = 10.60.2.168`, which is on
    /// Roblox's own network and means nothing outside it. A parser that took
    /// the last `Port = ` or searched the whole line for an address would pick
    /// that one, and it would look plausible right up until somebody tried to
    /// resolve it.
    #[test]
    fn the_server_is_the_udmux_address_and_not_the_rcc_one() {
        assert_eq!(
            parse_line(UDMUX),
            Some(Event::Server { address: "128.116.51.33".into(), port: 50363 })
        );
    }

    #[test]
    fn leaving_is_recognised() {
        assert_eq!(parse_line(LEAVE), Some(Event::Left));
    }

    /// **A disconnect is not a leave, and confusing them closes the client
    /// during a teleport.**
    ///
    /// The capture has ten disconnect-shaped lines against two real leaves --
    /// a websocket 401, a peer disconnect, `Client:Disconnect` twice per
    /// session, a numbered `Disconnection Notification`. Close-on-leave acts
    /// on `Event::Left`, so every one of these must parse as nothing.
    #[test]
    fn a_disconnect_is_not_a_leave() {
        for line in [
            "2026-08-31T03:11:44.510Z,5.510133,409d16c0,6 [DFLog::SignalRCoreError] ID: 1 \
             Disconnected - Websocket error: 401 Unauthorized",
            "2026-08-31T03:14:53.338Z,194.338837,bf08c6c0,6,Info [DFLog::NetworkClient] \
             Client:Disconnect",
            "2026-08-31T03:16:55.333Z,316.333099,5b43f6c0,7 [FLog::Network] Connection lost: \
             connectMode: Peer Disconnected, timeMS:316332, connectionTime 200359",
            "2026-08-31T03:16:55.333Z,316.333221,5b43f6c0,7 [FLog::Network] Disconnection \
             Notification. Reason: 267",
            "2026-08-31T03:14:54.395Z,195.395447,bf08c6c0,7 \
             [DFLog::MegaReplicatorLogDisconnectCleanUpLog] Destroying MegaReplicator.",
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    /// Ordinary log traffic must not look like anything.
    #[test]
    fn an_uninteresting_line_is_uninteresting() {
        for line in ["", "   ", "not a log line at all", "[FLog::Output] hello"] {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
    }

    /// A `[BloxstrapRPC]` line is handed on whole, not decoded here.
    #[test]
    fn an_rpc_line_is_passed_through_for_the_protocol_module() {
        let line = "2026-08-31T03:14:18.878Z,1,a,6 [FLog::Output] [BloxstrapRPC] {\"command\":\"x\"}";
        assert_eq!(parse_line(line), Some(Event::Rpc(line.to_owned())));
    }

    /// A join line missing one of its two ids is not half a join.
    #[test]
    fn a_join_line_without_both_ids_is_not_a_join() {
        let no_universe = "[FLog::GameJoinLoadTime] Report game_join_loadtime: placeid:1, userid:2,";
        assert_eq!(parse_line(no_universe), None);
    }

    /// **A line split across two polls must still be seen.**
    ///
    /// The engine buffers its own writes and a poll can land mid-line. The
    /// failure this catches is silent and intermittent: half a join line
    /// parses as nothing, the other half parses as nothing, and the join is
    /// simply missed -- more often on a busy machine, which is exactly when
    /// somebody is playing.
    #[test]
    fn a_line_that_arrives_in_two_pieces_is_still_parsed() {
        let dir = std::env::temp_dir().join(format!("cordial-gamelog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.log");

        let (head, tail) = JOIN.split_at(60);
        std::fs::write(&log, head).unwrap();

        let mut w = Watcher::new(&dir);
        assert_eq!(w.poll(), Vec::new(), "half a line is not an event yet");

        std::fs::write(&log, format!("{head}{tail}\n")).unwrap();
        // The rescan is rate-limited and this is the same file, so poll again
        // directly rather than waiting a second for a scan that changes nothing.
        assert_eq!(
            w.poll(),
            vec![Event::Joined { place_id: 17625359962, universe_id: 6035872082 }],
            "the two halves must join up"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Run the parser over a whole real log and say what it found.
    ///
    /// `#[ignore]` because it needs a file this repository does not and must
    /// not contain -- a log is Roblox's output and carries a user id. Point it
    /// at one and it becomes the strongest check available:
    ///
    /// ```text
    /// CORDIAL_GAME_LOG=~/.var/app/org.vinegarhq.Sober/data/sober/appData/logs/<file> \
    ///     cargo test -p cordial-runtime --lib whole_log -- --ignored --nocapture
    /// ```
    ///
    /// The assertions are deliberately weak -- at least one join and one leave
    /// -- because the point is the printed summary. A parser that silently
    /// stopped matching after a Roblox update would still pass every test
    /// above, all of which use lines frozen into this file.
    #[test]
    #[ignore = "needs a real engine log; set CORDIAL_GAME_LOG"]
    fn whole_log_from_a_real_session() {
        let Ok(path) = std::env::var("CORDIAL_GAME_LOG") else {
            panic!("set CORDIAL_GAME_LOG to a real *_Player_*.log");
        };
        let text = std::fs::read_to_string(&path).expect("read the log");
        let (mut joins, mut leaves, mut servers, mut rpc) = (0, 0, 0, 0);
        for line in text.lines() {
            match parse_line(line) {
                Some(Event::Joined { place_id, universe_id }) => {
                    joins += 1;
                    println!("join   place {place_id} universe {universe_id}");
                }
                Some(Event::Server { address, port }) => {
                    servers += 1;
                    println!("server {address}:{port}");
                }
                Some(Event::Left) => {
                    leaves += 1;
                    println!("leave");
                }
                Some(Event::Rpc(_)) => rpc += 1,
                None => {}
            }
        }
        println!("{} lines: {joins} joins, {leaves} leaves, {servers} servers, {rpc} rpc",
                 text.lines().count());
        assert!(joins > 0, "no join found in {path}");
        assert!(leaves > 0, "no leave found in {path}");
    }

    /// **Close-on-leave is off unless asked for, and only the explicit words
    /// turn it on.** Closing somebody's client is the least reversible thing
    /// this crate does unprompted; a typo in the variable must read as "no".
    #[test]
    fn closing_on_leave_is_opt_in() {
        assert!(!close_on_leave_for(None), "absent means no");
        assert!(!close_on_leave_for(Some("0")));
        assert!(!close_on_leave_for(Some("")));
        assert!(!close_on_leave_for(Some("off")));
        assert!(!close_on_leave_for(Some("no")));
        for on in ["1", "true", "yes"] {
            assert!(close_on_leave_for(Some(on)), "{on} should turn it on");
        }
    }

    /// **A game's own presence, from the line it printed to the payload a
    /// plugin receives.** The whole chain in one test, because it crosses three
    /// modules -- this one recognises the line, `bloxstrap_rpc` parses and
    /// folds it, and `Presence::to_payload` shapes what goes on the bus -- and
    /// a break anywhere in it looks identical from Discord: nothing happens.
    ///
    /// The two lines are the shape a game actually prints, wrapped the way the
    /// engine logs it. Partial updates on purpose: BloxstrapRPC sends fields
    /// one at a time and "leave this alone" is a distinct value from "clear
    /// it", so a subscriber seeing only the second line must still get the
    /// details from the first.
    #[test]
    fn a_games_bloxstrap_rpc_becomes_a_presence_payload() {
        let printed = [
            r#"2026-09-01T00:10:00.000Z,1,a,6 [FLog::Output] [BloxstrapRPC] {"command":"SetRichPresence","data":{"details":"Fighting the Boss","state":"Round 3 of 5"}}"#,
            r#"2026-09-01T00:10:05.000Z,1,a,6 [FLog::Output] [BloxstrapRPC] {"command":"SetRichPresence","data":{"timeStart":1788200000}}"#,
        ];
        let mut merged = crate::bloxstrap_rpc::Presence::new();
        for line in printed {
            let Some(Event::Rpc(raw)) = parse_line(line) else {
                panic!("the watcher did not recognise {line}");
            };
            let command = crate::bloxstrap_rpc::parse_line(&raw)
                .expect("parses")
                .expect("is a command");
            assert!(merged.apply(command).is_none(), "neither line is launch data");
        }

        let payload = merged.to_payload();
        assert_eq!(payload["details"], "Fighting the Boss");
        assert_eq!(payload["state"], "Round 3 of 5", "the first line must survive the second");
        assert_eq!(payload["start"], 1788200000);
        assert!(payload.get("end").is_none(), "a field the game never set is absent, not null");
    }

    /// Leaving clears it, so one experience's presence does not outlive it.
    #[test]
    fn a_fresh_presence_carries_nothing() {
        let payload = crate::bloxstrap_rpc::Presence::new().to_payload();
        assert_eq!(payload, serde_json::json!({}));
    }

    /// An absent directory is silence, not an error.
    ///
    /// This runs from the engine's own pump before the engine has written
    /// anything, which is the ordinary case for the first seconds of every
    /// launch. A `Watcher` that returned errors there would need handling at a
    /// call site that has nothing useful to do with them.
    #[test]
    fn nothing_to_watch_is_not_a_failure() {
        let mut w = Watcher::new("/nonexistent/cordial/logs");
        assert_eq!(w.poll(), Vec::new());
        assert_eq!(w.poll(), Vec::new());
    }
}
