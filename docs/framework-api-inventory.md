# Framework API inventory — the Phase 2 backlog

**Task C (§16.4). Status: populated.**

**Source:** Roblox for Android **2.732.1043** (versionCode 2814, minSdk 26, targetSdk 35),
pulled from a Waydroid instance where Google Play served the `x86_64` split. Package
`com.roblox.client`, `primaryCpuAbi=x86_64`, split-APK delivery:

| Artifact | Size | Contents |
|---|---|---|
| `base.apk` | 98.0 MB | dex ×3 (20.1 MB), resources, assets. **No `lib/` directory.** |
| `split_config.x86_64.apk` | 52.8 MB | `lib/x86_64/` — 11 native objects |

Raw enumeration output is in [`analysis/`](analysis):
[`needed-libs.tsv`](analysis/needed-libs.tsv) ·
[`undefined-symbols.tsv`](analysis/undefined-symbols.tsv) ·
[`jni-natives.tsv`](analysis/jni-natives.tsv) ·
[`framework-classes.txt`](analysis/framework-classes.txt)

---

## 1. Headline numbers

| Surface | Count | Layer |
|---|---|---|
| Android shared libraries linked (`DT_NEEDED` union) | **13** | Runtime |
| Undefined symbols across all native objects | **644 unique** | Runtime |
| Statically-registered JNI natives (`Java_*`) | **693** (518 in `libroblox.so`) | Framework |
| Android/AndroidX framework classes referenced from dex | **3,466** | Framework |
| Declared Roblox Activities | **8** | Framework |

The runtime surface is small and bounded. The framework surface is large but heavily
weighted toward AndroidX support libraries that ship *inside* the APK — only the
`android/*` platform classes (about 630 of the 3,466) are Cordial's responsibility.

## 2. Runtime layer — the native link surface

Every Android library the APK's native code links against. This is the complete list.

| Library | Used by | Class | Notes |
|---|---|---|---|
| `libc.so` | all | **implement** | bionic. The single largest Phase 1 item — 490 of the 644 undefined symbols are libc/libm/libdl. |
| `libm.so` | all | **implement** | Mostly satisfiable from glibc directly. |
| `libdl.so` | all | **implement** | Must be the *runtime's* loader, not the host's — `dlopen` has to resolve inside the Android namespace. |
| `liblog.so` | most | **stub-inert** | `__android_log_*`. Route to Cordial's log. Trivial and high value — do it first. (Spec §9a also called this the source of `onLogLine`; that event was never built, see liblog.cpp.) |
| `libGLESv2.so` | `libroblox` | **implement** | 74 undefined `gl*` symbols. → Mesa. |
| `libEGL.so` | `libroblox` | **implement** | 17 undefined `egl*` symbols. → Mesa/EGL. |
| `libandroid.so` | `libroblox`, `libsurface_util_jni`, `libimage_processing_util_jni` | **implement** | 31 symbols. NDK native API: `ANativeWindow`, `AInputEvent`, `ALooper`, `AChoreographer`, `AAssetManager`, `ASensor`. This is where host input and windowing actually meet Roblox. |
| `libmediandk.so` | `libroblox` | **implement** | 23 `AMedia*`/`AImage*` symbols. Video decode. |
| `libOpenSLES.so` | `libroblox` | **implement** | Audio out/in → PipeWire. |
| `libOpenMAXAL.so` | `libroblox` | **stub-inert** (probably) | Linked but likely unused at runtime; confirm before investing. |
| `libjnigraphics.so` | `librenderscript-toolkit`, `libimage_processing_util_jni` | **stub-inert** | 3 `AndroidBitmap*` symbols. Not on the launch path. |
| `libz.so` | `libbacktrace-native` | **implement** | Host zlib satisfies it. |
| `libeigen_blas.so` | `libeigen_lapack` | n/a | Ships in the APK; not a host responsibility. |

**Vulkan is `dlopen`ed, not linked.** `libroblox.so` contains the strings `libvulkan.so`,
**`libvulkan.so.1`**, `VK_KHR_android_surface` and `Unable to load Vulkan API:
vkCreateInstance is NULL`. So GLES2/EGL is the mandatory path and Vulkan is an optional
upgrade — which is the right order to build them in. The presence of the *Linux* soname
`libvulkan.so.1` alongside the Android one is a small gift: the renderer already has a
non-Android loader path.

**Deliberately absent:** no `libvulkan.so` in `DT_NEEDED`, no `libcamera2ndk.so`, no
`libnativewindow.so`, no `libaaudio.so` (audio is OpenSL ES, the older API).

## 3. Framework layer — the JNI boundary

