# ADR-012: A profile is storage; an instance is a window

**Status:** accepted
**Extended by:** [ADR-013](ADR-013-per-profile-configuration.md)
**Related:** [ADR-002](ADR-002-core-shell-and-ui-handoff.md), [ADR-005](ADR-005-flag-service.md), [ADR-006](ADR-006-plugin-events-and-first-party.md)

## Decision

Two words, kept distinct, because conflating them designs the wrong thing:

| | |
|---|---|
| **Instance** | a running Cordial process — a window. Roblox's own usage. |
| **Profile** | a directory holding one account's Roblox storage, plugin set and flag overrides. |

An instance *runs* a profile. Profiles live at
`$XDG_DATA_HOME/cordial/profiles/<name>/`, and **a profile may be held by at most
one instance at a time**, enforced by an advisory lock rather than by convention.

The "plugin set and flag overrides" half of that definition was aspirational
when this was written: only storage actually moved, and grants and flags stayed
global for several months afterwards.
[ADR-013](ADR-013-per-profile-configuration.md) finishes it and records why the
grants file in particular could not stay global — an approval given in a
throwaway profile was silently in force in the profile someone plays on.

Multiple instances is launching the process more than once, each against a
different profile.

## Why the distinction is load-bearing

The earlier path was `cordial/instances/default/run`, which held storage. That is
the wrong word for what is in it, and the wrong word here is not cosmetic: it
invites the reader to conclude that a second window needs a second data
directory, or — worse — that one directory can back two windows. The first is
false and the second is dangerous.

Roblox means *window* by instance. Matching that is not deference to their
vocabulary; it is that users asking for "multi-instance" are asking for more
windows, and the feature should be named after what they asked for.

## Why locking, rather than allowing a profile to be shared

Nothing structurally prevents two Cordial processes opening the same profile.
Unlike Fishstrap on Windows, there is no singleton mutex to defeat — each Cordial
process is genuinely independent, which makes multi-instance nearly free here and
is a real advantage of controlling the storage location ourselves.

That same freedom is the hazard. Two instances on one profile are two processes
writing one `appData` and one cookie store concurrently. Roblox's storage is not
built for that, and the realistic outcomes are a corrupted profile or one session
silently invalidating the other. Neither presents as "you did something
unsupported"; both present as bugs in Cordial.

So the lock is taken on the profile directory for the lifetime of the instance,
`LOCK_EX | LOCK_NB`. A second attempt fails immediately with a message naming the
profile, rather than blocking or racing.

**Advisory, not mandatory, and that is honest.** `flock` does not stop a process
that never asks. It stops *Cordial* from doing it by accident, which is the
actual failure mode — a user double-clicking the launcher twice, not an adversary
bypassing a lock.

### Correction: for three weeks only the launcher took it

> Everything above described the intent. From the day this was written
> (2026-08-01) to 2026-08-22 it was half implemented, and the missing half was
> the half this project's own instructions tell people to use. Kept in place
> rather than rewritten, because "the decision was right and the code did not do
> it" is a different failure from "the decision was wrong", and only one of them
> is fixed by editing a paragraph.

`launch.rs` built a claim and `Claim::hand_to` passed it to the client it
spawned, so a shell launch was protected. **`cordial-run` contained no claim
code at all.** A hand-run `cordial-run --profile X` — the invocation AGENTS.md
documents, and the one every agent working in this repository is told to type —
took no lock whatsoever. Measured 2026-08-22: four `--profile CordialTest`
engines running at once, not one refused. Found while investigating something
else, and confirmed in the source afterwards rather than the other way round.

So the guarantee this ADR is mostly about held for the launcher's users and not
for its developers, and AGENTS.md meanwhile assured the developers a second
launch "is refused rather than allowed to corrupt Roblox's storage". A
protection that is documented and absent is worse than one nobody claimed: it
is what made the separate data root read as belt-and-braces when it was in fact
the only thing standing between two agents and each other's Roblox storage.

**Fixed by giving the client the other end of the same lock, not a second
lock.** `cordial_shell::profile::claim_for_instance` is what `cordial-run` calls
now, before anything reads or writes the profile:

