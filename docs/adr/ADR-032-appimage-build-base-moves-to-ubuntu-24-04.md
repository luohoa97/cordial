# ADR-032: the AppImage's build base moves to Ubuntu 24.04, and the version floor that blocked it was wrong

**Status:** Accepted
**Date:** 2026-09-13
**Extends:** none
**Related:** [ADR-033](ADR-033-roblox-versions-are-a-keyed-store.md)

## Context

The shipped AppImage did not start on a large fraction of hosts. Two independent
defects, found by running `packaging/check-glibc-floor.sh` (written for the
`.rpm`) over every ELF in the built AppDir rather than only Cordial's own two
binaries, which is what both this script's own release-workflow callsite and
`.github/workflows/release.yml` did until now:

**Eighteen bundled libraries carried `GLIBC_2.43` symbol versions.** Among
them `libgtk-4.so.1`, `libglib-2.0.so.0` and `libwebkitgtk-6.0.so.4` — the ones
nothing starts without. glibc is backward compatible and not forward
compatible: a binary built against 2.43 refuses to load against an older
libc, with `version 'GLIBC_2.43' not found`, one line per library, which reads
like a wall of missing dependencies rather than what it is. An AppImage
bundles no libc of its own, so this was not a bug in what Cordial links — it
was the build container's glibc, silently newer than the declared floor.

**Thirteen further libraries were absent outright**, dropped by linuxdeploy's
own excludelist, which assumes a fixed set of libraries is present on every
host it might run on. That assumption is not the same claim as "these are
supplied by the graphics stack and must come from the host or nothing
renders" — the latter is a decision this project makes deliberately, for
`libEGL`, `libGLX`, `libGL`, `libOpenGL`, `libGLdispatch`, `libgbm`, `libdrm`
and `libGLESv2` — and linuxdeploy's list is broader than that and not audited
against Cordial's actual closure. The fix for both defects is the same:
compute the bundle's dependency closure from `ldd`/`objdump` over what is
actually built, rather than trust a tool's built-in idea of what a host has.

## Why the build base sets the glibc floor

The declared floor is glibc 2.39. A binary — or a bundled library — built in a
container whose glibc is newer than that binds against symbol versions the
floor does not have, and nothing after the build can undo that: glibc's own
compatibility guarantee runs one direction only. So the base image is not a
convenience choice like it would be for an ordinary build; it is the actual
mechanism that sets what floor the result can claim. Fedora 44 ships glibc
2.43, four minor versions past the floor, which is exactly what put those
eighteen libraries above it. The base has to be a distribution whose own glibc
is at or below 2.39 — no newer — or the AppImage's stated floor is simply
untrue of what it bundles.

## The version floor that blocked this was eight minor versions too high, and wrong

Fedora had been the base because `gtk4` was pinned at the `v4_20` Cargo
feature. That pin was never a measurement: the comment beside it in
`crates/cordial-shell/Cargo.toml` said GTK 4.10, the pin said 4.20, and neither
was what the code used. Lowering the feature to `v4_10` failed the build at
exactly three call sites — two uses of `CssProvider::load_from_string`, one of
`ToplevelState::SUSPENDED` — all three satisfied by GTK 4.12. `v4_12` builds
the workspace clean, and `libadwaita`'s floor was already 1.5 for `AdwDialog`.
Nothing in the shipped feature set needed 4.20; the pin had simply never been
revisited downward once it was raised to match whatever the newest development
host happened to carry, which is the same mistake this project's own justfile
already warns about for the build environment itself.

That is what made a different base possible at all. At `v4_20`, only a
distribution shipping GTK 4.20 or newer can even compile the workspace, and at
the time of writing that is Fedora 44 and little else — a floor that picks the
build base for you, and picks one four glibc minor versions past where the
AppImage needs to sit. At `v4_12`, the field of usable bases widens to
anything shipping GTK 4.12 or newer, which is most current LTS releases.

## Why Ubuntu 24.04, not the usual answer of 22.04

