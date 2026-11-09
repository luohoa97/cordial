# Headless harnesses are not a sway feature

`tools/` used to answer "how do I run this without a window" with a copy of the
same twenty lines: a sway config, `WLR_BACKENDS=headless`, a stamp file, and a
`distrobox enter cordial` wrapper. That made the compositor part of every
experiment. A harness could not be repeated under KWin without being rewritten,
and a machine without sway either installed something or gave up, even when its
own desktop already had a compositor that could do the job.

`tools/headless_compositor.py` replaces those twenty lines. It starts a nested
compositor on a private `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR`, and a harness
asks for the capabilities it needs:

| capability | meaning |
|---|---|
| `launch` | start a compositor and give the client a display |
| `ipc` | list windows and move, resize, focus, fullscreen or close one |
| `screenshot` | photograph the composited output |
| `virtual_input` | hold `zwp_virtual_keyboard_v1` / `zwlr_virtual_pointer_v1` devices open |
| `output_resize` | change the output mode while running |
| `multi_client` | host the client plus the input holders |

Selection is by capability, not by name. With KWin and sway both installed, a
launch-only harness gets KWin; `tools/pointer-lock-e2e.py` still gets sway,
because KWin has no virtual-pointer global and a run there would measure
nothing. Asking for an explicit backend that lacks a capability is an error
naming the capability, never a silently weaker experiment.

## What was measured

On 2026-11-09, KWin 6.8.2, on a KDE Wayland session, with a private
`XDG_RUNTIME_DIR`, a private D-Bus session and no window on the desktop:

```
$ python3 tools/headless_compositor.py --direct start --backend kwin \
      --out-dir /tmp/opencode/hl-final --state /tmp/opencode/hl-final/state.json \
      --width 800 --height 600 --require screenshot --require ipc
WAYLAND_DISPLAY=wayland-headless-470212-63723
XDG_RUNTIME_DIR=/tmp/opencode/hl-final/runtime
DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/opencode/hl-final/runtime/bus,guid=...
```

- `alacritty` connected to that display and mapped. `Session.windows()` read
  it back through a KWin script: `app_id=FinalTest`, `rect=(12, 25, 1000,
  718)`, `pid=...`.
- `set_geometry(w, 100, 80, 800, 600)` changed `frameGeometry`; the next read
  agreed with it. `set_fullscreen(w, True)` produced `(0, 0, 800, 600)` and
  `False` restored `(100, 80, 800, 600)`. KWin applies these on its own event
  loop, so the driver polls until the reading changes; a read in the same turn
  as the write is the old value.
- `screenshot` returned a valid 800x600 PNG. `org.kde.KWin.ScreenShot2` writes
  a raw RGBX8888 image down a pipe. The call deadlocks if the caller waits for
  its reply before reading the pipe, so a reader thread is part of the
  implementation. The byte order was checked against a window painted a known
  colour: KWin 6.8.2 writes R,G,B,X despite the format number, and the first
  version followed Qt's documentation and swapped red and blue.
- `--no-focus` put a focus-holder window up and the next client stayed
  inactive. KWin's default policy gave the new window focus even with the
  holder active; a private `kwinrc` with `FocusPolicy=ClickToFocus` and
  `FocusStealingPreventionLevel=4` is what made the arm mean what the sway
  `no_focus` rule means. The nested KWin also gets its own `XDG_CONFIG_HOME`,
  so it cannot rewrite the developer's `kwinrc`.

The engine itself had already been run under `kwin_wayland --virtual` before
this change (441 and 524 presents, Home loaded, 1280x799, fully off-screen), so
the missing piece was the harness, not the client.

## Usage

From a shell harness:

```sh
python3 tools/headless_compositor.py start --state /tmp/run/state.json \
    --out-dir /tmp/run --width 1280 --height 800 --no-focus
eval "$(python3 tools/headless_compositor.py env --state /tmp/run/state.json)"
python3 tools/headless_compositor.py exec --state /tmp/run/state.json -- \
    /tmp/cordial-wl-holders/wl-pointer-holder 1280 800
python3 tools/headless_compositor.py screenshot --state /tmp/run/state.json \
    --path /tmp/run/shot.png
python3 tools/headless_compositor.py stop --state /tmp/run/state.json
```

