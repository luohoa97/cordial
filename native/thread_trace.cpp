// `pthread_create`, traced only when asked for.
//
// docs/analysis/flag-init.md §29 traced `RbxStorage::init`'s failing three
// `stat("")` calls to a freshly spawned thread — a real tid, never seen in the
// log before that line, bottoming out at `start_thread`/`__clone3` rather than
// at `do_dlopen`. Nobody had asked who creates that thread or what it runs
// first, because Cordial did not intercept `pthread_create` at all: it is
// fixed-arity and the bionic/glibc layouts agree (`pthread.rs`'s own size
// table), so forwarding it untouched has always been correct and remains the
// default here.
//
// This file adds a wrapper that, off, does exactly what an unwrapped
// `pthread_create` does — one extra call and one `if`, no change to `attr` or
// to which thread runs what. On, it records three facts no debugger session in
// this document managed to get all of at once: who called `pthread_create`
// (the return address into libroblox.so, or wherever it was), what function
// the new thread was told to run, and the tid the kernel gives that thread —
// logged from inside the new thread itself, before it runs a single byte of
// what it was actually asked to do, so it is also an answer to "what does it
// do first" in every trace this produces.
//
// Gated behind `CORDIAL_TRACE_THREADS=1`, matching `CORDIAL_TRACE_PATHS` and
// `CORDIAL_TRACE_PROPS`: off by default, and a plain `fprintf(stderr, …)` per
// creation, not the `printf`-to-stdout libjnivm uses — §29's own instrument
// warning about the two streams buffering differently under redirection
// applies here as much as it did there.

#include "os_compat.h"
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <pthread.h>
#include <sys/syscall.h>
#include <unistd.h>

namespace {

bool g_trace = false;

// libroblox.so is loaded by Cordial's own bionic linker, not the host
// dynamic loader, so the host's `dladdr` has never heard of it and cannot
// resolve an address inside it. `/proc/self/maps` is the one place that
// mapping is recorded regardless of which loader made it. Found once, on
// first use — magic-statics initialisation is thread-safe without a mutex —
// and cached, because thread creation happens throughout the run and
// re-parsing the map file on every one of them would be a needless cost on
// a call this frequent once the game is up.
uintptr_t libroblox_base() {
    static const uintptr_t base = [] {
        FILE* f = std::fopen("/proc/self/maps", "r");
        if (!f) {
            return (uintptr_t)0;
        }
        char line[512];
        uintptr_t found = 0;
        while (std::fgets(line, sizeof line, f)) {
            if (std::strstr(line, "libroblox.so")) {
                unsigned long long start = 0;
                if (std::sscanf(line, "%llx-", &start) == 1) {
                    found = (uintptr_t)start;
                    break;
                }
            }
        }
        std::fclose(f);
        return found;
    }();
    return base;
}

// `addr` printed as `libroblox.so+0x…` when it falls inside that mapping,
// or as a bare address otherwise — a caller or start routine outside
// libroblox.so is itself a fact worth seeing plainly rather than folding into
// a meaningless offset.
void format_addr(char* buf, size_t n, uintptr_t addr) {
    uintptr_t base = libroblox_base();
    if (base != 0 && addr >= base) {
        std::snprintf(buf, n, "libroblox.so+%#lx", (unsigned long)(addr - base));
    } else {
        std::snprintf(buf, n, "%#lx (outside libroblox.so)", (unsigned long)addr);
    }
}

// Carries the real start routine across the `pthread_create` boundary. Freed
// by the trampoline itself once it has read it, on the new thread, before
// calling into Roblox's own function — nothing else ever touches it.
struct ThreadTraceCtx {
    void* (*start_routine)(void*);
    void* arg;
    uintptr_t caller;
    uintptr_t start_routine_addr;
};

void* trampoline(void* raw) {
    ThreadTraceCtx* ctx = static_cast<ThreadTraceCtx*>(raw);
    // SAFETY: gettid() takes no pointer arguments; this is the new thread's
    // own id, read before it does anything else, which is the point.
    long tid = cordial_gettid();

    char caller_s[64];
    char start_s[64];
    format_addr(caller_s, sizeof caller_s, ctx->caller);
    format_addr(start_s, sizeof start_s, ctx->start_routine_addr);
    std::fprintf(stderr, "[threads] tid=%ld spawned by caller=%s start_routine=%s\n",
                 tid, caller_s, start_s);

    void* (*fn)(void*) = ctx->start_routine;
    void* arg = ctx->arg;
    delete ctx;
    return fn(arg);
}

} // namespace

extern "C" int cordial_pthread_create(pthread_t* thread, const pthread_attr_t* attr,
                                       void* (*start_routine)(void*), void* arg) {
    if (!g_trace) {
        return ::pthread_create(thread, attr, start_routine, arg);
    }
    // `__builtin_return_address(0)` reads the address `call` pushed for this
    // frame — a compiler-known fixed slot, not a walk of the frame-pointer
    // chain, so it is exact regardless of `-fomit-frame-pointer` and does not
    // run into the "no frame pointers" caveat that makes anything past the
    // innermost frame guesswork elsewhere in this codebase.
    void* caller = __builtin_return_address(0);
    ThreadTraceCtx* ctx = new ThreadTraceCtx{
        start_routine, arg, (uintptr_t)caller, (uintptr_t)start_routine};
    int rc = ::pthread_create(thread, attr, trampoline, ctx);
    if (rc != 0) {
        // The thread never started, so nothing will reach the `delete` inside
        // `trampoline`.
        delete ctx;
    }
    return rc;
}

/// Turn on the thread-creation log. `CORDIAL_TRACE_THREADS=1` — see the file
/// comment for why this is a separate flag from `CORDIAL_TRACE_PATHS` rather
/// than folding into it.
extern "C" void cordial_set_thread_trace(int on) {
    g_trace = on != 0;
}

extern "C" struct CordialThreadSymbol {
    const char* name;
    void* addr;
};

extern "C" const CordialThreadSymbol* cordial_thread_symbols(size_t* count) {
    static const CordialThreadSymbol table[] = {
        {"pthread_create", (void*)&cordial_pthread_create},
    };
    *count = sizeof(table) / sizeof(table[0]);
    return table;
}
