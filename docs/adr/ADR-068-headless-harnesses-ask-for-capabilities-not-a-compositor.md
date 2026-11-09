# ADR-068: Headless harnesses ask for capabilities, not a compositor

**Status:** accepted
**Date:** 2026-11-09
**Related:** [ADR-019](ADR-019-development-control-surface.md), [ADR-024](ADR-024-x11-is-supported-again.md)
**Analysis:** [Headless harnesses are not a sway feature](../analysis/headless-compositors.md)

## Context

Every harness under `tools/` that needed a display grew its own copy of the
same setup: a sway config, `WLR_BACKENDS=headless`, a stamp file, and a
`distrobox enter cordial` wrapper. That made one compositor part of every
experiment. A KDE machine with `kwin_wayland` but no sway could not run a
headless test, even though the client already runs under KWin's virtual
backend; the missing piece was the tooling.

The harnesses do not all need the same things. `frame-pacing-check.py` needs a
display. `text-entry-check.sh` needs a display and a composited screenshot.
`pointer-lock-e2e.py`, `verify-slash-fix.py` and the touch test need a real
seat with a held-open virtual keyboard and pointer, which is a wlroots
protocol. `text-input-e2e.py` additionally needs a window model and an output
mode change. Treating those as one requirement is what forced every run
through sway.

## Decision

`tools/headless_compositor.py` starts the nested compositor and a harness
requests capabilities:

| capability | meaning |
|---|---|
| `launch` | a private display |
| `ipc` | list windows and move/resize/focus/fullscreen/close them |
| `screenshot` | photograph the composited output |
| `virtual_input` | hold wlroots virtual keyboard/pointer devices open |
| `output_resize` | change the output mode while running |
| `multi_client` | host the client plus the input holders |

A backend advertises only what has been implemented and measured: KWin has
`launch`, `ipc` and `screenshot`; sway has all six; Weston has `launch` and
`multi_client`; Mutter has `launch`. Selection takes the first backend in
preference order that provides every capability the harness asked for. Asking
for a backend that lacks one is an error naming the missing capability, never a
run that starts and measures half the question.

- **Private environment.** The runner creates its own `XDG_RUNTIME_DIR` (0700)
  and a private D-Bus session for KWin, and removes `WAYLAND_DISPLAY` and
  `DISPLAY` before starting the compositor. A nested KWin cannot register
  `org.kde.KWin` on the developer's bus or rewrite the real `kwinrc`.
- **No-focus is a capability result, not a sway config line.** Sway uses a
  `no_focus` rule. KWin uses a small focus-holder client plus a private
  `kwinrc` with `FocusPolicy=ClickToFocus` and
  `FocusStealingPreventionLevel=4`; KWin's default policy gave the new window
  focus even with the holder active.
- **The session provides neutral readings.** Harnesses use
  `Session.windows()`, `output_rect()`, `window_origin()`, `set_geometry()`,
  `set_fullscreen()`, `set_floating()`, `seat_capabilities()` and
  `screenshot()` rather than `swaymsg` and `grim`. The X11/Xwayland mode is the
  one exception: it deliberately starts sway's own Xwayland because the
  question there is what an X11 client does inside a Wayland compositor.
- **cage is not a backend.** It is a single-client kiosk with no keyboard on
  its seat. It would start, look green and switch off the input paths under
  test.

## What was measured

KWin 6.8.2 on KDE Wayland, 2026-11-09, repeated starts across the audit:

- `kwin_wayland --virtual --socket ... --width 800 --height 600` created its
  socket under the private runtime directory and an `alacritty` client mapped.
  `windows()` reported `app_id=MeasureClient`, `rect=(0, 0, 1000, 718)`,
  `client_rect=(0, 28, 1000, 690)`.
- `set_geometry` moved it to `(100, 80, 500, 400)`; `set_fullscreen(True)`
  produced `(0, 0, 800, 600)` and `False` restored the geometry.
- `screenshot()` produced a valid 800x600 image through
  `org.kde.KWin.ScreenShot2.CaptureWorkspace`. The call deadlocks unless the
  pipe is drained concurrently, and KWin writes R,G,B,X while reporting QImage
  format 16; both are now pinned by code and a unit test.
- `--no-focus` left `CordialFocusHolder` active and the next client inactive.
- `python3 -m unittest discover -s .github/scripts -p 'test_*.py'`: 43 passed.

## What this does not claim

- Weston and Mutter were not run here. They are offered for probes, not as
  equivalents. Mutter still dies when the engine presents, as
  `examples/pointer_capture_probe.rs` records.
- The whole of each harness was not re-run under KWin. The runner and the KWin
  primitives were; the capability table says what is measured and what is only
  selectable.
- The Wayland input tests remain sway-only: KWin advertises no
  `zwp_virtual_keyboard_v1` or `zwlr_virtual_pointer_v1`. A `ydotool`/uinput
  route was not added because those devices are visible to the developer's
  session too.

## Alternatives

**Keep a sway config in each harness.** Rejected: it is the status quo this
change removes, and it makes a compositor a property of every experiment.

**Use cage as the generic fallback.** Rejected: no keyboard on its seat and one
client only.

**Have KWin emulate sway's IPC.** Rejected: a second implementation of a
compositor's window semantics that would drift from the real one. The neutral
`Session` methods are the interface instead.

**Inject input into KWin with uinput.** Rejected on safety: the device is
visible to the developer's compositor as well, which is the accident the nested
compositor exists to prevent.
