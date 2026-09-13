# Documentation index

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
| [`plugins.md`](plugins.md) | Installing a plugin from an archive, and why Cordial fetches Deno |
| [`architecture.md`](architecture.md) | How the pieces fit, as a diagram: shell, linker, symbol table, JNI, framework, plugins |
| [`HANDOVER.md`](HANDOVER.md) | Written for whoever takes this on: every open thread, which claims are `INFERRED`, and the traps |
| [`../CHANGELOG.md`](../CHANGELOG.md) | What changed between releases, retractions included. [Releases](https://github.com/luohoa97/cordial/releases) |
| [`findings.md`](findings.md) | Bootstrap analysis: the architecture verdict and what is unknown |
| [`framework-api-inventory.md`](framework-api-inventory.md) | The framework backlog, enumerated from the shipping APK |
| [`traces/`](traces) | A capture of the same APK on real Android — the ground truth this project checks itself against |

## ADRs

| | |
|---|---|
| [ADR-001](adr/ADR-001-in-process-hooking.md) | Why Cordial has no in-process hooking, ever |
| [ADR-004](adr/ADR-004-plugin-asset-overrides.md) | Superseded by ADR-010 — why plugins were once refused asset overrides |
| [ADR-005](adr/ADR-005-flag-service.md) | Why the flag service has two surfaces |
| [ADR-006](adr/ADR-006-plugin-events-and-first-party.md) | Plugin-declared events, and why built-in features are still plugins |
| [ADR-007](adr/ADR-007-host-resources-are-brokered.md) | Why a plugin never holds a socket, and Discord RPC as the worked example |
| [ADR-008](adr/ADR-008-plugins-are-typescript-on-deno.md) | Why plugins are TypeScript rather than Lua, and what a Deno start actually costs |
| [ADR-009](adr/ADR-009-capture-yes-overlay-injection-no.md) | Recording Cordial is supported; loading an overlay into it is not |
| [ADR-010](adr/ADR-010-plugin-asset-overlays.md) | Why plugins may now overlay Roblox's assets, non-destructively |
| [ADR-012](adr/ADR-012-profiles-and-instances.md) | A profile is storage, an instance is a window, and why one profile takes a lock |
| [ADR-013](adr/ADR-013-per-profile-configuration.md) | Flags, grants and plugin settings belong to the profile; plugin code belongs to the machine |
| [ADR-014](adr/ADR-014-plugin-registry-and-unpacking.md) | Where plugins come from, and how an archive is unpacked without trusting it |
| [ADR-015](adr/ADR-015-fetching-the-roblox-build.md) | Cordial may fetch a Roblox build and may never ship one |
| [ADR-016](adr/ADR-016-per-profile-network-egress.md) | Why a profile can require a VPN, and what that does and does not guarantee |
| [ADR-017](adr/ADR-017-sober-issue-corpus.md) | Why the local Sober issue corpus exists and what it deliberately drops |
| [ADR-018](adr/ADR-018-plugin-sub-sandboxing.md) | A kernel sandbox under Deno, why it cannot replace the broker, and the Flatpak grant not taken |

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
| [`analysis/desktop-integration-audit.md`](analysis/desktop-integration-audit.md) | What is already native-feeling about the `.desktop` entry, icons and deep links, and what is not |

Writing a plugin rather than installing one: [`plugins/README.md`](../plugins/README.md).
