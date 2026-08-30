// The Java side of Cordial's framework layer.
//
// Roblox's native code calls out to Java for everything the platform is supposed
// to answer. libjnivm hands it stub classes by default, which return null — and
// Roblox notices:
//
//     W/JNIMain  DeviceStaticParams is null.
//
// Each class implemented here replaces one of those nulls. The method surface is
// not guessed: `--dump-classes` records exactly what Roblox reached for, and
// because it only reaches further once it gets a non-null answer, implementing
// one class reveals the next. See docs/analysis/observed-java-surface.md.

#include <jnivm.h>

#include <cstdio>
#include <sys/stat.h>
#include <atomic>
#include <chrono>
#include <cctype>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <vector>
#include <memory>

// ------------------------------------------------------- focused text box
//
// Which text box the engine currently has focus in, learned from
// `showKeyboard` and needed by `nativePassText`. See the comment on
// `NativeGLJavaInterface::showKeyboard` for why this has to be plumbed through
// rather than assumed.
//
// Written from the engine's thread inside `showKeyboard`, read from the input
// thread when a key is dispatched, so it is guarded rather than plain.

/// The `NativeTextBoxInfo` the engine built for the box it is focusing, in the
/// order its constructor takes its arguments: `(FFFFFZIIIIIIZZ)`.
///
/// This is a widget spec, and Cordial needs it because on Android the engine
/// does not draw a focused TextBox's contents at all — the platform does.
/// Established from the APK's own declarations: `com.roblox.client.RbxKeyboard`
/// extends `q.l` extends `android.widget.EditText`, and
/// `res/layout/activity_game.xml` declares one over the GL surface with
/// `background=@android:color/transparent` and `visibility=gone`. Android
/// raises a real, invisible text editor on top of the game and lets it paint
/// the characters.
///
/// The fourteen values were previously accepted and dropped, on the reasoning
/// that a desktop has no on-screen keyboard so none of this applies. That
/// mistook "no soft keyboard" for "no editor": there is still nothing drawing
/// the text, which is why a focused box stays blank while typing even though
/// the keystrokes now reach the engine.
///
/// Several slots are still named for their position rather than their meaning,
/// because the APK does not say which is which and only some of them have been
/// pinned down by running the thing.
///
/// The dex gives the class's **fifteen** fields — `x y width height fontSize`
/// (F), `editable textWrapped multiline manualFocusRelease` (Z), and `font
/// textColor textInputType returnKeyType xAlignment yAlignment` (I) — but
/// `field_ids` is sorted by name, so that is the *set* of fields and not the
/// order the constructor takes them. Parameter names are stripped from the
/// debug info and the class carries no annotations, so nothing declared in the
/// APK settles it. All the declarations settle is the grouping: slots 0-4 are
/// the five floats, 5, 12, 13 and 14 the four booleans, 6-11 the six ints.
///
/// **This paragraph said fourteen fields and three booleans until 2026-08-27,
/// and it was wrong.** A dex read taken then -- `classes2.dex`,
/// `class_data_off=0x6e33f5`, `field_idx` 4718-4732, bounded either side by
/// `DeviceStaticParams` and `PlatformParams` so the run is this class and only
/// this class -- lists `editable` as a declared field, which this comment had
/// never mentioned. The type tally is 6 I, 5 F, 4 Z, which matches the
/// constructor descriptor `(FFFFFZIIIIIIZZZ)V` exactly. `editable` is the
/// likeliest name for `z14`, the third trailing boolean discovered later and
/// never reconciled with the count above it -- likeliest, not settled, because
/// alphabetical order cannot rank the four booleans any more than it can the
/// six ints.
///
/// The rest came from `CORDIAL_TRACE_TEXT=1` on the Login screen at 1280x720,
/// where two boxes were focused in turn:
///
///     x=470 y=297 w=340 h=22 fontSize=16 z5=0 i6=0 i7=1 textColor=0xffd5d5dd
///                                        i9=46 i10=7  i11=3 z12=1 z13=0
///     x=470 y=363 w=304 h=22 fontSize=16 z5=0 i6=0 i7=1 textColor=0xffd5d5dd
///                                        i9=46 i10=5  i11=1 z12=1 z13=0
///
/// which establishes:
///
/// Slots 0-2 are x, y and width. 470 is exactly (1280-340)/2, and the two boxes
/// share slot 0 while differing in slot 1, which is a vertical stack. Read the
/// other way round the two boxes would be 66px apart horizontally and 340px
/// wide, overlapping each other and running off the surface.
///
/// Slot 8 is textColor: 0xffd5d5dd is an opaque light grey, and nothing else in
/// this class is a packed ARGB word. This is where the obvious guess at the
/// order — the field list read straight off in alphabetical-ish reading order,
/// which puts textColor at slot 7 — is wrong. It was worth not shipping.
///
/// Slots 3 and 4 are height and fontSize in that order, which is not proven but
/// is the only reading that is not absurd: a 22px line box holding 16pt text is
/// a text field, a 16px box holding 22pt text is not. INFERRED.
///
/// Slots 6, 7, 9, 10 and 11 hold 0, 1, 46, and a pair that differs per box.
/// Consistent with xAlignment=Left, yAlignment=Centre, a font id, then
/// textInputType and returnKeyType — the second box being the one below a
/// username field, slot 11 going 3 to 1 the way Next then Done would. Every
/// word of that is INFERRED; what is observed is only that 10 and 11 are the
/// ints that vary between two boxes and the other four do not.
///
/// **Nothing on this side picks between them, and nothing should.** The Rust
/// editor reads the font id out of whichever of 6, 7, 9, 10 and 11
/// `CORDIAL_TEXTBOX_FONT_SLOT` names, defaulting to 9 — see `font_slot` in
/// `crates/cordial-runtime/src/android/editor_font.rs`, which carries the
/// argument for the default and the reasons it is weak. Compiling the guess in
/// would have made the one capture that settles it cost a rebuild, and the
/// capture needs a person in a game that restyled a box, not a change here.
///
/// Slot 12 is not multiline. Both boxes are single-line login fields and both
/// report 1 there, so multiline is slot 5 or slot 13. Which, and which of the
/// remaining two is textWrapped and which manualFocusRelease, wants a box that
/// actually differs — an in-experience chat entry rather than a login form.
///
/// When a slot is settled, rename it here and in `RawTextBoxInfo` in
/// `crates/cordial-linker-sys/src/lib.rs` together. A wrong name would be worse
/// than no name, because it would be believed.
struct CordialTextBoxInfo {
    float x, y, width, height, font_size;
    // The three `Z` slots are widened to `int` rather than kept as `jboolean`.
    // This struct is mirrored field-for-field on the Rust side, and a one-byte
    // member sitting between four-byte ones is padding waiting to be got wrong.
    int z5;
    int i6, i7;
    int text_color;
    int i9, i10, i11;
    int z12, z13, z14;
};

namespace {
std::atomic<long long> g_textbox_handle{0};
std::mutex g_textbox_mutex;
std::string g_textbox_text;
/// The focused box's spec, and whether one was ever supplied. Guarded by
/// `g_textbox_mutex` alongside the text, which it always arrives with.
CordialTextBoxInfo g_textbox_info{};
bool g_textbox_info_known = false;
/// The most recently constructed `NativeTextBoxInfo`, kept because the engine
/// builds the object and hands it to `showKeyboard` as a separate step, and it
/// is only at `showKeyboard` that Cordial learns a box has focus. Also the
/// fallback for the object arriving null there, which would otherwise lose the
/// spec silently — the trace says which of the two supplied it.
CordialTextBoxInfo g_textbox_last_built{};
bool g_textbox_last_built_known = false;
/// Bumped on every focus change so the input side can tell "same box, keep
/// editing" from "new box, reseed the buffer" without comparing handles — a
/// handle can be reused after a box is destroyed.
std::atomic<unsigned> g_textbox_generation{0};

/// Every slot, named where a name has been earned and numbered where it has
/// not. `textColor` is printed in hex because that is the form in which it
/// identified itself as a colour at all.
///
/// **`z14` was missing from this line until 2026-08-27**, so the fifteenth
/// slot — the one whose absence originally made the whole `<init>` hook fail to
/// match — was invisible in every capture this project holds. A trace that
/// silently drops a field is worse than no trace of it: three of the four
/// booleans were being argued about from a log that only ever showed two.
void trace_textbox_info(const char* source, const CordialTextBoxInfo& i) {
    fprintf(stderr,
            "[cordial] textbox spec from %s x=%g y=%g w=%g h=%g fontSize=%g "
            "z5=%d i6=%d i7=%d textColor=%#x i9=%d i10=%d i11=%d z12=%d z13=%d z14=%d\n",
            source, static_cast<double>(i.x), static_cast<double>(i.y),
            static_cast<double>(i.width), static_cast<double>(i.height),
            static_cast<double>(i.font_size), i.z5, i.i6, i.i7,
            static_cast<unsigned>(i.text_color), i.i9, i.i10, i.i11, i.z12, i.z13,
            i.z14);
}
} // namespace

extern "C" void cordial_textbox_last_built(const CordialTextBoxInfo* info) {
    if (!info) return;
    std::lock_guard<std::mutex> lock(g_textbox_mutex);
    g_textbox_last_built = *info;
    g_textbox_last_built_known = true;
}

extern "C" void cordial_textbox_focused(long long handle, const char* text,
                                        const CordialTextBoxInfo* info) {
    const bool trace = getenv("CORDIAL_TRACE_TEXT") != nullptr;
    if (trace) {
        fprintf(stderr, "[cordial] textbox focused handle=%lld current=%zu bytes\n",
                handle, text ? strlen(text) : 0);
    }
    {
        std::lock_guard<std::mutex> lock(g_textbox_mutex);
        g_textbox_text = text ? text : "";
        const char* source = nullptr;
        if (info) {
            g_textbox_info = *info;
            g_textbox_info_known = true;
            source = "showKeyboard";
        } else if (g_textbox_last_built_known) {
            g_textbox_info = g_textbox_last_built;
            g_textbox_info_known = true;
            source = "last <init>";
        } else {
            // Neither path produced one. Keeping the previous box's numbers
            // would be worse than admitting the gap: an editor styled from a
            // stale spec sits in the wrong place and looks like a layout bug
            // rather than a missing value.
            g_textbox_info = CordialTextBoxInfo{};
            g_textbox_info_known = false;
        }
        if (trace) {
            if (source) {
                trace_textbox_info(source, g_textbox_info);
            } else {
                fprintf(stderr, "[cordial] textbox spec unavailable\n");
            }
        }
    }
    g_textbox_handle.store(handle, std::memory_order_release);
    g_textbox_generation.fetch_add(1, std::memory_order_acq_rel);
}

extern "C" void cordial_textbox_blurred() {
    if (getenv("CORDIAL_TRACE_TEXT")) {
        fprintf(stderr, "[cordial] textbox blurred\n");
    }
    {
        // The spec goes with the focus. A caller that kept drawing an editor
        // from the last known geometry would leave one over an unfocused box.
        std::lock_guard<std::mutex> lock(g_textbox_mutex);
        g_textbox_info_known = false;
    }
    g_textbox_handle.store(0, std::memory_order_release);
    g_textbox_generation.fetch_add(1, std::memory_order_acq_rel);
}

