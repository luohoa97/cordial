# NVIDIA graphics

<!-- description: What Cordial does on NVIDIA GPUs, the known driver problems, and what to try if the game will not start or crashes. -->
<!-- icon: microchip -->

> [!WARNING]
> **Most of this page is untested on NVIDIA hardware.** The first runs on
> NVIDIA were recorded 2026-10-03 (RTX 4070, driver 615.71.09, KDE on Wayland):
> the client boots, renders and survives resize and fullscreen, and nothing
> below has yet been run on a hybrid laptop, on a 535/550 driver, or in a
> Flatpak. What follows is what people running the same Roblox engine (through
> Sober) have reported, what Cordial does about the reports it can act on, and
> what to try. Where something is a guess it says so.

If you have an NVIDIA card, the tester plan in
[`analysis/nvidia-support.md`](analysis/nvidia-support.md) is the most useful
thing you can do for this page.

## What Cordial does on NVIDIA

It decides by the GPU the game is drawing on, not by whether an NVIDIA driver is
loaded, so a laptop that renders on its Intel or AMD chip is left alone
([ADR-046](adr/ADR-046-nvidia-is-gated-on-the-vendor-id.md)).

- **Prints which GPU it uses** at start, for every vendor:
  `[android] vulkan: physical device "..." vendor 0x10de`. `0x10de` is NVIDIA.
- **Warns about driver series 535 and 550.** On Roblox builds from June 2026
  those drivers were reported to crash the game the first time the window is
  resized. Untested here.
- **Asks again if the driver refuses to list the display's present modes**, the
  `vkGetPhysicalDeviceSurfacePresentModesKHR failed` error some two-GPU laptops
  hit on the first launch after boot. A guess: it may not help.
- **Says so, in a Flatpak, if the sandbox's NVIDIA driver does not match your
  machine's**, on the crash page and in `cordial --diagnostics` (the `Graphics`
  line).
