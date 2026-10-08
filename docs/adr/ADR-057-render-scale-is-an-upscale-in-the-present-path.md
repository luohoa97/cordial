# ADR-057: Render scale is a smaller extent for the engine and an upscale in the present path

**Status:** proposed
**Date:** 2026-10-08
**Related:** [ADR-001](ADR-001-in-process-hooking.md), [ADR-003](ADR-003-plugin-isolation.md), [ADR-007](ADR-007-host-resources-are-brokered.md), [ADR-046](ADR-046-nvidia-is-gated-on-the-vendor-id.md), [ADR-049](ADR-049-etc2-is-emulated-where-the-driver-lacks-it.md); measurements and the SSAO findings in [`docs/analysis/render-scale-and-ssao.md`](../analysis/render-scale-and-ssao.md)

## Context

A user on a weak GPU or a very large output has one lever the engine does not
give them on Linux: draw fewer pixels. The engine renders straight into the
swapchain images Cordial's Vulkan layer creates for it, at whatever extent
`vkGetPhysicalDeviceSurfaceCapabilitiesKHR` reports, so Cordial already
decides how many pixels it draws.

A prototype on branch `upscale-proto` shows the smallest path that works. It is
off unless `CORDIAL_RENDER_SCALE` is set to a value from 0.25 up to, but not
including, 1.0.

## Decision (proposed)

1. **Report a smaller surface.** `vkGetPhysicalDeviceSurfaceCapabilitiesKHR`
   returns `currentExtent` scaled, with `minImageExtent` lowered to match
   (X11 reports min = max = current, and an engine that clamps into that range
   would put the real size straight back). Cordial remembers the real extent
   per surface. (`vulkan.rs`, `vk_get_physical_device_surface_capabilities_khr`;
   the Wayland substitution and the resize debounce of
   `settle_resize_extent` run first and are unchanged.)
2. **Build the real swapchain at the window's size,** with the usage a draw
   into it and a screenshot of it need, and give the engine **proxy images** at
   the size it asked for, one per real image, through `vkGetSwapchainImagesKHR`.
   Acquire is not touched: index `i` of the proxies is index `i` of the real
   images. (`vulkan.rs`, `vk_create_swapchain_khr`; `render_scale.rs`, `attach`.)
3. **Upscale inside `vkQueuePresentKHR`.** A command buffer recorded once per
   image samples proxy `i` and draws into real image `i`; it waits on the
   semaphores the engine's present would have waited on, and the present waits
   on its one. (`render_scale.rs`, `before_present`; `vulkan.rs`,
   `vk_queue_present_khr`.) The capture behind `cordial_screenshot` runs after
   it, so a screenshot is the image the user sees.
4. **The filter is Snapdragon GSR 1** (single fragment pass, BSD-3-Clause),
   with bilinear as the cheap comparison (`CORDIAL_RENDER_SCALE_FILTER=bilinear`).
5. **Input and the text editor follow the engine's pixels.** The engine lays
   its interface out against the extent it was told, so a pointer position goes
   to it multiplied by the scale (`render_scale::to_engine`, applied in
   `input.rs` at the four mouse entry points and the AGDK mouse and scroll
   paths), and a text box rectangle it reports is divided by the scale before
   the GTK editor is placed over it (`wayland.rs`, `update_text_overlay`).

Nothing here reads or writes the engine's memory or code. The engine calls
function pointers Cordial gave it and Cordial answers at that boundary, which
is the reading ADR-049 gives for the ETC2 shim. ADR-001 and ADR-003 are not
touched, and no plugin is handed a swapchain: if a setting is exposed it is a
scale value, the effect and not the channel (ADR-007).

## What it costs

On a signed-out client on an Intel iGPU (three runs per arm, input driven throughout): the frame interval stayed at the output's 60 Hz in every arm, so no frame rate gain was measured; the render engine's busy share fell from 14.9% to 11.7% at 67% and 10.5% at 50% in a 1280x754 window, and from 40.9% to 33.9% and 27.4% at 2560x1554; the pass costs 0.55 to 0.6 ms at 1280x754 and 1.7 to 1.9 ms at 2560x1554 (GSR), a 2D page throughout. Details and the controls are in the analysis note. In kind: one extra image of the
render size per swapchain image; one queue submission, one full-screen draw and
one semaphore per frame; the real swapchain now needs `COLOR_ATTACHMENT` and
`TRANSFER_SRC` usage; nothing at all when the variable is unset (the hooks that
would add anything are only handed out when it is set).

## The consequence a user will see

**The interface gets bigger.** The engine takes the reported extent as the size
of its screen, so at 67% a sign-in form laid out for a 1280 x 754 window is
laid out for 858 x 505 and drawn 1.49 times larger. At 67% the sign-in page no
longer fits the window. This is the same as running a lower display
resolution, and for a game with offset-sized GUIs the same thing happens.
Cordial cannot ask the engine for a bigger interface scale without an engine
interface it does not have, and drawing the interface at full size would need
the engine's own separation of scene and interface.

## What was not done, and why

- **FidelityFX FSR 1 (EASU + RCAS, MIT) was not ported.** It is two passes and
  an intermediate image against GSR's one, which makes it the heavier design
  for the same position in the pipeline. It is a fine second filter if GSR's
  edge-adaptive look is disliked; the pass structure here would take it.
- **RAVU (mpv-prescalers) is not proposed.** It is LGPL-3.0-or-later, which can
  be combined into a GPL-3.0-or-later work, so licence is not the obstacle. The
  shaders are mpv user-shader hooks with their weights in lookup tables of about
  3.7 MB for one variant, written for video at 2x, and its own README warns that
  the zoom variants are slow. Porting one to a present pass is a project, not a
  prototype.
- **Touch contacts are not mapped.** There is no touchscreen to test with.
- **Dynamic resolution by the engine itself was not tried.** The engine ships
  an upscale pass and flags with names that say so (`FFlagAutomaticDRS4`,
  `FFlagRenderDynamicResolutionScale12`, `FFlagRenderWatermarkInUpscalePass`).
  If those render the 3D scene smaller and the interface at full size, they
  would avoid the one consequence above. That needs a 3D scene, which a
  signed-out client cannot reach. `INFERRED`, untested.

## Known deviations and risks

- The engine ends every frame with its image in `PRESENT_SRC_KHR`, a layout the
  specification allows only on a presentable image, and the proxies are not.
  The pass treats the proxy as being in it and puts it back. Mesa accepted
  this; no other driver has run it, and a validation layer will complain.
- The pass runs on the engine's queue family as recorded at `vkCreateDevice`
  (the first queue create info) and is submitted on the queue the engine
  presents on. An engine that presents on a queue of another family would break
  it.
- A failure building the pass fails the swapchain creation rather than showing
  the engine's small image in a corner.
- The X11 path has not been run; the measurements are Wayland.
- Only one swapchain per present is handled.

## If accepted

A Settings control (Graphics, beside the frame rate limit) and a `Cordial`
flag-layer key like `CordialPresentMode`, so a built-in plugin can offer it
without touching Vulkan; the environment variable stays as the override. The
unset path must stay byte-identical, which the prototype's control arm shows
only for the behaviour it measured.
