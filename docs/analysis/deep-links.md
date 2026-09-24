# Deep links: what a `roblox://` URL has to reach, and what happens when it does

**Status:** established by running, on Roblox for Android 2.732.1043
(`com.roblox.client`), against build `0.3.0-18-g4cb7624-dirty` — a working tree,
because this is the change that introduced the path, and other agents were
editing `crates/cordial-shell/` in the same tree at the time. Ten launches, all
on `--profile agent-deeplink`, none signed in. Sources: the shipping APK's dex
(`tools/dex_method.py`), `readelf --dyn-syms` over `libroblox.so`, the
`AndroidManifest.xml`, `docs/traces/waydroid-roblox-startup.log.gz`, Roblox's
own `ClientSettings` document, and the engine itself.

**Updated 2026-08-03** by a second session, build `0.3.0-21-gf70ee23-dirty`, five
launches on `--profile agent-dl2`, again none signed in: §6 no longer ends at a
warning. roblox.com's desktop link is now translated into the form §5 measured,
and the translation has a control. §6.3 is a correction — the line Cordial
printed on every desktop link carried the link's authentication ticket.

**Bottom line up front.** The engine never asks anybody for a URL. The URL is
delivered *to* it, and the delivery that works is a message-bus publish:

```text
MessageBus.publishRaw("Linking.detectURL", "{\"url\":\"roblox://…\"}")
```

after which the app shell answers on `Game.launch` with the place named in the
link. Everything else that looked like the entry point is not.

---

## 1. The engine asks for nothing

The starting hypothesis was Android's: `Activity.getIntent()` returning an
`Intent` whose `getData()` is the URL, read from native code over JNI.
`docs/analysis/framework-classes.txt` does list `Landroid/content/Intent;`,
`IntentFilter` and `IntentSender`, which is what made it worth checking.

It is not what happens, on three independent counts:

* **The Waydroid capture.** Every `Intent` line in
  `docs/traces/waydroid-roblox-startup.log.gz` belongs to pids 7363/7387/8049 —
  Google Play services' `BoundBrokerSvc` and the Google app — and none to
  Roblox's own pid 9880. Roblox's only `Intent`-shaped lines are two flag names,
  `UseExplicitStorePackageForIntents` and `EnableAtomicDeferredIntentProcessing`.
  (Search it with `zgrep`; the file is gzipped and a plain `grep -r` over
  `docs/traces/` finds nothing and looks like a clean negative.)
* **A full Cordial launch.** No `Intent`, `Uri` or `getIntent` appears anywhere
  in the run, and `--dump-classes`' 22-class inventory
  (`docs/analysis/observed-java-surface.md`) contains no `android/content/*`
  class at all.
* **The direction of the API.** Every URL-carrying symbol in `libroblox.so` is a
  `Java_*` export — something Java calls, not something the engine asks for.

So the deep link is not a question the engine asks. It is a statement Cordial
has to make. That is the same shape as the cookie jar (`native/cookies.cpp`):
Cordial is the Java side of this app, and the inward calls are the interface.

## 2. The manifest says which schemes exist

`base.apk`'s `AndroidManifest.xml` declares `com.roblox.client.ActivityProtocolLaunch`
alongside the schemes `roblox`, `robloxmobile` and `robloxglobal`, and App Links
for `https` on `www.roblox.com` and `ro.blox.com`. **`roblox-player` is not
among them** — it is the desktop scheme, which is why Sober handles it and why
§6 below matters.

## 3. The four inward calls, and what each one answered

All exported by `libroblox.so` and declared in the dex:

| Native | Signature | What it did, measured |
|---|---|---|
| `JNIBaseUrlProtocol.maybeHandleColdStartProtocolLaunch` | `(String)Z` | returned **false** for a game link |
| `JNIWebLoginProtocol.maybeHandleColdStartProtocolLaunch` | `(String)Z` | returned **false** for a game link |
| `JNILinkingProtocol.nativeReportReceived` | `(String,String)V` and `(String,String,Z)V` | returned cleanly, **changed nothing observable** |
| `MessageBus.publishRaw` | `(String,String)V` | **this is the one** — §5 |

The two `maybeHandle…` natives are worth keeping in the sequence even though
they say no: they are asked first, they answer honestly, and they are the
base-URL-switch and web-login special cases. A link Cordial cannot place is a
link one of them may yet claim, and their boolean is the only self-describing
answer in this whole area.

`nativeReportReceived` is not called by Cordial. It accepted the URL without
complaint and moved neither `Game.launch` nor `isColdStartDeeplinkToGame()`, and
its second argument would have to be invented — the neighbouring
`getAttributionUrlKey` and the AppsFlyer `DeepLinkResult` classes in the same
dex suggest it is the advertising-attribution channel, which is **INFERRED** and
was not chased.

