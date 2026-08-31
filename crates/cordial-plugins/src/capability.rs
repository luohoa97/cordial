//! What a plugin is allowed to do, and nothing else.
//!
//! Capabilities are named, granted per plugin, and checked at the point of use.
//! The list is closed: there is no capability that means "anything", and
//! [ADR-003](../../../docs/adr/ADR-003-plugin-isolation.md) is explicit that a
//! capability handing over the machine is not a capability but the absence of
//! one. That is why there is no `process.spawn`, no filesystem path, and no
//! memory access here.
//!
//! Adding a variant is a design decision, not a convenience. If a plugin needs
//! something, the question is what *narrow, named* effect it needs — not what
//! access would let it arrange the effect itself.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Read the resolved FastFlag set, including which layer set each value.
    FlagsRead,
    /// Contribute flags. Startup flags land in the plugin's own `flags.json` and
    /// take effect at the next launch; see ADR-005 for why that is not the same
    /// surface as changing one live.
    FlagsWrite,
    /// Change a `DFFlag`/`DFInt`/`DFString` while the client runs. Deliberately
    /// distinct from `FlagsWrite`: the static families cannot be changed live at
    /// all, and an API that accepted them here would silently do nothing.
    FlagsWriteDynamic,
    /// Emit log lines into Cordial's own output.
    Log,
    /// Observe client lifecycle events — launch, ready, shutdown.
    LifecycleRead,
    /// Read what the client is doing: which place and server, and who is
    /// playing.
    ///
    /// **Separate from `LifecycleRead`, and the difference is the point.**
    /// Knowing the client started is nearly nothing; knowing which experience
    /// somebody is in, on which server, as which user id, is a picture of what
    /// they are doing with their evening. A plugin granted lifecycle to show a
    /// Discord status must not acquire the second by having asked for the
    /// first, which is the argument `core_events` already makes for keeping
    /// the table per family.
    ///
    /// Reading only, and only what the engine's own log already said. See
    /// [`crate::state::SessionState`] for what is deliberately not in it.
    StateRead,
    /// Publish Discord Rich Presence.
    ///
    /// The effect, not the channel. Cordial owns the connection to Discord's
    /// IPC socket and the plugin sends a presence payload; it never learns the
    /// socket's location, cannot read Discord's state, and cannot send arbitrary
    /// frames. See ADR-007 — a plugin can never hold a host resource, because a
    /// Flatpak permission is app-wide and permanent while a capability is
    /// per-plugin and revocable, and the two cannot be made to mean the same
    /// thing.
    ///
    /// Off unless granted, and privacy-relevant: what someone is playing and
    /// when is not always something they want broadcast.
    PresenceSet,
    /// Post a desktop notification through the freedesktop portal.
    ///
    /// Brokered for the same reason as `PresenceSet`: the plugin sends a summary
    /// and a body, Cordial owns the D-Bus connection. A plugin that held the bus
    /// could talk to every other service on it.
    NotifySend,
    /// Open a URL in the user's browser, through the portal.
    ///
    /// The narrowest useful form of "leave the application". Cordial validates
    /// the scheme before handing it to the portal — `http` and `https` only, so
    /// this cannot become `file://` traversal or a handler-hijack for some
    /// arbitrary registered scheme.
    UrlOpen,
    /// Register a directory of files that resolve before Roblox's own assets
    /// of the same name — see
    /// [ADR-010](../../../docs/adr/ADR-010-plugin-asset-overlays.md).
    ///
    /// Narrow on purpose: this is one filesystem root the plugin owns, checked
    /// ahead of the APK for a name match, not a general filesystem capability.
    /// It cannot write into the APK or into anything Cordial extracts from it
    /// — both stay untouched — and it cannot read anything outside the root it
    /// registers. Uninstalling the plugin (or it giving up the root) makes the
    /// original asset resolve again with nothing to clean up, because nothing
    /// was ever overwritten to begin with.
    AssetsOverride,
    /// Read the plugin's own settings document.
    ///
    /// A plugin has nowhere of its own to keep anything — it runs with no file
    /// access at all — so before this existed it could not remember a single
    /// thing between launches, and the only way to give it one would have been
    /// a path or a descriptor. ADR-007 rules both out, so Cordial holds the
    /// file and the plugin exchanges a document: the effect, never the channel,
    /// exactly as `PresenceSet` owns Discord's socket.
    ///
    /// Scoped to the plugin's own id, which Cordial takes from its record of
    /// which process is on the other end of the pipe rather than from the
    /// request. A field a plugin can set is a field it can set to somebody
    /// else's name, which is why the event registry does not accept one either.
    SettingsRead,
    /// Replace the plugin's own settings document.
    ///
    /// Split from `SettingsRead` for the reason `EventsDeclare` is split from
    /// `EventsPublish`: a plugin that only reads its configuration should not
    /// have to be trusted to rewrite it. A user approving "remember which
    /// panel I had open" has not thereby approved "discard everything I set".
    SettingsWrite,
    /// Register event types under the plugin's own namespace. See ADR-006.
    ///
    /// Separate from `EventsPublish` on purpose: declaring is what makes a
    /// type's origin a fact the registry can check rather than a claim a
    /// plugin makes about itself, and that check is only worth anything if a
    /// plugin cannot skip straight to publishing.
    EventsDeclare,
    /// Broadcast on an event type the plugin declared with `EventsDeclare`.
    ///
    /// Deliberately distinct from declaring: a plugin that could publish on
    /// any string it liked could impersonate another plugin's events, and a
    /// subscriber would have no way to tell. This capability only ever lets a
    /// plugin speak inside a namespace the registry has already attributed to
    /// it.
    EventsPublish,
    /// Receive events, including ones other plugins declared.
    ///
    /// Broader than `EventsPublish` deliberately — hearing something happened
    /// is a different power from being believed when you say it did, and a
    /// plugin that only reacts should not have to be trusted to speak.
    EventsSubscribe,
}

