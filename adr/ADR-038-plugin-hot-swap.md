---
title: "ADR-038: A running client reconciles its plugin set; nothing pushes to it"
---
**Status:** accepted. Where the reconciler runs is amended by [ADR-055](/adr/ADR-055-the-launcher-and-the-runtime-are-two-programs): in the launcher, not the client. Everything else here stands.
**Related:** [ADR-003](/adr/ADR-003-plugin-isolation), [ADR-007](/adr/ADR-007-host-resources-are-brokered),
[ADR-008](/adr/ADR-008-plugins-are-typescript-on-deno), [ADR-012](/adr/ADR-012-profiles-and-instances),
[ADR-013](/adr/ADR-013-per-profile-configuration), [ADR-021](/adr/ADR-021-everything-is-a-plugin)

## Decision

Installing, updating, removing, enabling, disabling or granting a plugin
takes effect in a running Cordial client for that profile without a restart.
A background thread inside the client — not the shell, and nothing new
listening on a socket — polls this profile's discovered plugins, its
`plugin-enabled.json` and its `plugin-grants.json` once a second, compares
what it finds against what is actually running, and starts, stops or
restarts exactly the plugins that differ, through the same spawn path
`start_all` already uses for a fresh launch.

A plugin that is merely re-granted or re-revoked a capability, with its own
files untouched, is not restarted at all — `plugin_host::refresh_grant`
already carries that to a running plugin's next request, and has done since
before this ADR. What is new is everything upstream of that: noticing an
install, an update, a removal, or a plugin crossing in or out of "enabled and
holds at least one capability" while nobody is currently talking to it.

## Why this is a poll, not a push

Three shapes were available: the shell pushes an event down some new channel
to the client; the client watches the filesystem with `inotify`; the client
polls. The task that produced this ADR named the first two by name. This
picked the third, and the reasoning is worth recording because it is not the
obvious choice.

**There is no channel to push down that ADR-012 has not already closed.** A
profile is held by at most one instance at a time, and the shell that writes
`plugin-grants.json` is not that instance — it is a separate process that
launched the client and then, ordinarily, stopped watching it. Building a
push channel means building the very thing ADR-019's `devctl` socket already
is: a socket inside the profile directory, gated on an environment variable,
that only exists when asked for. Reusing `devctl` for this was considered and
rejected — it is explicitly a *development* control surface (`CORDIAL_DEV_CONTROL`,
off by default, undocumented to a plugin author), and Settings writing a
production file is not a development action. Building a second, always-on
socket for exactly one message ("something changed, go look") duplicates
`devctl`'s shape to save a `stat` loop, which is a bad trade before it is
measured and remains one after: see below.

**`inotify` was tried and set aside, not ruled out.** Every crate it needs —
`inotify` itself, `inotify-sys`, `libc`, `bitflags` — resolves and builds
offline against the versions already cached on this host with
`default-features = false` (blocking API, no `tokio`), so availability was
not the objection. Two things were. First, this codebase already has a
working, tested, documented pattern for exactly this question —
`plugin_host::refresh_grant`'s `mtime` check — and generalising a pattern
already proven in this file over the last several months is a smaller,
better-understood change than introducing a new dependency with its own watch
descriptor lifecycle, queue-overflow handling (`IN_Q_OVERFLOW`), and a rename
sequence to reason about (`unpack.rs`'s install path is two renames, not one:
the old directory moves aside before the new one moves in, so a watcher has
to either tolerate seeing the plugin briefly disappear or coalesce across
it). Second, and more mundane: the polling interval this ADR ships is a
second, chosen for a Settings toggle to feel roughly immediate to a human,
and nothing about a hot-swapped *plugin* needs the microsecond-scale reaction
`inotify` would buy that a one-second poll does not already deliver.
`CORDIAL_PLUGIN_RECONCILE_INTERVAL_MS` exists specifically so this choice is
revisited by a number rather than a rewrite if a second ever turns out to be
too slow for something this ADR did not anticipate.

**A poll generalises a mechanism already in the codebase; `refresh_grant`
is the specific one.** `crates/cordial-runtime/src/plugin_host.rs` already
compares a `stat(2)`'s `mtime` against what it last saw and only re-reads and
re-parses when it differs, on every request a running plugin makes. The
reconciler in this ADR is the same idea widened from "whenever a request
happens to arrive" to "once a second, for the whole plugin set" — because
unlike a grant change, an *install* has no in-flight request from the plugin
in question to piggyback the check on; the plugin that would make that
request does not exist yet.

