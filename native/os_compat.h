// os_compat.h — small portability shims for the parts of the native layer that
// assume Linux/glibc. Kept in one place so the fix is auditable and the call
// sites stay readable.
#pragma once

#include <cstdint>
#include <fcntl.h>
#include <pthread.h>

#if defined(__FreeBSD__)
#  include <pthread_np.h>
// Linux's gettid(2) has no FreeBSD syscall; pthread_getthreadid_np() is the
// documented equivalent and returns the same small integer tid.
static inline long cordial_gettid(void) { return (long)pthread_getthreadid_np(); }
// FreeBSD has no O_TMPFILE. Callers only test `flags & O_TMPFILE`; defining it
// to 0 makes that test always false, which is the correct behaviour when the
// kernel cannot create an unnamed temp file this way.
#  ifndef O_TMPFILE
#    define O_TMPFILE 0
#  endif
#else
#  include <unistd.h>
#  include <sys/syscall.h>
static inline long cordial_gettid(void) { return (long)::syscall(SYS_gettid); }
#endif

// A numeric identity for a pthread_t. On Linux pthread_t is an integer; on
// FreeBSD it is a pointer. Routing through uintptr_t yields a stable unsigned
// value on both without tripping C++'s static_cast rules.
static inline unsigned long cordial_pthread_id(pthread_t t) {
    return (unsigned long)(uintptr_t)t;
}
