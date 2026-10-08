//! Feral Interactive's GameMode, asked for over D-Bus.
//!
//! GameMode is a request rather than a wrapper. There is nothing to link and
//! nothing to `LD_PRELOAD`: `gamemoded` owns `com.feralinteractive.GameMode` on
//! the session bus and takes `RegisterGame(i pid)` / `UnregisterGame(i pid)`.
//! While a client is registered it puts the CPU governor in performance, raises
//! the process's I/O and scheduling priority, puts the GPU in its performance
//! profile and inhibits the screensaver. That last one is not a footnote for a
//! game the user plays with a controller and does not touch the keyboard for.
//!
//! **Absence is the ordinary case and must not fail a launch.** Most machines
//! do not have gamemoded, and this is an optimisation rather than a dependency
//! -- a client that refused to start because a performance daemon was missing
//! would be a far worse bug than the frame it was trying to save. Every failure
//! here is reported in one line and stepped over.
//!
//! On by default, which is what Sober does. `CORDIAL_GAMEMODE=0` turns it off,
//! and that is the control: it is the only way to show, in the same session,
//! that a timing difference came from this and not from something else.
//!
//! **This lived in `load.rs` and moved here so a live `settings.set` can
//! reach it (ADR-044).** The setting is a request to a daemon and registration
//! is per pid, so it can be made and withdrawn at any time; the only thing that
//! ever made it launch-only was that nothing outside the binary could call it.

use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

const SERVICE: &str = "com.feralinteractive.GameMode";
const OBJECT: &str = "/com/feralinteractive/GameMode";

/// How long a live change waits for gamemoded's answer before reporting that it
/// is still waiting. The launcher gives up on a request after two seconds and a
/// connection serves one request at a time, so a daemon that hangs must not hold
/// it.
const LIVE_WAIT: Duration = Duration::from_millis(1500);

/// Held for the life of the process rather than opened per call. Not because
/// `RegisterGame` needs it -- it registers a pid, and gamemoded watches that
/// pid rather than this connection -- but because [`unregister`] runs during
/// teardown, and opening a bus connection is the wrong thing to be doing at
/// the point where the engine's own destructors are already known to be unsafe
/// to run.
static CONNECTION: std::sync::OnceLock<Option<zbus::blocking::Connection>> =
    std::sync::OnceLock::new();

fn connection() -> Option<&'static zbus::blocking::Connection> {
    CONNECTION.get_or_init(|| zbus::blocking::Connection::session().ok()).as_ref()
}

/// `RegisterGame`/`UnregisterGame` both answer `0` for success and a negative
/// number for a refusal, so the reply has to be read rather than just checked
/// for not being a D-Bus error -- gamemoded returns `-1` for a pid it will not
/// accept and `-2` for one already registered, over a perfectly successful
/// method call.
fn call_daemon(method: &str) -> Result<i32, String> {
    let conn = connection().ok_or_else(|| "no session bus".to_string())?;
    let pid = std::process::id() as i32;
    let reply = conn
        .call_method(Some(SERVICE), OBJECT, Some(SERVICE), method, &(pid,))
        .map_err(|e| e.to_string())?;
    reply.body().deserialize::<i32>().map_err(|e| e.to_string())
}

/// What this process has asked of gamemoded, and what it was told.
struct State {
    /// The setting. `None` until first read, so the launch environment seeds it
    /// exactly once and a live change made before startup registration runs is
    /// not overwritten by it.
    enabled: Option<bool>,
    /// Whether gamemoded answered yes, so an `UnregisterGame` is never sent for
    /// a registration that did not happen.
    registered: bool,
    /// [`register`] has run. Before that a live change only records the wish:
    /// startup registration is still to come and will honour it.
    started: bool,
}

static STATE: Mutex<State> = Mutex::new(State { enabled: None, registered: false, started: false });

/// The state above is process-global, so every test that changes it -- here and
/// in `live_settings`, which reaches it through the socket handler -- takes this
/// one lock. Two locks, one per test module, let the two modules' tests overlap
/// and fail each other, which is what a whole-suite run did the first time.
#[cfg(test)]
pub(crate) static TEST_GUARD: Mutex<()> = Mutex::new(());

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn enabled_from_env() -> bool {
    enabled_for(std::env::var("CORDIAL_GAMEMODE").ok().as_deref())
}

