#!/usr/bin/env python3
"""A Wayland protocol proxy that gives a client a touchscreen it does not have.

Nothing on a development machine has a touchscreen and wlroots has no protocol
for a virtual touch device, so touch handling in Cordial -- and in GTK, which
listens to the same seat -- had never run before issue #36. This sits between
a client and a real compositor (a nested headless sway), forwards every byte
and file descriptor, and does two things to the stream:

  * it adds the touch bit to every `wl_seat.capabilities` event and answers
    `wl_seat.get_touch` itself, so the client binds a `wl_touch` that the real
    compositor never hears about;
  * on a command it writes `wl_touch.down/motion/up/frame/cancel` events to
    that client, aimed at a surface of the client's choosing.

It is a proxy rather than a compositor on purpose. A compositor would have to
accept the engine's Vulkan swapchain (dmabuf, explicit sync) before the
engine's canvas existed at all, and the thing under test is what GTK does with
a touch on a surface it did not create. Everything else -- pointer, keyboard,
buffers, the subsurface -- is the real sway's.

Never run it at the developer's compositor: point `--upstream` at a nested
headless sway.

Usage:
    wl-touch-proxy.py serve --upstream wayland-1 --listen cordial-touch \
                            --control /tmp/touch.sock
    wl-touch-proxy.py send /tmp/touch.sock tap 200 200 canvas
    WAYLAND_DISPLAY=cordial-touch cordial-run ...

Commands (one line each, answered with one line):
    list                        what the proxy has learned about the client
    down X Y [TARGET] [ID]      a finger goes down, then frame
    motion X Y [ID]             it moves, then frame
    up [ID]                     it lifts, then frame
    cancel                      the compositor takes the sequence over
    tap X Y [TARGET] [MS]       down, hold for MS (default 80), up
TARGET is `canvas` (the first subsurface the client created, which for Cordial
is the engine's), `toplevel` (that subsurface's parent), or a numeric object id.
X and Y are surface-local pixels.
"""

import argparse
import array
import os
import socket
import struct
import sys
import threading
import time

WL_DISPLAY = 1
SEAT_CAPS_TOUCH = 4


def log(msg):
    print(f"[touch-proxy] {msg}", file=sys.stderr, flush=True)


def pack_str(s):
    b = s.encode() + b"\0"
    return struct.pack("=I", len(b)) + b + b"\0" * (-len(b) % 4)


class Pipe:
    """One direction of a connection: bytes and fds in, whole messages out."""

    def __init__(self, src, dst, name):
        self.src, self.dst, self.name = src, dst, name
        self.buf = b""
        self.fds = []

    def read(self):
        data, anc, _flags, _ = self.src.recvmsg(65536, socket.CMSG_SPACE(64 * 4))
        for level, typ, payload in anc:
            if level == socket.SOL_SOCKET and typ == socket.SCM_RIGHTS:
                a = array.array("i")
                a.frombytes(payload[: len(payload) - len(payload) % a.itemsize])
                self.fds.extend(a)
        if not data:
            return None
        self.buf += data
        return data

    def messages(self):
        """Yield (obj, opcode, payload) for every complete message buffered."""
        while len(self.buf) >= 8:
            obj, w2 = struct.unpack_from("=II", self.buf)
            size, op = w2 >> 16, w2 & 0xFFFF
            if size < 8 or len(self.buf) < size:
                break
            payload = self.buf[8:size]
            self.buf = self.buf[size:]
            yield obj, op, payload


def send_raw(sock, data, fds=()):
    """Send bytes, attaching fds to the first byte, and then the rest plainly."""
    if not data:
        return
    if fds:
        sent = sock.sendmsg(
            [data],
            [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds))],
        )
        data = data[sent:]
    if data:
        sock.sendall(data)


