#!/usr/bin/env python3
"""How evenly do frames actually go out, with input flowing the whole time?

**The complaint this answers is "low fps randomly", which is a tail and not a
mean.** `cordial_fps` divides presents by wall time and reports an average; an
average is exactly the statistic that hides a stall. The frame-pacing ring in
`android/frame_pacing.rs` keeps the last 1024 inter-present intervals, so the
percentiles are already there -- this drives input for long enough to fill that
ring with input-driven frames and then reads them.

**Input has to flow for the whole measurement, and this is not optional.**
Presents run at about 60 a second for thirteen seconds and then drop to exactly
1.0 a second whether anything is wrong or not; that is an idle throttle, and
every frame-rate number this project recorded before 2026-08-02 was that curve
integrated. A p99 of 1000ms on an idle client is the throttle, not a stall, and
reading it as a stall is the mistake this file exists to prevent.

At 60fps the ring holds about seventeen seconds, so the default forty-second
measurement leaves nothing in it but frames drawn while the pointer was moving.

Usage:  tools/frame-pacing-check.py [--profile NAME] [--seconds N] [--idle]
        --idle drives no input, as the control. Expect the throttle.
"""

import argparse, os, re, socket, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools"))
from headless_compositor import Execution, start  # noqa: E402


def send(sock, line):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(15)
    s.connect(sock)
    s.sendall((line + "\n").encode())
    r = s.recv(65536).decode().strip()
    s.close()
    if r.startswith("err "):
        raise RuntimeError(r)
    return r[3:].strip() if r.startswith("ok") else r


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", default="default")
    ap.add_argument("--seconds", type=float, default=40)
    ap.add_argument("--idle", action="store_true", help="the control: no input at all")
    ap.add_argument("--hz", type=float, default=60)
    ap.add_argument("--backend", help="compositor backend (default: first available)")
    ap.add_argument("--direct", action="store_true",
                    help="run on this host rather than in the distrobox")
    args = ap.parse_args()

    out = "/tmp/cordial-pacing"
    os.makedirs(out, exist_ok=True)
    log = os.path.join(out, "client.log")
    sock = os.path.expanduser(f"~/.local/share/cordial/profiles/{args.profile}/devctl.sock")

    for pid in subprocess.run(["pidof", "cordial-run"], capture_output=True, text=True).stdout.split():
        os.kill(int(pid), 9)
    time.sleep(3)

    comp = start(1280, 800, out_dir=out, backend=args.backend,
                 execution=Execution.from_env(direct=args.direct),
                 required=("launch",))
    client = None
    try:
        env = comp.host_env({"GDK_BACKEND": "wayland", "CORDIAL_DEV_CONTROL": "1"})
        apk = os.path.expanduser("~/.var/app/org.vinegarhq.Sober/data/sober/packages/"
                                 "x86_64/com.roblox.client/base.apk")
        client = subprocess.Popen(
            [os.path.join(ROOT, "target/release/cordial-run"),
             "--lib-dir", os.path.expanduser("~/.cache/cordial/lib/x86_64"), "--apk", apk,
             "--host-libc", "--game-activity", "--run", "0", "--profile", args.profile],
            env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)

        for _ in range(120):
            if re.search(r"app ready: (Home|Landing)", open(log, errors="replace").read()):
                break
            time.sleep(1)
        else:
            client.kill()
            sys.exit(f"FAIL: never became ready; see {log}")
        time.sleep(6)

        first = int(re.search(r"presents=(\d+)", send(sock, "info")).group(1))
        if first < 10:
            time.sleep(6)
            if int(re.search(r"presents=(\d+)", send(sock, "info")).group(1)) == first:
                client.kill()
                sys.exit("SKIPPED: this launch hit the startup freeze (docs/NEXT.md §0), "
                         "which is not a pacing result. Run it again.")

        # A small square, not a repeated point: `pass_mouse_move` derives its delta
        # from the previous position, so the same coordinate twice reports no
        # movement -- which the throttle reads as idle, which is the state this
        # measurement exists to stay out of.
        started, sent = time.time(), 0
        box = [(500, 300), (700, 300), (700, 450), (500, 450)]
        while time.time() - started < args.seconds:
            if not args.idle:
                x, y = box[sent % 4]
                send(sock, f"move {x} {y}")
                sent += 1
            time.sleep(1.0 / args.hz)
        elapsed = time.time() - started
        info = send(sock, "info")
    finally:
        if client is not None:
            client.terminate()
            try:
                client.wait(timeout=15)
            except Exception:
                client.kill()
        comp.stop()

    mode = "idle (control)" if args.idle else f"input at {sent / elapsed:.0f}/s"
    print(f"== {mode}, {elapsed:.0f}s")
    print(f"== {info}")
    m = re.search(r"p50=([\d.]+)ms p95=([\d.]+)ms p99=([\d.]+)ms max=([\d.]+)ms", info)
    if m:
        p50, p95, p99, mx = (float(g) for g in m.groups())
        print(f"   p50 {p50:.1f}ms  p95 {p95:.1f}ms  p99 {p99:.1f}ms  max {mx:.1f}ms")
        print(f"   a frame late enough to see is anything past ~33ms; "
              f"p95 is {'over' if p95 > 33 else 'under'} that, p99 is "
              f"{'over' if p99 > 33 else 'under'}")


if __name__ == "__main__":
    main()
