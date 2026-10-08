---
title: "ADR-056: The host window keeps one pixel the engine does not cover"
---
**Status:** accepted
**Date:** 2026-10-08
**Related:** [ADR-011](/adr/ADR-011-wayland-and-libadwaita), [ADR-047](/adr/ADR-047-the-canvas-is-lowered-only-under-a-presented-frame), issues #39, #53, #99

## Context

The engine draws into an opaque subsurface above the GTK window. With the
header bar showing, the window is partly uncovered, because the bar sits
outside the engine's rectangle. With no header bar -- fullscreen, or the Title
bar setting on Hidden -- the engine's rectangle is the whole window, and a
compositor that works out visibility from what is on top of what sees the GTK
surface as completely hidden.

A hidden surface gets no frame callbacks and no presentation feedback.
wlroots says so with `wl_surface.leave` and then silence; the engine's buffers
are `XRGB8888`, which a compositor treats as fully opaque whatever opaque
region is declared. GDK's frame clock will not paint again until the last
frame's callback arrives, so GTK stops painting: nothing in the window is laid
out, drawn or committed again until something uncovers it.

Three reported symptoms are that one state:

- **A text box takes focus in fullscreen and no editor appears** (#53, and
  Sober #1026 for the same shape on that client). ADR-047's gate waits for GTK
  to present a frame before lowering the canvas, GTK cannot present one, so the
  gate times out every second for as long as the box has focus and the editor
  never shows. Before the gate this was the lowering going ahead on a stale
  buffer.
- **Leaving fullscreen leaves the window half restored** (#39, and "the title
  bar is missing after leaving fullscreen" in #53). The header bar comes back
  by a relayout, a relayout is a frame-clock phase, and the clock is stopped.
  The engine goes on rendering at the fullscreen size, 46 px taller than the
  window's content area.
- **Title bar Hidden gives the first symptom in a tiled or maximised window**,
  with no fullscreen involved.

## What was measured

Nested headless sway 1.11 and nested headless Mutter 50.5 (the GNOME the
reporters run), 1280x800 output, signed out, Roblox 2.738.0.1397, the same
binary for both arms. A synthetic text box focused with the `fakefocus` verb
(ADR-019); `grim` photographs the composited output; "extent" is the surface
size the engine was last told. **The control is `CORDIAL_VISIBILITY_ANCHOR=off`**,
which removes the pixel from the binary being tested.

Sway, three runs of fullscreen, tiled, fullscreen, tiled:

| Arm | Fullscreen, text box focused | Tiled after leaving fullscreen |
|---|---|---|
| pixel on | canvas lowered 3 of 3 runs, 6 of 6 fullscreen focuses; editor visible in the photograph; extent 1280x799 | extent 1280x754 (header bar back, engine resized); lowered 6 of 6 |
| pixel off (control) | never lowered, 6 of 6 focuses; "no frame within 750 ms" logged on the first of each run (the log prints attempt 1 only); no editor in the photographs inspected | extent stays 1280x800 (bar not back), 6 of 6; never lowered |

Title bar Hidden on sway, tiled and exactly the output size: pixel off, no
frame within 750 ms; pixel on, lowered. A floating window with a drop-shadow
margin was lowered in both arms, because the shadow is uncovered surface.

Mutter 50.5, fullscreen, tiled, fullscreen, tiled: pixel off, no frame within
750 ms on both fullscreens, extent 1280x800 after leaving fullscreen (bar
not back), second tiled attempt also fails; pixel on, all four lowered,
extent 1280x754 after each exit.

The protocol trace (`WAYLAND_DEBUG=1`, sway, fullscreen, pixel off) shows GTK
did attach and commit a 1280x800 buffer after being asked, so the gate's
verdict -- no frame -- was about feedback, not about GTK. The surface had left
its output before the request.

**What this did not reproduce.** The freeze in #99: with input driven for three
seconds (294 moves) the engine presented 176 to 179 frames in fullscreen with
the pixel off, against 173 to 178 windowed. Whatever freezes #99's game is not
this.

## Decision

1. While no header bar is revealed, `HostWindow` shows a one-pixel black
   strip at the foot of the window (`visibility_anchor_height`, a bottom bar on
   the toolbar view). The engine's rectangle is read from the content area, so
   it is one row shorter -- 1920x1079 in fullscreen at 1080p -- and there is no
   second source of truth. The strip is hidden whenever the bar is revealed, by
   one notify on the reveal property, which every writer already goes through.
2. A window that opens with the bar hidden asks for one extra row of height,
   so the content comes out the size the caller asked for.
3. `CORDIAL_VISIBILITY_ANCHOR=off` removes the strip. It is the control for a
   before-and-after on one binary, as `CORDIAL_STACKING_GATE=off` is for ADR-047.

The strip is black rather than the window colour because the window colour is
near white under a light theme.

## Alternatives

- **Leave the gate to time out, and lower anyway when the surface is hidden.**
  Spends GTK's one painted frame on the guess that it was the transparent one,
  and a clock already waiting on an earlier callback paints nothing at all; a
  state that depends on what happened before the focus.
- **Nudge the engine subsurface by a pixel while arming.** No lasting cost, but
  the subsurface is placed from `android/wayland.rs`, which another change owns,
  and it moves the whole picture for the length of the wait.
- **Declare a hole in the engine surface's opaque region.** A compositor
  ignores the region for a format with no alpha channel; the engine's is
  `XRGB8888`.
- **Ask for an alpha swapchain.** The engine's alpha channel is not guaranteed
  to be 1, and a hole in the picture is a worse failure than a missing row.
- **Keep the window permanently transparent over the canvas, stacked below.**
  Rejected in ADR-047; unchanged.

## Not fixed, and not known

- The game loses one row of pixels in fullscreen and with the bar hidden. At the
  common sizes that is a black line at the bottom edge.
- `INFERRED`: that Hyprland and KWin behave like sway and Mutter here. #53's
  Hyprland report did not say whether the window was fullscreen or had its
  title bar hidden. KWin's grey screen in the same issue was a different cause
  (GTK's GL renderer in a process whose engine is on GLES), which ADR-047
  already handles.
- `INFERRED`: that this is not what freezes #99. It reproduces the editor and
  the exit-from-fullscreen failures, not a stalled engine, and nothing here ran
  NVIDIA 580 on Mutter.
- The crash on leaving fullscreen in #39 (a SIGSEGV on the first swapchain
  recreation of the exit transition) is unrelated and unexplained.
- Not measured: a real game with a real focused TextBox. The box was
  synthetic, so the measurement shows GTK can present and the canvas is
  lowered, not what a particular game's box looks like.
