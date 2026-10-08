# touch-compositor

A touchscreen for machines that do not have one. Used to reproduce and then
fix [#36](https://github.com/luohoa97/cordial/issues/36), a SIGSEGV in
`libgtk-4` on the first touch on the engine canvas.

It is a Wayland protocol proxy rather than a compositor. `wl-touch-proxy.py`
sits between a client and a real compositor (a nested headless sway), forwards
everything, adds the touch bit to `wl_seat.capabilities`, answers
`wl_seat.get_touch` itself, and writes `wl_touch.down/motion/up/frame/cancel`
to the client on command. Building a compositor would have needed the engine's
Vulkan swapchain to work against it first. The proxy needs only Python 3.

| File | What it does |
|---|---|
| `wl-touch-proxy.py` | The proxy, and `send` for its control socket. `--trace-cursor` logs every `wl_pointer.set_cursor`. |
| `touch-e2e.py` | Starts sway, the virtual keyboard and pointer, the proxy and a signed-out `cordial-run` on its own data root, taps the canvas twice and GTK's toplevel once, then moves and clicks the pointer. |

```bash
tools/build-wl-holders.sh                       # once, in the cordial distrobox
tools/touch-compositor/touch-e2e.py --binary target/release/cordial-run
```

It needs an extracted Roblox build under `~/.cache/cordial/builds` (the newest
one is used and symlinked, never copied or modified) and writes everything
else under `~/.cache/cordial-agent-touch/<label>`.

Useful flags: `--gdb` attaches gdb and prints a backtrace if the client faults;
`--pointer-only` skips the touches, as the control for the pointer readings;
`--toplevel-only` shows the fault is specific to the canvas; `--env KEY=VALUE`.

Run it against a build without the fix first. On the unfixed tree the first tap
on the canvas kills the client with SIGSEGV in `gdk_surface_handle_event`, with
or without `CORDIAL_NO_TOUCH=1`, and a tap on the toplevel does not.

What it does not establish: a real touchscreen sends contacts with several
fingers, gestures and a compositor-side grab, and none of that is here. It
shows that a contact on a surface GTK does not own no longer faults, and that
Cordial's own `wl_touch` listener still receives it.

Never point `--upstream` at a desktop session.
