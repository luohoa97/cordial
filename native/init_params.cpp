// `MainGameActivity.nativeAppBridgeSetInitParams` — the engine's launch state.
//
// Modern Roblox renders its own app shell rather than using Java UI, so the
// engine draws nothing until it has been told where the service lives, what the
// device is, and what the viewport looks like. That is this object.
//
// Field names and types were read out of the shipping APK's dex, not guessed —
// `InitParams`, `DeviceParams` and `PlatformParams` are plain field-carrying
// classes and libjnivm binds field hooks by name and descriptor.
//
// **This file used to claim that `PlatformParams` is where spec §4.2's "Roblox
// thinks you're mobile" is really answered — that `isKeyboardDevice`,
// `isMouseDevice` and `isTouchDevice` decide which input scheme and which UI
// layout the engine picks. Two thirds of that is false.** Measured with
// `CORDIAL_TRACE_PARAM_READS=1` on a cold start: the engine reads `isTouchDevice`
// twice and `dpiScale` three times, and reads `isKeyboardDevice` and
// `isMouseDevice` **not once**. Setting those two to the desktop answer has
// therefore never told the engine anything. The control in the same run is
// `DeviceParams.deviceName`, whose getter fired and whose value came back out of
// the engine as `[FLog::Graphics] Vulkan Android Device: Cordial`, so the probe
// was working when the two peripheral fields stayed silent.
//
// What that leaves: the engine is told there is no touchscreen, and behaves as a
// mobile client anyway. Whatever carries "there is a keyboard and a mouse here",
// it is not this object. See docs/analysis/platform-identity.md.

#include <jnivm.h>
#include <vector>
#include <sys/statvfs.h>

#include <atomic>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <map>
#include <mutex>
#include <string>

// Defined in android_classes.cpp. Both game-loaded callbacks feed one counter
// so the join watchdog does not have to know which of them a given build calls.
extern "C" void cordial_note_game_loaded(long long place_id);

namespace cordial {
class Surface;

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

using jnivm::Class;
using jnivm::ENV;
using jnivm::Object;
using jnivm::String;

jnivm::ENV* process_env();
std::shared_ptr<String> jstr_shared(const char* v);

/// Who is signed in — defined in `android_classes.cpp`, beside the
/// `NativeUserJavaInterface` mirror it also answers.
///
/// Declared here rather than shared through a header because the whole of the
/// framework layer's cross-file surface is declarations like these, and one
/// header for four accessors would be the only one in `native/`.
///
/// **`StartAppParams` and `NativeUserJavaInterface` have to agree.** They are
/// different calls at different times, and the engine reads both; a client that
/// starts its app shell as nobody and then answers a later query as somebody is
/// self-contradicting in a way that shows up far from here. One source, read
/// twice, is what stops that.
jlong identity_user_id();
std::string identity_username();
jint identity_membership_type();
bool identity_is_under13();
bool identity_known();

namespace {
std::shared_ptr<String> S(const char* v) {
    return std::make_shared<String>(std::string(v ? v : ""));
}
} // namespace

std::shared_ptr<String> S_pub(const char* v) { return S(v); }

/// Screen size the Activity reports. AConfiguration, PlatformParams and
/// DisplayMetrics all have to agree about this.
static int g_width = 1280;
static int g_height = 720;

void set_display_size(int width, int height) {
    g_width = width;
    g_height = height;
}

class AndroidActivity;
std::shared_ptr<Object> make_display_metrics(ENV* env);
/// Defined below with `Insets`; declared here so game_activity.cpp can
/// return one without duplicating the class.
std::shared_ptr<Object> cordial_make_zero_insets(ENV* env);

/// Which device identity Cordial presents to the engine and to roblox.com.
///
/// Three, not two. **The default is unchanged: `pc-windows-11`.** The third
/// identity is shipped as a choice, not as a new default -- a tablet identity
/// was tried by a user and reported breaking PC features, `roblox-app` has no
/// number behind it either way, and the position this project takes when
/// nothing has been measured is that the working default keeps working while
/// the alternatives become things somebody can choose. See the closing
/// paragraph for what would justify moving it.
///
/// **What was measured, on 2026-08-22, before any of this moved.** roblox.com
/// was fetched four times with four `User-Agent` strings and nothing else
/// different, and the served HTML compared. The site's own attributes, off
/// `https://www.roblox.com/games/1818`:
///
/// ```text
/// User-Agent shape             data-app-type  data-device-type  data-is-desktop  site nav bar
/// ROBLOX Windows App .. Desktop uwp            computer          true             absent
/// ROBLOX Android App .. Tablet  android        tablet            false            absent
/// RobloxApp/<v>(GlobalDist;..)  universalapp   computer          true             absent
/// an ordinary browser           unknown        computer          true             PRESENT
/// ```
///
/// That settles the thing this switch was previously afraid of. The
/// in-experience web view gets the site's embedded layout for **every**
/// identity here, and the full desktop site with its own navigation bar only
/// for a browser `User-Agent` -- the branch roblox.com makes is app against
/// browser, not PC against Android. `crates/cordial-shell/src/webview.rs` said
/// the right thing about the browser case and was read as though the *PC*
/// identity were what preserved the in-app layout; it is not, and nothing here
/// trades the web view for a graphics tier.
///
/// [`RobloxApp`] is the bare app token, the shape Sober sends. Read out of
/// Sober's own `appData/LocalStorage/appStorage.json`, key `WebViewUserAgent`,
/// on this machine, rather than inferred:
///
/// ```text
/// Mozilla/5.0 AppleWebKit/605.1.15 (KHTML, like Gecko)  ROBLOX Android App
/// 2.730.790 Tablet Hybrid()  GooglePlayStore RobloxApp/2.730.790(GlobalDist;
/// GooglePlayStore)
/// ```
///
/// **Worth being exact about, because a shorter reading of that same file
/// caused a wrong brief:** Sober does send `ROBLOX Android App` and `Tablet
/// Hybrid()`. What it does *not* send is the parenthesised device block --
/// no memory, no resolution, no DPI, no API level -- and its WebKit token is
/// WebKitGTK's own. Only the trailing `RobloxApp/...(GlobalDist; ...)` clause
/// is what "the bare token" means, and that clause alone is enough for
/// roblox.com to serve the app layout, which the table above is the control
/// for.
///
/// So `roblox-app` claims to be the Roblox app and declines to claim a form
/// factor at all: no phone, no tablet, no Windows PC, and no device block full
/// of numbers whose only purpose is to look like hardware Cordial is not. The
/// channel stays `Cordial` rather than borrowing `GooglePlayStore`, on the same
/// rule every other identity field in this file follows.
///
/// **What this does not do, and must not be read as doing.** It is an identity,
/// not a graphics-quality request. Nothing here has established that Roblox
/// tiers its graphics defaults off the `User-Agent` at all -- the engine builds
/// its picture of the machine from `InitParams` (memory, resolution, density,
/// `isTablet`), which is a different set of values reaching it by a different
/// route. [`AndroidTablet`] is the only identity that asserts a mobile form
/// factor, through `InitParams.isTablet`, and it is the one to reach for if
/// mobile-tier defaults are what is wanted.
///
/// Worth knowing before changing this: the engine being loaded is Roblox's
/// **Android** build whatever this says. `osVersion` stays "33" in all three
/// profiles -- see the comment at that field, which records the engine
/// refusing Vulkan when it was lowered -- so the identity is not internally
/// uniform and nobody should read it as a complete disguise.
///
/// The accepted spellings match `crates/cordial-runtime/src/flags.rs`'s
/// `DeviceProfile::parse` exactly, **including what an unrecognised value
/// does**. That agreement is new. This function used to treat anything it did
/// not recognise as `pc-windows-11` while `flags.rs` treated the same string
/// as `android-tablet` -- not even its own default -- so
/// `CORDIAL_DEVICE_PROFILE=tablett` produced a client reporting one identity
/// in its log while sending the other to the engine, silently. Both sides now
/// fall back to `pc-windows-11` and both say so.
enum class DeviceIdentity { RobloxApp, AndroidTablet, PcWindows11 };

static DeviceIdentity device_identity() {
    static const DeviceIdentity v = [] {
        const char* e = getenv("CORDIAL_DEVICE_PROFILE");
        if (!e) return DeviceIdentity::PcWindows11;
        std::string s(e);
        for (char& c : s) c = static_cast<char>(tolower(static_cast<unsigned char>(c)));
        while (!s.empty() && isspace(static_cast<unsigned char>(s.front()))) s.erase(s.begin());
        while (!s.empty() && isspace(static_cast<unsigned char>(s.back()))) s.pop_back();
        if (s.empty()) return DeviceIdentity::PcWindows11;
        if (s == "roblox-app" || s == "app" || s == "roblox") {
            return DeviceIdentity::RobloxApp;
        }
        if (s == "android-tablet" || s == "android" || s == "tablet") {
            return DeviceIdentity::AndroidTablet;
        }
        if (s == "pc" || s == "pc-windows-11" || s == "windows" || s == "windows-11") {
            return DeviceIdentity::PcWindows11;
        }
        // Reported rather than guessed at, and reported here rather than only
        // in `flags.rs`: this translation unit is the one that actually
        // reaches the engine, so a value it did not understand has to be
        // visible from a run of the client alone.
        fprintf(stderr,
                "[cordial] CORDIAL_DEVICE_PROFILE=\"%s\" is not a device profile; using "
                "pc-windows-11. Known: roblox-app, android-tablet, pc-windows-11\n",
                e);
        return DeviceIdentity::PcWindows11;
    }();
    return v;
}

static const char* device_identity_label() {
    switch (device_identity()) {
        case DeviceIdentity::AndroidTablet: return "android-tablet";
        case DeviceIdentity::PcWindows11:   return "pc-windows-11";
        case DeviceIdentity::RobloxApp:     break;
    }
    return "roblox-app";
}

/// Device-identity strings for surfaces that need brand/model/build fields
/// without inventing a second `device_identity()` switch.
///
/// Exposed (non-static) the same way `S_pub` is: other translation units link
/// against one accessor rather than copy the profile table. `BuildInfo` in
/// `unanswered_classes.cpp` is the first consumer — hooking those getters
/// against a local table would make `CORDIAL_DEVICE_PROFILE` change InitParams
/// and the User-Agent while leaving WebRTC's BuildInfo on a stale answer.
///
/// Values stay inside Cordial's own honest vocabulary (manufacturer/device
/// name already report `"Cordial"`). Only the fields that legitimately differ
/// by profile — model/device/product form factor, and the Android release
/// string — change with the switch. `sdk_version` stays `"33"` for every
/// profile: `DeviceParams.osVersion` is load-bearing for Vulkan and is not
/// varied by profile either.
struct DeviceProfile {
    const char* brand;
    const char* manufacturer;
    const char* model;
    const char* device;
    const char* product;
    const char* build_id;
    const char* build_type;
    const char* build_release;
    const char* sdk_version;
};

const DeviceProfile& device_profile() {
    static const DeviceProfile v = []() -> DeviceProfile {
        switch (device_identity()) {
            case DeviceIdentity::PcWindows11:
                // model matches mocktail's `class=pc model="Windows 11 PC"`
                // line; device/product keep the PC form factor distinct from
                // the tablet identity so a BuildInfo read can see the switch.
                return DeviceProfile{"Cordial", "Cordial", "Windows 11 PC",
                                     "cordial_pc", "cordial_pc", "cordial", "user",
                                     "11", "33"};
            case DeviceIdentity::AndroidTablet:
                return DeviceProfile{"Cordial", "Cordial", "Cordial", "cordial",
                                     "cordial", "cordial", "user", "13", "33"};
            case DeviceIdentity::RobloxApp:
                // Bare app token: no form-factor claim beyond Cordial's own
                // name, matching the User-Agent arm that drops the device block.
                return DeviceProfile{"Cordial", "Cordial", "Cordial", "cordial",
                                     "cordial", "cordial", "user", "13", "33"};
        }
        return DeviceProfile{"Cordial", "Cordial", "Cordial", "cordial", "cordial",
                             "cordial", "user", "13", "33"};
    }();
    return v;
}

/// The `User-Agent` the engine puts on every HTTP request it makes.
///
/// **This was `"Roblox/Android"`, and the comment above it was wrong in the most
/// expensive way available.** It said the string was "Roblox's own client
/// string, not Cordial's: the service routes and gates on it, and a fabricated
/// one would be both untrue and likely rejected". `Roblox/Android` appears
/// **zero times** in `libroblox.so`. It was fabricated, by us, and the comment
/// stated the exact risk of doing so while doing it.
///
/// The real client's is in `docs/traces/waydroid-roblox-startup.log.gz` — the
/// capture AGENTS.md says to grep before investigating anything — and is about
/// 230 characters of structured device data:
///
/// ```text
/// Mozilla/5.0 (15701MB; 3440x1330; 180x180; 2584x999; Waydroid WayDroid x86_64 Device; 13)
///  AppleWebKit/537.36 (KHTML, like Gecko)  ROBLOX Android App 2.732.1043 Tablet Hybrid()
///  GooglePlayStore RobloxApp/2.732.1043 (GlobalDist; GooglePlayStore)
/// ```
///
/// Built here with the same **shape** and Cordial's own **honest values**: real
/// installed memory, the real window size, the density `DisplayMetrics` already
/// reports, the API level `Build.VERSION.SDK_INT` already answers, and the
/// engine version already read out of the binary. The device name stays
/// `Cordial` rather than borrowing a phone's — matching the format is not the
/// same as claiming to be hardware we are not, and every other identity field
/// in this file makes the same choice for the same reason.
///
/// **INFERRED that this matters to the server.** It is a candidate for the 304
/// disconnect because the previous value could not have come from any real
/// client, not because a server-side check has been observed. If it turns out
/// to be inert, the string is still right and the old one was still invented.
///
/// **[`device_identity`] chooses between three shapes**, and that function's
/// own doc carries the measurement that says what each one makes roblox.com
/// serve. Summarised here, because this is where the strings actually are:
///
/// `PcWindows11` swaps exactly the two words that would otherwise contradict
/// `isTablet` below (`Android` and `Tablet`, for `Windows` and `Desktop`),
/// drops the trailing Android API-level slot rather than filling it with a
/// guess, and replaces the two `GooglePlayStore` tokens with `Cordial`. Nobody
/// here has captured what Roblox's actual Windows client sends — only
/// mocktail's own `class=pc model="Windows 11 PC"` line
/// (`docs/analysis/flag-init.md` §13) — so it invents no Windows build number,
/// no NT version, and no other syntax nothing here has seen. **It is also the
/// one identity roblox.com reads as `data-app-type="uwp"`**, i.e. a Microsoft
/// Store app, which Cordial is not; that is a cost of the shape rather than
/// something chosen, and it is part of why it is no longer the default.
///
/// `RobloxApp` is the bare app token and nothing else: no `Mozilla`, no WebKit
/// clause, no `Windows App`/`Android App`, no `Desktop Hybrid`/`Tablet Hybrid`,
/// and **no parenthesised device block at all** — no memory, no resolution, no
/// density, no API level. That last omission is the substantive one. Every
/// number in the other two arms is a description of this machine that Roblox
/// may tier on, and none of it is information the server needs from a
/// `User-Agent` when `InitParams` already carries the same facts on a route the
/// engine actually reads. Dropping it is the smaller claim, not the larger one.
///
/// Everything shared between the arms — memory, resolution, density, engine
/// version, where those appear at all — is unchanged, because none of it is
/// Android- or Windows-specific in the first place.
static std::string build_user_agent() {
    long ram_mb = 0;
    if (FILE* f = fopen("/proc/meminfo", "re")) {
        char line[256];
        while (fgets(line, sizeof line, f)) {
            long kb = 0;
            if (sscanf(line, "MemTotal: %ld kB", &kb) == 1) {
                ram_mb = kb / 1024;
                break;
            }
        }
        fclose(f);
    }

    // The engine build is four parts (2.730.0.790); the app version the real
    // client puts in its User-Agent is three (2.732.1043 for engine
    // 2.732.0.1043), dropping the third. Derived rather than hardcoded, so it
    // cannot go stale across an APK update the way `nativeSetRobloxVersion`'s
    // literal silently did.
    std::string app = "0.0.0";
    if (const char* v = getenv("CORDIAL_ENGINE_VERSION")) {
        std::string ver(v);
        std::vector<std::string> parts;
        size_t start = 0;
        for (size_t i = 0; i <= ver.size(); ++i) {
            if (i == ver.size() || ver[i] == '.') {
                parts.push_back(ver.substr(start, i - start));
                start = i + 1;
            }
        }
        if (parts.size() == 4) {
            app = parts[0] + "." + parts[1] + "." + parts[3];
        }
    }

    char buf[512];
    switch (device_identity()) {
        case DeviceIdentity::PcWindows11:
            snprintf(buf, sizeof buf,
                     "Mozilla/5.0 (%ldMB; %dx%d; 160x160; %dx%d; Cordial) "
                     "AppleWebKit/537.36 (KHTML, like Gecko)  ROBLOX Windows App %s Desktop "
                     "Hybrid()  Cordial RobloxApp/%s (GlobalDist; Cordial)",
                     ram_mb, g_width, g_height, g_width, g_height, app.c_str(), app.c_str());
            break;
        case DeviceIdentity::AndroidTablet:
            snprintf(buf, sizeof buf,
                     "Mozilla/5.0 (%ldMB; %dx%d; 160x160; %dx%d; Cordial; 33) "
                     "AppleWebKit/537.36 (KHTML, like Gecko)  ROBLOX Android App %s Tablet "
                     "Hybrid()  GooglePlayStore RobloxApp/%s (GlobalDist; GooglePlayStore)",
                     ram_mb, g_width, g_height, g_width, g_height, app.c_str(), app.c_str());
            break;
        case DeviceIdentity::RobloxApp:
            // No space before the parenthesis, matching the clause Sober's
            // own `WebViewUserAgent` carries verbatim. The real Android
            // client's capture in `docs/traces/` has a space there and the
            // other two arms above keep it; this arm copies the shape that
            // was read off a working client on this machine rather than
            // normalising it to the other one, because which of the two
            // roblox.com's parser prefers is not something anybody here has
            // established, and the one with a live client behind it is the
            // safer copy.
            //
            // `ram_mb`, `g_width` and `g_height` are deliberately unused
            // here; see the doc above on why the device block is the part
            // worth dropping.
            (void)ram_mb;
            snprintf(buf, sizeof buf, "RobloxApp/%s(GlobalDist; Cordial)", app.c_str());
            break;
    }
    return std::string(buf);
}

} // namespace cordial

