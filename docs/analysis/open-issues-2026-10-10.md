# Cordial open issue audit — 2026-10-10

This is a point-in-time reading of the 25 **open** GitHub issues and their
105 public discussion comments, fetched from the GitHub API on 2026-10-10.
It is not a declaration that any issue is closed, or that a proposed fix has
been reproduced on the original reporter's setup.

The repository rules in `AGENTS.md` and `CONTRIBUTING.md` govern all
follow-up work: check Android traces before inferring engine behaviour; check
Sober's issue corpus for user-visible symptoms; use real measurements and
controls; label unverified explanations **INFERRED**; do not ship Roblox code or
in-process execution capabilities.

## Issue-by-issue register

| Issue | Latest evidence / present status | Next verification or deliverable |
|---|---|---|
| [#101](https://github.com/luohoa97/cordial/issues/101) | Requests a configurable FPS cap beyond 240. The launcher only offers measured presets; existing code cites a report that even a 1000 setting stopped at 240. | Reproduce the actual upper limit on a high-refresh monitor before promising an unlimited setting. |
| [#99](https://github.com/luohoa97/cordial/issues/99) | F11 reportedly freezes on one NVIDIA configuration. Maintainer and another reporter could not reproduce on different configurations; 0.27.0 may have helped. | Compare compositor, driver, backend, fullscreen transitions, and live present counts on the affected machine. |
| [#98](https://github.com/luohoa97/cordial/issues/98) | Persistent Vulkan microstutter on KDE/RADV; original reporter says 0.27.0 did **not** fix it. Sober and Cordial differ on the same environment. | Collect frame-time/present-mode and idle-input traces from both configurations; a frame cap or present-mode guess alone has not fixed it. |
| [#97](https://github.com/luohoa97/cordial/issues/97) | Real cursor movement on shift lock was reported on an old KDE Wayland build; maintainer points to pointer-lock change shipped in 0.23.1. | Obtain confirmation after upgrading on KDE. |
| [#94](https://github.com/luohoa97/cordial/issues/94) | TextBox font slanted/bold in a specific game. Source Sans fallback was added for 0.27.0 but reporter says the symptom persists; disabling font-slot choice stops the slant at the cost of the wrong face. | Capture the reported font slot, face resolution and GTK overlay styling; compare against the expected game text. |
| [#92](https://github.com/luohoa97/cordial/issues/92) | Intermittent signed-in startup freeze. Reports distinguish frozen and healthy render-lifecycle logs; the prior stalled-network explanation was withdrawn. | Repeat matched cold/warm starts and capture engine scheduler state and request callbacks using the supplied tools. |
| [#88](https://github.com/luohoa97/cordial/issues/88) | Startup failure on NixOS/Mango; an incorrect Vulkan-driver diagnostic has been fixed, but that was not confirmed as the startup cause. | Get terminal launch log and check `GSK_RENDERER=cairo` on the affected compositor. |
| [#87](https://github.com/luohoa97/cordial/issues/87) | Large camera jump during fast mouse motion, possibly related to Hyprland pointer capture. Pointer-lock changes shipped in 0.26.0; actual affected-machine outcome remains unclear. | Compare pointer-lock confirmation and relative motion under `CORDIAL_TRACE_MOUSE=1`. |
| [#86](https://github.com/luohoa97/cordial/issues/86) | Two symptoms: R6 character arms fail to animate and typed chat is invisible. Reports do not yet establish a common cause. | Split the reproductions; record diagnostics and renderer-control result for text, and game-specific animation details separately. |
| [#85](https://github.com/luohoa97/cordial/issues/85) | ARM64 Switch: earlier two-minute mutex crash fixed by 0.24.0; on 0.25.0 a different crash occurs shortly after joining when the camera turns. | Analyze full new crash-stop backtrace / GPU context on Switch hardware. |
| [#83](https://github.com/luohoa97/cordial/issues/83) | Authentication challenge window completes verification, but sign-in does not resume. Window creation alone is not sufficient. | Trace challenge web-view result / JS bridge and its delivery to the engine, without collecting credentials. |
| [#71](https://github.com/luohoa97/cordial/issues/71) | Experimental Nintendo Switch Linux support. ARM64 packages exist and a tester reached Home and joined a server, but #85 has further crashes. | Continue cross-device ARM64 compatibility matrix after #85 is diagnosed. |
| [#63](https://github.com/luohoa97/cordial/issues/63) | Uncommon libroblox.so SIGSEGV just after engine initialization (2/11), followed by many runs without recurrence. | Preserve fault offset and use a repeated control and crash capture rather than inferring cause from the offset. |
| [#61](https://github.com/luohoa97/cordial/issues/61) | `DFIntTaskSchedulerTargetFps` reverts after a settings refresh. Reapplication shipped from 0.23.1. | Reporter confirmation that the flag remains effective past five minutes and across joins. |
| [#56](https://github.com/luohoa97/cordial/issues/56) | Hyprland/sway pointer lock and cursor drift. Toplevel lock and relative-motion routing changed in 0.26.0. | Confirm both compositor lock and sustained movement with the reporting users; test drag/focus boundaries. |
| [#55](https://github.com/luohoa97/cordial/issues/55) | Long-running FFI-safety refactor across ~350 entry points, explicitly **not** intended as one mechanical sweep. Three call sites already share helpers. | Make separate, reviewed, trace-verified patches by file; preserve each ABI exactly. |
| [#53](https://github.com/luohoa97/cordial/issues/53) | Grey-screen and TextBox focusing changes were shipped through 0.27.0. On 2026-10-09 the reporting user replied **“Yup, it's fixed.”** | Maintainer can consider closing after checking any distinct Hyprland case. Do not rewrite the already-working fix. |
| [#41](https://github.com/luohoa97/cordial/issues/41) | X11 180-degree warp fixed from 0.16.0; key-release-on-focus-loss fix present from 0.23.0, per maintainer correction. | Re-test both symptoms on an affected X11 session and obtain reporter confirmation. |
| [#39](https://github.com/luohoa97/cordial/issues/39) | Enter-fullscreen freeze addressed via coalesced resizes from 0.16.0; later title-bar/fullscreen fixes shipped with 0.27.0. Another reporter can F11 repeatedly without a crash. | Seek original GPU/driver information and repeat exit/fullscreen case; do not conflate distinct faults. |
| [#38](https://github.com/luohoa97/cordial/issues/38) | No launcher window on certain non-Mutter Wayland setups. Earlier generalizations about all KWin machines and GTK blame were explicitly retracted. | Run compositor- and renderer-controlled tests with protocol trace and live CPU/backtrace. |
| [#35](https://github.com/luohoa97/cordial/issues/35) | Steam Deck startup SIGSEGV. The named `DeviceUtils` warning was shown on **healthy runs** and is not a demonstrated crash cause. | Get `coredumpctl` data and whether a touchscreen was involved; use a real crash site instead of the log's red herring. |
| [#33](https://github.com/luohoa97/cordial/issues/33) | Session recording is a new shell-native feature, distinct from the Android client's unimplemented Record button and from face-tracking camera API. | Design a user-consented swapchain capture → encoder → chosen file pipeline with bounded resource use. |
| [#29](https://github.com/luohoa97/cordial/issues/29) | Movement sometimes dead after spawning although keys reach Roblox. Reporter reproduced 6/6 good joins with pointer motion and 3/3 bad when switching away; later notes suggest a join-time pointer-keepalive change awaiting confirmation. | Compare release code against controlled join experiments, no-input vs real/synthesized pointer motion, and assert movement in a live game. |
| [#13](https://github.com/luohoa97/cordial/issues/13) | Research task: `FLog`/`DFLog` channels require different value syntax. A bad format can silence logging. 724 channels were counted in a specific binary; the live settings snapshot consulted in this review exposed only 139 entries. | This branch adds `tools/flog_channel_shapes.py`: an evidence-labelled lookup with optional override-format warnings. It does **not** map all 724 binary declarations or warn automatically at runtime; continue those parts separately. |
| [#2](https://github.com/luohoa97/cordial/issues/2) | The User-Agent half was fixed upstream. The base URL lacked the trailing slash observed in the Android trace. This branch now changes it and includes a red-to-green source regression guard. | Run signed out to Landing and verify first live HTTP requests still succeed; do not claim that a source-level match proves the live networking behaviour. |

## Implementation slices

1. **Low-risk contract alignment:** #2, verified by a trace-based test, then live navigation. The source patch is present on the current branch.
2. **Existing fixes awaiting reproduction:** #53, #97, #61, #41, #39, #56 and relevant parts of #87/#99. Confirm on reporter-equivalent hardware, rather than introducing speculative changes.
3. **Input, compositor and runtime stability:** #29, #38, #92, #98, #94, #83, #85, #35, #63 and the remaining symptoms of #86/#88. Each needs one independent issue-level hypothesis, a failing reproduction, a control and repeated measurements.
4. **Features and research:** #13, #33, #55, #71, #101. These need dedicated designs and bounded commits; several cannot be called complete from unit tests alone.

## Initial #13 diagnostic (partial, observed channels only)

The standalone lookup can read either the cached settings JSON or the CDN's
`applicationSettings` wrapper. Its output is a tab-separated table of channel,
observed value syntax, and the observed value. Unseen channels are `unknown`,
not labelled numeric just because most channels are numeric. Supply a
`flags.json` for advisory warnings about formats that differ from known entries.
It does not modify flags and does not claim to know every channel declaration.

```sh
curl -fsSL https://clientsettingscdn.roblox.com/v2/settings/application/GoogleAndroidApp > /tmp/roblox-settings.json
python3 tools/flog_channel_shapes.py --settings /tmp/roblox-settings.json --channel FLogAudio --channel FLogNetwork
python3 tools/flog_channel_shapes.py --settings /tmp/roblox-settings.json --flags /path/to/flags.json
python3 tools/test_flog_channel_shapes.py
```

## Verification environment and caveats

The October 10 Studio workspace is Linux x86-64 without a signed-in Roblox
profile or the specific GPUs/compositors reported in the issues. Its Rust
toolchain is 1.99.0; the native dependency packages and recursive submodules
have been installed. `cargo test --locked` has passed locally, including
under `umask 0077`, after making two archive test assertions reflect their
intended permission invariants. `cargo build --locked --release` completed
successfully but reported unrelated warnings. A global `cargo fmt --check`
currently reports thousands of baseline differences and was not followed by
a mass reformat. The game was **not** launched in this session.

No GitHub issue was closed and no code was pushed by this audit.