From Python:

```python
from headless_compositor import start
with start(1280, 800, out_dir="/tmp/run",
           required=("launch", "ipc", "screenshot")) as comp:
    env = comp.host_env({"GDK_BACKEND": "wayland"})
    # ... launch the client with `env` ...
    comp.screenshot("/tmp/run/shot.png")
```

Environment:

| variable | effect |
|---|---|
| `CORDIAL_HEADLESS_COMPOSITOR` | force `kwin`, `sway`, `weston` or `mutter` |
| `CORDIAL_HARNESS_BOX` | run inside this distrobox; `none` means this host |
| `--direct` | same as `CORDIAL_HARNESS_BOX=none` |
| `--backend` | per-run equivalent of `CORDIAL_HEADLESS_COMPOSITOR` |
| `--require` | an extra capability the harness needs (repeatable) |

The default execution is still the `cordial` distrobox when `distrobox` is
installed, because that is where the build and the input holders are. A host
without it runs directly. `CORDIAL_HARNESS_BOX=none` is the explicit form.

## What each harness asks for now

| harness | capabilities | backends |
|---|---|---|
| `frame-pacing-check.py` | launch | KWin, sway, weston |
| `startup-freeze-survey.sh` | launch (+ `no_focus`) | KWin, sway, weston |
| `text-entry-check.sh` | launch, screenshot | KWin, sway |
| `text-input-e2e.py` | launch, ipc, screenshot, output_resize, virtual_input | sway |
| `pointer-lock-e2e.py` | launch, virtual_input | sway |
| `verify-slash-fix.py` | launch, virtual_input | sway |
| `touch-compositor/touch-e2e.py` | launch, virtual_input | sway |
| `hyprland-drag-e2e.py` | its own KWin → Hyprland stack | KWin + Hyprland |

`text-input-e2e.py` still contains sway-shaped checks: `swaymsg -t get_tree`,
`output ... mode`, `floating enable`. That is not an oversight in the launcher;
those are the things the harness is measuring. They now go through
`Session.ipc()` and say `Unsupported` on a backend without IPC rather than
starting a compositor that cannot answer. Converting them to the backend-neutral
`Session.windows()`/`set_geometry()` calls is the next step and is what would
let the visibility cases run under KWin.

## Why cage is not a backend

Cage is a kiosk: it runs one client and exits with it. Every harness here needs
the compositor to stay up while the input holders connect *before* the client
starts, because Cordial reads seat capabilities once at `open()`. Cage's seat
also never gains a keyboard, which switches off the code the text tests exist
to exercise. A cage backend would start, look green, and measure half a client.
It is absent deliberately, not forgotten.

## Why mutter is launch-only

`mutter --headless --virtual-monitor` is still offered as a launch backend for
probes, but it is not advertised as an equivalent. It dies within a second of
the engine presenting (`examples/pointer_capture_probe.rs`, and the commit that
added `--headless`), so a harness that ran there would be measuring the crash
rather than its own question. If a later Mutter survives the client, the
capability list is where to say so, with the launch that proved it.

## What this does not establish

- Weston was written from its documented flags and has not been run here: no
  weston was installed. Its screenshot backend is `None` for the same reason.
- Mutter has not been run end to end since the crash above.
- KWin's window model covers the calls this change needed (list, geometry,
  fullscreen, active, close). It does not replace every `swaymsg` field the
  text-input harness reads; see the table.
- The input tests still cannot run under KWin because KWin advertises no
  `zwp_virtual_keyboard_v1` or `zwlr_virtual_pointer_v1`. A `ydotool`/uinput
  route was deliberately not added: those devices are visible to the
  developer's session too, which is the accident the nested compositor exists
  to prevent.
