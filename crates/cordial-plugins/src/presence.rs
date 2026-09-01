//! Discord Rich Presence — ADR-007's worked example, made to actually work.
//!
//! Cordial holds the connection to Discord's IPC socket; the plugin only ever
//! sends a [`PresencePayload`]. Nothing here hands a socket, a file
//! descriptor, or Discord's raw protocol to a plugin — see the module comment
//! in `broker.rs` and ADR-007 for why that boundary is the whole point.
//!
//! The protocol is Discord's own IPC framing, used by every third-party Rich
//! Presence integration (Cordial is not the first non-Electron client to
//! implement it): a Unix domain socket at a fixed, well-known location, and
//! frames of `opcode: u32 LE`, `length: u32 LE`, then that many bytes of
//! JSON. Opcode 0 is the handshake (`{"v":1,"client_id":"..."}`); opcode 1
//! carries every subsequent command, including `SET_ACTIVITY`. This is public
//! protocol documentation, not anything extracted from Roblox — AGENTS.md's
//! rule against decompiling Roblox has nothing to do with this file.

use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// What a plugin may ask Cordial to publish. Deliberately a closed struct
/// with `deny_unknown_fields` rather than a JSON value forwarded verbatim —
/// the whole reason this is brokered rather than a raw socket handed to the
/// plugin is that Cordial decides what shape crosses the wire to Discord, and
/// a permissive `Value` would quietly undo that the moment Discord's IPC grew
/// a field this struct does not know about.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PresencePayload {
    /// The Discord application this presence is published under. Not a
    /// secret and not itself a channel — it only selects which app's name
    /// and icon Discord shows next to the activity — but still validated as
    /// a snowflake so a plugin cannot smuggle an arbitrary string into a
    /// field Cordial does not otherwise inspect before it reaches Discord.
    pub client_id: String,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    /// Unix seconds. `start` without `end` renders as an elapsed counter;
    /// both together render as a countdown. Discord's own semantics, not
    /// Cordial's.
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
    /// The big picture beside the activity, as **a token Cordial minted**.
    ///
    /// Not a URL and not something a plugin composes. Cordial resolves Roblox
    /// ids to pictures in its runtime -- that is where the HTTP client is --
    /// and hands out an opaque key for each one; this field echoes a key back.
    /// A key nothing has resolved renders as no picture.
    ///
    /// **That is deliberately stronger than validating a URL.** These were
    /// free strings passed straight into Discord's `large_image`, which is the
    /// hole `buttons` is guarded against: a plugin could publish an arbitrary
    /// external link under Cordial's name and icon. Checking the host would
    /// still be letting a URL cross this boundary and then arguing about which
    /// hosts are acceptable; a key cannot be a link at all, so there is no
    /// argument to have.
    ///
    /// An empty string is "the game cleared this", a different statement from
    /// absent, and renders as no picture rather than Cordial's own. See
    /// `bloxstrap_rpc::Presence::to_payload`.
    #[serde(default)]
    pub large_image_key: Option<String>,
    #[serde(default)]
    pub large_text: Option<String>,
    /// The small badge in the corner. See [`PresencePayload::large_image_key`].
    #[serde(default)]
    pub small_image_key: Option<String>,
    #[serde(default)]
    pub small_text: Option<String>,
    /// The experience being played, for the buttons Cordial adds.
    ///
    /// **Not the same thing as `details` and `state`, which are decoration.**
    /// These two are identifiers, and Cordial turns them into URLs -- so they
    /// are validated here rather than passed through, and a plugin cannot set
    /// the resulting link directly. See [`buttons_for`].
    #[serde(default)]
    pub place_id: Option<u64>,
    /// The server instance, when one is known. `roblox://...&gameInstanceId=`
    /// needs it to join *this* server rather than start a new one.
    #[serde(default)]
    pub job_id: Option<String>,
}

/// Discord's own limit on `details` and `state`; rejecting past it here
/// means the plugin author finds out from `presence.set`'s response instead
/// of from Discord silently truncating or refusing the whole activity.
const TEXT_FIELD_LIMIT: usize = 128;

