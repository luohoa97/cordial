// Driving AGDK `GameActivity` bring-up.
//
// On Android the platform calls `GameActivity.initializeNativeCode` from Java
// with a real Activity behind it. Cordial has no Java, so it constructs the
// arguments through libjnivm and calls the exported JNI native directly:
//
//     jlong Java_com_google_androidgamesdk_GameActivity_initializeNativeCode(
//         JNIEnv*, jobject thiz,
//         jstring internalDataPath, jstring obbPath, jstring externalDataPath,
//         jobject assetMgr, jbyteArray savedState, jobject config)
//
// The descriptor came out of the shipping APK with tools/dex_method.py, not from
// AGDK's source — it changes between versions and Roblox ships whichever it
// built against.
//
// The returned handle is what every later callback carries:
// onSurfaceCreatedNative, onSurfaceChangedNative, onKeyDownNative and the rest.

#include <jnivm.h>

#include "insets.h"

#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

/// One contact as it crosses the C ABI, mirrored on the Rust side as
/// `cordial_linker_sys::game_activity::TouchContact`. The two definitions are
/// the same three words in the same order and have to stay that way.
///
/// At file scope rather than inside `namespace cordial` because it appears in
/// the signature of an `extern "C"` entry point at the bottom of this file, and
/// an elaborated `struct CordialTouchContact` written there would silently
/// declare a *second*, unrelated type rather than referring to this one.
struct CordialTouchContact {
    int id;
    float x, y;
};

namespace cordial {
std::shared_ptr<jnivm::Object> make_display_metrics(jnivm::ENV* env);
/// Defined beside `Insets` in init_params.cpp. Declared rather than
/// duplicated for the same reason make_display_metrics is: one class, one
/// definition, and the insets the engine gets here are the same object the
/// rest of the framework layer hands out.
std::shared_ptr<Insets> cordial_make_zero_insets(jnivm::ENV* env);
std::shared_ptr<jnivm::Object> make_resources(jnivm::ENV* env);
void set_display_size(int width, int height);

/// What `GameActivity.bootstrapTheApp()` runs, installed by the host side.
///
/// A plain function pointer rather than anything richer because it is read on
/// the engine's own thread from inside `initializeNativeCode`, and whatever the
/// host wants to keep hold of it can keep on its own side. See the hook itself
/// for why this matters at all.
using BootstrapFn = void (*)();
static BootstrapFn g_bootstrap = nullptr;
BootstrapFn bootstrap_callback() { return g_bootstrap; }

/// Convert a C++ object into a `jobject` the way libjnivm expects.
///
/// A raw `cordial::to_jni(env, p)` looks right — libjnivm does
/// represent a `jobject` as its own `Object*` — but it skips the two things
/// `ToJNIType` does on the way:
///
///   * it sets `obj->clazz`, without which `GetObjectClass` returns null and
///     libjnivm falls back to `FindClass("Invalid")`. Every field and method
///     lookup on the object then resolves against the wrong class and yields
///     nothing.
///   * it parks the `shared_ptr` in the environment's local frame, which is what
///     keeps the object alive for the duration of the call.
///
/// The failure is silent: the call succeeds, the engine reads its parameters
/// through a classless receiver, gets nothing, and carries on into its failure
/// path.
template <class T>
static auto to_jni(jnivm::ENV* env, const std::shared_ptr<T>& p) {
    return jnivm::JNITypes<std::shared_ptr<T>>::ToJNIType(env, p);
}
jnivm::ENV* process_env();

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

namespace {
std::shared_ptr<String> jstr(const char* v) {
    return std::make_shared<String>(std::string(v ? v : ""));
}
} // namespace

/// `java.lang.ClassLoader`
///
/// Roblox resolves classes by name at runtime rather than only through
/// `FindClass`, so it asks the current `Class` for its loader and calls
/// `loadClass`. Both resolve to the same place: whatever libjnivm has registered.
class ClassLoader : public Object {
public:
    static std::shared_ptr<jnivm::Class> loadClass(ENV* env, Object*, std::shared_ptr<String> name) {
        if (!name) {
            return nullptr;
        }
        // Java uses dots, the JNI uses slashes, and callers are inconsistent
        // about which they pass here.
        std::string path = *name;
        for (auto& ch : path) {
            if (ch == '.') {
                ch = '/';
            }
        }
        return env->GetClass(path.c_str());
    }

    static void Register(ENV* env) {
        env->GetClass<ClassLoader>("java/lang/ClassLoader");
        auto c = env->GetClass("java/lang/ClassLoader");
        c->HookInstanceFunction(env, "loadClass", &ClassLoader::loadClass);
        c->HookInstanceFunction(env, "findClass", &ClassLoader::loadClass);
    }
};

/// `android.content.res.AssetManager`
///
/// Deliberately empty. The native side reaches assets through
/// `AAssetManager_fromJava`, which Cordial answers with its single process-wide
/// manager (see `android::asset`), so this object carries no state — it exists
/// to satisfy `initializeNativeCode`'s signature.
class AssetManager : public Object {
public:
    static void Register(ENV* env) {
        env->GetClass<AssetManager>("android/content/res/AssetManager");
    }
};

/// `android.content.res.Configuration`
///
/// Likewise empty: the native side reads configuration through
/// `AConfiguration_*` rather than this object's fields.
class Configuration : public Object {
public:
    static void Register(ENV* env) {
        env->GetClass<Configuration>("android/content/res/Configuration");
    }
};

/// `com.google.androidgamesdk.GameActivity`, as an object to pass as `thiz`.
class GameActivity : public Object {
public:
    // Both genuine no-ops rather than missing hooks: the engine calls these
    // as part of the same IME bring-up as `InputConnection` below
    // (`setImeEditorInfoFields` right after constructing the editor info,
    // `setWindowFlags` for soft-input-mode-style window flags), and an
    // unresolved call here risks the same silent pending-exception hazard
    // `NativeTextBoxInfo`'s doc comment describes — resolving is the point,
    // not what either does with its arguments, which Cordial has no window
    // manager flags or `EditorInfo` layout to apply.
    void setImeEditorInfoFields(ENV*, jint, jint, jint) {}
    void setWindowFlags(ENV*, jint, jint) {}

    /// `getWindowInsets(int)` and `getWaterfallInsets()`.
    ///
    /// Both return an `androidx.core.graphics.Insets` with every edge zero,
    /// and that is the true answer rather than a placeholder: Cordial's window
    /// has no status bar, no navigation bar, no display cutout and no gesture
    /// exclusion areas, so there is nothing for the engine to inset its layout
    /// by. A phone's values invented here would push Roblox's UI inward from
    /// edges that do not exist.
    ///
    /// They were `Constructed Unresolved symbol` until now, which is worse than
    /// zero insets: the engine asked for a layout constraint and got a null it
    /// then has to interpret. The mask argument is ignored on purpose — every
    /// inset family is zero, so there is nothing for it to select between, and
    /// `WindowInsetsCompat$Type`'s own comment records that its bit values only
    /// have to be distinct for exactly this reason.
    std::shared_ptr<Object> getWindowInsets(ENV* env, jint /*typeMask*/) {
        return cordial_make_zero_insets(env);
    }
    /// **`Insets`, not `Object`, and the difference is whether it binds at
    /// all.** libjnivm derives the descriptor from this signature, so declaring
    /// `Object` bound it as `()Ljava/lang/Object;` while the dex declares
    /// `()Landroidx/core/graphics/Insets;` -- a mismatch libjnivm resolves by
    /// silently never calling the hook. `tools/hook_descriptors.py` had been
    /// printing this one as its only failure; it now prints none. Issue #11.
    ///
    /// `getWindowInsets` above is deliberately left returning `Object`: the
    /// shipping dex does not declare it on `GameActivity` at all, so there is
    /// no descriptor to agree with and the tool does not flag it. Changing a
    /// signature the dex says nothing about would be guessing at a contract
    /// rather than matching one.
    std::shared_ptr<Insets> getWaterfallInsets(ENV* env) {
        return cordial_make_zero_insets(env);
    }

    /// `bootstrapTheApp()` — the app's startup, called by the engine and until
    /// now answered by nobody.
    ///
    /// This is the one that decides the flags verdict. A traced startup run
    /// shows the engine resolving `getNativeHelper`, then:
    ///
    ///     Constructed Unresolved symbol, Class=`com/google/androidgamesdk/
    ///       GameActivity`, Method=`bootstrapTheApp`, Signature=`()V`
    ///     Call Unknown Member Function ... bootstrapTheApp ()V
    ///     Found symbol, Class=`com/roblox/client/startup/NativeHelper`,
    ///       Method=`gameActivity_onFlagsFailed`, Signature=`()V`
    ///
    /// — three consecutive lines. The engine calls the app's bootstrap, gets a
    /// placeholder that does nothing, looks for flags, finds none, and reports
    /// failure. That check happens *inside* `initializeNativeCode`, which is why
    /// two days of varying the settings call changed nothing: Cordial delivered
    /// the document correctly and did it after the verdict had already been
    /// reached.
    ///
    /// The dex declares it on `com/roblox/client/startup/MainGameActivity`, the
    /// subclass; the engine looks it up on `com/google/androidgamesdk/
    /// GameActivity`, the base. libjnivm does not walk a superclass chain the
    /// way ART does, so it is registered here, on the class the engine actually
    /// asks.
    ///
    /// The work itself belongs to Cordial's host-application side, which owns
    /// the settings document and the flag-name list, so this forwards to a
    /// callback installed before `initializeNativeCode` runs. With no callback
    /// installed it says so rather than returning quietly — an unanswered
    /// bootstrap that looks answered is how this cost two days.
    void bootstrapTheApp(ENV*) {
        auto fn = cordial::bootstrap_callback();
        if (!fn) {
            fprintf(stderr,
                    "[roblox] bootstrapTheApp: no bootstrap installed; the engine "
                    "will report onFlagsFailed\n");
            return;
        }
        fn();
    }

