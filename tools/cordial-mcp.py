#!/usr/bin/env python3
"""Cordial Development MCP -- drive and inspect a running Cordial from an agent.

Why this exists, in one paragraph, because it is the whole justification:
debugging Cordial has been bottlenecked on instruments rather than on
reasoning. On 2026-08-21 a rendering bug consumed most of a session and every
wrong turn in it came from a broken measurement -- `mainWorkCallback` read as a
per-frame heartbeat when it fires exactly twice in healthy runs too,
`onFlagsLoaded`'s byte count taken for a delivery readout when it is a
constant, present counts collapsing to 1.0/s from the idle throttle whenever
input was not being driven. Worse, nothing on the host could photograph a
Wayland window at all, so every visual check ended with a human being asked to
look at the screen and describe it. This server closes both gaps: it takes
screenshots out of Cordial's own swapchain, it drives Cordial's own input, and
it attaches a debugger.

What it deliberately does not do. There is no semantic access to Roblox's UI --
no "click the Play button", no element tree. That is not squeamishness: Roblox
exposes no accessibility tree on Android, measured four ways on 2026-08-21 and
recorded in `native/accessibility.cpp`, so there is nothing to read; and
obtaining one would mean engine introspection, which ADR-001 and ADR-003 place
permanently out of scope. This works in coordinates and pixels, which is what a
human tester has.

Input never goes through the compositor. `XTestFake*`, `ydotool`,
`wlr-virtual-keyboard` and the RemoteDesktop portal all land on whatever has
focus, which is the developer's own session, and one of them has already
hijacked a cursor mid-session. Every call here goes to Cordial's own
`input::pass_*` entry points through its control socket.

Run Cordial with `CORDIAL_DEV_CONTROL=1` and point this at the profile:

    tools/cordial-mcp.py --profile-root ~/.local/share/cordial

Speaks MCP over stdio with no third-party dependencies, because a debugging aid
that needs its own virtualenv is one more thing to be broken when you are
already stuck.
"""

import base64
import glob
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

PROTOCOL_VERSION = "2024-11-05"


def log(msg):
    """Diagnostics go to stderr; stdout is the protocol and must stay clean."""
    print(f"[cordial-mcp] {msg}", file=sys.stderr, flush=True)


# --------------------------------------------------------------- the socket

class Cordial:
    """One line in, one line out, against Cordial's `devctl` socket."""

    def __init__(self, path=None, profile_root=None):
        self.explicit = path
        self.profile_root = profile_root or os.path.expanduser("~/.local/share/cordial")

    def find_socket(self):
        if self.explicit:
            return self.explicit if os.path.exists(self.explicit) else None
        # Newest wins: several profiles may have stale sockets from killed
        # runs, and the one worth talking to is the one most recently bound.
        found = glob.glob(os.path.join(self.profile_root, "**", "devctl.sock"), recursive=True)
        found = [p for p in found if os.path.exists(p)]
        if not found:
            return None
        return max(found, key=lambda p: os.stat(p).st_mtime)

    def send(self, line, timeout=10.0):
        path = self.find_socket()
        if not path:
            raise RuntimeError(
                "no devctl socket found. Start Cordial with CORDIAL_DEV_CONTROL=1, "
                f"or pass --socket. Searched under {self.profile_root}"
            )
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(timeout)
        try:
            s.connect(path)
            s.sendall((line + "\n").encode())
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = s.recv(4096)
                if not chunk:
                    break
                buf += chunk
        finally:
            s.close()
        reply = buf.decode(errors="replace").strip()
        if reply.startswith("err "):
            raise RuntimeError(reply[4:])
        return reply[3:].strip() if reply.startswith("ok") else reply

    def pid(self):
        """The pid, from `info`, so the debugger tools need no separate search."""
        for field in self.send("info").split():
            if field.startswith("pid="):
                return int(field.split("=", 1)[1])
        raise RuntimeError("info did not report a pid")


# ------------------------------------------------------------------- gdb

# Preference order, and the paths each may live at. Homebrew first because this
# host is immutable ostree and that is the route that works without a reboot.
DEBUGGERS = (
    ("lldb", ("/home/linuxbrew/.linuxbrew/bin/lldb", "lldb")),
    ("gdb", ("/home/linuxbrew/.linuxbrew/bin/gdb", "gdb")),
)


