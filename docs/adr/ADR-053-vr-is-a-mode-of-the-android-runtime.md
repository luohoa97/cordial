# ADR-053: VR is a mode of the Android runtime, and a profile serves both builds

**Status:** accepted
**Date:** 2026-10-01
**Supersedes:** decision 5 of [ADR-043](ADR-043-the-roblox-build-is-the-binarys-architecture.md) ("Quest is rejected") and the premise of its decision 3, and, for the Quest build only, [`docs/multiarch.md`](../multiarch.md)'s "no translation layer will be designed". ADR-043's other decisions stand for the phone build.
**Related:** [ADR-001](ADR-001-in-process-hooking.md), [ADR-012](ADR-012-profiles-and-instances.md), [ADR-013](ADR-013-per-profile-configuration.md), [ADR-033](ADR-033-roblox-versions-are-a-keyed-store.md), [ADR-037](ADR-037-one-lock-and-a-content-hash-for-the-build-store.md), [ADR-050](ADR-050-other-runtimes-are-launched-not-built.md), [ADR-052](ADR-052-the-runtime-spec.md)
**Design and measurements:** [`docs/vr/dynarmic-design.md`](../vr/dynarmic-design.md). **User page:** [`docs/vr.md`](../vr.md).

## What changed

ADR-043 rejected the Quest build because running an arm64 `libroblox.so`
looked like it needed a second, arm64 `cordial-run` under qemu-user, and that
container saw only llvmpipe. The VR work did not do that. The x86-64
`cordial-run` links the Quest build's arm64 engine itself and runs its code
under dynarmic, in-process, and the engine's Vulkan and OpenXR calls reach the
host's own GPU driver and OpenXR runtime through generated thunks
(`cordial-run --guest-arm64 --app-bridge`). Measured on Monado's simulated HMD
(design §9.5, §9.8, §9.9): 89.95 to 90.00 `xrEndFrame`/s at 90 Hz on the
signed-out landing panel in 3 of 3 runs, 143.93/s at 144 Hz; in game, 44 to 48
frames/s at 90 Hz after the first performance pass and 51.7 to 65.3 at 72 Hz in
the second. **No frame rate has been measured on WiVRn.** The design's part B
(WiVRn, a Quest 3 displaying) has been used by hand but not measured: one
WiVRn run is recorded for a stop at 193 s (§9.6, since fixed), and a later
hands-on session is noted under "Not established" below.

## Decision

1. **VR is a launch mode of the built-in Android runtime, not a runtime in
   ADR-052's sense.** ADR-052's runtime is a separate program that advertises
   a `runtime.json` and is spoken to over a socket, and ADR-050 asked first
   what of Cordial's Android layers it would reuse. The VR mode reuses all of
   them: the same binary, the bionic linker, the JNI layer, the framework
   answers, profiles, FastFlags, plugins and live settings. It differs by two
   arguments. Calling it a runtime would mean Cordial implementing the spec
   to talk to itself, and listing it beside third-party runtimes that ADR-052
   deliberately does not list. ADR-052's "a runtime declares `arch`, and
   Cordial hides one that does not match the host" is not contradicted: the
   binary is x86-64 on an x86-64 host, and the guest ABI is internal to it, as
   an Android device's ABI is internal to Android.

2. **The translator stays inside ADR-001 on the design's three conditions**
   (§7): dispatch is keyed only on Cordial's own stub addresses and SVC ids,
   never on engine addresses; no plugin sees translator state; and the
   engine's code is never written. The engine's unmodified instructions run,
   and behaviour changes only through what a real platform answers. A change
   that breaks one of the three reopens this record.