impl PresencePayload {
    pub fn parse(value: &Value) -> Result<Self, String> {
        let payload: PresencePayload =
            serde_json::from_value(value.clone()).map_err(|e| format!("bad presence payload: {e}"))?;
        if payload.client_id.is_empty() || !payload.client_id.bytes().all(|b| b.is_ascii_digit()) {
            return Err("client_id must be a Discord application snowflake (digits only)".into());
        }
        for (name, text) in [("details", &payload.details), ("state", &payload.state)] {
            if let Some(t) = text {
                if t.chars().count() > TEXT_FIELD_LIMIT {
                    return Err(format!("{name} must be at most {TEXT_FIELD_LIMIT} characters, Discord's own limit"));
                }
            }
        }
        // A job id becomes part of a `roblox://` URL handed to another
        // program, so it is checked here rather than trusted. Same rule
        // `game_log` applies where it is read: a UUID and nothing else.
        if let Some(job) = &payload.job_id {
            let shaped = job.len() == 36
                && job.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
            if !shaped {
                return Err("job_id must be a server instance UUID".into());
            }
        }
        // A key is only ever looked up, never formatted into anything, so the
        // check here is a sanity bound rather than a security boundary -- the
        // boundary is that unknown keys resolve to nothing. Empty is allowed
        // and means cleared.
        for (name, key) in
            [("large_image_key", &payload.large_image_key), ("small_image_key", &payload.small_image_key)]
        {
            if let Some(key) = key {
                let shaped = key.len() <= 40 && key.bytes().all(|b| b.is_ascii_alphanumeric());
                if !key.is_empty() && !shaped {
                    return Err(format!("{name} is not a key Cordial issued"));
                }
            }
        }
        Ok(payload)
    }

    fn to_activity(&self) -> Value {
        let mut activity = serde_json::Map::new();
        if let Some(d) = &self.details {
            activity.insert("details".into(), json!(d));
        }
        if let Some(s) = &self.state {
            activity.insert("state".into(), json!(s));
        }
        if self.start.is_some() || self.end.is_some() {
            let mut ts = serde_json::Map::new();
            if let Some(s) = self.start {
                ts.insert("start".into(), json!(s));
            }
            if let Some(e) = self.end {
                ts.insert("end".into(), json!(e));
            }
            activity.insert("timestamps".into(), Value::Object(ts));
        }
        if self.large_image_key.is_some() || self.large_text.is_some() || self.small_image_key.is_some() || self.small_text.is_some() {
            let mut assets = serde_json::Map::new();
            if let Some(url) = resolved_image(self.large_image_key.as_deref()) {
                assets.insert("large_image".into(), json!(url));
            }
            if let Some(v) = &self.large_text {
                assets.insert("large_text".into(), json!(v));
            }
            if let Some(url) = resolved_image(self.small_image_key.as_deref()) {
                assets.insert("small_image".into(), json!(url));
            }
            if let Some(v) = &self.small_text {
                assets.insert("small_text".into(), json!(v));
            }
            activity.insert("assets".into(), Value::Object(assets));
        }
        // **Cordial's own button, on every presence, including one a game
        // drove.** BloxstrapRPC lets an experience set the details and the
        // state from inside itself; this is the one part of the activity that
        // is Cordial's and not the caller's, which is why it is added here in
        // the broker rather than by the plugin that happened to make the call.
        // ADR-007's shape exactly: Cordial performs the effect, so Cordial
        // decides what its own name is attached to.
        //
        // Deliberately not a field a payload may set. `PresencePayload` is
        // `deny_unknown_fields`, so a plugin asking for `buttons` is refused
        // rather than quietly ignored -- a plugin that could set its own would
        // be able to publish an arbitrary link under Cordial's name and icon,
        // which is a different capability from "show a presence" and is not
        // one anything has been granted.
        activity.insert("buttons".into(), json!(buttons_for(self.place_id, self.job_id.as_deref())));
        Value::Object(activity)
    }
}

