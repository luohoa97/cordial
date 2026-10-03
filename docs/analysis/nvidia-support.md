# NVIDIA support: what is known, what Cordial does, and what needs hardware

**No NVIDIA GPU exists on the development machine (Intel only), so "fully
supported" cannot be claimed and nothing below has been run against an NVIDIA
driver.** This file is the record that makes that sentence checkable: a
catalogue of every NVIDIA-specific failure the engine is known to have on Linux,
the fixes that were applied and the evidence each rests on, the ones that were
left out and why, and a test plan for somebody who has the hardware.
[`docs/nvidia.md`](../nvidia.md) is the short version for users.
[ADR-046](../adr/ADR-046-nvidia-is-gated-on-the-vendor-id.md) is the rule the
code follows.

Sources, and how far each can be trusted:

- **Sober's issue tracker**, read from `tools/sober-corpus/data/raw.jsonl`
  (2,194 issues, 2024-08 to 2026-08; [ADR-017](../adr/ADR-017-sober-issue-corpus.md)).
  Sober runs the same engine. It closes issues by inactivity, so `CLOSED` is not
  evidence of a fix; a fix is only counted where a comment says so. Author names
  are stripped, so the only trust signal is `authorAssociation`. Cited as
  "Sober #N".
- **Cordial's own tracker and notes**: `gh issue list --state all` (51 issues)
  has nothing about NVIDIA, 550, device loss or explicit sync;
  [`docs/NEXT.md`](../NEXT.md) has nothing NVIDIA-specific; the only NVIDIA item
  is TASKS.md T2 and [`nvidia-texture-manager2.md`](nvidia-texture-manager2.md).
- **mocktail** (`~/Projects/mocktail`, Apache-2.0), read for ideas and never
  copied.
- **Web sources**, labelled DOC (NVIDIA README or release notes, Khronos,
  protocol registry), SEC (press summarising a primary source), REPORT (one
  user) or LORE (community belief, no primary found). The Mesa issue that
  Sober's maintainers point at (mesa/mesa#12316) and NVIDIA's own Pascal
  support-plan page could not be fetched, so nothing here depends on either.

## Reports from NVIDIA users of Cordial itself

