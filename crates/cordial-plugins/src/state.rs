//! What the client is doing right now, kept by Cordial and read by plugins.
//!
//! **Cordial keeps this so that plugins do not each keep their own.** The
//! alternative is what the core bus alone would give you: every plugin that
//! wants to know which server you are on subscribes to a stream of events,
//! folds them into a private copy, and gets the folding subtly wrong in its own
//! way. Three plugins would disagree about how long you had been playing, and
//! each one would be right about the events it happened to see.
//!
//! **This is not a second source of truth.** Cordial derives it from the
//! engine's own log -- `cordial_runtime::game_log` -- and publishes the core
//! events *from the same updates*, so the snapshot and the stream cannot drift:
//! one is a view of the other. A plugin may read the snapshot when it starts
//! and follow the events after, or ignore the events entirely and poll; both
//! see the same thing.
//!
//! ## What is deliberately not here
//!
//! **Username and display name.** Only the numeric user id is in the log. A
//! name needs a request to `users.roblox.com` on the player's behalf, which is
//! a network call Cordial would be making for a plugin's convenience and a
//! third party would see. Left out rather than stubbed: a field that is always
//! `null` is worse than an absent one, because it reads as "not playing" rather
//! than "never implemented".
//!
//! **A server *location*.** [`SessionState::server_address`] is the address the
//! client connected through, which is what the log carries. Turning it into a
//! city means a geo-IP lookup against somebody else's service, which is the
//! same objection one step further along.
//!
//! **Anything the engine did not say.** No frame rate, no memory, no player
//! list. Those need engine introspection, which ADR-001 and ADR-003 rule out.

use serde::{Deserialize, Serialize};

/// A snapshot of the session, as Cordial understands it.
///
/// Every field is `Option` and absent means *not known*, which is a different
/// statement from a zero. Outside a game almost all of it is `None`, and that
/// is the honest answer rather than a stale copy of the last game.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    /// The place being played. What `roblox.com/games/<id>` names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place_id: Option<u64>,
    /// The experience the place belongs to. This is the one that resolves to a
    /// title through Roblox's API -- `games?universeIds=` takes universes, not
    /// places -- which is why both are carried rather than just the place.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub universe_id: Option<u64>,
    /// The server instance, when the join line named one.
    ///
    /// A UUID. This is what a `&gameInstanceId=` deep link needs to put
    /// somebody in *this* server rather than start a new one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// The address the client is connected through, from `UDMUX Address`.
    ///
    /// Not the "RCC Server Address" on the same log line, which is private to
    /// Roblox's own network and means nothing outside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_address: Option<String>,
    /// Who is playing, as a Roblox user id.
    ///
    /// From the same log line as the place. No name -- see the module comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<u64>,
    /// Unix seconds at which the current game was joined.
    ///
    /// A timestamp rather than a duration, deliberately: a duration is stale
    /// the moment it is read, and a reader that wants one can subtract. It also
    /// means two plugins asking a second apart get answers that agree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joined_at: Option<u64>,
}

impl SessionState {
    /// Whether a game is being played at all.
    ///
    /// Keyed on the place rather than on any of the others, because the place
    /// is the first thing known and the last thing cleared -- the job id can be
    /// missing for a session Cordial joined late, and the address arrives
    /// milliseconds after the join.
    pub fn in_game(&self) -> bool {
        self.place_id.is_some()
    }

    /// Forget the game, keeping nothing.
    ///
    /// Everything here describes one visit to one server, so leaving clears all
    /// of it. A `user_id` that outlived the game would be the one field a
    /// plugin could read on the home screen and believe was current.
    pub fn left(&mut self) {
        *self = SessionState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An empty state serialises to `{}`, not to a document of nulls.**
    ///
    /// A reader seeing `{"place_id": null, ...}` has to know that null means
    /// "not playing" rather than "playing something unnamed". An absent key
    /// cannot be misread that way, and it is what `skip_serializing_if` is
    /// there for.
    #[test]
    fn nothing_known_is_an_empty_document() {
        let json = serde_json::to_string(&SessionState::default()).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn a_full_state_round_trips() {
        let state = SessionState {
            place_id: Some(17625359962),
            universe_id: Some(6035872082),
            job_id: Some("3182c122-8e1c-4f50-b0ac-6fb67ba0082f".into()),
            server_address: Some("128.116.51.33".into()),
            user_id: Some(1826805362),
            joined_at: Some(1788200000),
        };
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<SessionState>(&text).unwrap(), state);
        assert!(state.in_game());
    }

    /// **Leaving clears everything, including who was playing.**
    ///
    /// A `user_id` left behind would be the one field a plugin could read on
    /// the home screen and reasonably believe was current, which is worse than
    /// not having it: the others are obviously absent there.
    #[test]
    fn leaving_keeps_nothing() {
        let mut state = SessionState {
            place_id: Some(1),
            user_id: Some(2),
            joined_at: Some(3),
            ..Default::default()
        };
        state.left();
        assert_eq!(state, SessionState::default());
        assert!(!state.in_game());
    }

    /// **A teleport must not carry the old server's job id.**
    ///
    /// This is the transition worth pinning, because getting it wrong is
    /// invisible until somebody clicks. A teleport reports a join for a new
    /// place; if the job id from the previous one survived, the "Join server"
    /// button would name a server the player has left and send whoever clicks
    /// it somewhere nobody is. `game_log` clears it whenever the place
    /// changes, and this is the property that guards.
    #[test]
    fn a_new_place_keeps_nothing_from_the_old_one() {
        let mut state = SessionState {
            place_id: Some(1),
            job_id: Some("3182c122-8e1c-4f50-b0ac-6fb67ba0082f".into()),
            server_address: Some("128.116.51.33".into()),
            user_id: Some(42),
            ..Default::default()
        };
        // What `game_log` does on a join for a different place.
        state.left();
        state.place_id = Some(2);

        assert_eq!(state.job_id, None, "the old server must not follow the player");
        assert_eq!(state.server_address, None);
        assert!(state.in_game());
    }

    /// A partial state is in a game as soon as the place is known.
    ///
    /// The job id can be missing for a session Cordial joined late, and the
    /// address arrives a few milliseconds after the join, so keying `in_game`
    /// on either would report "not playing" during a window when the player
    /// plainly is.
    #[test]
    fn the_place_alone_means_in_game() {
        let state = SessionState { place_id: Some(1), ..Default::default() };
        assert!(state.in_game());
    }
}
