#pragma once

/// `androidx.core.graphics.Insets`, shared between the two files that need it.
///
/// **It is in a header so a hook can *return* it, which is the whole point.**
/// libjnivm derives a method's JNI descriptor from its C++ signature, so a hook
/// declared `std::shared_ptr<Object>` binds as `()Ljava/lang/Object;` whatever
/// it actually hands back. `GameActivity.getWaterfallInsets` is declared in the
/// dex as `()Landroidx/core/graphics/Insets;`, so it did not bind at all --
/// silently, in both directions, which is the failure mode
/// `tools/hook_descriptors.py` exists to catch and had been reporting for
/// weeks (issue #11). Returning the real type is the fix, and the real type
/// has to be visible where the hook is written.
///
/// Four fields. Zero on every one is the correct answer and, unusually here,
/// not a placeholder: Cordial's window has no status bar, no navigation bar,
/// no display cutout and no gesture areas, so there is genuinely nothing for
/// the engine to inset its layout by. A phone's values invented here would
/// push Roblox's UI inward from edges that do not exist.
///
/// Registering the fields matters separately from returning the object: an
/// unresolved *field* is not the same as a field that reads zero.

#include <jnivm.h>

#include <memory>

namespace cordial {

class Insets : public jnivm::Object {
public:
    jint left = 0;
    jint top = 0;
    jint right = 0;
    jint bottom = 0;

    static std::shared_ptr<Insets> Create(jnivm::ENV* env) {
        auto p = std::make_shared<Insets>();
        // The same registration step every other class here performs on
        // construction. Spelled out rather than calling the `to_jni` helper
        // each .cpp defines privately: this header is included by both of
        // them, and a third copy of a one-line template is worse than the
        // call it wraps.
        jnivm::JNITypes<std::shared_ptr<Insets>>::ToJNIType(env, p);
        return p;
    }

    /// Make the class known, without hooking anything on it.
    ///
    /// **Order matters and cost a debugging session.** `jni_shim.cpp` registers
    /// the GameActivity classes *before* the init-params ones, and
    /// `GameActivity.getWaterfallInsets` now returns this type -- so libjnivm
    /// has to resolve `androidx/core/graphics/Insets` while deriving that
    /// hook's descriptor, before `Register` below has ever run. It could not,
    /// and threw; the exception unwound through Rust's `extern "C"` boundary
    /// and the process died on `fatal runtime error: Rust cannot catch foreign
    /// exceptions` immediately after `JNI_OnLoad` was found, with nothing
    /// naming the class.
    ///
    /// Separate from `Register` rather than merged into it because the fields
    /// must be hooked exactly once, and this may be called before the owner of
    /// those fields has had its turn. `GetClass` is get-or-create, so calling
    /// it twice is fine; `HookInstance` twice is not something to find out
    /// about later.
    static void Declare(jnivm::ENV* env) {
        env->GetClass<Insets>("androidx/core/graphics/Insets");
    }

    static void Register(jnivm::ENV* env) {
        Declare(env);
        auto c = env->GetClass("androidx/core/graphics/Insets");
#define CORDIAL_INSETS_FIELD(name) c->HookInstance(env, #name, &Insets::name)
        CORDIAL_INSETS_FIELD(left);
        CORDIAL_INSETS_FIELD(top);
        CORDIAL_INSETS_FIELD(right);
        CORDIAL_INSETS_FIELD(bottom);
#undef CORDIAL_INSETS_FIELD
    }
};

} // namespace cordial