/// Hands `build_user_agent`'s exact answer to the Rust side.
///
/// `build_user_agent` has internal linkage (`static`, inside `namespace
/// cordial`), which is correct for a value nothing outside this translation
/// unit is supposed to invent independently — but the desktop web view and
/// `NativeGLInterface.setWebviewUserAgent` both need to present the identical
/// string `InitParams.userAgent` was built with, and they live in
/// `crates/cordial-shell` and `crates/cordial-runtime/src/bin/load.rs`. The
/// alternative was a second copy of this function in Rust, which is exactly
/// the failure mode `docs/analysis/platform-identity.md` and this file's own
/// history warn about: two computations of "what device are we" drifting the
/// moment one changes and the other does not — a stale window size in one, a
/// device profile switch (`device_identity`) forgotten in the other. This
/// is a getter, not a reimplementation: it calls the same function and
/// copies the same bytes out.
///
/// `buf`/`n` rather than returning a `std::string` across the FFI boundary,
/// matching every other Rust-facing native in this tree (`cordial_set_display_size`'s
/// neighbours below, `cordial_messagebus_subscribe` in clipboard.cpp) — none
/// of them hand a C++ standard-library object across the boundary, all of
/// them fill a caller-owned buffer and report how much they needed. Returns
/// the full length of the User-Agent regardless of whether it fit, the same
/// convention `snprintf` itself uses, so a caller with too small a buffer can
/// tell truncation from success rather than silently reading a cut-off
/// string.
extern "C" size_t cordial_build_user_agent(char* buf, size_t n) {
    std::string ua = cordial::build_user_agent();
    if (buf && n > 0) {
        size_t copy = ua.size() < n - 1 ? ua.size() : n - 1;
        ua.copy(buf, copy);
        buf[copy] = '\0';
    }
    return ua.size();
}

namespace cordial {

/// `android.util.DisplayMetrics`
///
/// The engine asks the Activity for these and reads `density` off the result.
/// Android's density is the scale factor against 160 dpi — a desktop display at
/// roughly 96 dpi is therefore *below* 1.0, not above it, and reporting a
/// phone's 2.5-3.0 here would make the client lay itself out for a screen held
/// at arm's length.
class DisplayMetrics : public Object {
public:
    jfloat density = 1.0f;
    jfloat scaledDensity = 1.0f;
    jfloat xdpi = 96.0f;
    jfloat ydpi = 96.0f;
    jint densityDpi = 160;
    jint widthPixels = 1280;
    jint heightPixels = 720;

    static std::shared_ptr<DisplayMetrics> Create(ENV* env, int width, int height) {
        auto p = std::make_shared<DisplayMetrics>();
        p->widthPixels = width;
        p->heightPixels = height;
        // 1.0 means "one density-independent pixel is one real pixel", which is
        // what a desktop window wants: no scaling, no phone-sized controls.
        p->density = 1.0f;
        p->scaledDensity = 1.0f;
        p->densityDpi = 160;
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<DisplayMetrics>("android/util/DisplayMetrics");
        auto c = env->GetClass("android/util/DisplayMetrics");
#define F(name) c->HookInstance(env, #name, &DisplayMetrics::name)
        F(density); F(scaledDensity); F(xdpi); F(ydpi); F(densityDpi);
        F(widthPixels); F(heightPixels);
#undef F
    }
};


/// `android.content.res.Configuration`
///
/// Eighteen fields, every one of them read by the engine and none of them
/// answered before this. From the JNI inventory in
/// `docs/analysis/unresolved-jni.tsv`, all eighteen arrived as
/// `Constructed Unresolved symbol`, which answers zero.
///
/// **Zero is a specific wrong answer here, not a blank.** `orientation` 0 is
/// `ORIENTATION_UNDEFINED`, `densityDpi` 0 is a screen with no pixels, and
/// `fontScale` 0.0 scales every glyph to nothing. A layout engine given those
/// does not fail — it lays out for an impossible device.
///
/// Nothing here is invented. The numeric fields are derived from the same
/// `g_width`/`g_height` and the same 160 dpi that `DisplayMetrics` above
/// already reports, because the comment on `set_display_size` says AConfiguration,
/// PlatformParams and DisplayMetrics all have to agree — and three sources
/// disagreeing about the screen is worse than one being absent. At density 1.0
/// a density-independent pixel *is* a real pixel, so `screenWidthDp` is the
/// pixel width and no conversion is hiding here.
///
/// The categorical fields describe this machine honestly rather than imitating
/// a phone: a desktop has a hardware keyboard, no touchscreen, and no D-pad.
/// Their values are Android's own documented constants, named below so the
/// numbers are readable rather than magic.
// Both defined at the bottom of this file, beside their setters, and declared
// here because the fields that use them are built above that point.
//
// **Not wrapped in another `namespace cordial {}`, which is what this used to
// be.** Everything from `namespace cordial {` a few hundred lines up to
// `} // namespace cordial` well below is already inside it, so the wrapper
// declared `cordial::cordial::ui_mode_night_bits` -- a different function from
// the `cordial::ui_mode_night_bits` defined at the bottom, and one nothing
// anywhere defines. It never became a link error because `Configuration::Create`
// has no callers at all and `--gc-sections` discards the reference before the
// linker looks at it; adding a second function the same way, called from
// `PlatformParams::Create`, which *is* live, is what surfaced it.

/// The desktop's dark/light preference as `Configuration.uiMode` night bits.
jint ui_mode_night_bits();
/// Whether the display backend found a touchscreen on the seat, resolved
/// against `CORDIAL_INPUT_TOUCH` before it gets here.
bool host_has_touchscreen();

class Configuration : public Object {
public:
    // Android constants, spelled out so a reader does not have to trust a bare
    // integer. These are platform API values, not anything read out of Roblox.
    static constexpr jint kOrientationPortrait  = 1;
    static constexpr jint kOrientationLandscape = 2;
    static constexpr jint kTouchscreenNoTouch   = 1;
    static constexpr jint kTouchscreenFinger    = 3;
    static constexpr jint kKeyboardQwerty       = 2;
    static constexpr jint kKeyboardHiddenNo     = 1;
    static constexpr jint kHardKeyboardHiddenNo = 1;
    static constexpr jint kNavigationNoNav      = 1;
    static constexpr jint kNavigationHiddenNo   = 1;
    static constexpr jint kUiModeTypeNormal     = 1;
    static constexpr jint kUiModeNightNo        = 0x10;
    static constexpr jint kScreenlayoutSizeLarge = 0x03;
    static constexpr jint kScreenlayoutLongNo    = 0x10;
    static constexpr jint kColorModeWideCgNo     = 0x01;

    jfloat fontScale = 1.0f;
    jint densityDpi = 160;
    jint screenWidthDp = 1280;
    jint screenHeightDp = 720;
    jint smallestScreenWidthDp = 720;
    jint orientation = kOrientationLandscape;
    /// A desktop window is not a phone screen and not a watch. Large rather
    /// than XLarge because the window is resizable and usually well under a
    /// tablet's dimensions.
    jint screenLayout = kScreenlayoutSizeLarge | kScreenlayoutLongNo;
    /// **Was hardcoded to `kUiModeNightNo`, and Roblox believed it.**
    ///
    /// Reported as "Roblox does not read our dark mode setting; it just stays
    /// on light". It was not ignoring anything -- this told it, on every
    /// launch, that night mode is off. The engine did as it was told. Sober
    /// reports the real value, which is why the same account renders dark
    /// there after a restart and light here forever.
    ///
    /// `cordial_set_ui_mode_night` carries the desktop's actual preference in
    /// from the Rust side, which reads it from libadwaita's style manager and
    /// therefore from `org.freedesktop.appearance`'s `color-scheme` -- the
    /// same source the rest of the desktop uses, rather than a second opinion
    /// invented here. Unset leaves `kUiModeNightNo`, because a runtime started
    /// without the shell has nothing better to say and guessing dark would be
    /// as wrong as guessing light.
    jint uiMode = kUiModeTypeNormal | cordial::ui_mode_night_bits();
    jint colorMode = kColorModeWideCgNo;
    /// `TOUCHSCREEN_FINGER` when the host has a touchscreen, `NOTOUCH` when it
    /// does not.
    ///
    /// Was an unconditional `NOTOUCH` with a comment saying that claiming
    /// otherwise would ask Roblox for touch controls on a machine that cannot
    /// produce a touch event. The reasoning was right and the constant was
    /// wrong the moment Cordial could produce one: this and
    /// `PlatformParams.isTouchDevice` are two descriptions of the same fact,
    /// and a client told twice, differently, what kind of machine it is on is
    /// the inconsistency the `kSource*`/`kToolType*` pairing in
    /// `game_activity.cpp` exists to avoid.
    ///
    /// **This reaches nothing today, and saying so is the point.**
    /// `Configuration::Create` below has no callers anywhere in the tree --
    /// only `Register` does -- so no instance of this class is ever built from
    /// it. The object the engine is actually handed at `initializeNativeCode`
    /// is a *different* `cordial::Configuration`, the empty one in
    /// `native/game_activity.cpp`, which has no fields at all. So this is a
    /// hardcoded answer made honest in a builder nothing runs, not a fix; the
    /// field it keeps consistent with is `isTouchDevice`, which is measured
    /// being read and does reach the engine.
    jint touchscreen = kTouchscreenNoTouch;
    jint keyboard = kKeyboardQwerty;
    jint keyboardHidden = kKeyboardHiddenNo;
    jint hardKeyboardHidden = kHardKeyboardHiddenNo;
    jint navigation = kNavigationNoNav;
    jint navigationHidden = kNavigationHiddenNo;
    /// No SIM. 0 is `MCC_UNDEFINED`/`MNC_UNDEFINED` and is the honest answer
    /// for a desktop, not a placeholder — this is the one pair where zero is
    /// what Android itself would report.
    jint mcc = 0;
    jint mnc = 0;
    /// Android 14 and up; 0 means "no adjustment", which is correct here.
    jint fontWeightAdjustment = 0;

    static std::shared_ptr<Configuration> Create(ENV* env, int width, int height) {
        auto p = std::make_shared<Configuration>();
        p->touchscreen =
            cordial::host_has_touchscreen() ? kTouchscreenFinger : kTouchscreenNoTouch;
        // density is 1.0 at 160 dpi, so dp and px are the same number. Stated
        // rather than multiplied by 1 so the relationship survives a future
        // change to DisplayMetrics::densityDpi — if that moves, this must too.
        p->screenWidthDp = width;
        p->screenHeightDp = height;
        p->smallestScreenWidthDp = width < height ? width : height;
        p->orientation = width >= height ? kOrientationLandscape : kOrientationPortrait;
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<Configuration>("android/content/res/Configuration");
        auto c = env->GetClass("android/content/res/Configuration");
#define F(name) c->HookInstance(env, #name, &Configuration::name)
        F(fontScale); F(densityDpi); F(screenWidthDp); F(screenHeightDp);
        F(smallestScreenWidthDp); F(orientation); F(screenLayout); F(uiMode);
        F(colorMode); F(touchscreen); F(keyboard); F(keyboardHidden);
        F(hardKeyboardHidden); F(navigation); F(navigationHidden);
        F(mcc); F(mnc); F(fontWeightAdjustment);
#undef F
    }
};

/// `androidx.core.graphics.Insets`
///
/// Four fields. Zero on every one is the correct answer and, unusually here,
/// not a placeholder: Cordial's window has no status bar, no navigation bar,
/// no display cutout and no gesture areas, so there is genuinely nothing for
/// the engine to inset its layout by. Registering them matters anyway — an
/// unresolved *field* is not the same as a field that reads zero, and the
/// engine's `getWaterfallInsets`/`getWindowInsets` return one of these.
class Insets : public Object {
public:
    jint left = 0;
    jint top = 0;
    jint right = 0;
    jint bottom = 0;

