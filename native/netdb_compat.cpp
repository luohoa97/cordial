// `getaddrinfo` across the bionic/glibc boundary.
//
// This is the same class of defect as `struct stat` and `pthread_mutex_t`, and
// it belongs on that list: **bionic and glibc disagree about `struct addrinfo`,
// and they disagree about the `AI_*` constants.**
//
// Layout, from the two headers in this tree:
//
//     bionic                          glibc
//     int       ai_flags;             int       ai_flags;
//     int       ai_family;            int       ai_family;
//     int       ai_socktype;          int       ai_socktype;
//     int       ai_protocol;          int       ai_protocol;
//     socklen_t ai_addrlen;           socklen_t ai_addrlen;
//     char*     ai_canonname;   <-->  sockaddr* ai_addr;
//     sockaddr* ai_addr;        <-->  char*     ai_canonname;
//     addrinfo* ai_next;              addrinfo* ai_next;
//
// The two pointers are swapped. Roblox is compiled against the left column, so
// reading `ai_addr` off a glibc result hands it the canonical *name* — a
// NUL-terminated hostname — which it then reads as a `sockaddr`. `sa_family`
// comes out as the first two ASCII bytes of the hostname, matches no address
// family, and every address is discarded.
//
// The constants are worse, because they break the call before it runs:
//
//     AI_NUMERICSERV   bionic 0x0008    glibc 0x0400
//     AI_ALL           bionic 0x0100    glibc 0x0010
//     AI_V4MAPPED_CFG  bionic 0x0200    glibc —
//     AI_ADDRCONFIG    bionic 0x0400    glibc 0x0020
//     AI_V4MAPPED      bionic 0x0800    glibc 0x0008
//
// bionic's `AI_DEFAULT` is `AI_V4MAPPED_CFG|AI_ADDRCONFIG` = 0x600. glibc
// validates `ai_flags` against its own mask and answers `EAI_BADFLAGS` for the
// unknown 0x200 bit, so the lookup fails outright — which is exactly what the
// engine reported, on every request, for every host:
//
//     [DFLog::HttpTraceError] CURLOPT_ERRORBUFFER: Could not resolve host: apis.roblox.com
//
// `struct hostent` is identical in both and needs no translation.

#include <cstdlib>
#include <cstring>
#include <cstddef>

#include <netdb.h>
#include <sys/socket.h>

#include "freebsd_abi.h"

namespace {

/// `struct addrinfo` exactly as Roblox was compiled to see it.
struct BionicAddrinfo {
    int ai_flags;
    int ai_family;
    int ai_socktype;
    int ai_protocol;
    socklen_t ai_addrlen;
    char* ai_canonname;
    struct sockaddr* ai_addr;
    struct BionicAddrinfo* ai_next;
};

// bionic's AI_* values. Named here rather than included, because this file is
// compiled against glibc's headers and the whole point is that the two differ.
constexpr int B_AI_PASSIVE = 0x0001;
constexpr int B_AI_CANONNAME = 0x0002;
constexpr int B_AI_NUMERICHOST = 0x0004;
constexpr int B_AI_NUMERICSERV = 0x0008;
constexpr int B_AI_ALL = 0x0100;
constexpr int B_AI_V4MAPPED_CFG = 0x0200;
constexpr int B_AI_ADDRCONFIG = 0x0400;
constexpr int B_AI_V4MAPPED = 0x0800;

int flags_to_host(int b) {
    int g = 0;
    if (b & B_AI_PASSIVE) g |= AI_PASSIVE;
    if (b & B_AI_CANONNAME) g |= AI_CANONNAME;
    if (b & B_AI_NUMERICHOST) g |= AI_NUMERICHOST;
    if (b & B_AI_NUMERICSERV) g |= AI_NUMERICSERV;
    if (b & B_AI_ALL) g |= AI_ALL;
    if (b & B_AI_ADDRCONFIG) g |= AI_ADDRCONFIG;
    // Both of bionic's V4-mapped bits mean the same thing to glibc; the `_CFG`
    // spelling is "if the kernel supports it", which on Linux it does.
    if (b & (B_AI_V4MAPPED | B_AI_V4MAPPED_CFG)) g |= AI_V4MAPPED;
    return g;
}

/// glibc's `EAI_*` are negative and bionic's are positive; nothing about the
/// numbering survives the trip, so a caller checking for `EAI_NONAME` needs the
/// value it was compiled against.
int eai_to_bionic(int g) {
    switch (g) {
        case EAI_BADFLAGS: return 3;
        case EAI_NONAME: return 8;
        case EAI_AGAIN: return 2;
        case EAI_FAIL: return 4;
        case EAI_FAMILY: return 5;
        case EAI_SOCKTYPE: return 10;
        case EAI_SERVICE: return 9;
        case EAI_MEMORY: return 6;
        case EAI_SYSTEM: return 11;
        case EAI_OVERFLOW: return 14;
        case EAI_NODATA: return 7;
        case EAI_ADDRFAMILY: return 1;
        default: return g == 0 ? 0 : 4;
    }
}

void free_bionic_list(BionicAddrinfo* p) {
    while (p) {
        BionicAddrinfo* next = p->ai_next;
        std::free(p->ai_addr);
        std::free(p->ai_canonname);
        std::free(p);
        p = next;
    }
}

} // namespace

