# ADR-055: The launcher and the runtime are two programs, and plugins stay in the launcher

**Status:** proposed; implementation not started.
**Date:** 2026-10-08
**Supersedes:** [ADR-052](ADR-052-the-runtime-spec.md) decision 4 ("the built-in Android runtime is the first implementation, in-process") and `docs/runtime-spec.md` section 8 as first written. The rest of ADR-052, including the listing policy, stands.
**Amends:** [ADR-038](ADR-038-plugin-hot-swap.md) on *where* the reconciler runs (the launcher, not the client); its reasoning about polling and about never restarting a plugin for a re-grant stands. [ADR-044](ADR-044-settings-reach-a-running-game.md): the live or next-launch classification becomes a per-runtime declaration, with today's table as the built-in runtime's.
**Related:** [ADR-001](ADR-001-in-process-hooking.md), [ADR-002](ADR-002-core-shell-and-ui-handoff.md), [ADR-003](ADR-003-plugin-isolation.md), [ADR-007](ADR-007-host-resources-are-brokered.md), [ADR-010](ADR-010-plugin-asset-overlays.md), [ADR-011](ADR-011-wayland-and-libadwaita.md), [ADR-012](ADR-012-profiles-and-instances.md), [ADR-019](ADR-019-development-control-surface.md), [ADR-021](ADR-021-everything-is-a-plugin.md), [ADR-031](ADR-031-the-launcher-outlives-its-window.md), [ADR-039](ADR-039-a-runtime-backend-seam-and-why-macos-waits.md), [ADR-043](ADR-043-the-roblox-build-is-the-binarys-architecture.md), [ADR-051](ADR-051-overrides-are-reapplied-after-the-engines-refresh.md), [ADR-054](ADR-054-cordial-owns-its-roblox-builds.md)
**Spec:** [`docs/runtime-spec.md`](../runtime-spec.md). **Review and plan:** [`docs/analysis/runtime-protocol-review.md`](../analysis/runtime-protocol-review.md), which carries the code reading and the commit order.

## Context

ADR-052 published a spec and kept the first runtime in-process: the Android
runtime would speak it "over a channel, not a socket", with the plugin host
inside the client. The maintainer's direction since is different and simpler to
state: **a port forks the runtime and never the launcher.** For that to be true
the launcher cannot be a library inside the runtime, and the plugin host, which
is the largest piece of launcher behaviour, cannot live in the process a port
replaces.

The code is closer to this than the crate graph suggests, and further than the
spec's section 8 said. Read for this ADR:

- **The process boundary already exists.** `cordial-shell` spawns `cordial-run`
  as a sibling binary and never links it; the references to `cordial_runtime`
  in the shell are comments. What it passes is argv, about fifteen `CORDIAL_*`
  variables, an inherited `flock` descriptor and piped stdout and stderr, and
  afterwards `live/settings.sock` ([ADR-044](ADR-044-settings-reach-a-running-game.md)).
- **The dependency runs the wrong way for a port.** `cordial-runtime` depends on
  `cordial-shell`, and imports `host_window`, `webview`, `nvidia`, `profile`,
  `stacking_gate`, `title_bar` and `live_wire` from it. A port that forked the
  runtime would drag GTK, libadwaita and the launcher's modules with it.
- **The plugin host is in the client.** `plugin_host.rs` is 3,031 lines and is
  started from `load.rs`. It calls `crate::flags`, `crate::profile`,
  `crate::game_log` and `crate::android::asset`, so the spec's claim that the
  portable core "has almost no Android coupling" was wrong for the file that
  matters.
- **Two of the first draft's events have no publisher.** `client.ready` and
  `window.resized` are declared in `core_events.rs` and published by nothing.

## Decision

1. **The launcher and the runtime are two programs speaking
   `cordial.runtime/1` over a Unix socket.** The built-in runtime, `cordial-run`,
   stays a separate process and implements the protocol in place of the
   in-process channel ADR-052 described. It is the conformance suite. The
   version-0 sockets stay as an alias until the launcher no longer needs them.
2. **The launcher owns the plugin host, with hot-swap.** Grants, the broker, the
   Discord socket, the flag layer resolver and write grants, and the reconciler
   of ADR-038 move to the launcher. There is one host per runtime session
   because grants are per profile ([ADR-013](ADR-013-per-profile-configuration.md)).
   The launcher holds itself alive while a client runs
   ([ADR-031](ADR-031-the-launcher-outlives-its-window.md)), so the host's
   lifetime is the game's in every case except a launcher crash, where the game
   now survives and the plugins do not. That is a change from today, where a
   launcher crash kills the client through the stdout pipe, and it is the better
   way round.