- handed a descriptor by a launcher, it **adopts** it — verifying against
  `/proc/self/fd` that the descriptor really is this profile's lock file,
  re-asserting the `flock` so the kernel rather than an environment variable is
  the authority, and putting `FD_CLOEXEC` back on;
- handed nothing, it **takes** the lock itself, and a second hand-run client is
  refused by name and by holding process.

The adoption step is not a nicety, and the reason is the one thing about `flock`
that has to be internalised to read any of this code: **a lock belongs to the
open file description, not to the file and not to the process.** A client that
simply called `acquire` would open the lock file a second time and be refused by
the lock it had just inherited — by itself, on every shell launch. Demonstrated
rather than reasoned: with adoption removed, the test that spawns a real client
process reports `profile "default" is already open in another Cordial client`,
and the client it is talking about is itself.

Restoring `FD_CLOEXEC` matters for a reason nobody had noticed. `hand_to` clears
it to get the lock across one `exec`, and nothing had ever put it back, so the
descriptor kept crossing every later one — including the `bwrap` and `deno`
execs `cordial_plugins::sandbox` makes. With the restore removed, the test
observes exactly that: a bare `sleep` grandchild holds the profile after the
client has exited, and the refusal names `sleep 10` as the holder. Whether a
real `deno` sandbox outlives a client long enough to matter is **`INFERRED`** —
what is measured is that *any* process the client execs inherits the lock, and
the sandbox is the client's one exec. Fixed here as a consequence of adopting
the descriptor properly rather than by design.

**Rejected: an escape hatch for running two clients on one profile.** It was
considered, because the case is real — somebody testing wants two windows on one
account. It is refused on this ADR's own terms and on AGENTS.md's. The two
supported ways to run several clients at once already exist, cost nothing, and
are *safe* rather than merely permitted: another profile, or another
`XDG_DATA_HOME`. A `--force` beside them would buy only the unsafe shape, and a
flag that disables a correctness guarantee is copied out of a wiki by people who
do not know what it turns off — at which point the corruption it permits still
presents as a Cordial bug, and the lock has become documentation. The
refusal message therefore spends its space naming the two safe recipes instead.

**Accepted: contributors will experience this as a regression.** Several
clients against `default` has silently worked for as long as anyone here has
been running them, and it stops working now. That is the point, and the refusal
says so in full — what a profile is, that two writers corrupt it, both recipes,
and how to take the profile back — because a one-line "already open" would read
as the tool breaking rather than as the tool doing its job.

## Why not OAuth

Roblox's OAuth 2.0 grants API scopes to third-party applications through the
creator dashboard. It does not issue a play session, and there is no supported
mechanism for a client to authenticate on a user's behalf. Account switching is
therefore not an authentication feature at all: each profile logs in normally and
keeps its own session under its profile's storage key. The configured secret
backend stores that session in the keyring or in the profile directory. Nothing about this design needs
Roblox to grant anything, which is also why it cannot be withdrawn.

## On credentials — originally, why they do not go in a keyring

> **Superseded twice, and now wrong throughout.** The heading is left as it was
> written because a decision record that quietly renames its own conclusions is
> not a record. Credentials *do* go in a keyring; see the second correction at
> the end of this section. The first argument below also rested on a factual
> claim about the engine that turned out to be false, and the decision it
> supported — that Cordial never handles a session token — has been reversed.
> The original reasoning is kept in full both times, because the shape of the
> mistake is the useful part; the corrections follow it in order.

Roblox keeps its session cookie inside the profile. The obvious suggestion is to
put it in the desktop keyring instead, and it is a reasonable instinct, but it is
rejected for three reasons.

**It would make Cordial the custodian of a token it currently never touches.**
The engine writes and reads its own session; Cordial only decides which directory
that happens in. Reading the cookie out, holding it, and writing it back is a
large increase in responsibility for this project, and "Cordial never handles
your credentials" is a property worth more than the encryption would be.

**The protection would be mostly illusory.** The engine reads its cookie from a
file at startup, so a keyring copy would have to be written back to disk before
every launch and would sit there in plaintext for the whole session. Encryption
at rest would apply only while Cordial is not running. Meanwhile the keyring is
unlocked at login, so any process running as the user reads it as easily as it
reads the file.

