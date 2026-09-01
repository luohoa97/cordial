//! `roblox-player://` and `roblox://` — a URL from a browser click, into the
//! engine.
//!
//! The shell registers Cordial as the handler for those two schemes and hands
//! the client whatever the browser passed, as `cordial-run --join-url <url>`.
//! Everything after that argument is here.
//!
//! **What the engine asks for: nothing.** This was the first question, because
//! a protocol handler that accepts a click and drops it is worse than none at
//! all. The engine never asks for an `Intent`, a `Uri` or a URL — Roblox's own
//! Java receives it on Android, and the URL crosses into `libroblox.so` only
//! because Java *calls inward*. Cordial is the Java side here, so those inward
//! calls are the interface. `docs/analysis/deep-links.md` records how that was
//! established; `native/deeplink.cpp` holds the calls themselves.
//!
//! **What works, measured.** Publishing `{"url": …}` on the engine's own
//! `Linking.detectURL` message makes the app shell answer with a `Game.launch`
//! naming the place from the link:
//!
//! ```text
//! [deeplink] (cold start) published Linking.detectURL
//! [deeplink] (app ready) Game.launch is:
//!     Some("{\"placeId\":1818,\"referralPage\":\"DeepLink\",\"joinAttemptId\":…}")
//! ```
//!
//! Twice, on two consecutive runs, with `roblox://experiences/start?placeId=1818`.
//! The control is `CORDIAL_DEEPLINK_NO_PUBLISH=1`: same link, same launch,
//! publish suppressed, and `Game.launch` stays empty with
//! `isColdStartDeeplinkToGame()` false at both sampling points.
//!
//! **What the engine will not take, also measured.** A `roblox-player://` link
//! produces no `Game.launch` at all. The engine's own pattern for a game link —
//! `FStringGameLaunchLinkURL`, a client setting — admits `roblox://` and
//! `robloxmobile://` and nothing else. That is the scheme roblox.com's desktop
//! play button emits, and Cordial is the registered handler for it, so a click
//! on the web site arrives in a format this engine cannot read.
//!
//! **So it is translated.** [`translate`] takes the desktop launcher's format
//! apart — a version, then `+`-separated `key:value` pairs — percent-decodes its
//! `placelauncherurl`, and carries the `placeId` out of that launcher query into
//! `roblox://experiences/start?placeId=<id>`, which is the exact string measured
//! to work. `CORDIAL_DEEPLINK_NO_TRANSLATE=1` is the control. Nothing else is
//! carried: the desktop link's `gameinfo` is a one-time authentication ticket,
//! and a launcher query that names a *particular server* rather than an
//! experience is refused outright rather than flattened into a join somewhere
//! else.
//!
//! **This used to say the Android client "has no use for" that ticket, and
//! that was asserted without a citation.** It is what makes browser launches
//! sign you in on Windows -- Bloxstrap hands the desktop client a `gameinfo`
//! and the client redeems it -- so whether Cordial can do the same is a fair
//! question and the answer here was a guess.
//!
//! What is actually known, and it is less than the old sentence claimed:
//!
//! - This build's `libroblox.so` contains `AuthTicket`, `redeem`, `ticket=`
//!   and `/v1/authentication-ticket`. The Android client is not innocent of
//!   ticket machinery.
//! - **That proves the strings exist and nothing else.** The endpoint is also
//!   the one that *issues* a ticket, which is what the mobile client's own
//!   quick sign-in needs, so finding it says nothing about whether a *desktop*
//!   ticket arriving in a deeplink would be redeemed. AGENTS.md's first rule
//!   is about exactly this inference, and nine consecutive conclusions drawn
//!   from this binary have been wrong.
//! - Sober faces the same question, runs the same Android client, and does not
//!   do it: its sign-in paths are a password, Quick Sign-in by device code,
//!   and a browser-login window (their issues #1243, #1434, #1619). No ticket
//!   redemption appears anywhere in their tracker.
//!
//! So the honest state is *unverified*, not *impossible*. The experiment that
//! would settle it: carry `gameinfo` through the translation, click a play
//! button on roblox.com while signed out in Cordial, and see whether the
//! client ends up signed in. It needs an account and a real browser click,
//! which is why it has not been run rather than why it cannot be.
//!
//! Until then the ticket is dropped, which is also the conservative choice: it
//! is a live credential, and forwarding one into an engine that may ignore it
//! buys nothing and widens where it has been.
//!
//! When the translation refuses, the behaviour is what it was before it existed:
//! Cordial says which parameter stopped it and warns that the link is not going
//! to reach an experience, rather than waiting for the silence to speak.
//!
//! **Not verified: an actual join.** Reaching `Game.launch` is the engine
//! asking to launch an experience; whether it then joins one needs a signed-in
//! account, and no account was used here. Every run above ends at
//! `app ready: Landing`, which is where a signed-out client belongs.
//!
//! **The URL is hostile input.** It arrives from a browser, which got it from a
//! page, which got it from anywhere. It is length-capped, checked for one of
//! the two schemes Cordial claims, and restricted to printable ASCII before
//! anything else sees it — the last of those specifically so a URL cannot carry
//! a newline into the log and forge a line. It is never interpolated into a
//! shell command, never used to build a filesystem path, and never used as a
//! format string. It goes to exactly one place: a `String` argument of a JNI
//! native.
//!
//! **Its contents are not printed.** A `roblox://` link is almost entirely
//! query, and a Roblox query can carry a private-server `accessCode` or
//! `linkCode` — a capability, not a preference. The desktop form is worse: its
//! `gameinfo` is a live one-time authentication ticket. Cordial elides web-view
//! URLs' queries for exactly this reason (`native/android_classes.cpp`), so this
//! module reports the scheme, the parameter *names*, and the length, and never
//! a value. [`JoinUrl::describe`] carries the scar of getting that wrong once.

use cordial_linker_sys as linker;

/// `com.roblox.universalapp.linking.JNIBaseUrlProtocol`.
const BASE_URL: &str = "com/roblox/universalapp/linking/JNIBaseUrlProtocol";
/// `com.roblox.universalapp.linking.JNIWebLoginProtocol`.
const WEB_LOGIN: &str = "com/roblox/universalapp/linking/JNIWebLoginProtocol";
/// `com.roblox.universalapp.linking.JNILinkingProtocol`.
const LINKING: &str = "com/roblox/universalapp/linking/JNILinkingProtocol";
/// `com.roblox.universalapp.messagebus.MessageBus`.
const BUS: &str = "com/roblox/universalapp/messagebus/MessageBus";

