// freebsd_abi.c — Linux ABI values at the libc boundary, translated for FreeBSD.
//
// libroblox.so is compiled against bionic, so every flag word, option number,
// address family, sockaddr and errno it exchanges with libc is spelled the Linux
// way. On FreeBSD those calls used to resolve straight to the host libc, which
// reads the same bits as different things. Measured with a native FreeBSD 14.4
// program handing the host libc the engine's values:
//
//     eventfd(0, 0x800|0x80000)                 -> -1 EINVAL
//     socket(AF_INET, SOCK_STREAM|0x800|0x80000) -> -1 EPROTOTYPE
//     socketpair(AF_UNIX, SOCK_STREAM|0x80000)    -> -1 EPROTOTYPE
//     fcntl(s, F_SETFL, 0x800)                    ->  0, and the fd stays blocking
//
// and ktrace of the engine at startup shows the last one happening for real:
// `pipe2(..., 0)` then `fcntl(F_SETFL, 0x800)` on both ends, a self-pipe the
// engine believes non-blocking that is not. Linux O_NONBLOCK (0x800) is FreeBSD
// O_EXCL, which F_SETFL ignores, so the call "succeeds" and changes nothing --
// the worst kind of wrong answer, because nothing downstream points back here.
//
// The same class of defect as `pthread_mutex_t`, `struct addrinfo` and the
// `mmap` flags in freebsd_libc_compat.c, and the same remedy: Cordial owns the
// symbol table, so the translation belongs at this boundary (ADR-001 -- nothing
// here touches the engine, only what libc answers it).
//
// Every constant below was read from the headers, not remembered: Linux values
// from bionic's kernel uapi headers in third_party/mcpelauncher-linker, FreeBSD
// values from /usr/include on FreeBSD 14.4. Where a Linux value has no honest
// FreeBSD counterpart the call fails, with a Linux errno, rather than silently
// dropping the bit -- a stub that reports success is worse than one that
// reports failure (AGENTS.md).
//
// Whole file compiles to nothing off FreeBSD.
#if defined(__FreeBSD__)

#include "freebsd_abi.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <pthread.h>
#include <time.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/filio.h>
#include <sys/ioctl.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/sockio.h>
#include <sys/ttycom.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/un.h>
#include <arpa/inet.h>
#include <time.h>
#include <unistd.h>

// ── errno ───────────────────────────────────────────────────────────────────
//
// `__errno` hands the engine FreeBSD's errno slot, so after a failure it reads
// FreeBSD numbering: EAGAIN is 35 there and 11 on Linux, EINPROGRESS 36 against
// 115, ECONNREFUSED 61 against 111. A non-blocking read that would block
// reported "Resource deadlock avoided" to a caller testing for EAGAIN, and a
// connect in progress reported a number the engine has no name for. 1..34 agree
// (except 11), which is why nothing obvious broke.
//
// One table, both directions. Built by name from asm-generic/errno{,-base}.h
// and FreeBSD sys/errno.h; the FreeBSD-only codes at the bottom have no Linux
// name and are mapped to the nearest meaning a Linux caller can act on.
static const struct { int fb, lx; } k_errno[] = {
    {1, 1},     {2, 2},     {3, 3},     {4, 4},     {5, 5},     {6, 6},
    {7, 7},     {8, 8},     {9, 9},     {10, 10},   {11, 35},   /* EDEADLK */
    {12, 12},   {13, 13},   {14, 14},   {15, 15},   {16, 16},   {17, 17},
    {18, 18},   {19, 19},   {20, 20},   {21, 21},   {22, 22},   {23, 23},
    {24, 24},   {25, 25},   {26, 26},   {27, 27},   {28, 28},   {29, 29},
    {30, 30},   {31, 31},   {32, 32},   {33, 33},   {34, 34},
    {35, 11},   /* EAGAIN / EWOULDBLOCK */
    {36, 115},  /* EINPROGRESS */
    {37, 114},  /* EALREADY */
    {38, 88},   /* ENOTSOCK */
    {39, 89},   /* EDESTADDRREQ */
    {40, 90},   /* EMSGSIZE */
    {41, 91},   /* EPROTOTYPE */
    {42, 92},   /* ENOPROTOOPT */
    {43, 93},   /* EPROTONOSUPPORT */
    {44, 94},   /* ESOCKTNOSUPPORT */
    {45, 95},   /* EOPNOTSUPP / ENOTSUP */
    {46, 96},   /* EPFNOSUPPORT */
    {47, 97},   /* EAFNOSUPPORT */
    {48, 98},   /* EADDRINUSE */
    {49, 99},   /* EADDRNOTAVAIL */
    {50, 100},  /* ENETDOWN */
    {51, 101},  /* ENETUNREACH */
    {52, 102},  /* ENETRESET */
    {53, 103},  /* ECONNABORTED */
    {54, 104},  /* ECONNRESET */
    {55, 105},  /* ENOBUFS */
    {56, 106},  /* EISCONN */
    {57, 107},  /* ENOTCONN */
    {58, 108},  /* ESHUTDOWN */
    {59, 109},  /* ETOOMANYREFS */
    {60, 110},  /* ETIMEDOUT */
    {61, 111},  /* ECONNREFUSED */
    {62, 40},   /* ELOOP */
    {63, 36},   /* ENAMETOOLONG */
    {64, 112},  /* EHOSTDOWN */
    {65, 113},  /* EHOSTUNREACH */
    {66, 39},   /* ENOTEMPTY */
    {68, 87},   /* EUSERS */
    {69, 122},  /* EDQUOT */
    {70, 116},  /* ESTALE */
    {71, 66},   /* EREMOTE */
    {77, 37},   /* ENOLCK */
    {78, 38},   /* ENOSYS */
    {82, 43},   /* EIDRM */
    {83, 42},   /* ENOMSG */
    {84, 75},   /* EOVERFLOW */
    {85, 125},  /* ECANCELED */
    {86, 84},   /* EILSEQ */
    {89, 74},   /* EBADMSG */
    {90, 72},   /* EMULTIHOP */
    {91, 67},   /* ENOLINK */
    {92, 71},   /* EPROTO */
    {95, 131},  /* ENOTRECOVERABLE */
    {96, 130},  /* EOWNERDEAD */
    // FreeBSD-only. Nearest Linux meaning; none of these is expected from the
    // calls the engine makes, and each is a better answer than a number the
    // caller would read as something unrelated.
    {67, 11},   /* EPROCLIM -> EAGAIN, what Linux fork() says at the limit */
    {72, 5},    /* EBADRPC -> EIO */
    {73, 5},    /* ERPCMISMATCH -> EIO */
    {74, 5},    /* EPROGUNAVAIL -> EIO */
    {75, 5},    /* EPROGMISMATCH -> EIO */
    {76, 5},    /* EPROCUNAVAIL -> EIO */
    {79, 22},   /* EFTYPE -> EINVAL */
    {80, 13},   /* EAUTH -> EACCES */
    {81, 13},   /* ENEEDAUTH -> EACCES */
    {87, 61},   /* ENOATTR -> ENODATA, which is what Linux xattr calls use */
    {88, 22},   /* EDOOFUS -> EINVAL */
    {93, 1},    /* ENOTCAPABLE -> EPERM */
    {94, 1},    /* ECAPMODE -> EPERM */
    {97, 5},    /* EINTEGRITY -> EIO */
};

static int errno_fb_to_lx(int fb) {
    if (fb <= 0)
        return fb;
    for (size_t i = 0; i < sizeof k_errno / sizeof k_errno[0]; i++)
        if (k_errno[i].fb == fb)
            return k_errno[i].lx;
    return fb;
}

static int errno_lx_to_fb(int lx) {
    if (lx <= 0)
        return lx;
    for (size_t i = 0; i < sizeof k_errno / sizeof k_errno[0]; i++)
        if (k_errno[i].lx == lx)
            return k_errno[i].fb;
    // Linux codes with no FreeBSD name (ENODATA, ECHRNG, ...) have no message
    // either; EINVAL's is at least a real one.
    return lx < 35 ? lx : EINVAL;
}

// Whether the value in this thread's errno slot is already Linux-numbered.
//
// There is one slot and two vocabularies: the host libc writes FreeBSD numbers
// into it, and the engine reads Linux ones out of it. So a value is translated
// in place, once, and the result remembered; if the slot still holds exactly
// that value on the next `__errno` it is not translated again. Without the
// memory an EAGAIN read twice would go 35 -> 11 -> 35 (FreeBSD 11 is EDEADLK).
//
// The residual ambiguity is honest to name: if the engine clears errno, an
// untranslated host call then fails with a FreeBSD number that happens to equal
// the last Linux number handed out, it is taken as already translated. Only
// 11 and 35..97 can collide at all, and the calls that produce the common ones
// are wrapped below and set a Linux value directly.
static __thread int t_lx_errno_valid;
static __thread int t_lx_errno;

static void set_lx_errno(int lx) {
    errno = lx;
    t_lx_errno = lx;
    t_lx_errno_valid = 1;
}

void cordial_fbsd_errno_to_linux(void) {
    int v = errno;
    if (t_lx_errno_valid && v == t_lx_errno)
        return;
    set_lx_errno(errno_fb_to_lx(v));
}

// bionic's `__errno`. It is declared `__attribute_const__` in bionic's errno.h,
// so the compiler is free to call it once per function and reuse the pointer
// across later calls -- which is why this lazy translation is not enough on its
// own, and why every wrapper below also sets a Linux value itself on failure:
// a hoisted pointer reads the slot directly and never comes back through here.
int* cordial_fbsd_bionic_errno(void) {
    cordial_fbsd_errno_to_linux();
    return &errno;
}

