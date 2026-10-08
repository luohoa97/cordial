#!/usr/bin/env python3
"""One arm of the render-scale measurement: a signed-out client in a nested sway.

Why this is not just the MCP: the MCP drives a client somebody already started.
This has to start one per arm, on an environment variable it controls
(`CORDIAL_RENDER_SCALE`), in a compositor of a fixed size, with its own data
roots, so that the switch-off control and the switch-on arm differ in exactly
that one variable. It reuses `text-input-e2e.py`'s way of getting a headless
sway and of talking to the development control socket.

What it records, per arm:

  * the frame-interval summary from `info` (p50, p95, p99, max over the last
    1024 present-to-present intervals) taken after **input has flowed for the
    whole window** -- a count taken without input measures the idle throttle,
    not a frame rate (AGENTS.md);
  * the input rate beside it, and process CPU over the same window;
  * a `cordial_screenshot` of the frame, which reads Cordial's swapchain;
  * optionally a click at `--click X,Y` (window pixels, as `cordial_click`
    takes them), with a screenshot before and after, so a UI element's
    response is visible rather than asserted.

Never launches signed in: the profile is fresh under the redirected data root,
so there is no account to put at risk.

Usage:  tools/render-scale-bench.py --tag s067 --scale 0.67 [--binary PATH]
"""

import argparse, json, os, re, shutil, socket, subprocess, sys, time

BOX = "cordial"


