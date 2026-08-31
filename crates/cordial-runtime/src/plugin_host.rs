//! Running plugins alongside the client.
//!
//! Discovery, grants, spawning and the broker all live in `cordial-plugins`.
//! This is the join: it serves the methods those plugins call, backed by
//! Cordial's real state rather than a stand-in.
//!
//! One thread per plugin, each blocking on its own plugin's stdout. Plugins are
//! separate processes that mostly sit idle, so a thread each is the simple
//! correct thing. The broker's own decisions are per plugin and made before
//! dispatch, so they need nothing shared — but two effects genuinely are
//! shared across every running plugin in this process, and [`Shared`] is
//! where that state actually lives:
//!
//! * the event registry (ADR-006), because declaring, publishing and
//!   subscribing all have to agree about the same namespaces regardless of
//!   which plugin's thread is asking; and
//! * every running plugin's writable stdin, because delivering a published
//!   event to a subscriber means writing into a *different* plugin's pipe
//!   from the thread serving the publisher's `events.publish` call —
//!   `cordial_plugins::host::Writer` is what makes that safe without also
//!   sharing the read half, which stays owned by the one thread that reads
//!   it; and
//! * every running plugin's core-event queue and the capabilities it was
//!   granted, because the core bus (ADR-026) is published by the *client* --
//!   from `load.rs`, on threads that never see a plugin at all -- and has to
//!   find both from outside every serving thread.

use cordial_plugins::broker::Broker;
use cordial_plugins::capability::Capability;
use cordial_plugins::core_events::{self, CoreEvent};
use cordial_plugins::events::EventRegistry;
use cordial_plugins::host::{authorise, Delivered, Plugin as PluginProc, Pump, Writer};
use cordial_plugins::presence::{DiscordPresence, PresencePayload};
use cordial_plugins::protocol::{Push, Request, Response};
use cordial_plugins::preferences;
use cordial_plugins::settings::{self, Store};
use cordial_plugins::{denials, enablement, grants, manifest, notify, urlopen};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// One running plugin's end of the core bus: where an event is queued for it,
/// and what it is allowed to hear.
///
/// The grant is kept here rather than looked up through a `Broker` because
/// every serving thread owns its own `Broker` holding exactly one plugin's
/// grant -- there is no shared broker to ask, and inventing one so the client
/// could ask it would put a lock the engine's threads contend for in front of
/// something that is only ever read.
struct Listener {
    /// Behind an `Arc` so [`flush_core_events`] can take a copy under the map
    /// lock and then *release* it before waiting. Holding the lock across a
    /// wait would put every publisher behind the flush, which is the blocking
    /// publish this whole design exists to prevent, arriving through the lock
    /// instead of through the pipe.
    pump: Arc<Pump>,
    granted: BTreeSet<Capability>,
}

/// State shared by every plugin's serving thread within one Cordial run.
///
/// Fresh every launch, the same as `Broker` always is — nothing here persists
/// across a restart, and nothing here is visible outside this process.
#[derive(Clone)]
struct Shared {
    events: Arc<Mutex<EventRegistry>>,
    /// Every currently-running plugin's stdin, keyed by id, so a publisher's
    /// thread can push into a subscriber's pipe without becoming the thread
    /// that reads that subscriber's own stdout.
    writers: Arc<Mutex<BTreeMap<String, Writer>>>,
    /// The same plugins again, keyed the same way, for [`publish_core`].
    ///
    /// Separate from `writers` rather than folded into it because the two
    /// paths are deliberately different: `events.publish` writes straight down
    /// a `Writer` from the publishing plugin's own serving thread, and a core
    /// event goes through a `Pump` so the client never waits. See
    /// [`publish_core`] for why that distinction is the whole point.
    listeners: Arc<Mutex<BTreeMap<String, Listener>>>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            events: Arc::new(Mutex::new(EventRegistry::new())),
            writers: Arc::new(Mutex::new(BTreeMap::new())),
            listeners: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

/// The one `Shared` this process runs, reachable from the client.
///
/// **This exists because the bug it fixes was that nothing reached the
/// client at all.** ADR-026's core bus landed with its only producer on
/// `cordial_plugins::host::Session`, which is constructed nowhere outside
/// that crate's own tests -- so `plugins/discord-presence` called
/// `lifecycle.subscribe`, was told `ok`, and then waited forever for a
/// `cordial/client.launch` no code path could ever publish. The client
/// publishes long after `start_all` has returned, from threads that hold none
/// of its locals, so the map has to be reachable by name rather than passed
/// down.
static SHARED: OnceLock<Shared> = OnceLock::new();

fn shared() -> &'static Shared {
    SHARED.get_or_init(Shared::new)
}

/// Start every approved plugin. Returns how many are running.
///
/// Never fails the launch. A plugin that will not start is reported and skipped:
/// the client working without a plugin is a much better outcome than a plugin
/// stopping the client.
pub fn start_all() -> usize {
    let root = manifest::plugin_root();
    let found = manifest::discover(&root);
    if found.is_empty() {
        return 0;
    }

    // Plugin *code* is installed once for the machine; what a plugin is
    // allowed to do, and anything it remembers, belong to the profile
    // (ADR-013). An approval given in a throwaway profile is not an approval
    // here, so the grants are read from this profile and nowhere else.
    let profile = crate::profile::active();
    grants::migrate_legacy_into(&profile);
    let grants_path = grants::path_in(&profile);
    // Where a plugin that will not start says so, for the settings window to
    // read. See `cordial_plugins::health`: a plugin whose process fails is
    // otherwise indistinguishable in Settings from one that works and has not
    // done anything yet.
    let health_path = cordial_plugins::health::path_in(&profile);
    let approved = grants::load(&grants_path);
    let store = Store::new(&profile);
    // The process-global one rather than a local, because the client's own
    // `publish_core` calls arrive from `load.rs` after this function has long
    // returned and can reach nothing this scope owns.
    let shared = shared().clone();
    let mut started = 0usize;

    for plugin in found {
        let id = plugin.manifest.id.clone();

        // Checked before anything about grants, and before the process is
        // spawned at all. The bug this fixes was `start_all` never reading
        // `plugin-enabled.json` in the first place, so Settings' switch wrote
        // a file nothing consulted; a plugin that started and then had every
        // request refused would still be the "stub that lies" shape AGENTS.md
        // warns about, just moved into a process. Absence in the file means
        // enabled — `enablement::is_enabled` already encodes that, and a
        // plugin nobody has an opinion about must keep running subject to its
        // grants, the same as before this change, or turning this bug off
        // would quietly turn a different one on.
        if !enabled_in_profile(&profile, &id) {
            println!("  plugin {id}: disabled in Settings, not started");
            continue;
        }

        // A plugin with no entry module is data — a texture pack, a flag
        // preset, a set of preferences — and there is nothing to spawn. Said
        // plainly rather than falling through to "no capabilities granted",
        // which would report a working asset pack as a failed plugin and send
        // its author looking for a permission that was never involved.
        // ADR-021: code is a property read off the manifest, not a category.
        if !plugin.has_code() {
            println!("  plugin {id}: data only, nothing to start");
            continue;
        }

        let granted = approved.get(&id).cloned().unwrap_or_default();

        // Say what was withheld. A plugin silently doing less than it asked for
        // is otherwise indistinguishable from a plugin that is broken.
        let withheld: Vec<_> =
            plugin.requested.iter().filter(|c| !granted.contains(c)).copied().collect();
        if !withheld.is_empty() {
            let names: Vec<_> = withheld.iter().map(|c| c.name()).collect();
            println!("  plugin {id}: not granted {}", names.join(", "));
        }
        if granted.is_empty() {
            println!("  plugin {id}: no capabilities granted, not started");
            continue;
        }

        let entry = match plugin.entry_path() {
            Ok(e) => e,
            Err(e) => {
                println!("  plugin {id}: {e}");
                let _ = cordial_plugins::health::record(&health_path, &id, &e.to_string());
                continue;
            }
        };
        match PluginProc::spawn(&id, &entry) {
            Ok(mut proc) => {
                // The handshake, before the plugin has asked for anything, so
                // that reading its own configuration — the first thing most
                // plugins do — costs no round trip. Best effort: a plugin that
                // died on startup is reported by its stdout closing, not here.
                let _ = proc.push(&settings::init_push(
                    Some(&store),
                    &plugin.manifest.preferences,
                    &id,
                    &granted,
                ));

                // Registered before the process is handed to its own thread:
                // another plugin's `events.publish` has to be able to find
                // this writer immediately, not only once this thread gets
                // around to inserting it, which would be a race against
                // whichever plugin started first getting to publish first.
                shared.writers.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), proc.writer());

                // The core bus's end of the same plugin, registered at the
                // same moment and for the same reason: a `client.launch`
                // published the instant `start_all` returns must find this
                // plugin, not race the thread that is about to serve it.
                //
                // A `Pump` rather than the `Writer` above -- see
                // `publish_core`. Its own thread does the blocking write, so
                // the client thread that published never touches this pipe.
                shared.listeners.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    id.clone(),
                    Listener { pump: Arc::new(Pump::start(proc.writer())), granted: granted.clone() },
                );

                let mut broker = Broker::new();
                broker.grant(&id, granted);
                let store = store.clone();
                let shared = shared.clone();
                let plugin_dir = plugin.dir.clone();
                let declared = plugin.manifest.preferences.clone();
                let grants_path = grants_path.clone();
                std::thread::Builder::new()
                    .name(format!("plugin:{id}"))
                    .spawn(move || serve(proc, broker, store, shared, plugin_dir, declared, grants_path))
                    .ok();
                started += 1;
                println!("  plugin {id}: started");
                // **Clearing matters as much as recording.** A warning left
                // behind after the thing is fixed teaches people to ignore
                // warnings, which costs more than the one it was pointing at.
                // Writes nothing when there was nothing to clear.
                let _ = cordial_plugins::health::clear(&health_path, &id);
            }
            Err(e) => {
                println!("  plugin {id}: could not start ({e})");
                // The message a person sees in Settings. `e` here is the
                // spawn error -- "No such file or directory" when Deno is not
                // installed, which is the single most common way a plugin
                // fails on a machine that has never run one -- so it is
                // prefixed rather than shown bare, because on its own it
                // names no file and reads as a Cordial bug.
                let _ = cordial_plugins::health::record(
                    &health_path,
                    &id,
                    &format!("could not start: {e}"),
                );
            }
        }
    }
    started
}

