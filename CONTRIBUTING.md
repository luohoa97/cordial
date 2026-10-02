# Contributing to Cordial

Cordial is a runtime for a 116 MB stripped binary nobody here has the source to.
That shapes everything about how work gets done on it, and this document is
mostly about that method rather than about code style.

Read [`docs/NEXT.md`](docs/NEXT.md) first. It is written for someone picking the
project up cold and says what is blocking, what has been tried, and — the part
that matters most — what has been **disproved**.

## The one rule

**Grep the trace before disassembling anything.**

`docs/traces/` holds a logcat capture of the same Roblox APK running on real
Android. When a question comes up about what the engine expects, that capture is
a lookup, not an investigation.

This is not a stylistic preference. Over one long session, **nine consecutive
conclusions drawn from reading the stripped binary were wrong**, and every
conclusion drawn from running something held up. The capture exists so that
never has to happen again.

## Verify by running

A claim about this engine is worth what it was measured with.

- If you cannot test a claim, **label it `INFERRED`** and say so, in the code
  comment and in the pull request. That is a perfectly acceptable state for a
  finding to be in. Presenting it as established is not.
- Timing and stability claims need **repetition**. "It works now" after one run
  is not a result — one bug in this project's history reproduced on roughly one
  launch in three, and its rate moved with machine load.
- Use a **control**. The flag override mechanism was only confirmed by showing a
  log line disappears with the flag set and is present without it, in the same
  session. A change that appears to work is not the same as a change that works.

## Record what you disproved

Half of `docs/NEXT.md` is a list of explanations that turned out to be wrong.
That is deliberate and it is the highest-value thing you can contribute.

When you rule something out, write it down with the evidence. It stops the next
person spending a day on it, and it is why several sections of this repository
read like a lab notebook. Commit messages here are long for the same reason —
they record what was measured, not just what changed.

If you find that something already written down is wrong, **say so plainly and
correct it**. Several commits in this history exist only to retract an earlier
claim. That is a healthy thing for a project like this, not an embarrassment.

## Reading the engine

Roblox narrates itself. The single best diagnostic in the project is the
engine's own log:

```
<files>/appData/logs/<version>_<timestamp>_Player_*.log
```

It names subsystems, stages, file paths and exceptions in Roblox's own words.
Read the newest one before forming a theory. Most questions are answered there
and nobody finds it on their own, which is why it is mentioned three times in
this repository.

Useful switches:

| | |
|---|---|
| `CORDIAL_ANDROID_TRACE=1` | every Android API call Cordial serves |
| `CORDIAL_TRACE_PATHS=1` | every path-taking libc call, with thread id |
| `CORDIAL_COUNT_GL=1` | graphics call counts on exit |
| `CORDIAL_MONITOR=<n>` | open the window on another monitor |

`CORDIAL_TRACE=1` is **ABI-unsafe** — it wraps variadic functions with
fixed-arity declarations and makes the engine abort. It cannot answer "which
path?" questions; `CORDIAL_TRACE_PATHS=1` can.

## Debugging facts that cost real time to learn

- **lldb breakpoints inside `libroblox.so` do not work, and fail silently.**
  Cordial `mmap`s it outside the system linker, so lldb never lists the image and
  every breakpoint stays unresolved with hit count 0. The working technique is
  `memory write` of `0xCC`, then rewinding `$pc` and restoring the byte on trap.
  Crash-stop backtraces and breakpoints in Cordial's own code are unaffected.
- **Read syscall arguments from `/proc/<pid>/task/<tid>/syscall`** while lldb has
  the process stopped, rather than from registers. It gives the number and all
  six arguments with no guesswork about the libc wrapper's register shuffling.
- **There are three threads named `Main`.** Use `thread backtrace all`.
- lldb is at `/home/linuxbrew/.linuxbrew/bin/lldb`. There is no gdb and no
  strace.

## Things that are permanently out of scope

**No in-process code execution against the Roblox process.** No hooking, no
memory patching, no injected script environment, and no API by which a plugin
could request one. Not disabled — *absent*, so there is no primitive in the
binary to extract or re-enable in a fork. See
[ADR-001](docs/adr/ADR-001-in-process-hooking.md).

**We do not endorse exploiting.** Pull requests adding an executor, or anything
of that shape, will be declined.

Asset overlays used to be on this list, on the reasoning that replacing a texture
is the mechanism behind wallhacks. That reasoning did not survive checking — a
Roblox part is geometry with a `BasePart` colour and material, so a transparent
material texture gives a differently shaded surface rather than a see-through
one, and both Sober and Bloxstrap ship exactly this feature in the open. They are
now supported, non-destructively and off by default: see
[ADR-010](docs/adr/ADR-010-plugin-asset-overlays.md), which supersedes
[ADR-004](docs/adr/ADR-004-plugin-asset-overrides.md). What remains refused is
in-process injection, which is a different primitive.

