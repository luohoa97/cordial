# ADR-019: A development control surface, in coordinates and pixels

**Status:** accepted
**Related:** [ADR-001](ADR-001-in-process-hooking.md), [ADR-003](ADR-003-plugin-isolation.md), [ADR-007](ADR-007-host-resources-are-brokered.md), [ADR-011](ADR-011-wayland-and-libadwaita.md), [ADR-012](ADR-012-profiles-and-instances.md)

## Decision

Cordial gains an **opt-in development control surface**: a Unix socket, created
only when `CORDIAL_DEV_CONTROL` is set, which can

- capture the frame the engine just presented, read out of the Vulkan
  swapchain, and
- deliver pointer motion, clicks, keys, text and scroll through Cordial's own
  `input::pass_*` entry points, and
- fill a focused Roblox TextBox in one edit rather than one synthetic
  keystroke per character (`paste`/`settext`), for a harness payload too large
  to type.

`tools/cordial-mcp.py` speaks MCP over stdio against that socket and adds
debugger tools on top of it.

**`paste`/`settext` are the one pair of verbs that do not go through
`input::pass_*`.** Since `fd0f0c6` a real `gtk::Text` owns a focused box's text
on Wayland (see the text-entry sections of `docs/NEXT.md`), so the two verbs
write that widget directly through ordinary `GtkEditable` calls
(`insert_text`/`set_text`) and let its own `changed` signal do what a keystroke
already does -- mirror the write, push it to the engine, redraw. Writing
Cordial's own mirror buffer and the engine instead, bypassing the widget, was
tried first and is not what shipped: the widget would not agree with the
engine until the next pump tick's overlay refresh caught up, and a real
keystroke landing in that window would revert the edit. X11 has no such
widget (ADR-024) and keeps the buffer-and-engine path this whole surface used
before the widget existed.

It works in **coordinates and pixels only**. There is no semantic access to
Roblox's interface: no element tree, no "click the button named Play".

## Why

Debugging this client has been bottlenecked on instruments rather than on
reasoning, and one session made the cost impossible to ignore. On 2026-08-21 a
rendering bug consumed most of a day, and every wrong turn in it was a broken
measurement rather than bad thinking:

- `mainWorkCallback` was read as a per-frame heartbeat. It fires exactly twice
  in healthy runs too, so "it stopped after two" was never a symptom.
- `onFlagsLoaded`'s byte count looked like a delivery readout for the settings
  document. It is a constant: 1,308,253 for an `FInt` six characters longer, for
  an `FString` 1,001 longer, and for a document with 903 keys and 87 KB removed.
- Present counts collapse to exactly 1.0/s from the engine's idle throttle
  whenever input is not being driven for the whole measurement, which was very
  nearly recorded as a bisect signal.

Underneath all three sat a simpler problem: **nothing on the host could
photograph a Wayland window.** Five routes were tried and every one was refused
— `xprop`/`import` see nothing for a native Wayland client, GNOME's
`org.gnome.Shell.Screenshot` answers `AccessDenied`, `ffmpeg -f kmsgrab` wants
membership of the `video` group, the portal wants a human to click a dialog, and
no nested compositor is installable on an immutable host. So every visual check
ended with a person being asked to look at the screen and describe it, and a
bisect that should have taken three runs did not happen at all.

## Why the swapchain, rather than the compositor

Reading the presented image gets a strictly better answer than any screenshot
API could. It is what the engine drew, so it is unaffected by occlusion, by
another window covering Cordial, by the window being off-screen or on another
workspace, or by the compositor's own colour management. It also needs no
permission, no portal and no group membership.

The copy runs inside `vkQueuePresentKHR` before the frame is forwarded, because
that is the one moment the image is both complete and in a layout the copy can
name: the engine has finished rendering and has just left it in
`VK_IMAGE_LAYOUT_PRESENT_SRC_KHR`.

## Why no semantic access, and why that is not squeamishness

An element tree was the obvious thing to want, and it was investigated properly
before being ruled out. **Roblox exposes no accessibility tree on Android at
all**, measured four ways on 2026-08-21: `libroblox.so` has 517 `Java_*` exports
across twenty engine interfaces and none mention accessibility; no `com/roblox/`
class in the dex is named `*Accessib*` or implements a provider; the dex has no
`com/roblox/**` `View` or `Surface` subclass for a virtual-descendant provider
to hang off; and a run with the AT-SPI bridge genuinely attached, so the
`isEnabled` gate answered true honestly, produced zero calls into
`native/accessibility.cpp`.

So there is nothing to read, and obtaining it would mean introspecting the
engine — which [ADR-001](ADR-001-in-process-hooking.md) and
[ADR-003](ADR-003-plugin-isolation.md) place permanently out of scope, absent
rather than disabled. Coordinates and pixels are what a human tester has, and
they turn out to be enough.

## Why input goes through Cordial and never the compositor

AGENTS.md already forbids synthesising input with `XTestFake*`, `ydotool`,
`wlr-virtual-keyboard` or the RemoteDesktop portal, because they land on
whatever has focus — which is the developer's own session, and one of them has
already hijacked a cursor mid-session. Cordial *is* the client, so there is
nothing to send through: the socket queues commands and the pump applies them by
calling `input::pass_*` directly, on the same thread `CORDIAL_SCRIPT` has always
used.

## Why it is off by default and unreachable from plugins

The socket exists only when `CORDIAL_DEV_CONTROL` is set, and it lives inside
the profile directory so [ADR-012](ADR-012-profiles-and-instances.md)'s
one-instance `flock` already decides who owns it.

No plugin can reach it. [ADR-007](ADR-007-host-resources-are-brokered.md) gives
plugins effects rather than channels, and a socket that drives input is exactly
the channel [ADR-003](ADR-003-plugin-isolation.md) exists to prevent. This is a
development aid for whoever is running the client, not a capability.

## Consequences

A freeze is now investigated by asking the client rather than asking a human:
`cordial_info` twice a few seconds apart separates a wedged engine from a slow
one, because a wedged engine leaves the present count fixed while everything
else keeps running. That exact reading — 42 presents against 74 million polls —
is what finally characterised the 2026-08-21 freeze, and it took seconds once
the counters were reachable.

The cost is a per-capture command pool, buffer and device-wide wait. That is
deliberate: a screenshot is taken a handful of times a run, and keeping a
command pool and a mapped buffer alive for the whole session would be a
permanent cost paid for a rare benefit.

**What this does not settle.** The surface is only as good as the questions
asked through it, and nothing here prevents a badly chosen instrument — the
three retracted above were all arithmetic, not tooling. `cordial_backtrace`
therefore quotes the process's CPU beside the stack, because a spinning pump and
a blocked one produce byte-identical backtraces and that distinction has been
read backwards here before.