## Why unpacked (Developer-mode) plugins are excluded entirely

`manifest::unpacked_dirs()` — plugins loaded from `CORDIAL_UNPACKED_PLUGINS`
for someone actively editing one — already reload themselves. `PluginProc::spawn_with`
passes `reload: true` for these, which starts Deno with `--watch`, and Deno's
own file watcher restarts the process on a change. `cordial_plugins::reconcile::desired_state`
never returns an unpacked plugin, and the reconciler's own bookkeeping
(`Shared::running`, populated only by `spawn_one`) is seeded only from what
`start_all` actually started outside that path — so an unpacked plugin never
appears on either side of the diff. Two supervisors independently deciding to
restart the same process is not a sharper edge than one; it is a race, and
the existing one (Deno's) is the one already proven for this case.

## Why an empty grant set is a `Stop`, continuously

`start_all` has always refused to spawn a plugin whose grant set is empty,
and prints so: `no capabilities granted, not started`. This ADR applies that
same test on every tick rather than only at launch: `desired_state` simply
omits a plugin the moment its last capability is revoked, and the diff
reports that the same way it reports an uninstall — the plugin is no longer
one this profile is willing to run at all.

This was not the first design considered. The alternative — leave a plugin
running with an all-denying broker once its capabilities reach zero, on the
grounds that `refresh_grant` already makes this harmless — was rejected for
consistency rather than safety: a client running for an hour should not be
able to disagree with a fresh launch of the same profile about whether a
given plugin is running at all, and "capabilities are all denied but the
process is still alive" is exactly the kind of state a user cannot observe
and a bug report cannot describe. A plugin that keeps at least one capability
through a change is never affected by this — that case is `Regrant`, handled
below, and never stops anything.

## Why a hot-swapped plugin's grant is intersected with its *new* manifest, and an ordinary start's is not

`docs/plugin-api.md` documents, correctly and deliberately, that the grants
file is authoritative and is **not** intersected with a plugin's manifest: a
capability written into `plugin-grants.json` by hand, or left there from an
earlier version of a plugin's manifest, is granted regardless of what the
current manifest requests. That is safe as a *launch-time* property, because
the files on disk at launch are the ones a human is choosing to trust when
they choose to open Cordial at all.

It stops being safe the moment a plugin's files can change underneath an
already-running grant, which is the entire feature this ADR adds. Consider a
plugin whose version 1 requested and was granted `flags.write`. Version 2's
manifest drops that request — perhaps the author no longer needs it, perhaps
the update is malicious and dropping the request from the visible manifest is
an attempt to look narrower than the process actually is. Either way,
`plugin-grants.json` still says `flags.write`, because nothing has asked the
user about version 2 at all; no install prompt runs for an update, by design
(the capabilities were already approved once, and re-prompting on every patch
release would train users to click through it). If the restarted process were
handed the raw grants-file entry the way an ordinary launch is, it would keep
`flags.write` it never re-requested and, by `docs/plugin-api.md`'s own
account of that capability, could write into any `Cordial`-prefixed flag key
including `CordialGraphicsBackend`. `cordial_plugins::reconcile::intersect_for_restart`
exists to close exactly this: on a restart, and only on a restart, the
process gets the profile's grant narrowed to what the *new* manifest
actually requests. `Desired::granted` itself stays the raw, unintersected
value — used for `Start` and for what `RunningPlugin::snapshot` remembers —
so recording the narrower set does not make every later tick misread a
plugin that legitimately holds more than it currently asks for as having
just changed again.

If the intersection leaves nothing, the plugin is not restarted at all — the
same `not started` invariant as an ordinary launch, applied to an update
whose new manifest happens to ask for less than the profile ever granted.
The first version of that rule was a bypass; the next section is what it
became.

## An update that asks for nothing granted stays stopped, waiting for approval

The "not restarted" above only stopped the old process and moved on. The next
tick found the plugin absent from `running` and still wanted, `diff` said
`Start`, and `Start` hands over `Desired::granted`, the raw grants-file entry:
the update ran on exactly the grant this section says it must not inherit,
one second late. The same route was open whenever the replacement failed to
spawn and was retried, and whenever the old process was slow enough to die
that the restart was deferred a tick.

Decided (maintainer's choice, not derived): when an update requests nothing
the profile has granted, the plugin is **left stopped**. The new version is
not run, the old version is not run in its place, and no later tick starts it.

- `BlockKind::NeedsApproval` records the state in `Shared::blocked`, against
  the plugin's fingerprint and grant. It is not retried by time or by Deno
  appearing; it lifts only when the plugin's files change again or the
  profile's grant for it changes, which is the user approving in Settings.
- `Shared::updating` remembers that a plugin's files changed while it ran, and
  every start of it until one succeeds, including the retry after a spawn
  failure, goes through `intersect_for_restart`. A grant that changes but
  still intersects to nothing (the user switched on something the new
  manifest does not ask for) is recorded again rather than started.
- The plugin's health entry reads `Update needs approval: <permissions>`,
  naming what the manifest requests that the profile has not granted. Settings
  shows it as a warning icon on the plugin's row, not as a failure, and the
  entry is cleared when the plugin starts or stops being wanted.

Not covered, deliberately: an update that requests *some* of what was granted
plus something new still runs, with the granted part only, as above. And a
Cordial relaunch, or a plugin switched off and on, starts from the grants file
as `start_all` always has; the launch-time trust in `docs/plugin-api.md` is
unchanged.

## A plugin that failed to start is remembered, not retried every tick

`diff` reports `Start` for every wanted plugin that is not running, and a
plugin whose spawn failed is not running. The first version therefore retried
each failed plugin once a second for the whole session: with no Deno on the
machine, three plugins produced about nine log lines a second, each attempt
certain to fail as the last had. Found in a Flatpak client mid-game.

A failed start is now recorded (`Shared::blocked`, `reconcile::Blocked`)
against the plugin's fingerprint and grant, with a reason that decides when to
try again. No interpreter: retried when one is found at any of the paths
`sandbox::interpreter_present` looks in, checked once per tick and only while
something is waiting. An unusable `entry`: retried only when the plugin's files
or grant change. Any other spawn error: retried with a doubling wait, two
seconds up to about a minute. Plugins waiting for Deno are named in one log
line, not one each, and that line and the health entry Settings shows no longer
say to restart, because installing Deno is picked up by the next tick
(`plugin_host` tests drive `reconcile_tick_with` with a runtime that is absent
and then present).

The same investigation found the message was also wrong on the machine that
reported it. Inside the Flatpak the sandbox is always `Sandbox::None`, whose
command ran a bare `deno` resolved against `PATH`, while the check before it
also accepts Cordial's own downloaded copy. Deno 2.9.6 was in the data
directory, `interpreter_present` said yes, the spawn said ENOENT, and that was
reported as "Deno is not installed". The unconfined command now runs the path
that was found.

## What is deliberately out of scope

**Asset overlays and a plugin's own `flags.json` do not hot-swap.**
`register_static_overlays` and `flags::collect` both read every enabled
plugin's static files fresh at every launch, with no capability check at all
(ADR-021) — but only at launch. A texture pack installed while Cordial is
running is discovered the *next* time `start_all` runs, not this session.
Extending this reconciler to re-register overlays and re-collect flag layers
live is a reasonable next step and was left out of this change because
neither is process-lifecycle work: an overlay changing the *files* the asset
resolver reaches for while a game is running is a different risk profile
(ADR-010 already says an overlay is not vetted for gameplay effect) from a
Deno process starting or stopping, and folding both into one ADR would have
made the safety argument above harder to follow, not easier.

**`FFlag`/`FInt`/`FString` remain launch-only**, unaffected by any of this.
`flags.write` already only takes effect at the next launch (ADR-005); nothing
here changes that, and a plugin's own `flags.json` layer sits in the
out-of-scope category above alongside overlays for the same reason.

**No new core event tells other plugins a hot swap happened.** It was
considered — a `cordial/plugin.changed` event `discord-presence` or a future
plugin could subscribe to — and set aside as a second feature riding on this
one's name rather than a piece this one needs. If a use for it appears, it is
a small, separately-reviewable addition in the shape ADR-007 already asks
every broker to be.

## Testing

`cordial_plugins::reconcile`'s `desired_state` and `diff` are pure functions
with no process, broker or thread involved, and are unit-tested directly:
a plugin excluded for no grant, excluded for being disabled, excluded for
being data-only, included when it qualifies, a built-in shadowing a same-id
user copy, a stable fingerprint across two reads of untouched files, a
changed fingerprint when the entry module's bytes change with the manifest
untouched, and the five `diff` outcomes the task that produced this ADR named
by name: added (`Start`), removed (`Stop`), updated by version (`Restart`
naming both versions), updated by hash alone (`Restart` naming neither), and
permissions changed (`Regrant`, and separately, disabled folds into `Stop`
via `desired_state`'s own gate rather than being a sixth `diff` outcome —
see "Why an empty grant set is a `Stop`, continuously" for why disabled and
revoked-to-nothing are the same case). `intersect_for_restart` is tested
both ways: narrowing a grant the new manifest no longer requests, and
refusing to grant something the profile never approved regardless of what
the new manifest asks for.

**Verified live, on a throwaway signed-out profile**
(`XDG_DATA_HOME=~/.cache/cordial-agent-plugins`), because `cordial-runtime`
has no standalone way to run the reconciler without a real client. The first
attempt at this failed, and the failure is the more useful record than a
clean pass would have been.

`flag-inspector`, granted `flags.read` and `log` and left enabled, started
normally. Disabling it in `plugin-enabled.json` while the client kept
running produced `plugin flag-inspector: no longer wanted (removed, disabled,
or nothing left granted), stopping` — and then the process did not stop.
`ps -o pid,ppid,pgid` against the still-running tree found the cause: the
**outer** bwrap process — the one `std::process::Command::spawn` returns, and
the one this reconciler's `stop_one` had the pid of — was not its own process
group leader on this host's bwrap build. Its process group belonged to its
own parent; the **inner** bwrap fork, one level further down with a
different pid entirely, was the actual session and group leader.
`kill_process_group`'s `killpg` call was therefore signalling a group with
nothing in it every time — a syscall that succeeds and changes nothing,
which looks identical to working right up until something waits for the
effect. See `kill_process_group`'s own doc in `crates/cordial-plugins/src/host.rs`
for the fix (send the signal to the pid directly as well as to the group it
was assumed to lead) and for why `Plugin::kill`'s existing "measured
directly" comment about needing the group signal is, in hindsight, probably
describing the same confound: a test that exercises both signals together
cannot tell which one is doing the work.

With that fixed, the same sequence — disable while running, confirm the stop,
re-enable immediately (not after waiting; the point was to stress the case
where a second change lands before the first has finished tearing down) —
produced, in order and without a client restart:

```
plugin flag-inspector: no longer wanted (removed, disabled, or nothing left granted), stopping
plugin flag-inspector: now wanted (installed, enabled or granted while running), starting
[plugin] flag-inspector: bwrap + Deno permissions + broker
plugin flag-inspector: started
[flag-inspector] 1 flag override(s) in effect
[flag-inspector]   FFlagUserLaunchedWithBloxstrap = True  (from built-in)
[flag-inspector] writing a flag came back: denied (needs flags.write)
```

The re-enable racing the still-in-flight stop is also what exposed the
second bug this ADR's implementation fixes over its own first draft: without
`Shared::stopping`'s quarantine, a plugin re-enabled before its kill was
confirmed compared as "unchanged" against the stale, still-present
`shared.running` entry and was never started again at all. That failure mode
and its fix are recorded on `Shared::stopping` itself, in the source, because
it is exactly the kind of thing a reader modifying this file six months from
now needs at the point of the code rather than only in this ADR.

Verified separately and directly, outside the client: a standalone `bwrap
--unshare-all --new-session ... deno run` reproduces the same process-group
topology this bug depended on, and `kill(pid, SIGKILL)` against the outer
pid reaps the whole tree (`--die-with-parent` cascading the death down)
where `killpg` against the same pid reaches nothing. That reproduction is
not committed anywhere; it was a shell one-liner run to isolate the cause
before touching the source, and is recorded here rather than as a test
because it exercises bwrap's own process model, not anything this
repository owns.

## What would change this

If `CORDIAL_PLUGIN_RECONCILE_INTERVAL_MS`'s default of one second turns out
to be noticeably slow for a real workflow, the number is a one-line change
before anything about the mechanism needs revisiting. If Cordial ever grows a
production, always-on shell-to-client channel for an unrelated reason,
routing this feature's notification through it instead of a poll becomes a
reasonable follow-up — but building that channel *for* this feature, when a
poll already answers it correctly, was rejected above and stays rejected
until such a channel exists for its own reasons.