**The reachable threat is file permissions, and that is fixed instead.**
`create_dir_all` applies the umask, which on a normal desktop yields `0755` — on
a multi-user machine another account could read the session. Profile directories
are therefore forced to `0700`, on creation and on migration, with a test
asserting it. That defends against the case that actually occurs.

Users wanting encryption at rest should use full-disk encryption or
`systemd-homed`, which solve it properly and for everything rather than for one
application's cookie.

### Correction: Cordial is the custodian of the session token

**The engine never writes its cookies to disk.** Both paragraphs above that
describe it doing so — "the engine writes and reads its own session" and "the
engine reads its cookie from a file at startup" — are wrong. A complete
`CORDIAL_TRACE_PATHS=1` inventory of every non-system file the engine opens
contains no cookie jar and no credential store of any kind, and
`grep -rl ROBLOSECURITY` and `grep -rli set-cookie` over real profile trees find
nothing. The engine holds its cookies in memory for the life of the process.

On Android the **Java** side persists them. The Waydroid capture in
`docs/traces/` shows `OnSetCookieHandlerImpl.b(): Updated WebViewCookieHandler
with Cookies from URL ...` among nine cookie lines. Cordial has no Java side, so
nothing persisted anything, and the symptom was the one this project keeps
writing down: sign in, quit, restart the same profile, and you are on the
landing page.

This also disposes of the fix that suggests itself first. A shutdown hook cannot
flush a file that is never written — controlled for directly, by alternating
killed and graceful runs over two passes, which produced no file created or
updated at shutdown that a killed run does not also produce. The graceful
teardown descent is real and works and was never the missing piece.

So Cordial now reads the jar out of the engine through
`NativeSettingsInterface.nativeGetCookiesForDomain`, writes it into the profile,
and hands it back on the next launch through `nativeSetMultipleCookies`. **That
makes Cordial the custodian of a live session token, which the decision above
explicitly set out to avoid.** The trade is accepted because the alternative is
not "Cordial handles no credentials" but "Cordial cannot stay signed in", and
the account switcher this ADR is mostly about is not worth much if every profile
is signed out at every launch.

**What protects it.** *(Superseded by the second correction below: the store is
now an item in `org.freedesktop.secrets`, and a file only where there is no
service. Everything in this paragraph still describes that fallback, and the
`0700` directory still holds everything else about the account.)* The store is a
single file, `cookies`, inside the profile
directory. It is `0600` and the directory is `0700`, applied wherever a profile
is chosen rather than only where it is locked — a hand-started
`cordial-run --profile <name>` previously got the umask's `0755`, which was
survivable when the directory held only Roblox's storage and is not now. It is
written to a temporary file and renamed, so an interrupted write cannot leave
half a token that still parses. The value is carried in a type whose `Debug`
prints its length, so that no diagnostic can print a session by accident, and
nothing in the implementation logs, prints or traces a cookie value at any
verbosity or behind any flag.

**The keyring is still rejected, on the argument that survives.** Only the first
of the three reasons above depended on the false premise. The second stands and
is now the whole case: the token must be handed to the engine in plaintext on
every launch, so a keyring would encrypt it only during the window in which
nothing is reading it, in exchange for an unlock prompt on every start. The
third stands unchanged and is what is actually implemented.

Users wanting encryption at rest should still use full-disk encryption or
`systemd-homed`.

### Second correction: the keyring argument was wrong too, and it was mine

> The paragraph immediately above is superseded. It is kept because the shape of
> the mistake is the useful part, and because it is the second time in one ADR
> that a decision here rested on something nobody had measured.

The project owner's question was "fix the plain text cookies — who does that?",
and they were right. What was on disk was `<profile>/cookies`, mode `0600`,
containing a live `.ROBLOSECURITY` — a bearer token that is whole-account
access — and `<profile>/identity` beside it. A backup, a sync client, a
container mount, a second application running as the same user, or somebody
reading over a shoulder all reach it.