// A refusal here never reaches the kernel, so ktrace cannot see it: a RakNet
// join stalled after `binding socket on inaddr_any:0` with no socket() in the
// trace at all. `CORDIAL_TRACE_ABI=1` names every refusal.
#define FAIL_LX(e)                                                              \
    do {                                                                        \
        if (getenv("CORDIAL_TRACE_ABI"))                                        \
            fprintf(stderr, "[abi] %s refused: Linux errno %d\n", __func__, (e)); \
        set_lx_errno(e);                                                        \
        return -1;                                                              \
    } while (0)

#define LX_EINVAL 22
#define LX_ENOTTY 25
#define LX_ENOPROTOOPT 92
#define LX_ESOCKTNOSUPPORT 94
#define LX_EOPNOTSUPP 95
#define LX_EAFNOSUPPORT 97

// Pass a host result through, translating errno if it failed.
#define RET_TRANSLATED(expr)                        \
    do {                                            \
        __typeof__(expr) r_ = (expr);               \
        if (r_ < 0)                                 \
            cordial_fbsd_errno_to_linux();          \
        return r_;                                  \
    } while (0)

// ── open(2) flags ───────────────────────────────────────────────────────────
//
// Linux (asm-generic/fcntl.h, octal)       FreeBSD (sys/fcntl.h)
//   O_CREAT      0x40                       0x200
//   O_EXCL       0x80                       0x800
//   O_NOCTTY     0x100                      0x8000
//   O_TRUNC      0x200                      0x400
//   O_APPEND     0x400                      0x8
//   O_NONBLOCK   0x800                      0x4
//   O_DSYNC      0x1000                     0x1000000
//   FASYNC       0x2000                     0x40 (O_ASYNC)
//   O_DIRECT     0x4000                     0x10000
//   O_LARGEFILE  0x8000                     -- (off_t is 64-bit; meaningless)
//   O_DIRECTORY  0x10000                    0x20000
//   O_NOFOLLOW   0x20000                    0x100
//   O_NOATIME    0x40000                    -- (an atime hint; nothing to honour)
//   O_CLOEXEC    0x80000                    0x100000
//   __O_SYNC     0x100000 (O_SYNC=|O_DSYNC) 0x80 (O_SYNC)
//   O_PATH       0x200000                   0x400000
//   __O_TMPFILE  0x400000                   -- (no such thing on FreeBSD)
//
// Untranslated, Linux O_CREAT (0x40) reads as FreeBSD O_ASYNC, O_EXCL (0x80) as
// O_SYNC and O_TRUNC (0x200) as O_CREAT: a plain create failed ENOENT, and a
// create-and-truncate created the file but never truncated an existing one.
#define LX_O_ACCMODE   0x3
#define LX_O_CREAT     0x40
#define LX_O_EXCL      0x80
#define LX_O_NOCTTY    0x100
#define LX_O_TRUNC     0x200
#define LX_O_APPEND    0x400
#define LX_O_NONBLOCK  0x800
#define LX_O_DSYNC     0x1000
#define LX_O_ASYNC     0x2000
#define LX_O_DIRECT    0x4000
#define LX_O_LARGEFILE 0x8000
#define LX_O_DIRECTORY 0x10000
#define LX_O_NOFOLLOW  0x20000
#define LX_O_NOATIME   0x40000
#define LX_O_CLOEXEC   0x80000
#define LX___O_SYNC    0x100000
#define LX_O_PATH      0x200000
#define LX___O_TMPFILE 0x400000

static const struct { int lx, fb; } k_oflags[] = {
    {LX_O_CREAT, O_CREAT},         {LX_O_EXCL, O_EXCL},
    {LX_O_NOCTTY, O_NOCTTY},       {LX_O_TRUNC, O_TRUNC},
    {LX_O_APPEND, O_APPEND},       {LX_O_NONBLOCK, O_NONBLOCK},
    {LX_O_DSYNC, O_DSYNC},         {LX_O_ASYNC, O_ASYNC},
    {LX_O_DIRECT, O_DIRECT},       {LX_O_DIRECTORY, O_DIRECTORY},
    {LX_O_NOFOLLOW, O_NOFOLLOW},   {LX_O_CLOEXEC, O_CLOEXEC},
    {LX_O_PATH, O_PATH},
};

int cordial_fbsd_open_takes_mode(int lx) {
    return (lx & (LX_O_CREAT | LX___O_TMPFILE)) != 0;
}

int cordial_fbsd_open_flags(int lx, int* host) {
    int fb = lx & LX_O_ACCMODE; // 0/1/2 agree
    int rest = lx & ~LX_O_ACCMODE;
    if (rest & LX___O_TMPFILE) {
        // Linux answers EOPNOTSUPP for a filesystem without O_TMPFILE, and every
        // caller of it has a fallback for exactly that answer.
        FAIL_LX(LX_EOPNOTSUPP);
    }
    if ((rest & LX___O_SYNC)) {
        fb |= O_SYNC;
        rest &= ~(LX___O_SYNC | LX_O_DSYNC);
    }
    rest &= ~(LX_O_LARGEFILE | LX_O_NOATIME);
    for (size_t i = 0; i < sizeof k_oflags / sizeof k_oflags[0]; i++) {
        if (rest & k_oflags[i].lx) {
            fb |= k_oflags[i].fb;
            rest &= ~k_oflags[i].lx;
        }
    }
    if (rest)
        FAIL_LX(LX_EINVAL);
    *host = fb;
    return 0;
}

// F_GETFL's answer, FreeBSD -> Linux. Bits with no Linux spelling (FreeBSD's
// internal O_SHLOCK/O_EXLOCK/O_VERIFY) are not reported: the caller could only
// misread them.
static int oflags_to_lx(int fb) {
    int lx = fb & O_ACCMODE;
    if ((fb & O_SYNC))
        lx |= LX___O_SYNC | LX_O_DSYNC;
    for (size_t i = 0; i < sizeof k_oflags / sizeof k_oflags[0]; i++)
        if (fb & k_oflags[i].fb)
            lx |= k_oflags[i].lx;
    return lx;
}

extern const char* cordial_path_remap(const char* path, char* buf, size_t n);

int cordial_fbsd_open(const char* path, int flags, ...) {
    mode_t mode = 0;
    if (cordial_fbsd_open_takes_mode(flags)) {
        va_list ap;
        va_start(ap, flags);
        mode = (mode_t)va_arg(ap, unsigned);
        va_end(ap);
    }
    int fb;
    if (cordial_fbsd_open_flags(flags, &fb) != 0)
        return -1;
    char buf[PATH_MAX];
    RET_TRANSLATED(open(cordial_path_remap(path, buf, sizeof buf), fb, mode));
}

// bionic's FORTIFY `open` without a mode: only legal without O_CREAT.
int cordial_fbsd___open_2(const char* path, int flags) {
    int fb;
    if (cordial_fbsd_open_flags(flags, &fb) != 0)
        return -1;
    char buf[PATH_MAX];
    RET_TRANSLATED(open(cordial_path_remap(path, buf, sizeof buf), fb, 0));
}

int cordial_fbsd_openat(int dirfd, const char* path, int flags, ...) {
    mode_t mode = 0;
    if (cordial_fbsd_open_takes_mode(flags)) {
        va_list ap;
        va_start(ap, flags);
        mode = (mode_t)va_arg(ap, unsigned);
        va_end(ap);
    }
    int fb;
    if (cordial_fbsd_open_flags(flags, &fb) != 0)
        return -1;
    // AT_FDCWD is -100 on both.
    char buf[PATH_MAX];
    // Only absolute paths are remapped; a relative one is the caller's dirfd's.
    const char* p = (path && path[0] == '/') ? cordial_path_remap(path, buf, sizeof buf) : path;
    RET_TRANSLATED(openat(dirfd, p, fb, mode));
}

// ── pipe2 ───────────────────────────────────────────────────────────────────
// freebsd_libc_compat.c defines the process's `pipe2` (it records each pair so
// the GameActivity command pipe's write end can be found) and passes flags to
// the syscall raw -- correct for Cordial's own callers, which speak FreeBSD. The
// engine speaks Linux, so its calls are translated here and then handed to that
// same function, keeping the bookkeeping.
int cordial_fbsd_pipe2(int fds[2], int lx) {
    int fb = 0;
    if (lx & LX_O_NONBLOCK)
        fb |= O_NONBLOCK;
    if (lx & LX_O_CLOEXEC)
        fb |= O_CLOEXEC;
    // O_DIRECT here is Linux's packet-mode pipe, which FreeBSD has no form of.
    if (lx & ~(LX_O_NONBLOCK | LX_O_CLOEXEC))
        FAIL_LX(LX_EINVAL);
    RET_TRANSLATED(pipe2(fds, fb));
}

// ── fcntl ───────────────────────────────────────────────────────────────────
//
// Commands 0..4 agree. The rest do not:
//   Linux F_GETLK 5 / F_SETLK 6 / F_SETLKW 7   FreeBSD 11 / 12 / 13
//   Linux F_SETOWN 8 / F_GETOWN 9              FreeBSD 6 / 5
//   Linux F_GETLK64..F_SETLKW64 12..14         (the same struct on LP64)
//   Linux F_DUPFD_CLOEXEC 1030                 FreeBSD 17
//   Linux F_ADD_SEALS 1033 / F_GET_SEALS 1034  FreeBSD 19 / 20 (seal bits agree)
// Untranslated, a Linux F_SETLK (6) is FreeBSD's F_SETOWN.
//
// struct flock is laid out differently as well. Linux: short l_type@0, short
// l_whence@2, off_t l_start@8, off_t l_len@16, pid_t l_pid@24. FreeBSD: l_start@0,
// l_len@8, l_pid@16, l_type@20, l_whence@22, l_sysid@24. And the lock types
// differ: Linux RDLCK 0 / WRLCK 1 / UNLCK 2, FreeBSD RDLCK 1 / UNLCK 2 / WRLCK 3.
struct lx_flock {
    short l_type;
    short l_whence;
    off_t l_start;
    off_t l_len;
    pid_t l_pid;
};