/// The focused box's handle, or 0 when nothing is focused. A 0 here is why
/// text must not be sent at all, rather than sent to handle 0.
extern "C" long long cordial_textbox_handle() {
    return g_textbox_handle.load(std::memory_order_acquire);
}

extern "C" unsigned cordial_textbox_generation() {
    return g_textbox_generation.load(std::memory_order_acquire);
}

/// How many times the engine has said a place finished loading, and which one.
///
/// Two callbacks report it -- `NativeGLJavaInterface::gameLoadedCallback` and
/// `NativeHelper::onGameLoaded` -- and either is enough to mean "the join
/// completed", so both bump the same counter rather than the watchdog having to
/// know which build calls which.
static std::atomic<unsigned> g_games_loaded{0};
static std::atomic<long long> g_last_place{0};

extern "C" void cordial_note_game_loaded(long long place_id) {
    g_games_loaded.fetch_add(1, std::memory_order_release);
    g_last_place.store(place_id, std::memory_order_release);
}

extern "C" unsigned cordial_games_loaded(void) {
    return g_games_loaded.load(std::memory_order_acquire);
}

extern "C" long long cordial_last_place(void) {
    return g_last_place.load(std::memory_order_acquire);
}

/// Copy the focused box's spec into `*out`. Returns 1 when one is known, 0
/// otherwise — and 0 has to mean "do not style anything from this", because
/// `*out` is left untouched rather than zeroed. A box at (0, 0) sized 0x0 is
/// indistinguishable from a box Cordial was never told about, and only one of
/// those is worth drawing an editor for.
extern "C" int cordial_textbox_info(CordialTextBoxInfo* out) {
    if (!out) return 0;
    std::lock_guard<std::mutex> lock(g_textbox_mutex);
    if (!g_textbox_info_known) return 0;
    *out = g_textbox_info;
    return 1;
}

/// Test seam: publish a focused box built here, from the fourteen values in
/// the order `NativeTextBoxInfo.<init>` takes them.
///
/// It exists because the obvious test — hand a Rust-built struct to
/// `cordial_textbox_focused` and read it back — proves only that `memcpy`
/// works. That version was written first and passed with the Rust mirror
/// deliberately shifted by one field, which is precisely the bug it was meant
/// to catch. Naming the members on this side and naming them again on the
/// other is what makes the two layouts have to agree.
extern "C" void cordial_textbox_test_focus(long long handle, const char* text,
                                           float s0, float s1, float s2, float s3,
                                           float s4, int s5, int s6, int s7, int s8,
                                           int s9, int s10, int s11, int s12, int s13,
                                           int s14) {
    CordialTextBoxInfo info{};
    info.x = s0;
    info.y = s1;
    info.width = s2;
    info.height = s3;
    info.font_size = s4;
    info.z5 = s5;
    info.i6 = s6;
    info.i7 = s7;
    info.text_color = s8;
    info.i9 = s9;
    info.i10 = s10;
    info.i11 = s11;
    info.z12 = s12;
    info.z13 = s13;
    info.z14 = s14;
    cordial_textbox_focused(handle, text, &info);
}

/// Copy the focused box's current contents into `buf`. Returns the number of
/// bytes written, not counting the NUL.
extern "C" int cordial_textbox_text(char* buf, int n) {
    if (!buf || n <= 0) return 0;
    std::lock_guard<std::mutex> lock(g_textbox_mutex);
    int len = static_cast<int>(g_textbox_text.size());
    if (len > n - 1) len = n - 1;
    memcpy(buf, g_textbox_text.data(), static_cast<size_t>(len));
    buf[len] = '\0';
    return len;
}

namespace cordial {

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using String = jnivm::String;

/// libjnivm's `String` derives both `Object` and `std::string`, so a Java string
/// is simply constructed — there is no VM-side allocator to go through.
inline std::shared_ptr<String> str(const char* v) {
    return std::make_shared<String>(std::string(v ? v : ""));
}

/// `java.lang.String.getBytes(String charsetName)`
///
/// libjnivm provides `String` but not this method, and the engine reaches it on
/// the text-entry path: it reads AGDK's `gametextinput/State.text` field and
/// then converts that string to bytes before consuming it. Observed as the last
/// thing to happen on every keystroke before nothing happened:
///
///     Invoked Field Getter Class=`com/google/androidgamesdk/gametextinput/State` Field=`text`
///     Found symbol, Class=`java/lang/String`, Method=`getBytes`, Signature=`(Ljava/lang/String;)[B`
///     Call Unknown Member Function Class=`java/lang/String` Method=`getBytes`
///
/// "Call Unknown Member Function" is the engine asking and getting nothing, so
/// the text it had just read was converted to no bytes at all. Every keystroke
/// reached AGDK correctly and died here.
///
/// The charset argument is accepted and ignored. libjnivm's `String` derives
/// `std::string` and holds UTF-8 already, and the only charset Android's text
/// input path asks for is UTF-8; honouring a request for anything else would
/// mean carrying an encoder for a case that does not arise. If a non-UTF-8
/// charset is ever observed here, that is worth a real conversion rather than a
/// silent wrong answer.
class StringBridge {
public:
    static std::shared_ptr<jnivm::Array<jbyte>> getBytes(ENV*, Object* self,
                                                         std::shared_ptr<String>) {
        auto* str = dynamic_cast<String*>(self);
        const std::string text = str ? static_cast<const std::string&>(*str) : std::string();
        auto out = std::make_shared<jnivm::Array<jbyte>>(static_cast<jsize>(text.size()));
        if (!text.empty()) {
            memcpy(out->getArray(), text.data(), text.size());
        }
        return out;
    }

    static void Register(ENV* env) {
        auto c = env->GetClass("java/lang/String");
        c->HookInstanceFunction(env, "getBytes", &StringBridge::getBytes);
    }
};

/// `com.roblox.engine.jni.model.DeviceStaticParams`
///
/// The device description Roblox reads once at startup and then believes for the
/// rest of the session — form factor, screen metrics, identifiers. This is where
/// spec §4.2's "Roblox thinks you're mobile" is actually decided; the
/// `__system_property_get` values in `bionic` cover the native side, but this
/// object is what the engine consults.
///
/// Fields are added as Roblox is observed reading them. It stops at the first
/// null, so the surface only becomes visible one answer at a time.
class DeviceStaticParams : public Object {
public:
    // Field names and types were not guessed. Returning a live object instead of
    // null made Roblox read them, and libjnivm named each one as it did.
    std::shared_ptr<String> osVersion;
    std::shared_ptr<String> deviceName;
    std::shared_ptr<String> appVersion;
    std::shared_ptr<String> manufacturer;
    std::shared_ptr<String> deviceSku;
    std::shared_ptr<String> appBuildVariant;
    std::shared_ptr<String> socModel;
    jboolean cpu64Bit = true;

    static std::shared_ptr<DeviceStaticParams> Create() {
        auto p = std::make_shared<DeviceStaticParams>();
        // Desktop values, deliberately. Roblox reads this once and believes it for
        // the session, so it is the single most load-bearing place to be honest
        // about what Cordial is. Claiming to be a particular phone would invite
        // device-specific workarounds that do not apply here.
        p->osVersion       = str("15");
        p->deviceName      = str("Cordial");
        p->manufacturer    = str("Cordial");
        p->deviceSku       = str("cordial");
        p->socModel        = str("cordial");
        p->appBuildVariant = str("release");
        // Left as the client's own version until Cordial reads it from the APK
        // manifest; a wrong value here shows up in telemetry and support threads.
        p->appVersion      = str("");
        p->cpu64Bit        = true;
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<DeviceStaticParams>("com/roblox/engine/jni/model/DeviceStaticParams");
        auto c = env->GetClass("com/roblox/engine/jni/model/DeviceStaticParams");
        c->HookInstance(env, "osVersion", &DeviceStaticParams::osVersion);
        c->HookInstance(env, "deviceName", &DeviceStaticParams::deviceName);
        c->HookInstance(env, "appVersion", &DeviceStaticParams::appVersion);
        c->HookInstance(env, "manufacturer", &DeviceStaticParams::manufacturer);
        c->HookInstance(env, "deviceSku", &DeviceStaticParams::deviceSku);
        c->HookInstance(env, "appBuildVariant", &DeviceStaticParams::appBuildVariant);
        c->HookInstance(env, "socModel", &DeviceStaticParams::socModel);
        c->HookInstance(env, "cpu64Bit", &DeviceStaticParams::cpu64Bit);
    }
};

/// `com.roblox.engine.jni.model.NativeTextBoxInfo`
///
/// The spec for the editor Android lays over the focused text box. See
/// `CordialTextBoxInfo` at the top of this file for what the fourteen values
/// are, what is established about their order and what is not, and why an
/// editor is needed on a machine with no on-screen keyboard.
class NativeTextBoxInfo : public Object {
public:
    /// The values this object was constructed with. Held per-object because
    /// `showKeyboard` is handed the object itself, so reading them back off it
    /// is exact where the process-wide "most recently built" slot is only a
    /// good guess about which box the engine meant.
    CordialTextBoxInfo spec{};
    bool spec_known = false;

    // The engine constructs one of these and hands it to `showKeyboard`. With no
    // constructor registered, libjnivm logged
    //
    //     Call Unknown Static Function ... Method=`<init>`
    //
    // and produced nothing, so the engine passed a null down its own call. An
    // unresolved JNI call is not free: it can leave a pending exception that
    // every later JNI call on the thread then trips over, which is a plausible
    // way for text entry to be wedged well after the call that caused it.
    //
    // Every argument used to be discarded here. Resolving the call was only
    // half of it: these are the numbers that say where the box is on screen and
    // what its text should look like, and without them Cordial has nothing to
    // position or style an editor from.
    /// **Fifteen arguments, not fourteen, and the fifteenth is why text was
    /// invisible for a year.**
    ///
    /// libjnivm's trace prints the signature the engine actually asks for:
    ///
    ///     Found symbol, Class=`...NativeTextBoxInfo`, StaticMethod=`<init>`,
    ///       Signature=`(FFFFFZIIIIIIZZZ)L...NativeTextBoxInfo;`
    ///     Call Unknown Static Function ... Method=`<init>`
    ///
    /// Five floats, a boolean, six ints, then **three** booleans. This hook had
    /// two. One argument short is a signature that never matches, so `<init>`
    /// resolved to nothing, `new NativeTextBoxInfo(...)` produced null, and the
    /// engine passed that null straight into `showKeyboard` -- which is the
    /// `info=NULL` every trace here showed, and therefore the whole of
    /// "characters are invisible until the box loses focus". Android places a
    /// real `EditText` from these numbers; Cordial received none of them.
    ///
    /// An earlier attempt added a `shared_ptr<java::lang::Class>` third
    /// parameter, read off `--dump-classes`. That was wrong: the dump prints
    /// libjnivm's own receiver convention for the generated class, not a
    /// parameter of the Java constructor. The trace is the authority here
    /// because it prints the descriptor the engine looked up.
    ///
    /// `CORDIAL_JNI_TRACE=1 cargo build` is what makes that visible, and it is
    /// the tool to reach for whenever a hook silently does nothing.
    static std::shared_ptr<NativeTextBoxInfo> init(
        ENV*, Class*, jfloat f0, jfloat f1, jfloat f2, jfloat f3, jfloat f4,
        jboolean z5, jint i6, jint i7, jint i8, jint i9, jint i10, jint i11,
        jboolean z12, jboolean z13, jboolean z14) {
        auto o = std::make_shared<NativeTextBoxInfo>();
        // Positional on purpose: these are the constructor's slots, and only
        // some of them have earned a name. See `CordialTextBoxInfo`.
        o->spec = CordialTextBoxInfo{
            f0, f1, f2, f3, f4,
            z5 ? 1 : 0,
            i6, i7, i8, i9, i10, i11,
            z12 ? 1 : 0, z13 ? 1 : 0, z14 ? 1 : 0,
        };
        o->spec_known = true;
        cordial_textbox_last_built(&o->spec);
        return o;
    }

