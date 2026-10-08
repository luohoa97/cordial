---
title: "ADR-044: Settings that can change reach a running game"
---
**Status:** accepted; the transport in decisions 2, 3 and 5 is superseded by [ADR-055](/adr/ADR-055-the-launcher-and-the-runtime-are-two-programs) (2026-10-08). The classification below, the reasons in it, and everything after it stand.
**Date:** 2026-09-30
**Related:** [ADR-055](/adr/ADR-055-the-launcher-and-the-runtime-are-two-programs), [ADR-003](/adr/ADR-003-plugin-isolation), [ADR-007](/adr/ADR-007-host-resources-are-brokered), [ADR-012](/adr/ADR-012-profiles-and-instances), [ADR-019](/adr/ADR-019-development-control-surface), [ADR-038](/adr/ADR-038-plugin-hot-swap)

## Decision

1. **Each setting is either live or next-launch, and the row says which.** Live
   settings are sent to every client the shell started, as soon as `shell.json`
   changes. Next-launch rows carry "Applies at next launch". The classification
   is `live::CLASSIFICATION`, a table with a reason per key; a test fails if
   `ShellConfig` gains a key that is not in it.
2. **A new socket, not devctl.** The client listens on
   `<profile>/live/settings.sock`, in a `0700` directory, always on. devctl
   (ADR-019) is opt-in and can inject input and capture frames; switching it on
   for everyone to move a slider would be the wrong trade. The live socket has
   two verbs, `set` and `get`, and `set` accepts only the keys in
   `cordial_shell::live_wire::KEYS`. Unknown keys are reported back and ignored;
   a bad value refuses the whole message. Nothing runs, reads a file, or reaches
   the engine. Plugins cannot open it: the sandbox does not bind the profile
   (ADR-003, ADR-007). No peer-credential check sits on top; the only account that
   can open the path is the user's own, which can equally edit `shell.json`.
3. **One trigger.** The shell watches the config directory (GIO monitor, so
   rename-saves are seen), debounces 150 ms, parses strictly, and pushes the
   keys that moved. The shell's own saves, hand edits and a second shell
   instance all arrive this way, so no settings row has to call anything. A
   file that fails to parse changes nothing; it is not read as the defaults.
4. **Client side, each live setting is an atomic** in the module that uses it
   (two are not: the audio sink is a string the backend owns, and gamemode is a
   registration with a daemon), initialised from the launch environment. The launch environment is still
   set, so a client started by hand or by an older shell behaves as before.
5. **A client that has not answered yet is retried** once a second for about
   half a minute, then left until the wanted values change. A client registers
   with the values its environment carried, so a change made while it loads is
   delivered when its socket appears.

## What replaced the socket (2026-10-08)

Decision 2's socket, `<profile>/live/settings.sock` with its connect-per-request
`set` and `get`, is gone. The same two operations are `settings.set` and
`settings.get` on the persistent `cordial.runtime/1` connection the client
serves at `<profile>/runtime/<session>/ctl.sock`, in a `0700` directory the
launcher makes (ADR-055, `docs/runtime-spec.md`). What did not change:

- **The closed key set** is `cordial_protocol::settings::KEYS`, ten keys, and
  nothing runs, reads a file, or reaches the engine. An unknown key is named in
  the reply's `ignored` and the rest apply; a known key with a bad value refuses
  the whole message. The code that applies a key is `live_settings::apply`,
  moved nowhere, so every test of it carries over.
- **One trigger** (decision 3). The launcher watches `shell.json`, debounces,
  parses strictly and sends what moved.
- **Delivery is retried** (decision 5): the connection is opened once a second
  for about half a minute after the spawn, and a settings change made while the
  client loads is sent when the connection is up.