static int flock_type_to_fb(short t) {
    switch (t) {
    case 0: return F_RDLCK;
    case 1: return F_WRLCK;
    case 2: return F_UNLCK;
    default: return -1;
    }
}
static short flock_type_to_lx(short t) {
    switch (t) {
    case F_RDLCK: return 0;
    case F_WRLCK: return 1;
    default: return 2;
    }
}

static int fcntl_lock(int fd, int fbcmd, struct lx_flock* lx) {
    struct flock fl;
    memset(&fl, 0, sizeof fl);
    int t = flock_type_to_fb(lx->l_type);
    if (t < 0)
        FAIL_LX(LX_EINVAL);
    fl.l_type = (short)t;
    fl.l_whence = lx->l_whence; // SEEK_* agree
    fl.l_start = lx->l_start;
    fl.l_len = lx->l_len;
    fl.l_pid = lx->l_pid;
    int r = fcntl(fd, fbcmd, &fl);
    if (r < 0) {
        cordial_fbsd_errno_to_linux();
        return r;
    }
    if (fbcmd == F_GETLK) {
        lx->l_type = flock_type_to_lx(fl.l_type);
        lx->l_whence = fl.l_whence;
        lx->l_start = fl.l_start;
        lx->l_len = fl.l_len;
        lx->l_pid = fl.l_pid;
    }
    return r;
}

// fcntl's third argument is read unconditionally, as glibc and bionic do: on
// x86-64 it is the third integer register, present in the save area whether or
// not the caller meant it.
int cordial_fbsd_fcntl(int fd, int cmd, ...) {
    va_list ap;
    va_start(ap, cmd);
    uintptr_t arg = va_arg(ap, uintptr_t);
    va_end(ap);

    switch (cmd) {
    case 0: // F_DUPFD
    case 1: // F_GETFD
    case 2: // F_SETFD (FD_CLOEXEC is 1 on both)
        RET_TRANSLATED(fcntl(fd, cmd, (int)arg));
    case 3: { // F_GETFL
        int r = fcntl(fd, F_GETFL);
        if (r < 0) {
            cordial_fbsd_errno_to_linux();
            return r;
        }
        return oflags_to_lx(r);
    }
    case 4: { // F_SETFL
        // Linux ignores the access mode and the open-time-only bits here, and
        // callers rely on that: the idiom is F_SETFL(F_GETFL() | O_NONBLOCK).
        int lx = (int)arg & ~(LX_O_ACCMODE | LX_O_CREAT | LX_O_EXCL | LX_O_NOCTTY |
                              LX_O_TRUNC | LX_O_LARGEFILE | LX_O_CLOEXEC);
        int fb;
        if (cordial_fbsd_open_flags(lx, &fb) != 0)
            return -1;
        RET_TRANSLATED(fcntl(fd, F_SETFL, fb));
    }
    case 5: case 12: // F_GETLK, F_GETLK64
        return fcntl_lock(fd, F_GETLK, (struct lx_flock*)arg);
    case 6: case 13: // F_SETLK, F_SETLK64
        return fcntl_lock(fd, F_SETLK, (struct lx_flock*)arg);
    case 7: case 14: // F_SETLKW, F_SETLKW64
        return fcntl_lock(fd, F_SETLKW, (struct lx_flock*)arg);
    case 8: // F_SETOWN
        RET_TRANSLATED(fcntl(fd, F_SETOWN, (int)arg));
    case 9: // F_GETOWN
        RET_TRANSLATED(fcntl(fd, F_GETOWN));
    case 1030: // F_DUPFD_CLOEXEC
        RET_TRANSLATED(fcntl(fd, F_DUPFD_CLOEXEC, (int)arg));
    case 1033: // F_ADD_SEALS
        RET_TRANSLATED(fcntl(fd, F_ADD_SEALS, (int)arg));
    case 1034: // F_GET_SEALS
        RET_TRANSLATED(fcntl(fd, F_GET_SEALS));
    default:
        // F_SETSIG, leases, F_NOTIFY, pipe sizes, OFD locks, RW hints: Linux-only.
        FAIL_LX(LX_EINVAL);
    }
}

// ── socket type flags, eventfd ──────────────────────────────────────────────
// Linux SOCK_NONBLOCK/SOCK_CLOEXEC are O_NONBLOCK/O_CLOEXEC (0x800/0x80000);
// FreeBSD's are 0x20000000/0x10000000. The base types 1..5 agree. EFD_* are the
// O_ values on both, so they differ in the same way; EFD_SEMAPHORE is 1 on both.
#define LX_SOCK_TYPE_MASK 0xf

static int sock_flags_to_fb(int lx, int* fb) {
    *fb = 0;
    if (lx & LX_O_NONBLOCK)
        *fb |= SOCK_NONBLOCK;
    if (lx & LX_O_CLOEXEC)
        *fb |= SOCK_CLOEXEC;
    if (lx & ~(LX_O_NONBLOCK | LX_O_CLOEXEC))
        FAIL_LX(LX_EINVAL);
    return 0;
}

static int sock_type_to_fb(int lx, int* fb) {
    int base = lx & LX_SOCK_TYPE_MASK;
    int flags;
    if (sock_flags_to_fb(lx & ~LX_SOCK_TYPE_MASK, &flags) != 0)
        return -1;
    if (base < 1 || base > 5) // SOCK_DCCP 6 and SOCK_PACKET 10 are Linux-only
        FAIL_LX(LX_ESOCKTNOSUPPORT);
    *fb = base | flags;
    return 0;
}

// AF_UNIX 1, AF_INET 2 agree. AF_INET6 is 10 on Linux and 28 on FreeBSD; FreeBSD
// 10 is AF_CCITT, so an untranslated v6 socket asks for X.25. AF_NETLINK is 16
// against 38 (FreeBSD 14 speaks the Linux rtnetlink protocol over it).
int cordial_fbsd_af_from_linux(int lx) {
    switch (lx) {
    case 0: return AF_UNSPEC;
    case 1: return AF_UNIX;
    case 2: return AF_INET;
    case 10: return AF_INET6;
    case 16: return AF_NETLINK;
    default: return -1;
    }
}

int cordial_fbsd_af_to_linux(int fb) {
    switch (fb) {
    case AF_UNSPEC: return 0;
    case AF_UNIX: return 1;
    case AF_INET: return 2;
    case AF_INET6: return 10;
    case AF_NETLINK: return 16;
    default: return -1;
    }
}

int cordial_fbsd_socket(int domain, int type, int protocol) {
    int af = cordial_fbsd_af_from_linux(domain);
    if (af < 0 || af == AF_UNSPEC)
        FAIL_LX(LX_EAFNOSUPPORT);
    int fbtype;
    if (sock_type_to_fb(type, &fbtype) != 0)
        return -1;
    // IPPROTO_* are IANA numbers, and NETLINK_ROUTE is 0 on both.
    RET_TRANSLATED(socket(af, fbtype, protocol));
}

int cordial_fbsd_socketpair(int domain, int type, int protocol, int sv[2]) {
    int af = cordial_fbsd_af_from_linux(domain);
    if (af < 0 || af == AF_UNSPEC)
        FAIL_LX(LX_EAFNOSUPPORT);
    int fbtype;
    if (sock_type_to_fb(type, &fbtype) != 0)
        return -1;
    RET_TRANSLATED(socketpair(af, fbtype, protocol, sv));
}

int cordial_fbsd_eventfd(unsigned int initval, int lx) {
    int fb = 0;
    if (lx & LX_O_NONBLOCK)
        fb |= EFD_NONBLOCK;
    if (lx & LX_O_CLOEXEC)
        fb |= EFD_CLOEXEC;
    if (lx & 1)
        fb |= EFD_SEMAPHORE;
    if (lx & ~(LX_O_NONBLOCK | LX_O_CLOEXEC | 1))
        FAIL_LX(LX_EINVAL);
    RET_TRANSLATED(eventfd(initval, fb));
}

// ── sockaddr ────────────────────────────────────────────────────────────────
//
// Linux sockaddrs begin with a 2-byte sa_family; FreeBSD's with a 1-byte sa_len
// and a 1-byte sa_family. For AF_INET (16 bytes), AF_INET6 (28) and AF_NETLINK
// (12) everything after those two bytes is identical, so conversion is the
// header plus a copy. AF_UNIX differs in size as well: Linux sun_path is 108
// bytes (110 total), FreeBSD's 104 (106 total).
//
// Read by a FreeBSD kernel, a Linux sockaddr_in's family 2 (little-endian
// 0x02 0x00) is sa_len=2, sa_family=0 -- AF_UNSPEC -- so every connect() the
// engine made went nowhere at all.
#define LX_SUN_PATH 108