    static void Register(ENV* env) {
        env->GetClass<NativeTextBoxInfo>("com/roblox/engine/jni/model/NativeTextBoxInfo");
        auto c = env->GetClass("com/roblox/engine/jni/model/NativeTextBoxInfo");
        c->Hook(env, "<init>", &NativeTextBoxInfo::init);
    }
};

// ------------------------------------------------------------- who is signed in
//
// The account the mirrors below answer from, and the sinks the DataModel
// notification handler reports a login and a logout through.
//
// **This is what made a restored session still land on the landing page.** The
// cookie store persists a real session and the engine confirms it holds the
// cookies, and the run still ends `app ready: Landing`, because
// `PlatformAccountRouter` does not consult the cookie — it asks these mirrors,
// was told user 0 with an empty name, and routed accordingly without ever
// reaching the network. docs/design/sign-in.md §1.3 said these were
// presentation-layer only; §9 of that document records that it was wrong.
//
// Written from whichever thread the engine delivers `DID_LOG_IN` on and read
// from the engine's own threads whenever it wants to know who is playing, so
// it is guarded rather than plain.
//
// Nothing here prints a username or a user id at any verbosity. A username is a
// person; `crates/cordial-runtime/src/identity.rs` carries the rest of that
// reasoning and is the only place these values are written down.

namespace {
std::mutex g_identity_mutex;
bool g_identity_known = false;
jlong g_identity_user_id = 0;
std::string g_identity_username;
std::string g_identity_display_name;
jint g_identity_membership = 0;
bool g_identity_under13 = false;
bool g_identity_subscription = false;

// Atomic rather than under `g_identity_mutex`: the login sink calls back into
// `cordial_identity_publish`, which takes that mutex, so reading the pointer
// under it would deadlock the first time anybody signed in.
std::atomic<void (*)(const char*)> g_identity_login_sink{nullptr};
std::atomic<void (*)()> g_identity_logout_sink{nullptr};

// `APP_READY`, for whoever needs to act the moment the Lua app shell is up.
//
// Deliberately only a notification: it fires on the engine's own thread from
// inside the engine's own callback, so the one caller (`deeplink.rs`) sets a
// flag here and does its work from the looper thread, which is the thread every
// other native call in this process is made from.
std::atomic<void (*)(const char*)> g_app_ready_sink{nullptr};
} // namespace

jlong identity_user_id() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity_user_id;
}

std::string identity_username() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity_username;
}

/// Falls back to the username, because that is what an account with no display
/// name set actually shows on Roblox. Empty here would render a nameless header
/// for a user who is definitely signed in.
std::string identity_display_name() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity_display_name.empty() ? g_identity_username : g_identity_display_name;
}

jint identity_membership_type() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity_membership;
}

bool identity_is_under13() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity_under13;
}

bool identity_has_subscription() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity_subscription;
}

/// Whether anybody is signed in at all, for the diagnostics that must not name
/// who. `g_identity_known` rather than a non-zero id, so that the one place
/// that decides what "signed in" means is the publish call.
bool identity_known() {
    std::lock_guard<std::mutex> lock(g_identity_mutex);
    return g_identity_known;
}

/// `CORDIAL_TRACE_IDENTITY=1`: name each mirror the engine asks, and never the
/// answer.
///
/// This exists because filling the mirrors in was not enough on its own and
/// there was no way to tell "the engine never asked" from "the engine asked and
/// routed on something else" — two completely different pieces of work with the
/// same symptom, which is a landing page. It reports the *field*, and whether an
/// identity was known at the time; a username or a user id never reaches it.
void trace_identity(const char* field) {
    static const bool on = getenv("CORDIAL_TRACE_IDENTITY") != nullptr;
    if (!on) return;
    fprintf(stderr, "[cordial] identity asked: %s (%s)\n", field,
            identity_known() ? "signed in" : "nobody");
}

/// What Cordial answers when the engine asks which platform it is running on.
///
/// `Linux` is one of the engine's *own* platform names, not a word invented
/// here: `Android`, `AndroidTV`, `Linux`, `MetaOS`, `SteamOS`, `Windows` and
/// `XBoxOne` are the standalone platform tokens in libroblox.so's string table,
/// and they are `Enum.Platform` values. So this is not passing Cordial off as
/// something else — it is telling the truth in the vocabulary the engine
/// already has, on a machine that genuinely is Linux.
///
/// "Never make a stub lie" cuts both ways here. `Windows` would be a lie.
/// `Android`, which is what Cordial answered until now, is *also* a lie: the
/// host is a desktop Linux machine with a keyboard and a mouse and no
/// touchscreen. The engine is already told the last of those through
/// `PlatformParams.isTouchDevice`, and it does read that one — measured, twice
/// per cold start — so answering `Android` here contradicted a value the engine
/// had taken from us. (`isKeyboardDevice` and `isMouseDevice` sit beside it and
/// are never read at all; see `native/init_params.cpp`'s header. Do not reach
/// for those two.)
///
/// **What this is not.** It has not been established that this is what makes the
/// client behave as a mobile one; see docs/analysis/platform-identity.md for
/// what was measured and what was not. It is defensible as the truthful answer
/// on its own, independently of what it fixes.
///
/// It is also not settled whether `getPlatformName` means "the platform this
/// client runs on" or "the platform this *user* is on", the console-gamertag
/// sense that `FFlagAddPlatformNameToProfileHeader` and
/// `DFFlagConsumePlatformNameOverAlternateName` both read as. The two senses
/// give the same answer on a Linux desktop, which is why the value stands either
/// way — but do not build anything on the assumption that this is
/// `Enum.Platform` until someone has printed `UserInputService:GetPlatform()`
/// inside a running experience.
///
/// `CORDIAL_PLATFORM_NAME=<name>` overrides it, which is also the control: a
/// run with `CORDIAL_PLATFORM_NAME=Android` is the pre-change client in the same
/// session and the same binary, differing in exactly this string.
const char* platform_name() {
    static const std::string v = [] {
        const char* e = getenv("CORDIAL_PLATFORM_NAME");
        return (e && *e) ? std::string(e) : std::string("Linux");
    }();
    return v.c_str();
}

/// `com.roblox.engine.jni.NativeGLJavaInterface`
///
/// The engine's main line back into Java: device parameters, keyboard, screen
/// orientation, purchase prompts, overlays, and the leave/exit notifications a
/// plugin-visible `onLeave` would be built from, if one existed. None does.
class NativeGLJavaInterface : public Object {
public:
    /// The static half of the same game-loaded announcement `NativeHelper`
    /// carries as an instance method — the engine calls both, and until now
    /// this one was an unresolved symbol too. See the note beside
    /// `NativeHelper::onGameLoaded` in `init_params.cpp` for why the pair is
    /// being answered and what is not yet established about it.
    static void gameLoadedCallback(ENV*, Class*, jlong place_id) {
        fprintf(stderr, "[roblox] gameLoadedCallback: place %lld\n",
                static_cast<long long>(place_id));
        // Recorded, not only printed, so the join watchdog in `looper::pump`
        // has something to wait for. A join Cordial started and the engine
        // never completed is otherwise invisible: the pump keeps running, the
        // window keeps presenting the place it was already on, and the user is
        // left looking at a screen that simply never changes.
        cordial_note_game_loaded(static_cast<long long>(place_id));
    }

    static std::shared_ptr<DeviceStaticParams> getDeviceStaticParams(ENV*, Class*) {
        // Returning a live object rather than null is the whole point: Roblox
        // logs and gives up on null, and never reaches the code that would tell
        // us which fields it wants.
        return DeviceStaticParams::Create();
    }

    // The on-screen keyboard, and the half of text entry that runs engine->host.
    //
    // These were no-ops on the reasoning that a desktop has no soft keyboard, so
    // host key events could reach the engine through the input path alone. That
    // was wrong, and it is why the login form's boxes stayed empty: the `jlong`
    // is the *handle of the text box being edited*, and it is the only place the
    // host is ever told which box has focus. Android's IME keeps it and passes it
    // straight back as the first argument of `nativePassText`. Cordial threw it
    // away and then sent text for handle 0, so every keystroke arrived addressed
    // to a box that was not the focused one.
    //
    // The `byte[]` is the box's current contents. Editing has to start from that
    // rather than from an empty buffer, otherwise the first keystroke in a
    // pre-filled or re-focused field wipes it.
    //
    // Signature is (JZ[BLcom/roblox/engine/jni/model/NativeTextBoxInfo;)V. libjnivm
    // matches hooks on the descriptor derived from the C++ types, so `byte[]` has
    // to be an Array<jbyte> and the last argument the real class — Object would
    // produce Ljava/lang/Object; and silently never match.
    //
    // The `NativeTextBoxInfo` is the fourth thing this call carries and the
    // reason the other half of text entry can work at all: it says where the
    // box is and how its text is drawn. Android uses it to place a real
    // `EditText` over the GL surface, because the engine does not paint a
    // focused box's contents itself. Cordial dropped it, which is why typing
    // into a box that is genuinely receiving the keystrokes still shows
    // nothing.
    static void showKeyboard(ENV*, Class*, jlong handle, jboolean,
                             std::shared_ptr<jnivm::Array<jbyte>> current,
                             std::shared_ptr<NativeTextBoxInfo> info) {
        std::string text;
        if (current) {
            for (jsize i = 0; i < current->getSize(); i++) {
                jbyte b = (*current)[i];
                if (b == 0) break;
                text.push_back(static_cast<char>(b));
            }
        }
        // Straight off the object when it arrives, since that is the box the
        // engine actually named. A null falls back to the last one constructed
        // rather than to nothing, because an unmatched hook here would look
        // exactly like a box with no spec.
        const CordialTextBoxInfo* spec =
            (info && info->spec_known) ? &info->spec : nullptr;
        // Separates the two ways this arrives empty, which look identical from
        // "textbox spec unavailable" and lead somewhere completely different: a
        // null object means the engine had nothing to give, while an object
        // whose spec is unknown means our `<init>` hook never matched and the
        // fourteen values went past us.
        if (getenv("CORDIAL_TRACE_TEXT") != nullptr) {
            fprintf(stderr, "[cordial] showKeyboard: info=%s spec_known=%s\n",
                    info ? "object" : "NULL",
                    info ? (info->spec_known ? "true" : "false") : "n/a");
        }
        cordial_textbox_focused(handle, text.c_str(), spec);
    }
    static void hideKeyboard(ENV*, Class*) { cordial_textbox_blurred(); }