/// Register every enabled plugin's own `overlay/` directory with the asset
/// resolver, before the engine asks for anything.
///
/// **A shipped overlay directory needs no capability, and that is the existing
/// precedent rather than a new hole.** `flags::collect` already reads every
/// enabled plugin's `flags.json` with no capability check at all, because a
/// static file a plugin ships is not a request a process is making — it is
/// what the plugin *is*, and installing and enabling it is the consent. The
/// `assets.override` capability gates something different: a **running**
/// plugin asking Cordial to register a directory of its choosing at runtime,
/// which is a request from a process and is brokered like every other one
/// (ADR-007). Getting these two confused would mean a texture pack that
/// cannot work without a permission prompt for a process it does not have.
///
/// Without this, a data-only plugin could not overlay anything at all: the
/// only way to register a root was the `assets.override` call, and a plugin
/// with no entry module has nothing to make it from. That is the whole
/// mechanism ADR-021 needs for "a texture pack is a plugin" to be true rather
/// than aspirational.
///
/// **System first, then the user root, sorted within each**, matching
/// `flags::collect` exactly — including its rule that a user plugin may not
/// shadow a first-party id. Registration order is precedence order among
/// plugins (last wins), so this is the fact a shadow report can quote, and it
/// does not depend on directory iteration order.
pub fn register_static_overlays() -> usize {
    let profile = crate::profile::active();
    let mut claimed: std::collections::BTreeSet<String> = Default::default();
    let mut registered = 0usize;
    for root in crate::flags::plugin_dirs() {
        for plugin in manifest::discover(&root) {
            let id = plugin.manifest.id.clone();
            if !claimed.insert(id.clone()) {
                continue;
            }
            if !enabled_in_profile(&profile, &id) {
                continue;
            }
            let overlay = plugin.dir.join("overlay");
            if !overlay.is_dir() {
                continue;
            }
            crate::android::asset::register_plugin_root(&id, overlay);
            registered += 1;
        }
    }
    registered
}

/// Whether `id` is allowed to run in `profile_dir`, per Settings' plugin
/// toggle (`cordial_plugins::enablement`).
///
/// A thin wrapper rather than calling `enablement::is_enabled` straight from
/// `start_all`, so this file's own tests can exercise the decision on a
/// scratch profile directory without going through manifest discovery and
/// process spawning to do it.
fn enabled_in_profile(profile_dir: &std::path::Path, id: &str) -> bool {
    enablement::is_enabled(profile_dir, id)
}

fn serve(
    mut proc: PluginProc,
    mut broker: Broker,
    store: Store,
    shared: Shared,
    plugin_dir: PathBuf,
    declared: Vec<preferences::Declaration>,
    grants_path: PathBuf,
) {
    let id = proc.id.clone();
    // One Discord connection per plugin thread, held for the plugin's whole
    // run rather than opened fresh on every call — Session does the same for
    // the same reason: a plugin that calls presence.set on every tick must
    // not hand-shake with Discord that often.
    let mut presence = DiscordPresence::new();
    // `None` so the very first request always rereads and regrants, even
    // though `broker` already holds what `start_all` read moments earlier —
    // that one redundant read is cheaper than threading the mtime `start_all`
    // saw through the spawn, and it means this function has exactly one path
    // rather than a "first time" special case.
    let mut grants_seen: Option<std::time::SystemTime> = None;
    let denials_path = denials::path_in(store.profile_dir());
    // What this thread has already written to `denials_path`, so a plugin
    // retrying a call it does not have costs one disk write and not one per
    // retry. Reset only by the plugin actually being granted the capability
    // and calling again — `refresh_grant` below regrants but does not touch
    // this set, so a capability that is granted and then revoked again is
    // still recorded as denied rather than silently written twice.
    let mut denied_already: BTreeSet<Capability> = BTreeSet::new();
    while let Some(req) = proc.next_request() {
        let req = match req {
            Ok(r) => r,
            Err(e) => {
                println!("  plugin {id}: sent something unreadable ({e})");
                break;
            }
        };
        // Reread before deciding, not after: a grant written between two
        // requests must be in force for the request that arrives after it, or
        // "I turned the switch on and it still didn't work" is a fresh bug
        // wearing the clothes of the one that was just fixed. See
        // `refresh_grant`'s own comment for what this replaced.
        refresh_grant(&grants_path, &id, &mut broker, &shared, &mut grants_seen);
        let auth = authorise(&mut broker, &id, &req);
        if let Err(Response::Denied { capability, .. }) = &auth {
            if let Some(cap) = Capability::parse(capability) {
                if denied_already.insert(cap) {
                    if let Err(e) = denials::record(&denials_path, &id, cap) {
                        println!("  plugin {id}: could not record the {capability} denial: {e}");
                    }
                }
            }
        }
        let response = match auth {
            Err(refusal) => refusal,
            Ok(()) => dispatch(&id, &req, &store, &mut presence, &shared, &plugin_dir, &declared),
        };
        if proc.reply(&response).is_err() {
            break;
        }
    }
    // The plugin is gone; nothing should still be able to reach it. A
    // publish arriving after this looks up an id `writers` no longer has and
    // simply has one fewer subscriber to deliver to, and an asset overlay it
    // registered stops being consulted — falling straight back to whatever
    // would have resolved without it, because nothing was ever written to
    // undo (ADR-010). `unregister_plugin_root` is a no-op if this plugin
    // never registered one, so calling it unconditionally costs nothing.
    shared.writers.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    // And its core-event queue, so a plugin that died does not accumulate: a
    // `Pump` whose plugin is gone would keep a queue, a thread and a grant
    // alive for the rest of the run, and every later `publish_core` would
    // count it as a recipient. Dropping the `Pump` closes its channel, which
    // is what ends that thread.
    shared.listeners.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    crate::android::asset::unregister_plugin_root(&id);
    proc.kill();
}

/// Re-read `id`'s grant from `grants_path` if the file has changed since the
/// last check, so a capability turned on in Settings reaches a plugin that is
/// already running rather than only the next one `start_all` spawns.
///
/// **This is a real bug, not a hypothetical one.** `start_all` used to read
/// the grants file exactly once, before any plugin thread existed, and
/// `broker.grant` was never called again for the rest of the run. Measured on
/// a live Flatpak instance: `cordial-shell` started at 14:19:12, the user
/// granted `discord-presence` every capability it asked for through Settings,
/// and `plugin-grants.json` was written at 14:20:51 — ninety seconds into a
/// run whose broker still held the empty set it read before the file existed.
/// The grant was correct, the profile was correct, and the plugin kept being
/// refused anyway, which is indistinguishable from Settings simply not
/// working.
///
/// Checked by `mtime` rather than reloaded unconditionally: this runs on
/// every request a plugin makes, most of which will not have raced a grant at
/// all, and a `stat(2)` is cheap enough to make on every one where reading and
/// re-parsing the whole document is not. A metadata read that fails (the file
/// does not exist yet, or has just been deleted) is treated as `None`, so
/// appearing and disappearing both count as changes and both correctly cause
/// a reload — the disappearing case regrants to nothing, which is the right
/// answer for a grants file that was just wiped.
///
/// **Also updates `shared`'s `Listener`, and this half is not optional.**
/// `broker` only gates the calls a plugin *makes* — `flush_core_events`
/// checks `Listener::granted` instead, a second snapshot of the same grant
/// taken once at the same moment `start_all` read the first one and never
/// touched again. Fixing only `broker` would have made `lifecycle.subscribe`
/// start succeeding the moment it was granted while leaving the plugin
/// permanently deaf to the very core events that capability exists to unlock
/// — `discord-presence` calls `presence.set` from inside its
/// `client.launch`/`client.ready` handler, so a grant that reached one
/// snapshot and not the other would still look identical to the bug this
/// function exists to fix, just one hop further downstream.
fn refresh_grant(
    grants_path: &Path,
    id: &str,
    broker: &mut Broker,
    shared: &Shared,
    last_seen: &mut Option<std::time::SystemTime>,
) {
    let modified = std::fs::metadata(grants_path).and_then(|m| m.modified()).ok();
    if modified == *last_seen {
        return;
    }
    *last_seen = modified;
    let granted = grants::load(grants_path).get(id).cloned().unwrap_or_default();
    broker.grant(id, granted.clone());
    if let Some(listener) = shared.listeners.lock().unwrap_or_else(|e| e.into_inner()).get_mut(id) {
        listener.granted = granted;
    }
}