class Connection:
    def __init__(self, proxy, client, upstream, number):
        self.proxy, self.client, self.upstream, self.number = proxy, client, upstream, number
        self.client_lock = threading.Lock()   # writes toward the client
        self.registry = set()
        self.iface = {}                       # object id -> interface name
        self.surfaces = []                    # wl_surface ids, in creation order
        self.subsurfaces = []                 # (surface, parent), in creation order
        self.seats = set()
        self.touches = set()                  # wl_touch ids this proxy answers for
        self.shadows = set()                  # the same ids as the server knows them (wl_callback)
        self.pointers = []                    # wl_pointer ids, in creation order
        self.cursor_surfaces = set()          # surfaces a set_cursor has named
        self.last_attach = {}                 # wl_surface -> buffer id of its latest attach (0 = none)
        self.serial = 0x40000000
        self.rewritten_caps = 0
        self.t0 = time.monotonic()

    # ---- client -> server
    def c2s(self):
        p = Pipe(self.client, self.upstream, "c2s")
        while True:
            try:
                if p.read() is None:
                    break
            except OSError:
                break
            out = b""
            for obj, op, payload in p.messages():
                size = 8 + len(payload)
                raw = struct.pack("=II", obj, (size << 16) | op) + payload
                if obj == WL_DISPLAY and op == 1:                    # get_registry
                    self.registry.add(struct.unpack("=I", payload)[0])
                elif obj in self.registry and op == 0:               # bind
                    (slen,) = struct.unpack_from("=I", payload, 4)
                    name = payload[8 : 8 + slen - 1].decode()
                    off = 8 + slen + (-slen % 4)
                    _ver, new_id = struct.unpack_from("=II", payload, off)
                    self.iface[new_id] = name
                    if name == "wl_seat":
                        self.seats.add(new_id)
                elif self.iface.get(obj) == "wl_compositor" and op == 0:
                    self.surfaces.append(struct.unpack("=I", payload)[0])
                elif self.iface.get(obj) == "wl_subcompositor" and op == 1:
                    _new, surf, parent = struct.unpack("=III", payload)
                    self.subsurfaces.append((surf, parent))
                elif obj in self.seats and op == 0:                  # get_pointer
                    self.pointers.append(struct.unpack("=I", payload)[0])
                elif obj in self.pointers and op == 0 and self.proxy.trace_cursor:
                    serial, surf, hx, hy = struct.unpack("=IIii", payload)
                    self.cursor_surfaces.add(surf)
                    shown = ("none (hidden)" if not surf else
                             f"{surf}, whose latest attach was "
                             f"{'a NULL buffer (draws nothing)' if self.last_attach.get(surf) == 0 else 'buffer ' + str(self.last_attach.get(surf))}")
                    log(f"conn {self.number}: wl_pointer#{self.pointers.index(obj)}@{obj}.set_cursor "
                        f"serial={serial} surface={shown} hotspot={hx},{hy}")
                elif obj in self.surfaces and op == 1 and self.proxy.trace_cursor:
                    self.last_attach[obj] = struct.unpack_from("=I", payload)[0]
                    if obj in self.cursor_surfaces:
                        log(f"conn {self.number}: cursor wl_surface@{obj}.attach buffer="
                            f"{self.last_attach[obj] or 'NULL (draws nothing)'}")
                elif obj in self.seats and op == 2:                  # get_touch
                    (new_id,) = struct.unpack("=I", payload)
                    self.touches.add(new_id)
                    # The server has to see *something* create this id: it
                    # refuses a client map with a hole in it (the next bind
                    # fails with "invalid arguments"). A wl_display.sync makes
                    # a harmless wl_callback that answers and is deleted at
                    # once; its done and delete_id are swallowed on the way
                    # back so the client keeps believing the id is a wl_touch.
                    self.shadows.add(new_id)
                    raw = struct.pack("=III", WL_DISPLAY, (12 << 16) | 0, new_id)
                    log(f"conn {self.number}: wl_seat.get_touch -> wl_touch@{new_id} (answered here)")
                elif obj in self.touches and op == 0:                # wl_touch.release
                    self.touches.discard(obj)
                    self.to_client(struct.pack("=IIII", WL_DISPLAY, (12 << 16) | 1, obj, 0)[:12])
                    continue
                out += raw
            try:
                if out:
                    send_raw(self.upstream, out, p.fds)
                    p.fds = []
            except OSError:
                break
        self.close()

    # ---- server -> client
    def s2c(self):
        p = Pipe(self.upstream, self.client, "s2c")
        while True:
            try:
                if p.read() is None:
                    break
            except OSError:
                break
            out = b""
            for obj, op, payload in p.messages():
                if obj in self.shadows and op == 0:                        # callback.done
                    continue
                if obj == WL_DISPLAY and op == 1 and struct.unpack("=I", payload)[0] in self.shadows:
                    self.shadows.discard(struct.unpack("=I", payload)[0])   # delete_id
                    continue
                if obj in self.seats and op == 0 and len(payload) == 4:   # capabilities
                    (caps,) = struct.unpack("=I", payload)
                    if not caps & SEAT_CAPS_TOUCH:
                        self.rewritten_caps += 1
                        payload = struct.pack("=I", caps | SEAT_CAPS_TOUCH)
                        log(f"conn {self.number}: wl_seat@{obj}.capabilities {caps} -> {caps | SEAT_CAPS_TOUCH}")
                out += struct.pack("=II", obj, ((8 + len(payload)) << 16) | op) + payload
            try:
                if out:
                    with self.client_lock:
                        send_raw(self.client, out, p.fds)
                    p.fds = []
            except OSError:
                break
        self.close()

    def to_client(self, data):
        with self.client_lock:
            send_raw(self.client, data)

    def close(self):
        for s in (self.client, self.upstream):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        self.proxy.drop(self)

    # ---- injection
    def now_ms(self):
        return int((time.monotonic() - self.t0) * 1000) & 0xFFFFFFFF

    def resolve(self, target):
        if target in (None, "canvas"):
            return self.subsurfaces[0][0] if self.subsurfaces else None
        if target == "toplevel":
            return self.subsurfaces[0][1] if self.subsurfaces else None
        return int(target)

    def emit(self, op, payload):
        data = b""
        for t in sorted(self.touches):
            data += struct.pack("=II", t, ((8 + len(payload)) << 16) | op) + payload
        if data:
            self.to_client(data)
        return len(self.touches)

    def down(self, x, y, target, tid):
        surface = self.resolve(target)
        if surface is None:
            return 0
        self.serial += 1
        n = self.emit(0, struct.pack("=IIIiii", self.serial, self.now_ms(), surface, tid,
                                     int(x * 256), int(y * 256)))
        self.emit(3, b"")
        return n

    def motion(self, x, y, tid):
        n = self.emit(2, struct.pack("=Iiii", self.now_ms(), tid, int(x * 256), int(y * 256)))
        self.emit(3, b"")
        return n

    def up(self, tid):
        self.serial += 1
        n = self.emit(1, struct.pack("=IIi", self.serial, self.now_ms(), tid))
        self.emit(3, b"")
        return n

    def cancel(self):
        n = self.emit(4, b"")
        self.emit(3, b"")
        return n