    // In-app purchases go through Google Play Billing, which does not exist here.
    // Silently doing nothing is the honest behaviour: the alternative is
    // pretending a purchase flow started and leaving the engine waiting for a
    // result that never arrives.
    static void promptNativePurchase(ENV*, Class*, jlong, std::shared_ptr<String>,
                                     std::shared_ptr<String>) {}
    static void promptNativePurchaseShort(ENV*, Class*, jlong, std::shared_ptr<String>) {}
    static void promptNativePurchaseWithPayload(ENV*, Class*, jlong, std::shared_ptr<String>,
                                                std::shared_ptr<String>) {}

    static void exitGameWithError(ENV*, Class*, jint code) {
        fprintf(stderr, "[roblox] exitGameWithError(%d)\n", code);
    }
    static void gameDidLeave(ENV*, Class*) {
        // Spec §9a called this `onLeave` in the plugin event schema, and hung
        // "close when you leave an experience" off it. Neither was built: there
        // is no such core event, and nothing subscribes. This is still the right
        // place for both, which is why the note stays.
        fprintf(stderr, "[roblox] gameDidLeave\n");
    }
    static void onAppShellReloadNeeded(ENV*, Class*) {}

    // The engine notifies Java when the focused Lua text box changes, and when
    // one of its properties does. Observed as unresolved during sign-in:
    //
    //     Constructed Unresolved symbol ... `onLuaTextBoxChangedCallback`, `(Ljava/lang/String;)V`
    //     Constructed Unresolved symbol ... `onLuaTextBoxPropertyChangedCallback`, `()V`
    //
    // Nothing on a desktop needs to react to either — there is no IME view to
    // reposition — so these are no-ops. They exist because *resolving* is the
    // point: an unresolved call is a different thing from a call that did
    // nothing, and only the second one is safe.
    static void onLuaTextBoxChangedCallback(ENV*, Class*, std::shared_ptr<String>) {}
    static void onLuaTextBoxPropertyChangedCallback(ENV*, Class*) {}
    static void listenToMotionEvents(ENV*, Class*, std::shared_ptr<String>) {}
    static void screenOrientationChanged(ENV*, Class*, jint) {}

    // ------------------------------------------------------------ web views
    //
    // Marketplace, Profile, Friends, Communities, Create, the blog, gift cards
    // and most link-opening are web content on Android, not engine-rendered UI,
    // and these three calls are the whole of how the engine asks for them on the
    // transport this build uses. See docs/analysis/webview-surface.md for how
    // that was established, and in particular for why `android.webkit.WebView`
    // is *not* the boundary to implement: the engine never touches it, because
    // driving a `WebView` is Roblox's own Java code's job and Cordial stands in
    // for that code rather than running it.
    //
    // All three report rather than return, because none of them can be answered
    // honestly yet. `openNativeOverlay` used to be an empty body, which is the
    // failure this section exists to correct: a request to show a web page
    // vanished with no trace anywhere, so "Marketplace does nothing" looked
    // identical to "Marketplace was never asked for". Those need to be
    // distinguishable before anyone can work on either.

    /// Urls on this boundary can carry a single-use authentication ticket in
    /// their query string, and the session's own `.ROBLOSECURITY` travels the
    /// adjacent cookie path. Diagnostics print scheme, host and path and stop
    /// there; a truncation would still leak the front of a token.
    static std::string url_without_query(const std::string& url) {
        auto cut = url.find_first_of("?#");
        if (cut == std::string::npos) return url;
        std::string out = url.substr(0, cut);
        out += (url[cut] == '?') ? "?<query elided>" : "#<fragment elided>";
        return out;
    }

    /// The engine asking the platform to put a web page on screen: url, title.
    /// Corresponds to `BrowserService::openNativeOverlay` on the engine side,
    /// which logs the same two arguments as `openWebView_`.
    static void openNativeOverlay(ENV*, Class*, std::shared_ptr<String> url,
                                  std::shared_ptr<String> title) {
        fprintf(stderr,
                "[roblox] web view requested, and Cordial has none: %s (title: %s)\n",
                url ? url_without_query(*url).c_str() : "(null)",
                title ? title->c_str() : "");
    }

    /// `(String type, String data)` on both. The app bridge and the DataModel
    /// notification channel are how the engine tells the platform that app
    /// state changed — `APP_READY`, `PURCHASE_ROBUX` and the rest — and on
    /// Android some of those are what send the user to a web page.
    ///
    /// Most are still reports rather than acted on: which type maps to which
    /// destination lives in Roblox's Java, and is not established here.
    /// Resolving them is worth doing on its own account, because an unresolved
    /// JNI call leaves a pending exception that the next JNI call on the same
    /// thread trips over, which surfaces far away from the cause.
    static void onAppBridgeNotification(ENV*, Class*, std::shared_ptr<String> type,
                                        std::shared_ptr<String> data) {
        fprintf(stderr, "[roblox] app bridge: %s %s\n",
                type ? type->c_str() : "(null)", data ? data->c_str() : "");
    }

    /// The one notification Cordial acts on rather than only prints.
    ///
    /// **`DID_LOG_IN` is the engine handing over the answer the identity
    /// mirrors above were inventing a zero for.** Its payload carries exactly
    /// the fields `NativeUserJavaInterface` and `StartAppParams` want:
    ///
    ///     DID_LOG_IN {"username":…,"membershipType":…,"isUnder13":…,
    ///                 "hasRobloxSubscription":…,"countryCode":…,
    ///                 "userId":…,"displayName":…}
    ///
    /// and it lands roughly twenty-five milliseconds before `APP_READY Home`.
    /// Before this, Cordial received it, printed it in full, and dropped it —
    /// so the next launch went back to `Landing` with a perfectly good cookie,
    /// and a real person's username sat in the terminal scrollback meanwhile.
    ///
    /// `DID_SIGN_UP` and `DID_SWITCH_ACCOUNT` are handled the same way and are
    /// **INFERRED**: they are adjacent strings in `libroblox.so` and neither has
    /// been seen to fire under Cordial, so they are routed through the same
    /// parse, which stores nothing if the payload is not identity-shaped. That
    /// makes a wrong guess about them a no-op rather than a wrong account.
    ///
    /// `LUA_UNAUTHORIZED_LOG_OUT` clears alongside `DID_LOG_OUT` for the reason
    /// the clearing exists at all: it is precisely the case where the server has
    /// stopped honouring the session, and an identity that outlived it would
    /// present a signed-in shell that can fetch nothing.
    static void onDataModelNotificationCallback(ENV*, Class*, std::shared_ptr<String> type,
                                                std::shared_ptr<String> data) {
        const std::string kind = type ? static_cast<const std::string&>(*type) : std::string();
        const std::string payload = data ? static_cast<const std::string&>(*data) : std::string();

        const bool login = kind == "DID_LOG_IN" || kind == "DID_SIGN_UP"
                        || kind == "DID_SWITCH_ACCOUNT";
        const bool logout = kind == "DID_LOG_OUT" || kind == "LUA_UNAUTHORIZED_LOG_OUT";

        if (login) {
            // The payload is a username, a display name and a user id — a real
            // person — so its size is reported and its content is not. This
            // used to be printed whole, which is the leak this line replaces.
            fprintf(stderr, "[roblox] datamodel notification: %s <identity elided, %zu bytes>\n",
                    kind.c_str(), payload.size());
            if (auto* sink = g_identity_login_sink.load(std::memory_order_acquire)) {
                sink(payload.c_str());
            }
            return;
        }
        if (logout) {
            fprintf(stderr, "[roblox] datamodel notification: %s\n", kind.c_str());
            if (auto* sink = g_identity_logout_sink.load(std::memory_order_acquire)) {
                sink();
            }
            return;
        }
        fprintf(stderr, "[roblox] datamodel notification: %s %s\n",
                type ? type->c_str() : "(null)", payload.c_str());

        if (kind == "APP_READY") {
            if (auto* sink = g_app_ready_sink.load(std::memory_order_acquire)) {
                sink(payload.c_str());
            }
        }
    }

    /// Not a getter despite the name — it returns void because it is a request,
    /// and the platform is expected to answer later by calling
    /// `NativeGLInterface.setWebviewUserAgent(String)`, which `libroblox.so`
    /// exports.
    ///
    /// Deliberately unanswered. The truthful answer is the user agent of the web
    /// view that will show the page, and there is no web view, so any string put
    /// here would be a claim about a browser that does not exist — telling the
    /// engine one thing and Roblox's servers another the moment one does. The
    /// engine carries on with the agent `InitParams.userAgent` already gave it.
    static void getWebViewUserAgent(ENV*, Class*) {
        fprintf(stderr,
                "[roblox] web view user agent requested; unanswered, because "
                "there is no web view to ask (see docs/analysis/webview-surface.md)\n");
    }

    static void Register(ENV* env) {
        env->GetClass<NativeGLJavaInterface>("com/roblox/engine/jni/NativeGLJavaInterface");
        auto c = env->GetClass("com/roblox/engine/jni/NativeGLJavaInterface");
        c->Hook(env, "gameLoadedCallback", &NativeGLJavaInterface::gameLoadedCallback);
        c->Hook(env, "getDeviceStaticParams", &NativeGLJavaInterface::getDeviceStaticParams);
        c->Hook(env, "showKeyboard", &NativeGLJavaInterface::showKeyboard);
        c->Hook(env, "hideKeyboard", &NativeGLJavaInterface::hideKeyboard);
        c->Hook(env, "promptNativePurchase", &NativeGLJavaInterface::promptNativePurchase);
        c->Hook(env, "promptNativePurchaseWithPayload",
                &NativeGLJavaInterface::promptNativePurchaseWithPayload);
        c->Hook(env, "exitGameWithError", &NativeGLJavaInterface::exitGameWithError);
        c->Hook(env, "gameDidLeave", &NativeGLJavaInterface::gameDidLeave);
        c->Hook(env, "onAppShellReloadNeeded", &NativeGLJavaInterface::onAppShellReloadNeeded);
        c->Hook(env, "onLuaTextBoxChangedCallback",
                &NativeGLJavaInterface::onLuaTextBoxChangedCallback);
        c->Hook(env, "onLuaTextBoxPropertyChangedCallback",
                &NativeGLJavaInterface::onLuaTextBoxPropertyChangedCallback);
        c->Hook(env, "listenToMotionEvents", &NativeGLJavaInterface::listenToMotionEvents);
        c->Hook(env, "screenOrientationChanged",
                &NativeGLJavaInterface::screenOrientationChanged);
        c->Hook(env, "openNativeOverlay", &NativeGLJavaInterface::openNativeOverlay);
        c->Hook(env, "onAppBridgeNotification",
                &NativeGLJavaInterface::onAppBridgeNotification);
        c->Hook(env, "onDataModelNotificationCallback",
                &NativeGLJavaInterface::onDataModelNotificationCallback);
        c->Hook(env, "getWebViewUserAgent", &NativeGLJavaInterface::getWebViewUserAgent);
    }
};

