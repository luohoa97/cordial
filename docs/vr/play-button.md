# Play: who answers `Game.launch`

**Status:** fixed under `--app-bridge`. Verified by deep link on Monado's
simulated HMD, 2026-09-29, Quest build 2.740.927, `cordial-run --guest-arm64
--app-bridge`, signed in; and by pressing Play in a Quest 3 over WiVRn, in
three sessions, each of which joined (see "In the headset" below).

## The symptom

Pressing Play on a game page showed a loading screen for about half a second,
then nothing. With `CORDIAL_GUEST_JNI_LOG=1` there was no Java call, no engine
FLog line, no guest stop and no newly called import after
`APP_READY ExperienceDetail`. The engine had asked for something and nothing
answered.

## What Android does

Found from the Quest APK's dex call graph (which methods invoke which, method
names and prototypes only; no method body was read beyond its invoke targets)
and the manifest. The classes and methods in between are obfuscated and
renamed from build to build, so only the stable names are given here:

1. `ActivityNativeMain`, in the same method that calls
   `nativeAppBridgeStartLuaAppDM`, subscribes a listener to the name
   `JNIExperienceProtocol.getLaunchId()` returns through `MessageBus`: a
   subscription to `Game.launch` (the getter returns `"Game.launch"` on this
   build too), made before the Lua app starts.
2. The listener reads the payload with `JSONObject.optLong`/`optString`.
3. It is handed to the game controller, which posts it with
   `Activity.runOnUiThread` and builds `new ExperienceSession(Bundle)`.
4. The session's game object calls
   `NativeGLInterface.nativeAppBridgeV2StartGameWithParam(StartGameParams)I`.
   That is the only caller of that native in the dex.

No `startActivity` appears on that chain; the session runs on
`ActivityNativeMain`'s UI thread. The manifest's only other game Activity is
`MainGameActivity` (AGDK, not exported, `singleTask`), and nothing on the chain
names it. So the transition seen on a real Quest is **INFERRED** to be
`ExperienceSession`'s own, not a second Activity. `docs/traces/` has no join
to check this against: its one capture is signed out and ends at `Landing`.

Sober, running the phone build 2.737 on this host, logs the same native at the
start of a join (observed in its own FLog, `appData/logs/`):

    [FLog::JNIAppBridge] nativeAppBridgeV2StartGameWithParam:
    [FLog::SingleSurfaceApp] launchUGCGame: (stage:Native).

## Where it broke

Cordial is the Java side and nothing subscribed to `Game.launch`. A deep link
isolates it without the headset:

    --join-url 'roblox://experiences/start?placeId=1818'

The app shell publishes `Game.launch`
(`{"placeId":1818,"referralPage":"DeepLink","joinAttemptId":...}`), exactly as
on the phone build, and then nothing: no `launchUGCGame`, no join, 90 s.

Upstream Cordial's phone build under AGDK joins from the same publish with no
subscriber (docs/analysis/app-bridge.md §9, 8/8 Servers-list joins), so there the engine answers
`Game.launch` itself. Why it does not here is **INFERRED** to be the bring-up:
under `--app-bridge` the engine is in `ActivityNativeMain`'s arrangement and
leaves the launch to Java.

## The fix

`crates/cordial-runtime/src/game_launch.rs`, armed only under `--app-bridge`,
subscribes to `Game.launch` through `doSubscribeRaw`. The looper then builds a
`StartGameParams` (`native/init_params.cpp`) and calls
`nativeAppBridgeV2StartGameWithParam`:

- payload keys that match a `StartGameParams` accessor by name are carried;
  any other key is named in the log and dropped;
- a missing string is `""` and a missing number `0`, the documented defaults
  of the `optString`/`optLong` the Java uses (**INFERRED** that it passes them
  on unchanged);
- identity comes from the source `StartAppParams` uses; device, platform,
  surface and `vrContext` are the same objects the app half gets.

`CORDIAL_NO_GAME_LAUNCH=1` is the control.

| Run | Arm | `StartGameWithParam` | `launchUGCGame` | `Joining game ... place 1818` |
|---|---|---|---|---|
| dl-join-1 | before the change | not called | 0 | no |
| ctl-nolaunch-1, -2 | control | not called | 0 | no |
| dl-join-fix-1, -2, -3, -shot | fix | `-> 1` | 2 | yes, then `gameLoadedCallback: place 1818` |

The engine read 23 of the 24 accessors, once each, in all four runs; it never
asked for `deviceParams`. `joinRequestType` went in as 0,
because the deep link payload carries none, and the engine joined through a
public server regardless. The left eye 38 s in shows Crossroads with the VR
HUD, chat and leaderboard.

## In the headset

Play pressed in a Quest 3 over WiVRn (`system "Meta Quest 3 on WiVRn"`), in
three sessions. Each published `Game.launch`, and each joined:

    [ 131.63] [launch] Game.launch arrived: placeId 11256291667, carrying [joinAttemptId, joinAttemptOrigin, placeId]
    [ 138.29] [roblox] gameLoadedCallback: place 11256291667

and 103.42 s / 106.84 s for place 7406004869, 112.80 s / 123.16 s for
11256291667 again. These were Play presses and not deep links: a Play press
carries `joinAttemptOrigin`, where a deep link carries
`referralPage: "DeepLink"`. These lines show a first join in each session;
leaving by the in-game button and joining again in the headset are not shown
by them and stay open, below.

## Still open