Also out: client-side integrity flags or watermarks, and
obfuscation-as-security.

## The licence is settled

Cordial is GPL-3.0-or-later and stays that way. Requests to relicense — to MIT,
to Apache-2.0, to dual-licence, to carve out an exception for one downstream —
are declined, and the issue is closed with a link to this section rather than
argued out.

That is a rule, not a verdict on whoever asked. The argument has no new form
left: it has been had, the answer has not moved, and each fresh round costs an
evening that would otherwise go on the client. A maintainer closing one of these
is following what is written here, and is not obliged to relitigate it in the
thread.

Two nearby things are **not** covered by this and are genuinely welcome, because
they are different questions rather than the same one wearing a hat:

- Whether a third-party component's obligations are actually being met.
  [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) has to be accurate, and a
  gap there is a bug worth an issue.
- Whether some specific dependency you want to add is compatible with
  GPL-3.0-or-later in the first place. Ask before you write the code, not after.

## No Roblox code, ever

Cordial ships no Roblox code, APK, asset or decompiled material, and never will.
Do not commit any, do not vendor any, and do not paste decompiler output into an
issue or a comment.

Observing a running binary is fine and is how nearly everything here was
established — call order, load order, argument shapes, syscalls, timing.
Transcribing a decompilation of how it implements something is not. The line is
not the tool, it is what you take away.

## Do not test with your main account

Use a throwaway account, and put it on a different IP from the one your real
account uses — a VPN is the easy way.

This is not because Cordial does anything bannable. It runs the official build,
does not touch the engine's process, and asset overlays are the same thing
Bloxstrap and Sober already do in the open. The risk is collateral, not causal:
enforcement at this scale is automated, it runs in waves, and accounts that share
an address get associated with each other. If a test account is ever caught in a
wave — for any reason, including one that has nothing to do with Cordial — you do
not want the account you actually care about sitting next to it.

Cordial cannot make that decision for you and does not try to hide anything from
anyone. Testing pre-release software that loads a game client is simply not
something to do on an account you would be upset to lose.

The same goes for reporting: if you hit an account problem while testing, say so
in the issue. A ban that turns out to be Cordial's fault is the single most
important bug this project could have, and it is only findable if people mention
it.

## Practical

Besides Clang, `crates/cordial-shell` (the core shell — window, chooser,
minimal settings; see [ADR-002](docs/adr/ADR-002-core-shell-and-ui-handoff.md)
and [ADR-011](docs/adr/ADR-011-wayland-and-libadwaita.md)) needs GTK4 ≥ 4.10
and libadwaita ≥ 1.4 development headers on `PKG_CONFIG_PATH`, because
`gtk4-sys`/`libadwaita-sys` link against the system libraries rather than
vendoring them:

```bash
sudo dnf install gtk4-devel libadwaita-devel      # Fedora
sudo apt install libgtk-4-dev libadwaita-1-dev    # Debian/Ubuntu
sudo pacman -S gtk4 libadwaita                    # Arch
```

On x86-64 the VR mode's translator, dynarmic (`third_party/dynarmic`, a
submodule with submodules of its own, which `--recursive` fetches), also needs
Boost's headers: `boost-devel`, `libboost-dev` or `boost`. `cargo test` builds a
small arm64 test image for `crates/cordial-guest` and needs `lld` and `llvm`
for it; without them the tests that run it are ignored, and a build warning
says why. The build compiles the loader and dynarmic with `patches/0005`--`0008`
applied, in a copy under `target/`, and leaves the submodules untouched
([`patches/README.md`](patches/README.md)).

```bash
git clone --recursive https://github.com/luohoa97/cordial
cd cordial
cargo build --release      # Clang required; AOSP bionic does not build with GCC
cargo test --release
```

### If you cannot build or test locally

**Send the patch anyway.** A contribution that says "I could not run the tests,
here is why" is welcome and will be reviewed. It is not a lesser contribution and
you do not need to apologise for it — that assumption has already cost this
project one perfectly good patch from someone who made it.

This matters more here than in most repositories. The build wants Clang, GTK4,
libadwaita and, for the web view, `webkitgtk6.0-devel`, and an immutable or
sandboxed host will not give you those without layering packages and rebooting.
If that is your situation, say so in the pull request and a maintainer will run
the suite for you.

What is asked instead is the thing this project actually cares about: **be
explicit about what you did and did not verify.** "Builds here, tests not run,
reasoning checked against the dex" is a good pull request. "Should work" is not,
and that is true whether or not you could run anything.

The recommended way to get a complete environment, if you can, is the container
this project builds in — it pins Fedora 44 and installs the pieces most hosts
lack:

```bash
just build toolbox     # the recipe's comment has how to create the container
```

`just build host` works too if you have the headers listed above. Neither is
required to open a pull request.

Install `pipewire-devel` (`libpipewire-0.3-dev` on Debian/Ubuntu) before
building if you want to work on OpenSL ES audio — `native/CMakeLists.txt`
detects it with `pkg-config` at configure time and prints which way it went.
Without it the build still succeeds; `slCreateEngine` just reports failure, as
it did before audio existed here. `webkitgtk6.0-devel` is the same shape for the
web views.

**That optionality is a trap worth naming.** Those two probes mean the tree
compiles either way and quietly produces a different Cordial, so two people on
the same commit can measure different binaries and neither can tell. In a
project whose method is "verify by running", that costs more than a missing
feature.

### Or use the flake

```bash
nix develop      # or `direnv allow`, using the .envrc
just check
```

`flake.nix` pins Clang, Rust, GTK4, libadwaita **and** both optional
dependencies, so everyone builds the same thing. It prints the version of each
on entry. It builds Cordial and nothing else: you still run the client on your
own host with `just dev`, because the engine's behaviour depends on the real
graphics stack, compositor and glibc — `--host-libc` makes that dependence
explicit — and a hermetic runtime would be measuring something nobody ships.
Users install the Flatpak; this is a contributor's shell.

The per-distro lists above stay first-class. Most contributors do not have Nix
and should not need it.

**What has been run, as of 2026-10-01.** `packages.default` was built from this
tree on the developer's Fedora Atomic host and the result inspected, but not
launched into a game. `cordial --help` and `cordial --diagnostics` ran from the
built output, `ldd` on both binaries reports nothing `not found`, and
`readelf -d` shows `cordial-run` linked WebKitGTK. The PipeWire, ALSA and
PulseAudio backends are compiled in. `cargo test` is not run by the Nix build.
Nobody has run the `devShells.default` shell end to end, and the remote form,
`nix run "github:luohoa97/cordial?submodules=1"`, is `INFERRED` from the local
build and has not been tried against a pushed commit.

Two things the first build found. `eachDefaultSystem` evaluates x86_64-darwin,
which current nixpkgs refuses to evaluate, so the flake lists Linux systems
only. And `cargoBuildFeatures` is overwritten by nixpkgs' `buildFeatures`, so
the first package built cleanly with no web view; the flake now sets
`buildFeatures` and fails the build if `cordial-run` links no WebKitGTK.

**The submodules need `?submodules=1`.** Flakes do not fetch them otherwise;
from a checkout use `nix build '.?submodules=1#cordial'`. The package installs
the same files as the AUR package, puts `deno` on the wrapper's `PATH`, and
reports `Install unknown` in `cordial --diagnostics`, because nothing there
recognises a `/nix/store` path yet.

On an ostree host `nix` has no writable store to start with, and a bare
`nix develop` fails with

```text
error: creating directory "/nix/store/.links": Read-only file system
```

`/nix` is inside the read-only composefs image and `nix-daemon` is inactive. The
Determinate Systems installer handles ostree, or a systemd mount unit can bind a
writable directory over `/nix`. **Neither is needed to build**: a store under
your home directory works with no change to the host, and is how the build above
was done:

```bash
nix --store "local?root=$HOME/.cache/cordial-nix" build '.?submodules=1#cordial' -L
```

The binaries it produces point at `/nix/store`, which only exists inside that
store's namespace, so run them with `nix --store ... shell <store path>` rather
than directly.

`webkitgtk6.0-devel` (`libwebkitgtk-6.0-dev` on Debian/Ubuntu) will be needed by
whoever picks up the web view — Marketplace, Profile, Communities and most
link-opening are web content, and none of it works today. Nothing in the tree
requires it yet, so its absence breaks nothing; note that the *runtime* library
often ships already while the development package does not, which makes
`pkg-config --modversion webkitgtk-6.0` the check that matters rather than
`ldconfig -p`. `docs/analysis/webview-surface.md` maps what would sit on top of
it.

Before opening a pull request:

- `cargo test --release` passes
- `cargo build --release` is warning-clean for code you touched
- the client still launches — repeatedly, not once
- your commit message says what you **measured**, not just what you changed

Licensed GPL-3.0-or-later. By contributing you agree your work ships under it.
Third-party notices live in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and must be kept accurate;
MIT and Apache-2.0 obligations are satisfied only while those notices travel
with the build.

## A note on how this was built

Most of this repository was written by Claude (Anthropic) working with a human
directing the architecture. That is disclosed in the README and it is relevant
to you as a contributor: the code is real and the findings were verified by
running things, but **no human has reviewed every line**. Review accordingly, and
if you find something wrong, the project would rather hear it than not.
