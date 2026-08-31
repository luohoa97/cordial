//! The facts Cordial observes at the platform boundary, and who may hear them.
//!
//! **Cordial knows what the engine is doing without hooking it, and that is
//! not a trick -- it is the position.** Roblox's build runs on a bionic linker
//! this project ported, against a libc shim it wrote, on a JNI VM standing in
//! for ART, talking to a framework layer that answers every call the client
//! makes into the platform. The looper it polls is ours. The surface it draws
//! to is ours. The sockets, the paths, the audio streams and the input events
//! all cross a boundary this codebase owns. Nothing has to be patched, traced
//! or intercepted to know any of it, because it was already coming through
//! here on its way in.
//!
//! That is what makes this bus possible under
//! [ADR-001](../../../docs/adr/ADR-001-in-process-hooking.md), which forbids
//! in-process code execution against the Roblox process permanently and in the
//! strongest terms. An event here is not a hook; it is Cordial reporting what
//! it was asked to do.
//!
//! ## Events are observations. They are never cancellable
//!
//! This is the line, and it is the one place this design deliberately parts
//! company with the modding buses it otherwise resembles. NeoForge's events are
//! cancellable because mods exist to change the game. A plugin here cannot
//! change the game, and a cancellable platform event would be exactly the
//! forbidden thing wearing a friendlier name: a plugin returning "handled" from
//! a socket event is a plugin that can cut the engine's network; one vetoing a
//! path event can deny it a file. That is in-process control of the client,
//! reached through a callback rather than a patched instruction, and the
//! distinction would not survive contact with anybody who wanted to abuse it.
//!
//! So delivery is one-way and the return value is nothing. A plugin that wants
//! an effect asks for one through a capability Cordial performs -- ADR-007's
//! rule, unchanged.
//!
//! **Cordial's own decisions are a different matter** and are not covered here.
//! Whether *Cordial* shows a toast, or opens a URL in its own web view, is not
//! the engine's behaviour and could sensibly be influenced one day. If that is
//! ever built it wants its own name and its own ADR, precisely so nobody
//! reaches for it as a way to make platform events vetoable after all.
//!
//! ## Delivery is lossy, and the loss is counted
//!
//! A push is a blocking write into a plugin's stdin. A plugin that stops
//! reading fills the pipe -- 64 KiB on Linux -- and then **whoever published
//! blocks**, which for a platform event is a thread the client is waiting on.
//! The engine's looper is measured in millions of polls a second; it cannot
//! wait behind a wedged plugin, and a bus that let it would be a worse bug than
//! anything it was built to observe.
//!
//! So each plugin has a bounded queue and its own writer, and a publish that
//! finds the queue full **drops the event and counts it**. Dropping is the
//! honest outcome for an observation -- there is no correct way to make the
//! client wait for a plugin to catch up -- and the count is what stops it being
//! a silent one. `native/opensles.cpp` reports failure rather than handing back
//! a dead engine object for the same reason: a gap that reports itself stays
//! findable.
//!
//! Requests and their responses do **not** go through here and are not lossy.
//! An answer nobody receives is a plugin hung waiting for it.
//!
//! ## Names are closed, and the prefix cannot be forged
//!
//! Every event below is a `&'static str` from the table in this file, so there
//! is no path by which a name is assembled from anything a plugin said. They
//! all sit under `cordial/`, which [`crate::events`] reserves as
//! [`CORE_OWNER`](crate::events::CORE_OWNER) and refuses to let any plugin
//! declare under -- so a plugin cannot mint a convincing `cordial/…` event and
//! no subscriber has to wonder whether the one it received was real.

use crate::capability::Capability;

/// One thing Cordial observed.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreEvent {
    /// A name from [`ALL`]. Never constructed from plugin input.
    pub name: &'static str,
    pub payload: serde_json::Value,
}

impl CoreEvent {
    pub fn new(name: &'static str, payload: serde_json::Value) -> Self {
        CoreEvent { name, payload }
    }

    /// The wire name, always namespaced.
    pub fn wire_name(&self) -> String {
        format!("{}/{}", crate::events::CORE_OWNER, self.name)
    }
}

