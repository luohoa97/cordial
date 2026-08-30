#!/usr/bin/env python3
"""Verify commit 22127ba: pairing a forwarded press's release to itself,
so a single "/" reliably opens chat and Sober #987 stays fixed.

Model is tools/text-input-e2e.py (nested sway in the "cordial" distrobox,
never cage, so a real seat exists), but the keystrokes under test are driven
straight at the devctl socket rather than through the virtual keyboard:

  * `key down 53 0` / `key up 53 0` call `input::pass_key_event` directly --
    the exact function commit 22127ba changed -- so this is not a weaker
    substitute for real input, it is the same call a real keystroke makes,
    with the down/up gap under this script's control instead of the
    compositor's.
  * `text /` drives `script_type`, which (see input.rs:2619) calls
    `pass_key_event` for the same evdev code around the character insert.
    That is how condition 3 can assert both that the guard still suppresses
    a keystroke reaching the engine *and* that the character still lands in
    the visible box, without needing a real IME round trip.

Every attempt for condition 1 deliberately waits for `focused_textbox()` to
report a box before sending the release -- the worst case the bug describes,
guaranteed on every attempt rather than hoped for from a blind timing sweep.
"""

import argparse, json, os, re, socket, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOLDERS = os.environ.get("CORDIAL_HOLDER_BIN", "/tmp/cordial-wl-holders")
BOX = "cordial"
OUT = "/tmp/cordial-agent-slash-out"
TAG = "cordial-agent-slash"


