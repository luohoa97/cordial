#!/usr/bin/env python3
"""End-to-end test of Cordial's Roblox text editor, with real keys.

Why this exists rather than `tools/text-entry-check.sh`, which already tests
typing: that script asserts on the *path* -- a spec arrived, the editor was
placed somewhere with a size, the client survived -- and leaves "did the right
characters end up in the box" to a human looking at a screenshot. Every failure
this project has actually shipped in the text editor would have passed it. A
guard that inserted every character twice, a select-all that silently destroyed
the selection, a click that put the caret in the wrong place: all of them leave
a live client with a correctly placed editor.

Three instruments here are different from that script's, and each replaces one
that could not see a failure:

  * **sway, not cage.** cage advertises no `zwp_text_input_manager_v3` and its
    seat never gains a keyboard. Cordial reads seat capabilities once, at
    `open()`, so under cage it never binds its own `wl_keyboard` -- and the
    guard that stops GDK and Cordial both inserting the same character never
    runs. `docs/NEXT.md` labelled that guard `INFERRED` for exactly this
    reason.

  * **A virtual keyboard that is held open from before the client starts**, so
    the seat looks like a real one for the whole run and that guard is under
    test. `wlrctl` creates its keyboard and exits in the same breath, which is
    a race it sometimes wins.

  * **The `textbox` devctl verb**, which reports what the field actually
    contains. There was no readback at all before: the editor's change signal
    prints nothing, the `text ->` trace prints a byte count, and
    `cordial_screenshot` photographs the engine's swapchain, which cannot see a
    GTK widget.

Input is driven inside a nested compositor on its own `WAYLAND_DISPLAY`, which
is the one form AGENTS.md permits -- never at the developer's session.

Usage:  tools/text-input-e2e.py [--profile NAME] [--keep]
        tools/text-input-e2e.py --visibility windowed,fullscreen,tiled-after,floating
                                [--scale 1.5] [--gles] [--client-env ADW_DEBUG_COLOR_SCHEME=prefer-light]

        tools/text-input-e2e.py --x11 [--wm openbox|xfwm4|xfwm4c|icewm] [--client-env CORDIAL_X11_EDITOR=off]

`--x11` runs the same questions against the X11 backend (ADR-062) on a private
Xvfb with its own window manager, typing with XTest *inside that server only*
and photographing it with ImageMagick's `import`. It exists because the X11
editor was, for a stretch of this project's history, specified in ADR-024 and
absent: "typed text is sent and not drawn" was read from the code and never run,
the one attempt to start Xwayland under the nested sway having failed. The
control is `--client-env CORDIAL_X11_EDITOR=off`, which must fail every
visibility assertion and none of the readback ones.

`--visibility` is the one case here that asks whether typed text can be *seen*:
it photographs the compositor's output with grim (the only instrument that sees
a GTK widget) and compares the editor's rectangle with and without the text.
Its controls are `--client-env CORDIAL_VISIBILITY_ANCHOR=off` (fullscreen and
tiled-after must fail, ADR-056) and, with `--gles`, `VK_ICD_FILENAMES=/nonexistent
GSK_RENDERER=ngl` (every case must fail, ADR-047). Set `CORDIAL_E2E_XDG_ROOT` to
give the client its own data, cache and config; redirecting XDG_* for the harness
itself breaks `distrobox`.
"""

import argparse, json, os, re, shutil, socket, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools"))
from headless_compositor import Execution, Unsupported, start  # noqa: E402

HOLDERS = os.environ.get("CORDIAL_HOLDER_BIN", "/tmp/cordial-wl-holders")
BOX = "cordial"                       # fallback when no session has started yet
OUT = os.environ.get("CORDIAL_E2E_OUT", "/tmp/cordial-text-e2e")
COMP = None                           # the running Session, once main() starts one


def sh(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def in_box(cmd):
    """Run a shell command inside the compositor's environment."""
    if COMP is not None:
        return COMP.shell(cmd)
    return sh(["distrobox", "enter", BOX, "--", "bash", "-lc", cmd])


class Devctl:
    """The one-line-in, one-line-out development control socket."""

    def __init__(self, path):
        self.path = path

    def send(self, line):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(20)
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

    def textbox(self):
        """Parse the `textbox` reply into a dict; `text` stays a real string."""
        line = self.send("textbox")
        head, _, text = line.partition(" text=")
        # Quoted values, not just bare ones: `family` reports what fontconfig
        # calls the face and half of Roblox's shipped families have a space in
        # them ("Builder Sans", "Press Start 2P"). A plain `head.split()` cut
        # those in half and silently kept the first word.
        d = {k: json.loads(v) if v.startswith('"') else v
             for k, v in re.findall(r'(\w+)=("[^"]*"|\S*)', head)}
        # `redacted` emits Rust's `{:?}`, which for these strings is JSON.
        try:
            d["text"] = json.loads(text) if text.startswith('"') else None
        except Exception:
            d["text"] = None
        d["raw_text"] = text
        for k in ("gen", "rev", "chars", "bytes", "caret", "returns"):
            if k in d:
                d[k] = int(d[k])
        return d


class Holder:
    """A persistent virtual input device, driven a line at a time."""

    def __init__(self, binary, display, args=()):
        if COMP is not None:
            self.p = COMP.popen([binary] + list(args), stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True)
        else:
            self.p = subprocess.Popen(
                ["distrobox", "enter", BOX, "--", "bash", "-lc",
                 f"export WAYLAND_DISPLAY={display}; exec {binary} {' '.join(args)}"],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True)
        # The holder prints "ready" once the device exists. Waiting for it is
        # the difference between a keyboard the seat has and one it is about
        # to have; every key sent before it is silently dropped.
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


class Case:
    """One assertion, recorded whether it passed or not.

    Nothing here retries and nothing relaxes on failure. The whole value of
    this file is that a result which needed a second chance is a failure.
    """

    def __init__(self):
        self.results = []

    def check(self, name, got, want):
        ok = got == want
        self.results.append((ok, name, got, want))
        print(f"  {'ok  ' if ok else 'FAIL'}  {name}: got {got!r}"
              + ("" if ok else f", want {want!r}"), flush=True)
        return ok

    def note(self, name, value):
        print(f"        {name}: {value}", flush=True)

    @property
    def failed(self):
        return [r for r in self.results if not r[0]]


def start_compositor(width, height, backend=None, direct=False):
    """Start a compositor with everything this file's window checks need.

    The window tree, output mode changes, `grim` and the virtual keyboard are
    Sway's. KWin has a window model of its own, but no virtual keyboard and no
    output-mode change after start, so this harness asks for capabilities
    rather than silently running half of itself. KWin is still a working
    headless backend for the launch-only harnesses (`frame-pacing-check.py`,
    `startup-freeze-survey.sh`).
    """
    global COMP
    os.makedirs(OUT, exist_ok=True)
    COMP = start(width, height, out_dir=OUT, backend=backend,
                 execution=Execution.from_env(direct=direct),
                 required=("launch", "ipc", "screenshot", "virtual_input", "output_resize"),
                 extra_config=os.environ.get("CORDIAL_E2E_SWAY_EXTRA", ""))
    return COMP


def swaymsg(args, raw=True):
    """Run this run's compositor's own IPC command.

    The session names its socket by pid. `ls -t sway-ipc.*.sock | head -1`
    picked the newest socket, which is another agent's compositor whenever two
    of these run at once.
    """
    return COMP.ipc(args, raw=raw)


def profile_holder(profile):
    """The pid holding this profile's lock, or None.

    Never `pgrep -f`: the pattern matches the shell running it, and that has
    killed this session's own process five times in one day. `pidof` matches
    the executable, which the engine's thread rename does not touch.
    """
    out = sh(["pidof", "cordial-run"]).stdout.split()
    for pid in out:
        try:
            argv = open(f"/proc/{pid}/cmdline").read().split("\0")
        except OSError:
            continue
        if "--profile" in argv and argv[argv.index("--profile") + 1] == profile:
            return int(pid)
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", default="default",
                    help="a signed-in profile; must not be one another client holds")
    ap.add_argument("--keep", action="store_true", help="leave the client and compositor up")
    ap.add_argument("--only-engine-text", action="store_true",
                    help="run only the Enter and engine-text cases (sections 15-17), which "
                         "use a synthetic focused box and need no real one")
    ap.add_argument("--client-env", action="append", default=[], metavar="KEY=VALUE",
                    help="extra environment for the client; how a control arm is run, by "
                         "switching off the thing under test and showing the same assertions fail")
    ap.add_argument("--visibility", metavar="MODES", default=None,
                    help="run only the visibility cases, in each of these window modes "
                         "(comma list of windowed, fullscreen, tiled-after, floating): type into a "
                         "synthetic box and photograph the composited output, asserting the typed "
                         "characters changed pixels where the editor is. cordial_textbox reports "
                         "what the field holds; this reports whether anyone could see it")
    ap.add_argument("--x11", action="store_true",
                    help="run the X11 backend's cases on a private Xvfb (see the module doc); "
                         "needs xdotool, ImageMagick, xwininfo and a window manager in the container")
    ap.add_argument("--wm", default="openbox", choices=["openbox", "xfwm4", "xfwm4c", "icewm", "none"],
                    help="with --x11: the window manager on the private server. xfwm4c is xfwm4 "
                         "with its compositor on, the only way to get a composited X server "
                         "here, and so the transparent editor path; the others are opaque")
    ap.add_argument("--xwayland", action="store_true",
                    help="with --x11: use sway's own Xwayland instead of Xvfb, which is how most X11 "
                         "clients run on a Wayland desktop. The window is placed by sway, the editor "
                         "is an Xwayland override-redirect window, and the photograph is grim's")
    ap.add_argument("--cases", default=None, metavar="X1,X2,..",
                    help="with --x11: run only these sections (X1 is needed for nothing else)")
    ap.add_argument("--vulkan", action="store_true",
                    help="with --x11: leave Vulkan on. By default CORDIAL_NO_VULKAN=1 is set, "
                         "because the Vulkan client has crashed on Xvfb (software GL); say which "
                         "was used when reporting")
    ap.add_argument("--gles", action="store_true",
                    help="the engine is on GLES (no Vulkan), where `presents` stays 0 by design; "
                         "skips the presents-based startup-freeze check, which would mistake that for a freeze")
    ap.add_argument("--scale", type=float, default=None,
                    help="output scale for the nested compositor (1.5 is fractional)")
    ap.add_argument("--width", type=int, default=1280)
    ap.add_argument("--height", type=int, default=800)
    ap.add_argument("--backend", help="compositor backend (default: first with the needed capabilities)")
    ap.add_argument("--direct", action="store_true",
                    help="run the compositor on this host rather than in the distrobox")
    args = ap.parse_args()

    if args.x11:
        return main_x11(args)

    os.makedirs(OUT, exist_ok=True)
    log_path = os.path.join(OUT, "client.log")
    # Overridable for the same reason CORDIAL_APK/CORDIAL_LIB_DIR already are:
    # AGENTS.md's build split means a host that builds in the toolbox
    # container has no `target/release` at all, and building bare cargo into
    # one to satisfy this default would be the two-CARGO_TARGET_DIRs mistake
    # that file warns about, not a fix.
    binary = os.environ.get("CORDIAL_E2E_BINARY", os.path.join(ROOT, "target/release/cordial-run"))
    apk = os.environ.get(
        "CORDIAL_APK",
        os.path.expanduser("~/.var/app/org.vinegarhq.Sober/data/sober/packages/"
                           "x86_64/com.roblox.client/base.apk"))
    lib = os.environ.get("CORDIAL_LIB_DIR", os.path.expanduser("~/.cache/cordial/lib/x86_64"))

    for path, what in ((binary, "build -- cargo build --release first"), (apk, "APK"), (lib, "library directory")):
        if not os.path.exists(path):
            sys.exit(f"FAIL: no {what} at {path}")
    for tool in ("wl-keyboard-holder", "wl-pointer-holder"):
        if not os.path.exists(os.path.join(HOLDERS, tool)):
            sys.exit(f"FAIL: no {tool} -- run tools/build-wl-holders.sh in the container")
    held = profile_holder(args.profile)
    if held:
        sys.exit(f"FAIL: profile {args.profile!r} is already open in pid {held}. "
                 f"Pick another with --profile; do not kill a client someone is using.")

    comp = client = kbd = ptr = None
    case = Case()
    try:
        if args.scale:
            os.environ["CORDIAL_E2E_SWAY_EXTRA"] = (
                os.environ.get("CORDIAL_E2E_SWAY_EXTRA", "") + f"\noutput HEADLESS-1 scale {args.scale}")
        try:
            comp = start_compositor(args.width, args.height, args.backend, args.direct)
        except (Unsupported, SystemExit) as exc:
            chosen = args.backend or "the available backends"
            sys.exit(f"FAIL: this harness needs a window tree, a screenshot, output "
                     f"resize and wlroots virtual input, which {chosen} do not "
                     f"provide ({exc})")
        display = comp.wayland_display
        print(f"== nested {comp.backend.name} on {display}, {args.width}x{args.height}")

        # Both devices before the client, so the seat advertises a keyboard and
        # a pointer at the moment Cordial reads its capabilities. This is the
        # whole reason the double-insert guard has never been exercised.
        kbd = Holder(f"{HOLDERS}/wl-keyboard-holder", display)
        ptr = Holder(f"{HOLDERS}/wl-pointer-holder", display, [str(args.width), str(args.height)])
        caps = swaymsg("-t get_seats").stdout
        print(f"== seat: {caps.strip()[:200]}")
        if '"capabilities":3' not in caps.replace(" ", ""):
            sys.exit("FAIL: the seat does not advertise both a keyboard and a pointer; "
                     "every reading below would be taken with Cordial's key path switched off")

        env = comp.host_env({
            "GDK_BACKEND": "wayland",
            "CORDIAL_DEV_CONTROL": "1",
            "CORDIAL_TRACE_TEXT": "1",
            # The readback. Safe here and nowhere else: this harness types its
            # own known strings into a search box and never touches a sign-in
            # field. It also puts the clipboard in the log, so the log stays in
            # /tmp and is not something to paste into an issue.
            "CORDIAL_TRACE_TEXT_SHOW_PASSWORDS": "1",
        })
        # **Redirecting XDG_* for the whole script breaks `distrobox`**, which
        # finds its container store through XDG_DATA_HOME: sway never starts and
        # nothing says why. CORDIAL_E2E_XDG_ROOT redirects the client alone
        # (all three variables, as AGENTS.md requires) and leaves the harness's.
        xdg_root = os.environ.get("CORDIAL_E2E_XDG_ROOT")
        if xdg_root:
            env.update(XDG_DATA_HOME=f"{xdg_root}/data", XDG_CACHE_HOME=f"{xdg_root}/cache",
                       XDG_CONFIG_HOME=f"{xdg_root}/config")
        for kv in args.client_env:
            k, _, v = kv.partition("=")
            env[k] = v
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
                sys.exit(f"FAIL: the client exited with {client.returncode} before it was ready; "
                         f"see {log_path}")
            text = open(log_path, errors="replace").read()
            # Specifically Home or Landing. The shell announces several routers
            # before either -- `PlatformAccountRouter` arrives first and is not
            # a screen with a search box on it -- and taking the first `app
            # ready` cost a whole run clicking at a page that had not loaded.
            m = re.search(r"app ready: (Home|Landing)", text)
            if m:
                ready = m.group(1)
                break
            time.sleep(1)
        if not ready:
            sys.exit(f"FAIL: the app shell never became ready; see {log_path}")
        print(f"== app ready: {ready}")
        # Ready is not laid out, and nothing in the log marks that.
        time.sleep(14)

        # XDG_DATA_HOME-aware: AGENTS.md tells agents to redirect it so a
        # throwaway run does not collide with someone else's `default`
        # profile, and a hardcoded `~/.local/share` here would silently watch
        # the wrong socket (or none) for exactly the run that most needs
        # isolation -- confirmed the hard way, `ConnectionRefusedError`
        # against the real home directory while the client's actual socket
        # sat under the redirected one.
        data_home = env.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
        dev = Devctl(os.path.join(data_home, "cordial", "profiles", args.profile, "devctl.sock"))
        print(f"== {dev.send('info')}")

        # **Is this client actually running?** About a third of launches hit the
        # startup freeze -- the engine reaches Home, presents a frame or two and
        # stops (docs/NEXT.md §0). Every assertion below would then fail, and
        # they would fail describing the text editor, which is not what broke.
        # A test that blames the wrong thing is worse than one that refuses.
        def presents():
            return int(re.search(r"presents=(\d+)", dev.send("info")).group(1))
        first = presents()
        time.sleep(6)
        if presents() == first and first < 10 and not args.gles:
            sys.exit(f"SKIPPED: this launch hit the startup freeze -- presents stuck at "
                     f"{first}. That is docs/NEXT.md §0 and not a text-entry result. "
                     f"Run it again; it is about a third of launches.")
        if args.visibility:
            run_visibility(case, dev, kbd, client.pid, display, args)
        elif args.only_engine_text:
            run_enter_and_engine_text(case, dev, kbd, log_path)
        else:
            run_cases(case, dev, kbd, ptr, log_path, args, display)
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
            if comp:
                comp.stop()

    print()
    if case.failed:
        print(f"BROKEN -- {len(case.failed)} of {len(case.results)} assertions failed:")
        for _, name, got, want in case.failed:
            print(f"  {name}: got {got!r}, want {want!r}")
        sys.exit(1)
    print(f"PERFECT -- {len(case.results)} assertions, none failed.")