extern "C" {

/// bionic-layout `getaddrinfo`, forwarding to the host resolver.
///
/// The result list is copied rather than aliased: every node, its `sockaddr` and
/// its canonical name are owned by this allocation, and the host's list is
/// released before returning. That keeps `freeaddrinfo` below a plain free of
/// our own memory instead of a lifetime shared with glibc's allocator.
int cordial_getaddrinfo(const char* node, const char* service,
                        const BionicAddrinfo* hints, BionicAddrinfo** res) {
    struct addrinfo host_hints;
    struct addrinfo* host_hints_p = nullptr;
    if (hints) {
        std::memset(&host_hints, 0, sizeof host_hints);
        host_hints.ai_flags = flags_to_host(hints->ai_flags);
        // SOCK_* and IPPROTO_* agree everywhere. AF_* agree between bionic and
        // glibc, but not with FreeBSD: bionic's AF_INET6 is 10, FreeBSD's 28, and
        // FreeBSD 10 is AF_CCITT -- a v6-only lookup came back EAI_FAMILY.
#if defined(__FreeBSD__)
        host_hints.ai_family = cordial_fbsd_af_from_linux(hints->ai_family);
        if (host_hints.ai_family < 0) {
            if (res) *res = nullptr;
            return 5; // bionic EAI_FAMILY
        }
#else
        host_hints.ai_family = hints->ai_family;
#endif
        host_hints.ai_socktype = hints->ai_socktype;
        host_hints.ai_protocol = hints->ai_protocol;
        host_hints_p = &host_hints;
    }

    struct addrinfo* out = nullptr;
    int rc = ::getaddrinfo(node, service, host_hints_p, &out);
    if (rc != 0) {
        if (res) *res = nullptr;
        return eai_to_bionic(rc);
    }

    BionicAddrinfo* head = nullptr;
    BionicAddrinfo** tail = &head;
    for (struct addrinfo* g = out; g; g = g->ai_next) {
        auto* b = static_cast<BionicAddrinfo*>(std::calloc(1, sizeof(BionicAddrinfo)));
        if (!b) {
            free_bionic_list(head);
            ::freeaddrinfo(out);
            if (res) *res = nullptr;
            return 6; // bionic EAI_MEMORY
        }
        // ai_flags is an input field; glibc leaves it unset on results, and
        // echoing a host-numbered value back would be a small lie in the
        // caller's own vocabulary.
        b->ai_flags = 0;
        b->ai_socktype = g->ai_socktype;
        b->ai_protocol = g->ai_protocol;
#if defined(__FreeBSD__)
        // FreeBSD's sockaddrs lead with a 1-byte sa_len and a 1-byte family, and
        // number AF_INET6 28. Copied as they were, the engine read an IPv4
        // result's family as 0x0210 and a v6 one as a family it has no name
        // for -- every address it was handed was unusable. Translate to the
        // Linux layout the engine was compiled against.
        b->ai_family = cordial_fbsd_af_to_linux(g->ai_family);
        b->ai_addrlen = 0;
        if (g->ai_addr && g->ai_addrlen > 0) {
            unsigned char tmp[sizeof(struct sockaddr_storage)];
            size_t n = cordial_fbsd_sockaddr_to_linux(g->ai_addr, tmp, sizeof tmp);
            if (n > 0) {
                b->ai_addr = static_cast<struct sockaddr*>(std::malloc(n));
                if (b->ai_addr) {
                    std::memcpy(b->ai_addr, tmp, n);
                    b->ai_addrlen = static_cast<socklen_t>(n);
                }
            }
        }
#else
        b->ai_family = g->ai_family;
        b->ai_addrlen = g->ai_addrlen;
        if (g->ai_addr && g->ai_addrlen > 0) {
            b->ai_addr = static_cast<struct sockaddr*>(std::malloc(g->ai_addrlen));
            if (b->ai_addr) {
                std::memcpy(b->ai_addr, g->ai_addr, g->ai_addrlen);
            } else {
                b->ai_addrlen = 0;
            }
        }
#endif
        if (g->ai_canonname) {
            b->ai_canonname = strdup(g->ai_canonname);
        }
        *tail = b;
        tail = &b->ai_next;
    }
    ::freeaddrinfo(out);

    if (res) *res = head; else free_bionic_list(head);
    return 0;
}

void cordial_freeaddrinfo(BionicAddrinfo* p) {
    free_bionic_list(p);
}

} // extern "C"

extern "C" struct CordialNetdbSymbol {
    const char* name;
    void* addr;
};

extern "C" const CordialNetdbSymbol* cordial_netdb_symbols(size_t* count) {
    static const CordialNetdbSymbol table[] = {
        {"getaddrinfo", (void*)&cordial_getaddrinfo},
        {"freeaddrinfo", (void*)&cordial_freeaddrinfo},
    };
    *count = sizeof(table) / sizeof(table[0]);
    return table;
}
