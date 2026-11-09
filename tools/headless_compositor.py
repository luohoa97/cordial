#!/usr/bin/env python3
"""Start a nested compositor for a test run, on a display of its own.

Every instrument in `tools/` used to grow its own copy of the same twenty
lines: a `sway` config, `WLR_BACKENDS=headless`, a stamp file, and a
`distrobox enter` wrapper. That made the *compositor* part of every experiment,
so a result taken under one compositor could not be repeated under another
without rewriting the harness first. This module is the one place that knows
how to start one.

**Why the choice belongs to the harness and not to this file.** Sway is still
the only compositor here that can hold a virtual keyboard and pointer
(`zwp_virtual_keyboard_v1`, `zwlr_virtual_pointer_v1`), so the input tests need
it. KWin has no such globals, but it does have `--virtual`, a screen-shot API
and a scriptable window list. A harness says what it needs; this module picks a
compositor that can provide it, or says in one line which capability is
missing. It never silently runs a weaker experiment.

**What was measured, on 2026-11-09, with `kwin_wayland` 6.8.2 on a KDE
Wayland session:**

- `kwin_wayland --virtual --socket ... --width 1280 --height 800` starts with
  no window on the developer's session and creates its own socket under a
  private `XDG_RUNTIME_DIR`. `alacritty` connected, mapped and was listed.
- A KWin script loaded through `org.kde.kwin.Scripting.loadScript` can read
  `workspace.windowList()` and write `frameGeometry`, `fullScreen` and the
  active window. The first two are asynchronous: a script reads the old value
  immediately after writing, so the driver polls until the change appears.
- `org.kde.KWin.ScreenShot2.CaptureWorkspace` writes a raw RGBX8888 image
  (1280x800, stride 5120, format 16) down a pipe. The call deadlocks if the
  caller waits for its reply before reading the pipe; a reader thread is part
  of the implementation, not an optimisation.
- The older `mutter --headless` path still dies when the engine presents
  (`examples/pointer_capture_probe.rs`), so Mutter is offered as a launch
  backend and not advertised as an equivalent. See
  `docs/analysis/headless-compositors.md`.

Usage from Python:

    from headless_compositor import start
    with start(width=1280, height=800, out_dir="/tmp/my-run",
               required=("launch", "ipc", "screenshot")) as comp:
        print(comp.wayland_display)
        comp.screenshot("/tmp/my-run/shot.png")

Usage from a shell harness:

    python3 tools/headless_compositor.py start --state /tmp/run/comp.json \
        --width 1280 --height 800 --out-dir /tmp/run --no-focus
    eval "$(python3 tools/headless_compositor.py env --state /tmp/run/comp.json)"
    python3 tools/headless_compositor.py exec --state /tmp/run/comp.json -- \
        /tmp/cordial-wl-holders/wl-pointer-holder 1280 800
    python3 tools/headless_compositor.py stop --state /tmp/run/comp.json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time
import zlib
from dataclasses import dataclass
from typing import Iterable, Sequence

# Capabilities a harness can ask for. A backend advertises what it has actually
# been made to do here; "launch" is not enough to run an input test.
LAUNCH = "launch"
IPC = "ipc"                      # list windows and move/focus/fullscreen them
SCREENSHOT = "screenshot"        # photograph the composited output
VIRTUAL_INPUT = "virtual_input"  # wlroots virtual keyboard/pointer globals
OUTPUT_RESIZE = "output_resize"  # change the output mode while running
MULTI_CLIENT = "multi_client"    # host more than the one client it starts

BACKEND_ORDER = ("kwin", "sway", "weston", "mutter")

# How long a compositor may take to create its socket. The first KWin under a
# cold shader cache is the slowest thing here, and a timeout is not a result.
SOCKET_TIMEOUT = 30.0
# KWin and Weston apply a configure on their own event loop; a reading taken in
# the same turn as the write is the old one.
SETTLE_TIMEOUT = 5.0


@dataclass(frozen=True)
class Backend:
    name: str
    executable: str
    capabilities: frozenset[str]
    note: str

    def has(self, capability: str) -> bool:
        return capability in self.capabilities


BACKENDS = {
    "sway": Backend(
        "sway", "sway",
        frozenset({LAUNCH, IPC, SCREENSHOT, VIRTUAL_INPUT, OUTPUT_RESIZE, MULTI_CLIENT}),
        "wlroots headless backend; the only backend here with virtual input"),
    "kwin": Backend(
        "kwin", "kwin_wayland",
        frozenset({LAUNCH, IPC, SCREENSHOT, MULTI_CLIENT}),
        "KWin virtual backend; window control through KWin scripting, screenshots "
        "through ScreenShot2; no virtual keyboard or pointer globals"),
    "weston": Backend(
        "weston", "weston",
        frozenset({LAUNCH, MULTI_CLIENT}),
        "weston headless backend; no scripting IPC, no screenshot backend and "
        "no virtual input"),
    "mutter": Backend(
        "mutter", "mutter",
        frozenset({LAUNCH, MULTI_CLIENT}),
        "mutter headless; kept for probes, not advertised as a harness: the "
        "engine has been observed to break its socket when it starts presenting"),
}

# cage is deliberately absent. It runs one client as a kiosk and exits with it,
# so it cannot hold a session while the input holders connect before the client
# starts. Its seat also never gains a keyboard, which switches off the code
# `tools/text-input-e2e.py` exists to exercise. Naming it here would invite a
# run that looks like a test and is not one.


def backends_for(required: Iterable[str] = (LAUNCH,)) -> list[Backend]:
    """Backends that provide every capability, in preference order."""
    required = set(required)
    return [BACKENDS[n] for n in BACKEND_ORDER
            if required <= BACKENDS[n].capabilities]


# ---------------------------------------------------------------------------
# Where commands run
# ---------------------------------------------------------------------------

class Execution:
    """The environment commands run in: this host, or the project's distrobox.

    The old harnesses hardcoded `distrobox enter cordial`. That is still the
    default when distrobox is installed, because it is where the build and the
    input holders live, but a host that already has the compositor should not
    have to install a container to use the runner. `CORDIAL_HARNESS_BOX=none`
    (or `--direct`) chooses the host; `CORDIAL_HARNESS_BOX=<name>` chooses a
    named box.
    """

    def __init__(self, box: str | None, forced: bool = False):
        self.box = box
        # `forced` means the caller named this environment. When it did not,
        # `start()` may try the host if the box has no usable compositor: a KDE
        # developer with distrobox installed should not lose the KWin on their
        # own desktop to a container that never had it.
        self.forced = forced
        self._which_cache: dict[str, str | None] = {}

    @classmethod
    def from_env(cls, box: str | None = None, direct: bool = False) -> "Execution":
        env_box = os.environ.get("CORDIAL_HARNESS_BOX")
        if direct or env_box in ("none", "host", ""):
            return cls(None, forced=True)
        if box is not None:
            return cls(box, forced=True)
        if env_box:
            return cls(env_box, forced=True)
        if shutil.which("distrobox"):
            return cls("cordial")
        return cls(None, forced=True)

    def wrap(self, argv: Sequence[str]) -> list[str]:
        if self.box is None:
            return list(argv)
        return ["distrobox", "enter", self.box, "--"] + list(argv)

    def run(self, argv: Sequence[str], *, env: dict | None = None,
            timeout: float | None = None, check: bool = False,
            input: str | None = None) -> subprocess.CompletedProcess:
        return subprocess.run(self.wrap(argv), capture_output=True, text=True,
                              env=env, timeout=timeout, check=check, input=input)

    def popen(self, argv: Sequence[str], *, env: dict | None = None,
              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
              stderr=subprocess.DEVNULL) -> subprocess.Popen:
        return subprocess.Popen(self.wrap(argv), env=env, stdin=stdin,
                                stdout=stdout, stderr=stderr,
                                start_new_session=True)

    def shell(self, command: str, *, env: dict | None = None,
              timeout: float | None = None) -> subprocess.CompletedProcess:
        if self.box is None:
            return subprocess.run(["bash", "-lc", command], capture_output=True,
                                  text=True, env=env, timeout=timeout)
        return subprocess.run(["distrobox", "enter", self.box, "--", "bash", "-lc", command],
                              capture_output=True, text=True, env=env, timeout=timeout)

    def which(self, name: str) -> str | None:
        if self.box is None:
            return shutil.which(name)
        if name not in self._which_cache:
            try:
                r = self.shell(f"command -v {shlex.quote(name)}")
                self._which_cache[name] = r.stdout.strip() or None
            except OSError:
                # A named box that does not exist is reported by the selector
                # as "no compositor", not as a Python traceback.
                self._which_cache[name] = None
        return self._which_cache[name]

    def prefix(self) -> dict:
        return {"box": self.box}

    @classmethod
    def from_prefix(cls, prefix: dict) -> "Execution":
        return cls(prefix.get("box"), forced=True)


@dataclass
class Window:
    id: str
    caption: str
    app_id: str
    active: bool
    fullscreen: bool
    rect: tuple[int, int, int, int]
    pid: int | None = None
    # The client surface, not the frame. Sway's `rect` includes the container
    # and `window_rect` is the surface inside it; KWin's `frameGeometry` does
    # the same against `clientGeometry`. Harnesses that photograph a widget
    # need the surface, because the header bar shifts it down.
    client_rect: tuple[int, int, int, int] | None = None

    @property
    def width(self) -> int:
        return self.rect[2]

    @property
    def height(self) -> int:
        return self.rect[3]


class Unsupported(RuntimeError):
    """The chosen compositor has no way to do this. Says which capability."""

    def __init__(self, backend: str, capability: str, detail: str = ""):
        self.backend = backend
        self.capability = capability
        msg = f"{backend} does not provide `{capability}`"
        if detail:
            msg += f": {detail}"
        super().__init__(msg)


# ---------------------------------------------------------------------------
# Session
# ---------------------------------------------------------------------------

class Session:
    """A running nested compositor and everything started to support it."""

    def __init__(self, backend: Backend, execution: Execution, *, out_dir: str,
                 runtime_dir: str, width: int, height: int, display: str,
                 log_path: str, pid: int | None, state_path: str | None = None,
                 bus_address: str | None = None, bus_pid: int | None = None,
                 bridge_pid: int | None = None, focus_pid: int | None = None,
                 focus_marker: str | None = None,
                 marker: str = "", app_id: str = "Cordial",
                 output_name: str | None = None):
        self.backend = backend
        self.execution = execution
        self.out_dir = out_dir
        self.runtime_dir = runtime_dir
        self.width = width
        self.height = height
        self.wayland_display = display
        self.log_path = log_path
        self.pid = pid
        self.state_path = state_path
        self.bus_address = bus_address
        self.bus_pid = bus_pid
        self.bridge_pid = bridge_pid
        self.focus_pid = focus_pid
        self.focus_marker = focus_marker
        self.marker = marker
        self.app_id = app_id
        self.output_name = output_name
        self._sway_pid: int | None = None
        self._stopped = False

    # -- environment --------------------------------------------------------

    def host_env(self, extra: dict | None = None) -> dict:
        """An environment for a process on the host that should talk to it.

        `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` are the point. `DISPLAY` is
        removed because a client that falls back to X11 would render on the
        developer's session, which is the one thing a nested run must not do.
        """
        env = dict(os.environ)
        env.pop("DISPLAY", None)
        env.pop("GNOME_SETUP_DISPLAY", None)
        env["XDG_RUNTIME_DIR"] = self.runtime_dir
        env["WAYLAND_DISPLAY"] = self.wayland_display
        if extra:
            env.update(extra)
        return env

    def env(self, extra: dict | None = None) -> dict:
        """The compositor's own environment, for a command run inside it."""
        env = self.host_env(extra)
        if self.bus_address:
            env["DBUS_SESSION_BUS_ADDRESS"] = self.bus_address
        return env

    def compositor_env(self, extra: dict | None = None) -> dict:
        """The environment used to *start* a compositor.

        `WAYLAND_DISPLAY` must be absent, not empty: several compositors treat
        an empty value as "connect to the first socket" and nest into the
        developer's session, which is the one failure this module exists to
        prevent.
        """
        env = self.host_env(extra)
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)
        if self.bus_address:
            env["DBUS_SESSION_BUS_ADDRESS"] = self.bus_address
        else:
            env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        return env

    def exports(self) -> dict[str, str]:
        values = {"XDG_RUNTIME_DIR": self.runtime_dir,
                  "WAYLAND_DISPLAY": self.wayland_display}
        if self.bus_address:
            values["DBUS_SESSION_BUS_ADDRESS"] = self.bus_address
        return values

    def enter(self, argv: Sequence[str]) -> list[str]:
        return self.execution.wrap(argv)

    def run(self, argv: Sequence[str], **kw) -> subprocess.CompletedProcess:
        return self.execution.run(argv, env=kw.pop("env", self.env()), **kw)

    def popen(self, argv: Sequence[str], **kw) -> subprocess.Popen:
        return self.execution.popen(argv, env=kw.pop("env", self.env()), **kw)

    def shell(self, command: str, **kw) -> subprocess.CompletedProcess:
        return self.execution.shell(command, env=kw.pop("env", self.env()), **kw)

    def which(self, name: str) -> str | None:
        return self.execution.which(name)

    # -- state --------------------------------------------------------------

    def to_state(self) -> dict:
        return {
            "backend": self.backend.name,
            "execution": self.execution.prefix(),
            "out_dir": self.out_dir,
            "runtime_dir": self.runtime_dir,
            "width": self.width,
            "height": self.height,
            "wayland_display": self.wayland_display,
            "log_path": self.log_path,
            "pid": self.pid,
            "state_path": self.state_path,
            "bus_address": self.bus_address,
            "bus_pid": self.bus_pid,
            "bridge_pid": self.bridge_pid,
            "focus_pid": self.focus_pid,
            "focus_marker": self.focus_marker,
            "marker": self.marker,
            "app_id": self.app_id,
            "output_name": self.output_name,
        }

    def write_state(self, path: str | None = None) -> str:
        path = path or self.state_path
        if not path:
            raise ValueError("no state path")
        self.state_path = path
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        tmp = path + ".tmp"
        with open(tmp, "w") as fh:
            json.dump(self.to_state(), fh, indent=2)
            fh.write("\n")
        os.replace(tmp, path)
        return path

    @classmethod
    def from_state(cls, state: dict) -> "Session":
        return cls(BACKENDS[state["backend"]],
                   Execution.from_prefix(state.get("execution") or {}),
                   out_dir=state["out_dir"], runtime_dir=state["runtime_dir"],
                   width=state["width"], height=state["height"],
                   display=state["wayland_display"], log_path=state["log_path"],
                   pid=state.get("pid"), state_path=state.get("state_path"),
                   bus_address=state.get("bus_address"),
                   bus_pid=state.get("bus_pid"),
                   bridge_pid=state.get("bridge_pid"),
                   focus_pid=state.get("focus_pid"),
                   focus_marker=state.get("focus_marker"),
                   marker=state.get("marker", ""),
                   app_id=state.get("app_id", "Cordial"),
                   output_name=state.get("output_name"))

    # -- windows ------------------------------------------------------------

    def windows(self) -> list[Window]:
        if self.backend.name == "sway":
            return self._sway_windows()
        if self.backend.name == "kwin":
            return self._kwin_windows()
        raise Unsupported(self.backend.name, IPC)

    def ipc(self, args: str, raw: bool = True) -> subprocess.CompletedProcess:
        """Run a compositor's own IPC command where it has one.

        Only sway exposes this. It is kept public for the harnesses that need
        a reading the window model does not carry (seat capabilities, for
        example), and it says `Unsupported` rather than pretending on the
        backends that have no IPC.
        """
        self._require(IPC)
        if self.backend.name == "sway":
            return self._swaymsg(args, raw=raw)
        raise Unsupported(self.backend.name, IPC)

    def seat_capabilities(self) -> dict:
        """What the seat advertises: capabilities bitmask and device counts.

        A harness that needs a real keyboard or pointer asks here before it
        starts, because Cordial reads these once at `open()`. Sway can answer
        through `swaymsg`; the other backends cannot, so this raises rather
        than reporting an optimistic zero or three.
        """
        self._require(IPC)
        if self.backend.name != "sway":
            raise Unsupported(self.backend.name, IPC, "no seat introspection")
        r = self._swaymsg("-t get_seats", raw=True)
        if r.returncode != 0:
            raise RuntimeError(f"swaymsg get_seats failed: {r.stderr.strip()}")
        seats = json.loads(r.stdout)
        if not seats:
            return {"capabilities": 0, "keyboards": 0, "pointers": 0, "touch": 0}
        seat = seats[0]
        devices = seat.get("devices") or []
        return {
            "capabilities": int(seat.get("capabilities", 0)),
            "keyboards": sum(1 for d in devices if d.get("type") == "keyboard")
            if devices else int(seat.get("keyboards", 0)),
            "pointers": sum(1 for d in devices if d.get("type") == "pointer")
            if devices else int(seat.get("pointers", 0)),
            "touch": sum(1 for d in devices if d.get("type") == "touch")
            if devices else int(seat.get("touch", 0)),
        }

    def window(self, *, app_id: str | None = None, caption: str | None = None,
               pid: int | None = None, index: int = 0) -> Window | None:
        found = []
        for w in self.windows():
            if app_id is not None and w.app_id != app_id:
                continue
            if caption is not None and not re.search(caption, w.caption):
                continue
            if pid is not None and w.pid != pid:
                continue
            found.append(w)
        return found[index] if index < len(found) else None

    def activate(self, window: Window) -> None:
        self._require(IPC)
        if self.backend.name == "sway":
            self._swaymsg(f"[con_id={window.id}] focus")
        elif self.backend.name == "kwin":
            self._kwin_command({"op": "activate", "id": window.id})
        else:
            raise Unsupported(self.backend.name, IPC)

    def set_fullscreen(self, window: Window, on: bool) -> None:
        self._require(IPC)
        if self.backend.name == "sway":
            self._swaymsg(f"[con_id={window.id}] fullscreen {'enable' if on else 'disable'}")
        elif self.backend.name == "kwin":
            self._kwin_command({"op": "fullscreen", "id": window.id, "on": on})
            self._wait_window(window.id, lambda w: w.fullscreen == on)
        else:
            raise Unsupported(self.backend.name, IPC)

    def set_geometry(self, window: Window, x: int, y: int, w: int, h: int) -> None:
        self._require(IPC)
        if self.backend.name == "sway":
            self._swaymsg(f"[con_id={window.id}] floating enable, "
                          f"resize set {w} {h}, move position {x} {y}")
        elif self.backend.name == "kwin":
            self._kwin_command({"op": "geometry", "id": window.id,
                                "x": x, "y": y, "w": w, "h": h})
            self._wait_window(window.id, lambda win: win.rect == (x, y, w, h))
        else:
            raise Unsupported(self.backend.name, IPC)

    def close(self, window: Window) -> None:
        self._require(IPC)
        if self.backend.name == "sway":
            self._swaymsg(f"[con_id={window.id}] kill")
        elif self.backend.name == "kwin":
            self._kwin_command({"op": "close", "id": window.id})
        else:
            raise Unsupported(self.backend.name, IPC)

    def set_floating(self, window: Window, on: bool) -> None:
        """Take a window out of (or put it back into) tiling, where it tiles.

        KWin's windows are already free-floating, so this is a no-op there; it
        exists so a harness can talk about the state it wants instead of
        writing `floating enable` only sway understands.
        """
        self._require(IPC)
        if self.backend.name == "sway":
            self._swaymsg(f"[con_id={window.id}] floating {'enable' if on else 'disable'}")
        elif self.backend.name == "kwin":
            return  # every ordinary KWin toplevel is free-floating already
        else:
            raise Unsupported(self.backend.name, IPC)

    def resize_output(self, width: int, height: int) -> None:
        self._require(OUTPUT_RESIZE)
        if self.backend.name != "sway":
            raise Unsupported(self.backend.name, OUTPUT_RESIZE)
        r = self._swaymsg(f"output {self.output_name or 'HEADLESS-1'} mode {width}x{height}")
        if r.returncode != 0:
            raise RuntimeError(f"sway refused the output mode: {r.stderr.strip()}")
        self.width, self.height = width, height

    def output_rect(self) -> tuple[int, int, int, int]:
        """The first active output's rectangle, in the compositor's coordinates."""
        if self.backend.name == "sway":
            r = self._swaymsg("-t get_outputs", raw=True)
            if r.returncode != 0:
                raise RuntimeError(f"swaymsg get_outputs failed: {r.stderr.strip()}")
            for output in json.loads(r.stdout):
                if output.get("active", True):
                    rect = output["rect"]
                    return (int(rect.get("x", 0)), int(rect.get("y", 0)),
                            int(rect.get("width", self.width)),
                            int(rect.get("height", self.height)))
            return (0, 0, self.width, self.height)
        if self.backend.name == "kwin":
            # The virtual backend puts its one output at the origin.
            return (0, 0, self.width, self.height)
        raise Unsupported(self.backend.name, IPC)

    def window_origin(self, pid: int) -> tuple[int, int, int, int, int, int] | None:
        """Where a client's surface starts, plus the output and surface sizes.

        `(x_on_output, y_on_output, output_w, output_h, surface_w, surface_h)`.
        This is the reading the visibility cases need: the editor's rectangle
        is in surface coordinates, and the frame's header bar is between the
        two. Returning `None` means the window was not found, which the caller
        reports rather than photographing empty space.
        """
        self._require(IPC)
        out = self.output_rect()
        win = None
        if self.backend.name == "sway":
            windows = self._sway_windows()
            win = next((w for w in windows if w.pid == pid), None)
            if win is None and len(windows) == 1:
                # An Xwayland client's pid is not always the one sway records.
                win = windows[0]
        elif self.backend.name == "kwin":
            windows = self._kwin_windows()
            win = next((w for w in windows if w.pid == pid), None)
            if win is None and len(windows) == 1:
                win = windows[0]
        if win is None or win.client_rect is None:
            return None
        cx, cy, cw, ch = win.client_rect
        return (cx - out[0], cy - out[1], out[2], out[3], cw, ch)

    def _wait_window(self, window_id: str, predicate) -> None:
        deadline = time.time() + SETTLE_TIMEOUT
        while time.time() < deadline:
            for w in self.windows():
                if w.id == window_id and predicate(w):
                    return
            time.sleep(0.05)
        raise RuntimeError(f"{self.backend.name} did not apply the change to window "
                           f"{window_id} within {SETTLE_TIMEOUT:.0f}s")

    # -- screenshots --------------------------------------------------------

    def screenshot(self, path: str) -> str:
        self._require(SCREENSHOT)
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        ppm = path.lower().endswith(".ppm")
        if self.backend.name == "sway":
            r = self.run(["grim", "-t", "ppm", path] if ppm else ["grim", path])
            if r.returncode != 0:
                raise RuntimeError(f"grim failed: {r.stderr.strip()}")
            return path
        if self.backend.name == "kwin":
            raw = _kwin_capture(self.bus_address)
            if ppm:
                _write_ppm(path, raw["width"], raw["height"], raw["stride"],
                           raw["format"], raw["data"])
            elif raw.get("type") == "png":
                with open(path, "wb") as fh:
                    fh.write(raw["data"])
            else:
                _write_png(path, raw["width"], raw["height"], raw["stride"],
                           raw["format"], raw["data"])
            return path
        raise Unsupported(self.backend.name, SCREENSHOT)

    # -- sway ---------------------------------------------------------------

    def _sway_pid_value(self) -> int:
        if self._sway_pid is not None:
            return self._sway_pid
        if self.pid:
            # The host pid is not the container's pid, so ask inside it.
            r = self.shell("pidof sway")
            for token in r.stdout.split():
                try:
                    pid = int(token)
                except ValueError:
                    continue
                cmd = self.shell(f"tr '\\0' ' ' < /proc/{pid}/cmdline").stdout
                if self.marker and self.marker in cmd:
                    self._sway_pid = pid
                    return pid
            if self.execution.box is None:
                self._sway_pid = self.pid
                return self.pid
        raise RuntimeError("could not find the nested sway process to talk to")

    def _swaymsg(self, args: str, raw: bool = False) -> subprocess.CompletedProcess:
        pid = self._sway_pid_value()
        sock = f"{self.runtime_dir}/sway-ipc.{os.getuid()}.{pid}.sock"
        quoted = shlex.quote(sock)
        return self.shell(f"SWAYSOCK={quoted} swaymsg {'-r ' if raw else ''}{args}")

    def _sway_windows(self) -> list[Window]:
        r = self._swaymsg("-t get_tree", raw=True)
        if r.returncode != 0:
            raise RuntimeError(f"swaymsg get_tree failed: {r.stderr.strip()}")
        tree = json.loads(r.stdout)
        out: list[Window] = []

        def walk(node):
            if node.get("type") in ("con", "floating_con") and (
                    node.get("app_id") or node.get("window_properties")):
                rect = node.get("rect") or {}
                view = node.get("window_rect") or {}
                out.append(Window(
                    id=str(node.get("id")),
                    caption=node.get("name") or "",
                    app_id=(node.get("app_id")
                            or (node.get("window_properties") or {}).get("class", "")),
                    active=bool(node.get("focused")),
                    fullscreen=bool(node.get("fullscreen_mode")),
                    rect=(int(rect.get("x", 0)), int(rect.get("y", 0)),
                          int(rect.get("width", 0)), int(rect.get("height", 0))),
                    pid=int(node["pid"]) if node.get("pid") else None,
                    client_rect=(int(rect.get("x", 0)) + int(view.get("x", 0)),
                                 int(rect.get("y", 0)) + int(view.get("y", 0)),
                                 int(view.get("width", 0)),
                                 int(view.get("height", 0))),
                ))
            for child in node.get("nodes", []) + node.get("floating_nodes", []):
                walk(child)

        for root in (tree,):
            for child in root.get("nodes", []) + root.get("floating_nodes", []):
                walk(child)
        return out

    # -- KWin ---------------------------------------------------------------

    def _require(self, capability: str) -> None:
        if not self.backend.has(capability):
            raise Unsupported(self.backend.name, capability)

    def _kwin_tool(self) -> list[str]:
        # The bus socket is shared, so a qdbus on the host can drive a KWin in
        # the project box and vice versa. Prefer whichever side can see the
        # compositor's environment first.
        for name in ("qdbus6", "qdbus"):
            found = self.which(name)
            if found:
                return [found]
            host = shutil.which(name)
            if host:
                return [host]
        raise Unsupported("kwin", IPC, "qdbus is not installed")

    def _kwin_env(self) -> dict:
        env = self.env()
        return env

    def _kwin_run_script(self, source: str, marker: str) -> dict:
        """Load a KWin script that reports one JSON object back to the bridge."""
        if not self.bridge_pid or not self.bus_address:
            raise Unsupported("kwin", IPC, "the scripting bridge did not start")
        report = os.path.join(self.out_dir, "kwin-report.json")
        for path in (report, report + ".tmp"):
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
        script = os.path.join(self.out_dir, f"kwin-{marker}.js")
        with open(script, "w") as fh:
            fh.write(source.replace("__REPORT__", json.dumps(report)))
        name = f"cordial-{os.getpid()}-{marker}-{time.monotonic_ns()}"
        tool = self._kwin_tool()
        env = self._kwin_env()
        load = subprocess.run(tool + ["org.kde.KWin", "/Scripting",
                                      "org.kde.kwin.Scripting.loadScript", script, name],
                              capture_output=True, text=True, env=env)
        if load.returncode != 0:
            raise RuntimeError(f"KWin refused the script: {load.stderr.strip()}")
        try:
            start = subprocess.run(tool + ["org.kde.KWin", "/Scripting",
                                           "org.kde.kwin.Scripting.start"],
                                   capture_output=True, text=True, env=env)
            if start.returncode != 0:
                raise RuntimeError(f"KWin could not run the script: {start.stderr.strip()}")
            deadline = time.time() + SETTLE_TIMEOUT
            while time.time() < deadline:
                if os.path.exists(report):
                    try:
                        with open(report) as fh:
                            return json.load(fh)
                    except (OSError, json.JSONDecodeError):
                        pass
                time.sleep(0.02)
            raise RuntimeError(f"KWin script {marker} did not report within "
                               f"{SETTLE_TIMEOUT:.0f}s")
        finally:
            # `start` runs every loaded script. Leaving this one loaded would
            # make the next call re-run its side effect as well, which is how
            # a harmless query turns into a second fullscreen request.
            subprocess.run(tool + ["org.kde.KWin", "/Scripting",
                                   "org.kde.kwin.Scripting.unloadScript", name],
                           capture_output=True, text=True, env=env)

    def _kwin_windows(self) -> list[Window]:
        source = """
var out = [];
var ws = workspace.windowList();
for (var i = 0; i < ws.length; i++) {
    var w = ws[i], g = w.frameGeometry, c = w.clientGeometry;
    out.push({id: String(w.internalId), caption: w.caption || "",
              app_id: w.resourceClass || "", active: !!w.active,
              fullscreen: !!w.fullScreen, pid: w.pid,
              rect: [g.x, g.y, g.width, g.height],
              client_rect: [c.x, c.y, c.width, c.height]});
}
callDBus("io.github.cordial.Headless", "/", "", "Report",
         JSON.stringify({windows: out}));
"""
        data = self._kwin_run_script(source, "windows")
        rows = []
        for row in data.get("windows", []):
            rows.append(Window(id=row["id"], caption=row.get("caption", ""),
                               app_id=row.get("app_id", ""),
                               active=bool(row.get("active")),
                               fullscreen=bool(row.get("fullscreen")),
                               rect=tuple(row.get("rect") or (0, 0, 0, 0)),
                               pid=row.get("pid"),
                               client_rect=tuple(row["client_rect"])
                               if row.get("client_rect") else None))
        return rows

    def _kwin_command(self, command: dict) -> None:
        payload = json.dumps(command)
        source = f"""
var command = {payload};
var ws = workspace.windowList();
var w = null;
for (var i = 0; i < ws.length; i++) {{
    if (String(ws[i].internalId) === String(command.id)) {{ w = ws[i]; break; }}
}}
if (!w) {{
    callDBus("io.github.cordial.Headless", "/", "", "Report",
             JSON.stringify({{error: "window not found"}}));
}} else {{
    if (command.op === "activate") {{
        workspace.activeWindow = w;
    }} else if (command.op === "fullscreen") {{
        w.fullScreen = !!command.on;
    }} else if (command.op === "geometry") {{
        w.frameGeometry = {{x: command.x, y: command.y,
                            width: command.w, height: command.h}};
    }} else if (command.op === "close") {{
        w.closeWindow();
    }}
    callDBus("io.github.cordial.Headless", "/", "", "Report", JSON.stringify({{ok: true}}));
}}
"""
        data = self._kwin_run_script(source, f"cmd-{command['op']}")
        if data.get("error"):
            raise RuntimeError(f"KWin: {data['error']}")

    # -- focus --------------------------------------------------------------

    def hold_focus(self) -> None:
        """Put a harmless window in front so a later client never takes focus.

        KWin's default policy gives focus to the first window that maps. The
        client under test must map second, so this starts before it. Sway does
        this with a `no_focus` rule instead and does not need a holder.
        """
        if self.backend.name == "sway":
            return
        if self.backend.name != "kwin":
            return
        self._require(IPC)
        candidates = [
            ("alacritty", ["alacritty", "--class", "CordialFocusHolder",
                           "-e", "sleep", "3600"]),
            ("zenity", ["zenity", "--info", "--title=Cordial headless",
                        "--text=Focus holder. Safe to close.", "--no-wrap"]),
        ]
        available = [(name, argv) for name, argv in candidates if self.which(name)]
        if not available:
            raise Unsupported("kwin", IPC,
                              "no focus-holder client found (alacritty or zenity)")
        errors = []
        for name, argv in available:
            before = {w.id for w in self.windows()}
            log = open(os.path.join(self.out_dir, "focus-holder.log"), "w")
            proc = self.popen(argv, stdout=log, stderr=subprocess.STDOUT)
            self.focus_pid = proc.pid
            self.focus_marker = ("CordialFocusHolder" if name == "alacritty"
                                 else "Cordial headless")
            deadline = time.time() + 10.0
            while time.time() < deadline:
                if proc.poll() is not None:
                    errors.append(f"{name} exited with {proc.returncode}")
                    break
                new = [w for w in self.windows() if w.id not in before]
                if new:
                    self.activate(new[0])
                    return
                time.sleep(0.1)
            else:
                errors.append(f"{name} never mapped a window")
            _kill_pid(self.focus_pid)
            self.focus_pid = None
        raise RuntimeError("no focus holder could map: " + "; ".join(errors))

    # -- teardown -----------------------------------------------------------

    def stop(self) -> None:
        if self._stopped:
            return
        self._stopped = True
        for pid in (self.focus_pid, self.bridge_pid):
            _kill_pid(pid)
        if self.focus_marker:
            # In a box the pid above is the `distrobox enter` wrapper; the
            # holder itself is in the container's pid namespace. Match the
            # class/title it was started with instead of the wrapper. A box
            # that has gone away must not stop the host-side cleanup below.
            try:
                self.shell(
                    "for p in $(pidof alacritty zenity 2>/dev/null); do "
                    "tr '\\0' ' ' < /proc/$p/cmdline 2>/dev/null | "
                    f"grep -qF -- {shlex.quote(self.focus_marker)} && kill -9 $p; done",
                    env=self.env())
            except Exception:
                pass
        # The compositor may be in a container pid namespace. Kill by its
        # command line marker as well as by the host pid, for the same reason
        # `tools/text-input-e2e.py` stopped killing the `distrobox enter`
        # wrapper: that leaves the compositor holding the display.
        if self.marker:
            names = {"sway": "sway", "kwin": "kwin_wayland",
                     "weston": "weston", "mutter": "mutter"}
            name = names.get(self.backend.name)
            if name:
                try:
                    self.shell(
                        f"for p in $(pidof {name} 2>/dev/null); do "
                        f"tr '\\0' ' ' < /proc/$p/cmdline 2>/dev/null | "
                        f"grep -qF -- {shlex.quote(self.marker)} && kill -9 $p; done",
                        env=self.env())
                except Exception:
                    pass
        _kill_pid(self.pid)
        _kill_pid(self.bus_pid)
        if self.state_path:
            try:
                os.unlink(self.state_path)
            except FileNotFoundError:
                pass

    def __enter__(self) -> "Session":
        return self

    def __exit__(self, *exc) -> None:
        self.stop()