/// Serve one authorised request. The broker has already decided this may
/// proceed, so this only has to do the work.
fn dispatch(
    id: &str,
    req: &Request,
    store: &Store,
    presence: &mut DiscordPresence,
    shared: &Shared,
    plugin_dir: &Path,
    declared: &[preferences::Declaration],
) -> Response {
    match req.method.as_str() {
        // `id` is this thread's own plugin — the process on the other end of
        // its pipe — and it is the only id the settings broker is given. A
        // plugin naming another one in its params reads and writes its own
        // document; see cordial_plugins::settings.
        "settings.get" | "settings.set" => settings::serve(Some(store), id, req),
        // ADR-020. Read-only on purpose: the answers belong to the user, so
        // the launcher writes them and the plugin is only told. The
        // declarations come from this plugin's own manifest, held by the
        // serving thread rather than taken from the request, for the reason
        // `id` is -- a plugin that could name its own schema could claim a
        // range it does not have.
        "preferences.get" => {
            let prefs = preferences::Store::new(store.profile_dir());
            preferences::serve(Some(&prefs), declared, id, req)
        }
        // ADR-007's worked example, finally reachable from the host the
        // client actually runs: cordial-plugins already speaks Discord's IPC
        // framing (presence.rs) and cordial_plugins::host::Session already
        // wires it up, but Session is only ever constructed in that crate's
        // own tests. This is the same wiring, once, for the real host —
        // reusing DiscordPresence rather than re-opening the socket search
        // here, so there is exactly one place that knows where Discord's IPC
        // socket might be.
        "presence.set" => match PresencePayload::parse(&req.params) {
            Ok(payload) => respond(req.id, presence.set(&payload)),
            Err(message) => Response::Error { id: req.id, message },
        },
        "presence.clear" => respond(req.id, presence.clear()),
        // An acknowledgement, and nothing more, because delivery is gated on
        // the capability rather than on this call: `publish_core` sends every
        // core event to every plugin holding the event's capability, whether
        // or not it ever made this call. So `Ok` here means exactly "you hold
        // lifecycle.read", which is what it has always meant.
        //
        // This comment used to end "delivery ... stays unimplemented rather
        // than silently promised", which was honest when it was written and
        // is not any more: `publish_core` below, and the `client.launch` and
        // `engine.version` publishes in `load.rs`, are that delivery. Left as
        // a note rather than deleted, because a plugin author reading only
        // this arm would otherwise conclude a subscription is what makes
        // events arrive, and then wonder why one that never subscribed still
        // hears them.
        "lifecycle.subscribe" => Response::Ok { id: req.id, result: serde_json::Value::Null },
        "flags.list" => {
            let resolved = crate::flags::resolve(crate::flags::collect());
            let list: Vec<_> = resolved
                .iter()
                .map(|(k, r)| {
                    serde_json::json!({
                        "key": k,
                        "value": r.value,
                        "source": r.source.describe(),
                    })
                })
                .collect();
            Response::Ok { id: req.id, result: serde_json::Value::Array(list) }
        }
        "flags.get" => {
            let key = req.params.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            let resolved = crate::flags::resolve(crate::flags::collect());
            let value = resolved.get(key).map(|r| {
                serde_json::json!({ "value": r.value, "source": r.source.describe() })
            });
            Response::Ok { id: req.id, result: value.unwrap_or(serde_json::Value::Null) }
        }
        "log.write" => {
            let msg = req.params.get("message").and_then(|v| v.as_str()).unwrap_or("");
            println!("  [{id}] {msg}");
            append_plugin_log(store.profile_dir(), id, msg);
            Response::Ok { id: req.id, result: serde_json::Value::Null }
        }
        // ADR-007's other two brokered effects: the plugin sends a payload,
        // Cordial owns the D-Bus connection. Neither call learns anything
        // about the bus it went over.
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
        // ADR-010: a subdirectory of the plugin's own installed directory,
        // never an arbitrary path — `resolve_within` refuses anything that
        // would name somewhere else, the same treatment `manifest::Plugin`
        // gives a manifest's `entry`. Registration only takes effect for as
        // long as this plugin's own thread is alive; `serve` unregisters it
        // unconditionally when the process ends, so a disabled or removed
        // plugin's overlay never outlives it.
        "assets.override" => {
            if req.params.get("clear").and_then(|v| v.as_bool()) == Some(true) {
                crate::android::asset::unregister_plugin_root(id);
                return Response::Ok { id: req.id, result: serde_json::Value::Null };
            }
            let rel = req.params.get("dir").and_then(|v| v.as_str()).unwrap_or("overlay");
            match resolve_within(plugin_dir, rel) {
                Ok(resolved) => {
                    let shown = resolved.display().to_string();
                    crate::android::asset::register_plugin_root(id, resolved);
                    Response::Ok { id: req.id, result: serde_json::json!({"registered": shown}) }
                }
                Err(message) => Response::Error { id: req.id, message },
            }
        }
        // `flags.write`: a plugin's contribution to its own, machine-global
        // `flags.json` (ADR-013's open question — this file stays global
        // regardless of which profile granted the capability). Takes effect
        // at the next launch only; there is no live counterpart here because
        // `FFlag`/`FInt`/`FString` are read once at startup (ADR-005), which
        // is the entire reason `flags.write` and `flags.write.dynamic` are
        // two separate capabilities rather than one.
        "flags.set" => {
            let Some(values) = req.params.get("values").and_then(|v| v.as_object()) else {
                return Response::Error { id: req.id, message: "flags.set needs a values object".into() };
            };
            let flat: BTreeMap<String, String> = values
                .iter()
                .map(|(k, v)| {
                    let s = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), s)
                })
                .collect();
            respond(req.id, crate::flags::write_plugin_layer(id, &flat))
        }
        // ADR-006. `id` namespaces `declare` and gates `publish` the same way
        // it gates `settings.*` above: it is Cordial's own record of which
        // process is on the pipe, never a field the request could set.
        "events.declare" => match req.params.get("name").and_then(|v| v.as_str()) {
            Some(name) => {
                let mut events = shared.events.lock().unwrap_or_else(|e| e.into_inner());
                match events.declare(id, name) {
                    Ok(event_type) => Response::Ok { id: req.id, result: serde_json::json!({"type": event_type}) },
                    Err(message) => Response::Error { id: req.id, message },
                }
            }
            None => Response::Error { id: req.id, message: "events.declare needs a name".into() },
        },
        "events.subscribe" => match req.params.get("type").and_then(|v| v.as_str()) {
            Some(event_type) => {
                let mut events = shared.events.lock().unwrap_or_else(|e| e.into_inner());
                match events.subscribe(id, event_type) {
                    Ok(()) => Response::Ok { id: req.id, result: serde_json::Value::Null },
                    Err(message) => Response::Error { id: req.id, message },
                }
            }
            None => Response::Error { id: req.id, message: "events.subscribe needs a type".into() },
        },
        "events.publish" => {
            let Some(event_type) = req.params.get("type").and_then(|v| v.as_str()) else {
                return Response::Error { id: req.id, message: "events.publish needs a type".into() };
            };
            let subscribers = {
                let events = shared.events.lock().unwrap_or_else(|e| e.into_inner());
                if !events.may_publish(id, event_type) {
                    return Response::Error {
                        id: req.id,
                        message: format!(
                            "{id:?} may not publish on {event_type:?}; it must declare that type before publishing on it"
                        ),
                    };
                }
                events.subscribers(event_type).into_iter().map(str::to_string).collect::<Vec<_>>()
            };
            let payload = req.params.get("payload").cloned().unwrap_or(serde_json::Value::Null);
            // **Straight down the `Writer`, not through the `Pump` the core
            // bus uses, and that asymmetry is deliberate.** The reason a core
            // event may never block is that its publisher is a thread the
            // client is waiting on; the publisher here is a plugin's own
            // serving thread, which exists to wait for exactly this plugin
            // and blocks nothing else in the process. Widening this to the
            // pump would make one plugin's publish lossy to buy a property
            // this path does not need. See `publish_core`.
            let writers = shared.writers.lock().unwrap_or_else(|e| e.into_inner());
            for subscriber in subscribers {
                if let Some(writer) = writers.get(&subscriber) {
                    // Best effort, the same as every other push in this
                    // project: a subscriber that has already died is not a
                    // reason to fail the publisher's call, and the write
                    // error here would only repeat what that subscriber's own
                    // thread is about to discover reading its closed stdout.
                    let _ = writer.push(&Push { event: event_type.to_string(), payload: payload.clone() });
                }
            }
            Response::Ok { id: req.id, result: serde_json::Value::Null }
        }
        // Authorised but not implemented yet. Distinct from `denied`, which
        // would send an author looking for a permission that was never the
        // problem. `flags.setDynamic` lands here permanently rather than
        // temporarily: it needs a live write into the running engine's own
        // `DFFlag` table, and nothing in this project has ever reached into
        // the engine process to do that — ADR-001 and ADR-003 rule out the
        // in-process access that would take, so this is not a gap waiting to
        // be filled, it is a capability whose effect has nowhere to live.
        other => Response::Error {
            id: req.id,
            message: format!("{other} is not implemented yet"),
        },
    }
}