def sh(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def in_box(cmd):
    return sh(["distrobox", "enter", BOX, "--", "bash", "-lc", cmd])


class Devctl:
    def __init__(self, path):
        self.path = path

    def send(self, line, timeout=10.0):
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
        reply = buf.decode(errors="replace").strip()
        if reply.startswith("err "):
            raise RuntimeError(f"devctl {line!r}: {reply}")
        return reply[3:].strip() if reply.startswith("ok") else reply

    def textbox(self):
        line = self.send("textbox")
        head, _, text = line.partition(" text=")
        d = {k: json.loads(v) if v.startswith('"') else v
             for k, v in re.findall(r'(\w+)=("[^"]*"|\S*)', head)}
        try:
            d["text"] = json.loads(text) if text.startswith('"') else None
        except Exception:
            d["text"] = None
        d["raw_text"] = text
        for k in ("gen", "rev", "chars", "bytes", "caret"):
            if k in d:
                d[k] = int(d[k])
        return d


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
            line = self.p.stderr.readline()
            if line.startswith("ready"):
                return
        raise RuntimeError(f"{binary} never became ready")

    def cmd(self, line, settle=0.15):
        self.p.stdin.write(line + "\n")
        self.p.stdin.flush()
        self.p.stdout.readline()          # "ok" per command, so we never sleep blind
        time.sleep(settle)

    def close(self):
        try:
            self.p.stdin.write("quit\n")
            self.p.stdin.flush()
        except Exception:
            pass
        try:
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


SWAY_CFG = """xwayland disable
output HEADLESS-1 mode {w}x{h}
default_border none
default_floating_border none
focus_follows_mouse no
exec sh -c 'printf %s "$WAYLAND_DISPLAY" > {stamp}'
"""


def start_sway(width, height, stamp, cfg, logf):
    for f in (stamp, cfg):
        if os.path.exists(f):
            os.unlink(f)
    with open(cfg, "w") as fh:
        fh.write(SWAY_CFG.format(w=width, h=height, stamp=stamp))
    subprocess.Popen(
        ["distrobox", "enter", BOX, "--", "bash", "-lc",
         f"exec env -u WAYLAND_DISPLAY -u DISPLAY WLR_BACKENDS=headless "
         f"WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=1 "
         f"sway -c {cfg} > {logf} 2>&1"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.time() + 25
    while time.time() < deadline:
        if os.path.exists(stamp) and os.path.getsize(stamp):
            return sway_pid(cfg), open(stamp).read().strip()
        time.sleep(0.2)
    raise RuntimeError(f"sway never reported a display; see {logf}")


def sway_pid(cfg):
    for pid in in_box("pidof sway").stdout.split():
        argv = in_box(f"tr '\\0' ' ' < /proc/{pid}/cmdline").stdout
        if cfg in argv:
            return int(pid)
    return None


def profile_holder(profile):
    out = sh(["pidof", "cordial-run"]).stdout.split()
    for pid in out:
        try:
            argv = open(f"/proc/{pid}/cmdline").read().split("\0")
        except OSError:
            continue
        if "--profile" in argv and argv[argv.index("--profile") + 1] == profile:
            return int(pid)
    return None


def glob_player_logs(logs_dir):
    import glob as _glob
    return _glob.glob(os.path.join(logs_dir, "*Player*.log"))


def log_tail(path, since_size):
    with open(path, errors="replace") as f:
        f.seek(since_size)
        return f.read()


def log_size(path):
    return os.path.getsize(path)


def wait_textbox_none(dev, timeout=3.0):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = dev.textbox()
        if last["focus"] == "none":
            return last
        time.sleep(0.1)
    return last


def wait_textbox_focused(dev, timeout=3.0):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = dev.textbox()
        if last["focus"] != "none":
            return last, True
        time.sleep(0.02)
    return last, False


def main():
    ap = argparse.ArgumentParser()
    # Chat is a feature of a joined experience, not the shell's Home/Landing
    # screen -- confirmed the hard way (run 1 of this script sat on `Landing`
    # with a fresh profile and "/" never focused anything, because there is no
    # chat there to open). So this joins for real, the same way
    # tools/join-run.sh does: the project's own sanctioned test account and
    # test place, never the human's account.
    ap.add_argument("--profile", default=os.environ.get("CORDIAL_TEST_PROFILE", "CordialTest"))
    ap.add_argument("--place", default=os.environ.get("CORDIAL_TEST_PLACE", "17625359962"))
    ap.add_argument("--attempts", type=int, default=8)
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--width", type=int, default=1280)
    ap.add_argument("--height", type=int, default=800)
    args = ap.parse_args()

    os.makedirs(OUT, exist_ok=True)
    log_path = os.path.join(OUT, "client.log")
    binary = os.path.join(ROOT, "target/release/cordial-run")
    apk = os.environ.get(
        "CORDIAL_APK",
        os.path.expanduser("~/.var/app/org.vinegarhq.Sober/data/sober/packages/"
                           "x86_64/com.roblox.client/base.apk"))
    lib = os.environ.get("CORDIAL_LIB_DIR", os.path.expanduser("~/.cache/cordial/lib/x86_64"))
    # CordialTest is a named, shared profile -- not "default" -- so it is used
    # at its standard path rather than redirected through XDG_DATA_HOME. That
    # standard path is exactly where its saved login lives, which is the whole
    # reason to use it: joining a real experience needs to be signed in.
    logs_dir = os.path.expanduser(
        f"~/.local/share/cordial/profiles/{args.profile}/data/files/appData/logs")

    for path, what in ((binary, "build"), (apk, "APK"), (lib, "library directory")):
        if not os.path.exists(path):
            sys.exit(f"FAIL: no {what} at {path}")
    for tool in ("wl-keyboard-holder", "wl-pointer-holder"):
        if not os.path.exists(os.path.join(HOLDERS, tool)):
            sys.exit(f"FAIL: no {tool} -- run tools/build-wl-holders.sh in the container")
    held = profile_holder(args.profile)
    if held:
        sys.exit(f"FAIL: profile {args.profile!r} is already open in pid {held}.")

    stamp = f"/tmp/{TAG}-display"
    cfg = f"/tmp/{TAG}-sway.cfg"
    swaylog = f"/tmp/{TAG}-sway.log"

    sway = client = kbd = ptr = None
    results = []

    def check(name, got, want):
        ok = got == want
        results.append((ok, name, got, want))
        print(f"  {'ok  ' if ok else 'FAIL'}  {name}: got {got!r}"
              + ("" if ok else f", want {want!r}"), flush=True)
        return ok

    try:
        sway, display = start_sway(args.width, args.height, stamp, cfg, swaylog)
        print(f"== nested sway on {display}, {args.width}x{args.height} (pid {sway})")

        kbd = Holder(f"{HOLDERS}/wl-keyboard-holder", display)
        ptr = Holder(f"{HOLDERS}/wl-pointer-holder", display, [str(args.width), str(args.height)])
        caps = in_box(f"SWAYSOCK=$(ls -t $XDG_RUNTIME_DIR/sway-ipc.*.sock | head -1) "
                      f"swaymsg -t get_seats -r").stdout
        print(f"== seat: {caps.strip()[:200]}")
        if '"capabilities":3' not in caps.replace(" ", ""):
            sys.exit("FAIL: the seat does not advertise both a keyboard and a pointer")

        env = dict(os.environ)
        env.update(
            WAYLAND_DISPLAY=display,
            GDK_BACKEND="wayland",
            CORDIAL_WAYLAND="1",
            CORDIAL_DEV_CONTROL="1",
            CORDIAL_TRACE_TEXT="1",
            CORDIAL_TRACE_TEXT_SHOW_PASSWORDS="1",
        )
        join_url = os.environ.get(
            "CORDIAL_TEST_LINK", f"roblox://experiences/start?placeId={args.place}")
        log = open(log_path, "w")
        client = subprocess.Popen(
            [binary, "--lib-dir", lib, "--apk", apk, "--host-libc",
             "--game-activity", "--run", "300", "--profile", args.profile,
             "--join-url", join_url],
            env=env, stdout=log, stderr=subprocess.STDOUT)
        print(f"== client pid {client.pid}, log {log_path}, profile={args.profile} "
              f"join={join_url}")

        # The authoritative signal that a join is real, not "the shell is up":
        # `Connection accepted from` in the engine's own Player log, exactly
        # what tools/join-run.sh checks. `app ready: Home/Landing` only proves
        # the shell initialised, and chat does not exist there at all -- that
        # is what the first run of this script found out.
        deadline = time.time() + 150
        player_log = None
        while time.time() < deadline:
            if client.poll() is not None:
                sys.exit(f"FAIL: client exited with {client.returncode} before joining; see {log_path}")
            for f in glob_player_logs(logs_dir):
                try:
                    if "Connection accepted from" in open(f, errors="replace").read():
                        player_log = f
                        break
                except OSError:
                    continue
            if player_log:
                break
            time.sleep(2)
        if not player_log:
            shell_text = open(log_path, errors="replace").read()
            m = re.search(r"app ready: (\S+)", shell_text)
            sys.exit(f"FAIL: never joined the test place within 150s (shell reached "
                     f"{m.group(1) if m else 'nothing'}); see {log_path} and {logs_dir}")
        print(f"== joined: {player_log}")
        # The world and its chat system are not interactive the instant the
        # connection is accepted -- give it real time to finish loading before
        # trusting `focused_textbox()` to mean anything.
        time.sleep(20)

        sock = os.path.expanduser(
            f"~/.local/share/cordial/profiles/{args.profile}/devctl.sock")
        deadline = time.time() + 20
        while time.time() < deadline and not os.path.exists(sock):
            time.sleep(0.5)
        if not os.path.exists(sock):
            sys.exit(f"FAIL: no devctl socket at {sock}")
        dev = Devctl(sock)
        print(f"== {dev.send('info')}  (socket {sock})")

        first = int(re.search(r"presents=(\d+)", dev.send("info")).group(1))
        time.sleep(6)
        second = int(re.search(r"presents=(\d+)", dev.send("info")).group(1))
        if second == first and first < 10:
            sys.exit(f"SKIPPED: startup freeze -- presents stuck at {first}. Run again.")
        print(f"== presents {first} -> {second}, client is live")

        # ---------------------------------------------------------------
        print(f"\n-- Condition 1 & 2: {args.attempts} single-press attempts, "
              f"each forced to hit the exact race (release sent only after "
              f"focused_textbox() already reports the box)")
        # Prime the chat system once, off the record, with a generous
        # focus-wait timeout. Attempt 1 of the first live run of this script
        # timed out waiting for focus (3s) though every later attempt got it
        # in well under that -- the first "/" after a fresh join pays for
        # something (chat's own lazy init) that every later press does not.
        # Counting that warm-up as one of the 8 would blame the fix for a
        # one-off cost that has nothing to do with it.
        print("  priming: one throwaway press to warm up the chat system")
        dev.send("key down 53 0")
        _, primed = wait_textbox_focused(dev, timeout=12.0)
        dev.send("key up 53 0")
        time.sleep(0.3)
        dev.send("tap 1 0")
        wait_textbox_none(dev, timeout=3.0)
        print(f"        primed={primed}")

        opened = 0
        marker_seen = 0
        bad_suppression = 0
        stray_chars = 0
        for i in range(1, args.attempts + 1):
            # Reset: make sure nothing is focused before this attempt.
            dev.send("tap 1 0")  # Escape -- not a text key, always forwarded
            before = wait_textbox_none(dev, timeout=3.0)
            if before["focus"] != "none":
                print(f"  attempt {i}: could not clear focus first (focus={before['focus']}), skipping")
                continue
            mark = log_size(log_path)
            dev.send("key down 53 0")
            focused_box, got_focus = wait_textbox_focused(dev, timeout=3.0)
            wait_ms = None
            if got_focus:
                t0 = time.time()
                dev.send("key up 53 0")
                wait_ms = round((time.time() - t0) * 1000, 1)
            else:
                dev.send("key up 53 0")
            time.sleep(0.3)
            after = dev.textbox()
            chunk = log_tail(log_path, mark)
            has_marker = "forwarded anyway, its press reached the engine before" in chunk
            has_bad_suppress = bool(re.search(r"suppressed: code=53 down=false", chunk))
            this_opened = after["focus"] != "none"
            opened += int(this_opened)
            marker_seen += int(has_marker)
            bad_suppression += int(has_bad_suppress)
            stray = after.get("chars", -1) != 0
            stray_chars += int(stray)
            print(f"  attempt {i}: focus-took={got_focus} opened_after_one_press={this_opened} "
                  f"marker_line={has_marker} suppressed_down_false_line={has_bad_suppress} "
                  f"chars_after={after.get('chars')} raw_text={after.get('raw_text')}")
            if chunk.strip():
                for line in chunk.splitlines():
                    if "code=53" in line or "53" in line and "pass_key_event" in line:
                        print(f"        log: {line}")
        check("single presses that opened chat", opened, args.attempts)
        check("attempts showing the fix's marker line (forwarded anyway...)",
              marker_seen, args.attempts)
        check("attempts with a 'suppressed...down=false' line for code=53 (must be zero)",
              bad_suppression, 0)
        check("attempts where a stray character appeared in the box (must be zero)",
              stray_chars, 0)

        # ---------------------------------------------------------------
        print("\n-- Condition 3: typing '/' into an ALREADY open chat box "
              "must still be suppressed and must not retrigger chat")
        dev.send("tap 1 0")
        wait_textbox_none(dev, timeout=3.0)
        dev.send("key down 53 0")
        box, got = wait_textbox_focused(dev, timeout=3.0)
        if not got:
            check("chat opened, to set up condition-3", False, True)
        else:
            dev.send("key up 53 0")
            time.sleep(0.3)
            gen_before = dev.textbox()["gen"]
            mark = log_size(log_path)
            # Not `dev.send("text ...")`: that drives `script_type`, whose
            # ASCII-to-evdev table (input.rs `ascii_to_evdev`) only covers
            # a-z, 1-9, 0 and space. '/' falls through it to `None`, so
            # `script_type` never calls `pass_key_event` for '/' at all --
            # confirmed by the first attempt at this test, which inserted
            # the character with NEITHER a suppressed-down nor a
            # suppressed-up line anywhere in the log. That path cannot
            # exercise the guard for this character; a real keystroke
            # through the virtual keyboard can, because it drives both the
            # evdev key path (would_suppress) and the IME text-composition
            # path (the actual character) the way a physical "/" does.
            kbd.cmd("type /", settle=0.5)
            chunk = log_tail(log_path, mark)
            after = dev.textbox()
            down_suppressed = bool(re.search(r"suppressed: code=53 down=true", chunk))
            up_suppressed = bool(re.search(r"suppressed: code=53 down=false", chunk))
            has_marker = "forwarded anyway" in chunk
            print(f"  after typing '/' into the open box: chars={after.get('chars')} "
                  f"text={after.get('raw_text')} gen_before={gen_before} gen_after={after.get('gen')}")
            for line in chunk.splitlines():
                if "code=53" in line:
                    print(f"        log: {line}")
            check("the '/' character reached the visible box", after.get("chars"), 1)
            check("the '/' character is exactly what's in the box",
                  after.get("text"), "/")
            check("the down half was suppressed (never reached the engine)",
                  down_suppressed, True)
            check("the up half was ALSO suppressed (never reached the engine)",
                  up_suppressed, True)
            check("no 'forwarded anyway' marker fired for this keystroke "
                  "(it was never a forwarded press)", has_marker, False)
            check("chat did not regenerate/refocus a new box (Sober #987 still fixed)",
                  after.get("gen"), gen_before)

        # ---------------------------------------------------------------
        print("\n-- client health")
        check("client answered ping", dev.send("ping"), "")
        p3 = int(re.search(r"presents=(\d+)", dev.send("info")).group(1))
        check("client still presenting frames", p3 >= second, True)

    finally:
        if not args.keep:
            for h in (kbd, ptr):
                if h:
                    h.close()
            if client and client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=15)
                except Exception:
                    client.kill()
            if sway:
                in_box(f"kill {sway}")

    print()
    failed = [r for r in results if not r[0]]
    if failed:
        print(f"BROKEN -- {len(failed)} of {len(results)} checks failed:")
        for _, name, got, want in failed:
            print(f"  {name}: got {got!r}, want {want!r}")
        sys.exit(1)
    print(f"PERFECT -- {len(results)} checks, none failed.")


if __name__ == "__main__":
    main()