    static void Register(ENV* env) {
        env->GetClass<GameActivity>("com/google/androidgamesdk/GameActivity");
        auto c = env->GetClass("com/google/androidgamesdk/GameActivity");
        c->HookInstanceFunction(env, "setImeEditorInfoFields", &GameActivity::setImeEditorInfoFields);
        c->HookInstanceFunction(env, "setWindowFlags", &GameActivity::setWindowFlags);
        c->HookInstanceFunction(env, "getWindowInsets", &GameActivity::getWindowInsets);
        c->HookInstanceFunction(env, "getWaterfallInsets", &GameActivity::getWaterfallInsets);
        c->HookInstanceFunction(env, "bootstrapTheApp", &GameActivity::bootstrapTheApp);
    }
};

/// `InputDevice.SOURCE_*` and `MotionEvent.TOOL_TYPE_*`, the two fields that
/// together say what produced an event.
///
/// `SOURCE_CLASS_POINTER` is 0x2 in both, so a mouse is 0x2000|0x2 and a
/// touchscreen 0x1000|0x2. They travel in pairs everywhere below: claiming to
/// be a mouse in one and a finger in the other is the kind of inconsistency an
/// input stack is entitled to reject.
constexpr jint kSourceMouse = 0x00002002;
constexpr jint kSourceTouchscreen = 0x00001002;
constexpr jint kToolTypeFinger = 1;
constexpr jint kToolTypeMouse = 3;

// There used to be an `input_is_touch()` here: one `static const bool` read out
// of `CORDIAL_INPUT_TOUCH` at first use, seeding `source` and `toolType` for
// every event the process would ever build. Its own comment already named the
// bug — "an input device that changed identity mid-session would be a stranger
// thing than either choice" — and the answer is that a *device* does not
// change identity but a *seat* has several, and which one moved is a per-event
// fact. A laptop with a touchscreen and a trackpad had one of the two
// mislabelled for the whole session, whichever way the variable went.
//
// So the fields below are set by the factory that builds the event, from the
// device that produced it, and nothing in this file reads the environment any
// more. On a real phone the APK's own Java is what reads
// `MotionEvent.getSource()` and picks a native; Cordial replaces that Java, so
// the routing is ours and it lives one layer up in `android::input`, where the
// origin of an event is still known.

/// One contact in a `MotionEvent`: where it is, and which contact it is.
///
/// `id` is Android's *pointer id* — stable for as long as that finger stays
/// down — which is a different number from the pointer *index* AGDK addresses
/// it by. They agree until a finger that is not the last one lifts, and every
/// multi-touch bug worth having comes from assuming they always do.
struct MotionPointer {
    jint id;
    jfloat x, y;
};

/// `android.view.MotionEvent`, synthesised from a host pointer or touch event.
///
/// `onTouchEventNative`'s signature carries the event's scalar fields as
/// unpacked primitive arguments (see `cordial_game_activity_touch`, below) — that
/// unpacking is exactly what AGDK's own Java-side `processMotionEvent` does
/// before calling the native. What is *not* unpacked, and has to come from this
/// object when the native side (`GameActivityMotionEvent_fromJava` in AGDK's own
/// C++, statically linked into libroblox.so) asks for it, is the per-pointer
/// data: `getPointerId`, `getToolType`, `getRawX`/`getRawY` (SDK 29+), and
/// `getAxisValue` for whichever axes AGDK has enabled — X and Y by default,
/// which is all a single mouse pointer needs.
///
/// The single-pointer half of that mapping is not guessed: it is read directly
/// out of AGDK's own `GameActivityEvents.cpp` (Apache-2.0, google/agdk via the
/// game-activity prefab), which is the code that actually calls back onto
/// whatever object Roblox was handed here. **The multi-pointer half is
/// `INFERRED`**, because there is no copy of that file on this machine any
/// more: that a real `pointerCount` with per-index accessors and an
/// `ACTION_POINTER_DOWN` carrying its index in bits 8-15 is what AGDK expects
/// is Android's own documented `MotionEvent` contract, not something read back
/// off the code that consumes it here. Fetching the prefab again would turn
/// that back into a lookup.
///
/// With `historySize` always 0 in this implementation (no motion coalescing),
/// `getHistoricalEventTime`/`getHistoricalAxisValue` are registered — because
/// AGDK unconditionally resolves the method IDs at `initializeNativeCode` time
/// — but never actually invoked. The Waydroid capture agrees that this costs
/// nothing: `AndroidProcessHistoricalTouchEvents = false` on the real client.
class MotionEvent : public Object {
public:
    jint deviceId = 1;
    // A mouse unless the factory that built this event says otherwise, and set
    // per event rather than once per process. Roblox's Android UI may bind only
    // touch handlers, in which case a perfectly well-formed mouse event is
    // consumed by the input dispatcher and then ignored by the interface —
    // which is exactly the symptom: onTouchEventNative returns true and nothing
    // moves. That is a reason to send fingers *as* fingers when there are
    // fingers, not a reason to relabel a mouse.
    jint source = kSourceMouse;
    jint action = 0;
    jlong eventTime = 0, downTime = 0;
    jint flags = 0, metaState = 0, actionButton = 0, buttonState = 0;
    /// Every contact in this event, in *pointer index* order — the order
    /// Android and AGDK both address them by, and the order `getPointerId`
    /// translates out of.
    ///
    /// Never empty. A mouse and a wheel are one contact and fill slot 0;
    /// `getPointerCount() == 0` is not something Android can produce and
    /// nothing downstream checks for it.
    std::vector<MotionPointer> pointers{MotionPointer{0, 0.0f, 0.0f}};
    // Paired with `source` above and set by the same factory; see the
    // `kSource*`/`kToolType*` constants for why the two never disagree.
    jint toolType = kToolTypeMouse;
    // The wheel, in detents: +1 is one notch away from the user (or one notch
    // to the right), matching what `android.view.MotionEvent` documents for
    // AXIS_VSCROLL/AXIS_HSCROLL. Zero on every event that is not ACTION_SCROLL,
    // which is all of them except the ones `cordial_game_activity_scroll`
    // makes.
    jfloat vscroll = 0.0f, hscroll = 0.0f;

    /// The contact at a pointer *index*, clamped rather than trusted.
    ///
    /// AGDK reads indices 0..`getPointerCount()-1` and so cannot go out of
    /// range unless this file has got its own count wrong — but "cannot" and
    /// "does not" differ by one undefined-behaviour read of a `std::vector`,
    /// and a caller that has already been handed a wrong count is exactly the
    /// caller that will ask. Slot 0 always exists, so answering with it is the
    /// bounded wrong answer rather than the unbounded one.
    const MotionPointer& at(jint index) const {
        if (index < 0 || static_cast<size_t>(index) >= pointers.size()) {
            return pointers.front();
        }
        return pointers[static_cast<size_t>(index)];
    }

    jint getPointerId(ENV*, jint index) { return at(index).id; }
    // One tool type for the whole event: every contact in a `MotionEvent` here
    // comes from the same device, because Cordial builds one event per device
    // rather than merging a finger and a mouse into a single dispatch the way a
    // real Android InputReader could.
    jint getToolType(ENV*, jint) { return toolType; }
    jfloat getRawX(ENV*, jint index) { return at(index).x; }
    jfloat getRawY(ENV*, jint index) { return at(index).y; }
    jfloat getXPrecision(ENV*) { return 1.0f; }
    jfloat getYPrecision(ENV*) { return 1.0f; }
    // AMOTION_EVENT_AXIS_X = 0, AXIS_Y = 1, AXIS_VSCROLL = 9, AXIS_HSCROLL = 10.
    //
    // X and Y are the two AGDK enables by default (GameActivityEvents.cpp's
    // `enabledAxes`); the scroll pair is only ever read if something raises
    // that mask, and returning a real value costs nothing if nothing does.
    // Populating them without also sending ACTION_SCROLL would be the useless
    // half of the pair — an axis nothing asks about on an event that does not
    // say a wheel moved — so the two landed together, and the scroll path in
    // `cordial_game_activity_scroll` is the only thing that sets them.
    jfloat getAxisValue(ENV*, jint axis, jint index) {
        if (axis == 0) return at(index).x;
        if (axis == 1) return at(index).y;
        if (axis == 9) return vscroll;
        if (axis == 10) return hscroll;
        return 0.0f;
    }
    jlong getHistoricalEventTime(ENV*, jint) { return eventTime; }
    jfloat getHistoricalAxisValue(ENV*, jint, jint, jint) { return 0.0f; }

    // Registered for completeness — any caller reading the object directly
    // rather than through the unpacked primitives gets real values instead of
    // an unresolved-symbol stub — but not on AGDK's own call path.
    jint getDeviceId(ENV*) { return deviceId; }
    jint getSource(ENV*) { return source; }
    jint getAction(ENV*) { return action; }
    jlong getEventTime(ENV*) { return eventTime; }
    jlong getDownTime(ENV*) { return downTime; }
    jint getFlags(ENV*) { return flags; }
    jint getMetaState(ENV*) { return metaState; }
    jint getActionButton(ENV*) { return actionButton; }
    jint getButtonState(ENV*) { return buttonState; }
    jint getClassification(ENV*) { return 0; }
    jint getEdgeFlags(ENV*) { return 0; }
    jint getHistorySize(ENV*) { return 0; }
    jint getPointerCount(ENV*) { return static_cast<jint>(pointers.size()); }

    static std::shared_ptr<MotionEvent> Create(ENV* env, jfloat x, jfloat y, jint action,
                                               jint buttonState, jint actionButton,
                                               jlong eventTime, jlong downTime) {
        auto p = std::make_shared<MotionEvent>();
        p->pointers[0] = MotionPointer{0, x, y};
        p->action = action;
        p->buttonState = buttonState;
        p->actionButton = actionButton;
        p->eventTime = eventTime;
        p->downTime = downTime;
        to_jni(env, p);
        return p;
    }

    /// A wheel event: ACTION_SCROLL with the two scroll axes filled in.
    ///
    /// Separate from `Create` rather than two more arguments on it, because a
    /// scroll has no gesture behind it — no down time, no action button, and
    /// no button state that means anything — and threading four zeros through
    /// every existing call site to say so would make the common path harder to
    /// read for the sake of the rare one.
    static std::shared_ptr<MotionEvent> CreateScroll(ENV* env, jfloat x, jfloat y, jfloat hscroll,
                                                     jfloat vscroll, jlong eventTime) {
        auto p = std::make_shared<MotionEvent>();
        p->pointers[0] = MotionPointer{0, x, y};
        // AMOTION_EVENT_ACTION_SCROLL.
        p->action = 8;
        p->hscroll = hscroll;
        p->vscroll = vscroll;
        p->eventTime = eventTime;
        p->downTime = eventTime;
        to_jni(env, p);
        return p;
    }