    static std::shared_ptr<Insets> Create(ENV* env) {
        auto p = std::make_shared<Insets>();
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<Insets>("androidx/core/graphics/Insets");
        auto c = env->GetClass("androidx/core/graphics/Insets");
#define F(name) c->HookInstance(env, #name, &Insets::name)
        F(left); F(top); F(right); F(bottom);
#undef F
    }
};

std::shared_ptr<Object> cordial_make_zero_insets(ENV* env) { return Insets::Create(env); }

/// `androidx.core.view.WindowInsetsCompat$Type`
///
/// Nine static methods returning the bitmask AndroidX uses to name each inset
/// family. The engine calls them to build a mask it then passes to
/// `GameActivity.getWindowInsets(int)`.
///
/// **The exact bit values do not need to match AndroidX's**, and pretending
/// otherwise would be the guess. What has to hold is that they are distinct
/// and stable within one process, because the only consumer of the mask is
/// Cordial's own `getWindowInsets`, which answers zero insets for every family.
/// Distinct powers of two satisfy that. If a future change makes Cordial return
/// real insets per family, these become load-bearing and should be taken from
/// the capture rather than from here.
class WindowInsetsCompatType : public Object {
public:
    static jint statusBars(ENV*, Class*)              { return 1 << 0; }
    static jint navigationBars(ENV*, Class*)          { return 1 << 1; }
    static jint captionBar(ENV*, Class*)              { return 1 << 2; }
    static jint ime(ENV*, Class*)                     { return 1 << 3; }
    static jint systemGestures(ENV*, Class*)          { return 1 << 4; }
    static jint mandatorySystemGestures(ENV*, Class*) { return 1 << 5; }
    static jint tappableElement(ENV*, Class*)         { return 1 << 6; }
    static jint displayCutout(ENV*, Class*)           { return 1 << 7; }
    static jint systemBars(ENV*, Class*)              { return (1 << 0) | (1 << 1) | (1 << 2); }

    static void Register(ENV* env) {
        env->GetClass<WindowInsetsCompatType>("androidx/core/view/WindowInsetsCompat$Type");
        auto c = env->GetClass("androidx/core/view/WindowInsetsCompat$Type");
#define M(name) c->Hook(env, #name, &WindowInsetsCompatType::name)
        M(statusBars); M(navigationBars); M(captionBar); M(ime);
        M(systemGestures); M(mandatorySystemGestures); M(tappableElement);
        M(displayCutout); M(systemBars);
#undef M
    }
};

/// `android.view.Surface`
///
/// Typed rather than a bare Object because StartAppParams.surface is declared
/// `Landroid/view/Surface;` and libjnivm matches accessors on the descriptor it
/// derives from the C++ return type.
class AppSurface : public Object {
public:
    static std::shared_ptr<AppSurface> Create(ENV* env) {
        auto p = std::make_shared<AppSurface>();
        to_jni(env, p);
        return p;
    }
    static void Register(ENV* env) {
        env->GetClass<AppSurface>("android/view/Surface");
    }
};

/// `java.util.Map`, enough of it for the flag result's cache.
class JavaMap : public Object {
public:
    std::map<std::string, jboolean> entries;

    jint size(ENV*) { return static_cast<jint>(entries.size()); }
    jboolean isEmpty(ENV*) { return entries.empty(); }

    static std::shared_ptr<JavaMap> Create(ENV* env) {
        auto p = std::make_shared<JavaMap>();
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<JavaMap>("java/util/Map");
        auto c = env->GetClass("java/util/Map");
        c->HookInstanceFunction(env, "size", &JavaMap::size);
        c->HookInstanceFunction(env, "isEmpty", &JavaMap::isEmpty);
    }
};

/// `java.util.ArrayList`, enough of it to hand the engine an (empty) `List`.
///
/// `nativePostClientSettingsLoadedInitialization3(List)` is the only reason
/// this exists: it is the finishing step of the client-settings handshake,
/// and whatever it iterates has to be a real, well-formed object rather than
/// null or an unresolved stub.
///
/// **The list is the previous process's exit reasons, and empty is the correct
/// value rather than a placeholder.** The erased descriptor says only
/// `Ljava/util/List;`, which is why this was recorded as an open guess for two
/// sessions; the dex's generic signature says
///
///     (Ljava/util/List<Lcom/roblox/engine/jni/model/ApplicationExitInfoCpp;>;)V
///
/// — `tools/dex_signature.py` prints it. The same element type appears on
/// `nativeSetAppPreviousExitReasons`, and a traced startup shows the engine
/// doing `FindClass com/roblox/engine/jni/model/ApplicationExitInfoCpp`
/// immediately after this call, so the reading is confirmed twice over.
///
/// It is Android's `ActivityManager.getHistoricalProcessExitReasons()`: what
/// killed the app last time. Cordial has no such history to report, and an empty
/// list says exactly that — no prior abnormal exit. Inventing entries would be
/// telling the engine about crashes that did not happen, which is the kind of
/// stub that lies.
class JavaList : public Object {
public:
    jint size(ENV*) { return 0; }
    jboolean isEmpty(ENV*) { return true; }
    std::shared_ptr<Object> get(ENV*, jint) { return nullptr; }

    static std::shared_ptr<JavaList> ctor(ENV* env, Class*) {
        auto p = std::make_shared<JavaList>();
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<JavaList>("java/util/ArrayList");
        auto c = env->GetClass("java/util/ArrayList");
        c->Hook(env, "<init>", &JavaList::ctor);
        c->HookInstanceFunction(env, "size", &JavaList::size);
        c->HookInstanceFunction(env, "isEmpty", &JavaList::isEmpty);
        c->HookInstanceFunction(env, "get", &JavaList::get);

        // And again on the interface, because that is the name the engine
        // actually looks the methods up under. libjnivm resolves against the
        // exact class it is given and does not walk up to an interface the way
        // ART does, so registering only on `ArrayList` left a traced startup
        // reporting
        //
        //   Constructed Unresolved symbol, Class=`java/util/List`, Method=`get`
        //   Constructed Unresolved symbol, Class=`java/util/List`, Method=`size`
        //   Call Unknown Member Function Class=`java/util/ArrayList` Method=`size`
        //
        // — the object was fine and the *methods* were stubs, so the engine read
        // whatever a stub returns for the length of the list it had just been
        // handed. `JavaMap` above registers on `java/util/Map` and not on any
        // concrete class for the same reason; this one had it the other way
        // round and so never worked. Both names, so neither lookup can miss.
        env->GetClass<JavaList>("java/util/List");
        auto i = env->GetClass("java/util/List");
        i->HookInstanceFunction(env, "size", &JavaList::size);
        i->HookInstanceFunction(env, "isEmpty", &JavaList::isEmpty);
        i->HookInstanceFunction(env, "get", &JavaList::get);
    }
};

/// `java.util.Locale`
///
/// Reached as `configuration.getLocales().get(0)`. The engine reads all four
/// components; script and variant are legitimately empty for a plain en-US.
class JavaLocale : public Object {
public:
    std::shared_ptr<String> getLanguage(ENV*) { return S("en"); }
    std::shared_ptr<String> getCountry(ENV*) { return S("US"); }
    std::shared_ptr<String> getScript(ENV*) { return S(""); }
    std::shared_ptr<String> getVariant(ENV*) { return S(""); }
    std::shared_ptr<String> toString(ENV*) { return S("en_US"); }

    static std::shared_ptr<JavaLocale> Create(ENV* env) {
        auto p = std::make_shared<JavaLocale>();
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<JavaLocale>("java/util/Locale");
        auto c = env->GetClass("java/util/Locale");
        c->HookInstanceFunction(env, "getLanguage", &JavaLocale::getLanguage);
        c->HookInstanceFunction(env, "getCountry", &JavaLocale::getCountry);
        c->HookInstanceFunction(env, "getScript", &JavaLocale::getScript);
        c->HookInstanceFunction(env, "getVariant", &JavaLocale::getVariant);
        c->HookInstanceFunction(env, "toString", &JavaLocale::toString);
    }
};

/// `android.os.LocaleList`
///
/// `Configuration.getLocales()` returns one and the engine immediately asks it
/// for `size()`. A null there is not survivable.
class LocaleList : public Object {
public:
    jint size(ENV*) { return 1; }
    jboolean isEmpty(ENV*) { return false; }
    std::shared_ptr<JavaLocale> get(ENV* env, jint) { return JavaLocale::Create(env); }

    static std::shared_ptr<LocaleList> Create(ENV* env) {
        auto p = std::make_shared<LocaleList>();
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<LocaleList>("android/os/LocaleList");
        auto c = env->GetClass("android/os/LocaleList");
        c->HookInstanceFunction(env, "size", &LocaleList::size);
        c->HookInstanceFunction(env, "isEmpty", &LocaleList::isEmpty);
        c->HookInstanceFunction(env, "get", &LocaleList::get);
    }
};

static std::shared_ptr<LocaleList> configuration_get_locales(ENV* env, Object*) {
    return LocaleList::Create(env);
}

/// `com.roblox.client.flags.NativeFlagsInitResult`
///
/// `nativeInitializeNativeFlags` does not merely consume the flags — it *returns*
/// one of these, which it builds itself over JNI: `new NativeFlagsInitResult(id)`
/// then `addBoolean` per flag. With the class unimplemented the native could not
/// construct its result, so every flag load failed no matter what was passed in.
/// That is what `onFlagsFailed` was reporting.
///
/// **The root cause, found only by watching the live JNI trace, not by reading
/// disassembly:** libjnivm's own `GetMethodID` (`third_party/libjnivm/src/jnivm/
/// internal/method.cpp`) rewrites every *instance* lookup of `"<init>"` into a
/// **static** lookup before it ever consults the registered method table:
///
/// ```cpp
/// // Rewrite init to Static external function
/// if(!isStatic && sname == "<init>") {
///     // strips everything after ')' and appends "L<nativeprefix>;"
///     return GetMethodID<true, ...>(env, cl, str0, rewrittenSignature);
/// }
/// ```
///
/// So when the engine calls `GetMethodID(class, "<init>", "(I)V")`, libjnivm
/// actually looks for a **static** method named `"<init>"` with signature
/// `"(I)Lcom/roblox/client/flags/NativeFlagsInitResult;"` — a factory, not a
/// constructor. Registering `ctor` with `HookInstanceFunction` (an *instance*
/// hook, original `"(I)V"` signature) can never match that lookup. The engine
/// got an unresolved-symbol stub back, called it, got a null/default object, and
/// reported `onFlagsFailed` — nothing to do with the flag *contents* at all.
/// Confirmed live: before this fix, the JNI trace showed
/// `Constructed Unresolved symbol, Class=`NativeFlagsInitResult`,
/// StaticMethod=`<init>`, Signature=`(I)Lcom/.../NativeFlagsInitResult;`
/// immediately followed by `Call Unknown Static Function ... <init> ...` and
/// then `gameActivity_onFlagsFailed`.
///
/// The fix follows the same static-factory idiom already used elsewhere in this
/// file (`DeviceStaticParams::Create`, `JavaMap::Create`, etc.): register `ctor`
/// as a plain static function taking `(ENV*, Class*, jint)`, which `Class::Hook`
/// installs as a *static* method — its derived signature is exactly
/// `"(I)L<nativeprefix>;"`, matching libjnivm's rewritten lookup.
class NativeFlagsInitResult : public Object {
public:
    jint providerId = 0;
    std::shared_ptr<JavaMap> cached;

    std::shared_ptr<JavaMap>& map(ENV* env) {
        if (!cached) {
            cached = JavaMap::Create(env);
        }
        return cached;
    }

    static std::shared_ptr<NativeFlagsInitResult> ctor(ENV* env, Class*, jint id) {
        auto p = std::make_shared<NativeFlagsInitResult>();
        p->providerId = id;
        p->map(env);
        to_jni(env, p);
        return p;
    }
    void addBoolean(ENV* env, std::shared_ptr<String> name, jboolean value, jboolean) {
        if (name) {
            map(env)->entries[*name] = value;
        }
    }
    jint getNativeFlagProviderId(ENV*) { return providerId; }
    std::shared_ptr<JavaMap> getBooleanCachedMap(ENV* env) { return map(env); }
    jboolean resolveFlagValue(ENV* env, std::shared_ptr<String> name) {
        if (!name) {
            return false;
        }
        auto& m = map(env)->entries;
        auto it = m.find(*name);
        return it != m.end() ? it->second : false;
    }

    static void Register(ENV* env) {
        env->GetClass<NativeFlagsInitResult>("com/roblox/client/flags/NativeFlagsInitResult");
        auto c = env->GetClass("com/roblox/client/flags/NativeFlagsInitResult");
        c->Hook(env, "<init>", &NativeFlagsInitResult::ctor);
        c->HookInstanceFunction(env, "addBoolean", &NativeFlagsInitResult::addBoolean);
        // The other three the dex declares. They were written and then not
        // registered, so the class answered `<init>` and `addBoolean` and handed
        // back an unresolved stub for every question about what it had stored --
        // including `resolveFlagValue`, which is the engine asking for a flag it
        // just cached and getting whatever a stub returns instead of the value.
        //
        //     com/roblox/client/flags/NativeFlagsInitResult getNativeFlagProviderId ()I
        //     com/roblox/client/flags/NativeFlagsInitResult getBooleanCachedMap    ()Ljava/util/Map;
        //     com/roblox/client/flags/NativeFlagsInitResult resolveFlagValue       (Ljava/lang/String;)Z
        //
        // An implemented method that is never registered is the same silent
        // failure as a mismatched descriptor and does not show up in a grep for
        // the method name, which is why this survived a hook audit that was
        // looking for exactly this.
        c->HookInstanceFunction(env, "getNativeFlagProviderId",
                                &NativeFlagsInitResult::getNativeFlagProviderId);
        c->HookInstanceFunction(env, "getBooleanCachedMap",
                                &NativeFlagsInitResult::getBooleanCachedMap);
        c->HookInstanceFunction(env, "resolveFlagValue",
                                &NativeFlagsInitResult::resolveFlagValue);
        c->HookInstanceFunction(env, "getNativeFlagProviderId",
                                &NativeFlagsInitResult::getNativeFlagProviderId);
        c->HookInstanceFunction(env, "getBooleanCachedMap",
                                &NativeFlagsInitResult::getBooleanCachedMap);
        c->HookInstanceFunction(env, "resolveFlagValue",
                                &NativeFlagsInitResult::resolveFlagValue);
    }
};

/// `org.json.JSONObject`
///
/// Just enough of it for `ClientLocalFlags.getAll()` to return something real
/// instead of the unresolved-symbol default (null), which is not safe to hand
/// back to engine code that might call methods on it.
class JSONObject : public Object {
public:
    std::shared_ptr<JavaMap> cached;
    std::shared_ptr<JavaMap>& map(ENV* env) {
        if (!cached) {
            cached = JavaMap::Create(env);
        }
        return cached;
    }

    static std::shared_ptr<JSONObject> ctor(ENV* env, Class*) {
        auto p = std::make_shared<JSONObject>();
        p->map(env);
        to_jni(env, p);
        return p;
    }
    jint length(ENV* env) { return static_cast<jint>(map(env)->entries.size()); }