- 2026-10-01, on [#39](https://github.com/luohoa97/cordial/issues/39): Flatpak
  0.22.0, Debian forky, GNOME on Wayland, NVIDIA's own 610.57 driver, default
  settings. Entering and leaving fullscreen (F11), maximise, snap and restore,
  in the menus and in a game, all without a crash; the same user crashed on
  entering fullscreen with earlier builds. The first report of Cordial on an
  NVIDIA driver outside the 535/550 series, and it is clean for this path.
- 2026-10-03, an RTX 4070 (single GPU, EndeavourOS, KDE Plasma 6.7.5 on Wayland,
  the open kernel module 615.71.09, built from `main` at 0.23.2): tester-plan
  steps 0-4 run, plus the frame-rate arms. `APP_READY Landing` reached with a
  swapchain capture; F11 to 2560x1440 and back recreated the swapchain twice
  without a crash, so the 535/550 resize class does not reproduce on this
  driver. The driver offers `MAILBOX, unknown, FIFO, IMMEDIATE` on this session
  and MAILBOX is picked by default. With input driven throughout: 59.9
  presents/s at 232 moves/s on the 240.001 Hz output with the engine told the
  rate, `DFIntTaskSchedulerTargetFps=240` reaching 216-232/s (p50 4.2 ms) and
  `=30` holding 29.8/s, so the flat 60 is the engine's own target rather than
  a display lock. The signed-in startup freeze reproduced (1 of 1 runs
  restoring a signed-in session at start, 0 of 3 signing in later): presents
  frozen at 7 while the dev control socket still accepted 892 pointer moves.
  The core, taken about 50 minutes later, has the main thread parked in
  `pthread_cond_wait` inside `libroblox.so` from `looper::teardown`'s lifecycle
  sequence -- the issue #52 shape -- and no `teardown-watchdog` thread among
  64 stacks, so the 10 s bound was not in force; `teardown` now reports a
  watchdog spawn failure instead of answering `.ok()` (whether the spawn
  failed is INFERRED; the absent thread is what the core shows). One run of
  eight died with SIGSEGV on the main thread just after `APP_READY Startup`,
  not reproduced since.

## What the data contradicts

Worth putting first, because each was in the brief that started this work.

- **`caps.videoMemory = 67108864` (64 MiB) is not an NVIDIA fault.** It is in
  every Vulkan log regardless of vendor: 76 log blocks with vendor `10de`, 39
  with Intel, 33 with AMD, and an AMD RX 7900 GRE and a Radeon HD 7400M
  reproduce it (Sober #2341). A relayed maintainer statement in #2077 says the
  figure is hard-coded and not what allocation is decided on. It is a red
  herring for #2190, #2341 and #2374. Confirmed again on 2026-10-03 on a 12 GiB
  RTX 4070, where the same line reports `device memory = 12878610432` and still
  sets `caps.videoMemory = 67108864`, and `DFIntEstimatedGmaSafeVideoMemoryMB`
  moved nothing with override delivery proven in the same run
  (`DFIntTaskSchedulerTargetFps=30` holding 30.0 presents/s).
- **The `heapSize` figure in the same block is u32-truncated on every platform,
  not by Cordial's shim.** On the RTX 4070 it reads 4288675840 for a
  12878610432-byte heap (exactly the low 32 bits), and the Android trace
  (`waydroid-roblox-startup.log.gz`) shows 3758365696 for a 12348300288-byte
  heap -- the same truncation on real Android. It is the engine's own print.
- **`VK_ERROR_DEVICE_LOST` is not NVIDIA-specific.** Sober #1880 is an AMD RX
  7800 XT, #184 and #1298 are Intel, #1725 also has AMD and Steam Deck reports.
  It is NVIDIA-attributable only where an Xid line or `libnvidia-glcore` frames
  are shown (#2, #31, #244, #250, #512).
- **The `Driver A.B.C` in the engine's log is not the driver version.** It is
  NVIDIA's packing (10 bits major, 8 minor, 8 patch, 6 tail) read with the
  standard Vulkan 10/10/12 ruler, so `550.652.64` is really `550.163.01` and
  `535.212.64` is `535.309.01`. Five reporter-stated pairs check the formula. It
  also explains a "mismatch" a reporter noticed in #2180 that was not one.
- **`patch_libnvidiaglcore_overzealous_vram_caching` is fleet-wide.** It sits in
  Sober's remote manifest from 2026-02-21 to 2026-05-14 and appears in Intel,
  AMD, Crostini and no-display logs, so it says nothing about the reporter's
  GPU. (It also names an in-process patch of NVIDIA's library. Cordial will not
  do that: [ADR-001](../adr/ADR-001-in-process-hooking.md).)
- **The texture-manager premise did not hold.**
  [`nvidia-texture-manager2.md`](nvidia-texture-manager2.md) and
  [ADR-042](../adr/ADR-042-texture-format-query-observability.md): no issue ties
  an NVIDIA GPU to a texture-quality complaint, and nothing was translated.
- **The Sober fix for explicit-sync errors was Sober's.** Cause G below was a
  quirk in SDL's Wayland video driver that Sober fixed; Cordial does not use it.

## Catalogue

Sorted by how much evidence there is that a launcher can do anything. "Cordial"
says what this repository does about it now.

### 1. Driver 535/550 crash at the first render-target resize (CONFIRMED, fix is the driver)

- **Issues:** Sober #2180 (label `nvidia`, open), #2162, #2181, #2182, #2189,
  #2192, #2201, #2206, #2338 (label `nvidia`), #2327, #2351. June to July 2026.
- **Symptom:** the log ends at `[FLog::Graphics] SceneManager: resizing main
  targets to WxH`, exit 139, or `FATAL: Unrecognized Instruction` (a `ud2`) on
  later builds. GTX 1050 to 1080, GT 1030, RTX 3060/3060 Ti/3070, an RTX 4070
  laptop; drivers 535.183 to 535.309 and 550.163.01; mostly X11.
- **What was said:** a MEMBER: "a new incompatibility between this specific
  NVIDIA driver release and Roblox ... not a fix other than by changing driver
  versions"; another: "Nvidia 550 has bugs that prevent Roblox from running";
  "NVIDIA 535 bug unfortunately". GTX 1070 and 1070 Ti reporters went 550 or 535
  to 580 and it ran. OpenGL avoided it.
- **Not established:** that every 535/550 point release does it. 550.107.02 and
  550.120 were reported working on earlier Roblox builds, so the trigger is the
  engine build as much as the driver. That Cordial hits it too. Nobody has run
  Cordial on a 535 or 550.
- **Cordial:** a one-line advisory at device creation naming the evidence
  (`driver_advisory` in `crates/cordial-shell/src/nvidia.rs`), a hint on the
  crash page, and a pointer to Settings, Graphics, Renderer, OpenGL ES. It does
  **not** switch renderer by itself: the choice is made before any Vulkan device
  exists, so the gate this work uses (the device's vendor id) is not readable in
  time, and the GLES path is not known to be stable
  ([`crates/cordial-runtime/src/graphics.rs`](../../crates/cordial-runtime/src/graphics.rs)).
  `INFERRED` that the advisory applies here.
- **Pascal:** the newest driver a GTX 10-series can run is the 580 series
  (NVIDIA announced 580 as the last feature branch for Maxwell, Pascal and
  Volta on 2025-07-01, SEC; Arch's package news says 590 dropped them on Linux).
  Those users cannot follow "update to 580 or newer" past 580. Sober's own
  runtime also warns on pre-Turing cards about VRAM (cause 4).

### 2. Hybrid laptops: `vkGetPhysicalDeviceSurfacePresentModesKHR failed` (workaround CONFIRMED, cause INFERRED)

- **Issues:** Sober #95, #171, #256, #289, #315, #332, #699 (feature request,
  open), #1040, #1041, #1101, #1233, #1332, #1955, #1966; related #286, #2083
  where the dGPU was never used. RTX 3050/3060/4060/4070/5060 laptops and GTX
  1050 MaxQ/1650/950M/970M; drivers 560.35.03, 575.64, 580.82.09, 580.142.
- **Symptom:** the engine's own log line, at `Vulkan: creating framebuffer`
  (#171, #289): `[FLog::Graphics] VULKAN ERROR:
  vkGetPhysicalDeviceSurfacePresentModesKHR(physicalDevice,
  wd.presentationSurface, &modeCount, nullptr) returned -13`. So the call is the
  engine's, and Cordial's shim sees it. Later reports say `VK_ERROR_UNKNOWN`, sometimes `vkCreateSwapchainKHR` returning
  `-3` or `SURFACE_LOST` on the neighbouring call. Nearly always Wayland (KDE,
  Hyprland); older reports say X11 was fine.
- **What helped:** using the GPU once first: `vulkaninfo` (#1041, #1101), opening
  Steam (#1101), `vkcube` (#699, #1942), `switcherooctl glxgears` ("you only
  have to run something on your GPU once per boot", #1041). Four independent
  reporters. A one-off "enable OpenGL, launch, disable it" toggle (#1332).
  Forcing the dGPU with `__NV_PRIME_RENDER_OFFLOAD=1` and friends is reported in
  #1332, #1955 and #1966, but #1101 says it crashed anyway under `DRI_PRIME=1`.
  XWayland, which a MEMBER suggested repeatedly, avoids it at a cost in
  fullscreen and frame rate. A driver update fixed it in #171 and #2118.
- **Maintainer position:** a MEMBER on #1332 blamed Mesa and linked
  mesa/mesa#12316, closed as "not our bug". A CONTRIBUTOR on #699 said they had
  little experience with how PRIME works; it is still open. **The mechanism was
  never established by anyone.** That the dGPU is still waking is the obvious
  candidate and no source states it.
- **Not NVIDIA-only:** the same error string appears on Intel in a virtio VM
  (#579), ChromeOS (#1276) and an Intel Arrow Lake iGPU (#1389). So the string
  alone proves nothing; the reproducible subset is hybrid laptop plus Wayland.
- **Cordial:** when the physical device is NVIDIA's and the call fails with
  `VK_ERROR_UNKNOWN`, `VK_ERROR_INITIALIZATION_FAILED` or
  `VK_ERROR_SURFACE_LOST_KHR`, ask again after 100, 250, 500 and 1000 ms
  (`vk_get_physical_device_surface_present_modes_khr` in
  `crates/cordial-runtime/src/android/vulkan.rs`, `nvidia::retry`). It never
  runs when the call succeeds, and the worst case is 1.85 seconds spent on a
  path that ends in a fatal error anyway. **`INFERRED` that it helps.** A retry
  in the same process is a guess about what "run something first" does; nothing
  else in the record is a stronger mitigation Cordial controls.
  Not done: initialising or choosing the NVIDIA device ahead of the surface, or
  falling back to the iGPU or X11 (mocktail does the last, see below). All three
  change which GPU renders on evidence that is one reporter each.

### 3. Flatpak: the `GL.nvidia` extension does not match the host driver (CONFIRMED)

- **Issues:** Sober #154, #157, #234, #274, #284, #286, #289, #343, #521, #527,
  #880, #1013, #1127, #1128, #1267, #1307, #1325, #1388, #1517, #1621, #1685,
  #2156. Symptoms: "couldn't find a supported graphics device", `Couldn't find
  matching GLX visual`, "Installed Vulkan doesn't implement VK_KHR_surface", or
  silently rendering on the iGPU at low frame rate.
- **What fixed it, repeatedly:** `flatpak update` so
  `org.freedesktop.Platform.GL.nvidia-<version>` (and `GL32`) match the host.
  CONTRIBUTORs: "both nvidia system and flatpak drivers need to be in sync"
  (#880), "versions installed have to match" (#343). #1685 and #157 name Flathub
  publishing lag after a distribution moved to a new driver ("fixed within the
  next 24 hours"). #2156 lost half its frame rate to the iGPU until the
  extension was installed.
- **Documented, not just reported (DOC):** Flatpak reads the kernel module
  version from `/sys/module/nvidia/version` and picks that extension; the
  extension's branch is always `1.4`, independent of the runtime; on mismatch the
  sandbox has only Mesa. Read on the development machine from
  `flatpak info --show-metadata org.freedesktop.Platform//25.08`: the extension
  point is `directory = lib/x86_64-linux-gnu/GL`, `subdirectories = true`, so an
  installed extension appears as `.../GL/nvidia-<dashed-version>`.
- **Not this:** #842 (driver installed but not loaded), nouveau (#274, #407),
  #876 (`--filesystem=/usr/share/vulkan/icd.d:ro` fixed it), VMs.
- **Cordial:** a `Graphics` row in `cordial --diagnostics`, and on the crash page
  a sentence naming the extension to install when the host has an NVIDIA module
  and the sandbox has none that matches. It only speaks inside a Flatpak. The
  mount layout is read from the runtime's metadata, not from an NVIDIA machine,
  so **whether it fires on a real mismatch is `INFERRED`**. No auto-install;
  Flatpak is the tool for that and a launcher running `flatpak install` inside
  its own sandbox is a bigger decision than this work took.

### 4. Out of graphics memory on the proprietary driver (CONFIRMED as a driver-side class; unfixed)

- **Issues:** Sober #21, #266, #296, #411, #465, #801, #910, #1085, #1141, #1170,
  #1261, #1473, #1543, #1600, #1631, #1664, #1720, #2077, #2089, #2138, #2288,
  #2290, #2341, #2374. GTX 745 to RTX 5060 Ti; drivers 555 to 610.43.02.
- **Symptom:** `RBXCRASH: OutOfMemoryGraphics (Failed to allocate memory. size =
  1399808, alignment = 1024)`, typically minutes into a heavy place, VRAM climbing
  and not falling.
- **What was said (MEMBER, several, consistent):** NVIDIA's Linux driver does not
  free GPU memory properly for this engine and, unlike Mesa, does not spill to
  system RAM; "fragmenting their internal memory pool really hard until no memory
  is available". It "only affects the proprietary Nvidia drivers". Nobody found a
  fix. Still reproduces on 610.
- **Mitigations reported:** `DFFlagTextureQualityOverrideEnabled = true` with
  `DFIntTextureQualityOverride = 2` (a MEMBER on #465, several CONTRIBUTORs; it
  worked in #411, was "not a guarantee" in #910 and did nothing in #2290, #2341
  and #2374; values above 2 crashed #1543). OpenGL, which worked in #1600, #1935
  and #2288 but not for a second reporter on #2374. Nouveau or NVK. Sober itself
  stops caching textures after the first OOM ("Refusing to cache textures due to
  a previously reported OOM", from #1412) and warns on pre-Turing cards and on
  "a broken Vulkan memory allocator" (from #1414 and #1637), the latter on
  Turing, Ampere and Ada as well.
- **Cordial:** nothing in code. It is an engine and driver interaction with no
  Cordial call in it, and the only lever Cordial has (flags) is documented in
  `docs/nvidia.md` as a reported, unverified option rather than made a default,
  by the rule in `flags.rs`'s `BUILTIN` comment: an inferred change belongs
  behind a switch somebody chooses. The crash page now says what this is when
  `RBXCRASH: OutOfMemory` appears on an NVIDIA device.

### 5. X11 plus the dGPU launched from the desktop shortcut: `NV-GLX` BadMatch (mechanism INFERRED)

- **Issues:** Sober #1152, #1161, #1296, #1309, #1325, #1393, #1431, #1517,
  #1541, #1828, #1851, #1872, #1942, #2140, #2365.
- **Symptom:** `X Error of failed request: BadMatch ... Major opcode 156
  (NV-GLX) Minor opcode 43` right after `Vulkan: Loaded pipeline cache`, at
  experience join; works from a terminal, works on Wayland.
- **Reported cause:** Sober's shipped `.desktop` file carries
  `PrefersNonDefaultGPU=true` (#1942, and the file is pasted in #1690), which
  makes the shortcut launch on the dGPU.
- **Cordial:** **not exposed by construction, as far as the file can show.**
  `packaging/io.github.luohoa97.Cordial.desktop` has no `PrefersNonDefaultGPU`
  key. `INFERRED` that this avoids the crash; a tester on X11 with a hybrid
  laptop can confirm or refute it (plan below). Nothing to change.

### 6. Text input and fullscreen on Wayland (partly NVIDIA, partly not)

- **Issues:** Sober #877, #1026 (open), #1139, #1553, #1671, #1845, #1906 (open),
  #1940, #2039, #2195, #2205.
- **Not NVIDIA-only:** an AMD reporter in #877, Crostini and ChromeOS in #1940,
  #1906 and #2039, and #1906 says OpenGL does it too. #1026 is the narrow one:
  broken only fullscreen, only Wayland, when the surface is as large as the
  output. A MEMBER on #1553: "a quirk with the way NVIDIA interacts with Niri on
  Vulkan apps".
- **Cordial:** this is the same class as Cordial's own text-box work in
  fullscreen (the editor is a GTK field, ADR-019's `cordial_textbox` reads it
  back); nothing NVIDIA-shaped can be separated from it without hardware. Listed
  in the tester plan so that check is run on NVIDIA.

### 7. KWin explicit-sync protocol errors on 555 and 560 (fixed by Sober; Cordial not affected as far as is known)

- **Issues:** Sober #183, #244, #258, #558, #611, #1034 (and #202): `explicit
  sync is used, but no buffer is attached`, then the compositor closes the
  connection. Driver 560.35.03, RTX 3060 and 2060, KDE Plasma 6.1.
- **Fix:** Sober's; "the bug was due to a quirk only present in the Wayland video
  driver [of SDL]", fixed, native Wayland restored. `__NV_DISABLE_EXPLICIT_SYNC=1`
  did not help (#183). Driver 565 was reported to help (#183, #244).
- **Background (DOC):** NVIDIA 555 added `linux-drm-syncobj-v1` (the changelog
  says "in EGL"), the kernel needs drm-syncobj fixes present from 6.8, and a
  Vulkan client does nothing itself.
- **Cordial:** does not use SDL, so the specific bug cannot recur. Whether
  Cordial's own Wayland surface commits (`android/wayland.rs`, three sites) can
  trip the same protocol error is **not known**; it is a tester item on an
  NVIDIA 555+ system under KWin, checked by watching for a compositor
  disconnect, not by `WAYLAND_DEBUG`.

### 8. Vulkan-only rendering corruption (unfixed; OpenGL avoids it)

- **Issues:** Sober #559, #607, #693, #1121 (open), #1353, #1762 (label
  `nvidia`), #1974 (open); #682, #1075 and #1079 partly. Black scene at high
  graphics levels, some places unusable. MEMBERs: "a bug with some GPU drivers
  (particularly proprietary NVIDIA ones)" (#1353); "NVIDIA bug unfortunately"
  (#1762). A reporter's theory on #1974, unverified: a constant-buffer value
  arriving 85 times too large on NVIDIA.
- **Cordial:** nothing. OpenGL ES or NVK on Turing and later, per the
  maintainers.

### 9. Smaller classes

- **Old and open-source drivers:** Kepler and Fermi (340, 390, 470) have no
  Vulkan or no GBM (#9, #67, #94, #951); nouveau is not supported by Sober
  (#407); NVK on Turing and later is called "quite decent" (#1353).
- **The engine's webview and GBM in a helper process:** #43 (GTX 760: blank 2FA
  window, `nv_gbm_bo_create` failing), #140, #1364 (a Sober bug), #2112, #2292
  (Optimus, `EGL_BAD_PARAMETER`). Cordial's web view is WebKitGTK, and the
  research found many reports of WebKitGTK's DMA-BUF renderer failing on NVIDIA
  (blank window, "Failed to create GBM buffer"; `WEBKIT_DISABLE_DMABUF_RENDERER=1`
  is the usual workaround; LORE, no NVIDIA or WebKit primary). Whether Cordial's
  web window hits it is untested; it is in the tester plan.
- **GPU faults and Xid:** #2, #31, #244, #250, #512, #2190 (RTX 3060 Ti, 580 and
  595, OpenGL stable, no Xid). Nothing a launcher can do.
- **Regressions blamed on NVIDIA that were not:** Sober or Roblox-side
  (#297/#307/#313, #1773/#1778, the June 2026 CPU-feature crash on #2239 and
  friends, the silent 5-minute exits, error 277). Listed so they are not filed
  here again.
- **Performance ("why is my NVIDIA slow"):** almost always the iGPU rendering
  (cause 3), or OpenGL fallback, or FIFO on X11 (#278).

## Facts about NVIDIA and Wayland that bear on this path

Each with its label. Where a fact is DOC the source is NVIDIA's or Khronos's own.

| Fact | Label | Bearing on Cordial |
|---|---|---|
| Explicit sync (`linux-drm-syncobj-v1`) arrived in 555.42.02 beta and 555.58 stable, "in EGL" per the changelog; the driver version at which the Vulkan Wayland WSI itself used it was **not found**. Needs kernel 6.8, and a Vulkan client does nothing itself. | DOC, gap | Tester item, cause 7 |
| `nvidia-drm.modeset=1` was off by default in NVIDIA's own README through 580; **595.45.04 beta and 595.58.03 (2026-03) enable it by default**. `fbdev=1` is a documented workaround for a black screen or flicker when `simpledrm` stays active. Default by version for fbdev: only a search snippet. | DOC | `docs/nvidia.md` states it; only reporters mention either parameter in Sober's tracker |
| 555.42.02 added `IMMEDIATE` to the Vulkan Wayland WSI; 580.65.06 added `fifo-v1` on Vulkan. Which of `MAILBOX`, `FIFO_RELAXED` NVIDIA offers on Wayland versus X11: **not found**. | DOC / SEC, gap | Cordial asks for MAILBOX and falls back to the engine's own choice when the driver does not list it, so this is safe either way; tester item |
| A report on 550.90.07 and 555.58.02 says native-Wayland `vkQueuePresentKHR` waited for the GPU and did not return while the window was hidden; gone in 560.28.03. One user, no NVIDIA reply. | REPORT | This is the reason mocktail defaults NVIDIA-on-Wayland to XWayland |
| Vulkan Wayland `currentExtent` is `(0xFFFFFFFF, 0xFFFFFFFF)`; the client sizes the swapchain. | DOC | Already handled: `vk_get_physical_device_surface_capabilities_khr` |
| Swapchain bugs by driver: 545 and 550 native-Wayland `vkCreateSwapchainKHR` failing (535 worked); 570.144 and 575.51.02 crash with a second Vulkan instance destroying a Wayland swapchain; 580.65.06 to 580.76.05 hang Vulkan on Wayland at exit (fixed 580.82.07, DOC); 590.48.01 segfault on fullscreen swapchain recreation on hybrid GNOME 49; 595.71.05 Blackwell swapchain creation regression. | mostly REPORT | Driver-side. Listed so a tester's driver can be compared |
| GTK 4.16 and later use a Vulkan GSK renderer by default on Wayland, so the shell shares NVIDIA's Vulkan WSI with the engine. | SEC | Relevant to hangs at window close on 580.65 to 580.76 |
| `driverVersion` is packed 10/8/8/6 for NVIDIA (Khronos `vulkaninfo.h`, DOC); `VK_KHR_driver_properties` `driverInfo` carries the plain string on NVIDIA (REPORT). | DOC | Implemented in `nvidia::DriverVersion` |
| `__GL_THREADED_OPTIMIZATIONS`, `__GL_YIELD`, `__GL_SYNC_TO_VBLANK` are documented for OpenGL only. Only `__GL_SYNC_DISPLAY_DEVICE` (FIFO or FIFO_RELAXED) and `__GL_SHOW_GRAPHICS_OSD` are documented for Vulkan. `__GL_MaxFramesAllowed` is absent from the README. | DOC | **Do not recommend any of them.** They do not apply to this engine's renderer |
| `__NV_PRIME_RENDER_OFFLOAD=1` alone suffices for Vulkan; `__VK_LAYER_NV_optimus=NVIDIA_only` reorders enumeration; `VK_DRIVER_FILES` and `VK_LOADER_DRIVERS_SELECT` are the loader's own. | DOC | The user-facing workaround for cause 2 |
| ETC2 and ASTC are not supported natively on NVIDIA desktop GPUs (two gpuinfo.org reports show BC only). | DOC, two reports | Sober logs `emulated fast ETC2 support` on NVIDIA; [ADR-042](../adr/ADR-042-texture-format-query-observability.md) |
| Flatpak GL extension: branch `1.4`, matched on the kernel module's version; the Vulkan ICD JSON comes from it. | DOC | Cause 3 |
| Linux R590 dropped Maxwell, Pascal and Volta (Arch news, SEC); 595 is the first production branch without them; NVIDIA's own support-plan page returned 403. | SEC | Pascal users are on 580 |
| Vulkan 1.3 arrived in 510.47.03. No consolidated "minimum driver for reliable native-Wayland Vulkan" exists from NVIDIA. | SEC | **No minimum driver is stated in `docs/nvidia.md`**: none is supported by evidence |

## What mocktail does about NVIDIA

`~/Projects/mocktail`, Apache-2.0, read for ideas only:

1. **`FStringGraphicsVulkanShaderMTDenyPattern = "4318:.*"`** as a default for
   every Vulkan launch (`src/runtime/graphics_launch_policy.cc`,
   `scripts/real_bringup_smoke.sh`). The reason its own comment gives is not an
   NVIDIA hardware fault: "Roblox 2.725's multithreaded pack loader performs
   fseek/fread pairs against one shared Android FILE. Host stdio cannot make
   that two-call transaction atomic, so use Roblox's supported vendor deny policy
   for NVIDIA (0x10de = 4318) until the loader owns independent positional
   streams." It is a workaround for mocktail's own libc bridge that borrowed
   NVIDIA's vendor id as a switch. **See "Not applied" below.**
2. **NVIDIA-on-Wayland defaults to X11/XWayland**
   (`src/window/video_driver_policy.cc`): when the proprietary kernel module is
   present (`/proc/driver/nvidia/version`), the session is Wayland and `DISPLAY`
   is set, pick the X11 SDL driver. Its comment: "a blocked native Wayland
   present cannot be cancelled without violating VkQueue ownership." It keys on
   the kernel module, so a hybrid laptop rendering on the iGPU is switched too.
3. `NODEVICE_SELECT=1` and `DISABLE_LAYER_MESA_ANTI_LAG=1` for the Mesa
   device-select layer, and forwarding of `VK_DRIVER_FILES`, `DRI_PRIME`,
   `__NV_PRIME_RENDER_OFFLOAD` and `__VK_LAYER_NV_optimus` through its update
   harness.

The first two are recorded in [TASKS.md](../../TASKS.md) T2 and below.

## Applied

Every item is behind the device's `vendorID` (`0x10DE`) read from
`vkGetPhysicalDeviceProperties`, except the two that are observations about the
host and say so.

| Change | Where | Gate | Evidence | Label |
|---|---|---|---|---|
| Identity line for the device the engine builds its logical device on, printed for every vendor: name, vendor, device id, and for NVIDIA the decoded driver version and the kernel module's | `vulkan.rs` `announce_physical_device` | none (all vendors) | needed by every report; the crash page reads it | observation |
| Advisory for driver series 535 and 550 | `nvidia::driver_advisory`, printed with `[android] vulkan: NVIDIA advisory:` | vendor NVIDIA and driver major in {535, 550} | cause 1, maintainer-confirmed | `INFERRED` that it applies to Cordial |
| Retry of the present-modes query on `VK_ERROR_UNKNOWN`, `INITIALIZATION_FAILED`, `SURFACE_LOST` | `vulkan.rs` `vk_get_physical_device_surface_present_modes_khr`, `nvidia::retry` | vendor NVIDIA of the queried physical device | cause 2, four independent workaround reports | `INFERRED` that it helps |
| `Graphics` row in `cordial --diagnostics` | `diagnostics.rs`, `nvidia::graphics_line` | reads the kernel module, not Vulkan | cause 3 | observation |
| Flatpak GL-extension mismatch sentence on the crash page | `crash.rs`, `nvidia::flatpak_gl_here().advice()` | Flatpak and an NVIDIA module and no matching extension | cause 3, documented Flatpak behaviour | mount layout `INFERRED` |
| Crash-page hint for the advisory, the present-modes give-up and `RBXCRASH: OutOfMemory` | `nvidia::crash_hint` | the client's own identity line says the device was NVIDIA's | causes 1, 2, 4 | wording says "may" |

**How each was tested on this Intel machine.** `CORDIAL_FORCE_GPU_VENDOR=0x10de`
(optionally `@550.163.01`) makes the gate say NVIDIA for a device that is not,
and changes nothing the engine sees; `CORDIAL_TEST_FAIL_PRESENT_MODES=N` fails
the first N present-mode queries on a gated device so the retry can be watched.
Both are off by default, are named in `cordial-run --help`, and no packaging
script sets them. The unit tests cover the gate, the decode, the schedule, the
override parsing and the hint. What the runs on Intel show is that the NVIDIA
path activates and does no harm; **they say nothing about whether it helps on
NVIDIA.**

### Measured on the Intel machine, 2026-09-30

Debug build of `cordial-run` at this change, five runs, one each, signed out, in
a nested headless `sway` on its own socket (never the desktop's), each 30 seconds
with `--run 30`, same profile and data root. Every run exited 0 and reached
`app ready: Landing`. **One run per arm is not a stability result;** what these
show is that the NVIDIA path switches on and off as designed and does no harm
when it is on.

| Arm | Environment | What the log printed |
|---|---|---|
| A, control | none | `physical device "Intel(R) Graphics (RPL-P)" vendor 0x8086 device 0xa7a8`; no NVIDIA line anywhere |
| B, forced | `CORDIAL_FORCE_GPU_VENDOR=0x10de` | the same line plus `-- gating as vendor 0x10de ... (CORDIAL_FORCE_GPU_VENDOR, test only)`, then `NVIDIA device: present-mode queries will be retried if the driver refuses them`, which is the engine's own present-modes query passing through the guard |
| C, advisory | `...=0x10de@550.163.01` | as B, and `NVIDIA advisory: NVIDIA driver series 535 and 550 have been reported to crash ...` |
| D, retry recovers | as B, plus `CORDIAL_TEST_FAIL_PRESENT_MODES=2` | `NVIDIA present-modes query took 3 attempts and ended at 0` (two injected `-13`, then the driver answered); no error reached the engine |
| E, retry gives up | as B, plus `CORDIAL_TEST_FAIL_PRESENT_MODES=20` | `took 5 attempts and ended at -13`, then `NVIDIA present-modes query still failing (last result -13); the engine will report it`, four times in the run (two queries at two swapchain creations); the client still reached Landing |

The control is arm A against the rest: the same binary and profile, with only the
override differing. The unit tests were also run with a deliberate fault put in
the driver-version decode (the Vulkan 10/10/12 split instead of NVIDIA's
10/8/8/6): three tests failed, and passed again when it was put back.

What this cannot show: that any real NVIDIA driver behaves as arms D and E assume.
The injected failure is Cordial's own, made inside the guard, and proves only that
the schedule runs, recovers and gives up as the tests say. Arm E's four give-ups
can have cost at most 4 x 1.85 = 7.4 seconds of that run (computed from the
schedule, not timed), which is the worst case the retry can cost.

## Not applied, and why

- **The ShaderMT deny pattern (TASKS.md T2).** The only evidence is mocktail
  shipping it, and mocktail's own reason is its libc bridge's non-atomic
  `fseek`/`fread`, which has nothing to do with NVIDIA. No Sober issue mentions
  it (`ShaderMT` occurs nowhere in the corpus), no NVIDIA report is a
  shader-compile failure, and its effect on Cordial is unmeasured in either
  direction. `flags.rs` already records what shipping an inferred flag as a
  default cost once (`FStringGraphicsTextureManager2DenyPattern2`). It also
  cannot be gated on the device from the shim: flags are read before any Vulkan
  device exists, though the engine matches the pattern against `vendor:device`
  itself. The flag still works through `flags.json` for anyone who wants to test
  it; the tester plan gives the line, and the engine's default value for it is
  not known here (it is not in `docs/traces`), so the plan appends nothing and
  says to read it from the client's own log first.
- **Defaulting NVIDIA on Wayland to X11.** Cordial's X11 backend cannot attach web
  views (`crates/cordial-runtime/src/android/mod.rs`), Wayland is the primary
  backend ([ADR-011](../adr/ADR-011-wayland-and-libadwaita.md),
  [ADR-024](../adr/ADR-024-x11-is-supported-again.md)), the backend is chosen
  before the device is known, and the evidence is one forum report plus
  mocktail's comment. `CORDIAL_X11=1` remains the user's switch and
  `docs/nvidia.md` names it with its cost.
- **Switching to OpenGL ES on 535/550.** Same timing problem, and GLES is
  documented as not shown stable. The advisory says how.
- **Waking or choosing the NVIDIA device before the surface exists,
  `PrefersNonDefaultGPU`, `__NV_PRIME_RENDER_OFFLOAD` as a default.** Each
  changes which GPU renders, for hybrid users who may want the iGPU, on
  reporter-level evidence.
- **Auto-installing the Flatpak extension.** A launcher running
  `flatpak install` from inside its own sandbox is a decision, not a fix.
- **Anything that patches NVIDIA's library** (Sober's
  `patch_libnvidiaglcore_overzealous_vram_caching`). ADR-001.
- **Any `__GL_*` variable.** Not documented for Vulkan.
- **A minimum driver version** in user documentation. None is supported by
  evidence; only two series with reports, and the fact that Pascal cannot go past
  580.

## What cannot be fixed without hardware

Everything in this file that says `INFERRED`, and in particular: whether the
retry helps; whether the advisory applies to Cordial's own renderer; whether the
Flatpak check fires on a real mismatch; whether Cordial's Wayland commits trip
NVIDIA's explicit-sync checks; which present modes NVIDIA offers on Wayland;
whether MAILBOX behaves; whether Cordial's web view works on NVIDIA Wayland;
whether the out-of-memory class is worse or better under Cordial than under
Sober (the engine allocates the same way).

## Tester plan

For somebody with an NVIDIA GPU. Use a **test account, on a separate IP**, and
keep runs at least 90 seconds apart (AGENTS.md); most of this needs no account at
all. Run each numbered step once and report what it printed, not a summary.
Steps 1 to 4 are the essential set; everything after is one experiment each.

**0. Say what you have.** GPU model, whether it is a laptop with a second GPU,
distribution, session (`echo $XDG_SESSION_TYPE`), compositor and its version,
and how Cordial was installed. Then:

```
cordial --diagnostics                       # flatpak run io.github.luohoa97.Cordial --diagnostics
cat /proc/driver/nvidia/version
cat /sys/module/nvidia_drm/parameters/modeset /sys/module/nvidia_drm/parameters/fbdev
vulkaninfo --summary
nvidia-smi
```

The `Graphics` row of the first, and the `driverInfo` line of `vulkaninfo`,
are what everything else is compared against.

**1. Does it start and which GPU does it use.** Run from a terminal so output is
captured, signed out, for a minute:

```
cordial-shell 2>&1 | tee cordial-nvidia-1.log          # or the Flatpak equivalent
grep -E 'physical device|NVIDIA|present mode|vkCreateSwapchainKHR' cordial-nvidia-1.log
```

Expect one `[android] vulkan: physical device "..." vendor 0x10de` line naming
your GPU. **On a laptop, it must name the NVIDIA GPU or the rest of this plan is
about the wrong device**; say so if it names the iGPU. Capture the first 40 lines
of the log whatever happens.

**2. Reach Home, then two presents-counts for wedge detection.** With the MCP
attached (`just dev --play` and `just mcp`), call `cordial_info` twice five
seconds apart with the pointer moving between them. Present count must advance.
Take a `cordial_screenshot`. A fixed count is the wedged-client signature
([AGENTS.md](../../AGENTS.md)).

**3. Present mode.** From the log: the line `swapchain present mode ... (driver
offers ...)`. Report the list of modes the driver offers on your session. Then
repeat with `CORDIAL_PRESENT_MODE=off` and again with `fifo`, driving input for
the whole window, and report presents per second with the input rate beside it
([AGENTS.md](../../AGENTS.md), "Do not use present counts as a frame rate").
This answers which modes NVIDIA offers on Wayland and whether MAILBOX behaves.

**4. Resize and fullscreen.** With a window open at its default size, drag-resize
it, then press F11 twice. Watch for the exit and the last log line. A crash whose
last line is `SceneManager: resizing main targets` on a 535 or 550 driver is
cause 1: record the exact driver, then repeat the launch with Settings, Graphics,
Renderer set to OpenGL ES and report whether it survives. Do the same after
moving to driver 580 if you can.

**5. Cold-boot hybrid laptops only (cause 2).** Reboot. Launch Cordial as the
first graphical thing you do, before any other GPU application. Capture the log.
Look for:

```
[android] vulkan: NVIDIA device: present-mode queries will be retried ...
[android] vulkan: NVIDIA present-modes query took N attempts and ended at 0
```

`took 2 attempts and ended at 0` would be the first evidence the retry does what
it was written to. `still failing` means it does not. If it fails, run `vulkaninfo`
once and launch again, which is the reported workaround, and report both.
Control: the same reboot without the retry is not available; compare with the
Flatpak of Sober or the old behaviour if you have it, otherwise say so.

**6. Flatpak only (cause 3).** `flatpak list | grep GL.nvidia`, and compare with
`cat /proc/driver/nvidia/version`. If they already match, deliberately test the
message: `flatpak remove org.freedesktop.Platform.GL.nvidia-<version>`, run
`cordial --diagnostics`, and report the `Graphics` row (it should say NO matching
extension), then reinstall it. If they differ on their own, that is the most
useful report of all.

**7. Text input (cause 6).** Focus a text box in fullscreen and windowed,
`cordial_textbox` before and after typing ([ADR-019](../adr/ADR-019-development-control-surface.md)).
Report the compositor.

**8. Web view.** Open the Marketplace or Profile window. Blank, white or crashing
is the WebKitGTK DMA-BUF class (cause 9); retry with
`WEBKIT_DISABLE_DMABUF_RENDERER=1` and report both.

**9. Heavy place for fifteen minutes (cause 4).** Join a large place on a test
account and log VRAM beside it:

```
nvidia-smi --query-gpu=timestamp,memory.used,memory.total --format=csv -l 10 > vram.csv
```

Report the peak, whether `RBXCRASH: OutOfMemory` appears, and the crash-page hint
if it does. Repeat with the texture flags from `docs/nvidia.md`.

**10. Shader threading (TASKS.md T2).** Read the current value of
`FStringGraphicsVulkanShaderMTDenyPattern` from the client's own settings log
first. Then set it in `flags.json` to that value with `|4318:.*` appended, and
compare shader compile time and a hitch count on a first join against the same
place without it. One run each is not a result; three of each is.

**11. Explicit sync.** On a compositor with `linux-drm-syncobj-v1` (KWin 6.1,
Mutter 46.1, Hyprland 0.42, Sway 1.11), play for ten minutes and check the
compositor's own log (`journalctl --user -b`) for `explicit sync is used`,
`no buffer is attached` or a client disconnect. Do **not** use `WAYLAND_DEBUG`;
it changes the timing this is about.

**What to report.** The commands' output above, pasted rather than summarised;
`cordial --diagnostics`; whether each step passed, failed or was not run; and any
crash's last twenty log lines plus, for a freeze, `cordial_backtrace` (which
quotes CPU beside the stacks). File it as a `finding` in
`.github/ISSUE_TEMPLATE/finding.yml` if it establishes or disproves one of the
`INFERRED` items above.