/// The button Discord renders under every activity Cordial publishes.
///
/// One, not two. Discord allows two, and the second is the one that would get
/// filled with whatever seemed useful at the time; leaving it empty keeps the
/// activity small and keeps this from becoming a place to advertise.
///
/// The label is well inside Discord's 32-character limit and the URL is
/// `https`, which Discord requires -- it rejects the whole activity otherwise,
/// silently, which is the failure mode this comment exists to stop somebody
/// reintroducing with a nicer label.
/// Pictures Cordial has resolved, by the asset id they came from.
///
/// **A plugin names an id; it never supplies a URL.** This is the answer to
/// "should the host check be a whitelist" -- a whitelist would still be
/// letting a URL cross the plugin boundary and then arguing about which URLs
/// are acceptable, and the argument is unnecessary. Only Cordial writes this
/// map, from what Roblox's own thumbnail service answered, so the worst a
/// plugin can do with a made-up id is get no picture. There is no string a
/// plugin can send that becomes a link under Cordial's name and icon, which
/// is the same guarantee `buttons_for` gives and for the same reason.
///
/// A map rather than one current image because a presence carries two slots
/// and a game may change either between heartbeats.
static RESOLVED_IMAGES: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);

/// Record what one of Cordial's image keys resolved to. Cordial's runtime
/// calls this; nothing reachable from a plugin does.
pub fn remember_image(key: &str, url: &str) {
    let mut guard = RESOLVED_IMAGES.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(Default::default)
        .insert(key.to_string(), url.to_string());
}

/// The picture for one of Cordial's keys, if it resolved.
///
/// A key nothing has resolved answers `None` and the activity carries no
/// image -- which is also what an empty key means, the game having cleared the
/// slot. Both render as no picture, the honest outcome for "there is nothing
/// to show here".
fn resolved_image(key: Option<&str>) -> Option<String> {
    let id = key?;
    if id.is_empty() {
        return None;
    }
    let guard = RESOLVED_IMAGES.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref()?.get(id).cloned()
}

fn cordial_button() -> Value {
    json!({ "label": "Cordial on GitHub", "url": "https://github.com/luohoa97/cordial" })
}

/// The buttons under an activity: at most one about the game, then Cordial's.
///
/// **Discord allows exactly two, and that is what decides the shape.**
/// Bloxstrap uses both of its own -- "Join server" and "See game page" -- and
/// Cordial's own link has to be on every activity, so one of theirs gives way.
/// The join link goes first when there is one, because it is the only button
/// that does something you cannot do from the other; the game page is the
/// fallback, because a place id alone still tells somebody what is being
/// played.
///
/// The URL shape is Bloxstrap's, adapted rather than transcribed (MIT,
/// Bloxstrap Labs): `roblox://experiences/start?placeId=…&gameInstanceId=…`.
///
/// **No join button without a job id**, and that is not caution for its own
/// sake -- a `placeId` on its own launches a *new* server, so a "Join server"
/// button built from one would take somebody somewhere the player is not. A
/// button that does the wrong thing is worse than an absent one.
///
/// Cordial does not know whether the server is public. Bloxstrap does, and
/// hides the button for private ones; here the link is simply offered and
/// Roblox refuses it if the clicker cannot join, which is a worse experience
/// than Bloxstrap's and an honest one -- inventing a server-type guess would
/// be the kind of plausible inference this project keeps retracting.
fn buttons_for(place_id: Option<u64>, job_id: Option<&str>) -> Vec<Value> {
    let mut buttons = Vec::with_capacity(2);
    if let Some(place) = place_id {
        match job_id {
            Some(job) => buttons.push(json!({
                "label": "Join server",
                "url": format!(
                    "roblox://experiences/start?placeId={place}&gameInstanceId={job}"
                ),
            })),
            None => buttons.push(json!({
                "label": "See game page",
                "url": format!("https://www.roblox.com/games/{place}"),
            })),
        }
    }
    buttons.push(cordial_button());
    buttons
}

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;