3. **The user supplies the Quest build, and Cordial downloads none.** Settings
   → VR imports an APK file or pulls it from a connected Quest with `adb`
   (`pm path com.roblox.client`, then `adb pull`), and `cordial
   --import-quest-apk FILE` does the same with no window. The archive must
   verify against a pinned certificate, as ADR-033 requires of every build.
   **Measured:** the Quest build 2.740.0.927 verifies to a different
   certificate from the phone build's (`6d13fc84…`, self-signed, O=Roblox
   Corporation). It is pinned in a separate `quest_certificates` list that
   only the import reads, so it widens nothing an update or a mirror download
   will accept. An arm64 *phone* build is refused by the absence of
   `libovrplatformloader.so`. The headset route is a sequence of Settings
   subpages that each check before the next (adb, developer mode, the USB
   debugging prompt, the installed version, the copy), and is read-only on
   the headset: `devices`, `pm list packages`, `dumpsys package`, `pm path`,
   `pull`. Every `pm path` entry is pulled, so a split install is filed too.

4. **The store is keyed by ABI and version.** The phone build stays at
   `builds/<version>/`, unchanged. The Quest build goes to
   `builds/arm64-v8a/<version>/`, with every `lib/arm64-v8a/*.so` (the
   translator links more than the engine), the APK, `.version`, `.signer` and
   `.content-sha256`. `store::list_in` skips a non-version directory, so the
   phone store's listing, pruning and Version page never see it. The Quest
   store has its own lock and keeps three builds; there is no pin, because
   only the user adds to it.

5. **The OpenXR runtime is chosen per launch and never installed.** Cordial
   reads the system's active runtime (`$XDG_CONFIG_HOME`, then
   `$XDG_CONFIG_DIRS`, then `/etc`, the loader's order), WiVRn's Flatpak
   (`flatpak info --show-location`), SteamVR's manifest, and manifests in
   `share/openxr/1/`. A setting picks one, defaulting to the system's, and the
   launcher passes it to that client alone in `XR_RUNTIME_JSON`. Cordial never
   writes `active_runtime.json`. When WiVRn is the choice and `wivrn-server`
   is not running, the launcher says so and names the command
   (`--no-manage-active-runtime`); it does not start it, because the shell
   starts no long-lived helpers and owning a server's lifetime would be a
   design of its own.

6. **A profile serves both builds: one identity, two engine stores.**

   | Shared, at the profile's top level | Kept apart |
   |---|---|
   | `.lock` (ADR-012) | engine storage: phone in `data/`, `run/`; Quest in `quest/data/`, `quest/run/` |
   | the saved sign-in, keyed in the secret service by the profile's path (`cookies`/`identity` in the file fallback) | unpacked assets: `cordial/assets` and `cordial/assets-arm64-v8a` |
   | the engine's secure local-storage values, keyed the same way | |
   | `flags.json`, `plugin-grants.json`, plugin settings, the phone build's pin | |

   **Why the engine storage is split.** The builds are different engine
   versions (2.737 phone, 2.740 Quest) with different caches, and the Quest
   build writes VR state into `GlobalBasicSettings_13.xml`. The same 74 keys
   compared between a Quest profile and Sober's phone profile on one machine:
   `VREnabled`, `HasEverUsedVR`, `FramerateCap`, `GraphicsOptimizationMode`,
   `ControlMode` and the sensitivities differ. One file read by both would
   carry a VR frame-rate cap and control mode into the phone build. **Why the
   assets are split:** one directory re-extracted on every switch left each
   build's extra files in the other's tree (the Quest APK carries
   `shaders_vulkan_mobile_vr.pack`) and would rewrite files under a running
   client of the other build. **Why the sign-in is shared:** the session is
   the account's, both builds already resolve it from the profile path, and
   the requirement is that a user signs in once.

   The phone build keeps the top level, so an existing profile is untouched:
   nothing moves, and a profile never used for VR never gains `quest/`. A VR
   run before this change wrote Quest storage to `data/`, so the client moves
   `data/` and `run/` into `quest/` once, under the lock, only when
   `GlobalBasicSettings_13.xml` records `HasEverUsedVR` true, and never when
   `quest/` exists. **INFERRED:** that no phone build writes `HasEverUsedVR`
   true; Sober's phone 2.737 profile reads false.

