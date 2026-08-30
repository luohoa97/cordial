# Instances, launch handling, and multiple accounts

**Status:** design. Nothing here is built. Depends on Phase 2 (activity lifecycle)
and Phase 3 (instance manager).
**Related:** ADR-002, spec §4.2, §5, §9b

---

## 1. What was asked for

Three behaviours, which turn out to be one mechanism:

1. **Closing the window actually quits.** Pressing X exits the game rather than
   backgrounding it, and leaving an experience can close the client entirely.
2. **The browser is the Roblox browser again.** `roblox://` links open in the web
   browser rather than being swallowed by the installed app.
3. **Multiple instances, each with its own account** — using Roblox's own web
   session handling rather than anything Cordial invents, and without disturbing
   the session of whichever account you actually care about.

The third falls out of the first two almost for free. That is the interesting part.

## 2. Why "disable the app" and "multi-account" are the same feature

Roblox's web flow already knows how to hand a specific account's session to a
client. Press Play on the website and the page emits a `roblox://`-family URI
carrying a short-lived authentication ticket minted for **that browser session**.
The client consumes the ticket and joins as that account.

The account therefore comes from *the browser session that pressed Play*, not from
anything stored in the app. Which means:

- A browser profile, container tab, or private window holds its own
  `.ROBLOSECURITY` cookie, so it is its own account.
- Each Play press produces a ticket scoped to that session.
- If Cordial routes each incoming URI to a **new instance with its own data
  directory**, several accounts run at once and never share state.

Multi-account is not a feature Cordial builds. It is what happens when instance
isolation already works and the launch path is honest about where the ticket
came from.

**And the main account stays safe for the same reason.** The way an alt normally
clobbers your main is that both write the same on-disk session. With per-instance
data directories there is no shared file to clobber: the alt's instance never sees
the main's cookie jar, and closing it cannot log the main out.

> **To confirm before building.** The `roblox://` URI format and ticket semantics
> have not been verified against the current client — only that
> `com.roblox.client.ActivityProtocolLaunch` exists and is the declared handler
> (see [`framework-api-inventory.md`](../framework-api-inventory.md) §3.5). The
> claim that a ticket is single-use and session-scoped is how the flow is
> understood to work, not something measured. Verify before the design depends on
> it, because if a ticket is reusable or account-agnostic the isolation argument
> weakens.

## 3. Where each piece lives

| Behaviour | Layer | Mechanism |
|---|---|---|
| X quits the instance | Framework + Core | window close → `Activity.onDestroy` → instance teardown |
| Close after leaving an experience | Core | `onLeave` (§9a) → teardown, behind a setting. **Not built**: no such event exists; `gameDidLeave` in `android_classes.cpp` is where it would hang off. |
| Browser handles `roblox://` | **Core setting** | desktop MIME registration — see §5 |
| A launch URI starts an instance | Core (instance manager) | one URI → one instance, own data dir |
| Multiple accounts | falls out of the above | per-instance data dir = per-instance cookie jar |

Nothing here needs a new subsystem, and nothing touches the Roblox process.

### 3.1 X quitting is a real framework-layer job, not a preference

On Android the window manager does not own process lifetime; an Activity is
backgrounded, not killed, and the app decides when to die. Desktop users expect
the opposite, and expect it strongly enough that "X didn't close it" reads as a
bug rather than a platform difference.

Cordial gets to define that mapping because it implements the window and Activity
layers. Window close destroys the Activity, the Activity's destruction ends the
instance, and the instance's death is what closes the window for real. That
sequence has to be honest in both directions — an instance that dies for any other
reason must also take its window with it — which is why it belongs with the
activity work in Phase 2 rather than being bolted on afterwards.

## 4. Instances

An instance is a Roblox process plus everything it is allowed to see:

```
~/.var/app/<app-id>/data/instances/<instance-id>/
    data/         the app's private storage — cookies, cache, settings
    logs/         the engine's own log. §9a planned onJoin / onLeave / onLogLine
                  off it; none was built. `bloxstrap_rpc` parses it now.
```

Separate data directory, separate namespace, separate lifetime. Refcounted, per
§9b. The instance id is Cordial's, not Roblox's, and is never derived from the
account — a plugin that can see instance ids must not thereby learn which accounts
are signed in.

