# Review of the draft runtime spec, and the plan to adopt it

2026-10-08. Read-only: nothing was built, no client was launched, nothing here was measured by running. Every statement about code is from reading it at `94c4b97` and is labelled where it is only an inference. The decision is [ADR-055](../adr/ADR-055-the-launcher-and-the-runtime-are-two-programs.md); the spec as revised is [`docs/runtime-spec.md`](../runtime-spec.md).

## 1. What was wrong or unclear in the draft (v0.1)

**Wrong against the code**

1. Section 8 said the portable core has "almost no Android coupling" and that `live_settings.rs` and `devctl.rs` were the coupled files. `plugin_host.rs` calls `crate::android::asset` (`explain`, `register_plugin_root`, `unregister_plugin_root`), `crate::flags` (`collect`, `read_layer`, `write_plugin_layer`, `plugin_dirs`), `crate::profile::active` and `crate::game_log::session_state`. It is the file the split depends on and it is coupled four ways.
2. `lifecycle.ready` was required, and `core_events::CLIENT_READY` is declared "and published by nothing"; the only other reference is the plugin-side name mapping in `cordial-plugins/src/host.rs`. `window.resized` is the same. A required event with no publisher is the lie section 4 forbids.
3. `crash {signal, summary, log_path}` was a runtime-sent event. A crashed process cannot send it, so it has no publisher by construction, and `log_path` breaks section 7's rule against path-valued messages. The launcher already learns of exit and crash from `glib::child_watch_add_local` and keeps a stderr tail for the crash page (`launch.rs`, ADR-031).
4. `window.mode: "embedded"` cannot exist. The engine's surface is a `wl_subsurface` of the runtime's own GTK toplevel (ADR-011; `cordial-runtime` imports `cordial_shell::host_window` 18 times), and a subsurface does not cross processes. Dropped from version 1.

**Contradictions inside the draft**

5. Versioning was in four places: manifest `spec` and `spec_version`, `v` on every line, `hello.spec`, and a per-capability integer. The manifest listed capabilities as `{"lifecycle":1}` and the handshake as `{name:{ver}}`. Now: the major in `spec` once, the minor only in the handshake, no per-line `v`, one capability shape.
6. Section 7 forbade passing a path, and the draft passed several: `flags.path` in the handshake, `events.log {dir, glob}`, `crash.log_path`, `assets.overlay`. Now exactly one (`assets.overlay.set`), and the flags document sits at a fixed name inside `{session_dir}`.
7. "A message that does not parse closes the session" meant, read literally, that a bad line ends the game. It now closes the connection.
8. `profile` was required and carried `instances.max`. ADR-012 makes a profile one lock and one instance, the launcher's concern. Dropped.

**Missing**

9. **Capability negotiation had no rule for which version wins.** Now the lower of the two, which is safe because versions are additive within a major.
10. **Reattach.** The launcher is restarted while games run, and nothing said how it finds a running runtime or catches up on events it missed. Now: the runtime listens on a path, one controller at a time (newest replaces), `hello {reattach:true}`, and `state.get`.
11. **EOF.** Nothing said the runtime keeps running when the socket closes. ADR-031 shows how easily "the client dies with the launcher" happens (the stdout pipe); the runtime must survive both.
12. **Backpressure.** `live_wire` is connect-per-request with 1,024-byte lines, which suits four keys. Events come from engine threads, and a launcher that stops reading must not stall the engine: a bounded queue of 256 that drops and counts, with `events.dropped`.
13. **Validation of runtime-to-launcher data.** The launcher is the trusted side and the runtime is a foreign process that may be hostile: per-field caps, integer ids, closed enumerations, out-of-range drops the message and is counted. Keyring identity comes from the manifest, never from the handshake.
14. **Timeouts.** A request with no reply in two seconds (the `IO_TIMEOUT` `live.rs` uses) is `failed`, never success.
15. **Who manages builds.** The draft's `updates` capability invented a second path. ADR-054 made the launcher's store the only place a build comes from, so a manifest field says which side manages them.
16. **A runtime-reported "alive" is not engine liveness.** The control thread answers while the engine is wedged (the freeze of 2026-08-21 left everything else running). Documented as `health`, optional; the present-count test through devctl is still how a wedged client is found.
17. **Socket path length.** `sun_path` is 108 bytes and a Flatpak profile directory is long. Session names are eight characters and a fallback binds through `/proc/self/fd`.