7. **The launcher's entry is secondary.** "Play in VR" sits under the Roblox
   button, which keeps its place and look. It is absent on a host that is not
   x86-64, insensitive until all three prerequisites are met, and says the
   first missing one beneath it, with a link to Settings → VR. `--diagnostics`
   gains a VR line and `--doctor` VR checks, which are never worse than `info`
   unless a chosen runtime has gone.

## When Roblox stops accepting the build

Roblox refuses old clients after an update, so a Quest build goes stale with
every Roblox update. **Nothing tells Cordial in advance:** the `client-version`
endpoint answers only for the Windows and Mac players (`AndroidApp` gets 500,
`cordial_update::version`), and the release-notes number the updater reads
says a version was announced, not that the Quest build has it. **Nothing tells
it at run time either, yet:** the phone build reports `app upgrade status
0/3` through a GameActivity hook (`init_params.cpp`) the Quest build never
calls, and no Quest run on this machine (594 logs) contains an upgrade signal.
A detector written now would be a guess, so there is none. What is built
instead: the headset step compares the headset's `versionName` with the
stored build and says "update it on the headset first" when they match; the
crash page of a VR run says, conditionally, to update and copy again; and the
previous build stays in the store until a new one is filed. The first capture
of a refused Quest client is what a real detector needs.

## Measured for this change, 2026-10-01

From the launcher's own action (`win.launch-vr`, the same call as the
button) in a nested headless KWin, throwaway data root, Monado's simulated
HMD: the Quest build imported from the CLI, `XR_RUNTIME_JSON` set from the
setting, `FOCUSED`, the landing panel, and a 60 s run ending in exit 0. Rate in
10 s windows: 71.5, 85.9, 90.0, 62.2, 68.5, 63.5 frames/s at 90 Hz, on a host
shared with other builds. A second client on the same profile, phone build,
was refused with exit 3 while the VR client held it. A phone launch on the
same profile afterwards used `data/`, `run/` and `cordial/assets`, left
`quest/` alone, and read the same `<profile>/cookies` path the VR run had.

One run ended in SIGSEGV 29 s into the session, in MangoHud's Vulkan layer
inside Monado's compositor (`comp_target_swapchain_present` → `libMangoHud.so`),
with `MANGOHUD=1` in the environment; the same run with `MANGOHUD=0` ran to
the end. Seen once.

## Not established

- Sign-in carrying across the builds is **INFERRED** from both reading one
  path; no signed-in run of each build on one profile was made.
- Sharing `flags.json` across engine versions is **INFERRED** harmless: the
  engine ignores a flag it does not know.
- No frame rate has been measured on WiVRn. A person ran the onboarding
  (Settings → VR → Get It from Your Quest) end to end with a real headset,
  and played a game made for VR on a Quest 3 over WiVRn, reporting it mostly
  above 60 frames/s with occasional slight stutters; that is an observation,
  with no frame log taken.
- The in-game Leave button and rejoining on WiVRn are still open
  ([`vr/play-button.md`](../vr/play-button.md)). Pressing Play in the headset
  is not: on a Quest 3 over WiVRn it joined in three sessions of three, each
  `Game.launch` carrying `joinAttemptOrigin` as a Play press does rather than
  a deep link's `referralPage: "DeepLink"` (play-button.md, "In the
  headset"). Haptics
  over WiVRn 26.9 buzz continuously because of a bug in WiVRn's headset app,
  fixed upstream in WiVRn pull request #1131 and not yet released.
- Pinning the Quest certificate is a trust decision for the maintainer; its
  provenance is the headset's store install, recorded beside the digest.

## Reopen when

A WiVRn run from the launcher measures a frame rate; Roblox ships a Quest
build signed by another key; a phone build is seen writing `HasEverUsedVR`;
or a change to the translator crosses one of decision 2's conditions.