def resolve_debugger(candidates):
    """First of `candidates` that exists, as an absolute path, or None."""
    for c in candidates:
        found = shutil.which(c) or (c if os.path.exists(c) else None)
        if found:
            return found
    return None


def debugger():
    """Which debugger to drive, and where it is.

    lldb first. Cordial is a Clang project by necessity -- AOSP's bionic does
    not build with GCC -- so lldb is the toolchain that already has to be
    present, and matching it avoids a second debug-info reader to keep happy.
    It is **not** chosen for speed: attach-and-backtrace was measured at 1.63
    and 1.56 seconds against gdb's 1.64 and 1.65 on this host, which is noise,
    and both resolved Cordial frames to file and line equally well. Anyone
    repeating that measurement should expect a tie rather than a win.

    Homebrew paths first because that is the only route that works here. The
    host is immutable ostree, so `dnf install` needs rpm-ostree and a reboot,
    and a containerised debugger cannot attach at all: rootless podman puts the
    tracer in a user namespace that is not an ancestor of the tracee's, which
    no combination of --privileged, --pid=host and SYS_PTRACE repairs.
    """
    for kind, cands in DEBUGGERS:
        found = resolve_debugger(cands)
        if found:
            return kind, found
    return None, None


def run_debugger(pid, commands, timeout=60, kind_hint=None):
    """Run a batch of debugger commands against a live pid and return the text.

    `commands` are that debugger's own commands. The canned tools below phrase
    theirs for whichever one is present; anything passed straight through by
    `cordial_lldb` is the caller's to get right.
    """
    kind, path = debugger()
    # `kind_hint` was accepted and ignored until 2026-08-22, when `tool_backtrace`
    # needed to ask for gdb specifically after lldb returned a stack it could not
    # unwind. Honoured now: name a debugger and you get that one, or a plain
    # refusal saying it is absent -- never a silent substitution, because a
    # caller asking for a particular debugger is doing so precisely because the
    # other one just failed it.
    if kind_hint and kind_hint != kind:
        wanted = dict(DEBUGGERS).get(kind_hint)
        found = resolve_debugger(wanted) if wanted else None
        if not found:
            return f"{kind_hint} was asked for specifically and is not available here"
        kind, path = kind_hint, found
    if not path:
        return (
            "No debugger found. On this host the working route is Homebrew, which needs "
            "neither root nor a reboot:\n    brew install llvm    # lldb\n"
            "Do not try rpm-ostree (needs a reboot) or a container (cannot ptrace across "
            "a rootless user namespace)."
        )
    if kind == "lldb":
        argv = [path, "-p", str(pid), "-b"]
        for c in commands:
            argv += ["-o", c]
    else:
        argv = [path, "-p", str(pid), "-batch", "-ex", "set pagination off"]
        for c in commands:
            argv += ["-ex", c]
    try:
        r = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return f"{kind} timed out after {timeout}s"
    out = (r.stdout or "") + (r.stderr or "")
    # Both narrate every thread they attach to; that is never the answer and it
    # buries the part that is.
    noise = ("[New LWP", "[Thread ", "warning: ", "Using host libthread_db")
    keep = [l for l in out.split("\n") if not l.startswith(noise)]
    return f"({kind})\n" + ("\n".join(keep).strip() or "(no output)")


def backtrace_command(frames):
    """The all-threads backtrace, phrased for whichever debugger is present."""
    kind, _ = debugger()
    return (
        [f"thread backtrace all -c {frames}"] if kind == "lldb" else [f"thread apply all bt {frames}"]
    )


# ------------------------------------------------------------------ tools

def shrink_png(path, max_width=900):
    """Return (base64, mime) for the image, downscaled if a scaler exists.

    A 1280x721 capture is about 2.7 MB because the runtime writes PNGs without
    a deflate implementation, and base64 of that is far too much to put through
    a tool result. ImageMagick shrinks it when present; without it the caller
    still gets the path and can open the file itself, which is worse but not
    broken.
    """
    magick = shutil.which("magick") or shutil.which("convert")
    if not magick:
        return None, None
    tmp = tempfile.mktemp(suffix=".png")
    try:
        subprocess.run(
            [magick, path, "-resize", f"{max_width}x>", "-strip", tmp],
            capture_output=True, timeout=30,
        )
        if not os.path.exists(tmp):
            return None, None
        with open(tmp, "rb") as f:
            return base64.b64encode(f.read()).decode(), "image/png"
    except Exception:
        return None, None
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