What changed: the launcher **asks first**. On connecting it reads `settings.get`
and diffs against what the client says it is running, not against what the
launch environment was assumed to have given it. And the client **declares** how
each key reaches it. The table below is now that declaration for the built-in
runtime, `cordial_runtime::control::declared_settings`: the ten wire keys `live`,
and `graphics`, `graphics_optimization_mode`, `present_mode`, `mangohud`,
`vkbasalt` and `unpacked_plugins` `next-launch`. The launcher reports a changed
next-launch key as applying at the next launch and does not send it; the
launcher's own copy, `live::CLASSIFICATION`, is the fallback for a runtime that
did not answer and the check that no `shell.json` key was left undecided.

## Classification

| Key | Applies | Reason |
|---|---|---|
| `pointer_acceleration` | live | read on every locked-pointer motion event |
| `throttle` | live | the pump reads it each tick (it used to be read once) |
| `close_on_leave` | live | consulted when the log reports leaving a game |
| `carry_launch_ticket` | live | consulted each time a link is translated |
| `gamepad` | live | the pump polls it each tick; switching off sends a disconnect for every announced pad |
| `gamemode` | live | registration with gamemoded is per pid; the client registers or withdraws on the spot |
| `graphics`, `graphics_optimization_mode` | next launch | settled before engine initialisation |
| `present_mode` | next launch | a swapchain field; only the engine rebuilds swapchains, see below |
| `mangohud`, `vkbasalt` | next launch | Vulkan layers load at instance creation |
| `audio_output` | live | the playing streams are re-linked to the new sink in place; see below |
| `audio_input` | live | an open capture stream is re-linked to the new source in place; with none open nothing is opened and the choice is used the next time Roblox records; see below |
| `title_bar` | live | revealed, hidden or restyled on the game window in place |
| `roblox`, `profile`, `fullscreen_accel` | next launch | built or chosen at launch |
| `unpacked_plugins` | next launch | the reconciler never sees unpacked plugins, by design (ADR-038) |
| `appearance`, `automatic_updates`, `download_on`, `marketplace_*`, `multi_instance_warning_seen` | shell | read by the shell itself |

## Audio output

`audio_output` was next-launch because the PipeWire backend cached the sink name
in a function-local static. The cache is now a mutex-guarded string seeded from
`CORDIAL_AUDIO_SINK`, and a change re-links the streams that are already
playing. **The engine's streams are not recreated**: its OpenSL ES players and
AAudio streams keep their `pw_stream`, buffers and callbacks, so nothing FMOD
holds goes stale and the mixer sees no gap.

How the move is made was the part that needed measuring. The obvious route,
`pw_stream_update_properties` with a new `target.object`, returns success on
WirePlumber 0.5.14 (PipeWire 1.6.8) and leaves the link where it was; a stream
left on one sink was still on it four seconds and two updates later. What does
move it is what `pw-metadata <node> target.object <sink>` and `pactl
move-sink-input` do: write `target.object` for the stream's node into the
`default` metadata. A sink name goes in as `Spa:String`. Going back to the
system default is `-1` as `Spa:Id`; *deleting* the key is not the same thing, it
restores the sink the stream was opened on.

Measured with `tools/audio-switch-e2e.py`, which plays through the same
`CallbackStream` and `PlaybackStream` the engine uses, changes sink the way the
socket does, and reads the links out of `pw-dump` after each step: A to B to A to
the system default, for both the AAudio path (the default) and the OpenSL ES
path, all four links where they should be; the control, asking for the sink the
stream is already on, stays put. The AAudio stream delivered 47.5 to 48.0 thousand
frames a second across the whole run against a negotiated 48 000, which a
torn-down-and-rebuilt stream would not.

What this does not cover, and the reply says so: a host backend other than
PipeWire (`CORDIAL_AUDIO_HOST=pulse|alsa|oss`) has no sink to re-link a stream
between, so the choice applies to streams opened afterwards and the client
reports that in `notes`. Other session managers than WirePlumber, or a session
with no `default` metadata, are **INFERRED**: the client checks for the metadata
object and reports its absence instead of claiming the move. The client itself
was not run for this change; the native backend and the socket handler were
exercised separately.