/// `com.roblox.engine.jni.locale.NativeLocaleJavaInterface`
///
/// Roblox distinguishes three locales: the system's, the one the account is set
/// to, and the one the current experience is running in. Only the first is
/// Cordial's to answer; the other two are account and session state it does not
/// have, so they mirror the system locale until auth exists.
class NativeLocaleJavaInterface : public Object {
public:
    static std::shared_ptr<String> systemLocale() {
        // Android wants a BCP-47-ish tag. POSIX gives "en_AU.UTF-8"; take the
        // language and region and drop the encoding.
        const char* raw = getenv("LC_ALL");
        if (!raw || !*raw) raw = getenv("LC_MESSAGES");
        if (!raw || !*raw) raw = getenv("LANG");
        if (!raw || !*raw) return str("en_us");

        std::string tag(raw);
        if (auto dot = tag.find('.'); dot != std::string::npos) {
            tag.resize(dot);
        }
        if (tag.empty() || tag == "C" || tag == "POSIX") {
            return str("en_us");
        }
        for (auto& ch : tag) {
            ch = static_cast<char>(tolower(static_cast<unsigned char>(ch)));
        }
        return str(tag.c_str());
    }

    static std::shared_ptr<String> getLocale(ENV*, Class*) { return systemLocale(); }
    static std::shared_ptr<String> getRobloxLocale(ENV*, Class*) { return systemLocale(); }
    static std::shared_ptr<String> getGameLocale(ENV*, Class*) { return systemLocale(); }

    static void Register(ENV* env) {
        env->GetClass<NativeLocaleJavaInterface>(
            "com/roblox/engine/jni/locale/NativeLocaleJavaInterface");
        auto c = env->GetClass("com/roblox/engine/jni/locale/NativeLocaleJavaInterface");
        c->Hook(env, "getLocale", &NativeLocaleJavaInterface::getLocale);
        c->Hook(env, "getRobloxLocale", &NativeLocaleJavaInterface::getRobloxLocale);
        c->Hook(env, "getGameLocale", &NativeLocaleJavaInterface::getGameLocale);
    }
};

/// `com.roblox.engine.jni.user.NativeUserJavaInterface`
///
/// Who is signed in, answered from the identity above once a login has
/// established one and reported as nobody until then.
///
/// **The signed-out answers are still the honest ones and are not defaults to
/// be improved on.** A fabricated user id would flow straight into telemetry
/// and analytics as if it were real, and would make the client claim an account
/// it holds no cookie for. Nothing here invents a value; it reports what the
/// engine's own `DID_LOG_IN` said, or that there is nobody.
class NativeUserJavaInterface : public Object {
public:
    static jlong getUserId(ENV*, Class*) {
        trace_identity("getUserId");
        return identity_user_id();
    }
    static jboolean getIsUnder13(ENV*, Class*) {
        // This mirrors account state; it is not the age gate. Roblox enforces age
        // restrictions server-side from the account itself, so a real under-13
        // account is restricted whether or not the client says so here.
        //
        // Which is also why the signed-out answer is false rather than true:
        // defaulting to under-13 protects nobody and degrades the client for the
        // majority of players, who are teens and adults. Signed in, it is the
        // account's own value, straight out of `DID_LOG_IN`.
        trace_identity("getIsUnder13");
        return identity_is_under13();
    }
    static jint getMembershipType(ENV*, Class*) {
        trace_identity("getMembershipType");
        return identity_membership_type();
    }
    static jboolean getHasRobloxSubscription(ENV*, Class*) {
        trace_identity("getHasRobloxSubscription");
        return identity_has_subscription();
    }
    static std::shared_ptr<String> getUsername(ENV*, Class*) {
        trace_identity("getUsername");
        return str(identity_username().c_str());
    }
    static std::shared_ptr<String> getDisplayName(ENV*, Class*) {
        trace_identity("getDisplayName");
        return str(identity_display_name().c_str());
    }
    static std::shared_ptr<String> getAlternateName(ENV*, Class*) {
        // Left empty even when signed in. `DID_LOG_IN` carries no alternate
        // name, and this is the field Roblox uses for a region-specific second
        // name; echoing the username into it would tell the engine an alternate
        // name exists when the notification never said one did.
        return str("");
    }
    static std::shared_ptr<String> getPlatformName(ENV*, Class*) {
        trace_identity("getPlatformName");
        return str(platform_name());
    }
    static std::shared_ptr<String> getTheme(ENV*, Class*) { return str("Dark"); }

    static void Register(ENV* env) {
        env->GetClass<NativeUserJavaInterface>("com/roblox/engine/jni/user/NativeUserJavaInterface");
        auto c = env->GetClass("com/roblox/engine/jni/user/NativeUserJavaInterface");
        c->Hook(env, "getUserId", &NativeUserJavaInterface::getUserId);
        c->Hook(env, "getIsUnder13", &NativeUserJavaInterface::getIsUnder13);
        c->Hook(env, "getUsername", &NativeUserJavaInterface::getUsername);
        c->Hook(env, "getDisplayName", &NativeUserJavaInterface::getDisplayName);
        c->Hook(env, "getAlternateName", &NativeUserJavaInterface::getAlternateName);
        c->Hook(env, "getPlatformName", &NativeUserJavaInterface::getPlatformName);
        c->Hook(env, "getMembershipType", &NativeUserJavaInterface::getMembershipType);
        c->Hook(env, "getHasRobloxSubscription", &NativeUserJavaInterface::getHasRobloxSubscription);
        c->Hook(env, "getTheme", &NativeUserJavaInterface::getTheme);
    }
};

/// `com.roblox.universalapp.logging.LoggingProtocol`
class LoggingProtocol : public Object {
public:
    /// Milliseconds since the process started. Roblox timestamps its own log
    /// lines against this, so a constant would make every duration zero.
    static jlong getProcessTimestamp(ENV*, Class*) {
        static const auto start = std::chrono::steady_clock::now();
        auto now = std::chrono::steady_clock::now();
        return std::chrono::duration_cast<std::chrono::milliseconds>(now - start).count();
    }

    static void Register(ENV* env) {
        env->GetClass<LoggingProtocol>("com/roblox/universalapp/logging/LoggingProtocol");
        auto c = env->GetClass("com/roblox/universalapp/logging/LoggingProtocol");
        c->Hook(env, "getProcessTimestamp", &LoggingProtocol::getProcessTimestamp);
    }
};

/// The directory Roblox may write to.
///
/// On Android this is the app's private storage. Here it is per-instance, which
/// is the mechanism the multi-account design rests on: two instances that never
/// share a files directory can never share a session.
/// See docs/design/instances-and-launch.md §4.
/// Set from Rust, which is the only side that knows which profile is active.
///
/// Empty until `cordial_set_files_dir` runs, which is the state the fallback
/// below exists for.
std::string g_files_dir;

const char* files_dir() {
    static const std::string dir = [] {
        if (const char* override = getenv("CORDIAL_FILES_DIR")) {
            return std::string(override);
        }
        // What Rust passed to `nativeSetFilesDirectory`, when it has.
        if (!g_files_dir.empty()) {
            return g_files_dir;
        }
        std::string base;
        if (const char* xdg = getenv("XDG_DATA_HOME")) {
            base = xdg;
        } else if (const char* home = getenv("HOME")) {
            base = std::string(home) + "/.local/share";
        } else {
            base = "/tmp";
        }
        // Pre-ADR-012 layout, kept only as the last resort for a caller that
        // runs before the setter. ADR-012 moved storage to
        // `cordial/profiles/<name>/` and this path names neither the new layout
        // nor any particular profile, so anything that reaches it is answering
        // about a directory the client is not using.
        auto path = base + "/cordial/instances/default/data";
        // Roblox assumes the directory exists; on Android the platform made it.
        std::string acc;
        for (size_t i = 1; i <= path.size(); i++) {
            if (i == path.size() || path[i] == '/') {
                acc = path.substr(0, i);
                mkdir(acc.c_str(), 0700);
            }
        }
        return path;
    }();
    return dir.c_str();
}

/// Tell the framework layer which files directory the active profile uses.
///
/// C++ cannot work this out. ADR-012 moved storage to
/// `cordial/profiles/<name>/`, and which name is active is a `--profile`
/// decision that only Rust has; `files_dir()` was still computing
/// `cordial/instances/default/data`, the layout ADR-012 replaced, so it named a
/// directory the client was not using and would have named the same one for
/// every profile.
///
/// This is the third place that trap has been hit. `SharedPreferences::path`
/// and `getAllocatableBytes` both work around it by hanging off the process
/// working directory, and both carry a comment saying why. Those workarounds are
/// correct for what they do and are left alone -- the preferences store and the
/// free-space measurement genuinely want the directory the client is running in
/// -- but a third caller should not have to discover the same thing.
///
/// **`files_dir()` latches on first use**, so this has to run before anything
/// asks. Rust calls it beside `nativeSetFilesDirectory`, with the same value, so
/// the engine and the framework layer cannot disagree about where files live.
extern "C" void cordial_set_files_dir(const char* dir) {
    g_files_dir = dir ? dir : "";
}

/// `com.roblox.engine.jni.reporter.SessionReporterJavaInterface`
///
/// Crash and session telemetry. The reporting entry points are deliberately
/// inert — Cordial is not going to forward a user's session data to an analytics
/// endpoint on their behalf — but the getters have to answer, because the engine
/// uses `getFilesDir` for real storage and not merely for reports.
class SessionReporterJavaInterface : public Object {
public:
    static std::shared_ptr<String> getFilesDir(ENV*, Class*) { return str(files_dir()); }
    static std::shared_ptr<String> getAppVersion(ENV*, Class*) { return str(""); }
    static std::shared_ptr<String> getLastLoggedInUser(ENV*, Class*) { return str(""); }
    static std::shared_ptr<String> getLastLoggedInUserId(ENV*, Class*) { return str(""); }

    static void sendSessionReport(ENV*, Class*, std::shared_ptr<String>, std::shared_ptr<String>) {
        // Inert on purpose. See the class comment.
    }
    static void setEventTrackingGoogleAnalytics(ENV*, Class*, std::shared_ptr<String>,
                                                std::shared_ptr<String>,
                                                std::shared_ptr<String>, jlong) {
        // Likewise.
    }

