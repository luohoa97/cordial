# ADR-029: An overlay is three decisions, and every overlay makes all three

**Status:** accepted
**Related:** [ADR-011](ADR-011-wayland-and-libadwaita.md), [ADR-019](ADR-019-development-control-surface.md), [ADR-027](ADR-027-plugin-overlays.md)

## Decision

Anything Cordial draws over the running engine — the text editor on a focused
`TextBox`, a web-view dialog, and every plugin overlay ADR-027 proposes — is
**three independent pieces of compositor state, not one**:

1. **Stacking.** Is the engine's `wl_subsurface` above or below the GTK
   toplevel? `WaylandWindow::set_engine_stacking`.
2. **Paint.** Does GTK paint over the canvas rectangle, and does the parent
   surface claim to be opaque there? `HostWindow::set_canvas_see_through` and
   `refresh_opaque_region`.
3. **Input.** Does the parent's input region cover the canvas rectangle?
   `HostWindow::refresh_input_region`.

**Every overlay must set all three deliberately.** Setting one and inheriting
the others is not a shortcut, it is the bug — each of the three has been
shipped alone at least once and each produced a different, badly-diagnosed
failure. They are listed under "Why" because the list is the argument.

There are exactly **two supported overlay shapes**. A new shape needs a new ADR.

| | Stacking | Paint | Input |
|---|---|---|---|
| **Inset** — a widget on one thing, e.g. the text editor | engine below | transparent over the canvas | canvas punched out of GTK's region, **the widget's own rectangle unioned back in** |
| **Modal** — takes the window, e.g. a web-view dialog | engine below | transparent over the canvas | **no hole at all**; GTK claims the whole surface |

The difference between the two rows is the whole of "can you click past the
overlay into the game". For an inset overlay, **yes, and deliberately** — the
editor is a widget on one `TextBox` and the rest of the canvas is still the
game's. For a modal, **no, and deliberately** — that is what modal means, and
it is the same statement `wayland::dialog_in_front` already makes on the
event-forwarding side.

And one prohibition, which is the only thing here that is a rule rather than a
choice:

> **Never make the engine's canvas input-transparent.** Do not call
> `wl_surface.set_input_region` on the subsurface. An empty input region does
> not mean "give this click to the parent"; it means no surface here wants it,
> and because the parent's region already has the canvas punched out of it,
> nothing in the window claims the point and the compositor gives the click to
> **the next window down**.

## Why

### The engine owns a surface, so Cordial cannot simply stack a widget over it

[ADR-011](ADR-011-wayland-and-libadwaita.md) put the engine's canvas in a
`wl_subsurface` of Cordial's GTK toplevel, because `ANativeWindow` has to be a
real surface and the engine drives its own Vulkan swapchain against it. The
consequence is the thing this ADR exists to write down: **the canvas is
composited by the compositor, not laid out by GTK.** It is not in the widget
tree. GTK cannot stack anything above it, because from GTK's point of view
there is nothing there — the canvas slot is a `GtkDrawingArea` with no draw
function.

So "draw something over the game" is not a widget operation. It is: put the
engine *below* the toplevel, stop GTK painting over the hole, and decide who
gets the clicks. Three knobs, one intent.

**[ADR-027](ADR-027-plugin-overlays.md) states this too simply** and is
corrected by this ADR. It says "a GTK widget placed above it is drawn above it,
by the compositor, with no cooperation from the engine". The second half is
true and is the good news. The first half is not: the subsurface is placed
*above* the parent in the ordinary case, so a GTK widget put on top of it is
drawn **underneath** until somebody lowers the engine. Any plugin overlay built
on that sentence would have rendered nothing and looked like a GTK bug.

### Each knob has been shipped alone, and each failure looked like something else

This is the expensive part, and it is why the table above is normative rather
than advisory.