## Microphone

`audio_input` is the output row's twin: **Settings → Audio → Microphone**, stored
as a PipeWire source's `node.name`, launched as `CORDIAL_AUDIO_SOURCE`, and sent
live as the `audio_input` key. Unset or empty follows the session's default
source, which is what happened before the row existed. A chosen source that is
not in the session when a recording opens falls back to the default and says so
on the client's stderr; the choice is kept for when it returns.

**Choosing a microphone opens nothing.** This is the rule at the top of
`native/audio_classes.cpp` and the row is built around it. The list comes from
`cordial_audio_sources`, which walks PipeWire's registry and copies two strings
per node; a capture stream exists only between Roblox starting a recording and
stopping it. A live change moves an open capture stream the way the output side
moves a playback one (the `target.object` write into the `default` metadata) and
otherwise stores the choice, and the reply's `notes` says there was nothing to
move. `audio_devices.rs` has a test that reads the backend's open-capture count
before and after listing and wants zero both times.

Measured 2026-10-05 against the session's real WirePlumber with two null
`Audio/Source` nodes made for the purpose, reading `pw-link -l` and the links in
`pw-dump` from `audio_probe`'s `AUDIO_PROBE_SNAPSHOT` hook at the moment the
stream was open (the OpenSL ES `record` and the AAudio `aaudio-record` commands
reach `CaptureStream::open`; nothing signed out reaches it through the engine).
With `CORDIAL_AUDIO_SOURCE` set the capture node was linked to that null source
on both paths, and with it unset or empty to the session's default source, the
control. A live change from one null source to the other, and from a null source
to the system default, moved the one open stream each time. A name the session
does not have fell back to the default with a warning, once per open. The
`default.audio.source` metadata was the same before and after. The call in
`audio_classes.cpp` (`AudioRecord` and `WebRtcAudioRecord`) shares the helper but
has no harness here, so its use of the choice is **INFERRED** from the build.

Sink monitors are not offered. PipeWire reports a monitor as a port on its
sink, not as an `Audio/Source` node, so "record what the speakers play" is not
reachable from this row.

Four places open the microphone (`AudioRecord` and `WebRtcAudioRecord` in
`audio_classes.cpp`, the OpenSL ES recorder, the AAudio input stream) and all
four read the one choice, through `configured_input_device` and
`resolve_input_target`. The comment at the top of `audio_classes.cpp` still
says three; the AAudio input path arrived after it was written.

Roblox's own in-game device list is still empty of anything useful: it is
FMOD's, it shows one "Android audio output" driver, and filling it would mean
patching the engine, which ADR-001 and ADR-003 rule out. This row is Cordial's
routing and the only microphone choice there is.

## GameMode

`cordial_runtime::gamemode` (moved out of `load.rs` so the socket can reach it)
keeps a wish and a fact: whether the setting is on, and whether gamemoded has
said yes to `RegisterGame`. A live change stores the wish and, once startup
registration has run, asks the daemon for whatever differs. A change that arrives
before that only stores the wish, which startup then honours instead of the
environment. The call runs on its own thread and is waited for 1.5 s, because
`call_method` blocks for as long as the daemon takes and the socket serves one
peer at a time; a slow daemon gets a note saying the request is still in flight.
A daemon that declines, or is not there (the ordinary case), leaves the client
unregistered and the reply says why.

Checked against the session's real `gamemoded`, asking it `QueryStatus` for the
test process afterwards: 0 before, 2 (registered) after a live enable, 0 after a
live disable. The rest is unit tests against a fake daemon: one `RegisterGame`
and one `UnregisterGame` for an on and an off, nothing sent for a repeat, a
declined or absent daemon leaving the client unregistered.

## Controllers