TEXT_KEYS = set(range(2, 14)) | set(range(16, 28)) | set(range(30, 42)) | set(range(44, 54)) | {57}


def settle(dev, want_rev=None, tries=40):
    """Wait for the mirror to catch up with the widget, then read it.

    The `textbox` verb reports the buffer `adopt_editor_text` mirrors from the
    widget, which is one signal-hop behind it. Polling the revision counter is
    how that hop is waited out; sleeping a fixed amount instead is how a
    harness starts reporting the previous keystroke's answer.
    """
    last = None
    for _ in range(tries):
        last = dev.textbox()
        if want_rev is None or last["rev"] != want_rev:
            return last
        time.sleep(0.05)
    return last


def focus_box(case, dev, tries=5, geometry_timeout=4.0):
    """Click the home search bar until a TextBox takes focus and says where it is.

    Focus and geometry are two events, not one, and the gap between them is the
    whole reason this waits rather than reading once. Clicking the search bar
    opens a modal, and the engine focuses that modal before it has laid out --
    `showKeyboard` volunteers `x=0 y=0 w=0 h=0` and the real numbers only arrive
    from `nativeGetTextBoxInfo` about a second later. Reading at the instant of
    focus gets the zeroes every time, and then every coordinate computed from
    them is nonsense; that misread three assertions in the first full run.

    The returned box is the first one with a usable size. How long that took is
    reported, because a second of it is a second the user is looking at a box
    with no editor on it.
    """
    for attempt in range(1, tries + 1):
        dev.send("click 674 28")
        for _ in range(20):
            time.sleep(0.25)
            if dev.textbox()["focus"] != "none":
                break
        else:
            print(f"        attempt {attempt} focused nothing, retrying", flush=True)
            continue
        start = time.time()
        while time.time() - start < geometry_timeout:
            box = dev.textbox()
            if box["focus"] == "none":
                break                       # it blurred again; click once more
            if box["x"] != "none" and float(box["w"]) > 0 and float(box["h"]) > 0:
                box["geometry_wait"] = round(time.time() - start, 2)
                return box
            time.sleep(0.1)
    box = dev.textbox()
    box["geometry_wait"] = None
    return box


def wait_for(dev, pred, timeout=3.0):
    """Poll the `textbox` readback until `pred` holds, and return the last reading.

    The pump applies an engine push and seeds the widget on its own tick, so
    asserting immediately reads the state before the thing under test has
    happened. Returns the last reading either way; the caller's assertion is
    what fails, not this.
    """
    last = None
    end = time.time() + timeout
    while time.time() < end:
        last = dev.textbox()
        if pred(last):
            return last
        time.sleep(0.05)
    return last


def fake_box(dev, text=""):
    """Focus a synthetic multi-line box and wait for the editor to be on it.

    **Every reading taken in a box made here is SYNTHETIC.** `fakefocus` is
    the engine-side focus path with nothing behind it: it shows what the GTK
    editor does with a `multiline=1` spec, and says nothing about what a real
    chat box sends. A fresh blur first, so no case inherits the previous one's
    text, caret or echo history.
    """
    dev.send("fakeblur")
    time.sleep(0.6)
    dev.send(f"fakefocus 1 300 300 400 120 {text}")
    box = wait_for(dev, lambda b: b["focus"] != "none" and b["x"] != "none"
                   and float(b["w"]) > 0, timeout=5.0)
    # Placed is not focused: GTK takes keyboard focus a pump tick later, and a
    # key sent before it lands on nothing.
    time.sleep(1.0)
    return box