**One process per instance is the natural shape here.** The Windows client
enforces single-instance with a mutex, which is why Windows launchers have to
defeat it; nothing in this runtime imposes that, because each instance is a
separate process with its own loader state. Whether the *client* imposes its own
single-instance check is unverified and worth testing early — it is the one thing
that could make this materially harder.

## 5. `roblox://` handling is a core setting, not a plugin capability

Registering as the system handler for a URI scheme is a change to the user's
desktop, made outside Cordial's sandbox and persisting after Cordial exits. By
ADR-002's reasoning that puts it out of a plugin's reach: a capability that
rewrites system associations is not meaningfully enforceable and not something a
user can be expected to consent to inside a plugin install prompt.

So it is a core setting with three states:

| Setting | `x-scheme-handler/roblox` | Effect |
|---|---|---|
| **Cordial handles links** | Cordial | Links launch an instance directly |
| **Browser handles links** | left alone / restored | Links open on the web, as before Cordial |
| **Ask** | Cordial | Core shows the chooser and the user picks |

"Browser handles links" is the state that makes §2 work: the website keeps the
launch flow, and Cordial is the thing the ticket eventually reaches.

Two things to get right, both of which are easy to get wrong quietly:

- **Restoring must actually restore.** Record what handled the scheme before
  Cordial claimed it, and put that back — do not merely unset the association and
  leave the user with nothing.
- **Never claim it silently at install.** A launcher that grabs a scheme handler
  without asking is indistinguishable from one that does it maliciously.

## 6. Settings

Core, because §8's argument applies: every primitive shipped in core is a
category of duplicate plugins that never gets born.

| Setting | Default | Effect |
|---|---|---|
| `closeOnWindowClose` | on | X ends the instance |
| `closeOnLeave` | off | Leaving an experience ends the instance |
| `linkHandler` | ask | Who handles `roblox://` (§5) |
| `newInstancePerLaunch` | off | Each launch URI gets a fresh instance rather than reusing an idle one |

`closeOnLeave` defaults off because it is surprising: leaving an experience to
browse the app is normal, and a client that vanishes when you do it feels broken
until you know the setting exists.

## 7. What this is not

**Not an account manager.** Cordial stores no credentials, no cookies, and no
account list. It never sees a password, and the tickets it forwards are opaque to
it. Everything account-shaped stays in the browser, where the user can already see
and revoke it. Building an account manager would mean holding credentials for
multiple accounts in one place — a much larger security obligation, for a feature
the browser already provides.

**Not a bypass of anything.** Alt accounts are permitted; the web launch flow is
Roblox's own; per-instance data directories are ordinary sandboxing. The one
honest caveat: a lot of simultaneous sessions from one machine can look automated
to anti-abuse systems whether or not it is, and that risk lands on the user's
accounts, not on Cordial. Worth saying once in the UI, without ceremony.

## 7a. Update from the sign-in investigation

Two findings since this was written, both bearing on §2.

**The `roblox://` URIs declared by the APK are game-join-shaped, not
bare-auth-shaped.** That does not break the design — a join ticket still carries
the account that pressed Play — but it weakens the idea that a URI alone can
bring up a *logged-in client with no game to join*. A launcher that wants to open
an instance at the home screen as a particular account may not have a URI for it.
See [`sign-in.md`](sign-in.md).

**Plain login is Lua-rendered and reachable in-client**, verified on screen. So
an instance can sign itself in without a browser at all, which is a second route
to per-instance accounts that this document did not consider: the isolation
argument then rests entirely on separate data directories, and not at all on
ticket semantics. That is a *stronger* position than §2's, because it depends on
something already measured rather than something still unverified.

**Consequence for the account picker.** §7 refuses to be an account manager, and
that still holds. A picker over *instances* — which the user names, and whose
contents Cordial never inspects — gives the Chrome-profile experience without
Cordial learning which account is which. The picker chooses a data directory, not
an identity.

## 8. Open questions

- Verify the `roblox://` URI format and whether tickets are genuinely single-use
  and session-scoped (§2).
- Does the Android client enforce its own single-instance check? (§4)
- Does `ActivityProtocolLaunch` need a full Activity, or can the URI be parsed
  before an instance starts? The second is much cheaper and would let core route
  a launch without booting a client first.
- How does an instance that is already running handle a *second* launch URI for a
  different account? Refuse, or start another instance? Refusing is safer and
  probably right.
- GPU cost. Several concurrent clients is several GL contexts; there will be a
  practical ceiling well below what the design permits, and the UI should not
  pretend otherwise.
