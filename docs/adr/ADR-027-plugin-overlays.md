# ADR-027: Plugins describe an overlay; Cordial draws it

**Status:** proposed
**Related:** [ADR-029](ADR-029-overlays-are-three-decisions.md), [ADR-001](ADR-001-in-process-hooking.md), [ADR-003](ADR-003-plugin-isolation.md), [ADR-007](ADR-007-host-resources-are-brokered.md), [ADR-010](ADR-010-plugin-asset-overlays.md), [ADR-011](ADR-011-wayland-and-libadwaita.md), [ADR-026](ADR-026-the-core-event-bus.md)

## Decision

A plugin may put its own interface on screen over the running client. It does so
by **sending a description of what it wants shown**; Cordial builds the widgets
with GTK and composites them over the engine's surface.

Three rules.

1. **A plugin never draws.** It sends a declarative payload — a toast, a panel of
   rows, a HUD element — and receives a handle it can update or dismiss. It gets
   no surface, no drawing context, no pixels, and no way to ask for one.
2. **Overlays are split by what they cost the player, not by what they contain.**
   Three capabilities, granted separately: `ui.notify` for something transient,
   `ui.hud` for something that persists over gameplay, `ui.panel` for something
   that takes focus. A plugin permitted to show a toast is not thereby permitted
   to cover the screen.
3. **Cordial owns the frame.** Position, stacking, styling, dismissal and the
   right to refuse are Cordial's. A plugin asks for a region by name, not by
   coordinate.

## Why this needs no hooking, and is not a new architecture

> **Corrected by [ADR-029](ADR-029-overlays-are-three-decisions.md).** The
> paragraph below says a GTK widget placed above the canvas is drawn above it.
> The second half of that -- no cooperation from the engine -- is true and is
> the point. The first half is not: the subsurface is placed *above* the parent
> in the ordinary case, so a widget put on top of it is drawn **underneath**
> until something lowers the engine, stops GTK painting over the hole, and
> decides who gets the clicks. An overlay built on the sentence as written
> would render nothing and look like a GTK bug. ADR-029 has the three pieces of
> state and the two shapes an overlay may take; a plugin overlay is an Inset or
> a Modal in its terms.

**The engine's `wl_surface` is already a `wl_subsurface` of Cordial's GTK
toplevel** — ADR-011 records that and the reasons it had to be. So a GTK widget
placed above it is drawn above it, by the compositor, with no cooperation from
the engine and nothing injected into its process.

That is not a theory. `crates/cordial-shell/src/host_window.rs` already does it:
`TextOverlay` puts a real `gtk::Text` over the engine's output whenever a Roblox
TextBox takes focus, and the player sees the GTK widget's glyphs rather than the
engine's. The overlay system exists and has exactly one user.

So this ADR adds an API to a mechanism that is already shipping, rather than a
mechanism. That is the main argument for it, and it is also the reason to be
careful: the text editor took a long time to get right — input routing, focus,
IME, sizing against a surface that resizes — and every one of those problems
belongs to overlays too.

## Why not a web view

It was proposed, on the reasonable grounds that somebody might want to write an
overlay in React. Rejected for the overlay path specifically, on four counts.

**Cost per plugin.** A `WebKitWebView` is a browser: a web process, a network
process, its own compositor. Three plugins with overlays would mean three
browser instances composited over a game that is already the thing struggling
for frames. `TextOverlay` is one lightweight widget, and that is the shape an
overlay wants.

**It is the piece that does not travel.** WebKitGTK spawns its helpers by
absolute path, which is why the AppImage's embedded browser does not work
without WebKitGTK 6.0 installed on the host. Making overlays depend on it would
put plugin interfaces in the same condition, in the format with the most
downloads.

**Input routing is the hard part and a web view makes it harder.** An overlay
must decide, per region, what passes through to the game and what does not. That
is solved for the text editor and would have to be solved again for a browser
widget that grabs input on its own terms.

**It breaks the capability model.** ADR-007's rule is that a plugin receives an
effect and never a channel. A web view is a network stack, a storage API and a
JavaScript runtime that Cordial does not broker. Handing one to a plugin gives
it every channel at once, which is the thing "no sockets, no file descriptors"
exists to prevent.

**Where a web view is right, and is not ruled out:** a plugin's own settings or
dashboard, in a separate window, not composited over gameplay. Rich layout, no
latency budget, no passthrough problem, and the existing web view already does
it. That is a different feature and wants its own decision; nothing here forbids
it.

## Why declarative, which is the part that ages well

A plugin that sends pixels or markup pins Cordial to whatever rendered them.
A plugin that sends *"a toast saying this"* survives the renderer changing
underneath it.

Four things fall out of that and none of them are available otherwise:

- **Overlays cannot fight.** Cordial places them, so two plugins cannot both
  own the top-right corner, and a misbehaving one cannot cover the client.
- **They look like Cordial.** One libadwaita style, restyled in one place, and
  a plugin author does not have to reimplement a theme to look native.
- **They can be refused.** A HUD element while the client is loading, or a panel
  during a join, can be declined or deferred, because Cordial knows what the
  client is doing and the plugin does not.
- **The payload is inspectable.** A grant dialogue can say *what* a plugin wants
  to show, which it cannot do for an opaque surface.

The cost is real and should be stated: **a plugin cannot build an interface
Cordial has no vocabulary for.** Somebody will want a chart. The answer is to
add it to the vocabulary, deliberately, rather than to open a hole that makes
the question moot — and if the vocabulary grows without limit this decision has
failed and should be reversed rather than quietly stretched.

## What is not decided here

The payload schema, the region names, the update and dismissal shape, and
whether a HUD element may persist across a join. Those want writing against a
prototype rather than in advance.

Also not decided: whether overlays should be visible in a screenshot taken
through `cordial_screenshot`. That reads the engine's swapchain, so today they
would not be — which is right for debugging the engine and wrong for a bug
report about an overlay.

## Consequences

`ui.notify` supersedes nothing but overlaps `notify.send`, which sends a desktop
notification through the host. They are different: one is a system notification
that survives the client being minimised, the other is drawn over the game. Both
should exist and the documentation must not let them be confused.

The three capabilities are additive and default-denied like every other, so an
installed plugin gains nothing until somebody grants them.

`TextOverlay` should end up using the same machinery rather than sitting beside
it, but not in the first change: it is load-bearing, it is the one part of text
entry that works, and rewriting it to prove a point about symmetry is how that
stops being true.