def run_enter_and_engine_text(case, dev, kbd, log_path):
    """Sections 15-17: Enter in both editors, engine-to-editor text, and the single-line editor.

    Everything here runs in a SYNTHETIC box (`fakefocus`) and the engine's push
    is synthetic too (`enginetext`, the same entry point
    `onLuaTextBoxChangedCallback` uses). That is as far as a signed-out
    session reaches: a real chat box sending and clearing needs an account, and
    `nativeReturnPressedFromOnScreenKeyboard` is counted but not sent to a box
    the engine never issued a handle for.
    """
    def type_(s):
        kbd.cmd(f"type {s}")

    def key(k):
        kbd.cmd(f"key {k}")

    def log():
        return open(log_path, errors="replace").read()

    # Raw Enter handed to the engine through nativePassKeyEvent, and Enter the
    # editor consumed instead. Both are read from the client's own trace, so
    # "no raw Return" is the client saying it did not send one.
    enters = lambda: re.findall(r"pass_key_event down=true code=(?:28|96) mods=(0x[0-9a-f]+)", log())
    withheld = lambda: re.findall(r"Enter code=(?:28|96) down=true withheld", log())
    returns = lambda: dev.textbox()["returns"]

    def last_synced():
        """The last text Cordial handed the engine, from the `text ->` trace line."""
        got = re.findall(r'\[cordial\] text -> ("(?:[^"\\]|\\.)*") caret', log())
        return json.loads(got[-1]) if got else None

    print("\n-- 15. Enter in a multi-line editor (SYNTHETIC box): a newline in the text, no raw Return")
    fake_box(dev)
    type_("ok")
    s = wait_for(dev, lambda b: b["chars"] == 2)
    case.check("typed 'ok' into the multi-line editor", s["text"], "ok")
    raw0, held0, ret0 = len(enters()), len(withheld()), returns()
    key("Return")
    s = wait_for(dev, lambda b: b["chars"] == 3)
    case.check("text after Return: GTK inserted the newline", s["text"], "ok\n")
    case.check("the engine was synced the text with the newline", last_synced(), "ok\n")
    case.check("no raw Return reached the engine", len(enters()) - raw0, 0)
    case.check("the client withheld that Return (and said so)", len(withheld()) - held0, 1)
    case.check("a multi-line box gets no ReturnPressed call", returns() - ret0, 0)

    fake_box(dev)
    type_("ok")
    wait_for(dev, lambda b: b["chars"] == 2)
    raw0 = len(enters())
    key("shift+Return")
    s = wait_for(dev, lambda b: b["chars"] == 3)
    case.check("text after Shift+Return", s["text"], "ok\n")
    case.check("no raw Shift+Return reached the engine", len(enters()) - raw0, 0)

    # What the engine does next is its own business, and arrives as text.
    fake_box(dev)
    type_("ok")
    wait_for(dev, lambda b: b["chars"] == 2)
    key("Return")
    wait_for(dev, lambda b: b["chars"] == 3)
    rev = dev.textbox()["rev"]
    dev.send("enginetext ")
    s = wait_for(dev, lambda b: b["rev"] != rev and b["chars"] == 0)
    case.check("an engine-side clear after Enter empties the editor", (s["text"], s["chars"]), ("", 0))
    type_("x")
    s = wait_for(dev, lambda b: b["chars"] >= 1)
    case.check("typing continues from the engine's empty text", s["text"], "x")

    print("\n-- 16. the engine's text reaches the editor (SYNTHETIC push)")
    # The pre-fix symptom, with no push: the engine cleared its box and the
    # editor was never told. Typing on builds on the stale text.
    fake_box(dev)
    type_("ok")
    wait_for(dev, lambda b: b["chars"] == 2)
    key("Return")
    time.sleep(0.6)
    type_("x")
    s = wait_for(dev, lambda b: "x" in (b["text"] or ""))
    case.note("CONTROL (no engine push)", f"text after 'ok', Return, 'x': {s['text']!r}")
    case.check("control: without a push the editor keeps what was typed, newline included",
               s["text"], "ok\nx")

    fake_box(dev)
    type_("ok")
    wait_for(dev, lambda b: b["chars"] == 2)
    key("Return")
    time.sleep(0.6)
    rev = dev.textbox()["rev"]
    dev.send("enginetext ")
    s = wait_for(dev, lambda b: b["rev"] != rev and b["chars"] == 0)
    case.check("an engine-side clear empties the editor mirror", (s["text"], s["chars"], s["caret"]),
               ("", 0, 0))
    type_("x")
    s = wait_for(dev, lambda b: b["chars"] >= 1)
    case.check("the next keystroke builds on the engine's text, not the stale one",
               s["text"], "x")

    fake_box(dev)
    dev.send("enginetext hello")
    s = wait_for(dev, lambda b: b["chars"] == 5)
    case.check("engine text arrives in the box", (s["text"], s["caret"]), ("hello", 5))
    type_("Z")
    s = wait_for(dev, lambda b: b["chars"] == 6)
    case.check("the widget really holds the engine's text (typing builds on it)",
               s["text"], "helloZ")

    # A stale echo: "ab" was typed, so "a" is the engine reflecting the first
    # keystroke. Applying it would put the box back a character.
    fake_box(dev)
    type_("ab")
    wait_for(dev, lambda b: b["chars"] == 2)
    rev = dev.textbox()["rev"]
    dev.send("enginetext a")
    time.sleep(0.8)
    s = dev.textbox()
    case.check("an echo of an earlier keystroke is ignored", (s["text"], s["rev"]), ("ab", rev))
    # Same push once the entry has expired: now it is the engine's own edit.
    time.sleep(1.3)
    dev.send("enginetext a")
    s = wait_for(dev, lambda b: b["rev"] != rev)
    case.check("the same text after the echo window is applied", s["text"], "a")

    # Caret: kept when the new text only adds to the end of what was there and
    # the caret was not at the end.
    fake_box(dev)
    type_("ab")
    wait_for(dev, lambda b: b["chars"] == 2)
    key("Left")
    wait_for(dev, lambda b: b["caret"] == 1)
    dev.send("enginetext abc")
    s = wait_for(dev, lambda b: b["chars"] == 3)
    case.check("a pure extension keeps a mid-text caret", (s["text"], s["caret"]), ("abc", 1))
    type_("Q")
    s = wait_for(dev, lambda b: b["chars"] == 4)
    case.check("the widget's caret was placed there too", s["text"], "aQbc")

    fake_box(dev)
    type_("abc")
    wait_for(dev, lambda b: b["chars"] == 3)
    key("Left")
    dev.send("enginetext xy")
    s = wait_for(dev, lambda b: b["text"] == "xy")
    case.check("a rewrite puts the caret at the end", (s["text"], s["caret"]), ("xy", 2))
    type_("!")
    s = wait_for(dev, lambda b: b["chars"] == 3)
    case.check("and the widget agrees", s["text"], "xy!")
    print("\n-- 17. the single-line editor is unchanged (SYNTHETIC box, multiline=0)")
    # Sections 3-8 and 10 of this file, replayed in a synthetic single-line box.
    # They are the cases that assert the double-insert guard, caret placement and
    # selection on the `gtk::Text` widget this change did not mean to touch;
    # section 1's focus needs the signed-in Home search bar and cannot run on a
    # signed-out profile, so these stand in for 3-8 and 10 there.
    dev.send("fakeblur")
    time.sleep(0.6)
    dev.send("fakefocus 0 300 300 400 40")
    wait_for(dev, lambda b: b["focus"] != "none" and b["x"] != "none"
             and float(b["w"]) > 0, timeout=5.0)
    time.sleep(1.0)
    type_("hello")
    s = wait_for(dev, lambda b: b["chars"] == 5)
    case.check("single-line: text and caret after typing 'hello'", (s["text"], s["caret"]), ("hello", 5))
    key("BackSpace")
    s = wait_for(dev, lambda b: b["chars"] == 4)
    case.check("single-line: backspace", (s["text"], s["caret"]), ("hell", 4))
    key("Home")
    type_("X")
    s = wait_for(dev, lambda b: b["chars"] == 5)
    case.check("single-line: insertion at the caret", (s["text"], s["caret"]), ("Xhell", 1))
    key("End")
    type_("!")
    s = wait_for(dev, lambda b: b["chars"] == 6)
    case.check("single-line: append", s["text"], "Xhell!")
    key("shift+Left"); key("shift+Left")
    type_("?")
    s = wait_for(dev, lambda b: b["text"] == "Xhel?")
    case.check("single-line: a selection is replaced", (s["text"], s["caret"]), ("Xhel?", 5))
    key("ctrl+a")
    type_("abcdefgh")
    s = wait_for(dev, lambda b: b["text"] == "abcdefgh")
    case.check("single-line: select all, overtype", (s["text"], s["caret"]), ("abcdefgh", 8))
    key("ctrl+a"); key("ctrl+c"); key("End"); key("ctrl+v")
    s = wait_for(dev, lambda b: b["chars"] == 16, timeout=4.0)
    case.check("single-line: paste appends the copy once", s["text"], "abcdefghabcdefgh")
    raw0, held0, ret0 = len(enters()), len(withheld()), returns()
    key("Return")
    s = wait_for(dev, lambda b: b["returns"] != ret0)
    case.check("single-line: Return inserts nothing (gtk::Text has no newline to insert)",
               s["text"], "abcdefghabcdefgh")
    case.check("single-line: exactly one ReturnPressed call", s["returns"] - ret0, 1)
    case.check("single-line: the engine was synced the final text first",
               last_synced(), "abcdefghabcdefgh")
    case.check("single-line: no raw Return reached the engine", len(enters()) - raw0, 0)
    case.check("single-line: the client withheld that Return", len(withheld()) - held0, 1)
    time.sleep(0.8)
    s = dev.textbox()
    case.check("single-line: the editor keeps its text, and focus, until the engine says otherwise",
               (s["text"], s["focus"] != "none"), ("abcdefghabcdefgh", True))
    # And the engine's push reaches this widget as well.
    dev.send("enginetext cleared")
    s = wait_for(dev, lambda b: b["text"] == "cleared")
    case.check("single-line: an engine push replaces the text", (s["text"], s["caret"]), ("cleared", 7))
    type_("!")
    s = wait_for(dev, lambda b: b["chars"] == 8)
    case.check("single-line: typing builds on the pushed text", s["text"], "cleared!")

    # The short form of the same contract, with the engine's two possible
    # answers to Enter delivered by hand: it clears the box, or it lets go.
    dev.send("fakeblur")
    time.sleep(0.6)
    dev.send("fakefocus 0 300 300 400 40")
    wait_for(dev, lambda b: b["focus"] != "none" and b["x"] != "none"
             and float(b["w"]) > 0, timeout=5.0)
    time.sleep(1.0)
    type_("ok")
    wait_for(dev, lambda b: b["chars"] == 2)
    raw0, ret0 = len(enters()), returns()
    key("Return")
    s = wait_for(dev, lambda b: b["returns"] != ret0)
    time.sleep(0.8)
    s = dev.textbox()
    case.check("single-line 'ok' + Enter: one ReturnPressed, no raw Return",
               (s["returns"] - ret0, len(enters()) - raw0), (1, 0))
    case.check("single-line 'ok' + Enter: the editor still holds 'ok' and the focus",
               (s["text"], s["focus"] != "none"), ("ok", True))
    dev.send("enginetext ")
    s = wait_for(dev, lambda b: b["chars"] == 0)
    case.check("the engine clears it", (s["text"], s["focus"] != "none"), ("", True))
    dev.send("fakeblur")
    s = wait_for(dev, lambda b: b["focus"] == "none")
    case.check("the engine lets go (hideKeyboard)", s["focus"], "none")

    dev.send("fakeblur")
    time.sleep(0.6)


