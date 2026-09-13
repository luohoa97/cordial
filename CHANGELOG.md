# Changelog

What changed between releases, and what was measured rather than assumed.

Entries here follow the same rule as commit messages and pull requests: a claim
that was tested says what it was tested with, and a claim that was not is marked
`INFERRED`. Several entries are retractions of earlier claims, because that is
what the history contains and hiding it would make the rest less trustworthy.

The window title is `Cordial <version> (<commit>)` -- the version from
`Cargo.toml`, the commit from `git rev-parse --short=9`, stamped at compile
time by `crates/cordial-shell/build.rs`. A release reads `Cordial 0.14.0
(c572124a1)`; a build from a source drop with no git reads `Cordial 0.14.0`,
and `-dirty` on the commit means the tree had uncommitted changes.

**This used to say `git describe --tags`, and that was wrong** -- it made the
version and the commit two spellings of one fact when they are two facts, and
a tree whose manifest said 0.11.0 displayed the *previous* release's number.
See `crates/cordial-shell/src/version.rs`.

**Entries here stop at 0.6.0.** Everything from 0.7.0 onward is written up in
`docs/releases/`, one file per release, addressed to somebody who has just
installed that version rather than to whoever wrote it. This file is kept for
the history it holds, not because it is current.

**There was never a 0.1.0.** The first version this project gave itself was
0.2.0, in `8db7100`, once there was something a person could sign into. The 178
commits before that were the bionic linker port, the JNI layer and the framework
work, and none of them were released.

## 0.6.0 — 2026-08-20

**The embedded web view is integrated in both directions.** Opening a web
window worked in 0.5.2's successor commits; using one did not, and the gap was
narrow and specific.

`openWindow` already reached a dialog. Three things were missing and are here:

- **Bridge messages now reach the engine.** Pressing Join in the Servers window
  opened the game's detail page inside the same dialog instead of joining. That
  read as a WebKit navigation bug and was not: the page posts its command to
  `executeRoblox`/`RobloxWKHybrid`, nothing answered, and a page whose host is
  not listening falls back to ordinary navigation. `signalJavascriptCallback`'s
  shape is read from the dex's method table — `(Ljava/lang/String;)V` — rather
  than guessed, so the argument list carries no risk.
- **The engine is told when a window closes.** It blanks its own canvas when it
  opens a web window, expecting to be covered. Nothing was telling it the cover
  had gone, so the canvas stayed blank underneath a correctly stacked, visible
  dialog. Lowering the subsurface was necessary and not sufficient.
- **`setWebviewUserAgent` gets the identical string `InitParams.userAgent` was
  built with**, via `cordial_build_user_agent`, so the view and the engine
  cannot drift apart into two answers about what this client is.

Measured: builds with both webview features, `readelf -d` confirms
libwebkitgtk-6.0.so.4 actually linked, 608 tests across 19 suites, and all four
natives are exported by 2.734.0.917.

`INFERRED`, and written into the code rather than only here: that the engine
wants the JSON rendering of the script message the page posted. It is what
WebKitGTK hands over and the only thing available, but nothing observed
confirms the engine parses exactly that. **A successful forward is not evidence
that Join worked** — it means the native returned without throwing.

### RbxStorage: the cause is ours, and §38 is superseded

Local storage has never initialised. Nine sections of `docs/analysis/flag-init.md`
characterised the engine building a correct `./appData` path and then wiping it,
and §38 concluded it happened with no host call in the window — measured four
times, and true.

It still pointed the wrong way. The engine logs `LocalStorageManager is not
available` and `Not available on the current platform`, which is what a missing
platform implementation reports, and Cordial's own startup log says
`setPlatformImpl skipped` three lines below `initStorageManagerNativeV3 ok`. The
wipe was measured inside a function that had already been told this platform has
no local storage. The window began after the decision.

**This release does not fix storage and nothing here should be read as claiming
it does.** Lifting the skip turns a silent unavailability into a crash with a
named mechanism — ten `djinni ... weakRef` exceptions and a SIGTRAP — which is a
better place to be stuck than a disassembly.

## 0.5.2 — 2026-08-05

**The Flatpak works. It never had, on any machine, and six separate faults
were between it and a running client.**

Every one of them was invisible from `cargo run`, which is the reason they
survived: the packaged build is a different machine with a different libc, a
different filesystem and no session services, and nothing here had ever been
launched by a person from an installed package until today.