**Over-built for a first version**

18. `updates`, `doctor.requires`, `session.vault`, `window`, `launch.join`, the runtime-declared settings form, and `debug.control` are all removed from version 1. The settings form lets a foreign process put controls into the launcher's UI and raises localisation questions nobody has answered. `launch.join` is argv at spawn today. `debug.control` is the development surface and stays runtime-private (ADR-019).
19. `events.log` and `events.core` overlap; the shell's `freeze_recovery.rs` already parses the engine log itself and calls itself "a second copy of an idea" next to `cordial_runtime::game_log`. Left out until a second runtime wants the launcher to parse its log.

## 2. What the code sends today, mapped to the draft

| Today | Where | Becomes |
|---|---|---|
| Live settings: `{"set":{...}}` and `{"get":true}`, ten keys, 1,024-byte lines, connect per request, reply `{ok, applied, ignored, values, notes, error}` | `live_wire.rs`, `live.rs` to `live_settings.rs` | `settings.set` and `settings.get`; the reply keeps `notes`; the line cap becomes 64 KiB; the version-0 socket stays as an alias |
| Retry once a second for about 30 s when the socket is not there yet | `live.rs` (`GIVE_UP_AFTER`) | the connect retry in section 2 |
| Launch argv: `--lib-dir`, `--apk`, `--host-libc`, `--game-activity` (or `--guest-arm64 --app-bridge`), `--run`, `--profile`, `--join-url` | `launch.rs::spawn` | the built-in runtime keeps it in version 1; a third-party runtime gets manifest placeholders |
| About sixteen environment variables: `CORDIAL_GRAPHICS`, `_PRESENT_MODE`, `_FRAME_RATE_LIMIT`, `_THROTTLE`, `_POINTER_ACCEL`, `_GAMEPAD`, `_CLOSE_ON_LEAVE`, `_DEEPLINK_CARRY_TICKET`, `_UNPACKED_PLUGINS`, `_DEVICE_PROFILE`, `_PERFORMANCE`, `_AUDIO_SINK`, `_AUDIO_SOURCE`, `_GAMEMODE`, `_TITLE_BAR`, `_SECRET_STORE`, plus `MANGOHUD*`, `ENABLE_VKBASALT`, `XR_RUNTIME_JSON` | `launch.rs` | the first group is `settings` (next-launch or live by declaration). `CORDIAL_UNPACKED_PLUGINS` moves with the plugin host. The Vulkan layer variables are an open question (ADR-055) |
| Inherited `flock` descriptor | `Claim::hand_to` | unchanged; inherited at spawn, never sent |
| Piped stdout and stderr, last lines kept for the crash page | `launch.rs::pump` | unchanged; the runtime must survive `EPIPE` |
| Exit and crash | `glib::child_watch_add_local` | synthesised by the launcher; no runtime message |
| Freeze recovery: the shell reads the engine log directory for `Forcing finalize` without `RenderView destroyed[1]`, five seconds, restarts at most twice | `freeze_recovery.rs` | stays in the shell; optionally a `health` event later. It is Android-engine knowledge and belongs to the built-in runtime's side, which is why it is reported rather than parsed once there is a second runtime |
| devctl: `<profile>/devctl.sock`, `CORDIAL_DEV_CONTROL`, screenshot and input verbs | `devctl.rs`, `tools/cordial-mcp.py` | stays outside the protocol |
| Core events to plugins: `client.launch`, `engine.version`, `client.shutdown`, `game.presence`; `client.ready` and `window.resized` unpublished | `plugin_host::publish_core`, `load.rs` | launcher-synthesised: launch, shutdown. Runtime events: `engine.version`, `game.presence`, plus `game.joined` and `session.state` from `game_log::session_state`. Not offered: ready, resized |
| Plugin reconciler (poll once a second) | `plugin_host::start_reconciler` | the launcher |
| Asset overlay roots | `plugin_host` calling `android::asset::register_plugin_root` | `assets.overlay.set` |
| Flag layers: user, plugin, base; write grants; `Cordial*` pseudo-flags inside `flags.json` | `flags.rs`, `client_settings.rs` | the launcher resolves and strips `Cordial*`; the runtime applies and holds (`client_settings`, `flag_reapply`) |
| Deep-link translation | `cordial_runtime::deeplink::translate` | still the runtime; the launcher passes a validated, ticket-stripped URL |
| Secret store | `cordial_shell::secrets`, re-exported in `cordial-runtime/src/secrets.rs`, pinned by `CORDIAL_SECRET_STORE` | unchanged for the built-in runtime; `session.vault` deferred |
| In-experience web view | `cordial_shell::webview` imported 22 times by the runtime | stays in the runtime process; not protocol |