static int sa_from_linux(const void* lx, socklen_t len, struct sockaddr_storage* out,
                         socklen_t* outlen) {
    if (len < 2)
        FAIL_LX(LX_EINVAL);
    uint16_t fam;
    memcpy(&fam, lx, 2);
    memset(out, 0, sizeof *out);
    unsigned char* o = (unsigned char*)out;
    const unsigned char* in = (const unsigned char*)lx;
    size_t want;
    switch (fam) {
    case 2: // AF_INET
        if (len < sizeof(struct sockaddr_in))
            FAIL_LX(LX_EINVAL);
        want = sizeof(struct sockaddr_in);
        memcpy(o + 2, in + 2, want - 2);
        break;
    case 10: // AF_INET6: Linux accepts the 24-byte RFC 2133 form without scope id
        if (len < 24)
            FAIL_LX(LX_EINVAL);
        want = sizeof(struct sockaddr_in6);
        memcpy(o + 2, in + 2, (len < want ? len : want) - 2);
        break;
    case 16: // AF_NETLINK
        if (len < 12)
            FAIL_LX(LX_EINVAL);
        want = 12;
        memcpy(o + 2, in + 2, want - 2);
        break;
    case 1: { // AF_UNIX
        size_t cap = len - 2 > LX_SUN_PATH ? LX_SUN_PATH : len - 2;
        const char* p = (const char*)in + 2;
        // An abstract-namespace name (leading NUL) and Linux autobind (no path)
        // have no FreeBSD form; there is nowhere honest to put them.
        if (cap == 0 || p[0] == '\0')
            FAIL_LX(LX_EINVAL);
        size_t n = strnlen(p, cap);
        if (n > sizeof(((struct sockaddr_un*)0)->sun_path))
            FAIL_LX(LX_EINVAL);
        memcpy(o + 2, p, n);
        want = 2 + n;
        break;
    }
    case 0: // AF_UNSPEC: connect() with it dissolves a UDP association
        want = len > sizeof *out ? sizeof *out : len;
        memcpy(o + 2, in + 2, want - 2);
        break;
    default:
        FAIL_LX(LX_EAFNOSUPPORT);
    }
    o[0] = (unsigned char)want;
    o[1] = (unsigned char)cordial_fbsd_af_from_linux(fam);
    *outlen = (socklen_t)want;
    return 0;
}

size_t cordial_fbsd_sockaddr_to_linux(const struct sockaddr* sa, void* out, size_t cap) {
    unsigned char tmp[2 + LX_SUN_PATH];
    const unsigned char* in = (const unsigned char*)sa;
    size_t salen = sa->sa_len;
    size_t n;
    switch (sa->sa_family) {
    case AF_INET: n = sizeof(struct sockaddr_in); break;
    case AF_INET6: n = sizeof(struct sockaddr_in6); break;
    case AF_NETLINK: n = 12; break;
    case AF_UNIX: {
        const struct sockaddr_un* un = (const struct sockaddr_un*)sa;
        size_t plen = salen > 2 ? salen - 2 : 0;
        if (plen > sizeof un->sun_path)
            plen = sizeof un->sun_path;
        size_t pn = strnlen(un->sun_path, plen);
        // Linux reports a pathname socket as family + path + NUL, an unnamed one
        // as the family alone.
        n = pn ? 2 + pn + 1 : 2;
        memset(tmp, 0, sizeof tmp);
        memcpy(tmp + 2, un->sun_path, pn);
        uint16_t f = 1;
        memcpy(tmp, &f, 2);
        memcpy(out, tmp, n < cap ? n : cap);
        return n;
    }
    default: return 0;
    }
    memset(tmp, 0, sizeof tmp);
    memcpy(tmp + 2, in + 2, n - 2);
    uint16_t f = (uint16_t)cordial_fbsd_af_to_linux(sa->sa_family);
    memcpy(tmp, &f, 2);
    memcpy(out, tmp, n < cap ? n : cap);
    return n;
}

// The value-result half of accept/getsockname/recvfrom: the host filled `ss`,
// the engine's buffer is `lx` with capacity `*lxlen`. Truncates as the kernel
// would and reports the full length.
static void sa_out(const struct sockaddr_storage* ss, socklen_t hostlen, void* lx,
                   socklen_t* lxlen) {
    if (!lx || !lxlen)
        return;
    if (hostlen == 0) {
        *lxlen = 0;
        return;
    }
    size_t n = cordial_fbsd_sockaddr_to_linux((const struct sockaddr*)ss, lx, *lxlen);
    *lxlen = (socklen_t)n;
}

int cordial_fbsd_bind(int fd, const void* addr, socklen_t len) {
    struct sockaddr_storage ss;
    socklen_t sl;
    if (sa_from_linux(addr, len, &ss, &sl) != 0)
        return -1;
    RET_TRANSLATED(bind(fd, (struct sockaddr*)&ss, sl));
}

int cordial_fbsd_connect(int fd, const void* addr, socklen_t len) {
    struct sockaddr_storage ss;
    socklen_t sl;
    if (sa_from_linux(addr, len, &ss, &sl) != 0)
        return -1;
    RET_TRANSLATED(connect(fd, (struct sockaddr*)&ss, sl));
}

int cordial_fbsd_accept4(int fd, void* addr, socklen_t* len, int lxflags) {
    int fl;
    if (sock_flags_to_fb(lxflags, &fl) != 0)
        return -1;
    struct sockaddr_storage ss;
    socklen_t sl = sizeof ss;
    int r = accept4(fd, addr ? (struct sockaddr*)&ss : NULL, addr ? &sl : NULL, fl);
    if (r < 0) {
        cordial_fbsd_errno_to_linux();
        return r;
    }
    sa_out(&ss, sl, addr, len);
    return r;
}

int cordial_fbsd_accept(int fd, void* addr, socklen_t* len) {
    return cordial_fbsd_accept4(fd, addr, len, 0);
}

static int name_call(int (*fn)(int, struct sockaddr*, socklen_t*), int fd, void* addr,
                     socklen_t* len) {
    struct sockaddr_storage ss;
    socklen_t sl = sizeof ss;
    if (fn(fd, (struct sockaddr*)&ss, &sl) < 0) {
        cordial_fbsd_errno_to_linux();
        return -1;
    }
    sa_out(&ss, sl, addr, len);
    return 0;
}

int cordial_fbsd_getsockname(int fd, void* addr, socklen_t* len) {
    return name_call(getsockname, fd, addr, len);
}

int cordial_fbsd_getpeername(int fd, void* addr, socklen_t* len) {
    return name_call(getpeername, fd, addr, len);
}

// ── socket options ──────────────────────────────────────────────────────────
//
// SOL_SOCKET is 1 on Linux and 0xffff on FreeBSD, and the SO_* numbering is
// unrelated. IPPROTO_IP/IPV6/TCP levels agree but most option numbers inside
// them do not. Each entry below was checked in both sets of headers; anything
// not listed is Linux-only and answers ENOPROTOOPT, which callers of optional
// tuning already expect.
enum { V_NONE, V_ERRNO, V_AF, V_PMTU };

struct optmap {
    int lx_level, lx_opt, fb_level, fb_opt, value;
};

static const struct optmap k_opts[] = {
    // SOL_SOCKET
    {1, 1, SOL_SOCKET, SO_DEBUG, V_NONE},
    {1, 2, SOL_SOCKET, SO_REUSEADDR, V_NONE},
    {1, 3, SOL_SOCKET, SO_TYPE, V_NONE}, // SOCK_* base types agree
    {1, 4, SOL_SOCKET, SO_ERROR, V_ERRNO},
    {1, 5, SOL_SOCKET, SO_DONTROUTE, V_NONE},
    {1, 6, SOL_SOCKET, SO_BROADCAST, V_NONE},
    {1, 7, SOL_SOCKET, SO_SNDBUF, V_NONE},
    {1, 8, SOL_SOCKET, SO_RCVBUF, V_NONE},
    {1, 9, SOL_SOCKET, SO_KEEPALIVE, V_NONE},
    {1, 10, SOL_SOCKET, SO_OOBINLINE, V_NONE},
    {1, 13, SOL_SOCKET, SO_LINGER, V_NONE}, // struct linger {int, int} on both
    // Linux SO_REUSEPORT also load-balances, which is FreeBSD's SO_REUSEPORT_LB;
    // the plain form is what lets a second bind succeed, and that is the part
    // callers depend on.
    {1, 15, SOL_SOCKET, SO_REUSEPORT, V_NONE},
    {1, 18, SOL_SOCKET, SO_RCVLOWAT, V_NONE},
    {1, 19, SOL_SOCKET, SO_SNDLOWAT, V_NONE},
    {1, 20, SOL_SOCKET, SO_RCVTIMEO, V_NONE}, // struct timeval is 16 bytes on both
    {1, 21, SOL_SOCKET, SO_SNDTIMEO, V_NONE},
    {1, 29, SOL_SOCKET, SO_TIMESTAMP, V_NONE},
    {1, 30, SOL_SOCKET, SO_ACCEPTCONN, V_NONE},
    {1, 38, SOL_SOCKET, SO_PROTOCOL, V_NONE},
    {1, 39, SOL_SOCKET, SO_DOMAIN, V_AF},
    // IPPROTO_IP
    {0, 1, IPPROTO_IP, IP_TOS, V_NONE},
    {0, 2, IPPROTO_IP, IP_TTL, V_NONE},
    {0, 3, IPPROTO_IP, IP_HDRINCL, V_NONE},
    {0, 4, IPPROTO_IP, IP_OPTIONS, V_NONE},
    {0, 10, IPPROTO_IP, IP_DONTFRAG, V_PMTU}, // IP_MTU_DISCOVER
    {0, 13, IPPROTO_IP, IP_RECVTOS, V_NONE},
    {0, 32, IPPROTO_IP, IP_MULTICAST_IF, V_NONE},
    {0, 33, IPPROTO_IP, IP_MULTICAST_TTL, V_NONE},
    {0, 34, IPPROTO_IP, IP_MULTICAST_LOOP, V_NONE},
    {0, 35, IPPROTO_IP, IP_ADD_MEMBERSHIP, V_NONE},
    {0, 36, IPPROTO_IP, IP_DROP_MEMBERSHIP, V_NONE},
    // IPPROTO_IPV6
    {41, 16, IPPROTO_IPV6, IPV6_UNICAST_HOPS, V_NONE},
    {41, 17, IPPROTO_IPV6, IPV6_MULTICAST_IF, V_NONE},
    {41, 18, IPPROTO_IPV6, IPV6_MULTICAST_HOPS, V_NONE},
    {41, 19, IPPROTO_IPV6, IPV6_MULTICAST_LOOP, V_NONE},
    {41, 20, IPPROTO_IPV6, IPV6_JOIN_GROUP, V_NONE},
    {41, 21, IPPROTO_IPV6, IPV6_LEAVE_GROUP, V_NONE},
    {41, 23, IPPROTO_IPV6, IPV6_DONTFRAG, V_PMTU}, // IPV6_MTU_DISCOVER
    {41, 26, IPPROTO_IPV6, IPV6_V6ONLY, V_NONE},
    {41, 49, IPPROTO_IPV6, IPV6_RECVPKTINFO, V_NONE},
    {41, 50, IPPROTO_IPV6, IPV6_PKTINFO, V_NONE},
    {41, 51, IPPROTO_IPV6, IPV6_RECVHOPLIMIT, V_NONE},
    {41, 52, IPPROTO_IPV6, IPV6_HOPLIMIT, V_NONE},
    {41, 62, IPPROTO_IPV6, IPV6_DONTFRAG, V_NONE},
    {41, 66, IPPROTO_IPV6, IPV6_RECVTCLASS, V_NONE},
    {41, 67, IPPROTO_IPV6, IPV6_TCLASS, V_NONE},
    // IPPROTO_TCP. TCP_NODELAY and TCP_MAXSEG are 1 and 2 on both.
    {6, 1, IPPROTO_TCP, TCP_NODELAY, V_NONE},
    {6, 2, IPPROTO_TCP, TCP_MAXSEG, V_NONE},
    {6, 3, IPPROTO_TCP, TCP_NOPUSH, V_NONE}, // TCP_CORK: same "hold partial frames"
    {6, 4, IPPROTO_TCP, TCP_KEEPIDLE, V_NONE},
    {6, 5, IPPROTO_TCP, TCP_KEEPINTVL, V_NONE},
    {6, 6, IPPROTO_TCP, TCP_KEEPCNT, V_NONE},
    {6, 13, IPPROTO_TCP, TCP_CONGESTION, V_NONE},
    {6, 23, IPPROTO_TCP, TCP_FASTOPEN, V_NONE},
};