TOOLS = [
    {
        "name": "cordial_screenshot",
        "description": (
            "Capture what Roblox actually rendered, read out of Cordial's own Vulkan "
            "swapchain. Unaffected by occlusion, by other windows covering it, or by the "
            "window being off-screen -- this is the drawn frame, not the screen. Returns "
            "the image inline plus the file path. If it reports that no frame was "
            "presented, the engine is wedged, and that is itself the finding."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Where to write the PNG. Defaults to a temporary file."},
                "inline": {"type": "boolean", "description": "Return the image in the result as well as writing it. Default true."},
            },
        },
    },
    {
        "name": "cordial_info",
        "description": (
            "Live counters: presents, commands accepted, swapchain extent, pid. The present "
            "count is the one that separates a frozen client from a slow one -- a wedged "
            "engine leaves it fixed while everything else keeps running. Call it twice a few "
            "seconds apart and compare."
        ),
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "cordial_textbox",
        "description": (
            "What the focused Roblox TextBox contains right now: whether one has focus, "
            "how many characters and bytes are in it, where the caret is, and the box's "
            "geometry. This is the only readback of typed text there is -- the editor is a "
            "GTK widget, and cordial_screenshot photographs the engine's swapchain, which "
            "cannot see a GTK widget at all. The text itself is withheld unless "
            "CORDIAL_TRACE_TEXT_SHOW_PASSWORDS is set on the client, because a focused "
            "field is as often a password box as a search bar; the character count is not, "
            "and it is what catches a key being inserted twice. "
            "It also reports xAlign/yAlign (Roblox's TextXAlignment/TextYAlignment, "
            "confirmed via mocktail's NativeTextBoxInfo constructor field order, "
            "2026-08-30) and the three remaining unnamed constructor slots (i9 i10 i11), "
            "which of them Cordial is reading as the font id (fontSlot, confirmed as slot "
            "9), and the font family that resolved out of it. i10/i11 (textInputType, "
            "returnKeyType) are settled by the same evidence but nothing downstream reads "
            "them yet, so a restyled TextBox in a game and a default box side by side is "
            "still the way to catch a build that renumbers the constructor again."
        ),
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "cordial_loopers",
        "description": (
            "Every ALooper in the process: which thread owns it, how many descriptors it "
            "has registered, and how long since a poll last found anything or anybody woke "
            "it. This is the reading for a client that is stuck rather than slow -- a "
            "backtrace shows `epoll_wait` whether the looper is waiting between events or "
            "waiting for one that can never arrive, and only these numbers tell them apart. "
            "`fds=0` means nothing but a wake can ever make that poll return."
        ),
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "cordial_click",
        "description": (
            "Click at a coordinate inside the Roblox surface. Sends a move, a press and a "
            "release, because the engine tracks pointer position separately from buttons and "
            "a button at a stale position lands elsewhere. Coordinates are surface pixels, "
            "top-left origin -- take a screenshot first and read them off it."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "x": {"type": "number"}, "y": {"type": "number"},
                "button": {"type": "integer", "description": "Android button constant; 1 is primary. Default 1."},
            },
            "required": ["x", "y"],
        },
    },
    {
        "name": "cordial_move",
        "description": "Move the pointer to a coordinate without pressing anything. Also what drives the engine out of its idle throttle.",
        "inputSchema": {
            "type": "object",
            "properties": {"x": {"type": "number"}, "y": {"type": "number"}},
            "required": ["x", "y"],
        },
    },
    {
        "name": "cordial_key",
        "description": (
            "Press and release one key, by Linux evdev keycode (KEY_W is 17, KEY_ENTER 28, "
            "KEY_ESC 1, KEY_SPACE 57). evdev rather than a friendly name because that is the "
            "unit every other caller in the codebase uses, and a second keymap would be a "
            "second thing to keep correct."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "code": {"type": "integer", "description": "evdev keycode"},
                "modifiers": {"type": "integer", "description": "Android meta-state bitmask. Default 0."},
                "hold": {"type": "boolean", "description": "Press without releasing. Pair with release=true later."},
                "release": {"type": "boolean", "description": "Release a key held earlier."},
            },
            "required": ["code"],
        },
    },
    {
        "name": "cordial_text",
        "description": "Send a string through the engine's text path, for search boxes and chat. Not a substitute for cordial_key: this does not produce key events.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "cordial_scroll",
        "description": "Scroll at a coordinate, in wheel detents. Positive is away from the user.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "x": {"type": "number"}, "y": {"type": "number"},
                "detents": {"type": "number", "description": "Notches; 1.0 is one."},
            },
            "required": ["x", "y", "detents"],
        },
    },
    {
        "name": "cordial_fps",
        "description": (
            "Measure the frame rate honestly. Drives pointer motion for the WHOLE measurement "
            "and counts presents across it, then reports the frame rate with the input rate "
            "beside it. This matters: without input the engine's idle throttle drops presents "
            "to exactly 1.0/s after about thirteen seconds, and every frame-rate figure in this "
            "project recorded before 2026-08-02 is that curve integrated. A result with an input "
            "rate near zero is not a frame rate. Expect a hard vsync lock to the output refresh."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "seconds": {"type": "number", "description": "Measurement window. Default 10."},
                "input_hz": {"type": "number", "description": "Pointer motions per second. Default 60."},
            },
        },
    },
    {
        "name": "cordial_backtrace",
        "description": (
            "Attach gdb and dump every thread's stack. The first thing to reach for when the "
            "client is stuck. Read the main thread first: healthy it sits in epoll_wait inside "
            "looper::pump, and the SAME stack appears whether it is spinning at millions of "
            "polls a second or blocked forever -- so always compare the CPU figure this "
            "returns alongside it. A thread in Mutex::lock_contended is usually innocent."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {"frames": {"type": "integer", "description": "Frames per thread. Default 12."}},
        },
    },
    {
        "name": "cordial_debugger",
        "description": (
            "Run arbitrary debugger commands against the running client, in batch, and resume "
            "it afterwards. lldb is used when present (Cordial is a Clang project), gdb "
            "otherwise, and the reply says which -- so phrase the commands for that one. "
            "For lldb: 'frame variable', 'thread list', 'expression'. For gdb: 'info args', "
            "'print', 'info threads'. Use it for anything the canned backtrace omits."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "commands": {"type": "array", "items": {"type": "string"}},
                "timeout": {"type": "integer", "description": "Seconds. Default 60."},
            },
            "required": ["commands"],
        },
    },
]