/// The client was asked to start. Published by `load.rs` once the plugins are
/// running, which is the first moment there is anybody to tell.
pub const CLIENT_LAUNCH: &str = "client.launch";
/// The engine reported itself ready.
///
/// **Declared, and published by nothing.** A plugin holding the capability
/// will never receive it, and `plugins/discord-presence` has a branch on it
/// that cannot be reached. Said here rather than left for somebody to find,
/// because a name in this table reads as a promise: the honest signal is the
/// engine's own `APP_READY` notification or its first present, and neither has
/// a publisher yet. Whoever adds one deletes this paragraph and the note in
/// `Capability::LifecycleRead`'s arm at the same time.
pub const CLIENT_READY: &str = "client.ready";
/// The client stopped, however it stopped. Published by `load.rs` on the way
/// out, and waited for -- see `plugin_host::flush_core_events`.
pub const CLIENT_SHUTDOWN: &str = "client.shutdown";
/// The engine's version, once it is known. Published by `load.rs` beside
/// `CLIENT_LAUNCH`.
pub const ENGINE_VERSION: &str = "engine.version";
/// The window Roblox draws into changed size.
///
/// **Declared, and published by nothing**, for the same reason as
/// `CLIENT_READY` and one of its own: the only place that knows is
/// `android::window::dispatch_configure`, which the compositor drives and
/// which runs on every actual size change -- a stream of them through an
/// interactive drag. A publish there is bounded and would not stall it, but
/// nobody has measured what a plugin receiving a drag's worth of these does,
/// so it is left unwired rather than wired on the assumption.
pub const WINDOW_RESIZED: &str = "window.resized";

/// A running experience set its own Rich Presence, through BloxstrapRPC.
///
/// **The game is the author of this, not Cordial.** An experience calls Lua's
/// `print` with a `[BloxstrapRPC]` marker; the engine writes it to its own log;
/// `cordial_runtime::game_log` reads the log and
/// `cordial_runtime::bloxstrap_rpc` parses it. Nothing is injected and nothing
/// is hooked -- Cordial is reading a file it created the directory for.
///
/// Gated on `PresenceSet` rather than `LifecycleRead`, which is the point of
/// the table being per family. A plugin granted `lifecycle.read` to know when
/// the client started must not thereby learn what the player is doing inside
/// an experience; a plugin that is already allowed to publish a presence
/// learns nothing new by being told what to publish.
///
/// The payload is the merged presence as the game has built it up so far --
/// BloxstrapRPC is a stream of partial updates, and folding them is
/// `bloxstrap_rpc::Presence`'s job, not every subscriber's.
pub const GAME_PRESENCE: &str = "game.presence";

/// Every core event, with the capability that gates it.
///
/// **A closed table rather than a prefix convention**, for the reason
/// [`crate::protocol::required_capability`] gives about methods: a typo has to
/// fail as unknown rather than fall through to a check that happens to pass.
/// A new event with no entry here is delivered to nobody, which is the safe
/// direction and is asserted by a test.
pub const ALL: &[(&str, Capability)] = &[
    (CLIENT_LAUNCH, Capability::LifecycleRead),
    (CLIENT_READY, Capability::LifecycleRead),
    (CLIENT_SHUTDOWN, Capability::LifecycleRead),
    (ENGINE_VERSION, Capability::LifecycleRead),
    (WINDOW_RESIZED, Capability::LifecycleRead),
    (GAME_PRESENCE, Capability::PresenceSet),
];

/// Which capability a plugin needs to hear `name`, or `None` if unknown.
///
/// **The capability is per event family, not one grant for the whole bus.**
/// That matters more as the table grows: the events worth adding next are the
/// ones Cordial is uniquely placed to see -- which paths the engine opened,
/// which addresses it connected to, what it typed into -- and those are exactly
/// the ones nobody should receive because they were once granted
/// `lifecycle.read` to show a Discord status. A family that needs a new
/// permission gets one.
pub fn capability_for(name: &str) -> Option<Capability> {
    ALL.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_is_namespaced_under_the_reserved_owner() {
        for (name, _) in ALL {
            let e = CoreEvent::new(name, serde_json::Value::Null);
            assert!(e.wire_name().starts_with("cordial/"), "{}", e.wire_name());
        }
    }

    /// **An event with no entry in the table reaches nobody.**
    ///
    /// The failure this prevents is the quiet one: somebody adds an event,
    /// forgets the table, and it is delivered under whatever check the code
    /// happened to fall through to. `None` here means "no capability admits
    /// this", and `publish` sends it to no one.
    #[test]
    fn an_event_that_is_not_in_the_table_is_gated_by_nothing_and_so_goes_nowhere() {
        assert_eq!(capability_for("client.launch"), Some(Capability::LifecycleRead));
        assert_eq!(capability_for("network.connected"), None);
        assert_eq!(capability_for(""), None);
    }

    #[test]
    fn the_table_has_no_duplicate_names() {
        let mut names: Vec<&str> = ALL.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "two entries share a name");
    }
}
