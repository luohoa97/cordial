// freebsd_libc_compat.c — definitions for the handful of glibc/bionic libc
// symbols the AOSP linker and shim reference that FreeBSD's libc spells
// differently or does not provide. Whole file compiles to nothing off FreeBSD.
#if defined(__FreeBSD__)

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <pthread_np.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

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

// bionic pthread_rwlock_t (56 bytes) has no relation to FreeBSD's (an 8-byte
// pointer). Same side-table treatment as the mutex: a real FreeBSD rwlock keyed
// off the bionic object's address. Left stubbed, OpenSSL's OBJ_NAME table lock
// (crypto/objects/o_names.c) either no-ops or blocks on garbage — a deadlock.
struct br_entry {
    const void *key;
    pthread_rwlock_t real;
    struct br_entry *next;
};
static struct br_entry *br_tab[BM_SLOTS];
static pthread_mutex_t br_guard = PTHREAD_MUTEX_INITIALIZER;

static pthread_rwlock_t *br_lookup(const void *key) {
    unsigned h = (unsigned)(((uintptr_t)key >> 4) & (BM_SLOTS - 1));
    for (struct br_entry *e = __atomic_load_n(&br_tab[h], __ATOMIC_ACQUIRE); e;
         e = e->next)
        if (e->key == key)
            return &e->real;
    pthread_mutex_lock(&br_guard);
    for (struct br_entry *e = br_tab[h]; e; e = e->next)
        if (e->key == key) {
            pthread_mutex_unlock(&br_guard);
            return &e->real;
        }
    struct br_entry *e = calloc(1, sizeof *e);
    pthread_rwlock_init(&e->real, NULL);
    e->key = key;
    e->next = br_tab[h];
    __atomic_store_n(&br_tab[h], e, __ATOMIC_RELEASE);
    pthread_mutex_unlock(&br_guard);
    return &e->real;
}

int bionic_pthread_rwlock_init(void *l, const void *attr) { (void)attr; br_lookup(l); return 0; }
int bionic_pthread_rwlock_rdlock(void *l) { return pthread_rwlock_rdlock(br_lookup(l)); }
int bionic_pthread_rwlock_wrlock(void *l) { return pthread_rwlock_wrlock(br_lookup(l)); }
int bionic_pthread_rwlock_tryrdlock(void *l) { return pthread_rwlock_tryrdlock(br_lookup(l)); }
int bionic_pthread_rwlock_trywrlock(void *l) { return pthread_rwlock_trywrlock(br_lookup(l)); }
int bionic_pthread_rwlock_unlock(void *l) { return pthread_rwlock_unlock(br_lookup(l)); }
int bionic_pthread_rwlock_destroy(void *l) { (void)l; return 0; }

// FORTIFY open with no mode arg, and a registered prctl (unsupported).
int __open_2(const char *path, int flags) { return open(path, flags); }

// pipe2 logger + write-end table. The engine's GameActivity app thread makes a
// command pipe (pipe2), ALooper_addFd's the READ end, and its android_main loop
// dispatches commands read from it. cordial never feeds that pipe, so the
// StartupController (created on command 3) never gets made. To fix that cordial
// needs the WRITE end, but only sees the read end (via ALooper_addFd). Record
// every pipe2 pair here so the read->write mapping can be looked up later.
#define PIPE_TAB 256
static int g_pipe_rd[PIPE_TAB];
static int g_pipe_wr[PIPE_TAB];
static int g_pipe_n;
static pthread_mutex_t g_pipe_lock = PTHREAD_MUTEX_INITIALIZER;
int pipe2(int fds[2], int flags) {
    // Raw syscall, NOT the libc pipe2 (this IS the libc pipe2 for cordial-run, so
    // calling it would recurse).
    int r = (int)syscall(SYS_pipe2, fds, flags);
    if (r == 0) {
        pthread_mutex_lock(&g_pipe_lock);
        int i = g_pipe_n % PIPE_TAB;
        g_pipe_rd[i] = fds[0];
        g_pipe_wr[i] = fds[1];
        g_pipe_n++;
        pthread_mutex_unlock(&g_pipe_lock);
        if (getenv("CORDIAL_TRACE_PIPE"))
            fprintf(stderr, "[pipe2] read=%d write=%d flags=0x%x\n", fds[0], fds[1], flags);
    }
    return r;
}
// AGDK's android_native_app_glue uses the classic pipe(2), not pipe2.
int pipe(int fds[2]) {
    int r = (int)syscall(SYS_pipe2, fds, 0);
    if (r == 0) {
        pthread_mutex_lock(&g_pipe_lock);
        int i = g_pipe_n % PIPE_TAB;
        g_pipe_rd[i] = fds[0];
        g_pipe_wr[i] = fds[1];
        g_pipe_n++;
        pthread_mutex_unlock(&g_pipe_lock);
        if (getenv("CORDIAL_TRACE_PIPE"))
            fprintf(stderr, "[pipe] read=%d write=%d\n", fds[0], fds[1]);
    }
    return r;
}
// Look up the write end paired with a given read-end fd (from a pipe/pipe2 call).
// Returns -1 if unknown. cordial calls this to find the command pipe's write end.
int cordial_pipe_write_end(int read_fd) {
    int w = -1;
    pthread_mutex_lock(&g_pipe_lock);
    for (int i = 0; i < g_pipe_n && i < PIPE_TAB; i++)
        if (g_pipe_rd[i] == read_fd) w = g_pipe_wr[i];
    pthread_mutex_unlock(&g_pipe_lock);
    return w;
}

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

