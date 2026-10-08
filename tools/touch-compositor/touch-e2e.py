#!/usr/bin/env python3
"""Drive a touch at Cordial's engine canvas, in a nested headless sway.

Starts a headless sway, puts `wl-touch-proxy.py` between it and the client so
the client believes the seat has a touchscreen, launches `cordial-run` signed
out on its own data root, and injects finger-down/up on the canvas subsurface
(and, as a control, on GTK's own toplevel). It reports whether the client is
still alive after each contact and what Cordial's own touch path logged.

Issue #36: on a real touchscreen the first touch on the canvas killed the
client with SIGSEGV inside libgtk-4. No machine here has one, so this is the
only way the crash is ever seen. Run it before believing a fix, and run it
against a build without the fix first: a test that cannot fail is not a test.

Usage:  tools/touch-compositor/touch-e2e.py --binary target/release/cordial-run
        [--label NAME] [--hold 3] [--no-toplevel]

Exit status: 0 if every contact left the client running and the engine's own
`wl_touch` listener saw the canvas contact, 1 otherwise.
"""

import argparse
import os
import re
import shutil
import signal
import subprocess
import sys
import time

BOX = "cordial"
HERE = os.path.dirname(os.path.abspath(__file__))
PROXY = os.path.join(HERE, "wl-touch-proxy.py")


