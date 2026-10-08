# Documentation index

The user and plugin-developer pages here are also published, with search, at
[cordial.mintlify.app](https://cordial.mintlify.app). This index covers the
whole folder, including the working notes the site leaves out.

Start with [`NEXT.md`](NEXT.md). The rest here is reference, in roughly the
order a newcomer would want it.

| | |
|---|---|
| [`NEXT.md`](NEXT.md) | Where to start, what is blocking, and what has already been ruled out |
| [`status.md`](status.md) | The current feature table, what changed recently, and three of the harder bugs it took to get here |
| [`install.md`](install.md) | Full install detail: every package format, signing and repository trust, building from source |
| [`fastflags.md`](fastflags.md) | Overriding Roblox's FastFlags, and how layering between user/plugin/base works |
| [`controllers.md`](controllers.md) | Why controller button glyphs may show the wrong brand |
| [`rich-presence.md`](rich-presence.md) | The bundled Discord Rich Presence plugin: what it does, and what is not wired up yet |
| [`shaders.md`](shaders.md) | The vkBasalt switch: sharpening and anti-aliasing over the game, and its config file |
| [`mangohud.md`](mangohud.md) | The MangoHUD switch: what it shows, and how to install the layer |
| [`vr.md`](vr.md) | Play in VR: getting Roblox off your own Quest, choosing the OpenXR runtime, updating after Roblox updates, and what is still broken |
| [`nvidia.md`](nvidia.md) | NVIDIA graphics: what is known, what Cordial does, the Flatpak driver extension, and how to report a problem. Two users' NVIDIA reports so far |
| [`plugins.md`](plugins.md) | Installing a plugin from an archive, and why Cordial fetches Deno |
| [`runtime-spec.md`](runtime-spec.md) | Draft `cordial.runtime/1`: the protocol between the launcher and a runtime. Nothing implements it yet |
| [`architecture.md`](architecture.md) | How the pieces fit, as a diagram: shell, linker, symbol table, JNI, framework, plugins |
| [`HANDOVER.md`](HANDOVER.md) | Written for whoever takes this on: every open thread, which claims are `INFERRED`, and the traps |
| [`../CHANGELOG.md`](../CHANGELOG.md) | What changed between releases, retractions included. [Releases](https://github.com/luohoa97/cordial/releases) |
| [`findings.md`](findings.md) | Bootstrap analysis: the architecture verdict and what is unknown |
| [`framework-api-inventory.md`](framework-api-inventory.md) | The framework backlog, enumerated from the shipping APK |
| [`traces/`](traces) | A capture of the same APK on real Android — the ground truth this project checks itself against |

## ADRs

All 53. Status is marked where it is not plain "accepted"; [`HANDOVER.md`](HANDOVER.md#the-adr-index) carries every record's own status line.

| | |
|---|---|
| [ADR-001](adr/ADR-001-in-process-hooking.md) | Why Cordial has no in-process hooking, ever (the proposal was rejected) |
| [ADR-002](adr/ADR-002-core-shell-and-ui-handoff.md) | Core shell, UI handoff, and the cold-start ordering |
| [ADR-003](adr/ADR-003-plugin-isolation.md) | Plugins have no memory access to Cordial |
| [ADR-004](adr/ADR-004-plugin-asset-overrides.md) | Superseded by ADR-010 — why plugins were once refused asset overrides |
| [ADR-005](adr/ADR-005-flag-service.md) | Why the flag service has two surfaces |
| [ADR-006](adr/ADR-006-plugin-events-and-first-party.md) | Plugin-declared events, and why built-in features are still plugins |
| [ADR-007](adr/ADR-007-host-resources-are-brokered.md) | Why a plugin never holds a socket, and Discord RPC as the worked example |
| [ADR-008](adr/ADR-008-plugins-are-typescript-on-deno.md) | Why plugins are TypeScript rather than Lua, and what a Deno start actually costs |
| [ADR-009](adr/ADR-009-capture-yes-overlay-injection-no.md) | Recording Cordial is supported; loading an overlay into it is not |
| [ADR-010](adr/ADR-010-plugin-asset-overlays.md) | Why plugins may now overlay Roblox's assets, non-destructively |
| [ADR-011](adr/ADR-011-wayland-and-libadwaita.md) | Superseded in part by ADR-024. Wayland is primary and the window is libadwaita; X11 came back |
| [ADR-012](adr/ADR-012-profiles-and-instances.md) | A profile is storage, an instance is a window, and why one profile takes a lock |
| [ADR-013](adr/ADR-013-per-profile-configuration.md) | Flags, grants and plugin settings belong to the profile; plugin code belongs to the machine |
| [ADR-014](adr/ADR-014-plugin-registry-and-unpacking.md) | Where plugins come from, and how an archive is unpacked without trusting it |
| [ADR-015](adr/ADR-015-fetching-the-roblox-build.md) | Cordial may fetch a Roblox build and may never ship one |
| [ADR-016](adr/ADR-016-per-profile-network-egress.md) | Why a profile can require a VPN, and what that does and does not guarantee |
| [ADR-017](adr/ADR-017-sober-issue-corpus.md) | Why the local Sober issue corpus exists and what it deliberately drops |
| [ADR-018](adr/ADR-018-plugin-sub-sandboxing.md) | A kernel sandbox under Deno, why it cannot replace the broker, and the Flatpak grant not taken |
| [ADR-019](adr/ADR-019-development-control-surface.md) | A development control surface, in coordinates and pixels |
| [ADR-020](adr/ADR-020-declarative-plugin-preferences.md) | A plugin declares its preferences; Cordial draws them (proposed) |
| [ADR-021](adr/ADR-021-everything-is-a-plugin.md) | Everything is a plugin; code is a property, not a category (proposed) |
| [ADR-022](adr/ADR-022-plugins-observe-decide-act.md) | A plugin observes, decides and acts; that is what justifies a runtime (proposed) |
| [ADR-023](adr/ADR-023-host-audio-backends.md) | PipeWire is the primary audio backend, and the others go behind a seam |
| [ADR-024](adr/ADR-024-x11-is-supported-again.md) | X11 is supported again, and it gets the editor |
| [ADR-025](adr/ADR-025-fetching-from-a-third-party-mirror.md) | Cordial may fetch the build from a third-party mirror, if it can prove Roblox signed it |
| [ADR-026](adr/ADR-026-the-core-event-bus.md) | Cordial publishes what it observes, and plugins may never veto it |
| [ADR-027](adr/ADR-027-plugin-overlays.md) | Plugins describe an overlay; Cordial draws it (proposed) |
| [ADR-028](adr/ADR-028-x11-input-comes-from-xinput2.md) | X11 input comes from XInput2, with the warp as the fallback |
| [ADR-029](adr/ADR-029-overlays-are-three-decisions.md) | An overlay is three decisions, and every overlay makes all three |
| [ADR-030](adr/ADR-030-reports-arrive-from-discord.md) | Reports arrive from Discord, as forms rather than as messages |
| [ADR-031](adr/ADR-031-the-launcher-outlives-its-window.md) | The launcher outlives its window, and the client is a child process |
| [ADR-032](adr/ADR-032-appimage-build-base-moves-to-ubuntu-24-04.md) | The AppImage's build base moves to Ubuntu 24.04, and the version floor that blocked it was wrong |
| [ADR-033](adr/ADR-033-roblox-versions-are-a-keyed-store.md) | Roblox builds live in a keyed store, and a profile names one |
| [ADR-034](adr/ADR-034-symbol-resolution-asks-the-library.md) | Symbol resolution asks the library, not a checked-in list |
| [ADR-035](adr/ADR-035-browser-account-routing.md) | Match browser joins to saved accounts |
| [ADR-036](adr/ADR-036-unsafe-is-a-boundary-not-a-convention.md) | The unsafe/safe boundary is a lint, not a convention |
| [ADR-037](adr/ADR-037-one-lock-and-a-content-hash-for-the-build-store.md) | The build store's three writers share one lock, and an entry now proves its own bytes |
| [ADR-038](adr/ADR-038-plugin-hot-swap.md) | A running client reconciles its plugin set; nothing pushes to it |
| [ADR-039](adr/ADR-039-a-runtime-backend-seam-and-why-macos-waits.md) | A runtime-backend seam is cheap to describe and not worth building yet. Corrected 2026-10-01 on Metal, Darling and the loader, and on its quotation of "do not build a translation layer", which ADR-053 no longer follows for the Quest build |
| [ADR-040](adr/ADR-040-the-engine-already-runs-mimalloc.md) | The engine already runs mimalloc, so there is no allocator to switch |
| [ADR-041](adr/ADR-041-vkbasalt-post-processing.md) | vkBasalt post-processing is a driver-stack layer, not in-process hooking |
| [ADR-042](adr/ADR-042-texture-format-query-observability.md) | Vulkan texture-format queries are counted and, test-only, maskable — nothing is translated |
| [ADR-043](adr/ADR-043-the-roblox-build-is-the-binarys-architecture.md) | The Roblox build's architecture is the binary's; choosing another needs a second runtime, so Settings shows it read-only. Its "Quest is rejected" and the premise of its decision 3 (a second `cordial-run`) superseded by ADR-053 |
| [ADR-044](adr/ADR-044-settings-reach-a-running-game.md) | Settings that can change reach a running game over a small socket; the rest say "Applies at next launch" |
| [ADR-045](adr/ADR-045-one-report-screen-outside-settings.md) | One Report a Problem screen, outside Settings; the launcher says so when the game will open on X11 |
| [ADR-046](adr/ADR-046-nvidia-is-gated-on-the-vendor-id.md) | NVIDIA behaviour is gated on the device's vendor id, advisory unless the evidence is strong, and says what is inferred |
| [ADR-047](adr/ADR-047-the-canvas-is-lowered-only-under-a-presented-frame.md) | The canvas is lowered under GTK only once GTK has presented a frame, so a stalled GTK never leaves a grey screen |
| [ADR-048](adr/ADR-048-labels-and-edits-from-discord.md) | Labels and edits from Discord are allowlisted, attributed and logged before they happen |
| [ADR-049](adr/ADR-049-etc2-is-emulated-where-the-driver-lacks-it.md) | ETC2/EAC is decoded on the CPU where the driver lacks the feature, gated on the feature and not the vendor id; supersedes the "nothing is translated" half of ADR-042 |
| [ADR-050](adr/ADR-050-other-runtimes-are-launched-not-built.md) | Other runtimes (Mac O' Blox) are detected and launched, never built into Cordial; parked until after 1.0. Launching design superseded in part by ADR-052 |
| [ADR-051](adr/ADR-051-overrides-are-reapplied-after-the-engines-refresh.md) | The profile's flag overrides are handed to the engine again after each of its own settings refreshes, triggered by its log |
| [ADR-052](adr/ADR-052-the-runtime-spec.md) | Launcher features reach runtimes through a published spec, `cordial.runtime/1`; Cordial lists only its own runtime for now; supersedes ADR-050's launching design in part; decision 4 superseded by ADR-055 |
| [ADR-053](adr/ADR-053-vr-is-a-mode-of-the-android-runtime.md) | VR is a launch mode of the Android runtime: the Quest build, user-supplied and keyed by ABI in the store, run under an in-process translator; the OpenXR runtime is passed per launch; a profile shares its sign-in and lock across both builds and keeps their engine storage apart. Supersedes ADR-043's "Quest is rejected" |
| [ADR-054](adr/ADR-054-cordial-owns-its-roblox-builds.md) | Cordial owns its Roblox builds: launches resolve from a store of verified builds, each profile follows Latest or a pinned version, and Sober's copy is an explicit import |
| [ADR-055](adr/ADR-055-the-launcher-and-the-runtime-are-two-programs.md) | The launcher and the runtime are two programs speaking `cordial.runtime/1`; plugins and hot-swap stay in the launcher; a separately packaged runtime is a Flatpak extension. Proposed |

## Design notes

| | |
|---|---|
| [`design/instances-and-launch.md`](design/instances-and-launch.md) | Multi-instance, multi-account, and `roblox://` |
| [`design/sign-in.md`](design/sign-in.md) | What signing in actually requires — the current blocker |
| [`design/path-to-a-frame.md`](design/path-to-a-frame.md) | GameActivity, assets, surface |
| [`base-evaluation.md`](base-evaluation.md) | Port-vs-write assessment of the prior art |
| [`multiarch.md`](multiarch.md) | Multi-architecture decision |
| [`design/flatpak-remote-signing.md`](design/flatpak-remote-signing.md) | The exact procedure for signing the Flatpak remote, for whoever holds the key |
| [`design/apt-repository.md`](design/apt-repository.md) | The APT repository: the key, how it is published, and why official Debian is a different question |
| [`design/rpm-repository.md`](design/rpm-repository.md) | The dnf repository: `$releasever` layout, the key, and why official Fedora is a different question |
| [`design/pacman-repository.md`](design/pacman-repository.md) | The pacman repository: the key, Chaotic-AUR and the AUR as separate routes |
| [`analysis/macos-runtime.md`](analysis/macos-runtime.md) | Spike: how Mac O' Blox runs the macOS client, what Metal on Vulkan would cost, and the draft runtime spec. Read-only, nothing run |
| [`analysis/runtime-protocol-review.md`](analysis/runtime-protocol-review.md) | Review of the draft runtime spec against the code, the Flatpak and crate recommendations, and the commit order for ADR-055. Read-only, nothing run |
| [`analysis/desktop-integration-audit.md`](analysis/desktop-integration-audit.md) | What is already native-feeling about the `.desktop` entry, icons and deep links, and what is not |

Writing a plugin rather than installing one: [`plugins/README.md`](../plugins/README.md).