    /// A finger gesture: one entry per contact currently down, and the source
    /// and tool type that say so.
    ///
    /// Unconditionally touchscreen and finger, the way `Create` is
    /// unconditionally mouse and wheel. Nothing here consults an environment
    /// variable: these contacts arrived on a `wl_touch`, so there is a true
    /// answer to what produced them. Which factory an event reaches is decided
    /// once, in `android::input`, from the device it came in on.
    ///
    /// `action` arrives already packed: for `ACTION_POINTER_DOWN`/`_UP` Android
    /// carries the index of the contact the event is *about* in bits 8-15, and
    /// the caller in `android::input` is where that packing is done and tested.
    static std::shared_ptr<MotionEvent> CreateTouch(ENV* env, const CordialTouchContact* contacts,
                                                    int count, jint action, jlong eventTime,
                                                    jlong downTime) {
        auto p = std::make_shared<MotionEvent>();
        if (contacts && count > 0) {
            p->pointers.clear();
            p->pointers.reserve(static_cast<size_t>(count));
            for (int i = 0; i < count; ++i) {
                p->pointers.push_back(
                    MotionPointer{(jint)contacts[i].id, contacts[i].x, contacts[i].y});
            }
        }
        p->source = kSourceTouchscreen;
        p->toolType = kToolTypeFinger;
        p->action = action;
        p->eventTime = eventTime;
        p->downTime = downTime;
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<MotionEvent>("android/view/MotionEvent");
        auto c = env->GetClass("android/view/MotionEvent");
        c->HookInstanceFunction(env, "getPointerId", &MotionEvent::getPointerId);
        c->HookInstanceFunction(env, "getToolType", &MotionEvent::getToolType);
        c->HookInstanceFunction(env, "getRawX", &MotionEvent::getRawX);
        c->HookInstanceFunction(env, "getRawY", &MotionEvent::getRawY);
        c->HookInstanceFunction(env, "getXPrecision", &MotionEvent::getXPrecision);
        c->HookInstanceFunction(env, "getYPrecision", &MotionEvent::getYPrecision);
        c->HookInstanceFunction(env, "getAxisValue", &MotionEvent::getAxisValue);
        c->HookInstanceFunction(env, "getHistoricalEventTime", &MotionEvent::getHistoricalEventTime);
        c->HookInstanceFunction(env, "getHistoricalAxisValue", &MotionEvent::getHistoricalAxisValue);
        c->HookInstanceFunction(env, "getDeviceId", &MotionEvent::getDeviceId);
        c->HookInstanceFunction(env, "getSource", &MotionEvent::getSource);
        c->HookInstanceFunction(env, "getAction", &MotionEvent::getAction);
        c->HookInstanceFunction(env, "getEventTime", &MotionEvent::getEventTime);
        c->HookInstanceFunction(env, "getDownTime", &MotionEvent::getDownTime);
        c->HookInstanceFunction(env, "getFlags", &MotionEvent::getFlags);
        c->HookInstanceFunction(env, "getMetaState", &MotionEvent::getMetaState);
        c->HookInstanceFunction(env, "getActionButton", &MotionEvent::getActionButton);
        c->HookInstanceFunction(env, "getButtonState", &MotionEvent::getButtonState);
        c->HookInstanceFunction(env, "getClassification", &MotionEvent::getClassification);
        c->HookInstanceFunction(env, "getEdgeFlags", &MotionEvent::getEdgeFlags);
        c->HookInstanceFunction(env, "getHistorySize", &MotionEvent::getHistorySize);
        c->HookInstanceFunction(env, "getPointerCount", &MotionEvent::getPointerCount);
    }
};

/// `android.view.KeyEvent`, synthesised from an X11 key event.
///
/// Unlike `MotionEvent`, `onKeyDownNative`/`onKeyUpNative` carry no unpacked
/// primitives at all — the whole event is this object, and AGDK's
/// `GameActivityKeyEvent_fromJava` (same source file as the MotionEvent mapping
/// above) calls every one of these accessors directly.
/// `com.google.androidgamesdk.gametextinput.State` — the whole editing state.
///
/// Android text fields do not receive keystrokes. They receive *state*: the
/// complete contents of the field, the selection, and any in-progress composing
/// region from an IME. Cordial had no implementation of this, which is why keys
/// reached `onKeyDownNative` and the login form's text boxes stayed empty — the
/// engine resolves these five fields and nothing was ever answering them.
///
/// Fields rather than getters, matching the real class, which libjnivm binds by
/// name and descriptor.
class TextInputState : public Object {
public:
    std::shared_ptr<String> text = std::make_shared<String>(std::string());
    jint selectionStart = 0;
    jint selectionEnd = 0;
    // -1 means "no composing region", which is what a physical keyboard
    // produces — composition is an IME concept and there is no IME here.
    jint composingRegionStart = -1;
    jint composingRegionEnd = -1;

    static void Register(ENV* env) {
        env->GetClass<TextInputState>("com/google/androidgamesdk/gametextinput/State");
        auto c = env->GetClass("com/google/androidgamesdk/gametextinput/State");
        c->HookInstance(env, "text", &TextInputState::text);
        c->HookInstance(env, "selectionStart", &TextInputState::selectionStart);
        c->HookInstance(env, "selectionEnd", &TextInputState::selectionEnd);
        c->HookInstance(env, "composingRegionStart", &TextInputState::composingRegionStart);
        c->HookInstance(env, "composingRegionEnd", &TextInputState::composingRegionEnd);
    }
};

// -------------------------------------------------------------- IME (outbound)
//
// `docs/NEXT.md` §1 traced text entry to AGDK's *inbound* path
// (`onTextInputEventNative`, above) being accepted and ignored by Roblox's own
// interface, and concluded AGDK was not otherwise involved. That conclusion
// was half right: `onTextInputEventNative` genuinely does nothing useful, but
// a live run's jnivm log shows the engine separately reaching for
// `InputConnection.setState`/`setSoftKeyboardActive`/`restartInput` — the
// *outbound* half, engine calling out — and getting `Constructed Unresolved
// symbol` every time, because nothing here had ever constructed an
// `InputConnection` for it to call. That is a different failure from a call
// that runs and is ignored, and it is what `InputConnection` below answers.
//
// State is kept exactly like the existing `g_textbox_*` globals in
// `android_classes.cpp` — written from the engine's thread inside `setState`,
// read from the input thread — because `setState` and `showKeyboard` are
// answering two different questions (what the field's own editing state is,
// versus which field has focus) and conflating their storage would make a
// future bug in one look like a bug in the other.
namespace {
std::mutex g_ime_mutex;
std::string g_ime_text;
int g_ime_selection_start = 0;
int g_ime_selection_end = 0;
int g_ime_composing_start = -1;
int g_ime_composing_end = -1;
/// Bumped on every `setState`/`restartInput`, the same reasoning as
/// `g_textbox_generation`: lets a reader tell "the engine pushed new state"
/// from "nothing changed" without comparing the state itself.
std::atomic<unsigned> g_ime_state_generation{0};
std::atomic<int> g_ime_soft_keyboard_active{0};
} // namespace

extern "C" void cordial_ime_set_state(const char* text, int sel_start, int sel_end,
                                      int comp_start, int comp_end) {
    if (getenv("CORDIAL_TRACE_TEXT")) {
        fprintf(stderr,
                "[cordial] InputConnection.setState text=%zu bytes sel=[%d,%d) composing=[%d,%d)\n",
                text ? strlen(text) : 0, sel_start, sel_end, comp_start, comp_end);
    }
    {
        std::lock_guard<std::mutex> lock(g_ime_mutex);
        g_ime_text = text ? text : "";
        g_ime_selection_start = sel_start;
        g_ime_selection_end = sel_end;
        g_ime_composing_start = comp_start;
        g_ime_composing_end = comp_end;
    }
    g_ime_state_generation.fetch_add(1, std::memory_order_acq_rel);
}

extern "C" void cordial_ime_set_soft_keyboard_active(int active, int flags) {
    if (getenv("CORDIAL_TRACE_TEXT")) {
        fprintf(stderr, "[cordial] InputConnection.setSoftKeyboardActive(%d, flags=%d)\n", active, flags);
    }
    g_ime_soft_keyboard_active.store(active, std::memory_order_release);
}

extern "C" void cordial_ime_restart_input() {
    if (getenv("CORDIAL_TRACE_TEXT")) {
        fprintf(stderr, "[cordial] InputConnection.restartInput\n");
    }
    // `restartInput` means "forget whatever editing session was in progress",
    // which is exactly what bumping the generation without changing the
    // stored text achieves: the next read reseeds against the *next*
    // `setState`, not against state that is now stale.
    g_ime_state_generation.fetch_add(1, std::memory_order_acq_rel);
}

/// Read-side, for `crates/cordial-linker-sys` to expose to `android::input`.
extern "C" unsigned cordial_ime_state_generation() {
    return g_ime_state_generation.load(std::memory_order_acquire);
}
extern "C" int cordial_ime_soft_keyboard_active() {
    return g_ime_soft_keyboard_active.load(std::memory_order_acquire);
}
extern "C" int cordial_ime_state_text(char* buf, int n) {
    if (!buf || n <= 0) return 0;
    std::lock_guard<std::mutex> lock(g_ime_mutex);
    int len = static_cast<int>(g_ime_text.size());
    if (len > n - 1) len = n - 1;
    memcpy(buf, g_ime_text.data(), static_cast<size_t>(len));
    buf[len] = '\0';
    return len;
}
extern "C" void cordial_ime_state_selection(int* start, int* end) {
    std::lock_guard<std::mutex> lock(g_ime_mutex);
    if (start) *start = g_ime_selection_start;
    if (end) *end = g_ime_selection_end;
}

/// `com.google.androidgamesdk.gametextinput.InputConnection`
///
/// The object the engine calls `setState`/`setSoftKeyboardActive`/
/// `restartInput` on. On real Android this is constructed by `GameActivity`'s
/// Java side inside `onCreateInputConnection` and handed to native code via
/// `setInputConnectionNative`; Cordial has no Android view system to trigger
/// that callback, so `cordial_game_activity_set_input_connection` (below)
/// constructs one directly and drives `setInputConnectionNative` itself,
/// simulating what the platform would have done. One instance for the
/// process's life, the same reasoning as `shared_activity`/`shared_surface`.
class InputConnection : public Object {
public:
    void setState(ENV*, std::shared_ptr<TextInputState> state) {
        if (!state) return;
        std::string text = state->text ? static_cast<std::string>(*state->text) : std::string();
        cordial_ime_set_state(text.c_str(), state->selectionStart, state->selectionEnd,
                              state->composingRegionStart, state->composingRegionEnd);
    }
    void setSoftKeyboardActive(ENV*, jboolean active, jint flags) {
        cordial_ime_set_soft_keyboard_active(active ? 1 : 0, flags);
    }
    void restartInput(ENV*) { cordial_ime_restart_input(); }

