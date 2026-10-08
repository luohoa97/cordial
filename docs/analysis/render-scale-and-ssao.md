# Render scale prototype, and what "Roblox SSAO" can mean here

Branch `upscale-proto`, 2026-10-08, from `0814e2d` (0.27.0). The decision this
supports is [ADR-057](../adr/ADR-057-render-scale-is-an-upscale-in-the-present-path.md),
status proposed. Everything below was run on one machine: Intel RPL-P iGPU
(Mesa), nested headless sway at 60 Hz, **signed out**, on the Landing and
sign-in pages of engine 2.737 (Sober's `split_config.x86_64.apk` of 7 September).
No 3D scene was rendered at any point, because reaching one needs a signed-in
account. Every number is for a 2D page.

## 1. Upscaling

### Where it hooks

| What | Where (worktree) |
|---|---|
| Extent reported to the engine | `android/vulkan.rs` `vk_get_physical_device_surface_capabilities_khr` (wrapper, scales and lowers `minImageExtent`); the Wayland `0xFFFFFFFF` substitution and `settle_resize_extent` are the unchanged `..._unscaled` function beneath it |
| Real swapchain at window size, widened usage | `vk_create_swapchain_khr`; the old body is `vk_create_swapchain_khr_real` |
| Proxy images, pass, semaphores | `android/render_scale.rs` `attach` / `record` |
| Engine is given the proxies | `vk_get_swapchain_images_khr` (handed out only when the variable is set, from both `vk_get_instance_proc_addr` and `vk_get_device_proc_addr`) |
| Upscale at present | `vk_queue_present_khr` -> `render_scale::before_present`, before the capture |
| Teardown | `vkDestroySwapchainKHR` wrapper, `scaled_destroy_entry` |
| Pointer into the engine | `input.rs`: `deliver_mouse`, `deliver_scroll`, `pass_mouse_move_delta`, `pass_mouse_button`, `wheel` call `render_scale::to_engine` |
| Text editor rectangle out of the engine | `wayland.rs` `update_text_overlay` and `info_fits_canvas` call `render_scale::to_window` |
| Filter | `crates/cordial-runtime/shaders/render_scale/upscale.frag` (Snapdragon GSR 1) and `.vert`, SPIR-V beside them, `include_bytes!` |

The engine takes `currentExtent` as the size of its screen and renders straight
into the images it is given, so no engine call needs to change. Acquire does
not change: proxy `i` and real image `i` share an index.

Cost, per swapchain image: one image of the render size, one image view of it,
one view of the real image, one framebuffer, one descriptor set, one semaphore,
one pre-recorded command buffer. Per frame: one `vkQueueSubmit` and one
three-vertex draw. At 67% of 1280 x 754 with four images that is about 6.9 MB
of extra images.

### Measurements

Method: `tools/render-scale-bench.py` (reuses `text-input-e2e.py`'s sway
launch and devctl client), driven by a loop that ran the arms in a fixed order,
three repeats each, one client at a time, no other `cordial-run` present at
any start (the loop records it). Every arm: fresh profile and redirected
`XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_CONFIG_HOME`; 8 s of warm-up and then
**30 s with pointer motion sent at about 67 a second throughout**, so the
1024-interval ring holds input-driven frames only. Control arm: the same binary
with `CORDIAL_RENDER_SCALE` unset. An earlier run of the unmodified `main`
build gave the same presents (60.0 a second, p50 16.7 ms), so unset adds
nothing the frame interval can see.

"GPU render busy" is the delta of `drm-engine-render` across the 30 s, from
`/proc/<pid>/fdinfo`, over wall time. It is the render engine's busy share
for this process, upscale pass included. Frame intervals are from the engine's
own present-to-present ring (`frame_pacing`).

Means over three runs (per-run GPU values in brackets):

| window | arm | presents/s | input/s | p50 ms | p95 ms | p99 ms | max ms | GPU render busy % |
|---|---|---|---|---|---|---|---|---|
| 1280x754 | control (unset) | 59.8 | 67.2 | 16.6 | 18.7 | 20.0 | 50.5 | 14.9 (14.4, 15.9, 14.4) |
| 1280x754 | 67% GSR | 59.5 | 67.2 | 16.7 | 17.3 | 18.1 | 56.0 | 11.7 (11.7, 11.0, 12.3) |
| 1280x754 | 67% bilinear | 59.7 | 67.4 | 16.7 | 17.0 | 17.2 | 66.4 | 10.6 (11.6, 10.1, 10.1) |
| 1280x754 | 50% GSR | 59.8 | 67.5 | 16.7 | 17.0 | 17.2 | 44.6 | 10.5 (10.1, 10.6, 10.7) |
| 2560x1554 | control (unset) | 59.7 | 67.3 | 16.7 | 17.0 | 17.1 | 66.8 | 40.9 (42.8, 43.0, 36.9) |
| 2560x1554 | 67% GSR | 59.9 | 67.3 | 16.6 | 18.2 | 18.5 | 29.6 | 33.9 (34.4, 34.2, 33.2) |
| 2560x1554 | 50% GSR | 59.8 | 67.4 | 16.7 | 16.9 | 17.0 | 53.5 | 27.4 (28.3, 25.7, 28.2) |

The upscale pass on its own, from timestamp queries around it (mean of 300
frames, printed by the client): GSR 0.55 to 0.61 ms at 1280x754 output, 1.7 to
1.9 ms at 2560x1554, bilinear 0.18 to 0.21 ms at 1280x754.

What this does and does not show:

- **The frame interval did not move**, because nothing here was limited by the
  GPU: every arm, control included, sits at the output's 60 Hz with a p50 of
  16.6 to 16.7 ms. The small p95 and p99 differences are within the spread of
  three runs and are not claimed. A frame rate gain from this feature has not
  been measured, and could not be on this scene.
- **The GPU does less work**: render-engine busy fell from 14.9% to 11.7% at 67%
  and 10.5% at 50% in the small window, and from 40.9% to 33.9% and 27.4% in the
  large one. The pass itself is about 3% of a 16.7 ms frame at 1280x754 and
  about 11% at 2560x1554, so the engine's own work fell by more than the net
  figure says. That is a measurement of a 2D page; a 3D scene's cost will not
  scale like this.
- Process CPU is in the raw files and is not reported: it varied from 10% to 25%
  between runs of the same arm and the control arm was the high one at 1280,
  which nothing here explains.

Raw per-run JSON, logs and frames: the scratchpad `meas/` directory of the
session, tags `m1280-*` and `m2560-*`. Not committed: the frames contain
Roblox's own artwork.

### Screenshots (via `cordial_screenshot`'s capture, which reads the real swapchain)

All window 1280x800, so 1280x754 content. Paths are under
`/tmp/claude-1001/-var-home-neilluo-Projects-cordial/c844543c-3c82-4dea-a57c-5022771d9c1c/scratchpad/`:

- 100%: `meas/m1280-ctl-r1-before.png`
- 67% GSR: `meas/m1280-s067-r1-before.png`
- 67% bilinear: `meas/m1280-s067bil-r1-before.png`
- 50% GSR: `meas/m1280-s050-r1-before.png`
- sign-in page, native, then 67%: `rs/ctl-click-clicked.png`, `rs/s067-click-clicked.png`
- crop of the logo edges, 2x nearest: `crop-logo.png`

The image is captured after the upscale, so these are what the user sees. The
content is not the same size across arms (see below), which makes a
pixel-for-pixel comparison meaningless. Looking at logo edges at 2x, GSR at 67%
is crisp and bilinear is visibly softer; that is one flat logo judged by eye and
there is no image-quality metric behind it.

### The interface gets larger

The engine lays its interface out against the extent it was told. At 67% the
Landing page's buttons are 1.5 times as large relative to the window, and the
sign-in page no longer fits (`Quick Sign-in` and the links are below the fold at
1280x754). This is the same as running at a lower display resolution. It is a
property of this approach, not a bug in the prototype. The engine has its own
dynamic-resolution machinery (below) which may not have this property; that
needs a 3D scene to find out.

### Input

Negative control, no mapping (an earlier build of the same branch): at 67%,
a click at the window pixel where `Sign In` is drawn does nothing; the
hover state does not appear either (`rs/s067-nomap-clicked.png`). With
`to_engine`: the same click opens the sign-in form, as the native-size control
does (`rs/s067-click-clicked.png`, `rs/ctl-click-clicked.png`).

Text editor, with a click into the username field, `text scaletest` through
devctl (nine characters, `chars=9` on the `textbox` verb, nothing sent to
Roblox's servers and no sign-in attempted), and a photograph of the nested
compositor with `grim`, which is the only way to see the GTK widget:

- native: the engine reports `x=470 y=295 w=340 h=22`, the text sits in the field.
- 67%: the engine reports `x=259 y=260 w=340 h=22` in its own pixels, which is
  386 x 388, 507 x 33 in the window's; the text sits in the field and scales
  with it. (`rs/ed-ctl-compositor.png`, `rs/ed-s067-compositor.png`.)

`textbox` itself reports the engine's pixels, not the window's, at 67%.

Not done: touch contacts are not mapped, deltas of a locked pointer are left as
they are on purpose (a distance moved, not a place), and the X11 backend has
not been run.

### Not run

- FidelityFX FSR 1 EASU and RCAS. Not ported; see the ADR.
- 3D scenes of any kind, so no comparison of how the engine's quality settings
  interact with a smaller extent.
- NVIDIA, AMD, X11, a touchscreen, a validation layer. The prototype uses
  `PRESENT_SRC_KHR` on non-presentable images, which a validation layer will
  flag (ADR-057, "Known deviations").
- `cargo test --workspace`. Only `cargo test --release -p cordial-runtime --lib
  render_scale` ran: three tests, passing (structure sizes against the C
  compiler's, scale parsing, extent round trip).

### Licences

| Candidate | Licence | Position |
|---|---|---|
| Snapdragon GSR 1 | BSD-3-Clause | Used. Port of `sgsr1_shader_mobile.frag`; attribution in the shader header and an entry in `THIRD-PARTY-NOTICES.md` |
| FidelityFX FSR 1 (EASU, RCAS) | MIT (GitHub shows `license.txt`, "Copyright (c) 2021 Advanced Micro Devices, Inc.") | May be ported with the notice; not done |
| RAVU (`bjin/mpv-prescalers`) | LGPL-3.0-or-later (each hook's header says "version 3 ... or (at your option) any later version") | Compatible with Cordial's GPL-3.0-or-later, so licence is not the obstacle; size (3.7 MB for `ravu-zoom-r3.hook`), the mpv hook format and speed are |
| Stud | AGPL-3.0 with an extra permission for the engine it loads (from the evidence sweep's reading of the repo page; not checked again by me) | Its README only, for the feature list; nothing read from its source. Its README lists upscalers "RAVU-Zoom AR" and "SGSR1 ED" and no SSAO |
| Sober, iceblox | not source-available | Observed only; not used here |

## 2. SSAO

### The engine has one

Nothing was disassembled; these are names and strings (`strings -a`, `grep`).
Verified by me against the 2.737 library above (118,134,416 bytes), after an
agent's report said the same:

- shader and pass names: `SSAODepthDownFS`, `SSAOBlurXFS`, `SSAOBlurYFS`,
  `SSAOFS`, `SSAOVS`, `SSAOCompositFS`, `SSAOCompositVS`, `SSAOApply`,
  `SSAOMipLevels`, `SSAO Resolve`, `SSAO Noise`, `SSAO Depth Downsize`, and the
  profiler scope `render/ssao`; and for HBAO `HbaoFS`, `HbaoRenderCompute`,
  `HbaoCompositFS`, `HbaoCompositClippedFS`, `Hbao PS`, plus `AoBlurUpsampleCS`.
  20 strings match "ssao" in total.
- a debug name `DebugSSAOForce`, and FrameRateManager log text
  `[FLog::FRM] Profile override: invalid SsaoLevel {} for range '{}'`, beside
  `FRMLockstepProfileOverride` and `EnableFRMLockstepTableOverrides`. That reads
  as a per-quality-level table, overridable by JSON, with an `SsaoLevel` per
  range. `INFERRED` from the log text; not read, not run.
- `RenderDisableAoTransparent` (a published flag, `FFlagRenderDisableAoTransparent`,
  `client-settings-flag-names.txt:17175`).
- Sober #694 logs `shader SSAOCompositViewportVS is not available` on a
  Sober engine, which says the pass is requested there.
- Roblox's public documentation lists no AO property or setting (Lighting,
  post-processing effects, `QualityLevel`). DevForum threads, weaker evidence,
  say AO appears from about quality level 8 or 9, is HBAO on PC and SSAO on
  Xbox, and cannot be disabled by creators.

So "Roblox SSAO" in the engine means a quality-level feature, not a user setting
and not a published flag. **No published flag turns it on.** The only names that
could are binary-only (`DebugSSAOForce`) or inside a JSON override of the FRM
profile, and neither has been shown to do anything.

### I tried the flags, and learned very little

A profile `flags.json` with `FFlagAutomaticDRS4`,
`FIntDebugAutomaticDRSAreaScaleOverrideTenths`, `FIntDebugSSAOForce` and
`DFIntDebugSSAOForce` was accepted by Cordial's flag layer (`flags: 5
override(s) applied` in the log) and changed nothing the Landing page shows:
same extent, same render busy (12.2% against 12.2% in the control). A 2D page
is not a test of a 3D post effect. The type prefixes of the binary-only names
are guesses (the strings carry no prefix), so a null result would not even
mean the flag does not exist. **No claim is made that any of these work.** The
test that would settle it is signed in, in a 3D place, with shader-compile
counts and a screenshot pair, and AGENTS.md rules out launching signed in on
this project's test runs.

### Can Cordial's Vulkan layer build its own?

Reach: an SSAO pass needs the depth buffer, and the depth buffer is an image the
engine creates for its own render passes and never gives Cordial. The layer does
see every call the engine makes through it, which is the same reach ADR-049's
ETC2 shim uses: `vkCreateImage` with a depth format, `vkCreateRenderPass`,
`vkCmdBeginRenderPass`. Recording which image the main scene pass used as its
depth attachment and, after that pass, reading it in a Cordial-owned draw is
possible without reading or writing any engine memory or code. **That is
inside ADR-001 and ADR-003 as written**: it observes calls to function pointers
Cordial supplied, as the capture behind `cordial_screenshot` does.

It is still a poor idea:

- Finding "the" scene depth image is guesswork. The engine renders shadow maps,
  its own SSAO or HBAO depth downsample, reflection and viewport-frame passes,
  and the main scene may be a subpass of a larger render pass, in which case
  there is no point between subpasses at which Cordial can run a draw. That is
  a statement about Vulkan, not about this engine, but the engine's render
  graph has not been looked at, and looking at it is the decompilation
  AGENTS.md warns against.
- The result would be wrong in a way the engine's own AO is not. It has no
  per-material or per-pixel occluder knowledge, transparent parts would be
  occluded as if solid, and it would stack on top of the engine's own AO at the
  quality levels that already have one.
- It would be a Cordial-owned look applied to every game, with no engine switch
  to turn off, which is closer to a post-processing mod than to a compatibility
  layer.

Worth it? **No, not before the engine's own has been tried in a 3D scene.** If
the FRM override or `DebugSSAOForce` works on a signed-in test client, a
built-in plugin exposing it is the right shape and costs almost nothing:
`plugins/fps-flex` writes `values` through `flags.set` under the `flags.write`
capability, and an `Ambient occlusion` choice preference would write the flag or
the FRM override JSON string the same way; it would take effect at the next
launch because `FFlag`/`FInt` flags are read once (the flag layer's own
comment). The plugin would never see Vulkan.

### The better upscaling lead, from the same search

The same strings show the engine has dynamic resolution of its own:
`FFlagAutomaticDRS4`, `FIntAutomaticDRSHundredthPercent`,
`FFlagRenderDynamicResolutionScale12`, `FFlagRenderWatermarkInUpscalePass`,
`FFlagAutomaticDRSUseGpuTime2` (all in `client-settings-flag-names.txt`), and in
the binary `DebugAutomaticDRSAreaScaleOverrideTenths`, `DebugDisableDRS`,
`AutomaticDRSFullSizeFramebufferDenyList`. `INFERRED`: this renders the 3D scene
smaller and the interface at full size, which is exactly what the prototype
cannot do. Whether it is active on Android builds, what it does to the picture,
and what the area override means have not been run, and a 3D scene is needed to
see any of it.

### What Stud and the corpus add

Stud's README claims the two upscalers and no AO. Sober's tracker has one AO
report (#1790, "no ambient occlusion", closed "client limitation") and the
`RenderDisableSSAO` flag in copied user configs (#1259, #1335), a name that is
in neither the published flag list nor the 2.737 strings.