# ---------------------------------------------------------------------------
# Starting
# ---------------------------------------------------------------------------

def start(width: int = 1280, height: int = 800, *, out_dir: str,
          backend: str | None = None, required: Iterable[str] = (LAUNCH,),
          execution: Execution | None = None, no_focus: bool = False,
          app_id: str = "Cordial", state_path: str | None = None,
          clean: bool = True, extra_config: str = "") -> Session:
    """Start a nested compositor and return a session for it."""
    execution = execution or Execution.from_env()
    required = tuple(required)
    try:
        backend_name = _choose_backend(execution, backend, required)
    except SystemExit:
        if execution.forced or execution.box is None:
            raise
        host = Execution(None, forced=True)
        backend_name = _choose_backend(host, backend, required)
        execution = host
    spec = BACKENDS[backend_name]
    out_dir = os.path.abspath(out_dir)
    os.makedirs(out_dir, exist_ok=True)
    runtime_dir = os.path.join(out_dir, "runtime")
    if clean and os.path.isdir(runtime_dir):
        shutil.rmtree(runtime_dir, ignore_errors=True)
    os.makedirs(runtime_dir, mode=0o700, exist_ok=True)
    os.chmod(runtime_dir, 0o700)
    log_path = os.path.join(out_dir, f"{backend_name}.log")
    marker = f"--cordial-headless-{os.path.basename(out_dir)}-{os.getpid()}-{time.monotonic_ns()}"
    session = Session(spec, execution, out_dir=out_dir, runtime_dir=runtime_dir,
                      width=width, height=height, display="", log_path=log_path,
                      pid=None, state_path=state_path, marker=marker, app_id=app_id)
    try:
        if backend_name == "sway":
            _start_sway(session, extra_config, no_focus)
        elif backend_name == "kwin":
            _start_kwin(session, no_focus)
            if no_focus:
                session.hold_focus()
        elif backend_name == "weston":
            _start_weston(session)
        elif backend_name == "mutter":
            _start_mutter(session)
        else:  # pragma: no cover - _choose_backend only returns known names
            raise AssertionError(backend_name)
        if not session.wayland_display:
            raise RuntimeError(f"{backend_name} did not report a display; see {log_path}")
    except Exception:
        session.stop()
        raise
    if state_path:
        session.write_state(state_path)
    return session