static const struct optmap* find_opt(int level, int opt) {
    for (size_t i = 0; i < sizeof k_opts / sizeof k_opts[0]; i++)
        if (k_opts[i].lx_level == level && k_opts[i].lx_opt == opt)
            return &k_opts[i];
    return NULL;
}

int cordial_fbsd_setsockopt(int fd, int level, int opt, const void* val, socklen_t len) {
    const struct optmap* m = find_opt(level, opt);
    if (!m)
        FAIL_LX(LX_ENOPROTOOPT);
    if (m->value == V_PMTU) {
        // Linux IP_PMTUDISC_DO (2) and _PROBE (3) set DF; DONT (0) and WANT (1)
        // leave it clear, which is FreeBSD's default behaviour too.
        if (!val || len < sizeof(int))
            FAIL_LX(LX_EINVAL);
        int v = *(const int*)val;
        int df = (v == 2 || v == 3) ? 1 : 0;
        RET_TRANSLATED(setsockopt(fd, m->fb_level, m->fb_opt, &df, sizeof df));
    }
    RET_TRANSLATED(setsockopt(fd, m->fb_level, m->fb_opt, val, len));
}

int cordial_fbsd_getsockopt(int fd, int level, int opt, void* val, socklen_t* len) {
    const struct optmap* m = find_opt(level, opt);
    if (!m)
        FAIL_LX(LX_ENOPROTOOPT);
    int r = getsockopt(fd, m->fb_level, m->fb_opt, val, len);
    if (r < 0) {
        cordial_fbsd_errno_to_linux();
        return r;
    }
    if (val && len && *len >= sizeof(int)) {
        int* v = (int*)val;
        switch (m->value) {
        // SO_ERROR's value *is* an errno: the pending error of a non-blocking
        // connect. Left alone, a refused connection reports 61, which Linux
        // calls ENODATA.
        case V_ERRNO: *v = errno_fb_to_lx(*v); break;
        case V_AF: *v = cordial_fbsd_af_to_linux(*v); break;
        case V_PMTU: *v = *v ? 2 : 0; break;
        default: break;
        }
    }
    return r;
}

// ── MSG_* flags ─────────────────────────────────────────────────────────────
// OOB 1, PEEK 2, DONTROUTE 4 agree. The rest:
//   Linux CTRUNC 0x8, TRUNC 0x20, DONTWAIT 0x40, EOR 0x80, WAITALL 0x100,
//   NOSIGNAL 0x4000, WAITFORONE 0x10000, CMSG_CLOEXEC 0x40000000
//   FreeBSD CTRUNC 0x20, TRUNC 0x10, DONTWAIT 0x80, EOR 0x8, WAITALL 0x40,
//   NOSIGNAL 0x20000, WAITFORONE 0x80000, CMSG_CLOEXEC 0x40000
// Linux MSG_DONTWAIT (0x40) is FreeBSD MSG_WAITALL: a recv the engine asked not
// to block would instead block until the buffer filled.
#define LX_MSG_OOB          0x1
#define LX_MSG_PEEK         0x2
#define LX_MSG_DONTROUTE    0x4
#define LX_MSG_CTRUNC       0x8
#define LX_MSG_TRUNC        0x20
#define LX_MSG_DONTWAIT     0x40
#define LX_MSG_EOR          0x80
#define LX_MSG_WAITALL      0x100
#define LX_MSG_CONFIRM      0x800
#define LX_MSG_NOSIGNAL     0x4000
#define LX_MSG_MORE         0x8000
#define LX_MSG_WAITFORONE   0x10000
#define LX_MSG_CMSG_CLOEXEC 0x40000000

static const struct { int lx, fb; } k_msg[] = {
    {LX_MSG_OOB, MSG_OOB},           {LX_MSG_PEEK, MSG_PEEK},
    {LX_MSG_DONTROUTE, MSG_DONTROUTE}, {LX_MSG_CTRUNC, MSG_CTRUNC},
    {LX_MSG_TRUNC, MSG_TRUNC},       {LX_MSG_DONTWAIT, MSG_DONTWAIT},
    {LX_MSG_EOR, MSG_EOR},           {LX_MSG_WAITALL, MSG_WAITALL},
    {LX_MSG_NOSIGNAL, MSG_NOSIGNAL}, {LX_MSG_WAITFORONE, MSG_WAITFORONE},
    {LX_MSG_CMSG_CLOEXEC, MSG_CMSG_CLOEXEC},
};

static int msg_flags_to_fb(int lx, int* fb) {
    *fb = 0;
    // MSG_MORE and MSG_CONFIRM are performance hints (coalesce, skip an ARP
    // probe). Dropping them changes timing, never what is sent or received.
    lx &= ~(LX_MSG_MORE | LX_MSG_CONFIRM);
    for (size_t i = 0; i < sizeof k_msg / sizeof k_msg[0]; i++)
        if (lx & k_msg[i].lx) {
            *fb |= k_msg[i].fb;
            lx &= ~k_msg[i].lx;
        }
    // MSG_ERRQUEUE, MSG_FASTOPEN, MSG_ZEROCOPY: Linux-only machinery.
    if (lx)
        FAIL_LX(LX_EOPNOTSUPP);
    return 0;
}

static int msg_flags_to_lx(int fb) {
    int lx = 0;
    for (size_t i = 0; i < sizeof k_msg / sizeof k_msg[0]; i++)
        if (fb & k_msg[i].fb)
            lx |= k_msg[i].lx;
    return lx;
}

ssize_t cordial_fbsd_sendto(int fd, const void* buf, size_t n, int lxflags, const void* to,
                            socklen_t tolen) {
    int fl;
    if (msg_flags_to_fb(lxflags, &fl) != 0)
        return -1;
    struct sockaddr_storage ss;
    socklen_t sl = 0;
    if (to && sa_from_linux(to, tolen, &ss, &sl) != 0)
        return -1;
    RET_TRANSLATED(sendto(fd, buf, n, fl, to ? (struct sockaddr*)&ss : NULL, sl));
}

ssize_t cordial_fbsd___sendto_chk(int fd, const void* buf, size_t n, size_t blen, int fl,
                                  const void* to, socklen_t tolen) {
    (void)blen;
    return cordial_fbsd_sendto(fd, buf, n, fl, to, tolen);
}

ssize_t cordial_fbsd_send(int fd, const void* buf, size_t n, int lxflags) {
    return cordial_fbsd_sendto(fd, buf, n, lxflags, NULL, 0);
}

ssize_t cordial_fbsd_recvfrom(int fd, void* buf, size_t n, int lxflags, void* from,
                              socklen_t* fromlen) {
    int fl;
    if (msg_flags_to_fb(lxflags, &fl) != 0)
        return -1;
    struct sockaddr_storage ss;
    socklen_t sl = sizeof ss;
    ssize_t r = recvfrom(fd, buf, n, fl, from ? (struct sockaddr*)&ss : NULL,
                         from ? &sl : NULL);
    if (r < 0) {
        cordial_fbsd_errno_to_linux();
        return r;
    }
    if (from)
        sa_out(&ss, sl, from, fromlen);
    return r;
}

