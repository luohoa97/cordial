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