**Stacking alone.** Lowering the canvas without touching the opaque region: the
compositor was told the parent is opaque over the canvas, so it skipped
compositing the subsurface underneath and the window went a flat `#222226`.
Measured with the toplevel *and every descendant* forced transparent, still
that exact colour, while the engine presented at sixty frames a second and the
game reappeared the instant the canvas was raised. Every attempt to fix it with
CSS failed, because it was never a painting problem. It looked
state-dependent — it happened to work on the landing page, where moving between
pages resizes often enough that a geometry sync lands soon after the lower.

**Paint alone.** The opaque region punched but the background left opaque:
canvas completely black. Recorded in `set_engine_stacking`'s own comment,
because the two have to be paired or the window is see-through with nothing
behind it.

**Input, forgotten.** The editor's hole in the parent's input region was
punched by `set_text_overlay` and then immediately overwritten by
`refresh_input_region` running off the geometry sync, roughly twenty times a
second. The hole existed for a few milliseconds at a time, so a click into the
text field essentially never landed — reported as "you cant click to move the
caret ... you cant drag to select text". It survived a first fix that corrected
the hole's coordinates and left the overwrite in place.

**Input, done from the wrong end.** `07564e2` gave the *canvas* an empty input
region while a dialog was up, on the theory that the canvas was eating the
dialog's clicks. Every click over the canvas then fell through Cordial's window
and raised whatever was behind it: "as soon as we press anything it selects the
window behind and focuses it ... my terminal gets focused". Reverted in
`73c74eb`. The prohibition above exists because this is not obviously wrong
from the protocol's wording, and somebody will reach for it again.

**Input, the actual dialog bug.** The parent's region is the whole window minus
the canvas rectangle, and an `AdwDialog` draws inside that same toplevel,
centred — so its buttons sit inside the hole and every click on them went to
the subsurface rather than to GTK. Reported as "I cant click on the webview's
items", diagnosed twice as stacking and once as pointer lock before anybody
read `refresh_input_region`'s own doc comment, which had the measurement in it
the whole time: *"two synthetic clicks produced four `nativePassMouseButton`
calls into the engine and without it zero"*.

Five failures, four of them attributed to the wrong knob at first. That is the
case for writing the three down together.

## What this rules out

- **An overlay that does not say what it wants from input.** There is no
  default. A new overlay picks Inset or Modal, or it argues for a third shape
  in a new ADR.
- **`set_input_region` on the engine's subsurface**, for the reason above.
- **Treating the opaque region and the input region as the same shape.** They
  are not and must not be: the editor is drawn over the canvas with a
  transparent background, so it stays part of the *opaque* cut-out while being
  unioned back into the *input* region. `host_window.rs` says so at the
  function that would otherwise conflate them.
- **Deciding any of the three from CSS.** Two of them are compositor state that
  GTK does not model.

## The alternative, and why it is not this ADR

The shape that removes most of this is: **put the engine's canvas permanently
below the toplevel, leave GTK's background transparent over it always, and put
a real input-accepting widget in the canvas slot that forwards to
`input::pass_*`.** GTK's own hit-testing then decides what reaches the engine —
a click that lands on no overlay widget falls through to the forwarding widget
and is forwarded; a click on an overlay is GTK's. Overlays become ordinary
`GtkOverlay` children and none of the three knobs moves at runtime.

That deletes the restack, the see-through toggle, `POINTER_ON_CANVAS`,
`dialog_in_front`, and most of Cordial's second `wl_pointer`. It is the right
architecture and it is not adopted here, for three reasons worth stating rather
than leaving as an omission:

1. **It is untested and this file is a list of untested changes going wrong.**
   Every failure above was shipped on a plausible reading of the protocol. The
   correct order is a spike behind an environment variable, measured against
   the existing path in the same session, and then an ADR that supersedes this
   one.
2. **It moves the riskiest code in the client.** Text entry is the one area
   this project has regressed repeatedly and the one users notice immediately.
3. **The compositor stops being able to skip work.** A permanently transparent
   parent means the parent can never declare an opaque region over the canvas,
   which is the hint that lets a compositor avoid compositing what is
   underneath. Nobody has measured what that costs.

Until then, the table is the contract.