### 3.1 Registration is static, which is the good case

518 of `libroblox.so`'s exports are `Java_*` symbols, plus `JNI_OnLoad`. The Java→native
direction is therefore **resolvable ahead of time by symbol name** — no need to recover a
`RegisterNatives` table at runtime. (`RegisterNatives` appears 6 times in strings; treat
that as a small residue to check, not the main mechanism.)

The reverse direction — native calling *into* Java via `FindClass`/`GetMethodID` — is what
Cordial must actually provide, and it is defined by the Roblox Java classes those 518
natives belong to.

### 3.2 Roblox's own JNI-facing classes

Grouped from [`jni-natives.tsv`](analysis/jni-natives.tsv). Each is a Java class Cordial's
framework layer must be able to instantiate and call.

| Class | On launch path | Notes |
|---|---|---|
| `com.google.androidgamesdk.GameActivity` | **yes** | See §3.3. |
| `com.roblox.client.startup.MainGameActivity` | **yes** | `nativeAppBridgeSetInitParams`, `nativeSetAssetPath`, `nativeRetryInit`, `nativePreloadFlagOverrides`. |
| `com.roblox.client.JNIAAssetManagerSetup` | **yes** | Asset manager handoff; needs a working `AAssetManager`. |
| `com.roblox.client.LocalStorageManager` | **yes** | Persistent storage init. |
| `com.roblox.client.flags.FlagJniInterface` | **yes** | `nativeGetFFlag/FInt/FString`, `nativeRegisterJavaFlagProvider`. **This is the FastFlags mechanism** (spec §9b) and it is a documented JNI interface, not a config file to be reverse-engineered. |
| `com.roblox.universalapp.activitylifecyclecallbacks.JNIActivityLifecycleCallbacks` | **yes** | Native code observes Activity lifecycle. Confirms lifecycle fidelity is required, not optional. |
| `com.roblox.audio.AppRtcDeviceWrapper` | likely | `nativeAudioDeviceChanged` — audio device hotplug. |
| `com.roblox.client.purchase.IAPPurchaseManager` | no | 14 natives. Google Play Billing. **`stub-inert`** — must not crash, need not work. |
| `com.roblox.client.scheduledwork.OtaConfigHandler` | no | OTA config patching. `stub-inert`. |
| `com.roblox.engine.jni.dev.DevBranchAndCommitInterface` | no | Dev-branch switching. `stub-inert`. |

### 3.3 Roblox uses AGDK `GameActivity` — this is the biggest single finding

`Java_com_google_androidgamesdk_GameActivity_initializeNativeCode` is exported, and
`com.google.androidgamesdk.GameActivity`, `GameActivity$a…d`, and
`com.google.androidgamesdk.gametextinput.{InputConnection,State}` are all in the dex.

Roblox's game surface is built on the **Android Game Development Kit's `GameActivity`**,
not on a hand-rolled activity or the older `NativeActivity`. That matters a lot:

- `GameActivity` is **open source under Apache 2.0** (AndroidX Games / `games-activity`).
  Its Java side, its JNI contract, and its input and text-input plumbing are all readable.
  Cordial does not have to infer this interface — it can read it.
- It pins down exactly what the framework layer owes the game: a `SurfaceView`, the
  `SurfaceHolder.Callback` sequence, `Choreographer` frame callbacks, and the
  `gametextinput` IME bridge for the on-screen keyboard (which on desktop should map to
  ordinary host keyboard input).
- It also means the input path is `GameActivity` → `AInputEvent` via `libandroid.so`,
  which is a narrower and better-specified target than the general Android input stack.

**Read the `games-activity` source before writing any of Phase 2.**

### 3.4 Platform classes — `android/*` by package

630 platform classes referenced. Counts by package, with the Phase 2 verdict:

| Package | Classes | Class | Notes |
|---|---|---|---|
| `android/view` | 147 | **implement** | `Surface`, `SurfaceHolder`, `SurfaceView`, `Window`, `WindowManager`, `WindowInsets*`, `Choreographer`, input events. The core of the framework layer. |
| `android/graphics` | 101 | **implement** (partial) | Bitmap/Canvas/Paint. Much is only reached from AndroidX UI paths that desktop never shows. |
| `android/content` | 64 | **implement** | `Context`, `Intent`, `SharedPreferences`, `ContentResolver`. `Intent` is also how `roblox://` deep links arrive. |
| `android/widget` | 60 | **stub-inert** mostly | Classic View widgets, used by AndroidX UI, not by the game surface. |
| `android/app` | 55 | **implement** | `Activity`, `ActivityManager`, `ActivityOptions`, notifications. |
| `android/os` | 54 | **implement** | `Build`, `Handler`, `Looper`, `Bundle`, `Parcel`. `Build.*` is where desktop identification happens. |
| `android/media` | 45 | **implement** (partial) | `AudioManager`, `MediaCodec`. |
| `android/text` | 43 | **desktop-value** / stub | Text layout for AndroidX UI. |
| `android/hardware` | 35 | **stub-inert**, except input | `camera2`, `display`, `input`, `Sensor*`. Sensors → stub; `hardware/input` → real. Camera is only needed for the camera-based features Roblox gates behind permission. |
| `android/database` | 32 | **implement** | `Cursor`/SQLite, used by content providers. |
| `android/util` | 25 | **desktop-value** | `DisplayMetrics`, `Log`, `TypedValue`. Density values decide whether the UI is legible. |
| `android/net` | 22 | **implement** | `ConnectivityManager`, `Uri`. Roblox checks network state. |
| `android/webkit` | 14 | **implement** | See §3.5. |
| `android/credentials` | 12 | **implement** | See §3.6. |
| `android/opengl` | 8 | **implement** | `GLES20`/`EGL14` Java bindings. |
| `android/window` | 4 | **implement** | |
| `android/adservices` | 7 | **stub-inert** | Ads/attribution. |
| `android/bluetooth`, `android/telephony`, `android/nfc`, `android/location`, `android/accounts`, `android/appwidget`, `android/preference`, `android/service`, `android/accessibilityservice` | 17 total | **stub-inert** | Return empty/absent. |

### 3.5 The communities question is answered — and the answer is WebView

Spec §15 asked whether Roblox's communities view is a separate Android `Activity`.
The declared Activities are:

| Activity | Role |
|---|---|
| `com.roblox.client.startup.ActivitySplash` | launch |
| `com.roblox.client.ActivityNativeMain` | app shell |
| `com.roblox.client.startup.MainGameActivity` | the game (GameActivity-based) |
| **`com.roblox.client.RobloxWebActivity`** | **a WebView-hosted activity** |
| `com.roblox.client.ActivityProtocolLaunch` | `roblox://` URI handling |
| `com.roblox.client.captcha.ActivityFunCaptcha` | captcha (WebView) |
| `com.roblox.client.IncomingCallActivity` | voice/call UI |
| `com.roblox.client.NotificationStreamActivity` | notifications |

There is **no communities-specific Activity**. Communities is web content, and it opens in
`RobloxWebActivity` — a separate Activity hosting an `android.webkit.WebView`. That is
exactly why it surfaces as a separate window on desktop: a second Activity, launched
without the window management that would keep it inside the app.

So the fix in spec §4.2 is two things, not one:

1. **Activity + window management** so a second Activity is composited into the existing
   window rather than becoming an independent top-level surface. This is the actual
   §4.2 item.
2. **A WebView implementation.** `android.webkit.WebView`, `WebViewClient`,
   `WebChromeClient`, `WebSettings`, `CookieManager`, `JavascriptInterface`,
   `PermissionRequest`, `WebResourceRequest`. Sober solves the analogous problem with
   WebKitGTK, and Cordial will need the same. **This is a substantial Phase 2 workstream
   that the architecture spec does not currently name anywhere.** It is also on the login
   path (captcha is a WebView), so it is not optional.

### 3.6 Passkeys — harder than §4.3 assumes, and worth confirming early

The spec's §4.3 reference flow assumes Roblox calls
`androidx.credentials.CredentialManager` and that Cordial's JNI stub can intercept it.
What is actually in the APK:

- **`android.credentials.CredentialManager`** — the *platform* API (API 34+): 12 classes
  including `GetCredentialRequest$Builder`, `CredentialOption$Builder`,
  `GetCredentialResponse`, `GetCredentialException`.
- **`androidx.credentials.playservices.*`** — 33 classes: `CredentialProviderController`,
  `CredentialProviderBeginSignInController`, restore-credential controllers. This is the
  AndroidX **Google Play Services backend**, which ships inside the APK and brokers to
  Play Services on a real device.
- `android.permission.USE_BIOMETRIC` and `USE_FINGERPRINT` are requested.

So there are two possible interception points and they have very different costs:

- **Platform path** (`android.credentials.CredentialManager`) — Cordial implements the
  framework class directly and dispatches to libfido2 / `xdg-desktop-portal`. This is the
  §4.3 flow and it works.
- **Play Services path** (`androidx.credentials.playservices.*`) — the AndroidX shim calls
  into GMS, which does not exist on Linux. Cordial would have to satisfy a Play Services
  interface instead of a documented platform one.