def tool_screenshot(c, args):
    path = args.get("path") or tempfile.mktemp(prefix="cordial-", suffix=".png")
    desc = c.send(f"screenshot {path}", timeout=15)
    content = [{"type": "text", "text": f"captured {desc}"}]
    if args.get("inline", True):
        data, mime = shrink_png(path)
        if data:
            content.append({"type": "image", "data": data, "mimeType": mime})
        else:
            content[0]["text"] += "\n(no scaler available, so the image is on disk only)"
    return content


def tool_info(c, args):
    return [{"type": "text", "text": c.send("info")}]


def tool_loopers(c, args):
    return [{"type": "text", "text": c.send("loopers").replace(" | ", "\n")}]


def tool_textbox(c, args):
    return [{"type": "text", "text": c.send("textbox")}]


def tool_click(c, args):
    b = int(args.get("button", 1))
    c.send(f"click {float(args['x'])} {float(args['y'])} {b}")
    return [{"type": "text", "text": f"clicked ({args['x']}, {args['y']}) button {b}"}]


def tool_move(c, args):
    c.send(f"move {float(args['x'])} {float(args['y'])}")
    return [{"type": "text", "text": f"moved to ({args['x']}, {args['y']})"}]


def tool_key(c, args):
    code = int(args["code"])
    mods = int(args.get("modifiers", 0))
    if args.get("hold"):
        c.send(f"key down {code} {mods}")
        return [{"type": "text", "text": f"holding evdev {code}"}]
    if args.get("release"):
        c.send(f"key up {code} {mods}")
        return [{"type": "text", "text": f"released evdev {code}"}]
    c.send(f"tap {code} {mods}")
    return [{"type": "text", "text": f"tapped evdev {code}"}]