// Cached so the hot WAIT path (100k+/s under a busy-wait) does not getenv() on
// every call. Benign first-call race.
static int futex_trace_enabled(void) {
    static int v = -1;
    if (v < 0) v = getenv("CORDIAL_TRACE_FUTEX") ? 1 : 0;
    return v;
}

// FreeBSD errno -> Linux errno for values bionic (compiled for Android/Linux)
// inspects by number. Most low numbers agree; the ones that bite the futex path
// diverge. Passthrough for everything else — a wrong-but-nonzero error is still
// treated as an error by callers; only the specifically-checked values matter.
static int fbsd_errno_to_linux(int e) {
    switch (e) {
    case 35: return 11;   // EAGAIN / EWOULDBLOCK : FreeBSD 35 -> Linux 11
    case 60: return 110;  // ETIMEDOUT            : FreeBSD 60 -> Linux 110
    case 85: return 4;    // ERESTART             -> treat as EINTR(4), same both
    default: return e;    // EINTR(4), EINVAL(22), EFAULT(14), ... agree
    }
}

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
        if (futex_trace_enabled()) {
            struct timespec now_m = {0, 0}, now_r = {0, 0};
            clock_gettime(4 /*FreeBSD MONOTONIC*/, &now_m);
            clock_gettime(0 /*REALTIME*/, &now_r);
            fprintf(stderr,
                "[futex] tid=%d WAIT addr=%p cmd=%d op=0x%x val=%u to=%s{%lld.%09ld} flags=%u clk=%u "
                "| mono=%lld.%09ld real=%lld.%09ld\n",
                (int)pthread_getthreadid_np(), uaddr, cmd, op, val, to ? "" : "NULL",
                (long long)(to ? to->tv_sec : 0), (long)(to ? to->tv_nsec : 0),
                to ? ut._flags : 0u, to ? ut._clockid : 0u,
                (long long)now_m.tv_sec, now_m.tv_nsec,
                (long long)now_r.tv_sec, now_r.tv_nsec);
        }
        int r = _umtx_op(uaddr, FBSD_UMTX_OP_WAIT_UINT_PRIVATE,
                         (unsigned long)val, (void *)tsz, tptr);
        // bionic's __futex checks the raw negative-errno convention, but against
        // *Linux* errno numbers — it is compiled for Android. FreeBSD's errno
        // values differ (ETIMEDOUT 60 vs 110, EAGAIN 35 vs 11), so returning the
        // raw FreeBSD -errno made bionic's `rc == -ETIMEDOUT` / `rc == -EAGAIN`
        // checks fail: a timed pthread_cond wait never recognised its own
        // timeout, re-armed the same (now stale) deadline and busy-spun a core
        // flat out (measured ~340k/s across three threads, blocking the render).
        // Translate to the Linux value bionic expects.
        return r == 0 ? 0 : -fbsd_errno_to_linux(errno);
    }
    if (cmd == LX_FUTEX_WAKE || cmd == LX_FUTEX_WAKE_BITSET) {
        int r = _umtx_op(uaddr, FBSD_UMTX_OP_WAKE_PRIVATE,
                         (unsigned long)val, NULL, NULL);
        if (futex_trace_enabled())
            fprintf(stderr, "[futex] tid=%d WAKE addr=%p cmd=%d op=0x%x val=%u -> r=%d errno=%d\n",
                    (int)pthread_getthreadid_np(), uaddr, cmd, op, val, r, r == 0 ? 0 : errno);
        return r == 0 ? (long)val : -fbsd_errno_to_linux(errno);
    }
    // Any other op (requeue, PI, wake_op): report unsupported rather than lie
    // with 0, which would busy-spin the caller. Linux ENOSYS is 38 (FreeBSD 78).
    return -38;
}