class Proxy:
    def __init__(self, upstream_path, listen_path, trace_cursor=False):
        self.trace_cursor = trace_cursor
        self.upstream_path = upstream_path
        self.conns = []
        self.lock = threading.Lock()
        self.count = 0
        if os.path.exists(listen_path):
            os.unlink(listen_path)
        self.srv = socket.socket(socket.AF_UNIX)
        self.srv.bind(listen_path)
        self.srv.listen(8)
        log(f"listening on {listen_path}, forwarding to {upstream_path}")

    def serve(self):
        while True:
            client, _ = self.srv.accept()
            up = socket.socket(socket.AF_UNIX)
            up.connect(self.upstream_path)
            with self.lock:
                self.count += 1
                conn = Connection(self, client, up, self.count)
                self.conns.append(conn)
            log(f"connection {conn.number} opened")
            threading.Thread(target=conn.c2s, daemon=True).start()
            threading.Thread(target=conn.s2c, daemon=True).start()

    def drop(self, conn):
        with self.lock:
            if conn in self.conns:
                self.conns.remove(conn)
                log(f"connection {conn.number} closed")

    def command(self, line):
        parts = line.split()
        if not parts:
            return "err empty"
        cmd, a = parts[0], parts[1:]
        with self.lock:
            conns = [c for c in self.conns if c.touches]
            everyone = list(self.conns)
        try:
            if cmd == "list":
                return " | ".join(
                    f"conn{c.number} touch_objects={sorted(c.touches)} seats={sorted(c.seats)} "
                    f"subsurfaces={c.subsurfaces} caps_rewritten={c.rewritten_caps}"
                    for c in everyone) or "no connections"
            if not conns:
                return "err no client has bound a wl_touch yet"
            if cmd == "down":
                x, y = float(a[0]), float(a[1])
                tgt = a[2] if len(a) > 2 else "canvas"
                tid = int(a[3]) if len(a) > 3 else 0
                return f"ok delivered_to={sum(c.down(x, y, tgt, tid) for c in conns)}"
            if cmd == "motion":
                tid = int(a[2]) if len(a) > 2 else 0
                return f"ok delivered_to={sum(c.motion(float(a[0]), float(a[1]), tid) for c in conns)}"
            if cmd == "up":
                tid = int(a[0]) if a else 0
                return f"ok delivered_to={sum(c.up(tid) for c in conns)}"
            if cmd == "cancel":
                return f"ok delivered_to={sum(c.cancel() for c in conns)}"
            if cmd == "tap":
                x, y = float(a[0]), float(a[1])
                tgt = a[2] if len(a) > 2 else "canvas"
                ms = int(a[3]) if len(a) > 3 else 80
                n = sum(c.down(x, y, tgt, 0) for c in conns)
                time.sleep(ms / 1000)
                for c in conns:
                    c.up(0)
                return f"ok delivered_to={n}"
        except (IndexError, ValueError) as e:
            return f"err {e!r}"
        return f"err unknown command {cmd!r}"