    static void Register(ENV* env) {
        env->GetClass<SessionReporterJavaInterface>(
            "com/roblox/engine/jni/reporter/SessionReporterJavaInterface");
        auto c = env->GetClass("com/roblox/engine/jni/reporter/SessionReporterJavaInterface");
        c->Hook(env, "getFilesDir", &SessionReporterJavaInterface::getFilesDir);
        c->Hook(env, "getAppVersion", &SessionReporterJavaInterface::getAppVersion);
        c->Hook(env, "getLastLoggedInUser", &SessionReporterJavaInterface::getLastLoggedInUser);
        c->Hook(env, "getLastLoggedInUserId", &SessionReporterJavaInterface::getLastLoggedInUserId);
        c->Hook(env, "sendSessionReport", &SessionReporterJavaInterface::sendSessionReport);
        c->Hook(env, "setEventTrackingGoogleAnalytics",
                &SessionReporterJavaInterface::setEventTrackingGoogleAnalytics);
    }
};

/// `com.roblox.engine.jni.video.VideoCodecCapability`
class VideoCodecCapability : public Object {
public:
    static void Register(ENV* env) {
        env->GetClass<VideoCodecCapability>("com/roblox/engine/jni/video/VideoCodecCapability");
    }
};

/// `com.roblox.engine.jni.video.MediaCodecInfoUtils`
///
/// Hardware video codecs, which on Android come from MediaCodec. Cordial has no
/// MediaCodec: `libmediandk` is entirely stubbed. Reporting none is correct
/// rather than merely convenient — claiming a codec Cordial cannot decode would
/// fail later, inside video playback, with no way back to this decision.
class MediaCodecInfoUtils : public Object {
public:
    static std::shared_ptr<jnivm::Array<VideoCodecCapability>> getVideoCodecs(ENV*, Class*) {
        return std::make_shared<jnivm::Array<VideoCodecCapability>>(0);
    }
    static jboolean hevcHardwareEncodingSupported(ENV*, Class*, jint, jint, jint) {
        return false;
    }

    static void Register(ENV* env) {
        env->GetClass<MediaCodecInfoUtils>("com/roblox/engine/jni/video/MediaCodecInfoUtils");
        auto c = env->GetClass("com/roblox/engine/jni/video/MediaCodecInfoUtils");
        c->Hook(env, "getVideoCodecs", &MediaCodecInfoUtils::getVideoCodecs);
        c->Hook(env, "hevcHardwareEncodingSupported",
                &MediaCodecInfoUtils::hevcHardwareEncodingSupported);
    }
};

/// `android.os.Build$VERSION`
///
/// One static field, and the engine version-gates on it. From the JNI trace,
/// asked for and not answered:
///
///     Constructed Unresolved symbol, Class=`android/os/Build$VERSION`,
///       StaticField=`SDK_INT`, Signature=`I`
///
/// An unanswered `SDK_INT` is not a harmless gap. Code reading it is choosing
/// between two paths, and the answer for a field nobody registered is zero —
/// which reads as an Android older than any release, so every
/// `if (SDK_INT >= X)` takes the legacy branch. A wrong answer delivered
/// confidently, which is the failure this project has a rule about.
///
/// **33, because that is what the capture shows the real client running.**
/// `docs/traces/` has the same APK on real Android reporting Android 13 in its
/// User-Agent, and 13 is API level 33. Sourced rather than picked: a number
/// invented here would be a guess about which paths Roblox takes, and not
/// having to guess is the entire point of the capture.
///
/// Raise it when a future Roblox build wants a newer platform, and choose the
/// new value from what the trace says the real client reports — not from
/// whatever Android is current.
class BuildVersion : public jnivm::Object {
public:
    static jint SDK_INT;

    static void Register(ENV* env) {
        env->GetClass<BuildVersion>("android/os/Build$VERSION");
        auto c = env->GetClass("android/os/Build$VERSION");
        c->Hook(env, "SDK_INT", &BuildVersion::SDK_INT);
    }
};

jint BuildVersion::SDK_INT = 33;

} // namespace cordial

// Defined in accessibility.cpp — kept in its own file rather than added to
// this one because it answers a platform surface (`android.view.accessibility.*`)
// nothing else here touches, and because its own header comment is long
// enough (the push-vs-pull accessibility-tree question) that folding it into
// this file's already-long one would bury it.
namespace cordial {
void register_accessibility_classes(jnivm::ENV* env);
}

// Defined in cookies.cpp, and in its own file for the same reason: it answers
// one surface (`com.roblox.universalapp.cookie.*`) whose header comment has to
// carry the measurement that a session is not persisted anywhere on disk by
// the engine itself, which is long and does not belong in this file's preamble.
namespace cordial {
void register_cookie_classes(jnivm::ENV* env);
}

// Defined in audio_classes.cpp. Separate for the usual reason and one specific
// one: that file's header states the rule that no PipeWire capture stream may
// exist while Roblox is not recording, and that rule has to be the first thing
// anyone editing the microphone path reads rather than a paragraph buried in
// this file's preamble.
namespace cordial {
void register_audio_classes(jnivm::ENV* env);
}

// Defined in clipboard.cpp. Separate because its header comment has to carry
// the finding that `android.content.ClipboardManager` is not a class the engine
// ever asks for — the framework-class inventory lists it because Roblox's own
// Java uses it — and that copying out of an experience arrives as a message-bus
// publish instead. That is the first thing anyone who goes looking for a
// clipboard class needs to read, and it would be buried here.
namespace cordial {
void register_clipboard_classes(jnivm::ENV* env);
}

// Defined in local_storage.cpp. Separate for the same reason as the three
// above, and specifically because that file's header has to carry the
// distinction between `RbxStorage` (the content cache, unrelated) and
// `ILocalStorageHandlerCore`/`IPlatformLocalStorageHandler` (this), which
// `docs/analysis/flag-init.md` §12 spent a session establishing and which
// belongs next to the code it explains rather than buried here.
namespace cordial {
void register_local_storage_classes(jnivm::ENV* env);
}

// Defined in platform_classes.cpp. Separate because its header comment has to
// carry the direction-of-call reasoning for `ActivityThread`/`Application` —
// why the engine's own native code reaches for a `Context` this way rather
// than through the app's Java bootstrap Cordial never runs — and the account
// of which of the fork's requested classes were checked against the dex and
// found absent, neither of which belongs buried in this file's preamble.
namespace cordial {
void register_platform_classes(jnivm::ENV* env);
}

// Defined in battery.cpp. Separate because its header comment has to carry
// where `BatteryStatus`'s field names came from (`tools/dex_fields.py`, a
// declaration-only reader this task added alongside `dex_method.py`) and what
// is and is not confirmed about them without a live run to watch, neither of
// which belongs buried in this file's preamble.
namespace cordial {
void register_battery_classes(jnivm::ENV* env);
}

// -------------------------------------------------------- the identity, in and out
//
// Cordial's own boundary, not Roblox's: `crates/cordial-runtime/src/identity.rs`
// owns the profile directory, the parse and the file, and this side owns the
// mirrors the engine reads. Split there rather than parsing JSON here for the
// same reason `cookies.cpp` hands out a host and keeps the jar — the half that
// touches personal data should be the half with the tests and the `Debug` that
// refuses to print it.

/// Hand the mirrors an identity. Called on a restore before anything starts,
/// and again on every `DID_LOG_IN`.
///
/// Copies out of both pointers before returning, so the caller may free them.
extern "C" void cordial_identity_publish(long long user_id, const char* username,
                                         const char* display_name, long long membership_type,
                                         int is_under13, int has_subscription) {
    std::lock_guard<std::mutex> lock(cordial::g_identity_mutex);
    cordial::g_identity_user_id = static_cast<jlong>(user_id);
    cordial::g_identity_username = username ? username : "";
    cordial::g_identity_display_name = display_name ? display_name : "";
    cordial::g_identity_membership = static_cast<jint>(membership_type);
    cordial::g_identity_under13 = is_under13 != 0;
    cordial::g_identity_subscription = has_subscription != 0;
    cordial::g_identity_known = true;
}

/// Put the mirrors back to reporting nobody.
///
/// Everything is reset, not just the id. A leftover username with a zeroed id
/// is a self-contradicting client, and the engine reads the two through
/// different calls at different times, so it would see exactly that.
extern "C" void cordial_identity_clear() {
    std::lock_guard<std::mutex> lock(cordial::g_identity_mutex);
    cordial::g_identity_user_id = 0;
    cordial::g_identity_username.clear();
    cordial::g_identity_display_name.clear();
    cordial::g_identity_membership = 0;
    cordial::g_identity_under13 = false;
    cordial::g_identity_subscription = false;
    cordial::g_identity_known = false;
}

/// Install the sinks `onDataModelNotificationCallback` reports through, or
/// clear them with null.
///
/// Separate from class registration so the control run — same binary,
/// `CORDIAL_SKIP_IDENTITY=1` — differs in exactly whether anything is
/// listening, rather than in whether the engine's callback resolves at all.
/// That is the same split `cordial_cookies_set_host_sink` makes, and for the
/// same reason: a behavioural difference must not be confusable with a
/// registration failure.
extern "C" void cordial_identity_set_sinks(void (*on_login)(const char*),
                                           void (*on_logout)()) {
    cordial::g_identity_login_sink.store(on_login, std::memory_order_release);
    cordial::g_identity_logout_sink.store(on_logout, std::memory_order_release);
}

/// Where `APP_READY` is reported, or null to stop reporting it.
///
/// Same split as the two above: registration is unconditional and the sink is
/// what decides whether anything listens, so "nobody was listening" can never
/// be mistaken for "the engine never called".
extern "C" void cordial_app_ready_set_sink(void (*on_ready)(const char*)) {
    cordial::g_app_ready_sink.store(on_ready, std::memory_order_release);
}

namespace cordial {

/// `android.content.SharedPreferences` and its `Editor`.
///
/// The engine writes key/value state through this and, until now, every write
/// went nowhere: `Context.getSharedPreferences` was an unresolved symbol, so
/// libjnivm handed back a placeholder and the engine then called `edit()` and
/// `putString()` on the `Invalid` class it got — visible in a JNI trace as
/// `Call Unknown Member Function Class=Invalid Method=putString`.
///
/// Backed by a real file per preference name under the instance's own files
/// directory, so it survives a restart and, because `files_dir()` is
/// per-instance, two profiles cannot see each other's — the same property the
/// multi-account design rests on everywhere else (ADR-012).
///
/// **This is not a fix for 304** and was not written as one. It came out of a
/// sweep looking for integrity checks, and it is here because an unanswered
/// write is a `broken_feature` gap whatever else is going on.
///
/// The file format is Cordial's own. Android would write XML; nothing reads
/// this except the code below, and a line-oriented file is far easier to look
/// at when a question is "did the engine actually store that". One
/// type-tagged record per line, value newline-escaped:
///
/// ```text
/// S<TAB>key<TAB>value
/// ```
class SharedPreferences : public Object {
public:
    static std::string escape(const std::string& v) {
        std::string out;
        for (char c : v) {
            if (c == '\n') { out += "\\n"; }
            else if (c == '\\') { out += "\\\\"; }
            else { out += c; }
        }
        return out;
    }
    static std::string unescape(const std::string& v) {
        std::string out;
        for (size_t i = 0; i < v.size(); ++i) {
            if (v[i] == '\\' && i + 1 < v.size()) {
                out += (v[++i] == 'n') ? '\n' : v[i];
            } else {
                out += v[i];
            }
        }
        return out;
    }

