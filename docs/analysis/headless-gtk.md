# Photographing the launcher without a screen

**Nothing in this repository can currently take a picture of Cordial's own GTK
windows.** `cordial_screenshot` reads the engine's Vulkan swapchain, so it sees
the game and cannot see the launcher, Settings, or any dialog -- the same blind
spot that made `cordial_textbox` necessary for typing. Every UI change here has
therefore been checked by asking a human to look, which is slow and which has
already shipped one settings page nobody could read.

This is what was measured trying to fix that on 2026-09-09, so that the next
person does not repeat it. **It does not work yet, and the remaining obstacle is
upstream.**

## Broadway: GTK's own headless backend, and why it does not connect

`broadwayd` is GTK's HTML5 backend. It renders into a web page, which would be
ideal here -- no compositor, no screen, and the result is fetchable over HTTP.

**Fedora's GTK 4.22 does not include the broadway client backend at all.**
`GDK_BACKEND=broadway` produces `Gdk-WARNING: No such backend: broadway` from
GDK itself, and `Gdk.Display.open` returns `None`. The `broadwayd` *binary* is
present, which is misleading: this host can be a broadway server and can never
be a broadway client.

The GNOME Flatpak runtimes do include it. `org.gnome.Sdk//50` (GTK 4.22) and
`runtime/org.gnome.Sdk/x86_64/49` (GTK 4.20) both accept the backend, and a
host-built `cordial-shell` resolves every one of its libraries inside that
sandbox -- `ldd` reports nothing missing -- so running our own binary there
works.

**Then the client and the server disagree about what kind of socket it is.**

- `broadwayd :5` prints `Listening on /run/user/1001/broadway6.socket`, and
  no such file appears. `/proc/net/unix` shows
  `@/run/user/1001/broadway6.socket`: an **abstract** socket.
- The client connects to a **path**. Under strace, with
  `BROADWAY_DISPLAY=:5`:

      connect(5, {sa_family=AF_UNIX, sun_path="/run/user/1001/broadway6.socket"}, 110)
          = -1 ENOENT (No such file or directory)

  No leading `@`, and `ENOENT` rather than the `ECONNREFUSED` an absent
  abstract name would give.

They can never meet, and the symptom is
`Unable to init Broadway server: Could not connect: No such file or directory`
on a perfectly well configured pair. GTK's `main` builds the server socket with
`G_UNIX_SOCKET_ADDRESS_PATH`, so this looks like a defect fixed after both
released versions -- not something to work around permanently.

**Note the off-by-one, which wastes an hour on its own.** `broadwayd :N` creates
`broadway(N+1).socket`, and the client with `BROADWAY_DISPLAY=:N` looks for
`broadway(N+1).socket`. The two agree, so pass the *same* number to both.
Correcting `BROADWAY_DISPLAY` to match the socket's name is the natural mistake
and it breaks a pair that was already right.

## With the sockets bridged, broadwayd aborts

`tools/broadway-relay.py` listens on the path the client wants and forwards to
the abstract name the server bound, carrying `SCM_RIGHTS` because broadway
passes file descriptors over this socket -- five of them in a trivial session.

That gets much further. GTK opens the display, the theme loads, and
`cordial-shell` prints `shell: refresh -- 1 monitor(s)`. Then:

    broadwayd: Gdk-WARNING: Unknown request of type 17
    broadwayd: Gdk:ERROR:../gdk/broadway/broadway-server.c:1651:
               broadway_server_window_update: assertion failed:
               (window->width == cairo_image_surface_get_width (surface))
    client:    _gdk_broadway_events_got_input - Unknown input command d
    client:    Unable to read from broadway server: eof

**A twelve-line GTK control fails identically** -- one `Gtk.Window` holding one
`Gtk.Label`, same assertion, same abort. So this is not Cordial, not
libadwaita, and not the complexity of the settings page.

**What is not established is whether the relay causes it.** The relay is the
only thing between them and it forwards descriptors, so a mis-association of an
fd with the wrong byte would produce exactly this. There is no relay-free
control available, because without the relay the two cannot connect at all.
Treat "broadway is broken in 4.20 and 4.22" as `INFERRED`, not measured.

## What else was ruled out on this host

- **No Xvfb, no Xephyr, no Xnest, no weston, no cage, no labwc.** The host is
  immutable ostree, so adding one needs `rpm-ostree` and a reboot.
- **Containerised X is not obviously better**, and rootless podman has already
  been shown here to be unable to `ptrace` the host, so it is not a free
  environment either (see AGENTS.md on gdb).
- **`org.gnome.Shell.Screenshot` exists on this session bus**, with
  `ScreenshotWindow`, `ScreenshotArea` and `Screenshot`. That would photograph
  the launcher on the developer's own screen with no nested display at all. It
  is not used here and should not be used without asking: it captures the real
  desktop, and `Screenshot` captures all of it.

## If you pick this up

In rough order of how likely each is to end in a working screenshot:

1. **Check whether the relay is the culprit** by making broadwayd bind a path
   socket -- an `LD_PRELOAD` shim over `bind()` is enough, and removes the
   relay from the picture entirely. If broadwayd then survives, this whole
   approach works and only needed six lines.
2. **A newer GTK.** `main` already changed the server's socket type. A runtime
   built past that fix may need no relay and no shim.
3. **Ask, then use `org.gnome.Shell.Screenshot.ScreenshotWindow`.** It is the
   only route here known to be capable today, and its cost is that it
   photographs a real desktop rather than a private one.