**The argument that kept it there was mine, and both halves of it fail.**

*"A keyring adds an unlock prompt to every launch."* False on this platform, and
falsifiable in about a minute, which is the part that stings. Measured on the
owner's machine: `org.freedesktop.secrets` is up and answering
`org.freedesktop.DBus.Peer.Ping`; `Collections` lists `login` and `session` and
both report `Locked = false`, because the login keyring is unlocked by the
session's own login and nothing has to be typed. Sober — the closest comparable
project — links `libsecret-1.so.0` and exposes `use_libsecret` in its config,
which is a working existence proof that this is ordinary rather than exotic.

*"It protects against nothing extra, because the token has to be handed to the
engine in plaintext regardless."* The premise is true and the conclusion does
not follow from it. A token in the clear **inside a running process** is not a
token in the clear **on disk for ever**. The threats listed above are all reads
of the file, none of them require the process to be running, and none of them
are addressed by `0600` — every one of them acts as the user, and `0600` is
exactly the permission that grants the user. The argument compared the wrong two
states.

**Secret Service, not "GNOME keyring", and the distinction is the whole reason
this is portable.** `org.freedesktop.secrets` is the D-Bus interface;
`gnome-keyring-daemon` implements it on GNOME, KWallet and KeePassXC implement
it elsewhere, and libsecret is a client library for it. Cordial targets the
interface, so it works wherever one is implemented rather than wherever GNOME
is installed.

**Not through libsecret, though libsecret is the obvious client.** `zbus` is
already a dependency of `cordial-runtime`, and `android::accessibility` already
hand-rolls `org.a11y.atspi` over it for the same reason. Linking libsecret would
add glib and gobject and a build-time `libsecret-devel` which is *not installed
on the owner's machine*, where `pkg-config --libs libsecret-1` resolves to a
Homebrew prefix under `/home/linuxbrew` — a release binary linked against that
runs on one computer. The API reached is identical; only the client differs.

**A stored session is a convenience and never a prerequisite.** This is the hard
constraint, and it is the owner's: *users cannot play Roblox if they have not
unlocked their keyring*. Losing the stored session must degrade to "sign in
again" and never to "the client will not start", and never to a dialog standing
between somebody and the game. So `crates/cordial-shell/src/secrets.rs`, shared
with the runtime through its existing `secrets` module:

- reads the default collection's `Locked` property and **never calls `Unlock`**.
  A locked collection is "not available", not an error and not a prompt. With
  auto-login the login keyring is *never* unlocked, because the password that
  would unlock it was never typed, so locked is the ordinary state on those
  machines rather than an edge case;
- runs every D-Bus call on its own thread behind a two-second probe timeout and
  a five-second call timeout, because `zbus`'s blocking API has no per-call
  timeout and a save runs on the looper thread — an unbounded call there would
  present as the client freezing mid-game, not as a slow keyring;
- treats missing, locked, dismissed, absent and unusable identically: nothing
  saved, one line in the log, a client on the landing page.

**Where there is no Secret Service it falls back to the `0600` file, loudly.**
Refusing to persist was considered and rejected: a user on a headless or minimal
machine is not made safer by being signed out — they sign in every launch *and*
the next tool they use writes a token to their disk anyway. The fallback names
the file, says that anything which can read their files can take the account,
and says how to refuse it, on every launch. `CORDIAL_SECRET_STORE` is the
setting — `auto` (default), `keyring` (refuse the fallback and accept no saved
session), `file` (skip the service) — in the same spirit as Sober's
`use_libsecret`, and the shell should surface it and pass it in the
environment `launch.rs` builds for the client. (This cited `CORDIAL_WAYLAND`
as the example until that variable was removed: `android::backend()` now
prefers Wayland whenever a compositor is there, per ADR-011, so there is
nothing for a launcher to opt into.)

**Keyed by the profile's full path, not its name.** Every agent and every test
in this repository is told to run under its own `XDG_DATA_HOME`, and every one
of those roots contains a profile called `default`. Keying on the name would let
a scratch profile read, overwrite and delete the session of the profile somebody
actually plays on.