// bionic pthread_once on the 4-byte control word, using the real futex for
// waiting — NO global guard. A single global lock held across the init routine
// deadlocks when an init routine itself calls pthread_once (OpenSSL and the
// engine's reflection init both nest them).
#include <limits.h>
#define ONCE_NOT_STARTED 0
#define ONCE_IN_PROGRESS 1
#define ONCE_DONE 2
int bionic_pthread_once(int *control, void (*init)(void)) {
    for (;;) {
        int old = __atomic_load_n(control, __ATOMIC_ACQUIRE);
        if (old == ONCE_DONE)
            return 0;
        if (old == ONCE_NOT_STARTED) {
            int expected = ONCE_NOT_STARTED;
            if (__atomic_compare_exchange_n(control, &expected, ONCE_IN_PROGRESS,
                                            0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
                if (init)
                    init();
                __atomic_store_n(control, ONCE_DONE, __ATOMIC_RELEASE);
                do_futex(control, LX_FUTEX_WAKE, INT_MAX, NULL); // wake waiters
                return 0;
            }
            // Lost the race to start it; re-read and act on the new state.
        } else {
            // In progress on another thread: block on the control word until it
            // transitions (bionic's own protocol, via _umtx_op).
            do_futex(control, LX_FUTEX_WAIT, ONCE_IN_PROGRESS, NULL);
        }
    }
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

// glibc/bionic getauxval(3) over FreeBSD's elf_aux_info(3).
//
// Callers here are bionic, so `type` is a *Linux* AT_* number. Only 0..14 share
// values with FreeBSD; from 15 up they diverge and a blind passthrough is a
// silent corruption:
//   Linux AT_HWCAP=16   == FreeBSD AT_CANARY=16
//   Linux AT_RANDOM=25  == FreeBSD AT_HWCAP=25
// So a bionic caller asking AT_RANDOM (a pointer to 16 entropy bytes for the
// stack canary) would get a hwcap bitmask and dereference it -> SIGSEGV.
//
// We translate the numbers we care about and, for the pointer-valued ones,
// hand back a real pointer of our own rather than whatever elf_aux_info copies.
enum {
    LX_AT_PHDR   = 3,  LX_AT_PHENT = 4,  LX_AT_PHNUM = 5,
    LX_AT_PAGESZ = 6,  LX_AT_BASE  = 7,  LX_AT_ENTRY = 9,
    LX_AT_HWCAP  = 16, LX_AT_SECURE = 23, LX_AT_RANDOM = 25, LX_AT_HWCAP2 = 26,
    FB_AT_HWCAP  = 25, FB_AT_HWCAP2 = 26,
};
unsigned long getauxval(unsigned long type) {
    switch (type) {
    case LX_AT_RANDOM: {
        // Bionic wants a pointer to 16 random bytes (stack-guard seed). FreeBSD
        // exposes a canary via AT_CANARY, but elf_aux_info *copies* it rather
        // than returning a pointer, so just own the buffer ourselves.
        static unsigned char rnd[16];
        static int seeded = 0;
        if (!seeded) { arc4random_buf(rnd, sizeof(rnd)); seeded = 1; }
        return (unsigned long)rnd;
    }
    case LX_AT_HWCAP: {
        unsigned long v = 0;
        if (elf_aux_info(FB_AT_HWCAP, &v, sizeof(v)) != 0) return 0;
        return v;
    }
    case LX_AT_HWCAP2: {
        unsigned long v = 0;
        if (elf_aux_info(FB_AT_HWCAP2, &v, sizeof(v)) != 0) return 0;
        return v;
    }
    case LX_AT_SECURE:
        // No FreeBSD auxv equivalent; report "not a setuid exec" (safe default).
        return 0;
    case LX_AT_PHDR: case LX_AT_PHENT: case LX_AT_PHNUM:
    case LX_AT_PAGESZ: case LX_AT_BASE: case LX_AT_ENTRY: {
        // Shared numbers (0..14) — forward straight through.
        unsigned long v = 0;
        if (elf_aux_info((int)type, &v, sizeof(v)) != 0) return 0;
        return v;
    }
    default:
        return 0;
    }
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

// Linux sched_setscheduler/sched_setparam. The engine's FunctionMarshaller (and
// other threads) ask for SCHED_FIFO at max priority. On FreeBSD that either fails
// EPERM (unprivileged) or, if it succeeds, makes the thread real-time — and a
// real-time thread that then blocks on a mutex held by a normal-priority thread
// can priority-invert into a hang (no PI on these bionic mutexes). bionic itself
// treats these as best-effort hints. Report success without actually changing the
// policy, so scheduling stays uniform and no inversion is possible.
int sched_setscheduler(pid_t pid, int policy, const struct sched_param *param) {
    (void)pid; (void)policy; (void)param;
    // Report the honest unprivileged result ("could not set the policy") rather
    // than a fake success. bionic treats these as best-effort and does not check
    // the return, so EPERM changes nothing behaviourally but never lies that the
    // thread became real-time (verified: does not affect the boot either way).
    errno = EPERM;
    return -1;
}
int sched_setparam(pid_t pid, const struct sched_param *param) {
    (void)pid; (void)param;
    errno = EPERM;
    return -1;
}

// --- bionic pthread_attr_t family ------------------------------------------
//
// This is the single nastiest ABI mismatch in the port. bionic's
// `pthread_attr_t` is a *by-value* 56-byte struct (LP64 layout below); FreeBSD's
// is an *opaque pointer* (`struct pthread_attr *`). libroblox hands bionic attr
// structs to these symbols, so every one of them MUST be owned here — routing
// even one to host libthr means libthr reads bionic's first qword (`flags`) as a
// `struct pthread_attr *` and free()s it. Observed exactly that: a stubbed
// pthread_getattr_np left the struct uninitialised, then libthr's
// pthread_attr_destroy did free(0xffffffff) and jemalloc walked off into
// unmapped memory. Keep the whole family self-consistent over this one layout.
struct bionic_pthread_attr {
    uint32_t flags;          // 0x00
    uint32_t _pad;           // 0x04
    void*    stack_base;     // 0x08
    size_t   stack_size;     // 0x10
    size_t   guard_size;     // 0x18
    int32_t  sched_policy;   // 0x20
    int32_t  sched_priority; // 0x24
    char     __reserved[16]; // 0x28..0x38  (total 0x38 = 56 bytes)
};
#define BIONIC_ATTR_FLAG_DETACHED 1u

int bionic_pthread_attr_init(void* attr) {
    struct bionic_pthread_attr* a = attr;
    memset(a, 0, sizeof *a);
    a->stack_size = 8u * 1024u * 1024u;  // Roblox threads are heavy; 8 MiB.
    a->guard_size = (size_t)getpagesize();
    a->sched_policy = 0;                 // SCHED_OTHER / bionic SCHED_NORMAL
    return 0;
}

// bionic attr owns no heap, so destroy just scrubs the struct. Critically this
// keeps the pointer-vs-struct mismatch from ever reaching free().
int bionic_pthread_attr_destroy(void* attr) {
    memset(attr, 0, sizeof(struct bionic_pthread_attr));
    return 0;
}

// glibc/bionic extension: fill `attr` with a live thread's real attributes.
// `thread` is a genuine FreeBSD pthread_t here — every handle libroblox holds
// came from host pthread_create/pthread_self — so FreeBSD's pthread_attr_get_np
// accepts it directly. We copy the stack bounds into the bionic layout.
int bionic_pthread_getattr_np(pthread_t thread, void* attr) {
    struct bionic_pthread_attr* a = attr;
    memset(a, 0, sizeof *a);
    pthread_attr_t fa;
    if (pthread_attr_init(&fa) != 0)
        return ENOMEM;
    // If we cannot get the real bounds, DO NOT return success with a NULL/zero
    // stack — a bionic caller (jemalloc, the GC stack scanner) would trust those
    // and walk off a zero-length stack, the exact class of stack-attr lie the
    // header comment above records biting us before.
    int rc = pthread_attr_get_np(thread, &fa);
    if (rc != 0) {
        pthread_attr_destroy(&fa);
        return rc;
    }
    void* base = NULL;
    size_t size = 0, guard = 0;
    pthread_attr_getstack(&fa, &base, &size);
    pthread_attr_getguardsize(&fa, &guard);
    pthread_attr_destroy(&fa);
    if (base == NULL || size == 0)
        return EINVAL;
    a->stack_base = base;
    a->stack_size = size;
    a->guard_size = guard;
    return 0;
}

int bionic_pthread_attr_getstack(const void* attr, void** base, size_t* size) {
    const struct bionic_pthread_attr* a = attr;
    if (base) *base = a->stack_base;
    if (size) *size = a->stack_size;
    return 0;
}

int bionic_pthread_attr_setstacksize(void* attr, size_t stacksize) {
    ((struct bionic_pthread_attr*)attr)->stack_size = stacksize;
    return 0;
}

int bionic_pthread_attr_setdetachstate(void* attr, int state) {
    struct bionic_pthread_attr* a = attr;
    if (state) a->flags |= BIONIC_ATTR_FLAG_DETACHED;
    else       a->flags &= ~BIONIC_ATTR_FLAG_DETACHED;
    return 0;
}

// struct sched_param leads with `int sched_priority` on both bionic and FreeBSD,
// so reading the first int is layout-safe.
int bionic_pthread_attr_setschedparam(void* attr, const void* param) {
    ((struct bionic_pthread_attr*)attr)->sched_priority = *(const int*)param;
    return 0;
}

#endif /* __FreeBSD__ */