- **The bionic linker was compiled with `PATH_MAX=256`.** Upstream's
  `third_party/mcpelauncher-linker/CMakeLists.txt` defines it, shrinking seven
  `char buf[PATH_MAX]` buffers across four linker files, while the `realpath`
  and `readlink` they are handed to are the *host's* and may write 4096. The
  client aborted at linker init with `*** buffer overflow detected ***`, and the
  call site read `mov $0x100,%edx` into `__realpath_chk`. Corrected to 4096 from
  `native/CMakeLists.txt`, since the manifest clones that submodule fresh by
  commit.

  **The host build is the less trustworthy of the two results here.** It makes
  the same calls through plain `realpath@plt` with *zero* `__realpath_chk` in
  the binary — unfortified, so never checked. It has carried the same undersized
  buffers all along, and a resolved path over 255 characters smashes that frame
  today with nothing to report it. The Flatpak's runtime fortifies these calls,
  so it was the first thing to notice a latent overflow rather than the thing
  that introduced one.
- **The sandbox could not see Sober's APK.** "No Roblox build found" named the
  exact path it looked in, and that path held a 97 MB `base.apk` the sandbox had
  no grant for — `~/.var/app/` inside the sandbox contains only Cordial's own
  directory. The instructions were right, the user followed them, and the
  program said they had not. Granted read-only, narrowed to `packages` rather
  than the whole of Sober's data, which holds Roblox's own storage and session.
- **The session was being written to disk.** With no `org.freedesktop.secrets`
  grant, `secrets.rs` found no service and fell back to a 0600 file, announcing
  it plainly. So the install route the README recommends was the one build that
  kept the session on disk — the exact thing the keyring work removed. It now
  reports: *the session is kept in the desktop secret service; nothing is
  written to the profile.*
- **Every connection read as metered**, because there was no system bus at all.
  Granted `org.freedesktop.NetworkManager` read-only. Deliberately **not** the
  portal: `org.freedesktop.portal.NetworkMonitor` needs no grant and was the
  obvious answer, but it reports `metered` as a boolean where `NMMetered` has
  five values, and this project's rule is that only an explicit `NO` enables a
  background download. A boolean cannot carry that, and going through the portal
  would have quietly rewritten the rule while looking tidier.
- **AT-SPI needed two grants, and one would have been a lie.**
  `org.a11y.Bus.GetAddress` answers as soon as the name is granted and hands
  back a socket path that is not in the sandbox — so the name alone moves the
  failure from a lookup to a connect and looks like a fix. With
  `--filesystem=xdg-run/at-spi` as well it connects.
- **GameMode is not fixed and is not claimed to be.** The name resolves now, so
  the grant works; `gamemoded` itself declines to register the sandboxed process
  with `rc -1`. That is progress from `ServiceUnknown`, and it is a different
  problem.

**Measured on a clean install from the published remote, with the local
override reset so only the package's own permissions applied:**

```text
[secrets] the session is kept in the desktop secret service
[accessibility] connected to the AT-SPI bus as :1.559
[android] display backend: Wayland
LOADED in 25ms
[android] vulkan: swapchain present mode FIFO -> MAILBOX
[roblox] datamodel notification: APP_READY Landing
```

**Correcting 0.5.1.** That entry said the installed shell opening a window had
not been observed, and offered a name-collision hypothesis for why it exited
immediately. The hypothesis was right and it is now observed: the launcher runs,
draws its window, finds a Roblox build and starts the client. The earlier exits
were a `GApplication` with a fixed id handing off to a development build that
already owned the name — the single-instance mechanism working, not a fault.

- **CI could hang the queue indefinitely.** The build job had no
  `timeout-minutes`, so GitHub's six-hour default applied while the concurrency
  group allows one run at a time; a run stuck in `apt-get` held everything
  behind it and two queued runs were dropped as superseded. Capped at 45
  minutes against a 7–11 minute green run.
- The website shows the mark the README leads with, and has a favicon. Both are
  copied out of `packaging/icons/` by the workflow rather than duplicated into
  `site/`.

Still broken, and none of it changed today: text fields do not paint while
focused, the pointer is not captured in first person, X11 fullscreen segfaults,
web views are unimplemented, and audio initialises then fails with
`FMOD_ERR_OUTPUT_INIT`.

## 0.5.1 — 2026-08-05

**The Flatpak actually builds, the remote actually exists, and the application
ID no longer claims a domain this project does not own.**

- **The application ID is now `io.github.luohoa97.Cordial`.** It was
  `org.cordial.Cordial`, which is a claim on `cordial.org` — a domain registered
  in 1999 and in active use by somebody else. Flathub requires an ID you
  demonstrably control, so the old one was unsubmittable. Done on the same day
  the remote first went live, deliberately: a rename before a package has users
  costs a `git mv`, and after it costs everyone their profiles. **A Flatpak's
  data lives at `~/.var/app/<app-id>/`, so an existing install keeps its old
  directory and will look freshly installed.** Nobody had one.
