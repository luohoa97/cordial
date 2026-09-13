# Status: experimental, but playable

Sign in, load a game, move around. This is the full feature table that used
to live in the README, what changed recently in this fork, and three of the
harder bugs it took to get here. See also [`CHANGELOG.md`](../CHANGELOG.md)
and the [releases page](https://github.com/luohoa97/cordial/releases) for
what changed release by release.

## Recent desktop/runtime improvements in this fork

- **Reliable mouse capture on Wayland and X11.** Right-drag and first-person
  camera control now constrain the desktop cursor to the gameplay window. On
  Wayland the constraint is attached to GTK/GDK's real pointer and to the
  toplevel surface, which fixes the cursor escaping on KWin while Roblox's
  internal pointer remained centred. Relative, unaccelerated motion and side
  mouse buttons are carried through the Android input bridge as well.
- **Visible text entry.** A native GTK overlay mirrors the focused Android text
  field, including caret movement, editing operations and Wayland IME preedit,
  so typed characters no longer remain invisible until focus is lost.
- **Web-view bridge.** Marketplace, Profile and Communities continue to use a
  signed-in WebKitGTK view, and both Roblox bridge formats (`executeRoblox` and
  `RobloxWKHybrid.command`) are forwarded to the engine. The Vulkan canvas is
  lowered while a dialog or text overlay is visible and restored on close.
- **Fullscreen on the gameplay window.** F11 now targets the window containing
  the engine, hides the compact header bar and persists the choice per profile.
  The header uses the desktop's libadwaita/KDE theme colours instead of a
  transparent custom background.
- **Lower Android-runtime overhead.** Pointer positions use atomic pairs;
  ordinary Vulkan presents no longer contend on the screenshot mutex; looper
  accounting runs only when instrumentation is enabled; unchanged text avoids
  repeated cloning and GTK updates; and environment/configuration probes used
  by hot paths are cached for the process lifetime. These are runtime changes,
  not Roblox graphics options or FastFlags.

## The feature table

| | |
|---|---|
| Loads `libroblox.so` natively | ✅ |
| Warm start | ✅ the engine is extracted once and reused; only a new Roblox build re-extracts |
| App shell reaches `APP_READY (Landing)` | ✅ |
| Renders — Vulkan on both backends | ✅ |
| Networking / HTTPS | ✅ |
| **Signing in** | ✅ **via Quick Sign-in**, which is a code flow and needs no typing |
| **Keyboard in an experience** | ✅ WASD, space, the lot |
| Mouse: navigation, buttons, field focus | ✅ |
| Mouse: turning the camera | ✅ right-drag, and the delta is the compositor's *unaccelerated* one — using the accelerated pair made sensitivity depend on your desktop mouse settings and made the camera speed up through a fast sweep |
| Scroll wheel | ✅ |
| Frame rate | ✅ a flat 60 on MAILBOX, where FIFO gave a variable 35–50 |
| Feral GameMode | ✅ registered while the client runs |
| Typing into text fields | ✅ a GTK overlay draws focused Android fields live, including caret movement and Wayland IME preedit |
| Pointer capture in first person | ✅ the cursor stays in the window, reported from real play |
| Staying signed in across a restart | ✅ cookies and identity kept in the **desktop keyring**, not a file |
| Loading into an experience | ✅ world, avatar and UI render, signed in |
| **Two accounts at once** | ✅ two profiles, two instances, side by side — see below |
| Window — libadwaita header bar, engine as a subsurface | ✅ |
| Launching from the shell | ✅ finds a build, or explains how to get one |
| Choosing a profile | ✅ a chooser above the Launch button; creates one, and shows a profile another window holds as unavailable |
| Audio | ✅ sound in an experience, reported from real play; the OpenSL ES bridge into PipeWire was measured with a control before that |
| Web views (Marketplace, Profile, Communities…) | 🟡 they render in a real signed-in WebKitGTK window, with correct canvas stacking; both observed JavaScript bridge formats now reach the runtime, but more pages still need interactive coverage |
| **Asset overlays** (custom textures, sounds, fonts) | ✅ drop a file mirroring the APK's `assets/` tree into `~/.config/cordial/overlay` and it is served instead; nothing is modified, remove the file and the original returns |
| Fullscreen | ✅ F11 acts on the gameplay window, hides the compact themed header bar and persists per profile |
| Getting the cursor back | ✅ **The same way you would in any other game.** Roblox takes the cursor when it wants it and gives it back when it does not — pressing Escape opens Roblox's own menu, which releases it. Your compositor's own escape (Super, an overview, a workspace switch) always works and Cordial cannot take it away: the lock is a `zwp_locked_pointer_v1` and breaking it is the compositor's decision. `CORDIAL_NO_POINTER_LOCK=1` turns capture off for a whole session |
| **The engine's content store** | ✅ `RbxStorage` initialises and is read back — a real SQLite database, the engine's own `files` table, eight engine-created partitions, and cache hits rising across launches. Assets are no longer refetched every session |
| Clean shutdown | ✅ full pause/stop/destroy sequence, observed in the engine's own log |
| Plugins | 🟡 host, broker and per-profile grants now enforce every capability, not only `flags.*`/`presence.*` as before — notify, url.open, asset overlays, `flags.write` and cross-plugin events all reach a real effect; Settings can grant or revoke a capability, and install or remove a plugin from a local `.tar.zst` archive; still no in-app fetch from a remote index, so the marketplace half of the registry is unbuilt |

Frame rate measured with pointer motion driven for the whole run, because
presents drop to exactly 1/s when nothing is happening and every earlier figure
in this repository was that idle throttle integrated: a flat 60.0 on MAILBOX
against a variable 35–50 on FIFO, four runs of 120 s.

**What is left is polish and broader live coverage.** Focused text fields now
have a desktop overlay, and web views forward both bridge formats observed in
Roblox pages. Those paths still need testing across more field types, input
methods and web pages; a page-specific bridge command can still expose engine
vocabulary Cordial has not observed yet.

Pointer capture and the content store were both on this list and are not any
more.

## Three of the harder bugs

**The content store took fifty attempts and forty-six sections, and the answer
was a call made too late.** The engine wants `nativeSetCacheDirectory` before
`GameActivity.initializeNativeCode`, not after it. That is the whole of it.

The paragraph that stood here described a different mechanism — init running
during the engine's ELF constructors and memoising a failure — and it was
wrong. So were several of the explanations before it. Nearly every scoring
method used along the way turned out to be measuring something else: a log
channel believed silent that is not, a marker that fires in working runs too,
and an ordering signature that could not have come out any other way. The
corrections are in [`docs/analysis/flag-init.md`](analysis/flag-init.md)
§41 onwards, and they are more useful than the fix.

The store is verified rather than assumed: three runs producing a database
against a control producing none, and hit counts rising on a second launch
against the same profile.

**The keyboard took a week and the answer was one number.**
`nativePassKeyEvent` wants Linux evdev codes; it was being handed Android
keycodes. Exactly one key worked — `D`, because `AKEYCODE_D` and `KEY_D` are
both 32 — and Alt made the character jump, because `AKEYCODE_ALT_LEFT` is 57 and
so is `KEY_SPACE`. Four theories were measured and disproved first, every one of
them assuming a number was wrong somewhere. The numbers were fine; the
vocabulary was.

**Two accounts at once, and it was not built as a feature.** A profile is
storage and an instance is a window ([ADR-012](adr/ADR-012-profiles-and-instances.md)),
with an `flock` so one profile cannot be opened twice — which leaves nothing
stopping two *different* profiles running side by side, each with its own
session, settings and plugin grants. On Windows this traditionally needed a
second desktop session. Each instance is a whole engine, so budget around 1.5 GB
of memory apiece.

**Install it expecting rough edges.** It plays; it is not finished.