def _choose_backend(execution: Execution, explicit: str | None,
                    required: tuple[str, ...]) -> str:
    explicit = explicit or os.environ.get("CORDIAL_HEADLESS_COMPOSITOR")
    if explicit:
        if explicit not in BACKENDS:
            raise SystemExit(f"unknown compositor `{explicit}`; choose from "
                             f"{', '.join(BACKEND_ORDER)}")
        spec = BACKENDS[explicit]
        missing = [c for c in required if not spec.has(c)]
        if missing:
            raise SystemExit(f"{explicit} cannot run this harness: it provides no "
                             f"{', '.join(missing)}. Backends that do: "
                             + ", ".join(b.name for b in backends_for(required)))
        if not execution.which(spec.executable):
            hint = (f" inside the `{execution.box}` box" if execution.box else "")
            raise SystemExit(f"{explicit} needs `{spec.executable}`, which is not "
                             f"on PATH{hint}")
        return explicit
    for spec in backends_for(required):
        if execution.which(spec.executable):
            return spec.name
    names = ", ".join(f"{b.name} ({b.executable})" for b in backends_for(required))
    raise SystemExit(f"no compositor providing {', '.join(required)} was found; "
                     f"tried {names}. Set CORDIAL_HEADLESS_COMPOSITOR to choose one.")