    static void Register(ENV* env) {
        env->GetClass<InputConnection>("com/google/androidgamesdk/gametextinput/InputConnection");
        auto c = env->GetClass("com/google/androidgamesdk/gametextinput/InputConnection");
        c->HookInstanceFunction(env, "setState", &InputConnection::setState);
        c->HookInstanceFunction(env, "setSoftKeyboardActive", &InputConnection::setSoftKeyboardActive);
        c->HookInstanceFunction(env, "restartInput", &InputConnection::restartInput);
    }
};

class KeyEvent : public Object {
public:
    jint deviceId = 1;
    // InputDevice.SOURCE_KEYBOARD = SOURCE_CLASS_BUTTON(0x1) | 0x100.
    jint source = 0x00000101;
    jint action = 0;
    jlong eventTime = 0, downTime = 0;
    jint flags = 0, metaState = 0, modifiers = 0, repeatCount = 0;
    jint keyCode = 0, scanCode = 0, unicodeChar = 0;

    jint getDeviceId(ENV*) { return deviceId; }
    jint getSource(ENV*) { return source; }
    jint getAction(ENV*) { return action; }
    jlong getEventTime(ENV*) { return eventTime; }
    jlong getDownTime(ENV*) { return downTime; }
    jint getFlags(ENV*) { return flags; }
    jint getMetaState(ENV*) { return metaState; }
    jint getModifiers(ENV*) { return modifiers; }
    jint getRepeatCount(ENV*) { return repeatCount; }
    jint getKeyCode(ENV*) { return keyCode; }
    jint getScanCode(ENV*) { return scanCode; }
    jint getUnicodeChar(ENV*) { return unicodeChar; }

    static std::shared_ptr<KeyEvent> Create(ENV* env, jboolean down, jint keyCode, jint scanCode,
                                            jint metaState, jint repeatCount, jint unicodeChar,
                                            jlong eventTime, jlong downTime) {
        auto p = std::make_shared<KeyEvent>();
        // ACTION_DOWN = 0, ACTION_UP = 1.
        p->action = down ? 0 : 1;
        p->keyCode = keyCode;
        p->scanCode = scanCode;
        p->metaState = metaState;
        p->modifiers = metaState;
        p->repeatCount = repeatCount;
        p->unicodeChar = unicodeChar;
        p->eventTime = eventTime;
        p->downTime = downTime;
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<KeyEvent>("android/view/KeyEvent");
        auto c = env->GetClass("android/view/KeyEvent");
        c->HookInstanceFunction(env, "getDeviceId", &KeyEvent::getDeviceId);
        c->HookInstanceFunction(env, "getSource", &KeyEvent::getSource);
        c->HookInstanceFunction(env, "getAction", &KeyEvent::getAction);
        c->HookInstanceFunction(env, "getEventTime", &KeyEvent::getEventTime);
        c->HookInstanceFunction(env, "getDownTime", &KeyEvent::getDownTime);
        c->HookInstanceFunction(env, "getFlags", &KeyEvent::getFlags);
        c->HookInstanceFunction(env, "getMetaState", &KeyEvent::getMetaState);
        c->HookInstanceFunction(env, "getModifiers", &KeyEvent::getModifiers);
        c->HookInstanceFunction(env, "getRepeatCount", &KeyEvent::getRepeatCount);
        c->HookInstanceFunction(env, "getKeyCode", &KeyEvent::getKeyCode);
        c->HookInstanceFunction(env, "getScanCode", &KeyEvent::getScanCode);
        c->HookInstanceFunction(env, "getUnicodeChar", &KeyEvent::getUnicodeChar);
    }
};

namespace {
/// The single `GameActivity` `thiz` shared by every native call in this
/// process, the way Android hands the real Activity to all of them. A fresh
/// object per call (the previous behaviour here) is a needless difference
/// from what the engine actually receives — `GameActivity` carries no
/// per-call state in this file — and there is exactly one Activity to model.
std::shared_ptr<GameActivity>& shared_activity(ENV* env) {
    static std::shared_ptr<GameActivity> activity;
    if (!activity) {
        activity = std::make_shared<GameActivity>();
        to_jni(env, activity);
    }
    return activity;
}

/// The `Surface` object threaded through `onSurfaceCreatedNative`,
/// `onSurfaceChangedNative` and `onSurfaceRedrawNeededNative` — the three
/// calls Android makes against one surface's lifetime, which
/// `onSurfaceDestroyedNative` ends. A fresh object per call would let engine
/// code that compares `Surface` identity (e.g. "is this a resize of the
/// surface I already have, or a new one?") see every call as an unrelated new
/// surface. `make_new` starts a new lifetime; only `onSurfaceCreatedNative`
/// passes true.
std::shared_ptr<jnivm::Object>& shared_surface(ENV* env, bool make_new) {
    static std::shared_ptr<jnivm::Object> surface;
    if (make_new || !surface) {
        surface = std::make_shared<jnivm::Object>();
        to_jni(env, surface);
    }
    return surface;
}

/// The single `InputConnection` handed to `setInputConnectionNative`, for the
/// same reason `shared_activity` is single: the engine holds a reference to
/// whichever object it was given and calls back on it for the rest of the
/// session, so a second, different instance later would mean any state the
/// engine associates with the first is silently orphaned.
std::shared_ptr<InputConnection>& shared_input_connection(ENV* env) {
    static std::shared_ptr<InputConnection> ic;
    if (!ic) {
        ic = std::make_shared<InputConnection>();
        to_jni(env, ic);
    }
    return ic;
}
} // namespace

void register_game_activity_classes(ENV* env) {
    // Before `GameActivity`, whose `getWaterfallInsets` returns one. See
    // `Insets::Declare` for what happens when it is not.
    Insets::Declare(env);
    ClassLoader::Register(env);
    AssetManager::Register(env);
    Configuration::Register(env);
    GameActivity::Register(env);
    MotionEvent::Register(env);
    KeyEvent::Register(env);
    TextInputState::Register(env);
    InputConnection::Register(env);
}

/// `GameActivity.onTouchEventNative(J, MotionEvent, IIIIIJJIIIIIIFF) -> Z`.
///
/// The one place that call is made. Three entry points below build three
/// different `MotionEvent`s — a mouse pointer, a wheel, and a set of finger
/// contacts — and every one of them then makes the identical eighteen-argument
/// call. It was written out twice before the third arrived, and the two copies
/// had already drifted: the wheel's passed literal zeros where the pointer's
/// passed the event's own `actionButton`/`buttonState`, which was harmless
/// only because a scroll has neither. Reading every scalar back off the event
/// object closes that by construction — there is now exactly one description
/// of what the engine is told, and it is the object AGDK will call back into.
///
/// `Build` is a callable taking `ENV*` and returning the event, so that
/// construction happens inside the local frame this pushes. Returns 0 with
/// `*consumed` set, -1 on error, or -2 if the native is not registered yet —
/// the ordinary race against `initializeNativeCode` during startup.
template <typename Build>
int deliver_motion(long handle, Build&& build, int* consumed, char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or no native handle");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }
        void* fn;
        {
            std::lock_guard<std::mutex> lock(cls->mtx);
            auto it = cls->natives.find("onTouchEventNative");
            fn = it == cls->natives.end() ? nullptr : it->second;
        }
        if (!fn) {
            return -2;
        }

        // Wrapped in `PushLocalFrame`/`PopLocalFrame`: unlike the
        // once-per-launch calls elsewhere in this file, this runs once per
        // input event, and `cordial::to_jni` parks every object it touches in
        // the current local frame — without popping, a long session would grow
        // that frame without bound. The contacts are plain C++ in the event
        // object rather than a Java array, so the frame holds two references
        // however many fingers are down.
        jni->PushLocalFrame(8);

        auto jactivity = cordial::to_jni(env, cordial::shared_activity(env));
        auto event = build(env);
        auto jevent = cordial::to_jni(env, event);

        using TouchFn = jboolean (*)(JNIEnv*, jobject, jlong, jobject, jint, jint, jint, jint,
                                     jint, jlong, jlong, jint, jint, jint, jint, jint, jint,
                                     jfloat, jfloat);
        jboolean r = reinterpret_cast<TouchFn>(fn)(
            jni, jactivity, (jlong)handle, jevent,
            /*pointerCount=*/(jint)event->pointers.size(), /*historySize=*/0,
            /*deviceId=*/event->deviceId, /*source=*/event->source,
            /*action=*/event->action, /*eventTime=*/event->eventTime,
            /*downTime=*/event->downTime, /*flags=*/event->flags,
            /*metaState=*/event->metaState, /*actionButton=*/event->actionButton,
            /*buttonState=*/event->buttonState, /*classification=*/0, /*edgeFlags=*/0,
            /*precisionX=*/1.0f, /*precisionY=*/1.0f);

        jni->PopLocalFrame(nullptr);
        if (consumed) {
            *consumed = r ? 1 : 0;
        }
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

} // namespace cordial

extern "C" {

/// Install what `GameActivity.bootstrapTheApp()` should run.
///
/// Must be called before `cordial_game_activity_init`: the engine calls
/// `bootstrapTheApp` from inside `initializeNativeCode` and reads the flags
/// verdict on the very next line, so anything installed afterwards is too late
/// by construction — which is exactly the bug this exists to fix.
void cordial_set_bootstrap(void (*fn)()) { cordial::g_bootstrap = fn; }

/// Call `initializeNativeCode` and return its handle, or 0.
///
/// `err` receives a message on failure. Exceptions are contained here for the
/// same reason as in jni_shim.cpp: one crossing the Rust boundary is a core dump
/// with no explanation.
long cordial_game_activity_init(void* fn, const char* internal_path, const char* obb_path,
                                const char* external_path, char* err, size_t err_len) {
    using Init = jlong (*)(JNIEnv*, jobject, jstring, jstring, jstring, jobject, jbyteArray,
                           jobject);
    // Taken from the VM here rather than passed in. A `JNIEnv*` and a
    // `jnivm::ENV*` are unrelated types that both arrive as `void*`, and
    // confusing them does not fail at the boundary — it fails much later, as a
    // call through a null slot in what was assumed to be the function table.
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM or no initializeNativeCode");
        return 0;
    }

    try {
        auto activity = std::make_shared<cordial::GameActivity>();
        auto assets = std::make_shared<cordial::AssetManager>();
        auto config = std::make_shared<cordial::Configuration>();

        auto internal = cordial::jstr(internal_path);
        auto obb = cordial::jstr(obb_path);
        auto external = cordial::jstr(external_path);

        // libjnivm represents a `jobject` as its own `Object*`, so the shared_ptrs
        // above convert by taking their raw pointer. They stay in scope for the
        // duration of the call, which is what keeps the objects alive — the
        // engine must not retain them past this without its own reference.
        auto j = [env](const auto& p) { return cordial::to_jni(env, p); };

        return reinterpret_cast<Init>(fn)(
            env->GetJNIEnv(),
            j(activity),
            cordial::to_jni(env, internal),
            cordial::to_jni(env, obb),
            cordial::to_jni(env, external),
            j(assets),
            // savedState is null on a cold start, which this always is.
            nullptr,
            j(config));
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return 0;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return 0;
    }
}

} // extern "C"

