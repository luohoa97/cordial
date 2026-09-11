// extern "C" surface over libjnivm, so Rust can stand up a JavaVM.
//
// As with shim.cpp: translation only, no policy.

#include <jnivm.h>
#include "os_compat.h"

extern "C" void cordial_register_android_classes(void* env);
namespace cordial {
void register_game_activity_classes(jnivm::ENV* env);
void register_init_params_classes(jnivm::ENV* env);
}

/// The process VM, for translation units that need the real jnivm::ENV rather
/// than a JNIEnv. Keeping this on the C++ side means Rust never has to hold a
/// type it cannot name — passing a JNIEnv* and casting it to jnivm::ENV* is a
/// silent type confusion that surfaces as a null function pointer several calls
/// later.
namespace cordial { jnivm::ENV* process_env(); }

#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <exception>
#include <execinfo.h>
#include <unistd.h>
#include <stdexcept>
#include <memory>

namespace {
// The VM owns every class, object and environment it hands out, so it outlives
// anything derived from it. One per process.
std::unique_ptr<jnivm::VM> g_vm;

/// Report an uncaught C++ exception with its stack instead of dying mute.
///
/// Roblox spawns worker threads during `JNI_OnLoad` and they call straight back
/// into JNI. libjnivm reports misuse by throwing, and an exception escaping a
/// thread Cordial did not start cannot be caught anywhere — the default is
/// `std::terminate` and a core dump carrying no information about which thread,
/// which call, or why.
/// Write the observed Java surface out, if a VM exists and a path was given.
void dump_classes_now() {
#ifdef JNI_DEBUG
    const char* path = getenv("CORDIAL_JNI_DUMP");
    if (path && g_vm) {
        try {
            g_vm->GenerateClassDump(path);
            fprintf(stderr, "    Java surface Roblox reached for -> %s\n", path);
        } catch (...) {
            fprintf(stderr, "    class dump failed\n");
        }
    }
#endif
}

[[noreturn]] void report_terminate() {
    fprintf(stderr, "\n*** uncaught C++ exception on thread %ld ***\n", cordial_gettid());
    if (auto e = std::current_exception()) {
        try {
            std::rethrow_exception(e);
        } catch (const std::exception& ex) {
            fprintf(stderr, "    what(): %s\n", ex.what());
        } catch (...) {
            fprintf(stderr, "    non-standard exception\n");
        }
    }
    void* frames[32];
    int n = backtrace(frames, 32);
    fprintf(stderr, "    %d frames:\n", n);
    backtrace_symbols_fd(frames, n, 2);

    // Dump before dying. Everything Roblox asked Java for up to this point is
    // the most valuable thing in the process, and it is about to be lost — the
    // failure happens on a thread nobody can catch, so there is no later.
    dump_classes_now();
    _exit(70);
}
} // namespace

namespace {

// A JavaVM whose invocation table logs before delegating. Roblox died before
// asking for a single Java class, so the failure is in the invocation interface
// itself — which of DestroyJavaVM / AttachCurrentThread / DetachCurrentThread /
// GetEnv, called with which JavaVM, was not visible any other way.
JNIInvokeInterface g_traced_iface;
JavaVM g_traced_vm;
const JNIInvokeInterface* g_real_iface = nullptr;
JavaVM* g_real_vm = nullptr;
bool g_trace_invoke = false;

void note(const char* what, JavaVM* vm) {
    if (g_trace_invoke) {
        fprintf(stderr, "[jni] %s(vm=%p)%s\n", what, (void*)vm,
                vm == &g_traced_vm ? " [ours]" : " [NOT ours]");
    }
}

jint traced_destroy(JavaVM* vm) {
    note("DestroyJavaVM", vm);
    return g_real_iface->DestroyJavaVM(g_real_vm);
}
jint traced_attach(JavaVM* vm, JNIEnv** env, void* args) {
    note("AttachCurrentThread", vm);
    return g_real_iface->AttachCurrentThread(g_real_vm, env, args);
}
jint traced_attach_daemon(JavaVM* vm, JNIEnv** env, void* args) {
    note("AttachCurrentThreadAsDaemon", vm);
    return g_real_iface->AttachCurrentThreadAsDaemon(g_real_vm, env, args);
}
jint traced_detach(JavaVM* vm) {
    note("DetachCurrentThread", vm);
    return g_real_iface->DetachCurrentThread(g_real_vm);
}
jint traced_get_env(JavaVM* vm, void** env, jint version) {
    note("GetEnv", vm);
    return g_real_iface->GetEnv(g_real_vm, env, version);
}

} // namespace

