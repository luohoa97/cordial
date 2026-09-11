// fortify_chk.c — FORTIFY_SOURCE (_chk) wrappers glibc/bionic provide and
// FreeBSD's libc does not. Roblox's libroblox.so imports these; on Linux
// Cordial borrows them from the host glibc, but FreeBSD has no such symbols.
//
// Each forwards to the unchecked libc routine and treats the object-size bound
// as advisory (the same bargain Cordial's existing write_chk/poll_chk strike).
// Whole file compiles to nothing off FreeBSD.
#if defined(__FreeBSD__)

#include <poll.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

void *__memcpy_chk(void *d, const void *s, size_t n, size_t dlen) { (void)dlen; return memcpy(d, s, n); }
void *__memmove_chk(void *d, const void *s, size_t n, size_t dlen) { (void)dlen; return memmove(d, s, n); }
void *__memset_chk(void *d, int c, size_t n, size_t dlen) { (void)dlen; return memset(d, c, n); }

char *__strcpy_chk(char *d, const char *s, size_t dlen) { (void)dlen; return strcpy(d, s); }
char *__strcat_chk(char *d, const char *s, size_t dlen) { (void)dlen; return strcat(d, s); }
char *__strncpy_chk(char *d, const char *s, size_t n, size_t dlen) { (void)dlen; return strncpy(d, s, n); }

ssize_t __read_chk(int fd, void *buf, size_t n, size_t blen) { (void)blen; return read(fd, buf, n); }
ssize_t __pread64_chk(int fd, void *buf, size_t n, off_t off, size_t blen) { (void)blen; return pread(fd, buf, n, off); }

ssize_t __sendto_chk(int fd, const void *buf, size_t len, size_t blen, int flags,
                     const struct sockaddr *to, socklen_t tolen) {
    (void)blen;
    return sendto(fd, buf, len, flags, to, tolen);
}

size_t __fwrite_chk(const void *p, size_t size, size_t count, FILE *f, size_t blen) {
    (void)blen;
    return fwrite(p, size, count, f);
}

int __vsnprintf_chk(char *d, size_t n, int flags, size_t dlen, const char *fmt, va_list ap) {
    (void)flags; (void)dlen;
    return vsnprintf(d, n, fmt, ap);
}
int __vsprintf_chk(char *d, int flags, size_t dlen, const char *fmt, va_list ap) {
    (void)flags; (void)dlen;
    return vsprintf(d, fmt, ap);
}

void __FD_SET_chk(int fd, fd_set *set, size_t sz) { (void)sz; FD_SET(fd, set); }
void __FD_CLR_chk(int fd, fd_set *set, size_t sz) { (void)sz; FD_CLR(fd, set); }
int __FD_ISSET_chk(int fd, fd_set *set, size_t sz) { (void)sz; return FD_ISSET(fd, set); }

#endif /* __FreeBSD__ */