3. **The runtime owns the engine, its window, and the files the engine writes.**
   `client_settings`, `flag_reapply`, the log tail and the BloxstrapRPC parser
   stay with it, because they read the engine's own output. It owns its window:
   the engine's surface is a `wl_subsurface` of the runtime's own GTK toplevel
   ([ADR-011](ADR-011-wayland-and-libadwaita.md)), and a subsurface cannot cross
   a process boundary. The first draft's `window.mode: embedded` is therefore
   dropped, and "the launcher owns the window" means the launcher owns *its
   own* window.
4. **The protocol is events up, a closed set of requests down, and no verb that
   runs code.** ADR-001 and ADR-003 are untouched: there is no message for
   engine memory, code, a command or a descriptor. Exactly one message carries a
   path, `assets.overlay.set`, and the launcher canonicalises and confines it.
   Plugins never speak to a runtime; the launcher maps events onto the plugin
   API.
5. **The launcher synthesises exit and crash from the child's wait status.** A
   crashed process cannot report its own crash, and a `lifecycle.exit` would be
   the stub-that-lies pattern in a message. `lifecycle.ready` is optional until
   something publishes it. **The runtime survives EOF on the socket and a closed
   stdout**, so closing the launcher mid-game stays the ordinary case.
6. **Transport: the runtime listens on a path, the launcher connects.** A
   handed-over descriptor was considered and rejected: the launcher is
   restarted while games run, and a descriptor cannot be found again. Per
   message limits, a bounded drop-and-count event queue on engine threads,
   reply timeouts that count as failure, and manifest-derived identity for the
   keyring are in the spec.
7. **Launch configuration stays env and argv for the built-in runtime in
   version 1.** A third-party runtime gets placeholders (`{socket}`,
   `{session_dir}`, `{profile_dir}`, `{build_dir}`, `{join_url}`) and receives
   settings over the socket. The profile lock is inherited at spawn, not sent.
8. **Cordial's pseudo-flags become typed settings.** `CordialGraphicsBackend`,
   `CordialFrameRateLimit` and `CordialDeviceProfile` ride in `flags.json` today
   and would reach a foreign engine as unknown flag names. The launcher strips
   them from the resolved document and sends them as `settings`.
   `flags.live` means "apply and hold in force", which is what
   [ADR-051](ADR-051-overrides-are-reapplied-after-the-engines-refresh.md)'s
   re-apply already does.
9. **The development control surface is not in the protocol.** devctl
   ([ADR-019](ADR-019-development-control-surface.md)) stays a runtime-private
   socket behind its own variable, and `cordial-mcp.py` keeps talking to it.
10. **A manifest says who manages builds.** `builds: "cordial"` means the store
    of [ADR-054](ADR-054-cordial-owns-its-roblox-builds.md) supplies
    `{build_dir}`; `builds: "runtime"` means the launcher shows no Version row.
11. **Left out of version 1:** `updates`, `doctor`, `session.vault`, `window`,
    `launch.join`, a runtime-declared settings form. Each is added by a minor
    bump and an ADR when a second runtime needs it.

## Packaging

**A runtime that is packaged separately is a Flatpak extension of the launcher,
and runs inside the launcher's sandbox.** The manifest gains one block and
`finish-args` gains nothing:

```yaml
add-extensions:
  io.github.luohoa97.Cordial.Runtime:
    directory: runtimes
    subdirectories: true
    version: '1'
    no-autodownload: true
```

An extension `io.github.luohoa97.Cordial.Runtime.<Name>` on branch `1` (the
protocol major) mounts at `/app/runtimes/<Name>/`, where the launcher already
looks for a manifest. The socket is an ordinary path in the profile directory,
which both sides see because they are one sandbox, so nothing new is shared and
`child_watch` and the inherited lock work unchanged.

The alternatives need a grant this project has refused. Two separate apps
sharing a directory would need `--filesystem=xdg-run/cordial-runtime:create` in
both, and still could not start each other without
`--talk-name=org.freedesktop.Flatpak`, which is arbitrary host command
execution ([ADR-002](ADR-002-core-shell-and-ui-handoff.md) section 2,
[ADR-007](ADR-007-host-resources-are-brokered.md)). Passing a descriptor over a
portal does not apply: the Flatpak portal's `Spawn` starts a sandbox of the
caller's own application, and no portal brokers a connection between two apps.
**All of this is INFERRED from the Flatpak documentation; no extension was
built.**