22.04 is the community's default AppImage base — it is what most published
recipes assume, on the reasoning that an old-enough LTS maximises forward
compatibility. It does not work here: 22.04 ships GTK 4.6, well under the 4.12
the code now needs even after the floor was corrected, and no packaging
workaround changes what a distribution's own repositories carry. Passing over
the obvious choice is worth stating plainly rather than leaving a reader to
wonder why.

24.04 clears every bar at once: GTK 4.14.5, libadwaita 1.5.0, WebKitGTK 6.0
(`libwebkitgtk-6.0-dev` 2.52.6, with 2.44.0 also available in the release
pocket), and glibc 2.39 — not merely under the floor, but *exactly* it, which
is the strongest version of the claim this project can make about where the
AppImage runs.

## WebKitGTK's availability was checked before anything else, per this ADR's own precondition

Moving the base is only useful if the web view still builds. `apt-cache
policy libwebkitgtk-6.0-dev` against `ubuntu:24.04` was run before any other
work on this change, and returned a candidate (`2.52.6-0ubuntu0.24.04.1` in
`noble-updates/universe`, `2.44.0-2` in the base `noble/universe` pocket) —
so the answer was yes, and the rest of this change proceeded. Had it been no,
the right response was to stop and report that, not to drop the webview
feature or vendor a build of WebKitGTK.

One consequence of the switch surfaced during this: the WebKitGTK helper
processes and injected bundle live at a different absolute path on a
Debian-family build than on Fedora's. Fedora's package puts them under
`/usr/libexec/webkitgtk-6.0` (helpers) and `/usr/lib64/webkitgtk-6.0`
(injected bundle); Ubuntu's puts both in one directory,
`/usr/lib/x86_64-linux-gnu/webkitgtk-6.0`, confirmed with `strings` against
the package this base now bundles. `packaging/appimage/build-appimage.sh`'s
own comments had already anticipated this split by path before this change
was made. What has not been re-verified is `AppRun`'s mount-namespace bind,
which still targets the Fedora paths measured on 2026-09-02 against a
Fedora-built library — that measurement does not transfer to a
Ubuntu-built one, and re-measuring it needs a display this pass did not have.
Both files say so at the relevant line rather than silently carrying the stale
claim forward, and a follow-up has been raised to redo the measurement.
None of this affects whether `cordial-shell`/`cordial-run` start and pass
dynamic linking, which is what this ADR's own measurements below test — a
headless container never reaches the point where a `WebProcess` is spawned.

## The excludelist gap needed an actual fix, not just a measurement

