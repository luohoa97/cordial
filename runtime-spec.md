---
title: "`cordial.runtime/1`: the runtime spec"
description: "The protocol between the Cordial launcher and a runtime that turns Play into a running Roblox client. The built-in runtime and the launcher speak it as their only channel; no other runtime exists."
icon: "microchip"
---
<Warning>

**Draft v0.3, accepted as design in [ADR-052](/adr/ADR-052-the-runtime-spec) and reshaped by [ADR-055](/adr/ADR-055-the-launcher-and-the-runtime-are-two-programs).** The message types and conformance cases are in the [`cordial-protocol`](https://github.com/luohoa97/cordial/tree/main/crates/cordial-protocol) crate. **The built-in runtime, `cordial-run`, serves this protocol and the launcher speaks it, as the only channel between them** (section 8 says what each side does and does not do yet). No other runtime exists, `cordial` has no code to load one from a manifest, and no `--runtime-check` command exists; the conformance harness runs against a listening runtime through the crate's `check_runtime` example. Treat the sections that no running code exercises as proposals. Sections marked **draft** are the least settled.

</Warning>

This page is for someone building a runtime. Cordial lists only its own built-in runtime for now ([ADR-052](/adr/ADR-052-the-runtime-spec)), so a runtime written to this spec will not appear in Cordial until the maintainer vets it. One that injects code into the Roblox client will not be listed at all.

There are two programs. The **launcher** (`cordial-shell`) owns profiles, settings, the FastFlag layers, plugins and their grants, presence, the secret store, doctor, the report screen and the launcher's own window. A **runtime** (`cordial-run` today) is whatever turns Play into a running Roblox client: it loads the engine, owns the game window, reports what happened, and accepts a small closed set of requests. The protocol carries events and effects. It never carries channels or code.

A port forks the runtime and nothing else. Why the line is here: [ADR-055](/adr/ADR-055-the-launcher-and-the-runtime-are-two-programs).

**The reference implementation of the wire is a crate.** [`cordial-protocol`](https://github.com/luohoa97/cordial/tree/main/crates/cordial-protocol) holds the framing, version negotiation, typed messages, the manifest type and a conformance harness. It is `MIT OR Apache-2.0`, unlike the rest of Cordial, so a launcher or a runtime can depend on it; it needs only `serde` and `serde_json`. A runtime in any language can implement this page and the line vectors in the crate's `vectors/` directory without it. Where this page was ambiguous, [section 9](#9-where-the-draft-was-ambiguous) says which reading the crate takes.

## 1. Manifest

`runtime.json`, searched in `$XDG_DATA_HOME/cordial/runtimes/<id>/`, then each `$XDG_DATA_DIRS/cordial/runtimes/<id>/`. Inside the Flatpak the launcher also searches `/app/runtimes/<name>/`, where a runtime packaged as a Flatpak extension is mounted.

```json
{
  "spec": "cordial.runtime/1",
  "id": "org.example.runtime",
  "name": "Example",
  "version": "0.13",
  "arch": ["x86_64"],
  "launch": {
    "exec": ["bin/run"],
    "args": ["--socket", "{socket}", "--profile", "{profile_dir}"]
  },
  "builds": "runtime",
  "capabilities": { "lifecycle": 1, "events.core": 1, "settings": 1 },
  "support_url": "https://example.org/issues",
  "licence": "MIT"
}
```

- `spec` is the protocol **major**, written once. The minor lives only in the handshake, so the two cannot disagree. A manifest naming a major Cordial does not know is shown as "needs a newer Cordial", not hidden.
- `exec` is relative to the manifest's directory. `arch` is the host architectures the runtime runs on; Cordial hides it on any other and offers no translation ([ADR-043](/adr/ADR-043-the-roblox-build-is-the-binarys-architecture)).
- `builds` says who manages Roblox builds. `"cordial"` means the launcher's store ([ADR-054](/adr/ADR-054-cordial-owns-its-roblox-builds)) picks one and `{build_dir}` is that entry. `"runtime"` means the runtime fetches and keeps its own, and the launcher shows no Version row for it.
- `capabilities` has the same shape as the handshake: name to integer version.
- The manifest is advertisement only, and the handshake is the truth. The launcher's keyring entries and its report use the **manifest's** `id`, never the one the runtime claims while running; a handshake whose `runtime.id` differs from the manifest is refused.
- Placeholders in `args`: `{socket}`, `{session_dir}`, `{profile_dir}`, `{build_dir}`, `{join_url}`. Each is substituted as a single argument, never through a shell. `{join_url}` is a `roblox-player:` link the launcher has already validated and stripped of its launch ticket ([section 6](#6-what-stays-in-the-launcher)).

<Note>

The built-in runtime is the exception in version 1. It keeps the argv and about fifteen environment variables the launcher passes today, and the profile lock descriptor is inherited. The one thing it is told that is a path the launcher chose is the session directory, in `CORDIAL_SESSION_DIR`, which is what `{session_dir}` is for a manifest-launched runtime. A third-party runtime gets only the placeholders above; its settings arrive over the socket.

</Note>

## 2. Transport

JSON lines, UTF-8, `\n`-terminated, at most 64 KiB a line. A longer one is a protocol error.

The launcher creates `<profile>/runtime/<session>/` with mode `0700` and passes it as `{session_dir}`. The **runtime listens** on `{socket}` inside it and the launcher connects, retrying for up to 30 seconds while the runtime loads (the delivery rule [ADR-044](/adr/ADR-044-settings-reach-a-running-game) already uses). The runtime listens, rather than the launcher, because the launcher can be restarted mid-game and has to find the same runtime again. Plugins never see this directory.

- **One controller at a time.** A new connection that completes a handshake replaces the old one, which is sent `bye {reason:"superseded"}` (either side may send `bye` before closing).
- **Socket paths are short.** `sun_path` holds 108 bytes and a profile directory under a Flatpak is already long. The launcher keeps `<session>` to eight characters, and a runtime that finds `{socket}` too long binds through `/proc/self/fd/<dirfd>/ctl.sock` instead.
- **Closing the socket does not stop the runtime.** Closing the launcher's window while a game runs is the ordinary case ([ADR-012](/adr/ADR-012-profiles-and-instances), [ADR-031](/adr/ADR-031-the-launcher-outlives-its-window)). On EOF the runtime keeps the game running, keeps listening, and expects a controller to reattach. It must also survive a closed stdout and stderr, which the launcher pipes for its crash page. **The built-in runtime survives the socket closing and does not yet survive its stdout closing**: a launcher that crashes still takes the client with it through that pipe, and only a launcher that is restarted while the first one's pipe is open, or a client started by hand with its output elsewhere, can be reattached to.
- **A line that does not parse closes the connection**, never the runtime.

```
{"id":7,"m":"settings.set","p":{"throttle":"off"}}                       request
{"id":7,"ok":true,"p":{"applied":["throttle"]}}                           reply
{"id":7,"ok":false,"e":{"code":"unsupported","detail":"..."}}            error reply
{"ev":"game.joined","n":41,"p":{"place_id":1818,"at":1700000000000}}      event, runtime to launcher
```

- Every request gets exactly one reply. A request with none within two seconds is treated by the launcher as `failed`, never as success.
- Replies may arrive in any order and are matched on `id`. Ids are per direction. Version 1 defines no runtime-to-launcher requests; the shape is reserved.
- Events carry a counter `n`. A runtime that produces events on an engine thread hands them to a bounded queue of 256 that drops the newest and counts, and reports the count as `events.dropped {count}`. A launcher that stops reading must never stall the engine.
- Everything a runtime sends is bounded and validated by the launcher: strings at most 512 bytes unless a capability says otherwise, ids and counters as integers, enumerations checked against the closed set. Unknown fields are ignored. An out-of-range one drops the message and is counted in the report.

## 3. Handshake and versioning

The launcher sends `hello {protocol:{major:1,minor:0}, cordial:"0.25.0", session, caps:{name:ver}}`, listing what it can serve. The runtime answers `{protocol:{major,minor}, runtime:{id,version}, client:{name,version,build}, caps:{name:ver}}`, listing what it offers. The live set is the intersection, and for each capability the version is the lower of the two.

- A **minor** adds optional capabilities, events and fields; receivers ignore what they do not know. A **major** breaks. The launcher refuses a runtime whose major it does not speak and names it.
- A capability's version is an integer that is additive within a major: it never changes meaning or loses a field. Taking the lower of two is therefore always safe.
- Runtime-private events are prefixed `x-<id>.`. Plugins never see them; they appear only in the report.
- A reattaching launcher sends the same `hello` with `reattach:true`, and `state.get` then returns what it missed.
- **Draft:** `cordial --runtime-check <manifest>` spawns the runtime, runs the handshake and the conformance cases from `cordial-protocol`, and prints doctor-shaped results. The cases exist, in the crate's `conformance` feature, and can be run against a runtime over any socket today; the command that wraps them does not.

## 4. Lifecycle

| Step | What happens |
|---|---|
| Spawn | The launcher claims the profile lock ([ADR-012](/adr/ADR-012-profiles-and-instances)), creates the session directory and starts the runtime. The child inherits the lock descriptor and releases it by exiting, however it exits. |
| Ready | The handshake completing is the readiness signal: the runtime is up and answering. `lifecycle.ready` is an optional event meaning the engine itself is up. The built-in runtime publishes none today, so the launcher never waits for it. |
| Run | The launcher pushes `settings.set` with every value it holds, then `flags.apply`, and events begin to flow. The built-in runtime has its settings in its launch environment already, so the launcher asks `settings.get` first and sends only the values that differ from what it reports in force. |
| Stop | `lifecycle.stop {grace_ms}` asks for a clean exit. The launcher sends SIGTERM after the grace, and SIGKILL two seconds later. |
| Exit, crash | **The launcher decides these, from the child's wait status.** A crashed runtime cannot report its own crash, so `lifecycle.exit` does not exist. A runtime that exits cleanly may send `bye {reason}` first and the launcher records it. The crash page quotes the captured stderr tail and names the runtime id and version. |
| Restart | A launcher policy, not a message: stop, then a new spawn with a new session. The signed-in startup freeze recovery is the existing example, and today the shell reads the engine log for it. A runtime may report `health {state:"stalled", what}` so the launcher need not read its log. |

## 5. Capabilities, errors and what is not offered

**Unsupported is never faked.** A capability that is not offered is shown as unsupported in the interface, with the runtime's name. No request for it is sent. A plugin that needs it is shown "limited on `<runtime>`" rather than loaded and silently dead. Replies never default to success, and a runtime emits only events its client actually produces: a declared event that nothing publishes is a lie of the same kind.

Error codes: `unsupported`, `invalid`, `failed`, `busy`, and `not_ready` for a request before the handshake finished. An unknown request gets `unsupported`. An unknown event is ignored.

| Capability | Carries | Required |
|---|---|---|
| `lifecycle` | `lifecycle.stop`, and the optional `lifecycle.ready`, `bye` and `health`. Nothing that starts a runtime: the launcher spawns it from the manifest | yes |
| `events.core` | `game.joined {place_id, universe_id?, job_id?, at}`, `game.left {at}` (`at` is Unix time in milliseconds), `session.state {signed_in, user_id?}`, `engine.version {version}`. Never the token | no |
| `events.presence` | `game.presence`, the folded BloxstrapRPC payload | no |
| `state` | `state.get`: the latest value of each event above, for a launcher that reattached | no |
| `settings` | `settings.set` and `settings.get` over Cordial's **closed key set**. The runtime declares, per key, `live`, `next-launch` or `unsupported`. The reply names the keys applied and carries `notes` for anything applied with a caveat, so "applied" is never read as "you will hear it" | no |
| `flags` | `flags.apply {sha256, count}`, after the launcher writes the resolved document to `{session_dir}/flags.json`. Declares `{families, allowlist}`. The optional `flags.live` takes `DF*` names only; the runtime applies them to the running engine, holds them in force against the engine's own refresh ([ADR-051](/adr/ADR-051-overrides-are-reapplied-after-the-engines-refresh)), and answers applied or ignored per name | no |
| `assets.overlay` | `assets.overlay.set {plugin, root}` and `.clear {plugin}` ([ADR-010](/adr/ADR-010-plugin-asset-overlays)). **The one message that carries a path** | no |
| `diagnostics` | `diagnostics.get`: at most 200 ordered, redacted lines of 256 bytes. The report always names the runtime id and `support_url` | no |

**Left out of version 1, so not specified yet:** `updates`, `doctor`, `session.vault`, `window`, `launch.join`, and a runtime-declared settings form. The first draft had each; each is added by an ADR and a minor bump when a second runtime needs it. The development control surface ([ADR-019](/adr/ADR-019-development-control-surface)) is **not** part of the protocol at all: it is a runtime's own affair, behind its own environment variable.

- `settings` keys are the same words the launch environment uses (`throttle: "off"`, `pointer_acceleration: "unlocked"`), so one setting has one spelling at spawn and afterwards. Which keys are live is per runtime; [ADR-044](/adr/ADR-044-settings-reach-a-running-game) holds today's table for the built-in one. A Settings row for a key a runtime declares `unsupported` says so.
- The resolved `flags.json` holds Roblox flag names only. Cordial's own pseudo-flags (`CordialGraphicsBackend`, `CordialFrameRateLimit`, `CordialDeviceProfile`) are stripped by the launcher and travel as typed `settings`.
- `assets.overlay.root` is an absolute directory the launcher has canonicalised, checked to sit inside a plugin's install directory, and confirmed to exist. The runtime treats it as read-only and confines its reads to it.

## 6. What stays in the launcher

The plugin host, grants and broker, hot-swap ([ADR-038](/adr/ADR-038-plugin-hot-swap)), the Discord socket, notifications, URL opening, the secret store, the profile lock, the settings and report screens, the flag layer resolver and its write grants, deep-link acceptance, and aggregation of doctor output. One plugin host runs per runtime session, and it ends with the launcher; the game does not.

**Plugins never talk to a runtime.** The launcher maps events onto the existing plugin API: `game.joined` and `session.state` to `SessionState`, `game.presence` to `cordial/game.presence`, spawn and exit to `client.launch` and `client.shutdown`, `engine.version` to `cordial/engine.version`, `flags.apply` and `flags.live` behind the flag write grants, `assets.overlay` behind asset overrides. A plugin whose capability has no backing is marked limited.

**Deep links.** The launcher accepts a `roblox-player:` link, validates it, strips the launch ticket unless the profile asked to carry it, and hands the runtime the result as `{join_url}`. Turning that into what the engine wants is the runtime's job (`cordial_runtime::deeplink::translate` today).

## 7. Hard limits

- The verb set is closed per spec version. Not offered, ever: engine memory, loading code into the client, calling engine functions, executing a command, passing a descriptor, evaluating anything, a generic "set raw" or "call".
- Exactly one message carries a path, `assets.overlay.set`, and the launcher validated it. Everything else that names a file does so by convention (`{session_dir}/flags.json`), not by value.
- Plugin-supplied strings reach a runtime only as typed payloads the launcher has validated: flag names and values, never paths.
- A capability is not added because it would be convenient. Adding one needs an ADR and a spec bump ([ADR-001](/adr/ADR-001-in-process-hooking)).
- **The spec cannot police a runtime's own process.** A runtime that injects code into Roblox is outside anything a protocol can prevent. The only lever is Cordial's listing policy ([ADR-052](/adr/ADR-052-the-runtime-spec)): a runtime that injects is not listed.

## 8. The built-in Android runtime

`cordial-run` is a separate process and stays one. It serves this spec on `<profile>/runtime/<session>/ctl.sock`, behind the same socket a third-party runtime would use, so the launcher has one code path for it and for any other, and the built-in runtime is the conformance suite for everything above. The version-0 surface, `<profile>/live/settings.sock` with its `set` and `get` verbs ([ADR-044](/adr/ADR-044-settings-reach-a-running-game)), is gone: nothing in the repository used it after the launcher moved over.

**What it offers:** `lifecycle` (`lifecycle.stop`, mapped to the quit path the window's close button uses; the launcher's `SIGTERM` after the grace lands on the same flag), `events.core` (`game.joined`, `game.left`, `session.state`, `engine.version`), `events.presence`, `state` and `settings`. **What it does not:** `flags`, `assets.overlay` and `diagnostics`, which no code behind it implements, so the handshake leaves them out and a request for one is `unsupported`; `lifecycle.ready` and `health`, which nothing publishes.

- `session.state` is sent when a join names a user, as `{signed_in:true, user_id}`. The runtime learns that somebody is signed in from the join line in the engine's log and has no source for being signed out, so it never claims it.
- `settings.get` declares the ten keys the wire carries as `live`, and `graphics`, `graphics_optimization_mode`, `present_mode`, `mangohud`, `vkbasalt` and `unpacked_plugins` as `next-launch`. Those six are launch-environment settings the process reads once. They are not in the wire's closed key set, so `settings.set` cannot carry them and reports one in `ignored`; the declaration exists so the launcher can say "next launch" from the runtime's word.
- The launcher adopts a runtime it did not start: on startup it looks for `runtime/*/ctl.sock` under every profile, says `hello {reattach:true}` and takes control. That is what reattaching means for a launcher process that was restarted. A launcher whose window merely closed is the same process holding the same connection, and nothing is found or needed.
- The launch environment, the engine log the launcher reads for freeze recovery, and `devctl` are not part of the protocol and are unchanged.

The portable core moves to the launcher's side of the line: `plugin_host.rs`, the flag layer resolver in `flags.rs`, `roblox_api.rs`. `client_settings.rs`, `flag_reapply.rs`, `bloxstrap_rpc.rs` and the log tail stay with the runtime, because they read the engine's own files. The order of the move is in [ADR-055](/adr/ADR-055-the-launcher-and-the-runtime-are-two-programs); the plugin host is the next step and is not done.

## 9. Where the draft was ambiguous

The crate had to pick a reading, and picked the simplest. If one is wrong, the spec changes and the crate follows.

- **`hello` is an ordinary request** (`m: "hello"`, with an `id`), and the handshake answer is its reply payload. `reattach` is a field of the request. Any other request before it completes gets `not_ready`.
- **`bye` is an event**, `{"ev":"bye","n":..,"p":{"reason":".."}}`, and the one event either side may send. `n` is the sender's.
- **The event counter `n`** starts at 1 and increases by one for each event sent. It is assigned when an event leaves the queue, so a dropped event consumes no number and `events.dropped` is how a loss is reported. A runtime keeps one counter for its life, so a reattached launcher sees the numbers carry on.
- **A frame is a request if it has `m`, an event if it has `ev`, a reply if it has `ok`.** One of the three, never two or none.
- **The 64 KiB limit counts the line without its `\n`.** A line that ends at end-of-file with no newline was cut off and is discarded.
- **What closes the connection and what does not.** A line that is not a frame at all closes it: not UTF-8, not JSON, not an object, over the limit, none or more than one of `m`/`ev`/`ok`, a field of the wrong type (an `id` that is not a non-negative integer, a code outside the closed set). A well-formed frame carrying an out-of-range value is dropped and counted and the connection stays: a string over the bound, a typed payload that breaks its rules.
- **The 512-byte string bound applies to every string and object key** in a message. The one exception is `root` in `assets.overlay.set`, which is a path and may be 4,096 bytes.
- **`settings.set`** takes the key-to-value object itself as `p`, with no wrapper, and replies `{applied, ignored?, notes?}`. A known key with an unusable value fails the whole message with `invalid`; an unknown key is named in `ignored` and the rest apply. **`settings.get`** replies `{values, declared}`, where `declared` maps each key to `live`, `next-launch` or `unsupported`: that is where the per-key declaration travels. The declaration for `flags` (`{families, allowlist}`) has no stated carrier, so the crate leaves it untyped.
- **`flags.live`** takes `{flags: {name: value}}` with `DF*` names and scalar values, and replies `{applied, ignored}`. **`flags.apply`**'s `sha256` is 64 lowercase hex digits.
- **`state.get`** replies with one optional field per event: `game_joined`, `game_left`, `session_state`, `engine_version`, `game_presence`, each absent until it has happened.
- **`game.presence`** carries only the fields the game set: `details`, `state`, `start`, `end`, `large_image_key`, `large_text`, `small_image_key`, `small_text`. An empty string is the game clearing one.
- **`diagnostics.get`** replies `{lines: [..]}`. **`lifecycle.stop`** takes `{grace_ms}`. Replies that carry nothing may omit `p` or send `{}`.
- **The handshake reply requires `client`**, and `runtime.id` must be non-empty. A runtime must offer `lifecycle`, in the manifest and in the handshake.
- **Manifest placeholders** expand in one left-to-right pass, so a substituted value is never expanded again; a `{name}` that is not one of the five is refused when the manifest is read; a known placeholder with no value (`{join_url}` with nothing to join) is an error, not an empty string, and the launcher decides what that means. `launch.exec[0]` is relative with no `..`, and `id` is a single path component of letters, digits, `.`, `_` and `-`.
- **A launcher does not fight for control.** A `bye {reason:"superseded"}` means a newer controller took over; the launcher that received it stops sending and does not reconnect, because two controllers each replacing the other is not control.
- **The runtime is asked what it runs, not assumed.** A connecting launcher reads `settings.get`'s `values` as what is in force and diffs its wanted settings against that, so a value the runtime already holds is not sent again and a runtime that was started with different ones is corrected.
- **Events sent while no controller is attached are not queued.** The runtime keeps the latest value of each in the snapshot `state.get` returns and lets the events go, and the counter `n` carries on across the gap. A launcher cannot know the first number it will see, so it asks `state.get` on every attach and does not infer what it missed from `n`; a gap *within* one connection (a number skipped) is how it learns the stream lost something.