def run_cases(case, dev, kbd, ptr, log_path, args, display):
    print("\n-- 1. a click focuses a TextBox and the editor gets real geometry")
    box = focus_box(case, dev)
    case.check("a TextBox has focus", box["focus"] != "none", True)
    if box["focus"] == "none":
        shot = os.path.join(OUT, "no-focus.png")
        in_box(f"WAYLAND_DISPLAY={display} grim {shot}")
        try:
            dev.send(f"screenshot {os.path.join(OUT, 'no-focus-swapchain.png')}")
        except Exception as e:
            case.note("swapchain capture", f"refused: {e}")
        case.note("what was on screen", shot)
        return
    case.note("geometry", f"x={box['x']} y={box['y']} w={box['w']} h={box['h']} "
                          f"after {box.get('geometry_wait')}s")
    case.check("the box has a real width", box["x"] != "none" and float(box["w"]) > 0, True)
    case.check("the box has a real height", box["x"] != "none" and float(box["h"]) > 0, True)
    log = open(log_path, errors="replace").read()
    placed = re.findall(r"text editor placed from ([a-z ]+) x=", log)
    case.check("the editor ends up on the box", placed[-1] if placed else None,
               "engine geometry")
    # The editor must never be *seen* anywhere else on the way there. A focus
    # whose geometry has not arrived yet used to drop the editor into a bar at
    # the bottom of the window for about a second and then jump it back up,
    # which is visible and is a large part of what "the text field looks wrong"
    # means.
    case.check("the editor never flashed in the fallback bar",
               placed.count("fallback bar"), 0)

    # Clicking the search bar opens a modal, and the modal is the box that
    # focuses before it has laid out. Watching the placement source across that
    # transition is the only way to see the flinch this fix is about: by the
    # time anything settles, the editor is on the modal and looks fine.
    sources, deadline = [], time.time() + 4
    while time.time() < deadline:
        sources.append(dev.textbox().get("placed"))
        time.sleep(0.1)
    seen = sorted(set(s for s in sources if s))
    case.note("placement sources across the modal opening", " ".join(seen) or "none")
    case.check("the editor was never dropped into the placed bar",
               [s for s in seen if s == "fallback"], [])

    def type_(s):
        kbd.cmd(f"type {s}")

    def key(k):
        kbd.cmd(f"key {k}")

    def state():
        return settle(dev)

    print("\n-- 2. a clean start")
    key("ctrl+a"); key("BackSpace")
    s = state()
    case.check("the box empties", (s["text"], s["chars"], s["caret"]), ("", 0, 0))

    print("\n-- 3. typing five keys inserts five characters (the double-insert guard)")
    type_("hello")
    s = state()
    case.check("text after typing 'hello'", s["text"], "hello")
    case.check("caret after typing 'hello'", s["caret"], 5)

    print("\n-- 4. backspace")
    key("BackSpace")
    s = state()
    case.check("text after backspace", s["text"], "hell")
    case.check("caret after backspace", s["caret"], 4)

    print("\n-- 5. Home, and insertion at the caret rather than the end")
    key("Home")
    case.check("caret after Home", state()["caret"], 0)
    type_("X")
    s = state()
    case.check("text after inserting at the start", s["text"], "Xhell")
    case.check("caret after inserting at the start", s["caret"], 1)

    print("\n-- 6. End")
    key("End")
    case.check("caret after End", state()["caret"], 5)
    type_("!")
    s = state()
    case.check("text after appending", s["text"], "Xhell!")

    print("\n-- 7. shift-arrow selection, replaced by the next character")
    key("shift+Left"); key("shift+Left")
    type_("?")
    s = state()
    case.check("selection was replaced, not appended", s["text"], "Xhel?")
    case.check("caret after replacing a selection", s["caret"], 5)

    print("\n-- 8. select all, then overtype")
    key("ctrl+a")
    type_("abcdefgh")
    s = state()
    case.check("text after select-all and overtype", s["text"], "abcdefgh")
    case.check("caret after select-all and overtype", s["caret"], 8)

    print("\n-- 9. clicking positions the caret (a real click, through the compositor)")
    extent = re.search(r"extent=(\d+)x(\d+)", dev.send("info"))
    sw, shh = int(extent.group(1)), int(extent.group(2))
    header = args.height - shh
    case.note("surface", f"{sw}x{shh} inside a {args.width}x{args.height} output, "
                         f"header {header}px")
    box = settle(dev)
    if box["x"] == "none" or float(box["w"]) <= 0:
        case.check("the box still reports where it is, for the click test",
                   False, True)
        return
    bx, by, bw, bh = (float(box[k]) for k in ("x", "y", "w", "h"))
    row = int(by + bh / 2) + header

    def click_at(surface_x):
        ptr.cmd(f"move {int(surface_x)} {row}")
        ptr.cmd("click")
        time.sleep(0.6)
        return dev.textbox()

    left = click_at(bx + 2)
    case.check("clicking the left edge puts the caret at the start", left["caret"], 0)
    case.check("clicking inside the box keeps it focused", left["focus"] != "none", True)
    right = click_at(bx + bw - 4)
    case.check("clicking past the last character puts the caret at the end",
               right["caret"], 8)
    case.check("clicking did not change the text", right["text"], "abcdefgh")

    print("\n-- 9b. a double-click selects a word, and the next key replaces it")
    key("ctrl+a")
    type_("alpha beta")
    s2 = state()
    case.check("a string with a space in it survives", s2["text"], "alpha beta")
    # Land on the first word. A quarter of the way in is inside "alpha" for any
    # plausible font, and the assertion below does not depend on which
    # character it lands on -- only on the word boundary GTK finds from it.
    ptr.cmd(f"move {int(bx + bw * 0.08)} {row}")
    ptr.cmd("click")
    ptr.cmd("click", settle=0.05)
    time.sleep(0.5)
    type_("Z")
    s2 = state()
    case.check("double-click selected the word, and typing replaced just that word",
               s2["text"], "Z beta")

    print("\n-- 9c. Delete removes forwards, and cut removes the selection")
    key("ctrl+a")
    type_("abcdef")
    key("Home")
    key("Delete")
    case.check("Delete removes the character after the caret", state()["text"], "bcdef")
    key("ctrl+a"); key("ctrl+x")
    time.sleep(0.5)
    case.check("cut empties the field", state()["text"], "")

    print("\n-- 10. copy and paste")
    type_("abcdefgh")
    key("ctrl+a"); key("ctrl+c"); key("End")
    key("ctrl+v")
    time.sleep(1.0)
    s = state()
    case.check("paste appends the copied text exactly once", s["text"], "abcdefghabcdefgh")

    print("\n-- 11. Escape gives the box up")
    key("Escape")
    time.sleep(1.0)
    case.check("Escape blurs the TextBox", dev.textbox()["focus"], "none")

    print("\n-- 12. focusing again starts clean and still takes keys")
    box2 = focus_box(case, dev)
    case.check("the box takes focus a second time", box2["focus"] != "none", True)
    if box2["focus"] != "none":
        key("ctrl+a")
        type_("second")
        s = state()
        case.check("typing works after a refocus", s["text"], "second")

    print("\n-- 12c. the editor follows the window when it resizes")
    # **The case docs/NEXT.md says was never tested.** `editor_rect` is in
    # surface coordinates and the input region is punched from it, and the
    # region is rebuilt on placement and stacking changes -- not on a
    # configure. So the question is whether anything re-places the editor after
    # the surface changes size, or whether the hole is left where the box used
    # to be.
    #
    # It is not obviously broken: `sync_text_overlay` runs off the pump about
    # twenty times a second, re-reads the engine's geometry every 100ms, and
    # rebuilds when what it is about to draw differs from what it drew. If the
    # engine re-lays-out on resize, that should self-heal within a tick or two.
    # This asserts it rather than assuming it either way.
    #
    # Sober #1026 is the reason to care: same engine, same kind of desktop, and
    # users report the textbox dying *only* in fullscreen, only on Wayland, one
    # of them pinning it to the surface being exactly the output size.
    if box2["focus"] != "none":
        before = dev.textbox()
        case.note("editor rect before resize",
                  f"x={before.get('x')} y={before.get('y')} "
                  f"w={before.get('w')} h={before.get('h')}")
        new_w, new_h = int(args.width) - 120, int(args.height) - 80
        r = swaymsg(f"output HEADLESS-1 mode {new_w}x{new_h}")
        # **Prove the resize happened before asserting anything about it.**
        # The first version of this case did not, and passed while reporting a
        # byte-identical rectangle either side -- which is exactly what a
        # swaymsg that silently did nothing would produce. An instrument that
        # cannot fail is not evidence, and this suite exists because that
        # mistake has already been made here more than once.
        modes = swaymsg("-t get_outputs").stdout
        case.note("swaymsg said", (r.stdout or r.stderr).strip()[:120])
        got_mode = ""
        try:
            for o in json.loads(modes):
                if o.get("name") == "HEADLESS-1":
                    cm = o.get("current_mode") or {}
                    got_mode = f"{cm.get('width')}x{cm.get('height')}"
        except Exception as e:
            case.note("could not read the output mode", str(e))
        case.check("the output really did change size",
                   got_mode, f"{new_w}x{new_h}")
        # Generous: the engine has to notice the configure, re-lay-out, and
        # publish new geometry, and only then does the 100ms poll pick it up.
        time.sleep(3.0)
        after = dev.textbox()
        case.note("editor rect after resize",
                  f"x={after.get('x')} y={after.get('y')} "
                  f"w={after.get('w')} h={after.get('h')} placed={after.get('placed')}")

        # Three separate assertions, because they fail independently and the
        # third passing while the first fails is exactly the "the path worked"
        # illusion this suite exists to avoid.
        case.check("the box still has focus after a resize",
                   after.get("focus") != "none", True)
        case.check("the editor is still placed from engine geometry",
                   after.get("placed"), "engine")
        # And the one that matters: can you still type into it. A rectangle in
        # the right place proves nothing if keys no longer land.
        key("ctrl+a")
        type_("resized")
        s = state()
        case.check("typing still reaches the box after a resize", s["text"], "resized")

        swaymsg(f"output HEADLESS-1 mode {args.width}x{args.height}")
        time.sleep(2.0)

    run_enter_and_engine_text(case, dev, kbd, log_path)

    print("\n-- 12b. with no box focused, keys reach the game again")
    # Everything after this mark is deliberately sent with nothing focused, so
    # step 13 stops reading here rather than counting this on purpose.
    focused_phase = len(open(log_path, errors="replace").read())
    key("Escape")
    time.sleep(1.0)
    before = len(re.findall(r"pass_key_event down=\w+ code=17 ",
                            open(log_path, errors="replace").read()))
    key("w")
    time.sleep(0.6)
    after = len(re.findall(r"pass_key_event down=\w+ code=17 ",
                           open(log_path, errors="replace").read()))
    # The suppression is only correct if it stops. A guard stuck on is the
    # failure that looks like "movement is broken" and never like a text bug.
    case.check("'w' reaches the game once the box has been given up", after > before, True)

    print("\n-- 13. no character reached the game while a box had focus")
    log = open(log_path, errors="replace").read()[:focused_phase]
    suppressed = len(re.findall(r"pass_key_event suppressed", log))
    forwarded = [int(m) for m in re.findall(r"pass_key_event down=\w+ code=(\d+)", log)]
    leaked = sorted({c for c in forwarded if c in TEXT_KEYS})
    case.note("keys", f"{suppressed} suppressed, {len(forwarded)} forwarded to the game")
    case.check("text keys were suppressed at all", suppressed > 0, True)
    case.check("no text key reached the game's key handler", leaked, [])

    print("\n-- 14. the client is still alive")
    case.check("the client survived the whole run", dev.send("ping"), "")


# ----------------------------------------------------------------- visibility
#
# `cordial_textbox` reports what the GTK field *holds*. Whether anybody can see
# it is a different question, and the one #86 asks ("the text I type in chat
# doesn't show up, but it's still there"): the field can hold every character
# while the surface that would show them is never presented, is covered, or
# draws them in the colour of what is behind them. `cordial_screenshot` reads
# the engine's swapchain and cannot see a GTK widget, so these cases photograph
# the *compositor's* output with grim and compare the editor's rectangle with and
# without the typed text.


def write_png(path, w, h, rgb):
    """A minimal PNG writer, so a failing run leaves a picture without needing PIL."""
    import zlib, struct
    raw = b"".join(b"\x00" + rgb[y * w * 3:(y + 1) * w * 3] for y in range(h))

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xffffffff)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