/// The engine's own names for the messages a URL travels on, read out of a
/// running engine by [`probe`] rather than guessed:
///
/// ```text
/// getProtocolName      -> "Linking"
/// getDetectURLId       -> "Linking.detectURL"
/// getPendingURLId      -> "Linking.pendingURL"
/// getHandleLuaURLId    -> "Linking.handleLuaURL"
/// getHandlePlatformURLId -> "Linking.handlePlatformURL"
/// getUrlKey            -> "url"
/// ```
///
/// `Linking.detectURL` is the one Cordial publishes on. Measured: with the app
/// shell up, publishing a game link on it produced a `Game.launch` message
/// synchronously, and the three siblings published straight afterwards produced
/// no further one. The engine's own `isColdStartDeeplinkToGame()` goes from
/// false to true across the same publish, and stays false when the publish is
/// suppressed and nothing else changes.
const DETECT_URL: &str = "Linking.detectURL";

/// `JNIExperienceProtocol.getLaunchId()` answers with this, read from a running
/// engine. It is the message the app shell publishes when it wants an
/// experience launched, so it is the observable that says a link was understood.
const GAME_LAUNCH: &str = "Game.launch";

/// The longest URL Cordial will carry.
///
/// A `roblox://` link's `launchData` is developer-supplied and can be long;
/// Roblox's own documented ceiling for it is 200 characters, and the rest of a
/// join link is short. 2048 is well clear of any real link and well under the
/// ~8 kB a browser would hand over, which is the point: the cap exists so that
/// a megabyte of query cannot be walked into a JNI `String` on the strength of
/// somebody else's web page.
const MAX_LEN: usize = 2048;

/// The two schemes Cordial claims as a handler.
///
/// Both, because Roblox's own site emits both: `roblox-player://` from the web
/// site's play button and `roblox://` from older links and from the mobile
/// deep-link surface. A handler that took only one would leave half the links
/// on the machine pointing at nothing.
const SCHEMES: [&str; 2] = ["roblox-player", "roblox"];

/// A URL that has passed [`validate`]. Construct it no other way.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinUrl {
    raw: String,
    scheme: &'static str,
}

impl JoinUrl {
    /// The URL itself, for the one caller that hands it to the engine.
    ///
    /// Named rather than a `Deref`, and separate from `Display`, so that every
    /// place the raw value escapes is greppable — the same shape
    /// [`crate::cookies::Jar::expose`] uses for the same reason.
    pub fn expose(&self) -> &str {
        &self.raw
    }

    /// Which of the two schemes this arrived on.
    pub fn scheme(&self) -> &'static str {
        self.scheme
    }

    /// What is safe to print: the scheme, the parameter names, and the length.
    ///
    /// Never a parameter value. `accessCode` and `linkCode` are private-server
    /// capabilities, `launchData` is arbitrary developer payload, and the
    /// desktop form's `gameinfo` is a one-time authentication ticket; a log line
    /// that carried any of them would hand a shoulder-surfer or a pasted
    /// terminal transcript a working credential.
    ///
    /// **This printed the ticket until 2026-08-03**, and it printed it under the
    /// words "values not shown". The desktop form separates its parameters with
    /// `+` and `:`, neither of which the old splitter knew about, so a whole
    /// `roblox-player:1+launchmode:play+gameinfo:…` payload came out as a single
    /// "parameter name". That is the exact link this module is for, so the bug
    /// was live on the only input that carries a credential.
    pub fn describe(&self) -> String {
        let names = self.parameter_names();
        if names.is_empty() {
            return format!("{}:// ({} bytes, no parameters)", self.scheme, self.raw.len());
        }
        format!(
            "{}:// with {} ({} bytes; values not shown)",
            self.scheme,
            names.join(", "),
            self.raw.len()
        )
    }

    /// The parameter names in this link, in the order they appear, and nothing
    /// else.
    ///
    /// Fails closed in both directions. A token that carries no separator is
    /// dropped rather than reported as a name, because in the desktop form a
    /// separatorless token is either the leading version number or a fragment of
    /// a value that happened to contain the separator this form splits on — and
    /// `gameinfo` is the value most likely to. Anything that does not look like
    /// an identifier is dropped for the same reason.
    fn parameter_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = match self.desktop_payload() {
            // `key:value` joined by `+`. The version number leads and carries no
            // colon, so it drops out here without needing a special case.
            Some(payload) => payload
                .split('+')
                .filter_map(|t| t.split_once(':').map(|(k, _)| k))
                .filter(|k| is_identifier(k))
                .collect(),
            // `?a=1&b=2`, with the path before the first `?` carrying no `=` and
            // so contributing nothing.
            None => self
                .payload()
                .split(['&', '?'])
                .filter_map(|t| t.split_once('=').map(|(k, _)| k))
                .filter(|k| is_identifier(k))
                .collect(),
        };
        names.dedup();
        names
    }

    /// Everything after the scheme, with any leading slashes removed.
    ///
    /// The slashes are stripped rather than required because both shapes reach
    /// here: `roblox://experiences/start?…` has two, the desktop form has none,
    /// and a desktop link that has been through GIO has three — `GFile::uri`
    /// turns `roblox-player:1+…` into `roblox-player:///1+…`, which
    /// `cordial-shell`'s own tests pin down. The shell hands over `argv`
    /// precisely so that does not happen, but a link typed by hand or produced
    /// by some other launcher can still arrive reshaped.
    fn payload(&self) -> &str {
        self.raw
            .split_once(':')
            .map(|(_, rest)| rest.trim_start_matches('/'))
            .unwrap_or_default()
    }

    /// The payload, when this link is in the desktop launcher's form.
    ///
    /// ```text
    /// roblox-player:1+launchmode:play+gameinfo:<ticket>+placelauncherurl:<encoded>+…
    /// ```
    ///
    /// A version number, then `+`-separated `key:value` pairs. Recognised by
    /// that leading all-digits token rather than by the scheme alone, because
    /// `roblox-player://placeId=1818` is also a link somebody can produce and it
    /// is the query shape, not this one. Anything that does not match falls
    /// through to the query shape, which is the safe direction: the query
    /// splitter treats an unrecognised desktop payload as one nameless token and
    /// prints nothing of it.
    fn desktop_payload(&self) -> Option<&str> {
        if self.scheme != "roblox-player" {
            return None;
        }
        let payload = self.payload();
        let (version, _) = payload.split_once('+')?;
        let numbered = !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit());
        numbered.then_some(payload)
    }
}