- **The Flatpak had never built, on any machine.** The manifest asked for
  `org.freedesktop.Platform`, and the shell is GTK4 and libadwaita end to end,
  which that runtime does not carry. Every CI run had failed on
  `Package 'gtk4' not found` — five in a row, unlooked at, under a commit titled
  "A Flatpak that builds". Now `org.gnome.Platform`/`org.gnome.Sdk` 50.
- **The published `.flatpakrepo` could not be added.** Both the workflow and the
  file's own comment recorded that `GPGKey=` with an empty value is how a remote
  states "unsigned", and that omitting the line says the same thing by accident.
  Backwards: measured with flatpak 1.18.0, the empty form is refused with
  `error: Invalid gpg key`, and omitting it is accepted and sets
  `gpg-verify=false`. Only a green run could expose this, because until one
  happened the generated file had never existed.
- **GitHub Pages is enabled and the site is live** at
  <https://luohoa97.github.io/cordial/>, sharing one deployment with the OSTree
  remote under `/repo/`.
- **Releases exist.** `v0.2.0` and `v0.3.0` had been tagged and never published;
  `v0.4.0` was a version bump that never got a tag. All four are on the Releases
  page now, and this file is new.
- The documentation table stopped at ADR-013; ADR-014 through ADR-018 and
  `HANDOVER.md` were written and never listed.

Measured against the deployed remote: `remote-add` accepted, `remote-ls`
returning the app ref at 6.1 MB download and 16.1 MB installed, and `install`
placing **both** `cordial-shell` and `cordial-run` in `/app/bin`.
`cargo test --workspace`: 460 passed, 0 failed.

**Not verified:** that the installed shell opens a window. It exits 0 without
one on the developer's machine, and the leading candidate is a name collision
with a host-built instance rather than a packaging fault — but the control that
would settle it has not been run, and saying otherwise is what this file exists
not to do.

## 0.5.0 — 2026-08-05

**The app bridge, the register of what is missing, and a kernel sandbox under
plugins.**

- **The app bridge was never given its surface or platform params.** Sober makes
  87 `JNIAppBridge` calls during a join; Cordial made 3. Both
  `UpdateSurfaceAppWithPlatformParams` and `UpdateSurfaceGameWithPlatformParams`
  are now driven after `StartApp`, with `CORDIAL_SKIP_UPDATE_SURFACE=1` as the
  control. **Whether this fixes the 304 disconnect is untested** — it needs a
  join run on a real account and has not had one.
- **Error 304 is not what it looked like.** The join *succeeds*: connection at
  4.0 s, replication for roughly 61 s, then `Disconnect reason received: 304`
  from the server. The websocket to `10.110.101.222:5052` that looked like the
  cause is a red herring — Sober opens the identical connection and plays for
  942 s. `KeyRing` is identical between the two.
- **A register of everything unimplemented** (`unimplemented.rs`): unresolved
  JNI symbols, called libc stubs, unregistered natives and placeholder returns,
  printed as one report at exit. The JNI half only populates when libjnivm is
  built with `CORDIAL_JNI_TRACE=1`, and the report says so when the section is
  empty rather than letting an empty list read as a clean bill of health.
- **Graphics gets an explicit OpenGL ES option**, and loses the FastFlag that
  never worked. `FStringDebugGraphicsPreferredBackend` was measured to be inert:
  `libroblox.so` imports **zero** Vulkan symbols and 91 EGL/GL ones, and picks
  its backend by `dlopen`. Selection now works by withholding the virtual
  `libvulkan` soname, which is the mechanism that actually decides it.
  `flags_file.rs` is deleted.
- **A kernel sandbox under the plugin process.** `bwrap` with `--unshare-all`,
  a private tmpfs and the entry module bound read-only, below Deno's zero
  permissions and the capability broker. Its absence is a downgrade, not a hole,
  and every spawn prints which layers are in force ([ADR-018](docs/adr/ADR-018-plugin-sub-sandboxing.md)).
- **A Flatpak grant deliberately not taken.** `--talk-name=org.freedesktop.Flatpak`
  would have let Cordial create sub-sandboxes inside Flatpak — and, because
  `flatpak-spawn --sandbox` and `--host` are the same D-Bus name, would equally
  have handed every plugin arbitrary command execution on the host. A Flatpak
  install keeps two layers instead of three; that is the correct trade.
- **The clipboard reaches the engine.** Ctrl+V pastes into any Roblox text box
  or chat. A focused box still does not draw what you type — that is the open
  bug, not this one.