## 4. The protocol's own vocabulary, read out of the running engine

`JNILinkingProtocol` is otherwise a wall of zero-argument `String` getters.
Calling them (`CORDIAL_DEEPLINK_PROBE=1`) is how the protocol was learned from a
running engine rather than guessed at from symbol names:

```text
getProtocolName             -> "Linking"
getOpenURLId                -> "openURL"
getOpenURLRequestId         -> "Linking.openURLRequest"
getOpenURLResponseId        -> "Linking.openURLResponse"
getDetectURLId              -> "Linking.detectURL"
getPendingURLId             -> "Linking.pendingURL"
getRegisterURLId            -> "Linking.registerURL"
getIsURLRegisteredId        -> "isURLRegistered"
getIsURLRegisteredRequestId -> "Linking.isURLRegisteredRequest"
getHandleEngineURLId        -> "Linking.handleEngineURL"
getHandleLuaURLId           -> "Linking.handleLuaURL"
getHandlePlatformURLId      -> "Linking.handlePlatformURL"
getUrlKey                   -> "url"
getMatchedUrlKey            -> "matchedUrl"
getAttributionUrlKey        -> "attributionUrl"
JNIExperienceProtocol.getLaunchId -> "Game.launch"
```

`Game.launch` is the important one: it is what the app shell publishes when it
wants an experience launched, so it is the observable that says a link was
understood rather than merely accepted. `MessageBus.getLastRaw(id)` reads it
back, which is how any of this is checkable without implementing a `RawCallback`
class for the engine to call into.

## 5. What works

Publishing the URL on `Linking.detectURL` during bring-up — after
`nativeAppBridgeV2InitWithParams`, which is what builds the protocol machinery,
and before `nativeAppBridgeStartLuaAppDM`:

```text
[deeplink] (cold start) Game.launch before publishing: None
[deeplink] (cold start) isColdStartDeeplinkToGame before publishing: Some(false)
[deeplink] (cold start) published Linking.detectURL
[deeplink] (cold start) Game.launch after publishing: None
...
[roblox] app ready: PlatformAccountRouter
[deeplink] (app ready) Game.launch is:
  Some("{\"placeId\":1818,\"referralPage\":\"DeepLink\",\"joinAttemptId\":\"fe7bec78-…\"}")
[deeplink] the app shell asked to launch an experience; the link reached the engine
```

with `--join-url 'roblox://experiences/start?placeId=1818'`. Reproduced on two
consecutive runs (`joinAttemptId` differs, `placeId` and `referralPage` do not).
The engine parsed the URL itself: `placeId` and `referralPage: "DeepLink"` are
its words, not Cordial's, and Cordial passed the URL through as one opaque
string.

**Two things this pins down.**

*The publish is what carries it.* The control is
`CORDIAL_DEEPLINK_NO_PUBLISH=1` — same link, same launch, same everything, with
the one `publishRaw` call suppressed. `Game.launch` stays empty and
`isColdStartDeeplinkToGame()` stays false at both sampling points.

*The effect is asynchronous, and cold start is early enough.* Immediately after
the publish, nothing has changed: the Lua app shell does not exist yet. By the
first `APP_READY` the answer is there. So the message is queued and acted on
when the shell comes up, and Cordial does not need to wait to publish — only to
report. Publishing a second time after `APP_READY` was tried and produces a
*second* `Game.launch` with a second `joinAttemptId`, which is why the code
publishes once and reads back later rather than publishing twice.

`isColdStartDeeplinkToGame()`, the eleven-byte getter `ActivityNativeMain`
consults on Android between initialising the app bridge and starting the app
shell, goes `false -> true` across the same delivery. It is a second, independent
witness to the same event.

## 6. What the engine will not take: `roblox-player://`, and what is done about it

The engine carries its own pattern for what a game link looks like, as the
client setting `FStringGameLaunchLinkURL`:

```text
(?:(?:https?://\w+\.roblox(?:labs)?\.com…/games/start\?)|(?:roblox(?:mobile)?://(?:experiences/start\?)?))
(?:(?:id=\d+)|(?:placeid=\d+)|(?:launchData=…)|(?:linkCode=…)|(?:accessCode=…)|
 (?:reservedServerAccessCode=…)|(?:joinAttemptId=…)|(?:joinAttemptOrigin=…)|…)+
```

`roblox(?:mobile)?://` — and no `roblox-player`. Measured: the same link under
`roblox-player://experiences/start?placeId=1818` produces **no** `Game.launch`
and leaves the flag false.

This matters more than it sounds. `roblox-player://` is what roblox.com's play
button emits on desktop, in a different format again, and it is the handler
Cordial is taking over from Sober. **Taking that handler away from Sober without
handling it is the failure this whole investigation was told to avoid.**

### 6.1 The translation, and that it works