/// Whether a name is safe to print as a parameter name.
///
/// The names in a link come from somebody else's web page, so "it appeared
/// before a separator" is not by itself a reason to believe a string is a key.
/// Requiring an identifier shape, and a short one, is what keeps a fragment of a
/// value out of a log line if a value ever contains a separator this module
/// splits on.
fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl std::fmt::Debug for JoinUrl {
    /// Reports the shape, never the value — so a stray `{:?}` in a future
    /// caller cannot leak a join link into a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JoinUrl({})", self.describe())
    }
}

/// Accept a URL, or say precisely why not.
///
/// Deliberately strict and deliberately dull. Every rejection is a string the
/// user sees, because the alternative — a handler that starts, says nothing and
/// lands on the home page — is the failure this whole path exists to avoid.
pub fn validate(raw: &str) -> Result<JoinUrl, String> {
    if raw.is_empty() {
        return Err("--join-url was given an empty string".into());
    }
    if raw.len() > MAX_LEN {
        return Err(format!(
            "--join-url is {} bytes and the limit is {MAX_LEN}",
            raw.len()
        ));
    }
    // Printable ASCII only. Rejecting control characters is what stops a URL
    // forging a log line, and rejecting spaces and non-ASCII bytes is what a
    // browser would already have percent-encoded — so anything else here did
    // not come from a browser.
    if let Some(bad) = raw.bytes().position(|b| !(0x21..=0x7e).contains(&b)) {
        return Err(format!(
            "--join-url has a character that is not printable ASCII at byte {bad}"
        ));
    }
    let (scheme, _) = raw
        .split_once(':')
        .ok_or_else(|| "--join-url has no scheme".to_string())?;
    // Schemes are case-insensitive (RFC 3986 §3.1), and a browser is entitled
    // to hand over `Roblox-Player://`.
    let lower = scheme.to_ascii_lowercase();
    let scheme = SCHEMES
        .iter()
        .find(|s| **s == lower)
        .ok_or_else(|| {
            format!(
                "--join-url has scheme {lower:?}; Cordial handles {}",
                SCHEMES.join(" and ")
            )
        })?;
    Ok(JoinUrl {
        raw: raw.to_string(),
        scheme,
    })
}

/// Parameters in a `PlaceLauncher.ashx` query that choose a *particular server*
/// rather than an experience.
///
/// Lower-cased, and compared lower-cased, because the desktop launcher's own
/// keys are not consistently cased.
///
/// These are the reason [`translate`] can refuse. A private-server link, a
/// reserved-server link and a "join this running game" link all name a place
/// *and* one of these, so a translation that carried only the place id would
/// produce a link that joins — into a different server from the one clicked.
/// Refusing keeps the failure where somebody can see it, which is the same
/// reasoning `native/opensles.cpp` applies to a stub that could lie.
///
/// The desktop launcher's `request` kind is deliberately not consulted. It would
/// have to be enumerated from names nothing here has measured, and every kind
/// worth refusing carries one of these or no `placeId` at all, so the parameters
/// answer the question the request kind would have.
const SERVER_SELECTING: [&str; 5] =
    ["accesscode", "linkcode", "reservedserveraccesscode", "gameid", "jobid"];

/// What [`translate`] made of the link.
#[derive(Debug)]
pub enum Translated {
    /// Already in a form the engine's own pattern matches. Deliver it unchanged.
    AsIs,
    /// A desktop link, rewritten into the mobile form. `dropped` names the
    /// launcher-URL parameters that were not carried, so that a link which
    /// behaves differently from the click says which parameter explains it.
    To { url: JoinUrl, dropped: Vec<String> },
    /// A desktop link this will not translate, and why — in parameter names,
    /// never values.
    Refused(String),
}