impl Capability {
    /// The wire name, which is what appears in a manifest.
    pub fn name(self) -> &'static str {
        match self {
            Capability::FlagsRead => "flags.read",
            Capability::FlagsWrite => "flags.write",
            Capability::FlagsWriteDynamic => "flags.write.dynamic",
            Capability::Log => "log",
            Capability::LifecycleRead => "lifecycle.read",
            Capability::StateRead => "state.read",
            Capability::PresenceSet => "presence.set",
            Capability::NotifySend => "notify.send",
            Capability::UrlOpen => "url.open",
            Capability::AssetsOverride => "assets.override",
            Capability::SettingsRead => "settings.read",
            Capability::SettingsWrite => "settings.write",
            Capability::EventsDeclare => "events.declare",
            Capability::EventsPublish => "events.publish",
            Capability::EventsSubscribe => "events.subscribe",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "flags.read" => Capability::FlagsRead,
            "flags.write" => Capability::FlagsWrite,
            "flags.write.dynamic" => Capability::FlagsWriteDynamic,
            "log" => Capability::Log,
            "lifecycle.read" => Capability::LifecycleRead,
            "state.read" => Capability::StateRead,
            "presence.set" => Capability::PresenceSet,
            "notify.send" => Capability::NotifySend,
            "url.open" => Capability::UrlOpen,
            "assets.override" => Capability::AssetsOverride,
            "settings.read" => Capability::SettingsRead,
            "settings.write" => Capability::SettingsWrite,
            "events.declare" => Capability::EventsDeclare,
            "events.publish" => Capability::EventsPublish,
            "events.subscribe" => Capability::EventsSubscribe,
            _ => return None,
        })
    }

    /// What granting this actually lets the plugin do, in the second person,
    /// with the honest edge of it included and its limit stated.
    ///
    /// **The wording is the whole job.** A prompt saying "this plugin wants to
    /// run code, allow?" is worse than no prompt: it appears for everything,
    /// it is answered yes by everybody, and it trains the user to dismiss the
    /// one that mattered. ADR-007 already supplies the better vocabulary —
    /// capabilities are named *effects*, and an effect is something a person
    /// can judge. These sentences are that vocabulary spelled out.
    ///
    /// They live beside the enum rather than in the shell so a variant cannot
    /// be added without one; the test below is what enforces it, the same way
    /// `name`/`parse`/`all` are already held together by a test rather than by
    /// remembering.
    ///
    /// The rule each sentence follows: say the effect, then the part a
    /// reasonable person would be annoyed to discover later. `flags.write` is
    /// the case that makes the rule necessary — see its arm.
    pub fn consequence(self) -> &'static str {
        match self {
            Capability::FlagsRead => {
                "See which Roblox and Cordial settings are in effect, and which layer set each one. \
                 Reading only."
            }
            // ADR-020 records what this actually is, and the wording follows
            // from the record rather than from the capability's name. It is
            // not "contribute FastFlags": `graphics.rs::plugin_request` reads
            // the plugin layers deliberately, so any `Cordial`-prefixed key is
            // in reach — `CordialGraphicsBackend` and `CordialPresentMode`
            // included, and every future one inherits it. "Change some Roblox
            // settings" would be technically true and materially misleading,
            // which is the standard `webview_policy.rs` already holds a URL to.
            Capability::FlagsWrite => {
                "Change how Cordial itself renders and behaves. Sets Roblox FastFlags, and also \
                 Cordial's own settings including the graphics backend and present mode. Takes \
                 effect at the next launch. Your own choices in Settings still win."
            }
            Capability::FlagsWriteDynamic => {
                "Change a dynamic Roblox setting while you are playing, without a restart. Static \
                 settings are read once at startup and cannot be changed this way."
            }
            Capability::Log => "Write lines into Cordial's own log output. Nothing leaves this machine.",
            // Every word of this arm is what the *payloads* carry, not what
            // the event names suggest, and it had drifted from them the day
            // the client started publishing. It said "launches, becomes ready
            // and shuts down": three timings and a scope disclaimer, for a
            // grant that also hands over the running profile's name and the
            // Roblox build string. Profile names are user-chosen and
            // routinely name the account, so somebody reading this to decide
            // what a plugin learns about them was getting the wrong answer
            // from the only place they are shown one -- ADR-007's rule that a
            // privacy-relevant capability's UI states what it publishes.
            // "Becomes ready" went for a plainer reason: nothing publishes
            // `client.ready`, and a permission prompt must not promise an
            // event Cordial never sends.
            Capability::LifecycleRead => {
                "Know when the client starts and stops, which of your profiles is running, and \
                 which Roblox build it loaded. Not what you play."
            }
            // The sentence above ends "Not what you play", and this is the one
            // that is. Said in those terms because the honest summary of this
            // capability is not "read session state" -- it is that the plugin
            // can see your evening.
            Capability::StateRead => {
                "See what you are playing: which experience, which server, and your Roblox user \
                 id. Reading only, and only while the client is running."
            }
            // ADR-007 calls this out as privacy-relevant and says the UI
            // should state what it publishes rather than merely that it is on.
            Capability::PresenceSet => {
                "Publish what you are playing to Discord, where your friends can see it. Cordial \
                 owns the connection; the plugin decides what it says."
            }
            Capability::NotifySend => "Show you desktop notifications.",
            Capability::UrlOpen => {
                "Open a web page in your browser. Only http and https addresses, and only when the \
                 plugin asks — it cannot browse on its own."
            }
            // ADR-010's "what is still refused" section earned this second
            // sentence. Replacing a collision or hitbox mesh with a smaller or
            // absent one is a substantive advantage rather than a cosmetic
            // change, Cordial builds no detection for it, and the user is the
            // one deciding. Saying only "replace textures and sounds" would
            // describe the common case and hide the one that matters.
            Capability::AssetsOverride => {
                "Replace Roblox's own textures, sounds, fonts and models with files it supplies. \
                 Nothing is written into Roblox's files and removing the plugin puts the originals \
                 back. Replacing a model can change more than appearance, and Cordial does not \
                 check which is which."
            }
            Capability::SettingsRead => "Read what it previously saved for itself. Its own data only.",
            Capability::SettingsWrite => {
                "Replace what it saved for itself, including discarding all of it. Its own data only."
            }
            Capability::EventsDeclare => "Announce the kinds of message it can send to other plugins.",
            Capability::EventsPublish => {
                "Send messages to other plugins, under its own name only — it cannot speak as \
                 another plugin."
            }
            Capability::EventsSubscribe => {
                "Receive messages other plugins send, including ones it did not declare."
            }
        }
    }

    /// Every capability, so a UI can present the full set rather than a
    /// hand-maintained copy that drifts.
    pub fn all() -> &'static [Capability] {
        &[
            Capability::FlagsRead,
            Capability::FlagsWrite,
            Capability::FlagsWriteDynamic,
            Capability::Log,
            Capability::LifecycleRead,
            Capability::PresenceSet,
            Capability::NotifySend,
            Capability::UrlOpen,
            Capability::AssetsOverride,
            Capability::SettingsRead,
            Capability::SettingsWrite,
            Capability::EventsDeclare,
            Capability::EventsPublish,
            Capability::EventsSubscribe,
        ]
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_capability_round_trips_through_its_wire_name() {
        // `name`, `parse` and `all` are three hand-maintained lists of one
        // thing. A variant added to two of them and missed in the third fails
        // quietly: a grants file naming it would be refused as unknown, and the
        // user would be told they granted something that does not exist.
        for c in Capability::all() {
            assert_eq!(Capability::parse(c.name()), Some(*c), "{c} does not parse back");
        }
        let names: BTreeSet<&str> = Capability::all().iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), Capability::all().len(), "two capabilities share a wire name");
    }

    #[test]
    fn every_capability_says_what_it_actually_does() {
        // `consequence` is the text a user reads before deciding, so a
        // variant added without one would present as a blank line in the
        // prompt — a permission the user was asked to approve with no
        // description of it. The compiler catches a missing arm; this catches
        // an arm someone filled in with nothing.
        for c in Capability::all() {
            let text = c.consequence();
            assert!(!text.is_empty(), "{c} has no description");
            assert!(text.ends_with('.'), "{c}: a description is a sentence, not a label: {text:?}");
            // Only the dotted names are checked. `log`'s wire name is an
            // ordinary English word and its description says "log output"
            // legitimately; the jargon this guards against is `flags.write`
            // and `presence.set` appearing where a sentence should be.
            if c.name().contains('.') {
                assert!(
                    !text.contains(c.name()),
                    "{c}: the description must say what it does, not repeat its wire name: {text:?}"
                );
            }
        }
    }

    #[test]
    fn flags_write_says_it_changes_cordial_itself() {
        // The one that would otherwise be described as "change some Roblox
        // settings" — technically true and materially misleading. ADR-020
        // records that it reaches every `Cordial`-prefixed key, the graphics
        // backend and present mode included, so the description has to say so.
        let text = Capability::FlagsWrite.consequence();
        assert!(text.contains("Cordial"), "{text:?}");
        assert!(text.contains("graphics backend"), "{text:?}");
        assert!(text.contains("present mode"), "{text:?}");
    }

    #[test]
    fn reading_settings_is_not_writing_them() {
        // Split for the same reason declare and publish are split. A copied
        // arm returning the other's name here would make a grant of
        // settings.read parse as settings.write and silently widen it.
        assert_eq!(Capability::parse("settings.read"), Some(Capability::SettingsRead));
        assert_eq!(Capability::parse("settings.write"), Some(Capability::SettingsWrite));
        assert_ne!(Capability::SettingsRead, Capability::SettingsWrite);
    }
}
