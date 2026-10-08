---
title: "ADR-052: Launcher features reach runtimes through a published spec, and Cordial lists only its own"
---
**Status:** accepted as design; implementation not started. Decision 4 (the built-in runtime is in-process) is superseded by [ADR-055](/adr/ADR-055-the-launcher-and-the-runtime-are-two-programs); the rest stands.
**Date:** 2026-10-01
**Supersedes:** the launching design in [ADR-050](/adr/ADR-050-other-runtimes-are-launched-not-built) ("launch only, features do not carry over"). ADR-050's reasoning about why Cordial builds no macOS runtime stands.
**Related:** [ADR-001](/adr/ADR-001-in-process-hooking), [ADR-003](/adr/ADR-003-plugin-isolation), [ADR-007](/adr/ADR-007-host-resources-are-brokered), [ADR-012](/adr/ADR-012-profiles-and-instances), [ADR-019](/adr/ADR-019-development-control-surface), [ADR-039](/adr/ADR-039-a-runtime-backend-seam-and-why-macos-waits), [ADR-043](/adr/ADR-043-the-roblox-build-is-the-binarys-architecture), [ADR-044](/adr/ADR-044-settings-reach-a-running-game)
**Spec:** [`docs/runtime-spec.md`](/runtime-spec). **Spike:** [`docs/analysis/macos-runtime.md`](https://github.com/luohoa97/cordial/blob/main/docs/analysis/macos-runtime.md), which carries the measurements.

## Context

The maintainer asked whether Cordial could front Roblox's macOS client the way
Mac O' Blox runs it. The spike (2026-10-01, read-only, nothing run) found that
building a macOS runtime into Cordial fails on four independent counts: Mac O'
Blox's shim is injection, the window is X11, Darling is x86-64 only, and
whether Roblox tolerates it is unmeasured. It also corrected two premises:
the macOS client appears to render through its own OpenGL path under Darling,
so no Metal translation is needed today, and Mac O' Blox injects code into the
Roblox process with `DYLD_INSERT_LIBRARIES` and method swizzling.

What survives is the launcher half. Profiles, FastFlag layers, plugins,
presence, doctor, report and update UI are Cordial's, and ADR-050's answer for
another runtime was to launch it and carry none of that over. A spec lets them
carry over without Cordial owning the runtime.

## Decision

1. **A published spec, `cordial.runtime/1`.** A runtime is whatever turns Play
   into a running client. It advertises itself with a `runtime.json`, and
   Cordial talks to it over a versioned JSON-lines protocol on a per-session
   Unix socket, with capability negotiation in a handshake. The spec is
   [`docs/runtime-spec.md`](/runtime-spec).
2. **Plugins never talk to a runtime.** Cordial maps runtime events onto the
   existing plugin API, so a plugin works unchanged on any runtime or is shown
   as limited on one that cannot back it. Grants stay Cordial's
   ([ADR-003](/adr/ADR-003-plugin-isolation), [ADR-007](/adr/ADR-007-host-resources-are-brokered)).
3. **Effects and events only.** The verb set is closed per spec version. There
   is no capability for engine memory, loading code, calling engine functions,
   running a command, or passing a path or descriptor. A new capability needs
   an ADR and a spec bump ([ADR-001](/adr/ADR-001-in-process-hooking)).
4. **The built-in Android runtime is the first implementation, in-process.**
   Same messages, a channel instead of a socket, so there is one code path and
   the Android runtime is the conformance suite.
5. **A capability a runtime does not offer is shown as unsupported and never
   faked.** No request is sent for it. This is the stub rule from AGENTS.md
   applied to a protocol.

## Listing policy

The spec cannot police what a runtime does inside its own process. Mac O' Blox
injects into Roblox and no protocol stops that; the only lever Cordial has is
whether it lists the runtime. The spike gave three options:

- (a) a manifest field attesting that the runtime injects, shown to the user.
  It is an unverifiable claim by the party being asked.
- (b) list only runtimes the maintainers have read.
- (c) list none and ship only the built-in Android runtime.

**Chosen, conservatively and by the maintainer:** for now Cordial lists **only
its own built-in runtime**, which is (c). The spec stays published so a runtime
can be written against it. A third-party runtime becomes listed only after the
maintainer has vetted it, which is (b) as a later step, and **a runtime that
injects code into the client is not listed.** Mac O' Blox is one, so it is not
listed. Option (a) is rejected as the sole gate.

Not decided: how a vetted runtime is recorded (a list shipped with Cordial is
the obvious shape) and whether an unlisted runtime that is present on disk is
silent or named as unlisted. Neither is needed until a second runtime exists.

## What this does not change

- Cordial builds no macOS runtime, per ADR-050. If someone writes one outside
  this repository it is their project, and Cordial installs nothing for it.
- The proof-of-concept measurements in the spike are not done, and nothing in
  the spec depends on them.
- [ADR-039](/adr/ADR-039-a-runtime-backend-seam-and-why-macos-waits)'s "Settings
  picker and issue templates neither belong now" still holds. There is one
  listed runtime, so there is nothing to pick.
- [ADR-043](/adr/ADR-043-the-roblox-build-is-the-binarys-architecture): the
  architecture is still the binary's. A runtime declares `arch`, and Cordial
  hides one that does not match the host rather than offering translation.

## Consequences

- When built: `cordial-runtime` is renamed (`cordial-android`), a
  `cordial-runtime-api` crate takes the message types and manifest, a
  `cordial-host-core` crate takes the portable files, and `live.rs` pushes
  settings over the session instead of `live/settings.sock`
  ([ADR-044](/adr/ADR-044-settings-reach-a-running-game)), which stays as a
  version-0 alias until the shell migrates. The spike's section 6 has the
  measured seam.
- The spec is a public contract. Changing it is a versioned act, and a
  convenience is not a reason to widen it.
- Discovery across Flatpak sandboxes is unsolved. The first version is native
  or same-sandbox only.

## Alternatives considered

- **Launch-only, as ADR-050 said.** Rejected: flags, profiles and plugins would
  silently not apply, and the settings row would have to say so.
- **A plugin-style runtime.** Rejected in ADR-039, and it would hand a runtime a
  plugin's grants.
- **A trait with no wire form.** Rejected: it forces every runtime into
  Cordial's process and Cordial's licence.
- **An in-tree macOS runtime.** Rejected by the spike's section 1.
- **Metal on Vulkan, built here.** Rejected: the spike found no evidence for
  "easy" and no present need. Estimated 6 to 12 person-months, error bar 2x, a
  judgement and not a measurement.

## Reopen when

The spike's proof of concept reports; Darling gains a Wayland backend or arm64;
Roblox removes desktop OpenGL from the macOS client; or the maintainer vets a
third-party runtime and the listing mechanism has to exist.