    std::string name;
    std::map<std::string, std::string> values;
    std::mutex lock;

    /// Relative to the working directory, **not** `files_dir()`.
    ///
    /// That is deliberate and was measured. `files_dir()` hardcodes
    /// `instances/default/data` and does not follow `--profile`, so the first
    /// version of this wrote `CordialTest`'s preferences into the `default`
    /// profile — two profiles sharing one preference store, which is exactly
    /// what ADR-012 exists to prevent. The engine's own `appData` follows the
    /// profile because the client runs with its working directory set there, so
    /// hanging this off the same anchor puts it in the right place by
    /// construction. `unimplemented.rs` resolves its log the same way and for
    /// the same reason.
    ///
    /// The name the engine passes is a full path — observed:
    /// `…/CordialTest/data/files/appData/GlobalBasicSettings_13.xml` — so the
    /// basename is what makes a readable file, and a short digest of the whole
    /// string is appended so two settings files with the same basename in
    /// different directories cannot collide.
    std::string path() const {
        ::mkdir("shared_prefs", 0700);
        std::string base = name;
        auto slash = base.find_last_of('/');
        if (slash != std::string::npos) base = base.substr(slash + 1);
        std::string safe;
        for (char c : base) {
            safe += (std::isalnum(static_cast<unsigned char>(c)) || c == '.' || c == '_' || c == '-')
                        ? c : '_';
        }
        if (safe.empty()) safe = "prefs";
        // FNV-1a over the full name. Not security, just collision avoidance.
        uint64_t h = 1469598103934665603ULL;
        for (unsigned char c : name) { h ^= c; h *= 1099511628211ULL; }
        char digest[9];
        snprintf(digest, sizeof digest, "%08x", static_cast<unsigned>(h & 0xffffffffu));
        return "shared_prefs/" + safe + "-" + digest + ".prefs";
    }

    void load() {
        FILE* f = fopen(path().c_str(), "re");
        if (!f) return;
        char line[8192];
        while (fgets(line, sizeof line, f)) {
            std::string s(line);
            if (!s.empty() && s.back() == '\n') s.pop_back();
            auto a = s.find('\t');
            if (a == std::string::npos) continue;
            auto b = s.find('\t', a + 1);
            if (b == std::string::npos) continue;
            values[s.substr(a + 1, b - a - 1)] = unescape(s.substr(b + 1));
        }
        fclose(f);
    }

    /// Written whole and renamed over the old file. A half-written preferences
    /// file would be indistinguishable from a corrupt one on the next launch,
    /// and this is the store an engine restart reads its own state back out of.
    void save() {
        std::string tmp = path() + ".tmp";
        FILE* f = fopen(tmp.c_str(), "we");
        if (!f) return;
        for (auto& [k, v] : values) {
            fprintf(f, "S\t%s\t%s\n", k.c_str(), escape(v).c_str());
        }
        fclose(f);
        rename(tmp.c_str(), path().c_str());
    }
};

/// Returned by `SharedPreferences.edit()`. Android's editor stages changes and
/// applies them on `apply`/`commit`; this does the same rather than writing
/// through, because the engine batches and a write-through would fsync per key.
class SharedPreferencesEditor : public Object {
public:
    std::shared_ptr<SharedPreferences> owner;
    std::map<std::string, std::string> staged;
    std::vector<std::string> removed;
    bool clear_all = false;

    // Returned by every put/remove/clear so the engine can chain them, exactly
    // as Android's Editor does. libjnivm converts a `shared_ptr` return value
    // itself, so there is no `to_jni` here and must not be.
    std::shared_ptr<SharedPreferencesEditor> self(ENV*) {
        return std::static_pointer_cast<SharedPreferencesEditor>(shared_from_this());
    }
    std::shared_ptr<SharedPreferencesEditor> putString(ENV* env, std::shared_ptr<String> k,
                                                       std::shared_ptr<String> v) {
        if (k) staged[*k] = v ? std::string(*v) : std::string();
        return self(env);
    }
    std::shared_ptr<SharedPreferencesEditor> putInt(ENV* env, std::shared_ptr<String> k, jint v) {
        if (k) staged[*k] = std::to_string(v);
        return self(env);
    }
    std::shared_ptr<SharedPreferencesEditor> putLong(ENV* env, std::shared_ptr<String> k, jlong v) {
        if (k) staged[*k] = std::to_string(static_cast<long long>(v));
        return self(env);
    }
    std::shared_ptr<SharedPreferencesEditor> putFloat(ENV* env, std::shared_ptr<String> k, jfloat v) {
        if (k) staged[*k] = std::to_string(v);
        return self(env);
    }
    std::shared_ptr<SharedPreferencesEditor> putBoolean(ENV* env, std::shared_ptr<String> k,
                                                        jboolean v) {
        if (k) staged[*k] = v ? "true" : "false";
        return self(env);
    }
    std::shared_ptr<SharedPreferencesEditor> remove(ENV* env, std::shared_ptr<String> k) {
        if (k) removed.push_back(*k);
        return self(env);
    }
    std::shared_ptr<SharedPreferencesEditor> clear(ENV* env) {
        clear_all = true;
        return self(env);
    }
    void flush() {
        if (!owner) return;
        std::lock_guard<std::mutex> g(owner->lock);
        if (clear_all) owner->values.clear();
        for (auto& k : removed) owner->values.erase(k);
        for (auto& [k, v] : staged) owner->values[k] = v;
        owner->save();
        staged.clear();
        removed.clear();
        clear_all = false;
    }
    void apply(ENV*) { flush(); }
    /// `commit` returns whether the write happened. It is written to disk
    /// synchronously here, so `true` is the truth rather than optimism — but
    /// only when there is a store behind it, which is why the null case says
    /// false instead of pretending.
    jboolean commitEditor(ENV*) {
        if (!owner) return JNI_FALSE;
        flush();
        return JNI_TRUE;
    }

    static void Register(ENV* env) {
        env->GetClass<SharedPreferencesEditor>("android/content/SharedPreferences$Editor");
        auto c = env->GetClass("android/content/SharedPreferences$Editor");
        c->HookInstanceFunction(env, "putString", &SharedPreferencesEditor::putString);
        c->HookInstanceFunction(env, "putInt", &SharedPreferencesEditor::putInt);
        c->HookInstanceFunction(env, "putLong", &SharedPreferencesEditor::putLong);
        c->HookInstanceFunction(env, "putFloat", &SharedPreferencesEditor::putFloat);
        c->HookInstanceFunction(env, "putBoolean", &SharedPreferencesEditor::putBoolean);
        c->HookInstanceFunction(env, "remove", &SharedPreferencesEditor::remove);
        c->HookInstanceFunction(env, "clear", &SharedPreferencesEditor::clear);
        c->HookInstanceFunction(env, "apply", &SharedPreferencesEditor::apply);
        c->HookInstanceFunction(env, "commit", &SharedPreferencesEditor::commitEditor);
    }
};

/// The getters, and the `edit()` that hands out the editor above.
class SharedPreferencesImpl {
public:
    static std::shared_ptr<SharedPreferencesEditor> edit(ENV*, SharedPreferences* self) {
        auto e = std::make_shared<SharedPreferencesEditor>();
        e->owner = std::static_pointer_cast<SharedPreferences>(self->shared_from_this());
        return e;
    }
    static std::shared_ptr<String> getString(ENV*, SharedPreferences* self,
                                             std::shared_ptr<String> k,
                                             std::shared_ptr<String> fallback) {
        std::lock_guard<std::mutex> g(self->lock);
        auto it = k ? self->values.find(*k) : self->values.end();
        if (it == self->values.end()) return fallback;
        return std::make_shared<String>(it->second);
    }
    static jint getInt(ENV*, SharedPreferences* self, std::shared_ptr<String> k, jint fallback) {
        std::lock_guard<std::mutex> g(self->lock);
        auto it = k ? self->values.find(*k) : self->values.end();
        if (it == self->values.end()) return fallback;
        try { return std::stoi(it->second); } catch (...) { return fallback; }
    }
    static jlong getLong(ENV*, SharedPreferences* self, std::shared_ptr<String> k, jlong fallback) {
        std::lock_guard<std::mutex> g(self->lock);
        auto it = k ? self->values.find(*k) : self->values.end();
        if (it == self->values.end()) return fallback;
        try { return std::stoll(it->second); } catch (...) { return fallback; }
    }
    static jfloat getFloat(ENV*, SharedPreferences* self, std::shared_ptr<String> k,
                           jfloat fallback) {
        std::lock_guard<std::mutex> g(self->lock);
        auto it = k ? self->values.find(*k) : self->values.end();
        if (it == self->values.end()) return fallback;
        try { return std::stof(it->second); } catch (...) { return fallback; }
    }
    static jboolean getBoolean(ENV*, SharedPreferences* self, std::shared_ptr<String> k,
                               jboolean fallback) {
        std::lock_guard<std::mutex> g(self->lock);
        auto it = k ? self->values.find(*k) : self->values.end();
        if (it == self->values.end()) return fallback;
        return it->second == "true" ? JNI_TRUE : JNI_FALSE;
    }
    static jboolean contains(ENV*, SharedPreferences* self, std::shared_ptr<String> k) {
        std::lock_guard<std::mutex> g(self->lock);
        return (k && self->values.count(*k)) ? JNI_TRUE : JNI_FALSE;
    }