- **Leaving.** The devctl verb `leavegame` calls
  `nativeAppBridgeV2LeaveGame` from the looper, as Java does. The
  engine leaves (`leaveUGCGame`, disconnect reason 285) and waits at stage
  `Native` with its surface controller stopped; XR frames stopped and the
  menu never came back until the session's end also called
  `nativeAppBridgeV2StartAppWithParams` again (`game_launch::end_session`),
  whose engine side is `returnToLuaApp`. Measured on Monado: menu back and XR
  frames running at about 55 a second in 4 of 4 runs with it, frozen in 3 of 3
  without it (`CORDIAL_NO_APP_RESTART=1`, `CORDIAL_NO_GAME_LIFECYCLE=1`, and
  the build before). On Android its callers are the app view's
  `surfaceCreated` and the app fragment's hidden-changed override, so that
  Java makes it on every leave is
  **INFERRED**. The in-game Leave button is a different path: the engine
  returns to the Lua app itself before `gameDidLeave` (seen once over WiVRn),
  so `game_launch::left` does not restart the app; that path has not been run
  since the `mprotect` fix.
- **Joining again after a leave.** Fixed in Cordial's Vulkan and OpenXR
  layers; see "Joining again" below. Still open: on Monado's default
  in-process compositor the client still dies in NVIDIA's driver after the
  engine's `xrDestroyInstance`, which is a test-rig problem, not the engine's.
- **A stop about 60 s into a game.** A guest thread stopped on a raw
  `svc #0` (`Fault::Syscall`) about 67 s after joining, and the process
  aborted.
  *Fixed*: raw syscalls are now translated; it was `openat` of
  `/proc/self/maps` and the reads after it (dynarmic-design.md §9.6).

## Joining again

**Status:** join, leave, join, leave runs on Monado with
`XRT_COMPOSITOR_NULL=1`, 3 of 3, the second game in the left eye and the
menu back after each leave. Not yet run in the headset.

**Why the engine tears VR down.** The second `launchUGCGame` takes
`pauseLuaAppAndDestroyIfNeeded destroySurfaceView:true` and the first does
not. What decides that is not established, but three things were ruled out:
it is not how long the menu has run (a first join published 40 s in takes
the single-surface path), not the game surface (redelivering
`UpdateSurfaceGame` after the leave changes nothing), and not the session
pause and fragment stop at the leave (skipping them changes nothing). The
phone build takes the same branch on a Play-button join from a running menu
(upstream `docs/NEXT.md`), so it is the engine's own path, and the
headset's first Play may take it too (INFERRED). So the fix makes the
teardown survivable rather than steering around it. On this branch the
engine destroys the eye swapchain, the window swapchain and the
`XrInstance`, then creates a new instance, Vulkan instance, device and
session.

**Two faults, each fixed where it happens:**

1. `vkDestroyImageView` arrives twice for every view of the eye swapchain's
   images, from two call sites, with nothing in between. NVIDIA faults on
   the second. `guest_vk::live` tracks the image views and swapchains the
   engine creates, and does not forward a destroy of one it has already
   destroyed. It logs `already destroyed; not passed to the driver`. A
   destroy returns nothing, so the engine cannot see the difference.
   `CORDIAL_NO_VK_LIVE=1` is the control.
2. After the new session, the engine calls `xrRequestDisplayRefreshRateFB`
   through the pointer it fetched under the *first* instance. The desktop
   loader had unloaded the runtime with that instance and loaded it again
   elsewhere, so the call landed in unmapped memory at the old base plus
   `0x3e080`, which is `oxr_xrRequestDisplayRefreshRateFB` in
   `libopenxr_monado.so`. `xr_runtime_pin` keeps the runtime library loaded
   (`RTLD_NODELETE`) from the first instance on. `CORDIAL_NO_XR_PIN=1` is
   the control.

| Run (null compositor) | Build | Result |
|---|---|---|
| rj-pass-1, -2, -3 | fix | join, leave, join (`gameLoadedCallback: place 1818` twice), leave; XR frames running throughout; menu, game, menu in the left eye |
| rj-ctl-vr | `vr` at c12d684 | SIGSEGV right after `xrDestroySwapchain` |
| rj-ctl-nolive | fix, `CORDIAL_NO_VK_LIVE=1` | SIGSEGV right after `xrDestroySwapchain` |
| rj-ctl-nopin | fix, `CORDIAL_NO_XR_PIN=1` | SIGSEGV at old Monado base + `0x3e080` |

**Monado's default compositor still crashes, and the cause is outside the
engine.** With the brief's environment (`SIMULATED_ENABLE=1
XRT_COMPOSITOR_COMPUTE=0`, no `XRT_COMPOSITOR_NULL`), the engine's
`vkDestroySwapchainKHR` on the window swapchain calls a null function
pointer inside `libnvidia-glcore` (595.91.07). A hardware watchpoint on
that pointer shows two writes. GTK's renderer sets it to
`wl_proxy_get_version` when the window realises. Monado clears it inside
the engine's `xrDestroyInstance`, at `compositor_destroy` ->
`vkDestroyInstance` of Monado's own in-process compositor instance, which
enables `VK_KHR_wayland_surface`. After that, every Wayland WSI call in the
process faults. The null compositor creates no surface, and there the same
sequence survives.

A runtime whose compositor is in another process, as WiVRn's is, never
destroys a Vulkan instance inside Cordial, so it should not hit this
(INFERRED, not run). Two re-arm attempts did not restore the pointer: a
fresh host `VkInstance` with the Wayland surface extension, and
`eglInitialize` on a new Wayland connection. `XRT_COMPOSITOR_FORCE_XCB=1` is
no substitute, because the runtime then refuses the next instance
(`XR_ERROR_LIMIT_REACHED`) and the engine runs flat.

**The in-game Leave button** (`gameDidLeave` -> `game_launch::left`) has not
been run. Both fixes sit below the engine, in the Vulkan and OpenXR layers.
They act on the teardown whichever way the menu was reached. That this
path's rejoin takes the same teardown is INFERRED.