/// Every path Discord's IPC socket might be at, in search order.
///
/// ADR-007 names this search as the reason to broker presence at all rather
/// than let every plugin reimplement it: `discord-ipc-0` through `-9`
/// (Discord increments the suffix if an earlier one is taken, most often by
/// a second Discord instance), and, when Discord itself is a Flatpak, nested
/// under its own app-id directory because Flatpak sandboxes rewrite
/// `XDG_RUNTIME_DIR` per app.
fn candidate_sockets() -> Vec<PathBuf> {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    let runtime_dir = PathBuf::from(runtime_dir);
    let mut candidates = Vec::with_capacity(20);
    for slot in 0..10 {
        candidates.push(runtime_dir.join(format!("discord-ipc-{slot}")));
    }
    for slot in 0..10 {
        candidates.push(
            runtime_dir
                .join("app/com.discordapp.Discord")
                .join(format!("discord-ipc-{slot}")),
        );
    }
    candidates
}

fn write_frame(stream: &mut UnixStream, opcode: u32, body: &Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(body).expect("presence frames always serialise");
    stream.write_all(&opcode.to_le_bytes())?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)
}

/// Read one frame back. Discord acknowledges both the handshake and every
/// command, and a socket that never answers is as good as one that is not
/// there — a two second timeout on the stream (set by the caller) turns a
/// hang into a clean failure instead of blocking Cordial on a dead peer.
fn read_frame(stream: &mut UnixStream) -> std::io::Result<(u32, Vec<u8>)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header)?;
    let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok((opcode, body))
}

fn handshake(stream: &mut UnixStream, client_id: &str) -> std::io::Result<()> {
    write_frame(stream, OP_HANDSHAKE, &json!({"v": 1, "client_id": client_id}))?;
    // Discord answers the handshake with a DISPATCH/READY frame; any
    // well-formed frame back is proof the peer is really Discord's IPC
    // server and not some other process that happened to be listening on
    // that path.
    read_frame(stream)?;
    Ok(())
}

/// `CORDIAL_TRACE_PRESENCE=1` — print what Discord answers to each activity.
///
/// Off by default because the reply echoes the whole stored activity, which
/// on a game presence names the place and the server instance; that belongs
/// in a developer's terminal on request and not in every user's log.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CORDIAL_TRACE_PRESENCE").as_deref() == Ok("1"))
}

static NONCE: AtomicU64 = AtomicU64::new(1);

fn next_nonce() -> String {
    format!("cordial-{}", NONCE.fetch_add(1, Ordering::Relaxed))
}

/// Cordial's side of the Discord IPC connection. One instance per running
/// Cordial process; never exposed to a plugin.
pub struct DiscordPresence {
    connected: Option<(String, UnixStream)>,
    /// Whether "Discord is not running" has already been logged for the
    /// current stretch of unavailability. Discord not running is the normal
    /// case (most users do not have it open), so `presence.set` failing must
    /// not spam the log on every lifecycle event — one line per stretch,
    /// reset the moment a connection succeeds again, is enough to be found
    /// without becoming noise. See AGENTS.md: a stub must never claim
    /// success it did not have, so the call itself still fails every time;
    /// only the logging is throttled.
    unavailable_logged: bool,
}

impl DiscordPresence {
    pub fn new() -> Self {
        DiscordPresence { connected: None, unavailable_logged: false }
    }

    fn log_unavailable_once(&mut self, why: &str) {
        if !self.unavailable_logged {
            println!("  presence: {why}; presence.set will keep failing (silently, after this) until it is");
            self.unavailable_logged = true;
        }
    }

