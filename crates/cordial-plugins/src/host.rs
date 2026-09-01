//! Launching a plugin and talking to it.
//!
//! A plugin is a Deno process. That gives two independent layers of containment
//! rather than one: Deno's own permission model, and Cordial's capability
//! broker. The Deno process is started with **no permissions at all** — no file,
//! network, environment or subprocess access — so a plugin cannot reach the
//! machine even if the broker had a hole in it. Everything it is allowed to do
//! arrives over stdio and is checked by the broker first.
//!
//! This is ADR-003 made concrete: plugins are isolated by process, and the only
//! channel is a named, brokered one.

use crate::broker::Broker;
use crate::events::EventRegistry;
use crate::presence::{DiscordPresence, PresencePayload};
use crate::protocol::{required_capability, Push, Request, Response};
use crate::settings::{self, Store};
use crate::{notify, urlopen};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Mutex};

/// A plugin's stdin, shareable across threads.
///
/// A real host runs one thread per plugin, blocking on that plugin's own
/// stdout (see `cordial-runtime`'s `plugin_host.rs`). Delivering a published
/// event to a *subscriber* means writing to a different plugin's stdin from
/// whichever thread is serving the publisher's `events.publish` call — a
/// write that has nothing to do with that subscriber's own request/response
/// cycle and must not have to wait for one. Splitting the writable half out
/// from `Plugin` and wrapping it in a mutex is what makes that possible
/// without also having to share the read half, which only ever needs to be
/// read from the one thread that owns the `Plugin` itself.
///
/// Cheap to clone: an `Arc` around one mutex, never a duplicated file
/// descriptor.
#[derive(Clone)]
pub struct Writer(Arc<Mutex<ChildStdin>>);

impl Writer {
    fn write_line(&self, line: &str) -> std::io::Result<()> {
        // A poisoned mutex means some other write already panicked mid-line;
        // recovering the guard rather than propagating the poison lets this
        // write still land cleanly rather than every subsequent push to this
        // plugin failing forever over an unrelated panic.
        let mut stdin = self.0.lock().unwrap_or_else(|e| e.into_inner());
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// Deliver a [`Push`] through this handle, from whichever thread holds
    /// it — the counterpart to [`Plugin::push`] for a caller that only has
    /// the writable half. `&self` rather than `&mut self`: the mutex inside
    /// is what serialises concurrent writers, not Rust's own borrow checker,
    /// because a `Writer` is meant to be called from a thread that does not
    /// own the `Plugin` at all.
    pub fn push(&self, push: &Push) -> std::io::Result<()> {
        self.write_line(&serde_json::to_string(push).expect("Push always serialises"))
    }
}

/// What one publish achieved: [`Session::publish_core`], or the client's own
/// `plugin_host::publish_core`, which is the one a running Cordial calls.
///
/// Two numbers rather than a `Result` because a publish cannot fail. Every
/// plugin entitled to the event was either handed it or was too far behind to
/// take it, and both of those are outcomes the caller may want to report and
/// neither is an error it could act on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Delivered {
    pub sent: usize,
    /// Plugins that did not take it: the queue was full, or the plugin has
    /// gone. See [`Pump`] for why this is a number rather than a stall, and
    /// [`Pump::plugin_gone`] for telling the two apart.
    pub dropped: usize,
}

/// A plugin's own delivery queue, so publishing never blocks the publisher.
///
/// **This exists because [`Plugin::push`] is a blocking write into a pipe.** A
/// plugin that stops reading its stdin fills it -- 64 KiB on Linux -- and then
/// whoever called `push` waits, indefinitely, on a process that may never read
/// again. For a reply that is merely bad. For a platform event it is a thread
/// the client is waiting on, and the engine's looper is measured in millions of
/// polls a second; it cannot queue behind a wedged plugin.
///
/// So the write happens on a thread of the plugin's own and the publisher hands
/// over a bounded queue instead. A full queue means the plugin is not keeping
/// up, and the event is **dropped and counted** rather than waited on. Dropping
/// is the honest outcome for an observation -- there is no correct way to make
/// the client wait -- and the count is what keeps it from being silent.
///
/// This documentation used to sit on [`Delivered`], one item below, where the
/// reasoning for the whole mechanism was filed under its counter and `Pump`
/// itself had none. That was survivable while `Pump` was reachable only from
/// this crate's own tests; it stopped being so when `plugin_host::publish_core`
/// made it the primitive the shipping client's core bus is built on.
pub struct Pump {
    tx: std::sync::mpsc::SyncSender<Push>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
    /// Set when a send found the channel disconnected, which means the pump's
    /// own thread has ended -- the plugin's pipe refused a write, or the
    /// process is gone. Kept apart from a full queue because the two look
    /// identical to [`std::sync::mpsc::SyncSender::try_send`]'s caller and
    /// send a reader of the shutdown report to opposite places: one says the
    /// plugin was too slow, the other that it was not there.
    gone: Arc<std::sync::atomic::AtomicBool>,
    /// Queued but not yet written. What [`Pump::flush`] waits on.
    in_flight: Arc<std::sync::atomic::AtomicU64>,
}