**The cost is stated plainly:** an extension runs with Cordial's permissions and
no others, so a runtime that needs more (`--device=all`, a FUSE device) cannot be
one. That is the property ADR-007 asks for, the set of host resources fixed when
the launcher's manifest is written, and a runtime that cannot live with it is
native-only in version 1. A runtime built against another base than
`org.gnome.Platform` 50 must bundle what it needs under its own mount.

## The protocol crate and its licence

A new crate, `cordial-protocol`, depends on `serde` and `serde_json` and
nothing else, links no native code, and holds the message types, the framing
codec, the manifest type, the version negotiation and a conformance harness that
both sides run against shared line vectors. `live_wire.rs` moves into it with its
tests. Both the launcher and `cordial-run` depend on it.

It is licensed **GPL-3.0-or-later like the rest of the workspace**. The rule in
`CONTRIBUTING.md` ("The licence is settled") declines a permissive carve-out,
and a crate is not exempt from it. **This has a cost this ADR does not wave
away:** ADR-052 rejected "a trait with no wire form" partly because it forces a
runtime into Cordial's licence, and a GPL crate does the same to a Rust runtime
that links it. What keeps the contract neutral is that the wire is plain JSON
lines specified in `docs/runtime-spec.md`, so a runtime in any language, under
any licence, implements it without the crate. Whether the line vectors may be
taken under a permissive licence is the maintainer's call and is **not decided
here**. Publishing to crates.io waits for the split to land; only
`cordial-protocol` would be published, and every other crate stays
`publish = false` because each links the AOSP linker or GTK. No `NOTICE` or
`THIRD-PARTY-NOTICES.md` entry is needed for a first-party GPL crate. (`NOTICE`
says "See COPYING", a file that does not exist; the repository's text is
`LICENSE`, corrected in the same change as this ADR.)

## Order of work

Docs, then a pure refactor, then additive wiring, then the move. Each commit
keeps the single-binary path working; the numbered plan is in the review note.
**Nothing before the 1.0 gates (the signed-in startup freeze and the text
boxes) touches `window.rs`, the start or publish sites in `load.rs`, the native
shims or the startup path.** The crate skeleton and the `live_wire` move are
the only code that qualifies, and `flags.rs`, `profile.rs` and the plugin host
wait.

## What is not decided

- Whether the line vectors are permissively licensed (above).
- Where the session cookie lives for a third-party runtime. The built-in
  runtime reads the secret store itself through `cordial_shell::secrets`
  (re-exported in `cordial-runtime/src/secrets.rs`); `session.vault` is deferred
  until a runtime needs it.
- Whether `mangohud` and `vkbasalt`, which are Vulkan loader environment
  variables, are settings or a manifest-declared environment allowlist.
- The sandbox variants of discovery for a runtime that is not an extension.

## Consequences

- `cordial-runtime` is eventually renamed (`cordial-android`) and loses its
  dependency on `cordial-shell`'s non-window modules; the window code it imports
  moves with it or into a small crate of its own. That is the last step, not the
  first.
- The launcher gains a session object: a child, a socket, a plugin host, a
  state snapshot. It already has most of it in `live.rs` and `launch.rs`.
- A plugin's behaviour on a launcher crash changes (above), and a release note
  for the step that moves the host should say so.
- Hot-swap latency is unchanged: the launcher polls the same files once a
  second, from a process that wrote them.

## Alternatives considered

- **Keep the plugin host in the runtime and forward grants over the socket.**
  Rejected: every port would then carry the plugin host, which is the thing the
  split exists to avoid, and ADR-003's isolation would depend on each port
  getting the broker right.
- **The launcher binds, the runtime connects.** Removes the connect retry and
  stale sockets, but a restarted launcher cannot find a running game.
- **A descriptor handed at spawn.** Same objection, and ADR-031 already makes
  closing the launcher's window ordinary.
- **Embedding the runtime's surface in the launcher's window.** Not possible
  across processes with a `wl_subsurface`; a cross-process embed would need a
  different presentation path, which is a rewrite of the engine's windowing and
  is not proposed.
- **Per-line `v` fields and a minor in the manifest.** Rejected: three places
  for one number let them disagree.

## Reopen when

A second runtime exists and a capability the first version left out is needed;
the 1.0 gates have shipped and the migration order is re-planned; or the
Flatpak extension route is built and any part of the inferred packaging turns
out wrong.
