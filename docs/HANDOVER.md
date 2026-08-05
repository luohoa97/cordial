# Handover

Written for whoever picks this up next. It is deliberately not a summary of the
README — it is the things that are true, that cost somebody time to establish,
and that are not obvious from reading the tree.

Start with [AGENTS.md](../AGENTS.md). It is short, it is the contract, and every
rule in it was bought with a wasted afternoon.

## If you forked this

Open a pull request too. Not instead — as well.

Forking is fine and the licence exists to make it fine. What costs everybody is
**divergence**, and divergence is not measured in commits. Ten small fixes
cherry-pick in an afternoon, because the commit messages here say what was
measured and are still legible a year later. One fork that restructured a
subsystem to fix something its own way is not mergeable at all, and at that
point that fork is the real project and this one is a museum.

So the ask is narrow: whatever you are doing in your fork, also put it up as a
PR, and keep the branch rebased on `main`. That turns "somebody has to
reconstruct what you did" into "somebody clicks merge".

The queue will sit for a while. The author has stepped back and intends to come
back to hand the project over rather than to resume it, so PRs opened now are
read when that happens rather than next week. That is a real cost to you and it
is stated plainly rather than dressed up — but the queue is the first thing a
new maintainer inherits, and a PR in it is worth far more than a commit in a
fork nobody knows about.

## Where this stands

Cordial loads Roblox's official Android x86-64 `libroblox.so` natively on Linux
— ported AOSP bionic linker, bionic/glibc shim, libjnivm in place of ART, and a
framework layer answering the platform calls the client makes.

You can sign in, stay signed in across restarts, load a game, move around, turn
the camera and hear sound. That is further than it sounds: for most of this
project's life the client rendered a login form nobody could type into.

**The single most valuable habit here is refusing to state a result you did not
observe.** Several commits exist only to retract an earlier claim, and they are
the good ones. `docs/NEXT.md` is the long-form working record; it is 1700 lines
and it is honest, including about the things that turned out to be wrong.

## What needs an account, and therefore needs you

A large fraction of what is still unverified is unverified for one reason: it
requires a signed-in client inside a running experience. No contributor working
without an account can settle any of it, and no automated agent should try.

- **Does a granted pointer lock actually turn the camera?** The lock is
  requested, the compositor's refusal path is honest, and the release paths
  work. That relative motion reaches the camera is `INFERRED`. Granting requires
  the pointer over the canvas, which requires a real mouse in a real session.
- **Does game audio reach OpenSL ES at all?** Sound demonstrably comes out of
  the bridge, measured off the sink monitor with a zeroed-buffer control. But at
  the Landing screen the engine asks for no audio whatsoever, and
  `--dump-classes` shows Roblox naming `org.fmod.AudioDevice` — FMOD's *Java*
  `AudioTrack` output path. If FMOD picks that inside an experience, the whole
  OpenSL bridge is the wrong door. Unresolved.
- ~~**The 1 fps report.**~~ **Withdrawn, and worth reading as a cautionary
  tale.** A report of 1 fps in an experience looked like a perfect match for the
  documented idle throttle, which drops presents to exactly 1.0/s when nothing
  is happening. A whole theory followed: input not reaching the engine in the
  way its idle heuristic counts, with pointer capture as the likely fix. It was
  the developer's own machine under memory pressure from unrelated applications,
  and the same session on a quiet machine was "buttery smooth" on an Intel iGPU.
  Nothing in Cordial was at fault. Two coincidences did the damage — the number
  matched a real documented behaviour exactly, and the throttle was fresh in
  everyone's mind. **Ask what else was running before believing a performance
  report, including your own.**

**Do not test with an account anyone cares about**, and keep test accounts on a
separate IP. Enforcement is automated, runs in waves, and associates accounts
sharing an address. The risk is collateral rather than causal.

## Per-profile network egress (ADR-016), and what is still missing

A profile's `network.json` can now say `"mode": "vpn-required"`, which refuses
to start the client at all unless `pvpn status` reports traffic actually
passing — checked at both the shell's `launch.rs` and `cordial-run`'s own
`main`, so starting the client directly cannot skip it. Read ADR-016 before
touching any of this; the short version is below, with what to trust and what
not to.

