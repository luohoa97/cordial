# ADR-046: NVIDIA behaviour is gated on the device's vendor id, and says what is inferred

**Status:** accepted
**Supersedes:** nothing
**Related:** [ADR-001](ADR-001-in-process-hooking.md), [ADR-011](ADR-011-wayland-and-libadwaita.md), [ADR-017](ADR-017-sober-issue-corpus.md), [ADR-024](ADR-024-x11-is-supported-again.md), [ADR-042](ADR-042-texture-format-query-observability.md)

## Decision

Anything Cordial does only because a GPU is NVIDIA's is gated on the physical
device's `VkPhysicalDeviceProperties.vendorID` being `0x10DE`, read in
`crates/cordial-runtime/src/android/vulkan.rs` and decided by
`crates/cordial-shell/src/nvidia.rs`. It is never gated on "an NVIDIA kernel
module is loaded": a hybrid laptop has the module loaded and often renders on
the other GPU, and a workaround applied there is applied to the wrong hardware.
The one place the module is read is where the question is about the module, the
Flatpak's matching userspace driver.

What is built on that gate:

- an identity line for the device the engine builds its logical device on,
  printed for every vendor, because every GPU report needs it and the launcher's
  crash page reads it to decide whether to mention NVIDIA at all;
- an **advisory**, not a refusal, for driver series 535 and 550;
- a bounded retry of `vkGetPhysicalDeviceSurfacePresentModesKHR` when it fails
  with `VK_ERROR_UNKNOWN`, `INITIALIZATION_FAILED` or `SURFACE_LOST` on an NVIDIA
  device;
- a `Graphics` row in `cordial --diagnostics` and, in a Flatpak, a sentence when
  the sandbox lacks the `GL.nvidia` extension the host driver needs;
- hints on the crash page;
- three lines in `cordial --doctor` (`doctor::nvidia_checks`): the driver series,
  the Flatpak extension against the host module, and `nvidia-drm` modeset, each
  run only for a Vulkan device whose vendor id is `0x10DE`.

`CORDIAL_FORCE_GPU_VENDOR` and `CORDIAL_TEST_FAIL_PRESENT_MODES` exist so that
path can be run on a machine without NVIDIA hardware. They change what Cordial
decides and never what the engine is told, are named in `cordial-run --help`,
and are set by no packaging script, which is the same shape as
`CORDIAL_MASK_MOBILE_TEXTURE_FORMATS` in ADR-042.

## What was decided not to do

- **No renderer, window-system or GPU switch on NVIDIA's behalf** -- for the
  engine. GTK's own renderer is the exception since 2026-11-17: on NVIDIA's
  proprietary driver the window is drawn with cairo, because the client was
  crashing inside the driver from GTK's Vulkan frame rather than the engine's
  ([ADR-047](ADR-047-the-canvas-is-lowered-only-under-a-presented-frame.md)).
  It is still decided before the engine picks a device, but nothing the
  engine sees changes, and a wrong guess on a two-GPU laptop costs a title bar
  drawn in software. Defaulting
  NVIDIA-on-Wayland to X11 (mocktail's choice) loses the embedded web windows,
  which the X11 backend cannot attach, reverses the primary backend of ADR-011
  on one forum report and one comment, and is decided before the device is known.
  Defaulting 535 and 550 to OpenGL ES has the same timing problem and GLES is not
  known to be stable. Both are stated to the user with their cost instead.
- **`FStringGraphicsVulkanShaderMTDenyPattern` is not applied.** TASKS.md T2
  proposed it on the strength of mocktail shipping `"4318:.*"`. Reading mocktail
  for its reason found a workaround for its own libc bridge, which merely used
  NVIDIA's vendor id as a switch; nothing in Sober's tracker involves it. The
  rule in `flags.rs` (`BUILTIN`) applies unchanged: an inferred change belongs
  behind a switch somebody chooses, not in a default. It remains available in
  `flags.json`, and the tester plan measures it.
- **No patching of NVIDIA's library**, which Sober does for one VRAM problem
  (`patch_libnvidiaglcore_overzealous_vram_caching`). ADR-001 is unchanged.
- **No `__GL_*` variables.** They are documented for OpenGL only.
- **No minimum driver version in user documentation.** The evidence supports two
  series with reports and the fact that Pascal stops at 580, and nothing else.

## Why a message is acceptable where a fix is not

An advisory that names its evidence costs a user nothing when it is wrong and
saves an evening when it is right. A retry costs under two seconds on a path that
was ending in a fatal error already and does not run on success. Neither can make
a working install worse. A default switch of renderer or window system can, and
for everybody, on evidence this project cannot test.

## Consequences

- Every NVIDIA behaviour is `INFERRED` until somebody with the hardware runs the
  plan in [`docs/analysis/nvidia-support.md`](../analysis/nvidia-support.md), and
  its comment says so. The claim "NVIDIA is supported" is not made anywhere;
  `docs/nvidia.md` opens by saying how little has been run on NVIDIA (two users'
  reports by 2026-10-04).
- The crash page can speak about NVIDIA only when the client's own output shows an
  NVIDIA device, so an unrelated crash on an Intel machine never mentions it.
- If a run on real hardware shows the retry does nothing, delete it, and this ADR
  is amended rather than left describing a behaviour that is not there.