def sway_config(session: Session, extra_config: str, no_focus: bool) -> str:
    """The sway config for one session, as a string.

    Pure so the two decisions that matter here -- the output size and whether
    an app may take focus -- can be checked without a compositor. `xwayland
    disable` is deliberate: an X11 client here would render into a display the
    harness is not photographing.
    """
    stamp = os.path.join(session.out_dir, "sway-display")
    lines = [
        "xwayland disable",
        f"output HEADLESS-1 mode {session.width}x{session.height}",
        "default_border none",
        "default_floating_border none",
        "focus_follows_mouse no",
    ]
    if no_focus:
        lines.append(f'no_focus [app_id="{session.app_id}"]')
    if extra_config:
        lines.append(extra_config.rstrip())
    # `exec` here is sway's own command line, which runs through a shell, so
    # one level of quoting is enough. Wrapping it in `sh -c '...'` as the old
    # harnesses did meant a path with a space was quoted twice and wrote to a
    # file with a literal backslash in its name.
    lines.append(f"exec printf %s \"$WAYLAND_DISPLAY\" > {shlex.quote(stamp)}")
    return "\n".join(lines) + "\n"


def _start_sway(session: Session, extra_config: str, no_focus: bool) -> None:
    stamp = os.path.join(session.out_dir, "sway-display")
    cfg = os.path.join(session.out_dir, "sway.cfg")
    for path in (stamp, cfg):
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
    with open(cfg, "w") as fh:
        fh.write(sway_config(session, extra_config, no_focus))
    # The config path is unique to this run and appears in sway's argv, so it
    # is the marker cleanup matches on inside the container.
    session.marker = cfg
    log = open(session.log_path, "w")
    env = session.compositor_env()
    env.update({"WLR_BACKENDS": "headless", "WLR_LIBINPUT_NO_DEVICES": "1",
                "WLR_HEADLESS_OUTPUTS": "1"})
    proc = session.execution.popen(
        ["sway", "-c", cfg], env=env, stdout=log, stderr=subprocess.STDOUT)
    session.pid = proc.pid
    deadline = time.time() + SOCKET_TIMEOUT
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"sway exited with {proc.returncode}; see {session.log_path}")
        if os.path.exists(stamp) and os.path.getsize(stamp):
            with open(stamp) as fh:
                session.wayland_display = fh.read().strip()
            return
        time.sleep(0.05)
    raise RuntimeError(f"sway never reported a display; see {session.log_path}")