/// Deep enough to absorb a burst, shallow enough that a stuck plugin is
/// noticed rather than accumulating minutes of stale events to deliver in one
/// go when it wakes up. An event nobody read for a minute is not worth
/// delivering late.
const QUEUE_DEPTH: usize = 256;

impl Pump {
    pub fn start(writer: Writer) -> Pump {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Push>(QUEUE_DEPTH);
        let in_flight = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = in_flight.clone();
        std::thread::spawn(move || {
            // Ends when the Sender is dropped, which happens when the Plugin
            // does. A write error ends it too: the plugin is gone, and
            // retrying into a closed pipe would spin.
            while let Ok(push) = rx.recv() {
                let failed = writer.push(&push).is_err();
                counter.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                if failed {
                    // Whatever is still queued will never be written now, and
                    // leaving it counted would make `flush` wait its entire
                    // budget on a plugin that has already gone -- which at
                    // shutdown is Cordial's exit paying for a dead process.
                    // Draining is what decrements, so it is done here rather
                    // than by letting the receiver drop with items in it.
                    for _ in rx.try_iter() {
                        counter.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                    }
                    break;
                }
            }
        });
        Pump {
            tx,
            dropped: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            gone: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            in_flight,
        }
    }

    /// Queue `push`, or count a drop. Never blocks.
    pub fn offer(&self, push: Push) -> bool {
        // Counted before the send, so a flush racing an offer waits for it
        // rather than missing it. An over-count is corrected below.
        self.in_flight.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let outcome = self.tx.try_send(push);
        if let Err(std::sync::mpsc::TrySendError::Disconnected(_)) = outcome {
            // Remembered rather than merely counted, because the shutdown
            // report is the one place anybody reads these numbers and
            // "its queue was full" sends them to the queue depth and the
            // plugin's read loop when the plugin was not slow at all.
            self.gone.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        match outcome {
            Ok(()) => true,
            Err(_) => {
                self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                self.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            }
        }
    }

    /// Everything this plugin did not receive, whatever the reason.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether any of that was because the plugin had stopped reading rather
    /// than fallen behind. See `gone`.
    pub fn plugin_gone(&self) -> bool {
        self.gone.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Wait for what is queued to reach the plugin, up to `limit`.
    ///
    /// **Without this a plugin never learns the client shut down.** Delivery is
    /// asynchronous by design, so a publish followed by an exit is a race the
    /// exit wins: the pump thread is still holding the last event when the
    /// process goes. That is fine for an observation nobody is waiting on and
    /// wrong for the final one, which is the whole point of a shutdown event.
    ///
    /// Bounded, because a plugin that has stopped reading must not be able to
    /// hold up Cordial's exit -- which would be the blocking-publish hazard
    /// arriving at the one moment it is least welcome. Returns whether the
    /// queue actually drained.
    pub fn flush(&self, limit: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + limit;
        while self.in_flight.load(std::sync::atomic::Ordering::Acquire) > 0 {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        true
    }
}

pub struct Plugin {
    pub id: String,
    /// What this plugin's manifest declares it wants asked (ADR-020), so the
    /// handshake can carry the user's answers.
    ///
    /// Set by [`Plugin::declaring`] rather than by [`Plugin::spawn`], which is
    /// handed an id and an entry module and has no manifest to read it from.
    /// Empty is the honest default: a plugin that declares nothing has no
    /// preferences page and no answers to be given.
    pub preferences: Vec<crate::preferences::Declaration>,
    child: Child,
    writer: Writer,
    stdout: BufReader<ChildStdout>,
    /// Created on first use, because a plugin nobody publishes to should not
    /// cost a thread.
    pump: Option<Pump>,
}

impl Plugin {
    /// Start a plugin from an entry module.
    ///
    /// `--no-prompt` matters as much as the absence of allow flags: without it
    /// Deno would *ask* for a permission on first use, and a plugin host has
    /// nobody to ask. With it, an attempt to touch the filesystem fails
    /// immediately instead of hanging on a prompt nothing will answer.
    pub fn spawn(id: &str, entry: &Path) -> std::io::Result<Self> {
        Self::spawn_with(id, entry, false)
    }

    /// As [`Plugin::spawn`], with Deno's `--watch` when `reload` is set.
    ///
    /// Only unpacked plugins get it -- see `manifest::unpacked_dirs`. An
    /// installed one does not change under a running client, so watching it is
    /// a thread doing nothing, and a `.tar.zst` being replaced mid-session is
    /// an install rather than an edit.
    pub fn spawn_with(id: &str, entry: &Path, reload: bool) -> std::io::Result<Self> {
        // A third layer under the two above, when the host can enforce one. It
        // does not replace either: a sub-sandbox only ever subtracts from what
        // Cordial holds, so every effect is still performed by the broker. See
        // `crate::sandbox`, which says so at length because "we sandbox now" is
        // the argument someone will use to justify handing a plugin an fd.
        //
        // Absence is a downgrade rather than a hole -- the Deno process still
        // has no permissions at all -- so a missing `bwrap` does not stop a
        // plugin running. It is said out loud instead, because a layer nobody
        // can tell is missing is one nobody notices went away.
        let sandbox = crate::sandbox::available();
        println!("[plugin] {id}: {}", sandbox.describe());
        if reload {
            // Said, because a plugin restarting on its own is behaviour
            // somebody will otherwise attribute to a crash.
            println!("[plugin] {id}: reloading on change (Deno --watch)");
        }
        let mut child = crate::sandbox::command(sandbox, entry, reload)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
        Ok(Plugin {
            id: id.to_string(),
            preferences: Vec::new(),
            child,
            writer: Writer(Arc::new(Mutex::new(stdin))),
            stdout,
            pump: None,
        })
    }

    /// Record what this plugin's manifest declares it wants asked, so the
    /// handshake carries the user's answers (ADR-020).
    ///
    /// Separate from [`Plugin::spawn`] because spawning needs an entry module
    /// and nothing else, and a caller that has no manifest -- a test, or a
    /// bare process someone is driving by hand -- should not have to invent
    /// one. Not calling it means "declares nothing", which is true of every
    /// plugin written before this existed.
    pub fn declaring(mut self, fields: Vec<crate::preferences::Declaration>) -> Self {
        self.preferences = fields;
        self
    }

    /// Read one request from the plugin. `None` at end of stream.
    pub fn next_request(&mut self) -> Option<Result<Request, String>> {
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(line.trim()).map_err(|e| e.to_string())),
            Err(e) => Some(Err(e.to_string())),
        }
    }

    pub fn reply(&mut self, response: &Response) -> std::io::Result<()> {
        self.writer.write_line(&serde_json::to_string(response).expect("Response always serialises"))
    }

    /// Deliver a message the plugin did not ask for in this call — a
    /// lifecycle event, or another plugin's published event arriving for a
    /// subscriber. See [`Push`] for how a plugin tells this apart from a
    /// reply to one of its own requests.
    pub fn push(&mut self, push: &Push) -> std::io::Result<()> {
        self.writer.write_line(&serde_json::to_string(push).expect("Push always serialises"))
    }

    /// Queue a push without blocking. See [`Pump`].
    pub fn offer(&mut self, push: Push) -> bool {
        let writer = self.writer.clone();
        self.pump.get_or_insert_with(|| Pump::start(writer)).offer(push)
    }

    /// How many events this plugin was too slow to receive.
    pub fn dropped(&self) -> u64 {
        self.pump.as_ref().map(Pump::dropped).unwrap_or(0)
    }

    /// A cloneable handle to this plugin's stdin, for a host that wants to
    /// push to it from a thread other than the one reading its stdout — see
    /// [`Writer`].
    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }

    /// **A single `SIGKILL` to this pid is not enough**, and the first version
    /// of this method sent exactly that. `sandbox.rs` passes bwrap
    /// `--new-session`, which calls `setsid()` before bwrap forks again to set
    /// up the sandboxed pid namespace -- so this pid is the leader of its own
    /// session and process group, and bwrap's own inner fork lives in that
    /// group too, ahead of the point where it execs Deno. `Child::kill` signals
    /// only the one pid; `SIGKILL` cannot be caught, so it never gives bwrap
    /// the chance to tear its own children down on the way out, and the inner
    /// fork survives it, reparented to init with nothing left watching it.
    ///
    /// Measured directly: an otherwise clean, panic-free run of this crate's
    /// own tests, with every `kill()` call reached, still left one such
    /// process behind -- 21 seconds old and already reparented -- the same
    /// shape as the sandboxed stragglers this whole guard exists to stop.
    /// Signalling the *group* (the negative pid) reaches every process
    /// `--new-session` put in it, however many times bwrap forked.
    ///
    /// SAFETY: `kill(2)` with a negative pid is process-group signalling, not
    /// a memory operation. The only failures are ESRCH (the group is already
    /// gone) and EPERM, and both are fine to ignore: either way there is
    /// nothing left running that this call could still reach.
    pub fn kill(&mut self) {
        let pid = self.child.id() as libc::pid_t;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// **The reason this exists rather than trusting every caller to remember
/// `kill()`.** `std::process::Child` is not killed on drop -- that is
/// documented, deliberate, and exactly the wrong default for a sandboxed
/// child nobody else is going to reap.
///
/// It went unnoticed because most fixtures make it look harmless: a plugin
/// that keeps reading its own stdin notices the pipe close the moment a
/// `Plugin` is dropped and exits on its own, EOF standing in for a kill
/// nobody sent. `tests/fixtures/deaf_plugin.ts` does not read at all, so
/// dropping it without calling `kill()` first leaves a live `bwrap`-sandboxed
/// Deno process behind with nothing watching it -- measured on 2026-08-28,
/// ten or more of exactly this shape were found still running, reparented to
/// init, some past an hour old. Every caller in this file's own tests and in
/// `cordial-runtime`'s already called `kill()` on the path where its
/// assertions pass; what none of them survived was a panic *before* that
/// call, which unwinds straight past it. A guard on the type is the only
/// place that fires regardless of how the scope is left, including that one.
impl Drop for Plugin {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Decide what a request gets, without performing any effect.
///
/// Kept separate from dispatch so the decision is testable on its own, and so
/// the check cannot be accidentally skipped by a future handler that forgets to
/// call it — a handler receives an already-authorised request or nothing.
pub fn authorise(broker: &mut Broker, plugin: &str, req: &Request) -> Result<(), Response> {
    match required_capability(&req.method) {
        None => Err(Response::Error {
            id: req.id,
            message: format!("unknown method {:?}", req.method),
        }),
        Some(cap) if !broker.allows(plugin, cap) => {
            Err(Response::Denied { id: req.id, capability: cap.name().to_string() })
        }
        Some(_) => Ok(()),
    }
}

/// Everything one running Cordial process needs to serve the capabilities
/// this crate actually performs an effect for: the grants, the event
/// registry, and the one live Discord connection. A `Session` is where
/// ADR-007 stops being a description and starts being true — it is the one
/// place that has both a plugin's authorised request and the host resource
/// the request wants to act on, and nothing upstream of it ever holds the
/// two together.
///
/// `Session` only ever answers the methods it has a real broker for —
/// `presence.*`, `notify.send`, `url.open`, `settings.*`, `events.*`, and the
/// `lifecycle.subscribe` acknowledgement paired with `push_lifecycle` below.
/// Everything else (`flags.*`, `log.write`, `assets.override`) is still only
/// `authorise`d here; a caller that wants those served has to do it itself,
/// the way `tests/flag_inspector.rs` already does. Falling through to an
/// explicit "no broker wired" error rather than silently returning `Ok`
/// keeps this file honest about what it does and does not implement — see
/// AGENTS.md on a stub never claiming success it did not have.
pub struct Session {
    pub broker: Broker,
    pub events: EventRegistry,
    presence: DiscordPresence,
    plugins: BTreeMap<String, Plugin>,
    /// Where every plugin's settings live, which is a property of the profile
    /// this instance is running rather than of the process. `None` is honest
    /// about a session with no profile behind it — `settings.*` then fails and
    /// says why, instead of reporting a save that went nowhere.
    settings: Option<Store>,
}

impl Session {
    pub fn new() -> Self {
        Session {
            broker: Broker::new(),
            events: EventRegistry::new(),
            presence: DiscordPresence::new(),
            plugins: BTreeMap::new(),
            settings: None,
        }
    }

    /// A session running `profile_dir`, so plugins have somewhere to keep
    /// their settings. Everything else about a session is per process; this is
    /// the one thing that belongs to the profile.
    pub fn with_profile(profile_dir: impl Into<PathBuf>) -> Self {
        Session { settings: Some(Store::new(profile_dir)), ..Session::new() }
    }

    /// Adopt a spawned plugin so it can receive pushes — lifecycle events and
    /// other plugins' published events — in addition to answering its own
    /// requests through [`Session::handle`].
    ///
    /// The plugin's first line is the handshake, carrying whatever it had
    /// saved. That is what keeps the common case — read your configuration,
    /// then start — free of a round trip a plugin would otherwise have to make
    /// before it could do anything. Grants must therefore be in place before
    /// this is called; adopting first and granting afterwards would hand a
    /// plugin a handshake saying it holds nothing.
    pub fn add_plugin(&mut self, mut plugin: Plugin) {
        let granted = self.broker.granted(&plugin.id);
        let push =
            settings::init_push(self.settings.as_ref(), &plugin.preferences, &plugin.id, &granted);
        // Best effort, like every other push: a plugin that has already died
        // is not a reason to fail adopting it, and the write error would only
        // repeat what the next read of its stdout is about to say.
        let _ = plugin.push(&push);
        self.plugins.insert(plugin.id.clone(), plugin);
    }

    pub fn remove_plugin(&mut self, id: &str) -> Option<Plugin> {
        self.plugins.remove(id)
    }

    /// Access an adopted plugin directly — for reading its next request and
    /// replying, the way a caller drives any other `Plugin`. Needed because
    /// once a `Plugin` is adopted its stdio is also the channel `handle`
    /// pushes events down, so a caller cannot keep its own separate handle to
    /// the same process.
    pub fn plugin_mut(&mut self, id: &str) -> Option<&mut Plugin> {
        self.plugins.get_mut(id)
    }

    /// Deliver a client lifecycle event to every plugin holding
    /// `lifecycle.read`. There is no request to deny for a plugin that lacks
    /// the capability — only a push nobody asked to receive — so it is
    /// simply not sent rather than answered with a denial nobody is waiting
    /// for.
    pub fn push_lifecycle(&mut self, event: &str) {
        // Kept as the shorthand the lifecycle call sites already use. It maps
        // onto the core bus rather than being a second delivery path, because
        // two of those would drift and only one of them would have the
        // capability table.
        let name = match event {
            "launch" => crate::core_events::CLIENT_LAUNCH,
            "ready" => crate::core_events::CLIENT_READY,
            "shutdown" => crate::core_events::CLIENT_SHUTDOWN,
            // An unknown lifecycle string reaches nobody rather than being
            // forwarded raw. The old version pushed whatever it was given,
            // which meant a typo at a call site became an event no plugin
            // could match and nothing said so.
            other => {
                println!("[plugin] ignoring unknown lifecycle event {other:?}");
                return;
            }
        };
        self.publish_core(&crate::core_events::CoreEvent::new(name, serde_json::Value::Null));
    }

    /// Deliver one core event to every plugin entitled to hear it.
    ///
    /// `publish_core` rather than `publish`, which belongs to `events.publish`
    /// and is a plugin publishing to other plugins. Two different buses with
    /// two different authorisation rules; sharing a name would invite somebody
    /// to assume they share anything else.
    ///
    /// **Never blocks, and never fails.** Publishing is something the client
    /// does on its way past; there is no answer to wait for and nothing
    /// sensible to do about a plugin that is not listening. What comes back is
    /// how many heard it and how many were too slow, so a caller that wants to
    /// notice can.
    ///
    /// A plugin hears an event when it holds the capability the event's family
    /// requires -- see [`crate::core_events::capability_for`]. An event absent
    /// from that table requires a capability nobody has, so it reaches no one;
    /// that is deliberate and is the safe direction for a name somebody added
    /// and forgot to gate.
    pub fn publish_core(&mut self, event: &crate::core_events::CoreEvent) -> Delivered {
        let Some(needed) = crate::core_events::capability_for(event.name) else {
            println!(
                "[plugin] core event {:?} has no capability in the table, so nobody receives it",
                event.name
            );
            return Delivered::default();
        };

        let recipients: Vec<String> = self
            .plugins
            .keys()
            .filter(|id| self.broker.granted(id).contains(&needed))
            .cloned()
            .collect();

        let push = Push { event: event.wire_name(), payload: event.payload.clone() };
        let mut delivered = Delivered::default();
        for id in recipients {
            if let Some(plugin) = self.plugins.get_mut(&id) {
                if plugin.offer(push.clone()) {
                    delivered.sent += 1;
                } else {
                    delivered.dropped += 1;
                }
            }
        }
        delivered
    }

    /// Wait for queued events to reach every plugin, up to `limit` each.
    ///
    /// **Call this before Cordial exits.** See [`Pump::flush`]: without it the
    /// shutdown event loses a race against the process ending, and the last
    /// thing a plugin is told is the one thing it never hears.
    pub fn flush_events(&mut self, limit: std::time::Duration) -> bool {
        let mut all = true;
        for plugin in self.plugins.values_mut() {
            if let Some(pump) = plugin.pump.as_ref() {
                all &= pump.flush(limit);
            }
        }
        all
    }

    /// Total events each plugin was too slow to receive, for a report.
    pub fn dropped_by_plugin(&self) -> Vec<(String, u64)> {
        self.plugins
            .iter()
            .map(|(id, p)| (id.clone(), p.dropped()))
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    /// Authorise, then perform, one call from `plugin_id`.
    pub fn handle(&mut self, plugin_id: &str, req: &Request) -> Response {
        if let Err(refusal) = authorise(&mut self.broker, plugin_id, req) {
            return refusal;
        }
        match req.method.as_str() {
            "presence.set" => match PresencePayload::parse(&req.params) {
                Ok(payload) => respond(req.id, self.presence.set(&payload)),
                Err(message) => Response::Error { id: req.id, message },
            },
            "presence.clear" => respond(req.id, self.presence.clear()),
            // Delivery for lifecycle events is capability-gated, not a
            // subscription list — see push_lifecycle — so this call has
            // nothing to record. It exists so a plugin gets a definite
            // acknowledgement that it holds lifecycle.read, the same way
            // events.subscribe acknowledges a subscription, rather than the
            // plugin having to infer that from the first push ever arriving.
            "lifecycle.subscribe" => Response::Ok { id: req.id, result: serde_json::Value::Null },
            "notify.send" => {
                let summary = req.params.get("summary").and_then(|v| v.as_str());
                let body = req.params.get("body").and_then(|v| v.as_str()).unwrap_or("");
                match summary {
                    Some(summary) => respond(req.id, notify::send(summary, body)),
                    None => Response::Error { id: req.id, message: "notify.send needs a summary".into() },
                }
            }
            "url.open" => match req.params.get("url").and_then(|v| v.as_str()) {
                Some(url) => respond(req.id, urlopen::open(url)),
                None => Response::Error { id: req.id, message: "url.open needs a url".into() },
            },
            "events.declare" => match req.params.get("name").and_then(|v| v.as_str()) {
                Some(name) => match self.events.declare(plugin_id, name) {
                    Ok(event_type) => Response::Ok { id: req.id, result: serde_json::json!({"type": event_type}) },
                    Err(message) => Response::Error { id: req.id, message },
                },
                None => Response::Error { id: req.id, message: "events.declare needs a name".into() },
            },
            // `plugin_id` is this session's own record of which process is on
            // the other end of the pipe, and it is the only id `serve` is
            // given — a plugin naming another one in its params reads and
            // writes its own document. See settings.rs.
            "settings.get" | "settings.set" => {
                settings::serve(self.settings.as_ref(), plugin_id, req)
            }
            // ADR-020. The declarations come from the adopted plugin's own
            // manifest, so a plugin that was never adopted -- or was adopted
            // without one -- reads an empty page rather than somebody else's.
            "preferences.get" => {
                let declared = self
                    .plugins
                    .get(plugin_id)
                    .map(|p| p.preferences.clone())
                    .unwrap_or_default();
                let store = self
                    .settings
                    .as_ref()
                    .map(|s| crate::preferences::Store::new(s.profile_dir()));
                crate::preferences::serve(store.as_ref(), &declared, plugin_id, req)
            }
            "events.publish" => self.publish(plugin_id, req),
            "events.subscribe" => match req.params.get("type").and_then(|v| v.as_str()) {
                Some(event_type) => match self.events.subscribe(plugin_id, event_type) {
                    Ok(()) => Response::Ok { id: req.id, result: serde_json::Value::Null },
                    Err(message) => Response::Error { id: req.id, message },
                },
                None => Response::Error { id: req.id, message: "events.subscribe needs a type".into() },
            },
            other => Response::Error { id: req.id, message: format!("no broker wired for {other:?}") },
        }
    }

    fn publish(&mut self, plugin_id: &str, req: &Request) -> Response {
        let Some(event_type) = req.params.get("type").and_then(|v| v.as_str()) else {
            return Response::Error { id: req.id, message: "events.publish needs a type".into() };
        };
        if !self.events.may_publish(plugin_id, event_type) {
            return Response::Error {
                id: req.id,
                message: format!(
                    "{plugin_id:?} may not publish on {event_type:?}; it must declare that type before publishing on it"
                ),
            };
        }
        let payload = req.params.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        for subscriber in self.events.subscribers(event_type) {
            let subscriber = subscriber.to_string();
            if let Some(plugin) = self.plugins.get_mut(&subscriber) {
                let _ = plugin.push(&Push { event: event_type.to_string(), payload: payload.clone() });
            }
        }
        Response::Ok { id: req.id, result: serde_json::Value::Null }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

fn respond(id: u64, result: Result<(), String>) -> Response {
    match result {
        Ok(()) => Response::Ok { id, result: serde_json::Value::Null },
        Err(message) => Response::Error { id, message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;

    fn req(method: &str) -> Request {
        Request { id: 1, method: method.into(), params: serde_json::Value::Null }
    }

    /// **Publishing takes bounded time however slow the consumer is, and the
    /// overflow is counted.**
    ///
    /// The hazard is real: a push is a blocking write into a pipe, and a
    /// plugin that stops reading fills it -- 64 KiB on Linux -- after which
    /// whoever published waits on a process that may never read again. For a
    /// platform event that is a thread the client is waiting on.
    ///
    /// **What this test does *not* do is wedge a plugin**, and the first
    /// version of this comment claimed it did. Measured: swapping `offer` back
    /// for the blocking `push` does not hang the loop, it fails it in 0.38 s,
    /// because the fixture process exits and the pipe then returns `EPIPE`
    /// immediately rather than blocking. A genuinely wedged consumer -- alive,
    /// holding the pipe open, never reading -- is a harder fixture than this
    /// and does not exist here yet.
    ///
    /// **That last sentence stopped being true on 2026-08-28**:
    /// `tests/fixtures/deaf_plugin.ts` is exactly such a consumer -- alive on a
    /// timer, holding its stdin open, never reading -- and
    /// `cordial-runtime`'s `plugin_host` tests use it for this property and for
    /// the shutdown flush. This test is left on the reading fixture on purpose:
    /// the numbers below are quoted in ADR-026, and changing what they were
    /// measured against would make the ADR describe a run nobody did. A wedged
    /// version of this test belongs beside the ones that already exist on the
    /// host the client runs, which is where it now is.
    ///
    /// So what is actually demonstrated is the property that matters anyway:
    /// 4000 events at 4 KiB each, published in about 6 ms, with 3735 of them
    /// dropped and counted because the writer thread could not drain that
    /// fast. The publisher's cost does not depend on the reader's speed. That
    /// is the guarantee the engine's looper needs, and the drop count is what
    /// keeps the loss from being silent.
    #[test]
    fn publishing_is_bounded_time_and_counts_what_it_could_not_deliver() {
        if std::process::Command::new("deno").arg("--version").output().is_err() {
            eprintln!("skipping: deno is not installed");
            return;
        }
        let entry = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/events_subscriber.ts");
        let mut session = Session::default();
        session.broker.grant("wedged", [Capability::LifecycleRead]);
        session
            .plugins
            .insert("wedged".into(), Plugin::spawn("wedged", &entry).expect("deno should start"));

        // Comfortably more than QUEUE_DEPTH and more than a pipe will take,
        // with a payload big enough that it cannot all be buffered.
        let big = serde_json::json!({ "pad": "x".repeat(4096) });
        let started = std::time::Instant::now();
        let mut dropped = 0usize;
        for _ in 0..4000 {
            let d = session.publish_core(&crate::core_events::CoreEvent::new(
                crate::core_events::CLIENT_READY,
                big.clone(),
            ));
            dropped += d.dropped;
        }
        let took = started.elapsed();

        assert!(
            took < std::time::Duration::from_secs(10),
            "publishing 4000 events took {took:?}; the publisher's cost is \
             tracking the reader's speed, which is the thing the pump exists to \
             prevent"
        );
        // And the loss is counted rather than silent -- that is the whole
        // trade this design makes.
        assert!(dropped > 0, "a consumer this far behind should have missed something");
        assert_eq!(
            session.dropped_by_plugin().first().map(|(id, _)| id.as_str()),
            Some("wedged")
        );
        eprintln!("published 4000 in {took:?}, {dropped} dropped");
    }

    /// **The capability is what admits you, not being a plugin.**
    #[test]
    fn a_core_event_reaches_only_plugins_holding_its_capability() {
        if std::process::Command::new("deno").arg("--version").output().is_err() {
            eprintln!("skipping: deno is not installed");
            return;
        }
        let entry = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/events_subscriber.ts");
        let mut session = Session::default();
        session.broker.grant("listener", [Capability::LifecycleRead]);
        // Granted something else entirely. A plugin with capabilities is not
        // a plugin with *this* capability, and the bus must not confuse the
        // two -- which is exactly what a single "may receive events" grant
        // would do once the table grows past lifecycle.
        session.broker.grant("bystander", [Capability::Log]);
        session
            .plugins
            .insert("listener".into(), Plugin::spawn("listener", &entry).expect("deno"));
        session
            .plugins
            .insert("bystander".into(), Plugin::spawn("bystander", &entry).expect("deno"));

        let d = session.publish_core(&crate::core_events::CoreEvent::new(
            crate::core_events::CLIENT_LAUNCH,
            serde_json::Value::Null,
        ));
        assert_eq!(d.sent, 1, "exactly the one holding lifecycle.read");
        assert_eq!(d.dropped, 0);
    }

    /// A cloned [`Writer`] really does deliver to the same process, from a
    /// thread that never reads that process's stdout at all.
    ///
    /// This is the property `cordial-runtime`'s real host depends on: one
    /// thread blocks reading a plugin's own requests, and a *different*
    /// plugin's publish has to be able to push into this one's stdin without
    /// waiting for that read loop to be between requests. Proven against a
    /// real Deno process rather than a mock, because the property in question
    /// is about `ChildStdin` actually being safe to write from two threads
    /// through one mutex — a unit test with a fake writer would not exercise
    /// that at all.
    #[test]
    fn a_cloned_writer_pushes_into_the_same_process_from_a_different_thread() {
        if std::process::Command::new("deno").arg("--version").output().is_err() {
            eprintln!("skipping: deno is not installed");
            return;
        }
        let entry = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/events_subscriber.ts");
        let mut plugin = Plugin::spawn("writer-clone-test", &entry).expect("deno should start");

        // The clone is what a publishing plugin's own serving thread would
        // hold — never the `Plugin` itself, which stays owned by the thread
        // reading this process's stdout below.
        let writer = plugin.writer();
        std::thread::spawn(move || {
            let _ = writer.write_line(
                &serde_json::to_string(&Push {
                    event: "cross-thread/proof".into(),
                    payload: serde_json::json!({"from": "another thread"}),
                })
                .unwrap(),
            );
        });

        let mut logs = Vec::new();
        while let Some(Ok(req)) = plugin.next_request() {
            if req.method == "log.write" {
                logs.push(req.params["message"].as_str().unwrap_or_default().to_string());
                plugin.reply(&Response::Ok { id: req.id, result: serde_json::Value::Null }).unwrap();
                break;
            }
            // The fixture's own `events.subscribe` call; not answering it is
            // fine, since the pushed message arrives on an independent code
            // path in the fixture's event loop and does not wait for a reply.
        }
        plugin.kill();

        let joined = logs.join("\n");
        assert!(joined.contains("push: cross-thread/proof"), "got:\n{joined}");
        assert!(joined.contains(r#""from":"another thread""#), "got:\n{joined}");
    }

    #[test]
    fn an_authorised_call_passes() {
        let mut b = Broker::new();
        b.grant("p", [Capability::FlagsRead]);
        assert!(authorise(&mut b, "p", &req("flags.list")).is_ok());
    }

    #[test]
    fn an_unauthorised_call_is_denied_by_name() {
        let mut b = Broker::new();
        b.grant("p", [Capability::FlagsRead]);
        match authorise(&mut b, "p", &req("flags.set")) {
            Err(Response::Denied { capability, .. }) => assert_eq!(capability, "flags.write"),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_method_is_an_error_not_a_denial() {
        // A typo must not look like a missing permission, or the author goes
        // hunting for a capability that was never the problem.
        let mut b = Broker::new();
        b.grant("p", Capability::all().iter().copied());
        match authorise(&mut b, "p", &req("flags.nonsense")) {
            Err(Response::Error { message, .. }) => assert!(message.contains("unknown method")),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    fn call(method: &str, params: serde_json::Value) -> Request {
        Request { id: 1, method: method.into(), params }
    }

    #[test]
    fn session_denies_an_ungranted_brokered_capability_rather_than_erroring() {
        // The distinction protocol.rs draws between `denied` and `error` has
        // to survive contact with a real effect-performing broker, not just
        // the plain `authorise` check — a plugin without notify.send must see
        // a denial, not a message about the D-Bus call that never happened.
        let mut session = Session::new();
        let res = session.handle("p", &call("notify.send", serde_json::json!({"summary": "hi"})));
        match res {
            Response::Denied { capability, .. } => assert_eq!(capability, "notify.send"),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn session_refuses_a_non_http_url_scheme_once_granted() {
        // ADR-007's doc comment on UrlOpen is explicit that this must not
        // become file:// traversal — checked here past the capability gate,
        // where a granted-but-malicious call would otherwise reach the portal.
        let mut session = Session::new();
        session.broker.grant("p", [Capability::UrlOpen]);
        let res = session.handle("p", &call("url.open", serde_json::json!({"url": "file:///etc/passwd"})));
        match res {
            Response::Error { message, .. } => assert!(message.contains("refused"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn session_refuses_a_publish_on_a_type_the_plugin_never_declared() {
        // events.publish alone is not enough — ADR-006 splits declare from
        // publish precisely so holding this capability cannot be used to
        // impersonate a type nobody gave this plugin.
        let mut session = Session::new();
        session.broker.grant("evil", [Capability::EventsPublish]);
        let res = session.handle(
            "evil",
            &call("events.publish", serde_json::json!({"type": "flag-manager/profile-changed", "payload": {}})),
        );
        match res {
            Response::Error { message, .. } => assert!(message.contains("may not publish"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn session_lets_a_plugin_declare_then_publish_on_its_own_type() {
        let mut session = Session::new();
        session.broker.grant("flag-manager", [Capability::EventsDeclare, Capability::EventsPublish]);
        let declared = session.handle("flag-manager", &call("events.declare", serde_json::json!({"name": "profile-changed"})));
        let event_type = match declared {
            Response::Ok { result, .. } => result["type"].as_str().unwrap().to_string(),
            other => panic!("expected declare to succeed, got {other:?}"),
        };
        assert_eq!(event_type, "flag-manager/profile-changed");

        let published = session.handle(
            "flag-manager",
            &call("events.publish", serde_json::json!({"type": event_type, "payload": {"slot": 2}})),
        );
        assert!(matches!(published, Response::Ok { .. }), "{published:?}");
    }

    #[test]
    fn lifecycle_subscribe_acknowledges_holding_the_capability() {
        let mut session = Session::new();
        session.broker.grant("p", [Capability::LifecycleRead]);
        let res = session.handle("p", &call("lifecycle.subscribe", serde_json::Value::Null));
        assert!(matches!(res, Response::Ok { .. }), "{res:?}");
    }

    #[test]
    fn session_keeps_one_plugins_settings_out_of_anothers_reach() {
        // The escape this is guarding is not hypothetical: a settings API that
        // took the plugin id as a parameter is the obvious way to write one,
        // and it would have let any plugin holding settings.read address every
        // other plugin's document. `handle` passes its own record of who is
        // calling and nothing else, so the request below reads `thief`'s own
        // settings however it words the question. Remove that and `secret`
        // appears in the result.
        let dir = std::env::temp_dir().join("cordial-session-settings-namespace");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = Session::with_profile(&dir);
        session.broker.grant("victim", [Capability::SettingsWrite]);
        session.broker.grant("thief", [Capability::SettingsRead]);

        let stored = session.handle(
            "victim",
            &call("settings.set", serde_json::json!({"settings": {"secret": "cookie"}})),
        );
        assert!(matches!(stored, Response::Ok { .. }), "{stored:?}");

        let res = session.handle(
            "thief",
            &call("settings.get", serde_json::json!({"plugin": "victim"})),
        );
        match res {
            Response::Ok { result, .. } => {
                assert!(result.get("secret").is_none(), "read another plugin's settings: {result}");
                assert_eq!(result, serde_json::json!({}), "thief has saved nothing of its own");
            }
            other => panic!("expected the caller's own settings, got {other:?}"),
        }
    }

    #[test]
    fn reading_settings_does_not_carry_permission_to_replace_them() {
        let dir = std::env::temp_dir().join("cordial-session-settings-readonly");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = Session::with_profile(&dir);
        session.broker.grant("reader", [Capability::SettingsRead]);
        let res = session
            .handle("reader", &call("settings.set", serde_json::json!({"settings": {"a": 1}})));
        match res {
            Response::Denied { capability, .. } => assert_eq!(capability, "settings.write"),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn a_session_with_no_profile_refuses_settings_rather_than_dropping_them() {
        // `Session::new` has nowhere to put a document. Answering Ok would
        // tell the plugin the user's choice was saved when it went nowhere.
        let mut session = Session::new();
        session.broker.grant("themer", [Capability::SettingsWrite]);
        let res = session
            .handle("themer", &call("settings.set", serde_json::json!({"settings": {}})));
        match res {
            Response::Error { message, .. } => assert!(message.contains("profile"), "{message}"),
            other => panic!("expected an explicit failure, got {other:?}"),
        }
    }

    #[test]
    fn session_has_no_broker_for_flags_and_says_so_rather_than_pretending() {
        // flags.* is deliberately out of this module's scope (see the
        // Session doc comment); it must fail loudly as "no broker wired"
        // rather than silently answering Ok with nothing behind it, which is
        // exactly the stub-that-lies AGENTS.md warns against.
        let mut session = Session::new();
        session.broker.grant("p", [Capability::FlagsRead]);
        let res = session.handle("p", &call("flags.list", serde_json::Value::Null));
        match res {
            Response::Error { message, .. } => assert!(message.contains("no broker wired"), "{message}"),
            other => panic!("expected an explicit no-broker error, got {other:?}"),
        }
    }
}