**Migration destroys the plaintext rather than abandoning it.** The first launch
after this change takes an existing `cookies` or `identity` into the service,
asks for it straight back, compares it in memory, and only then overwrites the
file's bytes and unlinks it. `remove_file` unlinks and does not erase, and undelete
is a normal thing for a filesystem to support. What that does *not* do is also
written down in the code: on a copy-on-write filesystem — btrfs, Fedora's
default and what this was written on — a rewrite may land in new blocks, and no
user-space overwrite touches a snapshot, an SSD's remapped blocks, or yesterday's
backup. It is a floor, not a guarantee, and anyone whose file has been somewhere
it should not have been should still change their password.

**Measured, 2026-08-02, on a scratch profile with a fabricated token.** With the
service present, a plaintext store is adopted, the item appears under
`application=cordial`, `store=cookies`, `profile=<path>`, `xdg:schema=org.cordial.Session`,
and the file is gone. With `CORDIAL_SECRET_STORE=file` it stays a file and says
so. With the bus removed the fallback fires, warns, and the session still loads.
With the bus removed and `CORDIAL_SECRET_STORE=keyring` nothing is saved, the
plaintext file is named and ignored rather than silently used or deleted, and the
launch continues. **`INFERRED`:** that a *locked but present* collection takes
the same path as an absent one is not measured — both collections on the machine
this was written on are unlocked, and locking one to find out would have locked
the owner's real keyring. The `Locked` property read itself is measured, returns
without a prompt, and everything downstream of it failing is tested.

**Users wanting encryption at rest should still use full-disk encryption or
`systemd-homed`**, which remains true and is no longer an excuse for doing
nothing.

## Consequences

**Demonstrated, 2026-08-02:** two accounts signed in at once, in two windows,
side by side. Two profiles, two instances, two sessions, each with its own
cookies, identity, flag overrides and plugin grants. Nobody built this — it is
what the decision above produces: the `flock` stops one profile being opened
twice and says nothing at all about opening a *different* one. On Windows the
same thing has traditionally meant a second desktop session.

Three things follow that the decision did not consider:

- **Cost.** Each instance is a whole engine — around 1.5 GB resident, plus a
  task-scheduler pool sized to the core count. Two is comfortable on a 16 GB
  machine; four is not, and this project has already watched that machine swap.
- **Not tested, only observed.** Nobody has checked what happens when two
  instances write plugin state concurrently, or whether the *shared* plugin code
  directory leaks anything between profiles. Installed code is global by
  design; only grants and settings are per profile.
- **Describe it as profiles, not as multi-account.** The capability is
  identical either way, but this project has no arrangement with Roblox and
  tells contributors to keep test accounts on a separate IP. "Isolated profiles,
  each with its own session" is what it is; the other phrasing invites a reading
  the project has been careful not to earn.

**Accepted:** the storage path changes, and existing users have a session under
`instances/default`. The change must *move* that directory rather than start
fresh, because a silent reset presents as being logged out for no reason — which
is exactly the class of failure this project keeps writing down. Migration runs
once, when the old path exists and the new one does not.

**Accepted:** a profile is one object holding Roblox storage, plugins and flags
together, rather than three orthogonal selections. "My alt account with my main's
plugins" is not a use case worth the cost of explaining which combinations are
legal.

**Accepted:** the account switcher is a profile switcher. It does not
authenticate or know anything about accounts — it selects a directory. Cordial
never sees a password.

**Extended by [ADR-035](ADR-035-browser-account-routing.md):** the manual
switcher still selects a directory, but browser routing now redeems a launch
ticket and compares account IDs to select an existing profile automatically.
The directory-only description below records the earlier design; it no longer
describes every selection path in the shell.

> The rest of this point read "and never stores a session token itself; Roblox
> does that, inside the profile." That is no longer true and was never true of
> the engine: Cordial stores the session token, because nothing else does. See
> the correction above. What is unchanged is the part that matters to this
> ADR — the switcher still only selects a directory, and authentication still
> happens in Roblox's own UI.

**Placement superseded by "Changed 2026-10-08" at the end of this ADR; the reasoning
about the shell, the lock and the list still stands.**