def tool_text(c, args):
    c.send(f"text {args['text']}")
    return [{"type": "text", "text": f"sent {len(args['text'])} characters"}]


def tool_scroll(c, args):
    c.send(f"scroll {float(args['x'])} {float(args['y'])} {float(args['detents'])}")
    return [{"type": "text", "text": "scrolled"}]


def process_cpu(pid, window=1.0):
    """Percent CPU over a short window.

    Returned beside every backtrace because a stack alone cannot tell a
    spinning pump from a blocked one -- they are byte-identical -- and reading
    that distinction backwards has already cost this project an afternoon.
    """
    def ticks():
        with open(f"/proc/{pid}/stat") as f:
            parts = f.read().rsplit(")", 1)[1].split()
        return int(parts[11]) + int(parts[12])
    hz = os.sysconf("SC_CLK_TCK")
    a = ticks(); t0 = time.monotonic()
    time.sleep(window)
    return 100.0 * ((ticks() - a) / hz) / (time.monotonic() - t0)


def tool_fps(c, args):
    """Drive input across the whole window, then divide presents by wall time.

    The motion is a small square rather than a single repeated point because
    `pass_mouse_move` derives its delta from the previous position, so sending
    the same coordinate twice reports that the mouse did not move -- which the
    idle throttle reads as idle, which is the thing this measurement exists to
    avoid.
    """
    seconds = float(args.get("seconds", 10))
    hz = float(args.get("input_hz", 60))
    w, h = 640, 360
    for field in c.send("info").split():
        if field.startswith("extent="):
            try:
                dims = field.split("=", 1)[1].split("x")
                w, h = int(dims[0]) // 2, int(dims[1]) // 2
            except Exception:
                pass

    def presents():
        for f in c.send("info").split():
            if f.startswith("presents="):
                return int(f.split("=", 1)[1])
        raise RuntimeError("info did not report a present count")

    start = presents()
    t0 = time.monotonic()
    sent = 0
    step = 1.0 / hz if hz > 0 else 0
    box = [(w - 40, h - 40), (w + 40, h - 40), (w + 40, h + 40), (w - 40, h + 40)]
    while time.monotonic() - t0 < seconds:
        x, y = box[sent % 4]
        try:
            c.send(f"move {x} {y}", timeout=5)
            sent += 1
        except Exception:
            break
        remaining = step - ((time.monotonic() - t0) % step if step else 0)
        if step:
            time.sleep(max(0.0, min(step, remaining)))
    elapsed = time.monotonic() - t0
    end = presents()
    fps = (end - start) / elapsed if elapsed > 0 else 0.0
    input_hz = sent / elapsed if elapsed > 0 else 0.0
    verdict = ""
    if fps < 2.0:
        verdict = ("\nAt about one present a second with input flowing, the engine is not "
                   "throttling -- it is wedged. Take a backtrace.")
    elif input_hz < 5:
        verdict = "\nInput barely moved, so this number is not a frame rate. Ignore it."
    return [{
        "type": "text",
        "text": (
            f"{fps:.1f} presents/s over {elapsed:.1f}s, with input driven at "
            f"{input_hz:.1f}/s throughout ({end - start} presents, {sent} motions)."
            + verdict
        ),
    }]


def unwound(out):
    """Whether a backtrace actually walked past the innermost frame.

    lldb terminates a stack silently rather than reporting that it could not
    unwind, and a one-frame answer looks like a real one to anybody skimming.
    On 2026-08-22 that nearly cost the diagnosis of a live freeze: every thread
    came back as

        frame #0: libc.so.6`__syscall_cancel_arch_end

    and nothing else. The engine was deadlocked between `AudioDevice::close`
    and PipeWire's thread loop, which is only visible three frames up.

    The cause is specific, and worth writing down because "lldb is broken" is
    the wrong lesson. lldb unwinds libc perfectly well from an ordinary
    address: a `sleep(300)` stopped at `__internal_syscall_cancel+126` gives
    five correct frames, one more than gdb managed on the same process. What
    it cannot do is unwind from `__syscall_cancel_arch_end+0` -- a bare,
    zero-size label marking the end of the cancellable-syscall region. At
    offset zero of a symbol with no size there are no function bounds and so no
    unwind plan, and the walk stops. gdb gets past it on its own heuristics.

    Neither debuginfod nor glibc debuginfo is the difference: gdb reports
    debuginfod off, this host has no glibc debuginfo installed, and gdb still
    unwound from `.eh_frame` alone.
    """
    return sum(1 for l in out.split("\n") if "frame #" in l or l.lstrip().startswith("#")) > 2


