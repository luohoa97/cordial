// freebsd_libc_compat.c — definitions for the handful of glibc/bionic libc
// symbols the AOSP linker and shim reference that FreeBSD's libc spells
// differently or does not provide. Whole file compiles to nothing off FreeBSD.
#if defined(__FreeBSD__)

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <pthread_np.h>
#include <stdarg.h>
#include <stdlib.h>
#include <sys/auxv.h>
#include <sys/types.h>

// glibc's locale-aware MB_CUR_MAX accessor. bionic exports it too; FreeBSD does
// not, but its MB_CUR_MAX macro yields the same value for the current locale.
size_t __ctype_get_mb_cur_max(void) { return MB_CUR_MAX; }

// ── bionic pthread_mutex via a pointer-keyed side-table ─────────────────────
// bionic's pthread_mutex_t (a few bytes) and FreeBSD's (a pointer to an opaque
// struct) have incompatible layouts, so we cannot forward the bionic object to
// FreeBSD's pthread. Instead, key a real FreeBSD (recursive) mutex off the
// bionic object's *address*. The engine only ever touches the bionic mutex
// through these calls, so its bytes are never interpreted — the address is a
// stable identity. Recursive avoids self-deadlock from bionic type differences.
#define BM_SLOTS 8192
struct bm_entry {
    const void *key;
    pthread_mutex_t real;
    struct bm_entry *next;
};
static struct bm_entry *bm_tab[BM_SLOTS];
static pthread_mutex_t bm_guard = PTHREAD_MUTEX_INITIALIZER;