AndroidX `CredentialManager` prefers the platform API on API ≥34 and falls back to the
Play Services controller below that. Roblox targets SDK 35, so **the platform path should
be the live one** — but this depends on what Cordial reports as its API level, and it is
exactly the kind of assumption that costs a week if wrong.

**First Phase 2 task: confirm which path executes**, by instrumenting the call rather than
reasoning about it. Report API 34+ from the framework layer to force the platform path.

### 3.7 Permissions Roblox requests — a triage shortcut

From `dumpsys package`. Anything here that is not on the launch path is a `stub-inert`
candidate, and most of it is.

**Must work:** `INTERNET`, `ACCESS_NETWORK_STATE`, `ACCESS_WIFI_STATE`,
`MODIFY_AUDIO_SETTINGS`, `WAKE_LOCK`, `POST_NOTIFICATIONS`.

**Map to a portal when the feature is used:** `RECORD_AUDIO` (voice chat), `CAMERA`
(face tracking), `USE_BIOMETRIC`/`USE_FINGERPRINT` (passkeys — see §3.6).

**Stub inert:** `com.android.vending.BILLING`, `com.google.android.gms.permission.AD_ID`,
`ACCESS_ADSERVICES_*`, `READ_CONTACTS`, `VIBRATE`, `DETECT_SCREEN_CAPTURE`,
`DISABLE_KEYGUARD`, `USE_FULL_SCREEN_INTENT`, `RECEIVE_BOOT_COMPLETED`,
`READ_BASIC_PHONE_STATE`, `com.google.android.c2dm` (FCM push),
`com.google.android.finsky.*` (install referrer), and the Samsung/Huawei store
permissions.

**Note `DETECT_SCREEN_CAPTURE`** (with `android.app.Activity$ScreenCaptureCallback` in the
dex). Roblox asks to be told when the screen is captured. It is a callback, not an
enforcement mechanism, and stubbing it inert is correct — but it is worth knowing it is
there rather than discovering it later.

## 4. Suggested Phase 2 order

Derived from the above, cheapest-useful-first:

1. `liblog` → Cordial's log. Trivial. Spec §9a said `onLogLine`, `onJoin` and `onLeave`
   would be built from it; none of the three was, and the log parsing that does
   exist reads the engine's own log file instead — see `bloxstrap_rpc`.
2. `Build.*` + `__system_property_get` + `DisplayMetrics` — desktop identification. Small,
   visible, and unblocks judging what else changes when Roblox stops thinking it is a
   phone.
3. `GameActivity` + `SurfaceView`/`SurfaceHolder` + `Choreographer` — the minimum to get a
   frame on screen. Read `games-activity` first.
4. `libandroid.so` input path → `AInputEvent`, plus `gametextinput`.
5. `FlagJniInterface` — FastFlags, which is a whole spec feature for very little work.
6. Activity/window management → the communities-window fix (§3.5 item 1).
7. WebView (§3.5 item 2). Big. Login and captcha depend on it.
8. `CredentialManager` platform path → passkeys (§3.6).
9. Everything in the `stub-inert` column, in bulk.

## 5. Before implementing any of this

Read [`minecraft-linux/libjnivm`](https://github.com/ChristopherHX/libjnivm) and
`fake-jni` first — MIT-licensed working implementations of the JNI boundary and framework
stubbing for a different Android app, by authors Sober's own attribution notice credits.
See [`findings.md`](findings.md) §4. A large share of §3.4's `stub-inert` and
`desktop-value` rows plausibly already exist there.

## 6. Reproducing this

The APK is not committed (see `.gitignore`). To regenerate from a Waydroid instance with
Roblox installed:

```bash
adb connect 192.168.240.112:5555
adb shell pm path com.roblox.client            # base.apk + split_config.x86_64.apk
adb pull <path>/split_config.x86_64.apk
unzip -o split_config.x86_64.apk 'lib/x86_64/*' -d native/

for so in native/lib/x86_64/*.so; do
  readelf -d "$so" | grep NEEDED
  readelf --dyn-syms -W "$so" | awk '$7=="UND" {print $8}'      # runtime surface
  readelf --dyn-syms -W "$so" | awk '$8 ~ /^Java_/ {print $8}'  # JNI natives
done

unzip -o base.apk '*.dex' -d dex/
cat dex/*.dex | strings -a \
  | grep -oE 'L(android|androidx|com/google/android/gms)/[A-Za-z0-9/$_]+;' | sort -u
```

Dex type descriptors are stored as plain strings, so `strings` recovers the referenced
class list without needing `apktool` or a JVM. Method-level detail does need a real
decompiler; nothing above required one.