ssize_t cordial_fbsd_recv(int fd, void* buf, size_t n, int lxflags) {
    return cordial_fbsd_recvfrom(fd, buf, n, lxflags, NULL, NULL);
}

// ── sendmsg / recvmsg ───────────────────────────────────────────────────────
//
// struct msghdr is not the same size. Linux (bionic, LP64): name@0 namelen@8
// iov@16 iovlen(size_t)@24 control@32 controllen(size_t)@40 flags@48, 56 bytes.
// FreeBSD: iovlen is an int@24, controllen a socklen_t@40, flags@44, 48 bytes.
// A FreeBSD recvmsg writes msg_flags into the top half of the engine's
// msg_controllen and never touches the engine's msg_flags at all.
//
// struct cmsghdr differs too. Linux {size_t len; int level; int type} is 16
// bytes; FreeBSD {socklen_t len; int level; int type} is 12, padded to 16 before
// the data. CMSG_LEN and CMSG_SPACE therefore agree (16 + n, 16 + align8(n)), so
// a control buffer converts entry for entry at the same offsets and size.
struct lx_msghdr {
    void* msg_name;
    socklen_t msg_namelen;
    struct iovec* msg_iov;
    size_t msg_iovlen;
    void* msg_control;
    size_t msg_controllen;
    int msg_flags;
};

struct lx_mmsghdr {
    struct lx_msghdr msg_hdr;
    unsigned int msg_len;
};

struct lx_cmsghdr {
    size_t cmsg_len;
    int cmsg_level;
    int cmsg_type;
};

#define CM_HDR 16
#define CM_ALIGN(n) (((n) + 7) & ~(size_t)7)

// Control-message types, (Linux level, Linux type) <-> (FreeBSD level, type).
// SCM_RIGHTS is 1 on both; FreeBSD delivers SO_TIMESTAMP as SCM_TIMESTAMP (2)
// where Linux uses SO_TIMESTAMP (29) as the type; IP_RECVTOS delivers IP_TOS on
// Linux and IP_RECVTOS on FreeBSD; the IPv6 ones are renumbered.
static const struct { int lx_level, lx_type, fb_level, fb_type; } k_cmsg[] = {
    {1, 1, SOL_SOCKET, SCM_RIGHTS},
    {1, 29, SOL_SOCKET, SCM_TIMESTAMP},
    {0, 1, IPPROTO_IP, IP_RECVTOS},
    {41, 50, IPPROTO_IPV6, IPV6_PKTINFO},
    {41, 52, IPPROTO_IPV6, IPV6_HOPLIMIT},
    {41, 67, IPPROTO_IPV6, IPV6_TCLASS},
};

// Walk one control buffer into another of the same size. to_fb chooses the
// direction. Returns 0, or -1 (errno set) when sending something untranslatable;
// on receive an unknown entry is dropped and *truncated set, because the kernel
// has already consumed the datagram and failing now would lose it.
static int convert_cmsgs(const unsigned char* src, size_t srclen, unsigned char* dst,
                         size_t* dstlen, int to_fb, int* truncated) {
    size_t si = 0, di = 0;
    while (si + CM_HDR <= srclen) {
        size_t len;
        int level, type;
        if (to_fb) {
            const struct lx_cmsghdr* h = (const struct lx_cmsghdr*)(src + si);
            len = h->cmsg_len;
            level = h->cmsg_level;
            type = h->cmsg_type;
        } else {
            const struct cmsghdr* h = (const struct cmsghdr*)(src + si);
            len = h->cmsg_len;
            level = h->cmsg_level;
            type = h->cmsg_type;
        }
        if (len < CM_HDR || si + len > srclen)
            break;
        int ol = -1, ot = -1;
        for (size_t i = 0; i < sizeof k_cmsg / sizeof k_cmsg[0]; i++) {
            if (to_fb && k_cmsg[i].lx_level == level && k_cmsg[i].lx_type == type) {
                ol = k_cmsg[i].fb_level;
                ot = k_cmsg[i].fb_type;
            } else if (!to_fb && k_cmsg[i].fb_level == level && k_cmsg[i].fb_type == type) {
                ol = k_cmsg[i].lx_level;
                ot = k_cmsg[i].lx_type;
            }
        }
        if (ol < 0) {
            if (to_fb)
                FAIL_LX(LX_EINVAL);
            *truncated = 1;
        } else {
            if (to_fb) {
                struct cmsghdr* h = (struct cmsghdr*)(dst + di);
                h->cmsg_len = (socklen_t)len;
                h->cmsg_level = ol;
                h->cmsg_type = ot;
            } else {
                struct lx_cmsghdr* h = (struct lx_cmsghdr*)(dst + di);
                h->cmsg_len = len;
                h->cmsg_level = ol;
                h->cmsg_type = ot;
            }
            memcpy(dst + di + CM_HDR, src + si + CM_HDR, len - CM_HDR);
            di += CM_ALIGN(len);
        }
        si += CM_ALIGN(len);
    }
    *dstlen = di > *dstlen ? *dstlen : di;
    return 0;
}

ssize_t cordial_fbsd_sendmsg(int fd, const struct lx_msghdr* m, int lxflags) {
    int fl;
    if (msg_flags_to_fb(lxflags, &fl) != 0)
        return -1;
    if (m->msg_iovlen > INT_MAX)
        FAIL_LX(90 /* EMSGSIZE */);
    struct msghdr h;
    memset(&h, 0, sizeof h);
    struct sockaddr_storage ss;
    if (m->msg_name && m->msg_namelen) {
        socklen_t sl;
        if (sa_from_linux(m->msg_name, m->msg_namelen, &ss, &sl) != 0)
            return -1;
        h.msg_name = &ss;
        h.msg_namelen = sl;
    }
    h.msg_iov = m->msg_iov; // struct iovec agrees
    h.msg_iovlen = (int)m->msg_iovlen;
    unsigned char* ctl = NULL;
    if (m->msg_control && m->msg_controllen) {
        ctl = calloc(1, m->msg_controllen);
        if (!ctl)
            FAIL_LX(12 /* ENOMEM */);
        size_t cl = m->msg_controllen;
        int tr = 0;
        if (convert_cmsgs(m->msg_control, m->msg_controllen, ctl, &cl, 1, &tr) != 0) {
            free(ctl);
            return -1;
        }
        h.msg_control = ctl;
        h.msg_controllen = (socklen_t)cl;
    }
    ssize_t r = sendmsg(fd, &h, fl);
    if (r < 0)
        cordial_fbsd_errno_to_linux();
    free(ctl);
    return r;
}

ssize_t cordial_fbsd_recvmsg(int fd, struct lx_msghdr* m, int lxflags) {
    int fl;
    if (msg_flags_to_fb(lxflags, &fl) != 0)
        return -1;
    if (m->msg_iovlen > INT_MAX)
        FAIL_LX(90 /* EMSGSIZE */);
    struct msghdr h;
    memset(&h, 0, sizeof h);
    struct sockaddr_storage ss;
    if (m->msg_name) {
        h.msg_name = &ss;
        h.msg_namelen = sizeof ss;
    }
    h.msg_iov = m->msg_iov;
    h.msg_iovlen = (int)m->msg_iovlen;
    unsigned char* ctl = NULL;
    if (m->msg_control && m->msg_controllen) {
        ctl = calloc(1, m->msg_controllen);
        if (!ctl)
            FAIL_LX(12 /* ENOMEM */);
        h.msg_control = ctl;
        h.msg_controllen = (socklen_t)m->msg_controllen;
    }
    ssize_t r = recvmsg(fd, &h, fl);
    if (r < 0) {
        cordial_fbsd_errno_to_linux();
        free(ctl);
        return r;
    }
    if (m->msg_name) {
        socklen_t nl = m->msg_namelen;
        sa_out(&ss, h.msg_namelen, m->msg_name, &nl);
        m->msg_namelen = nl;
    }
    int truncated = 0;
    if (ctl) {
        size_t cl = m->msg_controllen;
        convert_cmsgs(ctl, h.msg_controllen, m->msg_control, &cl, 0, &truncated);
        m->msg_controllen = cl;
        free(ctl);
    } else {
        m->msg_controllen = 0;
    }
    m->msg_flags = msg_flags_to_lx(h.msg_flags) | (truncated ? LX_MSG_CTRUNC : 0);
    return r;
}

// sendmmsg/recvmmsg. The arrays are not even the same stride (Linux mmsghdr is
// 64 bytes, FreeBSD's 56), so these are built on the single-message calls above:
// the same loop the kernel runs, one message at a time.
int cordial_fbsd_sendmmsg(int fd, struct lx_mmsghdr* v, unsigned int n, int lxflags) {
    unsigned int i;
    for (i = 0; i < n; i++) {
        ssize_t r = cordial_fbsd_sendmsg(fd, &v[i].msg_hdr, lxflags);
        if (r < 0)
            return i ? (int)i : -1; // Linux reports the partial count
        v[i].msg_len = (unsigned int)r;
    }
    return (int)i;
}

