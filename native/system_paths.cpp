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
#include <cstdarg>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <cstdlib>

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
    // engine (and its anti-cheat's process introspection) sees what it expects.
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

FILE* s_fopen(const char* path, const char* mode) {
    REMAP(path);
    FILE* f = ::fopen(real, mode);
    trace("fopen", real, f ? "ok" : "null");
    return f;
}

/// `open` is variadic: the mode argument exists only for `O_CREAT`/`O_TMPFILE`.
/// Reading it unconditionally would walk the register save area for an argument
/// the caller never pushed, which is the mistake that makes `CORDIAL_TRACE=1`
/// abort the engine.
int s_open(const char* path, int flags, ...) {
    unsigned mode = 0;
    if (flags & (O_CREAT | O_TMPFILE)) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, unsigned);
        va_end(ap);
    }
    REMAP(path);
    int r = ::open(real, flags, mode);
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