def _start_kwin(session: Session, no_focus: bool) -> None:
    socket = f"wayland-headless-{os.getpid()}-{int(time.monotonic() * 1000) % 100000}"
    session.wayland_display = socket
    session.marker = socket
    bus = _start_private_bus(session)
    if bus:
        session.bus_address, session.bus_pid = bus
        session.bridge_pid = _start_bridge(session)
    env = session.compositor_env()
    env.update({"KWIN_WAYLAND_NO_PERMISSION_CHECKS": "1"})
    # A nested KWin must not read or rewrite the developer's `kwinrc`. Under a
    # private config root it also becomes possible to set the one focus rule
    # the no-focus arm needs: a new window must not steal focus from the
    # holder, however recently the holder was activated.
    config_home = os.path.join(session.out_dir, "config")
    os.makedirs(config_home, exist_ok=True)
    env["XDG_CONFIG_HOME"] = config_home
    if no_focus:
        with open(os.path.join(config_home, "kwinrc"), "w") as fh:
            fh.write("[Windows]\nFocusPolicy=ClickToFocus\n"
                     "FocusStealingPreventionLevel=4\n")
    log = open(session.log_path, "w")
    # `--virtual` is the headless/framebuffer backend. The socket lives under
    # the private runtime dir, so it cannot be mistaken for the session's own.
    argv = ["kwin_wayland", "--virtual", "--socket", socket,
            "--width", str(session.width), "--height", str(session.height),
            "--scale", "1", "--no-lockscreen", "--no-global-shortcuts"]
    proc = session.execution.popen(argv, env=env, stdout=log, stderr=subprocess.STDOUT)
    session.pid = proc.pid
    deadline = time.time() + SOCKET_TIMEOUT
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"kwin_wayland exited with {proc.returncode}; "
                               f"see {session.log_path}")
        if os.path.exists(os.path.join(session.runtime_dir, socket)):
            break
        time.sleep(0.05)
    else:
        raise RuntimeError(f"kwin_wayland never created {socket}; see {session.log_path}")
    # The socket exists before the D-Bus object does. Calling `loadScript` in
    # that gap fails with "Cannot find ... in object /Scripting", which looks
    # like a missing build feature rather than a race.
    if session.bus_address:
        try:
            tool = session._kwin_tool()
        except Unsupported:
            return
        deadline = time.time() + 10.0
        while time.time() < deadline:
            probe = subprocess.run(tool + ["org.kde.KWin", "/Scripting"],
                                   capture_output=True, text=True, env=session.env())
            if probe.returncode == 0 and "Scripting" in probe.stdout:
                return
            time.sleep(0.05)
        raise RuntimeError("KWin's scripting interface never appeared on "
                           f"{session.bus_address}")


