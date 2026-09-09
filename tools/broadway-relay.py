"""Bridge a filesystem unix socket onto broadwayd's abstract one.

**This does not yet give you a working headless display, and the last step is
not ours.** It fixes the connection -- with it, GTK opens a display and Cordial
gets as far as enumerating a monitor -- and then broadwayd aborts. See
`docs/analysis/headless-gtk.md` for the whole measurement, including the
twelve-line GTK control that fails identically.

GTK 4.22's broadwayd binds an *abstract* socket -- /proc/net/unix shows
`@/run/user/1001/broadwayN.socket` and nothing appears on disk -- while GDK's
broadway client connects to the same name as a *path*, which strace shows as
`sun_path="/run/user/1001/broadwayN.socket"` with no leading NUL. They can
never meet, which is why a correctly configured broadwayd still gives
"Unable to init Broadway server: Could not connect: No such file or directory".

The relay must forward SCM_RIGHTS. Broadway passes shared-memory file
descriptors over this socket, and a byte-only relay drops them: the server
then reads the next message misaligned, prints
"_gdk_broadway_events_got_input - Unknown input command d" and aborts. That
was measured, with a core dump, before this function existed.
"""
import array
import os
import socket
import sys
import threading

MAX_FDS = 16


def pump(src, dst, tag="?"):
    total = 0
    nfds = 0
    while True:
        try:
            data, ancdata, _flags, _addr = src.recvmsg(
                65536, socket.CMSG_SPACE(MAX_FDS * 4)
            )
        except OSError:
            break
        fds = []
        for level, kind, blob in ancdata:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                blob = blob[: len(blob) - (len(blob) % 4)]
                fds.extend(array.array("i", blob))
        total += len(data)
        nfds += len(fds)
        if not data and not fds:
            break
        try:
            if fds:
                # sendmsg may send fewer bytes than asked for, and a truncated
                # stream is what desynchronises broadway: the far side reads the
                # next message at the wrong offset and prints
                # "Unknown input command". The ancillary data rides on the first
                # byte, so only the remainder needs the ordinary loop.
                sent = dst.sendmsg(
                    [data],
                    [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds))],
                )
                if sent < len(data):
                    dst.sendall(data[sent:])
            else:
                dst.sendall(data)
        except OSError:
            break
        finally:
            for fd in fds:
                os.close(fd)
    print(f"relay {tag}: {total} bytes, {nfds} fds", flush=True)
    for s in (src, dst):
        try:
            s.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


def serve(client, target):
    upstream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    upstream.connect("\0" + target)
    threading.Thread(target=pump, args=(client, upstream, "client->server"), daemon=True).start()
    pump(upstream, client, "server->client")


def main():
    path = sys.argv[1]
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path)
    srv.listen(16)
    print(f"relay: {path} -> @{path}", flush=True)
    try:
        while True:
            conn, _ = srv.accept()
            threading.Thread(target=serve, args=(conn, path), daemon=True).start()
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass


main()
