# Cordial Z

A Roblox client for Linux, built on [Cordial](https://github.com/luohoa97/cordial) by
luohoa97. MIT licensed, same as its parent.

**This is agent-assisted software.** Large parts of it were written, reviewed and
diagnosed by language models working against a codebase that documents how to
work on it. The upstream project warns that agents get this engine wrong more
easily than almost anything else, so treat the claims below as claims and check
them yourself. The bugs are real too; some were found by the model, and some
were introduced by it. That is disclosed rather than hidden, because a project
that hides its construction method cannot be reviewed.

## What this is for

Upstream Cordial has no NVIDIA support document. `docs/analysis/nvidia-support.md`
exists, but Roblox does not test its Android renderer against NVIDIA drivers, so
nobody is on the hook for it. Every user with an NVIDIA card is on their own.

This fork exists to take that on: measure what actually happens on NVIDIA and
Wayland, fix what is in our control to fix, and write down what is not.

## Status

Builds and runs. `cordial-shell` and `cordial-run` are both built from this
tree and have been run against a live Roblox session; `cordial-z` is the
launch script that pins the backend.

The backend is GL ES over Mesa's Zink, and it is the only one this fork
supports by design — hence the name. Vulkan on NVIDIA never worked here
(Roblox does not test its Android renderer against NVIDIA, and upstream has no
NVIDIA support), so the fork does not compete on that path. Under Zink:

  * ETC2/EAC textures stop being decoded on the CPU. A Vulkan session ran
    50,411 emulated decodes; under Zink the count is 0, because Mesa's texture
    path replaces Roblox's.
  * VRAM sits flat at ~1753 MiB instead of climbing past 2.3 GiB.
  * Frame delivery is even, which is what Sell Lemons needed.

All three env vars in `cordial-z` are required. `CORDIAL_NO_VULKAN=1` makes the
engine choose GL ES; `MESA_LOADER_DRIVER_OVERRIDE=zink` routes that through
Zink; `VK_ICD_FILENAMES` pins the NVIDIA ICD, without which Zink picks the
integrated Radeon 680M and everything crawls.

Known from upstream, carried over unchanged:

- Two presentation stalls in one session are possible; upstream's detector
  reports only the first, because it latches on first detection and has no reset
  path. Two stalls were observed ~3.5 minutes apart and only one was printed.
- A 51-second window was observed in which keyboard input was dropped after a
  Wayland focus change.
- VRAM reaches ~2.7 GB mid-session and is fully reclaimed on exit. This is a
  high-water mark from place loading, not a leak.

## Rules this fork works by

Taken from upstream's `AGENTS.md`, which is the most useful thing in the
repository:

- **Do not use present counts as a frame rate.** `vkQueuePresentKHR` counts over a
  wall-clock window measure an idle throttle, not fps. Presents sit at about 60/s
  for several seconds and fall to exactly 1.0/s with no input, identically on
  X11 and Wayland. A real frame rate requires driving input for the whole
  measurement and reporting the input rate beside it.
- **Do not measure timing under `WAYLAND_DEBUG=1`.** It changes what it measures.
- **Say which build you are talking about.** The title carries version and
  commit; `-dirty` means uncommitted changes.
- **A backtrace is not an answer without the process CPU beside it.** A spinning
  pump and a blocked one produce identical stacks.
- **A one-frame backtrace is not an answer.**
- **Verify by running.** `INFERRED` is an acceptable label; presenting an
  inference as established is not.

## Building

Needs `clang` (AOSP bionic does not build with GCC), `libboost-dev`,
`libadwaita-1-dev`, and `PKG_CONFIG_PATH` including the system pkgconfig
directory.

```bash
export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig
export CC=clang CXX=clang++
git submodule update --init --recursive   # and check third_party/*/externals too
cargo build --release --bin cordial-run
```

The nested submodules matter: `mcpelauncher-linker` carries `bionic` and `core`,
and `dynarmic` carries its own `externals`. A partial checkout fails with a CMake
error naming a source file rather than a missing submodule, which is a
confusing way to learn this.

## Running

Needs an APK the user supplies. Cordial ships none. Upstream names the location it
expects on their machine; yours will differ.

```bash
./target/release/cordial-run --profile <yours>
```

Give each run its own data root. A profile is held by one instance at a time by
`flock`, and a second client against the same profile is refused.

## Licence

MIT, same as Cordial. Bundles Apache-2.0 bionic from AOSP and other components
with their own licences; see upstream's `NOTICE`.