def _start_weston(session: Session) -> None:
    socket = f"wayland-headless-{os.getpid()}"
    session.wayland_display = socket
    session.marker = socket
    log = open(session.log_path, "w")
    argv = ["weston", "-B", "headless", "--renderer=pixman", "-S", socket,
            f"--width={session.width}", f"--height={session.height}", "--fake-seat"]
    proc = session.execution.popen(argv, env=session.compositor_env(), stdout=log,
                                   stderr=subprocess.STDOUT)
    session.pid = proc.pid
    deadline = time.time() + SOCKET_TIMEOUT
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"weston exited with {proc.returncode}; "
                               f"see {session.log_path}")
        if os.path.exists(os.path.join(session.runtime_dir, socket)):
            return
        time.sleep(0.05)
    raise RuntimeError(f"weston never created {socket}; see {session.log_path}")


def _start_mutter(session: Session) -> None:
    socket = f"wayland-headless-{os.getpid()}"
    session.wayland_display = socket
    session.marker = socket
    log = open(session.log_path, "w")
    argv = ["mutter", "--headless", "--wayland", f"--wayland-display={socket}",
            f"--virtual-monitor={session.width}x{session.height}"]
    proc = session.execution.popen(argv, env=session.compositor_env(), stdout=log,
                                   stderr=subprocess.STDOUT)
    session.pid = proc.pid
    deadline = time.time() + SOCKET_TIMEOUT
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"mutter exited with {proc.returncode}; "
                               f"see {session.log_path}")
        if os.path.exists(os.path.join(session.runtime_dir, socket)):
            return
        time.sleep(0.05)
    raise RuntimeError(f"mutter never created {socket}; see {session.log_path}")