**Established by running on 2026-08-03, build `0.3.0-21-gf70ee23-dirty`** — a
working tree, because this is the change that introduced the translation. Five
launches, all `--profile agent-dl2`, none signed in, all with synthetic links.

The desktop format is not a URL query. It is a version, then `+`-separated
`key:value` pairs:

```text
roblox-player:1+launchmode:play+gameinfo:<ticket>+placelauncherurl:<percent-encoded>
              +launchtime:…+browsertrackerid:…+robloxLocale:…+gameLocale:…
```

The `placelauncherurl` decodes to a `PlaceLauncher.ashx` request whose *query*
names a `placeId`. `crates/cordial-runtime/src/deeplink.rs`'s `translate` takes
that id and only that id into the mobile form measured in §5:

```text
[deeplink] delivering roblox-player:// with launchmode, gameinfo, placelauncherurl,
    launchtime, browsertrackerid, robloxLocale, gameLocale (316 bytes; values not shown)
[deeplink] translated the desktop link to roblox:// with placeId (39 bytes; values not shown)
[deeplink] its launcher URL also carried request, browserTrackerId, isPlayTogetherGame,
    which Cordial does not carry across
...
[roblox] app ready: PlatformAccountRouter
[deeplink] (app ready) Game.launch is:
  Some("{\"placeId\":1818,\"referralPage\":\"DeepLink\",\"joinAttemptId\":\"434c89f0-…\"}")
[deeplink] the app shell asked to launch an experience; the link reached the engine
```

Reproduced on two runs (`joinAttemptId` differs; `placeId` and `referralPage` do
not). **The control is `CORDIAL_DEEPLINK_NO_TRANSLATE=1`** — the identical
desktop link, the identical launch, the rewrite suppressed and the desktop link
handed to the engine as it arrived. Twice:

```text
[deeplink] not translating (CORDIAL_DEEPLINK_NO_TRANSLATE)
[deeplink] warning: this engine's own link pattern (FStringGameLaunchLinkURL) matches
    roblox:// and robloxmobile:// only, so a roblox-player:// link is not expected to
    reach an experience
[deeplink] (app ready) Game.launch is: None
[deeplink] the app shell is up and nothing asked to launch an experience
```

So the rewrite, and nothing else in the launch, is what makes a desktop link
reach the engine.

### 6.2 What is deliberately not carried, and the objection that stands

**`gameinfo` is dropped.** It is a one-time authentication ticket the *desktop*
client redeems; this engine is the Android client, whose authentication is the
session it already holds, and which has no such ticket in any link it accepts.
Dropping it is consistent with what this engine is. **It is not established that
a join succeeds without it**, because a join needs a signed-in account and none
was used — see §7. What is established is that the app shell asks for the right
place. The distance between those two is the same gap §7 already records for
`roblox://`, and translating does not widen it.

**Update, 2026-09-25: a link that picks a particular server is now carried,
not refused.** The engine's own grammar above admits `accessCode`, `linkCode`
and `reservedServerAccessCode`, so `translate` carries those (and a `gameId`
or `jobId`, as `gameInstanceId`) under the engine's names; a value that cannot
be passed on safely still refuses the link. The paragraph below is the
reasoning as it stood when only the place id could be carried.

**A link that picks a particular server is refused rather than flattened.** A
private-server, reserved-server or join-a-running-game link names a place *and*
an `accessCode`, `linkCode`, `reservedServerAccessCode`, `gameId` or `jobId`.
Carrying only the place id out of one of those would produce a link that joins —
into a different server from the one clicked, which is worse than not joining, on
the same argument AGENTS.md makes about a stub that returns success. Measured,
with `accessCode` in the launcher query:

```text
[deeplink] not translating this link: its placelauncherurl carries accesscode, which
    picks a particular server rather than an experience; carrying only the place id
    would join a different game from the one clicked
[deeplink] (app ready) Game.launch is: None
```

The launcher's own `request` kind is deliberately not consulted. It would have to
be enumerated from names nothing here has measured, and every kind worth refusing
carries one of those parameters or no `placeId` at all — `RequestFollowUser`
names a user and is refused for having no place.

Everything else in the launcher query is named in the log and dropped.
`launchmode` must be `play`: `app` and `edit` are not requests to join anything.

**This refusal has an exception now, and it is a different code path rather
than a loosened check.** It exists because this section's own input — a
browser-clicked `roblox-player:` link — only ever gives Cordial a bare
`placeId` to work with, so carrying one of the parameters above out of it
*alone* would join the wrong server. Issue #40/#34 found a second, later way
a join reaches this file: intercepting the Servers-list Join button's own
`Roblox.Hybrid.Game.launchGame` call (`docs/analysis/app-bridge.md`), which
hands over the whole payload the page built, including a real `instanceId`
naming one server. Nothing is lost there the way it would be here, so
`deeplink::publish_hybrid_game_launch` carries the instance forward as
`gameInstanceId` instead of refusing — see that function's own doc and
`crates/cordial-runtime/src/deeplink.rs`'s `SERVER_SELECTING` comment for the
full reasoning. This section's own refusal is unchanged and still correct for
the input it guards.