The Context section above describes thirteen libraries linuxdeploy drops on
the assumption a host already has them. That was a measurement against the
Fedora build; nothing in `build-appimage.sh` actually closed the gap before
this pass. Building on Ubuntu 24.04 hit it for real: the produced AppImage
failed on plain `ubuntu:24.04` with `cordial-shell: error while loading
shared libraries: libharfbuzz.so.0: cannot open shared object file`, and
linuxdeploy's own log explains why -- `Skipping deployment of blacklisted
library .../libharfbuzz.so.0`. Its excludelist is tuned for a stock desktop,
not the minimal container this AppImage is meant to run on.

`build-appimage.sh` now closes the gap itself: `patchelf --print-needed`
against every bundled ELF, anything missing from the AppDir and not on an
explicit never-bundle list gets copied in from the build host via
`ldconfig -p`, to a fixed point (one library can need a further one
linuxdeploy also skipped). Measured: thirteen libraries completed in one
pass -- `libharfbuzz.so.0` and its own dependants -- a second pass completed
zero.

The first version of that fix filled the closure for `libc.so.6` and
`ld-linux-x86-64.so.2` as well, because both are genuinely `DT_NEEDED` by
`cordial-shell`/`cordial-run` and were genuinely absent. That would have
shipped a worse bug than the one it fixed. `cordial-shell` carries `RUNPATH
$ORIGIN/../lib` (`readelf -d`, checked before packaging), so a bundled
`libc.so.6` in `usr/lib` is found by the dynamic linker's own search order
ahead of the host's -- while the *interpreter* stays the host's regardless,
fixed at link time in `PT_INTERP` and loaded by the kernel before RUNPATH
exists to consult. A mismatched loader/libc pair is the same failure class
the bundled-loader route below was rejected for, reached by RUNPATH instead
of by deliberately rewriting `PT_INTERP`. The never-bundle list now excludes
the whole glibc/loader/compiler-runtime family (`libc`, `libm`, `libdl`,
`libpthread`, `librt`, `libresolv`, `libutil`, `libnsl`, `libanl`, `libcrypt`,
`ld-linux-x86-64.so.2`, `libstdc++`, `libgcc_s`) alongside the graphics stack:
all of them are present on any host with a working dynamic linker at all,
which the glibc-symbol floor already relies on regardless of whether this fix
bundles them.

## Why the bundled-loader route was rejected, despite working

A different fix was prototyped first: bundle a loader (`ld.so`) and its libc
alongside the AppImage's other libraries, and `exec` Cordial's binaries
through it directly, sidestepping the host's glibc entirely rather than
staying under a floor it might not meet. It worked — printed `Cordial 0.13.2
(323afa2)` on `debian:12-slim`, glibc 2.36, well under any floor a Fedora
build could have claimed.

It is still the wrong answer, for a reason specific to this AppImage rather
than to bundled loaders in general. `PT_INTERP` — the string in an ELF binary
naming which interpreter loads it — is baked into each binary at link time,
not supplied at exec time, so a loader shim placed at the `AppRun` boundary
only ever intercepts the *first* process. `cordial-shell` and `cordial-run`
would launch under the bundled loader, but WebKitGTK execs its own helper
processes — `WebKitWebProcess`, `WebKitNetworkProcess`, `WebKitGPUProcess` —
by absolute path, each carrying its own `PT_INTERP` pointing at the host's
interpreter, with no `AppRun` in that path to intercept them. The result is an
AppImage whose top-level smoke test passes completely — the shell starts, the
window draws — while the web view is silently broken underneath it, on a
class of host this fix was specifically meant to reach. That is a worse
failure than the one being fixed: a check that only exercises the outer
binary would call this AppImage good.

## Decision

Build the AppImage in `ubuntu:24.04`, not `registry.fedoraproject.org/fedora:44`.
Keep both crates' `gtk4`/`libadwaita` Cargo features at `v4_12`/`v1_5` — this
ADR is the reason not to raise them back, since doing so re-narrows the usable
base and can silently push it past the floor again the same way `v4_20` did.
Keep `check-glibc-floor.sh` running over the whole AppDir, not just Cordial's
two binaries, in both the build script and the release workflow, as the
standing gate against a base drifting newer than the floor again — that gate
is what caught this in the first place and is cheap to keep running forever.
Keep the closure-completion step in `build-appimage.sh` for the same reason
on the other defect: linuxdeploy's excludelist is not audited against what
this project actually bundles and has no reason to stay accurate as
dependencies change.

Measured against `Cordial-0.13.2-47-g6ff10ca-x86_64.AppImage`: the glibc gate
passes over 149 bundled ELF files; `--diagnostics` prints `Cordial 0.13.2
(6ff10ca)` and exits 0 on both `ubuntu:24.04` (2.39, exactly the floor) and
`fedora:44` (2.43); the same command on `debian:12-slim` (2.36, the control)
refuses with `version 'GLIBC_2.39' not found` and exits 1. The web view is not
covered by any of those four — see above — and remains the open question.

## What this does not do

**It does not touch the `.deb` job's own base.** That job's comment currently
still says Ubuntu 24.04 fails on the `v4_20` floor; the floor has since moved,
which may make that job's own base choice worth revisiting, but that is a
separate change with its own verification and is out of scope here.

**It does not re-verify the web view end-to-end on this base.** See above —
flagged, not silently carried forward, and not blocking the measurements this
ADR is actually about.
