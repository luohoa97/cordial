#!/usr/bin/env python3
"""End-to-end test of Cordial's pointer lock, with real buttons and real keys.

Three behavioural changes to the lock shipped on 2026-09-04 -- Escape becoming
a toggle, the right-drag latch, and the latch's timeout -- and none of them
could be tested by anything that existed:

  * **`cordial_click` and `cordial_key` cannot reach this code.** They call
    `input::script_button` and `input::pass_key_event` directly, one layer
    below the Wayland handlers where the lock decision is made. Driving them
    exercises the engine's input path and nothing of `dispatch_pointer_button`
    or the key handler.

  * **`wl-pointer-holder` only sent `BTN_LEFT`**, which is the one button the
    lock deliberately ignores -- a left drag is how every slider in Roblox's
    own interface works. The right-button camera drag was unreachable.

  * **There was no readback of the lock state at all**, so the only signal was
    a trace line written in the same commit as the behaviour. That tests that
    an `eprintln!` runs. `pointerlock` reports the real thing.

Input is driven inside a nested compositor on its own `WAYLAND_DISPLAY`, which
is the one form AGENTS.md permits -- never at the developer's session.

**The control.** Reverting the two behaviours in place -- Escape one-way, the
latch computed but not honoured -- and re-running with the same instruments
fails exactly three assertions and no others: "a second Escape hands it back",
"and clears the latch", and "the drag's leftover 'true' is disregarded". Run it
before believing a green result; a test that cannot fail is not a test. Note
that "and the latch says so" passes either way, because it reads the
bookkeeping rather than its effect, and "a request still standing after a
second is taken as real" only discriminates in combination with the assertion
before it.

**What this does not establish.** Two things, and neither is small.

The engine's own request needs a signed-in account and a joined experience, so
the false-to-true transition the latch keys on comes from the `fakeenginelock`
seam. Cordial's reaction to that transition is measured here.

**A hand-run first-person session on 2026-09-04 supplied the rest, and it
disagreed with the seam's premise.** On engine 2.736.0.1408, scrolling into
first person took the engine's answer to true and Cordial requested the lock;
Escape dropped it and a second Escape took it back. But holding the right button
in third person left the answer *false* for the whole drag -- so the
false-to-true crossing this file simulates is not something that build was seen
to do. See `wayland::LOCK_WANTED_BEFORE_RIGHT_DRAG`. The seam still tests the
state machine; it no longer stands in for observed engine behaviour.

And a headless nested sway never confirms a constraint: it answers no request
with `locked`, which the protocol permits and gives it no way to announce. So
`confirmed` is false throughout and every assertion is on `requested`, which is
the half Cordial decides. Whether a real compositor then grants these requests
is not tested here and is not what changed.

Usage:  tools/pointer-lock-e2e.py [--profile NAME] [--keep]
"""

import argparse, os, re, socket, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOLDERS = os.environ.get("CORDIAL_HOLDER_BIN", "/tmp/cordial-wl-holders")
BOX = "cordial"
OUT = os.environ.get("CORDIAL_LOCK_E2E_OUT", "/tmp/cordial-lock-e2e")

BUTTON_SECONDARY = 2          # input::BUTTON_SECONDARY
KEY_ESC = 1                   # evdev