def grab(display, name):
    """Photograph the compositor's output; returns (w, h, rgb bytes) and writes `name`.png."""
    ppm = os.path.join(OUT, name + ".ppm")
    r = in_box(f"WAYLAND_DISPLAY={display} grim -t ppm {ppm}")
    if r.returncode != 0:
        raise RuntimeError(f"grim failed: {r.stderr.strip()}")
    data = open(ppm, "rb").read()
    os.unlink(ppm)
    parts = data.split(b"\n", 3)
    w, h = (int(v) for v in parts[1].split())
    rgb = parts[3]
    write_png(os.path.join(OUT, name + ".png"), w, h, rgb)
    return w, h, rgb


def crop_diff(a, b, scale, rect, tol=16):
    """Pixels inside `rect` (logical x, y, w, h) that differ by more than `tol` in any channel.

    `tol` is low on purpose. The synthetic box carries the engine's own light
    grey (0xd5d5dd) and lands partly over the landing page's white "Create
    Account" button, where the text is only ~33 levels from its background; at
    40 the same drawn text counted as "nothing changed" and one case failed on
    a screen that plainly showed it.
    """
    (w, h, pa), (_, _, pb) = a, b
    x0, y0 = max(0, int(rect[0] * scale)), max(0, int(rect[1] * scale))
    x1, y1 = min(w, int((rect[0] + rect[2]) * scale)), min(h, int((rect[1] + rect[3]) * scale))
    n = 0
    for y in range(y0, y1):
        ra, rb = pa[(y * w + x0) * 3:(y * w + x1) * 3], pb[(y * w + x0) * 3:(y * w + x1) * 3]
        if ra == rb:
            continue
        for i in range(0, len(ra), 3):
            if (abs(ra[i] - rb[i]) > tol or abs(ra[i + 1] - rb[i + 1]) > tol
                    or abs(ra[i + 2] - rb[i + 2]) > tol):
                n += 1
    return n


def count_colour(img, scale, rect, rgb, tol=6):
    """Pixels inside `rect` within `tol` of `rgb`: how much ink is in the box's own colour."""
    w, h, px = img
    x0, y0 = max(0, int(rect[0] * scale)), max(0, int(rect[1] * scale))
    x1, y1 = min(w, int((rect[0] + rect[2]) * scale)), min(h, int((rect[1] + rect[3]) * scale))
    n = 0
    for y in range(y0, y1):
        row = px[(y * w + x0) * 3:(y * w + x1) * 3]
        for i in range(0, len(row), 3):
            if (abs(row[i] - rgb[0]) <= tol and abs(row[i + 1] - rgb[1]) <= tol
                    and abs(row[i + 2] - rgb[2]) <= tol):
                n += 1
    return n


# `fakefocus` hands the editor the one grey every real capture has reported,
# 0xd5d5dd, so ink that is not this colour is the theme's colour winning over
# the engine's. That is the failure that hides text: a light theme's near-black
# on a dark game, or a dark theme's white on a white button.
FAKE_INK = (0xd5, 0xd5, 0xdd)
VIS_INK_MIN = 8


def window_origin(pid):
    """Where the client's surface starts on the output, in logical pixels, plus the output size."""
    tree = json.loads(swaymsg("-t get_tree").stdout)
    out = json.loads(swaymsg("-t get_outputs").stdout)[0]["rect"]
    found = []

    def walk(n):
        if n.get("pid") == pid and n.get("type") in ("con", "floating_con"):
            found.append(n)
        for c in n.get("nodes", []) + n.get("floating_nodes", []):
            walk(c)
    walk(tree)
    if not found:
        # An Xwayland client's pid is not always the one sway records; with a
        # single window on the output the only leaf view is the client.
        def leaves(n):
            kids = n.get("nodes", []) + n.get("floating_nodes", [])
            if not kids and n.get("type") in ("con", "floating_con") and (n.get("window") or n.get("app_id")):
                found.append(n)
            for c in kids:
                leaves(c)
        leaves(tree)
    if not found:
        return None
    n = found[0]
    return (n["rect"]["x"] + n["window_rect"]["x"] - out["x"],
            n["rect"]["y"] + n["window_rect"]["y"] - out["y"], out["width"], out["height"],
            n["window_rect"]["width"], n["window_rect"]["height"])


VIS_TEXT = "MMMMMMMMMMMM"
# A twelve-character run of capital Ms at the synthetic box's 16 px is several
# hundred strokes of ink; the caret alone is a column of about twenty. The floor
# is far under the first and far over the second, and the noise check below
# proves two photographs of the same screen differ by less than it.
VIS_MIN_PIXELS = 120
VIS_NOISE_MAX = 50


def visibility_case(case, label, dev, kbd, pid, display, multiline=False):
    """Type into a synthetic box and assert the composited output changed where the editor is."""
    dev.send("fakeblur")
    time.sleep(0.6)
    # Inside the canvas at whatever size it is. A box that does not fit is
    # refused for the fallback bar (`info_fits_canvas`), correctly, and the
    # engine's space is the canvas's, which at output scale 2 is 640 wide: the
    # first scale-2 run put a box at x=300 w=400 and read the fallback bar's
    # behaviour as a finding.
    org = window_origin(pid)
    ext = re.search(r"extent=(\d+)x(\d+)", dev.send("info"))
    cw, ch = int(ext.group(1)), int(ext.group(2))
    if cw == 0 or ch == 0:
        # The GLES engine reports no swapchain extent (`extent=0x0`), and a box
        # sized from it is zero-sized, which the client answers with the
        # fallback bar -- the first no-Vulkan run measured that bar and called
        # it the editor's placement. Size from the window instead: its content
        # is the window less a 46 px header bar, unless it fills the output.
        _, _, _, oh_, win_w_, win_h_ = org
        cw, ch = win_w_, win_h_ - (0 if win_h_ >= oh_ - 1 else 46)
    bw, bh = min(400, int(cw * 0.6)), (120 if multiline else 40)
    dev.send(f"fakefocus {1 if multiline else 0} {int(cw * 0.15)} {int(ch * 0.35)} {bw} {bh}")
    box = wait_for(dev, lambda b: b["focus"] != "none" and b["x"] != "none"
                   and float(b["w"]) > 0, timeout=5.0)
    time.sleep(1.5)                     # GTK takes keyboard focus a tick after placement
    ok_focus = case.check(f"[{label}] the synthetic box took focus", box["focus"] != "none", True)
    if not ok_focus:
        return
    # So that a run photographing the fallback bar cannot pass as one
    # photographing the box; the rectangle below is only right for the former.
    case.check(f"[{label}] the editor is on the box (engine geometry), not the fallback bar",
               dev.textbox().get("placed"), "engine")
    kbd.cmd("key ctrl+a"); kbd.cmd("key BackSpace")
    time.sleep(0.8)
    origin = window_origin(pid)
    if origin is None:
        case.check(f"[{label}] the client's window was found in sway's tree", False, True)
        return
    ox, oy, ow, oh, win_w, win_h = origin
    box = dev.textbox()
    # The editor's rectangle is in the engine's surface coordinates, and the
    # surface is the window less its header bar. Taking the window's own origin
    # put the rectangle 46 px too high, and four single-line cases "failed"
    # photographing empty space above the box.
    ext = re.search(r"extent=(\d+)x(\d+)", dev.send("info"))
    ext_h = int(ext.group(2)) or (win_h - (0 if win_h >= oh - 1 else 46))
    header = max(0, win_h - ext_h) if (win_h - ext_h) > 1 else 0
    rect = (ox + float(box["x"]) - 4, oy + header + float(box["y"]) - 4,
            float(box["w"]) + 8, float(box["h"]) + 8)
    if int(ext.group(1)) == 0:
        # No swapchain extent means the GLES engine, whose canvas geometry does
        # not follow the window (it still read 1184x658 after a resize), so the
        # editor's rectangle cannot be located from it. Compare the whole
        # window: the page behind it is static, and the noise check still
        # proves two photographs of it agree.
        rect = (ox, oy, win_w, win_h)
    slug = re.sub(r"[^a-z0-9]+", "-", label.lower()).strip("-")
    empty1 = grab(display, f"{slug}-empty")
    empty2 = grab(display, f"{slug}-empty-again")
    scale = empty1[0] / ow
    noise = crop_diff(empty1, empty2, scale, rect)
    kbd.cmd(f"type {VIS_TEXT}")
    wait_for(dev, lambda b: b["chars"] == len(VIS_TEXT))
    time.sleep(1.0)                     # a frame for the compositor to show the new text
    typed = grab(display, f"{slug}-typed")
    delta = crop_diff(empty1, typed, scale, rect)
    held = dev.textbox()
    case.note("window origin / header / output", f"{ox},{oy} +{header} on {ow}x{oh}, image {empty1[0]}x{empty1[1]} (scale {scale:.2f})")
    case.note("editor rect on the output", f"{rect[0]:.0f},{rect[1]:.0f} {rect[2]:.0f}x{rect[3]:.0f}")
    case.note("photographs", os.path.join(OUT, slug + "-{empty,typed}.png"))
    case.check(f"[{label}] the field holds the typed characters", held["chars"], len(VIS_TEXT))
    # Scaled with the area: the same rectangle is scale^2 times as many pixels.
    noise_max = int(VIS_NOISE_MAX * scale * scale)
    case.check(f"[{label}] two photographs of the same screen agree (noise <= {noise_max})",
               noise <= noise_max, True)
    case.note("noise / delta pixels", f"{noise} / {delta}")
    case.check(f"[{label}] the typed characters are VISIBLE (>= {VIS_MIN_PIXELS} changed pixels)",
               delta >= VIS_MIN_PIXELS, True)
    ink = count_colour(typed, scale, rect, FAKE_INK)
    ink_before = count_colour(empty1, scale, rect, FAKE_INK)
    case.note("pixels in the engine's text colour, before / after typing", f"{ink_before} / {ink}")
    case.check(f"[{label}] the text is drawn in the box's own colour, not the theme's "
               f"(>= {VIS_INK_MIN} more pixels of #d5d5dd)", ink - ink_before >= VIS_INK_MIN, True)


def run_visibility(case, dev, kbd, pid, display, args):
    modes = [m.strip() for m in args.visibility.split(",") if m.strip()]
    print(f"\n-- V. typed text is visible in the composited output: {', '.join(modes)}"
          f"{f' at scale {args.scale}' if args.scale else ''}")
    for mode in modes:
        if mode == "windowed":
            pass
        elif mode == "fullscreen":
            swaymsg("fullscreen enable")
            time.sleep(3.0)
        elif mode == "tiled-after":
            swaymsg("fullscreen enable"); time.sleep(2.0)
            swaymsg("fullscreen disable"); time.sleep(3.0)
        elif mode == "floating":
            swaymsg("floating enable"); time.sleep(0.5)
            swaymsg("resize set 900 600"); swaymsg("move position 100 80")
            time.sleep(3.0)
        else:
            sys.exit(f"unknown visibility mode {mode!r}")
        case.note("extent", re.search(r"extent=\S+", dev.send("info")).group(0))
        visibility_case(case, mode + " single-line", dev, kbd, pid, display, multiline=False)
        visibility_case(case, mode + " multi-line", dev, kbd, pid, display, multiline=True)
        if mode == "floating":
            swaymsg("floating disable"); time.sleep(2.0)
        elif mode == "fullscreen":
            swaymsg("fullscreen disable"); time.sleep(2.0)


# --------------------------------------------------------------------- X11