/// Append one `log.write` line to a file in the plugin's own profile, beside
/// the `println!` in the arm above.
///
/// **Not a logging subsystem, and deliberately not one.** There is no
/// rotation, no level filtering, and no cap on how large `plugin.log` grows —
/// adding any of that is the scope this function exists to avoid taking on.
/// It exists to close one specific gap AGENTS.md keeps finding here: a
/// packaged or Flatpak launch has no terminal for `println!` to reach, so a
/// plugin whose only way to report what it did was stdout had, in practice,
/// no way to report anything at all. A single append-only file a user can
/// open is the whole fix; anything more belongs to a later change that has
/// actually been asked for.
///
/// Best effort. A plugin must not stop running because its log file could not
/// be opened — that would make logging load-bearing for the thing it is
/// meant to be a diagnostic for — so a failure here is silent and the
/// `println!` above remains this file's real belt-and-braces.
fn append_plugin_log(profile_dir: &Path, id: &str, message: &str) {
    use std::io::Write;
    let Ok(mut file) =
        std::fs::OpenOptions::new().create(true).append(true).open(profile_dir.join("plugin.log"))
    else {
        return;
    };
    let _ = writeln!(file, "[{id}] {message}");
}

/// A subdirectory of `base`, refusing anything that would name somewhere
/// else. `rel` comes from a plugin's own `assets.override` call and is
/// treated as attacker-controlled the same way `manifest::Plugin::entry_path`
/// treats a manifest's `entry` — both cross a trust boundary from a process
/// Cordial does not control, and both get the same refusal rather than a
/// path that is quietly rewritten into something safe.
fn resolve_within(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() || rel_path.components().any(|c| c.as_os_str() == "..") {
        return Err(format!("{rel:?} must be a path inside the plugin's own directory"));
    }
    Ok(base.join(rel_path))
}

/// Turn a broker effect's plain `Result` into the wire `Response` — the
/// success case carries nothing back, so this only exists to spell the
/// error case the same way every time. Copied from
/// `cordial_plugins::host::respond` rather than imported: it is three lines,
/// and pulling it in would mean making it `pub` in a crate whose own doc
/// comment says `Session` is the only thing with a real broker for this.
fn respond(id: u64, result: Result<(), String>) -> Response {
    match result {
        Ok(()) => Response::Ok { id, result: serde_json::Value::Null },
        Err(message) => Response::Error { id, message },
    }
}

/// Tell every plugin entitled to hear it that Cordial observed `name`.
///
/// **Called by the client, from the client's own threads**, which is the
/// difference between this and everything else in this file. `name` is a
/// `&'static str` from `cordial_plugins::core_events`; the wire name is built
/// by `CoreEvent::wire_name` so the reserved `cordial/` prefix is never
/// spelled at a call site and never assembled from anything a plugin said.
///
/// **Never blocks.** A push is a blocking write into a plugin's stdin, and a
/// plugin that has stopped reading fills the pipe -- 64 KiB on Linux -- after
/// which whoever published waits on a process that may never read again. For a
/// core event that is a thread the client is waiting on, and the engine's
/// looper polls millions of times a second; it cannot queue behind a wedged
/// plugin. So each plugin has a `Pump` with a bounded queue, and a publish
/// that finds it full drops the event and counts it rather than waiting.
/// ADR-026 is explicit that this is the single property the bus must have.
///
/// **Gated per event family, never broadcast.** The capability comes from
/// `core_events::capability_for`, which is a closed table: an event with no
/// entry there requires a capability nobody holds and so reaches nobody. That
/// is the safe direction for a name somebody adds and forgets to gate, and
/// `core_events.rs` has a test asserting it.
///
/// What comes back is how many heard it and how many were too slow, so a
/// caller that wants to notice can. There is nothing to fail: publishing is
/// something the client does on its way past.
pub fn publish_core(name: &'static str, payload: serde_json::Value) -> Delivered {
    let Some(needed) = core_events::capability_for(name) else {
        println!("  plugin core event {name:?} is not in the capability table, so nobody receives it");
        return Delivered::default();
    };
    let event = CoreEvent::new(name, payload);
    let push = Push { event: event.wire_name(), payload: event.payload };

    let listeners = shared().listeners.lock().unwrap_or_else(|e| e.into_inner());
    let mut delivered = Delivered::default();
    for listener in listeners.values() {
        if !listener.granted.contains(&needed) {
            continue;
        }
        // Holding the map's lock across `offer` is safe precisely because
        // every holder of it, this one included, only ever does bounded work
        // under it: `offer` is a `try_send` that never waits, `start_all` and
        // a serving thread on its way out do one map insertion or removal,
        // and `flush_core_events` copies the pumps out and drops the lock
        // before it waits on any of them. An earlier version of this comment
        // claimed the flush was not a user of this lock at all, which was
        // wrong and was the sentence that would have hidden the day somebody
        // published a core event from the engine's own looper thread and
        // found it queued behind a 500 ms shutdown wait.
        if listener.pump.offer(push.clone()) {
            delivered.sent += 1;
        } else {
            delivered.dropped += 1;
        }
    }
    delivered
}

/// Wait for queued core events to reach every plugin, within `limit` in
/// total. Names the plugins whose queue had not drained when it ran out.
///
/// **Call this before Cordial exits.** Delivery is asynchronous by design, so
/// a publish followed by an exit is a race the exit wins and the last thing a
/// plugin is told is the one thing it never hears -- which for
/// `client.shutdown` is the whole point of the event.
///
/// **`limit` is the whole budget, not each plugin's share of it**, and that
/// distinction is the entire reason this function exists rather than a loop
/// over [`Pump::flush`] at the call site. `Pump::flush` takes a fresh deadline
/// per call, so the obvious loop costs `limit` times however many plugins are
/// running -- a number the *user* chooses by installing plugins -- while the
/// comment above it and the line it prints both promise a fixed bound. A
/// shutdown that a plugin can lengthen without limit is the blocking-publish
/// hazard arriving at the one moment it is least welcome.
///
/// The lock is released before any waiting, so a publish arriving from another
/// thread during shutdown is not held up by it either.
pub fn flush_core_events(limit: std::time::Duration) -> Vec<String> {
    let deadline = std::time::Instant::now() + limit;
    let pumps: Vec<(String, Arc<Pump>)> = shared()
        .listeners
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(id, l)| (id.clone(), l.pump.clone()))
        .collect();

    let mut stuck = Vec::new();
    for (id, pump) in pumps {
        // Whatever is left of the shared budget. A plugin reached with none of
        // it left is still asked: `Pump::flush` returns true immediately for a
        // queue that is already empty, so only a plugin that genuinely has
        // something outstanding is named.
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if !pump.flush(left) {
            stuck.push(id);
        }
    }
    stuck
}

/// One plugin's core events that never arrived, and which of the two reasons
/// it was.
pub struct Undelivered {
    pub id: String,
    pub events: u64,
    /// The plugin had stopped reading -- its pump's channel was disconnected,
    /// which happens when a write to its stdin failed -- rather than merely
    /// falling behind.
    ///
    /// **Separated because the report is the only place these numbers are
    /// read, and it used to assert the wrong one.** It said "its queue was
    /// full" for every drop, which sends the reader to `QUEUE_DEPTH` and to
    /// the plugin's read loop for a plugin that had in fact crashed. That is
    /// the instrument reporting a cause it cannot see, which is the failure
    /// AGENTS.md opens with.
    pub plugin_gone: bool,
}

