# ADR-031: The launcher outlives its window, and the client is a child process

**Status:** Accepted
**Date:** 2026-09-13
**Extends:** [ADR-012](ADR-012-profiles-and-instances.md)
**Related:** [ADR-002](ADR-002-core-shell-and-ui-handoff.md), [ADR-011](ADR-011-wayland-and-libadwaita.md)

## Context

Cordial is two processes. `cordial-shell` is the launcher — a single-instance
`GtkApplication` on the session bus, which is what lets a `roblox-player:` link
clicked in a browser reach the launcher that is already open instead of starting
a second one. `cordial-run` is the client: it loads `libroblox.so`, holds the
profile's `flock` (ADR-012) and owns its own window.

Two questions were open, and they turn out to be the same question.

**"Closing the launcher closes Cordial completely"** was reported against the
Flatpak build on 2026-09-13. **"Is launching the binary the proper way, or is it
a hack?"** was asked in the same breath.

## The reported bug is real, and the obvious explanation for it is wrong

The natural reading is that a Flatpak sandbox tears down when its `command:`
process exits, taking the client with it. **That was measured and it is false.**
A marker process backgrounded inside `flatpak run --command=sh
io.github.luohoa97.Cordial`, with the sandbox's own main process then exiting,
was still alive on the host seconds later with `bwrap` still hosting it — both
as a plain child and under `setsid`. The sandbox is not the mechanism.

The mechanism is a pipe.

`launch.rs` gives the child `Stdio::piped()` on both streams and pumps them on
reader threads, so that the crash page can quote what the client printed. **The
read ends of those pipes belong to the launcher process.** Close the launcher
window, and with no window left the `GtkApplication` quits, the process exits,
both read ends close — and the next `println!` inside `cordial-run` panics:

    thread 'main' panicked at library/std/src/io/stdio.rs:1165:9:
    failed printing to stdout: Broken pipe (os error 32)

That message is quoted from a captured stderr, not inferred. It was reproduced
with a two-binary stand-in of exactly this shape — a Rust parent that pipes and
pumps, a Rust child that prints on a timer — **and with the control that decides
it: an otherwise identical child that never prints survives the same parent
exiting.** The client narrates constantly, so the gap between the launcher
quitting and the client dying is milliseconds, which is why it presents as "they
close together".

Nothing about this is sandbox-specific. It was reported on Flatpak because that
is what most users install.

**This also retracts a comment.** `window.rs` said that under ADR-012 "quitting
the launcher while a client runs is the ordinary case". That described an
intention. It had never been true.

## Decision

**The launcher holds itself alive for as long as a client it started is
running.** `GtkApplication::hold()` is taken when `launch::spawn` succeeds and
the guard is dropped by the `SIGCHLD` child watch when the client exits, at
which point the application quits by itself. Closing the launcher window now
closes a window; it does not end the process.

**The client stays a child process, spawned with `Command`.** That is not a
hack, and the alternatives are worse:

* **A `fork`/`exec` daemon that reparents to init** would keep the client alive
  past a launcher crash, and would cost the crash page — the captured output is
  the single most useful thing in a bug report here — and the `SIGCHLD` watch
  that drives it.
* **`flatpak-spawn --host`** is arbitrary command execution on the host and is
  refused outright by the manifest's own reasoning; see the header of
  `packaging/io.github.luohoa97.Cordial.yml`.
* **One process** contradicts ADR-002, which keeps the shell buildable without
  the engine on purpose, and would put a 1.5 GB engine load in the same address
  space as the launcher's WebKit view.

A child process with an inherited profile lock, a piped narration and a
`SIGCHLD` watch is the right shape. What was missing was the lifetime rule that
makes it safe, and that is what this ADR adds.

## What this does not fix

**A launcher that crashes still takes the client with it**, by the same pipe.
The hold makes the ordinary path safe; it does not make the client independent.
The defence in depth is for `cordial-run` to survive a broken stdout rather than
panic on it — Rust's `println!` panics on `EPIPE` because std sets `SIGPIPE` to
`SIG_IGN` at startup, so the write error surfaces rather than the signal. The
better shape is for the client to narrate into a log file inside its profile and
for the launcher to tail that, which would also give users a log to attach to a
bug report. Neither is done here, and neither should be claimed.

**Deep links still do not reach a client that is already running.** The launcher
is single-instance, so the *link* reaches the running launcher correctly; it is
then held until the user presses the button, and pressing it starts a new client
— which ADR-012's lock refuses if that profile is already open. Routing a join
into a live engine needs an IPC into `cordial-run` that does not exist outside
the development control surface (ADR-019). That is a separate decision and
wants its own ADR rather than being folded into this one.