int cordial_fbsd_recvmmsg(int fd, struct lx_mmsghdr* v, unsigned int n, int lxflags,
                          struct timespec* timeout) {
    struct timespec deadline;
    if (timeout) {
        clock_gettime(CLOCK_MONOTONIC, &deadline);
        deadline.tv_sec += timeout->tv_sec;
        deadline.tv_nsec += timeout->tv_nsec;
        if (deadline.tv_nsec >= 1000000000L) {
            deadline.tv_sec++;
            deadline.tv_nsec -= 1000000000L;
        }
    }
    unsigned int i;
    for (i = 0; i < n; i++) {
        int fl = lxflags & ~LX_MSG_WAITFORONE;
        if (i > 0 && (lxflags & LX_MSG_WAITFORONE))
            fl |= LX_MSG_DONTWAIT;
        ssize_t r = cordial_fbsd_recvmsg(fd, &v[i].msg_hdr, fl);
        if (r < 0)
            return i ? (int)i : -1;
        v[i].msg_len = (unsigned int)r;
        if (timeout) {
            // Like Linux, the timeout is only checked between datagrams.
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            if (now.tv_sec > deadline.tv_sec ||
                (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
                i++;
                break;
            }
        }
    }
    return (int)i;
}

// ── poll ────────────────────────────────────────────────────────────────────
// 0x1..0x80 agree. Linux POLLWRNORM 0x100 / POLLWRBAND 0x200 / POLLRDHUP 0x2000
// are FreeBSD 0x4 (== POLLOUT) / 0x100 / 0x4000. Linux itself ignores event bits
// it does not know, so unknown bits are dropped rather than refused here.
#define LX_POLLWRNORM 0x100
#define LX_POLLWRBAND 0x200
#define LX_POLLRDHUP  0x2000

static short poll_to_fb(short lx) {
    short fb = lx & 0xff;
    if (lx & LX_POLLWRNORM) fb |= POLLWRNORM;
    if (lx & LX_POLLWRBAND) fb |= POLLWRBAND;
    if (lx & LX_POLLRDHUP) fb |= POLLRDHUP;
    return fb;
}

static short poll_to_lx(short fb, short asked) {
    // FreeBSD POLLWRNORM *is* POLLOUT, so a writable descriptor comes back as
    // 0x4 whichever was asked; report it under the spelling(s) the caller used.
    short lx = fb & 0xff & ~POLLOUT;
    if (fb & POLLOUT) {
        if (asked & POLLOUT) lx |= POLLOUT;
        if (asked & LX_POLLWRNORM) lx |= LX_POLLWRNORM;
    }
    if (fb & POLLWRBAND) lx |= LX_POLLWRBAND;
    if (fb & POLLRDHUP) lx |= LX_POLLRDHUP;
    return lx;
}

int cordial_fbsd_poll(struct pollfd* fds, nfds_t n, int timeout) {
    int odd = 0;
    for (nfds_t i = 0; i < n; i++)
        if (fds[i].events & ~0xff)
            odd = 1;
    // The common case asks only for bits that agree, and FreeBSD reports only
    // what was asked plus ERR/HUP/NVAL, which agree too. No copy needed.
    if (!odd)
        RET_TRANSLATED(poll(fds, n, timeout));
    // Otherwise translate in place and put the caller's request back exactly
    // afterwards: event loops re-poll the same array without rebuilding it.
    short stack[64];
    short* asked = n <= 64 ? stack : malloc(n * sizeof(short));
    if (!asked)
        FAIL_LX(12 /* ENOMEM */);
    for (nfds_t i = 0; i < n; i++) {
        asked[i] = fds[i].events;
        fds[i].events = poll_to_fb(asked[i]);
    }
    int r = poll(fds, n, timeout);
    int saved = errno;
    for (nfds_t i = 0; i < n; i++) {
        fds[i].revents = poll_to_lx(fds[i].revents, asked[i]);
        fds[i].events = asked[i];
    }
    if (asked != stack)
        free(asked);
    errno = saved;
    if (r < 0)
        cordial_fbsd_errno_to_linux();
    return r;
}

int cordial_fbsd___poll_chk(struct pollfd* fds, nfds_t n, int timeout, size_t fds_len) {
    (void)fds_len;
    return cordial_fbsd_poll(fds, n, timeout);
}

// ── ioctl ───────────────────────────────────────────────────────────────────
// Linux request numbers are a different encoding altogether (FIONBIO 0x5421
// against FreeBSD's _IOW('f', 126, int) = 0x8004667e). Translate the ones a
// socket or tty user reaches for; answer ENOTTY for the rest, which is what
// either kernel says to a request the file does not support -- rather than let
// a Linux number land on whatever FreeBSD request happens to share it.
// bionic's pthread_condattr_t is a `long`, and nothing on this port owned
// it: pthread_condattr_init/destroy were generated stubs (they print
// `[stub] pthread_condattr_init`), so the object was never initialised, and
// setclock went to the host with a Linux clock id (1 = MONOTONIC on Linux,
// CLOCK_VIRTUAL on FreeBSD). The attribute is now just the FreeBSD clock id
// the condvar should use, read back by make_cond in bionic/pthread.rs.
// Handing the uninitialised object to the host's pthread_cond_init instead
// crashed a game join outright, which is how the stubs were found.
static int cordial_fbsd_condattr_init(long* a) {
    if (!a) return 22;
    *a = CLOCK_REALTIME; // bionic's default, and what libc++ deadlines assume
    return 0;
}
static int cordial_fbsd_condattr_destroy(long* a) {
    (void)a;
    return 0;
}
static int cordial_fbsd_condattr_setclock(long* a, int lx) {
    if (!a) return 22;
    switch (lx) {
    case 0: case 5: *a = CLOCK_REALTIME; return 0;           // REALTIME(_COARSE)
    case 1: case 4: case 6: case 7: *a = CLOCK_MONOTONIC; return 0;
    default: return 22; // Linux EINVAL, returned as pthread functions do
    }
}
static int cordial_fbsd_condattr_getclock(const long* a, int* lx) {
    if (!a || !lx) return 22;
    *lx = *a == CLOCK_MONOTONIC ? 1 : 0;
    return 0;
}

// Linux SIOCGIFCONF, answered from getifaddrs. See the call site in
// cordial_fbsd_ioctl for why FreeBSD's own ioctl cannot be passed through.
static int cordial_fbsd_siocgifconf(void* arg) {
    enum { LX_IFREQ = 40, LX_IFNAMSIZ = 16 };
    struct lx_ifconf { int len; int pad; char* buf; };
    struct lx_ifconf* ifc = (struct lx_ifconf*)arg;
    if (!ifc) FAIL_LX(LX_EINVAL);
    struct ifaddrs* all = NULL;
    if (getifaddrs(&all) != 0) {
        cordial_fbsd_errno_to_linux();
        return -1;
    }
    int n = 0, cap = ifc->buf ? ifc->len / LX_IFREQ : 0;
    for (struct ifaddrs* a = all; a; a = a->ifa_next) {
        if (!a->ifa_addr || a->ifa_addr->sa_family != AF_INET) continue;
        if (ifc->buf) {
            if (n >= cap) break;
            unsigned char* r = (unsigned char*)ifc->buf + (size_t)n * LX_IFREQ;
            memset(r, 0, LX_IFREQ);
            strncpy((char*)r, a->ifa_name, LX_IFNAMSIZ - 1);
            const struct sockaddr_in* in = (const struct sockaddr_in*)a->ifa_addr;
            uint16_t fam = 2; // Linux AF_INET, host order, no sa_len
            memcpy(r + 16, &fam, 2);
            memcpy(r + 18, &in->sin_port, 2);
            memcpy(r + 20, &in->sin_addr, 4);
        }
        n++;
    }
    freeifaddrs(all);
    ifc->len = n * LX_IFREQ;
    return 0;
}

// Linux per-interface queries (SIOCGIFADDR/NETMASK/BRDADDR/FLAGS) on a
// 40-byte struct ifreq named by its first 16 bytes. RakNet asks for each
// address after SIOCGIFCONF; refused, a join sat at stage UGCGame presenting
// nothing. Answered from getifaddrs for AF_INET, in Linux layout.
static int cordial_fbsd_siocgif(unsigned req, void* arg) {
    unsigned char* r = (unsigned char*)arg;
    if (!r) FAIL_LX(LX_EINVAL);
    char name[17] = {0};
    memcpy(name, r, 16);
    struct ifaddrs* all = NULL;
    if (getifaddrs(&all) != 0) {
        cordial_fbsd_errno_to_linux();
        return -1;
    }
    int found = 0;
    for (struct ifaddrs* a = all; a && !found; a = a->ifa_next) {
        if (strcmp(a->ifa_name, name) != 0) continue;
        if (req == 0x8913) { // SIOCGIFFLAGS: low bits agree; MULTICAST differs
            unsigned fl = a->ifa_flags, lx = fl & 0x7ff;
            if (fl & IFF_MULTICAST) lx |= 0x1000;
            short v = (short)lx;
            memcpy(r + 16, &v, 2);
            found = 1;
            break;
        }
        if (!a->ifa_addr || a->ifa_addr->sa_family != AF_INET) continue;
        const struct sockaddr* src = req == 0x8915 ? a->ifa_addr
                                   : req == 0x891b ? a->ifa_netmask
                                   : a->ifa_broadaddr;
        memset(r + 16, 0, 24);
        uint16_t fam = 2;
        memcpy(r + 16, &fam, 2);
        if (src) memcpy(r + 20, &((const struct sockaddr_in*)src)->sin_addr, 4);
        found = 1;
    }
    freeifaddrs(all);
    if (!found) FAIL_LX(19); // Linux ENODEV
    return 0;
}

int cordial_fbsd_ioctl(int fd, int req, ...) {
    va_list ap;
    va_start(ap, req);
    void* arg = va_arg(ap, void*);
    va_end(ap);
    unsigned long fb;
    switch ((unsigned)req) {
    case 0x5421: fb = FIONBIO; break;
    case 0x541B: fb = FIONREAD; break;
    case 0x5451: fb = FIOCLEX; break;
    case 0x5450: fb = FIONCLEX; break;
    case 0x5452: fb = FIOASYNC; break;
    case 0x8905: fb = SIOCATMARK; break;
    case 0x5413: fb = TIOCGWINSZ; break; // struct winsize agrees
    case 0x8912: // SIOCGIFCONF
        // RakNet lists the host's IPv4 addresses with this before it binds its
        // UDP socket. Refused with ENOTTY, a game join logged `binding socket
        // on inaddr_any:0` and never created a socket at all. FreeBSD's own
        // SIOCGIFCONF returns variable-length records with sa_len, so the
        // answer is built from getifaddrs in Linux's fixed layout instead:
        // struct ifconf { int len; void* buf } (buf at offset 8), and 40-byte
        // struct ifreq { char name[16]; sockaddr_in addr; pad }, with a
        // two-byte family and no sa_len.
        return cordial_fbsd_siocgifconf(arg);
    case 0x8913: case 0x8915: case 0x8919: case 0x891b:
        return cordial_fbsd_siocgif((unsigned)req, arg);
    default:
        if (getenv("CORDIAL_TRACE_ABI"))
            fprintf(stderr, "[abi] ioctl request %#x on fd %d has no translation\n", (unsigned)req, fd);
        FAIL_LX(LX_ENOTTY);
    }
    RET_TRANSLATED(ioctl(fd, fb, arg));
}

// ── calls whose only divergence is errno ────────────────────────────────────
ssize_t cordial_fbsd_read(int fd, void* b, size_t n) { RET_TRANSLATED(read(fd, b, n)); }
ssize_t cordial_fbsd_write(int fd, const void* b, size_t n) { RET_TRANSLATED(write(fd, b, n)); }
ssize_t cordial_fbsd___read_chk(int fd, void* b, size_t n, size_t blen) {
    (void)blen;
    RET_TRANSLATED(read(fd, b, n));
}
ssize_t cordial_fbsd___write_chk(int fd, const void* b, size_t n, size_t blen) {
    (void)blen;
    RET_TRANSLATED(write(fd, b, n));
}
ssize_t cordial_fbsd_readv(int fd, const struct iovec* v, int n) { RET_TRANSLATED(readv(fd, v, n)); }
ssize_t cordial_fbsd_writev(int fd, const struct iovec* v, int n) { RET_TRANSLATED(writev(fd, v, n)); }
ssize_t cordial_fbsd_pread(int fd, void* b, size_t n, off_t o) { RET_TRANSLATED(pread(fd, b, n, o)); }
ssize_t cordial_fbsd_pwrite(int fd, const void* b, size_t n, off_t o) {
    RET_TRANSLATED(pwrite(fd, b, n, o));
}
int cordial_fbsd_close(int fd) { RET_TRANSLATED(close(fd)); }
int cordial_fbsd_listen(int fd, int backlog) { RET_TRANSLATED(listen(fd, backlog)); }
int cordial_fbsd_shutdown(int fd, int how) { RET_TRANSLATED(shutdown(fd, how)); } // SHUT_* agree
int cordial_fbsd_dup2(int a, int b) { RET_TRANSLATED(dup2(a, b)); }
off_t cordial_fbsd_lseek(int fd, off_t o, int w) { RET_TRANSLATED(lseek(fd, o, w)); } // SEEK_* agree
// fd_set is 1024 bits of unsigned long on both; struct timeval agrees.
int cordial_fbsd_select(int n, fd_set* r, fd_set* w, fd_set* e, struct timeval* t) {
    RET_TRANSLATED(select(n, r, w, e, t));
}

// ── address-family arguments outside the socket calls ───────────────────────
int cordial_fbsd_inet_pton(int af, const char* src, void* dst) {
    int fb = cordial_fbsd_af_from_linux(af);
    if (fb != AF_INET && fb != AF_INET6)
        FAIL_LX(LX_EAFNOSUPPORT);
    RET_TRANSLATED(inet_pton(fb, src, dst));
}

const char* cordial_fbsd_inet_ntop(int af, const void* src, char* dst, socklen_t len) {
    int fb = cordial_fbsd_af_from_linux(af);
    if (fb != AF_INET && fb != AF_INET6) {
        set_lx_errno(LX_EAFNOSUPPORT);
        return NULL;
    }
    const char* r = inet_ntop(fb, src, dst, len);
    if (!r)
        cordial_fbsd_errno_to_linux();
    return r;
}

// NI_* and EAI_* are the BSD numbers in bionic too (checked: netdb.h in both),
// so only the sockaddr needs translating.
int cordial_fbsd_getnameinfo(const void* sa, socklen_t salen, char* host, socklen_t hostlen,
                             char* serv, socklen_t servlen, int flags) {
    struct sockaddr_storage ss;
    socklen_t sl;
    if (sa_from_linux(sa, salen, &ss, &sl) != 0)
        return EAI_FAMILY;
    int r = getnameinfo((struct sockaddr*)&ss, sl, host, hostlen, serv, servlen, flags);
    if (r == EAI_SYSTEM)
        cordial_fbsd_errno_to_linux();
    return r;
}

// ── strerror ────────────────────────────────────────────────────────────────
// The engine hands these a Linux errno. FreeBSD would describe Linux EAGAIN (11)
// as "Resource deadlock avoided".
char* cordial_fbsd_strerror(int lx) { return strerror(errno_lx_to_fb(lx)); }

int cordial_fbsd_strerror_r(int lx, char* buf, size_t n) {
    int r = strerror_r(errno_lx_to_fb(lx), buf, n);
    return r ? errno_fb_to_lx(r) : 0;
}

char* cordial_fbsd___gnu_strerror_r(int lx, char* buf, size_t n) {
    strerror_r(errno_lx_to_fb(lx), buf, n);
    return buf;
}

// ── registration ────────────────────────────────────────────────────────────
struct CordialFbsdAbiSymbol {
    const char* name;
    void* addr;
};

const struct CordialFbsdAbiSymbol* cordial_fbsd_abi_symbols(size_t* count) {
    static const struct CordialFbsdAbiSymbol table[] = {
        {"__errno", (void*)&cordial_fbsd_bionic_errno},
        {"pthread_condattr_init", (void*)&cordial_fbsd_condattr_init},
        {"pthread_condattr_destroy", (void*)&cordial_fbsd_condattr_destroy},
        {"pthread_condattr_setclock", (void*)&cordial_fbsd_condattr_setclock},
        {"pthread_condattr_getclock", (void*)&cordial_fbsd_condattr_getclock},
        {"__open_2", (void*)&cordial_fbsd___open_2},
        {"openat", (void*)&cordial_fbsd_openat},
        {"pipe2", (void*)&cordial_fbsd_pipe2},
        {"fcntl", (void*)&cordial_fbsd_fcntl},
        {"socket", (void*)&cordial_fbsd_socket},
        {"socketpair", (void*)&cordial_fbsd_socketpair},
        {"eventfd", (void*)&cordial_fbsd_eventfd},
        {"bind", (void*)&cordial_fbsd_bind},
        {"connect", (void*)&cordial_fbsd_connect},
        {"accept", (void*)&cordial_fbsd_accept},
        {"accept4", (void*)&cordial_fbsd_accept4},
        {"getsockname", (void*)&cordial_fbsd_getsockname},
        {"getpeername", (void*)&cordial_fbsd_getpeername},
        {"setsockopt", (void*)&cordial_fbsd_setsockopt},
        {"getsockopt", (void*)&cordial_fbsd_getsockopt},
        {"send", (void*)&cordial_fbsd_send},
        {"sendto", (void*)&cordial_fbsd_sendto},
        {"__sendto_chk", (void*)&cordial_fbsd___sendto_chk},
        {"recv", (void*)&cordial_fbsd_recv},
        {"recvfrom", (void*)&cordial_fbsd_recvfrom},
        {"sendmsg", (void*)&cordial_fbsd_sendmsg},
        {"recvmsg", (void*)&cordial_fbsd_recvmsg},
        {"sendmmsg", (void*)&cordial_fbsd_sendmmsg},
        {"recvmmsg", (void*)&cordial_fbsd_recvmmsg},
        {"poll", (void*)&cordial_fbsd_poll},
        {"__poll_chk", (void*)&cordial_fbsd___poll_chk},
        {"ioctl", (void*)&cordial_fbsd_ioctl},
        {"read", (void*)&cordial_fbsd_read},
        {"write", (void*)&cordial_fbsd_write},
        {"__read_chk", (void*)&cordial_fbsd___read_chk},
        {"__write_chk", (void*)&cordial_fbsd___write_chk},
        {"readv", (void*)&cordial_fbsd_readv},
        {"writev", (void*)&cordial_fbsd_writev},
        {"pread", (void*)&cordial_fbsd_pread},
        {"pread64", (void*)&cordial_fbsd_pread},
        {"pwrite", (void*)&cordial_fbsd_pwrite},
        {"pwrite64", (void*)&cordial_fbsd_pwrite},
        {"close", (void*)&cordial_fbsd_close},
        {"listen", (void*)&cordial_fbsd_listen},
        {"shutdown", (void*)&cordial_fbsd_shutdown},
        {"dup2", (void*)&cordial_fbsd_dup2},
        {"lseek", (void*)&cordial_fbsd_lseek},
        {"select", (void*)&cordial_fbsd_select},
        {"inet_pton", (void*)&cordial_fbsd_inet_pton},
        {"inet_ntop", (void*)&cordial_fbsd_inet_ntop},
        {"getnameinfo", (void*)&cordial_fbsd_getnameinfo},
        {"strerror", (void*)&cordial_fbsd_strerror},
        {"strerror_r", (void*)&cordial_fbsd_strerror_r},
        {"__gnu_strerror_r", (void*)&cordial_fbsd___gnu_strerror_r},
    };
    *count = sizeof table / sizeof table[0];
    return table;
}

#endif /* __FreeBSD__ */