**Built, 2026-08-02: the switcher lives in the shell, and it cannot live
anywhere else.** It is an `AdwComboRow` above the Launch button, listing
`profile::list()` with a suffix action that creates one. Three consequences of
the decision above turn out to constrain where such a control can go, and they
are worth stating because "put a profile switcher in the client" is the obvious
suggestion:

- **A running instance cannot change profile.** `profile::set_active` refuses a
  second, different directory outright; the `flock` is held for the process
  lifetime; the engine's storage root is resolved before the first frame. A
  switcher in the engine's window would be a control that cannot do what it
  looks like it does, which is the interface form of the stub AGENTS.md forbids.
  So the shell's control decides what the *next* instance runs, and "run a
  second profile alongside this one" is the same gesture as switching: choose
  another and launch.
- **What is running is asked of the lock, not of a second bookkeeping.** The
  list marks a profile unavailable by taking its claim and dropping it again, so
  what the menu shows and what a launch will do cannot disagree. The cost is
  that drawing the list briefly holds each lock; the failure that produces is
  the ordinary busy refusal, which names the profile.
- **The list is exactly what is on disk.** An earlier revision synthesised the
  chosen profile when it had no directory yet, so that a fresh install would not
  show an empty list. That was withdrawn: a profile is a signed-in session, and
  a launcher listing one that does not exist is claiming an account the user
  does not have.

The first attempt put this in the header bar as an `AdwAvatar`, which was
rejected on the reasoning that an avatar in the top right is a browser
convention — GNOME's HIG has no profile-switcher pattern, and the one libadwaita
precedent, Fractal, is an application where identity is ambient. In a launcher
the profile is a launch parameter, so it belongs beside the button it governs.

**Accepted:** two windows on one account is not supported. It is a real thing
people do while testing, but it requires duplicating a live session deliberately,
and Roblox may invalidate one of them. A profile copy is the honest way to ask
for it, and it should be an explicit action rather than something a second launch
does silently.

**Rejected: separating profiles from instances as independent concepts.** It
doubles the vocabulary and the settings surface to serve combinations nobody has
asked for.

**Rejected: making the lock mandatory** via a mount or a daemon. The failure this
guards against is an accident, and an advisory lock stops accidents. Anything
stronger costs more than the problem.

## What would change this

If Roblox ever ships storage that tolerates concurrent access from two clients,
the lock becomes unnecessary and same-profile multi-instance becomes free.
Nothing suggests that is coming.

## Added 2026-09-30: deleting a profile

A profile could be created and never deleted. The launcher's profile row now has
a Delete button, backed by `profile::remove`: it takes the profile's lock the way
a launch does (so an open profile is refused), erases the saved sign-in from the
desktop keyring as well as the profile (entries are keyed by the profile's
directory, so removing the directory alone would leave a live token behind), and
only then renames the directory aside and removes it. If the sign-in cannot be
erased nothing is deleted. Where the keyring cannot be reached the files are
still removed and the launcher says a keyring entry may remain. The row moves to
another profile afterwards, or to `default`, which is created when next
launched.

## Changed 2026-10-08: the switcher is in the header bar

The row described above, and the Delete button added on 2026-09-30, are now a
menu button at the start of the header bar. It is labelled with the profile the
next launch will use; its menu lists the profiles as radio items, then "New
Profile…" and "Delete Profile…". The window body is the Roblox button alone, with
a caption beneath it that is empty and hidden unless the profile is uncreated,
held by another window, or pinned (ADR-033).

This reverses the reasoning two sections up for placing it beside the button.
The avatar's fault was its shape and its corner, not the header bar: a titled
row, a subtitle and two icon buttons over the one control the window is for made
the launcher read as a form. `INFERRED`, not measured: GNOME's HIG has no page
for a profile switcher, so this follows its general header bar guidance.

Unchanged: the list is exactly what is on disk, availability is still asked of
the lock as the menu opens, a busy profile stays choosable (the dialog behind
Roblox offers to close its holder), an unusable one does not, deletion still
asks first and is still refused for a held profile, and "Choose a Profile" in the
busy dialog opens the menu. Code: `profile_switcher.rs`.