/// Rewrite roblox.com's desktop launch link into the mobile one the engine
/// matches, or say why not.
///
/// **The problem.** roblox.com's play button emits `roblox-player:`, in a format
/// that is not a URL query at all:
///
/// ```text
/// roblox-player:1+launchmode:play+gameinfo:<ticket>+placelauncherurl:<encoded>+launchtime:…
/// ```
///
/// and the engine's own pattern for a game link — the client setting
/// `FStringGameLaunchLinkURL` — admits `roblox://` and `robloxmobile://` and
/// nothing else. Measured: that link produces no `Game.launch`. Cordial is now
/// the registered handler for the scheme, so the click had to reach the engine
/// or stop being claimed.
///
/// **What is carried, and what is not.** The `placelauncherurl` decodes to a
/// `PlaceLauncher.ashx` request whose query names a `placeId`. That id, and only
/// that id, is carried into `roblox://experiences/start?placeId=<id>` — the
/// exact shape measured to produce a `Game.launch`. Everything else in the
/// launcher query is named in the log and dropped; nothing is guessed at, and
/// [`SERVER_SELECTING`] is the set whose presence stops the translation instead.
///
/// **`gameinfo` is deliberately dropped, and that is the open question.** It is
/// a one-time authentication ticket the desktop client redeems, and the Android
/// client this engine came from has no such thing — its authentication is the
/// session the client already holds. So dropping it is consistent with what the
/// engine is, and it is **not established** that a join succeeds without it,
/// because verifying a join needs a signed-in account and none was used. What is
/// established is that the translated link reaches the engine and the app shell
/// asks for the place; see `docs/analysis/deep-links.md`.
pub fn translate(url: &JoinUrl) -> Translated {
    let Some(payload) = url.desktop_payload() else {
        return Translated::AsIs;
    };

    let mut launch_mode = None;
    let mut launcher = None;
    for token in payload.split('+') {
        // A token with no colon is the leading version number, or a fragment of
        // a value that contained a `+`. Neither is a parameter, and guessing
        // which is which is exactly what this must not do.
        let Some((key, value)) = token.split_once(':') else {
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "launchmode" => launch_mode = Some(value),
            "placelauncherurl" => launcher = Some(value),
            _ => {}
        }
    }

    // `play` is the only mode that means "join an experience". `app` opens the
    // client and `edit` is Studio, and turning either into a join would send
    // somebody somewhere they did not click. The value is not printed: it is
    // short and harmless today, but this whole module's rule is that a value
    // from a browser click does not reach a log line, and one exception is how
    // the rule stops being one.
    if let Some(mode) = launch_mode {
        if !mode.eq_ignore_ascii_case("play") {
            return Translated::Refused(
                "this link's launchmode is not play, so it is not a request to join an experience"
                    .into(),
            );
        }
    }

    let Some(launcher) = launcher else {
        return Translated::Refused(
            "this link carries no placelauncherurl, so there is no place id in it to translate"
                .into(),
        );
    };
    let launcher = match percent_decode(launcher) {
        Ok(v) => v,
        Err(e) => return Translated::Refused(format!("its placelauncherurl {e}")),
    };

    let query = launcher.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let mut place = None;
    let mut blocking: Vec<&str> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let lower = key.to_ascii_lowercase();
        if lower == "placeid" {
            place = Some(value);
        } else if let Some(known) = SERVER_SELECTING.iter().find(|s| **s == lower) {
            // The canonical spelling rather than the one that arrived, so that
            // the sentence a user reads is Cordial's and not a web page's.
            blocking.push(known);
        } else if is_identifier(key) {
            dropped.push(key.to_string());
        }
    }

    if !blocking.is_empty() {
        return Translated::Refused(format!(
            "its placelauncherurl carries {}, which picks a particular server rather than an \
             experience; carrying only the place id would join a different game from the one \
             clicked",
            blocking.join(", ")
        ));
    }
    let Some(place) = place else {
        return Translated::Refused(
            "its placelauncherurl names no placeId, so there is nothing to join".into(),
        );
    };
    // Digits only, and short. This is what makes the line below safe to build by
    // formatting: a place id that is anything other than a run of digits cannot
    // reach the synthesised link, so nothing from the browser can add a
    // parameter to it. Roblox's place ids are 64-bit, so 20 digits is every one
    // that will ever exist and then some.
    if place.is_empty() || place.len() > 20 || !place.bytes().all(|b| b.is_ascii_digit()) {
        return Translated::Refused(
            "its placelauncherurl's placeId is not a number, and Cordial will not invent one"
                .into(),
        );
    }

    // `roblox://experiences/start?placeId=<id>` rather than any of the shorter
    // forms the engine's pattern also admits, because this is the exact string
    // that was measured to produce a `Game.launch` naming the place.
    match validate(&format!("roblox://experiences/start?placeId={place}")) {
        Ok(url) => Translated::To { url, dropped },
        // Unreachable as long as the digit check above holds, and reported
        // rather than unwrapped because a panic here would be a browser click
        // taking the client down.
        Err(e) => Translated::Refused(format!(
            "the link it translates to is not one Cordial takes: {e}"
        )),
    }
}

/// Undo the percent-encoding on a desktop link's `placelauncherurl`.
///
/// Hand-rolled rather than a dependency because it decodes one field, and strict
/// rather than lenient because this arrives from a browser click: a malformed
/// escape means the link is not the shape this claims to understand, and the
/// honest answer to that is to refuse the translation rather than to salvage
/// what parses. The decoded bytes are held to the same printable-ASCII rule
/// [`validate`] applies to the link itself, with the space allowed because a
/// launcher query can legitimately contain one once decoded.
fn percent_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .ok_or_else(|| "ends in a percent escape that is cut short".to_string())?;
        let byte = u8::from_str_radix(hex, 16)
            .map_err(|_| "has a percent escape that is not two hexadecimal digits".to_string())?;
        if !(0x20..=0x7e).contains(&byte) {
            return Err("has a percent escape for a character that is not printable".into());
        }
        out.push(byte as char);
        i += 3;
    }
    Ok(out)
}

/// What was *done* with the link at cold start.
///
/// Deliberately not a verdict on whether the link worked. Nothing at this point
/// in a launch knows that: the app shell does not exist yet, and the answer
/// arrives at `APP_READY`, which is [`tick`]'s job to report. An enum that said
/// "not handled" here would be a lie with a thirty-second head start on the
/// truth, and that is exactly the shape of report this path exists to avoid.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A `maybeHandleColdStartProtocolLaunch` claimed the URL outright. Nothing
    /// further is published and nothing further is expected.
    Claimed(&'static str),
    /// On the engine's message bus. Whether the app shell acts on it is
    /// reported later, by [`tick`].
    Published,
    /// The build exports none of it — a Roblox update moved the natives, which
    /// is a different problem from a link nobody wanted.
    NoSurface,
}