    static void Register(ENV* env) {
        env->GetClass<JSONObject>("org/json/JSONObject");
        auto c = env->GetClass("org/json/JSONObject");
        c->Hook(env, "<init>", &JSONObject::ctor);
        c->HookInstanceFunction(env, "length", &JSONObject::length);
    }
};

/// `com.roblox.engine.jni.model.ClientLocalFlags`
///
/// The offline counterpart to the network `ClientSettings` fetch:
/// `NativeGLInterface.readLocalFlags()` — implemented in the engine, exported
/// as a plain native taking no arguments — reads whatever bundled/cached flag
/// defaults the engine ships and hands them back wrapped in one of these,
/// built the same way `NativeFlagsInitResult` is: `new ClientLocalFlags()`
/// then repeated `add(name, value)`.
///
/// This class was entirely unimplemented, so any attempt at calling
/// `readLocalFlags` from Cordial would fault or silently do nothing useful —
/// nothing in the shipping dex ever called it either (the real app's only
/// caller is a different, non-`ActivityNativeMain` startup path Cordial does
/// not replicate), so this was dead on arrival either way.
///
/// The `<init>` registration uses the same static-factory idiom
/// `NativeFlagsInitResult` needed above — libjnivm rewrites every *instance*
/// `<init>` lookup into a *static* one with the return type folded into the
/// signature, so an instance-hooked constructor can never be found.
class ClientLocalFlags : public Object {
public:
    std::map<std::string, std::string> entries;

    static std::shared_ptr<ClientLocalFlags> ctor(ENV* env, Class*) {
        auto p = std::make_shared<ClientLocalFlags>();
        to_jni(env, p);
        return p;
    }
    void add(ENV*, std::shared_ptr<String> name, std::shared_ptr<String> value) {
        if (name) {
            entries[*name] = value ? *value : std::string();
        }
    }
    jboolean isEmpty(ENV*) { return entries.empty(); }
    jint size(ENV*) { return static_cast<jint>(entries.size()); }
    std::shared_ptr<JSONObject> getAll(ENV* env) {
        auto p = std::make_shared<JSONObject>();
        auto& m = p->map(env)->entries;
        for (auto& kv : entries) {
            // JavaMap's cache stores jboolean; ClientLocalFlags' values are
            // strings, so only presence/absence survives this bridge. Nothing
            // downstream in this build reads getAll()'s contents (see the
            // bridge function below), so this exists to make the call safe,
            // not to carry real values through it.
            m[kv.first] = true;
        }
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<ClientLocalFlags>("com/roblox/engine/jni/model/ClientLocalFlags");
        auto c = env->GetClass("com/roblox/engine/jni/model/ClientLocalFlags");
        c->Hook(env, "<init>", &ClientLocalFlags::ctor);
        c->HookInstanceFunction(env, "add", &ClientLocalFlags::add);
        c->HookInstanceFunction(env, "isEmpty", &ClientLocalFlags::isEmpty);
        c->HookInstanceFunction(env, "size", &ClientLocalFlags::size);
        c->HookInstanceFunction(env, "getAll", &ClientLocalFlags::getAll);
    }
};

/// `com.roblox.client.startup.NativeHelper`
///
/// The engine's own status channel back into the app. `onFlagsFailed` is the one
/// Cordial has always gotten, on every launch — this is now investigated to a
/// conclusion (docs/analysis/flag-init.md, commits e553d85 and bee6c14), not an
/// open question. Confirmed live, with a debugger: it is written by
/// `RBX::NativeDataModelManager::getFlagsFromEngine()`'s completion lambda, on a
/// background thread whose outcome did not move across every combination this
/// session's own bring-up could vary — the real 139-name flag list resolving
/// correctly, the real 1.2 MB ClientSettings document being accepted
/// (`nativeInitClientSettings` returns 0), ordering relative to
/// `initializeNativeCode`, none of it. It has also been confirmed, separately,
/// not to gate rendering (render-gate.md). So the message below is misleading
/// in a way that has already cost two people real investigation time: it reads
/// as "your flag data failed to load", and that is not what happened — the
/// flags did load. Whatever this verdict is actually testing remains unknown;
/// only that it is not "do I have flags".
///
/// **And it does not block the content store either — that claim is withdrawn
/// as of 2026-08-20.** The message used to say it did, which was the best guess
/// available while `RbxStorage` had never once initialised. It has now
/// initialised: `onFlagsFailed` fires twice in the same runs that produce a
/// 45,056-byte `rbx-storage.db` with the engine's own `files` table and eight
/// engine-created partitions, measured three times over. Both facts in one run
/// is as direct a refutation as this question can get.
///
/// The cause of the store never appearing was `nativeSetCacheDirectory` being
/// called after `GameActivity.initializeNativeCode` instead of before it
/// (`CORDIAL_EARLY_DIRS`, flag-init.md §46) and had nothing to do with this
/// verdict. Naming a second symptom as the consequence of a first, when both
/// were merely present together, is how this became load-bearing for two
/// investigations that went nowhere.
///
/// **"Blocks neither startup" is wrong for a real subset of machines, found
/// 2026-08-30 via GitHub issue #21 plus a second, independent Flatpak report.**
/// Both hit `RBXCRASH: FatalRuntimeError (Can't initialize the TaskScheduler
/// before flags have been loaded)`, SIGTRAP, on first launch, on CachyOS. The
/// unresolved-flags state this hook is reporting is not itself what crashes —
/// `onFlagsFailed` returning is a no-op — but it is exactly the state that a
/// *separate* engine assertion checks moments later, on the "Main" thread the
/// engine spawns for itself inside `nativeGameGlobalInit`
/// (`crates/cordial-runtime/src/bin/load.rs`, `call_globals`), which races
/// Cordial's own synchronous `nativeAppBridgeStartLuaAppDM` /
/// `nativeAppBridgeV2StartAppWithParams` calls through the same StartLuaAppDM
/// machinery. Both reporters' logs die between "app bridge initialised" and
/// the surface handoff — before `CORDIAL_LATE_POST_MS`'s late
/// `nativePostClientSettingsLoadedInitialization3` retry (the thing that
/// actually produces `areFlagsLoaded:true`, flag-init.md §23) ever gets to run.
/// Reproduced on this machine too, with the same message and signal, using the
/// existing `CORDIAL_LATE_SETTINGS=1` knob to bias that same race — confirmed
/// live under gdb, crash on the process's original main thread inside
/// libroblox.so, no symbols. What could not be reproduced here is the race
/// losing on the *default*, no-env-var path: nine attempts on this host
/// (Fedora 44), including under `stress-ng --cpu 4`, all launched cleanly.
/// Why CachyOS loses this race and this machine does not is **INFERRED, not
/// established** — a scheduler or thread-creation-latency difference is the
/// leading candidate, untested.
class NativeHelper : public Object {
public:
    static void onFlagsFailed(ENV*, Object*) {
        fprintf(stderr,
            "[roblox] flags: engine reported onFlagsFailed (the flag data did "
            "load, and this blocks neither startup nor the content store — "
            "what the verdict actually tests is still unknown; see "
            "docs/analysis/flag-init.md)\n");
    }
    /// The buffer is the flag cache the engine hands back for the app to keep.
    /// On Android that is where `flag_cache.dat` comes from — Sober's log writes
    /// it a second after this fires, 362 KB compressed.
    ///
    /// The parameter type is the whole point of this declaration. libjnivm
    /// derives the hook's descriptor from these C++ types, and the engine looks
    /// the method up as `(Ljava/nio/ByteBuffer;)V`. This took
    /// `std::shared_ptr<Object>` until now, which derives `(Ljava/lang/Object;)V`
    /// — so the hook registered, the symbol resolved, and it could never once be
    /// called. `tools/hook_descriptors.py` is what found it, and this was one of
    /// only two such hooks left in the tree.
    static void onFlagsLoaded(ENV*, Object*, std::shared_ptr<jnivm::ByteBuffer> buf) {
        fprintf(stderr, "[roblox] flags loaded (%lld bytes)\n",
                buf ? static_cast<long long>(buf->capacity) : -1LL);
    }
    static void onAppReady(ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] app ready: %s\n", s ? s->c_str() : "");
    }

    // The experience lifecycle, which the engine has been announcing to nobody.
    //
    // These four are the rest of the channel above, and until now every one of
    // them was an unresolved symbol: the engine looked them up at `JNI_OnLoad`,
    // libjnivm handed back a placeholder, and each announcement went into it.
    // On Android the app is listening — Sober's bridge answers the game-loaded
    // one and prints `{"place_id":…,"type":"game_loaded"}`.
    //
    // They are being answered now because of what sits beside them in the log.
    // `SessionTransitionFSM` reaches `Entered play session` in both clients and
    // then diverges: Sober logs `Sent play session success` and Cordial logs
    // nothing, and roughly sixty seconds later the server disconnects with 304.
    // Whether these callbacks are what unblocks that report is **not
    // established** — they are `void` notifications, so it is equally possible
    // the engine tells the app and carries on regardless. What is not in doubt
    // is that the engine is speaking and Cordial was not listening, which is
    // the `broken_feature` shape and worth closing on its own.
    //
    // Answering a `void` notification by receiving it is not a stub that lies.
    // Nothing here reports success at a question it cannot answer; there is no
    // question, only an announcement, and the honest response to an
    // announcement is to have received it.
    static void onExperienceStart(ENV*, Object*) {
        fprintf(stderr, "[roblox] experience start\n");
    }
    static void onGameLoaded(ENV*, Object*, jlong place_id) {
        fprintf(stderr, "[roblox] game loaded: place %lld\n",
                static_cast<long long>(place_id));
        // The same counter `gameLoadedCallback` bumps, for the join watchdog.
        // Both callbacks mean the join completed and different builds have been
        // seen to call different ones, so the watchdog waits on either.
        cordial_note_game_loaded(static_cast<long long>(place_id));
    }
    /// The argument is a session identifier, not a credential — but this
    /// boundary is next to the one that carries `.ROBLOSECURITY`, so it is
    /// counted rather than printed. A log line is a file somebody pastes into
    /// an issue.
    static void onDidLogInReceived(ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] logged in (%zu bytes, not shown)\n",
                s ? s->length() : 0u);
    }
    static void onScreenOrientationChanged(ENV*, Object*, jint orientation,
                                           jboolean locked) {
        fprintf(stderr, "[roblox] orientation %d (locked: %s)\n",
                static_cast<int>(orientation), locked ? "yes" : "no");
    }

    /// The rest of the channel, which was going into unresolved stubs.
    ///
    /// The dex declares 23 `gameActivity_*` callbacks on this class and Cordial
    /// answered seven. The other sixteen were not "unused": libjnivm hands the
    /// engine a placeholder for a name it has no hook for, and §3 records that
    /// the engine's template for these is "call the no-arg NativeHelper callback,
    /// log `FATAL: Java exception occurred in JNI call` if it throws". An
    /// announcement into a placeholder is the failure mode this project has now
    /// found four times, so the remaining ones are answered rather than left to
    /// be discovered a fifth time.
    ///
    /// `onEngineInitialized` is the one that prompted this. §3 found it sharing
    /// a call template with `onFlagsFailed` -- the two reporters sit seven bytes
    /// apart and are instantiated from the same generic helper -- so it is on the
    /// same startup path as the verdict this file exists to explain, and it was
    /// unregistered. **Whether answering it changes the verdict is not
    /// established**; it is answered because an unanswered callback next to the
    /// one we are investigating is a variable nobody should be leaving in.
    static void onEngineInitialized(ENV*, Object*) {
        fprintf(stderr, "[roblox] engine initialised\n");
    }
    static void onDidLogOutReceived(ENV*, Object*) {
        fprintf(stderr, "[roblox] logged out\n");
    }
    static void onDidSwitchAccountReceived(ENV*, Object*) {
        fprintf(stderr, "[roblox] account switched\n");
    }
    static void onLuaAppDidReturn(ENV*, Object*) {
        fprintf(stderr, "[roblox] lua app returned\n");
    }
    static void onRestartLuaApp(ENV*, Object*) {
        fprintf(stderr, "[roblox] lua app restart requested\n");
    }
    static void onScanQrCode(ENV*, Object*) {
        fprintf(stderr, "[roblox] QR scan requested (Cordial has no camera yet)\n");
    }
    /// Counted, not printed, for the same reason as `onDidLogInReceived`: this
    /// carries a sign-up identifier and log files end up in issues.
    static void onDidSignUp(ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] signed up (%zu bytes, not shown)\n",
                s ? s->length() : 0u);
    }
    static void onGameStreamingStatusChanged(ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] game streaming status: %s\n", s ? s->c_str() : "");
    }
    static void onScreenshotReady(ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] screenshot ready: %s\n", s ? s->c_str() : "");
    }
    static void onMotionEventListening(ENV*, Object*, std::shared_ptr<String> s) {
        fprintf(stderr, "[roblox] motion event listening: %s\n", s ? s->c_str() : "");
    }
    static void onExperienceStop(ENV*, Object*, jdouble seconds) {
        fprintf(stderr, "[roblox] experience stop (%.3f s)\n",
                static_cast<double>(seconds));
    }
    static void setAppUpgradeStatus(ENV*, Object*, jint status, jint flags,
                                    std::shared_ptr<String> a, std::shared_ptr<String> b) {
        fprintf(stderr, "[roblox] app upgrade status %d/%d %s %s\n",
                static_cast<int>(status), static_cast<int>(flags),
                a ? a->c_str() : "", b ? b->c_str() : "");
    }

    static void Register(ENV* env) {
        env->GetClass<NativeHelper>("com/roblox/client/startup/NativeHelper");
        auto c = env->GetClass("com/roblox/client/startup/NativeHelper");
        c->HookInstanceFunction(env, "gameActivity_onFlagsFailed", &NativeHelper::onFlagsFailed);
        c->HookInstanceFunction(env, "gameActivity_onFlagsLoaded", &NativeHelper::onFlagsLoaded);
        c->HookInstanceFunction(env, "gameActivity_onAppReady", &NativeHelper::onAppReady);
        c->HookInstanceFunction(env, "gameActivity_onExperienceStart", &NativeHelper::onExperienceStart);
        c->HookInstanceFunction(env, "gameActivity_onGameLoaded", &NativeHelper::onGameLoaded);
        c->HookInstanceFunction(env, "gameActivity_onDidLogInReceived", &NativeHelper::onDidLogInReceived);
        c->HookInstanceFunction(env, "gameActivity_onScreenOrientationChanged",
                                &NativeHelper::onScreenOrientationChanged);
        c->HookInstanceFunction(env, "gameActivity_onEngineInitialized",
                                &NativeHelper::onEngineInitialized);
        c->HookInstanceFunction(env, "gameActivity_onDidLogOutReceived",
                                &NativeHelper::onDidLogOutReceived);
        c->HookInstanceFunction(env, "gameActivity_onDidSwitchAccountReceived",
                                &NativeHelper::onDidSwitchAccountReceived);
        c->HookInstanceFunction(env, "gameActivity_onLuaAppDidReturn",
                                &NativeHelper::onLuaAppDidReturn);
        c->HookInstanceFunction(env, "gameActivity_onRestartLuaApp",
                                &NativeHelper::onRestartLuaApp);
        c->HookInstanceFunction(env, "gameActivity_onScanQrCode",
                                &NativeHelper::onScanQrCode);
        c->HookInstanceFunction(env, "gameActivity_onDidSignUp",
                                &NativeHelper::onDidSignUp);
        c->HookInstanceFunction(env, "gameActivity_onGameStreamingStatusChanged",
                                &NativeHelper::onGameStreamingStatusChanged);
        c->HookInstanceFunction(env, "gameActivity_onScreenshotReady",
                                &NativeHelper::onScreenshotReady);
        c->HookInstanceFunction(env, "gameActivity_onMotionEventListening",
                                &NativeHelper::onMotionEventListening);
        c->HookInstanceFunction(env, "gameActivity_onExperienceStop",
                                &NativeHelper::onExperienceStop);
        c->HookInstanceFunction(env, "gameActivity_setAppUpgradeStatus",
                                &NativeHelper::setAppUpgradeStatus);
    }
};

