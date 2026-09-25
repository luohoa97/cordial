// freebsd_abi.h — the Linux<->FreeBSD value translation that native/freebsd_abi.c
// performs for the engine, exposed for the two other files that sit on the
// same boundary: system_paths.cpp (whose `open` must translate flags before it
// remaps a path) and netdb_compat.cpp (whose `getaddrinfo` hands the engine
// sockaddrs). Everything here exists only on FreeBSD.
#pragma once

#if defined(__FreeBSD__)

#include <stddef.h>
#include <sys/socket.h>

#ifdef __cplusplus
extern "C" {
#endif

// Linux open(2) flags -> FreeBSD. Returns 0 and stores the host word, or -1 with
// the engine's errno already set (Linux-numbered) for a bit that has no honest
// FreeBSD equivalent.
int cordial_fbsd_open_flags(int lx_flags, int* host_flags);
// Whether a Linux open(2) flag word carries a mode argument (O_CREAT/O_TMPFILE).
int cordial_fbsd_open_takes_mode(int lx_flags);
// After a failed host call: rewrite the thread's errno from FreeBSD numbering to
// Linux numbering, and remember that it has been, so `__errno` does not
// translate it a second time.
void cordial_fbsd_errno_to_linux(void);
// FreeBSD sockaddr -> Linux layout into `out` (at most `cap` bytes; truncated
// like the kernel truncates). Returns the full Linux length, or 0 if the family
// has no translation.
size_t cordial_fbsd_sockaddr_to_linux(const struct sockaddr* in, void* out, size_t cap);
// Address family numbering, both directions. -1 for a family with no match.
int cordial_fbsd_af_to_linux(int host_af);
int cordial_fbsd_af_from_linux(int lx_af);

#ifdef __cplusplus
}
#endif

#endif // __FreeBSD__