extern "C" {

/// Create the process's JavaVM. Returns the `JavaVM*` Roblox expects in
/// `JNI_OnLoad`, or null if one already exists.
void* cordial_jni_create_vm() {
    if (g_vm) {
        return nullptr;
    }
    std::set_terminate(report_terminate);
    g_vm = std::make_unique<jnivm::VM>();
    // Cordial's Java side, before Roblox can ask for any of it.
    cordial_register_android_classes(g_vm->GetEnv().get());
    cordial::register_game_activity_classes(g_vm->GetEnv().get());
    cordial::register_init_params_classes(g_vm->GetEnv().get());
    g_real_vm = g_vm->GetJavaVM();
    g_real_iface = g_real_vm->functions;

    // Copy the table wholesale — reserved0 has to survive, or libjnivm cannot
    // recover its own VM — then replace only the entry points.
    g_traced_iface = *g_real_iface;
    g_traced_iface.DestroyJavaVM = traced_destroy;
    g_traced_iface.AttachCurrentThread = traced_attach;
    g_traced_iface.AttachCurrentThreadAsDaemon = traced_attach_daemon;
    g_traced_iface.DetachCurrentThread = traced_detach;
    g_traced_iface.GetEnv = traced_get_env;
    g_traced_vm.functions = &g_traced_iface;
    g_trace_invoke = getenv("CORDIAL_JNI_TRACE") != nullptr;

    JavaVM* vm = &g_traced_vm;
    // libjnivm recovers its VM from JavaVM::functions->reserved0. If that is not
    // set, every callback Roblox makes throws before it does anything.
    fprintf(stderr, "[jni] JavaVM=%p functions=%p reserved0=%p (expect %p)\n",
            (void*)vm, (void*)(vm ? vm->functions : nullptr),
            vm && vm->functions ? vm->functions->reserved0 : nullptr,
            (void*)g_vm.get());
    return vm;
}

/// The current thread's `JNIEnv*`.
void* cordial_jni_env() {
    return g_vm ? g_vm->GetJNIEnv() : nullptr;
}

/// Write C++ stubs for every Java class and method the native code has reached
/// for so far. This is the Phase 2 backlog, observed rather than guessed.
int cordial_jni_dump_classes(const char* path) {
#ifdef JNI_DEBUG
    if (!g_vm) {
        return -1;
    }
    g_vm->GenerateClassDump(path);
    return 0;
#else
    (void)path;
    return -2;  // built without JNI_DEBUG
#endif
}

/// Call `JNI_OnLoad` with the process JavaVM, containing any C++ exception.
///
/// libjnivm reports misuse by throwing. Those exceptions originate inside
/// Roblox's call stack and would otherwise cross the Rust FFI boundary, where
/// the only outcome is `std::terminate` and a core dump — which says nothing
/// about what went wrong. Catching here turns that into a message.
///
/// Returns the JNI version on success, or one of the negative codes below.
int cordial_jni_call_onload(void* fn, char* err, size_t err_len) {
    using OnLoad = jint (*)(JavaVM*, void*);
    if (!fn || !g_vm) {
        return -1;
    }
    try {
        return reinterpret_cast<OnLoad>(fn)(&g_traced_vm, nullptr);
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -2;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -3;
    }
}

} // extern "C"

namespace cordial {
jnivm::ENV* process_env() {
    if (!g_vm || !g_real_vm) {
        return nullptr;
    }
    // NOT `g_vm->GetEnv()`. That is `jnienvs[pthread_self()]` — an
    // unordered_map::operator[], which for a thread it has never seen
    // default-constructs a null shared_ptr and returns a reference to it. Every
    // JNI hook in this build calls process_env(), and the engine calls those
    // hooks from threads it created itself, so on those threads the old version
    // handed back a null ENV and the engine stored the null where it expected a
    // JNIEnv.
    //
    // AttachCurrentThread is the entry point that does create an env for an
    // unknown thread; on a thread that already has one it finds it and changes
    // nothing. Going through it makes "which thread am I on" stop mattering.
    JNIEnv* env = nullptr;
    if (g_real_vm->AttachCurrentThread(&env, nullptr) != JNI_OK || !env) {
        return nullptr;
    }
    return jnivm::ENV::FromJNIEnv(env);
}
} // namespace cordial
