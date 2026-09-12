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
    pthread_mutex_lock(&bm_guard);
    struct bm_entry *e = bm_tab[h];
    while (e && e->key != key)
        e = e->next;
    if (!e) {
        e = calloc(1, sizeof *e);
        pthread_mutexattr_t a;
        pthread_mutexattr_init(&a);
        pthread_mutexattr_settype(&a, PTHREAD_MUTEX_RECURSIVE);
        pthread_mutex_init(&e->real, &a);
        pthread_mutexattr_destroy(&a);
        e->key = key;
        e->next = bm_tab[h];
        bm_tab[h] = e;
    }
    pthread_mutex_unlock(&bm_guard);
    return e;
}

int bionic_pthread_mutex_init(void *m, const void *attr) { (void)attr; bm_lookup(m); return 0; }
int bionic_pthread_mutex_lock(void *m) { return pthread_mutex_lock(&bm_lookup(m)->real); }
int bionic_pthread_mutex_unlock(void *m) { return pthread_mutex_unlock(&bm_lookup(m)->real); }
int bionic_pthread_mutex_trylock(void *m) { return pthread_mutex_trylock(&bm_lookup(m)->real); }
int bionic_pthread_mutex_destroy(void *m) { (void)m; return 0; }

// FORTIFY open with no mode arg, and a registered prctl (unsupported).
int __open_2(const char *path, int flags) { return open(path, flags); }

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
        return clock_gettime((clockid_t)a0, (struct timespec *)a1);
    case LX_gettimeofday:
        return gettimeofday((struct timeval *)a0, (struct timezone *)a1);
    case LX_sched_yield:
        return sched_yield();
    case LX_nanosleep:
        return nanosleep((const struct timespec *)a0, (struct timespec *)a1);
    case LX_futex:
        // Not yet translated to _umtx_op; report "would block / done" as 0 so
        // callers do not treat it as a hard error. Revisit for real contention.
        return 0;
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