/// How many core events each plugin never received, and why.
///
/// Only plugins that actually missed something appear. The count exists so a
/// drop is not silent -- `native/opensles.cpp` reports failure rather than
/// handing back a dead engine object for the same reason -- and nothing else
/// in this process would ever print it.
///
/// A plugin that has already exited is not listed: `serve` drops its `Listener`
/// on the way out and the count goes with the queue. Said plainly rather than
/// left to be discovered, because "nothing was missed" is a weaker claim than
/// it looks for a plugin that died mid-run.
pub fn undelivered_core_events() -> Vec<Undelivered> {
    shared()
        .listeners
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(id, l)| Undelivered {
            id: id.clone(),
            events: l.pump.dropped(),
            plugin_gone: l.pump.plugin_gone(),
        })
        .filter(|u| u.events > 0)
        .collect()
}

/// Where plugins are installed, exposed so the loader can report it.
pub fn root() -> PathBuf {
    manifest::plugin_root()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: serde_json::Value) -> Request {
        Request { id: 1, method: method.into(), params }
    }

    fn scratch_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("cordial-plugin-host-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store::new(dir)
    }

    /// A plugin's own installed directory, standing in for `plugin.dir` —
    /// `assets.override` resolves relative to this.
    fn scratch_plugin_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-plugin-host-dir-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // XDG_RUNTIME_DIR is process-wide, and cargo runs this file's tests on
    // multiple threads by default; presence.rs's own tests take the same
    // lock for the same reason. Held for as long as the env var points at a
    // scratch directory, so the two tests below cannot race each other's
    // socket search.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn presence_set_fails_honestly_when_discord_is_not_running() {
        // AGENTS.md: a stub must never claim success it did not have. With
        // no Discord IPC socket present, dispatch must answer Error, not Ok
        // — the exact failure this dispatch arm exists to reach past the
        // "not implemented yet" catch-all in dispatch's `other` arm, and the
        // failure it must still report honestly now that it does.
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir()
            .join(format!("cordial-plugin-host-no-discord-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);

        let store = scratch_store("presence-set");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("presence-set");
        let req = call(
            "presence.set",
            serde_json::json!({"client_id": "1234567890123456", "details": "Playing Baseplate"}),
        );
        let res = dispatch("discord-presence", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        match res {
            Response::Error { message, .. } => assert!(message.contains("not running"), "{message}"),
            other => panic!("expected an honest failure with no Discord listening, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presence_clear_is_a_quiet_no_op_when_nothing_was_ever_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir()
            .join(format!("cordial-plugin-host-clear-noop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);

        let store = scratch_store("presence-clear");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("presence-clear");
        let req = call("presence.clear", serde_json::Value::Null);
        let res = dispatch("discord-presence", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        assert!(matches!(res, Response::Ok { .. }), "{res:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_presence_payload_is_an_error_not_a_panic() {
        let store = scratch_store("presence-bad-payload");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("presence-bad-payload");
        let req = call("presence.set", serde_json::json!({"client_id": "not-a-snowflake"}));
        let res = dispatch("discord-presence", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        match res {
            Response::Error { message, .. } => assert!(message.contains("snowflake"), "{message}"),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn lifecycle_subscribe_acknowledges_the_capability() {
        let store = scratch_store("lifecycle-subscribe");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("lifecycle-subscribe");
        let req = call("lifecycle.subscribe", serde_json::Value::Null);
        let res = dispatch("some-plugin", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        assert!(matches!(res, Response::Ok { .. }), "{res:?}");
    }

    #[test]
    fn an_unimplemented_method_still_says_so_rather_than_pretending() {
        // The catch-all this change carves presence and lifecycle.subscribe
        // out of must still hold for everything else `Session` answers that
        // this host does not yet wire up.
        let store = scratch_store("unimplemented");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("unimplemented");
        // `flags.setDynamic` is the one capability with nowhere to route to:
        // it would need a live write into the running engine's own `DFFlag`
        // table, which nothing in this project reaches into (ADR-001,
        // ADR-003). Every other method this test used to check here —
        // notify.send, url.open, events.*, assets.override, flags.set — is
        // wired for real below and is no longer a stand-in for "not written
        // yet".
        let req = call("flags.setDynamic", serde_json::json!({"key": "DFFlagX", "value": "true"}));
        let res = dispatch("some-plugin", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        match res {
            Response::Error { message, .. } => assert!(message.contains("not implemented yet"), "{message}"),
            other => panic!("expected the not-implemented-yet stub, got {other:?}"),
        }
    }

    #[test]
    fn notify_send_without_a_summary_is_refused_before_touching_the_bus() {
        // The one part of `notify.send` this file can check without a real
        // session bus — the shape check happens before `notify::send` ever
        // opens a connection, matching `notify.rs`'s own coverage of the
        // same rule.
        let store = scratch_store("notify-no-summary");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("notify-no-summary");
        let req = call("notify.send", serde_json::json!({"body": "no summary here"}));
        let res = dispatch("some-plugin", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        match res {
            Response::Error { message, .. } => assert!(message.contains("summary"), "{message}"),
            other => panic!("expected a shape refusal, got {other:?}"),
        }
    }

    #[test]
    fn url_open_refuses_a_non_http_scheme_through_the_real_dispatch() {
        // The exact case ADR-007's doc comment on `UrlOpen` calls out, proven
        // past the capability gate this time — a granted-but-malicious call
        // reaching the real host's dispatch, not only `urlopen.rs`'s own
        // unit tests.
        let store = scratch_store("url-open-bad-scheme");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("url-open-bad-scheme");
        let req = call("url.open", serde_json::json!({"url": "file:///etc/passwd"}));
        let res = dispatch("some-plugin", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        match res {
            Response::Error { message, .. } => assert!(message.contains("refused"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn assets_override_registers_a_root_inside_the_plugins_own_directory() {
        let store = scratch_store("assets-register");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("assets-register");
        std::fs::create_dir_all(plugin_dir.join("overlay/textures")).unwrap();
        std::fs::write(plugin_dir.join("overlay/textures/wood.png"), b"fake texture bytes").unwrap();

        let req = call("assets.override", serde_json::json!({}));
        let res = dispatch("themer", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        assert!(matches!(res, Response::Ok { .. }), "{res:?}");

        assert_eq!(
            crate::android::asset::explain("textures/wood.png"),
            Some("plugin:themer".to_string()),
            "the registered root should now be consulted ahead of the APK"
        );

        // And clearing it falls straight back to nothing being overlaid —
        // there was never a write to undo (ADR-010).
        let clear = call("assets.override", serde_json::json!({"clear": true}));
        let res = dispatch("themer", &clear, &store, &mut presence, &shared, &plugin_dir, &[]);
        assert!(matches!(res, Response::Ok { .. }), "{res:?}");
        assert_eq!(crate::android::asset::explain("textures/wood.png"), None);
    }

    #[test]
    fn assets_override_refuses_a_directory_that_would_escape_the_plugin() {
        let store = scratch_store("assets-escape");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("assets-escape");

        for bad in ["../../etc", "/etc"] {
            let req = call("assets.override", serde_json::json!({"dir": bad}));
            let res = dispatch("themer", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
            match res {
                Response::Error { message, .. } => assert!(message.contains("inside"), "{bad}: {message}"),
                other => panic!("{bad:?} should have been refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_plugin_removed_from_the_writer_map_stops_receiving_its_overlay() {
        // `serve` unregisters unconditionally when a plugin's process ends;
        // this exercises the same call `serve` makes, proving the overlay
        // genuinely stops resolving rather than lingering because nothing
        // ever tore it down.
        let store = scratch_store("assets-teardown");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("assets-teardown");
        std::fs::create_dir_all(plugin_dir.join("overlay")).unwrap();
        std::fs::write(plugin_dir.join("overlay/sound.ogg"), b"fake sound bytes").unwrap();

        let req = call("assets.override", serde_json::json!({}));
        dispatch("sound-pack", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        assert!(crate::android::asset::explain("sound.ogg").is_some());

        crate::android::asset::unregister_plugin_root("sound-pack");
        assert_eq!(crate::android::asset::explain("sound.ogg"), None);
    }

    #[test]
    fn flags_set_writes_the_plugins_own_global_flags_layer() {
        let store = scratch_store("flags-set");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("flags-set");
        let root = std::env::temp_dir().join("cordial-plugin-host-flags-set-plugindir");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // The same lock `flags.rs`'s own tests take before touching this
        // process-wide variable — see that module's note on why a
        // module-local mutex would not actually exclude this one.
        let _guard = crate::flags::tests::ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CORDIAL_PLUGIN_DIR", &root);

        let req = call("flags.set", serde_json::json!({"values": {"FFlagFoo": "true", "FIntBar": 3}}));
        let res = dispatch("tuner", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        assert!(matches!(res, Response::Ok { .. }), "{res:?}");

        let layer = crate::flags::read_layer(
            &root.join("tuner/flags.json"),
            crate::flags::Source::Plugin("tuner".into()),
        )
        .expect("the written file should read back");
        assert_eq!(layer.values["FFlagFoo"], "true");
        assert_eq!(layer.values["FIntBar"], "3");
    }

    #[test]
    fn events_publish_is_refused_before_a_declare_through_the_real_dispatch() {
        // The same refusal `cordial_plugins::host::Session` proves in its own
        // tests, checked here against the real host's `dispatch` and its
        // shared registry, not the test-only `Session` construct.
        let store = scratch_store("events-undeclared");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("events-undeclared");
        let req = call(
            "events.publish",
            serde_json::json!({"type": "flag-manager/profile-changed", "payload": {}}),
        );
        let res = dispatch("evil", &req, &store, &mut presence, &shared, &plugin_dir, &[]);
        match res {
            Response::Error { message, .. } => assert!(message.contains("may not publish"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_plugin_may_declare_then_publish_on_its_own_type_through_the_real_dispatch() {
        let store = scratch_store("events-declare-publish");
        let mut presence = DiscordPresence::new();
        let shared = Shared::new();
        let plugin_dir = scratch_plugin_dir("events-declare-publish");

        let declared = dispatch(
            "flag-manager",
            &call("events.declare", serde_json::json!({"name": "profile-changed"})),
            &store,
            &mut presence,
            &shared,
            &plugin_dir,
            &[],
        );
        let event_type = match declared {
            Response::Ok { result, .. } => result["type"].as_str().unwrap().to_string(),
            other => panic!("expected declare to succeed, got {other:?}"),
        };
        assert_eq!(event_type, "flag-manager/profile-changed");

        let published = dispatch(
            "flag-manager",
            &call("events.publish", serde_json::json!({"type": event_type, "payload": {"slot": 2}})),
            &store,
            &mut presence,
            &shared,
            &plugin_dir,
            &[],
        );
        // No subscriber is registered in `shared.writers` at all here, and
        // that must not be an error: publishing to nobody is exactly what a
        // plugin does before anything has subscribed yet.
        assert!(matches!(published, Response::Ok { .. }), "{published:?}");
    }

    /// The property the `Shared`/`Writer` refactor exists for, proven against
    /// a real Deno process and the exact `dispatch` a running Cordial calls —
    /// not `cordial_plugins::host::Session::handle`, which is still
    /// constructed nowhere outside that crate's own tests and still never
    /// serves a request inside the real client. (Its `Pump` now does: the
    /// core bus below reuses it rather than growing a second one. `Session`
    /// itself remains test-only.) If this regressed, `events.publish` would
    /// answer `Ok` while silently reaching nobody: exactly the "recorded but
    /// not enforced" shape this file's wiring exists to close.
    #[test]
    fn a_published_event_reaches_a_real_subscriber_through_the_shared_writer_map() {
        if std::process::Command::new("deno").arg("--version").output().is_err() {
            eprintln!("skipping: deno is not installed");
            return;
        }

        let shared = Shared::new();
        let store = scratch_store("events-cross-process");
        let plugin_dir = scratch_plugin_dir("events-cross-process");

        // The publisher is simulated from the Rust side — declaring and
        // publishing are pure `dispatch` calls with no process behind
        // them — the same choice `cordial-plugins`' own
        // `events_integration.rs` makes, and for the same reason: a second
        // Deno process that only ever declares and publishes would test
        // nothing this file does not already exercise by calling `dispatch`
        // directly.
        let mut publisher_presence = DiscordPresence::new();
        let declared = dispatch(
            "flag-manager",
            &call("events.declare", serde_json::json!({"name": "profile-changed"})),
            &store,
            &mut publisher_presence,
            &shared,
            &plugin_dir,
            &[],
        );
        let event_type = match declared {
            Response::Ok { result, .. } => result["type"].as_str().unwrap().to_string(),
            other => panic!("flag-manager should have been able to declare its own type, got {other:?}"),
        };

        // The subscriber has to be a real process: receiving a push over
        // stdio, from a thread that made no request for it, is the part that
        // cannot be faked without a genuine second pipe on the other end.
        // Reuses `cordial-plugins`' own fixture rather than a copy of it —
        // it declares nothing this crate does, only what a subscriber-only
        // plugin does.
        let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../cordial-plugins/tests/fixtures/events_subscriber.ts");
        let mut launcher = PluginProc::spawn("launcher", &entry).expect("deno should start");
        shared.writers.lock().unwrap().insert("launcher".to_string(), launcher.writer());

        let mut launcher_presence = DiscordPresence::new();
        let mut logs: Vec<String> = Vec::new();
        let mut published = false;
        while let Some(Ok(req)) = launcher.next_request() {
            if req.method == "log.write" {
                let message = req.params["message"].as_str().unwrap_or_default().to_string();
                launcher.reply(&Response::Ok { id: req.id, result: serde_json::Value::Null }).unwrap();
                logs.push(message);
                if logs.len() >= 2 {
                    break;
                }
                continue;
            }

            let res = dispatch("launcher", &req, &store, &mut launcher_presence, &shared, &plugin_dir, &[]);
            let subscribed_ok = req.method == "events.subscribe" && matches!(res, Response::Ok { .. });
            launcher.reply(&res).unwrap();

            if subscribed_ok && !published {
                published = true;
                // Now that the subscriber is actually registered, publish —
                // this is the call that should write a `Push` into
                // `launcher`'s stdin from a completely different thread's
                // point of view than the one reading it here.
                let pub_res = dispatch(
                    "flag-manager",
                    &call(
                        "events.publish",
                        serde_json::json!({"type": event_type, "payload": {"slot": 3}}),
                    ),
                    &store,
                    &mut publisher_presence,
                    &shared,
                    &plugin_dir,
                    &[],
                );
                assert!(matches!(pub_res, Response::Ok { .. }), "publish should succeed: {pub_res:?}");
            }
        }
        launcher.kill();

        assert!(published, "the test should have reached the point of publishing");
        let joined = logs.join("\n");
        assert!(joined.contains("subscribed: ok"), "got:\n{joined}");
        assert!(joined.contains("push: flag-manager/profile-changed"), "got:\n{joined}");
        assert!(joined.contains(r#""slot":3"#), "got:\n{joined}");
    }

    /// The process-global `SHARED` is one map for the whole test binary, and
    /// `cargo test` runs these on parallel threads. A second test registering
    /// a listener holding `lifecycle.read` while the first counts recipients
    /// would make `sent` two -- a failure that has nothing to do with what
    /// either test is about and would appear only sometimes. Every test that
    /// touches the global map takes this first.
    static GLOBAL_MAP: Mutex<()> = Mutex::new(());

    /// **The bug this change fixes, in one test.** ADR-026's core bus landed
    /// with `Session::publish_core` as its only producer, and `Session` is
    /// constructed nowhere outside `cordial-plugins`' own tests — so a plugin
    /// running under `cordial-run` called `lifecycle.subscribe`, was told
    /// `ok`, and waited forever for a `cordial/client.launch` nothing
    /// published. `plugins/discord-presence` is the shipped example that did
    /// exactly that.
    ///
    /// A real Deno process on the receiving end, because a push arriving over
    /// stdio from a thread that made no request for it is the part that
    /// cannot be faked. Reuses `cordial-plugins`' own subscriber fixture: it
    /// reports every push it receives, which is all this needs.
    ///
    /// **The control is the second plugin**, granted every capability except
    /// `lifecycle.read`. Without it this would assert only that publishing
    /// does something, not that the capability is what decides who hears it —
    /// and a bus that delivered to everybody would pass just as well.
    #[test]
    fn a_core_event_reaches_the_plugin_holding_its_capability_and_no_other() {
        if std::process::Command::new("deno").arg("--version").output().is_err() {
            eprintln!("skipping: deno is not installed");
            return;
        }

        let _serialised = GLOBAL_MAP.lock().unwrap_or_else(|e| e.into_inner());
        let store = scratch_store("core-events");
        let plugin_dir = scratch_plugin_dir("core-events");
        let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../cordial-plugins/tests/fixtures/events_subscriber.ts");

        let mut listener = PluginProc::spawn("core-listener", &entry).expect("deno should start");
        let mut deaf = PluginProc::spawn("core-deaf", &entry).expect("deno should start");

        // Everything except lifecycle.read for the control, so the only
        // difference between the two plugins is the one capability under
        // test. Granting it nothing at all would leave "it was ignored
        // because it holds nothing" as an alternative explanation.
        let hearing: BTreeSet<Capability> = [Capability::LifecycleRead, Capability::Log].into_iter().collect();
        let deafened: BTreeSet<Capability> = Capability::all()
            .iter()
            .copied()
            .filter(|c| *c != Capability::LifecycleRead)
            .collect();

        register_for_test("core-listener", &listener, hearing);
        register_for_test("core-deaf", &deaf, deafened);

        // The client's own call, by name, exactly as `load.rs` makes it.
        let delivered = publish_core(
            cordial_plugins::core_events::CLIENT_LAUNCH,
            serde_json::json!({"profile": "default"}),
        );
        assert_eq!(
            delivered,
            Delivered { sent: 1, dropped: 0 },
            "exactly the plugin holding lifecycle.read should have been offered it"
        );
        assert!(
            flush_core_events(std::time::Duration::from_secs(5)).is_empty(),
            "the queue should have drained well inside five seconds"
        );

        // Written after the flush, so it cannot overtake the core event on
        // the way down either pipe — which is what lets the control below
        // assert an absence rather than merely a timeout. It also guarantees
        // both processes say *something*, so a regression fails this test
        // rather than hanging it on a `next_request` that never returns.
        let sentinel = Push { event: "test/sentinel".into(), payload: serde_json::Value::Null };
        for id in ["core-listener", "core-deaf"] {
            let writers = shared().writers.lock().unwrap_or_else(|e| e.into_inner());
            writers.get(id).expect("registered above").push(&sentinel).unwrap();
        }

        let heard = drive_until_sentinel(&mut listener, "core-listener", &store, &plugin_dir);
        let ignored = drive_until_sentinel(&mut deaf, "core-deaf", &store, &plugin_dir);

        listener.kill();
        deaf.kill();
        let mut listeners = shared().listeners.lock().unwrap_or_else(|e| e.into_inner());
        let mut writers = shared().writers.lock().unwrap_or_else(|e| e.into_inner());
        for id in ["core-listener", "core-deaf"] {
            listeners.remove(id);
            writers.remove(id);
        }
        drop(listeners);
        drop(writers);

        let heard = heard.join("\n");
        assert!(heard.contains("push: cordial/client.launch"), "got:\n{heard}");
        assert!(heard.contains(r#""profile":"default""#), "got:\n{heard}");

        let ignored = ignored.join("\n");
        assert!(
            !ignored.contains("cordial/client.launch"),
            "a plugin without lifecycle.read must hear nothing, got:\n{ignored}"
        );
        assert!(ignored.contains("push: test/sentinel"), "the control should still be alive, got:\n{ignored}");
    }

    /// The direction `core_events.rs` asserts, held to at this end too: an
    /// event with no entry in the closed table requires a capability nobody
    /// holds, so it reaches nobody — rather than falling through to a prefix
    /// check that happens to pass. Needs no plugin at all, which is the
    /// point: the refusal happens before anything is looked up.
    #[test]
    fn a_core_event_missing_from_the_capability_table_reaches_nobody() {
        assert_eq!(
            publish_core("network.connected", serde_json::json!({"host": "example"})),
            Delivered::default()
        );
    }

    /// **The property this change calls its most important, measured on the
    /// path the client actually calls.**
    ///
    /// `cordial-plugins` has a test of the same shape, and it drives
    /// `Session::publish_core` -- the host nobody runs. ADR-026's one measured
    /// number comes from there too. That is the exact shape AGENTS.md opens
    /// with and the shape this whole change exists to correct, so leaving the
    /// boundedness claim verified only against the helper would have repeated
    /// it one level up: a regression that made `plugin_host::publish_core`
    /// block -- swapping the `Pump` for the direct `Writer`, or waiting on the
    /// listeners lock while somebody else holds it -- would leave the two
    /// tests above green, because a live reader with an empty queue never
    /// fills anything.
    ///
    /// **This one does wedge a plugin**, which the cordial-plugins version
    /// says at length it could not: `deaf_plugin.ts` is alive, holds the pipe
    /// open and never reads a byte, so the pipe fills, the pump blocks on a
    /// write that will never return, and the queue stays full for as long as
    /// the test cares to look. Against the reading fixture the same publish
    /// drained in 35 ms and demonstrated nothing about a reader that stops.
    #[test]
    fn publishing_a_core_event_is_bounded_time_however_far_behind_the_plugin_is() {
        if std::process::Command::new("deno").arg("--version").output().is_err() {
            eprintln!("skipping: deno is not installed");
            return;
        }
        let _serialised = GLOBAL_MAP.lock().unwrap_or_else(|e| e.into_inner());

        let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../cordial-plugins/tests/fixtures/deaf_plugin.ts");
        let mut behind = PluginProc::spawn("core-behind", &entry).expect("deno should start");
        register_for_test(
            "core-behind",
            &behind,
            [Capability::LifecycleRead, Capability::Log].into_iter().collect(),
        );

        // Comfortably more than QUEUE_DEPTH and more than a pipe will take,
        // with a payload big enough that it cannot all be buffered.
        let big = serde_json::json!({ "pad": "x".repeat(4096) });
        let started = std::time::Instant::now();
        let mut dropped = 0usize;
        for _ in 0..4000 {
            dropped += publish_core(
                cordial_plugins::core_events::CLIENT_LAUNCH,
                big.clone(),
            )
            .dropped;
        }
        let took = started.elapsed();

        let reported = undelivered_core_events();
        behind.kill();
        shared().listeners.lock().unwrap_or_else(|e| e.into_inner()).remove("core-behind");
        shared().writers.lock().unwrap_or_else(|e| e.into_inner()).remove("core-behind");

        assert!(
            took < std::time::Duration::from_secs(10),
            "publishing 4000 core events took {took:?}; the client's cost is tracking \
             the plugin's speed, which is the one property ADR-026 says this bus must have"
        );
        assert!(dropped > 0, "a plugin this far behind should have missed something");
        // And the loss is counted rather than silent, by name, which is what
        // the shutdown report reads.
        assert_eq!(
            reported.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            vec!["core-behind"],
            "the drop should be attributed to the plugin that could not keep up"
        );
        eprintln!("published 4000 in {took:?}, {dropped} dropped");
    }

    /// **The shutdown budget is the whole budget, not each plugin's share.**
    ///
    /// `Pump::flush` takes a fresh deadline per call, so the obvious loop over
    /// it costs `limit` times however many plugins are running -- a number the
    /// user chooses by installing plugins -- while `load.rs` prints a line
    /// promising 500 ms. That is a bound the code could not keep, and this is
    /// the test that would notice it coming back: two plugins that cannot take
    /// what they were sent, one budget, and an elapsed time closer to one
    /// budget than to two.
    ///
    /// The margin is wide on purpose. What it has to separate is 400 ms from
    /// 800 ms, so a slow machine has to be twice as slow before this becomes
    /// a false failure, and the sleep inside `Pump::flush` is 2 ms.
    #[test]
    fn the_shutdown_flush_is_bounded_once_rather_than_once_per_plugin() {
        if std::process::Command::new("deno").arg("--version").output().is_err() {
            eprintln!("skipping: deno is not installed");
            return;
        }
        let _serialised = GLOBAL_MAP.lock().unwrap_or_else(|e| e.into_inner());

        let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../cordial-plugins/tests/fixtures/deaf_plugin.ts");
        let hearing: BTreeSet<Capability> =
            [Capability::LifecycleRead, Capability::Log].into_iter().collect();
        let mut procs: Vec<PluginProc> = Vec::new();
        for id in ["core-stuck-a", "core-stuck-b"] {
            let proc = PluginProc::spawn(id, &entry).expect("deno should start");
            register_for_test(id, &proc, hearing.clone());
            procs.push(proc);
        }

        // Enough to fill both pipes and both queues, so each has something
        // outstanding when the flush runs. Against `events_subscriber.ts` this
        // drained in 35 ms and the test proved nothing: it reads as fast as the
        // pump writes. `deaf_plugin.ts` never reads at all, which is what puts
        // and keeps a queue in the state being measured.
        let big = serde_json::json!({ "pad": "x".repeat(4096) });
        for _ in 0..1000 {
            publish_core(cordial_plugins::core_events::CLIENT_LAUNCH, big.clone());
        }

        let budget = std::time::Duration::from_millis(400);
        let started = std::time::Instant::now();
        let stuck = flush_core_events(budget);
        let took = started.elapsed();

        for mut proc in procs {
            proc.kill();
        }
        let mut listeners = shared().listeners.lock().unwrap_or_else(|e| e.into_inner());
        let mut writers = shared().writers.lock().unwrap_or_else(|e| e.into_inner());
        for id in ["core-stuck-a", "core-stuck-b"] {
            listeners.remove(id);
            writers.remove(id);
        }
        drop(listeners);
        drop(writers);

        assert_eq!(stuck, vec!["core-stuck-a", "core-stuck-b"], "both should be named, and by name");
        assert!(
            took < std::time::Duration::from_millis(600),
            "the flush took {took:?} against a 400 ms budget for two plugins; a deadline \
             per plugin rather than one shared is what that looks like"
        );
    }

    /// Register a spawned fixture on both shared maps the way `start_all`
    /// does, so the test exercises the real `publish_core` rather than a
    /// stand-in for it.
    fn register_for_test(id: &str, proc: &PluginProc, granted: BTreeSet<Capability>) {
        shared()
            .writers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), proc.writer());
        shared().listeners.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id.to_string(),
            Listener { pump: Arc::new(Pump::start(proc.writer())), granted },
        );
    }

    /// Serve `proc` until it reports the sentinel push, returning every log
    /// line it produced. Terminates whether or not the core event arrived,
    /// which is what keeps a regression a failure rather than a hang.
    fn drive_until_sentinel(
        proc: &mut PluginProc,
        id: &str,
        store: &Store,
        plugin_dir: &Path,
    ) -> Vec<String> {
        let mut presence = DiscordPresence::new();
        let mut logs: Vec<String> = Vec::new();
        while let Some(Ok(req)) = proc.next_request() {
            if req.method == "log.write" {
                let message = req.params["message"].as_str().unwrap_or_default().to_string();
                let done = message.contains("test/sentinel");
                proc.reply(&Response::Ok { id: req.id, result: serde_json::Value::Null }).unwrap();
                logs.push(message);
                if done {
                    break;
                }
                continue;
            }
            let res = dispatch(id, &req, store, &mut presence, shared(), plugin_dir, &[]);
            if proc.reply(&res).is_err() {
                break;
            }
        }
        logs
    }

    #[test]
    fn resolve_within_refuses_a_path_that_would_escape_the_base() {
        let base = std::env::temp_dir().join("cordial-plugin-host-resolve-within-test");
        for bad in ["..", "../elsewhere", "/etc/passwd", "a/../../b"] {
            assert!(resolve_within(&base, bad).is_err(), "{bad:?} should have been refused");
        }
        assert_eq!(resolve_within(&base, "overlay").unwrap(), base.join("overlay"));
        assert_eq!(resolve_within(&base, "a/b").unwrap(), base.join("a/b"));
    }

    // The bug this file exists to fix: `start_all` discovered every plugin
    // with a nonempty grant and never once asked `enablement::is_enabled`,
    // so Settings' switch wrote `plugin-enabled.json` and nothing read it
    // back. These exercise the same decision `start_all` now makes —
    // `enabled_in_profile`, called before a plugin's grants are even looked
    // at — on a scratch profile directory, rather than against real
    // discovered plugins and spawned processes, which is what `start_all`
    // itself talks to and is not something a unit test should stand up.

    fn scratch_profile(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cordial-plugin-host-enablement-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_plugin_explicitly_disabled_does_not_start() {
        let dir = scratch_profile("disabled");
        enablement::set_enabled(&dir, "flag-inspector", false).unwrap();
        assert!(
            !enabled_in_profile(&dir, "flag-inspector"),
            "Settings turned this off; start_all must not spawn it"
        );
    }

    #[test]
    fn a_plugin_explicitly_enabled_does_start() {
        let dir = scratch_profile("enabled");
        // Written explicitly rather than left absent, so this test is
        // distinct from the absence case below: this covers the entry
        // reading `true`, not merely "nobody wrote anything".
        enablement::set_enabled(&dir, "flag-inspector", false).unwrap();
        enablement::set_enabled(&dir, "flag-inspector", true).unwrap();
        assert!(enabled_in_profile(&dir, "flag-inspector"));
    }

    #[test]
    fn a_plugin_absent_from_the_file_defaults_to_enabled() {
        // The design question this change had to answer: an installed
        // plugin the user has never touched must not be silently disabled
        // by wiring `start_all` up to enablement. `enablement.rs`'s own
        // contract is "absence means enabled" (see its module docs and
        // `is_enabled`); this asserts that `start_all`'s call site actually
        // gets that answer, rather than assuming the wrapper forwards it
        // correctly.
        let dir = scratch_profile("absent");
        assert!(!std::path::Path::new(&enablement::path_in(&dir)).exists());
        assert!(enabled_in_profile(&dir, "a-plugin-nobody-has-an-opinion-about"));
    }

    #[test]
    fn a_grant_written_after_the_broker_was_built_still_reaches_it() {
        // The bug: `start_all` used to read the grants file exactly once and
        // never again, so a capability switched on in Settings while a
        // plugin was already running had no effect until the next launch.
        // This pins `refresh_grant`, the fix, against exactly that sequence:
        // build a broker with nothing granted, write a grant afterwards, and
        // check the broker sees it without being told to.
        let dir = scratch_profile("live-grant");
        let path = grants::path_in(&dir);
        let mut broker = Broker::new();
        let shared = Shared::new();
        let mut last_seen = None;
        refresh_grant(&path, "discord-presence", &mut broker, &shared, &mut last_seen);
        assert!(!broker.allows("discord-presence", Capability::PresenceSet));

        // Sleep past filesystem mtime resolution before writing, or a grant
        // fast enough to land in the same tick as the first read could keep
        // the same `mtime` and be missed by the very check this test exists
        // to prove works.
        std::thread::sleep(std::time::Duration::from_millis(1050));
        grants::set(&path, "discord-presence", Capability::PresenceSet, true).unwrap();

        refresh_grant(&path, "discord-presence", &mut broker, &shared, &mut last_seen);
        assert!(
            broker.allows("discord-presence", Capability::PresenceSet),
            "a grant written after the broker existed must still reach it"
        );
    }

    // `refresh_grant` also updates `shared`'s `Listener` for this plugin, so
    // that a live grant reaches `flush_core_events`'s gate and not only
    // `broker`'s -- see the function's own doc comment for why the second
    // snapshot matters as much as the first. That half is exercised by
    // review rather than by a test here: constructing a `Listener` needs a
    // real `Writer`, which this crate can only obtain from a spawned Deno
    // process (`PluginProc::spawn`), and every test in this file that pays
    // that cost already does so to test something Deno-shaped -- adding one
    // solely to observe a `BTreeSet` field copy would be the heaviest
    // possible test for the smallest possible claim. The two tests below
    // cover the `broker` half, which shares the same `modified != last_seen`
    // gate and the same `grants::load` call as the `Listener` half.
    #[test]
    fn refresh_grant_does_nothing_when_the_file_has_not_changed() {
        // The other half of the same fix: this runs on every request a
        // plugin makes, so it must not re-read and re-grant when nothing
        // changed — that would be a stat and a parse on every call for no
        // reason, and `Broker::grant` replaces rather than accumulates, so a
        // spurious regrant would at least be harmless but is still work this
        // function exists to avoid doing needlessly.
        let dir = scratch_profile("live-grant-unchanged");
        let path = grants::path_in(&dir);
        grants::set(&path, "p", Capability::Log, true).unwrap();
        let mut broker = Broker::new();
        let shared = Shared::new();
        let mut last_seen = None;
        refresh_grant(&path, "p", &mut broker, &shared, &mut last_seen);
        assert!(broker.allows("p", Capability::Log));
        let seen_after_first = last_seen;

        refresh_grant(&path, "p", &mut broker, &shared, &mut last_seen);
        assert_eq!(last_seen, seen_after_first, "no write happened, so the recorded mtime must not move");
    }

    #[test]
    fn a_denial_is_recorded_once_even_if_the_call_is_retried() {
        // `denials::record` is idempotent on disk, but this pins the other
        // half: the serving loop's own `denied_already` set must stop it
        // from opening and rewriting the file for a plugin that keeps
        // asking for something it does not have, which is exactly what a
        // plugin retrying a refused call would do.
        let dir = scratch_profile("denial-once");
        let path = denials::path_in(&dir);
        let mut denied_already: BTreeSet<Capability> = BTreeSet::new();

        for _ in 0..3 {
            if denied_already.insert(Capability::PresenceSet) {
                denials::record(&path, "discord-presence", Capability::PresenceSet).unwrap();
            }
        }
        assert_eq!(denials::load(&path)["discord-presence"].len(), 1);
    }

    #[test]
    fn a_log_line_is_appended_for_a_terminal_that_is_not_there_to_read_it() {
        // `println!` is invisible on a packaged or Flatpak launch with no
        // terminal attached — this pins the file that exists so a plugin's
        // own report of what it did survives that launch too.
        let dir = scratch_profile("plugin-log");
        append_plugin_log(&dir, "discord-presence", "presence.set on launch came back: ok");
        append_plugin_log(&dir, "discord-presence", "presence.set on ready came back: ok");
        let text = std::fs::read_to_string(dir.join("plugin.log")).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("[discord-presence] presence.set on launch came back: ok"));
    }
}