def sh(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def in_box(cmd):
    return sh(["distrobox", "enter", BOX, "--", "bash", "-lc", cmd])


class Devctl:
    def __init__(self, path):
        self.path = path

    def send(self, line):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect(self.path)
        s.sendall((line + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(4096)
            if not chunk:
                break
            buf += chunk
        s.close()
        reply = buf.decode(errors="replace").strip()
        if reply.startswith("err"):
            raise RuntimeError(f"devctl {line!r}: {reply}")
        return reply

    def lock(self):
        """`pointerlock` as a dict of strings."""
        reply = self.send("pointerlock")
        return dict(kv.split("=", 1) for kv in reply.split() if "=" in kv)


class Holder:
    def __init__(self, binary, display, args=()):
        self.p = subprocess.Popen(
            ["distrobox", "enter", BOX, "--", "bash", "-lc",
             f"export WAYLAND_DISPLAY={display}; exec {binary} {' '.join(args)}"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        deadline = time.time() + 20
        while time.time() < deadline:
            if self.p.poll() is not None:
                raise RuntimeError(f"{binary} exited: {self.p.stderr.read()}")
            if self.p.stderr.readline().startswith("ready"):
                return
        raise RuntimeError(f"{binary} never became ready")

    def cmd(self, line, settle=0.25):
        self.p.stdin.write(line + "\n")
        self.p.stdin.flush()
        self.p.stdout.readline()
        time.sleep(settle)

    def close(self):
        try:
            self.p.stdin.write("quit\n")
            self.p.stdin.flush()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


class Case:
    """One assertion, recorded pass or fail. Nothing retries and nothing relaxes."""

    def __init__(self):
        self.rows = []

    def check(self, name, got, want):
        ok = got == want
        self.rows.append((ok, name, f"{got!r} (want {want!r})"))
        print(f"  {'PASS' if ok else 'FAIL'}  {name}: {got!r} (want {want!r})")
        return ok

    def note(self, name, value):
        self.rows.append((None, name, str(value)))
        print(f"  ....  {name}: {value}")

    def failed(self):
        return [r for r in self.rows if r[0] is False]


SWAY_CFG = """output HEADLESS-1 mode {w}x{h}
default_border none
exec sh -c 'printf %s "$WAYLAND_DISPLAY" > {stamp}'
"""


def start_sway(width, height):
    stamp, cfg = "/tmp/cordial-lock-e2e-display", "/tmp/cordial-lock-e2e-sway.cfg"
    for f in (stamp, cfg):
        if os.path.exists(f):
            os.unlink(f)
    with open(cfg, "w") as fh:
        fh.write(SWAY_CFG.format(w=width, h=height, stamp=stamp))
    subprocess.Popen(
        ["distrobox", "enter", BOX, "--", "bash", "-lc",
         f"exec env -u WAYLAND_DISPLAY -u DISPLAY WLR_BACKENDS=headless "
         f"WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=1 "
         f"sway -c {cfg} > /tmp/cordial-lock-e2e-sway.log 2>&1"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.time() + 25
    while time.time() < deadline:
        if os.path.exists(stamp) and os.path.getsize(stamp):
            return sway_pid(cfg), open(stamp).read().strip()
        time.sleep(0.2)
    raise RuntimeError("sway never reported a display; see /tmp/cordial-lock-e2e-sway.log")


def sway_pid(cfg):
    for pid in in_box("pidof sway").stdout.split():
        if cfg in in_box(f"tr '\\0' ' ' < /proc/{pid}/cmdline").stdout:
            return int(pid)
    return None


def profile_holder(profile):
    """The pid holding this profile's lock, or None.

    Never `pgrep -f`: the pattern matches the shell running it. `pidof` matches
    the executable, which the engine's thread rename does not touch.
    """
    for pid in sh(["pidof", "cordial-run"]).stdout.split():
        try:
            argv = open(f"/proc/{pid}/cmdline").read().split("\0")
        except OSError:
            continue
        if "--profile" in argv and argv[argv.index("--profile") + 1] == profile:
            return int(pid)
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", default="LockE2E")
    ap.add_argument("--width", type=int, default=1280)
    ap.add_argument("--height", type=int, default=800)
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    os.makedirs(OUT, exist_ok=True)
    binary = os.path.join(ROOT, "target/release/cordial-run")
    lib = os.path.expanduser("~/.cache/cordial/lib/x86_64")
    apk = os.path.expanduser(
        "~/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/"
        "com.roblox.client/base.apk")
    for path in (binary, lib, apk):
        if not os.path.exists(path):
            sys.exit(f"FAIL: missing {path}")
    for tool in ("wl-keyboard-holder", "wl-pointer-holder"):
        if not os.path.exists(f"{HOLDERS}/{tool}"):
            sys.exit(f"FAIL: no {tool} -- run tools/build-wl-holders.sh in the container")
    held = profile_holder(args.profile)
    if held:
        sys.exit(f"FAIL: profile {args.profile} is already open in pid {held}")

    case = Case()
    sway = client = kbd = ptr = None
    log_path = os.path.join(OUT, "client.log")
    try:
        sway, display = start_sway(args.width, args.height)
        print(f"== nested sway on {display}, {args.width}x{args.height}")
        kbd = Holder(f"{HOLDERS}/wl-keyboard-holder", display)
        ptr = Holder(f"{HOLDERS}/wl-pointer-holder", display,
                     [str(args.width), str(args.height)])
        caps = in_box("SWAYSOCK=$(ls -t $XDG_RUNTIME_DIR/sway-ipc.*.sock | head -1) "
                      "swaymsg -t get_seats -r").stdout
        if '"capabilities": 0' in caps or '"keyboards": 0' in caps:
            sys.exit("FAIL: the seat has no devices; every reading below would be "
                     "taken with Cordial's own input paths switched off")

        env = dict(os.environ)
        env.update(WAYLAND_DISPLAY=display, GDK_BACKEND="wayland",
                   CORDIAL_DEV_CONTROL="1", CORDIAL_TRACE_MOUSE="1")
        log = open(log_path, "w")
        client = subprocess.Popen(
            [binary, "--lib-dir", lib, "--apk", apk, "--host-libc",
             "--game-activity", "--run", "0", "--profile", args.profile],
            env=env, stdout=log, stderr=subprocess.STDOUT)
        print(f"== client pid {client.pid}, log {log_path}")

        deadline = time.time() + 120
        ready = None
        while time.time() < deadline:
            if client.poll() is not None:
                sys.exit(f"FAIL: client exited {client.returncode} before ready; see {log_path}")
            m = re.search(r"app ready: (\w+)", open(log_path, errors="replace").read())
            if m:
                ready = m.group(1)
                break
            time.sleep(1)
        if not ready:
            sys.exit(f"FAIL: the app shell never became ready; see {log_path}")
        print(f"== app ready: {ready}")
        time.sleep(10)

        dev = Devctl(os.path.expanduser(
            f"~/.local/share/cordial/profiles/{args.profile}/devctl.sock"))
        print(f"== {dev.send('info')}")
        run_cases(case, dev, kbd, ptr, args)
    finally:
        for h in (kbd, ptr):
            if h:
                h.close()
        if client and client.poll() is None:
            client.terminate()
            try:
                client.wait(timeout=15)
            except Exception:
                client.kill()
        if sway and not args.keep:
            in_box(f"kill {sway}")

    bad = case.failed()
    print()
    if bad:
        print(f"FAIL: {len(bad)} assertion(s) failed")
        for _, name, detail in bad:
            print(f"  - {name}: {detail}")
        return 1
    print("PASS: every assertion held")
    return 0


def centre(ptr, args):
    ptr.cmd(f"move {args.width // 2} {args.height // 2 + 60}")


def run_cases(case, dev, kbd, ptr, args):
    # **`requested`, not `confirmed`, and the distinction is the whole test.**
    # Cordial decides whether to ask for the lock; the compositor decides
    # whether to grant it, and a headless nested sway grants nothing -- it
    # never sends `locked` for any request, which the protocol allows and gives
    # it no way to say. A first version of this file asserted on the granted
    # state and reported four failures that were all sway declining. Every
    # decision this file exists to test lands in `requested`.
    print("\n-- the pointer is free before anything touches it")
    centre(ptr, args)
    before = dev.lock()
    case.note("engine's own answer at this screen", before.get("engine"))
    case.note("does this compositor grant constraints at all",
              f"confirmed={before.get('confirmed')}")
    case.check("nothing requested before any input", before.get("requested"), "false")
    case.note("focused", before.get("focused"))
    case.check("the pointer is over the canvas", before.get("on_canvas"), "true")

    print("\n-- a LEFT drag must not take the pointer (the control)")
    ptr.cmd("down left")
    case.check("no request during a left drag", dev.lock().get("requested"), "false")
    ptr.cmd("up left")

    print("\n-- a RIGHT drag asks for it, and the button coming up gives it back")
    ptr.cmd("down right")
    during = dev.lock()
    case.check("requested during a right drag", during.get("requested"), "true")
    case.check("the secondary button is what is down",
               str(int(during.get("buttons", "0")) & BUTTON_SECONDARY), str(BUTTON_SECONDARY))
    ptr.cmd("up right")
    time.sleep(0.5)
    after = dev.lock()
    case.check("released when the drag ends", after.get("requested"), "false")
    case.check("no latch left behind when the engine never asked",
               after.get("awaiting_drag_unlock"), "false")

    print("\n-- the engine's request is honoured while focused, full stop")
    dev.send("fakeenginelock true")          # stand in for first person
    time.sleep(0.5)
    case.check("the engine's request is honoured", dev.lock().get("requested"), "true")

    # **Escape is no longer special.** It used to set a suppression latch here
    # and this block asserted the toggle. Cordial does not second-guess the
    # lock while the window has focus any more -- the compositor owns the way
    # out (Super, an overview) and the protocol requires it to have one. With
    # the engine's answer pinned true by the seam, Escape must change nothing.
    kbd.cmd("key Escape")
    time.sleep(0.5)
    case.check("Escape does not override a focused engine request",
               dev.lock().get("requested"), "true")
    kbd.cmd("key Escape")
    time.sleep(0.5)
    case.check("and a second Escape does not either", dev.lock().get("requested"), "true")

    print("\n-- a right drag that ends must not inherit the engine's stale 'true'")
    dev.send("fakeenginelock false")
    time.sleep(0.5)
    case.check("nothing requested with the engine quiet", dev.lock().get("requested"), "false")
    ptr.cmd("down right")
    case.check("the drag itself asks", dev.lock().get("requested"), "true")
    # Exactly what Roblox does: the answer turns true on the press itself.
    dev.send("fakeenginelock true")
    time.sleep(0.3)
    ptr.cmd("up right")
    time.sleep(0.3)
    latched = dev.lock()
    case.check("the drag's leftover 'true' is disregarded", latched.get("requested"), "false")
    case.check("and the latch says so", latched.get("awaiting_drag_unlock"), "true")

    print("\n-- but the latch lets go rather than wedging")
    time.sleep(1.5)
    freed2 = dev.lock()
    case.check("a request still standing after a second is taken as real",
               freed2.get("requested"), "true")
    case.check("and the latch is gone", freed2.get("awaiting_drag_unlock"), "false")
    dev.send("fakeenginelock clear")


if __name__ == "__main__":
    sys.exit(main())