def tool_backtrace(c, args):
    pid = c.pid()
    frames = int(args.get("frames", 12))
    cpu = process_cpu(pid)
    kind, _ = debugger()
    out = run_debugger(pid, backtrace_command(frames))

    # lldb stays the default -- Cordial is a Clang project by necessity, so it
    # is the toolchain already required. But a stack that did not unwind is not
    # an answer, and handing one back as though it were is exactly the broken
    # instrument this whole tool exists to stop. Retry rather than report it.
    fallback = ""
    if kind == "lldb" and not unwound(out):
        second = run_debugger(pid, [f"thread apply all bt {frames}"], kind_hint="gdb")
        if unwound(second):
            fallback = (
                "\nlldb returned a stack it could not unwind (see `unwound`'s note: a return "
                "address landing on a zero-size label has no unwind plan). Retried with gdb, "
                "which walked it. The lldb attempt follows the gdb one below.\n"
            )
            return [{"type": "text", "text": (
                f"pid {pid}, {cpu:.1f}% CPU over one second.\n"
                "Near 0% with the main thread in epoll_wait means it is blocked, not polling; "
                "a healthy pump spins at millions of polls a second from the same stack.\n"
                + fallback + "\n" + second + "\n\n--- lldb's truncated attempt ---\n" + out
            )}]
        fallback = ("\nlldb could not unwind this stack and gdb did no better, so the frames "
                    "below are all there are -- treat them as incomplete, not as the answer.\n")

    header = (
        f"pid {pid}, {cpu:.1f}% CPU over one second.\n"
        "Near 0% with the main thread in epoll_wait means it is blocked, not polling; "
        "a healthy pump spins at millions of polls a second from the same stack.\n"
        + fallback
    )
    return [{"type": "text", "text": header + "\n" + out}]


def tool_gdb(c, args):
    pid = c.pid()
    return [{"type": "text", "text": run_debugger(pid, list(args["commands"]), int(args.get("timeout", 60)))}]


HANDLERS = {
    "cordial_screenshot": tool_screenshot,
    "cordial_info": tool_info,
    "cordial_textbox": tool_textbox,
    "cordial_loopers": tool_loopers,
    "cordial_click": tool_click,
    "cordial_move": tool_move,
    "cordial_key": tool_key,
    "cordial_text": tool_text,
    "cordial_scroll": tool_scroll,
    "cordial_fps": tool_fps,
    "cordial_backtrace": tool_backtrace,
    "cordial_debugger": tool_gdb,
}


# ----------------------------------------------------------------- protocol

def main():
    sock = None
    profile_root = None
    argv = sys.argv[1:]
    for i, a in enumerate(argv):
        if a == "--socket" and i + 1 < len(argv):
            sock = argv[i + 1]
        elif a == "--profile-root" and i + 1 < len(argv):
            profile_root = argv[i + 1]
    cordial = Cordial(sock, profile_root)
    log(f"ready; profile root {cordial.profile_root}")

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        method, rid = req.get("method"), req.get("id")

        if method == "initialize":
            result = {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "cordial-dev", "version": "0.1.0"},
            }
        elif method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            name = req["params"]["name"]
            args = req["params"].get("arguments") or {}
            handler = HANDLERS.get(name)
            if not handler:
                result = {"content": [{"type": "text", "text": f"unknown tool {name}"}], "isError": True}
            else:
                try:
                    result = {"content": handler(cordial, args)}
                except Exception as e:
                    # Reported as a tool error rather than a protocol error so
                    # the agent sees the reason and can act on it. "No socket"
                    # and "the engine is wedged" are both findings.
                    result = {"content": [{"type": "text", "text": str(e)}], "isError": True}
        elif rid is None:
            continue  # a notification, such as notifications/initialized
        else:
            result = {}

        if rid is not None:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": result}) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