/// `com.roblox.client.LocalStorageManager`
///
/// `getAllocatableBytes` is how the engine asks how much room it has before it
/// builds its content store, and it was unresolved: libjnivm handed back a
/// placeholder and the engine read the answer as zero.
///
/// That matters because RbxStorage gates itself on the answer. The live flag set
/// carries `DFFlagRbxStorageAvailableSpaceError`,
/// `DFFlagRbxStorageAvailableSpaceCreatePath` and `DFFlagRbxStorageFixEmptyPath`,
/// and Cordial's engine reports `RbxStorage is not initialized, cannot access
/// storage interface` on every run while Sober's `appData` carries a 167 MB
/// `rbx-storage.db`. A client that believes it has no disk has no reason to
/// build a cache.
///
/// **INFERRED that this is what gates it.** The mechanism fits and the gap is
/// real, but the run after this is what says whether storage comes up.
///
/// `statvfs` on the working directory rather than on `files_dir()`, which
/// hardcodes `instances/default` and does not follow `--profile` -- the same
/// trap `SharedPreferences` hit. The client runs with its working directory set
/// to the profile, so this measures the filesystem the store would actually
/// live on.
class LocalStorageManager : public Object {
public:
    static jlong getAllocatableBytes(ENV*, Object*) {
        struct statvfs vfs {};
        if (statvfs(".", &vfs) != 0) {
            // Reporting zero would be the lie that is suspected of causing this
            // in the first place, so say nothing is known by failing loudly in
            // the log rather than quietly claiming a full disk.
            fprintf(stderr, "[roblox] getAllocatableBytes: statvfs failed\n");
            return 0;
        }
        auto bytes = static_cast<jlong>(vfs.f_bavail) * static_cast<jlong>(vfs.f_frsize);
        return bytes;
    }

    static void Register(ENV* env) {
        env->GetClass<LocalStorageManager>("com/roblox/client/LocalStorageManager");
        auto c = env->GetClass("com/roblox/client/LocalStorageManager");
        // Instance, per the dex's own ACC_STATIC bit. An earlier commit moved
        // this to `Hook` after reading a JNI-trace line as evidence of a static
        // call -- `Call Member Function Class=java/lang/Class` -- but that logs
        // the *receiver object's* class, not how the method is bound, and the
        // same trace shows `Found symbol` for the instance hook right above it.
        // The hook was binding all along; the reading was wrong.
        c->HookInstanceFunction(env, "getAllocatableBytes",
                                &LocalStorageManager::getAllocatableBytes);
    }
};

/// `android.content.res.Resources`
///
/// Android's path to the screen is `activity.getResources().getDisplayMetrics()`,
/// not `activity.getDisplayMetrics()`. Hooking the Activity alone left
/// getResources returning null and the engine calling getDisplayMetrics on it.
class Resources : public Object {
public:
    std::shared_ptr<DisplayMetrics> getDisplayMetrics(ENV* env) {
        return DisplayMetrics::Create(env, g_width, g_height);
    }

    static std::shared_ptr<Resources> Create(ENV* env) {
        auto p = std::make_shared<Resources>();
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<Resources>("android/content/res/Resources");
        auto c = env->GetClass("android/content/res/Resources");
        c->HookInstanceFunction(env, "getDisplayMetrics", &Resources::getDisplayMetrics);
    }
};

std::shared_ptr<Object> make_resources(ENV* env) { return Resources::Create(env); }

/// `android.app.Activity`
///
/// Both parameter objects carry one, typed `Landroid/app/Activity;`, and it is
/// the only Activity the engine gets from the app-bridge path. Left null it
/// asks the null for its display metrics and stops.
class AndroidActivity : public Object {
public:
    std::shared_ptr<DisplayMetrics> getDisplayMetrics(ENV* env) {
        return DisplayMetrics::Create(env, g_width, g_height);
    }
    std::shared_ptr<Resources> getResources(ENV* env) { return Resources::Create(env); }

    static std::shared_ptr<AndroidActivity> Create(ENV* env) {
        auto p = std::make_shared<AndroidActivity>();
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<AndroidActivity>("android/app/Activity");
        auto c = env->GetClass("android/app/Activity");
        c->HookInstanceFunction(env, "getDisplayMetrics", &AndroidActivity::getDisplayMetrics);
        c->HookInstanceFunction(env, "getResources", &AndroidActivity::getResources);
    }
};

/// `CORDIAL_TRACE_PARAM_READS=1`: name each `PlatformParams` and `DeviceParams`
/// field the engine actually reads off Cordial's own object.
///
/// This exists to settle a premise the whole "Roblox thinks you're mobile" line
/// of work rests on. `isKeyboardDevice`, `isMouseDevice` and `isTouchDevice`
/// have been set to the desktop answer for some time and the client still
/// behaves as a mobile one, which has two completely different explanations:
/// the engine reads them and ignores them, or the engine never reads them at
/// all. `docs/analysis/unresolved-java.md` records a live observation of the
/// second — `platformParams`, `dpiScale` and `isTouchDevice` being reached
/// through `GetObjectClass(null)`, so against class `Invalid` rather than
/// against the object Cordial handed over — and nothing has re-checked it since
/// the bring-up moved to `nativeAppBridgeV2InitWithParams`.
///
/// `DeviceParams` is the control, and it is a real one rather than a hopeful
/// one: `deviceName` is known to arrive, because the engine echoes it into
/// `[FLog::Graphics] Vulkan Android Device: Cordial` on every run. So a trace
/// where the `DeviceParams` getters fire and the `PlatformParams` getters do
/// not is not a broken probe — it is the answer.
///
/// Off by default and registered as *getter functions* only when on, so the
/// ordinary client keeps the plain field hooks it has always had and the probe
/// cannot change what it is measuring.
static bool trace_param_reads() {
    static const bool on = getenv("CORDIAL_TRACE_PARAM_READS") != nullptr;
    return on;
}

static void note_param_read(const char* klass, const char* field) {
    fprintf(stderr, "[cordial] param read: %s.%s\n", klass, field);
}

/// `com.roblox.engine.jni.model.DeviceParams`
class DeviceParams : public Object {
public:
    std::shared_ptr<String> appBuildVariant, appVersion, country, deviceName, deviceSku;
    std::shared_ptr<String> displayResolution, manufacturer, networkType, osVersion;
    std::shared_ptr<String> socModel, testDeviceName;
    jboolean cpu64Bit = true;
    jboolean isChrome = false;
    jboolean isLowRamDevice = false;
    jint deviceTotalMemoryMB = 8192;
    jint displayPhysicalWidthPixels = 1280;
    jint displayPhysicalHeightPixels = 720;
    jint largeMemoryClass = 512;
    jint memoryClass = 256;
    jlong lowMemoryKillerBackgroundAppThreshold = 0;
    jlong lowMemoryKillerForegroundAppThreshold = 0;

    static std::shared_ptr<DeviceParams> Create(ENV* env, int width, int height) {
        auto p = std::make_shared<DeviceParams>();
        p->appBuildVariant = S("release");
        p->appVersion = S("");
        p->country = S("US");
        p->deviceName = S("Cordial");
        p->deviceSku = S("cordial");
        p->manufacturer = S("Cordial");
        p->socModel = S("cordial");
        // The API *level*, not the release name. The engine echoes this field
        // straight into `[FLog::Graphics] Android API <n>` and gates on it: at
        // 15 it refused Vulkan with "Android version is too old". 33 is what the
        // Waydroid capture reports the real client running at (`Lvl = 33`).
        //
        // Established by experiment, not inference: setting
        // `ro.build.version.sdk` and implementing
        // `android_get_device_api_level()` both left the log saying 15, and only
        // this field moved it.
        //
        // Left at "33" whatever `device_identity()` says -- unlike the
        // User-Agent and `InitParams.isTablet`, this field is a gate the
        // engine has been measured refusing Vulkan over, not just a
        // description, and there is no captured value for what a PC-presenting
        // client should send here. Changing a load-bearing compatibility
        // check on a guess is a worse experiment than leaving one field
        // pointing at Android while the presentational ones point at PC; this
        // whole switch does not claim to have closed every seam, only the two
        // that visibly contradicted each other.
        p->osVersion = S("33");
        p->testDeviceName = S("");
        // Reported as "not on a metered mobile connection". The engine uses this
        // to decide how aggressively to stream assets.
        p->networkType = S("WIFI");
        char res[64];
        snprintf(res, sizeof(res), "%dx%d", width, height);
        p->displayResolution = S(res);
        p->displayPhysicalWidthPixels = width;
        p->displayPhysicalHeightPixels = height;
        // Prime the class. Without this the object reaches Roblox with a null
        // clazz, GetObjectClass falls back to FindClass("Invalid"), and every
        // field read against it returns nothing.
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<DeviceParams>("com/roblox/engine/jni/model/DeviceParams");
        auto c = env->GetClass("com/roblox/engine/jni/model/DeviceParams");
        if (trace_param_reads()) {
            // The control. `deviceName` is the one parameter field with a
            // standing, independent proof that it reaches the engine — it comes
            // back out in `[FLog::Graphics] Vulkan Android Device: Cordial` —
            // so if this getter does not fire either, the probe is broken and
            // the run says nothing about PlatformParams.
            c->HookInstanceGetterFunction(env, "deviceName",
                                          [](Object* o) -> std::shared_ptr<String> {
                note_param_read("DeviceParams", "deviceName");
                return o ? static_cast<DeviceParams*>(o)->deviceName : nullptr;
            });
            c->HookInstanceGetterFunction(env, "osVersion",
                                          [](Object* o) -> std::shared_ptr<String> {
                note_param_read("DeviceParams", "osVersion");
                return o ? static_cast<DeviceParams*>(o)->osVersion : nullptr;
            });
#define F(name) c->HookInstance(env, #name, &DeviceParams::name)
            F(appBuildVariant); F(appVersion); F(country); F(deviceSku);
            F(displayResolution); F(manufacturer); F(networkType);
            F(socModel); F(testDeviceName); F(cpu64Bit); F(isChrome); F(isLowRamDevice);
            F(deviceTotalMemoryMB); F(displayPhysicalWidthPixels); F(displayPhysicalHeightPixels);
            F(largeMemoryClass); F(memoryClass);
            F(lowMemoryKillerBackgroundAppThreshold); F(lowMemoryKillerForegroundAppThreshold);
#undef F
            return;
        }
#define F(name) c->HookInstance(env, #name, &DeviceParams::name)
        F(appBuildVariant); F(appVersion); F(country); F(deviceName); F(deviceSku);
        F(displayResolution); F(manufacturer); F(networkType); F(osVersion);
        F(socModel); F(testDeviceName); F(cpu64Bit); F(isChrome); F(isLowRamDevice);
        F(deviceTotalMemoryMB); F(displayPhysicalWidthPixels); F(displayPhysicalHeightPixels);
        F(largeMemoryClass); F(memoryClass);
        F(lowMemoryKillerBackgroundAppThreshold); F(lowMemoryKillerForegroundAppThreshold);
#undef F
    }
};

/// `com.roblox.engine.jni.model.PlatformParams`
class PlatformParams : public Object {
public:
    std::shared_ptr<String> assetFolderPath;
    jfloat dpiScale = 1.0f;
    jboolean isKeyboardDevice = true;
    jboolean isMouseDevice = true;
    jboolean isTouchDevice = false;
    jint viewportWidthMm = 338;
    jint viewportHeightMm = 190;

    static std::shared_ptr<PlatformParams> Create(ENV* env, const char* assets, int width, int height) {
        auto p = std::make_shared<PlatformParams>();
        p->assetFolderPath = S(assets);
        // Only one third of this is load-bearing. `isTouchDevice` is read —
        // twice per cold start, measured — so it is the one the engine
        // genuinely learns from. `isKeyboardDevice` and `isMouseDevice` are
        // **never read at all**, so they are documentation rather than
        // configuration: true is what is honest about a host with a keyboard
        // and a mouse on its seat, and the engine has never asked. Do not reach
        // for these two when input misbehaves; they cannot be the cause.
        p->isKeyboardDevice = true;
        p->isMouseDevice = true;
        // Was a hardcoded `false`, which was true of every machine this has
        // been developed on and false of the ones the client is for. It now
        // reports what the display backend found on the seat, with
        // `CORDIAL_INPUT_TOUCH` overriding it either way — see
        // `android::input::report_touchscreen`, which resolves both into the
        // single answer stored here.
        p->isTouchDevice = cordial::host_has_touchscreen() ? JNI_TRUE : JNI_FALSE;
        // Roblox lays its UI out in dp and picks image-asset resolutions from
        // this. At 1.0 it builds the interface for a low-density phone, which
        // is why the app shell looks coarse on a desktop panel. Overridable
        // because the right value depends on the display, and nothing here can
        // measure the display's physical size reliably.
        {
            const char* v = getenv("CORDIAL_DPI_SCALE");
            float scale = 1.0f;
            if (v && *v) {
                float parsed = strtof(v, nullptr);
                if (parsed > 0.0f && parsed <= 8.0f) {
                    scale = parsed;
                } else {
                    fprintf(stderr,
                            "[android] CORDIAL_DPI_SCALE=%s is not a scale between 0 and 8;"
                            " using 1.0\n", v);
                }
            }
            p->dpiScale = scale;
        }
        // Physical size at roughly 96 DPI, which is what a desktop display is.
        // A phone's 400+ DPI here would make the engine scale its UI for a
        // screen held at arm's length.
        p->viewportWidthMm = static_cast<jint>(width * 25.4 / 96.0);
        p->viewportHeightMm = static_cast<jint>(height * 25.4 / 96.0);
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<PlatformParams>("com/roblox/engine/jni/model/PlatformParams");
        auto c = env->GetClass("com/roblox/engine/jni/model/PlatformParams");
        if (trace_param_reads()) {
            // Same values, read through a function so the read itself is
            // observable. The three peripheral booleans are the ones the
            // question is about; dpiScale rides along because it is the one
            // PlatformParams field with an independent report of having reached
            // the engine (docs/NEXT.md's density work), so it doubles as a
            // second control inside this same object.
            c->HookInstanceGetterFunction(env, "isKeyboardDevice", [](Object* o) -> jboolean {
                note_param_read("PlatformParams", "isKeyboardDevice");
                return o ? static_cast<PlatformParams*>(o)->isKeyboardDevice : JNI_FALSE;
            });
            c->HookInstanceGetterFunction(env, "isMouseDevice", [](Object* o) -> jboolean {
                note_param_read("PlatformParams", "isMouseDevice");
                return o ? static_cast<PlatformParams*>(o)->isMouseDevice : JNI_FALSE;
            });
            c->HookInstanceGetterFunction(env, "isTouchDevice", [](Object* o) -> jboolean {
                note_param_read("PlatformParams", "isTouchDevice");
                return o ? static_cast<PlatformParams*>(o)->isTouchDevice : JNI_FALSE;
            });
            c->HookInstanceGetterFunction(env, "dpiScale", [](Object* o) -> jfloat {
                note_param_read("PlatformParams", "dpiScale");
                return o ? static_cast<PlatformParams*>(o)->dpiScale : 1.0f;
            });
#define F(name) c->HookInstance(env, #name, &PlatformParams::name)
            F(assetFolderPath); F(viewportWidthMm); F(viewportHeightMm);
#undef F
            return;
        }
#define F(name) c->HookInstance(env, #name, &PlatformParams::name)
        F(assetFolderPath); F(dpiScale); F(isKeyboardDevice); F(isMouseDevice);
        F(isTouchDevice); F(viewportWidthMm); F(viewportHeightMm);
#undef F
    }
};

