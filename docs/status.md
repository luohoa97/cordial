# Status

<!-- description: What works in Cordial today, what is partly working, and what is broken. -->
<!-- icon: gauge -->

Experimental, but playable: sign in, load a game, move around. Install it
expecting rough edges. What changed release by release is on the
[releases page](https://github.com/luohoa97/cordial/releases) and in
[`CHANGELOG.md`](../CHANGELOG.md).

## What works

"Works" means it was observed working, not that it is finished.

| Area | State | Notes |
|---|---|---|
| Loading `libroblox.so` natively | Works | The engine is extracted once and reused; only a new Roblox build re-extracts |
| App shell | Works | Reaches `APP_READY (Landing)` |
| Rendering | Works | Vulkan, on both backends |
| Networking and HTTPS | Works | |
| Loading into an experience | Works | World, avatar and UI render, signed in |
| Engine content store | Works | `RbxStorage` is a real SQLite database; cache hits rise across launches, so assets are not refetched every session |
| Clean shutdown | Works | Full pause, stop and destroy sequence, seen in the engine's own log |
| Signing in | Works | Through Quick Sign-in, a code flow that needs no typing |
| Staying signed in | Works | Cookies and identity are kept in the desktop keyring, not a file |
| Profiles | Works | A chooser above Launch; creates one, and shows a profile another window holds as unavailable |
| Two accounts at once | Works | Two profiles, two instances, side by side; budget about 1.5 GB of memory each ([ADR-012](adr/ADR-012-profiles-and-instances.md)) |
| Keyboard in an experience | Works | WASD, space, the lot |
| Mouse: navigation, buttons, focus | Works | |
| Mouse: turning the camera | Works | Right-drag, using the compositor's unaccelerated delta, so sensitivity does not depend on your desktop mouse settings |
| Scroll wheel | Works | |
| Pointer capture in first person | Works | The cursor stays in the window |
| Typing into text fields | Works | A GTK overlay draws the focused Android field live, with caret movement and Wayland IME preedit |
| Window | Works | libadwaita header bar, engine as a subsurface |
| Fullscreen | Works | F11 acts on the gameplay window, hides the title bar and persists per profile |
| Audio | Works | Sound in an experience, from real play |
| Voice chat | Works | In a real game on the 0.15.0 binary, joined by deep link and the game's own Connect control; details in [voice-dual-response](analysis/voice-dual-response.md) |
| Feral GameMode | Works | Registered while the client runs |
| Asset overlays | Works | Custom textures, sounds and fonts; non-destructive, remove the file and the original returns ([asset-overrides](asset-overrides.md)) |
| Launching from the shell | Works | Finds a build, or explains how to get one |

## What is partly working

| Area | State | What is missing |
|---|---|---|
| Web views (Marketplace, Profile, Communities) | Partly | They render in a signed-in WebKitGTK window and both observed bridge formats reach the engine. More pages need interactive coverage, and a page-specific bridge command can still use engine vocabulary Cordial has not seen |
| Plugins | Partly | Host, broker and per-profile grants enforce every capability. Settings can grant or revoke one, and install or remove a plugin from a local `.tar.zst`. There is no in-app fetch from a remote index, so the marketplace half of the registry is unbuilt ([plugins](plugins.md)) |
| Text entry coverage | Partly | The overlay needs testing across more field types and input methods |
| Frame rate | Unsettled | See below |

### Frame rate

A flat 60 is the engine's own frame target, not a display lock. Measured
2026-10-03 on a 240.001 Hz output with input driven the whole run: the engine
was told the real rate and still held 59.9 presents a second until
`DFIntTaskSchedulerTargetFps` was raised past 60. **Settings → General →
Graphics → Frame rate limit** raises it ([fastflags.md](fastflags.md)); MAILBOX
and FIFO both clear 60 with it set. What that measurement did not cover, and
the older records it replaces, are in
[ADR-044](adr/ADR-044-settings-reach-a-running-game.md).

## What is broken

From the [0.23.2 release notes](releases/v0.23.2.md), the newest list:

- **The signed-in startup freeze is not fixed.**
- **Movement keys sometimes stop working after joining a game**
  ([#29](https://github.com/luohoa97/cordial/issues/29)). Respawning or re-joining
  brings them back.
- **A touchscreen can crash the client**
  ([#36](https://github.com/luohoa97/cordial/issues/36)).
- **A Roblox version released only for ARM64 is not shown on x86_64 machines.**
- **Some games show the mobile (touch) interface.**

The rest of the list in the
[0.23.0 notes](releases/v0.23.0.md) still applies.

## Not tested

- **NVIDIA GPUs.** First runs recorded 2026-10-03 (RTX 4070, driver 615.71.09):
  boots, renders, survives resize and fullscreen, and MAILBOX holds the panel's
  rate. Unverified on hybrid laptops, on 535/550, and in a Flatpak; see
  [NVIDIA graphics](nvidia.md).
- **Real ARM64 hardware.** See [Architectures](multiarch.md).
- **The AppImage's web view on a machine without WebKitGTK**, and on anything but
  Fedora. See [Installing Cordial](install.md).

<!-- cards -->

- [Installing Cordial](install.md): get it running
- [Checking this machine](doctor.md): diagnose a machine that will not start

<!-- /cards -->