/// The gate's decision, separate from where it reads it so it can be tested.
fn enabled_for(v: Option<&str>) -> bool {
    !matches!(v.map(str::trim), Some("0" | "off" | "false" | "no"))
}

/// Ask gamemoded for what `want` says, if that is not already so. One place, so
/// that startup, teardown and a live change cannot disagree about what "asked"
/// means. `call` is the daemon, swapped for a fake in tests.
///
/// Returns what to say about it, or `None` when nothing was needed.
fn reconcile(
    st: &mut State,
    want: bool,
    call: &dyn Fn(&str) -> Result<i32, String>,
    say: &dyn Fn(String),
) -> Option<String> {
    if want == st.registered {
        return None;
    }
    if want {
        match call("RegisterGame") {
            Ok(0) => {
                st.registered = true;
                say(format!(
                    "[gamemode] registered pid {}: performance governor, raised priority, \
                     GPU performance profile, screensaver inhibited",
                    std::process::id()
                ));
                None
            }
            // Said plainly rather than folded into the error path below. A
            // daemon that answered and declined is a different situation from
            // one that is not there, and only the second is the ordinary case.
            Ok(rc) => {
                let msg = format!("gamemoded declined to register this process (rc {rc})");
                say(format!("[gamemode] {msg}"));
                Some(msg)
            }
            Err(e) => {
                let msg = format!("not available, continuing without it: {e}");
                say(format!("[gamemode] {msg}"));
                Some(msg)
            }
        }
    } else {
        match call("UnregisterGame") {
            Ok(0) => {
                st.registered = false;
                say("[gamemode] unregistered".to_string());
                None
            }
            // Whatever it answered, this process is no longer asking. Keeping
            // `registered` true after a refusal would make every later "off"
            // retry a call the daemon has already told us it will not honour.
            Ok(rc) => {
                st.registered = false;
                let msg = format!("UnregisterGame returned {rc}");
                say(format!("[gamemode] {msg}"));
                Some(msg)
            }
            Err(e) => {
                st.registered = false;
                let msg = format!("UnregisterGame failed: {e}");
                say(format!("[gamemode] {msg}"));
                Some(msg)
            }
        }
    }
}

fn print(line: String) {
    println!("{line}");
}

/// Startup: register if the setting says so. Called once, before the engine
/// loads, so the governor is already up when the shader compiles and the asset
/// cache warms.
pub fn register() {
    let mut st = state();
    st.started = true;
    let want = *st.enabled.get_or_insert_with(enabled_from_env);
    if !want {
        println!("[gamemode] off (CORDIAL_GAMEMODE=0)");
        return;
    }
    reconcile(&mut st, true, &call_daemon, &print);
}

/// Teardown: withdraw the registration if there is one. gamemoded would notice
/// the process was gone on its own -- it reaps clients whose pid has vanished --
/// but that is a poll, so leaving it implicit means the governor stays raised
/// for however long the sweep takes after a session ends.
pub fn unregister() {
    let mut st = state();
    // `enabled` is left alone: this is the process ending, not a choice.
    reconcile(&mut st, false, &call_daemon, &print);
}

/// The setting as it stands, for the live `get` reply.
pub fn current() -> bool {
    *state().enabled.get_or_insert_with(enabled_from_env)
}

/// Whether this process is registered with gamemoded right now, which is not
/// the same as whether it is enabled: a machine with no daemon is enabled and
/// never registered.
pub fn registered() -> bool {
    state().registered
}

/// A live change. Records the wish and, once startup registration has run,
/// makes the matching request to gamemoded. The note is for the shell and is
/// `None` when the answer is simply yes.
///
/// The call is made on its own thread and waited for briefly, because
/// `call_method` blocks for as long as the daemon takes and the socket that
/// called this serves one peer at a time. If the daemon has not answered by
/// [`LIVE_WAIT`] the reply says the request is still in flight rather than
/// claiming a result; the outcome is printed to the client log when it lands.
pub fn set_enabled(on: bool) -> Option<String> {
    set_enabled_with(on, call_daemon, LIVE_WAIT)
}