extern "C" {

/// Whether to skip half of the AGDK sequence below, and why either would be.
///
/// **None of these is a candidate fix, and each produces a client that renders
/// nothing.** Measured 2026-08-27: skipping either half took the present-freeze
/// from 13/15 to 0/15 with p = 1.75e-06, and photographing the result through
/// the swapchain showed a blank grey window in both arms, byte-identical to
/// each other, presenting about 255 empty frames a second. The comment further
/// down this function had already said why -- the engine builds its renderer on
/// resume -- and the survey scored those blank clients healthy because its
/// verdict counts presents. They are instruments for locating the freeze. Do
/// not ship one.
///
/// **Two working references say this path is not the one the engine expects.**
/// Roblox's own Android build does not take it: `EnableGameActivity8` resolves
/// to false in `docs/traces/waydroid-roblox-startup.log.gz:626`, and across all
/// 2432 lines of that capture no GameActivity native is called even once. And
/// mocktail defaults all three of its equivalent switches off, with its
/// authors' reason written beside them at `src/legacy/legacy_runtime.cc:2867`
/// (Apache-2.0): the glue "currently stalls V2 init on surface flags", and the
/// lifecycle callbacks "can block on `android_app_set_activity_state` in some
/// Linux shims". Read there; the switches here are ours.
///
/// What makes that worth a switch rather than a note is that Cordial delivers
/// the surface through the app bridge as well -- `appbridge_start_app` and
/// both `UpdateSurface...WithPlatformParams` calls, at `load.rs:3849-3899` --
/// so by the time the calls below run, the engine has already been told about
/// this surface twice. These drop each half of the third delivery, separately,
/// because a startup that wedges 80% of the time on a signed-in profile is the
/// symptom mocktail describes and nobody here has ever run the arm without it.
///
/// **Not to be confused with `CORDIAL_SKIP_AGDK`**, which is a far larger
/// switch: it drops `initializeNativeCode` too, and that is what brings the
/// TaskScheduler up. The engine will not load flags behind a live scheduler,
/// so that path dies on `Can't initialize the TaskScheduler before flags have
/// been loaded` about half a second in -- measured three times out of three on
/// 2026-08-27, and explained in `docs/analysis/flag-init.md` 19.1 long before
/// that. These two leave all of it alone and change nothing but the calls.
///
/// Both default off, so every reading taken before 2026-08-27 still describes
/// the build it was taken on.
static bool skip_agdk_surface()
{
    static const bool skip = getenv("CORDIAL_SKIP_AGDK_SURFACE") != nullptr;
    return skip;
}

static bool skip_agdk_lifecycle()
{
    static const bool skip = getenv("CORDIAL_SKIP_AGDK_LIFECYCLE") != nullptr;
    return skip;
}

/// The lifecycle half, split where AGDK itself splits it.
///
/// Measured 2026-08-27, fifteen runs an arm interleaved against fifteen
/// controls on a signed-in profile: skipping all three lifecycle natives took
/// the present-freeze from 9/15 to 2/15, Fisher one-tailed p = 0.011. That is
/// the first intervention that has ever moved this bug rather than described
/// it.
///
/// It is not a fix, and these two exist because of what else that arm did.
/// Counting every shape of stall rather than the survey's threshold it was
/// 4/15 against 9/15, p = 0.14, and two of those runs were a failure the
/// control never produced once -- stuck on the Startup screen past a hundred
/// seconds while presenting twenty-six thousand frames. The engine also
/// stopped logging `StartupController started: stage` in all fifteen, against
/// four of fifteen controls. Something real changed and not all of it was an
/// improvement.
///
/// The split is where the mechanism is. `onStartNative` and `onResumeNative`
/// reach `android_app_set_activity_state`, which writes its command and then
/// **blocks on a condition variable until the engine's own loop acknowledges
/// it**; `onWindowFocusChangedNative` writes and returns. mocktail names that
/// function specifically (`legacy_runtime.cc:2872`, Apache-2.0). A wait for an
/// acknowledgement that never comes is the shape of every capture taken of
/// this freeze: nine events delivered on the command pipe, a tenth that never
/// arrives, and the write end of that pipe open in the same process.
///
/// So these two arms answer different questions. `CORDIAL_SKIP_AGDK_STATE`
/// drops only the pair that can block. `CORDIAL_SKIP_AGDK_FOCUS` drops only
/// the one that cannot, and is the control for it -- if the freeze moves when
/// focus alone goes, the blocking-ack account is wrong and should be dropped.
///
/// **It moved, and the account is dropped.** Fifteen runs an arm against
/// fifteen controls: 0/15 frozen with the blocking pair gone, 0/15 frozen with
/// only the non-blocking call gone, 13/15 frozen with neither. No daylight
/// between them, so waiting on an acknowledgement is not what distinguishes a
/// frozen run. Kept above rather than deleted because a refuted mechanism that
/// still reads plausibly is exactly the one somebody re-derives.
static bool skip_agdk_state()
{
    static const bool skip = getenv("CORDIAL_SKIP_AGDK_STATE") != nullptr;
    return skip || skip_agdk_lifecycle();
}

static bool skip_agdk_focus()
{
    static const bool skip = getenv("CORDIAL_SKIP_AGDK_FOCUS") != nullptr;
    return skip || skip_agdk_lifecycle();
}

/// Drive the Activity lifecycle and hand the engine its surface.
///
/// Android's order is onCreate, onStart, onResume, then the surface callbacks as
/// the window becomes available. AGDK's natives are registered on the
/// `GameActivity` class rather than exported, so they are reached through the
/// JNI method table rather than `dlsym`.
///
/// Every call carries the handle `initializeNativeCode` returned.
int cordial_game_activity_start(long handle, int width, int height, int format,
                                char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or initializeNativeCode gave no handle");
        return -1;
    }

    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }

        // The natives are called directly rather than through GetMethodID and
        // CallVoidMethod, because libjnivm's method lookup deliberately excludes
        // them: `AllowNative == (bool)namesp->native` is false for an ordinary
        // GetMethodID, and CallVoidMethod dispatches on `nativehandle`, which
        // RegisterNatives never sets. Going through JNI silently finds nothing
        // and does nothing — which is exactly what it did.
        //
        // RegisterNatives does keep the raw function pointers, so this looks them
        // up there and calls them with the signatures AGDK registered.
        auto native = [&](const char* name) -> void* {
            std::lock_guard<std::mutex> lock(cls->mtx);
            auto it = cls->natives.find(name);
            return it == cls->natives.end() ? nullptr : it->second;
        };

        // The shared thiz and the surface starting a new lifetime — see
        // `shared_activity`/`shared_surface`'s own doc comments.
        auto jactivity = cordial::to_jni(env, cordial::shared_activity(env));
        auto jsurface = cordial::to_jni(env, cordial::shared_surface(env, /*make_new=*/true));

        using HandleOnly = void (*)(JNIEnv*, jobject, jlong);
        using SurfaceFn = void (*)(JNIEnv*, jobject, jlong, jobject);
        using SurfaceChangedFn = void (*)(JNIEnv*, jobject, jlong, jobject, jint, jint, jint);

        // Say which arm this run is, unconditionally and on stdout, because an
        // arm that silently did nothing has cost this project a day three times
        // -- see the survey script's header on its two dead NUDGE arms. A
        // harness can grep this line to prove the switch engaged rather than
        // assuming it from the environment it thinks it set.
        std::printf("  agdk natives: state=%s focus=%s surface=%s\n",
                    skip_agdk_state() ? "skipped" : "on",
                    skip_agdk_focus() ? "skipped" : "on",
                    skip_agdk_surface() ? "skipped" : "on");
        std::fflush(stdout);

        // Lifecycle first: the engine builds its renderer on resume and ignores a
        // surface that arrives before it is ready for one.
        if (!skip_agdk_state()) {
            if (auto f = native("onStartNative")) {
                reinterpret_cast<HandleOnly>(f)(jni, jactivity, (jlong)handle);
            }
            if (auto f = native("onResumeNative")) {
                reinterpret_cast<HandleOnly>(f)(jni, jactivity, (jlong)handle);
            }
        }

        if (!skip_agdk_surface()) {
            auto created = native("onSurfaceCreatedNative");
            if (!created) {
                snprintf(err, err_len, "onSurfaceCreatedNative was never registered");
                return -1;
            }
            reinterpret_cast<SurfaceFn>(created)(jni, jactivity, (jlong)handle, jsurface);

            // Size and format come after creation; this is what tells the engine how
            // big its framebuffers have to be.
            if (auto f = native("onSurfaceChangedNative")) {
                reinterpret_cast<SurfaceChangedFn>(f)(jni, jactivity, (jlong)handle, jsurface,
                                                      (jint)format, (jint)width, (jint)height);
            }

            // The content rectangle, which on Android arrives from the view layout
            // pass. Nothing here performs one, so the engine would never be told
            // where inside the window it is allowed to draw.
            using RectFn = void (*)(JNIEnv*, jobject, jlong, jint, jint, jint, jint);
            if (auto f = native("onContentRectChangedNative")) {
                reinterpret_cast<RectFn>(f)(jni, jactivity, (jlong)handle, 0, 0, (jint)width,
                                            (jint)height);
            }
        }

        // Focus, last, and it matters more than it looks. An Android game that
        // has never been told it has the window renders as if it were in the
        // background — which is what about one frame per second is. Cordial
        // drove the lifecycle up to onResume and then never sent this.
        //
        // Grouped with the lifecycle half rather than the surface half because
        // that is what it is, and because dropping it alone is how you get the
        // one-frame-a-second reading back.
        using FocusFn = void (*)(JNIEnv*, jobject, jlong, jboolean);
        if (!skip_agdk_focus()) {
            if (auto f = native("onWindowFocusChangedNative")) {
                reinterpret_cast<FocusFn>(f)(jni, jactivity, (jlong)handle, JNI_TRUE);
            }
        }
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

} // extern "C"