- **Checks the same things in `cordial --doctor`** and the report screen: the
  driver series, whether a Flatpak's GL extension matches your driver, and
  whether `nvidia-drm` has `modeset` on ([details](doctor.md#nvidia-lines)).
- **Adds a hint to the crash page** when the game stops after any of the above,
  or after `RBXCRASH: OutOfMemory`, on an NVIDIA GPU.

It does not switch renderer, window system or GPU for you.

## Problems and what to try

| Symptom | Try |
|---|---|
| Game closes as the window first appears or is resized (driver 535 or 550) | Update to driver 580 or newer, or set **Settings, Graphics, Renderer** to **OpenGL ES** and relaunch. OpenGL ES has been shown to reach the landing page, not to be stable in play |
| Game exits at start with `vkGetPhysicalDeviceSurfacePresentModesKHR failed` (two-GPU laptop) | Use the NVIDIA GPU once after boot before launching, see [Laptops with two GPUs](#laptops-with-two-gpus) |
| Window will not appear, or the compositor drops it (Wayland) | `CORDIAL_X11=1` for the game. The cost: the embedded web windows do not attach on X11 |
| No NVIDIA GPU found, or the integrated one is used and it runs slowly (Flatpak) | The sandbox's driver does not match yours, see [Flatpak](#flatpak) |
| `RBXCRASH: OutOfMemoryGraphics` some minutes into a big place | See [Out of graphics memory](#out-of-graphics-memory) |

### Drivers

No minimum driver version has been established, so none is claimed.

- **GTX 10-series and older (Pascal, Maxwell):** 580 is the newest driver they
  can use, so "update to 580" is the ceiling.
- **Wayland:** `nvidia-drm.modeset=1` is needed on drivers before 595, which turns
  it on itself. Explicit sync needs driver 555 or newer, kernel 6.8 or newer and a
  compositor that supports it (KWin 6.1, Mutter 46.1, Hyprland 0.42, Sway 1.11).
- **`__GL_THREADED_OPTIMIZATIONS`, `__GL_YIELD` and `__GL_SYNC_TO_VBLANK`** are for
  OpenGL. They do nothing for this renderer.

## Laptops with two GPUs

If the game exits at start with `vkGetPhysicalDeviceSurfacePresentModesKHR
failed`, several people got past it by using the NVIDIA GPU once after boot before
launching: `vulkaninfo`, or `switcherooctl glxgears`. It is a once-per-boot effect
in their reports.

To make the game use the NVIDIA GPU rather than the Intel or AMD one, people
report that these work. They are not Cordial settings and are untested here:

```bash
__NV_PRIME_RENDER_OFFLOAD=1 __VK_LAYER_NV_optimus=NVIDIA_only cordial-shell
flatpak override --user --env=__NV_PRIME_RENDER_OFFLOAD=1 \
  --env=__VK_LAYER_NV_optimus=NVIDIA_only io.github.luohoa97.Cordial
```

Check the `physical device` line in the output afterwards. If it names the wrong
GPU, nothing else about your setup matters yet.

## Flatpak

The Flatpak carries its own copy of the NVIDIA driver, as an extension named
`org.freedesktop.Platform.GL.nvidia-<version>`, and it has to match the driver on
your machine exactly. When it does not, the sandbox has only the open-source
drivers: Cordial finds no NVIDIA GPU, or quietly uses the integrated one.

```bash
flatpak list | grep GL.nvidia        # what the sandbox has
cat /proc/driver/nvidia/version      # what your machine runs
flatpak update
```

After a driver update on your machine, Flathub can be a day behind; wait, or
install the older version. `cordial --diagnostics` prints the `Graphics` line that
compares the two. There is nothing to do on the AppImage or a native package; they
use your machine's driver directly.

## Out of graphics memory

`RBXCRASH: OutOfMemoryGraphics`, some minutes into a big place. It is reported for
NVIDIA's proprietary driver on every version up to the newest checked. The Sober
maintainers consider it a driver problem and nobody has a fix. Sober's own warning
recommends lowering the graphics quality heavily.

Some people reported success with these in `flags.json`. The names exist in the
engine; the effect is untested here, did nothing for several people, and values
above 2 crashed one:

```json
{ "DFFlagTextureQualityOverrideEnabled": true, "DFIntTextureQualityOverride": 2 }
```

The "video memory: 64 MiB" line in the log is the engine's fixed figure on every
vendor, not a sign your card is misread.

**There is no known way to raise it, and the figure is not what allocation is
decided on.** Measured 2026-10-03 on a 12 GiB RTX 4070: `DFIntEstimatedGmaSafeVideoMemoryMB`,
the one flag name in Roblox's own settings document that speaks of video memory,
moved nothing — the line still read `caps.videoMemory = 67108864` with the flag
set to 8192, in a run where override delivery was proven in the same session
(`DFIntTaskSchedulerTargetFps=30` holding 30.0 presents a second). A relayed
Roblox maintainer statement (Sober #2077) says the figure is hard-coded and not
what allocation is decided on. Do not spend a session on it.

## What Cordial does not do

It does not apply `FStringGraphicsVulkanShaderMTDenyPattern` or patch NVIDIA's
driver library the way Sober does for one problem. The reasons are in
[ADR-046](adr/ADR-046-nvidia-is-gated-on-the-vendor-id.md) and
[ADR-001](adr/ADR-001-in-process-hooking.md). The flag works from `flags.json` if
you want to test it; the test is in the analysis page.

## Reporting a problem

Use **Report a Problem** in the main menu, or run `cordial --diagnostics` and paste
the block, including the `Graphics` line. Add:

1. The `physical device` line from the output when you launch from a terminal.
2. `vulkaninfo --summary`, `nvidia-smi` and `cat /proc/driver/nvidia/version`.
3. For a crash, the last twenty lines of output. For a freeze, do not kill it yet;
   see [`analysis/nvidia-support.md`](analysis/nvidia-support.md).

The catalogue of known NVIDIA failures, with issue numbers, evidence and the test
plan, is [`analysis/nvidia-support.md`](analysis/nvidia-support.md).