# ---------------------------------------------------------------------------
# KWin's private bus and scripting bridge
# ---------------------------------------------------------------------------

def _start_private_bus(session: Session) -> tuple[str, int] | None:
    """A session bus for this compositor alone.

    Without one, the nested KWin registers `org.kde.KWin` on the developer's
    real bus and KDE applications start talking to the wrong compositor. The
    address is exported to every child so the scripting calls land here too.
    """
    sock = os.path.join(session.runtime_dir, "bus")
    args = ["--session", "--fork", "--print-address=1", "--print-pid=1",
            f"--address=unix:path={sock}"]
    daemon = session.execution.which("dbus-daemon")
    if daemon:
        r = session.execution.run([daemon] + args, timeout=10)
    else:
        daemon = shutil.which("dbus-daemon")
        if not daemon:
            return None
        r = subprocess.run([daemon] + args, capture_output=True, text=True,
                           timeout=10)
    if r.returncode != 0:
        return None
    lines = [line.strip() for line in r.stdout.splitlines() if line.strip()]
    if len(lines) < 2:
        return None
    return lines[0], int(lines[1])


def _start_bridge(session: Session) -> int | None:
    """A tiny D-Bus service the KWin scripts can radiate results into.

    KWin's script API has no file access; `callDBus` is how a script reports
    anything. The bridge is a separate process because the caller also uses
    the same interpreter for screenshot pipes.
    """
    if not session.bus_address:
        return None
    # The same path `_kwin_run_script` clears and waits on, so the script and
    # the bridge cannot disagree about where a report goes.
    report = os.path.join(session.out_dir, "kwin-report.json")
    ready = os.path.join(session.out_dir, "kwin-bridge.ready")
    for path in (report, ready):
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
    env = session.env()
    log = open(os.path.join(session.out_dir, "kwin-bridge.log"), "w")
    proc = subprocess.Popen([sys.executable, os.path.abspath(__file__),
                             "--bridge", report, "--ready", ready],
                            env=env, stdin=subprocess.DEVNULL, stdout=log,
                            stderr=subprocess.STDOUT, start_new_session=True)
    deadline = time.time() + 5.0
    while time.time() < deadline:
        if os.path.exists(ready):
            return proc.pid
        if proc.poll() is not None:
            return None
        time.sleep(0.02)
    _kill_pid(proc.pid)
    return None


def _bridge_main(report: str, ready: str) -> int:
    try:
        import dbus
        import dbus.service
        from dbus.mainloop.glib import DBusGMainLoop
        from gi.repository import GLib
    except Exception as exc:  # pragma: no cover - host dependent
        print(f"bridge unavailable: {exc}", file=sys.stderr)
        return 1
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName("io.github.cordial.Headless", bus)

    class Bridge(dbus.service.Object):
        @dbus.service.method("io.github.cordial.Headless", in_signature="s",
                             out_signature="")
        def Report(self, payload):
            tmp = report + ".tmp"
            with open(tmp, "w") as fh:
                fh.write(str(payload))
            os.replace(tmp, report)

    Bridge(bus, "/")
    with open(ready, "w") as fh:
        fh.write("ready\n")
    GLib.MainLoop().run()
    return 0


def _kwin_capture(bus_address: str) -> dict:
    """One CaptureWorkspace, drained while the call is still in flight.

    KWin writes the whole image into the pipe passed to the call. Waiting for
    the reply first fills the pipe buffer and deadlocks both ends; measured on
    KWin 6.8.2. The reader thread is the fix, not a precaution.
    """
    try:
        import dbus
        import dbus.types
    except ImportError as exc:
        raise Unsupported("kwin", SCREENSHOT,
                          f"python dbus bindings are missing: {exc}") from exc
    bus = dbus.bus.BusConnection(bus_address)
    obj = bus.get_object("org.kde.KWin", "/org/kde/KWin/ScreenShot2")
    iface = dbus.Interface(obj, "org.kde.KWin.ScreenShot2")
    r, w = os.pipe()
    data = bytearray()

    def drain():
        while True:
            chunk = os.read(r, 65536)
            if not chunk:
                return
            data.extend(chunk)

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    try:
        meta = iface.CaptureWorkspace({"include-cursor": dbus.Boolean(False)},
                                      dbus.types.UnixFd(w), timeout=20)
    finally:
        os.close(w)
    reader.join(timeout=5)
    os.close(r)
    meta = {str(k): meta[k] for k in meta}
    return {
        "type": str(meta.get("type", "raw")),
        "width": int(meta.get("width", 0)),
        "height": int(meta.get("height", 0)),
        "stride": int(meta.get("stride", 0)),
        "format": int(meta.get("format", 0)),
        "data": bytes(data),
    }


# ---------------------------------------------------------------------------
# Utilities
# ---------------------------------------------------------------------------

def _pid_alive(pid: int) -> bool:
    """Whether `pid` is still running, treating a zombie as gone.

    `os.kill(pid, 0)` succeeds for a zombie, and a child we started but did
    not wait for stays one until somebody reaps it. Without this every
    `stop()` spent two seconds per pid waiting for a process that had already
    exited.
    """
    try:
        with open(f"/proc/{pid}/stat") as fh:
            state = fh.read().rsplit(") ", 1)[1].split()[0]
    except (OSError, IndexError):
        return False
    return state != "Z"