/// `com.roblox.engine.jni.autovalue.InitParams`
class InitParams : public Object {
public:
    std::shared_ptr<String> baseURL, buildVariant, userAgent;
    std::shared_ptr<DeviceParams> deviceParams;
    std::shared_ptr<PlatformParams> platformParams;
    std::shared_ptr<AndroidActivity> vrContext;
    jboolean isPotato = false;
    jboolean isTablet = false;
    jboolean isVrDevice = false;


    // AutoValue generates accessor methods, so the engine calls
    // `initParams.platformParams()` rather than reading a field. These are the
    // methods; the field hooks above stay for anything that does read directly.
    std::shared_ptr<DeviceParams> get_deviceParams(ENV*) { return deviceParams; }
    std::shared_ptr<PlatformParams> get_platformParams(ENV*) { return platformParams; }
    std::shared_ptr<String> get_baseURL(ENV*) { return baseURL; }
    std::shared_ptr<String> get_buildVariant(ENV*) { return buildVariant; }
    std::shared_ptr<String> get_userAgent(ENV*) { return userAgent; }
    jboolean get_isPotato(ENV*) { return isPotato; }
    jboolean get_isTablet(ENV*) { return isTablet; }
    jboolean get_isVrDevice(ENV*) { return isVrDevice; }
    std::shared_ptr<AndroidActivity> get_vrContext(ENV*) { return vrContext; }

    static std::shared_ptr<InitParams> Create(ENV* env, const char* assets, int width, int height) {
        auto p = std::make_shared<InitParams>();
        p->baseURL = S("https://www.roblox.com");
        p->buildVariant = S("release");
        // See `build_user_agent`. The literal that used to be here was
        // invented, and the comment beside it claimed the opposite.
        //
        // Printed unconditionally, same reasoning `graphics.rs`'s `report()`
        // gives for doing the same thing there: this is the one line that
        // makes the switch's own doc comment's claim checkable against a run
        // rather than only against the source, and the User-Agent itself
        // never appears in Cordial's own logs or the engine's FLog output --
        // it goes out on the wire, not into anything grep can reach here.
        std::string ua = build_user_agent();
        fprintf(stderr, "[cordial] device identity: %s (isTablet=%s, User-Agent: %s)\n",
                device_identity_label(),
                device_identity() == DeviceIdentity::AndroidTablet ? "true" : "false",
                ua.c_str());
        p->userAgent = S(ua.c_str());
        p->deviceParams = DeviceParams::Create(env, width, height);
        p->platformParams = PlatformParams::Create(env, assets, width, height);
        // "Potato" is Roblox's own name for a device below the quality floor.
        p->isPotato = false;
        // **True only under `android-tablet`, and that makes it the one field
        // in this file that asserts a mobile form factor.**
        //
        // Tablet rather than phone, when it is claimed at all: a desktop window
        // is a large screen and that agrees with the XLARGE reported through
        // AConfiguration. It is false under `pc-windows-11` because a
        // User-Agent saying `Windows` and `Desktop` beside a field saying
        // tablet is a worse story than either half told alone, and false under
        // the default `roblox-app` for the plainer reason that Cordial is a
        // window on a desktop with a keyboard and a mouse, and the bare app
        // token deliberately claims no form factor at all.
        //
        // Worth stating because it is the thing most likely to be assumed the
        // other way: **`roblox-app` is not a request for mobile-tier
        // graphics.** Nothing here has established that Roblox tiers graphics
        // defaults off the User-Agent, and this field -- which the engine does
        // read -- says the same under `roblox-app` as it did under
        // `pc-windows-11`. Anyone chasing mobile-tier defaults wants
        // `android-tablet`, and wants to measure it rather than assume it.
        p->isTablet = device_identity() == DeviceIdentity::AndroidTablet;
        p->isVrDevice = false;
        p->vrContext = AndroidActivity::Create(env);
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<InitParams>("com/roblox/engine/jni/autovalue/InitParams");
        auto c = env->GetClass("com/roblox/engine/jni/autovalue/InitParams");
#define F(name) c->HookInstance(env, #name, &InitParams::name)
        F(baseURL); F(buildVariant); F(userAgent); F(deviceParams); F(platformParams);
        F(vrContext); F(isPotato); F(isTablet); F(isVrDevice);
#undef F
        // AutoValue exposes the fields through accessors as well, and the engine
        // uses whichever the generated class provided.
#define G(name) c->HookInstanceFunction(env, #name, &InitParams::get_##name)
        G(baseURL); G(buildVariant); G(userAgent); G(deviceParams); G(platformParams);
        G(isPotato); G(isTablet); G(isVrDevice); G(vrContext);
#undef G
    }
};

/// `com.roblox.engine.jni.autovalue.StartAppParams`
///
/// What actually delivers the surface. `nativeAppBridgeV2StartAppWithParams`
/// takes one of these and its `surface` field is the window the engine renders
/// into — a completely separate path from AGDK's `onSurfaceCreatedNative`
/// lifecycle, which disassembly shows structurally cannot produce a frame here.
class StartAppParams : public Object {
public:
    std::shared_ptr<String> appStarterPlace, appStarterScript, selectedTheme, username;
    std::shared_ptr<PlatformParams> platformParams;
    std::shared_ptr<AppSurface> surface;
    std::shared_ptr<AndroidActivity> vrContext;
    jlong appUserId = 0;
    jboolean isUnder13 = false;
    jint membershipType = 0;


    std::shared_ptr<PlatformParams> get_platformParams(ENV*) { return platformParams; }
    std::shared_ptr<AppSurface> get_surface(ENV*) { return surface; }
    std::shared_ptr<AndroidActivity> get_vrContext(ENV*) { return vrContext; }
    std::shared_ptr<String> get_appStarterPlace(ENV*) { return appStarterPlace; }
    std::shared_ptr<String> get_appStarterScript(ENV*) { return appStarterScript; }
    std::shared_ptr<String> get_selectedTheme(ENV*) { return selectedTheme; }
    std::shared_ptr<String> get_username(ENV*) { return username; }
    jlong get_appUserId(ENV*) { return appUserId; }
    jboolean get_isUnder13(ENV*) { return isUnder13; }
    jint get_membershipType(ENV*) { return membershipType; }

    static std::shared_ptr<StartAppParams> Create(ENV* env, const char* assets, int width,
                                                  int height,
                                                  std::shared_ptr<AppSurface> surface) {
        auto p = std::make_shared<StartAppParams>();
        // Empty starter place and script mean "the default app shell" rather than
        // a specific experience. Naming one here would launch straight into a
        // game, which is not what a cold start does.
        p->appStarterPlace = S("");
        p->appStarterScript = S("");
        p->selectedTheme = S("Dark");
        p->platformParams = PlatformParams::Create(env, assets, width, height);
        p->surface = std::move(surface);
        // The four identity fields, from the account `DID_LOG_IN` named, or the
        // signed-out values when nobody has signed in on this profile.
        //
        // **These four being hardcoded to zero is what kept a restored session
        // on the landing page.** `PlatformAccountRouter` runs after the cookie
        // has gone back into the engine, asks who is signed in, is told user 0
        // with an empty name, and routes to Landing without ever asking the
        // network — so a perfectly good cookie changed nothing. Measured with a
        // real signed-in store: five cookies held per domain, still `Landing`.
        //
        // This object is built once, inside
        // `nativeAppBridgeV2StartAppWithParams`, and the engine never asks for
        // these fields again — so the identity has to be restored before that
        // call, which `crates/cordial-runtime/src/identity.rs` does at startup
        // rather than anywhere near here.
        p->username = S(identity_username().c_str());
        p->appUserId = identity_user_id();
        p->isUnder13 = identity_is_under13();
        p->membershipType = identity_membership_type();
        fprintf(stderr, "[cordial] app start as %s\n",
                identity_known() ? "a signed-in user" : "nobody signed in");
        p->vrContext = AndroidActivity::Create(env);
        to_jni(env, p);
        return p;
    }

    static void Register(ENV* env) {
        env->GetClass<StartAppParams>("com/roblox/engine/jni/autovalue/StartAppParams");
        auto c = env->GetClass("com/roblox/engine/jni/autovalue/StartAppParams");
#define F(name) c->HookInstance(env, #name, &StartAppParams::name)
        F(appStarterPlace); F(appStarterScript); F(selectedTheme); F(username);
        F(platformParams); F(surface); F(vrContext); F(appUserId); F(isUnder13);
        F(membershipType);
#undef F
#define G(name) c->HookInstanceFunction(env, #name, &StartAppParams::get_##name)
        G(appStarterPlace); G(appStarterScript); G(selectedTheme); G(username);
        G(platformParams); G(surface); G(vrContext); G(appUserId); G(isUnder13);
        G(membershipType);
#undef G
    }
};




std::shared_ptr<Object> make_display_metrics(ENV* env) {
    return DisplayMetrics::Create(env, g_width, g_height);
}

/// Hook `getResources` onto the Activity classes registered elsewhere.
///
/// Typed here rather than in game_activity.cpp because libjnivm binds by the JNI
/// descriptor it derives from the C++ signature: a `shared_ptr<Object>` return
/// becomes `Ljava/lang/Object;`, which never matches the
/// `()Landroid/content/res/Resources;` Roblox asks for. The hook registers
/// happily and is simply never called.
static std::shared_ptr<Resources> activity_get_resources(ENV* env, Object*) {
    return Resources::Create(env);
}

/// The engine reaches its own status channel through the Activity:
/// `activity.getNativeHelper().gameActivity_onFlagsFailed()`. A null helper here
/// means the failure report itself crashes, which is how the verdict stayed
/// invisible.
static std::shared_ptr<NativeHelper> activity_get_native_helper(ENV* env, Object*) {
    auto p = std::make_shared<NativeHelper>();
    to_jni(env, p);
    return p;
}

static void hook_activity_resources(ENV* env, const char* klass) {
    auto c = env->GetClass(klass);
    if (c) {
        c->HookInstanceFunction(env, "getResources", &activity_get_resources);
        c->HookInstanceFunction(env, "getNativeHelper", &activity_get_native_helper);
    }
}

void register_init_params_classes(ENV* env) {
    JavaLocale::Register(env);
    LocaleList::Register(env);
    JavaMap::Register(env);
    JavaList::Register(env);
    NativeFlagsInitResult::Register(env);
    JSONObject::Register(env);
    ClientLocalFlags::Register(env);
    NativeHelper::Register(env);
    LocalStorageManager::Register(env);
    DisplayMetrics::Register(env);
    AppSurface::Register(env);
    Resources::Register(env);
    Configuration::Register(env);
    Insets::Register(env);
    WindowInsetsCompatType::Register(env);
    AndroidActivity::Register(env);
    // These classes are registered by register_game_activity_classes, which runs
    // first; only the descriptor-correct hook belongs here.
    if (auto cfg = env->GetClass("android/content/res/Configuration")) {
        cfg->HookInstanceFunction(env, "getLocales", &configuration_get_locales);
    }
    for (const char* k : {"com/google/androidgamesdk/GameActivity",
                          "com/roblox/client/startup/MainGameActivity",
                          "android/app/Activity"}) {
        hook_activity_resources(env, k);
    }
    DeviceParams::Register(env);
    PlatformParams::Register(env);
    InitParams::Register(env);
    StartAppParams::Register(env);
}

} // namespace cordial

extern "C" {

/// Call `MainGameActivity.nativeAppBridgeSetInitParams(InitParams)`.
int cordial_set_init_params(void* fn, const char* assets, int width, int height, char* err,
                            size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeAppBridgeSetInitParams is not exported");
        return -1;
    }
    try {
        auto params = cordial::InitParams::Create(env, assets, width, height);
        auto activity = std::make_shared<jnivm::Object>();
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   cordial::to_jni(env, activity),
                                   cordial::to_jni(env, params));
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

/// `JNIAAssetManagerSetup.initNative(AssetManager)` — a *static* native, so the
/// second argument is the class rather than an instance.
///
/// This is how the engine gets its asset manager. Without it the engine has no
/// way to read its own content, which is why nothing downstream ever starts:
/// no assets, no app shell, no reason to open a socket or draw a frame.
int cordial_asset_manager_init(void* fn, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or initNative is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/client/JNIAAssetManagerSetup");
        auto assets = std::make_shared<jnivm::Object>();
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   cordial::to_jni(env, cls),
                                   cordial::to_jni(env, assets));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `LocalStorageManager.initStorageManagerNativeV3(AssetManager, String, String)`
int cordial_storage_init(void* fn, const char* a, const char* b, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jobject, jstring, jstring);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or the storage native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/client/LocalStorageManager");
        auto assets = std::make_shared<jnivm::Object>();
        auto s1 = cordial::S_pub(a);
        auto s2 = cordial::S_pub(b);
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   cordial::to_jni(env, cls),
                                   cordial::to_jni(env, assets),
                                   cordial::to_jni(env, s1),
                                   cordial::to_jni(env, s2));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// A static native on a named class taking one to three `String` arguments.