def sh(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def in_box(cmd):
    return sh(["distrobox", "enter", BOX, "--", "bash", "-lc", cmd])


HOLDERS = os.environ.get("CORDIAL_HOLDER_BIN", "/tmp/cordial-wl-holders")


class Holder:
    """A persistent virtual device inside the nested sway (tools/build-wl-holders.sh)."""

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

    def cmd(self, line, settle=0.3):
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--label", default="run")
    ap.add_argument("--output", default="1280x800")
    ap.add_argument("--hold", type=float, default=3.0, help="seconds to wait after each contact")
    ap.add_argument("--no-toplevel", action="store_true", help="skip the control tap on GTK's own surface")
    ap.add_argument("--env", action="append", default=[], help="extra KEY=VALUE for the client")
    ap.add_argument("--no-pointer", dest="pointer", action="store_false",
                    help="do not start the virtual keyboard and pointer, and skip the pointer checks")
    ap.add_argument("--pointer-only", action="store_true",
                    help="skip the touch contacts; only the pointer checks (the no-touch control)")
    ap.add_argument("--toplevel-only", action="store_true",
                    help="tap only GTK's own surface (shows the crash is specific to the canvas)")
    ap.add_argument("--gdb", action="store_true", help="attach gdb to the client and print a backtrace if it faults")
    a = ap.parse_args()
    W, H = map(int, a.output.split("x"))

    root = os.path.expanduser(f"~/.cache/cordial-agent-touch/{a.label}")
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root + "/data")
    build = os.path.expanduser("~/.cache/cordial-agent-touch/build")
    os.makedirs(build, exist_ok=True)
    builds = os.path.expanduser("~/.cache/cordial/builds")
    if not os.path.isdir(builds) or not os.listdir(builds):
        sys.exit(f"FAIL: no extracted Roblox build under {builds}; start Cordial once so it fetches one")
    src = os.path.join(builds, sorted(os.listdir(builds))[-1])
    for f in ("libroblox.so", "base.apk", "split_config.x86_64.apk"):
        if not os.path.exists(f"{build}/{f}"):
            os.symlink(f"{src}/{f}", f"{build}/{f}")

    # ---- the real compositor
    stamp, cfg = f"{root}/display", f"{root}/sway.cfg"
    open(cfg, "w").write(
        f"xwayland disable\noutput HEADLESS-1 mode {W}x{H}\ndefault_border none\n"
        f"exec sh -c 'printf %s \"$WAYLAND_DISPLAY\" > {stamp}'\n")
    subprocess.Popen(
        ["distrobox", "enter", BOX, "--", "bash", "-lc",
         f"exec env -u WAYLAND_DISPLAY -u DISPLAY WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 "
         f"WLR_HEADLESS_OUTPUTS=1 sway -c {cfg} > {root}/sway.log 2>&1"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    t = time.time() + 30
    while time.time() < t and not (os.path.exists(stamp) and os.path.getsize(stamp)):
        time.sleep(0.2)
    if not os.path.exists(stamp):
        sys.exit(f"FAIL: sway never reported a display; see {root}/sway.log")
    upstream = open(stamp).read().strip()
    sway_pid = None
    for pid in in_box("pidof sway").stdout.split():
        if cfg in in_box(f"tr '\\0' ' ' < /proc/{pid}/cmdline").stdout:
            sway_pid = int(pid)
    print(f"== sway {upstream} {W}x{H} pid {sway_pid}", flush=True)

    # A seat with no keyboard or pointer makes Cordial skip binding them, which
    # would leave the pointer half of this test measuring nothing.
    kbd = ptr = None
    if a.pointer:
        kbd = Holder(f"{HOLDERS}/wl-keyboard-holder", upstream)
        ptr = Holder(f"{HOLDERS}/wl-pointer-holder", upstream, [str(W), str(H)])

    # ---- the proxy that adds the touchscreen
    listen, control = f"cordial-touch-{os.getpid()}", f"{root}/control.sock"
    proxy = subprocess.Popen(
        [sys.executable, PROXY, "serve", "--upstream", upstream, "--listen", listen, "--control", control, "--trace-cursor"],
        stdout=open(f"{root}/proxy.log", "w"), stderr=subprocess.STDOUT)
    t = time.time() + 10
    while time.time() < t and not os.path.exists(control):
        time.sleep(0.1)

    def ctl(*words):
        return sh([sys.executable, PROXY, "send", control, *map(str, words)]).stdout.strip()

    env = dict(os.environ)
    env.update(WAYLAND_DISPLAY=listen, GDK_BACKEND="wayland", CORDIAL_DEV_CONTROL="1",
               CORDIAL_TRACE_TOUCH="1", CORDIAL_TRACE_MOUSE="1", XDG_DATA_HOME=root + "/data")
    env.pop("DISPLAY", None)
    for kv in a.env:
        k, _, v = kv.partition("=")
        env[k] = v
    log_path = f"{root}/client.log"
    client = subprocess.Popen(
        [a.binary, "--lib-dir", build, "--apk", f"{build}/base.apk", "--host-libc",
         "--game-activity", "--run", "0", "--profile", "TouchE2E"],
        env=env, stdout=open(log_path, "w"), stderr=subprocess.STDOUT)
    print(f"== client pid {client.pid}, log {log_path}", flush=True)

    gdb = None
    if a.gdb:
        gdb = subprocess.Popen(
            ["gdb", "-p", str(client.pid), "-batch", "-ex", "set pagination off",
             "-ex", "handle SIGSEGV stop print nopass", "-ex", "continue",
             "-ex", "thread apply all bt 14", "-ex", "kill"],
            stdout=open(f"{root}/gdb.log", "w"), stderr=subprocess.STDOUT)

    failures = []

    def alive():
        return client.poll() is None

    def check(name, ok, detail=""):
        print(f"  {'PASS' if ok else 'FAIL'}  {name} {detail}", flush=True)
        if not ok:
            failures.append(name)

    try:
        t = time.time() + 150
        while time.time() < t:
            if not alive():
                sys.exit(f"FAIL: client exited {client.returncode} before it was ready; see {log_path}")
            if re.search(r"app ready: \w+", open(log_path, errors="replace").read()):
                break
            time.sleep(1)
        else:
            sys.exit("FAIL: never ready")
        time.sleep(8)
        print(f"== {ctl('list')}", flush=True)
        check("the client bound a wl_touch through the proxy", "touch_objects=[" in ctl("list")
              and "touch_objects=[]" not in ctl("list"))

        def contact(name, *args):
            before = len(open(log_path, errors="replace").read())
            reply = ctl("tap", *args)
            time.sleep(a.hold)
            new = open(log_path, errors="replace").read()[before:]
            lines = [l for l in new.splitlines() if "wl_touch" in l or "touch" in l.lower()]
            print(f"  -- {name}: proxy says {reply!r}; client alive={alive()}")
            for l in lines[:12]:
                print(f"       log: {l}")
            return alive(), new

        if a.toplevel_only:
            ok, new = contact("tap on GTK's own toplevel only", 40, 15, "toplevel")
            check("client survives a touch on the toplevel", ok)
            raise SystemExit(0 if ok else 1)
        ok, new = (True, "") if a.pointer_only else contact("tap on the engine canvas", 200, 200, "canvas")
        if not a.pointer_only:
            check("client survives a touch on the canvas", ok,
                  "" if ok else f"(exit status {client.returncode})")
        if ok and not a.pointer_only:
            check("Cordial's own wl_touch listener saw it", bool(re.search(r"wl_touch\.down id=0 x=", new)))
            ok2, _ = contact("second tap, same place", 300, 250, "canvas")
            check("and a second one", ok2)
            if not a.no_toplevel and ok2:
                ok3, new3 = contact("control: tap on GTK's own toplevel", 40, 15, "toplevel")
                check("client survives a touch on the toplevel", ok3)
                check("Cordial ignored the toplevel contact", "wl_touch.down id=0 x=" not in new3)

        if ptr and alive():
            print("\n-- the pointer still works, and still hides the host cursor over the canvas")
            before = len(open(log_path, errors="replace").read())
            proxy_before = len(open(f"{root}/proxy.log", errors="replace").read())
            ptr.cmd(f"move {W // 2} {H // 2 + 60}")       # onto the canvas
            time.sleep(0.5)
            ptr.cmd(f"move {W // 2} 20")                  # across to GTK's header bar
            time.sleep(0.5)
            ptr.cmd(f"move {W // 2} {H // 2 + 60}")       # and back onto the canvas
            time.sleep(1)
            ptr.cmd("down left")
            ptr.cmd("up left")
            time.sleep(1)
            new = open(log_path, errors="replace").read()[before:]
            cur = open(f"{root}/proxy.log", errors="replace").read()[proxy_before:]
            check("client survives pointer motion and a click on the canvas", alive())
            hits = [l for l in new.splitlines() if re.search(r"pointer|mouse|button", l, re.I)]
            for l in hits[:10]:
                print(f"       log: {l}")
            for l in [l for l in cur.splitlines() if "set_cursor" in l][:20]:
                print(f"       {l}")
    finally:
        for h in (kbd, ptr):
            if h:
                h.close()
        if gdb:
            try:
                gdb.wait(timeout=20)
            except Exception:
                gdb.kill()
        if alive():
            client.terminate()
            try:
                client.wait(timeout=15)
            except Exception:
                client.kill()
        proxy.terminate()
        if sway_pid:
            in_box(f"kill {sway_pid}")

    print()
    if failures:
        print(f"FAIL: {len(failures)} check(s): {failures}")
        return 1
    print("PASS: every check held")
    return 0


if __name__ == "__main__":
    sys.exit(main())