def _kill_pid(pid: int | None) -> None:
    if not pid:
        return
    try:
        os.killpg(pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            return
    deadline = time.time() + 2.0
    while time.time() < deadline:
        if not _pid_alive(pid):
            return
        time.sleep(0.05)
    try:
        os.killpg(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def _write_ppm(path: str, width: int, height: int, stride: int,
               fmt: int, data: bytes) -> None:
    """KWin's raw capture as a binary PPM, for the pixel-comparison harness."""
    if fmt not in (16, 17):
        raise RuntimeError(f"KWin returned unsupported QImage format {fmt}")
    if len(data) < stride * height:
        raise RuntimeError("KWin returned a truncated image")
    rgb = bytearray()
    for y in range(height):
        row = data[y * stride:(y + 1) * stride]
        for x in range(width):
            rgb.extend(row[x * 4:x * 4 + 3])
    with open(path, "wb") as fh:
        fh.write(f"P6\n{width} {height}\n255\n".encode())
        fh.write(rgb)


def _write_png(path: str, width: int, height: int, stride: int,
               fmt: int, data: bytes) -> None:
    """Write KWin's raw QImage as PNG without an image library.

    KWin 6.8.2 reports format 16 (RGBX8888) and writes bytes in R,G,B,X order,
    which was measured against a window painted a known colour rather than
    taken from Qt's documentation: the first version swapped red and blue and
    produced a plausible-looking screenshot of the wrong colours. Format 17
    (RGBA8888) is treated the same way. Anything else is reported rather than
    guessed at.
    """
    if fmt not in (16, 17):
        raise RuntimeError(f"KWin returned unsupported QImage format {fmt}")
    if len(data) < stride * height:
        raise RuntimeError("KWin returned a truncated image")
    raw = bytearray()
    for y in range(height):
        row = data[y * stride:(y + 1) * stride]
        raw.append(0)  # PNG filter: none
        for x in range(width):
            raw.extend(row[x * 4:x * 4 + 3])

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (len(payload).to_bytes(4, "big") + tag + payload
                + (zlib.crc32(tag + payload) & 0xFFFFFFFF).to_bytes(4, "big"))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", width.to_bytes(4, "big") + height.to_bytes(4, "big")
                   + bytes((8, 2, 0, 0, 0)))
           + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
           + chunk(b"IEND", b""))
    with open(path, "wb") as fh:
        fh.write(png)


# ---------------------------------------------------------------------------
# Command line
# ---------------------------------------------------------------------------

def _load_state(path: str) -> tuple[dict, Session]:
    with open(path) as fh:
        state = json.load(fh)
    return state, Session.from_state(state)


def _cmd_start(args) -> int:
    if args.state and os.path.exists(args.state):
        try:
            _, old = _load_state(args.state)
            old.stop()
        except Exception:
            pass
    required = tuple(args.require or (LAUNCH,))
    session = start(args.width, args.height, out_dir=args.out_dir,
                    backend=args.backend, required=required,
                    execution=Execution.from_env(box=args.box, direct=args.direct),
                    no_focus=args.no_focus, app_id=args.app_id,
                    state_path=args.state, clean=not args.reuse,
                    extra_config=args.sway_extra or "")
    if args.json:
        print(json.dumps(session.to_state()))
    else:
        print(f"WAYLAND_DISPLAY={session.wayland_display}")
        print(f"XDG_RUNTIME_DIR={session.runtime_dir}")
        if session.bus_address:
            print(f"DBUS_SESSION_BUS_ADDRESS={session.bus_address}")
    return 0


def _cmd_stop(args) -> int:
    if not os.path.exists(args.state):
        return 0
    _, session = _load_state(args.state)
    session.stop()
    return 0


def _cmd_env(args) -> int:
    _, session = _load_state(args.state)
    for key, value in session.exports().items():
        print(f"export {key}={shlex.quote(value)}")
    return 0


def _cmd_exec(args) -> int:
    _, session = _load_state(args.state)
    argv = args.command
    if argv and argv[0] == "--":
        argv = argv[1:]
    if not argv:
        raise SystemExit("exec needs a command after --")
    env = session.env()
    wrapped = session.enter(argv)
    os.execvpe(wrapped[0], wrapped, env)
    return 1


def _cmd_info(args) -> int:
    state, _ = _load_state(args.state)
    print(json.dumps(state, indent=2))
    return 0


def _cmd_screenshot(args) -> int:
    _, session = _load_state(args.state)
    print(session.screenshot(args.path))
    return 0


def _cmd_check(args) -> int:
    execution = Execution.from_env(box=args.box, direct=args.direct)
    print(f"execution: {'host' if execution.box is None else 'box ' + execution.box}")
    for spec in BACKENDS.values():
        found = execution.which(spec.executable)
        caps = ", ".join(sorted(spec.capabilities))
        print(f"{spec.name:<8} {'yes' if found else 'no ':3}  {caps}")
        if found:
            print(f"         {spec.note}")
    return 0


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv[:1] == ["--bridge"]:
        ap = argparse.ArgumentParser()
        ap.add_argument("--bridge", required=True, metavar="REPORT")
        ap.add_argument("--ready", required=True)
        a = ap.parse_args(argv)
        return _bridge_main(a.bridge, a.ready)

    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--box", help="distrobox to run inside (default: auto)")
    ap.add_argument("--direct", action="store_true", help="run on this host")
    sub = ap.add_subparsers(dest="cmd", required=True)

    def execution_flags(parser):
        # Also accepted after the subcommand, because a shell harness builds
        # `start --state ... --direct` and argparse otherwise rejects it.
        # SUPPRESS keeps an absent subcommand flag from overwriting the
        # top-level spelling.
        parser.add_argument("--box", default=argparse.SUPPRESS,
                            help="distrobox to run inside (default: auto)")
        parser.add_argument("--direct", action="store_true",
                            default=argparse.SUPPRESS, help="run on this host")

    p = sub.add_parser("start", help="start a nested compositor")
    execution_flags(p)
    p.add_argument("--width", type=int, default=1280)
    p.add_argument("--height", type=int, default=800)
    p.add_argument("--out-dir", default="/tmp/cordial-headless")
    p.add_argument("--backend", choices=BACKEND_ORDER)
    p.add_argument("--require", action="append",
                   choices=(LAUNCH, IPC, SCREENSHOT, VIRTUAL_INPUT,
                            OUTPUT_RESIZE, MULTI_CLIENT),
                   help="capability the harness needs (repeatable)")
    p.add_argument("--no-focus", action="store_true")
    p.add_argument("--app-id", default="Cordial")
    p.add_argument("--state", help="write the session description here")
    p.add_argument("--json", action="store_true")
    p.add_argument("--reuse", action="store_true",
                   help="keep an existing runtime directory")
    p.add_argument("--sway-extra", help="extra lines for the sway config")
    p.set_defaults(func=_cmd_start)

    p = sub.add_parser("stop", help="stop the compositor in a state file")
    execution_flags(p)
    p.add_argument("--state", required=True)
    p.set_defaults(func=_cmd_stop)

    p = sub.add_parser("env", help="print shell exports for a state file")
    execution_flags(p)
    p.add_argument("--state", required=True)
    p.set_defaults(func=_cmd_env)

    p = sub.add_parser("exec", help="run a command inside the session environment")
    execution_flags(p)
    p.add_argument("--state", required=True)
    p.add_argument("command", nargs=argparse.REMAINDER)
    p.set_defaults(func=_cmd_exec)

    p = sub.add_parser("info", help="print a session state file")
    execution_flags(p)
    p.add_argument("--state", required=True)
    p.set_defaults(func=_cmd_info)

    p = sub.add_parser("screenshot", help="photograph the composited output")
    execution_flags(p)
    p.add_argument("--state", required=True)
    p.add_argument("--path", required=True)
    p.set_defaults(func=_cmd_screenshot)

    p = sub.add_parser("check", help="report which compositors and capabilities exist")
    execution_flags(p)
    p.set_defaults(func=_cmd_check)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