**What this actually guarantees, and does not.** A `vpn-required` profile will
never make even Cordial's own client-settings request on the machine's
ordinary route while believing itself protected. It does **not** isolate two
profiles running at once from each other — `pvpn`'s tunnel is one, global,
machine-wide route, and ADR-012's own two-windows-at-once case means a second
profile running alongside a `vpn-required` one shares whatever route is
active, VPN or not. Read the mechanism's name literally: a launch gate, not a
sandbox.

**Why it stops at a gate rather than a namespace, and this is the load-bearing
fact for whoever picks this up next.** `pvpn` drives Proton's own Linux client,
which brings its tunnel up as a NetworkManager connection — confirmed by
reading `bin/pvpn` in the sibling project, not assumed. NetworkManager is a
system service in the host's own network namespace, so the interface it
creates lands there regardless of which namespace the command that asked for
it was run inside. `ip netns exec cordial-<profile> pvpn up` would not produce
a namespace-scoped tunnel; it would produce the same machine-wide one `pvpn up`
always produces. A real per-profile tunnel needs `pvpn` (or something
alongside it) to hand over the WireGuard parameters an established connection
negotiated, so a second, namespace-local interface can be brought up directly
with `wg-quick`, bypassing NetworkManager entirely. Nothing in `pvpn` exposes
that today. **This is the concrete next step**, not "make the namespace work"
in the abstract — the namespace mechanics themselves (veth, routing, `ip netns
exec`) are ordinary and not the hard part; extracting a usable tunnel
definition out of a NetworkManager-managed Proton connection is.

**`unshare --net -- ip link` fails with `Operation not permitted` in an
ordinary unprivileged shell** — measured directly, on the machine this was
written on, not assumed from `CAP_NET_ADMIN` documentation. Building the
namespace path also means deciding how Cordial's packaging (Flatpak in
particular) gets that capability at all, which ADR-007's existing argument
against broad sandbox permissions bears on directly.

**`INFERRED`, and deliberately not leaned on:** whether Roblox's own curl
usage inside the engine honours `http_proxy`/`HTTPS_PROXY` is unresolved —
the structural path for it to work is real (same process, same `environ`,
`bionic/mod.rs` does not shim `getenv`), but whether Roblox's code explicitly
disables that via `CURLOPT_PROXY` was not observed and cannot be without
tracing a signed-in client. It does not matter for anything built here: the
decision not to ship an `http_proxy`-shaped setting rests on `RtcIoRna` (the
real-time game transport) not being HTTP at all, which holds regardless.

**A measurement trap this work tripped over and is worth adding to the list
above:** `CORDIAL_PROFILE_ROOT` was being guarded by three *different*,
mutually unaware mutexes — one each in `profile.rs`, `profile_switcher.rs`,
and (new, in this change) `launch.rs`. Each looked correct in isolation and
none of them stopped a different file's tests setting the same process-wide
variable at the same moment. It surfaced as one failure in several runs of
`cargo test -p cordial-shell`, reading another test's scratch directory back
mid-assertion — exactly the "passed anyway on the first run" shape
`profile.rs`'s own tests already warn about. Fixed by sharing one mutex
(`crate::PROFILE_ROOT_ENV`, declared in `main.rs`) across every file in that
binary that touches the variable. If a future file needs to point
`CORDIAL_PROFILE_ROOT` at a scratch directory in a test, use that one rather
than adding a fourth private mutex that looks like it works.

## Open threads, with what is actually known

**Fullscreen clips and letterboxes** until you switch workspaces and back. Not
started. One read-only lead, unverified: on Wayland the swapchain extent does
not come from the compositor — `vk_get_physical_device_surface_capabilities_khr`
substitutes `wayland::current().geometry()` when Mesa reports `0xFFFFFFFF`, and
that geometry is written only by `apply_resize`, which early-returns when the
size is unchanged. So the thing to measure first is whether `apply_resize` fires
with the fullscreen size at all — not whether the swapchain was recreated.

