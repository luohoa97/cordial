# ADR-047: The canvas is lowered only once GTK has presented a frame

**Status:** accepted
**Date:** 2026-09-30
**Related:** [ADR-011](ADR-011-wayland-and-libadwaita.md), issue #53

## Context

A focused Roblox TextBox, or a web-view dialog, is drawn by GTK over the game by
lowering the engine's canvas subsurface beneath the toplevel and making the
toplevel transparent over the canvas rectangle. That is only right if the
compositor already holds a GTK buffer that is transparent there. Until it does,
lowering shows GTK's previous buffer, which was painted with the canvas above
and is opaque across the window: a grey screen for as long as GTK does not
present.

`repaint_now` waited 40 ms for the frame clock's `after-paint`, logged that none
had come, and lowered anyway.

## What was measured (nested KWin 6.7, own machine, signed out, `fakefocus`)

`VK_ICD_FILENAMES=/nonexistent` gives the engine no Vulkan, so it falls back to
GLES3 in the same process as GTK, and GTK falls back to its GL renderer. That
reproduces the reporter's log line for line: `gdk_gl_context_make_current()
failed` every 15 s, and on the second and third focus `repaint_now: no GTK frame
landed within 40ms`. With the old ordering, three focuses gave three
`place_below` and no `attach` on the window surface at any point after startup:
GDK sent the frame's regions, `frame` and presentation feedback, and Mesa never
attached a buffer. Even the focus where `repaint_now` reported "painted after
6 ms" had no attach behind it, so `after-paint` is not evidence of a commit.

The same binary with `GSK_RENDERER=cairo`, or with Vulkan available (GTK then
renders through Vulkan; forcing the engine to GLES with Vulkan present does not
break it), presents normally: the window surface attaches, `repaint_now` reports
2-5 ms, and every restack is safe.

## Decision

1. `cordial_shell::stacking_gate::Gate` decides. The canvas is lowered only
   after GTK has been asked for a frame with the transparent background
   (`HostWindow::arm_present_probe`) and GDK's frame timings show a frame laid
   out after that request with a presentation time
   (`present_probe_committed`). Raising is never gated.
2. No frame within 750 ms: the transparency is reverted, the canvas stays above
   the window (the game stays visible, the editor does not), a line names the
   renderer and the frame-clock state, and the attempt is repeated a second
   later for as long as the box has focus.
3. Waiting is spread over pump ticks instead of blocking one, and Cordial sends
   no parent commit of its own while waiting, because a commit of ours would
   resolve GDK's pending presentation feedback and pass the check without GTK
   having attached anything.
4. Without `wp_presentation` GDK's timings cannot report a presentation, so the
   check falls back to `after-paint`, the weaker old answer.
5. `CORDIAL_STACKING_GATE=off` restores the old ordering. It is the control for
   a before-and-after on one binary.

## Alternatives

- **Keep the window permanently transparent over the canvas.** Removes the
  dependency on a new frame at lowering time, but the last buffer GTK committed
  may itself never have been the transparent one (GTK in the failing
  configuration commits nothing at all), and 5a295e3 records the invisible
  window an always-transparent toplevel gives when the engine stops painting.
- **Draw the editor in a popover, above the canvas without lowering it.**
  Would end the dependency for the editor but not for dialogs, and changes
  focus and input-method behaviour. Not attempted.

## What the gate did not fix, and what chooses cairo (2026-10-01)

In the failing configuration GTK presents nothing, so the gate alone leaves the
game visible and the editor invisible. The cause is GTK's GL renderer sharing a
process with the engine's GLES; `GSK_RENDERER=cairo` cures it.

`cordial_shell::gtk_renderer` now chooses cairo for the machines that would
otherwise get nothing. `cordial-run` starts the Vulkan probe of
[`vulkan_probe`](../../crates/cordial-shell/src/vulkan_probe.rs) in a background
thread right after the Graphics line is printed, and collects it in
`wayland::open` before `init_wayland`, which is before GTK realises the window
and reads the variable. The rule:

| Situation | `GSK_RENDERER` |
|---|---|
| set by the user, non-empty | untouched, and the probe is never started |
| a device on NVIDIA's own driver (added 2026-11-17, see the last section) | `cairo` |
| otherwise, a non-CPU Vulkan device exists | untouched (GTK renders through Vulkan) |
| no loader, `vkCreateInstance` fails, no devices, or only a CPU renderer | `cairo` |
| the probe timed out or crashed | untouched, and a line says so |

One line is logged either way, `[gtk] renderer: ...`, naming the reason and how
long the launch waited for the probe.

**The Graphics setting is not an input.** `CORDIAL_GRAPHICS=gles` was measured
on this machine with working Vulkan and GTK stayed on `GskVulkanRenderer`,
presented, and lowered 3 of 3, so forcing GLES is not by itself a reason; the
probe is. The probe runs in a child of `cordial-run`
(`cordial-run --vulkan-probe`, handled before the profile is claimed), so a
driver that faults at load costs the probe and not the client.

Measured on the #53 reproduction (nested KWin 6.7, signed out, `fakefocus` three
times; `GSK_RENDERER=ngl` set by the user is the "before", on the same binary):

| Run | Renderer GTK reported | Lowerings under a presented frame | Window-surface attaches after arming |
|---|---|---|---|
| no Vulkan, user sets `ngl` | GskGLRenderer | 0 of 3 (gate holds) | 0 |
| no Vulkan, nothing set | GskCairoRenderer, chosen by the log line | 3 of 3 | 257 |
| Vulkan, nothing set | GskVulkanRenderer, left alone | 3 of 3 | 266 |
| Vulkan, `CORDIAL_GRAPHICS=gles` | GskVulkanRenderer, left alone | 3 of 3 | 269 |

The probe takes about 60 ms on a working Vulkan stack and 29 ms with no
driver (10 runs each, a child of `cordial-run`), and `settle` waited 0 to 10 ms
for it because it runs while the engine loads.

INFERRED: that this is the reporter's cause. It reproduces their log signature
on a second KWin with the same trigger; their GPU stack was not seen. Also
INFERRED: that the editor is then *visible*. What was observed is that GTK
presents under cairo and the restack lowers under presented frames; the
editor is a GTK widget the engine's screenshot cannot see, and no compositor
capture of the nested session was taken. The claim that a presentation time
implies an attached buffer follows from the protocol and matches every trace
here, and was not tested against a compositor that withholds feedback. That
cairo is needed when the only Vulkan device is a CPU renderer is a choice made
on the brief and not measured: the GL conflict was only reproduced with the
engine on GLES.

## NVIDIA's own driver is cairo as well (2026-11-17)

The opposite failure: GTK has a Vulkan renderer and it is what crashes. Four
cores from the 0.34.1 Flatpak (RTX 3090 Ti, driver 620.70.06, COSMIC on
Wayland, 2026-11-15 and 16) stop on the main thread at address zero, called
from inside `libnvidia-eglcore`, called from `gdk_vulkan_context_begin_frame`
at its `vkAcquireNextImageKHR` call (three) or from
`gdk_vulkan_context_end_frame` (one). Three were at start, one 15 s after
leaving a game an hour into a session. The engine's own threads in the driver
were all parked in `pthread_cond_wait` or `poll`.

So a device with vendor id `0x10DE` whose `driverName` is `NVIDIA`, or which
does not name its driver, now gets `cairo` too, with its own log line and
doctor line. Mesa's NVK on the same card names itself and is left alone.

Measured on that machine, one binary, signed out, the window starting
maximised and toggled in and out of fullscreen eight times per launch:

| `GSK_RENDERER` | Launches | Died |
|---|---|---|
| `vulkan`, set by hand (the old default) | 30 | 2, both SIGSEGV at address zero under `libnvidia-glcore` and GTK's renderer |
| unset, so cairo is chosen | 40 | 0 |

Two in thirty against none in forty is weak on its own (Fisher's exact,
p = 0.18). Counting every launch of the day, signed in or out, GTK on Vulkan
died in 3 of 43 and cairo in 0 of 41 (p = 0.13). What carries the change is
the mechanism: every core from both sources was in GTK's frame, and cairo is
the one renderer that leaves GTK nothing to call in the driver.

Presents over the same toggling were the same in both arms (about 1,650), so
drawing the header bar on the CPU did not cost the game frames. Starting the
window un-maximised, GTK on Vulkan died in 0 of 8, so the window's state
matters; why is not known.

INFERRED: that the fault is the driver's rather than GTK's, that other driver
series crash the same way (only 620.70.06 was seen), and that NVK does not.