    /// Connect and handshake for `client_id` if not already connected under
    /// it. No retry loop inside this function: one attempt per call, because
    /// a caller that wants another attempt makes another call — an internal
    /// retry here would be the tight loop AGENTS.md and ADR-007 both warn
    /// against for a resource that is routinely just absent.
    fn ensure_connected(&mut self, client_id: &str) -> Result<(), String> {
        if let Some((connected_id, _)) = &self.connected {
            if connected_id == client_id {
                return Ok(());
            }
            // A different client_id: drop the old connection rather than
            // hold two, since Cordial only ever brokers one presence at a
            // time in practice and a stale handle left open would leak.
            self.connected = None;
        }
        let candidates = candidate_sockets();
        if candidates.is_empty() {
            self.log_unavailable_once("XDG_RUNTIME_DIR is not set, so there is nowhere to look for Discord's IPC socket");
            return Err("Discord is not running".into());
        }
        for path in &candidates {
            let Ok(mut stream) = UnixStream::connect(path) else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            if handshake(&mut stream, client_id).is_ok() {
                self.connected = Some((client_id.to_string(), stream));
                self.unavailable_logged = false;
                return Ok(());
            }
        }
        self.log_unavailable_once("no Discord IPC socket answered a handshake; is Discord running?");
        Err("Discord is not running".into())
    }

    fn send_activity(&mut self, activity: Value) -> Result<(), String> {
        let (_, stream) = self.connected.as_mut().expect("ensure_connected must be called first");
        let frame = json!({
            "cmd": "SET_ACTIVITY",
            "args": {"pid": std::process::id(), "activity": activity},
            "nonce": next_nonce(),
        });
        let result = write_frame(stream, OP_FRAME, &frame).and_then(|()| read_frame(stream));
        let (_, body) = match result {
            Ok(frame) => frame,
            Err(e) => {
                // The connection is no good any more; drop it so the next
                // call starts a fresh handshake instead of writing into a
                // socket we already know is broken.
                self.connected = None;
                self.log_unavailable_once("Discord's IPC socket stopped answering");
                return Err(format!("presence update failed: {e}"));
            }
        };

        // **Discord's answer is read and then judged, which it was not.** This
        // used to be `Ok(_) => Ok(())`: any well-formed frame back counted as
        // success. Discord replies to a `SET_ACTIVITY` it *rejects* with an
        // equally well-formed frame carrying `evt: "ERROR"`, so a refused
        // activity was reported to the plugin as `ok` and written to the
        // plugin log as `ok`. That is the shape AGENTS.md calls a stub that
        // lies -- the caller proceeds on an answer that is not true -- and it
        // made the log useless as evidence for the one question anybody asks
        // of it, which is whether the presence actually landed.
        //
        // Found while trying to verify a real game's presence end to end: the
        // log said `ok` and there was no way to tell from inside Cordial
        // whether Discord had stored anything.
        let reply: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if trace_enabled() {
            eprintln!("[presence] discord replied: {reply}");
        }
        if reply["evt"] == json!("ERROR") {
            // Not a dead socket, so the connection is kept: Discord is
            // answering, it just did not like this payload. Dropping it here
            // would turn one bad activity into a reconnect on every retry.
            let code = &reply["data"]["code"];
            let message = reply["data"]["message"].as_str().unwrap_or("no message");
            return Err(format!("Discord refused the activity ({code}): {message}"));
        }
        Ok(())
    }

    /// Publish a presence payload. Fails cleanly, without panicking or
    /// blocking, when Discord is not running — see the module comment.
    pub fn set(&mut self, payload: &PresencePayload) -> Result<(), String> {
        self.ensure_connected(&payload.client_id)?;
        self.send_activity(payload.to_activity())
    }

    /// Clear whatever presence is currently set. A no-op, not an error, if
    /// nothing has been set in this process yet — there is nothing to clear
    /// and no connection worth opening just to say so.
    pub fn clear(&mut self) -> Result<(), String> {
        let Some((client_id, _)) = &self.connected else {
            return Ok(());
        };
        let client_id = client_id.clone();
        self.ensure_connected(&client_id)?;
        self.send_activity(Value::Null)
    }
}

impl Default for DiscordPresence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {

    /// **Cordial's button is on every activity, including a game's.** That is
    /// the requirement -- "keep the github link for every game btw" -- and it
    /// is the one thing here a caller cannot remove.
    #[test]
    fn cordials_button_is_always_last_and_always_present() {
        for (place, job) in
            [(None, None), (Some(1u64), None), (Some(1u64), Some("a"))]
        {
            let b = super::buttons_for(place, job);
            assert_eq!(b.last().unwrap()["label"], "Cordial on GitHub", "{place:?}/{job:?}");
            assert!(b.len() <= 2, "Discord allows two buttons, got {}", b.len());
        }
    }

    /// A join link needs the server, not just the place.
    ///
    /// **A `placeId` on its own starts a *new* server**, so a "Join server"
    /// button built from one would send somebody to a place the player is not.
    /// The game page is offered instead, which is honest about what it does.
    #[test]
    fn without_a_job_id_there_is_no_join_button() {
        let b = super::buttons_for(Some(17625359962), None);
        assert_eq!(b[0]["label"], "See game page");
        assert_eq!(b[0]["url"], "https://www.roblox.com/games/17625359962");
    }

    /// With one, the deeplink names the instance.
    ///
    /// The shape is Bloxstrap's (MIT), adapted: their
    /// `ActivityData.GetInviteDeeplink` builds
    /// `roblox://experiences/start?placeId=…&gameInstanceId=…`.
    #[test]
    fn a_job_id_produces_a_join_deeplink_naming_the_instance() {
        let b = super::buttons_for(Some(17625359962), Some("3182c122-8e1c-4f50-b0ac-6fb67ba0082f"));
        assert_eq!(b[0]["label"], "Join server");
        assert_eq!(
            b[0]["url"],
            "roblox://experiences/start?placeId=17625359962&gameInstanceId=3182c122-8e1c-4f50-b0ac-6fb67ba0082f"
        );
    }

    /// Outside a game there is only Cordial's own button.
    #[test]
    fn the_home_screen_gets_one_button() {
        assert_eq!(super::buttons_for(None, None).len(), 1);
    }

    /// **A job id that is not a UUID is refused before it reaches a URL.**
    ///
    /// It arrives from a plugin over the wire and ends up in a `roblox://`
    /// link handed to another program, so the check is at the boundary rather
    /// than trusted from wherever it came.
    #[test]
    fn a_job_id_that_is_not_a_uuid_is_refused_at_the_payload() {
        let bad = serde_json::json!({
            "client_id": "1543200871767212062",
            "job_id": "../../etc/passwd",
        });
        let err = super::PresencePayload::parse(&bad).unwrap_err();
        assert!(err.contains("job_id"), "{err}");

        let good = serde_json::json!({
            "client_id": "1543200871767212062",
            "job_id": "3182c122-8e1c-4f50-b0ac-6fb67ba0082f",
        });
        assert!(super::PresencePayload::parse(&good).is_ok());
    }
    use super::*;
    use std::os::unix::net::UnixListener;