class XSession:
    """A private X server and window manager, and one shell inside the container that drives them.

    **Never the developer's display.** Every command here runs with `DISPLAY`
    set to the server this class started, and XTest key events go to that
    server and nowhere else -- AGENTS.md forbids injecting at the developer's
    session, and a private Xvfb is what makes injecting at all permissible. The
    display number is chosen from ones with no socket and no lock file, and the
    server's own pid is what is killed afterwards, never a name match.

    One long-lived shell rather than a `distrobox enter` per command, because
    entering the container costs most of a second and a typed string is a dozen
    commands.
    """

    def __init__(self, wm, width, height, xwayland=False):
        self.wm = wm
        self.xwayland = xwayland
        self.rt = os.path.join(OUT, "rt")
        os.makedirs(self.rt, mode=0o700, exist_ok=True)
        if xwayland:
            return self._init_xwayland(width, height)
        self.num = self._free_display()
        self.display = f":{self.num}"
        self.p = subprocess.Popen(
            # `bash -c`, not `-lc`: the login shell's rc file reads stdin, and
            # the first command sent was never answered.
            ["distrobox", "enter", BOX, "--", "bash", "-c",
             f"export DISPLAY={self.display} XDG_RUNTIME_DIR={self.rt} "
             f"XDG_CONFIG_HOME={OUT}/wm-config; mkdir -p $XDG_CONFIG_HOME; "
             'while IFS= read -r l; do eval "$l"; echo "__END__ $?"; done'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.pids = []
        xvfb = self.sh(f"nohup setsid Xvfb {self.display} -screen 0 {width}x{height}x24 +extension COMPOSITE "
                       f">{OUT}/xvfb.log 2>&1 & echo $!")
        self.pids.append(xvfb.strip())
        for _ in range(50):
            if self.sh(f"test -S /tmp/.X11-unix/X{self.num} && echo up").strip() == "up":
                break
            time.sleep(0.2)
        else:
            raise RuntimeError(f"Xvfb never came up on {self.display}; see {OUT}/xvfb.log")
        wm_cmd = {"openbox": "openbox", "xfwm4": "xfwm4 --compositor=off", "xfwm4c": "xfwm4 --compositor=on",
                  "icewm": "icewm", "none": ""}[wm]
        if wm_cmd:
            self.pids.append(self.sh(f"nohup setsid {wm_cmd} >{OUT}/wm.log 2>&1 & echo $!").strip())
            time.sleep(2.0)
        print(f"== private X server {self.display} ({wm_cmd or 'no window manager'}), "
              f"{width}x{height}", flush=True)

    def _init_xwayland(self, width, height):
        """sway with its own Xwayland, in a private mount namespace.

        sway's integrated Xwayland refuses `/tmp/.X11-unix` because it is
        root-owned -- the failure that stopped the first attempt at an X11
        photograph. A user and mount namespace with a tmpfs over that directory
        gets past it; the abstract socket Xwayland also listens on is per
        *network* namespace, which is not unshared, so the client and xdotool
        outside still connect. `focus_follows_mouse no` and borderless windows
        keep the compositor from moving anything the cases measure.
        """
        stamp = lambda n: os.path.join(OUT, n)
        for n in ("wl.display", "x.display", "swaysock"):
            if os.path.exists(stamp(n)):
                os.unlink(stamp(n))
        cfg = os.path.join(OUT, "sway-xwl.cfg")
        with open(cfg, "w") as fh:
            fh.write(f"""xwayland enable
output HEADLESS-1 mode {width}x{height}
default_border none
default_floating_border none
focus_follows_mouse no
for_window [title="^otherapp$"] floating enable
exec sh -c 'printf %s "$WAYLAND_DISPLAY" > {stamp("wl.display")}; printf %s "$DISPLAY" > {stamp("x.display")}; printf %s "$SWAYSOCK" > {stamp("swaysock")}'
""")
        self.p = subprocess.Popen(
            ["distrobox", "enter", BOX, "--", "bash", "-c", 'while IFS= read -r l; do eval "$l"; echo "__END__ $?"; done'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.pids = []
        pid = self.sh(f"nohup setsid unshare -Urm sh -c 'mount -t tmpfs none /tmp/.X11-unix && "
                      f"exec env -u WAYLAND_DISPLAY -u DISPLAY -u SWAYSOCK XDG_RUNTIME_DIR={self.rt} "
                      f"WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=1 "
                      f"sway -c {cfg}' >{OUT}/sway-xwl.log 2>&1 < /dev/null & echo $!")
        self.pids.append(pid.strip())
        for _ in range(100):
            if os.path.exists(stamp("x.display")) and os.path.getsize(stamp("x.display")):
                break
            time.sleep(0.2)
        else:
            raise RuntimeError(f"sway never reported an X display; see {OUT}/sway-xwl.log")
        self.display = open(stamp("x.display")).read().strip()
        self.wl = open(stamp("wl.display")).read().strip()
        self.swaysock = open(stamp("swaysock")).read().strip()
        self.sh(f"export DISPLAY={self.display} XDG_RUNTIME_DIR={self.rt} WAYLAND_DISPLAY={self.wl} "
                f"SWAYSOCK={self.swaysock}")
        print(f"== sway {self.wl} with Xwayland on {self.display}, {width}x{height}", flush=True)

    def swaymsg(self, args):
        return self.sh(f"swaymsg {args}")

    def bind_accents(self):
        """Give é, ü and ï keycodes of their own, once, before anything is typed.

        `xdotool type` types a character the keymap lacks by remapping a spare
        keycode for the length of one key press, and GTK sometimes sees the key
        before the remap: the same string lost its é in one run and its ü in
        another, on different servers, and `xev` loses the first key of a
        remapped string in the same way. A flaky instrument asserting on a
        non-ASCII string would say nothing about the editor, so the three
        characters are bound permanently up front and sent as keysyms, on
        keycodes 250 to 252. 200 to 202 were tried first and `ï` arrived as
        Ctrl+ï: those keycodes carry a modifier in the default keymap, and the
        character was dropped by the widget, correctly.
        """
        self.sh("xmodmap -e 'keycode 250 = eacute' -e 'keycode 251 = udiaeresis' -e 'keycode 252 = idiaeresis'")
        time.sleep(0.5)

    @staticmethod
    def _free_display():
        for n in range(90, 140):
            if not os.path.exists(f"/tmp/.X11-unix/X{n}") and not os.path.exists(f"/tmp/.X{n}-lock"):
                return n
        raise RuntimeError("no free X display number between 90 and 139")

    def sh(self, cmd, timeout=40):
        """Run `cmd` in the container's shell and return its stdout."""
        self.p.stdin.write(cmd + "\n")
        self.p.stdin.flush()
        out, deadline = [], time.time() + timeout
        while time.time() < deadline:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError(f"the container shell closed while running {cmd!r}")
            if line.startswith("__END__"):
                return "".join(out)
            out.append(line)
        raise RuntimeError(f"{cmd!r} did not finish in {timeout}s")

    def cmd(self, line, settle=0.15):
        """The same two verbs the Wayland cases send a held virtual keyboard."""
        import shlex
        verb, _, rest = line.partition(" ")
        if verb == "type":
            self.sh(f"xdotool type --delay 45 -- {shlex.quote(rest)}")
        elif verb == "key":
            self.sh(f"xdotool key {rest}")
        else:
            raise ValueError(line)
        time.sleep(settle)

    primed = False

    def prime(self):
        """Send one harmless key, once, before the first typing.

        Under Xwayland the first XTest key event after a window is first mapped
        is lost: `xev`, which is not Cordial, loses the `h` of "hello" the same
        way, on every new window. A lost first character here would be a
        failure of the instrument read as a failure of the editor. Real
        keyboard events do not come through XTest and were not measured.
        """
        if self.xwayland and not self.primed:
            self.primed = True
            self.cmd("key Shift_L", settle=0.4)

    def info(self, win):
        """`(x, y, w, h, mapped)` of a window, absolute on the root, or None if it does not exist."""
        out = self.sh(f"xwininfo -id {win} 2>&1")
        g = lambda k: re.search(k + r":\s+(-?\d+)", out)
        if "Map State" not in out or not g("Absolute upper-left X"):
            return None
        return (int(g("Absolute upper-left X").group(1)), int(g("Absolute upper-left Y").group(1)),
                int(g("Width").group(1)), int(g("Height").group(1)), "IsViewable" in out)

    def find(self, name_regex):
        import shlex
        out = self.sh(f"xdotool search --name {shlex.quote(name_regex)}").split()
        return out[0] if out else None

    def focus_name(self):
        return self.sh("xdotool getwindowfocus getwindowname 2>/dev/null").strip()

    def topmost(self):
        """The topmost top-level window under the root, as xwininfo prints it.

        xwininfo lists children topmost first (it walks XQueryTree's bottom-to-top
        array backwards), so the first line is the one on top. Assuming the
        opposite made the stacking assertion fail against a correct stack.
        """
        tree = self.sh("xwininfo -root -tree")
        tops = [l for l in tree.splitlines() if re.match(r"^     0x[0-9a-f]+ ", l)]
        return tops[0].strip() if tops else ""

    def grab(self, name):
        ppm = os.path.join(OUT, name + ".ppm")
        # `-depth 8`: ImageMagick's PPM is 16 bits a channel by default, which
        # reads as a 1280x800 picture of diagonal stripes if parsed as 8.
        if self.xwayland:
            # The X root of a rootless Xwayland is empty; what is on screen is
            # the compositor's output.
            self.sh(f"grim -t ppm {ppm}")
        else:
            self.sh(f"import -depth 8 -window root ppm:{ppm}")
        data = open(ppm, "rb").read()
        os.unlink(ppm)
        parts = data.split(b"\n", 3)
        w, h = (int(v) for v in parts[1].split())
        assert parts[2].strip() == b"255", f"not an 8-bit PPM: {parts[2]!r}"
        write_png(os.path.join(OUT, name + ".png"), w, h, parts[3])
        return w, h, parts[3]

    def close(self):
        for pid in reversed(self.pids):
            try:
                self.sh(f"kill {pid} 2>/dev/null; true", timeout=10)
            except Exception:
                pass
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def x11_box(dev, xs, multiline, x=None, y=None, w=None, h=None, text=""):
    """Focus a synthetic box and wait for the editor window to be mapped and holding X focus.

    Returns the engine window id and geometry, the editor window id and the box
    reading. The editor is placed from the engine's spec, so the box rectangle
    is a fraction of the *engine window's* own size: a box outside the canvas is
    refused for the fallback bar by `info_fits_canvas`, correctly, and
    photographing that bar would be reading a different rule.
    """
    dev.send("fakeblur")
    time.sleep(0.8)
    eng = xs.find(r"^Cordial [0-9]")
    ex, ey, ew, eh, _ = xs.info(eng)
    bw = w or min(400, int(ew * 0.5))
    bh = h or (120 if multiline else 40)
    bx = x if x is not None else int(ew * 0.15)
    by = y if y is not None else int(eh * 0.35)
    dev.send(f"fakefocus {1 if multiline else 0} {bx} {by} {bw} {bh}" + (f" {text}" if text else ""))
    editor_off = os.environ.get("CORDIAL_X11_EDITOR_OFF_EXPECTED") == "1"
    box = wait_for(dev, lambda b: b["focus"] != "none" and (editor_off or (b["x"] != "none" and float(b["w"]) > 0)),
                   timeout=6.0)
    if editor_off and box["x"] == "none":
        # The control arm has no editor, and `textbox` reports where the editor
        # is. The box is where this function asked the engine to put it.
        box = dict(box, x=str(bx), y=str(by), w=str(bw), h=str(bh), placed="engine")
    ed = None
    for _ in range(40):
        ed = xs.find(r"^Cordial text editor$")
        if ed:
            info = xs.info(ed)
            if info and info[4] and xs.focus_name() == "Cordial text editor":
                break
        time.sleep(0.15)
    xs.prime()
    return eng, (ex, ey, ew, eh), ed, box


def x11_visibility(case, label, dev, xs, multiline=False, editor_expected=True):
    """Type into a synthetic box on X11 and assert the X display changed where the editor is.

    The same instrument as `visibility_case`, for a different display server:
    the photograph is of the whole private screen, so it is what a person
    looking at that display would see.
    """
    eng, (ex, ey, ew, eh), ed, box = x11_box(dev, xs, multiline)
    if not case.check(f"[{label}] the synthetic box took focus", box["focus"] != "none", True):
        return
    case.check(f"[{label}] the engine placed the box (not the fallback bar)", box.get("placed"), "engine")
    info = xs.info(ed) if ed else None
    bx, by, bw, bh = (float(box[k]) for k in ("x", "y", "w", "h"))
    if info is None and not editor_expected:
        # The control arm (CORDIAL_X11_EDITOR=off): no editor window exists, and
        # the assertions below about where it is cannot be asked. What *can* be
        # asked is the thing the editor is for -- does the typed text reach the
        # screen -- and it must fail while the readback keeps passing.
        case.note(f"[{label}]", "no editor window, as expected for the control; photographing anyway")
    else:
        case.check(f"[{label}] the editor window is mapped", bool(info and info[4]), True)
        if not info:
            return
        # Exact, except under GDK_SCALE=N, where GDK keeps a window's position on
        # its own logical grid and rounds it down: (193, 273) became (192, 272)
        # at 2. Up to N-1 pixels is that rounding, and anything more is not.
        tol = int(os.environ.get("CORDIAL_E2E_POS_TOL", "0"))
        want_xy = (ex + round(bx), ey + round(by))
        got_xy = (info[0], info[1])
        near = all(0 <= w - g <= tol for g, w in zip(got_xy, want_xy))
        case.check(f"[{label}] the editor window sits on the box (root x,y = engine origin + box"
                   f"{f', down to GDK_SCALE rounding, {tol}px' if tol else ''})",
                   got_xy if not near else want_xy, want_xy)
        if not multiline:
            case.check(f"[{label}] and is the box's size", (info[2], info[3]), (round(bw), round(bh)))
        case.check(f"[{label}] the editor holds the X keyboard focus", xs.focus_name(), "Cordial text editor")
    xs.cmd("key ctrl+a"); xs.cmd("key BackSpace")
    time.sleep(0.6)
    rect = (ex + bx - 4, ey + by - 4, bw + 8, bh + 8)
    slug = re.sub(r"[^a-z0-9]+", "-", label.lower()).strip("-")
    empty1 = xs.grab(f"{slug}-empty")
    empty2 = xs.grab(f"{slug}-empty-again")
    noise = crop_diff(empty1, empty2, 1.0, rect)
    xs.cmd(f"type {VIS_TEXT}")
    wait_for(dev, lambda b: b["chars"] == len(VIS_TEXT))
    time.sleep(0.8)
    typed = xs.grab(f"{slug}-typed")
    delta = crop_diff(empty1, typed, 1.0, rect)
    held = dev.textbox()
    case.note("editor rect on the screen", f"{rect[0]:.0f},{rect[1]:.0f} {rect[2]:.0f}x{rect[3]:.0f}")
    case.note("photographs", os.path.join(OUT, slug + "-{empty,typed}.png"))
    case.check(f"[{label}] the field holds the typed characters", held["chars"], len(VIS_TEXT))
    case.check(f"[{label}] two photographs of the same screen agree (noise <= {VIS_NOISE_MAX})",
               noise <= VIS_NOISE_MAX, True)
    case.note("noise / delta pixels", f"{noise} / {delta}")
    case.check(f"[{label}] the typed characters are VISIBLE (>= {VIS_MIN_PIXELS} changed pixels)",
               delta >= VIS_MIN_PIXELS, True)
    # tol 10, not 6: GTK's glyph edges come out a little darker than the colour
    # asked for -- the commonest ink pixel is (207, 207, 214) for #d5d5dd, seven
    # off in blue -- and at tol 6 the count hangs on the few pixels that are not.
    ink = count_colour(typed, 1.0, rect, FAKE_INK, tol=10)
    ink_before = count_colour(empty1, 1.0, rect, FAKE_INK, tol=10)
    case.note("pixels in the engine's text colour, before / after typing", f"{ink_before} / {ink}")
    case.check(f"[{label}] the text is drawn in the box's own colour "
               f"(>= {VIS_INK_MIN} more pixels of #d5d5dd)", ink - ink_before >= VIS_INK_MIN, True)


def run_x11(case, dev, xs, log_path, args):
    log = lambda: open(log_path, errors="replace").read()
    editor_on = not any(kv.startswith("CORDIAL_X11_EDITOR=off") for kv in args.client_env)
    case.note("compositing on the private server", "yes" if (args.wm == "xfwm4c" or args.xwayland) else "no")

    def x1():
        print("\n-- X1. the editor started on this display")
        case.check("the log says the X11 text editor started",
                   "[android] X11 text editor: a GTK window" in log(), editor_on)
        case.check("the engine is on the X11 backend",
                   "display backend: X11 (forced by CORDIAL_X11" in log(), True)

    def x2():
        print("\n-- X2. single-line: where it is, who holds the keyboard, whether it is visible")
        x11_visibility(case, "single-line", dev, xs, multiline=False, editor_expected=editor_on)

    def x3():
        print("\n-- X3. editing keys belong to the widget")
        x11_box(dev, xs, False)
        xs.cmd("key ctrl+a"); xs.cmd("key BackSpace")
        xs.cmd("type hello")
        s = wait_for(dev, lambda b: b["chars"] == 5)
        case.check("typed 'hello'", s["text"], "hello")
        xs.cmd("key Left"); xs.cmd("key Left"); xs.cmd("type X")
        s = wait_for(dev, lambda b: b["chars"] == 6)
        case.check("a caret move then a character inserts at the caret, not the end", s["text"], "helXlo")
        case.check("and the caret is after the inserted character", s["caret"], 4)
        xs.cmd("key BackSpace")
        s = wait_for(dev, lambda b: b["chars"] == 5)
        case.check("Backspace deletes before the caret", s["text"], "hello")
        xs.cmd("key ctrl+a"); xs.cmd("type Z")
        s = wait_for(dev, lambda b: b["text"] == "Z")
        case.check("select-all then a character replaces the lot", s["text"], "Z")
        xs.cmd("key ctrl+a"); xs.cmd("key BackSpace")
        xs.cmd("type h"); xs.cmd("key eacute"); xs.cmd("type llo "); xs.cmd("key udiaeresis")
        xs.cmd("type n"); xs.cmd("key idiaeresis")
        s = wait_for(dev, lambda b: b["chars"] >= 9, timeout=4)
        case.check("non-ASCII text arrives whole", s["text"], "héllo ünï")

    def x4():
        print("\n-- X4. Enter on a single-line box is an editor action, and no raw key reaches the engine")
        x11_box(dev, xs, False)
        xs.cmd("key ctrl+a"); xs.cmd("key BackSpace")
        xs.cmd("type go")
        wait_for(dev, lambda b: b["chars"] == 2)
        ret0 = dev.textbox()["returns"]
        raw0 = len(re.findall(r"pass_key_event down=true code=(?:28|96) ", log()))
        xs.cmd("key Return")
        time.sleep(0.8)
        case.check("one ReturnPressed call", dev.textbox()["returns"] - ret0, 1)
        case.check("no raw Return reached the engine",
                   len(re.findall(r"pass_key_event down=true code=(?:28|96) ", log())) - raw0, 0)
        case.check("the text is still there (what happens next is the engine's)", dev.textbox()["text"], "go")
        # Escape is how somebody leaves a box, and the editor holds the keyboard,
        # so it has to be sent to the engine from there (evdev code 1).
        esc = lambda: len(re.findall(r"pass_key_event down=true code=1 ", log()))
        esc0 = esc()
        xs.cmd("key Escape")
        time.sleep(0.8)
        case.check("Escape typed in the editor is forwarded to the engine", esc() - esc0, 1)

    def x5():
        print("\n-- X5. multi-line")
        x11_visibility(case, "multi-line", dev, xs, multiline=True, editor_expected=editor_on)
        x11_box(dev, xs, True)
        ret1 = dev.textbox()["returns"]
        xs.cmd("key ctrl+a"); xs.cmd("key BackSpace")
        xs.cmd("type ab"); xs.cmd("key Return"); xs.cmd("type cd")
        s = wait_for(dev, lambda b: b["chars"] == 5)
        case.check("Return in a multi-line box is a newline in the text", s["text"], "ab\ncd")
        case.check("and is not a ReturnPressed call", dev.textbox()["returns"] - ret1, 0)

    def x6():
        print("\n-- X6. focus out hands the keyboard back")
        x11_box(dev, xs, False)
        dev.send("fakeblur")
        time.sleep(1.5)
        ed = xs.find(r"^Cordial text editor$")
        info = xs.info(ed) if ed else None
        case.check("the editor window is unmapped once the box blurs", bool(info and info[4]), False)
        case.check("X keyboard focus is back on the game window", xs.focus_name().startswith("Cordial 0"), True)
        before = dev.textbox()["text"]
        xs.cmd("type zz")
        time.sleep(0.5)
        case.check("a key typed after the blur changes nothing in the box's mirror", dev.textbox()["text"], before)

    def x7():
        print("\n-- X7. the window moves and resizes under the editor")
        eng, (ex, ey, ew, eh), ed, box = x11_box(dev, xs, False)
        want = (round(float(box["x"])), round(float(box["y"])))
        if xs.xwayland:
            # sway places the window; an X client's own move is a request it may
            # ignore. Floating and then moved, which is what a user does.
            xs.swaymsg('"[title=\\"^Cordial 0\\"] floating enable, resize set 1000 640, move position 180 90"')
        else:
            xs.sh(f"xdotool windowmove {eng} 180 90")
        time.sleep(2.5)
        nx, ny, nw, nh, _ = xs.info(eng)
        case.note("window after move", f"{nw}x{nh} at {nx},{ny}")
        bx, by, bw, bh = (int(float(box[k])) for k in ("x", "y", "w", "h"))

        def refocus(dy):
            # A fresh spec for the box, `dy` pixels lower. A resize does not make
            # the engine re-report a box, and `text_geometry::Resolver` (the same
            # rule Wayland has) treats a rectangle repeated *exactly* under a
            # different canvas as stale and sends it to the fallback bar -- which
            # is a different assertion, and was read here as a failure of the
            # origin. sway resizes the window in the same step as it moves it.
            dev.send("fakeblur"); time.sleep(0.5)
            dev.send(f"fakefocus 0 {bx} {by + dy} {bw} {bh}")
            wait_for(dev, lambda b: b["focus"] != "none" and b["x"] != "none" and float(b["w"]) > 0, timeout=5.0)
            time.sleep(1.5)

        if xs.xwayland:
            refocus(1)
        ei = xs.info(ed)
        case.check("the editor followed the window", (ei[0] - nx, ei[1] - ny), (want[0], want[1] + (1 if xs.xwayland else 0)))
        if xs.xwayland:
            xs.swaymsg('"[title=\\"^Cordial 0\\"] resize set 900 600"')
        else:
            xs.sh(f"xdotool windowsize {eng} 900 600")
        time.sleep(2.5)
        nx, ny, nw, nh, _ = xs.info(eng)
        case.note("window after resize", f"{nw}x{nh} at {nx},{ny}")
        refocus(3)
        ei = xs.info(ed)
        case.check("the editor sits on its box under the resized window", (ei[0] - nx, ei[1] - ny), (want[0], want[1] + 3))
        xs.cmd("key ctrl+a"); xs.cmd("key BackSpace"); xs.cmd("type moved")
        s = wait_for(dev, lambda b: b["text"] == "moved")
        case.check("typing still lands in the field", s["text"], "moved")
        if xs.xwayland:
            xs.swaymsg('"[title=\\"^Cordial 0\\"] resize set 1000 640"')
        else:
            xs.sh(f"xdotool windowsize {eng} {ew} {eh}; xdotool windowmove {eng} 0 0")
        time.sleep(2.0)

    def x8():
        print("\n-- X8. fullscreen: the editor is above the game, and typing is visible there")
        dev.send("fullscreen")
        time.sleep(4.0)
        x11_visibility(case, "fullscreen single-line", dev, xs, multiline=False, editor_expected=editor_on)
        case.check("the editor is the topmost window", "Cordial text editor" in xs.topmost(), True)
        case.note("topmost top-level window", xs.topmost())
        dev.send("windowed")
        time.sleep(3.0)
        # F11 pressed while the editor holds the keyboard is the backend's own
        # fullscreen toggle, which the editor would otherwise swallow.
        eng, _, _, _ = x11_box(dev, xs, False)
        full = (args.width, args.height)
        was = tuple(xs.info(eng)[2:4])
        xs.cmd("key F11")
        time.sleep(3.5)
        now = tuple(xs.info(eng)[2:4])
        case.check("F11 typed in the editor reaches the fullscreen toggle", now, full)
        xs.cmd("key F11")
        time.sleep(3.5)
        case.check("and F11 again leaves fullscreen", tuple(xs.info(eng)[2:4]) == full, False)

    def x9():
        print("\n-- X9. another application takes the keyboard, and gives it back")
        eng, _, ed, box = x11_box(dev, xs, False)
        xs.sh("nohup setsid xev -name otherapp -geometry 300x200+900+100 >/dev/null 2>&1 & echo $! > " + OUT + "/xev.pid")
        time.sleep(1.5)
        other = xs.find(r"^otherapp$")
        if xs.xwayland:
            # A real focus change: the compositor gives the keyboard to the other
            # window, which is what Alt-Tab does. (XSetInputFocus between two X
            # windows is not.)
            xs.swaymsg('"[title=\\"^otherapp$\\"] focus"')
        else:
            xs.sh(f"xdotool windowfocus {other}")
        time.sleep(1.5)
        ei = xs.info(ed)
        case.check("the editor hides rather than float over the other application", bool(ei and ei[4]), False)
        if xs.xwayland:
            xs.swaymsg('"[title=\\"^Cordial 0\\"] focus"')
        else:
            xs.sh(f"xdotool windowfocus {eng}")
        time.sleep(1.5)
        ei = xs.info(ed)
        case.check("it comes back when the game has focus again", bool(ei and ei[4]), True)
        case.check("holding the keyboard", xs.focus_name(), "Cordial text editor")
        xs.cmd("key ctrl+a"); xs.cmd("key BackSpace"); xs.cmd("type back")
        s = wait_for(dev, lambda b: b["text"] == "back")
        case.check("and typing works again", s["text"], "back")
        xs.sh(f"kill $(cat {OUT}/xev.pid) 2>/dev/null; true")

    def x10():
        print("\n-- X10. the real sign-in fields (typed with dummy strings; nothing is submitted)")
        if not editor_on:
            case.note("X10", "skipped for the control arm: `textbox` reports where the editor is, and there is none")
            return
        dev.send("fakeblur"); time.sleep(0.8)
        ex, ey, ew, eh, _ = xs.info(xs.find(r"^Cordial [0-9]"))
        case.note("engine window at the click", f"{ew}x{eh} at {ex},{ey}")
        xs.grab("x10-before-sign-in")
        dev.send(f"click {ew // 2} {int(eh * 0.51)}")           # the landing page's Sign In
        time.sleep(3.0)
        xs.grab("x10-after-sign-in")
        for label, fy in (("username", 0.407), ("password", 0.499)):
            # The generation, not just "something is focused": the username box
            # is still focused when the password is clicked, and reading at once
            # returned *its* rectangle for the password's photograph.
            gen0 = dev.textbox()["gen"]
            dev.send(f"click {ew // 2} {int(eh * fy)}")
            box = wait_for(dev, lambda b: b["gen"] != gen0 and b["focus"] != "none" and b["x"] != "none"
                           and float(b["w"]) > 0, timeout=6.0)
            if not case.check(f"[{label}] a click focuses the real field", box["gen"] != gen0 and box["focus"] != "none", True):
                continue
            time.sleep(1.2)
            box = dev.textbox()
            case.check(f"[{label}] placed from the engine's own spec", box.get("placed"), "engine")
            case.check(f"[{label}] the editor holds the keyboard", xs.focus_name(), "Cordial text editor")
            xs.prime()
            rect = (ex + float(box["x"]) - 4, ey + float(box["y"]) - 4, float(box["w"]) + 8, float(box["h"]) + 8)
            before = xs.grab(f"real-{label}-empty")
            xs.cmd("type dummy1234")
            wait_for(dev, lambda b: b["chars"] == 9)
            time.sleep(0.8)
            after = xs.grab(f"real-{label}-typed")
            case.check(f"[{label}] the field holds the characters (readback)", dev.textbox()["chars"], 9)
            delta = crop_diff(before, after, 1.0, rect)
            case.note(f"[{label}] changed pixels in the field", delta)
            case.check(f"[{label}] the characters are visible on the display", delta >= 60, True)
            xs.cmd("key ctrl+a"); xs.cmd("key BackSpace")
            wait_for(dev, lambda b: b["chars"] == 0)

    # X10 clicks fixed fractions of the landing page, whose layout moves after
    # any resize or fullscreen round trip (it was 30 px lower after X7 and X8,
    # and the click then missed the Sign In button). So it runs first, on the
    # page as it is when the client comes up.
    every = [("X1", x1), ("X10", x10), ("X2", x2), ("X3", x3), ("X4", x4), ("X5", x5), ("X6", x6),
             ("X7", x7), ("X8", x8), ("X9", x9)]
    chosen = {c.strip().upper() for c in args.cases.split(",")} if args.cases else None
    for name, fn in every:
        if chosen is None or name in chosen:
            fn()


def main_x11(args):
    os.makedirs(OUT, exist_ok=True)
    log_path = os.path.join(OUT, "client.log")
    binary = os.environ.get("CORDIAL_E2E_BINARY", os.path.join(ROOT, "target/release/cordial-run"))
    apk = os.environ.get(
        "CORDIAL_APK",
        os.path.expanduser("~/.var/app/org.vinegarhq.Sober/data/sober/packages/"
                           "x86_64/com.roblox.client/base.apk"))
    lib = os.environ.get("CORDIAL_LIB_DIR", os.path.expanduser("~/.cache/cordial/lib/x86_64"))
    for path, what in ((binary, "build"), (apk, "APK"), (lib, "library directory")):
        if not os.path.exists(path):
            sys.exit(f"FAIL: no {what} at {path}")
    held = profile_holder(args.profile)
    if held:
        sys.exit(f"FAIL: profile {args.profile!r} is already open in pid {held}. Pick another with --profile.")
    xdg_root = os.environ.get("CORDIAL_E2E_XDG_ROOT")
    if not xdg_root:
        sys.exit("FAIL: set CORDIAL_E2E_XDG_ROOT to a directory on disk: the client must not "
                 "touch the real data, cache or config, and the shell's first-launch migration "
                 "moves a real build store into whatever XDG_CACHE_HOME it is given")
    case, xs, client = Case(), None, None
    if any(kv.startswith("CORDIAL_X11_EDITOR=off") for kv in args.client_env):
        os.environ["CORDIAL_X11_EDITOR_OFF_EXPECTED"] = "1"
    for kv in args.client_env:
        if kv.startswith("GDK_SCALE=") and kv.partition("=")[2].isdigit():
            os.environ["CORDIAL_E2E_POS_TOL"] = str(max(0, int(kv.partition("=")[2]) - 1))
    try:
        xs = XSession(args.wm, args.width, args.height, xwayland=args.xwayland)
        xs.bind_accents()
        env = dict(os.environ)
        for k in ("WAYLAND_DISPLAY", "XDG_SESSION_TYPE", "GDK_BACKEND"):
            env.pop(k, None)
        env.update(DISPLAY=xs.display, CORDIAL_X11="1", CORDIAL_DEV_CONTROL="1", CORDIAL_TRACE_TEXT="1",
                   CORDIAL_TRACE_TEXT_SHOW_PASSWORDS="1",
                   XDG_DATA_HOME=f"{xdg_root}/data", XDG_CACHE_HOME=f"{xdg_root}/cache",
                   XDG_CONFIG_HOME=f"{xdg_root}/config")
        if not args.vulkan:
            env["CORDIAL_NO_VULKAN"] = "1"
        for kv in args.client_env:
            k, _, v = kv.partition("=")
            env[k] = v
        print(f"== engine graphics: {'Vulkan' if args.vulkan else 'GLES (CORDIAL_NO_VULKAN=1)'}", flush=True)
        log = open(log_path, "w")
        client = subprocess.Popen(
            [binary, "--lib-dir", lib, "--apk", apk, "--host-libc",
             "--game-activity", "--run", "0", "--profile", args.profile],
            env=env, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        print(f"== client pid {client.pid}, log {log_path}", flush=True)
        deadline, ready = time.time() + 150, None
        while time.time() < deadline:
            if client.poll() is not None:
                sys.exit(f"FAIL: the client exited with {client.returncode} before it was ready; see {log_path}")
            m = re.search(r"app ready: (Home|Landing)", open(log_path, errors="replace").read())
            if m:
                ready = m.group(1)
                break
            time.sleep(1)
        if not ready:
            sys.exit(f"FAIL: the app shell never became ready; see {log_path}")
        print(f"== app ready: {ready}", flush=True)
        time.sleep(8)
        dev = Devctl(os.path.join(xdg_root, "data", "cordial", "profiles", args.profile, "devctl.sock"))
        print(f"== {dev.send('info')}", flush=True)
        run_x11(case, dev, xs, log_path, args)
    finally:
        if not args.keep:
            if client and client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=20)
                except Exception:
                    client.kill()
            if xs:
                xs.close()
    print()
    if case.failed:
        print(f"BROKEN -- {len(case.failed)} of {len(case.results)} assertions failed:")
        for _, name, got, want in case.failed:
            print(f"  {name}: got {got!r}, want {want!r}")
        sys.exit(1)
    print(f"PERFECT -- {len(case.results)} assertions, none failed.")


if __name__ == "__main__":
    main()