///
/// `NativeSettingsInterface` is where the app tells the engine which directories
/// it owns — `nativeSetFilesDirectory`, `nativeSetCacheDirectory`,
/// `nativeSetExternalDirectory`, `nativeSetBaseDataDirectories`. Cordial called
/// none of them, so the engine ran with those roots unset and resolved every
/// path it built from them against the working directory: `./appData`, `cache`,
/// `http`, `sounds`, `ContentProvider_<pid>`. The Waydroid capture shows the
/// real client using absolute paths under the app's own storage for all of them.
///
/// The signatures come from the shipping APK's own declarations, read out of the
/// dex — the host app's side of a contract Cordial is reimplementing.
int cordial_call_static_strings(void* fn, const char* class_name, const char* const* args,
                                size_t n, char* err, size_t err_len) {
    auto* env = cordial::process_env();
    if (!fn || !env || !class_name) {
        snprintf(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    if (n > 3) {
        snprintf(err, err_len, "at most three string arguments are supported");
        return -1;
    }
    try {
        auto cls = env->GetClass(class_name);
        auto* jenv = env->GetJNIEnv();
        auto self = (jobject)cordial::to_jni(env, cls);
        jstring a[3] = {nullptr, nullptr, nullptr};
        std::shared_ptr<cordial::String> keep[3];
        for (size_t i = 0; i < n; ++i) {
            keep[i] = cordial::S_pub(args[i] ? args[i] : "");
            a[i] = (jstring)cordial::to_jni(env, keep[i]);
        }
        switch (n) {
            case 0:
                reinterpret_cast<void (*)(JNIEnv*, jobject)>(fn)(jenv, self);
                break;
            case 1:
                reinterpret_cast<void (*)(JNIEnv*, jobject, jstring)>(fn)(jenv, self, a[0]);
                break;
            case 2:
                reinterpret_cast<void (*)(JNIEnv*, jobject, jstring, jstring)>(fn)(
                    jenv, self, a[0], a[1]);
                break;
            default:
                reinterpret_cast<void (*)(JNIEnv*, jobject, jstring, jstring, jstring)>(fn)(
                    jenv, self, a[0], a[1], a[2]);
                break;
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

/// A static native taking `(boolean, String)`.
///
/// `NativeGLInterface.setTaskSchedulerBackgroundMode(Z, Ljava/lang/String;)`. The
/// capture shows the real client calling it immediately before
/// `nativeAppBridgeV2StartApp`:
///
/// ```text
/// [FLog::AndroidGLView] rbx.datamodel: setTaskSchedulerBackgroundMode() enable:false context:ASMA.start
/// [FLog::JNIAppBridge] nativeAppBridgeV2StartApp:
/// ```
///
/// A task scheduler left in background mode is a scheduler that has been told
/// not to render.
int cordial_call_static_bool_string(void* fn, const char* class_name, int flag, const char* text,
                                    char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jboolean, jstring);
    auto* env = cordial::process_env();
    if (!fn || !env || !class_name) {
        snprintf(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass(class_name);
        auto s = cordial::S_pub(text ? text : "");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   (jobject)cordial::to_jni(env, cls),
                                   flag ? JNI_TRUE : JNI_FALSE,
                                   (jstring)cordial::to_jni(env, s));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeSettingsInterface.nativeSetDeviceInfo(DeviceParams)`.
///
/// The dedicated path for telling the engine what it is running on. Cordial only
/// ever delivered `DeviceParams` nested inside `InitParams`, and never called
/// this at all.
int cordial_set_device_info(void* fn, int width, int height, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeSetDeviceInfo is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeSettingsInterface");
        auto dev = cordial::DeviceParams::Create(env, width, height);
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   (jobject)cordial::to_jni(env, cls),
                                   (jobject)cordial::to_jni(env, dev));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `LocalStorageManager.initStorageManagerNativeV3(AssetManager, String, String)`
///
/// The engine's content store, which Cordial has never initialised. Its own log
/// says so on every run -- `RbxStorage is not initialized, cannot access storage
/// interface`, and `CrashMetricStorage: Failed to initialize storage interface`
/// -- and the effect is visible on disk: Sober's `appData` carries a 167 MB
/// `rbx-storage.db` plus `rbx-storage/` and `rbx-storage.id`, and Cordial's
/// carries none of the three.
///
/// The prototype is read from the dex with `tools/dex_method.py`, not guessed.
/// Guessing arity has already cost this project two crashes in one session.
///
/// The `AssetManager` is the deliberately-empty object from `game_activity.cpp`:
/// the native side reaches assets through `AAssetManager_fromJava`, which
/// resolves to Cordial's process-wide manager, so the object only has to exist
/// and be of the right class.
///
/// **What the two strings are is not established.** They are passed the files
/// and cache directories, in that order, because those are the two paths the
/// engine is already given separately by `nativeSetBaseDataDirectories(files,
/// cache)` and the shapes match. If that is wrong, the engine's own RbxStorage
/// logging is what will say so.
int cordial_init_storage_manager(void* fn, const char* a, const char* b, char* err,
                                 size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jobject, jstring, jstring);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or initStorageManagerNativeV3 is not exported");
        return -1;
    }
    try {
        // **An instance, not the class.** The dex declares this as
        // `LocalStorageManager.initStorageManagerNativeV3(AssetManager, String,
        // String)V` with no ACC_STATIC, so JNI's second argument is `this`.
        // This used to pass `to_jni(env, cls)` -- the `Class` object itself --
        // which is what a *static* native expects and is a different thing
        // entirely from an instance of that class.
        //
        // Nothing threw, so `cordial_init_storage_manager` returned 0 and
        // Cordial has been logging `initStorageManagerNativeV3 ok` on every
        // run while handing the engine a receiver of the wrong kind. That is
        // the same failure as the static-vs-instance mismatch on
        // `NetworkUtils.getPublicIPv4Addresseses` found hours earlier, and the
        // reason it survives is always the same: an exception would have been
        // noticed, and there isn't one.
        //
        // mocktail, whose store works, passes `NewObject(env,
        // "com/roblox/client/LocalStorageManager")` here. Reading that is what
        // exposed this; `docs/analysis/flag-init.md` had spent nine sections
        // downstream of a call that was being made wrongly.
        auto cls = env->GetClass("com/roblox/client/LocalStorageManager");
        auto self = std::make_shared<jnivm::Object>();
        self->clazz = cls;
        auto am_cls = env->GetClass("android/content/res/AssetManager");
        auto am = std::make_shared<jnivm::Object>();
        am->clazz = am_cls;
        auto sa = cordial::S_pub(a ? a : "");
        auto sb = cordial::S_pub(b ? b : "");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   (jobject)cordial::to_jni(env, self),
                                   (jobject)cordial::to_jni(env, am),
                                   (jstring)cordial::to_jni(env, sa),
                                   (jstring)cordial::to_jni(env, sb));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// A native taking only `(JNIEnv*, jobject)` — `nativeRetryInit`.
int cordial_call_bare(void* fn, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    try {
        auto obj = std::make_shared<jnivm::Object>();
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), cordial::to_jni(env, obj));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// A static, zero-argument native returning `boolean` — specifically added to
/// observe `NativeSettingsInterface.nativeIsLuaLoginEnabled()`'s own verdict,
/// diagnostic-only instrumentation for `docs/design/sign-in.md`. This does not
/// drive any UI or enter any credentials; it only reads the engine's boolean
/// answer. Mirrors `cordial_call_static_strings`'s convention: a static
/// native's receiver is the `Class` object itself, per JNI.
int cordial_call_static_bare_bool(void* fn, const char* class_name, int* out_result,
                                   char* err, size_t err_len) {
    using Call = jboolean (*)(JNIEnv*, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env || !class_name) {
        snprintf(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass(class_name);
        jboolean r = reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                                 (jobject)cordial::to_jni(env, cls));
        if (out_result) {
            *out_result = r ? 1 : 0;
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

/// `FlagJniInterface.nativeInitializeNativeFlags(String[])`
///
/// This is what `bootstrapTheApp()` exists to reach. On Android the Kotlin
/// bootstrap fetches the flag set and passes it here; the engine then reports
/// back through `NativeHelper.gameActivity_onFlagsLoaded` or, failing that,
/// `gameActivity_onFlagsFailed` — and the second is what Cordial has been
/// getting, because nothing ever called this.
///
/// An empty array means "no overrides": the engine falls back to the defaults
/// compiled into it. That is the honest starting point — inventing flag values
/// would change engine behaviour in ways nothing here could account for.
int cordial_init_flags(void* fn, const char* settings_json, char* err, size_t err_len) {
    using Call = jobject (*)(JNIEnv*, jclass, jobjectArray);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeInitializeNativeFlags is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/client/flags/FlagJniInterface");

        // The array is a list of flag *names to cache*, not a settings document.
        //
        // This was wrong for several iterations: passing Roblox's ClientSettings
        // JSON here made the engine call addBoolean with the entire document as a
        // single flag name, which is exactly what the trace showed. The flag
        // *values* come from the engine's own load, not from this argument, so
        // supplying a document here could never have fixed onFlagsFailed (the
        // real cause was the `<init>` registration bug documented on
        // `NativeFlagsInitResult`, above).
        //
        // The real Android client passes 139 specific names here. That is not a
        // guess: a Waydroid capture of this same APK logs
        //
        //   nativeInitializeNativeFlags: Registered Flag Provider ID from Java: 0
        //   nativeInitializeNativeFlags: flagCount = 139.
        //   ... 0: EnableAndroidBinaryChannelDownloadTiming not found.
        //   ... 5: FixAndroidWebDialogPaymentSessionId = true
        //
        // and docs/traces/native-flag-names.txt is that list, in order. An empty
        // array is what Cordial sent for a long time; it is accepted, but it is
        // not what the client does.
        //
        // `settings_json` is a newline-separated list of names. Blank lines are
        // skipped so the file can be edited by hand without care.
        std::vector<std::string> names;
        if (settings_json) {
            std::string all(settings_json);
            size_t pos = 0;
            while (pos <= all.size()) {
                size_t nl = all.find('\n', pos);
                if (nl == std::string::npos) nl = all.size();
                std::string one = all.substr(pos, nl - pos);
                while (!one.empty() && (one.back() == '\r' || one.back() == ' ')) one.pop_back();
                if (!one.empty()) names.push_back(one);
                if (nl == all.size()) break;
                pos = nl + 1;
            }
        }
        auto arr = std::make_shared<jnivm::Array<jnivm::String>>(names.size());
        for (size_t k = 0; k < names.size(); ++k) {
            (*arr)[k] = std::make_shared<jnivm::String>(names[k]);
        }
        reinterpret_cast<Call>(fn)(
            env->GetJNIEnv(),
            (jclass)cordial::to_jni(env, cls),
            (jobjectArray)cordial::to_jni(env, arr));
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

/// `NativeGLInterface.readLocalFlags()` — `()Lcom/roblox/engine/jni/model/ClientLocalFlags;`
///
/// The offline counterpart to the network `ClientSettings` fetch: the engine
/// reads whatever bundled/cached flag defaults it has on disk and hands them
/// back as a `ClientLocalFlags`, built the same `new` + repeated `add(name,
/// value)` way `nativeInitializeNativeFlags` builds its result. Nothing in
/// the shipping dex calls this on the `ActivityNativeMain` path Cordial
/// drives — its only caller is a different startup path (`com/roblox/client/
/// startup/a.l`, found by dex xref) that Cordial does not replicate — so it
/// is otherwise dead code here. Calling it directly, with no argument and no
/// forged network response, is legitimate: it is the engine's own exported
/// native reading its own bundled state.
/// `NativeGLInterface.nativePassCurrentDisplayRefreshRate(F)V` and
/// `nativePassSupportedRefreshRates([F)V`.
///
/// How a client tells the engine what its display can do. Cordial has never
/// called either, so the engine has been running on whatever it assumes when the
/// application says nothing — and AGENTS.md records the frame rate as a hard
/// FIFO vsync lock to the output's refresh whenever input is flowing. Whether
/// speaking changes that is untested; being the only party able to speak and
/// staying silent is worth ending either way.
///
/// The choice of *which* rate, when a window is on two outputs at once, is made
/// in `crates/cordial-runtime/src/refresh.rs` and tested there. These two only
/// carry the answer across.
int cordial_pass_current_refresh_rate(void* fn, float hz, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jfloat);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePassCurrentDisplayRefreshRate is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jclass)cordial::to_jni(env, cls),
                                   static_cast<jfloat>(hz));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

int cordial_pass_supported_refresh_rates(void* fn, const float* rates, size_t count, char* err,
                                         size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jfloatArray);
    auto* env = cordial::process_env();
    if (!fn || !env || (!rates && count)) {
        snprintf(err, err_len, "no JavaVM, or nativePassSupportedRefreshRates is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        auto arr = std::make_shared<jnivm::Array<jfloat>>(count);
        for (size_t i = 0; i < count; ++i) {
            (*arr)[i] = static_cast<jfloat>(rates[i]);
        }
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jclass)cordial::to_jni(env, cls),
                                   (jfloatArray)cordial::to_jni(env, arr));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

int cordial_read_local_flags(void* fn, char* err, size_t err_len) {
    using Call = jobject (*)(JNIEnv*, jclass);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or readLocalFlags is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jclass)cordial::to_jni(env, cls));
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

/// `NativeGLInterface.nativeInitClientSettings(String, String, String)I` —
/// `com/roblox/engine/jni/NativeGLInterface.nativeInitClientSettings
/// (Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I` per the dex.
///
/// On Android this is what the app calls once it has fetched
/// `https://clientsettings.roblox.com/...` itself — the engine does not fetch
/// its own flags; its *host app* does, and hands the response to the engine
/// through this native. Cordial *is* the host app in this architecture, so
/// calling it directly, with Roblox's own real ClientSettings response body,
/// is the legitimate interface, not a workaround: no HTTP stub, no forged
/// server, no impersonation of `clientsettings.roblox.com`.
///
/// The three `String` parameters' exact roles were not able to be pinned
/// down with confidence in this pass (see the accompanying report); this
/// wrapper passes them through as given so the caller can supply candidates
/// and read the `int` back, which is a far more reliable signal than
/// anything printed to the log.
int cordial_init_client_settings(void* fn, const char* a, const char* b, const char* c,
                                 jint* out_result, char* err, size_t err_len) {
    using Call = jint (*)(JNIEnv*, jclass, jstring, jstring, jstring);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeInitClientSettings is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        auto sa = cordial::S_pub(a ? a : "");
        auto sb = cordial::S_pub(b ? b : "");
        auto sc = cordial::S_pub(c ? c : "");
        jint result = reinterpret_cast<Call>(fn)(
            env->GetJNIEnv(),
            (jclass)cordial::to_jni(env, cls),
            (jstring)cordial::to_jni(env, sa),
            (jstring)cordial::to_jni(env, sb),
            (jstring)cordial::to_jni(env, sc));
        if (out_result) {
            *out_result = result;
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

/// Tell the framework layer how big the window actually is.
///
/// `set_display_size` has existed since the Activity was first stubbed and had
/// **zero call sites anywhere in the tree** — it is not `extern "C"`, so nothing
/// on the Rust side could ever have reached it. `g_width`/`g_height` therefore
/// sat at the compiled 1280x720 for the life of every process, and every Java
/// answer built from them lied by exactly the difference between that and the
/// real window: `DisplayMetrics.widthPixels`/`heightPixels`, the resolution
/// fields in the User-Agent, and the `AConfiguration` screen size the comment on
/// those globals says all have to agree.
///
/// It matters most where it is least visible. At the tool's own default
/// resolution the lie is zero, which is why every harness run looked consistent;
/// go fullscreen on a 3440x1440 display and the engine is still being told
/// 1280x720 by everything that reads these.
///
/// **Whether the engine scales pointer deltas by any of this is `INFERRED`** and
/// is being chased separately. It is wired regardless, because an answer Cordial
/// gives the engine should be true whether or not the current bug turns out to
/// depend on it.
/// The desktop's dark/light preference, as an `android.content.res.Configuration`
/// `uiMode` night field.
///
/// Set from Rust before the init params are built. -1 means "nobody said",
/// which reports `kUiModeNightNo` -- see the field's own comment for why that
/// is the honest default rather than a guess.
static std::atomic<int> g_ui_mode_night{-1};

extern "C" void cordial_set_ui_mode_night(int night) {
    g_ui_mode_night.store(night, std::memory_order_relaxed);
}

/// Whether this host has a touchscreen, as the display backend found it.
///
/// -1 means nobody has said, which reports "no touchscreen" and which
/// `host_has_touchscreen` distinguishes out loud the first time anything asks.
/// That is the honest default rather than a convenient one: a run where the
/// window never opened has not established that a touchscreen exists, and
/// `isTouchDevice` is the one field of the three the engine actually reads.
static std::atomic<int> g_touchscreen_present{-1};

/// Set from Rust once the seat's devices are known, before the engine is
/// initialised.
///
/// **This is latched for the session in practice, and the reason is ordering
/// rather than policy.** `android::input::report_touchscreen` runs from the
/// display backend's `open()`, which `load.rs` calls before
/// `cordial_appbridge_init`/`cordial_set_init_params`; the engine reads
/// `isTouchDevice` during that initialisation and there is no call anywhere in
/// this build by which a platform revises it afterwards. So a touchscreen
/// plugged in after startup gets its events routed — `android::input` decides
/// that per event — but arrives too late to change what the engine was told
/// about the device. Writing it later is harmless and simply has no reader.
extern "C" void cordial_set_touchscreen_present(int present) {
    g_touchscreen_present.store(present ? 1 : 0, std::memory_order_relaxed);
}

// `extern "C++"` because this whole region sits inside an `extern "C"` block,
// and a definition in there acquires C language linkage however deep in a
// namespace it is: `nm` on the object showed a bare `T ui_mode_night_bits`
// against a mangled `U cordial::...` at the call site. That is the second half
// of the same latent link failure the declarations near `Configuration`
// describe -- `ui_mode_night_bits` has had it since it was written and only
// escaped notice because its one caller is dead code that `--gc-sections`
// throws away. The linkage specification is the smallest honest fix; splitting
// the surrounding block would move a dozen unrelated entry points.
extern "C++" {
namespace cordial {
bool host_has_touchscreen() {
    int reported = g_touchscreen_present.load(std::memory_order_relaxed);
    // Once, and only from the first caller, because the two fields that ask are
    // built one after the other and two identical lines in a log read as a bug
    // in whatever is between them. -1 and 0 are worth distinguishing from a
    // log: "the backend looked and found no touchscreen" and "the backend never
    // got as far as looking" are different runs and both report false.
    static std::once_flag said;
    std::call_once(said, [reported] {
        printf("[android] host touchscreen: %s (%s)\n", reported > 0 ? "yes" : "no",
               reported < 0 ? "no display backend reported a seat"
                            : "reported by the display backend");
        fflush(stdout);
    });
    return reported > 0;
}

jint ui_mode_night_bits() {
    // `Configuration.UI_MODE_NIGHT_YES`/`_NO`. Spelled as literals because the
    // named constants are class-scoped members of the InitParams builder above
    // and are not reachable from here; they carry the same two values.
    return g_ui_mode_night.load(std::memory_order_relaxed) > 0 ? 0x20 : 0x10;
}
} // namespace cordial
} // extern "C++"

extern "C" void cordial_set_display_size(int width, int height) {
    if (width > 0 && height > 0) {
        cordial::set_display_size(width, height);
    }
}

/// `FlagJniInterface.nativeGetFInt(String, int)I` — read a live `FInt` back out
/// of the engine.
///
/// Read-only, and that is the point. This exists because Cordial had no way to
/// ask the engine what value a flag actually holds, only to push values in and
/// infer from behaviour. That inference went wrong: setting `FLogNativeDM` in
/// `flags.json` *silenced* the channel at every value tried, including 100,
/// while the same mechanism took `FLogAppShellReporter` from 0 to 14 lines. One
/// of those two readings has to be wrong about what the override does, and
/// guessing which cost a session. See docs/analysis/flag-init.md §22.
///
/// The second argument is the default the engine returns when the flag is not
/// registered, so passing a sentinel distinguishes "set to zero" from "not a
/// flag" — a distinction the log alone cannot make.
int cordial_get_fint(void* fn, const char* name, jint fallback, jint* out_result,
                     char* err, size_t err_len) {
    using Call = jint (*)(JNIEnv*, jclass, jstring, jint);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeGetFInt is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/client/flags/FlagJniInterface");
        auto sname = cordial::S_pub(name ? name : "");
        jint result = reinterpret_cast<Call>(fn)(
            env->GetJNIEnv(),
            (jclass)cordial::to_jni(env, cls),
            (jstring)cordial::to_jni(env, sname),
            fallback);
        if (out_result) {
            *out_result = result;
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

/// `NativeGLInterface.nativeInitClientSettingsCachedCompressed([B, String,
/// String, String, long, boolean)I`
///
/// The engine writes its own compressed flag cache -- `flag_cache.dat`, 365 KB
/// on this machine, produced by `[DFLog::FlagCache] writeFlagCache` -- and
/// exports three natives that take a cache back in besides the plain
/// three-string form Cordial has always used. Cordial has never handed one
/// back, so every launch has been a cold one from the engine's point of view
/// even when the cache was sitting on disk beside it.
///
/// Whether the cached path is what sets the engine's flags-loaded state is
/// **not established** -- it is being tried because thirteen candidates that
/// varied the plain path have all left the verdict where it was, and this is a
/// different path rather than another variation of the same one. See
/// docs/analysis/flag-init.md §22.
///
/// The trailing `long` and `boolean` are passed through as given rather than
/// guessed at, for the same reason the three strings are: the caller can vary
/// them and read the `int` back, which is a better signal than the log.
int cordial_init_client_settings_cached_compressed(void* fn, const void* data, size_t len,
                                                   const char* a, const char* b, const char* c,
                                                   long long when, int flag, jint* out_result,
                                                   char* err, size_t err_len) {
    using Call = jint (*)(JNIEnv*, jclass, jbyteArray, jstring, jstring, jstring, jlong, jboolean);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativeInitClientSettingsCachedCompressed is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        auto bytes = std::make_shared<jnivm::Array<jbyte>>(static_cast<jsize>(len));
        if (len != 0 && data != nullptr) {
            memcpy(bytes->getArray(), data, len);
        }
        auto sa = cordial::S_pub(a ? a : "");
        auto sb = cordial::S_pub(b ? b : "");
        auto sc = cordial::S_pub(c ? c : "");
        jint result = reinterpret_cast<Call>(fn)(
            env->GetJNIEnv(),
            (jclass)cordial::to_jni(env, cls),
            (jbyteArray)cordial::to_jni(env, bytes),
            (jstring)cordial::to_jni(env, sa),
            (jstring)cordial::to_jni(env, sb),
            (jstring)cordial::to_jni(env, sc),
            static_cast<jlong>(when),
            static_cast<jboolean>(flag ? JNI_TRUE : JNI_FALSE));
        if (out_result) {
            *out_result = result;
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

/// `NativeGLInterface.nativePostClientSettingsLoadedInitialization3(List)V`
///
/// The finishing step of the client-settings handshake on the real app's
/// side. Called with an empty `ArrayList` — the honest starting point, since
/// nothing here knows what real elements the list would otherwise carry.
int cordial_post_client_settings_loaded(void* fn, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jclass, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePostClientSettingsLoadedInitialization3 is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        auto list = cordial::JavaList::ctor(env, nullptr);
        reinterpret_cast<Call>(fn)(
            env->GetJNIEnv(),
            (jclass)cordial::to_jni(env, cls),
            (jobject)cordial::to_jni(env, list));
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

/// `MainGameActivity.nativePreloadFlagOverrides(String)V`
///
/// Takes one `String` per the dex descriptor
/// (`com/roblox/client/startup/MainGameActivity.nativePreloadFlagOverrides
/// (Ljava/lang/String;)V`). This wrapper hands whatever JSON text it is given
/// straight through, unexamined, so the caller can experiment with candidate
/// shapes (a flat `{"FlagName":"value"}` map vs. the doubly-wrapped
/// `{"applicationSettings":{...}}` shape the real `ClientSettings` endpoint
/// returns) and compare the resulting JNI trace / flags verdict.
int cordial_preload_flag_overrides(void* fn, const char* json, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jstring);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or nativePreloadFlagOverrides is not exported");
        return -1;
    }
    try {
        // An instance native (per `cordial_set_init_params`'s precedent just
        // above): the second argument is an Activity instance, not the class.
        auto activity = std::make_shared<jnivm::Object>();
        auto s = cordial::S_pub(json ? json : "");
        reinterpret_cast<Call>(fn)(
            env->GetJNIEnv(),
            (jobject)cordial::to_jni(env, activity),
            (jstring)cordial::to_jni(env, s));
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

/// `NativeGLInterface.nativeAppBridgeV2InitWithParams(InitParams)`
///
/// The real entry to Roblox's app bridge. The launcher Activity is
/// `ActivitySplash`, whose default target is `ActivityNativeMain` — not the AGDK
/// `MainGameActivity`, which the manifest marks `exported=false`. The chain that
/// actually brings the client up runs through here, not through
/// `MainGameActivity.nativeAppBridgeSetInitParams`.
int cordial_appbridge_init(void* fn, const char* assets, int width, int height, char* err,
                           size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or the app-bridge native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        auto params = cordial::InitParams::Create(env, assets, width, height);
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   (jobject)cordial::to_jni(env, cls),
                                   (jobject)cordial::to_jni(env, params));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// A no-argument native on an arbitrary class.
///
/// `nativeAppBridgeAppStart` lives on `NativeAppBridgeInterface`, not
/// `NativeGLInterface` — which is why searching the GL interface for it kept
/// coming up empty. The Waydroid capture shows the real client calling it first,
/// before `nativeAppBridgeV2Init`.
int cordial_appbridge_call_bare_cls(void* fn, const char* class_name, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env || !class_name) {
        snprintf(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass(class_name);
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// A `NativeGLInterface` native taking no arguments — `nativeAppBridgeStartLuaAppDM`.
///
/// "Start Lua App DataModel": the Lua app shell is what Roblox actually renders
/// on this platform, so this is the call that turns a live engine into a drawing
/// one.
int cordial_appbridge_call_bare(void* fn, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or the native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(), (jobject)cordial::to_jni(env, cls));
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

/// `NativeGLInterface.nativeAppBridgeV2StartAppWithParams(StartAppParams)`
///
/// The call that hands the engine its window. Everything before it is setup.
int cordial_appbridge_start_app(void* fn, const char* assets, int width, int height, char* err,
                                size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or StartAppWithParams is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        // Reuses the android/view/Surface type registered in game_activity.cpp —
        // registering a second C++ class for the same Java name makes libjnivm throw.
        auto surface = cordial::AppSurface::Create(env);
        auto params = cordial::StartAppParams::Create(env, assets, width, height, surface);
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   (jobject)cordial::to_jni(env, cls),
                                   (jobject)cordial::to_jni(env, params));
        return 0;
    } catch (const std::exception& e) {
        snprintf(err, err_len, "%s", e.what());
        return -1;
    } catch (...) {
        snprintf(err, err_len, "non-standard C++ exception");
        return -1;
    }
}

/// `NativeGLInterface.nativeAppBridgeV2UpdateSurfaceAppWithPlatformParams(Surface, PlatformParams)`
/// and `...UpdateSurfaceGameWithPlatformParams(Surface, PlatformParams, Activity)`.
///
/// **Measured gap, 2026-08-04.** Sober makes 87 `FLog::JNIAppBridge` calls in a
/// session and Cordial made 3; these two were among the ones Cordial never made,
/// and neither was referenced anywhere in the tree. Sober calls both at about
/// 3.79s — before any join — and again at 109s, so they are not a one-shot part
/// of startup. Everything they need was already built here for
/// `StartAppWithParams`; nothing was constructing them and handing them over.
///
/// Whether this is what the server wants before it stops sending disconnect
/// reason 304 is **not established**. It is the largest measured difference
/// between a client that stays connected and one that does not, which is a
/// reason to try it and not a reason to claim it.
///
/// The signatures come from the dex's own method table
/// (`tools/dex_method.py`), not from reading what the Java side does with them.
static int update_surface(void* fn, const char* assets, int width, int height, bool with_activity,
                          char* err, size_t err_len) {
    using CallApp = void (*)(JNIEnv*, jobject, jobject, jobject);
    using CallGame = void (*)(JNIEnv*, jobject, jobject, jobject, jobject);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or the UpdateSurface native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass("com/roblox/engine/jni/NativeGLInterface");
        // The same Surface and PlatformParams `StartAppWithParams` builds — one
        // registered `android/view/Surface` C++ class for the one Java name, or
        // libjnivm throws on the second registration.
        auto surface = cordial::AppSurface::Create(env);
        auto params = cordial::PlatformParams::Create(env, assets, width, height);
        if (with_activity) {
            auto activity = cordial::AndroidActivity::Create(env);
            reinterpret_cast<CallGame>(fn)(env->GetJNIEnv(),
                                           (jobject)cordial::to_jni(env, cls),
                                           (jobject)cordial::to_jni(env, surface),
                                           (jobject)cordial::to_jni(env, params),
                                           (jobject)cordial::to_jni(env, activity));
        } else {
            reinterpret_cast<CallApp>(fn)(env->GetJNIEnv(),
                                          (jobject)cordial::to_jni(env, cls),
                                          (jobject)cordial::to_jni(env, surface),
                                          (jobject)cordial::to_jni(env, params));
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

int cordial_appbridge_update_surface_app(void* fn, const char* assets, int width, int height,
                                         char* err, size_t err_len) {
    return update_surface(fn, assets, width, height, /*with_activity=*/false, err, err_len);
}

int cordial_appbridge_update_surface_game(void* fn, const char* assets, int width, int height,
                                          char* err, size_t err_len) {
    return update_surface(fn, assets, width, height, /*with_activity=*/true, err, err_len);
}

} // extern "C"

extern "C" {

/// One of `JNIActivityLifecycleCallbacks`' natives, all of which take the
/// Activity's name.
///
/// Android's `Application.ActivityLifecycleCallbacks` fires these as the Activity
/// moves through its states, and the engine stores per-Activity context —
/// including the JNI environment it later reaches through — when it does.
/// Nothing in Cordial was driving them, which is why the engine held a null
/// environment on the game thread and faulted calling FindClass through it.
int cordial_activity_lifecycle(void* fn, const char* activity, char* err, size_t err_len) {
    using Call = void (*)(JNIEnv*, jobject, jstring);
    auto* env = cordial::process_env();
    if (!fn || !env) {
        snprintf(err, err_len, "no JavaVM, or the lifecycle native is not exported");
        return -1;
    }
    try {
        auto cls = env->GetClass(
            "com/roblox/universalapp/activitylifecyclecallbacks/JNIActivityLifecycleCallbacks");
        auto name = cordial::S_pub(activity);
        reinterpret_cast<Call>(fn)(env->GetJNIEnv(),
                                   (jobject)cordial::to_jni(env, cls),
                                   (jstring)cordial::to_jni(env, name));
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