    static void Register(ENV* env) {
        env->GetClass<SharedPreferences>("android/content/SharedPreferences");
        auto c = env->GetClass("android/content/SharedPreferences");
        c->HookInstanceFunction(env, "edit", &SharedPreferencesImpl::edit);
        c->HookInstanceFunction(env, "getString", &SharedPreferencesImpl::getString);
        c->HookInstanceFunction(env, "getInt", &SharedPreferencesImpl::getInt);
        c->HookInstanceFunction(env, "getLong", &SharedPreferencesImpl::getLong);
        c->HookInstanceFunction(env, "getFloat", &SharedPreferencesImpl::getFloat);
        c->HookInstanceFunction(env, "getBoolean", &SharedPreferencesImpl::getBoolean);
        c->HookInstanceFunction(env, "contains", &SharedPreferencesImpl::contains);
    }
};

/// One store per name for the process's lifetime, which is what Android
/// guarantees: two `getSharedPreferences("x", …)` calls return the same
/// instance, so a write through one is visible to the other without a reload.
static std::shared_ptr<SharedPreferences> prefs_named(ENV*, const std::string& name) {
    static std::mutex reg_lock;
    static std::map<std::string, std::shared_ptr<SharedPreferences>> registry;
    std::lock_guard<std::mutex> g(reg_lock);
    auto it = registry.find(name);
    if (it != registry.end()) {
        return it->second;
    }
    auto p = std::make_shared<SharedPreferences>();
    p->name = name;
    p->load();
    registry[name] = p;
    return p;
}

/// `Context.getSharedPreferences(String, int)`.
///
/// The mode argument is ignored, and that is correct rather than lazy: every
/// value it can take describes multi-process sharing on Android
/// (`MODE_MULTI_PROCESS` and friends), and Cordial's whole storage model is one
/// instance per profile holding an `flock` — there is no second process to
/// share with by construction.
static std::shared_ptr<SharedPreferences> context_get_shared_preferences(
    ENV* env, Object*, std::shared_ptr<String> name, jint) {
    return prefs_named(env, name ? std::string(*name) : std::string("default"));
}

void register_shared_preferences(ENV* env) {
    SharedPreferencesEditor::Register(env);
    SharedPreferencesImpl::Register(env);
    // Hooked on all three, because the engine resolved the method against
    // `android/content/Context` but calls it on whatever object it is holding.
    // `android/app/Application` joined `Activity` here for the same reason
    // `platform_classes.cpp`'s own header documents at length: since
    // `ActivityThread.getApplication()` started handing the engine a real
    // `android/app/Application` object, that object is a third kind of thing
    // capable of being asked this question, and libjnivm does not walk from a
    // class to a same-shaped-but-differently-named one to find a hook that was
    // only ever registered under the other name.
    for (const char* klass :
         {"android/content/Context", "android/app/Activity", "android/app/Application"}) {
        env->GetClass<Object>(klass);
        env->GetClass(klass)->HookInstanceFunction(env, "getSharedPreferences",
                                                   &context_get_shared_preferences);
    }
    // Put `jnivm::Object` back where `VM::VM` left it, and do not remove this.
    //
    // libjnivm keeps ONE class per C++ type -- `VM::typecheck`, keyed by
    // `typeid` -- and `GetClass<T>(name)` overwrites that entry. Every
    // signature libjnivm derives for a hook is built from it, so after the
    // loop above the last name registered won, and any `std::shared_ptr<Object>`
    // or `jobject` in a hook signature came out as `Landroid/app/Application;`
    // rather than `Ljava/lang/Object;`. A hook registered after this point
    // could then never match the engine's lookup, and the failure is silent:
    // the trace says `Constructed Unresolved symbol` with the signature the
    // engine asked for, which is the correct one, and nothing says the
    // registered side spelled it differently.
    //
    // Measured, not deduced. Registering `java/lang/ref/WeakReference` in
    // `local_storage.cpp` -- which runs immediately after this function --
    // produced `<init>` at `(Landroid/app/Application;)Ljava/lang/ref/
    // WeakReference;` against an engine asking for
    // `(Ljava/lang/Object;)Ljava/lang/ref/WeakReference;`, printed side by side
    // out of the class's own method table. See docs/analysis/flag-init.md §40.
    env->GetClass<Object>("java/lang/Object");
}

/// `Context.checkSelfPermission(String)`, needed for voice chat's uplink:
/// `native/audio_classes.cpp`'s `WebRtcAudioRecord` implements
/// `RECORD_AUDIO` capture, and the real Android class this stands in for
/// checks this before ever calling `startRecording`.
///
/// `tools/dex_method.py` finds exactly one declared caller-side shape —
/// `android/content/Context.checkSelfPermission(Ljava/lang/String;)I` — and no
/// `android/support/v4/content/ContextCompat` or
/// `androidx/core/content/ContextCompat`/`ActivityCompat` in this dex at all,
/// so there is no compat shape to answer as well as the platform one. Two
/// close relatives are declared and are not implemented here on the same
/// "not observed" grounds `platform_classes.cpp`'s own header applies to
/// `PackageManager` — `checkPermission(String,int,int)` and
/// `checkCallingOrSelfPermission(String)` — both answerable identically if
/// something is ever seen calling them.
///
/// **Always grants**, regardless of which permission string is asked about.
/// That is not a shortcut specific to `RECORD_AUDIO`: Cordial has no
/// permission system to consult for any permission, on this platform or any
/// other Android one Roblox might ask about, so "granted" is the only honest
/// answer available rather than one this file happens to prefer. The
/// microphone rule at the top of `audio_classes.cpp` is what actually keeps
/// the capture stream honest — tied to `startRecording`/`stopRecording`, not
/// to a permission dialog nothing here can show. `PackageManager
/// .PERMISSION_GRANTED` is `0`, a stable, long-public Android SDK constant
/// the calling bytecode already has inlined at every call site rather than
/// something this file looks up.
static jint context_check_self_permission(ENV*, Object*, std::shared_ptr<String>) {
    return 0; // PackageManager.PERMISSION_GRANTED
}

/// **`tools/hook_descriptors.py` does not check the hooks this function
/// registers, and an earlier report claimed it did.** That tool only reads
/// hook calls textually inside a method literally named `Register` (its own
/// regex: `static void Register\s*\([^)]*\)\s*\{...`), because "each
/// `Register()` body names one Java class then hooks onto it" is the shape
/// every other class in this file follows. This function is a free function
/// registering the same class three times in a loop instead, so it is
/// invisible to that regex — silently, the same way a descriptor mismatch
/// itself is silent, which is the exact failure mode the tool exists to
/// catch and here does not.
///
/// Checked by hand instead, against the shipping dex, on 2026-08-28:
///
///     $ python3 tools/dex_method.py <dex-dir> checkSelfPermission
///     android/content/Context.checkSelfPermission(Ljava/lang/String;)I
///
/// which matches what `context_check_self_permission` above registers --
/// instance (`HookInstanceFunction`), one `String` parameter, `jint` return --
/// and the same lookup restricted to `--class android/app/Activity` and
/// `--class android/app/Application` found no match in either, confirming the
/// comment above that only `Context` declares this call in the dex. This
/// hand check covers exactly the one descriptor below; it is not a substitute
/// for `hook_descriptors.py` learning to see free-function registrations, and
/// nobody has made that change.
void register_permission_checks(ENV* env) {
    // Same three-class loop as `register_shared_preferences` above, and for
    // the identical reason: the engine resolved this call against
    // `android/content/Context` but the receiver it holds at any given call
    // site may actually be the `Activity` or `Application` object
    // `platform_classes.cpp` hands back from `ActivityThread.getApplication()`,
    // and libjnivm dispatches on the receiver's own registered class rather
    // than walking from it to a same-shaped differently-named one.
    for (const char* klass :
         {"android/content/Context", "android/app/Activity", "android/app/Application"}) {
        env->GetClass<Object>(klass);
        env->GetClass(klass)->HookInstanceFunction(env, "checkSelfPermission",
                                                    &context_check_self_permission);
    }
    // Same reset as `register_shared_preferences` above, and required for the
    // same measured reason: the loop just above leaves `jnivm::Object` mapped
    // to whichever of the three classes it last touched, and every hook
    // registered after this point derives its descriptor from that mapping.
    env->GetClass<Object>("java/lang/Object");
}

} // namespace cordial

extern "C" void cordial_register_android_classes(void* env_ptr) {
    auto* env = static_cast<jnivm::ENV*>(env_ptr);
    if (!env) {
        return;
    }
    cordial::StringBridge::Register(env);
    cordial::DeviceStaticParams::Register(env);
    cordial::NativeTextBoxInfo::Register(env);
    cordial::NativeGLJavaInterface::Register(env);
    cordial::NativeLocaleJavaInterface::Register(env);
    cordial::NativeUserJavaInterface::Register(env);
    cordial::LoggingProtocol::Register(env);
    cordial::SessionReporterJavaInterface::Register(env);
    cordial::VideoCodecCapability::Register(env);
    cordial::MediaCodecInfoUtils::Register(env);
    cordial::BuildVersion::Register(env);
    cordial::register_accessibility_classes(env);
    cordial::register_cookie_classes(env);
    cordial::register_audio_classes(env);
    cordial::register_clipboard_classes(env);
    cordial::register_shared_preferences(env);
    cordial::register_permission_checks(env);
    cordial::register_local_storage_classes(env);
    cordial::register_platform_classes(env);
    cordial::register_battery_classes(env);
    if (getenv("CORDIAL_JNI_TRACE")) {
        fprintf(stderr, "[classes] Cordial's Java side registered\n");
    }
}

// ------------------------------------------------- asking the engine directly
//
// Down here rather than beside `cordial_textbox_info` because it needs
// `cordial::NativeTextBoxInfo`, which is defined above and cannot be
// forward-declared usefully.

namespace cordial {
/// Defined in `game_activity.cpp`; declared rather than duplicated for the
/// same reason `make_display_metrics` is declared there.
jnivm::ENV* process_env();
} // namespace cordial

/// `NativeGLInterface.nativeGetTextBoxInfo()` — the engine's own answer to
/// "where is the focused box", as opposed to the one it volunteered at
/// `showKeyboard`.
///
/// **The two disagree, and the getter is the one that catches up.** Roblox's
/// search modal is focused with a spec of `x=0 y=0 w=0 h=0`, because the engine
/// builds it before the modal has laid out; a second later this call returns
/// `x=332 y=10 w=592 h=36` for the same box. Measured twice, identically, on
/// 2026-08-25. See the comment in `sync_text_overlay` for the whole shape of it
/// and for why `showKeyboard` still comes first.
///
/// The dex descriptor is `()Lcom/roblox/engine/jni/model/NativeTextBoxInfo;` —
/// read with `tools/dex_method.py`, not guessed, because a wrong arity here
/// would be the same silent nothing the `<init>` hook was for a year. The
/// object it returns is built through that same hook, so every slot arrives
/// named the way `CordialTextBoxInfo` names them.
///
/// Returns 1 with `*out` filled, 0 when the engine answered null — which it
/// does for the whole of the sign-in page — and -1 on error. **A 0 is not a
/// zeroed box:** `*out` is left untouched, for the reason
/// `cordial_textbox_info` gives at more length.
extern "C" int cordial_textbox_info_now(void* fn, CordialTextBoxInfo* out,
                                        char* err, size_t err_len) {
    using Call = jobject (*)(JNIEnv*, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeGetTextBoxInfo is not exported");
        return -1;
    }
    // **The pop has to happen on every path out, including the throwing one.**
    // This is polled ten times a second while a box is focused with no usable
    // geometry, so a frame leaked per failure is not a one-off -- it is a leak
    // that grows for as long as the box has focus, on the pump thread. Written
    // as a guard rather than a pop before each `return` because the next person
    // to add an early return will not remember.
    struct LocalFrame {
        JNIEnv* jni;
        ~LocalFrame() {
            if (jni) jni->PopLocalFrame(nullptr);
        }
    };
    try {
        auto* jni = env->GetJNIEnv();
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        // `to_jni` parks every object it touches in the current local frame --
        // the same unbounded growth `cordial_game_activity_touch` documents.
        jni->PushLocalFrame(16);
        LocalFrame frame{jni};
        jobject r = reinterpret_cast<Call>(fn)(
            jni,
            (jobject)jnivm::JNITypes<std::shared_ptr<jnivm::Class>>::ToJNIType(env, cls));
        int rc = 0;
        if (r) {
            auto obj =
                jnivm::JNITypes<std::shared_ptr<cordial::NativeTextBoxInfo>>::JNICast(env, r);
            // `spec_known` is false for an object that never went through the
            // `<init>` hook. Reporting that as success would hand the caller a
            // default-constructed struct dressed up as geometry.
            if (obj && obj->spec_known) {
                if (out) *out = obj->spec;
                rc = 1;
            }
        }
        return rc;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}