### 6.3 `describe` printed the ticket, and now does not

**A correction to what this document and the code both implied.** `JoinUrl::describe`
claimed to report parameter *names* and never values, and its own test asserted
it. It split on `&` and `?` only — the query form's separators. The desktop form
uses `+` and `:`, so the whole payload came back as a single "parameter name",
and the line Cordial printed on every desktop link was:

```text
[deeplink] delivering roblox-player:// with 1+launchmode:play+gameinfo:<ticket>+…
    (N bytes; values not shown)
```

The ticket, under the words "values not shown", on the only input that carries a
credential. Observed by calling `describe` on a synthetic desktop link before
anything was changed.

It now parses whichever form the link is in, drops any token carrying no
separator, and requires an identifier shape of anything it prints — a `+` inside
a value cannot turn a slice of that value into a "name". Five full launch logs
were grepped afterwards for the synthetic ticket, the launcher host and the
encoded launcher URL: none appears in any of them, including the two control runs
that hand the untranslated desktop link to the engine.

**Still open, and outside this change's files:** `cordial-shell`'s
`deep_link::summarise` truncates a link to 64 characters for the launcher banner,
and `roblox-player:1+launchmode:play+gameinfo:` is 40 of them — so roughly the
first two dozen characters of a real ticket would reach that banner. Not a whole
credential, and not verified against a real link, but it is the same mistake in
the other crate.

## 7. Verified, inferred, and not established

**Verified by running, this session:**

- The engine asks for no `Intent`, `Uri` or URL (§1).
- Both `maybeHandleColdStartProtocolLaunch` natives return false for a game link.
- `nativeReportReceived` changes nothing observable.
- Every string in §4, read from the running engine.
- `Linking.detectURL` + `{"url": …}` produces `Game.launch` naming the place,
  twice, with a suppressed-publish control that produces neither it nor the flag.
- `isColdStartDeeplinkToGame()` moves false -> true across the same delivery.
- `roblox-player://` produces neither.
- A desktop `roblox-player:1+…+placelauncherurl:…` link, rewritten to
  `roblox://experiences/start?placeId=<id>`, produces a `Game.launch` naming that
  place — twice, with `CORDIAL_DEEPLINK_NO_TRANSLATE=1` as the control producing
  neither, twice (§6.1).
- A launcher query carrying `accessCode` is refused rather than translated, and
  the refusal names the parameter and not its value (§6.2).
- `JoinUrl::describe` printed the desktop form's `gameinfo` ticket before this
  change and does not after (§6.3).
- **A join through this publish mechanism does land in the requested game,
  and in the specific requested server, not just the place.** Issue #40/#34's
  fix (`docs/analysis/app-bridge.md`) extends this same `Linking.detectURL`
  publish with `gameInstanceId` and the join-attempt fields, on a real
  signed-in click from a game's Servers list. 8/8 attempts across two public
  games, each confirmed against the engine's own join log naming the exact
  server id requested. This is the first signed-in measurement anything in
  this document had; the gaps below that a signed-in account would close are
  closed by it specifically for this path, not for `--join-url`,
  `roblox-player://` or the `gameinfo` ticket question, which remain open.

**Inferred, not verified:**

- That `nativeReportReceived` is the advertising-attribution channel. Its
  neighbours in the dex say so; nothing was measured.
- That `Linking.detectURL` is the *right* message rather than merely a
  sufficient one. The three siblings were published immediately after it in one
  run and produced no further `Game.launch`, which is consistent with detectURL
  having already done the work, and does not prove the others are inert.
- That `JNIBaseUrlProtocol.init(Context)` needs calling at all. It is driven
  with a bare object stand-in, succeeds, and asks nothing of it.

**Not established:**

- **Whether any of this actually joins an experience.** `Game.launch` is the app
  shell *asking*. A join needs a signed-in account, and no account was used —
  every run here ends at `app ready: Landing`, which is where a signed-out
  client belongs. This is the single largest gap and it cannot be closed without
  an account.
- **Whether a translated desktop link joins without its `gameinfo` ticket.**
  Same gap, and it is the honest objection to §6.1: the ticket is dropped on the
  reasoning that this engine is the Android client and has none, which is an
  argument and not a measurement. One signed-in launch closes it.
- Whether `launchData`, `linkCode`, `accessCode` and the rest survive the trip.
  Only `placeId` was exercised; the engine's own pattern lists the others, so
  they are expected to, which is not the same as having seen it. §6.2 refuses to
  translate a desktop link that needs any of them for exactly that reason.
