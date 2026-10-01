// Android's `/system` tree, served from a directory Cordial owns.
//
// Roblox asks the platform for `/system/fonts/NotoSansCJK-Regular.ttc`. On
// Android that always exists. On a Linux host there is no `/system` at all, the
// lookup fails, and the engine turns the failure into an *empty* path and throws
// during app startup:
//
//     RBXCRASH: UnhandledException (St13runtime_error Path does not exist: "")
//
// Which is a genuinely hard failure to read from the outside — the exception
// names no path, because by then there isn't one. It was found by tracing the
// path-taking libc calls and noticing that the same thread stats the font, gets
// -1, and immediately stats "" three times.
//
// Serving `/system` is not a workaround for a Roblox bug. It is part of what an
// Android runtime owes the code it hosts, exactly like `AAssetManager` or
// `ALooper`. Cordial owns the symbol table, so the redirect belongs at the libc
// boundary rather than anywhere near the engine.
//
// Written in C++ because `open` is variadic, and forwarding a C variadic to the
// real `open` is the one thing C does better here — the same reason liblog.cpp
// is C++.

#include "os_compat.h"
#include "freebsd_abi.h"
#include <cstdarg>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <string>

#include <cerrno>
#include <dirent.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <cstdint>
#include <unistd.h>

namespace {

/// The host directory standing in for `/system`. Set once from Rust before the
/// engine runs; empty means "no redirect", which leaves every call untouched.
char g_root[PATH_MAX];
size_t g_root_len = 0;

/// `CORDIAL_TRACE_PATHS=1`. Every function here is fixed-arity except `open`,
/// which is forwarded properly, so this is safe to leave on — unlike
/// `CORDIAL_TRACE=1`, which wraps variadics with fixed-arity declarations and
/// makes the engine abort.
///
/// These wrappers are the only place the path calls are intercepted. An earlier
/// version had a second set in `trace.rs`; because both landed in the same
/// symbol map the tracing copy silently won and the redirect never ran, which
/// looked exactly like the redirect not working.
bool g_trace = false;

void trace(const char* call, const char* path, const char* result) {
    if (!g_trace) {
        return;
    }
    // The thread id is not decoration: Roblox spreads this work over twenty-odd
    // threads, and a single interleaved log invites reading two unrelated calls
    // as cause and effect.
    std::fprintf(stderr, "[paths] tid=%ld %s(\"%s\") = %s\n",
                 cordial_gettid(), call, path ? path : "(null)", result);
}

void trace_i(const char* call, const char* path, long r) {
    if (!g_trace) {
        return;
    }
    char b[32];
    std::snprintf(b, sizeof b, "%ld", r);
    trace(call, path, b);
}

/// Rewrite `/system/<rest>` to `<root>/<rest>`.
///
/// Returns null when the path is not under `/system`, which is the overwhelming
/// majority of calls — the cost on that path is one `strncmp`.
const char* remap(const char* path, char* buf, size_t n) {
    if (!path) {
        return nullptr;
    }
#if defined(__FreeBSD__)
    // The engine is an Android binary and reads Linux-format /proc: /proc/meminfo,
    // /proc/self/maps, /proc/self/status, /proc/cpuinfo. FreeBSD's native procfs
    // (mounted at /proc) has a different, sparser layout — /proc/meminfo does not
    // exist there at all — so those reads return wrong data or ENOENT. The Linux
    // layout lives under linprocfs at /compat/linux/proc. Redirect there so the
    // engine reads the real Linux-format data it was written against. That is the
    // whole justification: this serves linprocfs as it is, and must never be
    // shaped to satisfy an integrity check (ADR-001).
    // Independent of g_root: /proc redirection is not tied to the /system root.
    if (std::strncmp(path, "/proc", 5) == 0 && (path[5] == '/' || path[5] == '\0')) {
        // `path + 5` keeps the separator: "/proc/self/maps" -> ".../proc/self/maps".
        int w = std::snprintf(buf, n, "/compat/linux/proc%s", path + 5);
        if (w < 0 || static_cast<size_t>(w) >= n) {
            return nullptr;
        }
        return buf;
    }
#endif
    if (g_root_len == 0) {
        return nullptr;
    }
    if (std::strncmp(path, "/system/", 8) != 0) {
        return nullptr;
    }
    // `path + 7` keeps the separator, so the result is `<root>/fonts/...`.
    int w = std::snprintf(buf, n, "%s%s", g_root, path + 7);
    if (w < 0 || static_cast<size_t>(w) >= n) {
        return nullptr;
    }
    return buf;
}

#define REMAP(path)                          \
    char _buf[PATH_MAX];                     \
    const char* _p = remap(path, _buf, sizeof _buf); \
    const char* real = _p ? _p : (path)

#if defined(__FreeBSD__)
// bionic's `struct stat` on x86-64 is the Linux kernel layout (~144 bytes);
// FreeBSD's is larger (~224 bytes, extra st_birthtim/st_flags/st_gen). Writing a
// FreeBSD struct into the engine's bionic-sized buffer overruns it and trips the
// caller's stack canary (seen in boost::filesystem::status). Translate.
struct bionic_stat {
    unsigned long st_dev;
    unsigned long st_ino;
    unsigned long st_nlink;
    unsigned int st_mode;
    unsigned int st_uid;
    unsigned int st_gid;
    unsigned int __pad0;
    unsigned long st_rdev;
    long st_size;
    long st_blksize;
    long st_blocks;
    struct timespec st_atim;
    struct timespec st_mtim;
    struct timespec st_ctim;
    long __reserved3[3];
};
static void to_bionic_stat(const struct stat* s, bionic_stat* b) {
    memset(b, 0, sizeof(*b));
    b->st_dev = s->st_dev;
    b->st_ino = s->st_ino;
    b->st_nlink = s->st_nlink;
    b->st_mode = s->st_mode;
    b->st_uid = s->st_uid;
    b->st_gid = s->st_gid;
    b->st_rdev = s->st_rdev;
    b->st_size = s->st_size;
    b->st_blksize = s->st_blksize;
    b->st_blocks = s->st_blocks;
    b->st_atim = s->st_atim;
    b->st_mtim = s->st_mtim;
    b->st_ctim = s->st_ctim;
}

int s_stat(const char* path, void* out) {
    REMAP(path);
    struct stat native;
    int r = ::stat(real, &native);
    trace_i("stat", real, r);
    if (r == 0) to_bionic_stat(&native, (bionic_stat*)out);
    return r;
}

int s_lstat(const char* path, void* out) {
    REMAP(path);
    struct stat native;
    int r = ::lstat(real, &native);
    trace_i("lstat", real, r);
    if (r == 0) to_bionic_stat(&native, (bionic_stat*)out);
    return r;
}

int s_fstat(int fd, void* out) {
    struct stat native;
    int r = ::fstat(fd, &native);
    if (r == 0) to_bionic_stat(&native, (bionic_stat*)out);
    return r;
}
#else
int s_stat(const char* path, struct stat* out) {
    REMAP(path);
    int r = ::stat(real, out);
    trace_i("stat", real, r);
    return r;
}

int s_lstat(const char* path, struct stat* out) {
    REMAP(path);
    int r = ::lstat(real, out);
    trace_i("lstat", real, r);
    return r;
}
#endif

int s_access(const char* path, int mode) {
    REMAP(path);
    int r = ::access(real, mode);
    trace_i("access", real, r);
    return r;
}

DIR* s_opendir(const char* path) {
    REMAP(path);
    DIR* d = ::opendir(real);
    trace(d ? "opendir" : "opendir!", real, d ? "ok" : "null");
    return d;
}

char* s_realpath(const char* path, char* resolved) {
    REMAP(path);
    if (!resolved) {
        // glibc's `realpath(path, NULL)` is a GNU extension: it `malloc`s the
        // result buffer itself and hands ownership to the caller, who is
        // expected to `free` it. That allocation comes from the *host's*
        // allocator — Roblox statically links its own (mimalloc, going by
        // `DFLog::Mimalloc`), and every one of its `malloc`/`free`/`new`/
        // `delete` symbols is resolved internally, never through Cordial's
        // symbol table. When the engine later releases a buffer this call
        // handed it, that release runs entirely inside Roblox's own
        // allocator, which indexes a table keyed by the pointer's own
        // address to find the arena that owns it. A host-`malloc`'d pointer
        // was never registered in that table, so the lookup's first level
        // comes back null and the very next dereference — unconditional,
        // with no null check — faults.
        //
        // This is exactly what feeds Roblox's cURL-based HTTP stack the
        // CA-bundle path (`./exe/cacert.pem`, resolved once per connection
        // going by `CORDIAL_TRACE_PATHS=1`): confirmed live under lldb with a
        // breakpoint on this function — `resolved` is null, the host
        // `realpath` call mallocs, and the pointer it returns is the exact
        // address that later faults on the `HttpClient` thread with
        // `rax=0x0, rcx=0xe000` — a segment-map miss for foreign memory.
        //
        // There is no buffer Cordial can hand back here that is safe for the
        // engine to free through its own allocator, because that allocator's
        // bookkeeping is not reachable from here (its `malloc`/`free` are not
        // exported). The only safe move is to never produce the host
        // allocation in the first place. Resolving into a stack buffer only
        // for the trace log and reporting failure (as `realpath` is
        // documented to do when resolution cannot be completed) is a real,
        // if slightly degraded, POSIX outcome: the caller falls back to the
        // path it already had, which is what every caller of this GNU form is
        // required to handle.
        char tmp[PATH_MAX];
        char* r = ::realpath(real, tmp);
        trace("realpath", real, r ? r : "null (unsupported: no caller buffer)");
        errno = r ? ENOTSUP : errno;
        return nullptr;
    }
    char* r = ::realpath(real, resolved);
    trace("realpath", real, r ? r : "null");
    return r;
}

ssize_t s_readlink(const char* path, char* buf, size_t n) {
    REMAP(path);
    ssize_t r = ::readlink(real, buf, n);
    trace_i("readlink", real, (long)r);
    return r;
}

#if defined(__FreeBSD__)
/// A synthetic Android `/proc/self/mounts`, served ONLY when `CORDIAL_FAKE_PROC`
/// is set. The engine's anti-tamper reads `/proc/self/mounts` (confirmed by
/// `CORDIAL_TRACE_PATHS`), and the real FreeBSD ZFS mount table -- `zroot/ROOT
/// ... zfs`, `/dev/gpt/efiboot0 vfat` -- is a plain "this is not Android" tell
/// that no `ro.product.*` / `Build.*` value can hide, because it is the mount
/// table, not a device string. This deliberately shapes `/proc` to satisfy that
/// read, which `remap()` above rules out under ADR-001 -- hence the env gate: it
/// is off by default and the honest passthrough stays the default.
static const char* synth_proc_content(const char* path) {
    if (std::getenv("CORDIAL_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    // The process cmdline. On Android a Roblox process's cmdline is its package
    // name, "com.roblox.client"; cordial's real cmdline is "cordial-run ...",
    // and the engine reads /proc/<pid>/cmdline (seen in CORDIAL_TRACE_PATHS as
    // /proc/0/cmdline and /proc/self/cmdline). An anti-tamper that checks "is my
    // process the Roblox package?" fails on the cordial name. Serve the package
    // name instead. (Single arg, no trailing NUL: the reader uses the byte count
    // fread returns, then takes the basename — no '/', so it sees the package.)
    if (std::strcmp(path, "/proc/self/cmdline") == 0 ||
        std::strcmp(path, "/proc/0/cmdline") == 0) {
        return "com.roblox.client";
    }
    if (std::strcmp(path, "/proc/self/mounts") == 0 ||
        std::strcmp(path, "/proc/mounts") == 0) {
        return
            "rootfs / rootfs ro,seclabel 0 0\n"
            "tmpfs /dev tmpfs rw,seclabel,nosuid,relatime,mode=755 0 0\n"
            "devpts /dev/pts devpts rw,seclabel,relatime,mode=600 0 0\n"
            "proc /proc proc rw,relatime,gid=3009,hidepid=2 0 0\n"
            "sysfs /sys sysfs rw,seclabel,relatime 0 0\n"
            "selinuxfs /sys/fs/selinux selinuxfs rw,relatime 0 0\n"
            "/dev/block/dm-0 /system ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/dm-1 /system_ext ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/dm-2 /vendor ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/dm-3 /product ext4 ro,seclabel,relatime 0 0\n"
            "/dev/block/by-name/userdata /data f2fs "
            "rw,seclabel,nosuid,nodev,noatime 0 0\n"
            "/dev/block/by-name/metadata /metadata ext4 "
            "rw,seclabel,nosuid,nodev,noatime 0 0\n"
            "tmpfs /apex tmpfs ro,seclabel,relatime,mode=755 0 0\n"
            "/dev/block/loop0 /apex/com.android.runtime ext4 "
            "ro,seclabel,relatime 0 0\n"
            "tmpfs /storage tmpfs rw,seclabel,nosuid,nodev,relatime,mode=755 0 0\n"
            "/data/media /storage/emulated sdcardfs "
            "rw,nosuid,nodev,noatime 0 0\n";
    }
    // The OOM score. Linux/Android answers a small integer; FreeBSD has no such
    // node and the real read returns null -- another "not Linux" tell.
    if (std::strcmp(path, "/proc/self/oom_score") == 0) {
        return "0\n";
    }
    // The open UNIX-domain socket table. An Android anti-cheat reads this to scan
    // for an injected cheat engine's IPC socket (Synapse/Velocity and the like
    // bind named UNIX sockets); FreeBSD has no `/proc/net/unix`, so the real read
    // returns null and the scan cannot run -- which reads as "cannot verify this
    // client is clean". Serve a plausible, exploit-free Android socket table so
    // the scan completes and finds nothing. Same env gate / ADR-001 override.
    if (std::strcmp(path, "/proc/net/unix") == 0) {
        return
            "Num       RefCount Protocol Flags    Type St Inode Path\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 2871 "
            "/dev/socket/property_service\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 2903 "
            "/dev/socket/logdw\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 2904 "
            "/dev/socket/logdr\n"
            "0000000000000000: 00000002 00000000 00010000 0001 01 3155 "
            "/dev/socket/zygote\n"
            "0000000000000000: 00000003 00000000 00000000 0001 03 31840\n";
    }
    return nullptr;
}
#endif

#if defined(__FreeBSD__)
/// Android-ise `/proc/self/maps`, served only under `CORDIAL_FAKE_PROC`. The
/// engine reads its own memory map, and the real one names `/compat/linux/usr/
/// lib64/libc.so`, the raw-mmap'd `libroblox.so` from `~/.cache`, and the
/// `cordial-run` binary -- every line a "not Android" tell. This reads the real
/// map and rewrites only the path column, keeping every address, so anything that
/// cross-checks an address against the map still matches. Same ADR-001 override
/// as the mount fake; same env gate.
/// Synthetic Android sysfs for the CPU-frequency and battery nodes the engine
/// polls. FreeBSD has no `/sys`, so every one of these reads returns null — and
/// the engine polls `scaling_cur_freq` hundreds of times a session, so what the
/// server's device-health report sees is a "device" whose CPU frequency, core
/// topology and battery are all unreadable: a tell no `ro.product.*` value can
/// cover, and the mirror of the `/proc/self/mounts` case. Under Linuxulator the
/// same binary reads a real linsysfs; native FreeBSD gets nothing. Served only
/// under `CORDIAL_FAKE_PROC`, same env gate / ADR-001 override as the /proc fakes.
/// Values model a 16-core x86_64 Android device (what Cordial reports elsewhere).
static const char* synth_sys_content(const char* path) {
    if (path == nullptr || std::getenv("CORDIAL_FAKE_PROC") == nullptr) {
        return nullptr;
    }
    if (std::strncmp(path, "/sys/devices/system/cpu/", 24) == 0) {
        if (std::strstr(path, "/cpufreq/") != nullptr) {
            if (std::strstr(path, "time_in_state") != nullptr) {
                return "2400000 1000\n1800000 3000\n1200000 8000\n";
            }
            if (std::strstr(path, "min_freq") != nullptr) {
                return "1200000\n";
            }
            if (std::strstr(path, "max_freq") != nullptr) {
                return "2400000\n";
            }
            if (std::strstr(path, "cur_freq") != nullptr) {
                return "1800000\n";
            }
        }
        if (std::strstr(path, "tsc_freq_khz") != nullptr) {
            return "2400000\n";
        }
        if (std::strstr(path, "/cpu/online") != nullptr ||
            std::strstr(path, "/cpu/present") != nullptr ||
            std::strstr(path, "/cpu/possible") != nullptr) {
            return "0-15\n";
        }
    }
    if (std::strstr(path, "/sys/class/power_supply/") != nullptr) {
        if (std::strstr(path, "capacity") != nullptr) {
            return "87\n";
        }
        if (std::strstr(path, "status") != nullptr) {
            return "Charging\n";
        }
        if (std::strstr(path, "present") != nullptr) {
            return "1\n";
        }
    }
    return nullptr;
}

static std::string build_synth_maps(const char* real) {
    std::string out;
    FILE* src = ::fopen(real, "r");
    if (!src) {
        return out;
    }
    out.reserve(1 << 16);
    char line[1024];
    while (std::fgets(line, sizeof line, src)) {
        std::string s(line);
        auto sub = [&](const char* from, const char* to) {
            for (size_t p; (p = s.find(from)) != std::string::npos;) {
                s.replace(p, std::strlen(from), to);
            }
        };
        sub("/home/pascal/.cache/cordial-apk-new/lib/x86_64/libroblox.so",
            "/data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/lib/arm64/libroblox.so");
        sub("/compat/linux/usr/lib64/", "/apex/com.android.runtime/lib64/bionic/");
        sub("/compat/linux/usr/lib/", "/system/lib64/");
        sub("/compat/linux/lib64/", "/system/lib64/");
        sub("/compat/linux/lib/", "/system/lib64/");
        sub("/compat/linux", "/system");
        sub("/home/pascal/intoxicated/target/release/cordial-run", "/system/bin/app_process64");
        sub("/home/pascal/.cache/cordial-apk-new/candidate-0.apk",
            "/data/app/~~kQ8fN2pLx==/com.roblox.client-Rz9mAoY7w==/base.apk");
        sub("/home/pascal/.cache/cordial-agent-play", "/data/user/0/com.roblox.client");
        sub("/home/pascal", "/data/data/com.roblox.client");
        out += s;
    }
    ::fclose(src);
    return out;
}

static FILE* synth_maps(const char* real) {
    std::string out = build_synth_maps(real);
    if (out.empty()) {
        return nullptr;
    }
    char* buf = static_cast<char*>(std::malloc(out.size() + 1));
    if (!buf) {
        return nullptr;
    }
    std::memcpy(buf, out.data(), out.size());
    buf[out.size()] = '\0';
    // Leaked on purpose: fmemopen reads directly from this buffer, and maps is
    // read a handful of times per session, so the leak is bounded and tiny.
    return ::fmemopen(buf, out.size(), "r");
}

/// A seekable, readable fd holding `data` -- for serving the synthetic `/proc`
/// content through the raw `open()` path, not just `fopen()`. The engine's
/// anti-tamper reads `/proc/self/{maps,mounts,cmdline}` and `s_fopen` already
/// Android-ises those, but a reader that calls `open()`+`read()` bypasses the
/// stdio layer entirely and used to get the real FreeBSD procfs -- every "not
/// Android" tell intact. An anonymous temp file (created, then unlinked, so it
/// never appears in the tree) gives a real fd the reader can `read()`/`lseek()`.
static int fd_from_bytes(const char* data, size_t len) {
    char tmpl[] = "/tmp/cordial_synthproc_XXXXXX";
    int fd = ::mkstemp(tmpl);
    if (fd < 0) {
        return -1;
    }
    ::unlink(tmpl);
    size_t off = 0;
    while (off < len) {
        ssize_t w = ::write(fd, data + off, len - off);
        if (w <= 0) {
            ::close(fd);
            return -1;
        }
        off += static_cast<size_t>(w);
    }
    ::lseek(fd, 0, SEEK_SET);
    return fd;
}
#endif

FILE* s_fopen(const char* path, const char* mode) {
#if defined(__FreeBSD__)
    if (const char* synth = synth_proc_content(path)) {
        trace("fopen", path, "synth-android");
        return ::fmemopen(const_cast<char*>(synth), std::strlen(synth), "r");
    }
    if (const char* ssynth = synth_sys_content(path)) {
        trace("fopen", path, "synth-sys");
        return ::fmemopen(const_cast<char*>(ssynth), std::strlen(ssynth), "r");
    }
    if (std::getenv("CORDIAL_FAKE_PROC") != nullptr &&
        std::strcmp(path, "/proc/self/maps") == 0) {
        char _mb[PATH_MAX];
        const char* mreal = remap(path, _mb, sizeof _mb);
        if (FILE* f = synth_maps(mreal ? mreal : path)) {
            trace("fopen", path, "synth-maps");
            return f;
        }
    }
#endif
    REMAP(path);
    FILE* f = ::fopen(real, mode);
    trace("fopen", real, f ? "ok" : "null");
    return f;
}

/// `open` is variadic: the mode argument exists only for `O_CREAT`/`O_TMPFILE`.
/// Reading it unconditionally would walk the register save area for an argument
/// the caller never pushed, which is the mistake that makes `CORDIAL_TRACE=1`
/// abort the engine.
///
/// On FreeBSD the flags are the engine's *Linux* O_ bits, and the test for
/// whether a mode was passed has to be made in that vocabulary too: Linux
/// O_CREAT is 0x40, which FreeBSD calls O_ASYNC, so testing the host's O_CREAT
/// (0x200, Linux O_TRUNC) read a mode that was never passed on every truncating
/// open and skipped the one that was on every creating one.
int s_open(const char* path, int flags, ...) {
    unsigned mode = 0;
#if defined(__FreeBSD__)
    const bool has_mode = cordial_fbsd_open_takes_mode(flags) != 0;
#else
    const bool has_mode = (flags & (O_CREAT | O_TMPFILE)) != 0;
#endif
    if (has_mode) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
#if defined(__FreeBSD__)
    int host_flags;
    if (cordial_fbsd_open_flags(flags, &host_flags) != 0) {
        trace_i("open", path, -1);
        return -1;
    }
    flags = host_flags;
    // Serve the same synthetic /proc content `s_fopen` does, but through the raw
    // `open()` path: an anti-tamper that reads /proc/self/{maps,mounts,cmdline}
    // with open()+read() instead of fopen() otherwise gets the real FreeBSD
    // procfs here (full of /compat/linux, ~/.cache, cordial-run tells). Only the
    // read path is synthesised; a writing/creating open falls through.
    if ((flags & (O_WRONLY | O_RDWR | O_CREAT)) == 0) {
        if (const char* synth = synth_proc_content(path)) {
            int sfd = fd_from_bytes(synth, std::strlen(synth));
            if (sfd >= 0) {
                trace_i("open", path, sfd);
                return sfd;
            }
        }
        if (const char* ssynth = synth_sys_content(path)) {
            int sfd = fd_from_bytes(ssynth, std::strlen(ssynth));
            if (sfd >= 0) {
                trace_i("open", path, sfd);
                return sfd;
            }
        }
        if (std::getenv("CORDIAL_FAKE_PROC") != nullptr &&
            std::strcmp(path, "/proc/self/maps") == 0) {
            char _mb[PATH_MAX];
            const char* mreal = remap(path, _mb, sizeof _mb);
            std::string m = build_synth_maps(mreal ? mreal : path);
            if (!m.empty()) {
                int sfd = fd_from_bytes(m.data(), m.size());
                if (sfd >= 0) {
                    trace_i("open", path, sfd);
                    return sfd;
                }
            }
        }
    }
#endif
    REMAP(path);
    int r = ::open(real, flags, mode);
#if defined(__FreeBSD__)
    if (r < 0)
        cordial_fbsd_errno_to_linux();
#endif
    trace_i("open", real, r);
    return r;
}

/// `statvfs`, which the engine imports and Cordial had never intercepted.
///
/// Two separate defects, and the second is why this is not just a redirect.
///
/// **It was not path-translated.** Every other path-taking call here is; this
/// one went straight to the host with whatever the engine built, and because it
/// was not in the table it did not appear in `CORDIAL_TRACE_PATHS=1` output
/// either. A trace that cannot see a call is not evidence the call did not
/// happen, and a conclusion in flag-init.md §23.2 -- "storage is never
/// attempted, 19,296 path calls and none of them `rbx-storage`" -- was drawn
/// with this blind spot in it.
///
/// **The struct layouts differ.** bionic's `struct statvfs` runs
/// `f_fsid, f_flag, f_namemax`; glibc's inserts an `int __f_unused` after
/// `f_fsid`, which on LP64 pushes `f_flag` and `f_namemax` eight bytes along.
/// The size fields the engine cares about for free space -- `f_bsize` through
/// `f_favail` -- happen to align, so this is not obviously fatal, but the engine
/// reading `f_flag` gets glibc's padding and reading `f_namemax` gets glibc's
/// `f_flag`. `ST_RDONLY` lives in `f_flag`. This is the same family as the
/// `sigset_t`, `struct sigaction` and `mallinfo` divergences already recorded,
/// and it is fixed the same way: fill the bionic shape by hand rather than hope
/// the two agree.
struct bionic_statvfs {
    unsigned long f_bsize;
    unsigned long f_frsize;
    unsigned long f_blocks;
    unsigned long f_bfree;
    unsigned long f_bavail;
    unsigned long f_files;
    unsigned long f_ffree;
    unsigned long f_favail;
    unsigned long f_fsid;
    unsigned long f_flag;
    unsigned long f_namemax;
    uint32_t __f_reserved[6];
};

int s_statvfs(const char* path, bionic_statvfs* out) {
    REMAP(path);
    struct ::statvfs host {};
    const int r = ::statvfs(real, &host);
    trace_i("statvfs", real, r);
    if (r == 0 && out) {
        *out = bionic_statvfs{};
        out->f_bsize = host.f_bsize;
        out->f_frsize = host.f_frsize;
        out->f_blocks = host.f_blocks;
        out->f_bfree = host.f_bfree;
        out->f_bavail = host.f_bavail;
        out->f_files = host.f_files;
        out->f_ffree = host.f_ffree;
        out->f_favail = host.f_favail;
        out->f_fsid = host.f_fsid;
        out->f_flag = host.f_flag;
        out->f_namemax = host.f_namemax;
    }
    return r;
}

#undef REMAP

} // namespace

/// The same rewrite for freebsd_abi.c, whose `open`/`__open_2`/`openat`
/// replaced this file's for the engine when the ABI layer landed and so
/// silently dropped the /proc and /system redirects: the engine then logged
/// `Failed to open /proc/meminfo` and reported 0 MB of memory. Returns `path`
/// itself when nothing applies.
extern "C" const char* cordial_path_remap(const char* path, char* buf, size_t n) {
    const char* r = remap(path, buf, n);
    return r ? r : path;
}

extern "C" struct CordialSystemSymbol {
    const char* name;
    void* addr;
};

/// Point the redirect at a host directory. Passing null or "" disables it.
extern "C" void cordial_set_system_root(const char* root) {
    if (!root || !*root) {
        g_root_len = 0;
        g_root[0] = '\0';
        return;
    }
    std::snprintf(g_root, sizeof g_root, "%s", root);
    g_root_len = std::strlen(g_root);
    // A trailing slash would produce `<root>//fonts`, which works but reads
    // badly in a trace, and would break a later exact-prefix comparison.
    while (g_root_len > 1 && g_root[g_root_len - 1] == '/') {
        g_root[--g_root_len] = '\0';
    }
}

/// Turn on the path log. Separate from the root so tracing works even when the
/// redirect is disabled.
extern "C" void cordial_set_path_trace(int on) {
    g_trace = on != 0;
}

extern "C" const CordialSystemSymbol* cordial_system_symbols(size_t* count) {
    static const CordialSystemSymbol table[] = {
        {"stat", (void*)&s_stat},
        {"lstat", (void*)&s_lstat},
#if defined(__FreeBSD__)
        {"fstat", (void*)&s_fstat},
#endif
        {"access", (void*)&s_access},
        {"opendir", (void*)&s_opendir},
        {"realpath", (void*)&s_realpath},
        {"readlink", (void*)&s_readlink},
        {"fopen", (void*)&s_fopen},
        {"statvfs", (void*)&s_statvfs},
        {"open", (void*)&s_open},
    };
    *count = sizeof(table) / sizeof(table[0]);
    return table;
}