## 3. Transport and Flatpak, with the reasoning

**Path, runtime listens.** The launcher is restarted while a game runs (ADR-012, ADR-031). A descriptor handed at spawn dies with the launcher and cannot be handed again; a path can be found again. The cost is a connect retry, which `live.rs` already has. Directory `0700` under the profile is the shape ADR-044 chose and nothing here argues with it.

**Flatpak.** Three ways two programs reach each other were looked at:

1. *Shared xdg-run directory between two apps.* Needs `--filesystem=xdg-run/cordial-runtime:create` on both, and the launcher still cannot start the runtime without `--talk-name=org.freedesktop.Flatpak`, which ADR-002 and ADR-007 refuse. It also puts the socket outside the profile and its lock.
2. *A descriptor over a portal.* There is no portal that brokers a connection between two apps; `org.freedesktop.portal.Flatpak.Spawn` starts a sandbox of the caller's own application. (From the Flatpak documentation, not tried.)
3. *The launcher spawns the runtime, which is a Flatpak extension.* **Recommended.** One `add-extensions` block (see ADR-055) and no change to `finish-args`. The runtime is in the launcher's sandbox, so the socket is a plain profile path, `child_watch` works, the profile `flock` is inherited and Cordial's existing grants (dri, wayland, PipeWire, Discord socket) are the runtime's too.

The cost: an extension cannot hold a permission the launcher lacks. **Not built and not run here; every sentence about extension mounting is INFERRED from the Flatpak documentation.** The first thing to measure when it is built: that `/app/runtimes/<Name>/runtime.json` is visible to the launcher, that a child spawned from it inherits the lock descriptor, and that the socket path stays under 108 bytes with a real profile name.

## 4. The `cordial-protocol` crate

```
crates/cordial-protocol/
  src/lib.rs          re-exports
  src/version.rs      Protocol {major, minor}; negotiate(offered, accepted) -> live capability set
  src/frame.rs        Frame::{Request, Reply, Event}; encode_line; decode_line (MAX_LINE 64 KiB)
  src/lines.rs        bounded LineReader over Read: discards to newline on overflow, reports it
  src/error.rs        Code::{Unsupported, Invalid, Failed, Busy, NotReady}
  src/settings.rs     today's live_wire Update, Accel, Throttle, TitleBar, FrameRateLimit,
                      KEYS, valid_sink_name; one spelling for env and wire
  src/msg.rs          typed payloads per capability: Hello, HelloReply, SettingsReply,
                      FlagsApply, GameJoined, GameLeft, SessionState, EngineVersion,
                      GamePresence, AssetsOverlaySet, Diagnostics
  src/manifest.rs     RuntimeManifest: parse, validate, expand placeholders as argv items
  src/queue.rs        BoundedEvents: capacity 256, drop newest, count
  tests/vectors/      accept/*.jsonl and reject/*.jsonl, one line each, with the expected result
  src/conformance.rs  (feature "conformance") the shared cases below
```

Dependencies: `serde`, `serde_json`. No `libc`, no GTK, nothing that links native code, so a port builds it on any host.

**The conformance test is one set of cases both sides run.**