/// Hand the URL to the engine.
///
/// Called during bring-up rather than after the app shell settles. That is
/// measured rather than assumed: publishing at cold start produces nothing
/// immediately and a `Game.launch` by the first `APP_READY`, so the bus holds
/// the message until there is something to act on it. Publishing again after
/// the shell is up produces a *second* `Game.launch` with a second
/// `joinAttemptId`, so it is done once, here.
///
/// Placed after `nativeAppBridgeV2InitWithParams` for the same reason the
/// cookie restore is: the protocol machinery it talks to does not exist until
/// that call has built it.
///
/// Everything below [`translate`] works on the translated link rather than the
/// one that arrived, which is deliberate: a desktop link's `gameinfo` ticket
/// then never crosses into the engine at all, and there is one link in play
/// rather than two that could disagree about which place was clicked.
pub fn deliver(lib: linker::Library, url: &JoinUrl) -> Outcome {
    println!("[deeplink] delivering {}", url.describe());

    // `CORDIAL_DEEPLINK_NO_TRANSLATE=1` is the control for the translation, the
    // way `CORDIAL_DEEPLINK_NO_PUBLISH=1` is the control for the publish: same
    // link, same launch, the rewrite suppressed, and the desktop link goes to
    // the engine as it arrived.
    let translated = if std::env::var_os("CORDIAL_DEEPLINK_NO_TRANSLATE").is_some() {
        println!("[deeplink] not translating (CORDIAL_DEEPLINK_NO_TRANSLATE)");
        Translated::AsIs
    } else {
        translate(url)
    };
    let url = match &translated {
        Translated::AsIs => url,
        Translated::To { url: rewritten, dropped } => {
            println!("[deeplink] translated the desktop link to {}", rewritten.describe());
            if !dropped.is_empty() {
                // Named rather than silently discarded. A link that joins the
                // right place and behaves differently from the click is the
                // failure this line exists to explain, and the parameter that
                // explains it is always in this list.
                println!(
                    "[deeplink] its launcher URL also carried {}, which Cordial does not carry \
                     across",
                    dropped.join(", ")
                );
            }
            rewritten
        }
        Translated::Refused(why) => {
            println!("[deeplink] not translating this link: {why}");
            url
        }
    };

    // Said up front rather than after thirty seconds of nothing happening.
    //
    // The engine carries its own pattern for what a game link looks like, as
    // the client setting `FStringGameLaunchLinkURL`, and that pattern admits
    // `roblox://` and `robloxmobile://` and no other scheme. Measured: the same
    // link published under `roblox-player://` produces no `Game.launch` at all,
    // where under `roblox://` it produces one naming the place. So reaching this
    // with a `roblox-player://` link still in hand means the translation above
    // declined it, and the click is not going to reach an experience.
    if url.scheme() == "roblox-player" {
        println!(
            "[deeplink] warning: this engine's own link pattern (FStringGameLaunchLinkURL) \
             matches roblox:// and robloxmobile:// only, so a roblox-player:// link is not \
             expected to reach an experience"
        );
    }

    if std::env::var_os("CORDIAL_DEEPLINK_PROBE").is_some() {
        probe(lib);
    }

    // `init(Context)` first on each class that has one. Whether it is required
    // before `maybeHandleColdStartProtocolLaunch` is not established; it is
    // driven because a native that needs setting up and did not get it is the
    // kind of silence this path cannot afford, and because a failure here is
    // printed rather than assumed away.
    let mut any_surface = false;
    for (class, tag) in [(BASE_URL, "JNIBaseUrlProtocol"), (WEB_LOGIN, "JNIWebLoginProtocol")] {
        let sym = format!("Java_{}_init", class.replace('/', "_"));
        if let Some(f) = lib.symbol(&sym) {
            match linker::game_activity::protocol_init(f, class) {
                Ok(()) => println!("[deeplink] {tag}.init ok"),
                Err(e) => println!("[deeplink] {tag}.init failed: {e}"),
            }
        }

        let sym = format!(
            "Java_{}_maybeHandleColdStartProtocolLaunch",
            class.replace('/', "_")
        );
        let Some(f) = lib.symbol(&sym) else {
            println!("[deeplink] {tag}.maybeHandleColdStartProtocolLaunch is not exported");
            continue;
        };
        any_surface = true;
        match linker::game_activity::cold_start_protocol_launch(f, class, url.expose()) {
            Ok(true) => {
                println!("[deeplink] {tag} took the link");
                return Outcome::Claimed(tag);
            }
            Ok(false) => println!("[deeplink] {tag} did not claim this link"),
            Err(e) => println!("[deeplink] {tag} failed: {e}"),
        }
    }

    // Neither claimed it, so it goes on the message bus, which is where the
    // engine's own deep-link handling lives. The engine holds the pattern it
    // matches game links against as a client setting — `FStringGameLaunchLinkURL`
    // accepts `roblox://` and `robloxmobile://`, with or without
    // `experiences/start?`, carrying `placeid`, `linkCode`, `accessCode`,
    // `launchData` and the rest — so the URL is parsed inside the engine and
    // Cordial does not have to understand it.
    if publish_url(lib, url, "cold start") {
        any_surface = true;
    }

    if !any_surface {
        return Outcome::NoSurface;
    }

    // Reading the result back is deferred, not the publish. The bus takes the
    // message at cold start and the app shell acts on it when it comes up, so
    // the answer does not exist yet at this point in the sequence; [`tick`]
    // reports it once `APP_READY` arrives.
    arm(lib, url);
    println!("[deeplink] handed to the engine; the app shell will act on it when it starts");
    Outcome::Published
}

/// The link, waiting for something to confirm the engine acted on it.
///
/// A `Mutex` rather than a channel because there is exactly one of these per
/// process and it is consumed once: the looper takes it, reports, and leaves
/// `None` behind, so the second and third `APP_READY` of an ordinary launch
/// (`PlatformAccountRouter`, `Startup`, `Landing` — all three fire) cannot
/// report the same link three times.
static ARMED: std::sync::Mutex<Option<(linker::Library, JoinUrl)>> = std::sync::Mutex::new(None);

/// Set by the engine's own `APP_READY`, read by the looper thread.
static APP_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The engine's thread calls this from inside its own notification callback, so
/// it does nothing but raise a flag. Every JNI call this module makes is made
/// from the looper thread, which is where [`tick`] runs.
extern "C" fn on_app_ready(_state: *const std::ffi::c_char) {
    APP_READY.store(true, std::sync::atomic::Ordering::Release);
}

fn arm(lib: linker::Library, url: &JoinUrl) {
    *ARMED.lock().expect("no other thread panics holding this") = Some((lib, url.clone()));
    linker::game_activity::app_ready_set_sink(Some(on_app_ready));
}

/// Called from the looper each pass. Does nothing at all until the engine has
/// reported `APP_READY` and a link is waiting.
///
/// This reports; it does not re-deliver. Publishing a second time produces a
/// second `Game.launch` with a second `joinAttemptId` — measured — and two join
/// attempts for one click is a worse failure than a slow one.
pub fn tick() {
    if !APP_READY.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let taken = ARMED.lock().expect("no other thread panics holding this").take();
    let Some((lib, _url)) = taken else { return };

    let launched = lib
        .symbol("Java_com_roblox_universalapp_messagebus_MessageBus_getLastRaw")
        .and_then(|f| read_last(f, GAME_LAUNCH));
    // The payload names a place and carries a join attempt id the engine
    // minted, so it is diagnostic output rather than something every launch
    // should print.
    if std::env::var_os("CORDIAL_DEEPLINK_PROBE").is_some() {
        println!("[deeplink] (app ready) {GAME_LAUNCH} is: {launched:?}");
    }
    match (launched, cold_start_flag(lib)) {
        (Some(_), _) => println!(
            "[deeplink] the app shell asked to launch an experience; the link reached the engine"
        ),
        (None, Some(true)) => println!(
            "[deeplink] the engine registered a deep link, but has not asked to launch an \
             experience"
        ),
        (None, _) => println!(
            "[deeplink] the app shell is up and nothing asked to launch an experience — this \
             link did not reach an experience. Signing in is required before a join can proceed"
        ),
    }
}