    fn snowflake() -> &'static str {
        "1234567890123456"
    }

    #[test]
    fn a_payload_needs_a_numeric_client_id() {
        let bad = json!({"client_id": "not-a-snowflake", "details": "In a game"});
        assert!(PresencePayload::parse(&bad).is_err());
    }

    #[test]
    fn a_payload_rejects_fields_discord_does_not_define() {
        // The struct is deny_unknown_fields on purpose: a plugin must not be
        // able to smuggle a field Cordial does not itself construct into
        // what eventually reaches Discord's socket.
        let bad = json!({"client_id": snowflake(), "cmd": "SOMETHING_ELSE"});
        assert!(PresencePayload::parse(&bad).is_err());
    }

    #[test]
    fn details_over_the_discord_limit_is_refused() {
        let bad = json!({"client_id": snowflake(), "details": "x".repeat(200)});
        let e = PresencePayload::parse(&bad).unwrap_err();
        assert!(e.contains("128"), "{e}");
    }

    #[test]
    fn a_well_formed_payload_parses() {
        let ok = json!({
            "client_id": snowflake(),
            "details": "Playing Baseplate",
            "state": "In a server",
            "start": 1_700_000_000,
            "large_image_key": "a13913198647",
        });
        let p = PresencePayload::parse(&ok).unwrap();
        assert_eq!(p.client_id, snowflake());
        assert_eq!(p.details.as_deref(), Some("Playing Baseplate"));
    }

    /// **A key Cordial resolved becomes the picture; one it did not is
    /// nothing.** The bug behind this: `to_payload` dropped images entirely on
    /// a comment claiming they were Discord asset keys, so a game that set
    /// cover art got Cordial's own icon.
    #[test]
    fn a_resolved_key_becomes_the_image_and_an_unknown_one_is_ignored() {
        super::remember_image("a13913198647", "https://tr.rbxcdn.com/180DAY-abc/420/420/Image/Png/noFilter");

        let payload = PresencePayload::parse(&json!({
            "client_id": snowflake(),
            "large_image_key": "a13913198647",
            "large_text": "Game: Crossroads",
        }))
        .unwrap();
        let assets = &payload.to_activity()["assets"];
        assert_eq!(assets["large_image"], "https://tr.rbxcdn.com/180DAY-abc/420/420/Image/Png/noFilter");
        assert_eq!(assets["large_text"], "Game: Crossroads");

        // **The property that replaces host-checking a URL.** A plugin can
        // send any key it likes; one Cordial never issued resolves to nothing,
        // so there is no string it can send that becomes a link.
        let invented = PresencePayload::parse(&json!({
            "client_id": snowflake(),
            "large_image_key": "notakeycordialissued",
        }))
        .unwrap();
        assert!(
            invented.to_activity()["assets"]["large_image"].is_null(),
            "an unresolved key must not become an image"
        );
    }

    #[test]
    fn a_key_that_is_not_key_shaped_is_refused() {
        for bad in ["../../evil", "1&x=2", "http://example.com/x.png", "a13913198647 ", &"a".repeat(41)] {
            let e = PresencePayload::parse(&json!({"client_id": snowflake(), "large_image_key": bad}))
                .unwrap_err();
            assert!(e.contains("key"), "{bad:?} gave: {e}");
        }
    }

    /// **Cleared is not the same as never set.** A game that clears its image
    /// must get no picture, not Cordial's icon back.
    #[test]
    fn a_cleared_image_writes_no_key() {
        let payload =
            PresencePayload::parse(&json!({"client_id": snowflake(), "large_image_key": ""}))
                .unwrap();
        let activity = payload.to_activity();
        assert!(
            activity["assets"]["large_image"].is_null(),
            "a cleared image must not become a URL: {activity}"
        );
    }

    /// Stands in for Discord: accepts one connection, reads a handshake
    /// frame, answers it, then reads one SET_ACTIVITY frame and reports its
    /// opcode and JSON body back over a channel — this is what "verify
    /// socket discovery and framing against a local test double" means. It
    /// is not, and cannot be, a substitute for having watched a real Discord
    /// client show the activity; see the written report for that caveat.
    fn spawn_fake_discord(path: PathBuf) -> std::sync::mpsc::Receiver<(u32, Value)> {
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (op, body) = read_frame(&mut stream).unwrap();
            assert_eq!(op, OP_HANDSHAKE);
            let handshake: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(handshake["v"], 1);
            tx.send((op, handshake)).unwrap();
            // Acknowledge the handshake the way Discord does, so
            // DiscordPresence::ensure_connected's read_frame does not hang.
            write_frame(&mut stream, OP_FRAME, &json!({"evt": "READY"})).unwrap();

            let (op, body) = read_frame(&mut stream).unwrap();
            let cmd: Value = serde_json::from_slice(&body).unwrap();
            tx.send((op, cmd)).unwrap();
            write_frame(&mut stream, OP_FRAME, &json!({"evt": null})).unwrap();
        });
        rx
    }

    // XDG_RUNTIME_DIR is process-wide state, and cargo runs tests in this
    // file on multiple threads by default. Every test that points it at a
    // scratch directory holds this lock for as long as the env var is set,
    // so the two below cannot race each other and corrupt the socket search.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn presence_set_speaks_discords_framing_to_a_local_socket() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("cordial-discord-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let socket_path = dir.join("discord-ipc-0");

        let rx = spawn_fake_discord(socket_path.clone());

        let mut presence = DiscordPresence::new();
        let payload = PresencePayload::parse(&json!({
            "client_id": snowflake(),
            "details": "Playing Baseplate",
            "state": "In a server",
        }))
        .unwrap();
        presence.set(&payload).expect("the fake Discord should accept a well-formed activity");

        let (op, handshake_body) = rx.recv().unwrap();
        assert_eq!(op, OP_HANDSHAKE);
        assert_eq!(handshake_body["client_id"], snowflake());

        let (op, activity_frame) = rx.recv().unwrap();
        assert_eq!(op, OP_FRAME);
        assert_eq!(activity_frame["cmd"], "SET_ACTIVITY");
        assert_eq!(activity_frame["args"]["activity"]["details"], "Playing Baseplate");
        assert_eq!(activity_frame["args"]["activity"]["state"], "In a server");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake Discord that refuses the activity the way the real one does.
    fn spawn_refusing_discord(path: PathBuf) -> std::sync::mpsc::Receiver<Value> {
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_frame(&mut stream).unwrap();
            write_frame(&mut stream, OP_FRAME, &json!({"evt": "READY"})).unwrap();

            let (_, body) = read_frame(&mut stream).unwrap();
            tx.send(serde_json::from_slice(&body).unwrap()).unwrap();
            // The shape Discord answers with when it will not store the
            // activity: a perfectly well-formed frame that happens to say no.
            write_frame(
                &mut stream,
                OP_FRAME,
                &json!({
                    "cmd": "SET_ACTIVITY",
                    "evt": "ERROR",
                    "data": {"code": 4000, "message": "Invalid activity"},
                }),
            )
            .unwrap();
        });
        rx
    }

    /// **A refused activity is an error, not an `ok`.**
    ///
    /// This is the bug the test exists for: `send_activity` used to treat any
    /// well-formed reply as success, and Discord refuses an activity with a
    /// well-formed reply. So a rejected presence was reported to the plugin as
    /// `ok` and written to the plugin log as `ok`, which made the log useless
    /// as evidence for the only question anybody asks of it. AGENTS.md: never
    /// make a stub lie -- reporting failure keeps the gap where somebody can
    /// find it.
    #[test]
    fn a_refused_activity_is_reported_as_a_failure() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("cordial-discord-refuse-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);

        let rx = spawn_refusing_discord(dir.join("discord-ipc-0"));

        let mut presence = DiscordPresence::new();
        let payload = PresencePayload::parse(&json!({"client_id": snowflake()})).unwrap();
        let err = presence
            .set(&payload)
            .expect_err("Discord said ERROR, so this must not report success");

        // The message carries Discord's own words, because "it failed" without
        // them sends the reader back to a packet capture.
        assert!(err.contains("refused"), "got: {err}");
        assert!(err.contains("4000"), "Discord's code must survive: {err}");
        assert!(err.contains("Invalid activity"), "Discord's message must survive: {err}");

        // And the activity really was sent -- this is a refusal, not a
        // failure to transmit.
        assert_eq!(rx.recv().unwrap()["cmd"], "SET_ACTIVITY");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presence_set_fails_cleanly_when_nothing_is_listening() {
        // The normal case: Discord is not running. This must return an
        // error promptly, not hang and not panic.
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("cordial-discord-absent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);

        let mut presence = DiscordPresence::new();
        let payload = PresencePayload::parse(&json!({"client_id": snowflake()})).unwrap();
        let err = presence.set(&payload).expect_err("nothing is listening on any candidate socket");
        assert!(err.contains("not running"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