- **The queued-link banner was showing part of an auth ticket.** `summarise()`
  truncated a deep link to 64 characters, which was enough to include it. Fixed,
  with a test that a synthetic ticket cannot appear in the banner.
- **Updates:** the build window is a changelog with one button under it, sourced
  from Roblox's Creator Hub release-notes table and rendered from markdown.
  Two icons, and Download only when there is something to download.
- **Download on Wi-Fi was the metered switch wearing the wrong name.** One
  switch now: download on metered connections, off by default.
- **Per-profile VPN gate** (#8): a profile marked vpn-required refuses to launch
  without `pvpn` rather than leaking the connection.
- Deep links reach the engine; `roblox-player://` does not, and the README says
  which does.
- Tooling: the Sober issue corpus fetcher, ported to Deno, with its data kept
  out of history ([ADR-017](docs/adr/ADR-017-sober-issue-corpus.md)).

Known broken: text fields do not paint while focused; the pointer is not
captured in first person; X11 fullscreen segfaults; web views are unimplemented;
audio initialises then fails with `FMOD_ERR_OUTPUT_INIT` (51) at about t=3.3 s on
a signed-in session.

## 0.4.0 — 2026-08-03

**Sound comes out, the pointer locks, and two claims are withdrawn.**

- **Sound comes out**, and the microphone is only open while Roblox is recording.
- **The pointer locks**, and the run ends when the window closes.
- **The launcher stops imposing a session length.** `--run 30` was a debugging
  aid that had become a default.
- **The 1 fps report is withdrawn.** It was the desktop, not the engine. See the
  standing warning about present counts in [AGENTS.md](AGENTS.md) — every count
  recorded before 2026-08-02 is an idle throttle integrated over a window.
- **ADR-015 was wrong and is corrected:** Roblox publishes no Android build that
  Cordial may ship, so Cordial may fetch a build and may never ship one.
- The busy-profile message names the process actually holding the lock instead
  of guessing.
- The MangoHUD hint named two packages and sent people to the wrong one.
- A handover document, and a note at the top that this needs a maintainer.

## 0.3.0 — 2026-08-02

**Keys work in an experience, and the frame-rate metric is retracted.**

- **Keys work in an experience.** `nativePassKeyEvent` wants evdev codes, not
  Android keycodes — which is why every keystroke had been arriving as the wrong
  key or as nothing.
- **Present mode MAILBOX instead of FIFO:** a flat 60 where FIFO gave a variable
  35–50.
- **The frame-rate metric measured an idle throttle, not a frame rate.**
  `vkQueuePresentKHR` counts run at about 60/s for thirteen seconds and then drop
  to exactly 1.0/s, identically on X11 and Wayland. Synthetic pointer motion holds
  50–60 for a whole 240 s run and toggling it flips the rate both ways. Several
  earlier numbers quoted as evidence were this curve.
- **`pthread_cond_t` is 48 bytes in bionic, not 32** — a recorded finding was
  wrong and is corrected. `pthread_once` and thread-specific data are implemented
  in the shim; the stub that returned success for `pthread_once` could not be
  survived.
- **The extracted engine cache never invalidated**, so a new APK ran the old
  engine. Warm start now invalidates on APK change.
- **The build is stamped with `git describe`**, so a binary says which tree it
  came from — including `-dirty`.

## 0.2.0 — 2026-08-02

**You can sign in, stay signed in, and run two accounts side by side.**

- **Sign-in works** via Quick Sign-in, and **the session persists across
  restarts in the desktop keyring** rather than in a plaintext file on disk.
- **Profiles:** storage per account, owner-only directories, and an `flock` so
  one instance holds one profile ([ADR-012](docs/adr/ADR-012-profiles-and-instances.md)).
  Two accounts run at once.
- **A libadwaita shell** that finds a Roblox build, explains how to get one when
  there is not, and launches the client beside itself.
- **A native Wayland backend** — xdg_shell, EGL, input, and the `zwp_text_input_v3`
  IME bridge — replacing the blank window.
- **Real plugin brokers:** `presence.set`, `notify.send`, `url.open` and
  `events.*` do something, as payloads and effects rather than channels
  ([ADR-007](docs/adr/ADR-007-host-resources-are-brokered.md)).
- **An AT-SPI accessibility bridge** over Roblox's Android accessibility surface.
- The scroll wheel works; every mouse button and a real delta are passed, so the
  camera can turn.
- **The XSendEvent injection advice is retracted:** Wayland has no such thing.
- Text is invisible while typing because Android draws it with a widget — the
  cause is recorded, the fix is not in this release.