/// Put the URL on the engine's message bus.
///
/// Returns whether the bus was reachable at all — not whether the link worked.
/// The difference matters: a build that does not export `publishRaw` is a
/// Roblox update to chase, and a bus that took the message and did nothing is
/// a link nobody claimed.
///
/// `CORDIAL_DEEPLINK_NO_PUBLISH=1` suppresses the publish and is the control:
/// with it set, and everything else identical, `Game.launch` stays empty and
/// `isColdStartDeeplinkToGame()` stays false. That is what establishes that
/// this publish, and not something else in the launch, is what carries the link.
fn publish_url(lib: linker::Library, url: &JoinUrl, phase: &str) -> bool {
    let Some(publish) = lib.symbol("Java_com_roblox_universalapp_messagebus_MessageBus_publishRaw")
    else {
        println!("[deeplink] MessageBus.publishRaw is not exported by this build");
        return false;
    };
    let last = lib.symbol("Java_com_roblox_universalapp_messagebus_MessageBus_getLastRaw");

    // `getUrlKey()` answers `"url"`, read from the running engine rather than
    // guessed. Built by hand rather than with a JSON library because there is
    // exactly one field — but escaped, because [`validate`] admits every
    // printable ASCII byte including `"` and `\`, and this payload is built
    // from text somebody else's web page chose.
    let payload = format!("{{\"url\":\"{}\"}}", escape_json(url.expose()));

    let verbose = std::env::var_os("CORDIAL_DEEPLINK_PROBE").is_some();
    if verbose {
        let before = last.and_then(|f| read_last(f, GAME_LAUNCH));
        println!("[deeplink] ({phase}) {GAME_LAUNCH} before publishing: {before:?}");
        println!(
            "[deeplink] ({phase}) isColdStartDeeplinkToGame before publishing: {:?}",
            cold_start_flag(lib)
        );
    }

    if std::env::var_os("CORDIAL_DEEPLINK_NO_PUBLISH").is_some() {
        println!("[deeplink] ({phase}) not publishing (CORDIAL_DEEPLINK_NO_PUBLISH)");
        return true;
    }
    match linker::game_activity::call_static_strings(publish, BUS, &[DETECT_URL, &payload]) {
        Ok(()) => println!("[deeplink] ({phase}) published {DETECT_URL}"),
        Err(e) => {
            println!("[deeplink] ({phase}) publishing {DETECT_URL} failed: {e}");
            return false;
        }
    }
    if verbose {
        if let Some(f) = last {
            println!(
                "[deeplink] ({phase}) {GAME_LAUNCH} after publishing: {:?}",
                read_last(f, GAME_LAUNCH)
            );
        }
    }
    true
}

/// `MessageBus.getLastRaw(id)` — the last payload published on a message id,
/// or `None` when there has never been one.
fn read_last(f: *mut std::ffi::c_void, id: &str) -> Option<String> {
    match linker::game_activity::call_static_string_ret_string(f, BUS, id) {
        Ok(v) if v.is_empty() => None,
        Ok(v) => Some(v),
        Err(e) => {
            println!("[deeplink] getLastRaw({id}) failed: {e}");
            None
        }
    }
}

/// The two characters that would break out of a JSON string literal.
///
/// [`validate`] admits every printable ASCII byte, which includes `"` and `\`,
/// so a URL is entitled to carry both and the payload above is built from
/// attacker-influenced text. Escaping is cheaper than a JSON dependency and
/// exact for this one field, because nothing else in printable ASCII needs it.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `NativeGLInterface.isColdStartDeeplinkToGame()` — the engine's own answer to
/// "was this launch a deep link into an experience".
///
/// An eleven-byte tail call to an internal getter, so it reads engine state and
/// decides nothing. On Android it is what `ActivityNativeMain` consults between
/// initialising the app bridge and starting the Lua app shell, which places it
/// exactly where Cordial hands the URL over.
fn cold_start_flag(lib: linker::Library) -> Option<bool> {
    let f = lib.symbol("Java_com_roblox_engine_jni_NativeGLInterface_isColdStartDeeplinkToGame")?;
    linker::game_activity::call_static_bare_bool(f, "com/roblox/engine/jni/NativeGLInterface").ok()
}