def sh(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def in_box(cmd):
    return sh(["distrobox", "enter", BOX, "--", "bash", "-lc", cmd])


class Devctl:
    def __init__(self, path):
        self.path = path

    def send(self, line, timeout=20):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(self.path)
        s.sendall((line + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        s.close()
        reply = buf.decode().strip()
        if reply.startswith("err "):
            raise RuntimeError(f"devctl {line!r}: {reply}")
        return reply[3:].strip() if reply.startswith("ok") else reply


def start_sway(width, height, tag):
    stamp, cfg = f"/tmp/cordial-rs-{tag}-display", f"/tmp/cordial-rs-{tag}-sway.cfg"
    for f in (stamp, cfg):
        if os.path.exists(f):
            os.unlink(f)
    with open(cfg, "w") as fh:
        fh.write(f"""xwayland disable
output HEADLESS-1 mode {width}x{height}
default_border none
default_floating_border none
focus_follows_mouse no
exec sh -c 'printf %s "$WAYLAND_DISPLAY" > {stamp}'
""")
    subprocess.Popen(
        ["distrobox", "enter", BOX, "--", "bash", "-lc",
         f"exec env -u WAYLAND_DISPLAY -u DISPLAY WLR_BACKENDS=headless "
         f"WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=1 "
         f"sway -c {cfg} > /tmp/cordial-rs-{tag}-sway.log 2>&1"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.time() + 25
    while time.time() < deadline:
        if os.path.exists(stamp) and os.path.getsize(stamp):
            # The compositor's own pid, found by the config path only this run uses.
            for pid in in_box("pidof sway").stdout.split():
                argv = in_box(f"tr '\\0' ' ' < /proc/{pid}/cmdline").stdout
                if cfg in argv:
                    return int(pid), open(stamp).read().strip()
        time.sleep(0.2)
    raise RuntimeError("sway never reported a display")


def cpu_ticks(pid):
    with open(f"/proc/{pid}/stat") as f:
        parts = f.read().rsplit(")", 1)[1].split()
    return int(parts[11]) + int(parts[12])


def gpu_ns(pid):
    """Render-engine busy nanoseconds this process has used, from DRM fdinfo.

    The kernel exposes `drm-engine-render: N ns` per open DRM client, so the
    difference across the measured window over its wall time is the share of
    the render engine this client kept busy -- a measurement of the work, which
    a frame interval pinned by the compositor's 60 Hz cannot be. Summed over
    every DRM fd (duplicates of one client are counted once by `drm-client-id`).
    """
    seen, total, keys = set(), 0, set()
    try:
        fds = os.listdir(f"/proc/{pid}/fdinfo")
    except OSError:
        return None, keys
    for fd in fds:
        try:
            text = open(f"/proc/{pid}/fdinfo/{fd}").read()
        except OSError:
            continue
        if "drm-driver" not in text:
            continue
        cid = re.search(r"drm-client-id:\s*(\d+)", text)
        if cid and cid.group(1) in seen:
            continue
        if cid:
            seen.add(cid.group(1))
        for m in re.finditer(r"^(drm-engine-[\w-]+):\s*(\d+) ns", text, re.M):
            keys.add(m.group(1))
            if m.group(1) == "drm-engine-render":
                total += int(m.group(2))
    return total, keys


def drive(dev, seconds, hz, box):
    """Input for the whole window: a small square, never the same point twice."""
    t0 = time.monotonic()
    sent = 0
    step = 1.0 / hz
    while time.monotonic() - t0 < seconds:
        x, y = box[sent % 4]
        dev.send(f"move {x} {y}", timeout=5)
        sent += 1
        time.sleep(max(0.0, step - 0.002))
    return sent, time.monotonic() - t0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True)
    ap.add_argument("--scale", default="", help="CORDIAL_RENDER_SCALE; empty leaves it unset (the control)")
    ap.add_argument("--binary", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--seconds", type=float, default=30)
    ap.add_argument("--hz", type=float, default=60)
    ap.add_argument("--width", type=int, default=1280)
    ap.add_argument("--height", type=int, default=800)
    ap.add_argument("--click", default="", help="X,Y in window pixels, clicked after the measurement")
    ap.add_argument("--env", action="append", default=[], metavar="K=V")
    ap.add_argument("--flags", default="", help="JSON object written to the profile's flags.json before launch")
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    apk = os.environ.get("CORDIAL_APK", os.path.expanduser(
        "~/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/com.roblox.client/base.apk"))
    lib = os.environ.get("CORDIAL_LIB_DIR", os.path.expanduser("~/.cache/cordial/lib/x86_64"))
    root = os.path.expanduser(f"~/.cache/cordial-agent-upscale-{args.tag}")
    if os.path.exists(root):
        shutil.rmtree(root)
    for d in ("data", "cache", "config"):
        os.makedirs(os.path.join(root, d))
    log_path = os.path.join(args.out, f"{args.tag}.log")

    sway = client = None
    result = {"tag": args.tag, "scale": args.scale or "unset"}
    try:
        sway, display = start_sway(args.width, args.height, args.tag)
        env = dict(os.environ)
        env.pop("DISPLAY", None)
        env.update(WAYLAND_DISPLAY=display, GDK_BACKEND="wayland", CORDIAL_DEV_CONTROL="1",
                   XDG_DATA_HOME=os.path.join(root, "data"), XDG_CACHE_HOME=os.path.join(root, "cache"),
                   XDG_CONFIG_HOME=os.path.join(root, "config"))
        env.pop("CORDIAL_RENDER_SCALE", None)
        if args.scale:
            env["CORDIAL_RENDER_SCALE"] = args.scale
        for kv in args.env:
            k, _, v = kv.partition("=")
            env[k] = v
        if args.flags:
            pdir = os.path.join(root, "data", "cordial", "profiles", "upscale")
            os.makedirs(pdir, exist_ok=True)
            with open(os.path.join(pdir, "flags.json"), "w") as fh:
                fh.write(args.flags)
        log = open(log_path, "w")
        client = subprocess.Popen(
            [args.binary, "--lib-dir", lib, "--apk", apk, "--host-libc", "--game-activity",
             "--run", "0", "--profile", "upscale"],
            env=env, stdout=log, stderr=subprocess.STDOUT)
        print(f"== {args.tag}: client pid {client.pid} on {display}, scale {args.scale or 'unset'}", flush=True)

        deadline = time.time() + 150
        while time.time() < deadline:
            if client.poll() is not None:
                sys.exit(f"client exited {client.returncode}; see {log_path}")
            if re.search(r"app ready: (Home|Landing)", open(log_path, errors="replace").read()):
                break
            time.sleep(1)
        else:
            sys.exit(f"never ready; see {log_path}")
        time.sleep(14)
        dev = Devctl(os.path.join(root, "data", "cordial", "profiles", "upscale", "devctl.sock"))
        info = dev.send("info")
        print(f"== {info}", flush=True)
        m = re.search(r"extent=(\d+)x(\d+)", info)
        ew, eh = (int(m.group(1)), int(m.group(2))) if m else (args.width, args.height)
        result["extent_reported_by_info"] = f"{ew}x{eh}"

        shot0 = os.path.join(args.out, f"{args.tag}-before.png")
        dev.send(f"screenshot {shot0}", timeout=20)

        # The window's content area, not the swapchain's: input is in window pixels.
        cx, cy = ew // 2, eh // 2
        box = [(cx - 40, cy - 40), (cx + 40, cy - 40), (cx + 40, cy + 40), (cx - 40, cy + 40)]
        # Warm up: the ring must hold input-driven frames only.
        drive(dev, 8, args.hz, box)
        p0 = int(re.search(r"presents=(\d+)", dev.send("info")).group(1))
        c0, w0 = cpu_ticks(client.pid), time.monotonic()
        g0, gkeys = gpu_ns(client.pid)
        sent, elapsed = drive(dev, args.seconds, args.hz, box)
        c1, w1 = cpu_ticks(client.pid), time.monotonic()
        g1, _ = gpu_ns(client.pid)
        info = dev.send("info")
        p1 = int(re.search(r"presents=(\d+)", info).group(1))
        hz = os.sysconf("SC_CLK_TCK")
        result.update(
            presents_per_s=round((p1 - p0) / elapsed, 1),
            input_per_s=round(sent / elapsed, 1),
            cpu_percent=round(100 * ((c1 - c0) / hz) / (w1 - w0), 1),
            gpu_render_busy_percent=(round(100 * (g1 - g0) / 1e9 / (w1 - w0), 1)
                                     if g0 is not None and g1 is not None and gkeys else None),
            gpu_engine_keys=sorted(gkeys),
            frames=(re.search(r"frames n=.*", info) or [None])[0],
            info=info,
        )
        print(f"== {json.dumps(result)}", flush=True)

        if args.click:
            x, y = (float(v) for v in args.click.split(","))
            dev.send(f"move {x} {y}")
            time.sleep(0.6)
            dev.send(f"screenshot {os.path.join(args.out, args.tag + '-hover.png')}", timeout=20)
            dev.send(f"click {x} {y} 1")
            time.sleep(2.0)
            dev.send(f"screenshot {os.path.join(args.out, args.tag + '-clicked.png')}", timeout=20)
            result["clicked"] = f"{x},{y}"
        with open(os.path.join(args.out, f"{args.tag}.json"), "w") as fh:
            json.dump(result, fh, indent=1)
    finally:
        if not args.keep:
            if client and client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=15)
                except Exception:
                    client.kill()
            if sway:
                in_box(f"kill {sway}")
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