static struct bm_entry *bm_lookup(const void *key) {
    unsigned h = (unsigned)(((uintptr_t)key >> 4) & (BM_SLOTS - 1));
    // The list is append-only (entries are never removed or reordered), so an
    // existing key is found lock-free — critical because the flag parser locks
    // mutexes tens of thousands of times and a global guard here would serialise
    // all of it and lose the flag-load race against the main thread.
    for (struct bm_entry *e = __atomic_load_n(&bm_tab[h], __ATOMIC_ACQUIRE); e;
         e = e->next)
        if (e->key == key)
            return e;
    // Miss: take the guard, re-check (someone may have added it), then append.
    pthread_mutex_lock(&bm_guard);
    for (struct bm_entry *e = bm_tab[h]; e; e = e->next)
        if (e->key == key) {
            pthread_mutex_unlock(&bm_guard);
            return e;
        }
    struct bm_entry *e = calloc(1, sizeof *e);
    pthread_mutexattr_t a;
    pthread_mutexattr_init(&a);
    pthread_mutexattr_settype(&a, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_init(&e->real, &a);
    pthread_mutexattr_destroy(&a);
    e->key = key;
    e->next = bm_tab[h];
    __atomic_store_n(&bm_tab[h], e, __ATOMIC_RELEASE);
    pthread_mutex_unlock(&bm_guard);
    return e;
}

// Create a cond whose clock is CLOCK_MONOTONIC. bionic's conds use monotonic
// deadlines for pthread_cond_timedwait; FreeBSD's default is CLOCK_REALTIME, so
// a monotonic absolute deadline (~seconds since boot) reads as long past →
// timedwait returns ETIMEDOUT instantly and every wait_for loop busy-spins.
int bionic_cond_init_monotonic(void *cond) {
    pthread_condattr_t a;
    pthread_condattr_init(&a);
    pthread_condattr_setclock(&a, 4 /* CLOCK_MONOTONIC on FreeBSD */);
    int r = pthread_cond_init((pthread_cond_t *)cond, &a);
    pthread_condattr_destroy(&a);
    return r;
}

// The real FreeBSD mutex backing a bionic mutex object — so pthread_cond_wait,
// which must operate on the same lock the engine's mutex_lock/unlock use, gets
// the side-table entry rather than the (uninterpreted) bionic bytes.
pthread_mutex_t *bionic_mutex_real(void *m) { return &bm_lookup(m)->real; }

int bionic_pthread_mutex_init(void *m, const void *attr) { (void)attr; bm_lookup(m); return 0; }
int bionic_pthread_mutex_lock(void *m) { return pthread_mutex_lock(&bm_lookup(m)->real); }
int bionic_pthread_mutex_unlock(void *m) { return pthread_mutex_unlock(&bm_lookup(m)->real); }
int bionic_pthread_mutex_trylock(void *m) { return pthread_mutex_trylock(&bm_lookup(m)->real); }
int bionic_pthread_mutex_destroy(void *m) { (void)m; return 0; }

// FORTIFY open with no mode arg, and a registered prctl (unsupported).
int __open_2(const char *path, int flags) { return open(path, flags); }

// mmap(2) flag translation. The engine passes *Linux* MAP_* flag numbers, but
// this reaches FreeBSD's mmap, which numbers them differently — most fatally
// Linux MAP_ANONYMOUS=0x20 vs FreeBSD MAP_ANON=0x1000, and Linux
// MAP_NORESERVE=0x4000 which is FreeBSD's MAP_EXCL. Untranslated, every
// anonymous allocation fails EINVAL and the engine's allocator gets no memory.
#include <sys/mman.h>
#define LX_MAP_SHARED    0x01
#define LX_MAP_PRIVATE   0x02
#define LX_MAP_FIXED     0x10
#define LX_MAP_ANONYMOUS 0x20
#define LX_MAP_STACK     0x20000
void *bionic_mmap(void *addr, size_t len, int prot, int lx, int fd, off_t off) {
    int f = 0;
    if (lx & LX_MAP_SHARED)    f |= MAP_SHARED;
    if (lx & LX_MAP_PRIVATE)   f |= MAP_PRIVATE;
    if (lx & LX_MAP_FIXED)     f |= MAP_FIXED;
    if (lx & LX_MAP_ANONYMOUS) {
        f |= MAP_ANON;
        // Linux ignores fd for anonymous maps; FreeBSD requires fd == -1 and
        // returns EINVAL otherwise (the engine passes fd 0 for some of them).
        fd = -1;
    }
    if (lx & LX_MAP_STACK)     f |= MAP_STACK;
    // Linux-only advisory flags (NORESERVE, POPULATE, DENYWRITE, GROWSDOWN,
    // LOCKED, HUGETLB, NONBLOCK) have no FreeBSD equivalent — drop them.
    // PROT_* bits match between the two, so prot passes through unchanged.
    return mmap(addr, len, prot, f, fd, off);
}

// Linux sysinfo(2): fills total/free RAM etc. Roblox's allocator sizes its
// arenas from totalram, so a stub (garbage) makes it abort. FreeBSD has no
// sysinfo; source the same numbers from sysctl.
#include <sys/sysctl.h>
struct linux_sysinfo {
    long uptime;
    unsigned long loads[3];
    unsigned long totalram, freeram, sharedram, bufferram, totalswap, freeswap;
    unsigned short procs, pad;
    unsigned long totalhigh, freehigh;
    unsigned int mem_unit;
    // bionic's trailing pad `_f` is `20 - 2*sizeof(long) - sizeof(int)` = 0 bytes
    // on LP64, making sizeof(struct sysinfo) == 104. A larger struct here makes
    // memset() overrun the caller's buffer and trips its stack canary.
};
int bionic_sysinfo(struct linux_sysinfo *info) {
    memset(info, 0, sizeof(*info));
    unsigned long physmem = 0;
    size_t len = sizeof(physmem);
    // FreeBSD sysconf selector numbers (BSD_VISIBLE macros aren't exposed under
    // the build's strict feature flags, so spell them out): _SC_PAGESIZE=47,
    // _SC_PHYS_PAGES=121, _SC_NPROCESSORS_ONLN=58.
    long pgsz = sysconf(47);
    if (pgsz <= 0)
        pgsz = 4096;
    if (sysctlbyname("hw.physmem", &physmem, &len, NULL, 0) != 0)
        physmem = (unsigned long)sysconf(121) * (unsigned long)pgsz;
    unsigned long freepg = 0;
    len = sizeof(freepg);
    sysctlbyname("vm.stats.vm.v_free_count", &freepg, &len, NULL, 0);
    info->mem_unit = 1;
    info->totalram = physmem;
    info->freeram = freepg * (unsigned long)pgsz;
    if (info->freeram == 0 || info->freeram > physmem)
        info->freeram = physmem / 2;
    long ncpu = sysconf(58);
    info->procs = (unsigned short)(ncpu > 0 ? ncpu : 1);
    return 0;
}

// ── bionic syscall(2) shim ──────────────────────────────────────────────────
// libroblox calls syscall() with *Linux* numbers. FreeBSD's syscall uses
// different numbers, so forwarding raw would be catastrophic. Dispatch the
// handful bionic actually issues during init to real FreeBSD calls, and return
// -ENOSYS (not garbage) for the rest so callers take their error path. Named
// bionic_syscall so the host process keeps libc's real syscall.
#include <sched.h>
#include <sys/random.h>
#include <time.h>

// Real futex over FreeBSD's _umtx_op. Returning 0 from a FUTEX_WAIT (as a stub
// does) makes every thread that should block busy-spin, livelocking the engine.
struct fbsd_umtx_time {
    struct timespec _timeout;
    unsigned int _flags;
    unsigned int _clockid;
};
extern int _umtx_op(void *obj, int op, unsigned long val, void *uaddr, void *uaddr2);
#define FBSD_UMTX_OP_WAIT_UINT_PRIVATE 15
#define FBSD_UMTX_OP_WAKE_PRIVATE 16
#define FBSD_UMTX_ABSTIME 1
#define FBSD_CLOCK_REALTIME 0
#define FBSD_CLOCK_MONOTONIC 4
#define LX_FUTEX_WAIT 0
#define LX_FUTEX_WAKE 1
#define LX_FUTEX_WAIT_BITSET 9  // bionic's timed waits use this — absolute deadline
#define LX_FUTEX_WAKE_BITSET 10
#define LX_FUTEX_PRIVATE_FLAG 128
#define LX_FUTEX_CLOCK_REALTIME 256

static long do_futex(void *uaddr, int op, unsigned int val, const struct timespec *to) {
    // Strip the PRIVATE / CLOCK_REALTIME flag bits to get the base command.
    int cmd = op & ~(LX_FUTEX_PRIVATE_FLAG | LX_FUTEX_CLOCK_REALTIME);
    if (cmd == LX_FUTEX_WAIT || cmd == LX_FUTEX_WAIT_BITSET) {
        struct fbsd_umtx_time ut;
        void *tptr = NULL;
        unsigned long tsz = 0;
        if (to) {
            ut._timeout = *to;
            if (cmd == LX_FUTEX_WAIT_BITSET) {
                // BITSET carries an *absolute* deadline (this is how bionic's
                // pthread_cond / lock timeouts are implemented — treating it as
                // a no-op returning 0 was the flag-parse busy-spin).
                ut._flags = FBSD_UMTX_ABSTIME;
                ut._clockid = (op & LX_FUTEX_CLOCK_REALTIME) ? FBSD_CLOCK_REALTIME
                                                             : FBSD_CLOCK_MONOTONIC;
            } else {
                ut._flags = 0; // plain WAIT is a relative timeout
                ut._clockid = FBSD_CLOCK_MONOTONIC;
            }
            tptr = &ut;
            tsz = sizeof(ut);
        }
        int r = _umtx_op(uaddr, FBSD_UMTX_OP_WAIT_UINT_PRIVATE,
                         (unsigned long)val, (void *)tsz, tptr);
        // bionic's __futex checks the raw negative-errno convention (-ETIMEDOUT,
        // -EAGAIN, -EINTR), so return -errno rather than -1.
        return r == 0 ? 0 : -errno;
    }
    if (cmd == LX_FUTEX_WAKE || cmd == LX_FUTEX_WAKE_BITSET) {
        int r = _umtx_op(uaddr, FBSD_UMTX_OP_WAKE_PRIVATE,
                         (unsigned long)val, NULL, NULL);
        return r == 0 ? (long)val : -errno;
    }
    // Any other op (requeue, PI, wake_op): report unsupported rather than lie
    // with 0, which would busy-spin the caller.
    return -ENOSYS;
}

// Translate a Linux clockid to FreeBSD's — they disagree: Linux CLOCK_MONOTONIC
// is 1, but FreeBSD 1 is CLOCK_VIRTUAL (process user-CPU time); FreeBSD's
// monotonic is 4. Untranslated, every engine timer reads CPU time, not wall
// time, and timed loops never satisfy their deadlines.
static int fbsd_clockid(int lx) {
    switch (lx) {
    case 0: return 0;   // CLOCK_REALTIME
    case 1: return 4;   // CLOCK_MONOTONIC        -> FreeBSD 4
    case 2: return 15;  // CLOCK_PROCESS_CPUTIME  -> FreeBSD 15
    case 3: return 14;  // CLOCK_THREAD_CPUTIME   -> FreeBSD 14
    case 4: return 4;   // CLOCK_MONOTONIC_RAW    -> monotonic
    case 5: return 10;  // CLOCK_REALTIME_COARSE  -> REALTIME_FAST
    case 6: return 12;  // CLOCK_MONOTONIC_COARSE -> MONOTONIC_FAST
    case 7: return 4;   // CLOCK_BOOTTIME         -> monotonic
    default: return lx;
    }
}

// clock_gettime / clock_getres translating the clockid. The engine imports
// these directly (not only via syscall), so both need an override.
int bionic_clock_gettime(int lx_clockid, struct timespec *ts) {
    return clock_gettime(fbsd_clockid(lx_clockid), ts);
}
int bionic_clock_getres(int lx_clockid, struct timespec *ts) {
    return clock_getres(fbsd_clockid(lx_clockid), ts);
}

// Linux x86-64 syscall numbers.
#define LX_getpid          39
#define LX_gettid          186
#define LX_set_tid_address 218
#define LX_getrandom       318
#define LX_futex           202
#define LX_clock_gettime   228
#define LX_gettimeofday    96
#define LX_sched_yield     24
#define LX_nanosleep       35

long bionic_syscall(long number, ...) {
    va_list ap;
    va_start(ap, number);
    long a0 = va_arg(ap, long), a1 = va_arg(ap, long), a2 = va_arg(ap, long);
    long a3 = va_arg(ap, long);
    (void)a3;
    va_end(ap);

    switch (number) {
    case LX_gettid:
    case LX_set_tid_address:
        // Both want the current thread id; set_tid_address also returns it.
        return (long)pthread_getthreadid_np();
    case LX_getpid:
        return (long)getpid();
    case LX_getrandom:
        return (long)getrandom((void *)a0, (size_t)a1, (unsigned)a2);
    case LX_clock_gettime:
        return bionic_clock_gettime((int)a0, (struct timespec *)a1);
    case LX_gettimeofday:
        return gettimeofday((struct timeval *)a0, (struct timezone *)a1);
    case LX_sched_yield:
        return sched_yield();
    case LX_nanosleep:
        return nanosleep((const struct timespec *)a0, (struct timespec *)a1);
    case LX_futex:
        // futex(uaddr=a0, op=a1, val=a2, timeout=a3) -> real _umtx_op wait/wake.
        return do_futex((void *)a0, (int)a1, (unsigned int)a2,
                        (const struct timespec *)a3);
    default:
        errno = ENOSYS;
        return -1;
    }
}

// glibc exposes errno's address as __errno_location(); FreeBSD calls it __error().
extern int *__error(void);
int *__errno_location(void) { return __error(); }

// Linux gettid(2). FreeBSD's documented equivalent returns the same small tid.
pid_t gettid(void) { return (pid_t)pthread_getthreadid_np(); }

// glibc getauxval(3) over FreeBSD's elf_aux_info(3). The AT_* type numbers the
// linker asks for (AT_HWCAP, AT_PAGESZ, AT_PHDR, …) share values on both.
unsigned long getauxval(unsigned long type) {
    unsigned long value = 0;
    if (elf_aux_info((int)type, &value, sizeof(value)) != 0)
        return 0;
    return value;
}

// glibc's LFS alias. FreeBSD's off_t is already 64-bit, so open64 == open.
int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = (mode_t)va_arg(ap, int);
        va_end(ap);
    }
    return open(path, flags, mode);
}

// Linux prctl(2). The linker uses it only for best-effort VMA naming
// (PR_SET_VMA) and similar; FreeBSD has no analogue, and every caller tolerates
// failure. Report unsupported rather than pretend success.
int prctl(int option, ...) {
    (void)option;
    errno = ENOSYS;
    return -1;
}

#endif /* __FreeBSD__ */