fn set_enabled_with(
    on: bool,
    call: impl Fn(&str) -> Result<i32, String> + Send + 'static,
    wait: Duration,
) -> Option<String> {
    {
        let mut st = state();
        st.enabled = Some(on);
        if !st.started {
            // Startup registration has not happened, and will read the value
            // just stored. Nothing to ask the daemon yet.
            return None;
        }
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("cordial-gamemode".into()).spawn(move || {
        // The lock is held across the daemon call on purpose: two quick flips
        // must reach gamemoded in the order they were made, and this is what
        // serialises them.
        let mut st = state();
        // Decided under the lock, from the wish as it stands now, so a flip that
        // overtook this one is honoured rather than undone by it.
        let want = st.enabled.unwrap_or(on);
        let note = reconcile(&mut st, want, &call, &print);
        let _ = tx.send(note);
    });
    if spawned.is_err() {
        return Some("could not start a thread to ask gamemoded".to_string());
    }
    match rx.recv_timeout(wait) {
        Ok(note) => note,
        Err(_) => Some("gamemoded has not answered yet; the result is in the client's log".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A daemon that records what it was asked and answers from a script.
    struct Fake {
        log: RefCell<Vec<String>>,
        answer: Result<i32, String>,
    }
    impl Fake {
        fn new(answer: Result<i32, String>) -> Self {
            Fake { log: RefCell::new(Vec::new()), answer }
        }
        fn call(&self, method: &str) -> Result<i32, String> {
            self.log.borrow_mut().push(method.to_string());
            self.answer.clone()
        }
        fn calls(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    fn fresh() -> State {
        State { enabled: Some(true), registered: false, started: true }
    }

    fn quiet(_: String) {}

    #[test]
    fn turning_it_on_registers_once_and_turning_it_off_unregisters_once() {
        let d = Fake::new(Ok(0));
        let mut st = fresh();
        assert_eq!(reconcile(&mut st, true, &|m| d.call(m), &quiet), None);
        assert!(st.registered);
        // Already registered: asking again must not call the daemon, which
        // answers -2 to a pid it already has.
        assert_eq!(reconcile(&mut st, true, &|m| d.call(m), &quiet), None);
        assert_eq!(reconcile(&mut st, false, &|m| d.call(m), &quiet), None);
        assert!(!st.registered);
        assert_eq!(reconcile(&mut st, false, &|m| d.call(m), &quiet), None);
        assert_eq!(d.calls(), vec!["RegisterGame", "UnregisterGame"]);
    }

    #[test]
    fn a_daemon_that_declines_or_is_absent_leaves_us_unregistered_and_says_so() {
        let mut st = fresh();
        let declined = Fake::new(Ok(-1));
        let note = reconcile(&mut st, true, &|m| declined.call(m), &quiet).expect("a note");
        assert!(note.contains("declined") && note.contains("-1"), "{note}");
        assert!(!st.registered);

        let absent = Fake::new(Err("no session bus".into()));
        let note = reconcile(&mut st, true, &|m| absent.call(m), &quiet).expect("a note");
        assert!(note.contains("not available"), "{note}");
        assert!(!st.registered);
        // And so turning it off has nothing to withdraw: no call is made.
        assert_eq!(reconcile(&mut st, false, &|m| absent.call(m), &quiet), None);
        assert_eq!(absent.calls(), vec!["RegisterGame"]);
    }

    #[test]
    fn a_refused_unregister_still_ends_our_claim() {
        let mut st = fresh();
        st.registered = true;
        let d = Fake::new(Ok(-1));
        let note = reconcile(&mut st, false, &|m| d.call(m), &quiet).expect("a note");
        assert!(note.contains("UnregisterGame returned -1"));
        assert!(!st.registered, "retrying a call the daemon refused would never end");
    }

    #[test]
    fn the_environment_spellings_that_turn_it_off() {
        assert!(enabled_for(None));
        assert!(enabled_for(Some("1")));
        assert!(enabled_for(Some("")));
        for off in ["0", "off", "false", "no", " 0 "] {
            assert!(!enabled_for(Some(off)), "{off:?}");
        }
    }

    use super::TEST_GUARD as GLOBALS;

    #[test]
    fn a_live_change_before_startup_only_records_the_wish() {
        let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        *state() = State { enabled: None, registered: false, started: false };
        let asked = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = asked.clone();
        let note = set_enabled_with(
            false,
            move |m| {
                seen.lock().unwrap().push(m.to_string());
                Ok(0)
            },
            Duration::from_secs(2),
        );
        assert_eq!(note, None);
        assert!(asked.lock().unwrap().is_empty(), "nothing to ask before startup registration");
        assert!(!current(), "startup registration will read this, not the environment");
        *state() = State { enabled: None, registered: false, started: false };
    }

    #[test]
    fn a_live_change_after_startup_reaches_the_daemon_and_the_opposite_flips_it_back() {
        let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        *state() = State { enabled: Some(false), registered: false, started: true };
        let asked = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let call = |asked: &std::sync::Arc<Mutex<Vec<String>>>| {
            let seen = asked.clone();
            move |m: &str| {
                seen.lock().unwrap().push(m.to_string());
                Ok(0)
            }
        };

        assert_eq!(set_enabled_with(true, call(&asked), Duration::from_secs(2)), None);
        assert!(registered());
        assert!(current());
        // Control: the opposite message withdraws it in the same process.
        assert_eq!(set_enabled_with(false, call(&asked), Duration::from_secs(2)), None);
        assert!(!registered());
        assert!(!current());
        assert_eq!(*asked.lock().unwrap(), vec!["RegisterGame", "UnregisterGame"]);
        *state() = State { enabled: None, registered: false, started: false };
    }

    #[test]
    fn a_slow_daemon_is_reported_as_still_pending_not_as_a_result() {
        let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        *state() = State { enabled: Some(false), registered: false, started: true };
        let note = set_enabled_with(
            true,
            |_| {
                std::thread::sleep(Duration::from_millis(400));
                Ok(0)
            },
            Duration::from_millis(50),
        );
        assert!(note.expect("a note").contains("not answered yet"));
        // The request still lands afterwards; wait for it so the next test sees a
        // settled state.
        let end = std::time::Instant::now() + Duration::from_secs(3);
        while !registered() && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(registered());
        *state() = State { enabled: None, registered: false, started: false };
    }

    /// What gamemoded says about this pid: 0 not active, 1 active but this
    /// client is not registered, 2 registered (`QueryStatus` in its D-Bus API).
    fn status() -> Result<i32, String> {
        let conn = connection().ok_or_else(|| "no session bus".to_string())?;
        let pid = std::process::id() as i32;
        let reply = conn
            .call_method(Some(SERVICE), OBJECT, Some(SERVICE), "QueryStatus", &(pid,))
            .map_err(|e| e.to_string())?;
        reply.body().deserialize::<i32>().map_err(|e| e.to_string())
    }

    /// The live path against the session's real `gamemoded`, asked about
    /// afterwards through a different call so the test does not mark its own
    /// homework. Ignored because it needs the daemon, and because registering
    /// raises the machine's CPU governor for the second or two it lasts.
    #[test]
    #[ignore = "needs gamemoded on the session bus; briefly raises the CPU governor"]
    fn against_the_real_daemon_a_live_change_registers_and_withdraws_this_process() {
        let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        *state() = State { enabled: Some(false), registered: false, started: true };
        let before = status().expect("gamemoded must answer QueryStatus");
        println!("status before: {before}");
        assert_ne!(before, 2, "this process must not start out registered");

        assert_eq!(set_enabled(true), None, "gamemoded should have said yes");
        let during = status().unwrap();
        println!("status after enabling: {during}");
        assert_eq!(during, 2, "gamemoded does not list this process as registered");

        assert_eq!(set_enabled(false), None);
        let after = status().unwrap();
        println!("status after disabling: {after}");
        assert_ne!(after, 2, "gamemoded still lists this process as registered");
        *state() = State { enabled: None, registered: false, started: false };
    }
}