/// Read the linking protocol's own vocabulary out of the running engine.
///
/// Every one of these is a zero-argument `String` getter on
/// `JNILinkingProtocol` — the message names and JSON field names the engine
/// uses on its own MessageBus. Reading them is how the protocol is learned from
/// a running engine rather than guessed at from symbol names, which is the
/// mistake this project has paid for nine times over (AGENTS.md).
///
/// `CORDIAL_DEEPLINK_PROBE=1`. Diagnostic only: it passes nothing in.
fn probe(lib: linker::Library) {
    const GETTERS: [&str; 18] = [
        "getProtocolName",
        "getOpenURLId",
        "getOpenURLRequestId",
        "getOpenURLResponseId",
        "getDetectURLId",
        "getPendingURLId",
        "getRegisterURLId",
        "getIsURLRegisteredId",
        "getIsURLRegisteredRequestId",
        "getIsURLRegisteredResponseId",
        "getHandleEngineURLId",
        "getHandleLuaURLId",
        "getHandlePlatformURLId",
        "getUrlKey",
        "getMatchedUrlKey",
        "getAttributionUrlKey",
        "getIsRegisteredKey",
        "getSuccessKey",
    ];
    let prefix = format!("Java_{}_", LINKING.replace('/', "_"));
    for name in GETTERS {
        match lib.symbol(&format!("{prefix}{name}")) {
            None => println!("[deeplink probe] {name}: not exported"),
            Some(f) => match linker::game_activity::call_static_ret_string(f, LINKING) {
                Ok(v) => println!("[deeplink probe] {name} -> {v:?}"),
                Err(e) => println!("[deeplink probe] {name} failed: {e}"),
            },
        }
    }
    match lib.symbol("Java_com_roblox_universalapp_experience_JNIExperienceProtocol_getLaunchId") {
        None => println!("[deeplink probe] JNIExperienceProtocol.getLaunchId: not exported"),
        Some(f) => match linker::game_activity::call_static_ret_string(
            f,
            "com/roblox/universalapp/experience/JNIExperienceProtocol",
        ) {
            Ok(v) => println!("[deeplink probe] JNIExperienceProtocol.getLaunchId -> {v:?}"),
            Err(e) => println!("[deeplink probe] JNIExperienceProtocol.getLaunchId failed: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link of the shape the web site emits, with a place that belongs to
    /// nobody. No real account, server or access code appears in this file.
    const SAMPLE: &str = "roblox-player://placeId=1818&launchData=hello";

    #[test]
    fn accepts_both_schemes() {
        assert_eq!(validate(SAMPLE).unwrap().scheme(), "roblox-player");
        assert_eq!(validate("roblox://placeId=1818").unwrap().scheme(), "roblox");
    }

    #[test]
    fn scheme_is_case_insensitive() {
        // RFC 3986 §3.1. A browser is entitled to hand over any case, and a
        // handler that only took the lowercase one would drop real links.
        assert_eq!(validate("ROBLOX-PLAYER://placeId=1").unwrap().scheme(), "roblox-player");
    }

    #[test]
    fn rejects_other_schemes() {
        for bad in ["http://roblox.com", "file:///etc/passwd", "javascript:alert(1)"] {
            assert!(validate(bad).is_err(), "{bad} should not be accepted");
        }
    }

    #[test]
    fn rejects_a_missing_scheme() {
        assert!(validate("placeId=1818").is_err());
    }

    /// The reason the character check exists. A URL carrying a newline could
    /// otherwise write its own line into Cordial's log, and the line above it
    /// would be indistinguishable from one Cordial wrote.
    #[test]
    fn rejects_control_characters() {
        assert!(validate("roblox://placeId=1\n[deeplink] joined").is_err());
        assert!(validate("roblox://place\0id=1").is_err());
        assert!(validate("roblox://placeId=1 2").is_err());
    }

    #[test]
    fn rejects_an_overlong_url() {
        let long = format!("roblox://launchData={}", "a".repeat(MAX_LEN));
        assert!(validate(&long).is_err());
    }

    #[test]
    fn rejects_an_empty_url() {
        assert!(validate("").is_err());
    }

    /// The privacy rule, as a test rather than a comment: a description names
    /// the parameters and never their values.
    #[test]
    fn describe_never_shows_a_value() {
        let u = validate("roblox-player://placeId=1818&accessCode=SECRETVALUE").unwrap();
        let d = u.describe();
        assert!(d.contains("placeId"), "{d}");
        assert!(d.contains("accessCode"), "{d}");
        assert!(!d.contains("SECRETVALUE"), "{d}");
        assert!(!d.contains("1818"), "{d}");
        assert!(!format!("{u:?}").contains("SECRETVALUE"));
    }

    #[test]
    fn expose_returns_the_url_unchanged() {
        assert_eq!(validate(SAMPLE).unwrap().expose(), SAMPLE);
    }

    /// The bus payload is one JSON field built from a URL somebody else's web
    /// page chose, and a quote is a printable ASCII character that [`validate`]
    /// admits. Without this, `roblox://a=","x":"` would add a field to a message
    /// going into the engine.
    #[test]
    fn a_quote_cannot_add_a_field_to_the_payload() {
        let url = validate(r#"roblox://placeId=1","evil":"yes"#).unwrap();
        let payload = format!("{{\"url\":\"{}\"}}", escape_json(url.expose()));
        assert!(!payload.contains(r#","evil":"yes""#), "{payload}");
        assert!(payload.contains(r#"\"evil\""#), "{payload}");
        assert_eq!(payload.matches("\":\"").count(), 1, "{payload}");
    }

    #[test]
    fn a_backslash_cannot_escape_the_closing_quote() {
        assert_eq!(escape_json(r"a\"), r"a\\");
    }

    /// A desktop link of the shape roblox.com's play button emits, built here
    /// rather than captured. `SYNTHETIC-NOT-A-TICKET` is not a ticket and 1818
    /// belongs to nobody; no real link, ticket or account appears in this file
    /// or anywhere else in this repository.
    fn desktop(launcher_query: &str) -> String {
        let encoded = launcher_query
            .replace('%', "%25")
            .replace(':', "%3A")
            .replace('/', "%2F")
            .replace('?', "%3F")
            .replace('&', "%26")
            .replace('=', "%3D");
        format!(
            "roblox-player:1+launchmode:play+gameinfo:SYNTHETIC-NOT-A-TICKET\
             +placelauncherurl:{encoded}+launchtime:1754179200000+browsertrackerid:1\
             +robloxLocale:en_us+gameLocale:en_us"
        )
    }

    const LAUNCHER: &str =
        "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGame\
         &browserTrackerId=1&placeId=1818&isPlayTogetherGame=false";

    /// **The bug this fix exists for, as a test.** Until 2026-08-03 `describe`
    /// split only on `&` and `?`, so a desktop payload — which uses `+` and `:`
    /// — came back as one giant "parameter name" carrying the `gameinfo`
    /// ticket, printed under the words "values not shown". The ticket is a live
    /// credential and this is the only input that carries one.
    #[test]
    fn a_desktop_link_never_prints_its_ticket() {
        let u = validate(&desktop(LAUNCHER)).unwrap();
        for rendered in [u.describe(), format!("{u:?}")] {
            assert!(!rendered.contains("SYNTHETIC"), "{rendered}");
            assert!(!rendered.contains("1818"), "{rendered}");
            assert!(!rendered.contains("assetgame"), "{rendered}");
            assert!(rendered.contains("gameinfo"), "{rendered}");
            assert!(rendered.contains("placelauncherurl"), "{rendered}");
            assert!(rendered.contains("launchmode"), "{rendered}");
        }
    }

    /// The desktop format splits on `+`, and a ticket is entitled to contain
    /// one. A splitter that reported every `+`-separated token's leading text as
    /// a parameter name would print a slice of the ticket; requiring a `:` and
    /// an identifier shape is what stops it.
    #[test]
    fn a_plus_inside_a_ticket_does_not_become_a_parameter_name() {
        let u = validate("roblox-player:1+gameinfo:AAAA+BBBB==+launchmode:play").unwrap();
        let d = u.describe();
        assert!(!d.contains("BBBB"), "{d}");
        assert!(d.contains("gameinfo") && d.contains("launchmode"), "{d}");
    }

    #[test]
    fn a_desktop_link_becomes_the_mobile_link_that_was_measured_to_work() {
        match translate(&validate(&desktop(LAUNCHER)).unwrap()) {
            Translated::To { url, dropped } => {
                assert_eq!(url.expose(), "roblox://experiences/start?placeId=1818");
                assert_eq!(url.scheme(), "roblox");
                // Named, not silently swallowed. `placeId` is carried and so is
                // absent from this list.
                assert!(dropped.contains(&"request".to_string()), "{dropped:?}");
                assert!(dropped.contains(&"browserTrackerId".to_string()), "{dropped:?}");
                assert!(!dropped.contains(&"placeId".to_string()), "{dropped:?}");
            }
            other => panic!("a plain play link should translate: {other:?}"),
        }
    }

    /// GIO turns `roblox-player:1+…` into `roblox-player:///1+…`, which
    /// `cordial-shell`'s own tripwire pins down. The shell reads `argv` so that
    /// does not happen, but a link from a hand-typed command or another launcher
    /// can still arrive reshaped, and it is the same link.
    #[test]
    fn a_desktop_link_reshaped_into_a_url_still_translates() {
        let reshaped = desktop(LAUNCHER).replacen("roblox-player:", "roblox-player:///", 1);
        match translate(&validate(&reshaped).unwrap()) {
            Translated::To { url, .. } => {
                assert_eq!(url.expose(), "roblox://experiences/start?placeId=1818")
            }
            other => panic!("{other:?}"),
        }
    }

    /// The honest refusal. A private-server link names a place *and* an access
    /// code, so carrying only the place id would produce a link that joins —
    /// into the public server, not the one that was clicked. That is worse than
    /// not joining, which is the whole argument in AGENTS.md against a stub that
    /// returns success.
    #[test]
    fn a_link_that_picks_a_particular_server_is_refused_by_name() {
        for (query, named) in [
            ("https://x/PlaceLauncher.ashx?placeId=1818&accessCode=abc", "accesscode"),
            ("https://x/PlaceLauncher.ashx?placeId=1818&linkCode=abc", "linkcode"),
            ("https://x/PlaceLauncher.ashx?placeId=1818&gameId=abc", "gameid"),
            ("https://x/PlaceLauncher.ashx?placeId=1818&jobId=abc", "jobid"),
            (
                "https://x/PlaceLauncher.ashx?placeId=1818&reservedServerAccessCode=abc",
                "reservedserveraccesscode",
            ),
        ] {
            match translate(&validate(&desktop(query)).unwrap()) {
                Translated::Refused(why) => {
                    assert!(why.contains(named), "{why}");
                    assert!(!why.contains("abc"), "the reason must not carry a value: {why}");
                }
                other => panic!("{query} should be refused: {other:?}"),
            }
        }
    }

    #[test]
    fn a_launcher_url_with_no_place_id_is_refused_rather_than_invented() {
        // `RequestFollowUser` is the shape this catches without having to
        // enumerate the launcher's request kinds: it names a user, not a place.
        let l = "https://x/PlaceLauncher.ashx?request=RequestFollowUser&userId=1";
        assert!(matches!(
            translate(&validate(&desktop(l)).unwrap()),
            Translated::Refused(_)
        ));
    }

    /// The check that makes the synthesised link safe to build by formatting.
    /// Without it a place id could carry `&` or a quote into a string that goes
    /// on to become one field of a JSON payload inside the engine.
    #[test]
    fn a_place_id_that_is_not_a_number_is_refused() {
        for bad in ["1818x", "1818%26accessCode%3Dabc", "", "9".repeat(21).as_str()] {
            let l = format!("https://x/PlaceLauncher.ashx?placeId={bad}");
            match translate(&validate(&desktop(&l)).unwrap()) {
                Translated::Refused(_) => {}
                other => panic!("placeId={bad:?} should be refused: {other:?}"),
            }
        }
    }

    #[test]
    fn a_link_that_is_not_asking_to_play_is_refused() {
        let link = desktop(LAUNCHER).replace("launchmode:play", "launchmode:edit");
        match translate(&validate(&link).unwrap()) {
            Translated::Refused(why) => assert!(why.contains("launchmode"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_desktop_link_without_a_launcher_url_is_refused() {
        let link = "roblox-player:1+launchmode:play+gameinfo:SYNTHETIC-NOT-A-TICKET";
        match translate(&validate(link).unwrap()) {
            Translated::Refused(why) => assert!(why.contains("placelauncherurl"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    /// A link already in a form the engine matches is not touched. This is what
    /// keeps the measured `roblox://` path exactly as it was.
    #[test]
    fn a_mobile_link_is_left_alone() {
        for link in [
            "roblox://experiences/start?placeId=1818",
            // A `roblox-player://` link in the query shape rather than the
            // desktop launcher's shape: not something to translate, and it still
            // gets the warning it got before.
            "roblox-player://placeId=1818",
        ] {
            assert!(matches!(translate(&validate(link).unwrap()), Translated::AsIs), "{link}");
        }
    }

    #[test]
    fn a_malformed_percent_escape_refuses_rather_than_salvages() {
        for launcher in ["https%3A%2F%2Fx%3FplaceId%3D1818%2", "https%3A%2Fx%3FplaceId%3D18%ZZ"] {
            let link = format!("roblox-player:1+launchmode:play+placelauncherurl:{launcher}");
            assert!(matches!(
                translate(&validate(&link).unwrap()),
                Translated::Refused(_)
            ));
        }
    }

    #[test]
    fn percent_decoding_is_exact() {
        assert_eq!(percent_decode("a%3Db%26c").unwrap(), "a=b&c");
        assert_eq!(percent_decode("%25").unwrap(), "%");
        assert_eq!(percent_decode("plain").unwrap(), "plain");
        // A decoded control character would be a newline in a log line or a NUL
        // in a JNI string, and neither belongs in a link.
        assert!(percent_decode("a%0Ab").is_err());
        assert!(percent_decode("a%00b").is_err());
    }

    /// The translated link goes through the same gate as one that arrived, so
    /// the JSON payload built from it is the same shape the measured run used.
    #[test]
    fn the_translated_link_survives_the_payload_builder() {
        let Translated::To { url, .. } = translate(&validate(&desktop(LAUNCHER)).unwrap()) else {
            panic!("should translate");
        };
        let payload = format!("{{\"url\":\"{}\"}}", escape_json(url.expose()));
        assert_eq!(payload, r#"{"url":"roblox://experiences/start?placeId=1818"}"#);
    }
}