`gamepad::enabled` was a `OnceLock` read from the environment. It is an atomic
now, and turning it off makes the pump's next `poll` (the thread the engine's
natives are called from) send `deliver_gamepad_disconnect` for every pad that had
been announced and close the device files. That is the call an unplugged
pad already gets, so the engine is being asked to handle nothing it does not
handle when somebody pulls a cable. A pad that was opened but never announced
gets no disconnect: the engine was never told it existed. Turning it on needs
nothing: the two-second rescan finds the pads and announces them as at launch.

Unit tests cover the withdrawal with an injected disconnect (announced pads told
once, unannounced ones not, files released, a second tick a no-op) and the switch
changing in both directions after it was seeded. **No pad was attached for
this, and the client was not run, so the engine's reaction to the disconnect is
INFERRED** from its being the unplug path, not observed.

## Title bar

The header bar is a property of the game window, which lives in the client, so
the live socket cannot touch it (GTK objects belong to the pump's thread). It
leaves the choice in an atomic and a pending flag; `WaylandWindow::pump` applies
it before it next iterates GTK. `HostWindow::set_title_bar` reveals or hides the
`ToolbarView`'s top bars, honouring fullscreen as the fullscreen handler does,
and loads or clears the compact stylesheet, which is now a provider of its own
rather than text appended to the main sheet.

**The window keeps its size, so the canvas takes the difference.** That reaches
the engine as an ordinary resize and inherits whatever the resize path does,
including the swapchain rebuild that Sober #2180 reports crashing on NVIDIA
drivers (535 and 550) and that issues #35 and #39 report crashing on other
hardware, whose cause is not known; toggling the row is one resize. That path
was not run for this change (INFERRED). The X11 backend has no Cordial header bar at all,
so the setting did nothing there at launch and does nothing there now.

Verified against real GTK in a nested headless sway on its own display
(`crates/cordial-shell/tests/title_bar_window.rs`, run with `--ignored`): started
on the default bar at 46 px, Hidden gave 0 and no reveal, Compact 40, and
Default again 46, so the compact sheet was removed and not just outvoted. The
existing Hidden-at-launch fixture still passes.

## What stays next launch, re-checked against the code

`mangohud` and `vkbasalt` are environment variables on the client (`MANGOHUD=1`,
`ENABLE_VKBASALT=1`, set in `launch.rs`) that the Vulkan loader reads when the
engine creates its instance. `graphics` decides whether Cordial offers a Vulkan
loader at all, before the engine's first `dlopen`; `graphics_optimization_mode`
is the device profile and core count `native/init_params.cpp` hands the engine
during initialisation; `roblox`, `profile` and `fullscreen_accel` choose what is
started. None of those can be re-read by something already running.

**`present_mode` was expected to be live by making Cordial rebuild the swapchain
the way it does on a resize, and Cordial does not do that.** Cordial only
substitutes the mode in `vkCreateSwapchainKHR` (`vk_create_swapchain_inner`) and
rate-limits the extent it reports (`settle_resize_extent`). The *engine* rebuilds
its swapchain, and it does so when the `currentExtent` it reads from
`vkGetPhysicalDeviceSurfaceCapabilitiesKHR` changes (how a fullscreen toggle has
been seen to cause one; that it is the only trigger is INFERRED);
`vkAcquireNextImageKHR` is
not interposed and the present path forwards `VK_SUBOPTIMAL_KHR` and
`VK_ERROR_OUT_OF_DATE_KHR` untouched. So there is no lever for "rebuild at the
same size". The two ways to make one are both worse than saying "next launch":
report a wrong extent for a poll (a swapchain at the wrong size, then another),
or return `VK_ERROR_OUT_OF_DATE_KHR` from a present and hope the engine treats
it as it should. Neither was tried, because the client was not run for this
change, and swapchain rebuilds are the path Sober #2180 (NVIDIA drivers only)
and issues #35 and #39 (a Steam Deck, and an unknown GPU) report crashing.
Making the *next* rebuild use the new mode (an atomic in place of the
`OnceLock`) would apply at the next resize or experience entry, which is what
the code comment on `present_mode_choice` already argued is worse than a plain
"next launch". A route that changes the mode without a
rebuild exists on paper, `VK_EXT_swapchain_maintenance1`'s per-present mode,
but the engine does not enable it and it was not investigated (INFERRED).

`unpacked_plugins` was not done. ADR-038 excludes unpacked plugins from the
reconciler on purpose, so that Deno's own `--watch` and the reconciler are never
two supervisors of one process; taking a changed folder list live means a
second start and stop path beside `start_all`, in the code that decides what a
plugin may do, with nothing here to run it against. That is a change to
ADR-038, not a setting.

## Consequences

`carry_launch_ticket` is live and moves a credential, so flipping it applies to
the next link the running client translates. That is what the row says.

The Settings window does not refresh its rows when another process edits
`shell.json`, and its next save writes its own stale copy back. That predates
this decision and is not fixed by it.

`unpacked_plugins` edits inside a listed folder already reload (ADR-038); adding
or removing a folder is next launch.

## Notes moved from docs/status.md (2026-10-02)

The frame-rate row on the status page was cut down to "unsettled"; the reasoning behind that, and what would settle it, is kept here because `present_mode` is the setting it concerns.

Frame rate was measured with pointer motion driven for the whole run, because presents drop to exactly 1/s when nothing is happening and every earlier figure in this repository was that idle throttle integrated. That run, on 2026-08-02, read a flat 60.0 on MAILBOX against a variable 35-50 on FIFO across four runs of 120 s.

**Do not quote those two numbers as settled, because they contradict the other record of the same thing.** `crates/cordial-runtime/src/android/vulkan.rs` says FIFO tracks the output exactly, 60.0 on the 59.88 Hz panel and 49.4 on the 49.96 Hz one, and that Sober clears both on the same machine and APK. A 35-50 FIFO sits below both refresh rates and was noted as unexplained at the time; the same 35-47 band later turned up as the *uncapped* arm in `docs/analysis/flag-init.md` §49, which is what a scheduler-paced rate would look like rather than a vsync-locked one. A flat 60 is also the one thing MAILBOX is supposed not to produce.

What would settle it, and neither costs much: take the same input-driven count under `CORDIAL_PRESENT_MODE=fifo` and then `=mailbox`, on each output in turn. If MAILBOX follows the panel, near 60 on the 59.88 Hz one and near 50 on the 49.96 Hz one, the ceiling is the display and the present mode is not escaping it. If it stays near 60 on the 49.96 Hz output, the ceiling is the engine's own pacing and the display is irrelevant. `refresh.rs` notes Cordial has never told the engine the real refresh rate, so that is the third arm worth running.

**Measured 2026-10-03, and the ceiling is the engine's own pacing.** On a 240.001 Hz output (RTX 4070, NVIDIA 615.71.09), with the engine told the real rates (`refresh: nativePassCurrentDisplayRefreshRate 240.001` in the run's log) and input driven at 224-233 moves/s: `CORDIAL_PRESENT_MODE=auto` held 59.9 presents/s and `=fifo` 59.9, p50 frame 16.7 ms in both. Raising `DFIntTaskSchedulerTargetFps` to 240 moved MAILBOX to 216-232/s and FIFO to 193/s (p50 4.2 ms); `=30` held 29.8/s with the same input flowing. So the flat 60 the 2026-08-02 run read is the engine's frame target and the present mode never escapes it — and the third arm above, telling the engine the refresh rate, was in force during this measurement and changed nothing. What this does not cover: an output below 60 Hz (whether the engine follows one down, which the old 49.4 FIFO figure claims), the 35-50 FIFO band of that same 2026-08-02 record, and anything but NVIDIA.