extern "C" {

/// A `GameActivity` native shaped `(J)V` — every lifecycle stage that carries
/// no argument beyond the handle: `onPauseNative`, `onStopNative`,
/// `onSurfaceDestroyedNative`, and `terminateNativeCode` itself.
///
/// `terminateNativeCode` is not exported the way `initializeNativeCode` is —
/// `nm -D` on the shipping `libroblox.so` shows only
/// `Java_com_google_androidgamesdk_GameActivity_initializeNativeCode` among
/// its defined symbols. It is one of the 24 natives AGDK registers
/// dynamically through `RegisterNatives` during `initializeNativeCode`,
/// exactly like `onPauseNative` and the rest, so it is looked up here the
/// same way — by name, in `cls->natives` — rather than through `dlsym`.
///
/// Returns 0 on success, -1 on error (`err` populated), or -2 if
/// `native_name` was never registered.
int cordial_game_activity_lifecycle(long handle, const char* native_name, char* err,
                                    size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or no native handle");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }
        void* fn;
        {
            std::lock_guard<std::mutex> lock(cls->mtx);
            auto it = cls->natives.find(native_name);
            fn = it == cls->natives.end() ? nullptr : it->second;
        }
        if (!fn) {
            return -2;
        }
        using HandleOnly = void (*)(JNIEnv*, jobject, jlong);
        auto jactivity = cordial::to_jni(env, cordial::shared_activity(env));
        reinterpret_cast<HandleOnly>(fn)(jni, jactivity, (jlong)handle);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `onWindowFocusChangedNative(J Z)V`, callable in both directions: `true` at
/// bring-up (see `cordial_game_activity_start`, which still drives that call
/// inline) and `false` at teardown — Android sends this immediately before
/// `onPauseNative` when a run ends, the same way it sends the `true` case
/// immediately after `onResumeNative` when one starts.
int cordial_game_activity_window_focus(long handle, int focused, char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or no native handle");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }
        void* fn;
        {
            std::lock_guard<std::mutex> lock(cls->mtx);
            auto it = cls->natives.find("onWindowFocusChangedNative");
            fn = it == cls->natives.end() ? nullptr : it->second;
        }
        if (!fn) {
            return -2;
        }
        using FocusFn = void (*)(JNIEnv*, jobject, jlong, jboolean);
        auto jactivity = cordial::to_jni(env, cordial::shared_activity(env));
        reinterpret_cast<FocusFn>(fn)(jni, jactivity, (jlong)handle,
                                      (jboolean)(focused ? JNI_TRUE : JNI_FALSE));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `onSurfaceRedrawNeededNative(J Landroid/view/Surface;)V` — Android's "the
/// compositor needs a frame before the next normal draw slot" nudge. Driven
/// here from X11 `Expose`: a damaged window (uncovered, restored, redirected
/// through a compositor) would otherwise sit un-repainted until whatever the
/// engine's own next frame happens to be, which on a stalled or backgrounded
/// engine may not come for a while, if at all.
///
/// Uses the *existing* surface (`make_new=false`) — this does not start a new
/// surface lifetime, it re-announces the current one, exactly as Android does
/// when asking an already-created surface to be redrawn.
int cordial_game_activity_surface_redraw_needed(long handle, char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or no native handle");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }
        void* fn;
        {
            std::lock_guard<std::mutex> lock(cls->mtx);
            auto it = cls->natives.find("onSurfaceRedrawNeededNative");
            fn = it == cls->natives.end() ? nullptr : it->second;
        }
        if (!fn) {
            return -2;
        }
        using SurfaceFn = void (*)(JNIEnv*, jobject, jlong, jobject);
        auto jactivity = cordial::to_jni(env, cordial::shared_activity(env));
        auto jsurface = cordial::to_jni(env, cordial::shared_surface(env, /*make_new=*/false));
        reinterpret_cast<SurfaceFn>(fn)(jni, jactivity, (jlong)handle, jsurface);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

} // extern "C"

extern "C" {

/// Deliver a synthesised mouse pointer event through `onTouchEventNative`.
///
/// `action` is a caller-supplied Android `MotionEvent.ACTION_*` constant; this
/// function does not interpret X11 semantics itself, only the AGDK call
/// contract — the X11-to-Android policy (which button maps to which action,
/// hover vs. drag) lives on the Rust side, in `android::window`.
///
/// Returns 0 on success with `*consumed` set to the engine's boolean result,
/// -1 on error (`err` populated), or -2 if `onTouchEventNative` has not been
/// registered yet — a normal race against `initializeNativeCode` early in
/// startup, not a failure worth reporting as one.
///
/// Wrapped in `PushLocalFrame`/`PopLocalFrame`: unlike the once-per-launch
/// calls elsewhere in this file, this runs once per input event, and
/// `cordial::to_jni` parks every object it touches in the current local frame
/// (see its own doc comment) — without popping, a long session would grow that
/// frame without bound.
/// `NativeInputInterface.nativePassMouseMove(F,F,F,F)` and
/// `nativePassMouseButton(F,F,Z,I)`.
///
/// This is the input path Roblox's *interface* actually reads. AGDK's
/// `onTouchEventNative` is a different pipe: it accepts events and returns true,
/// and the engine buffers them, but the Lua app shell never hit-tests anything
/// delivered that way. Feeding only AGDK produced a client where every click was
/// accepted and nothing on screen ever moved — pixel-identical before and after,
/// including hover.
///
/// Signatures read from the shipping APK's dex, not guessed.
/// `GameActivity.onTextInputEventNative(J, State)`.
///
/// Delivers the field's entire contents, not a keystroke. The caller owns the
/// buffer and sends the whole thing each time it changes, which is what the real
/// Android implementation does when an IME edits the text.
int cordial_game_activity_text_input(long handle, const char* text, int sel_start, int sel_end,
                                     char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or no native handle");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }
        auto it = cls->natives.find("onTextInputEventNative");
        if (it == cls->natives.end()) {
            snprintf(err, err_len, "onTextInputEventNative was never registered");
            return -1;
        }
        auto state = std::make_shared<cordial::TextInputState>();
        state->text = std::make_shared<cordial::String>(std::string(text ? text : ""));
        state->selectionStart = sel_start;
        state->selectionEnd = sel_end;

        using Call = void (*)(JNIEnv*, jobject, jlong, jobject);
        reinterpret_cast<Call>(it->second)(jni, (jobject)cordial::to_jni(env, cordial::shared_activity(env)), (jlong)handle,
                                           (jobject)cordial::to_jni(env, state));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `GameActivity.setInputConnectionNative(J, InputConnection)`.
///
/// On real Android, Java calls this once — from inside `onCreateInputConnection`,
/// with the `InputConnection` it just built — to hand native code a reference it
/// then calls back through for the rest of the session. Cordial has no view
/// system to trigger that callback, so this drives it directly: construct one
/// `InputConnection` (see `cordial::shared_input_connection`'s doc for why it is
/// one, kept alive for the process) and call the native the same way Java would
/// have. Meant to run once, early — see the call site in `load.rs` — not per
/// frame.
///
/// Returns 0 on success, -1 on error (`err` populated), or -2 if
/// `setInputConnectionNative` has not been registered yet, the same
/// not-yet-vs-failed distinction `touch`/`key` make.
int cordial_game_activity_set_input_connection(long handle, char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or no native handle");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }
        auto it = cls->natives.find("setInputConnectionNative");
        if (it == cls->natives.end()) {
            snprintf(err, err_len, "setInputConnectionNative was never registered");
            return -2;
        }
        auto ic = cordial::shared_input_connection(env);

        using Call = void (*)(JNIEnv*, jobject, jlong, jobject);
        reinterpret_cast<Call>(it->second)(
            jni, (jobject)cordial::to_jni(env, cordial::shared_activity(env)), (jlong)handle,
            (jobject)cordial::to_jni(env, ic));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeGLInterface.nativePassKeyEvent(Z down, I keyCode, I modifiers, Z isRepeat)`.
///
/// Roblox's own keyboard path, the counterpart to `nativePassMouseButton`. AGDK's
/// `onKeyDownNative` is accepted and ignored by the interface in exactly the way
/// `onTouchEventNative` was.
int cordial_input_key_event(void* fn, int down, int key_code, int modifiers, int is_repeat,
                            char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jboolean, jint, jint, jboolean);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePassKeyEvent is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls),
                                   down ? JNI_TRUE : JNI_FALSE, key_code, modifiers,
                                   is_repeat ? JNI_TRUE : JNI_FALSE);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeGLInterface.nativePassText(J, String, Z, I)` — text entered into a
/// focused text box, which is a different thing from a key being pressed.
int cordial_input_pass_text(void* fn, long long which, const char* text, int flag, int cursor,
                            char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jlong, jstring, jboolean, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePassText is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        auto str = std::make_shared<cordial::String>(std::string(text ? text : ""));
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls),
                                   (jlong)which, (jstring)cordial::to_jni(env, str),
                                   flag ? JNI_TRUE : JNI_FALSE, cursor);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `onSurfaceChangedNative` + `onContentRectChangedNative` for a live resize.
///
/// Bring-up drives these once, from the same place it creates the surface. A
/// window that the user drags to a new size has to drive them again, or the
/// engine keeps rendering at the size it was told at startup while the window
/// is a different shape.
///
/// The surface object is deliberately *not* new: `shared_surface(env, false)`
/// returns the one the engine already has, because this is the same surface
/// changing size rather than a replacement. Passing a fresh object would read
/// to the engine as a surface it has never seen.
int cordial_game_activity_surface_resized(long long handle, int format, int width, int height,
                                          char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env) {
        snprintf(err, err_len, "no JavaVM");
        return -1;
    }
    try {
        auto* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        auto native = [&](const char* name) -> void* {
            std::lock_guard<std::mutex> lock(cls->mtx);
            auto it = cls->natives.find(name);
            return it == cls->natives.end() ? nullptr : it->second;
        };
        auto jactivity = cordial::to_jni(env, cordial::shared_activity(env));
        auto jsurface = cordial::to_jni(env, cordial::shared_surface(env, /*make_new=*/false));

        using SurfaceChangedFn = void (*)(JNIEnv*, jobject, jlong, jobject, jint, jint, jint);
        using RectFn = void (*)(JNIEnv*, jobject, jlong, jint, jint, jint, jint);
        if (auto f = native("onSurfaceChangedNative")) {
            reinterpret_cast<SurfaceChangedFn>(f)(jni, jactivity, (jlong)handle, jsurface,
                                                  (jint)format, (jint)width, (jint)height);
        }
        if (auto f = native("onContentRectChangedNative")) {
            reinterpret_cast<RectFn>(f)(jni, jactivity, (jlong)handle, 0, 0, (jint)width,
                                        (jint)height);
        }
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeGLInterface.updateKeyboardSize(boolean, int, int, int, int)`.
///
/// The acknowledgement that closes the text-entry handshake. Android's order is
/// engine calls `showKeyboard`, the platform opens an IME, and the platform then
/// reports the keyboard's geometry back through this. Until it arrives the
/// engine has focused the box but has not begun capturing — which renders as a
/// focus outline with no blinking caret, and is exactly what Cordial showed
/// while it answered `showKeyboard` and said nothing further.
///
/// The Waydroid capture has the Android side of this as
/// `onUpdateKeyboardSize() v:false x:0 y:999 w:2491 h:0`, which is where the
/// argument order below comes from.
///
/// Cordial reports a zero-height keyboard: there is no soft keyboard taking up
/// screen space on a desktop, and a non-zero height would make the engine shift
/// its layout up to avoid something that is not there.
int cordial_input_update_keyboard_size(void* fn, int visible, int x, int y, int w, int h,
                                       char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jboolean, jint, jint, jint, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or updateKeyboardSize is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls),
                                   visible ? JNI_TRUE : JNI_FALSE, x, y, w, h);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeGLInterface.syncTextboxTextAndCursorPosition2(String, int)`.
///
/// The per-keystroke half of text entry, and the one that was missing. Its dex
/// signature is `(Ljava/lang/String;I)V` — text and cursor, and notably *no*
/// box handle, because it applies to whichever box currently has focus. That is
/// what an IME calls as the user types; `nativePassText` carries a handle and is
/// a different moment in the contract.
///
/// Driving only `nativePassText` left the login form's fields empty even with a
/// correct handle, which is what sent this looking at the declared shapes.
int cordial_input_sync_textbox(void* fn, const char* text, int cursor, char* err,
                               size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jstring, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or syncTextboxTextAndCursorPosition2 is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        auto str = std::make_shared<cordial::String>(std::string(text ? text : ""));
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls),
                                   (jstring)cordial::to_jni(env, str), cursor);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

int cordial_input_mouse_move(void* fn, float x, float y, float dx, float dy, char* err,
                             size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jfloat, jfloat, jfloat, jfloat);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePassMouseMove is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), x, y, dx,
                                   dy);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

int cordial_input_mouse_button(void* fn, float x, float y, int down, int button, char* err,
                               size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jfloat, jfloat, jboolean, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePassMouseButton is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), x, y,
                                   down ? JNI_TRUE : JNI_FALSE, button);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeInputInterface.nativePassMouseWheel(F,F,F)`.
///
/// The wheel's counterpart to `cordial_input_mouse_button`, and the reason the
/// scroll wheel did nothing at all: the export exists, the dex declares it, and
/// nothing here had ever called it. The dex strips parameter names, so which
/// float is which is not readable — but every `nativePassMouse*` in that class
/// begins with the two that `nativePassMouseButton` uses for the cursor
/// position, so `(x, y, delta)` is the shape, `INFERRED` from that family
/// rather than from the one signature alone.
///
/// `delta` is in detents, positive away from the user. See
/// `android::input::pass_mouse_wheel` for why that unit and that sign, and for
/// the knob that flips it without a rebuild.
int cordial_input_mouse_wheel(void* fn, float x, float y, float delta, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jfloat, jfloat, jfloat);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePassMouseWheel is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), x, y,
                                   delta);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeInputInterface.nativePassInput(I pointerId, F x, F y, I action, I width, I height)`.
///
/// Roblox's own touch path, the finger counterpart to `nativePassMouseButton`
/// — one call per contact per event, with the pointer id as a first-class
/// argument rather than something to be read back off a `MotionEvent`.
///
/// **The descriptor is read out of this build's own dex**, not inferred:
/// `nativePassInput(IFFIII)V` on `com/roblox/engine/jni/NativeInputInterface`.
/// **The three `action` values are not.** 0/1/2 for down/move/up come from
/// mocktail (Apache-2.0), which resolves and drives this same export; they are
/// `Enum.UserInputState`'s Begin/Change/End ordering and specifically *not*
/// `MotionEvent`'s, where UP is 1 and MOVE is 2. Nothing readable in this build
/// settles which of the two this native wants, and one session on a machine
/// with a touchscreen would. Until that happens the mapping is `INFERRED` and
/// `CORDIAL_NO_TOUCH=1` is how a user turns off a wrong one.
///
/// `width`/`height` are the surface the coordinates are in. Passed rather than
/// assumed because the engine has been told the canvas size separately through
/// `onSurfaceChangedNative` and the two disagreeing during a resize is a
/// mis-scaled touch, not a crash — which is the kind of bug that gets blamed on
/// the mapping above.
int cordial_input_pass_input(void* fn, int pointer_id, float x, float y, int action, int width,
                             int height, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jint, jfloat, jfloat, jint, jint, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePassInput is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), pointer_id,
                                   x, y, action, width, height);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

// ------------------------------------------------------------------- gamepad
//
// `NativeInputInterface`'s six gamepad natives. Every descriptor below was read
// out of the shipping APK's dex with `tools/dex_method.py` and cross-checked
// against `readelf --dyn-syms` on `libroblox.so`; none of them is guessed. What
// *is* guessed, and is marked INFERRED wherever it appears, is what the integer
// arguments mean -- the dex carries types and not parameter names.
//
// The thing worth knowing before reading any of this: this build has no
// type-less connect entry point. `nativeGamepadConnectEvent` is absent from both
// the dex (`dex_method.py` prints `no match`) and the export table (`readelf |
// grep -c` prints 0), so a pad cannot be announced to this engine without
// supplying a `gamepadType` whose ordinals nothing available here establishes.
// `android::gamepad` is off by default for exactly that reason; that module's
// comment carries the whole argument.
//
// No rumble. `android/os/Vibrator` is declared in the dex and implemented
// nowhere in Cordial, and a force-feedback call that silently does nothing is
// the stub that lies. It is absent rather than stubbed.
//
// The atomic-gating rule these serve -- resolve all of the registration natives
// or use none of them -- is mocktail's (Apache-2.0,
// `src/runtime/roblox_capability_resolver.cc`), which nulls its whole gamepad
// symbol set when the `WithGamepadType` trio is incomplete rather than falling
// back to the removed exports. The idea, not the code.

/// `NativeInputInterface.nativeGamepadConnectEventWithGamepadType(I id, I gamepadType)`.
///
/// The engine remembers the type against the id: `nativeGamepadDisconnectEvent`
/// takes the id alone, so the pairing has to be established here and cannot be
/// restated later.
///
/// `gamepadType` is INFERRED as the trailing argument, not read. Every method on
/// this interface whose name ends `WithGamepadType` is its type-less TV-remote
/// counterpart plus exactly one trailing `int` -- `nativeTVRemoteConnectEvent(I)`
/// against `(II)` here, and `nativeSetTVRemoteSupportedKey(IIZ)` against
/// `(IIZI)`. Three for three is a structural control rather than a hunch, but it
/// is still not an observation.
int cordial_input_gamepad_connect(void* fn, int id, int gamepad_type, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jint, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len,
                 "no JavaVM, or nativeGamepadConnectEventWithGamepadType is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), id,
                                   gamepad_type);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeInputInterface.nativeGamepadDisconnectEvent(I id)`.
///
/// The only one of the six that carries no type, which is itself the evidence
/// that the engine keeps the type it was handed at connect.
int cordial_input_gamepad_disconnect(void* fn, int id, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeGamepadDisconnectEvent is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), id);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeInputInterface.nativeGamepadButtonEvent(I id, I keyCode, I action)`.
///
/// Slot meanings INFERRED, and the weakest of the set. `nativeTVRemoteButtonEvent`
/// carries the identical `(III)` descriptor, which says the two share a shape but
/// not what the shape is. The reading taken here -- id, then an Android
/// `KeyEvent.KEYCODE_BUTTON_*` constant, then an `ACTION_DOWN`/`ACTION_UP` -- is
/// the one the Android platform contract implies, because the Java caller on a
/// real device is handed a `KeyEvent` from an `InputDevice` and has
/// `getKeyCode()` and `getAction()` to forward. Nothing here observed it.
int cordial_input_gamepad_button(void* fn, int id, int key_code, int action, char* err,
                                 size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jint, jint, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeGamepadButtonEvent is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), id,
                                   key_code, action);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeInputInterface.nativeGamepadAxisEvent(I id, I axis, F x, F y, F z)`.
///
/// Three floats for one axis event, which is the shape of a `Vector3` -- and
/// Roblox's Lua `InputObject.Position` for a thumbstick is a `Vector3`, so
/// `(id, axis, x, y, z)` is the reading taken. INFERRED, with no control behind
/// it at all: the TV-remote family has no axis counterpart to difference
/// against, so unlike the three `WithGamepadType` methods there is nothing
/// structural supporting this one. `android::gamepad` sends the unused
/// components as 0.0 rather than repeating a value into them, because an
/// invented number is harder to recognise as wrong than a zero.
int cordial_input_gamepad_axis(void* fn, int id, int axis, float x, float y, float z, char* err,
                               size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jint, jint, jfloat, jfloat, jfloat);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeGamepadAxisEvent is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), id, axis,
                                   x, y, z);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `nativeSetGamepadSupportedKeyWithGamepadType(I id, I keyCode, Z supported, I gamepadType)`.
///
/// The capability declaration: which buttons this pad has, one call per button,
/// before any button event is sent. Wiring button and axis events without first
/// running this would produce a client that looks like it has gamepad support
/// and drops half of it on the floor, so `android::gamepad` refuses to send
/// anything at all until the declaration has been made.
///
/// Slots INFERRED from the difference against `nativeSetTVRemoteSupportedKey(IIZ)`,
/// which is this method minus the trailing type.
int cordial_input_gamepad_supported_key(void* fn, int id, int key_code, int supported,
                                        int gamepad_type, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jint, jint, jboolean, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len,
                 "no JavaVM, or nativeSetGamepadSupportedKeyWithGamepadType is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), id,
                                   key_code, supported ? JNI_TRUE : JNI_FALSE, gamepad_type);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `nativeSetGamepadSupportedMotionWithGamepadType(I id, I axis, I source, Z supported, I gamepadType)`.
///
/// The axis half of the capability declaration, and the least established
/// descriptor of the six: `(IIIZI)` has one more `int` than the key variant and
/// there is no TV-remote counterpart to difference it against, so the middle
/// pair is read here as Android's own way of naming one motion range --
/// `InputDevice.getMotionRange(axis, source)` is keyed on exactly that pair.
/// INFERRED, and the argument names in this signature are a hypothesis rather
/// than a reading. If a logcat capture taken with a pad attached ever lands in
/// `docs/traces/`, this is the line it settles first.
int cordial_input_gamepad_supported_motion(void* fn, int id, int axis, int source, int supported,
                                           int gamepad_type, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jint, jint, jint, jboolean, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len,
                 "no JavaVM, or nativeSetGamepadSupportedMotionWithGamepadType is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeInputInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls), id, axis,
                                   source, supported ? JNI_TRUE : JNI_FALSE, gamepad_type);
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// Deliver a wheel movement through AGDK's `onTouchEventNative` as ACTION_SCROLL.
///
/// The same both-pipes policy the button and move paths already follow: AGDK's
/// contract is real and the engine consumes it, it is simply not what
/// hit-tests the Lua UI. Unlike those two this one is unpacked by hand rather
/// than sharing `cordial_game_activity_touch`, because a scroll carries no
/// pressed button and no gesture start — see `MotionEvent::CreateScroll`.
///
/// Returns 0 / -1 / -2 exactly as `cordial_game_activity_touch` does.
int cordial_game_activity_scroll(long handle, float x, float y, float hscroll, float vscroll,
                                 long long event_time_ms, int* consumed, char* err,
                                 size_t err_len) {
    return cordial::deliver_motion(
        handle,
        [&](cordial::ENV* env) {
            return cordial::MotionEvent::CreateScroll(env, x, y, hscroll, vscroll,
                                                      (jlong)event_time_ms);
        },
        consumed, err, err_len);
}

int cordial_game_activity_touch(long handle, int action, float x, float y, int button_state,
                                int action_button, long long event_time_ms,
                                long long down_time_ms, int* consumed, char* err,
                                size_t err_len) {
    return cordial::deliver_motion(
        handle,
        [&](cordial::ENV* env) {
            return cordial::MotionEvent::Create(env, x, y, action, button_state, action_button,
                                                (jlong)event_time_ms, (jlong)down_time_ms);
        },
        consumed, err, err_len);
}

/// Deliver a set of finger contacts through `onTouchEventNative`.
///
/// The multi-contact counterpart to `cordial_game_activity_touch`, and the
/// reason `MotionEvent` grew a contact vector: the single-contact path hard-
/// coded `pointerCount=1`, `getPointerId` ignored its index argument and
/// returned 0, and `getPointerCount` returned the literal 1 — so a second
/// finger had nowhere to go even though AGDK asks for it by index.
///
/// `contacts` is every contact currently down, in pointer-index order,
/// including the one lifting on an `ACTION_UP`/`ACTION_POINTER_UP` — Android
/// keeps the departing pointer in the array for the event that reports it
/// leaving, and a caller that removes it first reports one fewer finger than
/// were on the glass. `action` arrives already packed with the index for the
/// two `_POINTER_` actions; see `android::input`, which owns that arithmetic
/// and has the tests for it.
///
/// Returns 0 / -1 / -2 exactly as `cordial_game_activity_touch` does.
int cordial_game_activity_touch_multi(long handle, int action,
                                      const struct CordialTouchContact* contacts, int count,
                                      long long event_time_ms, long long down_time_ms,
                                      int* consumed, char* err, size_t err_len) {
    if (!contacts || count <= 0) {
        snprintf(err, err_len, "a touch event with no contacts");
        return -1;
    }
    return cordial::deliver_motion(
        handle,
        [&](cordial::ENV* env) {
            return cordial::MotionEvent::CreateTouch(env, contacts, count, action,
                                                     (jlong)event_time_ms, (jlong)down_time_ms);
        },
        consumed, err, err_len);
}

/// Deliver a synthesised key event through `onKeyDownNative`/`onKeyUpNative`.
///
/// `down` selects which of the two natives is called; both share the single
/// `(J, KeyEvent) -> Z` signature. See `cordial_game_activity_touch`'s doc
/// comment for the return-code convention and the local-frame wrapping.
int cordial_game_activity_key(long handle, int down, int key_code, int scan_code, int meta_state,
                              int repeat_count, int unicode_char, long long event_time_ms,
                              long long down_time_ms, int* consumed, char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!env || handle == 0) {
        snprintf(err, err_len, "no JavaVM, or no native handle");
        return -1;
    }
    try {
        JNIEnv* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/google/androidgamesdk/GameActivity");
        if (!cls) {
            snprintf(err, err_len, "GameActivity class is not registered");
            return -1;
        }
        const char* name = down ? "onKeyDownNative" : "onKeyUpNative";
        void* fn;
        {
            std::lock_guard<std::mutex> lock(cls->mtx);
            auto it = cls->natives.find(name);
            fn = it == cls->natives.end() ? nullptr : it->second;
        }
        if (!fn) {
            return -2;
        }

        jni->PushLocalFrame(8);

        auto jactivity = cordial::to_jni(env, cordial::shared_activity(env));
        auto event = cordial::KeyEvent::Create(env, down ? JNI_TRUE : JNI_FALSE, (jint)key_code,
                                               (jint)scan_code, (jint)meta_state,
                                               (jint)repeat_count, (jint)unicode_char,
                                               (jlong)event_time_ms, (jlong)down_time_ms);
        auto jevent = cordial::to_jni(env, event);

        using KeyFn = jboolean (*)(JNIEnv*, jobject, jlong, jobject);
        jboolean r = reinterpret_cast<KeyFn>(fn)(jni, jactivity, (jlong)handle, jevent);

        jni->PopLocalFrame(nullptr);
        if (consumed) {
            *consumed = r ? 1 : 0;
        }
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

} // extern "C"

// ---------------------------------------------------- registered-native probe
//
// **What natives has the engine actually registered on a class?**
//
// This exists because a claim in `docs/HANDOVER.md` stood for weeks on the
// wrong evidence: voice chat's downlink "cannot be written" because
// `nativeGetPlayoutData` does not appear among `libroblox.so`'s exports. It does
// not, and that proves nothing -- `docs/analysis/jni-natives.tsv` is an `nm -D`
// table, and these natives arrive through `RegisterNatives` at run time. The one
// WebRTC symbol that *is* exported is a loader, which is what registers them.
//
// Cordial has depended on that distinction for months without being able to
// *see* it: `terminateNativeCode` is looked up in `cls->natives` a few hundred
// lines above precisely because it is absent from `nm -D`. So the machinery to
// answer this was already here and there was no way to ask.
//
// Now there is, and it costs one call: the answer to "is this path dead or is
// Cordial simply not on it" is a list rather than an argument.
extern "C" int cordial_registered_natives(const char* class_name, char* out, size_t out_len) {
    if (!class_name || !out || out_len == 0) return -1;
    out[0] = '\0';
    jnivm::ENV* env = cordial::process_env();
    if (!env) {
        snprintf(out, out_len, "no JNI environment yet");
        return -1;
    }
    auto cls = env->GetClass(class_name);
    if (!cls) {
        snprintf(out, out_len, "class not registered");
        return 0;
    }
    // The class's own lock, held only while copying names out -- the same
    // discipline the AGDK lookups above use, and for the same reason: the
    // engine may register more natives at any time.
    std::lock_guard<std::mutex> lock(cls->mtx);
    size_t used = 0;
    int count = 0;
    for (const auto& entry : cls->natives) {
        // The pointer is printed as well as the name, because "registered" and
        // "registered to something real" are different claims and this project
        // has been caught by that distinction before.
        int n = snprintf(out + used, out_len - used, "%s%s@%p",
                         count ? " " : "", entry.first.c_str(), entry.second);
        if (n < 0 || static_cast<size_t>(n) >= out_len - used) break;
        used += static_cast<size_t>(n);
        ++count;
    }
    return count;
}
