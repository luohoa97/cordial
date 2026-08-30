# ADR-028: X11 input comes from XInput2, with the warp as the fallback

**Status:** accepted
**Related:** [ADR-024](ADR-024-x11-is-supported-again.md), [ADR-011](ADR-011-wayland-and-libadwaita.md), [ADR-009](ADR-009-input-capture-and-injection.md)

## Decision

**The X11 backend takes its input from XInput2 wherever XI2 has an answer.**
Core X11 input stays only where XI2 does not reach, and as the degradation path
on a server too old to negotiate XI 2.0.

**That degradation path is [PR #22](https://github.com/luohoa97/cordial/pull/22)'s
warp-based pointer lock, by HaltingMachine, retained rather than deleted.**
When `XIQueryVersion` refuses, the lock falls back to grab, hide, warp to the
centre and filter the synthetic motion — the mechanism that change introduces.
This is the whole reason it is worth landing that pull request on its own terms
rather than asking its author to wait for XI2: the work is not a stopgap that
gets thrown away, it is the branch that runs when the modern one is
unavailable.

Concretely, in `crates/cordial-runtime/src/android/window.rs`:

- **Pointer motion comes from `XI_RawMotion`**, not `MotionNotify`, and the
  pointer lock stops warping. `XIQueryVersion` negotiates, `XISelectEvents`
  subscribes on the root window, and each event arrives as a `GenericEvent`
  read through `XGetEventData` / `XFreeEventData`.
- **The grab and the cursor hiding do not change.** `XGrabPointer` with
  `confine_to` set to Cordial's own window, and `XDefineCursor` with a 1×1
  bitmap. Not `XFixesHideCursor`: the bitmap needs no library Cordial does not
  already have open, and `window.rs` loads Xlib by `dlsym` by hand rather than
  through a crate, so every additional `.so` is a new failure mode on somebody
  else's distribution.
- **Scroll comes from valuator axes** rather than core buttons 4–7, which needs
  XI 2.1.
- **Touch becomes available** through `XI_TouchBegin`/`Update`/`End`, which
  needs XI 2.2. `window.rs` already records that "X11 core input has no touch at
  all"; this is what changes that, if anyone wants it.

## Why

**Core X11 cannot give an unaccelerated delta, and that is not a limitation
that can be worked around.** A `MotionNotify` reports a pointer position the
server has already put through its own acceleration curve. Subtracting two of
them gives an accelerated delta. There is no core request that recovers what
the device actually reported.

That matters here because Cordial's own pointer acceleration is meant to apply
to the cursor over Roblox's UI and *not* when the pointer is locked, where the
camera wants what the mouse actually did. On core X11 that setting can only ever
be our curve stacked on the server's.

`XI_RawMotion` carries both numbers: `raw_values` is what the device reported
before acceleration, `valuators` is after it.

**That pair is exactly what `zwp_relative_pointer_v1` already hands the Wayland
backend**, which is the second reason and the more durable one. Today the two
backends are different shapes: Wayland reasons about a pre/post acceleration
pair, X11 reasons about warping the pointer back to the centre and filtering out
the synthetic motion its own warp produced. ADR-024 asks that "shared logic
moves to a common module rather than being duplicated", and that is not
honestly achievable while the two paths compute different things from different
primitives. With XI2 it becomes a real refactor rather than a wrapper over two
unlike mechanisms.

**Third, per-device routing stops being inferred.** `80f9ccb` renamed
`deliver_touch` to `deliver_mouse` because the touch path arriving made the old
name a lie, and its subject was routing input by the device that produced it.
XI2 events carry a source device id. On core X11 the device is something we
deduce; on XI2 it is a field.

## What this costs

- **`libXi` must be opened.** One more `dlsym`'d library, and one more thing
  that can be absent.
- **XI2 must be negotiated, and can be refused.** `XIQueryVersion` against a
  server that predates X.Org 1.7 will not give 2.0. The warp path therefore
  stays, and must be a real fallback that has been run and not a branch nobody
  executes — a fallback that rots is exactly what ADR-011 predicted about
  parallel backends, and it applies within a backend too. Whatever exercises
  the XI2 path in CI or by hand exercises this one with XI2 forced off, in the
  same session, or the claim that it still works is unmeasured.
- **Raw events ignore focus and grabs.** `XI_RawMotion` on the root window
  reports the pointer moving whatever has focus, including when that is not
  Cordial. They must be consumed only while the lock is held. Reading them
  otherwise would mean Cordial recording the user's mouse across their whole
  session, which is a privacy failure and not merely a bug.
- **Cookie lifetime is a real trap.** The data behind a `GenericEvent` is only
  valid between `XGetEventData` and `XFreeEventData`, and nothing in the type
  system says so.

## What the user sees when XI2 is missing

**The "Use system mouse acceleration settings" row is disabled, pinned to
"Cursor and camera", and says why.** Not merely greyed out: pinned to the value
that is actually in force.

That row offers "Only the cursor" and "Cursor and camera", and what it really
decides is whether the desktop's pointer profile reaches the *camera*. On
Wayland both are answerable, because `zwp_relative_pointer_v1` delivers the
accelerated and unaccelerated delta side by side and the setting picks one. On
X11 with XI2 the same is true of `raw_values` and `valuators`. Without XI2
there is only the accelerated number, so "Only the cursor" cannot be honoured
and the camera follows the desktop profile whatever the row says.

Greying the row while it still displayed "Only the cursor" would therefore
state something false about the running system. Disabling it *and* moving it to
"Cursor and camera" states the truth, and the subtitle carries the reason —
that this X session offers no XInput2, so Cordial cannot separate the
acceleration the server already applied.

This is the same judgement `settings.rs` already made one step away, and its
comment is worth honouring rather than re-deriving: there is no "never" option
because while the cursor is unlocked the compositor hands over an
already-accelerated absolute position, and an entry that silently does nothing
is "the interface shape of a stub that returns success". A row offering a
choice the backend cannot make is that same shape.

**Open, and to be settled by running it rather than reasoning about it:**
whether this row is *already* misstating things on X11 today, before any of
this lands. That depends on what the current grab-only capture feeds the camera
path, which has not been checked. If it is, that is a present-tense bug to fix
when the greying goes in, and the fix is the same code.

## Sequencing

**After [PR #22](https://github.com/luohoa97/cordial/pull/22) lands, not
alongside it.** That change rewrites the same pointer path, and CLAUDE.md names
`window.rs` as a file parallel work has already collided on more than once.
PR #22's warp-based lock is a working improvement on the grab-only capture that
preceded it and should land on its own terms. The warp and its
`ignore_next_warp` latch are **not** deleted afterwards: they become the branch
taken when `XIQueryVersion` refuses. What XI2 changes is which path is default,
not whether the other one exists.

## What would change this

A measurement showing `raw_values` is not in fact pre-acceleration on some
driver that matters — the semantics are the X server's and evdev's, not ours.
The rule at the top of AGENTS.md applies: this is settled by running something
and reading both numbers with the server's acceleration turned up, not by
reasoning about what the field ought to mean.