**Textures and meshes render wrong, and the font is broken until it isn't.**
Unexplained. The leading suspect is Cordial's own MAILBOX-for-FIFO present-mode
substitution, which is what took the frame rate from a variable 35–50 to a flat
60 — if the engine's upload path relied on FIFO's pacing, MAILBOX would let it
sample buffers before their uploads land. `CORDIAL_PRESENT_MODE=fifo` is the
control and it has not been run. Ruled out already: ASTC support (this
developer's Intel iGPU has it) and a fall back to the software rasteriser
(`intel_icd` and `libvulkan_intel.so` are both installed).

**Typing into text fields draws nothing** until the field loses focus. The
per-keystroke sync path is understood; what is missing is an EditText-equivalent
overlay. `CORDIAL_TRACE_TEXT=1`.

**Shift+F5 does not open Roblox's stats menu.** Two candidates, neither
established: the key path sends an evdev keycode with Android `META_*` modifier
bits, which is a mixed vocabulary and mixed vocabulary is what cost four failed
keyboard theories before — or these are desktop-only debug shortcuts that the
Android build never wires at all. Settling it is cheap.

**Web views are unimplemented**, which is why a lot of Roblox's UI does nothing.
Needs `webkitgtk6.0-devel`; it is absent on the developer's host and present in
their distrobox.

**There is no Roblox Android build to download.** ADR-015 permits fetching and
the entire fetcher is built and proven — streaming, SHA-256, zip refusals,
metered detection. It has no URL because Roblox publishes no Android artefact:
`setup.rbxcdn.com/android/DeployHistory.txt` is 403, `client-version/AndroidApp`
is 500, and `roblox.com/download` offers Google Play and the Amazon Appstore and
no file. Sober does not fetch from Roblox either; it routes users through Google
Play. Aptoide is deliberately not wired — a mirror offering only a hash it
supplied itself is verification theatre.

## Traps that have already caught people

**Do not use present counts as a frame rate.** Every figure recorded before
2026-08-02 is an idle throttle integrated over a window, and several were quoted
as evidence.

**Do not measure timing under `WAYLAND_DEBUG=1`.** Three findings taken that way
vanished on untraced repeats minutes later.

**`CORDIAL_TRACE=1` aborts the engine.** It wraps variadic functions ABI-unsafely.

**`/proc/locks` names the wrong process.** Its PID column is whoever *acquired*
the lock, which `Claim::hand_to` makes the launcher — and the launcher has
usually exited. Scan `/proc/*/fd` instead. Observed: `/proc/locks` naming a PID
that no longer existed while a live process held the descriptor.

**Cumulative counters are not rates.** `pgsteal_kswapd` and friends integrate
since boot; reading one as a rate produced a confident and wrong diagnosis of
system-wide thrashing in this very project's history. Sample twice.

**`mutter --headless` segfaults** within a second of Cordial's window mapping,
on unmodified HEAD, with and without GPU rendering, while `gtk4-demo` in the
same nested compositor runs indefinitely. AGENTS.md offers nesting a headless
compositor as the way to drive Cordial's own window without touching the
developer's session, and it does not currently work. That is why
`crates/cordial-runtime/examples/pointer_capture_probe.rs` exists.

**Never synthesise input at the compositor.** `XTestFake*`, `ydotool`,
`wlr-virtual-keyboard`, the RemoteDesktop portal — all land on whatever has
focus, which is the developer's session. This has hijacked a developer's cursor
once already, mid-session.

## Layout

    crates/cordial-runtime   the loader, bionic shim, Android framework layer
    crates/cordial-shell     the launcher; also owns the shared window definition
    crates/cordial-plugins   registry, dependency resolution, unpacking
    crates/cordial-update    version check, download, verification, metered
    native/                  C++ shims: OpenSL ES, PipeWire, Java classes
    third_party/             the AOSP bionic linker port
    docs/adr/                decisions, including the reversed ones
    docs/traces/             a logcat capture of the same APK on real Android

**`docs/traces/` is the single most under-used thing in this repository.** Grep
it before disassembling anything. Note that the startup log is gzipped, so a
plain `grep -r` silently misses it — use `zgrep`, and do not read a zero hit as
evidence of absence.

## The two profile modules

`cordial_shell::profile` and `cordial_runtime::profile` implement the same
contract twice. That is not a design; `cordial-runtime` depends on
`cordial-shell` for `host_window`, so the dependency cannot be inverted without
a cycle. The shell's copy is the live one — `cordial-run` never calls its own.
Unifying them is a genuinely good first contribution.

## House style

Comments explain *why*, anchored in the failure that motivated the code. Commit
messages say what you measured, and they are long here on purpose. British-ish
prose, no emoji, no bullet-list comment blocks. Read the surrounding file before
writing; the voice is consistent and matching it is not optional.

Arguing with an ADR is welcome — ADR-004 was reversed exactly that way. What is
not acceptable is quietly contradicting one in code.