- *Codec vectors.* Every line in `accept/` decodes and re-encodes to the same value; every line in `reject/` fails with the named error. Cases include an oversize line, a request with two verbs, an unknown field (ignored), an out-of-range integer, and a string over 512 bytes.
- *Launcher-side cases, run against a runtime under test* (`cordial --runtime-check` and the built-in runtime's own CI): handshake and version negotiation; an unknown request returns `unsupported`; a line that does not parse closes the connection and the process lives; the socket closing leaves the process running and a second connection reattaches; `settings.set` with an unknown key reports it in `ignored` and applies the rest; no reply is a failure, not a success; `lifecycle.stop` ends it within the grace.
- *Runtime-side cases, run against a scripted hostile runtime* (a fake that the launcher's decoder reads): a flood of events does not stall the reader; a handshake with a different `runtime.id` than the manifest is refused; an oversize or malformed message is dropped and counted.

Both sets run over a `Link` that is either an in-process pair or a Unix socket, so the same cases cover the codec and the transport.

**Publishing.** Only `cordial-protocol` is a candidate for crates.io, after the split lands; all other crates stay `publish = false` (each links the AOSP linker or GTK). Its licence is **GPL-3.0-or-later**, the workspace's, because `LICENSE` is the GPL text and `CONTRIBUTING.md` says "The licence is settled" and declines a permissive carve-out. `NOTICE` and `THIRD-PARTY-NOTICES.md` need no entry for a first-party crate. The tension with ADR-052's "a trait with no wire form forces a runtime into Cordial's licence" is real for a Rust runtime that links the crate, and is left for the maintainer; the wire spec is the licence-neutral contract. `NOTICE` pointed at `COPYING`, which does not exist; it is `LICENSE`, and is corrected with this change.

## 5. Order of work

The first block is the only code before the 1.0 gates, and it touches none of `window.rs`, the start or publish sites in `load.rs`, the native shims or the startup path. Each commit keeps `cargo build --release` and `cargo test --workspace` green and the single-binary path working; the counts of passing tests are the control, quoted before and after.

**Before the gates (docs, plus code that is unused or a pure move)**

1. This change: spec v0.2, ADR-055, this note.
2. `cordial-protocol` skeleton: version, frame, lines, error, queue, vectors, tests. Nothing depends on it.
3. Move `live_wire.rs` and the four enums it needs into `cordial-protocol::settings`; leave `pub use` in `cordial_shell::live_wire`, `title_bar` and `frame_rate_limit` so no call site changes. The moved tests pass unchanged. The wire bytes are checked unchanged by a vector test taken from today's `encode_set`.

**After the gates, no behaviour change**

4. Extract `cordial-host-core`: profile layout and lock, the flag layer resolver split out of `flags.rs`, `roblox_api.rs`. Both crates depend on it and the old paths re-export. Check first that `cordial_runtime::profile` and `cordial_shell::profile` agree before merging them; they are two modules with the same name.
5. Add the conformance harness and an in-process fake runtime; still nothing in production uses the protocol.

**After the gates, additive**

6. `cordial-run` serves `ctl.sock` beside `live/settings.sock`: `hello`, `state.get`, `settings.set` and `.get` through the existing `live_settings` handler. The launcher connects when `CORDIAL_RUNTIME_PROTOCOL=1`, default off. Control: the same settings set both ways in one session produce the same `values`.
7. The runtime publishes `game.joined`, `game.left`, `session.state`, `game.presence` and `engine.version` over it through the bounded queue. The in-process plugin host is unchanged, so the launcher can compare what it received with what the host delivered in the same session. This is the A/B control for step 9.
8. The launcher keeps a session object (child, socket, state snapshot) and synthesises exit and crash from `child_watch`. Freeze recovery is unchanged.

**The move**

9. The launcher starts a plugin host per session fed by the events (flag off). Test with a recording plugin, not Discord: only one host may own the presence socket.
10. The flag layer resolver, write grants, `flags.apply` and `assets.overlay.set` move; the runtime's `register_plugin_root` becomes message-driven.
11. Flip the default to the launcher-hosted plugins. Keep the in-client host for one release behind an environment variable, then delete it.
12. Retire `live/settings.sock` after one release; move ADR-044's table into the runtime's declaration.
13. Manifest discovery, `--runtime-check`, the Flatpak `add-extensions` block, rename `cordial-runtime` to `cordial-android`, and the first crates.io publish of `cordial-protocol`.

**Release.** Nothing before step 11 is visible to a user, so no release is due for it. Step 11 changes what happens to plugins when the launcher crashes (the game now survives, the plugins do not), and its release notes should say that and what is still unmeasured.

## 6. What was not checked

Nothing was run or built, and no Flatpak was built. Not looked at: how `host_window` would be split out of `cordial-shell` for the last step; whether `cordial_runtime::profile` and `cordial_shell::profile` can merge; how often `live/settings.sock` is used by anything other than the shell (`tools/` was not searched). The sizes of a resolved `flags.json` were not measured, which is why the draft passes it as a file and not as a line. The Flatpak extension behaviour is inferred (section 3).