def control_server(proxy, path):
    if os.path.exists(path):
        os.unlink(path)
    srv = socket.socket(socket.AF_UNIX)
    srv.bind(path)
    srv.listen(4)

    def handle(c):
        with c:
            line = c.makefile().readline()
            reply = proxy.command(line)
            log(f"control: {line.strip()!r} -> {reply}")
            c.sendall((reply + "\n").encode())

    while True:
        c, _ = srv.accept()
        threading.Thread(target=handle, args=(c,), daemon=True).start()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="mode", required=True)
    s = sub.add_parser("serve")
    s.add_argument("--upstream", default=os.environ.get("WAYLAND_DISPLAY"),
                   help="the real compositor's socket name or path (default $WAYLAND_DISPLAY)")
    s.add_argument("--listen", default="cordial-touch", help="socket name to create in $XDG_RUNTIME_DIR")
    s.add_argument("--control", default="/tmp/cordial-touch-proxy.sock")
    s.add_argument("--trace-cursor", action="store_true",
                   help="log every wl_pointer.set_cursor the client sends (pointer #0 is GTK's, #1 Cordial's)")
    c = sub.add_parser("send")
    c.add_argument("sock")
    c.add_argument("words", nargs="+")
    args = ap.parse_args()

    if args.mode == "send":
        k = socket.socket(socket.AF_UNIX)
        k.connect(args.sock)
        k.sendall((" ".join(args.words) + "\n").encode())
        print(k.makefile().readline().strip())
        return

    rt = os.environ["XDG_RUNTIME_DIR"]
    if not args.upstream:
        sys.exit("no --upstream and no $WAYLAND_DISPLAY")
    up = args.upstream if os.path.isabs(args.upstream) else os.path.join(rt, args.upstream)
    proxy = Proxy(up, os.path.join(rt, args.listen), args.trace_cursor)
    threading.Thread(target=control_server, args=(proxy, args.control), daemon=True).start()
    try:
        proxy.serve()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
